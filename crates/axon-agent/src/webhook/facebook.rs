use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use once_cell::sync::OnceCell;
use serde::Deserialize;
use serde_json::Value;

/// Debug logger — writes to plain-text file readable over SSH
fn dbg_log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/autoreply_debug.log")
    {
        let now_manila =
            chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
        let _ = writeln!(f, "[{}] {}", now_manila.format("%H:%M:%S"), msg);
    }
}

// ── Serializing reply queue ───────────────────────────────────────────────────
// All comment auto-replies go through this single-worker channel so they are
// processed one at a time with a 15-30s gap between each send.
// The LIKE is fired inside the worker (not on webhook arrival) so it feels like
// a real person opened the comment, hit Like, then typed a reply.

struct ReplyJob {
    event: super::responder::FacebookEvent,
    tools: crate::tools::ToolRegistry,
    router: crate::router::SharedRouter,
    settings: std::sync::Arc<crate::config::RuntimeSettings>,
    messaging: std::sync::Arc<crate::messaging::MessagingHub>,
    memory: std::sync::Arc<crate::memory::MemoryStore>,
}

static REPLY_QUEUE: OnceCell<tokio::sync::mpsc::Sender<ReplyJob>> = OnceCell::new();

fn reply_queue() -> &'static tokio::sync::mpsc::Sender<ReplyJob> {
    REPLY_QUEUE.get_or_init(|| {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ReplyJob>(256);
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                let object_id_for_gap = job.event.object_id.clone();
                let f_event = job.event.clone();
                let f_tools = job.tools.clone();
                let f_settings = job.settings.clone();
                let f_messaging = job.messaging.clone();
                let f_memory = job.memory.clone();

                // Wait 15-30s BEFORE doing anything — this applies to every
                // comment including the very first one, so it always looks like
                // a human noticed the comment after a natural delay.
                let mut hash = 0usize;
                for b in object_id_for_gap.bytes() {
                    hash = hash.wrapping_add(b as usize);
                }
                let gap_secs = 5 + (hash % 11) as u64; // 5 to 15 seconds
                dbg_log(&format!(
                    "QUEUE: waiting {}s before processing job",
                    gap_secs
                ));
                tokio::time::sleep(tokio::time::Duration::from_secs(gap_secs)).await;

                // Fire the Like — after the human-like wait, same as a real
                // person opening the notification and hitting Like first.
                let like_tools = job.tools.clone();
                let like_id = job.event.object_id.clone();
                tokio::spawn(async move {
                    super::responder::handle_fb_like(like_id, like_tools).await;
                });

                // Run the full reply pipeline with the existing 45s timeout.
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(45),
                    super::responder::handle_auto_reply(
                        job.event,
                        job.tools,
                        job.router,
                        job.settings,
                        job.messaging,
                        job.memory,
                    ),
                )
                .await;

                match result {
                    Ok(()) => {}
                    Err(_) => {
                        dbg_log("QUEUE TIMEOUT: auto-reply pipeline exceeded 45s");
                        tracing::error!("FB reply queue: pipeline timed out (45s)");
                        super::responder::handle_fallback_reply(
                            f_event,
                            f_tools,
                            f_settings,
                            f_messaging,
                            f_memory,
                        )
                        .await;
                    }
                }
            }
        });
        tx
    })
}

// ── Credentials from credentials.json (agent working dir) ────────────────────

static FB_CREDS: OnceCell<FbCreds> = OnceCell::new();

#[derive(Debug, Clone, Default)]
pub struct FbCreds {
    pub app_secret: String,
    pub verify_token: String,
    pub page_id: String,
}

pub fn load_fb_creds() -> &'static FbCreds {
    FB_CREDS.get_or_init(|| {
        // credentials.json lives in the agent's working dir (CWD): crates/axon-agent
        // in dev, or the deploy `core/` dir in prod. ../mcp/ kept as a legacy fallback.
        let paths = [
            "credentials.json",
            "../mcp/credentials.json",
        ];
        for path in &paths {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(json) = serde_json::from_str::<Value>(&data) {
                    let fb = json.get("facebook").cloned().unwrap_or_default();
                    let creds = FbCreds {
                        app_secret: fb
                            .get("app_secret")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        verify_token: fb
                            .get("verify_token")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        page_id: fb
                            .get("page_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    };
                    tracing::info!("FB webhook: loaded credentials from {}", path);
                    return creds;
                }
            }
        }
        tracing::warn!("FB webhook: credentials.json not found, webhook verification disabled");
        FbCreds::default()
    })
}

// ── Facebook Webhook Verification (GET) ──────────────────────────────────────

#[derive(Deserialize)]
pub struct FbVerifyQuery {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
}

pub async fn fb_verify(Query(q): Query<FbVerifyQuery>) -> impl IntoResponse {
    let creds = load_fb_creds();

    if creds.verify_token.is_empty() {
        tracing::warn!("FB webhook verify: verify_token not set in credentials.json");
        return (StatusCode::FORBIDDEN, "Webhook not configured".to_string());
    }

    match (q.mode.as_deref(), q.verify_token.as_deref(), q.challenge) {
        (Some("subscribe"), Some(token), Some(challenge)) if token == creds.verify_token => {
            tracing::info!("FB webhook verified successfully");
            (StatusCode::OK, challenge)
        }
        _ => {
            tracing::warn!("FB webhook verification failed");
            (StatusCode::FORBIDDEN, "Verification failed".to_string())
        }
    }
}

// ── Facebook Webhook Event Handler (POST) ────────────────────────────────────

pub async fn fb_event(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let creds = load_fb_creds();

    // HMAC signature validation
    if !creds.app_secret.is_empty() {
        let sig_header = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_signature(&creds.app_secret, &body, sig_header) {
            tracing::warn!("FB webhook: invalid HMAC signature");
            return StatusCode::UNAUTHORIZED;
        }
    }

    // Parse payload
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("FB webhook: invalid JSON: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    // Process entries
    if let Some(entries) = payload.get("entry").and_then(|e| e.as_array()) {
        for entry in entries {
            if let Some(changes) = entry.get("changes").and_then(|c| c.as_array()) {
                for change in changes {
                    process_change(&state, change).await;
                }
            }
            // Handle messaging events (different structure)
            if let Some(messaging) = entry.get("messaging").and_then(|m| m.as_array()) {
                for msg in messaging {
                    process_messaging(&state, msg).await;
                }
            }
        }
    }

    // Facebook expects 200 within 20 seconds
    StatusCode::OK
}

// ── Process a feed change (comment, post, reaction) ──────────────────────────

async fn process_change(state: &AppState, change: &Value) {
    let field = change.get("field").and_then(|f| f.as_str()).unwrap_or("");
    let value = match change.get("value") {
        Some(v) => v,
        None => return,
    };

    let item = value.get("item").and_then(|i| i.as_str()).unwrap_or("");
    let verb = value.get("verb").and_then(|v| v.as_str()).unwrap_or("");

    let event_type = match (field, item) {
        ("feed", "comment") => "comment",
        ("feed", "post") => "post",
        ("feed", "reaction") => "reaction",
        ("feed", "share") => "share",
        _ => {
            tracing::debug!("FB webhook: unhandled field={}, item={}", field, item);
            return;
        }
    };

    // Skip deletes — only track new/edited content
    if verb == "remove" {
        return;
    }

    let from_name = value
        .get("from")
        .and_then(|f| f.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let from_id = value
        .get("from")
        .and_then(|f| f.get("id"))
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let message = value
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let object_id = value
        .get("comment_id")
        .or_else(|| value.get("post_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parent_id = value
        .get("parent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let post_id = value
        .get("post_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let permalink = value
        .get("post")
        .and_then(|p| p.get("permalink_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let raw = serde_json::to_string(value).unwrap_or_default();

    if let Ok(conn) = state.db.get() {
        let res = conn.execute(
            "INSERT INTO webhook_events (source, event_type, from_name, from_id, object_id, parent_id, message, permalink, raw_json)
             VALUES ('facebook', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![event_type, from_name, from_id, object_id, post_id, message, permalink, raw],
        );
        match res {
            Ok(_) => tracing::info!(
                "FB webhook: stored {} from '{}': {}",
                event_type,
                from_name,
                message.chars().take(60).collect::<String>()
            ),
            Err(e) => tracing::error!("FB webhook: DB insert failed: {}", e),
        }
    }

    // Spawn auto-reply for comments (non-blocking)
    // NOTE: Like is fired inside the queue worker when the comment is picked up,
    // not on raw webhook arrival — so it looks like a real person noticing it.
    if event_type == "comment" && !message.is_empty() {
        dbg_log(&format!(
            "WEBHOOK COMMENT from='{}' msg='{}' comment_id='{}'",
            from_name,
            message.chars().take(60).collect::<String>(),
            object_id
        ));
        let event = super::responder::FacebookEvent {
            event_type: "comment".to_string(),
            from_name,
            from_id,
            message,
            object_id,
            parent_id,
            post_id: post_id.clone(),
            permalink,
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        let tools = state.tools.clone();
        let router = state.router.clone();
        let settings = state.settings.clone();
        let messaging = state.messaging.clone();
        let memory = state.memory.clone();
        let job = ReplyJob {
            event,
            tools,
            router,
            settings,
            messaging,
            memory,
        };
        match reply_queue().try_send(job) {
            Ok(()) => dbg_log("QUEUE: comment enqueued"),
            Err(e) => {
                dbg_log(&format!("QUEUE FULL or closed: {}", e));
                tracing::error!("FB reply queue send failed: {}", e);
            }
        }
    }
}

// ── Process a messaging event (Messenger) ────────────────────────────────────

async fn process_messaging(state: &AppState, msg: &Value) {
    let sender_id = msg
        .get("sender")
        .and_then(|s| s.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let sender_name = msg
        .get("sender")
        .and_then(|s| s.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let message_text = msg
        .get("message")
        .and_then(|m| m.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    if message_text.is_empty() {
        return;
    }

    let raw = serde_json::to_string(msg).unwrap_or_default();

    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "INSERT INTO webhook_events (source, event_type, from_name, from_id, object_id, message, raw_json)
             VALUES ('facebook', 'message', ?1, ?2, '', ?3, ?4)",
            rusqlite::params![sender_name, sender_id, message_text, raw],
        );
        tracing::info!(
            "FB webhook: stored message from {} (PSID {})",
            sender_name,
            sender_id
        );
    }

    // Spawn auto-reply (non-blocking)
    dbg_log(&format!(
        "WEBHOOK MESSAGE from='{}' (PSID {}) msg='{}'",
        sender_name,
        sender_id,
        message_text.chars().take(60).collect::<String>()
    ));
    let event = super::responder::FacebookEvent {
        event_type: "message".to_string(),
        from_name: sender_name,
        from_id: sender_id,
        message: message_text,
        object_id: String::new(),
        parent_id: String::new(),
        post_id: String::new(),
        permalink: String::new(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    let tools = state.tools.clone();
    let router = state.router.clone();
    let settings = state.settings.clone();
    let messaging = state.messaging.clone();
    let memory = state.memory.clone();
    tokio::spawn(async move {
        let f_event = event.clone();
        let f_tools = tools.clone();
        let f_settings = settings.clone();
        let f_messaging = messaging.clone();
        let f_memory = memory.clone();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            super::responder::handle_auto_reply(event, tools, router, settings, messaging, memory),
        )
        .await;
        match result {
            Ok(()) => {}
            Err(_) => {
                dbg_log("TIMEOUT: auto-reply message pipeline exceeded 45s");
                tracing::error!("FB auto-reply message pipeline timed out (45s)");
                super::responder::handle_fallback_reply(
                    f_event,
                    f_tools,
                    f_settings,
                    f_messaging,
                    f_memory,
                )
                .await;
            }
        }
    });
}

// ── HMAC-SHA256 Signature Verification ───────────────────────────────────────

fn verify_signature(app_secret: &str, body: &[u8], sig_header: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let expected = match sig_header.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };

    let mut mac = match Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);

    let computed = hex::encode(mac.finalize().into_bytes());
    computed == expected
}

// ── Query notifications (used by internal tool) ──────────────────────────────

pub fn get_unread_events(
    state: &AppState,
    source: &str,
    event_type: Option<&str>,
    limit: u32,
    mark_read: bool,
) -> anyhow::Result<Value> {
    let conn = state.db.get()?;

    let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match event_type {
        Some(et) => (
            "SELECT id, event_type, from_name, from_id, object_id, parent_id, message, permalink, created_at
             FROM webhook_events WHERE source=?1 AND read=0 AND event_type=?2
             ORDER BY created_at DESC LIMIT ?3".to_string(),
            vec![
                Box::new(source.to_string()) as Box<dyn rusqlite::types::ToSql>,
                Box::new(et.to_string()),
                Box::new(limit),
            ],
        ),
        None => (
            "SELECT id, event_type, from_name, from_id, object_id, parent_id, message, permalink, created_at
             FROM webhook_events WHERE source=?1 AND read=0
             ORDER BY created_at DESC LIMIT ?2".to_string(),
            vec![
                Box::new(source.to_string()) as Box<dyn rusqlite::types::ToSql>,
                Box::new(limit),
            ],
        ),
    };

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "event_type": row.get::<_, String>(1)?,
            "from_name": row.get::<_, String>(2).unwrap_or_default(),
            "from_id": row.get::<_, String>(3).unwrap_or_default(),
            "object_id": row.get::<_, String>(4).unwrap_or_default(),
            "parent_id": row.get::<_, String>(5).unwrap_or_default(),
            "message": row.get::<_, String>(6).unwrap_or_default(),
            "permalink": row.get::<_, String>(7).unwrap_or_default(),
            "created_at": row.get::<_, String>(8)?,
        }))
    })?;

    let mut events = Vec::new();
    let mut ids = Vec::new();
    for row in rows {
        if let Ok(v) = row {
            if let Some(id) = v.get("id").and_then(|i| i.as_i64()) {
                ids.push(id);
            }
            events.push(v);
        }
    }
    drop(stmt);

    // Mark as read
    if mark_read && !ids.is_empty() {
        let placeholders: String = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE webhook_events SET read=1 WHERE id IN ({})",
            placeholders
        );
        let _ = conn.execute(&sql, rusqlite::params_from_iter(ids.iter()));
    }

    Ok(serde_json::json!({
        "source": source,
        "unread_count": events.len(),
        "events": events,
    }))
}

/// Cleanup old webhook events (called periodically)
pub fn cleanup_old_events(state: &AppState, days: u32) {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "DELETE FROM webhook_events WHERE created_at < datetime('now', ?1)",
            rusqlite::params![format!("-{} days", days)],
        );
    }
}
