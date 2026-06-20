-- Migration 0001: base schema (tables, indexes, triggers).
-- All statements are idempotent (IF NOT EXISTS) so a partial apply re-runs safely.
-- Seed data lives in seed.sql, not here — this file is schema only.

CREATE TABLE IF NOT EXISTS short_term (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role       TEXT NOT NULL,
    content    TEXT NOT NULL,
    tool_name  TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_st_session ON short_term(session_id, created_at);

CREATE TABLE IF NOT EXISTS long_term (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    content    TEXT NOT NULL,
    embedding  BLOB,
    source     TEXT,
    tags       TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE VIRTUAL TABLE IF NOT EXISTS long_term_fts
    USING fts5(content, content=long_term, content_rowid=id);
CREATE TRIGGER IF NOT EXISTS long_term_fts_insert AFTER INSERT ON long_term BEGIN
    INSERT INTO long_term_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS long_term_fts_delete AFTER DELETE ON long_term BEGIN
    INSERT INTO long_term_fts(long_term_fts, rowid, content) VALUES('delete', old.id, old.content);
END;

CREATE TABLE IF NOT EXISTS runs (
    id           TEXT PRIMARY KEY,
    task         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'running',
    result       TEXT,
    iterations   INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    models_used  TEXT,
    tools_used   TEXT,
    platform     TEXT DEFAULT 'dashboard',
    session_id   TEXT,
    job_id       TEXT,
    parent_run_id TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_runs_status  ON runs(status);
CREATE INDEX IF NOT EXISTS idx_runs_created ON runs(created_at DESC);

CREATE TABLE IF NOT EXISTS run_iterations (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    iteration   INTEGER NOT NULL,
    model_name  TEXT NOT NULL,
    tokens      INTEGER NOT NULL,
    tier        TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    tool_name   TEXT NOT NULL,
    args        TEXT,
    result      TEXT,
    error       TEXT,
    duration_ms INTEGER,
    parallel    INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_tc_run ON tool_calls(run_id);

CREATE TABLE IF NOT EXISTS jobs (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    task           TEXT NOT NULL,
    schedule_nl    TEXT NOT NULL,
    cron_expr      TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'active',
    created_by     TEXT DEFAULT 'user',
    platform       TEXT DEFAULT 'dashboard',
    chat_id        TEXT,
    parent_run_id  TEXT,
    max_runs       INTEGER,
    run_count      INTEGER DEFAULT 0,
    last_run_at    TEXT,
    next_run_at    TEXT,
    last_result    TEXT,
    stop_condition TEXT,
    created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS job_fire_locks (
    job_id      TEXT NOT NULL,
    slot_key    TEXT NOT NULL,
    claimed_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (job_id, slot_key)
);
CREATE INDEX IF NOT EXISTS idx_job_fire_locks_claimed_at ON job_fire_locks(claimed_at);

CREATE TABLE IF NOT EXISTS tool_patterns (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_name   TEXT NOT NULL,
    pattern     TEXT NOT NULL,
    description TEXT,
    enabled     INTEGER DEFAULT 1,
    created_at  TEXT DEFAULT (datetime('now')),
    UNIQUE(tool_name, pattern)
);
CREATE INDEX IF NOT EXISTS idx_tp_tool ON tool_patterns(tool_name, enabled);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    value_type TEXT NOT NULL,
    description TEXT,
    category   TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS files (
    id          TEXT PRIMARY KEY,
    filename    TEXT NOT NULL,
    mime_type   TEXT,
    path        TEXT NOT NULL,
    direction   TEXT NOT NULL,
    size_bytes  INTEGER,
    platform    TEXT,
    chat_id     TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS mcp_servers (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    url        TEXT NOT NULL,
    api_key    TEXT,
    status     TEXT DEFAULT 'disconnected',
    last_ping  TEXT,
    tools_json TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS models (
    name         TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    model_id     TEXT,
    api_key      TEXT NOT NULL,
    base_url     TEXT,
    timeout_secs INTEGER,
    priority     INTEGER DEFAULT 99,
    max_tokens   INTEGER DEFAULT 4096,
    enabled      INTEGER DEFAULT 1,
    role         TEXT,
    created_at   TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS observations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT    NOT NULL,
    tool_name   TEXT    NOT NULL,
    compressed  TEXT    NOT NULL,
    raw_size    INTEGER,
    model_used  TEXT,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_obs_run  ON observations(run_id);
CREATE INDEX IF NOT EXISTS idx_obs_tool ON observations(tool_name);
CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts
    USING fts5(compressed, content=observations, content_rowid=id);
CREATE TRIGGER IF NOT EXISTS obs_fts_insert AFTER INSERT ON observations BEGIN
    INSERT INTO observations_fts(rowid, compressed) VALUES (new.id, new.compressed);
END;
CREATE TRIGGER IF NOT EXISTS obs_fts_delete AFTER DELETE ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, compressed)
    VALUES ('delete', old.id, old.compressed);
END;

CREATE TABLE IF NOT EXISTS watchers (
    id             TEXT PRIMARY KEY,
    service        TEXT NOT NULL,
    tool_name      TEXT DEFAULT '',
    tool_args      TEXT DEFAULT '{}',
    label          TEXT DEFAULT '',
    enabled        INTEGER DEFAULT 1,
    poll_mins      REAL DEFAULT 5,
    last_check     TEXT,
    last_seen_ids  TEXT DEFAULT '[]',
    trigger_condition TEXT NOT NULL DEFAULT 'on_change',
    created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS watcher_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    watcher_id  TEXT NOT NULL,
    new_count   INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_wl_watcher ON watcher_log(watcher_id, created_at);

CREATE TABLE IF NOT EXISTS watcher_emails (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    service         TEXT NOT NULL,
    email_id        TEXT NOT NULL UNIQUE,
    thread_id       TEXT NOT NULL DEFAULT '',
    sender_name     TEXT NOT NULL DEFAULT '',
    sender_email    TEXT NOT NULL DEFAULT '',
    subject         TEXT NOT NULL DEFAULT '',
    body            TEXT NOT NULL DEFAULT '',
    date_received   TEXT NOT NULL DEFAULT '',
    has_attachments INTEGER NOT NULL DEFAULT 0,
    reported        INTEGER NOT NULL DEFAULT 0,
    created_at      DATETIME NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_watcher_emails_email_id  ON watcher_emails (email_id);
CREATE INDEX IF NOT EXISTS idx_watcher_emails_service   ON watcher_emails (service);
CREATE INDEX IF NOT EXISTS idx_watcher_emails_reported  ON watcher_emails (reported);

CREATE TABLE IF NOT EXISTS watcher_command_results (
    id              INTEGER  PRIMARY KEY AUTOINCREMENT,
    watcher_id      TEXT     NOT NULL,
    watcher_label   TEXT     NOT NULL DEFAULT '',
    result          TEXT     NOT NULL,
    result_hash     TEXT     NOT NULL,
    created_at      DATETIME NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_wcr_watcher_id   ON watcher_command_results (watcher_id);
CREATE INDEX IF NOT EXISTS idx_wcr_created_at   ON watcher_command_results (watcher_id, created_at DESC);

CREATE TABLE IF NOT EXISTS web_search_accounts (
    id                  TEXT    PRIMARY KEY,
    name                TEXT    NOT NULL,
    api_key             TEXT    NOT NULL,
    priority            INTEGER NOT NULL DEFAULT 1,
    enabled             INTEGER NOT NULL DEFAULT 1,
    queries_this_month  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_wsa_enabled ON web_search_accounts(enabled, priority);

CREATE TABLE IF NOT EXISTS webhook_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source      TEXT NOT NULL,
    event_type  TEXT NOT NULL,
    from_name   TEXT,
    from_id     TEXT,
    object_id   TEXT,
    parent_id   TEXT,
    message     TEXT,
    permalink   TEXT,
    raw_json    TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    read        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_wh_source ON webhook_events(source, read, created_at);

CREATE TABLE IF NOT EXISTS oauth_tokens (
    provider        TEXT PRIMARY KEY,
    access_token    TEXT NOT NULL,
    refresh_token   TEXT,
    expires_at      INTEGER,
    extra_data      TEXT,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
