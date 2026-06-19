mod api;
mod config;
mod error;
mod pool;
mod proxy;

use axum::{
    http::Method,
    response::{Html, IntoResponse},
    routing::{any, delete, get, post, put},
    Json, Router,
};
use config::{load_config, load_env_overrides};
use pool::{build_pool, GlobalPool};
use reqwest::Client;
use serde_json::json;
use std::{collections::HashMap, env, sync::Arc};
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};

// ─────────────────────────────────────────────
//  App state
// ─────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub pool: Arc<RwLock<Arc<GlobalPool>>>,
    pub config: Arc<RwLock<config::Config>>,
    pub config_path: String,
    pub env_path: String,
    pub http: Client,
    pub proxy_secret: String,
    /// Runtime env-var overrides — replaces unsafe env::set_var (#3)
    pub env_overrides: Arc<RwLock<HashMap<String, String>>>,
}

// ─────────────────────────────────────────────
//  Main
// ─────────────────────────────────────────────

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "axon_api_proxy=info".into()),
        )
        .init();

    let config_path = env::var("MODELS_TOML").unwrap_or_else(|_| "models.toml".into());
    let env_path = env::var("ENV_FILE").unwrap_or_else(|_| ".env".into());

    // Load config without fragile line-by-line TOML hack (#6)
    let cfg = load_config(&config_path);

    // Load .env into a HashMap for runtime resolution (#3)
    let env_overrides = load_env_overrides(&env_path);

    let pool = build_pool(&cfg, &env_overrides);
    info!(
        "Loaded {} model(s), {} active in pool",
        cfg.models.len(),
        pool.slots.len()
    );

    let proxy_secret = env::var("AXON_API_KEY").unwrap_or_else(|_| {
        warn!("AXON_API_KEY not set — proxy and management API are open");
        String::new()
    });

    let port = env::var("PORT").unwrap_or_else(|_| "5000".to_string());

    let state = AppState {
        pool: Arc::new(RwLock::new(Arc::new(pool))),
        config: Arc::new(RwLock::new(cfg)),
        config_path,
        env_path,
        http: Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            // Connection pool tuning (#16)
            .pool_max_idle_per_host(20)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(60))
            .build()
            .unwrap(),
        proxy_secret,
        env_overrides: Arc::new(RwLock::new(env_overrides)),
    };

    let cors = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any)
        .allow_origin(Any)
        .expose_headers([axum::http::HeaderName::from_static("x-axon-proxy-slot")]);

    let app = Router::new()
        // Dashboard (#11 — embedded with static fallback)
        .route("/", get(dashboard))
        // Config API — all authenticated via check_auth (#2)
        .route("/api/config", get(api::api_get_config))
        .route("/api/models", post(api::api_add_model))
        .route("/api/models/:name", put(api::api_edit_model))
        .route("/api/models/:name", delete(api::api_delete_model))
        .route("/api/models/:name/toggle", post(api::api_toggle_model))
        .route("/api/providers", post(api::api_upsert_provider))
        .route("/api/providers/:name", delete(api::api_delete_provider))
        // Proxy
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/v1/models", get(proxy::list_models))
        .route("/v1/*path", any(proxy::proxy_handler))
        .route("/chat/completions", any(proxy::proxy_handler))
        .with_state(state)
        .layer(cors);

    let addr = format!("0.0.0.0:{}", port);
    info!("🚀  axon-api-proxy  →  http://localhost:{port}/");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ─────────────────────────────────────────────
//  Dashboard (#11 — try static file, fallback to embedded)
// ─────────────────────────────────────────────

async fn dashboard() -> impl IntoResponse {
    // Check for a runtime static file first (for dev iteration without recompilation)
    if let Ok(content) = tokio::fs::read_to_string("static/dashboard.html").await {
        return Html(content);
    }
    Html(include_str!("dashboard.html").to_string())
}
