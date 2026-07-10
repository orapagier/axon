use super::*;

pub async fn get_tools(State(state): State<AppState>) -> Json<Value> {
    // ToolRegistry already contains MCP tools (registered during connect).
    // Do NOT also add mcp.all_tools() — that would duplicate them.
    let tools = state.tools.all().await;
    Json(json!({"tools": tools}))
}

pub async fn get_fonts() -> Json<Value> {
    let dir = crate::tools::image_tool::app_data_files_dir();
    let dir_str = dir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "None".to_string());
    let fonts = crate::tools::image_tool::discover_fonts().unwrap_or_default();
    tracing::info!("get_fonts: dir={}, count={}", dir_str, fonts.len());
    Json(json!({"fonts": fonts}))
}

pub async fn get_fovea_folders() -> Json<Value> {
    let folders =
        crate::tools::image_tool::discover_image_folders().unwrap_or_else(|| vec![".".to_string()]);
    Json(json!({ "folders": folders }))
}

/// SQLite database files in the managed `databases/` folder — powers the
/// Database node's file picker.
pub async fn get_database_list() -> Json<Value> {
    Json(crate::tools::workflow::nodes::database::list_sqlite_databases())
}

pub async fn toggle_tool(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    state.tools.set_enabled(&name, enabled).await;
    if let Err(e) = crate::tools::overrides::upsert(&state.db, &name, enabled) {
        tracing::warn!("Failed to persist tool override for '{}': {}", name, e);
    }
    Json(json!({"ok": true}))
}

pub async fn reload_tools(State(state): State<AppState>) -> Json<Value> {
    match state.tools.reload("tools").await {
        Ok(count) => Json(json!({"ok":true, "count": count})),
        Err(e) => Json(json!({"ok":false, "error": e.to_string()})),
    }
}

/// Run the database retention sweep on demand (same routine as the daily one).
/// Blocking SQLite work runs off the async runtime.
pub async fn run_retention_now(State(state): State<AppState>) -> Json<Value> {
    let db = state.db.clone();
    let settings = state.settings.clone();
    match tokio::task::spawn_blocking(move || crate::maintenance::run_retention(&db, &settings))
        .await
    {
        Ok(Ok(stats)) => {
            let summary = stats.to_string();
            Json(json!({"ok": true, "summary": summary, "stats": stats}))
        }
        Ok(Err(e)) => Json(json!({"ok": false, "error": e.to_string()})),
        Err(e) => Json(json!({"ok": false, "error": format!("task join error: {e}")})),
    }
}

pub async fn get_patterns(State(state): State<AppState>) -> Json<Value> {
    let pats = state.tool_router.all_patterns().await.unwrap_or_default();
    Json(json!({"patterns": pats}))
}

pub async fn add_pattern(State(state): State<AppState>, Json(p): Json<Value>) -> Json<Value> {
    let tool = p.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    let pat = p.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let desc = p.get("description").and_then(|v| v.as_str());
    match state.tool_router.add_pattern(tool, pat, desc).await {
        Ok(_) => Json(json!({"ok":true})),
        Err(e) => Json(json!({"ok":false, "error": e.to_string()})),
    }
}

pub async fn update_patterns_bulk(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let items = payload
        .get("patterns")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match state.tool_router.update_bulk_patterns(items).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn toggle_pattern(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let _ = state.tool_router.set_enabled(id, enabled).await;
    Json(json!({"ok": true}))
}

pub async fn delete_pattern(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let _ = state.tool_router.delete_pattern(id).await;
    Json(json!({"ok": true}))
}

pub async fn test_routing(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let msg = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tools = state.tools.all_enabled_for_agent().await;
    let (matched, info) = state.tool_router.filter_tools(msg, &tools, &[]).await;
    Json(json!({
        "matched_tools": matched.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "routing_info": info
    }))
}
