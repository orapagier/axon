-- Default settings + tool-routing patterns.
-- Every statement is INSERT OR IGNORE, so this runs on every boot and only
-- fills in keys that don't exist yet — it never overwrites operator changes.
-- (One-time value corrections live in normalize.sql, not here.)

INSERT OR IGNORE INTO settings VALUES
    ('agent.max_iterations',        '20',    'int',    'Max agent loop iterations per task',            'agent',     datetime('now')),
    ('agent.max_repeated_tool_calls','3',    'int',    'Stop retrying after this many identical consecutive tool calls; disables tools for one turn and returns a best-effort answer (0=off)', 'agent', datetime('now')),
    ('agent.max_parallel_tools',    '3',     'int',    'Max tools to run in parallel (keep low on small/shared-core hosts)', 'agent', datetime('now')),
    ('agent.tool_timeout_secs',     '30',    'int',    'Python tool subprocess timeout seconds',         'agent',     datetime('now')),
    ('agent.allow_tool_writing',    'true',  'bool',   'Allow agent to write temporary tools',           'agent',     datetime('now')),
    ('agent.temp_tool_max_retries', '2',     'int',    'Retries for agent-written tools',               'agent',     datetime('now')),
    ('agent.quality_check',         'true',  'bool',   'Run quality check on responses that used tools (requires quality_checker model role)', 'agent', datetime('now')),
    ('agent.utc_offset_hours',      '8',     'int',    'Operator timezone as a fixed UTC offset in hours (-12..14); used for schedule parsing and the agent''s time context', 'agent', datetime('now')),
    ('router.error_threshold',      '2',     'int',    'Consecutive non-rate-limit errors before parking a model until midnight', 'router', datetime('now')),
    ('router.model_call_timeout_secs','30',  'int',    'Flat per-attempt model timeout in seconds (overridable per model); on timeout the router fails over immediately', 'router', datetime('now')),
    ('memory.short_term_max_msgs',  '50',    'int',    'Max messages kept per session',                 'memory',    datetime('now')),
    ('memory.dashboard_context_window','20',  'int',    'Newest messages fed to the model as context per dashboard chat (0=send full thread; transcript is still retained up to short_term_max_msgs)', 'memory', datetime('now')),
    ('memory.long_term_top_k',      '5',     'int',    'Memories injected per agent call',              'memory',    datetime('now')),
    ('messaging.telegram_token',    '',      'string', 'Telegram Bot Token (TELOXIDE_TOKEN)',           'messaging', datetime('now')),
    ('messaging.discord_token',     '',      'string', 'Discord Bot Token (DISCORD_TOKEN)',             'messaging', datetime('now')),
    ('messaging.slack_token',       '',      'string', 'Slack Bot Token (SLACK_BOT_TOKEN)',             'messaging', datetime('now')),
    ('instagram.public_base_url',   'https://mcp.yourdomain.com', 'string', 'Public HTTPS base URL used for temporary local media links', 'instagram', datetime('now')),
    ('instagram.bind_addr',         '0.0.0.0:8080', 'string', 'Bind address for the in-process media server (restart the agent after changing)', 'instagram', datetime('now')),
    ('instagram.media_url_ttl_secs','7200',  'int',  'Temporary local-media URL TTL in seconds',        'instagram', datetime('now')),
    ('instagram.image_poll_interval_secs','2','int', 'Image container status poll interval in seconds',  'instagram', datetime('now')),
    ('instagram.image_poll_timeout_secs','60','int', 'Image container wait timeout in seconds before publish', 'instagram', datetime('now')),
    ('instagram.video_poll_interval_secs','10','int','Video/Reels container status poll interval in seconds', 'instagram', datetime('now')),
    ('instagram.video_poll_timeout_secs','600','int','Video/Reels container wait timeout in seconds before publish', 'instagram', datetime('now')),
    ('watcher.user_name',           'boss',  'string', 'Your name for personalized watcher notifications', 'watcher', datetime('now')),
    ('watcher.user_title',          '',      'string', 'Your title (e.g. Pastor, Dr.) for varied greetings', 'watcher', datetime('now')),
    ('watcher.notify_platform',     'telegram','string','Platform to send watcher notifications to',    'watcher',   datetime('now')),
    ('watcher.notify_chat_id',      '',      'string', 'Chat ID to send watcher notifications to',     'watcher',   datetime('now'));

-- Agent system prompt (kept verbatim; complex value isolated in its own insert).
INSERT OR IGNORE INTO settings VALUES
    ('agent.system_prompt',
     'You are Axon, a capable AI agent. Always provide responses in plain text only, no Markdown formatting (no asterisks, no bolding, no code blocks unless essential for data). Complete tasks efficiently using available tools. If a tool is missing and tool writing is enabled, write one. If a task needs follow-up later, schedule it.',
     'string', 'Agent system prompt', 'agent', datetime('now'));

-- Quality-check scoping.
INSERT OR IGNORE INTO settings VALUES
    ('agent.quality_check_mode',    'mutating', 'string', 'When to spend an LLM quality check: all (every tool-backed answer), mutating (only state-changing actions, false refusals, or blank/fake-success responses), off', 'agent', datetime('now'));

-- Harness refactor: tool scope + tool-result context budget + reasoning.
INSERT OR IGNORE INTO settings VALUES
    ('agent.tool_scope',            'hybrid', 'string', 'Tools shown to the model each iteration: hybrid (routed subset + search_tools discovery; best for large registries), all (every enabled tool), routed (legacy regex/embedding/LLM pre-filter)', 'agent', datetime('now')),
    ('agent.tool_result_budget_chars','100000','int',   'Max chars of tool results kept in the model''s context per run; oldest complete tool exchanges are dropped first', 'agent', datetime('now')),
    ('agent.reasoning_effort',      'medium', 'string', 'Reasoning depth on complex/tool-use turns: off, low, medium, high. Providers that reject the field are detected and skipped automatically', 'agent', datetime('now')),
    ('agent.planning',              'true',   'bool',   'On multi-step tasks, instruct the model to lay out a checklist via update_plan first and check steps off as it works', 'agent', datetime('now'));

-- Cost redesign: per-run budgets + observation compression (defaults mirror the code).
INSERT OR IGNORE INTO settings VALUES
    ('agent.temperature',             '0.3',  'float', 'Sampling temperature for agent model calls; low values reduce hallucinated tool syntax and correction oscillation', 'agent', datetime('now')),
    ('agent.run_timeout_secs',        '300',  'int',   'Hard wall-clock deadline for one agent run in seconds; on expiry the run stops with a best-effort answer', 'agent', datetime('now')),
    ('agent.max_corrections',         '6',    'int',   'Global correction budget per run across ALL retry reasons (claim guard, refusal nudge, blank answer, hallucinated tool syntax, quality check)', 'agent', datetime('now')),
    ('agent.max_total_tokens',        '0',    'int',   'Hard ceiling on cumulative tokens (input+output) per run — caps spend even when a few iterations carry huge contexts (0 = off)', 'agent', datetime('now')),
    ('agent.compress_observations',   'true', 'bool',  'Compress large tool observations in the background for later recall (each compression is one LLM call)', 'agent', datetime('now')),
    ('agent.max_observations_per_run','4',    'int',   'Max background observation compressions per run; bounds the most invisible recurring cost', 'agent', datetime('now'));

-- Tool-router tiers (pattern → embedding → LLM), used by hybrid and routed tool scopes.
INSERT OR IGNORE INTO settings VALUES
    ('router.use_embeddings',  'true', 'bool',  'Use the embedding tier for tool routing: one cheap embedding call instead of an LLM completion when regex patterns miss', 'router', datetime('now')),
    ('router.embed_top_k',     '5',    'int',   'Max tools the embedding tier returns per routing decision', 'router', datetime('now')),
    ('router.embed_floor',     '0.45', 'float', 'Minimum cosine similarity for the embedding tier; below it the router falls back to the LLM tier. Re-tune after switching embedder.model', 'router', datetime('now')),
    ('router.system_prompt',   'You are a routing proxy. Reply ONLY with comma-separated names of the tools needed, or exactly NONE. Do not use quotes or backticks.', 'string', 'System prompt for the LLM routing tier (blank = built-in default)', 'router', datetime('now')),
    ('router.user_prompt',     '',     'string', 'User-prompt template for the LLM routing tier; placeholders: {tool_list}, {prior}, {multi}, {msg}. Blank = built-in template, which includes the auto-generated Google/Microsoft disambiguation rules', 'router', datetime('now'));

-- Telegram /workflows access control (blank = anyone in a chat the bot serves).
INSERT OR IGNORE INTO settings VALUES
    ('messaging.workflow_runner_chat_ids', '', 'string', 'Comma-separated Telegram chat IDs allowed to use the /workflows run menu (blank = any chat)', 'messaging', datetime('now')),
    ('messaging.workflow_runner_user_ids', '', 'string', 'Comma-separated Telegram user IDs allowed to use the /workflows run menu (blank = any user)', 'messaging', datetime('now'));

-- Watcher / Smart Notifications.
INSERT OR IGNORE INTO settings VALUES
    ('watcher.enabled',                'false',     'bool',   'Enable Smart Notifications (auto-poll Gmail, Outlook, Calendar, Facebook)',  'watcher', datetime('now')),
    ('watcher.quiet_hours_start',      '22:00',     'string', 'Quiet hours start (HH:MM, no notifications)',                               'watcher', datetime('now')),
    ('watcher.quiet_hours_end',        '04:00',     'string', 'Quiet hours end (HH:MM)',                                                   'watcher', datetime('now')),
    ('watcher.timezone_offset_hours',  '8',         'int',    'UTC offset for quiet hours (e.g., 8 for PHT, -5 for EST)',                   'watcher', datetime('now'));

-- Database retention / housekeeping (pruned daily by crate::maintenance).
INSERT OR IGNORE INTO settings VALUES
    ('retention.enabled',                     'true', 'bool', 'Prune append-only history tables daily to bound database growth', 'retention', datetime('now')),
    ('retention.workflow_runs_per_workflow',  '50',   'int',  'Workflow runs kept per workflow (each stores full node-result JSON; biggest growth source)', 'retention', datetime('now')),
    ('retention.runs_days',                   '30',   'int',  'Days of agent run history kept (runs, run_iterations, tool_calls)', 'retention', datetime('now')),
    ('retention.observations_days',           '30',   'int',  'Days of compressed tool observations kept (only the last 24h are ever read)', 'retention', datetime('now')),
    ('retention.webhook_events_days',         '30',   'int',  'Days of inbound webhook events kept', 'retention', datetime('now')),
    ('retention.vacuum_min_free_mb',          '20',   'int',  'Run VACUUM after a sweep only when at least this many MB are reclaimable', 'retention', datetime('now')),
    ('retention.workflow_versions_per_workflow', '50', 'int', 'Workflow version snapshots kept per workflow (labeled ones are always kept beyond this cap)', 'retention', datetime('now')),
    ('workflow.version_min_interval_secs',    '30',   'int',  'Minimum seconds between version snapshots of the same workflow (throttles editor autosave noise)', 'workflow', datetime('now')),
    ('workflow.binary_inline_max_bytes',      '65536','int',  'Node-output strings larger than this are offloaded to the blob store instead of stored inline in run history (0 disables)', 'workflow', datetime('now')),
    ('workflow.max_concurrent_runs',          '10',   'int',  'Maximum workflow runs executing at once (bounds CPU/memory under trigger bursts; applied at startup)', 'workflow', datetime('now')),
    ('workflow.max_queue_depth',              '0',    'int',  'Maximum runs queued waiting to execute before new trigger fires are shed (0 = unbounded queue)', 'workflow', datetime('now')),
    ('workflow.public_base_url',              '',     'string','Public HTTPS base URL used to build resume/approve/reject links for Wait-for-webhook & Approval nodes (blank = emit relative paths)', 'workflow', datetime('now')),
    ('workflow.resume_token_default_ttl_secs','0',    'int', 'Default lifetime of a Wait-for-webhook/Approval resume token when the node sets no explicit timeout (0 = wait forever — the C1 contract; set >0 for a fallback auto-timeout)', 'workflow', datetime('now')),
    ('workflow.webhook_dedup_window_secs',    '0',    'int',  'Seconds to dedup generic webhooks by body hash when they send no Idempotency-Key/event_id (0 = off; explicit-key dedup is always on)', 'workflow', datetime('now')),
    ('retention.trigger_dedup_days',          '7',    'int',  'Days of trigger idempotency keys (webhook/github redelivery dedup) kept before pruning', 'retention', datetime('now'));

-- Scheduled local backups of axon.db/crm.db (crate::maintenance::run_backup).
-- These are on-instance snapshots only, not off-site disaster recovery.
INSERT OR IGNORE INTO settings VALUES
    ('backup.enabled',        'true', 'bool', 'Back up axon.db and crm.db daily to the Files page directory (local, on-instance — off-site copy is the operator''s responsibility)', 'backup', datetime('now')),
    ('backup.retention_days', '14',   'int',  'Days to keep local database backups before pruning', 'backup', datetime('now'));

-- Embeddings provider (semantic tool-router tier + long-term memory recall).
-- One OpenAI-compatible code path; switching providers is a settings change:
--   Google: https://generativelanguage.googleapis.com/v1beta/openai + gemini-embedding-001
--   Ollama: http://localhost:11434/v1 + all-minilm (leave api_key blank)
--   Voyage: https://api.voyageai.com/v1 + voyage-4
-- Blank base_url keeps the legacy VOYAGE_API_KEY env-var behavior.
INSERT OR IGNORE INTO settings VALUES
    ('embedder.base_url', '', 'string', 'OpenAI-compatible embeddings base URL (Google: https://generativelanguage.googleapis.com/v1beta/openai, Ollama: http://localhost:11434/v1, Voyage: https://api.voyageai.com/v1). Blank = legacy VOYAGE_API_KEY fallback. Restart after changing.', 'embedder', datetime('now')),
    ('embedder.model',    '', 'string', 'Embedding model name (e.g. gemini-embedding-001, all-minilm, voyage-4). Changing it re-embeds stored memories in the background on next start; re-check router.embed_floor after switching.', 'embedder', datetime('now')),
    ('embedder.api_key',  '', 'string', 'API key for the embeddings endpoint; a ${VAR} placeholder resolves from settings then environment (e.g. ${GEMINI_API_KEY}). Leave blank for local Ollama.', 'embedder', datetime('now'));

-- Dashboard voice input (Chat page microphone). One OpenAI-compatible
-- /audio/transcriptions code path; switching providers is a settings change:
--   Groq:   https://api.groq.com/openai/v1 + whisper-large-v3-turbo
--   OpenAI: https://api.openai.com/v1 + gpt-4o-mini-transcribe (or whisper-1)
-- The mic button always renders; until base_url and model are both set the
-- endpoint answers with a configuration hint instead of transcribing.
INSERT OR IGNORE INTO settings VALUES
    ('stt.base_url', '', 'string', 'OpenAI-compatible audio base URL (Groq: https://api.groq.com/openai/v1, OpenAI: https://api.openai.com/v1). Blank disables dashboard voice input.', 'stt', datetime('now')),
    ('stt.model',    '', 'string', 'Transcription model (e.g. whisper-large-v3-turbo on Groq, gpt-4o-mini-transcribe or whisper-1 on OpenAI).', 'stt', datetime('now')),
    ('stt.api_key',  '', 'string', 'API key for the transcription endpoint; a ${VAR} placeholder resolves from settings then environment (e.g. ${GROQ_API_KEY}).', 'stt', datetime('now')),
    ('stt.language', '', 'string', 'Optional ISO-639-1 language hint (e.g. en, tl); blank lets the model auto-detect.', 'stt', datetime('now'));

-- CRM agent access. Read tools are always agent-callable; the write tools
-- (create/update/delete/convert/archive/restore) are gated per-tool via the
-- ToolsPage Enable toggle (see tools/overrides.rs), same as social/messaging
-- write tools. Workflow nodes always have full CRM access either way.
INSERT OR IGNORE INTO settings VALUES
    ('crm.default_currency', 'USD', 'string', 'Currency assigned to deals created without an explicit one (3-letter code, e.g. PHP, USD, EUR). Applies immediately, no restart needed.', 'crm', datetime('now'));

-- Web search.
INSERT OR IGNORE INTO settings VALUES
    ('websearch.enabled',             'false', 'bool',   'Enable Web Search tool (requires Tavily accounts below)',      'websearch', datetime('now')),
    ('websearch.max_results',         '5',     'int',    'Default max results per search (1-10)',                       'websearch', datetime('now')),
    ('websearch.search_depth',        'basic', 'string', 'Tavily search depth: basic (1 credit) or advanced (2 credits)', 'websearch', datetime('now'));

-- Scheduler nudge prompt (single quotes inside are SQL-escaped as '').
INSERT OR IGNORE INTO settings VALUES
    ('scheduler.nudge_prompt', 'IT IS NOW THE SCHEDULED TIME FOR: **{job_name}**. Task/Reminder: {task}. Act as a close human friend of {user_name} (who is also a {user_title}). Remind them about this task in a purely natural, warm, and conversational way, as if you''re just casually mentioning it to a friend. Randomly choose a unique greeting (Hi, Hello, Hey, etc.) and use their name or title naturally. Vary your response so it sounds human and not like a bot. IMPORTANT: Output ONLY the actual reminder message as you would say it to a friend. DO NOT include meta-talk like ''Sure, here is your reminder'' or ''Certainly!''. Just start speaking to them immediately with no technical labels or any prefixes.', 'string', 'Scheduler Prompt (placeholders: {job_name}, {task}, {user_name}, {user_title})', 'scheduler', datetime('now'));

-- ── Built-in tool routing patterns ──────────────────────────────────────────
INSERT OR IGNORE INTO tool_patterns (tool_name, pattern, description) VALUES
    ('web_search_tool', '\blook\s+it\s+up\b',                       'Look it up'),
    ('web_search_tool', '\bsearch\s+(the\s+web|online)\s+for\b',    'Explicit web search'),
    ('web_search_tool', '\bwhat.s\s+the\s+latest\b',                'Latest news query'),
    ('web_search_tool', '\bcurrent\s+(price|news|score|status)\b',  'Current info query'),
    ('gmail_tool',      '\b(my\s+)?unread\s+(emails?|messages?)\b', 'Unread email check'),
    ('gmail_tool',      '\bcheck\s+my\s+(inbox|email)\b',           'Inbox check'),
    ('gmail_tool',      '\bsend\s+an?\s+email\b',                   'Send email'),
    ('ssh_tool',        '\bmy\s+(linux\s+)?server\b',               'Server reference'),
    ('ssh_tool',        '\btail\s+(the\s+)?logs?\b',                'Log tailing'),
    ('file_tool',       '\bread\s+(the\s+)?(file|document|pdf)\b',  'Read file'),
    ('file_tool',       '\bparse\s+(the\s+)?(pdf|xlsx|docx|csv)\b', 'Parse document'),
    ('agent_memory_tool', '\bdo\s+you\s+remember\b',                'Memory recall'),
    ('agent_memory_tool', '\bremember\s+that\b',                    'Memory store'),
    ('ssh_tool',          '\bssh\b',                                'SSH command'),
    ('ssh_tool',          '\bremote\s+server\b',                    'Remote server reference'),
    ('shell_tool',        '\bshell\b',                              'Shell command'),
    ('shell_tool',        '\bbash\b',                               'Bash command'),
    ('shell_tool',        '\bterminal\b',                           'Terminal command'),
    ('shell_tool',        '\b(run|execute)\s+(a\s+|the\s+|this\s+)?(command|script)\b', 'Run local command'),
    ('shell_tool',        '\blocal\s+(server|machine|host|system|files?|folders?|director(y|ies)|disk)\b', 'Local host reference'),
    ('shell_tool',        '\b(disk\s+space|memory\s+usage|cpu\s+usage|uptime)\b', 'Local system health'),
    ('cron_job_tool',     '\bcron(job|tab)?\b',                     'Cron job reference'),
    ('cron_job_tool',     '\bschedule\b',                           'Schedule task reference'),
    ('cron_job_tool',     '\bevery\s+(minute|hour|day|week|month|year|\d+)\b', 'Periodic task reference'),
    ('gmail_list',       '\b(my\s+)?(gmail|email|inbox)\b',         'Gmail inbox reference'),
    ('gmail_list',       '\b(unread|new)\s+(emails?|messages?)\b',  'Unread Gmail'),
    ('gmail_send',       '\bsend\s+(an?\s+)?(gmail|email)\b',       'Send Gmail'),
    ('gmail_search',     '\bsearch\s+(my\s+)?(gmail|email)\b',      'Search Gmail'),
    ('outlook_list_emails', '\b(my\s+)?outlook\b',                  'Outlook reference'),
    ('outlook_list_emails', '\boutlook\s+(inbox|email)\b',           'Outlook inbox'),
    ('outlook_send_email',  '\bsend\s+(.*\s+)?outlook\b',           'Send via Outlook'),
    ('outlook_search',      '\bsearch\s+(my\s+)?outlook\b',         'Search Outlook'),
    ('mscal_list_events', '\b(microsoft|ms)\s+calendar\b',          'Microsoft Calendar'),
    ('mscal_list_events', '\boutlook\s+calendar\b',                 'Outlook Calendar'),
    ('mscal_create_event', '\b(create|add|new)\s+(.*\s+)?meeting\b','Create meeting'),
    ('gcal_list_events',  '\bgoogle\s+calendar\b',                  'Google Calendar'),
    ('gcal_list_events',  '\b(my\s+)?(calendar|events?|meetings?|appointments?)\b', 'Calendar reference'),
    ('gcal_create_event', '\b(create|add|schedule)\s+(.*\s+)?(event|meeting|appointment)\b', 'Create calendar event'),
    ('gdrive_list',       '\bgoogle\s+drive\b',                     'Google Drive reference'),
    ('gdrive_search',     '\bsearch\s+(my\s+)?drive\b',             'Search Drive'),
    ('gdrive_upload_text','\bupload\s+to\s+drive\b',                'Upload to Drive'),
    ('gdrive_move_file',  '\bmove\s+(a\s+)?(google\s+)?drive\s+file\b', 'Move Drive file'),
    ('onedrive_list',     '\bone\s*drive\b',                        'OneDrive reference'),
    ('onedrive_search',   '\bsearch\s+(my\s+)?one\s*drive\b',       'Search OneDrive'),
    ('onedrive_move_file','\bmove\s+(a\s+)?one\s*drive\s+file\b',   'Move OneDrive file'),
    ('gmail_add_label',   '\badd\s+label\b',                        'Add Gmail label'),
    ('gmail_remove_label','\bremove\s+label\b',                     'Remove Gmail label'),
    ('outlook_download_attachment', '\bdownload\s+attachment\b',    'Download Outlook attachment'),
    ('fb_list_messenger_chats', '\bfacebook\s+(chats?|messages?|inbox|messenger)\b', 'Facebook Messenger chats'),
    ('fb_list_posts',     '\bfacebook\s+posts?\b',                  'Facebook posts'),
    ('fb_get_page',       '\bfacebook\s+(page|insight|analytic)s?\b', 'Facebook page info'),
    ('gcon_list_contacts','\b(my\s+)?(google\s+)?contacts?\b',      'Google Contacts'),
    ('gcon_search_contacts', '\bsearch\s+(my\s+)?(google\s+)?contacts?\b', 'Search Google Contacts'),
    ('gcon_create_contact', '\b(create|add|new)\s+(google\s+)?contact\b', 'Create Google Contact'),
    ('gmeet_get_full_transcript', '\b(meeting\s+)?transcript\b',    'Meet transcript'),
    ('gtasks_list_tasks', '\b(my\s+)?(tasks|to-do|todo)\b',         'Google Tasks'),
    ('gdocs_create',      '\b(create|new)\s+(google\s+)?doc(ument)?\b', 'Create Google Doc'),
    ('gsheets_create',    '\b(create|new)\s+(google\s+)?sheet|spreadsheet\b', 'Create Google Sheet'),
    ('gsheets_read_range','\b(read|get|show|view)\s+(google\s+)?(sheet|spreadsheet|cells?|range)\b', 'Read Google Sheet range'),
    ('gsheets_write_range','\b(write|update|set|put|edit)\s+(google\s+)?(sheet|spreadsheet|cells?|range)\b', 'Write to Google Sheet'),
    ('gsheets_append_rows','\b(append|add)\s+(rows?|data)\s+(to\s+)?(google\s+)?(sheet|spreadsheet)\b', 'Append rows to Sheet'),
    ('gsheets_find',      '\b(search|find|look\s*up)\s+(in\s+)?(google\s+)?(sheet|spreadsheet)\b', 'Search in Google Sheet'),
    ('gslides_create',    '\b(create|new)\s+(google\s+)?(slides|presentation)\b', 'Create Google Slides'),
    ('gchat_send_message','\b(send\s+)?(google\s+)?chat\s+message\b', 'Google Chat message'),
    ('list_workflows',    '\b(list|show|get|see)\s+workflows?\b',   'List workflows'),
    ('run_workflow',      '\b(run|execute|start|trigger)\s+workflow\b', 'Run workflow');
