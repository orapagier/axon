-- Prefetched list of each provider's currently-available model IDs, refreshed
-- by a daily background job (see src/model_cache.rs). The ModelsPage add/edit
-- dropdown reads from this table instead of hitting providers live on every
-- keystroke. Keyed by (provider, base_url) so a custom host lists separately
-- from the provider's default endpoint; base_url '' means "the provider default".
CREATE TABLE IF NOT EXISTS provider_model_cache (
    provider   TEXT NOT NULL,
    base_url   TEXT NOT NULL DEFAULT '',
    model_id   TEXT NOT NULL,
    label      TEXT,
    fetched_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (provider, base_url, model_id)
);
