use crate::state::AppState;
use crate::tools::workflow::WorkflowEngine;
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use once_cell::sync::OnceCell;
use serde::Deserialize;
use serde_json::{json, Value};

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

// ── Credentials from credentials.json (agent working dir) ────────────────────
//
// The app-level secrets (app_secret for HMAC, verify_token for the GET
// handshake) are still read from credentials.json — they belong to the Facebook
// *App*, which is shared across every connected Page. Per-Page tokens live in
// the `credentials` table and are selected per workflow node.

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
        let paths = ["credentials.json", "../mcp/credentials.json"];
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

    // Process entries. `entry.id` is the Page ID the event belongs to — used to
    // route the event to workflows bound to that Page's credential.
    if let Some(entries) = payload.get("entry").and_then(|e| e.as_array()) {
        for entry in entries {
            let page_id = entry
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if let Some(changes) = entry.get("changes").and_then(|c| c.as_array()) {
                for change in changes {
                    process_change(&state, &page_id, change).await;
                }
            }
            // Handle messaging events (different structure)
            if let Some(messaging) = entry.get("messaging").and_then(|m| m.as_array()) {
                for msg in messaging {
                    process_messaging(&state, &page_id, msg).await;
                }
            }
        }
    }

    // Facebook expects 200 within 20 seconds
    StatusCode::OK
}

// ── Process a feed change (comment, post, reaction, mention, rating, …) ───────

async fn process_change(state: &AppState, page_id: &str, change: &Value) {
    let field = change.get("field").and_then(|f| f.as_str()).unwrap_or("");
    let value = match change.get("value") {
        Some(v) => v,
        None => return,
    };

    let item = value.get("item").and_then(|i| i.as_str()).unwrap_or("");
    let verb = value.get("verb").and_then(|v| v.as_str()).unwrap_or("");

    // Map Meta's webhook (field, item) onto our flat event_type vocabulary. The
    // `feed` field carries most Page activity (split by `item`); `mention` and
    // `ratings` are separate subscription fields. Anything we don't recognise is
    // logged and dropped so an unexpected payload can't fire a workflow.
    let event_type = match (field, item) {
        ("feed", "comment") => "comment",
        ("feed", "post") => "post",
        ("feed", "status") => "status",
        ("feed", "photo") => "photo",
        ("feed", "video") => "video",
        ("feed", "album") => "album",
        ("feed", "reaction") => "reaction",
        ("feed", "like") => "like",
        ("feed", "share") => "share",
        ("mention", _) => "mention",
        ("ratings", _) => "rating",
        _ => {
            tracing::debug!("FB webhook: unhandled field={}, item={}", field, item);
            return;
        }
    };

    // Skip deletes — only track new/edited content
    if verb == "remove" {
        return;
    }

    // `ratings`/recommendations name the author under `reviewer` and put the body
    // in `review_text`, so fall back to those when the usual `from`/`message`
    // fields are absent.
    let actor = value.get("from").or_else(|| value.get("reviewer"));
    let from_name = actor
        .and_then(|f| f.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let from_id = actor
        .and_then(|f| f.get("id"))
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let message = value
        .get("message")
        .or_else(|| value.get("review_text"))
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
            "INSERT INTO webhook_events (source, event_type, from_name, from_id, object_id, parent_id, message, permalink, raw_json, page_id)
             VALUES ('facebook', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![event_type, from_name, from_id, object_id, post_id, message, permalink, raw, page_id],
        );
        match res {
            Ok(_) => tracing::info!(
                "FB webhook: stored {} from '{}' (page {}): {}",
                event_type,
                from_name,
                page_id,
                message.chars().take(60).collect::<String>()
            ),
            Err(e) => tracing::error!("FB webhook: DB insert failed: {}", e),
        }
    }

    // Only drive workflows for content with a body (comments/posts with text).
    if message.is_empty() {
        return;
    }

    let comment_id = if event_type == "comment" {
        object_id.clone()
    } else {
        String::new()
    };
    let event = json!({
        "trigger": "facebook",
        "page_id": page_id,
        "event_type": event_type,
        "from_name": from_name,
        "from_id": from_id,
        "message": message,
        "object_id": object_id,
        "comment_id": comment_id,
        "parent_id": parent_id,
        "post_id": post_id,
        "permalink": permalink,
    });
    dispatch_facebook_workflows(state, page_id, event_type, event).await;
}

// ── Process a messaging event (Messenger) ────────────────────────────────────

async fn process_messaging(state: &AppState, page_id: &str, msg: &Value) {
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
            "INSERT INTO webhook_events (source, event_type, from_name, from_id, object_id, message, raw_json, page_id)
             VALUES ('facebook', 'message', ?1, ?2, '', ?3, ?4, ?5)",
            rusqlite::params![sender_name, sender_id, message_text, raw, page_id],
        );
        tracing::info!(
            "FB webhook: stored message from {} (PSID {}, page {})",
            sender_name,
            sender_id,
            page_id
        );
    }

    let event = json!({
        "trigger": "facebook",
        "page_id": page_id,
        "event_type": "message",
        "from_name": sender_name,
        "from_id": sender_id,
        "recipient_id": sender_id,
        "message": message_text,
    });
    dispatch_facebook_workflows(state, page_id, "message", event).await;
}

// ── Workflow dispatch (replaces the built-in auto-reply pipeline) ─────────────

/// Find every enabled workflow whose Facebook Stimulus trigger is bound to this
/// Page (via its selected credential's `page_id`) and start a run, injecting the
/// event as the trigger node's output. A trigger with no credential selected
/// acts as a catch-all for any Page. An optional comma-separated `events` field
/// on the trigger node filters by event_type ("comment", "message", "post").
async fn dispatch_facebook_workflows(
    state: &AppState,
    page_id: &str,
    event_type: &str,
    event: Value,
) {
    let mut targets: Vec<String> = Vec::new();
    {
        let conn = match state.db.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("FB dispatch: DB error: {e}");
                return;
            }
        };
        let mut stmt = match conn.prepare(
            "SELECT wn.workflow_id, wn.config
             FROM workflow_nodes wn
             JOIN workflows w ON w.id = wn.workflow_id
             WHERE w.enabled = 1
               AND wn.node_type IN ('trigger', 'stimulus', 'circadian')
               AND json_extract(wn.config, '$.type') = 'facebook'",
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("FB dispatch: query prepare failed: {e}");
                return;
            }
        };

        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        });
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("FB dispatch: query failed: {e}");
                return;
            }
        };

        for row in rows.flatten() {
            let (workflow_id, cfg_str) = row;
            let cfg: Value = serde_json::from_str(&cfg_str).unwrap_or(Value::Null);

            // event_type filter
            let events = cfg.get("events").and_then(|v| v.as_str()).unwrap_or("");
            if !events.trim().is_empty()
                && !events
                    .split(',')
                    .map(|s| s.trim())
                    .any(|s| s.eq_ignore_ascii_case(event_type))
            {
                continue;
            }

            // page binding via the selected credential
            let cred_id = cfg
                .get("credential_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if cred_id.is_empty() {
                targets.push(workflow_id); // catch-all
                continue;
            }
            let cred_page = conn
                .query_row(
                    "SELECT data FROM credentials WHERE id = ?1",
                    [cred_id],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .and_then(|d| serde_json::from_str::<Value>(&d).ok())
                .and_then(|d| {
                    d.get("page_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            if cred_page == page_id {
                targets.push(workflow_id);
            }
        }
    }

    for workflow_id in targets {
        dbg_log(&format!(
            "FB dispatch: firing workflow {workflow_id} for page {page_id} ({event_type})"
        ));
        WorkflowEngine::set_external_trigger_data(workflow_id.clone(), event.clone()).await;
        if let Err(e) =
            WorkflowEngine::run_in_background_with_source(&workflow_id, state, "facebook", None)
        {
            tracing::error!("FB dispatch: failed to start workflow {workflow_id}: {e}");
        }
    }
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
