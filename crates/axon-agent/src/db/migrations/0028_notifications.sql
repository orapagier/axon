-- Universal notification hub: a server-backed record of things that happened
-- on the server while the operator wasn't looking — job delivery outcomes,
-- watcher hits, runtime/router errors. Surfaced in the dashboard bell and
-- broadcast live to every connected WS client.
--
-- `source` is a dotted category ('scheduler', 'watcher', 'agent.runtime',
-- 'agent.router', 'workflow', 'system'). `level` is 'info' | 'warning' |
-- 'error'. `read` is 0/1. Idempotent per the migration convention.
CREATE TABLE IF NOT EXISTS notifications (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    source     TEXT NOT NULL,
    level      TEXT NOT NULL DEFAULT 'info',
    title      TEXT NOT NULL DEFAULT '',
    message    TEXT NOT NULL,
    read       INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_notifications_created ON notifications(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_notifications_unread ON notifications(read, created_at DESC);
