-- B1: Workflow versioning / history (single-operator undo + restore).
--
-- Every save snapshots the PRIOR persisted state of a workflow into this table
-- before the new state overwrites it (see api::snapshot_workflow_version). The
-- snapshot is a full AxonWorkflowBundle (the same JSON shape A5 export produces),
-- so one serializer feeds both export and history.
--
-- Growth is bounded two ways (see retention.workflow_versions_per_workflow and
-- workflow.version_min_interval_secs): content-hash dedupe + a per-save throttle
-- skip no-op/rapid autosaves, and a per-workflow cap prunes old rows while always
-- keeping any the operator explicitly labeled.
CREATE TABLE IF NOT EXISTS workflow_versions (
    id           TEXT PRIMARY KEY,
    workflow_id  TEXT NOT NULL,
    version      INTEGER NOT NULL,        -- monotonic per workflow_id (max+1)
    label        TEXT,                    -- operator-set name; labeled rows survive pruning
    content_hash TEXT,                    -- sha256 of {workflow,nodes,edges}; dedupe key
    snapshot     TEXT NOT NULL,           -- full AxonWorkflowBundle JSON
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_wv_workflow ON workflow_versions(workflow_id, version DESC);
