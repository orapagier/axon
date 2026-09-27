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

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    req: axum::extract::Request,
) -> impl IntoResponse {
    // require_auth (auth.rs) validated the `Sec-WebSocket-Protocol:
    // axon-ws.<key>` subprotocol in constant time and inserted the matched
    // ValidatedWsSubproto into request extensions. Echo it here on the 101 —
    // the browser rejects the upgrade unless the server echoes one of the
    // subprotocols it offered, and this is the one and only way a browser can
    // prove to the master key on a WS upgrade (it cannot set an Authorization
    // header on a native WebSocket, and the key must never ride in a URL).
    let matched = req
        .extensions()
        .get::<crate::dashboard::auth::ValidatedWsSubproto>()
        .map(|v| v.0.clone());
    let upgrade = ws.protocols(matched.into_iter().collect::<Vec<String>>());
    upgrade.on_upgrade(move |socket| handle_socket(socket, state))
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

/// Same idea for the outer safety timeout, which *drops* the run future instead
/// of letting it return. Nothing inside a dropped future runs on the way out —
/// neither `finalize()` nor the error path in `run_task_streaming` — so the row
/// has to be closed from here or it sits at `running` until the next restart
/// sweeps it (see `db::recover_stale_state`). Recorded as `failed` to match how
/// the agent loop's own deadline finalizes a timed-out run.
fn mark_run_timed_out(state: &AppState, run_id: &str, secs: u64) {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE runs SET status='failed', result=?2, finished_at=datetime('now') WHERE id=?1 AND status='running'",
            rusqlite::params![run_id, format!("Terminated: exceeded the {secs}s safety timeout")],
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
                    // Backstop only. The agent loop enforces its own
                    // `agent.run_timeout_secs` deadline and finalizes the run row
                    // on the way out, so keep this strictly longer — otherwise the
                    // two fire together and this one wins by dropping the run
                    // future, losing the graceful finalize entirely.
                    let timeout_dur = tokio::time::Duration::from_secs(
                        s2.settings.get_int("agent.run_timeout_secs", 300).max(1) as u64 + 30,
                    );
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
                            mark_run_timed_out(&s2, &run_id, timeout_dur.as_secs());
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

    // Server-wide notifications (scheduler outcomes, watcher hits, background
    // errors) arrive on a separate broadcast channel and are forwarded to this
    // socket interleaved with its own run events. They carry an empty run_id,
    // which is what lets the frontend's run-scoped guard pass them to the bell.
    let mut notifications = fwd_state.notify.subscribe();
    // Disables the broadcast select branch if the hub ever closes, so the socket
    // keeps serving run events instead of spinning on a closed channel.
    let mut notify_open = true;

    loop {
        let event = tokio::select! {
            run_event = rx.recv() => match run_event {
                Some(ev) => ev,
                // The run channel closed: the receiver task is gone and no
                // further chat events can arrive, so this socket is done.
                None => break,
            },
            broadcast = notifications.recv(), if notify_open => match broadcast {
                Ok(ev) => ev,
                // Lagged: this socket fell behind the broadcast buffer. The rows
                // are persisted, and the frontend re-fetches /api/notifications
                // on reconnect, so skipping the missed ones is safe.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("WS notification receiver lagged, skipped {n}");
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    notify_open = false;
                    continue;
                }
            },
        };

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

#[cfg(test)]
mod subproto_tests {
    use axum::extract::{Request, WebSocketUpgrade};
    use axum::http::header;
    use axum::middleware::Next;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;

    #[derive(Clone)]
    struct Marker(String);

    // Mirrors the production pipeline: auth middleware validates the
    // `axon-ws.<key>` subprotocol, stores it in extensions, and the handler
    // reads it back to echo on the 101. The browser aborts the upgrade
    // otherwise, so the echo reaching the wire is the assertion.
    async fn mw(mut req: Request, next: Next) -> Response {
        let offered = req
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|h| h.to_str().ok())
            .map(str::to_string);
        if let Some(v) = offered {
            req.extensions_mut().insert(Marker(v));
        }
        next.run(req).await
    }

    async fn handler(ws: WebSocketUpgrade, req: Request) -> impl IntoResponse {
        let matched = req.extensions().get::<Marker>().map(|m| m.0.clone());
        ws.protocols(matched.into_iter().collect::<Vec<String>>())
            .on_upgrade(|_socket| async {})
    }

    #[tokio::test]
    async fn validated_subprotocol_is_echoed_on_the_101() {
        let app = Router::new()
            .route("/ws", get(handler))
            .layer(axum::middleware::from_fn(mw));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /ws HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
                  Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                  Sec-WebSocket-Protocol: axon-ws.testkey\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = [0u8; 1024];
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
            .await
            .expect("server sent no response")
            .unwrap();
        let head = String::from_utf8_lossy(&buf[..n]).to_lowercase();
        assert!(head.contains("101"), "expected 101, got: {head}");
        assert!(
            head.contains("sec-websocket-protocol: axon-ws.testkey"),
            "subprotocol was not echoed. response head:\n{head}"
        );
        server.abort();
    }
}
