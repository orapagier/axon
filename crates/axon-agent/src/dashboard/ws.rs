use crate::agent::{context::AgentEvent, run_task_streaming, RunContext};
use crate::state::AppState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures::{sink::SinkExt, stream::StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;

#[derive(Deserialize)]
pub struct WsTask {
    pub task: String,
    pub session_id: String,
    pub user_time: Option<String>,
    #[serde(default)]
    pub attached_files: Vec<crate::files::AttachedFile>,
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Ensure a `conversations` row exists for this dashboard chat thread and keep
/// its `updated_at` fresh. The title is seeded from the first user message and
/// left untouched afterwards (unless still the default), so the sidebar shows a
/// meaningful label without ever clobbering a name the user set later.
fn upsert_conversation(state: &AppState, session_id: &str, task: &str) {
    let title: String = task
        .trim()
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(60)
        .collect();
    let title = if title.trim().is_empty() {
        "New chat".to_string()
    } else {
        title
    };
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "INSERT INTO conversations (id, title) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET
               updated_at = datetime('now'),
               title = CASE WHEN conversations.title IN ('New chat', '')
                            THEN excluded.title ELSE conversations.title END",
            rusqlite::params![session_id, title],
        );
    }
}

/// Best-effort: flip a still-running run row to `cancelled` so it doesn't linger
/// as `running` forever after the task future was aborted.
fn mark_run_cancelled(state: &AppState, run_id: &str, reason: &str) {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE runs SET status='cancelled', result=?2, finished_at=datetime('now') WHERE id=?1 AND status='running'",
            rusqlite::params![run_id, reason],
        );
    }
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(100);

    tokio::spawn(async move {
        // In-flight runs by session_id -> (run_id, task handle). One socket only
        // ever runs one chat task at a time, but keying by session keeps cancel
        // robust if that ever changes.
        let mut active: std::collections::HashMap<String, (String, tokio::task::JoinHandle<()>)> =
            std::collections::HashMap::new();

        while let Some(msg) = receiver.next().await {
            let Ok(msg) = msg else { continue };
            let Ok(text) = msg.to_text() else { continue };

            // Control frame: stop the run in flight for this session.
            if let Ok(ctrl) = serde_json::from_str::<serde_json::Value>(text) {
                if ctrl.get("type").and_then(|t| t.as_str()) == Some("cancel") {
                    let sid = ctrl.get("session_id").and_then(|s| s.as_str()).unwrap_or("");
                    if let Some((run_id, handle)) = active.remove(sid) {
                        handle.abort();
                        mark_run_cancelled(&state, &run_id, "Cancelled by user");
                        // Unlock any other listeners; the initiating client has
                        // already unlocked itself.
                        let _ = tx
                            .send(AgentEvent::Done {
                                run_id,
                                full_text: String::new(),
                                total_tokens: 0,
                                iterations: 0,
                                total_duration_ms: 0,
                            })
                            .await;
                    }
                    continue;
                }
            }

            // Try to parse as WsTask, but don't panic if it fails
            if let Ok(task_data) = serde_json::from_str::<WsTask>(text) {
                // Create/refresh this thread's sidebar row before the run so a
                // brand-new conversation is persisted the moment it's used.
                upsert_conversation(&state, &task_data.session_id, &task_data.task);
                let mut context = RunContext::new(
                    &task_data.task,
                    "dashboard",
                    Some(&task_data.session_id),
                    Some(&task_data.session_id),
                    None,
                    task_data.user_time.as_deref(),
                    None,
                );
                context.attached_files = task_data.attached_files.clone();

                let sid = task_data.session_id.clone();
                let run_id = context.run_id.clone();

                // Supersede any prior run still tracked for this session (normally
                // already finished; abort is a no-op on a completed handle).
                if let Some((prev_run, prev_handle)) = active.remove(&sid) {
                    prev_handle.abort();
                    mark_run_cancelled(&state, &prev_run, "Superseded by a new request");
                }

                let s2 = state.clone();
                let tx2 = tx.clone();
                let t = task_data.task.clone();
                let handle = tokio::spawn(async move {
                    let run_id = context.run_id.clone();
                    let timeout_dur = tokio::time::Duration::from_secs(300); // 5 min safety
                    let result = tokio::time::timeout(
                        timeout_dur,
                        run_task_streaming(&t, &s2, context, tx2.clone()),
                    )
                    .await;

                    match result {
                        Ok(Ok(_)) => {} // Success — Done event already emitted by run_inner
                        Ok(Err(e)) => {
                            tracing::error!("Agent task failed: {}", e);
                            let _ = tx2
                                .send(AgentEvent::Error {
                                    run_id: run_id.clone(),
                                    message: format!("Agent error: {}", e),
                                })
                                .await;
                            let _ = tx2
                                .send(AgentEvent::Done {
                                    run_id,
                                    full_text: String::new(),
                                    total_tokens: 0,
                                    iterations: 0,
                                    total_duration_ms: 0,
                                })
                                .await;
                        }
                        Err(_timeout) => {
                            tracing::error!("Agent task timed out after {:?}", timeout_dur);
                            let _ = tx2
                                .send(AgentEvent::Error {
                                    run_id: run_id.clone(),
                                    message: "Request timed out. Please try again.".into(),
                                })
                                .await;
                            let _ = tx2
                                .send(AgentEvent::Done {
                                    run_id,
                                    full_text: String::new(),
                                    total_tokens: 0,
                                    iterations: 0,
                                    total_duration_ms: 0,
                                })
                                .await;
                        }
                    }
                });
                active.insert(sid, (run_id, handle));
            }
        }
    });

    while let Some(event) = rx.recv().await {
        if let Ok(json) = serde_json::to_string(&event) {
            if sender.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    }
}
