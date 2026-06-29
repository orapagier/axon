-- Migration 0007: per-node retry-on-fail config + sub-workflow run linkage.
-- All additive (ALTER TABLE ADD COLUMN) so it is tolerant of databases created
-- from the current base schema where the columns already exist.

-- Retry-on-fail: how many times to re-execute a node after a transient failure,
-- how long to wait between attempts, and whether the wait grows exponentially.
ALTER TABLE workflow_nodes ADD COLUMN retries       INTEGER DEFAULT 0;
ALTER TABLE workflow_nodes ADD COLUMN retry_wait_ms INTEGER DEFAULT 0;
ALTER TABLE workflow_nodes ADD COLUMN retry_backoff TEXT    DEFAULT 'fixed';  -- 'fixed' | 'exponential'

-- Sub-workflow linkage: a run spawned by a Sub-workflow node records the run id
-- of the parent that invoked it, so history can show the parent/child chain.
ALTER TABLE workflow_runs ADD COLUMN parent_run_id TEXT;
