use serde::{Deserialize, Serialize};
use std::{collections::HashMap, env, fs};
use tracing::warn;

// ─────────────────────────────────────────────
//  Config types  (serializable ↔ models.toml)
// ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub api_key: String,
    #[serde(default)]
    pub role: String,
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
    /// Per-slot HTTP timeout in seconds. Overrides the global client timeout.
    /// Set low (e.g. 15) for fast providers like Cerebras/Gemini so the proxy
    /// moves to the next slot quickly instead of blocking the agent's router.
    /// Leave unset to inherit from the provider default, or falls back to 30s.
    #[serde(default)]
    pub timeout_secs: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    #[serde(default = "default_bearer")]
    pub auth_style: String,
    /// Default timeout for all models on this provider (can be overridden per-model).
    #[serde(default)]
    pub timeout_secs: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(rename = "models", default)]
    pub models: Vec<ModelEntry>,
}

pub fn default_priority() -> u32 { 1 }
pub fn default_max_tokens() -> u32 { 4096 }
pub fn default_true() -> bool { true }
pub fn default_bearer() -> String { "bearer".into() }

// ─────────────────────────────────────────────
//  Env-var helpers
// ─────────────────────────────────────────────

pub fn expand_env(s: &str, overrides: &HashMap<String, String>) -> Option<String> {
    if let Some(var_name) = s.strip_prefix("${").and_then(|v| v.strip_suffix("}")) {
        overrides
            .get(var_name)
            .cloned()
            .or_else(|| env::var(var_name).ok())
            .filter(|k| !k.is_empty())
    } else {
        Some(s.to_string())
    }
}

pub fn extract_var(s: &str) -> Option<String> {
    s.strip_prefix("${")?.strip_suffix("}").map(|v| v.to_string())
}

pub fn make_env_var(provider: &str, model_name: &str) -> String {
    let clean: String = model_name
        .to_uppercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    format!("{}_API_KEY_{}", provider.to_uppercase(), clean)
}

fn atomic_write(path: &str, content: &str) -> Result<(), String> {
    let tmp = format!("{}.tmp", path);
    fs::write(&tmp, content).map_err(|e| format!("write to {}: {}", tmp, e))?;
    let _ = fs::remove_file(path);
    fs::rename(&tmp, path).map_err(|e| format!("rename {} → {}: {}", tmp, path, e))
}

pub fn write_env_var(path: &str, key: &str, value: &str) -> Result<(), String> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let prefix = format!("{}=", key);
    let mut found = false;
    let mut lines: Vec<String> = existing
        .lines()
        .map(|l| {
            if l.starts_with(&prefix) {
                found = true;
                format!("{}={}", key, value)
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        lines.push(format!("{}={}", key, value));
    }
    atomic_write(path, &(lines.join("\n") + "\n"))
}

pub fn remove_env_var(path: &str, key: &str) -> Result<(), String> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    let prefix = format!("{}=", key);
    let lines: Vec<String> = existing
        .lines()
        .filter(|l| !l.starts_with(&prefix))
        .map(|l| l.to_string())
        .collect();
    atomic_write(path, &(lines.join("\n") + "\n"))
}

pub fn save_config(path: &str, config: &Config) -> Result<(), String> {
    let content = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    atomic_write(path, &content)
}

pub fn load_config(path: &str) -> Config {
    match fs::read_to_string(path) {
        Err(e) => {
            warn!("Cannot read '{}': {} — starting with empty config", path, e);
            Config::default()
        }
        Ok(raw) => match toml::from_str::<Config>(&raw) {
            Ok(c) => c,
            Err(e) => {
                warn!("Failed to parse '{}': {} — starting with empty config", path, e);
                Config::default()
            }
        },
    }
}

pub fn load_env_overrides(path: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    if let Ok(content) = fs::read_to_string(path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = trimmed.split_once('=') {
                vars.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_literal() {
        let o = HashMap::new();
        assert_eq!(expand_env("literal-key", &o), Some("literal-key".into()));
    }

    #[test]
    fn expand_env_with_override() {
        let mut o = HashMap::new();
        o.insert("MY_KEY".into(), "secret123".into());
        assert_eq!(expand_env("${MY_KEY}", &o), Some("secret123".into()));
    }

    #[test]
    fn expand_env_missing_returns_none() {
        let o = HashMap::new();
        assert_eq!(expand_env("${NONEXISTENT_KEY_XYZ_99}", &o), None);
    }

    #[test]
    fn expand_env_empty_value_returns_none() {
        let mut o = HashMap::new();
        o.insert("EMPTY".into(), "".into());
        assert_eq!(expand_env("${EMPTY}", &o), None);
    }

    #[test]
    fn extract_var_works() {
        assert_eq!(extract_var("${MY_KEY}"), Some("MY_KEY".into()));
        assert_eq!(extract_var("plain"), None);
        assert_eq!(extract_var("${"), None);
    }

    #[test]
    fn make_env_var_formats_correctly() {
        assert_eq!(
            make_env_var("google", "gemini-account1"),
            "GOOGLE_API_KEY_GEMINI_ACCOUNT1"
        );
    }

    #[test]
    fn write_and_remove_env_var() {
        let tmp = std::env::temp_dir().join("api_proxy_test.env");
        let path = tmp.to_str().unwrap();
        let _ = fs::remove_file(path);
        write_env_var(path, "FOO", "bar").unwrap();
        assert!(fs::read_to_string(path).unwrap().contains("FOO=bar"));
        write_env_var(path, "FOO", "baz").unwrap();
        let content = fs::read_to_string(path).unwrap();
        assert!(content.contains("FOO=baz"));
        assert!(!content.contains("FOO=bar"));
        remove_env_var(path, "FOO").unwrap();
        assert!(!fs::read_to_string(path).unwrap().contains("FOO="));
        let _ = fs::remove_file(path);
    }
}
