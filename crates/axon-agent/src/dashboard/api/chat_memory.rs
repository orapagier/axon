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

/// Chat-history search: full-text search over message content (`short_term_fts`,
/// see `db/migrations/0022_short_term_fts.sql`), returning the conversations
/// that contain a match with a highlighted snippet — powers the Chat page
/// sidebar search box. Distinct from `search_memory`, which searches
/// long-term memory rather than conversation transcripts.
pub async fn search_conversations(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let q = params.get("q").map(|s| s.trim()).unwrap_or("");
    if q.is_empty() {
        return Json(json!({ "conversations": [] }));
    }
    // Quote the term so FTS5 query-syntax characters in free-typed search
    // text (-, *, :, etc.) are treated literally instead of as operators.
    let fts_query = format!("\"{}\"", q.replace('"', "\"\""));
    let mut items: Vec<Value> = Vec::new();
    if let Ok(conn) = state.db.get() {
        // No GROUP BY here: SQLite's snippet()/highlight()/bm25() auxiliary
        // functions only work against a row that's a direct FTS5 match, and
        // error out ("unable to use function ... in the requested context")
        // once the query plan introduces a GROUP BY. Instead fetch matches
        // ordered by most-recently-matched message and dedupe to one (the
        // most relevant) row per conversation in application code below.
        if let Ok(mut s) = conn.prepare(
            "SELECT c.id, c.title, c.updated_at,
                    snippet(short_term_fts, 0, '<mark>', '</mark>', '…', 8) AS snippet
             FROM short_term_fts
             JOIN short_term ON short_term.id = short_term_fts.rowid
             JOIN conversations c ON c.id = short_term.session_id
             WHERE short_term_fts MATCH ?1
             ORDER BY short_term.created_at DESC
             LIMIT 200",
        ) {
            if let Ok(iter) = s.query_map(rusqlite::params![fts_query], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    json!({
                        "id": r.get::<_, String>(0)?,
                        "title": r.get::<_, String>(1)?,
                        "updated_at": r.get::<_, String>(2)?,
                        "snippet": r.get::<_, String>(3)?,
                    }),
                ))
            }) {
                let mut seen = std::collections::HashSet::new();
                for (id, row) in iter.filter_map(|r| r.ok()) {
                    if items.len() >= 50 {
                        break;
                    }
                    if seen.insert(id) {
                        items.push(row);
                    }
                }
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
///
/// Assistant rows are stored with raw `<send_file>` tags (the canonical form
/// the agent itself uses); resolve them to authorized download links here so
/// reloaded chats keep working links. Trace rows (persisted at run end, i.e.
/// right AFTER their assistant row) are re-ordered in front of the answer to
/// match the live layout: user → trace → agent bubble.
pub async fn get_conversation_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    let rows = state.memory.get_session(&id).unwrap_or_default();
    let mut messages: Vec<Value> = Vec::new();
    for r in rows {
        match r.role.as_str() {
            "user" => messages.push(json!({
                "role": "user",
                "content": r.content,
                "created_at": r.created_at,
            })),
            "assistant" => messages.push(json!({
                "role": "assistant",
                "content": crate::agent::r#loop::resolve_send_file_links(&r.content),
                "created_at": r.created_at,
            })),
            "trace" => {
                let items: Value = serde_json::from_str(&r.content).unwrap_or_else(|_| json!([]));
                if items.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                    continue;
                }
                let msg = json!({
                    "role": "trace",
                    "items": items,
                    "created_at": r.created_at,
                });
                let insert_at = if messages
                    .last()
                    .map(|m| m["role"] == "assistant")
                    .unwrap_or(false)
                {
                    messages.len() - 1
                } else {
                    messages.len()
                };
                messages.insert(insert_at, msg);
            }
            _ => {}
        }
    }
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
