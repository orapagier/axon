use super::embeddings::Embedder;
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
        embedder: Option<Embedder>,
        min_similarity: f32,
        dedup_similarity: f32,
        vector_scan_limit: usize,
    ) -> Self {
        MemoryStore {
            short: ShortTermMemory::new(Arc::clone(&db), max_short),
            long: LongTermMemory::new(
                db,
                embedder,
                min_similarity,
                dedup_similarity,
                vector_scan_limit,
            ),
        }
    }
    pub fn add_user(&self, s: &str, t: &str) -> anyhow::Result<()> {
        self.short.store_message(s, "user", t, None)
    }
    pub fn add_assistant(&self, s: &str, t: &str) -> anyhow::Result<()> {
        self.short.store_message(s, "assistant", t, None)
    }
    /// Store a run's reasoning trace (JSON array of display items) alongside
    /// the transcript. Trace rows are display-only: `to_messages*` filters
    /// them out so they never reach the model's context.
    pub fn add_trace(&self, s: &str, t: &str) -> anyhow::Result<()> {
        self.short.store_message(s, "trace", t, None)
    }
    /// Store a user turn, trimming this session to `cap` most-recent messages.
    pub fn add_user_capped(&self, s: &str, t: &str, cap: usize) -> anyhow::Result<()> {
        self.short.store_message_capped(s, "user", t, None, cap)
    }
    /// Store an assistant turn, trimming this session to `cap` most-recent messages.
    pub fn add_assistant_capped(&self, s: &str, t: &str, cap: usize) -> anyhow::Result<()> {
        self.short
            .store_message_capped(s, "assistant", t, None, cap)
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
    /// `remember`, but vectorizing only `embed_text`. Used for task results,
    /// where the stored record keeps the request for readability but retrieval
    /// should match on the fact that was learned, not the request's phrasing.
    pub async fn remember_with_embed_text(
        &self,
        c: &str,
        embed_text: &str,
        src: &str,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        self.long
            .store_with_embed_text(c, embed_text, Some(src), tags)
            .await
    }
    /// Store into a node-private long-term partition (Cortex node with
    /// Long-term Memory enabled). Never surfaces in global recall.
    pub async fn remember_scoped(
        &self,
        c: &str,
        scope: &str,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        self.long.store_scoped(c, scope, tags).await
    }
    /// `remember_scoped`, but vectorizing only `embed_text` — see
    /// `remember_with_embed_text`.
    pub async fn remember_scoped_with_embed_text(
        &self,
        c: &str,
        embed_text: &str,
        scope: &str,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        self.long
            .store_scoped_with_embed_text(c, embed_text, scope, tags)
            .await
    }
    pub async fn search(
        &self,
        q: &str,
        k: usize,
        source_exclude: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.long.search(q, k, source_exclude).await
    }
    /// Search ONLY the given node-private long-term partition.
    pub async fn search_scoped(
        &self,
        q: &str,
        k: usize,
        scope: &str,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        self.long.search_scoped(q, k, scope).await
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
