//! Prompt optimizer: "an editor who tests its own rewrites".
//!
//! A safe-by-default analog of DSPy's MIPROv2 / GEPA. Once per interval
//! (`optimizer.interval_hours`, default 24h) the nightly job:
//!
//!   1. Collects recent failures — agent runs that failed outright or that any
//!      guard / quality-checker correction caught (`qc_correction_count > 0`,
//!      `nudge_count > 0`, or a failed run) from the last
//!      `optimizer.lookback_days`.
//!   2. Asks a cheap LLM (the `router` role) to propose
//!      `optimizer.candidates` revised system-prompt versions that would fix
//!      those failure modes, keeping the SPIRITUAL & BIBLICAL section intact.
//!   3. Scores each candidate 0–10 with the `quality_checker` role (when one is
//!      configured) as a judge against the same failure list.
//!   4. Records everything in `prompt_history` (+ `optimizer_runs` audit trail)
//!      and notifies the operator.
//!
//! **Nothing is deployed automatically unless `optimizer.auto_apply` is true**
//! (default false) *and* the top candidate scores ≥ `optimizer.min_score`. Every
//! apply first snapshots the current prompt to `prompt_history` (`baseline`), so
//! a regressed change can be reverted reviewable via the `/api/optimizer/*`
//! endpoints. This keeps the optimizer a *proposal engine* by default — the
//! per-request latency and drift caveats that make self-editing risky are gated
//! behind explicit opt-in.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde_json::{json, Value};

use crate::config::RuntimeSettings;
use crate::router::model_router::SharedRouter;
use crate::state::AppState;

/// Banner in every generated prompt asking the model to keep the operator's
/// worldview section exactly as-is (see seed.sql / normalize.sql).
const PRESERVE_CLAUSE: &str =
    "IMPORTANT: keep the entire 'SPIRITUAL & BIBLICAL QUESTIONS' section verbatim \
     (never alter, shorten, or reword it) and preserve every existing rule already present.";

/// Sentinel results that mean "no answer was ever produced", which are noise
/// for the optimizer. Reaped/stale runs and boot recovery use these.
const SENTINELS: &[&str] = &[
    "Terminated: no completion was ever recorded",
    "Terminated: agent restarted",
    "Agent task terminated unexpectedly",
];

// ── Public entry points ──────────────────────────────────────────────────────

/// Daily background entry. Returns `Some(summary)` when an optimizer cycle
/// actually ran, `None` when it was skipped (disabled / too soon / nothing to
/// fix). Never panics; every failure is logged and recorded to `optimizer_runs`.
pub async fn maybe_run(state: &AppState) -> Option<String> {
    let settings = &state.settings;
    if !settings.get_bool("optimizer.enabled", true) {
        return None;
    }
    let interval_hours = settings.get_int("optimizer.interval_hours", 24).max(1);
    if let Some(secs) = seconds_since_last_run(&state.db) {
        if secs < interval_hours * 3600 {
            return None;
        }
    }
    match run_once(state).await {
        Ok(Some(summary)) => Some(summary),
        Ok(None) => None,
        Err(e) => {
            let msg = format!("Prompt optimizer failed: {e:#}");
            tracing::warn!("{msg}");
            record_run(state, 0, 0, None, "error", Some(&msg));
            None
        }
    }
}

/// One full optimizer cycle. Returns `Some(summary)` when candidates were
/// produced (or one was applied), `None` when there was nothing to optimize.
pub async fn run_once(state: &AppState) -> anyhow::Result<Option<String>> {
    // Open the audit trail — record_run() below updates this row.
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute("INSERT INTO optimizer_runs (started_at) VALUES (datetime('now'))", []);
    }

    let settings = &state.settings;
    let lookback_days = settings.get_int("optimizer.lookback_days", 3).max(1);
    let max_failures = settings.get_int("optimizer.max_failures", 6).max(1) as usize;
    let want_candidates = settings.get_int("optimizer.candidates", 3).max(1) as usize;

    let failures = recent_failures(&state.db, lookback_days, max_failures)?;
    if failures.is_empty() {
        record_run(state, 0, 0, None, "fresh — no recent failures", None);
        return Ok(None);
    }

    let current_prompt = settings.system_prompt();
    let candidates = generate_candidates(
        &state.router,
        settings,
        &current_prompt,
        &failures,
        want_candidates,
    )
    .await?;
    if candidates.is_empty() {
        record_run(
            state,
            failures.len(),
            0,
            None,
            "candidate generation returned nothing",
            None,
        );
        return Ok(None);
    }

    // Score with the quality_checker role when one exists; NULL score otherwise.
    let qc_available =
        crate::router::model_router::has_available_role(&state.router, "quality_checker").await;
    let mut scored: Vec<(Option<f64>, String, String)> = Vec::with_capacity(candidates.len());
    for (rationale, prompt) in candidates {
        let score = if qc_available {
            score_candidate(&state.router, settings, &current_prompt, &failures, &prompt).await
        } else {
            None
        };
        scored.push((score, rationale, prompt));
    }

    // Persist candidates to history (dashboard review), remembering each row id.
    let mut candidate_ids: Vec<(Option<f64>, i64)> = Vec::with_capacity(scored.len());
    for (score, rationale, prompt) in &scored {
        let id = insert_candidate(&state.db, prompt, rationale, *score, &failures)?;
        candidate_ids.push((*score, id));
    }

    // Best-scoring candidate, if any were scored at all.
    let best_score = scored
        .iter()
        .filter_map(|(s, _, _)| *s)
        .fold(f64::NEG_INFINITY, f64::max);
    let best_index = scored
        .iter()
        .enumerate()
        .max_by(|a, b| a.1 .0.partial_cmp(&b.1 .0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .filter(|i| scored[*i].0.is_some());
    let winner_id = best_index.map(|i| candidate_ids[i].1);

    // Auto-apply only when explicitly enabled and the winner clears the bar.
    let auto = settings.get_bool("optimizer.auto_apply", false);
    let min_score = settings.get_int("optimizer.min_score", 7).max(0) as f64;
    let mut applied = false;
    if auto && best_score.is_finite() && best_score >= min_score {
        if let Some(i) = best_index {
            let (_, rationale, prompt) = scored[i].clone();
            if apply_prompt(&state.db, &state.settings, &prompt, &rationale).is_ok() {
                mark_applied(&state.db, candidate_ids[i].1);
                applied = true;
            }
        }
    }

    let outcome = if applied {
        format!("applied best candidate (score {best_score:.1}, auto_apply on)")
    } else if auto {
        format!(
            "{} candidate(s) proposed; best score {best_score:.1} below auto_apply threshold {min_score:.0}",
            scored.len()
        )
    } else {
        format!("{} candidate(s) proposed for review", scored.len())
    };
    record_run(state, failures.len(), scored.len(), winner_id, &outcome, None);

    let notify = state.notify.clone();
    notify
        .emit(
            "optimizer",
            "info",
            "Prompt optimizer proposals ready",
            &format!(
                "{} revision candidate(s) generated from {} recent failure(s); best score {best_score:.1}/10{}.",
                scored.len(),
                failures.len(),
                if qc_available { "" } else { " (no quality_checker model configured — unscored)" }
            ),
        )
        .await;

    Ok(Some(outcome))
}

/// Manually apply any history row (candidate or historical prompt) as the live
/// system prompt. Snapshots the current prompt first so nothing is lost.
pub fn apply_prompt(
    db: &Pool<SqliteConnectionManager>,
    settings: &RuntimeSettings,
    prompt: &str,
    rationale: &str,
) -> anyhow::Result<()> {
    let conn = db.get()?;
    let current = settings.system_prompt();
    if current != prompt {
        conn.execute(
            "INSERT INTO prompt_history (prompt, rationale, status, failures, created_at)
             VALUES (?1, 'snapshot before apply', 'baseline', '[]', datetime('now'))",
            rusqlite::params![current],
        )?;
        settings.set("agent.system_prompt", prompt)?;
        tracing::info!("optimizer: system prompt updated ({rationale})");
    }
    Ok(())
}

/// Apply a specific history row by id (ideally a candidate). Used by the
/// dashboard Review buttons.
pub fn apply_history(
    db: &Pool<SqliteConnectionManager>,
    settings: &RuntimeSettings,
    id: i64,
) -> anyhow::Result<()> {
    let conn = db.get()?;
    let prompt: String = conn.query_row(
        "SELECT prompt FROM prompt_history WHERE id = ?1",
        rusqlite::params![id],
        |r| r.get(0),
    )?;
    apply_prompt(db, settings, &prompt, &format!("applied from history #{id}"))?;
    // Only one prompt is ever "live": demote any other applied rows, then flag
    // this one. Earlier live versions remain restorable via their baseline rows.
    let _ = conn.execute(
        "UPDATE prompt_history SET status='rejected' WHERE status='applied' AND id != ?1",
        rusqlite::params![id],
    );
    mark_applied(db, id);
    Ok(())
}

/// Mark a candidate as rejected (dismissed from review).
pub fn reject_history(db: &Pool<SqliteConnectionManager>, id: i64) -> anyhow::Result<()> {
    let conn = db.get()?;
    conn.execute(
        "UPDATE prompt_history SET status='rejected' WHERE id=?1 AND status='candidate'",
        rusqlite::params![id],
    )?;
    Ok(())
}

fn mark_applied(db: &Pool<SqliteConnectionManager>, id: i64) {
    if let Ok(conn) = db.get() {
        let _ = conn.execute(
            "UPDATE prompt_history SET status='applied' WHERE id=?1",
            rusqlite::params![id],
        );
    }
}

fn insert_candidate(
    db: &Pool<SqliteConnectionManager>,
    prompt: &str,
    rationale: &str,
    score: Option<f64>,
    failures: &[(String, String)],
) -> anyhow::Result<i64> {
    let conn = db.get()?;
    let failures_json = serde_json::to_string(
        &failures
            .iter()
            .map(|(task, issue)| json!({ "task": task, "issue": issue }))
            .collect::<Vec<_>>(),
    )?;
    conn.execute(
        "INSERT INTO prompt_history (prompt, rationale, score, status, failures, created_at)
         VALUES (?1, ?2, ?3, 'candidate', ?4, datetime('now'))",
        rusqlite::params![prompt, rationale, score, failures_json],
    )?;
    Ok(conn.last_insert_rowid())
}

fn record_run(
    state: &AppState,
    failures_count: usize,
    candidates_count: usize,
    winner_id: Option<i64>,
    outcome: &str,
    error: Option<&str>,
) {
    if let Ok(conn) = state.db.get() {
        if let Err(e) = conn.execute(
            "UPDATE optimizer_runs SET finished_at=datetime('now'), failures_count=?1, \
             candidates_count=?2, winner_id=?3, outcome=?4, error=?5
              WHERE id = (SELECT MAX(id) FROM optimizer_runs)",
            rusqlite::params![
                failures_count as i64,
                candidates_count as i64,
                winner_id,
                outcome,
                error
            ],
        ) {
            tracing::warn!("optimizer: failed to record run outcome: {e}");
        }
    }
}

fn seconds_since_last_run(db: &Pool<SqliteConnectionManager>) -> Option<i64> {
    let conn = db.get().ok()?;
    conn.query_row(
        "SELECT strftime('%s','now') - strftime('%s', COALESCE(MAX(started_at), '1970-01-01 00:00:00'))
           FROM optimizer_runs",
        [],
        |r| r.get(0),
    )
    .ok()
}

fn recent_failures(
    db: &Pool<SqliteConnectionManager>,
    lookback_days: i64,
    limit: usize,
) -> anyhow::Result<Vec<(String, String)>> {
    let conn = db.get()?;
    let mut s = conn.prepare(
        "SELECT task, COALESCE(result, '') FROM runs
          WHERE created_at >= datetime('now', ?1)
            AND (status = 'failed' OR qc_correction_count > 0 OR nudge_count > 0)
          ORDER BY created_at DESC LIMIT ?2",
    )?;
    let rows = s
        .query_map(
            rusqlite::params![format!("-{lookback_days} days"), limit as i64],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?
        .filter_map(|r| r.ok())
        .filter(|(task, result)| {
            task.trim().len() >= 8
                && !SENTINELS.iter().any(|sent| result.contains(sent))
        })
        .map(|(task, result)| {
            let issue = if result.trim().is_empty() {
                "no answer was ever produced".to_string()
            } else {
                truncate(&result, 400)
            };
            (task, issue)
        })
        .collect();
    Ok(rows)
}

// ── Candidate generation ─────────────────────────────────────────────────────

async fn generate_candidates(
    router: &SharedRouter,
    settings: &RuntimeSettings,
    current_prompt: &str,
    failures: &[(String, String)],
    want: usize,
) -> anyhow::Result<Vec<(String, String)>> {
    let failure_list = failures
        .iter()
        .enumerate()
        .map(|(i, (task, issue))| format!("{}. Task: {}\n   Issue: {}", i + 1, task, issue))
        .collect::<Vec<_>>()
        .join("\n");

    let user = format!(
        "The agent's system prompt below has repeatedly produced these failure modes:

{failure_list}

CURRENT SYSTEM PROMPT:
<system_prompt>
{current_prompt}
</system_prompt>

Act as a senior prompt engineer. Produce exactly {want} revised versions of the
system prompt that would fix, mitigate, or prevent the failure modes above —
without changing the prompt's overall voice, without removing any existing rule,
and without making it substantially longer. {PRESERVE}

Return ONLY a JSON array of exactly {want} objects, each with:
  - \"rationale\": one sentence explaining what this revision fixes,
  - \"prompt\": the full revised system prompt (the complete text, ready to swap in).

No markdown, no code fence, no commentary outside the JSON array.",
        PRESERVE = PRESERVE_CLAUSE,
        want = want
    );

    let (resp, model, _tier) = crate::router::model_router::call_llm_with_options(
        &[crate::providers::types::Message::user(&user)],
        "You are an expert prompt engineer. Output valid JSON only.",
        &[],
        Some(8000),
        "router",
        router.clone(),
        settings,
        crate::router::model_router::CallLlmOptions {
            temperature: Some(0.7),
            ..Default::default()
        },
    )
    .await?;
    let text = resp.text_content();
    tracing::info!("optimizer: candidate generation via {model}: {} chars", text.len());
    Ok(parse_candidates(&text, want))
}

fn parse_candidates(text: &str, want: usize) -> Vec<(String, String)> {
    let cleaned = strip_code_fence(text);
    let val: Value = cleaned
        .trim()
        .parse()
        .or_else(|_| {
            // Last resort: the model wrapped the array in text/narration.
            let start = cleaned.find('[').unwrap_or(0);
            let end = cleaned.rfind(']').map(|i| i + 1).unwrap_or(cleaned.len());
            cleaned[start..end].parse()
        })
        .unwrap_or(Value::Null);

    let mut out = Vec::new();
    if let Value::Array(items) = val {
        for item in items.into_iter() {
            let prompt = item
                .get("prompt")
                .and_then(|p| p.as_str())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            let rationale = item
                .get("rationale")
                .and_then(|r| r.as_str())
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if prompt.len() > 100 {
                out.push((rationale, prompt));
            }
            if out.len() >= want {
                break;
            }
        }
    }
    out
}

fn strip_code_fence(s: &str) -> String {
    let t = s.trim();
    if let Some(stripped) = t.strip_prefix("```json") {
        return stripped.strip_suffix("```").unwrap_or(stripped).to_string();
    }
    t.to_string()
}

// ── Candidate scoring (quality-checker as judge) ─────────────────────────────

async fn score_candidate(
    router: &SharedRouter,
    settings: &RuntimeSettings,
    _current_prompt: &str,
    failures: &[(String, String)],
    candidate: &str,
) -> Option<f64> {
    let failure_list = failures
        .iter()
        .enumerate()
        .map(|(i, (task, issue))| format!("{}. Task: {}\n   Issue: {}", i + 1, task, issue))
        .collect::<Vec<_>>()
        .join("\n");

    let user = format!(
        "A prompt optimizer proposes a revision to an AI agent's system prompt.
Judge ONLY whether the revision plausibly reduces these observed failures:

{failure_list}

PROPOSED SYSTEM PROMPT:
<system_prompt>
{candidate}
</system_prompt>

Score it 0 to 10 (integer) where 10 means every identified failure mode is
directly addressed without weakening any existing behavior. Consider: does the
prompt retain all prior rules? Does it keep the SPIRITUAL & BIBLICAL section
intact? Would it stop the model repeating these specific mistakes?
Output ONLY the integer score."
    );

    let (resp, _model, _tier) = crate::router::model_router::call_llm_with_options(
        &[crate::providers::types::Message::user(&user)],
        "You are a strict prompt-evaluation judge. Output ONLY an integer 0-10.",
        &[],
        Some(8),
        "quality_checker",
        router.clone(),
        settings,
        crate::router::model_router::CallLlmOptions {
            temperature: Some(0.0),
            ..Default::default()
        },
    )
    .await
    .ok()?;

    let text = resp.text_content();
    parse_score(&text)
}

fn parse_score(text: &str) -> Option<f64> {
    // First number 0..=10 anywhere in the reply (models love narration).
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_ascii_digit() || c == '.' { c } else { ' ' })
        .collect();
    cleaned
        .split_whitespace()
        .filter_map(|tok| tok.parse::<f64>().ok())
        .find(|n| (0.0..=10.0).contains(n))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

// ── Status / history (dashboard) ─────────────────────────────────────────────

pub fn status(
    db: &Pool<SqliteConnectionManager>,
    settings: &RuntimeSettings,
) -> Value {
    let mut last: Option<Value> = None;
    if let Ok(conn) = db.get() {
        if let Ok((id, started, finished, failures, candidates, outcome, error)) = conn
            .query_row(
                "SELECT id, started_at, finished_at, failures_count, candidates_count, outcome, error
                   FROM optimizer_runs ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                    ))
                },
            )
        {
            last = Some(json!({
                "id": id,
                "started_at": started,
                "finished_at": finished,
                "failures_count": failures,
                "candidates_count": candidates,
                "outcome": outcome,
                "error": error,
            }));
        }
    }

    // Currently applied prompt + how long it has been live.
    let mut applied: Option<(i64, String, String)> = None;
    if let Ok(conn) = db.get() {
        if let Ok(row) = conn.query_row(
            "SELECT id, substr(prompt, 1, 200), created_at FROM prompt_history
              WHERE status='applied' ORDER BY id DESC LIMIT 1",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        ) {
            applied = Some(row);
        }
    }

    json!({
        "few_shot": {
            "enabled": settings.get_bool("agent.few_shot.enabled", true),
            "max_examples": settings.get_int("agent.few_shot.max_examples", 3),
            "max_chars": settings.get_int("agent.few_shot.max_chars", 800),
            "example_count": crate::fewshot::count_examples(db),
            "categories": crate::fewshot::categories(db),
        },
        "optimizer": {
            "enabled": settings.get_bool("optimizer.enabled", true),
            "interval_hours": settings.get_int("optimizer.interval_hours", 24),
            "auto_apply": settings.get_bool("optimizer.auto_apply", false),
            "min_score": settings.get_int("optimizer.min_score", 7),
            "last_run": last,
            "applied_prompt_id": applied.as_ref().map(|(id, _, _)| *id),
            "applied_prompt_head": applied.as_ref().map(|(_, head, _)| head.clone()),
            "applied_at": applied.as_ref().map(|(_, _, at)| at.clone()),
        }
    })
}

pub fn history(db: &Pool<SqliteConnectionManager>, limit: i64) -> anyhow::Result<Value> {
    let conn = db.get()?;
    let mut s = conn.prepare(
        "SELECT id, prompt, rationale, score, status, failures, created_at
           FROM prompt_history ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = s
        .query_map(rusqlite::params![limit], |r| {
            let failures_json: String = r.get(5)?;
            let failures: Vec<Value> = serde_json::from_str(&failures_json).unwrap_or_default();
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "prompt": r.get::<_, String>(1)?,
                "rationale": r.get::<_, String>(2)?,
                "score": r.get::<_, Option<f64>>(3)?,
                "status": r.get::<_, String>(4)?,
                "failures": failures,
                "created_at": r.get::<_, String>(6)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    Ok(json!({ "history": rows }))
}

pub fn run_history(db: &Pool<SqliteConnectionManager>, limit: i64) -> anyhow::Result<Value> {
    let conn = db.get()?;
    let mut s = conn.prepare(
        "SELECT id, started_at, finished_at, failures_count, candidates_count, outcome, error
           FROM optimizer_runs ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = s
        .query_map(rusqlite::params![limit], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "started_at": r.get::<_, String>(1)?,
                "finished_at": r.get::<_, Option<String>>(2)?,
                "failures_count": r.get::<_, i64>(3)?,
                "candidates_count": r.get::<_, i64>(4)?,
                "outcome": r.get::<_, Option<String>>(5)?,
                "error": r.get::<_, Option<String>>(6)?,
            }))
        })?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>();
    Ok(json!({ "optimizer_runs": rows }))
}