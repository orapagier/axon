-- Legacy value normalizations: one-time corrections to existing settings.
-- Each is guarded by a WHERE clause so re-running every boot is a no-op once
-- applied, and operator-customized values are left untouched.
-- These are intentionally DATA fixes (not schema) and are kept apart from
-- migrations/0001-0003 and seed.sql. They can be retired once no live database
-- predates them.

-- Replace the very first terse system prompt with the plain-text version.
UPDATE settings
   SET value = 'You are Axon, a capable AI agent. Always provide responses in plain text only, no Markdown formatting (no asterisks, no bolding, no code blocks unless essential for data). Complete tasks efficiently using available tools. If a tool is missing and tool writing is enabled, write one. If a task needs follow-up later, schedule it.'
 WHERE key = 'agent.system_prompt'
   AND value LIKE 'You are Axon, a capable AI agent. Complete%';

-- Stop weaker models hallucinating ```json / <tool_call> blocks.
UPDATE settings
   SET value = value || '
5. CRITICAL: You MUST use the native JSON tool calling mechanism provided by the API. NEVER output raw JSON snippets, markdown code blocks (```json), or XML tags like <tool_call> in your message body. Speak in plain text only.'
 WHERE key = 'agent.system_prompt'
   AND value NOT LIKE '%native JSON tool calling%';

-- Also call out the call:tool{args} hallucination format.
UPDATE settings
   SET value = REPLACE(value,
        'or XML tags like <tool_call> in your message body',
        'XML tags like <tool_call>, or call:tool_name{args} syntax in your message body')
 WHERE key = 'agent.system_prompt'
   AND value LIKE '%or XML tags like <tool_call> in your message body%';

-- Fix the old quiet-hours-end default (07:00 -> 04:00).
UPDATE settings SET value = '04:00'
 WHERE key = 'watcher.quiet_hours_end' AND value = '07:00';

-- Lower the old parallel-tool default (5 -> 3) on hosts that never changed it,
-- so existing installs also benefit from the small/shared-core default.
UPDATE settings SET value = '3'
 WHERE key = 'agent.max_parallel_tools' AND value = '5';

-- Router latency rework (flat per-window rate-limit cooldowns + flat per-attempt
-- timeout, no adaptive/fair-share math or exponential backoff).
-- Bump the flat per-model call timeout from the old 20s default to 30s, and park
-- a model after 2 consecutive errors instead of 3 — only on hosts still on the
-- old defaults (operator-customized values are left untouched).
UPDATE settings SET value = '30'
 WHERE key = 'router.model_call_timeout_secs' AND value = '20';
UPDATE settings SET value = '2'
 WHERE key = 'router.error_threshold' AND value = '3';

-- C1 correction: the resume-token default TTL shipped as 604800 (7 days), which
-- contradicted the documented "Approval/webhook Wait parks forever until resumed"
-- contract. Reset hosts still on that old seeded default to 0 (wait forever); a
-- deliberately-chosen TTL is left untouched.
UPDATE settings SET value = '0'
 WHERE key = 'workflow.resume_token_default_ttl_secs' AND value = '604800';

-- Harness refactor: widen the dashboard context window (5 -> 20 messages) on
-- hosts still on the old seeded default. A 5-message window forced the model
-- to answer follow-ups blind; 20 messages (10 exchanges) resolves anaphora
-- without stressing free-tier token limits. Operator-set values are untouched.
UPDATE settings SET value = '20'
 WHERE key = 'memory.dashboard_context_window' AND value = '5';

-- Drop settings made defunct by the rework: the adaptive/fair-share timeout knobs,
-- the exponential-backoff cap, and the per-iteration chain budget are no longer
-- read (the per-iteration deadline is now just the run deadline). Harmless if absent.
DELETE FROM settings WHERE key IN (
    'router.model_call_timeout_min_secs',
    'router.model_call_timeout_max_secs',
    'router.model_call_timeout_per_1k_chars_secs',
    'router.model_call_timeout_fair_share_grace_secs',
    'router.rate_limit_max_cooldown',
    'router.rate_limit_cooldown',
    'agent.min_model_chain_secs',
    'agent.request_timeout_secs',
    'agent.request_timeout_max_secs'
);
