use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;

use crate::config::RuntimeSettings;
use crate::providers::types::Message;
use crate::router::{call_llm, SharedRouter};

// Minimum raw size worth compressing — skip tiny results
const MIN_COMPRESS_BYTES: usize = 150;

// Maximum raw content sent to compressor — trim before sending
const MAX_RAW_SEND: usize = 1500;

/// Truncate to `max` *characters*, never bytes, so multibyte input can't panic.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max).collect();
    let dropped = s.chars().count() - max;
    format!("{kept}... [trimmed {dropped} chars]")
}

pub async fn compress_and_store(
    run_id: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    tool_result: &serde_json::Value,
    router: SharedRouter,
    settings: Arc<RuntimeSettings>,
    db: Arc<Pool<SqliteConnectionManager>>,
    budget: Arc<std::sync::atomic::AtomicU32>,
) {
    let raw = tool_result.to_string();

    if !crate::router::has_available_role(&router, "memory_compressor").await {
        return;
    }

    // Skip if result is too small to be worth compressing
    if raw.len() < MIN_COMPRESS_BYTES {
        return;
    }

    // Skip error results entirely — storing them as observations causes future
    // agent runs (and scheduler/watcher tasks) to believe tools are broken even
    // after the underlying issue has been resolved.
    if tool_result.get("error").is_some()
        || tool_result.get("success").and_then(|v| v.as_bool()) == Some(false)
    {
        tracing::debug!("Compressor skipped error result for tool '{}'", tool_name);
        return;
    }

    // Consume one unit of the per-run compression budget. This bounds background
    // LLM spend on observation extraction (the most invisible recurring cost on a
    // rate-limited free pool). Only charged here — after the cheap skips — so a
    // run of tiny/error results doesn't exhaust the budget. When the budget is 0
    // (feature disabled or cap reached) we skip the LLM call entirely.
    use std::sync::atomic::Ordering;
    if budget
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |x| {
            if x > 0 {
                Some(x - 1)
            } else {
                None
            }
        })
        .is_err()
    {
        tracing::debug!("Compressor budget exhausted — skipping '{}'", tool_name);
        return;
    }

    // Build a concise args summary for context
    let args_summary = summarize_args(tool_args);

    // Trim raw result to avoid over-spending tokens on the compressor.
    // Counted in characters, not bytes: `raw` is a serialized tool result, so
    // any non-ASCII payload (search results, email bodies, emoji) puts multibyte
    // characters in it, and byte-slicing at a fixed offset panics whenever the
    // cut lands mid-character. This runs inside a spawned task, so that panic
    // was invisible — the observation was simply never stored.
    let raw_trimmed = truncate_chars(&raw, MAX_RAW_SEND);

    let msgs = [Message::user(&format!(
        "Extract only the key facts worth remembering from this tool result.\n\
         Be extremely concise — 1 to 3 bullet points maximum.\n\
         Only include facts that would be useful in a future similar task.\n\
         Skip errors unless they reveal something important.\n\
         Do not include the word 'Result' or any preamble.\n\n\
         Tool: {tool_name}\n\
         Args: {args_summary}\n\
         Result:\n{raw_trimmed}"
    ))];

    let system_prompt = "Extract key facts from tool outputs. Reply with bullet points only. \
         No preamble, no markdown headers, no explanation.";

    let (resp, model_used, _) = match call_llm(
        &msgs,
        system_prompt,
        &[],
        Some(2000),
        "memory_compressor",
        router,
        &settings,
        None,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("Compressor skipped ({}): {}", tool_name, e);
            return;
        }
    };

    let compressed = resp.text_content().trim().to_string();
    if compressed.is_empty() || compressed.to_uppercase() == "NONE" {
        return;
    }

    if let Ok(conn) = db.get() {
        let _ = conn.execute(
            "INSERT INTO observations (run_id, tool_name, compressed, raw_size, model_used)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![run_id, tool_name, compressed, raw.len() as i64, model_used,],
        );
        tracing::debug!(
            "Compressed {} ({} → {} chars) via {}",
            tool_name,
            raw.len(),
            compressed.len(),
            model_used
        );
    }
}

/// Build an FTS5 OR-query from free text, or `None` when nothing usable is
/// left. Punctuation that FTS5 treats as a token separator is replaced with a
/// space rather than deleted: dropping it outright fused terms together
/// ("wf:build" became the single token "wfbuild"), which matched nothing.
fn observation_fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|w| w.replace(['"', '*', '-', ':'], " ").trim().to_string())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

/// Retrieve compressed observations relevant to a query
/// Used to inject into agent context at session start
pub fn search_observations(
    query: &str,
    top_k: usize,
    db: &Pool<SqliteConnectionManager>,
) -> Vec<String> {
    let conn = match db.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // FTS5 search across compressed observations
    let fts_query = match observation_fts_query(query) {
        Some(q) => q,
        None => return vec![],
    };

    let mut stmt = match conn.prepare(
        "SELECT o.compressed, o.tool_name, o.created_at
         FROM observations o
         JOIN observations_fts fts ON o.id = fts.rowid
         WHERE observations_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let results = stmt.query_map(rusqlite::params![fts_query, top_k as i64], |row| {
        Ok(format!(
            "({} ago) [{}] {}",
            relative_time(&row.get::<_, String>(2).unwrap_or_default()), // created_at
            row.get::<_, String>(1)?,                                    // tool_name
            row.get::<_, String>(0)?                                     // compressed
        ))
    });

    match results {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

/// Summarize args for the compressor prompt — keep it short
fn summarize_args(args: &serde_json::Value) -> String {
    // Extract the most meaningful field — command, path, query, etc.
    for key in &["command", "query", "path", "url", "action", "server_name"] {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            return format!("{key}={}", v.chars().take(120).collect::<String>());
        }
    }
    // Fallback: serialize and trim
    args.to_string().chars().take(150).collect()
}

/// Convert UTC timestamp to relative "5m ago" etc
fn relative_time(ts: &str) -> String {
    let past = chrono::DateTime::parse_from_rfc3339(ts).ok().or_else(|| {
        // Fallback for sqlite sqlite's datetime('now') which is YYYY-MM-DD HH:MM:SS
        chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|n| {
                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(n, chrono::Utc).into()
            })
    });

    if let Some(past) = past {
        let diff = chrono::Utc::now().signed_duration_since(past);
        if diff.num_weeks() > 0 {
            format!("{}w", diff.num_weeks())
        } else if diff.num_days() > 0 {
            format!("{}d", diff.num_days())
        } else if diff.num_hours() > 0 {
            format!("{}h", diff.num_hours())
        } else if diff.num_minutes() > 0 {
            format!("{}m", diff.num_minutes())
        } else {
            format!("{}s", diff.num_seconds().max(0))
        }
    } else {
        "unknown time".into()
    }
}

/// Retrieve compressed observations from the last 24 hours only.
/// Stale observations (especially error messages) can mislead the agent
/// into believing tools are broken when they've since been fixed.
pub fn search_recent_observations(
    query: &str,
    top_k: usize,
    db: &Pool<SqliteConnectionManager>,
) -> Vec<String> {
    let conn = match db.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // FTS5 search across recent compressed observations (last 24 hours only)
    let fts_query = match observation_fts_query(query) {
        Some(q) => q,
        None => return vec![],
    };

    let mut stmt = match conn.prepare(
        "SELECT o.compressed, o.tool_name, o.created_at
         FROM observations o
         JOIN observations_fts fts ON o.id = fts.rowid
         WHERE observations_fts MATCH ?1
           AND o.created_at > datetime('now', '-24 hours')
         ORDER BY rank
         LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let results = stmt.query_map(rusqlite::params![fts_query, top_k as i64], |row| {
        Ok(format!(
            "({} ago) [{}] {}",
            relative_time(&row.get::<_, String>(2).unwrap_or_default()), // created_at
            row.get::<_, String>(1)?,                                    // tool_name
            row.get::<_, String>(0)?                                     // compressed
        ))
    });

    match results {
        Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
        Err(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_fts_query_splits_on_punctuation_instead_of_fusing_terms() {
        // Deleting ':' used to turn "wf:build" into the single token "wfbuild",
        // which matches nothing the tokenizer ever indexed.
        assert_eq!(
            observation_fts_query("wf:build failed").as_deref(),
            Some("\"wf build\" OR \"failed\"")
        );
    }

    #[test]
    fn observation_fts_query_is_none_when_only_punctuation() {
        assert_eq!(observation_fts_query(""), None);
        assert_eq!(observation_fts_query("  :  * "), None);
    }

    #[test]
    fn observation_fts_query_keeps_ordinary_words() {
        assert_eq!(
            observation_fts_query("disk usage").as_deref(),
            Some("\"disk\" OR \"usage\"")
        );
    }

    /// A serialized tool result is arbitrary UTF-8. Byte-slicing it at a fixed
    /// offset panicked whenever the cut landed inside a multibyte character —
    /// and because compression runs in a spawned task, the panic was silent and
    /// the observation was simply lost.
    #[test]
    fn truncate_chars_survives_a_multibyte_cut() {
        // Byte 1500 falls inside the 'é'; the old `&raw[..1500]` panicked here.
        let raw = format!("{}é{}", "a".repeat(MAX_RAW_SEND - 1), "b".repeat(50));
        assert!(!raw.is_char_boundary(MAX_RAW_SEND));

        let out = truncate_chars(&raw, MAX_RAW_SEND);
        assert!(out.starts_with(&"a".repeat(MAX_RAW_SEND - 1)));
        assert!(out.contains("[trimmed 50 chars]"), "got: {}", &out[out.len() - 40..]);
    }

    #[test]
    fn truncate_chars_is_a_noop_under_the_limit() {
        assert_eq!(truncate_chars("héllo 🌍", 100), "héllo 🌍");
    }

    #[test]
    fn truncate_chars_counts_characters_not_bytes() {
        // 10 emoji = 40 bytes; a 10-char limit must keep all of them.
        let s = "🌍".repeat(10);
        assert_eq!(truncate_chars(&s, 10), s);
        assert!(truncate_chars(&s, 4).starts_with(&"🌍".repeat(4)));
    }
}
