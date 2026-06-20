-- Migration 0002: tables added after the original base schema
-- (SSH, saved HTTP requests, visual workflows). All idempotent.

CREATE TABLE IF NOT EXISTS ssh_servers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE NOT NULL,
    ip TEXT NOT NULL,
    port INTEGER NOT NULL DEFAULT 22,
    username TEXT NOT NULL,
    auth_type TEXT NOT NULL,
    password TEXT,
    private_key TEXT,
    public_key TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS credentials (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    service TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS http_requests (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    method      TEXT NOT NULL,
    url         TEXT NOT NULL,
    headers     TEXT DEFAULT '{}',
    body        TEXT DEFAULT '',
    "limit"     INTEGER,
    proxy       TEXT,
    next_request_id TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS workflows (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    description     TEXT DEFAULT '',
    enabled         INTEGER DEFAULT 1,
    trigger_type    TEXT NOT NULL DEFAULT 'manual',
    trigger_config  TEXT DEFAULT '{}',
    last_run_at     TEXT,
    last_status     TEXT DEFAULT 'idle',
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS workflow_nodes (
    id              TEXT PRIMARY KEY,
    workflow_id     TEXT NOT NULL,
    position        INTEGER NOT NULL DEFAULT 0,
    position_x      REAL NOT NULL DEFAULT 0.0,
    position_y      REAL NOT NULL DEFAULT 0.0,
    node_type       TEXT NOT NULL DEFAULT 'synapse',
    name            TEXT NOT NULL DEFAULT 'Step',
    config          TEXT NOT NULL DEFAULT '{}',
    enabled         INTEGER DEFAULT 1,
    continue_on_fail INTEGER DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_wn_workflow ON workflow_nodes(workflow_id, position);

CREATE TABLE IF NOT EXISTS workflow_edges (
    id              TEXT PRIMARY KEY,
    workflow_id     TEXT NOT NULL,
    source_id       TEXT NOT NULL,
    target_id       TEXT NOT NULL,
    source_handle   TEXT,
    target_handle   TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_we_workflow ON workflow_edges(workflow_id);

CREATE TABLE IF NOT EXISTS workflow_runs (
    id              TEXT PRIMARY KEY,
    workflow_id     TEXT NOT NULL,
    status          TEXT DEFAULT 'running',
    trigger_type    TEXT,
    started_at      TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at     TEXT,
    node_results    TEXT DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_wr_workflow ON workflow_runs(workflow_id);
CREATE INDEX IF NOT EXISTS idx_wr_status   ON workflow_runs(status);
CREATE INDEX IF NOT EXISTS idx_wr_started  ON workflow_runs(started_at DESC);

-- De-duplicate any legacy tool_patterns rows, then enforce uniqueness.
DELETE FROM tool_patterns WHERE rowid NOT IN (
    SELECT MIN(rowid) FROM tool_patterns GROUP BY tool_name, pattern
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tp_unique ON tool_patterns(tool_name, pattern);
