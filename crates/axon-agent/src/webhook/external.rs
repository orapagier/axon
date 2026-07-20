use crate::state::AppState;
use crate::tools::workflow::nodes::respond_to_webhook::{ResponseBody, WebhookHttpResponse};
use crate::tools::workflow::WorkflowEngine;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
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

/// 3.1: does this workflow contain an enabled Respond to Webhook node? Decides
/// whether the handler holds the request open for a workflow-authored response
/// or acknowledges immediately (the legacy fire-and-forget path).
fn workflow_has_respond_node(state: &AppState, workflow_id: &str) -> bool {
    let Ok(conn) = state.db.get() else {
        return false;
    };
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_nodes WHERE workflow_id = ?1 \
         AND node_type = 'respondToWebhook' AND enabled = 1)",
        [workflow_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n != 0)
    .unwrap_or(false)
}

/// The pre-3.1 acknowledgement: 200 + run id, sent when no respond node fires
/// (none on the taken branch, respond timeout, or plain fire-and-forget).
fn default_ack(workflow_id: &str, run_id: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "workflow_id": workflow_id,
            "run_id": run_id,
            "message": "Workflow triggered successfully"
        })),
    )
        .into_response()
}

/// Materialize a Respond-to-Webhook payload as the actual HTTP response. The
/// node already validated the status; author headers are applied best-effort
/// (an invalid name/value is skipped, never a 500) and content-type defaults
/// by body kind unless the author set their own.
fn build_custom_response(resp: WebhookHttpResponse) -> Response {
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::OK);
    let mut out = match resp.body {
        ResponseBody::Json(v) => (status, Json(v)).into_response(),
        ResponseBody::Text(s) => (status, s).into_response(),
        ResponseBody::Empty => status.into_response(),
    };
    let headers = out.headers_mut();
    for (name, value) in resp.headers {
        let Ok(name) = axum::http::header::HeaderName::from_bytes(name.as_bytes()) else {
            tracing::warn!(
                "Respond to Webhook: invalid header name '{}', skipped",
                name
            );
            continue;
        };
        let Ok(value) = axum::http::header::HeaderValue::from_str(&value) else {
            tracing::warn!(
                "Respond to Webhook: invalid value for header '{}', skipped",
                name
            );
            continue;
        };
        headers.insert(name, value);
    }
    out
}

/// This endpoint has no authentication — the workflow id in the URL is the only
/// thing standing between a caller and a workflow run, so it has to be
/// unguessable. New workflows get a UUID v4, but `upsert_workflow_core` accepts
/// a caller-supplied `id` (needed for export/import round-trips), and a
/// human-friendly one turns this into a public trigger anyone can hit.
///
/// Not enforced, because rejecting these ids would break webhook URLs already
/// registered with third parties; flagged so it is at least visible.
fn warn_if_guessable(workflow_id: &str) {
    let uuid_shaped = uuid::Uuid::parse_str(workflow_id).is_ok();
    if !uuid_shaped {
        tracing::warn!(
            "External webhook fired for workflow '{}', whose id is not a random UUID. \
             This URL is unauthenticated, so a guessable id lets anyone trigger the \
             workflow. Consider recreating it so it gets a generated id.",
            workflow_id
        );
    }
}

pub async fn handle_external_webhook(
    Path(workflow_id): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    tracing::info!("Received external webhook for workflow {}", workflow_id);
    warn_if_guessable(&workflow_id);
    let payload = parse_webhook_body(&body);

    // C2: skip a duplicate delivery (sender retry / double-submit) when an
    // idempotency key is available. Acknowledge with 200 so the sender stops.
    if let Some(key) = webhook_dedup_key(&state, &workflow_id, &headers, &payload, &body) {
        if crate::tools::workflow::trigger_dedup_seen(&state, "webhook", &key) {
            tracing::info!("External webhook {}: duplicate event, skipped", workflow_id);
            return (
                StatusCode::OK,
                Json(json!({ "ok": true, "duplicate": true, "workflow_id": workflow_id })),
            )
                .into_response();
        }
    }

    // 3.1: a workflow with a Respond to Webhook node authors its own HTTP
    // response — hold the request open (bounded) and serve what the node sends.
    if workflow_has_respond_node(&state, &workflow_id) {
        match WorkflowEngine::run_in_background_for_webhook(&workflow_id, &state, Some(payload)) {
            Ok((run_id, rx)) => {
                let timeout = state.settings.workflow_webhook_respond_timeout_secs();
                return match tokio::time::timeout(std::time::Duration::from_secs(timeout), rx).await
                {
                    Ok(Ok(resp)) => build_custom_response(resp),
                    // Channel closed: the run ended (or suspended on a durable
                    // Wait) without the respond node firing — e.g. it sits on a
                    // branch that wasn't taken. Fall back to the default ack.
                    Ok(Err(_)) => default_ack(&workflow_id, &run_id),
                    // Timeout: release the caller; the run keeps executing and
                    // the run-end guard will drop the now-orphaned channel.
                    Err(_) => {
                        tracing::warn!(
                            "External webhook {}: respond node didn't fire within {}s, sent default ack (run {} continues)",
                            workflow_id, timeout, run_id
                        );
                        default_ack(&workflow_id, &run_id)
                    }
                };
            }
            Err(e) => {
                tracing::error!(
                    "Failed to trigger workflow {} via webhook: {}",
                    workflow_id,
                    e
                );
                return (
                    StatusCode::OK,
                    Json(json!({ "ok": false, "error": e.to_string() })),
                )
                    .into_response();
            }
        }
    }

    // Fire with the real "webhook" source (not "manual"): this isolates the run to
    // the workflow's webhook trigger node(s) and — critically — keeps the engine on
    // the production path so A4 pinned data is NOT applied (pins are an editor-only
    // convenience; a live webhook must execute the real nodes). The payload rides
    // the call, staged keyed by the new run id so concurrent deliveries to the
    // same workflow each see their own body.
    match WorkflowEngine::run_in_background_with_payload(
        &workflow_id,
        &state,
        "webhook",
        None,
        Some(payload),
    ) {
        Ok(run_id) => default_ack(&workflow_id, &run_id),
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
                .into_response()
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
    node_id: String,
    run_id: String,
    outcome: &str,
    payload: Value,
) -> (StatusCode, Json<Value>) {
    match WorkflowEngine::resume_by_node(&state, &node_id, &run_id, outcome, payload).await {
        Ok(v) => (StatusCode::OK, Json(v)),
        // No run waiting at this node+run, or an already-resumed/finished run:
        // 410 Gone so a double-submit is idempotent, not an error to retry.
        Err(e) => (StatusCode::GONE, Json(json!({ "ok": false, "error": e }))),
    }
}

/// `GET|POST /webhook/resume/:node_id/:run_id` — wake a Wait-for-webhook (or
/// generic approval) run parked on `node_id`, attaching the request body as the
/// resumed node's payload. The (unguessable) run id scopes the wake to that one
/// run; it only works while that run is still parked here.
pub async fn handle_resume(
    Path((node_id, run_id)): Path<(String, String)>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let payload = parse_resume_body(&body);
    do_resume(state, node_id, run_id, "resumed", payload).await
}

/// `GET|POST /webhook/approve/:node_id/:run_id` — resume an Approval run down output 0.
pub async fn handle_approve(
    Path((node_id, run_id)): Path<(String, String)>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let payload = parse_resume_body(&body);
    do_resume(state, node_id, run_id, "approved", payload).await
}

/// `GET|POST /webhook/reject/:node_id/:run_id` — resume an Approval run down output 1.
pub async fn handle_reject(
    Path((node_id, run_id)): Path<(String, String)>,
    State(state): State<AppState>,
    body: Bytes,
) -> impl IntoResponse {
    let payload = parse_resume_body(&body);
    do_resume(state, node_id, run_id, "rejected", payload).await
}
