-- Multi-conversation dashboard chat. One row per chat thread; message bodies
-- stay in `short_term` keyed by the same id used as the run's session_id, so
-- this table only carries the sidebar metadata (title + timestamps) needed to
-- list, rename, and delete a thread cheaply.
CREATE TABLE IF NOT EXISTS conversations (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL DEFAULT 'New chat',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_conversations_updated ON conversations(updated_at DESC);
