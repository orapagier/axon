use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::info;

use crate::{
    config::*,
    error::{internal, ApiErr},
    pool::build_pool,
    AppState,
};

// ─────────────────────────────────────────────
//  Auth check for management API (#2)
// ─────────────────────────────────────────────

pub fn check_auth(headers: &HeaderMap, secret: &str) -> Result<(), ApiErr> {
    if secret.is_empty() {
        return Ok(());
    }
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if auth.trim_start_matches("Bearer ").trim() == secret {
        Ok(())
    } else {
        Err(ApiErr(
            StatusCode::UNAUTHORIZED,
            "Invalid or missing authentication".into(),
        ))
    }
}

// ─────────────────────────────────────────────
//  GET /api/config — masked keys (#1)
// ─────────────────────────────────────────────

pub async fn api_get_config(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, &s.proxy_secret)?;

    let cfg = s.config.read().await;

    let models: Vec<Value> = cfg
        .models
        .iter()
        .map(|m| {
            let env_var = extract_var(&m.api_key).unwrap_or_default();
            // Never send raw API key values (#1)
            let masked = if env_var.is_empty() {
                "(literal) ••••".to_string()
            } else {
                format!("${{{}}}", env_var)
            };
            json!({
                "name": m.name, "provider": m.provider, "model_id": m.model_id,
                "api_key_display": masked, "env_var": env_var,
                "max_tokens": m.max_tokens, "enabled": m.enabled, "role": m.role,
                "base_url": m.base_url, "auth_style": m.auth_style,
                "priority": m.priority, "timeout_secs": m.timeout_secs,
            })
        })
        .collect();

    let providers: Vec<Value> = cfg
        .providers
        .iter()
        .map(|(name, p)| {
            json!({
                "name": name, "base_url": p.base_url, "auth_style": p.auth_style,
            })
        })
        .collect();

    Ok(Json(json!({ "models": models, "providers": providers })))
}

// ─────────────────────────────────────────────
//  Model CRUD
// ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ModelReq {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub auth_style: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u32>,
}

async fn reload_pool(state: &AppState, config: &Config) {
    let env_o = state.env_overrides.read().await;
    let pool = build_pool(config, &env_o);
    info!("Pool reloaded — {} active slot(s)", pool.slots.len());
    *state.pool.write().await = std::sync::Arc::new(pool);
}

pub async fn api_add_model(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ModelReq>,
) -> Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, &s.proxy_secret)?;

    let mut cfg = s.config.write().await;
    if cfg.models.iter().any(|m| m.name == req.name) {
        return Err(ApiErr(
            StatusCode::CONFLICT,
            format!("Model '{}' already exists", req.name),
        ));
    }

    let var = make_env_var(&req.provider, &req.name);
    write_env_var(&s.env_path, &var, &req.api_key).map_err(internal)?;
    // Store in runtime overrides instead of unsafe env::set_var (#3)
    s.env_overrides
        .write()
        .await
        .insert(var.clone(), req.api_key);

    cfg.models.push(ModelEntry {
        name: req.name,
        provider: req.provider,
        model_id: req.model_id,
        api_key: format!("${{{}}}", var),
        role: String::new(),
        priority: req.priority,
        max_tokens: req.max_tokens,
        enabled: req.enabled,
        base_url: req.base_url,
        auth_style: req.auth_style,
        timeout_secs: req.timeout_secs,
    });

    save_config(&s.config_path, &cfg).map_err(internal)?;
    reload_pool(&s, &cfg).await;
    Ok(Json(json!({ "ok": true, "env_var": var })))
}

pub async fn api_edit_model(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(req): Json<ModelReq>,
) -> Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, &s.proxy_secret)?;

    let mut cfg = s.config.write().await;
    let entry = cfg
        .models
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, format!("'{}' not found", name)))?;

    let var = extract_var(&entry.api_key).unwrap_or_else(|| make_env_var(&req.provider, &name));

    if !req.api_key.is_empty() {
        write_env_var(&s.env_path, &var, &req.api_key).map_err(internal)?;
        s.env_overrides
            .write()
            .await
            .insert(var.clone(), req.api_key);
    }

    entry.provider = req.provider;
    entry.model_id = req.model_id;
    entry.api_key = format!("${{{}}}", var);
    entry.priority = req.priority;
    entry.max_tokens = req.max_tokens;
    entry.enabled = req.enabled;
    if req.base_url.is_some() {
        entry.base_url = req.base_url;
    }
    if req.auth_style.is_some() {
        entry.auth_style = req.auth_style;
    }
    if req.timeout_secs.is_some() {
        entry.timeout_secs = req.timeout_secs;
    }

    save_config(&s.config_path, &cfg).map_err(internal)?;
    reload_pool(&s, &cfg).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn api_delete_model(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, &s.proxy_secret)?;

    let mut cfg = s.config.write().await;
    let pos = cfg
        .models
        .iter()
        .position(|m| m.name == name)
        .ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, format!("'{}' not found", name)))?;

    let removed = cfg.models.remove(pos);
    if let Some(var) = extract_var(&removed.api_key) {
        let _ = remove_env_var(&s.env_path, &var);
        s.env_overrides.write().await.remove(&var);
    }

    save_config(&s.config_path, &cfg).map_err(internal)?;
    reload_pool(&s, &cfg).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn api_toggle_model(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, &s.proxy_secret)?;

    let mut cfg = s.config.write().await;
    let entry = cfg
        .models
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, format!("'{}' not found", name)))?;
    entry.enabled = !entry.enabled;
    let enabled = entry.enabled;
    save_config(&s.config_path, &cfg).map_err(internal)?;
    reload_pool(&s, &cfg).await;
    Ok(Json(json!({ "ok": true, "enabled": enabled })))
}

// ─────────────────────────────────────────────
//  Provider CRUD
// ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ProviderReq {
    pub name: String,
    pub base_url: String,
    #[serde(default = "default_bearer")]
    pub auth_style: String,
    #[serde(default)]
    pub timeout_secs: Option<u32>,
}

pub async fn api_upsert_provider(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProviderReq>,
) -> Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, &s.proxy_secret)?;

    let mut cfg = s.config.write().await;
    cfg.providers.insert(
        req.name,
        ProviderConfig {
            base_url: req.base_url,
            auth_style: req.auth_style,
            timeout_secs: req.timeout_secs,
        },
    );
    save_config(&s.config_path, &cfg).map_err(internal)?;
    reload_pool(&s, &cfg).await;
    Ok(Json(json!({ "ok": true })))
}

pub async fn api_delete_provider(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiErr> {
    check_auth(&headers, &s.proxy_secret)?;

    let mut cfg = s.config.write().await;
    cfg.providers.remove(&name);
    save_config(&s.config_path, &cfg).map_err(internal)?;
    reload_pool(&s, &cfg).await;
    Ok(Json(json!({ "ok": true })))
}
