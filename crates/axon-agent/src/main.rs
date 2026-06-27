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
        axon::db::init(&conn).context("initialize database")?;
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

    // Make integration settings (Instagram base URL, OAuth redirect host, poll
    // timeouts) visible to the in-process MCP services before they're built.
    axon::dashboard::api::apply_mcp_env_from_settings(&settings);

    let mcp = Arc::new(McpManager::new());
    tools.set_mcp_manager(Arc::clone(&mcp));
    {
        // Built-in integrations (Google/Microsoft/Facebook/Instagram/CRM/business)
        // run in-process — no separate process, no SSE hop.
        match mcp.connect_inprocess("axon-mcp").await {
            Ok(ts) => {
                let n = ts.len();
                for t in ts {
                    tools.register(t).await;
                }
                tracing::info!("Loaded {} in-process MCP tools", n);
            }
            Err(e) => tracing::warn!("In-process MCP init failed: {}", e),
        }

        // Reconnect any *external* MCP servers the operator added. Skip any
        // local-internal rows — those are served in-process above.
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
        for (name, url, key) in rows {
            if name == "axon-mcp"
                || url.contains("127.0.0.1:8080")
                || url.contains("localhost:8080")
                || url.contains(":3001/")
            {
                continue; // built-in integrations, now handled in-process
            }
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

    // Database retention sweep: prune append-only history tables so the DB
    // stays bounded. Runs immediately on boot, then daily. The work is blocking
    // SQLite (incl. an occasional VACUUM) so it runs off the async runtime.
    let retention_db = state.db.clone();
    let retention_settings = state.settings.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
        loop {
            interval.tick().await;
            let db = retention_db.clone();
            let settings = retention_settings.clone();
            match tokio::task::spawn_blocking(move || {
                axon::maintenance::run_retention(&db, &settings)
            })
            .await
            {
                Ok(Ok(stats)) => tracing::info!("Retention sweep: {}", stats),
                Ok(Err(e)) => tracing::warn!("Retention sweep failed: {:#}", e),
                Err(e) => tracing::warn!("Retention sweep task join error: {}", e),
            }
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

    if settings.model_health_check_enabled() {
        tokio::spawn(axon::router::model_router::start_health_checker(
            state.router.clone(),
            state.settings.clone(),
        ));
        tracing::info!("Model router health checker initialized");
    } else {
        tracing::info!(
            "Model router health checker disabled (router.model_health_check_enabled=false); \
             routing stays reactive — no proactive quota-consuming pings"
        );
    }

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
