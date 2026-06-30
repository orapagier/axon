//! GitHub webhook trigger receiver.
//!
//! GitHub posts repository events (push, pull_request, issues, …) to a single
//! per-repo URL. Each Axon workflow that should react exposes its own endpoint
//! `/webhook/github/:workflow_id`; the user pastes that URL into the repo's
//! Settings → Webhooks, with content type **application/json**.
//!
//! On each delivery this handler:
//!   1. loads the workflow's GitHub trigger node (`config.type == "github"`) to
//!      read its optional shared `secret` and `events` allowlist;
//!   2. verifies the `X-Hub-Signature-256` HMAC when a secret is set (401 on
//!      mismatch) — the same scheme as the Facebook receiver;
//!   3. acknowledges GitHub's one-off `ping` without running anything;
//!   4. skips (200) events outside the allowlist;
//!   5. stores an enriched payload and starts the run scoped to the GitHub
//!      trigger branch via `trigger_source` "github".

use crate::state::AppState;
use crate::tools::workflow::WorkflowEngine;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

/// The GitHub trigger node's relevant config, loaded from the DB at delivery time.
struct TriggerCfg {
    secret: Option<String>,
    /// Lower-cased event allowlist; empty = allow every event.
    events: Vec<String>,
}

/// Read the workflow's GitHub trigger node config straight from `workflow_nodes`.
/// Returns None when the workflow has no GitHub trigger (treated as "no secret,
/// allow all" by the caller).
fn load_trigger_cfg(state: &AppState, workflow_id: &str) -> Option<TriggerCfg> {
    let conn = state.db.get().ok()?;
    let cfg_str: String = conn
        .query_row(
            "SELECT config FROM workflow_nodes
             WHERE workflow_id = ?1 AND json_extract(config, '$.type') = 'github'
             LIMIT 1",
            [workflow_id],
            |r| r.get(0),
        )
        .ok()?;
    let cfg: Value = serde_json::from_str(&cfg_str).ok()?;
    let secret = cfg
        .get("secret")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let events = cfg
        .get("events")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_lowercase())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(TriggerCfg { secret, events })
}

/// Validate GitHub's `X-Hub-Signature-256` (`sha256=<hex hmac of the raw body>`).
fn verify_signature(secret: &str, body: &[u8], sig_header: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let expected = match sig_header.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());
    computed == expected
}

pub async fn handle_github_webhook(
    Path(workflow_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let delivery = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let cfg = load_trigger_cfg(&state, &workflow_id);

    // Signature verification runs only when the node carries a secret.
    if let Some(secret) = cfg.as_ref().and_then(|c| c.secret.as_deref()) {
        let sig = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !verify_signature(secret, &body, sig) {
            tracing::warn!("GitHub webhook {workflow_id}: invalid HMAC signature");
            return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
        }
    }

    // GitHub sends a one-off `ping` when the webhook is created. Acknowledge it
    // (so the hook shows green) without starting a run.
    if event == "ping" {
        return Json(json!({ "ok": true, "pong": true })).into_response();
    }

    // Requires the repo webhook's content type to be application/json; GitHub's
    // other option (form-urlencoded) wraps the JSON in a `payload=` field and
    // won't parse here.
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "GitHub webhook {workflow_id}: non-JSON body ({e}). Set the repo webhook content type to application/json."
            );
            return (StatusCode::BAD_REQUEST, "expected application/json body").into_response();
        }
    };

    // Event allowlist: when set and this event isn't listed, acknowledge & skip.
    if let Some(c) = cfg.as_ref() {
        if !c.events.is_empty() && !c.events.contains(&event.to_lowercase()) {
            return Json(json!({ "ok": true, "skipped": event })).into_response();
        }
    }

    // C2: idempotency. GitHub redelivers failed/old deliveries with the SAME
    // X-Github-Delivery id; ack with 200 and skip if we've already fired for it
    // (scoped per workflow so the same hook on two workflow URLs fires both).
    if !delivery.is_empty()
        && crate::tools::workflow::trigger_dedup_seen(
            &state,
            "github",
            &format!("{workflow_id}:{delivery}"),
        )
    {
        tracing::info!("GitHub webhook {workflow_id}: duplicate delivery {delivery}, skipped");
        return Json(json!({ "ok": true, "duplicate": true, "delivery": delivery }))
            .into_response();
    }

    // The trigger node receives this whole object as its output, so downstream
    // expressions can read {{ $json.event }}, {{ $json.action }}, and the full
    // {{ $json.payload.* }} from GitHub.
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let enriched = json!({
        "event": event,
        "action": action,
        "delivery": delivery,
        "payload": payload,
    });
    WorkflowEngine::set_external_trigger_data(workflow_id.clone(), enriched).await;

    match WorkflowEngine::run_in_background_with_source(&workflow_id, &state, "github", None) {
        Ok(run_id) => {
            Json(json!({ "ok": true, "workflow_id": workflow_id, "run_id": run_id }))
                .into_response()
        }
        Err(e) => {
            tracing::error!("GitHub webhook: failed to trigger {workflow_id}: {e}");
            Json(json!({ "ok": false, "error": e.to_string() })).into_response()
        }
    }
}
