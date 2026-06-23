use crate::providers::types::Message;
use anyhow::Context;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortTermRow {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub tool_name: Option<String>,
    pub created_at: String,
}

pub struct ShortTermMemory {
    db: Arc<Pool<SqliteConnectionManager>>,
    max_msgs: usize,
}

impl ShortTermMemory {
    pub fn new(db: Arc<Pool<SqliteConnectionManager>>, max_msgs: usize) -> Self {
        ShortTermMemory { db, max_msgs }
    }

    pub fn store_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
    ) -> anyhow::Result<()> {
        self.store_message_capped(session_id, role, content, tool_name, self.max_msgs)
    }

    /// Like `store_message` but trims the session to `cap` most-recent rows
    /// instead of the global `max_msgs`. Lets a caller (e.g. an Axon workflow
    /// node) pick a per-session sliding-window size. `cap` is clamped to >= 1.
    pub fn store_message_capped(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_name: Option<&str>,
        cap: usize,
    ) -> anyhow::Result<()> {
        let cap = cap.max(1);
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "INSERT INTO short_term (session_id,role,content,tool_name) VALUES (?1,?2,?3,?4)",
            rusqlite::params![session_id, role, content, tool_name],
        )?;
        conn.execute(
            "DELETE FROM short_term WHERE session_id=?1 AND id NOT IN (SELECT id FROM short_term WHERE session_id=?1 ORDER BY id DESC LIMIT ?2)",
            rusqlite::params![session_id, cap as i64])?;
        Ok(())
    }

    pub fn get_messages(&self, session_id: &str) -> anyhow::Result<Vec<ShortTermRow>> {
        let conn = self.db.get().context("DB pool")?;
        let mut s = conn.prepare("SELECT id,session_id,role,content,tool_name,created_at FROM short_term WHERE session_id=?1 ORDER BY id ASC")?;
        let rows: Vec<ShortTermRow> = s
            .query_map(rusqlite::params![session_id], |r| {
                Ok(ShortTermRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    role: r.get(2)?,
                    content: r.get(3)?,
                    tool_name: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    }

    pub fn to_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        Ok(self
            .get_messages(session_id)?
            .into_iter()
            .map(|r| match r.role.as_str() {
                "assistant" => Message::assistant(r.content),
                _ => Message::user(r.content),
            })
            .collect())
    }

    /// Same as `to_messages` but returns only the last `limit` rows, so a caller
    /// can feed a bounded sliding window to the model even if the stored history
    /// is longer (e.g. left over from a previous, larger window).
    pub fn to_messages_limited(
        &self,
        session_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Message>> {
        let mut rows = self.get_messages(session_id)?;
        let limit = limit.max(1);
        if rows.len() > limit {
            rows = rows.split_off(rows.len() - limit);
        }
        Ok(rows
            .into_iter()
            .map(|r| match r.role.as_str() {
                "assistant" => Message::assistant(r.content),
                _ => Message::user(r.content),
            })
            .collect())
    }

    pub fn clear_session(&self, session_id: &str) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "DELETE FROM short_term WHERE session_id=?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }
}
