-- Migration 0008: track when a workflow was last added or edited so the UI can
-- surface the most-recently-touched workflow first in the list.
-- SQLite forbids a non-constant DEFAULT (e.g. datetime('now')) on ADD COLUMN, so
-- the column is added nullable and backfilled from created_at. Every save through
-- the upsert in get_workflows' POST path then sets updated_at = datetime('now').
-- TOLERANT: on databases that already have the column the ALTER raises
-- "duplicate column name" (ignored) and the UPDATE only ever fills NULLs.
ALTER TABLE workflows ADD COLUMN updated_at TEXT;
UPDATE workflows SET updated_at = created_at WHERE updated_at IS NULL;
