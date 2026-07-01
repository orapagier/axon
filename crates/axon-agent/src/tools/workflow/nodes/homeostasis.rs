//! Homeostasis — self-manage dashboard resources from inside a workflow.
//!
//! Today the only resource is AI models: Add / Update / Delete / List rows in
//! the `models` table, plus a Health Check that live-probes every model. It
//! reuses the exact DB helpers that back the
//! `/api/models` dashboard endpoints (`dashboard::api::apply_*`) so encryption,
//! provider normalization and validation never drift, then hot-reloads the live
//! router the same way those HTTP handlers do. The `resource` field is a
//! dropdown with a single value for now, leaving room to add settings /
//! workflows later without reshaping the node.
//!
//! Unlike the HTTP API — which only receives the keys the user changed — a node
//! config always carries every field. So mutations go through
//! `build_model_payload`, which keeps only fields the user actually set: for Add
//! the omitted ones fall back to the helper's defaults, and for Update they mean
//! "leave unchanged". Secrets are never echoed back (List omits `api_key`).
//!
//! List is special: it reports the LIVE router snapshot (the same health view as
//! `/api/models`) rather than the `models` table, so each row also carries the
//! model's runtime `status`, cooldown reset and error counts — not just config.
//! But List is still a passive snapshot: a model only turns `unavailable` after
//! it has actually failed in real traffic, so a freshly-added bad key still reads
//! as healthy. Health Check closes that gap by sending a live one-line probe to
//! every model and grouping them by real outcome (healthy / unhealthy).

use crate::state::AppState;
use serde_json::{json, Value};

pub(crate) async fn execute(config: &Value, state: &AppState) -> Result<Value, String> {
    let resource = config
        .get("resource")
        .and_then(|v| v.as_str())
        .unwrap_or("model");
    if resource != "model" {
        return Err(format!("Homeostasis: unsupported resource '{resource}'"));
    }
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("list");

    // List reports the LIVE router view — per-model status, cooldown resets and
    // error counts — not just static config, so a workflow can see which models
    // are healthy, rate-limited or parked. Sourced from the router snapshot (the
    // same data the dashboard's /api/models shows); api_key is never included.
    // Handled before the DB block below because get_status is async and
    // rusqlite's Connection must not be held across an await.
    if operation == "list" {
        let models = crate::router::get_status(&state.router).await;
        let count = models.len();
        return Ok(json!({
            "ok": true, "resource": "model", "operation": "list",
            "count": count, "models": models
        }));
    }

    // Health Check goes further than List: it sends a real (tiny) provider call
    // to every model and reports which ones actually work right now — surfacing
    // bad API keys, wrong endpoints or unreachable providers that List (a cached
    // snapshot) can't see. The report groups models by outcome (healthy /
    // unhealthy) and sorts each group alphabetically by name; api_key is never
    // included. Async and DB-free, so it sits with `list` above the DB block.
    if operation == "health_check" {
        let mut report = crate::router::health_check(&state.router, &state.settings).await;
        if let Some(obj) = report.as_object_mut() {
            obj.insert("ok".into(), json!(true));
            obj.insert("resource".into(), json!("model"));
            obj.insert("operation".into(), json!("health_check"));
        }
        return Ok(report);
    }

    // Do every DB touch synchronously and drop the connection before awaiting
    // the router reload: rusqlite's Connection is !Sync and must not be held
    // across an `.await` (the workflow future has to stay Send for tokio::spawn).
    let reload_models: Option<Vec<crate::providers::types::ModelRecord>>;
    let result = {
        let conn = state.db.get().map_err(|e| format!("DB pool: {e}"))?;
        match operation {
            "add" => {
                let payload = build_model_payload(config);
                crate::dashboard::api::apply_add_model(&conn, &payload)?;
                reload_models = Some(crate::config::load_models_from_db(&conn).unwrap_or_default());
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                json!({ "ok": true, "resource": "model", "operation": "add", "name": name })
            }
            "update" => {
                let name = trimmed_field(config, "name");
                if name.is_empty() {
                    return Err("Homeostasis Update requires a model Name".into());
                }
                let mut patch = build_model_payload(config);
                crate::dashboard::api::apply_update_model(&conn, &name, &patch)?;
                reload_models = Some(crate::config::load_models_from_db(&conn).unwrap_or_default());
                // Echo which fields changed; redact any key so it never leaves the node.
                if let Some(obj) = patch.as_object_mut() {
                    obj.remove("name");
                    if obj.contains_key("api_key") {
                        obj.insert("api_key".into(), json!("***"));
                    }
                }
                json!({ "ok": true, "resource": "model", "operation": "update", "name": name, "changed": patch })
            }
            "delete" => {
                let name = trimmed_field(config, "name");
                if name.is_empty() {
                    return Err("Homeostasis Delete requires a model Name".into());
                }
                let deleted = crate::dashboard::api::apply_delete_model(&conn, &name)?;
                reload_models = Some(crate::config::load_models_from_db(&conn).unwrap_or_default());
                json!({ "ok": true, "resource": "model", "operation": "delete", "name": name, "deleted": deleted })
            }
            other => return Err(format!("Homeostasis: unknown operation '{other}'")),
        }
        // `conn` is dropped here, before the await below.
    };

    if let Some(new_models) = reload_models {
        crate::router::update_models(&state.router, new_models).await;
    }
    Ok(result)
}

/// Build a typed, sparse model payload from the node config. Only fields the user
/// set are included — string fields when non-blank, numbers coerced from a real
/// JSON number or a numeric string, and `enabled` as a tri-state where '' (or
/// absent) means "leave unchanged". Blank/absent numeric fields are omitted so
/// the add helper applies its own defaults and the update helper skips them.
fn build_model_payload(config: &Value) -> Value {
    let mut p = serde_json::Map::new();

    for key in ["name", "provider", "model_id", "api_key", "base_url", "role"] {
        if let Some(s) = config.get(key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                p.insert(key.to_string(), json!(s));
            }
        }
    }

    for key in ["priority", "max_tokens", "timeout_secs"] {
        if let Some(n) = number_field(config.get(key)) {
            p.insert(key.to_string(), json!(n));
        }
    }

    match config.get("enabled") {
        Some(Value::Bool(b)) => {
            p.insert("enabled".into(), json!(*b));
        }
        Some(Value::String(s)) => match s.trim() {
            "true" => {
                p.insert("enabled".into(), json!(true));
            }
            "false" => {
                p.insert("enabled".into(), json!(false));
            }
            _ => {}
        },
        _ => {}
    }

    Value::Object(p)
}

/// Coerce a config field to i64: real numbers pass through, numeric strings
/// parse, and blank/non-numeric yields None (so the field is left untouched).
fn number_field(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<i64>().ok()
            }
        }
        _ => None,
    }
}

fn trimmed_field(config: &Value, key: &str) -> String {
    config
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}
