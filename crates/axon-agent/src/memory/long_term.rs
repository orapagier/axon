use super::embeddings::{bytes_to_vec, cosine_similarity, vec_to_bytes, Embedder};
use anyhow::Context;
use qdrant_client::qdrant::PointStruct;
use qdrant_client::Qdrant;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub content: String,
    pub source: Option<String>,
    pub tags: Vec<String>,
    pub created_at: String,
    pub score: Option<f32>,
}

/// A row in the running for recall, before ranking decides what survives.
struct Candidate {
    id: i64,
    content: String,
    source: Option<String>,
    tags: Option<String>,
    created_at: String,
    /// True when FTS surfaced this row, i.e. it shares wording with the query.
    /// Rows whose vector can't be compared (none stored, or embedded under a
    /// previous model and still awaiting the re-embed sweep) survive ranking
    /// only when this holds: lexical overlap is the one piece of relevance
    /// evidence left once semantic scoring is unavailable.
    lexical: bool,
}

/// Build an FTS5 OR-query from a free-text task string, or `None` when the
/// string carries no usable term. Callers must skip the MATCH entirely on
/// `None` — FTS5 errors on an empty match expression.
fn fts_or_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| w.replace('"', "").trim().to_string())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

pub struct LongTermMemory {
    db: Arc<Pool<SqliteConnectionManager>>,
    embedder: Option<Embedder>,
    qdrant: Option<Qdrant>,
    collection_name: String,
    /// Minimum cosine similarity a scored hit must reach to be recalled at all.
    /// Below it, recall returns nothing rather than padding the agent's context
    /// with loosely-related rows: an irrelevant memory is worse than no memory,
    /// because the model reads whatever is in the block as signal.
    min_similarity: f32,
    /// Cosine similarity at or above which a new memory counts as a repeat of an
    /// existing one from the same source and refreshes it in place instead of
    /// inserting. Bounds the unbounded row growth a recurring scheduled task
    /// otherwise produces. 0 disables dedup.
    dedup_similarity: f32,
    /// How many recent embedded rows to cosine-scan as semantic candidates on
    /// each recall, on top of whatever FTS matched. 0 falls back to FTS-only
    /// candidate selection. Bounds the cost of semantic recall on a large store.
    vector_scan_limit: usize,
}

impl LongTermMemory {
    pub fn new(
        db: Arc<Pool<SqliteConnectionManager>>,
        embedder: Option<Embedder>,
        min_similarity: f32,
        dedup_similarity: f32,
        vector_scan_limit: usize,
    ) -> Self {
        let qdrant = std::env::var("QDRANT_URL").ok().and_then(|url| {
            let mut builder = Qdrant::from_url(&url);
            if let Ok(api_key) = std::env::var("QDRANT_API_KEY") {
                builder = builder.api_key(api_key);
            }
            builder.build().ok()
        });

        if qdrant.is_some() {
            tracing::info!("Qdrant Cloud integration enabled for LongTermMemory");
        }

        LongTermMemory {
            db,
            embedder,
            qdrant,
            collection_name: "axon_memory".to_string(),
            min_similarity: min_similarity.clamp(0.0, 1.0),
            dedup_similarity: dedup_similarity.clamp(0.0, 1.0),
            vector_scan_limit,
        }
    }

    /// Embed `embed_text` (when an embedder is configured) and insert one row;
    /// returns the row id plus the raw embedding bytes for an optional Qdrant
    /// mirror.
    ///
    /// `embed_text` is what gets vectorized; `content` is what gets stored and
    /// later read back into the agent's context. They differ for task results,
    /// where storing "Task: … / Result: …" keeps the exchange readable but
    /// embedding the request phrasing alongside the fact would make retrieval
    /// match on how a task happened to be worded rather than on what was learned.
    ///
    /// When the new vector is a near-duplicate of an existing row from the same
    /// source (see `dedup_similarity`), that row is refreshed in place and its
    /// id returned instead of inserting. A daily scheduled job then keeps one
    /// current memory rather than accumulating a near-identical row per run
    /// forever, all competing for the same `top_k` recall slots.
    async fn insert_row(
        &self,
        content: &str,
        embed_text: &str,
        source: Option<&str>,
        tags: &[&str],
    ) -> anyhow::Result<(i64, Option<Vec<u8>>)> {
        let emb_vec: Option<Vec<f32>> = if let Some(e) = &self.embedder {
            e.embed_one(embed_text)
                .await
                .ok()
                .filter(|v| !v.is_empty())
        } else {
            None
        };
        let emb: Option<Vec<u8>> = emb_vec.as_ref().map(|v| vec_to_bytes(v));
        // Tag the vector with the model that produced it — embeddings from a
        // different provider/model live in another space and must never be
        // cosine-compared against this one.
        let emb_model: Option<&str> = emb
            .as_ref()
            .and_then(|_| self.embedder.as_ref().map(|e| e.model_id()));
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".into());

        if self.dedup_similarity > 0.0 {
            if let (Some(v), Some(model)) = (emb_vec.as_ref(), emb_model) {
                if let Some(dup_id) = self.find_near_duplicate(v, source, model)? {
                    let conn = self.db.get().context("DB pool")?;
                    conn.execute(
                        "UPDATE long_term SET content=?1, embedding=?2, tags=?3, embedding_model=?4, created_at=datetime('now') WHERE id=?5",
                        rusqlite::params![content, emb, tags_json, model, dup_id],
                    )?;
                    tracing::debug!("Memory dedup: refreshed row {dup_id} in place");
                    return Ok((dup_id, emb));
                }
            }
        }

        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "INSERT INTO long_term (content,embedding,source,tags,embedding_model) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![content, emb, source, tags_json, emb_model],
        )?;
        Ok((conn.last_insert_rowid(), emb))
    }

    /// Highest-scoring recent row from the same source whose vector is directly
    /// comparable (same embedding model) and at/above the dedup threshold.
    /// Bounded to the newest 200 rows of that source so a large store doesn't
    /// turn every write into a full scan.
    fn find_near_duplicate(
        &self,
        vec: &[f32],
        source: Option<&str>,
        model: &str,
    ) -> anyhow::Result<Option<i64>> {
        // Sourceless rows are ad-hoc notes a human asked to keep, not
        // machine-generated repeats — never silently fold two of them together.
        let source = match source {
            Some(s) => s,
            None => return Ok(None),
        };
        let conn = self.db.get().context("DB pool")?;
        let mut s = conn.prepare(
            "SELECT id, embedding FROM long_term
             WHERE source = ?1 AND embedding IS NOT NULL AND embedding_model = ?2
             ORDER BY id DESC LIMIT 200",
        )?;
        let best = s
            .query_map(rusqlite::params![source, model], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .filter_map(|r| r.ok())
            .map(|(id, b)| (id, cosine_similarity(vec, &bytes_to_vec(&b))))
            .filter(|(_, score)| *score >= self.dedup_similarity)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, _)| id);
        Ok(best)
    }

    /// Store a memory in a node-private partition (`source` = the node's scope,
    /// e.g. "wf:{workflow_id}:node:{node_id}"). Deliberately NOT mirrored to
    /// Qdrant: the Qdrant collection backs GLOBAL recall only and has no cheap
    /// prefix filter to keep partitions out, so scoped recall runs entirely on
    /// the SQLite path (`search_scoped`).
    pub async fn store_scoped(
        &self,
        content: &str,
        scope: &str,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        Ok(self.insert_row(content, content, Some(scope), tags).await?.0)
    }

    /// `store_scoped`, but vectorizing only `embed_text` — see `insert_row`.
    pub async fn store_scoped_with_embed_text(
        &self,
        content: &str,
        embed_text: &str,
        scope: &str,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        Ok(self
            .insert_row(content, embed_text, Some(scope), tags)
            .await?
            .0)
    }

    pub async fn store(
        &self,
        content: &str,
        source: Option<&str>,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        self.store_with_embed_text(content, content, source, tags)
            .await
    }

    /// `store`, but vectorizing `embed_text` instead of the stored `content`.
    /// Lets a caller keep a readable record while retrieval matches only on the
    /// part that carries the fact — see `insert_row`.
    pub async fn store_with_embed_text(
        &self,
        content: &str,
        embed_text: &str,
        source: Option<&str>,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        let (id, emb) = self.insert_row(content, embed_text, source, tags).await?;

        if let Some(q_client) = &self.qdrant {
            if let Some(e_bytes) = &emb {
                let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".into());
                let vec = bytes_to_vec(e_bytes);
                use qdrant_client::qdrant::Value;
                let mut payload: std::collections::HashMap<String, Value> =
                    std::collections::HashMap::new();
                payload.insert("content".to_string(), Value::from(content.to_string()));
                if let Some(s) = source {
                    payload.insert("source".to_string(), Value::from(s.to_string()));
                }
                payload.insert("tags".to_string(), Value::from(tags_json));
                // Mirror the row's timestamp so the Qdrant read path can render
                // recency without a SQLite round-trip. Same shape SQLite's
                // datetime('now') default produces.
                payload.insert(
                    "created_at".to_string(),
                    Value::from(
                        chrono::Utc::now()
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string(),
                    ),
                );

                // Upsert is by id, so a dedup hit (which returns the existing
                // row's id) refreshes that point rather than adding a new one.
                let point = PointStruct::new(id as u64, vec, payload);
                use qdrant_client::qdrant::UpsertPointsBuilder;
                let upsert_req = UpsertPointsBuilder::new(&self.collection_name, vec![point])
                    .wait(false)
                    .build();
                // Fire-and-forget: SQLite already persisted, Qdrant syncs in background
                let q = q_client.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        q.upsert_points(upsert_req),
                    )
                    .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(err)) => tracing::warn!(
                            "Qdrant background sync failed for memory {}: {}",
                            id,
                            err
                        ),
                        Err(_) => tracing::warn!(
                            "Qdrant background sync timed out for memory {} (3s)",
                            id
                        ),
                    }
                });
            }
        }

        Ok(id)
    }

    pub async fn chunk_and_store(
        &self,
        content: &str,
        source: Option<&str>,
        tags: &[&str],
        chunk_size: usize,
    ) -> anyhow::Result<Vec<i64>> {
        let mut ids = Vec::new();
        // Basic semantic-aware chunking: split by paragraphs, then hard cutoff if still too long
        let paragraphs: Vec<&str> = content.split("\n\n").collect();
        let mut current_chunk = String::new();

        for p in paragraphs {
            if current_chunk.len() + p.len() > chunk_size && !current_chunk.is_empty() {
                if let Ok(id) = self.store(current_chunk.trim(), source, tags).await {
                    ids.push(id);
                }
                current_chunk.clear();
            }
            if p.len() > chunk_size {
                // If a single paragraph is massive, chunk it by characters
                let chars: Vec<char> = p.chars().collect();
                for chunk in chars.chunks(chunk_size) {
                    let s: String = chunk.iter().collect();
                    if let Ok(id) = self.store(s.trim(), source, tags).await {
                        ids.push(id);
                    }
                }
            } else {
                current_chunk.push_str(p);
                current_chunk.push_str("\n\n");
            }
        }

        if !current_chunk.trim().is_empty() {
            if let Ok(id) = self.store(current_chunk.trim(), source, tags).await {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    /// Embed a query for recall, or `None` when no embedder is configured or the
    /// provider call fails — recall then degrades to lexical matching rather
    /// than failing.
    async fn embed_query(&self, query: &str) -> Option<Vec<f32>> {
        let embedder = self.embedder.as_ref()?;
        match embedder.embed_one(query).await {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("Query embedding failed, recall stays lexical: {}", e);
                None
            }
        }
    }

    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
        source_exclude: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // Embed the query exactly once and reuse it for the Qdrant lookup, the
        // SQLite vector scan and final ranking. Embedding calls are billed and
        // rate-limited, and these three consumers would otherwise make three of
        // them for a single recall.
        let qv = self.embed_query(query).await;

        if let (Some(q_client), Some(qv)) = (&self.qdrant, qv.clone()) {
            // (Nesting below is vestigial from the three `if let`s this single
            // binding replaced; kept to avoid re-indenting the Qdrant block.)
            {
                {
                    {
                        use qdrant_client::qdrant::{
                            Condition, FieldCondition, Filter, Match, SearchPointsBuilder,
                        };

                        let filter = source_exclude.map(|exc| Filter {
                            must_not: vec![Condition {
                                condition_one_of: Some(
                                    qdrant_client::qdrant::condition::ConditionOneOf::Field(
                                        FieldCondition {
                                            key: "source".to_string(),
                                            r#match: Some(Match {
                                                match_value: Some(qdrant_client::qdrant::r#match::MatchValue::Keyword(exc.to_string())),
                                            }),
                                            ..Default::default()
                                        },
                                    ),
                                ),
                            }],
                            ..Default::default()
                        });

                        let search_req =
                            SearchPointsBuilder::new(&self.collection_name, qv, top_k as u64)
                                .with_payload(true)
                                .filter(filter.unwrap_or_default())
                                .build();

                        // Short timeout — fall back to SQLite if Qdrant is slow
                        let search_result = tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            q_client.search_points(search_req),
                        )
                        .await;

                        match search_result {
                            Ok(Ok(res)) => {
                                let mut entries = Vec::new();
                                for point in res.result {
                                    let id = match point.id {
                                        Some(qdrant_client::qdrant::PointId {
                                            point_id_options:
                                                Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(
                                                    n,
                                                )),
                                        }) => n as i64,
                                        _ => 0,
                                    };
                                    let content = point
                                        .payload
                                        .get("content")
                                        .and_then(|v| v.kind.as_ref())
                                        .and_then(|k| match k {
                                            qdrant_client::qdrant::value::Kind::StringValue(s) => {
                                                Some(s.clone())
                                            }
                                            _ => None,
                                        })
                                        .unwrap_or_default();
                                    let source = point
                                        .payload
                                        .get("source")
                                        .and_then(|v| v.kind.as_ref())
                                        .and_then(|k| match k {
                                            qdrant_client::qdrant::value::Kind::StringValue(s) => {
                                                Some(s.clone())
                                            }
                                            _ => None,
                                        });
                                    let tags_json = point
                                        .payload
                                        .get("tags")
                                        .and_then(|v| v.kind.as_ref())
                                        .and_then(|k| match k {
                                            qdrant_client::qdrant::value::Kind::StringValue(s) => {
                                                Some(s.clone())
                                            }
                                            _ => None,
                                        })
                                        .unwrap_or_else(|| "[]".to_string());
                                    let created_at = point
                                        .payload
                                        .get("created_at")
                                        .and_then(|v| v.kind.as_ref())
                                        .and_then(|k| match k {
                                            qdrant_client::qdrant::value::Kind::StringValue(s) => {
                                                Some(s.clone())
                                            }
                                            _ => None,
                                        })
                                        .unwrap_or_default();
                                    let tags = serde_json::from_str(&tags_json).unwrap_or_default();

                                    entries.push(MemoryEntry {
                                        id,
                                        content,
                                        source,
                                        tags,
                                        created_at,
                                        score: Some(point.score),
                                    });
                                }
                                // Same relevance floor the SQLite path applies —
                                // a weak vector match is still a weak match.
                                entries.retain(|e| e.score.unwrap_or(0.0) >= self.min_similarity);
                                // SQLite is the source of truth. Drop hits whose
                                // row is gone (including points orphaned by
                                // deletes that predate Qdrant-aware `delete`)
                                // and backfill timestamps for points written
                                // before created_at was mirrored into payloads.
                                if let Err(e) = self.reconcile_qdrant_hits(&mut entries) {
                                    tracing::warn!("Qdrant hit reconciliation failed: {}", e);
                                }
                                if !entries.is_empty() {
                                    return Ok(entries);
                                }
                            }
                            Ok(Err(e)) => tracing::warn!(
                                "Qdrant search failed: {}, falling back to SQLite",
                                e
                            ),
                            Err(_) => tracing::warn!(
                                "Qdrant search timed out (5s), falling back to SQLite"
                            ),
                        }
                    }
                }
            }
        }

        let mut hits: Vec<Candidate> = {
            let conn = self.db.get().context("DB pool")?;
            // No usable FTS term is not the end of the search any more — the
            // vector scan below can still find semantically related rows.
            let fts_q = fts_or_query(query).unwrap_or_default();
            if fts_q.is_empty() {
                Vec::new()
            } else {

            // Node-private partitions (source "wf:…", see `store_scoped`) never
            // surface in global recall — only `search_scoped` can read them.
            let sql = if source_exclude.is_some() {
                "SELECT lt.id,lt.content,lt.source,lt.tags,lt.created_at FROM long_term lt JOIN long_term_fts fts ON lt.id=fts.rowid WHERE long_term_fts MATCH ?1 AND (lt.source IS NULL OR (lt.source != ?2 AND lt.source NOT LIKE 'wf:%')) ORDER BY rank LIMIT ?3"
            } else {
                "SELECT lt.id,lt.content,lt.source,lt.tags,lt.created_at FROM long_term lt JOIN long_term_fts fts ON lt.id=fts.rowid WHERE long_term_fts MATCH ?1 AND (lt.source IS NULL OR lt.source NOT LIKE 'wf:%') ORDER BY rank LIMIT ?2"
            };
            let mut s = conn.prepare(sql)?;
            let mapped: Vec<Candidate> =
                if let Some(exc) = source_exclude {
                    s.query_map(rusqlite::params![fts_q, exc, (top_k * 3) as i64], |r| {
                        Ok(Candidate {
                            id: r.get(0)?,
                            content: r.get(1)?,
                            source: r.get(2)?,
                            tags: r.get(3)?,
                            created_at: r.get(4)?,
                            lexical: true,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .collect()
                } else {
                    s.query_map(rusqlite::params![fts_q, (top_k * 3) as i64], |r| {
                        Ok(Candidate {
                            id: r.get(0)?,
                            content: r.get(1)?,
                            source: r.get(2)?,
                            tags: r.get(3)?,
                            created_at: r.get(4)?,
                            lexical: true,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .collect()
                };
            mapped
            }
        };

        self.add_vector_candidates(qv.as_deref(), &mut hits, None, source_exclude, top_k);

        // Deliberately no recency fallback on the global path: when nothing
        // matches lexically OR semantically, recall returns nothing. Falling
        // back to "the newest rows" put unrelated content in front of the model
        // under a "[Relevant memories]" header on essentially every run — the
        // model reads that block as signal, so a wrong memory costs more than
        // an absent one.
        if hits.is_empty() {
            return Ok(vec![]);
        }
        self.rank_candidates(qv.as_deref(), hits, top_k)
    }

    /// Union semantically-similar rows into an existing candidate set, skipping
    /// ids FTS already surfaced. Failures are logged and ignored: a degraded
    /// (lexical-only) recall beats erroring out of the agent's context build.
    fn add_vector_candidates(
        &self,
        qv: Option<&[f32]>,
        hits: &mut Vec<Candidate>,
        scope: Option<&str>,
        source_exclude: Option<&str>,
        top_k: usize,
    ) {
        let embedder = match &self.embedder {
            Some(e) => e,
            None => return,
        };
        if self.vector_scan_limit == 0 {
            return;
        }
        let qv = match qv {
            Some(v) if !v.is_empty() => v,
            _ => return,
        };
        let seen: std::collections::HashSet<i64> = hits.iter().map(|c| c.id).collect();
        let ids = match self.vector_candidate_ids(
            qv,
            embedder.model_id(),
            scope,
            source_exclude,
            top_k * 3,
        ) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Vector candidate scan failed: {}", e);
                return;
            }
        };
        let fresh: Vec<i64> = ids.into_iter().filter(|id| !seen.contains(id)).collect();
        match self.rows_by_id(&fresh) {
            Ok(rows) => hits.extend(rows),
            Err(e) => tracing::warn!("Vector candidate load failed: {}", e),
        }
    }

    /// Search ONLY memories stored under the given node-private scope (an Axon
    /// Cortex node's long-term partition). SQLite-only by design — scoped rows
    /// are never mirrored to Qdrant (see `store_scoped`).
    pub async fn search_scoped(
        &self,
        query: &str,
        top_k: usize,
        scope: &str,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        // One embedding, shared by the vector scan and ranking below.
        let qv = self.embed_query(query).await;

        // A query with no usable FTS term skips the MATCH entirely (FTS5 errors
        // on an empty expression); the vector scan and the scoped recency read
        // below can both still produce candidates.
        let mut hits: Vec<Candidate> = match fts_or_query(query) {
            None => Vec::new(),
            Some(fts_q) => {
                let conn = self.db.get().context("DB pool")?;
                let mut s = conn.prepare(
                    "SELECT lt.id,lt.content,lt.source,lt.tags,lt.created_at FROM long_term lt JOIN long_term_fts fts ON lt.id=fts.rowid WHERE long_term_fts MATCH ?1 AND lt.source = ?2 ORDER BY rank LIMIT ?3",
                )?;
                let mapped: Vec<Candidate> = s
                    .query_map(rusqlite::params![fts_q, scope, (top_k * 3) as i64], |r| {
                        Ok(Candidate {
                            id: r.get(0)?,
                            content: r.get(1)?,
                            source: r.get(2)?,
                            tags: r.get(3)?,
                            created_at: r.get(4)?,
                            lexical: true,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                mapped
            }
        };

        self.add_vector_candidates(qv.as_deref(), &mut hits, Some(scope), None, top_k);

        // Unlike global recall, the scoped path keeps a recency fallback: this
        // partition IS the node's own memory, so its newest entries are the
        // right context when nothing else matches. These carry `lexical: true`
        // so ranking treats them as the deliberate fallback they are rather than
        // dropping them for scoring below the floor.
        if hits.is_empty() {
            let conn = self.db.get().context("DB pool")?;
            let mut s = conn.prepare(
                "SELECT id,content,source,tags,created_at FROM long_term WHERE source = ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let mapped: Vec<Candidate> = s
                .query_map(rusqlite::params![scope, (top_k * 2) as i64], |r| {
                    Ok(Candidate {
                        id: r.get(0)?,
                        content: r.get(1)?,
                        source: r.get(2)?,
                        tags: r.get(3)?,
                        created_at: r.get(4)?,
                        lexical: true,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            hits = mapped;
        }
        if hits.is_empty() {
            return Ok(vec![]);
        }
        self.rank_candidates(qv.as_deref(), hits, top_k)
    }

    /// Ids of the strongest semantic matches among rows carrying a vector in the
    /// active embedding space, scanning the newest `vector_scan_limit` rows.
    ///
    /// This is what makes SQLite recall actually semantic. FTS can only surface
    /// memories sharing a literal token with the query, so ranking FTS output by
    /// cosine — all this store used to do — caps recall at keyword matching no
    /// matter how good the embeddings are: a memory that is exactly on point but
    /// worded differently was simply unreachable. These ids get unioned with the
    /// FTS hits before ranking.
    ///
    /// Reads (id, embedding) only; content for the survivors is fetched with the
    /// rest of the candidate set, so a large store never materializes thousands
    /// of content strings per recall.
    fn vector_candidate_ids(
        &self,
        qv: &[f32],
        model: &str,
        scope: Option<&str>,
        source_exclude: Option<&str>,
        want: usize,
    ) -> anyhow::Result<Vec<i64>> {
        if self.vector_scan_limit == 0 || want == 0 {
            return Ok(vec![]);
        }
        let conn = self.db.get().context("DB pool")?;
        let limit = self.vector_scan_limit as i64;
        const BASE: &str = "SELECT id, embedding FROM long_term \
                            WHERE embedding IS NOT NULL AND embedding_model = ?1";

        let rows: Vec<(i64, Vec<u8>)> = match (scope, source_exclude) {
            (Some(sc), _) => {
                let mut s =
                    conn.prepare(&format!("{BASE} AND source = ?2 ORDER BY id DESC LIMIT ?3"))?;
                let v = s
                    .query_map(rusqlite::params![model, sc, limit], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                v
            }
            (None, Some(exc)) => {
                let mut s = conn.prepare(&format!(
                    "{BASE} AND (source IS NULL OR (source != ?2 AND source NOT LIKE 'wf:%')) \
                     ORDER BY id DESC LIMIT ?3"
                ))?;
                let v = s
                    .query_map(rusqlite::params![model, exc, limit], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                v
            }
            (None, None) => {
                let mut s = conn.prepare(&format!(
                    "{BASE} AND (source IS NULL OR source NOT LIKE 'wf:%') \
                     ORDER BY id DESC LIMIT ?2"
                ))?;
                let v = s
                    .query_map(rusqlite::params![model, limit], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                v
            }
        };

        let mut scored: Vec<(i64, f32)> = rows
            .into_iter()
            .map(|(id, b)| (id, cosine_similarity(qv, &bytes_to_vec(&b))))
            .filter(|(_, score)| *score >= self.min_similarity)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(want);
        Ok(scored.into_iter().map(|(id, _)| id).collect())
    }

    /// Load full candidate rows for ids the vector scan selected.
    fn rows_by_id(&self, ids: &[i64]) -> anyhow::Result<Vec<Candidate>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.db.get().context("DB pool")?;
        let ph = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let mut s = conn.prepare(&format!(
            "SELECT id,content,source,tags,created_at FROM long_term WHERE id IN ({})",
            ph
        ))?;
        let v = s
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok(Candidate {
                    id: r.get(0)?,
                    content: r.get(1)?,
                    source: r.get(2)?,
                    tags: r.get(3)?,
                    created_at: r.get(4)?,
                    lexical: false,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(v)
    }

    /// Align Qdrant hits with SQLite, which is the source of truth: drop hits
    /// whose row no longer exists and backfill any missing `created_at`.
    fn reconcile_qdrant_hits(&self, entries: &mut Vec<MemoryEntry>) -> anyhow::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let conn = self.db.get().context("DB pool")?;
        let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
        let ph = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT id, created_at FROM long_term WHERE id IN ({})", ph);
        let mut s = conn.prepare(&sql)?;
        let live: std::collections::HashMap<i64, String> = s
            .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .filter_map(|r| r.ok())
            .collect();
        entries.retain(|e| live.contains_key(&e.id));
        for e in entries.iter_mut() {
            if e.created_at.is_empty() {
                if let Some(ts) = live.get(&e.id) {
                    e.created_at = ts.clone();
                }
            }
        }
        Ok(())
    }

    /// Rank the candidate set by cosine similarity against the query embedding
    /// and cut everything below `min_similarity`, falling back to the incoming
    /// order when no embedder is configured.
    ///
    /// Two deliberate choices here:
    ///
    /// * A row whose vector isn't comparable (none stored, or embedded under a
    ///   previous model and still awaiting the re-embed sweep) scores 0.0, not a
    ///   mid-range placeholder. On a typical cosine distribution a "neutral" 0.5
    ///   outranks genuinely relevant matches, which floated every un-swept row
    ///   to the top of recall after an embedder switch.
    /// * Those unscoreable rows are kept only when FTS surfaced them. Shared
    ///   wording is the one relevance signal still available for them, so
    ///   lexical hits degrade gracefully during a re-embed sweep instead of
    ///   vanishing, while unscoreable rows pulled in by the vector scan — which
    ///   have no evidence at all behind them — are dropped.
    fn rank_candidates(
        &self,
        qv: Option<&[f32]>,
        hits: Vec<Candidate>,
        top_k: usize,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let entry = |c: Candidate, score: Option<f32>| MemoryEntry {
            id: c.id,
            content: c.content,
            source: c.source,
            tags: c
                .tags
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default(),
            created_at: c.created_at,
            score,
        };

        if let Some(embedder) = &self.embedder {
            if let Some(qv) = qv.filter(|v| !v.is_empty()) {
                {
                    let conn = self.db.get().context("DB pool")?;
                    let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
                    let ph = ids
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("?{}", i + 1))
                        .collect::<Vec<_>>()
                        .join(",");
                    // Only compare vectors produced by the active model.
                    let sql = format!("SELECT id,embedding,embedding_model FROM long_term WHERE id IN ({}) AND embedding IS NOT NULL", ph);
                    let mut s = conn.prepare(&sql)?;
                    let cur_model = embedder.model_id().to_string();
                    let emap: std::collections::HashMap<i64, Vec<f32>> = s
                        .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                            Ok((
                                r.get(0)?,
                                r.get::<_, Vec<u8>>(1)?,
                                r.get::<_, Option<String>>(2)?,
                            ))
                        })?
                        .filter_map(|r| r.ok())
                        .filter(|(_, _, model)| model.as_deref() == Some(cur_model.as_str()))
                        .map(|(id, b, _)| (id, bytes_to_vec(&b)))
                        .collect();

                    let (mut scored, unscoreable): (Vec<_>, Vec<_>) = hits
                        .into_iter()
                        .map(|h| {
                            let score = emap.get(&h.id).map(|v| cosine_similarity(qv, v));
                            (score, h)
                        })
                        .partition(|(score, _)| score.is_some());

                    scored.retain(|(score, _)| score.unwrap_or(0.0) >= self.min_similarity);
                    scored.sort_by(|a, b| {
                        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
                    });

                    let ranked = scored
                        .into_iter()
                        .map(|(score, h)| entry(h, score))
                        // Lexical hits we couldn't score keep their FTS order,
                        // behind everything the vectors could vouch for.
                        .chain(
                            unscoreable
                                .into_iter()
                                .filter(|(_, h)| h.lexical)
                                .map(|(_, h)| entry(h, None)),
                        )
                        .take(top_k)
                        .collect();
                    return Ok(ranked);
                }
            }
        }
        Ok(hits
            .into_iter()
            .take(top_k)
            .map(|c| entry(c, None))
            .collect())
    }

    /// Re-embed rows whose stored vector was produced by a different model (or
    /// never produced at all — e.g. stored while no embedder was configured).
    /// Run in the background after startup so a provider/model switch converges
    /// the whole store back into one comparable vector space. Returns the
    /// number of rows re-embedded; stops (and can resume next boot) on the
    /// first provider error.
    pub async fn reembed_stale(&self) -> anyhow::Result<usize> {
        let embedder = match &self.embedder {
            Some(e) => e,
            None => return Ok(0),
        };
        let model = embedder.model_id().to_string();
        let mut total = 0usize;
        loop {
            let batch: Vec<(i64, String)> = {
                let conn = self.db.get().context("DB pool")?;
                let mut s = conn.prepare(
                    "SELECT id, content FROM long_term
                     WHERE embedding IS NULL OR embedding_model IS NULL OR embedding_model != ?1
                     ORDER BY id LIMIT 32",
                )?;
                let rows: Vec<(i64, String)> = s
                    .query_map(rusqlite::params![model], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .filter_map(|r| r.ok())
                    .collect();
                rows
            };
            if batch.is_empty() {
                break;
            }
            let texts: Vec<&str> = batch.iter().map(|(_, c)| c.as_str()).collect();
            let embs = embedder.embed(&texts).await?;
            if embs.len() != batch.len() {
                anyhow::bail!(
                    "re-embed: expected {} vectors, got {}",
                    batch.len(),
                    embs.len()
                );
            }
            {
                let conn = self.db.get().context("DB pool")?;
                for ((id, _), emb) in batch.iter().zip(&embs) {
                    conn.execute(
                        "UPDATE long_term SET embedding=?1, embedding_model=?2 WHERE id=?3",
                        rusqlite::params![vec_to_bytes(emb), model, id],
                    )?;
                }
            }
            total += batch.len();
            // Gentle pacing so a big backlog stays polite to free-tier quotas.
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        Ok(total)
    }

    /// Forget a memory everywhere it is stored. SQLite is authoritative and is
    /// deleted synchronously; the Qdrant mirror is removed on a background task
    /// (same fire-and-forget shape as the upsert in `store_with_embed_text`), so
    /// a slow or unreachable vector DB can't block the caller.
    ///
    /// Deleting the mirror matters: `search` prefers Qdrant and returns early on
    /// a non-empty result, so a point left behind here would keep being recalled
    /// forever after the row it mirrors is gone.
    pub fn delete(&self, id: i64) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute("DELETE FROM long_term WHERE id=?1", rusqlite::params![id])?;

        if let Some(q_client) = &self.qdrant {
            // Only reachable from async call sites, but don't assume a runtime.
            if tokio::runtime::Handle::try_current().is_ok() {
                use qdrant_client::qdrant::{DeletePointsBuilder, PointsIdsList};
                let q = q_client.clone();
                let collection = self.collection_name.clone();
                tokio::spawn(async move {
                    let req = DeletePointsBuilder::new(collection)
                        .points(PointsIdsList {
                            ids: vec![(id as u64).into()],
                        })
                        .wait(false)
                        .build();
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        q.delete_points(req),
                    )
                    .await
                    {
                        Ok(Ok(_)) => {}
                        Ok(Err(err)) => {
                            tracing::warn!("Qdrant delete failed for memory {}: {}", id, err)
                        }
                        Err(_) => {
                            tracing::warn!("Qdrant delete timed out for memory {} (3s)", id)
                        }
                    }
                });
            }
        }
        Ok(())
    }

    pub fn recent(
        &self,
        limit: usize,
        source_exclude: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        let conn = self.db.get().context("DB pool")?;
        let sql = if source_exclude.is_some() {
            "SELECT id,content,source,tags,created_at FROM long_term WHERE source IS NULL OR source != ?1 ORDER BY id DESC LIMIT ?2"
        } else {
            "SELECT id,content,source,tags,created_at FROM long_term ORDER BY id DESC LIMIT ?1"
        };
        let mut s = conn.prepare(sql)?;
        let rows: Vec<(i64, String, Option<String>, Option<String>, String)> =
            if let Some(exc) = source_exclude {
                s.query_map(rusqlite::params![exc, limit as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .filter_map(|r| r.ok())
                .collect()
            } else {
                s.query_map(rusqlite::params![limit as i64], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .filter_map(|r| r.ok())
                .collect()
            };

        Ok(rows
            .into_iter()
            .map(|(id, content, source, tags_json, created_at)| MemoryEntry {
                id,
                content,
                source,
                tags: tags_json
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_default(),
                created_at,
                score: None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (Arc<Pool<SqliteConnectionManager>>, std::path::PathBuf) {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "axon_longterm_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = Pool::new(SqliteConnectionManager::file(&path)).unwrap();
        {
            let conn = pool.get().unwrap();
            crate::db::init(&conn).unwrap();
        }
        (Arc::new(pool), path)
    }

    /// No embedder configured, so recall exercises the FTS path only — which is
    /// exactly the path these tests are about.
    fn mem(db: Arc<Pool<SqliteConnectionManager>>) -> LongTermMemory {
        LongTermMemory::new(db, None, 0.25, 0.95, 2000)
    }

    #[test]
    fn fts_or_query_quotes_and_joins_terms() {
        assert_eq!(
            fts_or_query("tunnel flapped again").as_deref(),
            Some("\"tunnel\" OR \"flapped\" OR \"again\"")
        );
    }

    #[test]
    fn fts_or_query_is_none_when_nothing_usable_remains() {
        // FTS5 errors on an empty MATCH expression, so callers need None rather
        // than an empty string here.
        assert_eq!(fts_or_query(""), None);
        assert_eq!(fts_or_query("   "), None);
        assert_eq!(fts_or_query("\"\""), None);
    }

    #[test]
    fn fts_or_query_strips_embedded_quotes() {
        assert_eq!(
            fts_or_query("say \"hello\"").as_deref(),
            Some("\"say\" OR \"hello\"")
        );
    }

    #[tokio::test]
    async fn search_returns_nothing_rather_than_recent_rows_on_a_miss() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        m.store("the tunnel supervisor restarted twice", Some("task_result"), &[])
            .await
            .unwrap();
        m.store("disk usage on the media volume is 71%", Some("task_result"), &[])
            .await
            .unwrap();

        // Nothing lexically matches. The old recency fallback would have handed
        // back the newest rows under a "[Relevant memories]" header regardless.
        let hits = m.search("photosynthesis chlorophyll", 5, None).await.unwrap();
        assert!(
            hits.is_empty(),
            "unrelated query must not fall back to recent memories, got {hits:?}"
        );

        // A real match still comes back.
        let hits = m.search("tunnel supervisor", 5, None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].content.contains("tunnel supervisor"));
    }

    #[tokio::test]
    async fn search_excludes_the_named_source() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        m.store("nightly backup completed", Some("scheduler"), &[])
            .await
            .unwrap();
        m.store("nightly backup question from chat", Some("task_result"), &[])
            .await
            .unwrap();

        let hits = m.search("nightly backup", 5, Some("scheduler")).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source.as_deref(), Some("task_result"));
    }

    #[tokio::test]
    async fn scoped_rows_never_surface_in_global_recall() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        m.store_scoped("node private note about widgets", "wf:abc:node:1", &[])
            .await
            .unwrap();

        assert!(m.search("widgets", 5, None).await.unwrap().is_empty());
        let scoped = m
            .search_scoped("widgets", 5, "wf:abc:node:1")
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);
    }

    #[tokio::test]
    async fn updating_a_row_keeps_the_fts_index_in_sync() {
        // Guards migration 0032. Write-side dedup refreshes rows in place, and
        // long_term_fts is external-content — without the UPDATE trigger it
        // would keep serving the row's original wording forever.
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        let id = m
            .store("the kettle is broken", Some("task_result"), &[])
            .await
            .unwrap();

        db.get()
            .unwrap()
            .execute(
                "UPDATE long_term SET content=?1 WHERE id=?2",
                rusqlite::params!["the kettle was repaired", id],
            )
            .unwrap();

        assert!(
            m.search("repaired", 5, None).await.unwrap().len() == 1,
            "new content must be searchable after an in-place update"
        );
        assert!(
            m.search("broken", 5, None).await.unwrap().is_empty(),
            "stale terms must not survive in the FTS index"
        );
    }

    #[tokio::test]
    async fn delete_removes_the_row_from_recall() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        let id = m
            .store("remember the alamo please", Some("agent_note"), &[])
            .await
            .unwrap();
        assert_eq!(m.search("alamo", 5, None).await.unwrap().len(), 1);

        m.delete(id).unwrap();
        assert!(m.search("alamo", 5, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dedup_is_skipped_without_comparable_vectors() {
        // With no embedder there are no vectors to compare, so every write must
        // still insert — dedup must never fall back to collapsing rows blindly.
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        let a = m
            .store("identical text", Some("task_result"), &[])
            .await
            .unwrap();
        let b = m
            .store("identical text", Some("task_result"), &[])
            .await
            .unwrap();
        assert_ne!(a, b);
    }

    /// Insert a row with a hand-written vector, so the semantic path can be
    /// tested without standing up a real embedding provider.
    fn insert_vec_row(
        db: &Pool<SqliteConnectionManager>,
        content: &str,
        source: &str,
        model: &str,
        v: &[f32],
    ) -> i64 {
        let conn = db.get().unwrap();
        conn.execute(
            "INSERT INTO long_term (content,embedding,source,tags,embedding_model) VALUES (?1,?2,?3,'[]',?4)",
            rusqlite::params![content, vec_to_bytes(v), source, model],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn vector_scan_finds_rows_that_share_no_wording_with_the_query() {
        // The whole point of the vector union: FTS cannot reach this row, since
        // the query and the content have no token in common.
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        let wanted = insert_vec_row(&db, "the kettle boils", "task_result", "m1", &[1.0, 0.0, 0.0]);
        insert_vec_row(&db, "unrelated content", "task_result", "m1", &[0.0, 1.0, 0.0]);

        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", None, None, 10)
            .unwrap();
        assert_eq!(ids, vec![wanted]);
    }

    #[test]
    fn vector_scan_applies_the_similarity_floor() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        // ~0.196 cosine against the query — real but well under the 0.25 floor.
        insert_vec_row(&db, "faintly related", "task_result", "m1", &[0.2, 1.0, 0.0]);

        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", None, None, 10)
            .unwrap();
        assert!(ids.is_empty(), "sub-floor matches must not become candidates");
    }

    #[test]
    fn vector_scan_ignores_rows_from_another_embedding_model() {
        // Vectors from a different model live in a different space; comparing
        // them is meaningless even when the arithmetic succeeds.
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        insert_vec_row(&db, "perfect match", "task_result", "old-model", &[1.0, 0.0, 0.0]);

        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", None, None, 10)
            .unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn vector_scan_honours_source_exclusion_and_scope() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        let chat = insert_vec_row(&db, "a", "task_result", "m1", &[1.0, 0.0, 0.0]);
        insert_vec_row(&db, "b", "scheduler", "m1", &[1.0, 0.0, 0.0]);
        let node = insert_vec_row(&db, "c", "wf:x:node:1", "m1", &[1.0, 0.0, 0.0]);

        // Global recall excluding scheduler: node-private rows are filtered too.
        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", None, Some("scheduler"), 10)
            .unwrap();
        assert_eq!(ids, vec![chat]);

        // Scoped recall sees only its own partition.
        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", Some("wf:x:node:1"), None, 10)
            .unwrap();
        assert_eq!(ids, vec![node]);
    }

    #[test]
    fn vector_scan_returns_best_first_and_respects_want() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        let far = insert_vec_row(&db, "far", "task_result", "m1", &[0.7, 0.7, 0.0]);
        let near = insert_vec_row(&db, "near", "task_result", "m1", &[1.0, 0.0, 0.0]);

        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", None, None, 10)
            .unwrap();
        assert_eq!(ids, vec![near, far], "strongest match must come first");

        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", None, None, 1)
            .unwrap();
        assert_eq!(ids, vec![near]);
    }

    #[test]
    fn vector_scan_disabled_by_zero_limit() {
        let (db, _p) = temp_db();
        let m = LongTermMemory::new(Arc::clone(&db), None, 0.25, 0.95, 0);
        insert_vec_row(&db, "perfect match", "task_result", "m1", &[1.0, 0.0, 0.0]);

        let ids = m
            .vector_candidate_ids(&[1.0, 0.0, 0.0], "m1", None, None, 10)
            .unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn rows_by_id_loads_candidates_as_non_lexical() {
        let (db, _p) = temp_db();
        let m = mem(Arc::clone(&db));
        let id = insert_vec_row(&db, "content here", "task_result", "m1", &[1.0, 0.0, 0.0]);

        let rows = m.rows_by_id(&[id]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].content, "content here");
        // Vector-origin candidates carry no lexical evidence, so ranking must
        // drop them if their score can't be established.
        assert!(!rows[0].lexical);
        assert!(m.rows_by_id(&[]).unwrap().is_empty());
    }

    #[test]
    fn find_near_duplicate_never_folds_sourceless_rows() {
        let (db, _p) = temp_db();
        let m = mem(db);
        // Sourceless rows are human-kept notes; two of them must stay distinct
        // even when their vectors are identical.
        assert_eq!(
            m.find_near_duplicate(&[1.0, 0.0, 0.0], None, "some-model")
                .unwrap(),
            None
        );
    }
}
