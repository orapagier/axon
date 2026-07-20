//! REST surface for the notification bell.
//!
//! Live updates arrive over the WS broadcast (see `dashboard::ws`); these
//! endpoints cover the cases the socket can't: the initial load, catching up
//! after a reconnect, and mutating read/deleted state.

use super::*;

/// `GET /api/notifications?unread=1&limit=50`
pub async fn get_notifications(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let only_unread = matches!(
        q.get("unread").map(|s| s.as_str()),
        Some("1") | Some("true")
    );
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(50);

    let items = try_json!(state.notify.list(only_unread, limit));
    let unread = try_json!(state.notify.unread_count());
    Json(json!({"ok": true, "notifications": items, "unread": unread}))
}

/// `GET /api/notifications/unread_count` — cheap badge refresh on reconnect.
pub async fn get_notifications_unread_count(State(state): State<AppState>) -> Json<Value> {
    let unread = try_json!(state.notify.unread_count());
    Json(json!({"ok": true, "unread": unread}))
}

/// Pull an optional `id` out of a body that may be absent entirely — "act on
/// all" is expressed as a missing body, a missing key, or an explicit null, and
/// all three must mean the same thing.
fn target_id(body: Option<Json<Value>>) -> Option<i64> {
    body.and_then(|Json(v)| v.get("id").and_then(|v| v.as_i64()))
}

/// `POST /api/notifications/mark_read` `{"id": 12}` — omit `id` to mark all.
pub async fn mark_notifications_read(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> Json<Value> {
    try_json!(state.notify.mark_read(target_id(body)));
    let unread = try_json!(state.notify.unread_count());
    Json(json!({"ok": true, "unread": unread}))
}

/// `DELETE /api/notifications` `{"id": 12}` — omit `id` to clear all.
pub async fn delete_notifications(
    State(state): State<AppState>,
    body: Option<Json<Value>>,
) -> Json<Value> {
    try_json!(state.notify.delete(target_id(body)));
    let unread = try_json!(state.notify.unread_count());
    Json(json!({"ok": true, "unread": unread}))
}
