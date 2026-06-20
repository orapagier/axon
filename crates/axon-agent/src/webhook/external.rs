use crate::state::AppState;
use crate::tools::workflow::WorkflowEngine;
use axum::{
    extract::{Path, State},
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
