use super::*;

pub async fn get_memory_recent(State(state): State<AppState>) -> Json<Value> {
    let entries = state.memory.recent_memories(30, None).unwrap_or_default();
    Json(json!({"entries": entries}))
}

/// Dashboard chat: list conversation threads for the sidebar, newest first.
/// The transcript itself lives in `short_term` (keyed by the conversation id);
/// this only returns the lightweight title/timestamp metadata.
pub async fn list_conversations(State(state): State<AppState>) -> Json<Value> {
    let mut items: Vec<Value> = Vec::new();
    if let Ok(conn) = state.db.get() {
        if let Ok(mut s) = conn.prepare(
            "SELECT id, title, created_at, updated_at FROM conversations ORDER BY updated_at DESC",
        ) {
            if let Ok(iter) = s.query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "title": r.get::<_, String>(1)?,
                    "created_at": r.get::<_, String>(2)?,
                    "updated_at": r.get::<_, String>(3)?,
                }))
            }) {
                items = iter.filter_map(|r| r.ok()).collect();
            }
        }
    }
    Json(json!({ "conversations": items }))
}

/// Create an empty conversation and return its id. Clients can also just start
/// sending with a fresh id — the row is created lazily on the first message
/// (see `dashboard::ws`), so this is only for callers that want the id up front.
pub async fn create_conversation(State(state): State<AppState>) -> Json<Value> {
    let id = Uuid::new_v4().to_string();
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "INSERT OR IGNORE INTO conversations (id, title) VALUES (?1, 'New chat')",
            rusqlite::params![id],
        );
    }
    Json(json!({ "id": id }))
}

/// Return the stored user/assistant transcript for one conversation so the UI
/// can rehydrate the thread when the user reopens it.
pub async fn get_conversation_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    let rows = state.memory.get_session(&id).unwrap_or_default();
    let messages: Vec<Value> = rows
        .into_iter()
        .filter(|r| r.role == "user" || r.role == "assistant")
        .map(|r| json!({ "role": r.role, "content": r.content, "created_at": r.created_at }))
        .collect();
    Json(json!({ "messages": messages }))
}

/// Manual title override for a conversation from the sidebar.
pub async fn rename_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if title.is_empty() {
        return Json(json!({ "ok": false, "error": "title required" }));
    }
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE conversations SET title=?2, updated_at=datetime('now') WHERE id=?1",
            rusqlite::params![id, title],
        );
    }
    Json(json!({ "ok": true }))
}

/// Delete a conversation thread and its stored messages.
pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    let _ = state.memory.clear_session(&id);
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "DELETE FROM conversations WHERE id=?1",
            rusqlite::params![id],
        );
    }
    Json(json!({ "ok": true }))
}

pub async fn search_memory(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let q = payload.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = payload.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let results = state.memory.search(q, k, None).await.unwrap_or_default();
    Json(json!({"results": results}))
}

pub async fn delete_memory(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let _ = state.memory.forget(id);
    Json(json!({"ok": true}))
}
