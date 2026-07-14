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
    memory::{embeddings::Embedder, MemoryStore},
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

    // AXON_LOG_FORMAT=json switches to structured JSON log lines (one object
    // per line, easy to ship to a log aggregator) — gated behind an env var so
    // local `cargo run` output stays human-readable by default.
    let json_logs = std::env::var("AXON_LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

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
        let env_filter = tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("axon=info".parse()?)
            .add_directive("tower_http=info".parse()?);

        // `OpenTelemetryLayer` is generic over the subscriber stack below it, so
        // it can't be built once and shared across both branches (json vs.
        // plain fmt layer are different concrete types) — built fresh in each
        // branch instead, off a cloned tracer handle.
        if json_logs {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer().json())
                .with(tracing_opentelemetry::layer().with_tracer(tracer.clone()))
                .init();
        } else {
            tracing_subscriber::registry()
                .with(env_filter)
                .with(tracing_subscriber::fmt::layer())
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .init();
        }
    } else if json_logs {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("axon=info".parse()?)
                    .add_directive("tower_http=info".parse()?),
            )
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

    // D1: fail closed if secrets would be protected only by the public dev key.
    if let Err(e) = axon::crypto::validate_master_key() {
        tracing::error!("{e}");
        anyhow::bail!(e);
    }

    // C3: install the Prometheus recorder before anything emits metrics.
    axon::observability::init();
    let cfg = AppConfig::from_env();

    for dir in &["memory", "tools", "tools_temp"] {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir))?;
    }
    // Shared binary staging dir (honors AXON_DATA_DIR). Every node and the agent
    // read/write here, so a file one saves is found by the sender / Files page.
    let files_dir = axon_core::data_files_dir();
    std::fs::create_dir_all(&files_dir)
        .with_context(|| format!("create {}", files_dir.display()))?;

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

    let manager = SqliteConnectionManager::file(&cfg.db_path)
        .with_flags(
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE,
        )
        // Applied to every connection the pool opens, not just the bootstrap
        // one below. Without busy_timeout, a connection that finds the DB
        // write-locked (WAL still serializes writers) errors immediately
        // instead of waiting — and callers like agent::loop::finalize()
        // swallow that error, silently leaving a run's status stuck at
        // 'running' forever. cache_size caps each connection's page cache at
        // 1MB (SQLite default is 2MB) — across the pool that ceiling matters
        // on the 1GB host.
        .with_init(|c| {
            c.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=10000; PRAGMA cache_size=-1024;",
            )
        });
    // r2d2's default is 10 connections kept open (min_idle defaults to
    // max_size), each with its own page cache. WAL serializes writers anyway,
    // so 5 handles cover this host's concurrency; one stays warm and the rest
    // open on demand and are reaped after the idle timeout.
    let pool = Pool::builder()
        .max_size(5)
        .min_idle(Some(1))
        .build(manager)
        .context("create SQLite pool")?;
    // axon.db holds encrypted credentials, OAuth tokens, and CRM PII (via
    // crm.db) — same 0600-owner-only treatment as tokens.json/credentials.json
    // (axon_core::storage). WAL/SHM siblings carry the same data in-flight.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm"] {
            let p = std::path::PathBuf::from(format!("{}{}", cfg.db_path, suffix));
            if p.exists() {
                if let Err(e) = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))
                {
                    tracing::warn!("Failed to chmod 0600 {}: {}", p.display(), e);
                }
            }
        }
    }
    {
        let conn = pool.get().context("get DB connection")?;
        axon::db::init(&conn).context("initialize database")?;
        // D1: upgrade any pre-KDF (v1) stored secrets to the v2 scheme in place.
        axon::crypto::reencrypt_legacy_secrets(&conn);
        // D1: encrypt the credentials.data JSON blob at rest (was plaintext).
        axon::crypto::encrypt_credentials_at_rest(&conn);
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

    // Seed watcher.notify_chat_id from AXON_NOTIFY_CHAT_ID on first boot only — once
    // set (via this, the dashboard, or a DB restored from a prior deploy) the DB
    // value always wins, so per-instance .env files can pin a default without a
    // dashboard visit while still letting operators override it later.
    if let Ok(chat_id) = std::env::var("AXON_NOTIFY_CHAT_ID") {
        let chat_id = chat_id.trim();
        if !chat_id.is_empty() && settings.get_str("watcher.notify_chat_id", "").is_empty() {
            let _ = settings.set("watcher.notify_chat_id", chat_id);
            tracing::info!("Seeded watcher.notify_chat_id from AXON_NOTIFY_CHAT_ID");
        }
    }

    // Live provider for the CRM's default deal currency (crm.default_currency):
    // read per call so dashboard changes apply without a restart.
    {
        let settings = Arc::clone(&settings);
        axon_crm::set_default_currency_provider(move || settings.crm_default_currency());
    }

    let models = {
        let conn = db.get().context("get DB connection for model sync")?;

        // Insert-only seed: models.toml is the Source of Truth only on a first
        // deploy (empty table). On redeploys the DB wins — existing rows are
        // never overwritten and nothing is pruned; only names new to the file
        // are added. So dashboard (ModelsPage) edits always survive a redeploy.
        match load_models("config/models.toml") {
            Ok(toml_models) => axon::config::sync_toml_models(&conn, toml_models),
            Err(e) => {
                tracing::error!("Failed to load config/models.toml: {:?}", e);
            }
        }

        load_models_from_db(&conn).context("load_models_from_db after sync")?
    };
    tracing::info!("Loaded {} models", models.len());

    // Persist provider API keys into the DB so it becomes their source of truth,
    // exactly like the insert-only models.toml sync. On the first boot that sees
    // a `${VAR}` model, copy the key from the environment (.env) into settings —
    // ENCRYPTED at rest, like model keys. Once a value is stored, resolve()
    // prefers it over the environment, so a later redeploy that ships a changed
    // or stale .env can never clobber a working key. Only a VAR the DB doesn't
    // own yet (missing/empty row) is seeded from .env, and a VAR dropped from
    // .env keeps its stored value. Rotate keys in the dashboard, not .env.
    {
        let conn = db.get().context("sync settings")?;
        for m in &models {
            if m.api_key.starts_with("${") && m.api_key.ends_with("}") {
                let key = &m.api_key[2..m.api_key.len() - 1];
                // Track the key so it's a known setting (value may still be empty).
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO settings (key, value, value_type, category, description) VALUES (?1, '', 'string', 'providers', ?2)",
                    rusqlite::params![key, format!("API Key for {}", m.name)],
                );
                // Seed from .env only while the DB has no value yet — never
                // overwrite one already stored (that's the DB winning on redeploy).
                if let Ok(env_val) = std::env::var(key) {
                    let env_val = env_val.trim();
                    if !env_val.is_empty() {
                        let _ = conn.execute(
                            "UPDATE settings SET value=?2 WHERE key=?1 AND (value IS NULL OR value='')",
                            rusqlite::params![key, axon::crypto::encrypt_key(env_val)],
                        );
                    }
                }
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
        Embedder::from_settings(&settings),
    ));

    // A provider/model switch leaves persisted memory vectors in the old
    // embedding space. Sweep them back into the active one in the background;
    // until a row is re-embedded, search treats it as having no embedding.
    {
        let mem = Arc::clone(&memory);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
            match mem.long.reembed_stale().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(
                    "Memory: re-embedded {} rows for the active embedding model",
                    n
                ),
                Err(e) => tracing::warn!(
                    "Memory re-embed sweep stopped (will resume next boot): {}",
                    e
                ),
            }
        });
    }

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

    // Sensitive write tools (social/CRM/messaging) default to off for the
    // agent regardless of what their source registered — the operator opts
    // each one in via the ToolsPage Enable toggle. Persisted overrides are
    // applied after, so an earlier opt-in survives this restart instead of
    // being reset back off every time.
    tools.apply_agent_gate_defaults().await;
    for (name, enabled) in axon::tools::overrides::load_all(&db) {
        tools.set_enabled(&name, enabled).await;
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

    // B3: size the run semaphore from settings once at startup. Changing the
    // limit takes effect on restart (tokio semaphores don't cleanly shrink).
    let max_concurrent_runs = settings.workflow_max_concurrent_runs().max(1) as usize;
    tracing::info!("Workflow run concurrency limit: {}", max_concurrent_runs);

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
        run_semaphore: Arc::new(tokio::sync::Semaphore::new(max_concurrent_runs)),
        active_runs: Arc::new(std::sync::atomic::AtomicI64::new(0)),
        run_queue_depth: Arc::new(std::sync::atomic::AtomicI64::new(0)),
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

    // Scheduled local backups (axon.db + crm.db): local, on-instance snapshots —
    // NOT off-site disaster recovery on their own. Runs immediately on boot,
    // then daily, same interval-tick pattern as the retention sweep above.
    let backup_db = state.db.clone();
    let backup_settings = state.settings.clone();
    let backup_axon_db_path = std::path::PathBuf::from(cfg.db_path.clone());
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
        loop {
            interval.tick().await;
            if !backup_settings.backup_enabled() {
                tracing::debug!("Backup sweep disabled (backup.enabled=false)");
                continue;
            }
            let retention_days = backup_settings.backup_retention_days();
            match axon::maintenance::run_backup(
                backup_db.clone(),
                backup_axon_db_path.clone(),
                retention_days,
            )
            .await
            {
                Ok(stats) => tracing::info!("Backup sweep: {}", stats),
                Err(e) => tracing::warn!("Backup sweep failed: {:#}", e),
            }
        }
    });

    // Off-instance workflow backups to Google Drive: exports every workflow
    // definition as a portable JSON bundle and pushes it off the box. Opt-in
    // (workflow_backup.enabled) since it needs Google connected. First tick is
    // immediate (a quick confirmation when enabled), then on the configured
    // interval. Same interval-tick pattern as the local backup loop above.
    let wf_backup_state = state.clone();
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(
            wf_backup_state.settings.workflow_backup_interval_hours() as u64 * 3600,
        );
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            if !wf_backup_state.settings.workflow_backup_enabled() {
                tracing::debug!("Workflow Drive backup disabled (workflow_backup.enabled=false)");
                continue;
            }
            match axon::maintenance::run_workflow_drive_backup(&wf_backup_state).await {
                Ok(stats) => tracing::info!("Workflow Drive backup: {}", stats),
                Err(e) => tracing::warn!("Workflow Drive backup failed: {:#}", e),
            }
        }
    });

    // Provider model-list prefetch: populate the ModelsPage "Model ID" dropdown
    // from each provider's live catalogue. Runs once ~shortly after boot (so the
    // dropdown works immediately), then daily, aligned to the next UTC midnight.
    // Read-only against providers; per-provider failures are logged, not fatal.
    {
        let cache_db = state.db.clone();
        let cache_settings = state.settings.clone();
        tokio::spawn(async move {
            // Let the models table + settings settle before the first sweep.
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
            loop {
                let summary =
                    axon::model_cache::refresh_all(cache_db.clone(), cache_settings.clone()).await;
                tracing::info!("Model list prefetch: {}", summary);
                let secs = axon::providers::secs_until_next_utc_midnight().max(1) as u64;
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            }
        });
    }

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

    let app = build_router(state);
    let addr = format!("0.0.0.0:{}", cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {}", addr))?;
    tracing::info!("Dashboard: http://localhost:{}", cfg.port);
    tracing::info!("Axon ready ✓");

    let shutdown_signal = async {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to install Ctrl+C handler");
        };
        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();
        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
        tracing::info!("Shutdown signal received. Shutting down gracefully...");
    };

    // with_connect_info: the rate-limiting layer's SmartIpKeyExtractor falls
    // back to the raw peer IP when no forwarded-for header is present, which
    // needs ConnectInfo<SocketAddr> plumbed through from the listener.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal)
    .await
    .context("serve")?;

    opentelemetry::global::shutdown_tracer_provider();
    Ok(())
}
