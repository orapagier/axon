use crate::state::AppState;
use crate::tools::workflow::WorkflowEngine;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

/// Parse a webhook body as JSON; an empty body is `null`, a non-JSON body is
/// wrapped as `{ "body": "<raw>" }` so plain-text/form callers still reach the
/// workflow (the Json extractor used to 4xx those before they got here).
fn parse_webhook_body(body: &Bytes) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(v) => v,
        Err(_) => json!({ "body": String::from_utf8_lossy(body) }),
    }
}

/// C2: idempotency key for a generic webhook. Prefers an explicit
/// `Idempotency-Key` header, then a body `event_id`/`idempotency_key`, then —
/// only when `workflow.webhook_dedup_window_secs > 0` — a body hash bucketed by
/// that window so rapid sender retries dedup without permanently suppressing a
/// deliberately repeated payload. `None` ⇒ don't dedup (fire every call).
fn webhook_dedup_key(
    state: &AppState,
    workflow_id: &str,
    headers: &HeaderMap,
    payload: &Value,
    body: &Bytes,
) -> Option<String> {
    let explicit = headers
        .get("idempotency-key")
        .or_else(|| headers.get("x-idempotency-key"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            payload
                .get("event_id")
                .or_else(|| payload.get("idempotency_key"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });
    if let Some(k) = explicit {
        return Some(format!("{workflow_id}:{k}"));
    }
    let window = state.settings.workflow_webhook_dedup_window_secs();
    if window > 0 && !body.is_empty() {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(body);
        let hash = hex::encode(h.finalize());
        let bucket = chrono::Utc::now().timestamp() / window;
        return Some(format!("{workflow_id}:{hash}:{bucket}"));
    }
    None
}

pub async fn handle_external_webhook(
    Path(workflow_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    tracing::info!("Received external webhook for workflow {}", workflow_id);
    let payload = parse_webhook_body(&body);

    // C2: skip a duplicate delivery (sender retry / double-submit) when an
    // idempotency key is available. Acknowledge with 200 so the sender stops.
    if let Some(key) = webhook_dedup_key(&state, &workflow_id, &headers, &payload, &body) {
        if crate::tools::workflow::trigger_dedup_seen(&state, "webhook", &key) {
            tracing::info!("External webhook {}: duplicate event, skipped", workflow_id);
            return (
                StatusCode::OK,
                Json(json!({ "ok": true, "duplicate": true, "workflow_id": workflow_id })),
            );
        }
    }

    // Store the payload in the engine's external trigger data map, then fire.
    WorkflowEngine::set_external_trigger_data(workflow_id.clone(), payload).await;

    match WorkflowEngine::run_in_background(&workflow_id, &state, None) {
        Ok(run_id) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "workflow_id": workflow_id,
                "run_id": run_id,
                "message": "Workflow triggered successfully"
            })),
        ),
        Err(e) => {
            tracing::error!(
                "Failed to trigger workflow {} via webhook: {}",
                workflow_id,
                e
            );
            (
                StatusCode::OK,
                Json(json!({
                    "ok": false,
                    "error": e.to_string()
                })),
            )
        }
    }
}

/// C1: parse a resume request body as JSON; an empty body (e.g. a GET click from
/// an approval email) resumes with a null payload. Non-JSON bodies are wrapped as
/// `{ "body": "<raw>" }` so a form post or plain text still reaches downstream.
fn parse_resume_body(body: &Bytes) -> Value {
    if body.is_empty() {
        return Value::Null;
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(v) => v,
        Err(_) => json!({ "body": String::from_utf8_lossy(body) }),
    }
}

async fn do_resume(
    state: AppState,
    token: String,
    outcome: &str,
    payload: Value,
) -> (StatusCode, Json<Value>) {
    match WorkflowEngine::resume_by_token(&state, &token, outcome, payload).await {
        Ok(v) => (StatusCode::OK, Json(v)),
        // Unknown / expired / already-used token, or a finished run: 410 Gone so a
        // double-submit is idempotent rather than an error the caller retries.
        Err(e) => (
            StatusCode::GONE,
            Json(json!({ "ok": false, "error": e })),
        ),
    }
}

/// `GET|POST /webhook/resume/:token` — wake a Wait-for-webhook (or generic
/// approval) run, attaching the request body as the resumed node's payload.
pub async fn handle_resume(
    Path(token): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let payload = parse_resume_body(&body);
    do_resume(state, token, "resumed", payload).await
}

/// `GET|POST /webhook/approve/:token` — resume an Approval run down output 0.
pub async fn handle_approve(
    Path(token): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let payload = parse_resume_body(&body);
    do_resume(state, token, "approved", payload).await
}

/// `GET|POST /webhook/reject/:token` — resume an Approval run down output 1.
pub async fn handle_reject(
    Path(token): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let payload = parse_resume_body(&body);
    do_resume(state, token, "rejected", payload).await
}
