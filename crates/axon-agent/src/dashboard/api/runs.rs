use super::*;

// ── NEW API ENDPOINTS ─────────────────────────────────────────────────────────

pub async fn get_runs(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let job_id = params.get("job_id");
        let query = if job_id.is_some() {
            "SELECT id, task, status, iterations, total_tokens, platform, models_used, tools_used, result, created_at, job_id, parent_run_id FROM runs WHERE job_id = ?1 ORDER BY created_at DESC LIMIT 10"
        } else {
            "SELECT id, task, status, iterations, total_tokens, platform, models_used, tools_used, result, created_at, job_id, parent_run_id FROM runs ORDER BY created_at DESC LIMIT 10"
        };
        let mut s = match conn.prepare(query) {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::error!("Failed to prepare get_runs query: {}", e);
                return Json(json!({"runs": []}));
            }
        };

        // Helper to map a row safely ignoring potential NULLs on numerics
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "task": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "iterations": r.get::<_, Option<u32>>(3)?.unwrap_or(0),
                "total_tokens": r.get::<_, Option<u32>>(4)?.unwrap_or(0),
                "platform": r.get::<_, Option<String>>(5)?,
                "models_used": r.get::<_, Option<String>>(6)?,
                "tools_used": r.get::<_, Option<String>>(7)?,
                "result": r.get::<_, Option<String>>(8)?,
                "created_at": r.get::<_, String>(9)?,
                "job_id": r.get::<_, Option<String>>(10)?,
                "parent_run_id": r.get::<_, Option<String>>(11)?,
            }))
        };

        let runs: Vec<Value> = if let Some(jid) = job_id {
            match s.query_map(rusqlite::params![jid], map_row) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(e) => {
                    tracing::error!("Failed to query_map get_runs (job_id): {}", e);
                    vec![]
                }
            }
        } else {
            match s.query_map([], map_row) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(e) => {
                    tracing::error!("Failed to query_map get_runs: {}", e);
                    vec![]
                }
            }
        };
        return Json(json!({"runs": runs}));
    }
    Json(json!({"runs": []}))
}

pub async fn get_run_detail(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s_iter = try_json!(conn.prepare("SELECT id, iteration, model_name, tokens, tier, duration_ms, created_at FROM run_iterations WHERE run_id=?1 ORDER BY iteration ASC"));
        let iterations: Vec<Value> = try_json!(s_iter.query_map([&id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "iteration": r.get::<_, u32>(1)?,
                "model_name": r.get::<_, String>(2)?,
                "tokens": r.get::<_, u32>(3)?,
                "tier": r.get::<_, String>(4)?,
                "duration_ms": r.get::<_, u64>(5)?,
                "created_at": r.get::<_, String>(6)?,
            }))
        }))
        .filter_map(|r| r.ok())
        .collect();

        let mut s_tools = try_json!(conn.prepare("SELECT id, run_id, tool_name, args, result, error, duration_ms, parallel, created_at FROM tool_calls WHERE run_id=?1 ORDER BY created_at ASC"));
        let calls: Vec<Value> = try_json!(s_tools.query_map([&id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "run_id": r.get::<_, String>(1)?,
                "tool_name": r.get::<_, String>(2)?,
                "args": r.get::<_, Option<String>>(3)?,
                "result": r.get::<_, Option<String>>(4)?,
                "error": r.get::<_, Option<String>>(5)?,
                "duration_ms": r.get::<_, Option<u64>>(6)?,
                "parallel": r.get::<_, bool>(7)?,
                "created_at": r.get::<_, String>(8)?,
            }))
        }))
        .filter_map(|r| r.ok())
        .collect();

        return Json(json!({"iterations": iterations, "tool_calls": calls}));
    }
    Json(json!({"iterations": [], "tool_calls": []}))
}
