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

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(100);

    tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            let Ok(msg) = msg else { continue };
            let Ok(text) = msg.to_text() else { continue };

            // Try to parse as WsTask, but don't panic if it fails
            if let Ok(task_data) = serde_json::from_str::<WsTask>(text) {
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

                let s2 = state.clone();
                let tx2 = tx.clone();
                let t = task_data.task.clone();
                tokio::spawn(async move {
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
