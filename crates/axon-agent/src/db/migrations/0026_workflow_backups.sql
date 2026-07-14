-- Change-tracking state for the off-instance workflow → Google Drive backup
-- (crate::maintenance::run_workflow_drive_backup).
--
-- One row per workflow that has ever been backed up. `content_hash` is the same
-- SHA-256 over {workflow, nodes, edges} used for version snapshots, so a backup
-- run can skip workflows whose definition is byte-identical to what Drive
-- already holds. `drive_file_id` lets the next change UPDATE that one file in
-- place instead of creating a new timestamped copy — so backups never pile up
-- and no time-based pruning is needed.
--
-- Deliberately NOT foreign-keyed to workflows: when a workflow is deleted the
-- row survives one more sweep so the backup routine can delete the now-orphan
-- Drive file, then removes the row itself.
CREATE TABLE IF NOT EXISTS workflow_backups (
    workflow_id   TEXT PRIMARY KEY,
    content_hash  TEXT NOT NULL,
    drive_file_id TEXT,
    backed_up_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
