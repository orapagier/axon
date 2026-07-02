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
//! every model and grouping them by real outcome: `by_status.healthy` plus a
//! `by_status.unhealthy` that is itself split by failure reason (rate_limited,
//! payment_required, invalid_key, not_found, …).

use crate::state::AppState;
use rusqlite::OptionalExtension;
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
    // snapshot) can't see. The report keeps the by_status/summary shape but splits
    // `unhealthy` into fixed failure-reason buckets (rate_limited, payment_required,
    // invalid_key, not_found, …), each list sorted alphabetically by name; api_key is
    // never included. The probe itself is async and DB-free (it sits with `list`
    // above the DB block), and only opens a connection afterwards if `auto_delete`
    // asks it to prune terminal-failure models — see below.
    if operation == "health_check" {
        let mut report = crate::router::health_check(&state.router, &state.settings).await;

        // Optional inline cleanup. If the node opts into auto-deleting one or more
        // TERMINAL failure categories, prune those models right here so the workflow
        // needs no downstream Delete node. `auto_delete_categories` only ever admits
        // irrecoverable reasons (DELETABLE_CATEGORIES) — recoverable ones
        // (rate_limited, payment_required, server_error, timeout, unreachable) and,
        // critically, `misconfigured` (an unresolved ${VAR}, i.e. a server-env
        // problem, not a bad model) are never eligible, so a transient outage or a
        // missing env var can't wipe a good row. The health probe already ran
        // DB-free above, so the connection is opened only now and dropped before the
        // router reload await, honoring the !Sync-across-await rule.
        let auto_delete = auto_delete_categories(config);
        if !auto_delete.is_empty() {
            // Names to remove come from the report's own unhealthy buckets.
            let mut targets: Vec<(String, String)> = Vec::new(); // (category, name)
            if let Some(unhealthy) = report
                .get("by_status")
                .and_then(|v| v.get("unhealthy"))
                .and_then(|v| v.as_object())
            {
                for cat in &auto_delete {
                    if let Some(entries) = unhealthy.get(cat).and_then(|v| v.as_array()) {
                        for e in entries {
                            if let Some(name) = e.get("name").and_then(|v| v.as_str()) {
                                targets.push((cat.clone(), name.to_string()));
                            }
                        }
                    }
                }
            }

            // Seed the selected categories so the `deleted` shape tracks the ticked
            // boxes (a category is present even if it removed nothing this run).
            let mut deleted_by_cat: serde_json::Map<String, Value> =
                auto_delete.iter().map(|c| (c.clone(), json!([]))).collect();
            let mut delete_errors: Vec<Value> = Vec::new();
            let mut deleted_count: u64 = 0;

            if !targets.is_empty() {
                let reload_models = {
                    let conn = state.db.get().map_err(|e| format!("DB pool: {e}"))?;
                    for (cat, name) in &targets {
                        match crate::dashboard::api::apply_delete_model(&conn, name) {
                            Ok(0) => {} // already gone — nothing to record
                            Ok(_) => {
                                deleted_count += 1;
                                if let Some(Value::Array(arr)) = deleted_by_cat.get_mut(cat) {
                                    arr.push(json!(name));
                                }
                            }
                            // Don't discard the whole health report over one bad
                            // delete; record it and carry on.
                            Err(e) => delete_errors
                                .push(json!({ "name": name, "category": cat, "error": e })),
                        }
                    }
                    crate::config::load_models_from_db(&conn).unwrap_or_default()
                    // `conn` is dropped here, before the await below.
                };
                // Hot-reload the router so later nodes and live traffic see the
                // pruned model set immediately.
                crate::router::update_models(&state.router, reload_models).await;
            }

            let mut deleted = serde_json::Map::new();
            deleted.insert("count".into(), json!(deleted_count));
            deleted.insert("by_category".into(), Value::Object(deleted_by_cat));
            if !delete_errors.is_empty() {
                deleted.insert("errors".into(), Value::Array(delete_errors));
            }
            if let Some(obj) = report.as_object_mut() {
                obj.insert("deleted".into(), Value::Object(deleted));
            }
        }

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
            // Upsert: patch the row when the name already exists, otherwise
            // insert it. This lets a workflow loop register a batch of models
            // idempotently — a re-run refreshes existing rows instead of
            // failing on the `name` primary key, and a name that isn't in the
            // DB yet gets added rather than silently updating zero rows.
            "update" => {
                let name = trimmed_field(config, "name");
                if name.is_empty() {
                    return Err("Homeostasis Upsert requires a model Name".into());
                }
                let mut patch = build_model_payload(config);
                let exists = conn
                    .query_row(
                        "SELECT 1 FROM models WHERE name=?1",
                        rusqlite::params![name],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(|e| format!("DB lookup: {e}"))?
                    .is_some();
                let action = if exists {
                    crate::dashboard::api::apply_update_model(&conn, &name, &patch)?;
                    "update"
                } else {
                    // New model: Add needs name + provider, which
                    // build_model_payload already carries when set. A blank
                    // provider errors here — the router can't route a
                    // providerless model, so that's the right failure.
                    crate::dashboard::api::apply_add_model(&conn, &patch)?;
                    "add"
                };
                reload_models = Some(crate::config::load_models_from_db(&conn).unwrap_or_default());
                // Echo which fields changed; redact any key so it never leaves the node.
                if let Some(obj) = patch.as_object_mut() {
                    obj.remove("name");
                    if obj.contains_key("api_key") {
                        obj.insert("api_key".into(), json!("***"));
                    }
                }
                json!({ "ok": true, "resource": "model", "operation": "upsert", "action": action, "name": name, "changed": patch })
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

/// Failure categories eligible for Health Check auto-deletion. Deliberately only
/// the TERMINAL ones — a model that's gone (`not_found`) or whose credentials /
/// permissions are rejected (`invalid_key` / `forbidden`), plus `bad_request` for a
/// model the provider won't accept a call for. Recoverable categories
/// (rate_limited, payment_required, server_error, timeout, unreachable) and
/// especially `misconfigured` (an unresolved ${VAR} — a server-env problem, not a
/// bad model) are intentionally absent, so a transient outage or a missing env var
/// can never auto-delete a good model. Must stay a subset of the router's
/// `FAILURE_CATEGORIES`.
const DELETABLE_CATEGORIES: &[&str] = &["not_found", "invalid_key", "forbidden", "bad_request"];

/// Parse the node's `auto_delete` selection into the categories to prune, keeping
/// only `DELETABLE_CATEGORIES` members (deduplicated, order preserved). Accepts the
/// multiOptions array shape or a comma-separated string, and silently drops any
/// category not on the allow-list — so even a hand-edited config can't opt into
/// deleting `misconfigured` or a recoverable failure.
fn auto_delete_categories(config: &Value) -> Vec<String> {
    let raw: Vec<String> = match config.get("auto_delete") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .collect(),
        Some(Value::String(s)) => s.split(',').map(|s| s.trim().to_string()).collect(),
        _ => Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    for c in raw {
        if DELETABLE_CATEGORIES.contains(&c.as_str()) && !out.contains(&c) {
            out.push(c);
        }
    }
    out
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
