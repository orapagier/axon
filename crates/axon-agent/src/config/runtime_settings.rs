use anyhow::Context;
use once_cell::sync::Lazy;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;

static WARNED_UNRESOLVED_ENV: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingRow {
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub updated_at: String,
}

#[derive(Clone)]
pub struct RuntimeSettings {
    db: Arc<Pool<SqliteConnectionManager>>,
}

impl RuntimeSettings {
    pub fn new(db: Arc<Pool<SqliteConnectionManager>>) -> Self {
        RuntimeSettings { db }
    }

    pub fn get_raw(&self, key: &str) -> Option<String> {
        let conn = self.db.get().ok()?;
        conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .ok()
    }
    pub fn get_int(&self, key: &str, default: i64) -> i64 {
        self.get_raw(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get_raw(key)
            .map(|v| v == "true" || v == "1")
            .unwrap_or(default)
    }
    pub fn get_str(&self, key: &str, default: &str) -> String {
        self.get_raw(key).unwrap_or_else(|| default.to_string())
    }
    pub fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let conn = self.db.get().context("DB pool")?;
        conn.execute(
            "UPDATE settings SET value=?1, updated_at=datetime('now') WHERE key=?2",
            rusqlite::params![value, key],
        )?;
        Ok(())
    }
    pub fn all(&self) -> anyhow::Result<Vec<SettingRow>> {
        let rows: Vec<SettingRow> = {
            let conn = self.db.get().context("DB pool")?;
            let mut s = conn.prepare("SELECT key,value,value_type,description,category,updated_at FROM settings ORDER BY category,key")?;
            let res = s
                .query_map([], |r| {
                    Ok(SettingRow {
                        key: r.get(0)?,
                        value: r.get(1)?,
                        value_type: r.get(2)?,
                        description: r.get(3)?,
                        category: r.get(4)?,
                        updated_at: r.get(5)?,
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();
            res
        };
        Ok(rows)
    }
    pub fn max_iterations(&self) -> i64 {
        self.get_int("agent.max_iterations", 20)
    }
    pub fn max_parallel_tools(&self) -> usize {
        // Conservative default for small/shared-core hosts (e.g. e2-micro): a
        // single request fanning out CPU-bound tools (image gen, JS, shell) can
        // starve the runtime. Tune up via the dashboard on larger machines.
        self.get_int("agent.max_parallel_tools", 3) as usize
    }
    pub fn tool_timeout_secs(&self) -> u64 {
        self.get_int("agent.tool_timeout_secs", 30) as u64
    }
    pub fn allow_tool_writing(&self) -> bool {
        self.get_bool("agent.allow_tool_writing", true)
    }
    pub fn temp_tool_max_retries(&self) -> u32 {
        self.get_int("agent.temp_tool_max_retries", 2) as u32
    }
    pub fn request_timeout_secs(&self) -> u64 {
        self.get_int("agent.request_timeout_secs", 45) as u64
    }
    pub fn request_timeout_max_secs(&self) -> u64 {
        self.get_int("agent.request_timeout_max_secs", 120) as u64
    }
    pub fn min_model_chain_secs(&self) -> u64 {
        self.get_int("agent.min_model_chain_secs", 60) as u64
    }
    pub fn stream_model_tokens(&self) -> bool {
        self.get_bool("agent.stream_model_tokens", false)
    }
    pub fn system_prompt(&self) -> String {
        self.get_str("agent.system_prompt", "\
You are Axon, a capable AI agent. Always provide responses in plain text only, no Markdown formatting.

TOOL SELECTION RULES — CRITICAL:
You have access to BOTH Google and Microsoft services. You MUST select the correct one based on the user's words:
- 'mscal', 'microsoft calendar', 'outlook calendar' → use mscal_* tools (NEVER gcal_*)
- 'gcal', 'google calendar' → use gcal_* tools (NEVER mscal_*)
- 'outlook', 'outlook email', 'microsoft email' → use outlook_* tools (NEVER gmail_*)
- 'gmail', 'google email' → use gmail_* tools (NEVER outlook_*)
- 'onedrive', 'microsoft drive' → use onedrive_* tools (NEVER gdrive_*)
- 'gdrive', 'google drive' → use gdrive_* tools (NEVER onedrive_*)
If the user specifies a service, you MUST use exactly that service's tools. NEVER substitute one for another.
If ambiguous and the user doesn't specify which service, ASK which one they mean.

HALLUCINATION PREVENTION — CRITICAL:
1. You MUST use the native JSON tool calling mechanism provided by the API.
2. NEVER output raw JSON snippets, markdown code blocks (```json), XML tags like <tool_call>, or call:tool_name{args} syntax in your message body.
3. Speak in plain text only. No Markdown formatting (no asterisks, no bolding, no code blocks) unless essential for data representation.

RESPONSE RULES:
1. ALWAYS call the relevant tool to get fresh data. NEVER fabricate or invent data from memory.
2. When a tool returns data, report ONLY what the tool returned. Do not add, embellish, or invent extra entries.
3. EMAIL PRESENTATION: When reporting email(s), always provide:
   - Sender name (display name)
   - Subject
   - Full date & time (converted to Asia/Manila PHT, e.g., Monday, March 23, 2026, 4:00 PM)
   - Natural paragraph summary of the body (avoid listing keywords; speak in a human-like way)
4. If a tool call fails, report the failure honestly. Do not make up results.
5. If the user corrects you (e.g., 'no, I said mscal'), immediately call the CORRECT tool. Do not restate previous wrong results.
6. CRITICAL: You MUST use the native JSON tool calling mechanism provided by the API. DO NOT output raw XML `<tool_call>` tags in your message body.")
    }
    pub fn rate_limit_cooldown(&self) -> i64 {
        self.get_int("router.rate_limit_cooldown", 2)
    }
    /// Upper bound (minutes) for the exponential rate-limit backoff in
    /// `ModelRecord::mark_rate_limited`. Caps how long a repeatedly-429'd model
    /// (e.g. one that exhausted a daily free-tier quota) stays quarantined.
    pub fn rate_limit_max_cooldown(&self) -> i64 {
        self.get_int("router.rate_limit_max_cooldown", 60)
    }
    pub fn error_threshold(&self) -> u32 {
        self.get_int("router.error_threshold", 3) as u32
    }
    pub fn long_term_top_k(&self) -> usize {
        self.get_int("memory.long_term_top_k", 5) as usize
    }
    // Database retention / housekeeping (see `crate::maintenance`).
    pub fn retention_enabled(&self) -> bool {
        self.get_bool("retention.enabled", true)
    }
    pub fn retention_workflow_runs_per_workflow(&self) -> i64 {
        self.get_int("retention.workflow_runs_per_workflow", 50)
    }
    pub fn retention_runs_days(&self) -> i64 {
        self.get_int("retention.runs_days", 30)
    }
    pub fn retention_observations_days(&self) -> i64 {
        self.get_int("retention.observations_days", 30)
    }
    pub fn retention_webhook_events_days(&self) -> i64 {
        self.get_int("retention.webhook_events_days", 30)
    }
    pub fn retention_vacuum_min_free_mb(&self) -> i64 {
        self.get_int("retention.vacuum_min_free_mb", 20)
    }

    pub fn websearch_enabled(&self) -> bool {
        self.get_bool("websearch.enabled", false)
    }
    pub fn websearch_max_results(&self) -> i64 {
        self.get_int("websearch.max_results", 5)
    }

    // FIX #5: Configurable per-model call timeout so a hung provider
    // doesn't block the entire fallback chain. Defaults to 20s.
    pub fn model_call_timeout_secs(&self) -> u64 {
        self.get_int("router.model_call_timeout_secs", 20) as u64
    }
    pub fn model_call_timeout_min_secs(&self) -> u64 {
        self.get_int("router.model_call_timeout_min_secs", 10) as u64
    }
    pub fn model_call_timeout_max_secs(&self) -> u64 {
        self.get_int("router.model_call_timeout_max_secs", 90) as u64
    }
    pub fn model_call_timeout_per_1k_chars_secs(&self) -> u64 {
        self.get_int("router.model_call_timeout_per_1k_chars_secs", 3) as u64
    }
    pub fn model_call_timeout_fair_share_grace_secs(&self) -> u64 {
        self.get_int("router.model_call_timeout_fair_share_grace_secs", 4) as u64
    }

    pub fn resolve(&self, input: &str) -> String {
        if !input.starts_with("${") || !input.ends_with("}") {
            return input.to_string();
        }
        let key = &input[2..input.len() - 1];
        if let Some(val) = self.get_raw(key) {
            if !val.is_empty() {
                return val;
            }
        }
        match std::env::var(key) {
            Ok(val) if !val.is_empty() => val,
            _ => {
                if let Ok(mut warned) = WARNED_UNRESOLVED_ENV.lock() {
                    if warned.insert(key.to_string()) {
                        tracing::warn!(
                            "Runtime placeholder '{}' could not be resolved from settings or environment",
                            key
                        );
                    }
                }
                input.to_string()
            }
        }
    }
}
