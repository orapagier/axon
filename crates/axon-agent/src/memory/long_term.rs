use super::embeddings::{bytes_to_vec, cosine_similarity, vec_to_bytes, VoyageEmbedder};
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

pub struct LongTermMemory {
    db: Arc<Pool<SqliteConnectionManager>>,
    embedder: Option<VoyageEmbedder>,
    qdrant: Option<Qdrant>,
    collection_name: String,
}

impl LongTermMemory {
    pub fn new(db: Arc<Pool<SqliteConnectionManager>>, key: Option<String>) -> Self {
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
            embedder: key.filter(|k| !k.is_empty()).map(VoyageEmbedder::new),
            qdrant,
            collection_name: "axon_memory".to_string(),
        }
    }

    pub async fn store(
        &self,
        content: &str,
        source: Option<&str>,
        tags: &[&str],
    ) -> anyhow::Result<i64> {
        let emb: Option<Vec<u8>> = if let Some(e) = &self.embedder {
            e.embed_one(content)
                .await
                .ok()
                .filter(|v| !v.is_empty())
                .map(|v| vec_to_bytes(&v))
        } else {
            None
        };
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".into());
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "INSERT INTO long_term (content,embedding,source,tags) VALUES (?1,?2,?3,?4)",
            rusqlite::params![content, emb, source, tags_json],
        )?;
        let id = conn.last_insert_rowid();

        if let Some(q_client) = &self.qdrant {
            if let Some(e_bytes) = &emb {
                let vec = bytes_to_vec(e_bytes);
                use qdrant_client::qdrant::Value;
                let mut payload: std::collections::HashMap<String, Value> =
                    std::collections::HashMap::new();
                payload.insert("content".to_string(), Value::from(content.to_string()));
                if let Some(s) = source {
                    payload.insert("source".to_string(), Value::from(s.to_string()));
                }
                payload.insert("tags".to_string(), Value::from(tags_json));

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

    pub async fn search(
        &self,
        query: &str,
        top_k: usize,
        source_exclude: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        if let Some(q_client) = &self.qdrant {
            if let Some(embedder) = &self.embedder {
                if let Ok(qv) = embedder.embed_one(query).await {
                    if !qv.is_empty() {
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
                                    let tags = serde_json::from_str(&tags_json).unwrap_or_default();

                                    entries.push(MemoryEntry {
                                        id,
                                        content,
                                        source,
                                        tags,
                                        created_at: String::new(),
                                        score: Some(point.score),
                                    });
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

        let hits: Vec<(i64, String, Option<String>, Option<String>, String)> = {
            let conn = self.db.get().context("DB pool")?;
            let fts_q = query
                .split_whitespace()
                .map(|w| format!("\"{}\"", w.replace('"', "")))
                .collect::<Vec<_>>()
                .join(" OR ");

            let sql = if source_exclude.is_some() {
                "SELECT lt.id,lt.content,lt.source,lt.tags,lt.created_at FROM long_term lt JOIN long_term_fts fts ON lt.id=fts.rowid WHERE long_term_fts MATCH ?1 AND (lt.source IS NULL OR lt.source != ?2) ORDER BY rank LIMIT ?3"
            } else {
                "SELECT lt.id,lt.content,lt.source,lt.tags,lt.created_at FROM long_term lt JOIN long_term_fts fts ON lt.id=fts.rowid WHERE long_term_fts MATCH ?1 ORDER BY rank LIMIT ?2"
            };
            let mut s = conn.prepare(sql)?;
            let mapped: Vec<(i64, String, Option<String>, Option<String>, String)> =
                if let Some(exc) = source_exclude {
                    s.query_map(rusqlite::params![fts_q, exc, (top_k * 3) as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect()
                } else {
                    s.query_map(rusqlite::params![fts_q, (top_k * 3) as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect()
                };
            mapped
        };
        let mut hits = hits;
        if hits.is_empty() {
            let conn = self.db.get().context("DB pool")?;
            let sql = if source_exclude.is_some() {
                "SELECT id,content,source,tags,created_at FROM long_term WHERE source IS NULL OR source != ?1 ORDER BY id DESC LIMIT ?2"
            } else {
                "SELECT id,content,source,tags,created_at FROM long_term ORDER BY id DESC LIMIT ?1"
            };
            let mut s = conn.prepare(sql)?;
            let mapped: Vec<(i64, String, Option<String>, Option<String>, String)> =
                if let Some(exc) = source_exclude {
                    s.query_map(rusqlite::params![exc, (top_k * 2) as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect()
                } else {
                    s.query_map(rusqlite::params![(top_k * 2) as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                    })?
                    .filter_map(|r| r.ok())
                    .collect()
                };
            hits = mapped;
        }
        if hits.is_empty() {
            return Ok(vec![]);
        }

        if let Some(embedder) = &self.embedder {
            if let Ok(qv) = embedder.embed_one(query).await {
                if !qv.is_empty() {
                    let conn = self.db.get().context("DB pool")?;
                    let ids: Vec<i64> = hits.iter().map(|h| h.0).collect();
                    let ph = ids
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("?{}", i + 1))
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!("SELECT id,embedding FROM long_term WHERE id IN ({}) AND embedding IS NOT NULL", ph);
                    let mut s = conn.prepare(&sql)?;
                    let emap: std::collections::HashMap<i64, Vec<f32>> = s
                        .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
                            Ok((r.get(0)?, r.get::<_, Vec<u8>>(1)?))
                        })?
                        .filter_map(|r| r.ok())
                        .map(|(id, b)| (id, bytes_to_vec(&b)))
                        .collect();
                    let mut scored: Vec<(f32, _)> = hits
                        .into_iter()
                        .map(|h| {
                            (
                                emap.get(&h.0)
                                    .map(|v| cosine_similarity(&qv, v))
                                    .unwrap_or(0.5),
                                h,
                            )
                        })
                        .collect();
                    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
                    return Ok(scored
                        .into_iter()
                        .take(top_k)
                        .map(|(score, h)| MemoryEntry {
                            id: h.0,
                            content: h.1,
                            source: h.2,
                            tags: h
                                .3
                                .and_then(|t| serde_json::from_str(&t).ok())
                                .unwrap_or_default(),
                            created_at: h.4,
                            score: Some(score),
                        })
                        .collect());
                }
            }
        }
        Ok(hits
            .into_iter()
            .take(top_k)
            .map(|h| MemoryEntry {
                id: h.0,
                content: h.1,
                source: h.2,
                tags: h
                    .3
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_default(),
                created_at: h.4,
                score: None,
            })
            .collect())
    }

    pub fn delete(&self, id: i64) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute("DELETE FROM long_term WHERE id=?1", rusqlite::params![id])?;
        // Ignore qdrant deletion error if any
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
