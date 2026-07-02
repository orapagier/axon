use crate::providers::types::{
    normalize_base_url, normalize_provider_name, normalize_role, ModelRecord,
};
use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct BootConfig {
    pub settings: Option<BootSettings>,
    #[serde(default)]
    pub models: Vec<RawModel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ParsedModelId {
    Single(String),
    Multiple(Vec<String>),
}

impl ParsedModelId {
    pub fn to_string(&self) -> String {
        match self {
            ParsedModelId::Single(s) => s.clone(),
            ParsedModelId::Multiple(arr) => arr.join(","),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct BootSettings {
    pub rate_limit_cooldown_minutes: Option<i64>,
    pub error_threshold: Option<u32>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RawModel {
    pub name: String,
    pub provider: String,
    pub model_id: Option<ParsedModelId>,
    pub api_key: String,
    pub base_url: Option<String>,
    pub timeout_secs: Option<u64>,
    pub priority: Option<i32>,
    pub max_tokens: Option<u32>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub role: String,
}
fn default_true() -> bool {
    true
}

pub fn load_models(path: &str) -> anyhow::Result<Vec<ModelRecord>> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("Cannot read {}", path))?;
    let config: BootConfig = toml::from_str(&raw).with_context(|| format!("Parse {}", path))?;
    let models: Vec<ModelRecord> = config
        .models
        .into_iter()
        .map(|m| ModelRecord {
            model_id: m
                .model_id
                .map(|mid| mid.to_string())
                .unwrap_or_else(|| m.name.clone()),
            name: m.name,
            provider: normalize_provider_name(&m.provider),
            api_key: m.api_key,
            base_url: normalize_base_url(m.base_url),
            timeout_secs: m.timeout_secs,
            priority: m.priority.unwrap_or(99),
            max_tokens: m.max_tokens.unwrap_or(4096),
            enabled: m.enabled,
            role: normalize_role(&m.role),
            status: "available".into(),
            rate_limit_reset_at: None,
            consecutive_errors: 0,
            consecutive_rate_limits: 0,
            total_calls: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            rl_snapshot: Default::default(),
        })
        .collect();
    tracing::info!("Loaded {} models from {}", models.len(), path);
    Ok(models)
}

pub fn load_models_from_db(conn: &rusqlite::Connection) -> anyhow::Result<Vec<ModelRecord>> {
    let mut s = conn.prepare("SELECT name, provider, model_id, api_key, base_url, timeout_secs, priority, max_tokens, enabled, role FROM models")?;
    let rows = s.query_map([], |r| {
        let provider: String = r.get(1)?;
        let base_url: Option<String> = r.get(4)?;
        Ok(ModelRecord {
            name: r.get(0)?,
            provider: normalize_provider_name(&provider),
            model_id: r
                .get::<_, Option<String>>(2)?
                .unwrap_or_else(|| r.get::<_, String>(0).unwrap_or_default()),
            api_key: crate::crypto::decrypt_key(&r.get::<_, String>(3)?),
            base_url: normalize_base_url(base_url),
            timeout_secs: r.get::<_, Option<u64>>(5)?,
            priority: r.get(6)?,
            max_tokens: r.get(7)?,
            enabled: r.get::<_, i32>(8)? != 0,
            role: normalize_role(&r.get::<_, String>(9)?),
            status: "available".into(),
            rate_limit_reset_at: None,
            consecutive_errors: 0,
            consecutive_rate_limits: 0,
            total_calls: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            rl_snapshot: Default::default(),
        })
    })?;
    let mut res = Vec::new();
    for r in rows {
        res.push(r?);
    }
    tracing::info!("Loaded {} models from database", res.len());
    Ok(res)
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub port: u16,
    pub db_path: String,
}
impl AppConfig {
    pub fn from_env() -> Self {
        AppConfig {
            port: std::env::var("AXON_PORT")
                .unwrap_or_else(|_| "3000".into())
                .parse()
                .unwrap_or(3000),
            db_path: std::env::var("AXON_DB_PATH").unwrap_or_else(|_| "memory/axon.db".into()),
        }
    }
}
