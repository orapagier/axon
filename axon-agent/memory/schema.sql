CREATE TABLE IF NOT EXISTS short_term (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role       TEXT NOT NULL,
    content    TEXT NOT NULL,
    tool_name  TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_st_session ON short_term(session_id, created_at);

CREATE TABLE IF NOT EXISTS long_term (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    content    TEXT NOT NULL,
    embedding  BLOB,
    source     TEXT,
    tags       TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE VIRTUAL TABLE IF NOT EXISTS long_term_fts
    USING fts5(content, content=long_term, content_rowid=id);
CREATE TRIGGER IF NOT EXISTS long_term_fts_insert AFTER INSERT ON long_term BEGIN
    INSERT INTO long_term_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS long_term_fts_delete AFTER DELETE ON long_term BEGIN
    INSERT INTO long_term_fts(long_term_fts, rowid, content) VALUES('delete', old.id, old.content);
END;

CREATE TABLE IF NOT EXISTS runs (
    id           TEXT PRIMARY KEY,
    task         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'running',
    result       TEXT,
    iterations   INTEGER DEFAULT 0,
    total_tokens INTEGER DEFAULT 0,
    models_used  TEXT,
    tools_used   TEXT,
    platform     TEXT DEFAULT 'dashboard',
    session_id   TEXT,
    job_id       TEXT,
    parent_run_id TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at  TEXT
);

CREATE TABLE IF NOT EXISTS run_iterations (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    iteration   INTEGER NOT NULL,
    model_name  TEXT NOT NULL,
    tokens      INTEGER NOT NULL,
    tier        TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id          TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES runs(id),
    tool_name   TEXT NOT NULL,
    args        TEXT,
    result      TEXT,
    error       TEXT,
    duration_ms INTEGER,
    parallel    INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_tc_run ON tool_calls(run_id);

CREATE TABLE IF NOT EXISTS jobs (
    id             TEXT PRIMARY KEY,
    name           TEXT NOT NULL,
    task           TEXT NOT NULL,
    schedule_nl    TEXT NOT NULL,
    cron_expr      TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'active',
    created_by     TEXT DEFAULT 'user',
    platform       TEXT DEFAULT 'dashboard',
    chat_id        TEXT,
    parent_run_id  TEXT,
    max_runs       INTEGER,
    run_count      INTEGER DEFAULT 0,
    last_run_at    TEXT,
    next_run_at    TEXT,
    last_result    TEXT,
    stop_condition TEXT,
    created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS job_fire_locks (
    job_id      TEXT NOT NULL,
    slot_key    TEXT NOT NULL,
    claimed_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (job_id, slot_key)
);
CREATE INDEX IF NOT EXISTS idx_job_fire_locks_claimed_at ON job_fire_locks(claimed_at);

CREATE TABLE IF NOT EXISTS tool_patterns (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_name   TEXT NOT NULL,
    pattern     TEXT NOT NULL,
    description TEXT,
    enabled     INTEGER DEFAULT 1,
    created_at  TEXT DEFAULT (datetime('now')),
    UNIQUE(tool_name, pattern)
);
CREATE INDEX IF NOT EXISTS idx_tp_tool ON tool_patterns(tool_name, enabled);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    value_type TEXT NOT NULL,
    description TEXT,
    category   TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS files (
    id          TEXT PRIMARY KEY,
    filename    TEXT NOT NULL,
    mime_type   TEXT,
    path        TEXT NOT NULL,
    direction   TEXT NOT NULL,
    size_bytes  INTEGER,
    platform    TEXT,
    chat_id     TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS mcp_servers (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    url        TEXT NOT NULL,
    api_key    TEXT,
    status     TEXT DEFAULT 'disconnected',
    last_ping  TEXT,
    tools_json TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS models (
    name         TEXT PRIMARY KEY,
    provider     TEXT NOT NULL,
    model_id     TEXT,
    api_key      TEXT NOT NULL,
    base_url     TEXT,
    timeout_secs INTEGER,
    priority     INTEGER DEFAULT 99,
    max_tokens   INTEGER DEFAULT 4096,
    enabled      INTEGER DEFAULT 1,
    role         TEXT,
    created_at   TEXT DEFAULT (datetime('now'))
);

INSERT OR IGNORE INTO settings VALUES
    ('agent.max_iterations',        '20',    'int',    'Max agent loop iterations per task',            'agent',     datetime('now')),
    ('agent.max_parallel_tools',    '5',     'int',    'Max tools to run in parallel',                  'agent',     datetime('now')),
    ('agent.tool_timeout_secs',     '30',    'int',    'Python tool subprocess timeout seconds',         'agent',     datetime('now')),
    ('agent.allow_tool_writing',    'true',  'bool',   'Allow agent to write temporary tools',           'agent',     datetime('now')),
    ('agent.temp_tool_max_retries', '2',     'int',    'Retries for agent-written tools',               'agent',     datetime('now')),
    ('agent.request_timeout_secs',  '45',    'int',    'Total wall-clock budget in seconds for one agent run', 'agent', datetime('now')),
    ('agent.stream_model_tokens',   'false', 'bool',   'Reserved switch for provider token streaming; disabled by default', 'agent', datetime('now')),
    ('agent.quality_check',         'true',  'bool',   'Run quality check on responses that used tools (requires quality_checker model role)', 'agent', datetime('now')),
    ('agent.system_prompt',         'You are Axon, a capable AI agent. Always provide responses in plain text only, no Markdown formatting (no asterisks, no bolding, no code blocks unless essential for data). Complete tasks efficiently using available tools. If a tool is missing and tool writing is enabled, write one. If a task needs follow-up later, schedule it.',
                                             'string', 'Agent system prompt',                           'agent',     datetime('now')),
    ('router.rate_limit_cooldown',  '1',     'int',    'Minutes before retrying a rate-limited model',  'router',    datetime('now')),
    ('router.error_threshold',      '3',     'int',    'Errors before marking a model unavailable',     'router',    datetime('now')),
    ('router.model_call_timeout_secs','20',  'int',    'Default per-call model timeout in seconds when a model-specific timeout is not set', 'router', datetime('now')),
    ('router.model_health_check_interval_secs','90','int','Background model health-check cadence in seconds', 'router', datetime('now')),
    ('memory.short_term_max_msgs',  '50',    'int',    'Max messages kept per session',                 'memory',    datetime('now')),
    ('memory.long_term_top_k',      '5',     'int',    'Memories injected per agent call',              'memory',    datetime('now')),
    ('scheduler.max_jobs',          '100',   'int',    'Maximum active scheduled jobs',                 'scheduler', datetime('now')),
    ('scheduler.follow_up_retries', '3',     'int',    'Follow-up attempts before abandoning task',     'scheduler', datetime('now')),
    ('messaging.telegram_token',    '',      'string', 'Telegram Bot Token (TELOXIDE_TOKEN)',           'messaging', datetime('now')),
    ('messaging.discord_token',     '',      'string', 'Discord Bot Token (DISCORD_TOKEN)',             'messaging', datetime('now')),
    ('messaging.slack_token',       '',      'string', 'Slack Bot Token (SLACK_BOT_TOKEN)',             'messaging', datetime('now')),
    ('instagram.public_base_url',   'https://mcp.yourdomain.com', 'string', 'Public HTTPS base URL used by axon-mcp for temporary local media links', 'instagram', datetime('now')),
    ('instagram.bind_addr',         '0.0.0.0:8080', 'string', 'Bind address for axon-mcp HTTP server (restart axon-mcp after changing)', 'instagram', datetime('now')),
    ('instagram.media_url_ttl_secs','7200',  'int',  'Temporary local-media URL TTL in seconds',        'instagram', datetime('now')),
    ('instagram.image_poll_interval_secs','2','int', 'Image container status poll interval in seconds',  'instagram', datetime('now')),
    ('instagram.image_poll_timeout_secs','60','int', 'Image container wait timeout in seconds before publish', 'instagram', datetime('now')),
    ('instagram.video_poll_interval_secs','10','int','Video/Reels container status poll interval in seconds', 'instagram', datetime('now')),
    ('instagram.video_poll_timeout_secs','600','int','Video/Reels container wait timeout in seconds before publish', 'instagram', datetime('now')),
    ('watcher.user_name',           'boss',  'string', 'Your name for personalized watcher notifications', 'watcher', datetime('now')),
    ('watcher.user_title',          '',      'string', 'Your title (e.g. Pastor, Dr.) for varied greetings', 'watcher', datetime('now')),
    ('watcher.notify_platform',     'telegram','string','Platform to send watcher notifications to',    'watcher',   datetime('now')),
    ('watcher.notify_chat_id',      '',      'string', 'Chat ID to send watcher notifications to',     'watcher',   datetime('now'));

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
    -- Gmail MCP tool patterns
    ('gmail_list',       '\b(my\s+)?(gmail|email|inbox)\b',         'Gmail inbox reference'),
    ('gmail_list',       '\b(unread|new)\s+(emails?|messages?)\b',  'Unread Gmail'),
    ('gmail_send',       '\bsend\s+(an?\s+)?(gmail|email)\b',       'Send Gmail'),
    ('gmail_search',     '\bsearch\s+(my\s+)?(gmail|email)\b',      'Search Gmail'),
    -- Outlook MCP tool patterns
    ('outlook_list_emails', '\b(my\s+)?outlook\b',                  'Outlook reference'),
    ('outlook_list_emails', '\boutlook\s+(inbox|email)\b',           'Outlook inbox'),
    ('outlook_send_email',  '\bsend\s+(.*\s+)?outlook\b',           'Send via Outlook'),
    ('outlook_search',      '\bsearch\s+(my\s+)?outlook\b',         'Search Outlook'),
    -- Microsoft Calendar MCP tool patterns
    ('mscal_list_events', '\b(microsoft|ms)\s+calendar\b',          'Microsoft Calendar'),
    ('mscal_list_events', '\boutlook\s+calendar\b',                 'Outlook Calendar'),
    ('mscal_create_event', '\b(create|add|new)\s+(.*\s+)?meeting\b','Create meeting'),
    -- Google Calendar MCP tool patterns
    ('gcal_list_events',  '\bgoogle\s+calendar\b',                  'Google Calendar'),
    ('gcal_list_events',  '\b(my\s+)?(calendar|events?|meetings?|appointments?)\b', 'Calendar reference'),
    ('gcal_create_event', '\b(create|add|schedule)\s+(.*\s+)?(event|meeting|appointment)\b', 'Create calendar event'),
    -- Google Drive MCP tool patterns
    ('gdrive_list',       '\bgoogle\s+drive\b',                     'Google Drive reference'),
    ('gdrive_search',     '\bsearch\s+(my\s+)?drive\b',             'Search Drive'),
    ('gdrive_upload_text','\bupload\s+to\s+drive\b',                'Upload to Drive'),
    -- OneDrive MCP tool patterns
    ('onedrive_list',     '\bone\s*drive\b',                        'OneDrive reference'),
    ('onedrive_search',   '\bsearch\s+(my\s+)?one\s*drive\b',       'Search OneDrive');
CREATE TABLE IF NOT EXISTS observations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT    NOT NULL,
    tool_name   TEXT    NOT NULL,
    compressed  TEXT    NOT NULL,   -- AI-compressed learning
    raw_size    INTEGER,            -- original byte size before compression
    model_used  TEXT,               -- which compressor model was used
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_obs_run  ON observations(run_id);
CREATE INDEX IF NOT EXISTS idx_obs_tool ON observations(tool_name);

CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts
    USING fts5(compressed, content=observations, content_rowid=id);

CREATE TRIGGER IF NOT EXISTS obs_fts_insert AFTER INSERT ON observations BEGIN
    INSERT INTO observations_fts(rowid, compressed) VALUES (new.id, new.compressed);
END;
CREATE TRIGGER IF NOT EXISTS obs_fts_delete AFTER DELETE ON observations BEGIN
    INSERT INTO observations_fts(observations_fts, rowid, compressed)
    VALUES ('delete', old.id, old.compressed);
END;

-- ── Watchers (Smart Notifications) ──────────────────────────────────────────
CREATE TABLE IF NOT EXISTS watchers (
    id             TEXT PRIMARY KEY,
    service        TEXT NOT NULL,
    tool_name      TEXT DEFAULT '',
    tool_args      TEXT DEFAULT '{}',
    label          TEXT DEFAULT '',
    enabled        INTEGER DEFAULT 1,
    poll_mins      REAL DEFAULT 5,
    last_check     TEXT,
    last_seen_ids  TEXT DEFAULT '[]',
    created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS watcher_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    watcher_id  TEXT NOT NULL,
    new_count   INTEGER DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_wl_watcher ON watcher_log(watcher_id, created_at);

-- ── Watcher Email Cache (for dedup + reply context) ─────────────────────────
CREATE TABLE IF NOT EXISTS watcher_emails (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    service         TEXT NOT NULL,
    email_id        TEXT NOT NULL UNIQUE,
    thread_id       TEXT NOT NULL DEFAULT '',
    sender_name     TEXT NOT NULL DEFAULT '',
    sender_email    TEXT NOT NULL DEFAULT '',
    subject         TEXT NOT NULL DEFAULT '',
    body            TEXT NOT NULL DEFAULT '',
    date_received   TEXT NOT NULL DEFAULT '',
    has_attachments INTEGER NOT NULL DEFAULT 0,
    reported        INTEGER NOT NULL DEFAULT 0,
    created_at      DATETIME NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_watcher_emails_email_id  ON watcher_emails (email_id);
CREATE INDEX IF NOT EXISTS idx_watcher_emails_service   ON watcher_emails (service);
CREATE INDEX IF NOT EXISTS idx_watcher_emails_reported  ON watcher_emails (reported);

-- ── 2. Command/Task watcher result store ──────────────────────────────────────
CREATE TABLE IF NOT EXISTS watcher_command_results (
    id              INTEGER  PRIMARY KEY AUTOINCREMENT,
    watcher_id      TEXT     NOT NULL,
    watcher_label   TEXT     NOT NULL DEFAULT '',
    result          TEXT     NOT NULL,
    result_hash     TEXT     NOT NULL,
    created_at      DATETIME NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_wcr_watcher_id   ON watcher_command_results (watcher_id);
CREATE INDEX IF NOT EXISTS idx_wcr_created_at   ON watcher_command_results (watcher_id, created_at DESC);

INSERT OR IGNORE INTO settings VALUES
    ('watcher.enabled',                'false',     'bool',   'Enable Smart Notifications (auto-poll Gmail, Outlook, Calendar, Facebook)',  'watcher', datetime('now')),
    ('watcher.notify_platform',        'telegram',  'string', 'Platform to send notifications (telegram, discord, slack)',                  'watcher', datetime('now')),
    ('watcher.notify_chat_id',         '',          'string', 'Chat/channel ID for notifications',                                         'watcher', datetime('now')),
    ('watcher.quiet_hours_start',      '22:00',     'string', 'Quiet hours start (HH:MM, no notifications)',                               'watcher', datetime('now')),
    ('watcher.quiet_hours_end',        '04:00',     'string', 'Quiet hours end (HH:MM)',                                                   'watcher', datetime('now')),
    ('watcher.timezone_offset_hours',  '8',         'int',    'UTC offset for quiet hours (e.g., 8 for PHT, -5 for EST)',                   'watcher', datetime('now')),
    ('watcher.user_name',              'Jelmar',    'string', 'Owner name for personalized notifications',                                 'watcher', datetime('now')),
    ('watcher.user_title',             'Pastor',    'string', 'Owner title/role for personalized notifications',                           'watcher', datetime('now'));

CREATE TABLE IF NOT EXISTS web_search_accounts (
    id                  TEXT    PRIMARY KEY,
    name                TEXT    NOT NULL,
    api_key             TEXT    NOT NULL,
    priority            INTEGER NOT NULL DEFAULT 1,
    enabled             INTEGER NOT NULL DEFAULT 1,
    queries_this_month  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_wsa_enabled ON web_search_accounts(enabled, priority);

INSERT OR IGNORE INTO settings VALUES
    ('websearch.enabled',             'false', 'bool',   'Enable Web Search tool (requires Tavily accounts below)',      'websearch', datetime('now')),
    ('websearch.max_results',         '5',     'int',    'Default max results per search (1-10)',                       'websearch', datetime('now')),
    ('websearch.search_depth',        'basic', 'string', 'Tavily search depth: basic (1 credit) or advanced (2 credits)', 'websearch', datetime('now'));

-- ── Webhook Events (real-time from Facebook, etc.) ──────────────────────────
CREATE TABLE IF NOT EXISTS webhook_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source      TEXT NOT NULL,             -- 'facebook'
    event_type  TEXT NOT NULL,             -- 'comment', 'message', 'reaction', 'post'
    from_name   TEXT,                      -- commenter/sender name
    from_id     TEXT,                      -- commenter/sender platform ID
    object_id   TEXT,                      -- post_id, conversation_id, etc.
    parent_id   TEXT,                      -- parent post for comments
    message     TEXT,                      -- comment/message body
    permalink   TEXT,                      -- link to the post/comment
    raw_json    TEXT,                      -- full webhook payload
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    read        INTEGER NOT NULL DEFAULT 0 -- 0=unread, 1=read
);
CREATE INDEX IF NOT EXISTS idx_wh_source ON webhook_events(source, read, created_at);

-- ── OAuth Tokens (persist across rebuilds) ────────────────────────────────────
CREATE TABLE IF NOT EXISTS oauth_tokens (
    provider        TEXT PRIMARY KEY,   -- 'google', 'microsoft', 'facebook'
    access_token    TEXT NOT NULL,
    refresh_token   TEXT,             -- nullable
    expires_at      INTEGER,          -- Unix timestamp ms
    extra_data      TEXT,              -- JSON for provider-specific data (e.g., Facebook page_access_token)
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT OR IGNORE INTO settings VALUES
    ('webhook.fb_verify_token',  '',   'string', 'Facebook Webhook Verify Token (set in FB App → Webhooks)',            'webhook', datetime('now')),
    ('webhook.fb_app_secret',    '',   'string', 'Facebook App Secret (for HMAC signature validation)',                 'webhook', datetime('now')),
    ('webhook.fb_auto_reply',    'true',  'bool',   'Auto-reply to Facebook comments and messages via webhook',         'webhook', datetime('now')),
    ('webhook.fb_reply_prompt',  'You are an AI assistant managing a Facebook page. Reply warmly and helpfully on behalf of the page owner. Keep replies concise (1-3 sentences). Match the language of the commenter/sender. If the comment is spam or just a reaction (amen, nice, emoji-only), respond with EXACTLY: DO_NOT_REPLY. Output ONLY the reply text, nothing else.', 'string', 'System prompt for Facebook auto-replies (editable)', 'webhook', datetime('now')),
    ('webhook.fb_notify_replies','true',  'bool',   'Send notification when Axon auto-replies to Facebook',             'webhook', datetime('now'));
