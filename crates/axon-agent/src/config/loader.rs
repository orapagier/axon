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

/// Boot-time seed of `config/models.toml` into the `models` table. This is
/// insert-only: models.toml is the Source of Truth only on a first deploy (the
/// table is empty), and on later deploys it can add models new to the file. The
/// DB is otherwise authoritative — a row that already exists (matched by name)
/// is never overwritten, whatever its origin, so dashboard (ModelsPage) edits
/// always survive a redeploy; and nothing is pruned, so a model dropped from the
/// file stays in the DB. Remove or edit a shipped model in the dashboard, not
/// the TOML. Per-row failures are ignored, matching the historical boot behavior
/// of never letting one bad row keep the agent from starting.
pub fn sync_toml_models(conn: &rusqlite::Connection, toml_models: Vec<ModelRecord>) {
    for m in toml_models {
        // Insert-only seed. models.toml is the source of truth ONLY on a first
        // deploy (empty `models` table); on redeploys the DB is authoritative.
        // `INSERT OR IGNORE` means a name that already exists in the DB is left
        // completely untouched — dashboard (ModelsPage) edits always win — while
        // a name new to models.toml is added alongside the saved DB models. There
        // is no prune: a model dropped from the file is NOT deleted from the DB.
        // Edit or remove a shipped model in the dashboard, never via the TOML.
        let _ = conn.execute(
            "INSERT OR IGNORE INTO models (name, provider, model_id, api_key, base_url, timeout_secs, priority, max_tokens, enabled, role, thinking_mode, origin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'toml')",
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
                m.role,
                m.thinking_mode
            ],
        );
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

/// Write-through the dashboard's chosen `model_id` into `config/models.toml`
/// so the file (the boot Source of Truth) stays in step with the DB. This is a
/// surgical, line-level edit of just the `model_id = "…"` line inside the
/// matching `[[models]]` block — every comment, other key, and the rest of the
/// file are preserved (no full re-serialization). Secrets are never touched.
///
/// Returns `Ok(true)` if a block named `name` was found and updated, `Ok(false)`
/// if no such block exists in the file (e.g. a runtime/dashboard-only model that
/// was never written to models.toml — it lives in the DB alone and needs no file
/// edit). Any I/O error is returned so the caller can log it without failing the
/// request.
pub fn set_model_id_in_toml(path: &str, name: &str, new_model_id: &str) -> anyhow::Result<bool> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read {} for model_id write-through", path))?;
    let (new_content, changed) = rewrite_model_id(&content, name, new_model_id);
    if changed {
        std::fs::write(path, new_content)
            .with_context(|| format!("write {} after model_id update", path))?;
    }
    Ok(changed)
}

/// Pure string transform behind [`set_model_id_in_toml`] (kept separate so it's
/// unit-testable without touching the filesystem). Preserves the file's original
/// newline style and the alignment left of `=`.
fn rewrite_model_id(content: &str, name: &str, new_model_id: &str) -> (String, bool) {
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines: Vec<String> = content
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();

    let is_header = |l: &str| l.trim_start().starts_with('[');
    let escaped = new_model_id.replace('\\', "\\\\").replace('"', "\\\"");

    let n = lines.len();
    let mut i = 0;
    while i < n {
        if lines[i].trim() != "[[models]]" {
            i += 1;
            continue;
        }
        // Block body spans from just after the header to the next header/EOF.
        let start = i + 1;
        let mut end = start;
        while end < n && !is_header(&lines[end]) {
            end += 1;
        }
        let block_name = (start..end).find_map(|j| toml_string_value(&lines[j], "name"));
        if block_name.as_deref() == Some(name) {
            let model_id_line =
                (start..end).find(|&j| toml_key(&lines[j]).as_deref() == Some("model_id"));
            match model_id_line {
                // Swap only the value, keeping the original left-of-`=` text
                // (indentation + alignment spaces) intact.
                Some(j) => {
                    if let Some(eq) = lines[j].find('=') {
                        lines[j] = format!("{} \"{}\"", &lines[j][..=eq], escaped);
                    }
                }
                // Block has no model_id line (it defaulted to `name`): insert one
                // right after the header, matching the file's usual key style.
                None => lines.insert(start, format!("model_id   = \"{}\"", escaped)),
            }
            return (lines.join(newline), true);
        }
        i = end;
    }
    (content.to_string(), false)
}

/// The bare key on a TOML `key = value` line, or `None` for blank/comment lines
/// or anything that isn't a simple `key = …` assignment.
fn toml_key(line: &str) -> Option<String> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    let (key, _) = t.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    Some(key.to_string())
}

/// The string value of `line` when it's `key = "value"`, else `None`.
fn toml_string_value(line: &str, key: &str) -> Option<String> {
    if toml_key(line).as_deref() != Some(key) {
        return None;
    }
    let (_, rhs) = line.split_once('=')?;
    let rhs = rhs.trim();
    let inner = rhs.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
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
    fn toml_rows_survive_removal_from_file() {
        let conn = test_db();
        sync_toml_models(&conn, vec![toml_model("a"), toml_model("b")]);
        assert_eq!(origin_of(&conn, "b").as_deref(), Some("toml"));

        // "b" was deleted from models.toml → the DB is the source of truth now,
        // so a redeploy must NOT prune it. Both "a" and "b" remain.
        sync_toml_models(&conn, vec![toml_model("a")]);
        assert_eq!(origin_of(&conn, "a").as_deref(), Some("toml"));
        assert_eq!(origin_of(&conn, "b").as_deref(), Some("toml"));
    }

    #[test]
    fn toml_never_overwrites_an_existing_db_row() {
        let conn = test_db();
        // First deploy seeds "m" from models.toml.
        sync_toml_models(&conn, vec![toml_model("m")]);

        // The operator edits it in the dashboard (priority 1 -> 5).
        crate::dashboard::api::apply_update_model(&conn, "m", &json!({ "priority": 5 })).unwrap();

        // A redeploy re-runs the sync with the original TOML (priority 1). The
        // dashboard edit must win — the DB row is left completely untouched.
        sync_toml_models(&conn, vec![toml_model("m")]);
        let m = load_models_from_db(&conn)
            .unwrap()
            .into_iter()
            .find(|m| m.name == "m")
            .unwrap();
        assert_eq!(m.priority, 5);
    }

    #[test]
    fn toml_adds_new_models_on_redeploy() {
        let conn = test_db();
        sync_toml_models(&conn, vec![toml_model("a")]);

        // A later deploy adds "b" to models.toml → it joins the saved DB models.
        sync_toml_models(&conn, vec![toml_model("a"), toml_model("b")]);
        let names: Vec<String> = load_models_from_db(&conn)
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn thinking_mode_seeds_through_boot_sync() {
        // Regression: the sync insert used to omit thinking_mode entirely, so
        // a `thinking_mode = "level"` set in models.toml never reached the DB
        // and (since the boot path loads models from the DB) never activated.
        let conn = test_db();
        let mut m = toml_model("thinker");
        m.thinking_mode = Some("level".into());
        sync_toml_models(&conn, vec![m]);
        let loaded = load_models_from_db(&conn).unwrap();
        let thinker = loaded.iter().find(|m| m.name == "thinker").unwrap();
        assert_eq!(thinker.thinking_mode.as_deref(), Some("level"));

        // A redeploy whose TOML omits thinking_mode does NOT clear the seeded
        // value: the row already exists, so the insert-only sync leaves it be.
        sync_toml_models(&conn, vec![toml_model("thinker")]);
        let loaded = load_models_from_db(&conn).unwrap();
        let thinker = loaded.iter().find(|m| m.name == "thinker").unwrap();
        assert_eq!(thinker.thinking_mode.as_deref(), Some("level"));
    }

    #[test]
    fn rewrite_model_id_replaces_only_target_block_and_keeps_comments() {
        let toml = "# top comment\n\
                    [settings]\n\n\
                    [[models]]\n\
                    name       = \"gemini-a\"\n\
                    provider   = \"google\"\n\
                    model_id   = \"gemini-3.1-flash-lite\"  # inline\n\
                    priority   = 1\n\n\
                    [[models]]\n\
                    name       = \"gemini-b\"\n\
                    provider   = \"google\"\n\
                    model_id   = \"gemini-3.1-flash-lite\"\n";
        let (out, changed) = super::rewrite_model_id(toml, "gemini-b", "gemini-3.1-pro");
        assert!(changed);
        // Target block updated…
        assert!(out.contains(
            "name       = \"gemini-b\"\nprovider   = \"google\"\nmodel_id   = \"gemini-3.1-pro\""
        ));
        // …the other block's model_id is untouched…
        assert!(out.contains("name       = \"gemini-a\""));
        assert_eq!(out.matches("gemini-3.1-flash-lite").count(), 1);
        // …and comments survive.
        assert!(out.contains("# top comment"));
    }

    #[test]
    fn rewrite_model_id_preserves_alignment_before_equals() {
        let toml = "[[models]]\nname = \"m\"\nmodel_id   = \"old\"\n";
        let (out, changed) = super::rewrite_model_id(toml, "m", "new");
        assert!(changed);
        // The alignment spaces left of `=` are kept.
        assert!(out.contains("model_id   = \"new\""));
    }

    #[test]
    fn rewrite_model_id_inserts_when_block_has_no_model_id_line() {
        // A block that relied on the model_id-defaults-to-name behavior.
        let toml = "[[models]]\nname = \"bare\"\nprovider = \"groq\"\n";
        let (out, changed) = super::rewrite_model_id(toml, "bare", "openai/gpt-oss-120b");
        assert!(changed);
        assert!(out.contains("model_id   = \"openai/gpt-oss-120b\""));
        assert!(out.contains("name = \"bare\""));
    }

    #[test]
    fn rewrite_model_id_is_noop_for_unknown_name() {
        let toml = "[[models]]\nname = \"m\"\nmodel_id = \"x\"\n";
        let (out, changed) = super::rewrite_model_id(toml, "not-there", "y");
        assert!(!changed);
        assert_eq!(out, toml);
    }

    #[test]
    fn toml_never_reclaims_a_name_first_added_at_runtime() {
        let conn = test_db();
        crate::dashboard::api::apply_add_model(
            &conn,
            &json!({ "name": "shared", "provider": "groq", "api_key": "k" }),
        )
        .unwrap();

        // The operator later adds the same name to models.toml. The DB row is the
        // source of truth, so the insert-only sync leaves it alone: origin stays
        // 'runtime' and it survives even after the name is dropped from the file.
        sync_toml_models(&conn, vec![toml_model("shared")]);
        assert_eq!(origin_of(&conn, "shared").as_deref(), Some("runtime"));
        sync_toml_models(&conn, vec![toml_model("other")]);
        assert_eq!(origin_of(&conn, "shared").as_deref(), Some("runtime"));
    }
}
