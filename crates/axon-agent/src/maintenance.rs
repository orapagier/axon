//! Database housekeeping: bounded retention for the append-only history tables.
//!
//! Several tables grow without limit unless something prunes them:
//!   * `workflow_runs`  — each run stores the full `node_results` JSON blob
//!     (HTTP bodies, image data, …); a handful of busy workflows dominate the
//!     whole DB file. Bounded here to the last N runs *per workflow*.
//!   * `runs` / `run_iterations` / `tool_calls` — per-agent-run audit log.
//!   * `observations` — auto-compressed tool results (only ever *read* for the
//!     last 24h, yet kept forever otherwise).
//!   * `webhook_events` — inbound Facebook events.
//!
//! Self-bounding tables (`short_term`, `watcher_log`, `watcher_command_results`,
//! `job_fire_locks`) are left alone, and `long_term` memories are deliberately
//! retained — they are intentional, small, and user-meaningful.
//!
//! Runs once at startup and then daily (see `main.rs`). Safe to call repeatedly;
//! every step is independent and a failure in one is logged, not fatal.

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::params;
use std::fmt;

use crate::config::RuntimeSettings;

#[derive(Default, Debug, serde::Serialize)]
pub struct RetentionStats {
    pub workflow_runs: usize,
    pub runs: usize,
    pub run_iterations: usize,
    pub tool_calls: usize,
    pub observations: usize,
    pub webhook_events: usize,
    pub blobs_deleted: usize,
    pub vacuumed: bool,
    pub freed_mb: i64,
}

impl RetentionStats {
    fn total_rows(&self) -> usize {
        self.workflow_runs
            + self.runs
            + self.run_iterations
            + self.tool_calls
            + self.observations
            + self.webhook_events
    }
}

impl fmt::Display for RetentionStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} rows pruned (workflow_runs={}, runs={}, run_iterations={}, tool_calls={}, observations={}, webhook_events={}); blobs_deleted={}; vacuum={} (~{}MB reclaimable)",
            self.total_rows(),
            self.workflow_runs,
            self.runs,
            self.run_iterations,
            self.tool_calls,
            self.observations,
            self.webhook_events,
            self.blobs_deleted,
            self.vacuumed,
            self.freed_mb,
        )
    }
}

/// Prune append-only history tables according to the operator's retention
/// settings, then reclaim disk space if enough has accumulated. Blocking
/// (SQLite); call from `spawn_blocking`.
pub fn run_retention(
    db: &Pool<SqliteConnectionManager>,
    settings: &RuntimeSettings,
) -> anyhow::Result<RetentionStats> {
    let mut stats = RetentionStats::default();

    if !settings.retention_enabled() {
        tracing::debug!("Retention sweep disabled (retention.enabled=false)");
        return Ok(stats);
    }

    let conn = db.get()?;

    // ── workflow_runs: keep the last N per workflow ───────────────────────────
    // Bounds the table hard while always preserving each workflow's most recent
    // run, which the engine reads to seed node_results on resume/continuation.
    let keep = settings.retention_workflow_runs_per_workflow().max(1);
    match conn.execute(
        "DELETE FROM workflow_runs WHERE id IN (
             SELECT id FROM (
                 SELECT id, ROW_NUMBER() OVER (
                     PARTITION BY workflow_id ORDER BY started_at DESC, id DESC
                 ) AS rn
                 FROM workflow_runs
             ) WHERE rn > ?1
         )",
        params![keep],
    ) {
        Ok(n) => stats.workflow_runs = n,
        Err(e) => tracing::warn!("Retention: workflow_runs prune failed: {}", e),
    }

    // ── runs + children: age-based, children first to avoid orphans ───────────
    let runs_days = settings.retention_runs_days().max(1);
    let cutoff = format!("-{} days", runs_days);
    let old_runs = "SELECT id FROM runs WHERE created_at < datetime('now', ?1)";

    match conn.execute(
        &format!("DELETE FROM tool_calls WHERE run_id IN ({old_runs})"),
        params![cutoff],
    ) {
        Ok(n) => stats.tool_calls = n,
        Err(e) => tracing::warn!("Retention: tool_calls prune failed: {}", e),
    }
    match conn.execute(
        &format!("DELETE FROM run_iterations WHERE run_id IN ({old_runs})"),
        params![cutoff],
    ) {
        Ok(n) => stats.run_iterations = n,
        Err(e) => tracing::warn!("Retention: run_iterations prune failed: {}", e),
    }
    match conn.execute(
        "DELETE FROM runs WHERE created_at < datetime('now', ?1)",
        params![cutoff],
    ) {
        Ok(n) => stats.runs = n,
        Err(e) => tracing::warn!("Retention: runs prune failed: {}", e),
    }

    // ── observations: age-based (FTS shadow stays in sync via DELETE trigger) ──
    let obs_days = settings.retention_observations_days().max(1);
    match conn.execute(
        "DELETE FROM observations WHERE created_at < datetime('now', ?1)",
        params![format!("-{} days", obs_days)],
    ) {
        Ok(n) => stats.observations = n,
        Err(e) => tracing::warn!("Retention: observations prune failed: {}", e),
    }

    // ── webhook_events: age-based (supersedes the never-called
    //    webhook::facebook::cleanup_old_events helper) ──────────────────────────
    let wh_days = settings.retention_webhook_events_days().max(1);
    match conn.execute(
        "DELETE FROM webhook_events WHERE created_at < datetime('now', ?1)",
        params![format!("-{} days", wh_days)],
    ) {
        Ok(n) => stats.webhook_events = n,
        Err(e) => tracing::warn!("Retention: webhook_events prune failed: {}", e),
    }

    // ── resume tokens (C1): drop tokens whose run is no longer waiting (resumed,
    //    finished, or cancelled) plus any past their expiry. A live token belongs
    //    to a still-suspended approval/webhook run; survivors here are abandoned. ─
    match conn.execute(
        "DELETE FROM workflow_resume_tokens \
         WHERE (expires_at IS NOT NULL AND expires_at < strftime('%Y-%m-%dT%H:%M:%SZ','now')) \
            OR run_id NOT IN (SELECT id FROM workflow_runs WHERE status = 'waiting')",
        [],
    ) {
        Ok(n) if n > 0 => tracing::debug!("Retention: pruned {} dead resume tokens", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("Retention: resume_tokens prune failed: {}", e),
    }

    // ── workflow binary blobs (B2): drop payloads no surviving run references ──
    // Gather every blob id still referenced by a remaining run, then delete the
    // rest. A blob shared by several runs is kept until the last is pruned.
    {
        use std::collections::HashSet;
        let mut referenced: HashSet<String> = HashSet::new();
        if let Ok(mut stmt) = conn.prepare("SELECT node_results FROM workflow_runs") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for nr in rows.flatten() {
                    crate::tools::workflow::binary::collect_referenced_ids(&nr, &mut referenced);
                }
            }
        }
        stats.blobs_deleted = crate::tools::workflow::binary::gc_unreferenced(&referenced);
    }

    // ── reclaim space ─────────────────────────────────────────────────────────
    // Freed rows become free pages, not a smaller file. Truncate the WAL each
    // sweep (cheap) and run a full VACUUM only when enough is reclaimable, since
    // VACUUM briefly takes an exclusive lock and rewrites the whole DB.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");

    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap_or(0);
    let freelist: i64 = conn
        .query_row("PRAGMA freelist_count", [], |r| r.get(0))
        .unwrap_or(0);
    stats.freed_mb = page_size.saturating_mul(freelist) / 1_048_576;

    if stats.freed_mb >= settings.retention_vacuum_min_free_mb() {
        match conn.execute_batch("VACUUM;") {
            Ok(_) => stats.vacuumed = true,
            Err(e) => tracing::warn!("Retention: VACUUM failed: {}", e),
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_db() -> (Arc<Pool<SqliteConnectionManager>>, std::path::PathBuf) {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "axon_retention_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        let manager = SqliteConnectionManager::file(&path);
        let pool = Pool::new(manager).unwrap();
        {
            let conn = pool.get().unwrap();
            crate::db::init(&conn).unwrap();
        }
        (Arc::new(pool), path)
    }

    fn count(pool: &Pool<SqliteConnectionManager>, table: &str) -> i64 {
        pool.get()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn retention_bounds_workflow_runs_and_prunes_old_history() {
        // Isolate the B2 blob-GC sweep onto a throwaway dir so it can never touch
        // a dev instance's real wf_blobs while running the suite.
        std::env::set_var(
            "AXON_WF_BLOB_DIR",
            std::env::temp_dir().join(format!("axon_blobs_test_{}", std::process::id())),
        );
        let (pool, path) = temp_db();
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO workflows (id, name) VALUES ('wf1','test')",
                [],
            )
            .unwrap();
            // 60 runs for one workflow — only the last 50 should survive.
            for i in 0..60 {
                conn.execute(
                    "INSERT INTO workflow_runs (id, workflow_id, status, started_at)
                     VALUES (?1, 'wf1', 'success', datetime('now', ?2))",
                    params![format!("wfr{i}"), format!("-{} minutes", 60 - i)],
                )
                .unwrap();
            }

            // 3 old + 2 recent agent runs, each with a child iteration + tool_call.
            for (i, age) in ["-40 days", "-35 days", "-31 days", "-2 days", "-1 days"]
                .into_iter()
                .enumerate()
            {
                let run_id = format!("run_{i}");
                conn.execute(
                    "INSERT INTO runs (id, task, status, created_at)
                     VALUES (?1, 'task', 'done', datetime('now', ?2))",
                    params![run_id, age],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO run_iterations (id, run_id, iteration, model_name, tokens, tier, duration_ms)
                     VALUES (?1, ?2, 0, 'm', 1, 't', 1)",
                    params![format!("it_{i}"), run_id],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO tool_calls (id, run_id, tool_name) VALUES (?1, ?2, 'tool')",
                    params![format!("tc_{i}"), run_id],
                )
                .unwrap();
            }

            // 3 old + 1 recent observation.
            for age in ["-40 days", "-35 days", "-31 days", "-1 days"] {
                conn.execute(
                    "INSERT INTO observations (run_id, tool_name, compressed, created_at)
                     VALUES ('r', 'tool', 'fact', datetime('now', ?1))",
                    params![age],
                )
                .unwrap();
            }

            // 2 old + 1 recent webhook event.
            for age in ["-40 days", "-31 days", "-1 days"] {
                conn.execute(
                    "INSERT INTO webhook_events (source, event_type, created_at)
                     VALUES ('fb', 'comment', datetime('now', ?1))",
                    params![age],
                )
                .unwrap();
            }
        }

        let settings = RuntimeSettings::new(Arc::clone(&pool));
        let stats = run_retention(&pool, &settings).unwrap();

        assert_eq!(stats.workflow_runs, 10, "should drop 60->50");
        assert_eq!(count(&pool, "workflow_runs"), 50);
        assert_eq!(stats.runs, 3);
        assert_eq!(count(&pool, "runs"), 2);
        assert_eq!(stats.run_iterations, 3);
        assert_eq!(count(&pool, "run_iterations"), 2);
        assert_eq!(stats.tool_calls, 3);
        assert_eq!(count(&pool, "tool_calls"), 2);
        assert_eq!(stats.observations, 3);
        assert_eq!(count(&pool, "observations"), 1);
        assert_eq!(stats.webhook_events, 2);
        assert_eq!(count(&pool, "webhook_events"), 1);

        // Idempotent: a second sweep with nothing stale prunes nothing.
        let again = run_retention(&pool, &settings).unwrap();
        assert_eq!(again.total_rows(), 0);

        // Disabled flag short-circuits.
        settings.set("retention.enabled", "false").unwrap();
        assert_eq!(run_retention(&pool, &settings).unwrap().total_rows(), 0);

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }
}
