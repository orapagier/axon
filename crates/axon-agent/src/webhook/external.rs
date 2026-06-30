use crate::state::AppState;
use crate::tools::workflow::WorkflowEngine;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};

pub async fn handle_external_webhook(
    Path(workflow_id): Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    tracing::info!("Received external webhook for workflow {}", workflow_id);

    // 1. Store the payload in the engine's external trigger data map
    WorkflowEngine::set_external_trigger_data(workflow_id.clone(), payload).await;

    // 2. Trigger the workflow run in the background
    match WorkflowEngine::run_in_background(&workflow_id, &state, None) {
        Ok(run_id) => Json(json!({
            "ok": true,
            "workflow_id": workflow_id,
            "run_id": run_id,
            "message": "Workflow triggered successfully"
        })),
        Err(e) => {
            tracing::error!(
                "Failed to trigger workflow {} via webhook: {}",
                workflow_id,
                e
            );
            Json(json!({
                "ok": false,
                "error": e.to_string()
            }))
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
