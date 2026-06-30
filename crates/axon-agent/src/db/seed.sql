-- Default settings + tool-routing patterns.
-- Every statement is INSERT OR IGNORE, so this runs on every boot and only
-- fills in keys that don't exist yet — it never overwrites operator changes.
-- (One-time value corrections live in normalize.sql, not here.)

INSERT OR IGNORE INTO settings VALUES
    ('agent.max_iterations',        '20',    'int',    'Max agent loop iterations per task',            'agent',     datetime('now')),
    ('agent.max_parallel_tools',    '3',     'int',    'Max tools to run in parallel (keep low on small/shared-core hosts)', 'agent', datetime('now')),
    ('agent.tool_timeout_secs',     '30',    'int',    'Python tool subprocess timeout seconds',         'agent',     datetime('now')),
    ('agent.allow_tool_writing',    'true',  'bool',   'Allow agent to write temporary tools',           'agent',     datetime('now')),
    ('agent.temp_tool_max_retries', '2',     'int',    'Retries for agent-written tools',               'agent',     datetime('now')),
    ('agent.stream_model_tokens',   'false', 'bool',   'Reserved switch for provider token streaming; disabled by default', 'agent', datetime('now')),
    ('agent.quality_check',         'true',  'bool',   'Run quality check on responses that used tools (requires quality_checker model role)', 'agent', datetime('now')),
    ('router.error_threshold',      '2',     'int',    'Consecutive non-rate-limit errors before parking a model until midnight', 'router', datetime('now')),
    ('router.model_call_timeout_secs','30',  'int',    'Flat per-attempt model timeout in seconds (overridable per model); on timeout the router fails over immediately', 'router', datetime('now')),
    ('memory.short_term_max_msgs',  '50',    'int',    'Max messages kept per session',                 'memory',    datetime('now')),
    ('memory.long_term_top_k',      '5',     'int',    'Memories injected per agent call',              'memory',    datetime('now')),
    ('scheduler.max_jobs',          '100',   'int',    'Maximum active scheduled jobs',                 'scheduler', datetime('now')),
    ('scheduler.follow_up_retries', '3',     'int',    'Follow-up attempts before abandoning task',     'scheduler', datetime('now')),
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
    ('workflow.max_concurrent_runs',          '16',   'int',  'Maximum workflow runs executing at once (bounds CPU/memory under trigger bursts; applied at startup)', 'workflow', datetime('now')),
    ('workflow.max_queue_depth',              '500',  'int',  'Maximum runs queued waiting to execute before new trigger fires are shed (0 = unbounded queue)', 'workflow', datetime('now')),
    ('workflow.public_base_url',              '',     'string','Public HTTPS base URL used to build resume/approve/reject links for Wait-for-webhook & Approval nodes (blank = emit relative paths)', 'workflow', datetime('now')),
    ('workflow.resume_token_default_ttl_secs','0',    'int', 'Default lifetime of a Wait-for-webhook/Approval resume token when the node sets no explicit timeout (0 = wait forever — the C1 contract; set >0 for a fallback auto-timeout)', 'workflow', datetime('now')),
    ('workflow.webhook_dedup_window_secs',    '0',    'int',  'Seconds to dedup generic webhooks by body hash when they send no Idempotency-Key/event_id (0 = off; explicit-key dedup is always on)', 'workflow', datetime('now')),
    ('retention.trigger_dedup_days',          '7',    'int',  'Days of trigger idempotency keys (webhook/github redelivery dedup) kept before pruning', 'retention', datetime('now'));

-- Web search.
INSERT OR IGNORE INTO settings VALUES
    ('websearch.enabled',             'false', 'bool',   'Enable Web Search tool (requires Tavily accounts below)',      'websearch', datetime('now')),
    ('websearch.max_results',         '5',     'int',    'Default max results per search (1-10)',                       'websearch', datetime('now')),
    ('websearch.search_depth',        'basic', 'string', 'Tavily search depth: basic (1 credit) or advanced (2 credits)', 'websearch', datetime('now'));

-- Facebook webhook.
INSERT OR IGNORE INTO settings VALUES
    ('webhook.fb_verify_token',  '',   'string', 'Facebook Webhook Verify Token (set in FB App -> Webhooks)',            'webhook', datetime('now')),
    ('webhook.fb_app_secret',    '',   'string', 'Facebook App Secret (for HMAC signature validation)',                 'webhook', datetime('now')),
    ('webhook.fb_auto_reply',    'true',  'bool',   'Auto-reply to Facebook comments and messages via webhook',         'webhook', datetime('now')),
    ('webhook.fb_reply_prompt',  'You are an AI assistant managing a Facebook page. Reply warmly and helpfully on behalf of the page owner. Keep replies concise (1-3 sentences). Match the language of the commenter/sender. If the comment is spam or just a reaction (amen, nice, emoji-only), respond with EXACTLY: DO_NOT_REPLY. Output ONLY the reply text, nothing else.', 'string', 'System prompt for Facebook auto-replies (editable)', 'webhook', datetime('now')),
    ('webhook.fb_notify_replies','true',  'bool',   'Send notification when Axon auto-replies to Facebook',             'webhook', datetime('now'));

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
    ('ssh_tool',          '\bserver\s*(\d+)?\b',                    'Server reference'),
    ('ssh_tool',          '\bbash\b',                               'Bash command'),
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
