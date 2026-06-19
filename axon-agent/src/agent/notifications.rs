//! Error/alert notification dispatch, extracted from `agent::r#loop`.
//!
//! When the agent loop hits a router alert or a runtime error it either streams
//! a `Notification` event to a connected client (when `tx` is present) or, for
//! background runs, falls back to the global error-notification path so the user
//! still hears about it out-of-band.

use crate::agent::AgentEvent;
use crate::error_reporting::send_global_error_notification;
use crate::router::format_alerts;
use crate::state::AppState;
use tokio::sync::mpsc;

pub(crate) async fn dispatch_router_alert_notifications(
    alerts: &[crate::router::RouterAlert],
    state: &AppState,
    tx: Option<&mpsc::Sender<AgentEvent>>,
    run_id: &str,
    platform: &str,
    chat_id: Option<&str>,
    summary: &str,
) {
    if alerts.is_empty() {
        return;
    }
    let alert_text = format_alerts(alerts);
    if alert_text.trim().is_empty() {
        return;
    }

    if let Some(t) = tx {
        let _ = t
            .send(AgentEvent::Notification {
                run_id: run_id.to_string(),
                level: "error".to_string(),
                title: summary.to_string(),
                message: alert_text,
            })
            .await;
        return;
    }
    if let Err(e) = send_global_error_notification(
        state,
        "agent.router",
        summary,
        &alert_text,
        Some(platform),
        chat_id,
    )
    .await
    {
        tracing::warn!("dispatch_router_alert_notifications: {}", e);
    }
}

pub(crate) async fn dispatch_global_error_notification_event(
    state: &AppState,
    tx: Option<&mpsc::Sender<AgentEvent>>,
    run_id: &str,
    platform: &str,
    chat_id: Option<&str>,
    summary: &str,
    details: &str,
) {
    let details_trimmed = details.trim();
    if details_trimmed.is_empty() {
        return;
    }

    if let Some(t) = tx {
        let _ = t
            .send(AgentEvent::Notification {
                run_id: run_id.to_string(),
                level: "error".to_string(),
                title: summary.to_string(),
                message: details_trimmed.to_string(),
            })
            .await;
        return;
    }
    if let Err(e) = send_global_error_notification(
        state,
        "agent.runtime",
        summary,
        details_trimmed,
        Some(platform),
        chat_id,
    )
    .await
    {
        tracing::warn!("dispatch_global_error_notification_event: {}", e);
    }
}
