use super::*;

// ── SELF-IMPROVEMENT (few-shot memory + prompt optimizer) ─────────────────────

/// Status of both self-improvement features (few-shot pool + optimizer), the
/// last optimizer run, and the currently applied prompt version.
pub async fn optimizer_status(State(state): State<AppState>) -> Json<Value> {
    Json(axum_state_status(&state))
}

fn axum_state_status(state: &AppState) -> Value {
    crate::optimizer::status(&state.db, &state.settings)
}

/// Trigger a prompt-optimizer cycle immediately (spawned in the background so
/// the request returns fast; completion is surfaced via dashboard
/// notifications and the optimizer run history).
pub async fn run_optimizer(State(state): State<AppState>) -> Json<Value> {
    let st = state.clone();
    tokio::spawn(async move {
        match crate::optimizer::run_once(&st).await {
            Ok(Some(outcome)) => tracing::info!("Manual optimizer run: {outcome}"),
            Ok(None) => tracing::info!("Manual optimizer run: nothing to optimize"),
            Err(e) => tracing::warn!("Manual optimizer run failed: {e:#}"),
        }
    });
    Json(json!({ "started": true, "note": "optimizer cycle started in the background" }))
}

/// Re-mine qualifying completed runs into the few-shot pool now (blocking
/// SQLite, so off the async runtime).
pub async fn run_fewshot_sweep(State(state): State<AppState>) -> Json<Value> {
    let db = state.db.clone();
    let settings = state.settings.clone();
    match tokio::task::spawn_blocking(move || crate::fewshot::sweep(&db, &settings)).await {
        Ok(Ok(inserted)) => Json(json!({ "inserted": inserted })),
        Ok(Err(e)) => Json(json!({ "error": format!("sweep failed: {e:#}") })),
        Err(e) => Json(json!({ "error": format!("sweep join failed: {e}") })),
    }
}

/// Prompt versions + optimizer run history for review.
pub async fn optimizer_history(State(state): State<AppState>) -> Json<Value> {
    let limit = 100_i64;
    let history = crate::optimizer::history(&state.db, limit).unwrap_or_else(|e| {
        json!({ "error": format!("history read failed: {e:#}") })
    });
    let runs = crate::optimizer::run_history(&state.db, limit).unwrap_or_else(|e| {
        json!({ "error": format!("run history read failed: {e:#}") })
    });
    Json(json!({ "status": axum_state_status(&state), "history": history, "runs": runs }))
}

/// Apply a stored prompt (candidate or historical version) as the live system
/// prompt. The current prompt is snapshotted first, so this is reversible.
pub async fn optimizer_apply(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Json<Value> {
    match crate::optimizer::apply_history(&state.db, &state.settings, id) {
        Ok(()) => Json(json!({ "ok": true, "applied_id": id })),
        Err(e) => Json(json!({ "error": format!("apply failed: {e:#}") })),
    }
}

/// Dismiss a proposed candidate without applying it.
pub async fn optimizer_reject(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Json<Value> {
    match crate::optimizer::reject_history(&state.db, id) {
        Ok(()) => Json(json!({ "ok": true, "rejected_id": id })),
        Err(e) => Json(json!({ "error": format!("reject failed: {e:#}") })),
    }
}

/// The mined few-shot examples, for debugging / curation from the dashboard.
pub async fn optimizer_examples(State(state): State<AppState>) -> Json<Value> {
    match crate::fewshot::list_examples(&state.db, 200) {
        Ok(rows) => Json(json!({ "examples": rows })),
        Err(e) => Json(json!({ "error": format!("examples read failed: {e:#}") })),
    }
}