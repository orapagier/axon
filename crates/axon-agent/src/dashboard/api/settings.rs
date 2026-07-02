use super::*;

pub async fn get_settings(State(state): State<AppState>) -> Json<Value> {
    let mut settings = state.settings.all().unwrap_or_default();
    // Hide legacy "providers" category — Models page handles API keys now
    settings.retain(|s| s.category.as_deref() != Some("providers"));
    Json(json!({"settings": settings}))
}

fn instagram_setting_to_mcp_env(key: &str) -> Option<&'static str> {
    match key {
        "instagram.public_base_url" => Some("AXON_PUBLIC_BASE_URL"),
        "instagram.bind_addr" => Some("AXON_BIND_ADDR"),
        "instagram.media_url_ttl_secs" => Some("AXON_MEDIA_URL_TTL_SECS"),
        "instagram.image_poll_interval_secs" => Some("AXON_IG_IMAGE_POLL_INTERVAL_SECS"),
        "instagram.image_poll_timeout_secs" => Some("AXON_IG_IMAGE_POLL_TIMEOUT_SECS"),
        "instagram.video_poll_interval_secs" => Some("AXON_IG_VIDEO_POLL_INTERVAL_SECS"),
        "instagram.video_poll_timeout_secs" => Some("AXON_IG_VIDEO_POLL_TIMEOUT_SECS"),
        _ => None,
    }
}

/// The Instagram/MCP settings that map to process env vars the in-process
/// integration services (and OAuth redirect-URI builder) read.
const MCP_ENV_SETTING_KEYS: &[&str] = &[
    "instagram.public_base_url",
    "instagram.bind_addr",
    "instagram.media_url_ttl_secs",
    "instagram.image_poll_interval_secs",
    "instagram.image_poll_timeout_secs",
    "instagram.video_poll_interval_secs",
    "instagram.video_poll_timeout_secs",
];

/// Apply these settings to the agent's own process environment at startup, so
/// the in-process MCP services see the operator's configuration. Must be called
/// before the in-process backend is constructed. Explicit `.env`/environment
/// values win, and the placeholder base URL is ignored.
pub fn apply_mcp_env_from_settings(settings: &crate::config::RuntimeSettings) {
    for key in MCP_ENV_SETTING_KEYS {
        let Some(env_key) = instagram_setting_to_mcp_env(key) else {
            continue;
        };
        if std::env::var(env_key).is_ok() {
            continue; // respect values already set via .env / environment
        }
        let val = settings.get_str(key, "");
        let val = val.trim();
        if val.is_empty() || val == "https://mcp.yourdomain.com" {
            continue;
        }
        std::env::set_var(env_key, val);
    }
}

fn sync_instagram_setting_to_mcp_env(key: &str, value: &str) {
    // Integrations run in-process now, so apply the change to our own
    // environment instead of writing a separate server's .env file. Values read
    // per-request (OAuth redirect URI, media base URL) take effect immediately;
    // values read at service construction still need an agent restart.
    if let Some(env_key) = instagram_setting_to_mcp_env(key) {
        std::env::set_var(env_key, value.trim());
    }
}

pub async fn update_setting(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    if let (Some(k), Some(v)) = (
        payload.get("key").and_then(|v| v.as_str()),
        payload.get("value").and_then(|v| v.as_str()),
    ) {
        let _ = state.settings.set(k, v);
        sync_instagram_setting_to_mcp_env(k, v);
    }
    Json(json!({"ok":true}))
}

pub async fn update_setting_by_key(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    if let Some(v) = payload.get("value").and_then(|v| v.as_str()) {
        let _ = state.settings.set(&key, v);
        sync_instagram_setting_to_mcp_env(&key, v);
    }
    Json(json!({"ok":true}))
}
