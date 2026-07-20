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
    /// Set by the clients when the message was spoken (mic / wake word / PTT)
    /// and the reply will be read aloud — drives the SPOKEN REPLY system hint.
    #[serde(default)]
    pub voice: bool,
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

/// Cap on persisted trace items per run so a pathological run can't bloat the
/// transcript store.
const MAX_TRACE_ITEMS: usize = 300;

fn push_trace_item(
    traces: &mut std::collections::HashMap<String, Vec<serde_json::Value>>,
    run_id: &str,
    item: serde_json::Value,
) {
    let items = traces.entry(run_id.to_string()).or_default();
    if items.len() < MAX_TRACE_ITEMS {
        items.push(item);
    }
}

/// Mirror the ChatPage trace rendering ({text, color} items) from the event
/// stream so a finished run's reasoning trace can be persisted and rehydrated
/// on reload exactly as it looked live. Returns the run_id when the run just
/// finished (Done/Error) and its accumulated trace should be persisted.
fn tee_trace_event(
    traces: &mut std::collections::HashMap<String, Vec<serde_json::Value>>,
    ev: &AgentEvent,
) -> Option<String> {
    use serde_json::json;
    match ev {
        AgentEvent::Thinking { run_id, text } => {
            // Skip the 4s model-wait heartbeats — they'd dominate the stored
            // trace without adding information after the fact.
            if !text.starts_with("Waiting for the model") {
                push_trace_item(
                    traces,
                    run_id,
                    json!({"text": format!("... {}", text), "color": "#98a6a1"}),
                );
            }
            None
        }
        AgentEvent::Model {
            run_id,
            model,
            iteration,
            duration_ms,
        } => {
            let dur = if *duration_ms > 0 {
                format!(" ({}ms)", duration_ms)
            } else {
                String::new()
            };
            push_trace_item(
                traces,
                run_id,
                json!({"text": format!("Model {} iter {}{}", model, iteration, dur), "color": "#d7e7bc"}),
            );
            None
        }
        AgentEvent::Tools {
            run_id,
            tools,
            tier,
            parallel,
        } => {
            let par = if *parallel { "parallel" } else { "sequential" };
            push_trace_item(
                traces,
                run_id,
                json!({"text": format!("Tools {} -> [{}] {}", tier, tools.join(", "), par), "color": "#b5cbc6"}),
            );
            None
        }
        AgentEvent::ToolStart {
            run_id,
            tool,
            tool_call_id,
        } => {
            push_trace_item(
                traces,
                run_id,
                json!({"id": tool_call_id, "text": format!("Start {}...", tool), "color": "#d9c187"}),
            );
            None
        }
        AgentEvent::ToolEnd {
            run_id,
            tool,
            tool_call_id,
            duration_ms,
            ok,
        } => {
            let text = format!(
                "{} {} {}ms",
                if *ok { "OK" } else { "ERR" },
                tool,
                duration_ms
            );
            let color = if *ok { "#b7d79a" } else { "#e4a1a1" };
            let items = traces.entry(run_id.clone()).or_default();
            if let Some(it) = items
                .iter_mut()
                .find(|i| i.get("id").and_then(|v| v.as_str()) == Some(tool_call_id.as_str()))
            {
                *it = json!({"id": tool_call_id, "text": text, "color": color});
            } else if items.len() < MAX_TRACE_ITEMS {
                items.push(json!({"text": text, "color": color}));
            }
            None
        }
        AgentEvent::MemoryHit { run_id, count } => {
            push_trace_item(
                traces,
                run_id,
                json!({"text": format!("{} memories retrieved", count), "color": "#b5cbc6"}),
            );
            None
        }
        AgentEvent::Done { run_id, .. } | AgentEvent::Error { run_id, .. } => Some(run_id.clone()),
        _ => None,
    }
}

/// Persist a finished run's trace as a `trace` row in the session transcript.
/// The assistant row for the run is always written before Done is emitted, so
/// the trace row lands right after it; the messages endpoint re-orders it in
/// front of the answer for display.
fn persist_trace(state: &AppState, run_id: &str, items: &[serde_json::Value]) {
    if items.is_empty() {
        return;
    }
    let session: Option<String> = state.db.get().ok().and_then(|conn| {
        conn.query_row(
            "SELECT session_id FROM runs WHERE id=?1",
            rusqlite::params![run_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    });
    let Some(session) = session else { return };
    if let Ok(json) = serde_json::to_string(items) {
        let _ = state.memory.add_trace(&session, &json);
    }
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(100);

    // The receiver task takes ownership of `state`; the forward loop below
    // keeps its own handle for trace persistence.
    let fwd_state = state.clone();

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
                    let sid = ctrl
                        .get("session_id")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
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
                context.voice = task_data.voice;

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

    // Tee the event stream into a per-run reasoning trace so the transcript
    // keeps the collapsed "how I got here" block across page reloads.
    let mut traces: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    while let Some(event) = rx.recv().await {
        if let Some(finished_run) = tee_trace_event(&mut traces, &event) {
            if let Some(items) = traces.remove(&finished_run) {
                persist_trace(&fwd_state, &finished_run, &items);
            }
        }
        if let Ok(json) = serde_json::to_string(&event) {
            if sender.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    }
}
