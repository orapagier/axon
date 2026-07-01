-- Drop the workflow_resume_tokens table (introduced in 0011).
--
-- Resume is now node+run-scoped: a Wait-for-webhook/Approval run parks with
-- status 'waiting' and resume_node_id set, and wakes via
-- /webhook/{resume,approve,reject}/<node_id>/<run_id>. `resume_by_node` claims
-- the run with an atomic UPDATE guarded by (id, resume_node_id, status='waiting'),
-- so the run row itself is the credential + single-winner lock — no minted token.
-- A leaked link can't touch another run and dies the instant this one leaves
-- 'waiting' (resume, timeout via resume_at, finish, or cancel), which the resume
-- endpoint surfaces as 410. That left workflow_resume_tokens with no writers and
-- only dead prune/cleanup deletes, so it is removed here.
--
-- The `workflow.resume_token_default_ttl_secs` setting is intentionally kept: it
-- no longer backs a token, but still supplies the fallback timeout deadline that
-- webhook/approval waits mirror into workflow_runs.resume_at.
DROP INDEX IF EXISTS idx_resume_run;
DROP TABLE IF EXISTS workflow_resume_tokens;
