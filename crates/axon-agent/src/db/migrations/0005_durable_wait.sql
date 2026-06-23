-- Migration 0005: durable Wait node.
-- TOLERANT: on databases already carrying these columns each ALTER raises a
-- "duplicate column name" error, which the runner ignores. Older databases gain
-- the columns. (Keep comments free of the statement-separator character: the
-- tolerant runner splits on it, so one inside a comment is parsed as broken SQL.)
--
-- A long Wait no longer blocks an in-process sleep. The engine suspends the run
-- (status 'waiting'), recording WHEN to wake (resume_at) and WHICH node it
-- paused on (resume_node_id). A background poller re-enters the workflow once
-- resume_at passes, so a multi-day wait survives an agent restart.
--   resume_at      - wake time, stored as canonical UTC strftime format
--                    so it compares correctly against strftime(now).
--   resume_node_id - the Wait node the run paused on (already in node_results,
--                    resume continues from its downstream edges).
-- trigger_type already exists on workflow_runs and is reused to remember the
-- run's trigger source so resume re-enters the same trigger branch.

ALTER TABLE workflow_runs ADD COLUMN resume_at TEXT;
ALTER TABLE workflow_runs ADD COLUMN resume_node_id TEXT;

-- Poller lookup: "waiting runs whose time has come".
CREATE INDEX IF NOT EXISTS idx_wr_resume ON workflow_runs(status, resume_at);
