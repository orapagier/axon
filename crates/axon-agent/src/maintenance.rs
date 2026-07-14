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
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

    // ── trigger idempotency keys (C2): age-based. Keys only need to outlive a
    //    sender's retry window; old ones are dead weight. ─────────────────────
    let dd_days = settings.retention_trigger_dedup_days();
    match conn.execute(
        "DELETE FROM trigger_dedup WHERE seen_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', ?1)",
        params![format!("-{} days", dd_days)],
    ) {
        Ok(n) if n > 0 => tracing::debug!("Retention: pruned {} trigger_dedup keys", n),
        Ok(_) => {}
        Err(e) => tracing::warn!("Retention: trigger_dedup prune failed: {}", e),
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

    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .unwrap_or(0);
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

// ── Scheduled backups ───────────────────────────────────────────────────────
//
// Local, on-instance snapshots of axon.db and crm.db, written next to the
// existing manual crm_backup_db output (`axon_core::data_files_dir()`) so both
// live in one place instead of introducing a third directory. This is NOT
// disaster recovery on its own — a backup living on the same disk as the data
// it protects doesn't survive that disk failing. Off-instance copy (rsync,
// object storage, ...) is the operator's responsibility.

#[derive(Default, Debug, serde::Serialize)]
pub struct BackupStats {
    pub axon_db_file: Option<String>,
    pub crm_db_file: Option<String>,
    pub pruned: usize,
}

impl fmt::Display for BackupStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "axon.db={}, crm.db={}, pruned={} old backup(s)",
            self.axon_db_file.as_deref().unwrap_or("skipped"),
            self.crm_db_file.as_deref().unwrap_or("skipped"),
            self.pruned,
        )
    }
}

#[cfg(unix)]
fn chmod_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        tracing::warn!("Backup: failed to chmod 0600 {}: {}", path.display(), e);
    }
}
#[cfg(not(unix))]
fn chmod_owner_only(_path: &Path) {}

/// Lock down the live database files' permissions — same 0600-owner-only
/// treatment as tokens.json/credentials.json (`axon_core::storage`), applied
/// here since axon.db/crm.db hold the same class of secrets (encrypted
/// credentials, OAuth tokens, CRM PII).
fn secure_live_db_files(axon_db_path: &Path) {
    chmod_owner_only(axon_db_path);
    let crm_db_path = axon_core::data_dir().join("crm.db");
    chmod_owner_only(&crm_db_path);
    for db_path in [axon_db_path, &crm_db_path] {
        for suffix in ["-wal", "-shm"] {
            let sibling = PathBuf::from(format!("{}{}", db_path.display(), suffix));
            if sibling.exists() {
                chmod_owner_only(&sibling);
            }
        }
    }
}

/// Delete backup files older than `retention_days` in `dir`, matching either
/// naming convention (`axon-backup-*.db` / `crm-backup-*.db`). Age is judged
/// by file mtime, not the timestamp embedded in the filename, so a restored
/// or copied-in backup ages out normally too.
fn prune_old_backups(dir: &Path, retention_days: i64) -> usize {
    let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(
        retention_days.max(0) as u64 * 86400,
    ));
    let Some(cutoff) = cutoff else {
        return 0;
    };

    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut pruned = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".db")
            || !(name.starts_with("axon-backup-") || name.starts_with("crm-backup-"))
        {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < cutoff {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => pruned += 1,
                Err(e) => {
                    tracing::warn!("Backup: failed to prune {}: {}", entry.path().display(), e)
                }
            }
        }
    }
    pruned
}

/// Back up `axon.db` (`VACUUM INTO`) and `crm.db` (via
/// `axon_crm::records::backup_db`) into `axon_core::data_files_dir()`, then
/// prune backups older than `retention_days`. The `axon.db` half is blocking
/// SQLite work and runs off the async runtime; the `crm.db` half is native
/// sqlx-async, so it's awaited directly.
pub async fn run_backup(
    db: Arc<Pool<SqliteConnectionManager>>,
    axon_db_path: PathBuf,
    retention_days: i64,
) -> anyhow::Result<BackupStats> {
    let mut stats = BackupStats::default();
    let dir = axon_core::data_files_dir();
    std::fs::create_dir_all(&dir)?;

    // ── axon.db: VACUUM INTO, same technique as crm_backup_db ────────────────
    let axon_backup_dir = dir.clone();
    let axon_backup_result = tokio::task::spawn_blocking(move || -> anyhow::Result<PathBuf> {
        let file_name = format!(
            "axon-backup-{}.db",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );
        let path = axon_backup_dir.join(&file_name);
        // VACUUM INTO takes a filename literal, not a bind parameter; single
        // quotes in the path are SQL-escaped by doubling (mirrors crm_backup_db).
        let path_sql = path.display().to_string().replace('\'', "''");
        let conn = db.get()?;
        conn.execute_batch(&format!("VACUUM INTO '{path_sql}'"))?;
        Ok(path)
    })
    .await;

    match axon_backup_result {
        Ok(Ok(path)) => {
            chmod_owner_only(&path);
            stats.axon_db_file = Some(path.display().to_string());
        }
        Ok(Err(e)) => tracing::warn!("Backup: axon.db VACUUM INTO failed: {:#}", e),
        Err(e) => tracing::warn!("Backup: axon.db backup task join error: {}", e),
    }

    // ── crm.db: reuse the existing manual-backup implementation ──────────────
    match axon_crm::backup_pool() {
        Some(pool) => match axon_crm::records::backup_db(&pool).await {
            Ok(v) => {
                if let Some(file) = v.get("file").and_then(|f| f.as_str()) {
                    chmod_owner_only(Path::new(file));
                    stats.crm_db_file = Some(file.to_string());
                }
            }
            Err(e) => tracing::warn!("Backup: crm.db backup failed: {:#}", e),
        },
        None => {
            tracing::warn!("Backup: CRM pool not initialized yet — skipping crm.db this round")
        }
    }

    secure_live_db_files(&axon_db_path);

    // ── prune ──────────────────────────────────────────────────────────────
    stats.pruned = tokio::task::spawn_blocking(move || prune_old_backups(&dir, retention_days))
        .await
        .unwrap_or(0);

    Ok(stats)
}

// ── Off-instance workflow backups to Google Drive (incremental) ──────────────
//
// Unlike `run_backup` (raw axon.db/crm.db snapshots that live on the same disk
// as the data), this exports each workflow *definition* as a portable,
// import-compatible JSON bundle and pushes it off the box to Google Drive — real
// disaster recovery for the thing operators build by hand. Bundles carry
// credential *references*, not secret values, so the files are safe to store in
// Drive. Each file restores via POST /api/workflows/import.
//
// Incremental by content hash: exactly one Drive file per workflow, created once
// and thereafter UPDATED IN PLACE only when the definition actually changes.
// Unchanged workflows are skipped entirely; a deleted workflow's orphan file is
// removed. So backups never pile up and no time-based pruning is needed. State
// lives in the `workflow_backups` table (workflow_id → last hash + drive id).
//
// Opt-in (`workflow_backup.enabled`, default false) since it needs Google
// connected. Scheduled from `main.rs`; also callable on demand via
// POST /api/workflows/backup.

/// A workflow whose Drive copy is stale (new or changed) and must be (re)uploaded.
struct BackupUpload {
    workflow_id: String,
    /// Human-readable Drive display name, e.g. `Lead intake [a1b2c3d4].json`.
    file_name: String,
    /// Content hash to record once the upload succeeds.
    hash: String,
    /// Pretty-printed bundle JSON.
    bytes: Vec<u8>,
    /// `Some` → update that Drive file in place; `None` → create a new one.
    existing_file_id: Option<String>,
}

#[derive(Default, Debug, serde::Serialize)]
pub struct WorkflowBackupStats {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// Orphan Drive files removed (their workflow was deleted).
    pub deleted: usize,
    pub errors: usize,
}

impl fmt::Display for WorkflowBackupStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "created={}, updated={}, unchanged={}, deleted={}, errors={}",
            self.created, self.updated, self.unchanged, self.deleted, self.errors,
        )
    }
}

/// Local mirror directory for the JSON bundles: one file per workflow, id-named
/// and overwritten on each change, so the on-instance copy never piles up either.
fn workflow_backup_dir() -> PathBuf {
    axon_core::data_files_dir().join("workflow_backups")
}

/// Turn a workflow name into a Drive-friendly display-name component.
fn safe_workflow_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "workflow".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Incrementally back up workflow definitions to Google Drive: upload only the
/// new/changed ones (one file each, updated in place), skip unchanged ones, and
/// delete files whose workflow was removed. Runs unconditionally when called —
/// the `enabled` gate lives in the scheduler loop, so the manual endpoint always
/// works.
pub async fn run_workflow_drive_backup(
    state: &crate::state::AppState,
) -> anyhow::Result<WorkflowBackupStats> {
    let mut stats = WorkflowBackupStats::default();

    // ── 1. Decide what to do (one blocking SQLite pass) ───────────────────────
    //    Produce the upload plan (new/changed) and the orphan list (backup rows
    //    whose workflow no longer exists), comparing each workflow's current
    //    content hash to what Drive already holds.
    let db = Arc::clone(&state.db);
    #[allow(clippy::type_complexity)]
    let (uploads, orphans, unchanged): (
        Vec<BackupUpload>,
        Vec<(String, Option<String>)>,
        usize,
    ) = tokio::task::spawn_blocking(
        move || -> anyhow::Result<(Vec<BackupUpload>, Vec<(String, Option<String>)>, usize)> {
            use std::collections::{HashMap, HashSet};
            let conn = db.get()?;

            // Last-backed-up state: workflow_id → (content_hash, drive_file_id).
            let mut last: HashMap<String, (String, Option<String>)> = HashMap::new();
            if let Ok(mut stmt) = conn
                .prepare("SELECT workflow_id, content_hash, drive_file_id FROM workflow_backups")
            {
                if let Ok(rows) = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                }) {
                    for (id, h, fid) in rows.flatten() {
                        last.insert(id, (h, fid));
                    }
                }
            }

            let mut uploads: Vec<BackupUpload> = Vec::new();
            let mut unchanged = 0usize;
            let mut current: HashSet<String> = HashSet::new();

            for id in crate::dashboard::api::list_workflow_ids(&conn) {
                current.insert(id.clone());
                let Some(bundle) = crate::dashboard::api::build_workflow_bundle(&conn, &id) else {
                    continue;
                };
                let hash = crate::dashboard::api::workflow_content_hash(&bundle);

                // Unchanged AND we still hold a Drive file id for it → skip.
                if let Some((prev_hash, Some(_))) = last.get(&id) {
                    if *prev_hash == hash {
                        unchanged += 1;
                        continue;
                    }
                }

                let wf_name = bundle
                    .get("workflow")
                    .and_then(|w| w.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("workflow");
                let short = id.get(..8).unwrap_or(id.as_str());
                let file_name = format!("{} [{}].json", safe_workflow_name(wf_name), short);
                let bytes = serde_json::to_vec_pretty(&bundle)?;
                let existing_file_id = last.get(&id).and_then(|(_, fid)| fid.clone());

                uploads.push(BackupUpload {
                    workflow_id: id,
                    file_name,
                    hash,
                    bytes,
                    existing_file_id,
                });
            }

            // Orphans: backup rows whose workflow no longer exists.
            let orphans: Vec<(String, Option<String>)> = last
                .into_iter()
                .filter(|(id, _)| !current.contains(id))
                .map(|(id, (_, fid))| (id, fid))
                .collect();

            Ok((uploads, orphans, unchanged))
        },
    )
    .await??;

    stats.unchanged = unchanged;

    if uploads.is_empty() && orphans.is_empty() {
        tracing::debug!("Workflow Drive backup: nothing changed");
        return Ok(stats);
    }

    let dir = workflow_backup_dir();
    std::fs::create_dir_all(&dir)?;
    let folder_id = state.settings.workflow_backup_drive_folder_id();

    // ── 2. Upload new/changed workflows ───────────────────────────────────────
    //    Successes are recorded and persisted together after the loop.
    let mut recorded: Vec<(String, String, Option<String>)> = Vec::new();
    let mut first_error: Option<anyhow::Error> = None;

    for up in uploads {
        // Local mirror (id-named → overwritten each change, never piles up).
        let local_path = dir.join(format!("{}.json", up.workflow_id));
        if let Err(e) = std::fs::write(&local_path, &up.bytes) {
            tracing::warn!(
                "Workflow Drive backup: local write failed for {}: {}",
                up.workflow_id,
                e
            );
            stats.errors += 1;
            continue;
        }
        chmod_owner_only(&local_path);

        let is_update = up.existing_file_id.is_some();
        let result = if let Some(ref fid) = up.existing_file_id {
            state
                .tools
                .run(
                    "gdrive_update_binary",
                    serde_json::json!({
                        "file_id": fid,
                        "local_path": local_path.to_string_lossy(),
                        "name": up.file_name,
                        "mime_type": "application/json",
                    }),
                )
                .await
        } else {
            let mut args = serde_json::json!({
                "local_path": local_path.to_string_lossy(),
                "name": up.file_name,
                "mime_type": "application/json",
            });
            if !folder_id.is_empty() {
                args["folder_id"] = serde_json::json!(folder_id);
            }
            state.tools.run("gdrive_upload_binary", args).await
        };

        match result {
            Ok(v) => {
                // Update returns the same id; create returns a fresh one. Fall
                // back to the known id so a sparse response still records state.
                let new_fid = v
                    .get("id")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
                    .or(up.existing_file_id);
                recorded.push((up.workflow_id, up.hash, new_fid));
                if is_update {
                    stats.updated += 1;
                } else {
                    stats.created += 1;
                }
            }
            Err(e) => {
                stats.errors += 1;
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }

    // ── 3. Remove orphan Drive files + their local mirrors ────────────────────
    let mut removed_rows: Vec<String> = Vec::new();
    for (id, fid) in orphans {
        if let Some(fid) = fid {
            match state
                .tools
                .run("gdrive_delete", serde_json::json!({ "file_id": fid }))
                .await
            {
                Ok(_) => stats.deleted += 1,
                // Log but still drop the row: retrying a delete that fails on an
                // already-gone / forbidden file would never start succeeding.
                Err(e) => tracing::warn!(
                    "Workflow Drive backup: orphan delete failed for {}: {:#}",
                    id,
                    e
                ),
            }
        }
        let _ = std::fs::remove_file(dir.join(format!("{}.json", id)));
        removed_rows.push(id);
    }

    // ── 4. Persist results (one blocking transaction) ─────────────────────────
    if !recorded.is_empty() || !removed_rows.is_empty() {
        let db = Arc::clone(&state.db);
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let mut conn = db.get()?;
            let tx = conn.transaction()?;
            for (id, hash, fid) in &recorded {
                tx.execute(
                    "INSERT INTO workflow_backups (workflow_id, content_hash, drive_file_id, backed_up_at)
                     VALUES (?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%SZ','now'))
                     ON CONFLICT(workflow_id) DO UPDATE SET
                         content_hash  = excluded.content_hash,
                         drive_file_id = excluded.drive_file_id,
                         backed_up_at  = excluded.backed_up_at",
                    params![id, hash, fid],
                )?;
            }
            for id in &removed_rows {
                tx.execute(
                    "DELETE FROM workflow_backups WHERE workflow_id = ?1",
                    params![id],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await??;
    }

    // If nothing at all succeeded but we hit errors, surface the first so the
    // manual endpoint / logs show *why* (e.g. Google not connected).
    if stats.created == 0 && stats.updated == 0 && stats.deleted == 0 {
        if let Some(e) = first_error {
            return Err(e.context("workflow Drive backup made no progress"));
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
        // a dev instance's real wf_blobs while running the suite. The shared guard
        // serializes against the binary unit tests, which also set this env var.
        let _g = crate::tools::workflow::binary::BLOB_DIR_TEST_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "AXON_WF_BLOB_DIR",
            std::env::temp_dir().join(format!("axon_blobs_test_{}", std::process::id())),
        );
        let (pool, path) = temp_db();
        {
            let conn = pool.get().unwrap();
            conn.execute("INSERT INTO workflows (id, name) VALUES ('wf1','test')", [])
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

    #[test]
    fn trigger_dedup_is_idempotent_and_pruned_by_age() {
        let (pool, path) = temp_db();
        {
            let conn = pool.get().unwrap();
            // INSERT OR IGNORE: the first mark inserts (1 row), a re-mark of the
            // same key is a no-op (0 rows) — this is what makes a retried webhook
            // skip. A different key still inserts.
            let mark = |k: &str| {
                conn.execute(
                    "INSERT OR IGNORE INTO trigger_dedup (source, event_key) VALUES ('webhook', ?1)",
                    params![k],
                )
                .unwrap()
            };
            assert_eq!(mark("evt-1"), 1, "first time inserts");
            assert_eq!(mark("evt-1"), 0, "duplicate is ignored");
            assert_eq!(mark("evt-2"), 1, "distinct key inserts");

            // Backdate one key past the retention horizon.
            conn.execute(
                "UPDATE trigger_dedup SET seen_at = strftime('%Y-%m-%dT%H:%M:%SZ','now','-30 days') WHERE event_key = 'evt-1'",
                [],
            )
            .unwrap();
        }

        let settings = RuntimeSettings::new(Arc::clone(&pool));
        run_retention(&pool, &settings).unwrap();
        // Old key pruned (default 7-day horizon), recent one kept.
        assert_eq!(count(&pool, "trigger_dedup"), 1);

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "axon_backup_test_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch_with_age(path: &Path, age_secs: u64) {
        std::fs::write(path, b"x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(old).unwrap();
    }

    #[test]
    fn prune_old_backups_removes_only_stale_matching_files() {
        let dir = temp_dir("prune");

        // Old, matching prefix — should be pruned.
        touch_with_age(&dir.join("axon-backup-20250101-000000.db"), 20 * 86400);
        touch_with_age(&dir.join("crm-backup-20250101-000000.db"), 20 * 86400);
        // Recent, matching prefix — should survive.
        touch_with_age(&dir.join("axon-backup-20260101-000000.db"), 86400);
        // Old but non-matching name/extension — must be left alone (shared dir
        // with regular staged Files-page uploads).
        touch_with_age(&dir.join("some-user-upload.pdf"), 20 * 86400);
        touch_with_age(&dir.join("axon-backup-old-not-db.txt"), 20 * 86400);

        let pruned = prune_old_backups(&dir, 14);
        assert_eq!(pruned, 2);
        assert!(!dir.join("axon-backup-20250101-000000.db").exists());
        assert!(!dir.join("crm-backup-20250101-000000.db").exists());
        assert!(dir.join("axon-backup-20260101-000000.db").exists());
        assert!(dir.join("some-user-upload.pdf").exists());
        assert!(dir.join("axon-backup-old-not-db.txt").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
