-- Migration 0035: self-improvement storage.
-- TOLERANT: the three `runs` columns are additive (skip "duplicate column name"),
-- the new tables are CREATE TABLE IF NOT EXISTS, so the whole migration is safe
-- to re-run on any existing database.

-- Close the latent gap in `agent::loop::finalize`: it writes guard counts that
-- no earlier migration created, so every finalization UPDATE errored with
-- "no such column" and runs were left stranded as `running` (later reaped as
-- `failed` by maintenance). Adding them here makes finalize actually stick.
ALTER TABLE runs ADD COLUMN nudge_count INTEGER DEFAULT 0;
ALTER TABLE runs ADD COLUMN claim_guard_count INTEGER DEFAULT 0;
ALTER TABLE runs ADD COLUMN qc_correction_count INTEGER DEFAULT 0;

-- High-quality completed agent runs, mined for few-shot injection.
-- One row per source run; re-captured runs with the same source_run_id replace
-- the row, so the table holds the freshest good example per task.
CREATE TABLE IF NOT EXISTS few_shot_examples (
    id            TEXT PRIMARY KEY,
    source_run_id TEXT NOT NULL UNIQUE,
    category      TEXT NOT NULL DEFAULT '',
    task          TEXT NOT NULL,
    response      TEXT NOT NULL,
    tools         TEXT NOT NULL DEFAULT '[]',
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_fse_created  ON few_shot_examples(created_at DESC);

-- System-prompt versions: baseline snapshots (about to be replaced), candidates
-- from the optimizer, and applied prompts. Enables restore with one click.
CREATE TABLE IF NOT EXISTS prompt_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    prompt     TEXT NOT NULL,
    rationale  TEXT NOT NULL DEFAULT '',
    score      REAL,
    status     TEXT NOT NULL DEFAULT 'candidate', -- candidate | applied | baseline | rejected
    failures   TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_ph_status ON prompt_history(status);

-- Audit trail of optimizer runs (candidate generation / scoring cycles).
CREATE TABLE IF NOT EXISTS optimizer_runs (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    started_at       TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at      TEXT,
    failures_count   INTEGER DEFAULT 0,
    candidates_count INTEGER DEFAULT 0,
    winner_id        INTEGER,
    outcome          TEXT,
    error            TEXT
);