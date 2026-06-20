-- Migration 0003: columns added to tables that predate them.
-- TOLERANT: on databases created from the current 0001/0002 schema these
-- columns already exist, so each ALTER raises "duplicate column name", which
-- the runner ignores. On older databases the ALTER actually adds the column.
-- Any OTHER error still aborts startup (no more silently-swallowed failures).

ALTER TABLE mcp_servers ADD COLUMN api_key TEXT;
ALTER TABLE runs ADD COLUMN parent_run_id TEXT;
ALTER TABLE runs ADD COLUMN job_id TEXT;
ALTER TABLE watchers ADD COLUMN trigger_condition TEXT NOT NULL DEFAULT 'on_change';
ALTER TABLE models ADD COLUMN timeout_secs INTEGER;
ALTER TABLE http_requests ADD COLUMN "limit" INTEGER;
ALTER TABLE http_requests ADD COLUMN proxy TEXT;
ALTER TABLE http_requests ADD COLUMN next_request_id TEXT;
ALTER TABLE workflow_nodes ADD COLUMN position_x REAL NOT NULL DEFAULT 0.0;
ALTER TABLE workflow_nodes ADD COLUMN position_y REAL NOT NULL DEFAULT 0.0;
ALTER TABLE workflow_nodes ADD COLUMN continue_on_fail INTEGER DEFAULT 0;
