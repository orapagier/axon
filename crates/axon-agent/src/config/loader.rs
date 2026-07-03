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
    /// Anthropic-provider thinking mode: "adaptive" | "budget" | unset ("off").
    #[serde(default)]
    pub thinking_mode: Option<String>,
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
            thinking_mode: m.thinking_mode,
            no_reasoning: false,
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

/// Boot-time sync of `config/models.toml` into the `models` table. The TOML is
/// the Source of Truth for the rows it names: each is upserted with
/// origin='toml' (re-claiming the name even if it was first added at runtime),
/// and toml-owned rows that vanished from the file are pruned. Rows with
/// origin='runtime' (added via the dashboard or the Homeostasis workflow node)
/// are never overwritten or pruned, so they survive restarts. Per-row failures
/// are ignored, matching the historical boot behavior of never letting one bad
/// row keep the agent from starting.
pub fn sync_toml_models(conn: &rusqlite::Connection, toml_models: Vec<ModelRecord>) {
    let mut current_names = Vec::new();
    for m in toml_models {
        current_names.push(m.name.clone());
        let _ = conn.execute(
            "INSERT INTO models (name, provider, model_id, api_key, base_url, timeout_secs, priority, max_tokens, enabled, role, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'toml')
             ON CONFLICT(name) DO UPDATE SET
                provider=excluded.provider,
                model_id=excluded.model_id,
                api_key=excluded.api_key,
                base_url=excluded.base_url,
                timeout_secs=excluded.timeout_secs,
                priority=excluded.priority,
                max_tokens=excluded.max_tokens,
                enabled=excluded.enabled,
                role=excluded.role,
                origin='toml'",
            rusqlite::params![
                m.name,
                m.provider,
                m.model_id,
                crate::crypto::encrypt_key(&m.api_key),
                m.base_url,
                m.timeout_secs.map(|v| v as i64),
                m.priority,
                m.max_tokens,
                if m.enabled { 1 } else { 0 },
                m.role
            ],
        );
    }

    if !current_names.is_empty() {
        let placeholders = current_names
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "DELETE FROM models WHERE origin='toml' AND name NOT IN ({})",
            placeholders
        );
        let _ = conn.execute(&query, rusqlite::params_from_iter(current_names));
    }
}

pub fn load_models_from_db(conn: &rusqlite::Connection) -> anyhow::Result<Vec<ModelRecord>> {
    let mut s = conn.prepare("SELECT name, provider, model_id, api_key, base_url, timeout_secs, priority, max_tokens, enabled, role, thinking_mode FROM models")?;
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
            thinking_mode: r.get::<_, Option<String>>(10)?,
            no_reasoning: false,
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

#[cfg(test)]
mod tests {
    use super::{load_models_from_db, sync_toml_models};
    use crate::providers::types::ModelRecord;
    use rusqlite::Connection;
    use serde_json::json;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::init(&conn).unwrap();
        conn
    }

    fn toml_model(name: &str) -> ModelRecord {
        ModelRecord {
            name: name.into(),
            provider: "groq".into(),
            model_id: name.into(),
            api_key: "k".into(),
            base_url: None,
            timeout_secs: None,
            priority: 1,
            max_tokens: 4096,
            enabled: true,
            role: "".into(),
            thinking_mode: None,
            no_reasoning: false,
            status: "available".into(),
            rate_limit_reset_at: None,
            consecutive_errors: 0,
            consecutive_rate_limits: 0,
            total_calls: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            rl_snapshot: Default::default(),
        }
    }

    fn origin_of(conn: &Connection, name: &str) -> Option<String> {
        conn.query_row("SELECT origin FROM models WHERE name=?1", [name], |r| {
            r.get(0)
        })
        .ok()
    }

    #[test]
    fn runtime_models_survive_boot_sync() {
        let conn = test_db();
        // A model added at runtime (dashboard / Homeostasis node)...
        crate::dashboard::api::apply_add_model(
            &conn,
            &json!({ "name": "node-added", "provider": "groq", "api_key": "k" }),
        )
        .unwrap();
        assert_eq!(origin_of(&conn, "node-added").as_deref(), Some("runtime"));

        // ...survives a boot sync whose TOML doesn't mention it.
        sync_toml_models(&conn, vec![toml_model("from-toml")]);
        let names: Vec<String> = load_models_from_db(&conn)
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(names.contains(&"node-added".to_string()));
        assert!(names.contains(&"from-toml".to_string()));

        // And a second boot (same TOML) is idempotent for it too.
        sync_toml_models(&conn, vec![toml_model("from-toml")]);
        assert_eq!(origin_of(&conn, "node-added").as_deref(), Some("runtime"));
    }

    #[test]
    fn toml_rows_are_pruned_when_removed_from_file() {
        let conn = test_db();
        sync_toml_models(&conn, vec![toml_model("a"), toml_model("b")]);
        assert_eq!(origin_of(&conn, "b").as_deref(), Some("toml"));

        // "b" was deleted from models.toml → next boot prunes it, keeps "a".
        sync_toml_models(&conn, vec![toml_model("a")]);
        assert_eq!(origin_of(&conn, "a").as_deref(), Some("toml"));
        assert_eq!(origin_of(&conn, "b"), None);
    }

    #[test]
    fn toml_reclaims_a_name_first_added_at_runtime() {
        let conn = test_db();
        crate::dashboard::api::apply_add_model(
            &conn,
            &json!({ "name": "shared", "provider": "groq", "api_key": "k" }),
        )
        .unwrap();

        // The operator later adds the same name to models.toml: the file wins
        // ownership, so removing it from the file afterwards prunes the row.
        sync_toml_models(&conn, vec![toml_model("shared")]);
        assert_eq!(origin_of(&conn, "shared").as_deref(), Some("toml"));
        sync_toml_models(&conn, vec![toml_model("other")]);
        assert_eq!(origin_of(&conn, "shared"), None);
    }
}
