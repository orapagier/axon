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
    // above the DB block), and only opens a connection afterwards if `auto_delete` /
    // `auto_disable` asks it to prune or park failing models — see below.
    if operation == "health_check" {
        let mut report = crate::router::health_check(&state.router, &state.settings).await;

        // Optional inline cleanup. The node can opt into two independent, disjoint
        // actions, each driven by the report's own unhealthy buckets:
        //   • auto-DELETE terminal-failure models (DELETABLE_CATEGORIES): gone
        //     (not_found), rejected credentials/permissions (invalid_key/forbidden),
        //     or an unacceptable request (bad_request) — irrecoverable, so pruning
        //     them right here needs no downstream Delete node.
        //   • auto-DISABLE recoverable-failure models (DISABLEABLE_CATEGORIES): out
        //     of credits (payment_required), provider outage (server_error), timeout
        //     or unreachable — transient reasons that typically clear within a day or
        //     two, so we PARK the row (enabled=false) instead of destroying it, ready
        //     to be flipped back on later. `rate_limited` is deliberately excluded (it
        //     recovers in minutes/an hour via the router's own cooldown), as is
        //     `misconfigured` (an unresolved ${VAR} — a server-env problem, not a bad
        //     model). The two category sets never overlap, so no model is both deleted
        //     and disabled in one run. The health probe already ran DB-free above, so
        //     the connection is opened only now and dropped before the router-reload
        //     await, honoring the !Sync-across-await rule.
        let auto_delete = auto_delete_categories(config);
        let auto_disable = auto_disable_categories(config);
        if !auto_delete.is_empty() || !auto_disable.is_empty() {
            // Names to act on come from the report's own unhealthy buckets.
            let delete_targets = collect_targets(&report, &auto_delete);
            let disable_targets = collect_targets(&report, &auto_disable);

            // Seed the selected categories so each summary tracks the ticked boxes
            // (a category is present even if it changed nothing this run).
            let mut deleted_by_cat: serde_json::Map<String, Value> =
                auto_delete.iter().map(|c| (c.clone(), json!([]))).collect();
            let mut disabled_by_cat: serde_json::Map<String, Value> = auto_disable
                .iter()
                .map(|c| (c.clone(), json!([])))
                .collect();
            let mut delete_errors: Vec<Value> = Vec::new();
            let mut disable_errors: Vec<Value> = Vec::new();
            let mut deleted_count: u64 = 0;
            let mut disabled_count: u64 = 0;

            if !delete_targets.is_empty() || !disable_targets.is_empty() {
                let reload_models = {
                    let conn = state.db.get().map_err(|e| format!("DB pool: {e}"))?;
                    for (cat, name) in &delete_targets {
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
                    for (cat, name) in &disable_targets {
                        match crate::dashboard::api::apply_disable_model(&conn, name, cat) {
                            Ok(0) => {} // missing or already disabled — nothing to record
                            Ok(_) => {
                                disabled_count += 1;
                                if let Some(Value::Array(arr)) = disabled_by_cat.get_mut(cat) {
                                    arr.push(json!(name));
                                }
                            }
                            Err(e) => disable_errors
                                .push(json!({ "name": name, "category": cat, "error": e })),
                        }
                    }
                    crate::config::load_models_from_db(&conn).unwrap_or_default()
                    // `conn` is dropped here, before the await below.
                };
                // Hot-reload the router so later nodes and live traffic see the
                // pruned/parked model set immediately.
                crate::router::update_models(&state.router, reload_models).await;
            }

            if let Some(obj) = report.as_object_mut() {
                if !auto_delete.is_empty() {
                    let mut deleted = serde_json::Map::new();
                    deleted.insert("count".into(), json!(deleted_count));
                    deleted.insert("by_category".into(), Value::Object(deleted_by_cat));
                    if !delete_errors.is_empty() {
                        deleted.insert("errors".into(), Value::Array(delete_errors));
                    }
                    obj.insert("deleted".into(), Value::Object(deleted));
                }
                if !auto_disable.is_empty() {
                    let mut disabled = serde_json::Map::new();
                    disabled.insert("count".into(), json!(disabled_count));
                    disabled.insert("by_category".into(), Value::Object(disabled_by_cat));
                    if !disable_errors.is_empty() {
                        disabled.insert("errors".into(), Value::Array(disable_errors));
                    }
                    obj.insert("disabled".into(), Value::Object(disabled));
                }
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

/// Collect the categories to prune from the node's per-reason checkboxes
/// (`auto_delete_<category>` booleans). Driven by `DELETABLE_CATEGORIES`, so only
/// terminal reasons are ever consulted — there is no `auto_delete_misconfigured` /
/// `auto_delete_rate_limited` box, so even a hand-edited config can't opt into
/// deleting a recoverable or local-config failure. Order follows the allow-list.
fn auto_delete_categories(config: &Value) -> Vec<String> {
    DELETABLE_CATEGORIES
        .iter()
        .filter(|cat| config_bool(config, &format!("auto_delete_{cat}")))
        .map(|cat| (*cat).to_string())
        .collect()
}

/// Failure categories eligible for Health Check auto-DISABLE — the RECOVERABLE
/// ones that typically clear within a day or two, so parking the model
/// (enabled=false) beats deleting it: out of credits (`payment_required`), a
/// provider outage (`server_error`), a `timeout` or an `unreachable` provider.
/// `rate_limited` is intentionally absent (it recovers in minutes/an hour via the
/// router's own cooldown — no need to disable), as is `misconfigured` (a server-env
/// problem, not a bad model). Disjoint from `DELETABLE_CATEGORIES`, so a model is
/// never both deleted and disabled in one run. Must stay a subset of the router's
/// `FAILURE_CATEGORIES`.
const DISABLEABLE_CATEGORIES: &[&str] =
    &["payment_required", "server_error", "timeout", "unreachable"];

/// Collect the categories to park from the node's per-reason checkboxes
/// (`auto_disable_<category>` booleans). Driven by `DISABLEABLE_CATEGORIES`, so a
/// hand-edited config can't opt into disabling a terminal or excluded category.
fn auto_disable_categories(config: &Value) -> Vec<String> {
    DISABLEABLE_CATEGORIES
        .iter()
        .filter(|cat| config_bool(config, &format!("auto_disable_{cat}")))
        .map(|cat| (*cat).to_string())
        .collect()
}

/// Pull `(category, model name)` pairs out of a health report's
/// `by_status.unhealthy.<category>` buckets for the given categories. Shared by
/// auto-delete and auto-disable so both read the exact set the probe just produced.
fn collect_targets(report: &Value, cats: &[String]) -> Vec<(String, String)> {
    let mut targets = Vec::new();
    if let Some(unhealthy) = report
        .get("by_status")
        .and_then(|v| v.get("unhealthy"))
        .and_then(|v| v.as_object())
    {
        for cat in cats {
            if let Some(entries) = unhealthy.get(cat).and_then(|v| v.as_array()) {
                for e in entries {
                    if let Some(name) = e.get("name").and_then(|v| v.as_str()) {
                        targets.push((cat.clone(), name.to_string()));
                    }
                }
            }
        }
    }
    targets
}

/// Read a boolean node-config field, tolerating both a real JSON bool and the
/// stringy "true"/"false" some form widgets emit. Absent/blank ⇒ false.
fn config_bool(config: &Value, key: &str) -> bool {
    match config.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Build a typed, sparse model payload from the node config. Only fields the user
/// set are included — string fields when non-blank, numbers coerced from a real
/// JSON number or a numeric string, and `enabled` as a tri-state where '' (or
/// absent) means "leave unchanged". Blank/absent numeric fields are omitted so
/// the add helper applies its own defaults and the update helper skips them.
fn build_model_payload(config: &Value) -> Value {
    let mut p = serde_json::Map::new();

    for key in [
        "name", "provider", "model_id", "api_key", "base_url", "role",
    ] {
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

#[cfg(test)]
mod tests {
    use super::{
        auto_delete_categories, auto_disable_categories, collect_targets, DELETABLE_CATEGORIES,
        DISABLEABLE_CATEGORIES,
    };
    use serde_json::json;

    #[test]
    fn ticked_checkboxes_select_categories_in_allowlist_order() {
        // Boxes ticked out of order still come back in DELETABLE_CATEGORIES order.
        let cfg = json!({
            "auto_delete_forbidden": true,
            "auto_delete_not_found": true,
            "auto_delete_invalid_key": true,
        });
        assert_eq!(
            auto_delete_categories(&cfg),
            vec!["not_found", "invalid_key", "forbidden"]
        );
    }

    #[test]
    fn accepts_stringy_boolean() {
        let cfg = json!({ "auto_delete_not_found": "true", "auto_delete_bad_request": "false" });
        assert_eq!(auto_delete_categories(&cfg), vec!["not_found"]);
    }

    #[test]
    fn no_checkbox_exists_for_non_deletable_categories() {
        // The safety guarantee: there is no auto_delete_misconfigured /
        // auto_delete_rate_limited box, so a hand-edited config that sets one is
        // simply never consulted.
        let cfg = json!({
            "auto_delete_misconfigured": true,
            "auto_delete_rate_limited": true,
            "auto_delete_payment_required": true,
            "auto_delete_healthy": true,
            "auto_delete_not_found": true,
        });
        assert_eq!(auto_delete_categories(&cfg), vec!["not_found"]);
        // And the allow-list itself never contains a recoverable/local category.
        for banned in [
            "misconfigured",
            "rate_limited",
            "payment_required",
            "server_error",
            "timeout",
            "unreachable",
        ] {
            assert!(
                !DELETABLE_CATEGORIES.contains(&banned),
                "{banned} must not be deletable"
            );
        }
    }

    #[test]
    fn empty_when_no_boxes_ticked() {
        assert!(auto_delete_categories(&json!({})).is_empty());
        assert!(auto_delete_categories(&json!({ "auto_delete_not_found": false })).is_empty());
        assert!(auto_disable_categories(&json!({})).is_empty());
    }

    #[test]
    fn disable_boxes_select_categories_in_allowlist_order() {
        // Ticked out of order, returned in DISABLEABLE_CATEGORIES order; stringy
        // booleans are honored just like the delete path.
        let cfg = json!({
            "auto_disable_unreachable": true,
            "auto_disable_payment_required": "true",
            "auto_disable_server_error": true,
            "auto_disable_timeout": "false",
        });
        assert_eq!(
            auto_disable_categories(&cfg),
            vec!["payment_required", "server_error", "unreachable"]
        );
    }

    #[test]
    fn no_disable_box_exists_for_rate_limited_or_terminal_reasons() {
        // rate_limited is excluded (fast auto-recovery) and terminal reasons belong
        // to Delete, not Disable — a hand-edited config can't opt into either.
        let cfg = json!({
            "auto_disable_rate_limited": true,
            "auto_disable_misconfigured": true,
            "auto_disable_not_found": true,
        });
        assert!(auto_disable_categories(&cfg).is_empty());
        for banned in ["rate_limited", "misconfigured", "error", "healthy"] {
            assert!(
                !DISABLEABLE_CATEGORIES.contains(&banned),
                "{banned} must not be disableable"
            );
        }
    }

    #[test]
    fn delete_and_disable_sets_are_disjoint() {
        // The core safety invariant: no model can be both deleted and disabled in
        // one run, because the two category allow-lists never overlap.
        for cat in DELETABLE_CATEGORIES {
            assert!(
                !DISABLEABLE_CATEGORIES.contains(cat),
                "{cat} is both deletable and disableable"
            );
        }
    }

    #[test]
    fn collect_targets_reads_only_requested_buckets() {
        // Mirrors the health report shape: names are pulled from the requested
        // unhealthy buckets only, ignoring healthy and non-selected categories.
        let report = json!({
            "by_status": {
                "healthy": [{ "name": "good" }],
                "unhealthy": {
                    "timeout": [{ "name": "slow-1" }, { "name": "slow-2" }],
                    "payment_required": [{ "name": "broke" }],
                    "not_found": [{ "name": "gone" }],
                }
            }
        });
        let cats = vec!["payment_required".to_string(), "timeout".to_string()];
        assert_eq!(
            collect_targets(&report, &cats),
            vec![
                ("payment_required".to_string(), "broke".to_string()),
                ("timeout".to_string(), "slow-1".to_string()),
                ("timeout".to_string(), "slow-2".to_string()),
            ]
        );
        // Not-selected categories (not_found) and healthy names are never collected.
        assert!(collect_targets(&report, &[]).is_empty());
    }
}
