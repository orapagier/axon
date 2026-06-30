-- C1: Wait-for-webhook + Approval (human-in-the-loop resume).
--
-- A Wait node in "webhook" or "approval" mode suspends the run durably (status
-- 'waiting', like a long timed Wait) but with NO time deadline — it parks until
-- an external caller hits a tokenized resume URL. Each suspend mints one row
-- here; the token IS the credential for the (necessarily unauthenticated) resume
-- endpoint, so it is an unguessable UUID and is consumed (deleted) on first use.
--
--   run_id / workflow_id / node_id - the suspended run and the Wait/Approval node
--                                    it parked on (resume continues from that
--                                    node's downstream edges, reusing the durable
--                                    Wait resume path).
--   expires_at - optional hard deadline. When set it is mirrored into
--                workflow_runs.resume_at so the existing time poller wakes the run
--                on a timeout branch if no one resumes it; NULL = wait forever.
--
-- Pruned by maintenance alongside other retention sweeps (expired/orphaned rows).
CREATE TABLE IF NOT EXISTS workflow_resume_tokens (
    token       TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    node_id     TEXT NOT NULL,
    mode        TEXT NOT NULL DEFAULT 'webhook',  -- 'webhook' | 'approval'
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at  TEXT
);

CREATE INDEX IF NOT EXISTS idx_resume_run ON workflow_resume_tokens(run_id);
