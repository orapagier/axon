//! Few-shot "muscle memory": learn from Axon's best past work.
//!
//! Two halves, mirroring DSPy's BootstrapFewShot idea:
//!
//!   * **Capture** — when a run finishes *cleanly* (no guard nudges, no claim
//!     guard, no quality-check correction, real tools used, non-trivial answer)
//!     it is stored in `few_shot_examples` as a reference example.
//!   * **Inject** — at context-build time, the closest stored examples (by
//!     keyword overlap with the current task, within a strict token budget) are
//!     appended to the system prompt, so the model mimics proven past behavior
//!     instead of guessing.
//!
//! A background sweep (`sweep`) back-fills qualifying runs from the last
//! `agent.few_shot.sweep_days` and evicts the oldest examples past
//! `agent.few_shot.max_stored`. The per-request token cost is bounded by
//! `agent.few_shot.max_examples` × `agent.few_shot.max_chars`.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use crate::config::RuntimeSettings;

/// Fallback blocks when the agent finds nothing good to mine from yet.
const MIN_RESPONSE_CHARS: usize = 60;
/// Never mine/capture a run whose answer is this large (probably a file dump).
const MAX_CAPTURE_CHARS: usize = 3200;

/// Insert a just-completed clean run as a reference example. Called from
/// `agent::loop::finalize` on the "completed" path; a no-op when few-shot is
/// disabled or the run happened to need any guard/quality correction.
pub fn capture_good_run(
    db: &Pool<SqliteConnectionManager>,
    settings: &RuntimeSettings,
    id: &str,
    task: &str,
    result: &str,
    tools: &[String],
    clean: bool,
) {
    if !settings.get_bool("agent.few_shot.enabled", true) {
        return;
    }
    if !clean {
        return;
    }
    let result = result.trim();
    if result.len() < MIN_RESPONSE_CHARS || result.len() > MAX_CAPTURE_CHARS {
        return;
    }
    let tools: Vec<String> = tools
        .iter()
        .filter(|t| !t.is_empty())
        .cloned()
        .collect();
    if tools.is_empty() {
        // Tool-backed answers only — the pattern value is in the tool use.
        return;
    }
    if let Ok(conn) = db.get() {
        let category = tools[0].clone();
        let tools_json = serde_json::to_string(&tools).unwrap_or_else(|_| "[]".into());
        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO few_shot_examples (id, source_run_id, category, task, response, tools, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                id.to_string(),
                category,
                task.to_string(),
                result.to_string(),
                tools_json,
            ],
        ) {
            tracing::warn!("few-shot: failed to capture good run: {e}");
            return;
        }
        prune_to_cap(&conn, settings);
    }
}

/// Prune `few_shot_examples` to the configured cap, oldest first.
pub fn prune_to_cap(
    conn: &rusqlite::Connection,
    settings: &RuntimeSettings,
) {
    let max_stored = settings.get_int("agent.few_shot.max_stored", 500).max(0);
    if max_stored == 0 {
        let _ = conn.execute("DELETE FROM few_shot_examples", []);
        return;
    }
    let _ = conn.execute(
        "DELETE FROM few_shot_examples WHERE id IN (
             SELECT id FROM few_shot_examples
             ORDER BY created_at DESC, rowid DESC
             LIMIT -1 OFFSET ?1
         )",
        rusqlite::params![max_stored],
    );
}

/// Background re-mining: back-fill qualifying completed runs from the last
/// `sweep_days` into `few_shot_examples` and evict to the cap. Safe to call
/// often; every insert is INSERT OR IGNORE so existing rows are never replaced
/// by this path (only a fresh capture may supersede them).
pub fn sweep(
    db: &Pool<SqliteConnectionManager>,
    settings: &RuntimeSettings,
) -> anyhow::Result<usize> {
    if !settings.get_bool("agent.few_shot.enabled", true) {
        return Ok(0);
    }
    let days = settings.get_int("agent.few_shot.sweep_days", 7).max(1);
    let conn = db.get()?;
    let mut stmt = conn.prepare(
        "SELECT id, task, result, tools_used
            FROM runs
           WHERE status = 'completed'
             AND qc_correction_count = 0
             AND nudge_count = 0
             AND claim_guard_count = 0
             AND result IS NOT NULL
             AND length(result) >= ?1
             AND length(result) <= ?2
             AND tools_used IS NOT NULL
             AND tools_used != ''
             AND tools_used != '[]'
             AND tools_used != 'null'
             AND created_at >= datetime('now', ?3)
           ORDER BY created_at DESC",
    )?;

    let max_capture = MAX_CAPTURE_CHARS;
    let rows: Vec<(String, String, String, String)> = stmt
        .query_map(
            rusqlite::params![
                MIN_RESPONSE_CHARS as i64,
                max_capture as i64,
                format!("-{days} days")
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let mut inserted = 0usize;
    for (run_id, task, result, tools_json) in rows {
        let tools: Vec<String> =
            serde_json::from_str(&tools_json).unwrap_or_default();
        let category = tools.first().cloned().unwrap_or_default();
        if category.is_empty() {
            continue;
        }
        match conn.execute(
            "INSERT OR IGNORE INTO few_shot_examples
                 (id, source_run_id, category, task, response, tools, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, COALESCE(
                 (SELECT created_at FROM few_shot_examples WHERE source_run_id = ?2),
                 datetime('now')))",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                run_id,
                category,
                task,
                result,
                serde_json::to_string(&tools).unwrap_or_else(|_| "[]".into())
            ],
        ) {
            Ok(n) => inserted += n,
            Err(e) => tracing::warn!("few-shot sweep: insert failed for run {run_id}: {e}"),
        }
    }
    if inserted > 0 {
        tracing::info!("few-shot: back-filled {inserted} example(s) from the last {days} days");
    }
    prune_to_cap(&conn, settings);
    Ok(inserted)
}

/// Count examples currently stored (for the dashboard status endpoint).
pub fn count_examples(db: &Pool<SqliteConnectionManager>) -> i64 {
    if let Ok(conn) = db.get() {
        if let Ok(n) = conn.query_row("SELECT COUNT(*) FROM few_shot_examples", [], |r| {
            r.get::<_, i64>(0)
        }) {
            return n;
        }
    }
    0
}

/// Top categories by example count (for the dashboard status endpoint).
pub fn categories(db: &Pool<SqliteConnectionManager>) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    if let Ok(conn) = db.get() {
        if let Ok(mut s) = conn.prepare(
            "SELECT category, COUNT(*) FROM few_shot_examples GROUP BY category ORDER BY 2 DESC LIMIT 25",
        ) {
            if let Ok(iter) = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?))) {
                out = iter.filter_map(|r| r.ok()).collect();
            }
        }
    }
    out
}

/// Raw example rows for the dashboard examples view.
#[derive(Debug, serde::Serialize)]
pub struct ExampleRow {
    pub id: String,
    pub source_run_id: String,
    pub category: String,
    pub task: String,
    pub response: String,
    pub tools: String,
    pub created_at: String,
}

pub fn list_examples(
    db: &Pool<SqliteConnectionManager>,
    limit: i64,
) -> anyhow::Result<Vec<ExampleRow>> {
    let conn = db.get()?;
    let mut s = conn.prepare(
        "SELECT id, source_run_id, category, task, response, tools, created_at
           FROM few_shot_examples ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = s
        .query_map(rusqlite::params![limit], |r| {
            Ok(ExampleRow {
                id: r.get(0)?,
                source_run_id: r.get(1)?,
                category: r.get(2)?,
                task: r.get(3)?,
                response: r.get(4)?,
                tools: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

const STOPWORDS: &[&str] = &[
    "about", "after", "again", "ahead", "also", "another", "around", "because",
    "before", "between", "could", "does", "doing", "email", "even", "every",
    "from", "have", "here", "just", "like", "make", "more", "most", "much",
    "need", "next", "only", "over", "please", "really", "right", "should",
    "some", "such", "than", "that", "their", "there", "these", "they", "this",
    "those", "through", "time", "want", "were", "what", "when", "where",
    "which", "while", "will", "with", "would", "your", "yours", "check",
    "look", "send", "show", "tell", "give", "find", "last", "been", "might",
];

/// Render a few-shot block for the current task, or `None` when nothing
/// relevant exists (or the feature is off / this run is ineligible).
///
/// Cheap first: every gate below runs before the DB is ever touched, so
/// conversational and structured-node runs pay nothing.
#[allow(clippy::too_many_arguments)]
pub fn render_for_task(
    task: &str,
    settings: &RuntimeSettings,
    db: &Pool<SqliteConnectionManager>,
    is_conversational: bool,
    tool_free: bool,
    isolated_memory: bool,
) -> Option<String> {
    if !settings.get_bool("agent.few_shot.enabled", true) {
        return None;
    }
    if is_conversational || tool_free || isolated_memory {
        return None;
    }
    if task.trim().len() < 12 {
        return None;
    }
    let max_examples = settings.get_int("agent.few_shot.max_examples", 3).max(0) as usize;
    let max_chars = settings.get_int("agent.few_shot.max_chars", 800).max(80) as usize;
    if max_examples == 0 {
        return None;
    }

    let conn = db.get().ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT task, response FROM few_shot_examples
              ORDER BY created_at DESC LIMIT 500",
        )
        .ok()?;
    let candidates: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    let want = significant_tokens(task);
    if want.is_empty() {
        return None;
    }

    // Score by weighted keyword overlap (DSPy demo selection, kept cheap).
    let mut scored: Vec<(f64, String, String)> = candidates
        .into_iter()
        .filter_map(|(t, resp)| {
            let have = significant_tokens(&t);
            if have.is_empty() {
                return None;
            }
            let overlap = want.iter().filter(|w| have.contains(w)).count();
            if overlap == 0 {
                return None;
            }
            let denom = ((want.len() * have.len()) as f64).sqrt();
            if denom <= 0.0 {
                return None;
            }
            let score = overlap as f64 / denom;
            Some((score, t, resp))
        })
        .collect();

    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut block = String::from(
        "\n\n[Reference examples of how past similar tasks were handled well]\n\
         Follow the style of the examples below when the current request matches them. \
         They are examples only — ALWAYS re-fetch fresh data with tools, never copy their data.",
    );
    let mut count = 0;
    for (_, past_task, resp) in scored {
        if count >= max_examples {
            break;
        }
        let intro = format!("Example {} — requested: {}", count + 1, trim(&past_task, 200));
        let answer = trim(&resp, max_chars);
        block.push_str(&format!("\n{intro}\nAnswer: {answer}"));
        count += 1;
    }
    if count == 0 {
        None
    } else {
        Some(block)
    }
}

fn trim(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn significant_tokens(s: &str) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|tok| {
            let t = tok.trim();
            t.len() >= 4
                && t.chars().any(|c| c.is_alphabetic())
                && !t.chars().all(|c| c.is_numeric())
                && !STOPWORDS.contains(&t)
        })
        .map(|t| t.to_string())
        .filter(|t| seen.insert(t.clone()))
        .collect()
}