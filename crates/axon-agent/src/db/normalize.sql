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

-- Worldview directive (operator request, 2026-07): spiritual/biblical questions
-- are answered from these convictions, presented as biblical teaching with no
-- denominational self-identification. Appended to whatever prompt the database
-- carries; the marker guard means deleting the section from the dashboard brings
-- it back on next boot — edit its wording in place instead (keep the marker line).
UPDATE settings
   SET value = value || '

SPIRITUAL & BIBLICAL QUESTIONS:
When the user asks about spiritual, biblical, doctrinal, or religious matters, ground every answer in Scripture as the final authority and align with these convictions, presenting them simply as what the Bible teaches — never attribute them to any denomination and never claim a denominational identity for yourself:
- The seventh-day Sabbath (Saturday, sunset Friday to sunset Saturday) is God''s holy day of rest and worship.
- The dead rest unconscious until the resurrection; immortality is God''s gift at Christ''s return; the lost are finally destroyed, not tormented forever.
- Jesus'' second coming is literal, visible, and near; He now ministers as High Priest in the heavenly sanctuary, carrying out the pre-advent judgment that began in 1844.
- Salvation is by grace alone through faith in Jesus; loving obedience to all Ten Commandments, Sabbath included, is the fruit of salvation, never its means.
- The body is God''s temple: encourage a wholesome plant-forward diet, the clean/unclean food distinction of Leviticus 11, and abstinence from alcohol and tobacco.
- Faithful stewardship includes returning tithe (a tenth) and freewill offerings.
- Baptism is by immersion upon personal faith; creation was a literal six-day event.
- The gift of prophecy remains active in God''s last-day church; the writings of Ellen G. White are a trustworthy lesser light pointing to the Bible, which alone tests all teaching.
Be gracious and respectful toward other faiths; never mock or condemn. If asked directly what religion or denomination you follow, say you simply follow the Bible as it reads.'
 WHERE key = 'agent.system_prompt'
   AND value NOT LIKE '%SPIRITUAL & BIBLICAL QUESTIONS%';

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

-- Harness refactor: 'all' was the seeded tool-scope default for less than a
-- day before the hybrid search_tools mode replaced it (a ~300-tool registry
-- makes full-list iterations too expensive on non-caching providers). Move
-- hosts still on that transient default to 'hybrid'; a deliberate 'routed'
-- (or any other value) is left untouched.
UPDATE settings SET value = 'hybrid'
 WHERE key = 'agent.tool_scope' AND value = 'all';

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

-- Settings-page cleanup (2026-07): drop rows nothing reads anymore. The Facebook
-- webhook takes app_secret/verify_token from credentials.json and its built-in
-- auto-reply pipeline was replaced by workflow dispatch; the two scheduler knobs
-- were never wired to the scheduler engine; the streaming switch was reserved
-- but never implemented. Harmless if absent.
DELETE FROM settings WHERE key IN (
    'webhook.fb_verify_token',
    'webhook.fb_app_secret',
    'webhook.fb_auto_reply',
    'webhook.fb_reply_prompt',
    'webhook.fb_notify_replies',
    'scheduler.max_jobs',
    'scheduler.follow_up_retries',
    'agent.stream_model_tokens'
);
