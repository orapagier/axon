-- Cross-execution Remove Duplicates (Sort/Limit node, dedupeScope=acrossRuns).
--
-- One row per item key a given node has ever let through, so "only new items"
-- survives across runs (n8n's Remove Duplicates "across executions" mode, which
-- Axon previously lacked). Keys are stored as SHA-256 hashes — never raw item
-- data — scoped per workflow AND node so two dedupe nodes never share memory.
--
-- Test/partial runs check this table but do not record into it (the engine
-- passes record=false), so experimenting in the editor can't eat real events —
-- an improvement over n8n, whose manual runs pollute its dedupe database.
--
-- Growth is bounded per node by dedupeMaxEntries (default 10k), enforced at
-- insert time by evicting the oldest rows.
CREATE TABLE IF NOT EXISTS workflow_dedupe_seen (
    workflow_id TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    key_hash    TEXT NOT NULL,
    first_seen  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (workflow_id, node_id, key_hash)
);

CREATE INDEX IF NOT EXISTS idx_workflow_dedupe_seen_age
    ON workflow_dedupe_seen(workflow_id, node_id, first_seen);
