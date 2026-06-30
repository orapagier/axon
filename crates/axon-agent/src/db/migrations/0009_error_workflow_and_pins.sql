-- Migration 0009: Milestone A finishers — error workflow (A3) + pinned data (A4).
-- All additive (ALTER TABLE ADD COLUMN) so it is tolerant of databases created
-- from the current base schema where the columns already exist
-- (tolerant_dup_column = true in db/mod.rs).

-- A3 Error workflow: the workflow to run when THIS workflow's run finishes with
-- errors. NULL falls back to the global default setting
-- `workflow.default_error_workflow_id`. A workflow is never its own error
-- handler and an error run never spawns another error run (engine loop-guard).
ALTER TABLE workflows ADD COLUMN error_workflow_id TEXT;

-- A4 Pinned data: per-node saved output used INSTEAD of executing the node on
-- manual/editor runs only (production/trigger/scheduled runs ignore it). NULL =
-- not pinned; a JSON value = the pinned output object the engine routes
-- downstream without running the node.
ALTER TABLE workflow_nodes ADD COLUMN pinned_data TEXT;
