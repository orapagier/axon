use anyhow::Context;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;
use tokio::sync::Mutex;

fn load_env_files() {
    if let Ok(path) = std::env::var("AXON_ENV_FILE") {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            let _ = dotenvy::from_path(&p);
        }
    }

    let _ = dotenvy::dotenv();

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(".env"));
            candidates.push(dir.join("axon-agent.env"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(".env"));
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".env"));
    }

    for path in candidates {
        if path.exists() {
            let _ = dotenvy::from_path(&path);
        }
    }
}

/// Migrate OAuth tokens from tokens.json to SQLite database for persistence across rebuilds
fn migrate_oauth_tokens_to_db(pool: &Pool<SqliteConnectionManager>) {
    // Helper to get data directory (same logic as axon-core)
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("axon-mcp");

    // Look for tokens.json in various locations
    let possible_paths = [
        data_dir.join("tokens.json"),
        std::path::PathBuf::from("tokens.json"),
        dirs::home_dir()
            .map(|h| h.join(".local/share/axon-mcp/tokens.json"))
            .unwrap_or_default(),
    ];

    let tokens_file = possible_paths.iter().find(|p| p.exists());
    let tokens_path = match tokens_file {
        Some(p) => p,
        None => return, // No tokens file to migrate
    };

    let tokens_content = match std::fs::read_to_string(tokens_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Could not read tokens.json for migration: {}", e);
            return;
        }
    };

    let tokens: serde_json::Value = match serde_json::from_str(&tokens_content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Could not parse tokens.json: {}", e);
            return;
        }
    };

    let conn = match pool.get() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("Could not get DB connection for token migration: {}", e);
            return;
        }
    };

    // Migrate Google token
    if let Some(google) = tokens.get("google").and_then(|v| v.as_object()) {
        if let (Some(access), Some(expires)) = (
            google.get("access_token").and_then(|v| v.as_str()),
            google.get("expires_at").and_then(|v| v.as_i64()),
        ) {
            let refresh = google
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = conn.execute(
                "INSERT OR REPLACE INTO oauth_tokens (provider, access_token, refresh_token, expires_at, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                rusqlite::params!["google", access, refresh, expires],
            );
            tracing::info!("Migrated Google OAuth token to database");
        }
    }

    // Migrate Microsoft token
    if let Some(microsoft) = tokens.get("microsoft").and_then(|v| v.as_object()) {
        if let (Some(access), Some(expires)) = (
            microsoft.get("access_token").and_then(|v| v.as_str()),
            microsoft.get("expires_at").and_then(|v| v.as_i64()),
        ) {
            let refresh = microsoft
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = conn.execute(
                "INSERT OR REPLACE INTO oauth_tokens (provider, access_token, refresh_token, expires_at, updated_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                rusqlite::params!["microsoft", access, refresh, expires],
            );
            tracing::info!("Migrated Microsoft OAuth token to database");
        }
    }

    // Migrate Facebook token
    if let Some(facebook) = tokens.get("facebook").and_then(|v| v.as_object()) {
        if let Some(page_token) = facebook.get("page_access_token").and_then(|v| v.as_str()) {
            let user_token = facebook
                .get("user_access_token")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let extra = serde_json::json!({"user_access_token": user_token}).to_string();
            let _ = conn.execute(
                "INSERT OR REPLACE INTO oauth_tokens (provider, access_token, refresh_token, expires_at, extra_data, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
                rusqlite::params!["facebook", page_token, "", 0_i64, extra],
            );
            tracing::info!("Migrated Facebook OAuth token to database");
        }
    }
}

use axon::{
    config::{load_models, load_models_from_db, AppConfig, RuntimeSettings},
    dashboard::build_router,
    mcp::McpManager,
    memory::MemoryStore,
    messaging::{DiscordGateway, MessagingHub, SlackGateway, TelegramGateway},
    router::{RouterState, ToolRouter},
    scheduler::{JobStore, SchedulerEngine},
    state::AppState,
    tools::{web_search::WebSearchTool, FileHandler, ToolRegistry, WorkflowEngine},
    watcher::engine::WatcherEngine,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_env_files();

    if let Ok(otlp_endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        use opentelemetry_otlp::WithExportConfig;
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(otlp_endpoint),
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .expect("Failed to initialize OTLP tracer");

        use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
        let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("axon=info".parse()?)
                    .add_directive("tower_http=info".parse()?),
            )
            .with(tracing_subscriber::fmt::layer())
            .with(telemetry)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("axon=info".parse()?)
                    .add_directive("tower_http=info".parse()?),
            )
            .init();
    }

    tracing::info!("AXON v{} starting...", env!("CARGO_PKG_VERSION"));
    let cfg = AppConfig::from_env();

    for dir in &["memory", "tools", "tools_temp", "data/files"] {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir))?;
    }

    // Clean up staged files on startup and periodically (every 24 hours)
    // Threshold set to 30 days for "permanent" feel
    let retention = std::time::Duration::from_secs(30 * 24 * 3600);
    axon::files::cleanup_old(retention);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
        loop {
            interval.tick().await;
            axon::files::cleanup_old(retention);
        }
    });

    let manager = SqliteConnectionManager::file(&cfg.db_path).with_flags(
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE,
    );
    let pool = Pool::new(manager).context("create SQLite pool")?;
    {
        let conn = pool.get().context("get DB connection")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let schema = std::fs::read_to_string("memory/schema.sql").context("read schema.sql")?;
        conn.execute_batch(&schema).context("apply schema")?;
        let _ = conn.execute("ALTER TABLE mcp_servers ADD COLUMN api_key TEXT", []);
        let _ = conn.execute("ALTER TABLE runs ADD COLUMN parent_run_id TEXT", []);
        let _ = conn.execute("ALTER TABLE runs ADD COLUMN job_id TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE watchers ADD COLUMN trigger_condition TEXT NOT NULL DEFAULT 'on_change'",
            [],
        );
        let _ = conn.execute("ALTER TABLE models ADD COLUMN timeout_secs INTEGER", []);
        // Cleanup duplicate tool_patterns
        let _ = conn.execute(
            "DELETE FROM tool_patterns WHERE rowid NOT IN (SELECT MIN(rowid) FROM tool_patterns GROUP BY tool_name, pattern)",
            [],
        );
        let _ = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_tp_unique ON tool_patterns(tool_name, pattern)",
            [],
        );

        // Use Tool-specific migrations
        let _ = conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS observations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                compressed TEXT NOT NULL,
                raw_size INTEGER,
                model_used TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_obs_run ON observations(run_id);
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

            -- Update settings for existing users
            UPDATE settings SET value='You are Axon, a capable AI agent. Always provide responses in plain text only, no Markdown formatting (no asterisks, no bolding, no code blocks unless essential for data). Complete tasks efficiently using available tools. If a tool is missing and tool writing is enabled, write one. If a task needs follow-up later, schedule it.' 
            WHERE key='agent.system_prompt' AND value LIKE 'You are Axon, a capable AI agent. Complete%';
            
            -- Prevent XML/JSON code block hallucination in weaker models (migration)
            UPDATE settings SET value = value || '
5. CRITICAL: You MUST use the native JSON tool calling mechanism provided by the API. NEVER output raw JSON snippets, markdown code blocks (```json), or XML tags like <tool_call> in your message body. Speak in plain text only.' 
            WHERE key='agent.system_prompt' AND value NOT LIKE '%native JSON tool calling%';

            -- Prevent call:tool{args} hallucination format (migration)
            UPDATE settings SET value = REPLACE(value, 'or XML tags like <tool_call> in your message body', 'XML tags like <tool_call>, or call:tool_name{args} syntax in your message body')
            WHERE key='agent.system_prompt' AND value LIKE '%or XML tags like <tool_call> in your message body%';
            
            UPDATE settings SET value='1' WHERE key='router.rate_limit_cooldown' AND value='60';

            -- Cleanup stale runs stuck in 'running' from previous crashes
            UPDATE runs SET status='failed', result='Terminated: agent restarted', finished_at=datetime('now')
            WHERE status='running';

            CREATE TABLE IF NOT EXISTS ssh_servers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT UNIQUE NOT NULL,
                ip TEXT NOT NULL,
                port INTEGER NOT NULL DEFAULT 22,
                username TEXT NOT NULL,
                auth_type TEXT NOT NULL,
                password TEXT,
                private_key TEXT,
                public_key TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS credentials (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                service TEXT NOT NULL,
                data TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS watcher_emails (
                id              INTEGER  PRIMARY KEY AUTOINCREMENT,
                service         TEXT     NOT NULL,
                email_id        TEXT     NOT NULL UNIQUE,
                thread_id       TEXT     NOT NULL DEFAULT '',
                sender_name     TEXT     NOT NULL DEFAULT '',
                sender_email    TEXT     NOT NULL DEFAULT '',
                subject         TEXT     NOT NULL DEFAULT '',
                body            TEXT     NOT NULL DEFAULT '',
                date_received   TEXT     NOT NULL DEFAULT '',
                has_attachments INTEGER  NOT NULL DEFAULT 0,
                reported        INTEGER  NOT NULL DEFAULT 0,
                created_at      DATETIME NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_watcher_emails_email_id ON watcher_emails (email_id);
            CREATE INDEX IF NOT EXISTS idx_watcher_emails_service  ON watcher_emails (service);
            CREATE INDEX IF NOT EXISTS idx_watcher_emails_reported ON watcher_emails (reported);

            CREATE TABLE IF NOT EXISTS watcher_command_results (
                id              INTEGER  PRIMARY KEY AUTOINCREMENT,
                watcher_id      TEXT     NOT NULL,
                watcher_label   TEXT     NOT NULL DEFAULT '',
                result          TEXT     NOT NULL,
                result_hash     TEXT     NOT NULL,
                created_at      DATETIME NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_wcr_watcher_id  ON watcher_command_results (watcher_id);
            CREATE INDEX IF NOT EXISTS idx_wcr_created_at  ON watcher_command_results (watcher_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS http_requests (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                method      TEXT NOT NULL,
                url         TEXT NOT NULL,
                headers     TEXT DEFAULT '{}',
                body        TEXT DEFAULT '',
                "limit"     INTEGER,
                proxy       TEXT,
                next_request_id TEXT,
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS workflows (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                description     TEXT DEFAULT '',
                enabled         INTEGER DEFAULT 1,
                trigger_type    TEXT NOT NULL DEFAULT 'manual',
                trigger_config  TEXT DEFAULT '{}',
                last_run_at     TEXT,
                last_status     TEXT DEFAULT 'idle',
                created_at      TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS workflow_nodes (
                id              TEXT PRIMARY KEY,
                workflow_id     TEXT NOT NULL,
                position        INTEGER NOT NULL DEFAULT 0,
                position_x      REAL NOT NULL DEFAULT 0.0,
                position_y      REAL NOT NULL DEFAULT 0.0,
                node_type       TEXT NOT NULL DEFAULT 'synapse',
                name            TEXT NOT NULL DEFAULT 'Step',
                config          TEXT NOT NULL DEFAULT '{}',
                enabled         INTEGER DEFAULT 1,
                continue_on_fail INTEGER DEFAULT 0,
                created_at      TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_wn_workflow ON workflow_nodes(workflow_id, position);

            CREATE TABLE IF NOT EXISTS workflow_edges (
                id              TEXT PRIMARY KEY,
                workflow_id     TEXT NOT NULL,
                source_id       TEXT NOT NULL,
                target_id       TEXT NOT NULL,
                source_handle   TEXT,
                target_handle   TEXT,
                created_at      TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_we_workflow ON workflow_edges(workflow_id);

            CREATE TABLE IF NOT EXISTS workflow_runs (
                id              TEXT PRIMARY KEY,
                workflow_id     TEXT NOT NULL,
                status          TEXT DEFAULT 'running',
                trigger_type    TEXT,
                started_at      TEXT NOT NULL DEFAULT (datetime('now')),
                finished_at     TEXT,
                node_results    TEXT DEFAULT '[]'
            );
            CREATE INDEX IF NOT EXISTS idx_wr_workflow ON workflow_runs(workflow_id);
            CREATE INDEX IF NOT EXISTS idx_wr_status ON workflow_runs(status);
            CREATE INDEX IF NOT EXISTS idx_wr_started ON workflow_runs(started_at DESC);

            -- Cleanup stale workflow_runs stuck in 'running' from previous crashes
            UPDATE workflow_runs SET status='failed', finished_at=datetime('now')
            WHERE status='running';

            CREATE INDEX IF NOT EXISTS idx_runs_status ON runs(status);
            CREATE INDEX IF NOT EXISTS idx_runs_created ON runs(created_at DESC);
        "#);

        // Migration: Add columns if they don't exist (legacy http_requests)
        let _ = conn.execute("ALTER TABLE http_requests ADD COLUMN \"limit\" INTEGER", []);
        let _ = conn.execute("ALTER TABLE http_requests ADD COLUMN proxy TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE http_requests ADD COLUMN next_request_id TEXT",
            [],
        );

        // Migration: Add coordinates for visual DAG editor
        let _ = conn.execute(
            "ALTER TABLE workflow_nodes ADD COLUMN position_x REAL NOT NULL DEFAULT 0.0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE workflow_nodes ADD COLUMN position_y REAL NOT NULL DEFAULT 0.0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE workflow_nodes ADD COLUMN continue_on_fail INTEGER DEFAULT 0",
            [],
        );

        // Legacy http_requests to workflows auto-migration removed to prevent re-populating user's deleted workflow instances on server re-deployment.

        // Insert MCP tool patterns for proper routing
        let mcp_patterns = [
            (
                "gmail_list",
                r"\b(my\s+)?(gmail|email|inbox)\b",
                "Gmail inbox reference",
            ),
            (
                "gmail_list",
                r"\b(unread|new)\s+(emails?|messages?)\b",
                "Unread Gmail",
            ),
            (
                "gmail_send",
                r"\bsend\s+(an?\s+)?(gmail|email)\b",
                "Send Gmail",
            ),
            (
                "gmail_search",
                r"\bsearch\s+(my\s+)?(gmail|email)\b",
                "Search Gmail",
            ),
            (
                "outlook_list_emails",
                r"\b(my\s+)?outlook\b",
                "Outlook reference",
            ),
            (
                "outlook_list_emails",
                r"\boutlook\s+(inbox|email)\b",
                "Outlook inbox",
            ),
            (
                "outlook_send_email",
                r"\bsend\s+(.*\s+)?outlook\b",
                "Send via Outlook",
            ),
            (
                "outlook_search",
                r"\bsearch\s+(my\s+)?outlook\b",
                "Search Outlook",
            ),
            (
                "mscal_list_events",
                r"\b(microsoft|ms)\s+calendar\b",
                "Microsoft Calendar",
            ),
            (
                "mscal_list_events",
                r"\boutlook\s+calendar\b",
                "Outlook Calendar",
            ),
            (
                "mscal_create_event",
                r"\b(create|add|new)\s+(.*\s+)?meeting\b",
                "Create meeting",
            ),
            (
                "gcal_list_events",
                r"\bgoogle\s+calendar\b",
                "Google Calendar",
            ),
            (
                "gcal_list_events",
                r"\b(my\s+)?(calendar|events?|meetings?|appointments?)\b",
                "Calendar reference",
            ),
            (
                "gcal_create_event",
                r"\b(create|add|schedule)\s+(.*\s+)?(event|meeting|appointment)\b",
                "Create calendar event",
            ),
            (
                "gdrive_list",
                r"\bgoogle\s+drive\b",
                "Google Drive reference",
            ),
            (
                "gdrive_search",
                r"\bsearch\s+(my\s+)?drive\b",
                "Search Drive",
            ),
            (
                "gdrive_move_file",
                r"\bmove\s+(a\s+)?(google\s+)?drive\s+file\b",
                "Move Drive file",
            ),
            ("onedrive_list", r"\bone\s*drive\b", "OneDrive reference"),
            (
                "onedrive_search",
                r"\bsearch\s+(my\s+)?one\s*drive\b",
                "Search OneDrive",
            ),
            (
                "onedrive_move_file",
                r"\bmove\s+(a\s+)?one\s*drive\s+file\b",
                "Move OneDrive file",
            ),
            ("gmail_add_label", r"\badd\s+label\b", "Add Gmail label"),
            (
                "gmail_remove_label",
                r"\bremove\s+label\b",
                "Remove Gmail label",
            ),
            (
                "outlook_download_attachment",
                r"\bdownload\s+attachment\b",
                "Download Outlook attachment",
            ),
            (
                "fb_list_messenger_chats",
                r"\bfacebook\s+(chats?|messages?|inbox|messenger)\b",
                "Facebook Messenger chats",
            ),
            ("fb_list_posts", r"\bfacebook\s+posts?\b", "Facebook posts"),
            (
                "fb_get_page",
                r"\bfacebook\s+(page|insight|analytic)s?\b",
                "Facebook page info",
            ),
            (
                "gcon_list_contacts",
                r"\b(my\s+)?(google\s+)?contacts?\b",
                "Google Contacts",
            ),
            (
                "gcon_search_contacts",
                r"\bsearch\s+(my\s+)?(google\s+)?contacts?\b",
                "Search Google Contacts",
            ),
            (
                "gcon_create_contact",
                r"\b(create|add|new)\s+(google\s+)?contact\b",
                "Create Google Contact",
            ),
            (
                "gmeet_get_full_transcript",
                r"\b(meeting\s+)?transcript\b",
                "Meet transcript",
            ),
            (
                "gtasks_list_tasks",
                r"\b(my\s+)?(tasks|to-do|todo)\b",
                "Google Tasks",
            ),
            (
                "gdocs_create",
                r"\b(create|new)\s+(google\s+)?doc(ument)?\b",
                "Create Google Doc",
            ),
            (
                "gsheets_create",
                r"\b(create|new)\s+(google\s+)?sheet|spreadsheet\b",
                "Create Google Sheet",
            ),
            (
                "gsheets_read_range",
                r"\b(read|get|show|view)\s+(google\s+)?(sheet|spreadsheet|cells?|range)\b",
                "Read Google Sheet range",
            ),
            (
                "gsheets_write_range",
                r"\b(write|update|set|put|edit)\s+(google\s+)?(sheet|spreadsheet|cells?|range)\b",
                "Write to Google Sheet",
            ),
            (
                "gsheets_append_rows",
                r"\b(append|add)\s+(rows?|data)\s+(to\s+)?(google\s+)?(sheet|spreadsheet)\b",
                "Append rows to Sheet",
            ),
            (
                "gsheets_find",
                r"\b(search|find|look\s*up)\s+(in\s+)?(google\s+)?(sheet|spreadsheet)\b",
                "Search in Google Sheet",
            ),
            (
                "gslides_create",
                r"\b(create|new)\s+(google\s+)?(slides|presentation)\b",
                "Create Google Slides",
            ),
            (
                "gchat_send_message",
                r"\b(send\s+)?(google\s+)?chat\s+message\b",
                "Google Chat message",
            ),
            (
                "list_workflows",
                r"\b(list|show|get|see)\s+workflows?\b",
                "List workflows",
            ),
            (
                "run_workflow",
                r"\b(run|execute|start|trigger)\s+workflow\b",
                "Run workflow",
            ),
        ];
        for (tool, pattern, desc) in &mcp_patterns {
            let _ = conn.execute(
                "INSERT OR IGNORE INTO tool_patterns (tool_name, pattern, description, enabled) VALUES (?1, ?2, ?3, 1)",
                rusqlite::params![tool, pattern, desc],
            );
        }

        // Add quality_check setting for existing databases
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('agent.quality_check', 'true', 'bool', 'Run quality check on responses that used tools (requires quality_checker model role)', 'agent', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('agent.request_timeout_secs', '45', 'int', 'Total wall-clock budget in seconds for one agent run', 'agent', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('agent.request_timeout_max_secs', '120', 'int', 'Hard cap for adaptive per-LLM fallback-chain timeout in seconds', 'agent', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('agent.min_model_chain_secs', '60', 'int', 'Guaranteed minimum per-call budget in seconds to prevent starving slow models late in a run', 'agent', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('agent.stream_model_tokens', 'false', 'bool', 'Reserved switch for provider token streaming; disabled by default', 'agent', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('router.model_call_timeout_secs', '20', 'int', 'Default per-call model timeout in seconds when a model-specific timeout is not set', 'router', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('router.model_health_check_interval_secs', '90', 'int', 'Background model health-check cadence in seconds', 'router', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('router.model_call_timeout_min_secs', '10', 'int', 'Minimum per-attempt timeout in seconds used by adaptive model timeout logic', 'router', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('router.model_call_timeout_max_secs', '90', 'int', 'Maximum per-attempt timeout in seconds used by adaptive model timeout logic', 'router', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('router.model_call_timeout_per_1k_chars_secs', '3', 'int', 'Extra timeout seconds added per 1k prompt characters for adaptive model timeout', 'router', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('router.model_call_timeout_fair_share_grace_secs', '4', 'int', 'Extra seconds above fair-share budget for each model attempt during fallback routing', 'router', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('instagram.public_base_url', 'https://mcp.yourdomain.com', 'string', 'Public HTTPS base URL used by axon-mcp for temporary local media links', 'instagram', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('instagram.bind_addr', '0.0.0.0:8080', 'string', 'Bind address for axon-mcp HTTP server (restart axon-mcp after changing)', 'instagram', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('instagram.media_url_ttl_secs', '7200', 'int', 'Temporary local-media URL TTL in seconds', 'instagram', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('instagram.image_poll_interval_secs', '2', 'int', 'Image container status poll interval in seconds', 'instagram', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('instagram.image_poll_timeout_secs', '60', 'int', 'Image container wait timeout in seconds before publish', 'instagram', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('instagram.video_poll_interval_secs', '10', 'int', 'Video/Reels container status poll interval in seconds', 'instagram', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('instagram.video_poll_timeout_secs', '600', 'int', 'Video/Reels container wait timeout in seconds before publish', 'instagram', datetime('now'))",
            [],
        );

        // Seed new watcher settings for existing databases
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('watcher.user_name', 'Jelmar', 'string', 'Owner name for personalized notifications', 'watcher', datetime('now'))",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('watcher.user_title', 'Pastor', 'string', 'Owner title/role for personalized notifications', 'watcher', datetime('now'))",
            [],
        );

        // Fix quiet_hours_end default for existing databases (07:00 → 04:00)
        let _ = conn.execute(
            "UPDATE settings SET value='04:00' WHERE key='watcher.quiet_hours_end' AND value='07:00'",
            [],
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO settings VALUES ('scheduler.nudge_prompt', 'IT IS NOW THE SCHEDULED TIME FOR: **{job_name}**. Task/Reminder: {task}. Act as a close human friend of {user_name} (who is also a {user_title}). Remind them about this task in a purely natural, warm, and conversational way, as if you''re just casually mentioning it to a friend. Randomly choose a unique greeting (Hi, Hello, Hey, etc.) and use their name or title naturally. Vary your response so it sounds human and not like a bot. IMPORTANT: Output ONLY the actual reminder message as you would say it to a friend. DO NOT include meta-talk like ''Sure, here is your reminder'' or ''Certainly!''. Just start speaking to them immediately with no technical labels or any prefixes.', 'string', 'Scheduler Prompt (placeholders: {job_name}, {task}, {user_name}, {user_title})', 'scheduler', datetime('now'))",
            [],
        );
    }
    // Web Search Tool migration (after main connection is dropped)
    tracing::info!("Starting WebSearchTool migration...");
    WebSearchTool::migrate(&pool).context("websearch migrate")?;
    tracing::info!("WebSearchTool migration successful.");

    // Migrate OAuth tokens from tokens.json to database for persistence
    migrate_oauth_tokens_to_db(&pool);

    tracing::info!("Database ready: {}", cfg.db_path);

    let db = Arc::new(pool);
    let settings = Arc::new(RuntimeSettings::new(Arc::clone(&db)));

    let models = {
        let conn = db.get().context("get DB connection for model sync")?;

        // Always try to load from TOML as the "Source of Truth" for configuration
        match load_models("config/models.toml") {
            Ok(toml_models) => {
                let mut current_names = Vec::new();
                for m in toml_models {
                    current_names.push(m.name.clone());
                    let _ = conn.execute(
                        "INSERT INTO models (name, provider, model_id, api_key, base_url, timeout_secs, priority, max_tokens, enabled, role) 
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                         ON CONFLICT(name) DO UPDATE SET 
                            provider=excluded.provider, 
                            model_id=excluded.model_id, 
                            api_key=excluded.api_key, 
                            base_url=excluded.base_url, 
                            timeout_secs=excluded.timeout_secs,
                            priority=excluded.priority, 
                            max_tokens=excluded.max_tokens, 
                            enabled=excluded.enabled, 
                            role=excluded.role",
                        rusqlite::params![
                            m.name, m.provider, m.model_id, axon::crypto::encrypt_key(&m.api_key), m.base_url,
                            m.timeout_secs.map(|v| v as i64), m.priority, m.max_tokens, if m.enabled { 1 } else { 0 }, m.role
                        ],
                    );
                }

                // DELETE models from DB that are no longer in TOML to keep it as Source of Truth
                if !current_names.is_empty() {
                    let placeholders = current_names
                        .iter()
                        .map(|_| "?")
                        .collect::<Vec<_>>()
                        .join(",");
                    let query = format!("DELETE FROM models WHERE name NOT IN ({})", placeholders);
                    let _ = conn.execute(&query, rusqlite::params_from_iter(current_names));
                }
            }
            Err(e) => {
                tracing::error!("Failed to load config/models.toml: {:?}", e);
            }
        }

        load_models_from_db(&conn).context("load_models_from_db after sync")?
    };
    tracing::info!("Loaded {} models", models.len());

    // Sync placeholders to DB settings so they appear in the dashboard
    {
        let conn = db.get().context("sync settings")?;
        for m in &models {
            if m.api_key.starts_with("${") && m.api_key.ends_with("}") {
                let key = &m.api_key[2..m.api_key.len() - 1];
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO settings (key, value, value_type, category, description) VALUES (?1, '', 'string', 'providers', ?2)",
                    rusqlite::params![key, format!("API Key for {}", m.name)],
                );
            }
        }
    }

    let messaging = Arc::new(MessagingHub::new());
    let router = Arc::new(Mutex::new(RouterState::new(models)));
    let mut tools = ToolRegistry::new(
        "tools",
        settings.get_int("agent.tool_timeout_secs", 30) as u64,
    )
    .await
    .context("create tool registry")?;
    tools.load_dir("tools_temp").await.ok();

    let tool_router = Arc::new(ToolRouter::new(
        Arc::clone(&db),
        Arc::clone(&router),
        Arc::clone(&settings),
    ));
    tool_router
        .load_patterns()
        .await
        .context("load tool patterns")?;

    let memory = Arc::new(MemoryStore::new(
        Arc::clone(&db),
        settings.get_int("memory.short_term_max_msgs", 50) as usize,
        std::env::var("VOYAGE_API_KEY").ok(),
    ));

    let mcp = Arc::new(McpManager::new());
    tools.set_mcp_manager(Arc::clone(&mcp));
    {
        let conn = db.get()?;
        let mut s = conn
            .prepare("SELECT name, url, api_key FROM mcp_servers WHERE status != 'disconnected'")?;
        let rows: Vec<(String, String, Option<String>)> = s
            .query_map([], |r| {
                let enc_key: Option<String> = r.get(2)?;
                let dec_key = enc_key.map(|k| axon::crypto::decrypt_key(&k));
                Ok((r.get(0)?, r.get(1)?, dec_key))
            })?
            .filter_map(|r| r.ok())
            .collect();
        drop(s);
        let mut has_servers = false;
        for (name, url, key) in rows {
            has_servers = true;
            match mcp.connect(&name, &url, key).await {
                Ok(ts) => {
                    for t in ts {
                        tools.register(t).await;
                    }
                    tracing::info!("Reconnected MCP '{}'", name);
                }
                Err(e) => tracing::warn!("MCP reconnect '{}' failed: {}", name, e),
            }
        }

        if !has_servers {
            tracing::info!("No MCP servers in DB, auto-connecting to local axon-mcp...");
            let _ = db.get().unwrap().execute(
                "INSERT OR IGNORE INTO mcp_servers (name, url, status) VALUES ('axon-mcp', 'http://127.0.0.1:8080/sse', 'connected')",
                [],
            );
            // Also fix any cached wrong port (3001 -> 8080)
            let _ = db.get().unwrap().execute(
                "UPDATE mcp_servers SET url = 'http://127.0.0.1:8080/sse', status = 'connected' WHERE name = 'axon-mcp' AND url LIKE '%:3001/%'",
                [],
            );
            match mcp
                .connect("axon-mcp", "http://127.0.0.1:8080/sse", None)
                .await
            {
                Ok(ts) => {
                    for t in ts {
                        tools.register(t).await;
                    }
                    tracing::info!("Auto-connected local MCP");
                }
                Err(e) => tracing::warn!("Auto-connect local MCP failed: {}", e),
            }
        }
    }

    let files = Arc::new(FileHandler::new(Arc::clone(&db)).context("create file handler")?);
    let job_store = Arc::new(JobStore::new(Arc::clone(&db)));
    let scheduler = Arc::new(
        SchedulerEngine::new(
            Arc::clone(&job_store),
            Arc::clone(&router),
            Arc::clone(&settings),
            Arc::clone(&messaging),
        )
        .await
        .context("create scheduler")?,
    );

    let (workflow_tx, mut workflow_rx) =
        tokio::sync::mpsc::unbounded_channel::<axon::state::WorkflowCompletion>();
    let workflow_cancellations =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));

    let state = AppState {
        router: Arc::clone(&router),
        tool_router,
        tools,
        memory,
        scheduler: Arc::clone(&scheduler),
        mcp,
        files,
        messaging: Arc::clone(&messaging),
        settings: Arc::clone(&settings),
        db,
        workflow_tx,
        workflow_cancellations,
    };

    // Stored files cleanup (every 6 hours)
    let files_handler = state.files.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(21600));
        loop {
            interval.tick().await;
            let _ = files_handler
                .cleanup_old(std::time::Duration::from_secs(30 * 24 * 3600))
                .await;
        }
    });

    // ── Workflow Agent Processor ──────────────────────────────────────────────
    // This consumer breaks the circular dependency between WorkflowEngine and Agent
    // by handling agent tasks asynchronously when a workflow completes.
    {
        let s2 = state.clone();
        tokio::spawn(async move {
            while let Some(msg) = workflow_rx.recv().await {
                tracing::info!(
                    "Workflow '{}' completed. Triggering agent processing with description: {}",
                    msg.workflow_id,
                    msg.description
                );
                let prompt = format!(
                    "Instructions: {}\n\nData to process: {}",
                    msg.description,
                    msg.output.to_string()
                );

                let ctx = axon::agent::RunContext::new(
                    &prompt,
                    "workflow",
                    Some(&format!("wf_{}", msg.workflow_id)),
                    None,
                    None,
                    None,
                    None,
                );

                if let Err(e) = axon::agent::r#loop::run_task(&prompt, &s2, ctx).await {
                    tracing::error!(
                        "Agent processing for workflow '{}' failed: {}",
                        msg.workflow_id,
                        e
                    );
                }
            }
        });
    }

    scheduler.start().await.context("start scheduler")?;
    tokio::spawn(WorkflowEngine::start_background_loop(state.clone()));

    let tg_token = settings.get_str("messaging.telegram_token", "");
    let tg = if tg_token.is_empty() {
        std::env::var("TELOXIDE_TOKEN").unwrap_or_default()
    } else {
        tg_token
    };
    if !tg.is_empty() {
        let gw = Arc::new(TelegramGateway::new(tg));
        {
            let mut h = messaging.telegram.lock().await;
            *h = Some(Arc::clone(&gw));
        }
        let s2 = Arc::new(state.clone());
        tokio::spawn(async move { gw.start_polling(s2).await });
    }

    let dc_token = settings.get_str("messaging.discord_token", "");
    let dc = if dc_token.is_empty() {
        std::env::var("DISCORD_TOKEN").unwrap_or_default()
    } else {
        dc_token
    };
    if !dc.is_empty() {
        let gw = Arc::new(DiscordGateway::new(dc));
        {
            let mut h = messaging.discord.lock().await;
            *h = Some(Arc::clone(&gw));
        }
        let s2 = Arc::new(state.clone());
        tokio::spawn(async move { gw.start_gateway(s2).await });
    }

    let sl_token = settings.get_str("messaging.slack_token", "");
    let sl = if sl_token.is_empty() {
        std::env::var("SLACK_BOT_TOKEN").unwrap_or_default()
    } else {
        sl_token
    };
    if !sl.is_empty() {
        let gw = Arc::new(SlackGateway::new(sl));
        {
            let mut h = messaging.slack.lock().await;
            *h = Some(Arc::clone(&gw));
        }
        tracing::info!("Slack webhook active at POST /api/slack/events");
    }

    // ── Smart Notifications Watcher ────────────────────────────────────────────
    let watcher = Arc::new(WatcherEngine::new(
        Arc::clone(&state.db),
        Arc::clone(&state.router),
        Arc::clone(&settings),
        Arc::clone(&messaging),
        Arc::clone(&state.memory),
        state.tools.clone(),
    ));
    watcher.start(state.clone()).await;
    tracing::info!("Watcher engine initialized (enable via dashboard settings)");

    tokio::spawn(axon::router::model_router::start_health_checker(
        state.router.clone(),
        state.settings.clone(),
    ));
    tracing::info!("Model router health checker initialized");

    let app = build_router(state);
    let addr = format!("0.0.0.0:{}", cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {}", addr))?;
    tracing::info!("Dashboard: http://localhost:{}", cfg.port);
    tracing::info!("Axon ready ✓");

    let shutdown_signal = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
        tracing::info!("Shutdown signal received. Shutting down gracefully...");
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .context("serve")?;

    opentelemetry::global::shutdown_tracer_provider();
    Ok(())
}
