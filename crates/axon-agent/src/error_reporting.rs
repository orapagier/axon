use crate::state::AppState;

fn normalize_platform(platform: Option<&str>) -> Option<String> {
    let candidate = platform.unwrap_or_default().trim().to_lowercase();
    if matches!(candidate.as_str(), "telegram" | "discord" | "slack") {
        return Some(candidate);
    }
    None
}

fn state_default_platform_fallback() -> String {
    "telegram".to_string()
}

fn resolve_notification_target(
    state: &AppState,
    preferred_chat_id: Option<&str>,
) -> Option<String> {
    if let Some(chat_id) = preferred_chat_id.and_then(|cid| {
        let trimmed = cid.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }) {
        return Some(chat_id);
    }

    let configured_chat_id = state.settings.get_str("watcher.notify_chat_id", "");
    if !configured_chat_id.is_empty() {
        return Some(configured_chat_id);
    }

    let Ok(conn) = state.db.get() else {
        return None;
    };

    conn.query_row(
        "SELECT session_id
         FROM short_term
         WHERE session_id NOT IN ('dashboard', 'workflow', 'owner')
         ORDER BY id DESC
         LIMIT 1",
        [],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

pub async fn send_global_error_notification(
    state: &AppState,
    source: &str,
    summary: &str,
    details: &str,
    preferred_platform: Option<&str>,
    preferred_chat_id: Option<&str>,
) -> Result<(), String> {
    // The dashboard bell gets the error unconditionally, and first: this is the
    // single funnel every background path (agent loop, watcher triage, workflow
    // failures) already reports through, and the messaging send below can bail
    // out entirely when no chat target is configured. Emitting up here is what
    // makes those otherwise-invisible failures reach the operator.
    // Many callers pass only a summary; fall back to it so the bell never shows
    // a titled notification with an empty body.
    let body = if details.trim().is_empty() {
        summary.trim()
    } else {
        details.trim()
    };
    state
        .notify
        .emit(source.trim(), "error", summary.trim(), body)
        .await;

    let Some(chat_id) = resolve_notification_target(state, preferred_chat_id) else {
        return Err("No notification target configured".to_string());
    };

    let configured_platform = state
        .settings
        .get_str("watcher.notify_platform", "telegram")
        .to_lowercase();
    let mut platform = normalize_platform(preferred_platform).unwrap_or_default();
    if !matches!(platform.as_str(), "telegram" | "discord" | "slack") {
        platform = configured_platform;
    }
    if !matches!(platform.as_str(), "telegram" | "discord" | "slack") {
        platform = state_default_platform_fallback();
    }

    let mut message = format!(
        "[GLOBAL ERROR]\nSource: {}\nSummary: {}",
        source.trim(),
        summary.trim()
    );
    let details_trimmed = details.trim();
    if !details_trimmed.is_empty() {
        message.push_str("\nDetails:\n");
        message.push_str(details_trimmed);
    }

    state
        .messaging
        .send_to_active_platform(&chat_id, &message, &platform)
        .await
        .map(|_| ())
        .map_err(|e| format!("Failed to send global error notification: {}", e))
}
