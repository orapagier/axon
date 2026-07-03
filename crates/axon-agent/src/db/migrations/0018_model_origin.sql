-- Model provenance: 'toml' rows are owned by config/models.toml (the boot sync
-- overwrites them and prunes ones removed from the file), 'runtime' rows were
-- added via the dashboard or the Homeostasis workflow node and must survive
-- restarts. Existing rows are backfilled as 'runtime' on purpose: the next boot
-- sync re-claims every name present in models.toml back to 'toml' before the
-- prune runs, so the backfill can never cause a wrong deletion.
ALTER TABLE models ADD COLUMN origin TEXT NOT NULL DEFAULT 'runtime';
