-- Follow-up support for task activities: an optional due date (fixed-format
-- UTC, same lexicographic-comparison contract as every other timestamp) and a
-- done flag. Non-task kinds simply leave due_at NULL and done 0.

ALTER TABLE activities ADD COLUMN due_at TEXT;
ALTER TABLE activities ADD COLUMN done INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_activities_due_at ON activities(due_at);
