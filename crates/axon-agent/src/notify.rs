//! Universal notification hub.
//!
//! A single fan-out point for anything the server wants the operator to see in
//! the dashboard bell: scheduler job outcomes, watcher hits, runtime/router
//! errors. Each notification is persisted to the `notifications` table (so it
//! survives reloads and reaches clients that connect later) and broadcast live
//! to every connected WS client via a `tokio::broadcast` channel.
//!
//! The wire format reuses [`crate::agent::AgentEvent::Notification`] with an
//! empty `run_id` so the ChatPage's run-scoped guard doesn't drop broadcasts.
//!
//! The hub is additive: it never replaces delivery to Telegram/Discord/Slack —
//! those code paths keep their existing `send_text` calls and additionally call
//! [`NotifyHub::emit`] so the dashboard sees the same outcome.

use crate::agent::AgentEvent;
use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

/// How many notifications each WS receiver keeps in its local queue before the
/// oldest get dropped. Broadcasts are best-effort — persistence is the source of
/// truth, so a lagging client just re-fetches `/api/notifications` on reconnect.
const BROADCAST_CAPACITY: usize = 256;

/// A row read back from the `notifications` table, shaped for the dashboard API
/// and the frontend bell store.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationRow {
    pub id: i64,
    pub source: String,
    pub level: String,
    pub title: String,
    pub message: String,
    pub read: bool,
    pub created_at: String,
}

pub struct NotifyHub {
    db: Arc<Pool<SqliteConnectionManager>>,
    tx: broadcast::Sender<AgentEvent>,
}

impl NotifyHub {
    pub fn new(db: Arc<Pool<SqliteConnectionManager>>) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Arc::new(NotifyHub { db, tx })
    }

    /// Subscribe to the live broadcast. Each WS socket takes one receiver in
    /// `dashboard::ws::handle_socket` and forwards events to the browser.
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.tx.subscribe()
    }

    /// Persist a notification and broadcast it to all connected clients.
    ///
    /// `source` is a dotted category (`scheduler`, `watcher`, `agent.runtime`,
    /// `agent.router`, `workflow`, `system`). `level` is `info` | `warning` |
    /// `error` and maps directly to the frontend's ok/severity flag. Failures
    /// here only log — a notification subsystem must never take down the work
    /// that was trying to notify.
    pub async fn emit(&self, source: &str, level: &str, title: &str, message: &str) {
        let message_trimmed = message.trim();
        if message_trimmed.is_empty() && title.trim().is_empty() {
            return;
        }
        let db = Arc::clone(&self.db);
        let source = source.to_string();
        let level = level.to_string();
        let title = title.to_string();
        let message = message_trimmed.to_string();
        // The blocking closure below takes ownership of these, so keep copies
        // for the broadcast that follows it.
        let (ev_level, ev_title, ev_message) = (level.clone(), title.clone(), message.clone());

        // r2d2 acquisition + rusqlite are blocking — run on the blocking pool.
        let persist_result = tokio::task::spawn_blocking(move || {
            let conn = db.get().context("notify: DB pool")?;
            conn.execute(
                "INSERT INTO notifications (source, level, title, message) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![source, level, title, message],
            )
            .context("notify: insert")?;
            Ok::<(), anyhow::Error>(())
        })
        .await;

        match persist_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!("NotifyHub persist failed: {e}"),
            Err(e) => tracing::warn!("NotifyHub persist task panicked: {e}"),
        }

        // Broadcast is best-effort: no receivers / a lagging receiver is fine —
        // the row is already persisted and will show on the next fetch.
        let _ = self.tx.send(AgentEvent::Notification {
            run_id: String::new(),
            level: ev_level,
            title: ev_title,
            message: ev_message,
        });
    }

    /// List notifications, newest first. When `only_unread` is set, skip read
    /// rows. `limit` is capped to keep the payload sane.
    pub fn list(&self, only_unread: bool, limit: i64) -> Result<Vec<NotificationRow>> {
        let limit = limit.clamp(1, 500);
        let conn = self.db.get().context("notify: DB pool")?;
        let sql = if only_unread {
            "SELECT id, source, level, title, message, read, created_at
             FROM notifications WHERE read = 0
             ORDER BY created_at DESC, id DESC LIMIT ?1"
        } else {
            "SELECT id, source, level, title, message, read, created_at
             FROM notifications
             ORDER BY created_at DESC, id DESC LIMIT ?1"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params![limit], |r| {
                Ok(NotificationRow {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    level: r.get(2)?,
                    title: r.get(3)?,
                    message: r.get(4)?,
                    read: r.get::<_, i64>(5)? != 0,
                    created_at: r.get(6)?,
                })
            })?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    /// Count unread rows — used by the badge on reconnect.
    pub fn unread_count(&self) -> Result<i64> {
        let conn = self.db.get().context("notify: DB pool")?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM notifications WHERE read = 0",
            [],
            |r| r.get(0),
        )?;
        Ok(count)
    }

    /// Mark one notification (or all, when `id` is `None`) as read.
    pub fn mark_read(&self, id: Option<i64>) -> Result<()> {
        let conn = self.db.get().context("notify: DB pool")?;
        match id {
            Some(id) => {
                conn.execute(
                    "UPDATE notifications SET read = 1 WHERE id = ?1",
                    rusqlite::params![id],
                )?;
            }
            None => {
                conn.execute("UPDATE notifications SET read = 1", [])?;
            }
        }
        Ok(())
    }

    /// Delete one notification (or all, when `id` is `None`).
    pub fn delete(&self, id: Option<i64>) -> Result<()> {
        let conn = self.db.get().context("notify: DB pool")?;
        match id {
            Some(id) => {
                conn.execute(
                    "DELETE FROM notifications WHERE id = ?1",
                    rusqlite::params![id],
                )?;
            }
            None => {
                conn.execute("DELETE FROM notifications", [])?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    /// A hub over a single-connection in-memory pool. `max_size(1)` matters:
    /// every r2d2 connection to `:memory:` would otherwise get its own empty
    /// database, so capping the pool at one keeps all calls on the same DB.
    fn fresh_hub() -> Arc<NotifyHub> {
        let manager = SqliteConnectionManager::memory();
        let pool = Pool::builder().max_size(1).build(manager).unwrap();
        {
            let conn = pool.get().unwrap();
            db::init(&conn).unwrap();
        }
        NotifyHub::new(Arc::new(pool))
    }

    #[tokio::test]
    async fn emit_persists_and_broadcasts() {
        let hub = fresh_hub();
        let mut rx = hub.subscribe();
        hub.emit("scheduler", "warning", "Daily Summary", "Output discarded")
            .await;

        // Broadcast reached a live subscriber.
        let ev = rx.recv().await.expect("broadcast received");
        match ev {
            AgentEvent::Notification {
                run_id,
                level,
                title,
                message,
            } => {
                assert!(run_id.is_empty(), "broadcast run_id must be empty");
                assert_eq!(level, "warning");
                assert_eq!(title, "Daily Summary");
                assert_eq!(message, "Output discarded");
            }
            other => panic!("expected Notification, got {other:?}"),
        }

        // Persisted.
        let rows = hub.list(false, 50).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "scheduler");
        assert_eq!(rows[0].level, "warning");
        assert!(!rows[0].read);

        assert_eq!(hub.unread_count().unwrap(), 1);
        hub.mark_read(Some(rows[0].id)).unwrap();
        assert_eq!(hub.unread_count().unwrap(), 0);

        let unread = hub.list(true, 50).unwrap();
        assert!(unread.is_empty());
    }

    #[tokio::test]
    async fn emit_with_empty_message_is_dropped() {
        let hub = fresh_hub();
        let mut rx = hub.subscribe();
        hub.emit("system", "info", "", "   \n   ").await;
        // Nothing persisted, nothing broadcast.
        assert!(hub.list(false, 50).unwrap().is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn delete_one_and_all() {
        let hub = fresh_hub();
        hub.emit("scheduler", "info", "a", "first").await;
        hub.emit("scheduler", "info", "b", "second").await;
        let rows = hub.list(false, 50).unwrap();
        assert_eq!(rows.len(), 2);

        hub.delete(Some(rows[1].id)).unwrap();
        assert_eq!(hub.list(false, 50).unwrap().len(), 1);

        hub.delete(None).unwrap();
        assert_eq!(hub.list(false, 50).unwrap().len(), 0);
    }

    #[tokio::test]
    async fn emit_without_subscribers_still_persists() {
        // Nobody is listening on the broadcast — the row must still land, since
        // persistence (not the broadcast) is the source of truth.
        let hub = fresh_hub();
        hub.emit("watcher", "info", "Inbox", "3 new").await;
        assert_eq!(hub.list(false, 50).unwrap().len(), 1);
    }
}
