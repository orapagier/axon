-- Companion devices (phones/PCs running a device-control HTTP API the agent
-- can call, e.g. AndroidCompanion). Mirrors ssh_servers: a named list of
-- remote targets, secret encrypted at rest via crypto::encrypt_key/decrypt_key.
-- `kind` defaults to 'androidcompanion' so other companion-app kinds (e.g. a
-- future Windows companion) can be added later without a schema change.
CREATE TABLE IF NOT EXISTS devices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT UNIQUE NOT NULL,
    kind         TEXT NOT NULL DEFAULT 'androidcompanion',
    base_url     TEXT NOT NULL,
    bearer_token TEXT,
    notes        TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
