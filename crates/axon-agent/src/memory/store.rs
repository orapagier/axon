use super::long_term::{LongTermMemory, MemoryEntry};
use super::short_term::{ShortTermMemory, ShortTermRow};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;

pub struct MemoryStore {
    pub short: ShortTermMemory,
    pub long: LongTermMemory,
}

impl MemoryStore {
    pub fn new(
        db: Arc<Pool<SqliteConnectionManager>>,
        max_short: usize,
        voyage_key: Option<String>,
    ) -> Self {
        MemoryStore {
            short: ShortTermMemory::new(Arc::clone(&db), max_short),
            long: LongTermMemory::new(db, voyage_key),
        }
    }
    pub fn add_user(&self, s: &str, t: &str) -> anyhow::Result<()> {
        self.short.store_message(s, "user", t, None)
    }
    pub fn add_assistant(&self, s: &str, t: &str) -> anyhow::Result<()> {
        self.short.store_message(s, "assistant", t, None)
    }
    /// Store a user turn, trimming this session to `cap` most-recent messages.
    pub fn add_user_capped(&self, s: &str, t: &str, cap: usize) -> anyhow::Result<()> {
        self.short.store_message_capped(s, "user", t, None, cap)
    }
    /// Store an assistant turn, trimming this session to `cap` most-recent messages.
    pub fn add_assistant_capped(&self, s: &str, t: &str, cap: usize) -> anyhow::Result<()> {
        self.short.store_message_capped(s, "assistant", t, None, cap)
    }
    pub fn get_session(&self, s: &str) -> anyhow::Result<Vec<ShortTermRow>> {
        self.short.get_messages(s)
    }
    pub fn clear_session(&self, s: &str) -> anyhow::Result<()> {
        self.short.clear_session(s)
    }
    pub async fn remember(&self, c: &str, src: &str, tags: &[&str]) -> anyhow::Result<i64> {
        self.long.store(c, Some(src), tags).await
    }
    pub async fn search(
        &self,
        q: &str,
        k: usize,
        source_exclude: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.long.search(q, k, source_exclude).await
    }
    pub fn forget(&self, id: i64) -> anyhow::Result<()> {
        self.long.delete(id)
    }
    pub fn recent_memories(
        &self,
        n: usize,
        source_exclude: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.long.recent(n, source_exclude)
    }
}
