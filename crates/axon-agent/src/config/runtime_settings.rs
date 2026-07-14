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
    pub fn get_f64(&self, key: &str, default: f64) -> f64 {
        self.get_raw(key)
            .and_then(|v| v.trim().parse().ok())
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
    /// Currency assigned to deals created without an explicit one. Read per
    /// call (via the provider registered in main.rs) so changing the setting
    /// applies without a restart; axon-crm validates and falls back to USD.
    pub fn crm_default_currency(&self) -> String {
        self.get_str("crm.default_currency", "USD")
    }
    pub fn temp_tool_max_retries(&self) -> u32 {
        self.get_int("agent.temp_tool_max_retries", 2) as u32
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
    /// Consecutive non-rate-limit errors before a model is parked until the next
    /// midnight. Default 2 — one transient blip is tolerated, a genuinely broken
    /// endpoint drops out fast.
    pub fn error_threshold(&self) -> u32 {
        self.get_int("router.error_threshold", 2) as u32
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
    // Scheduled local backups of axon.db/crm.db (see `crate::maintenance::run_backup`).
    pub fn backup_enabled(&self) -> bool {
        self.get_bool("backup.enabled", true)
    }
    pub fn backup_retention_days(&self) -> i64 {
        self.get_int("backup.retention_days", 14)
    }
    // Off-instance backup of workflow definitions to Google Drive
    // (see `crate::maintenance::run_workflow_drive_backup`). Opt-in — needs
    // Google connected on the Services page.
    pub fn workflow_backup_enabled(&self) -> bool {
        self.get_bool("workflow_backup.enabled", false)
    }
    pub fn workflow_backup_interval_hours(&self) -> i64 {
        self.get_int("workflow_backup.interval_hours", 24).max(1)
    }
    /// Destination Drive folder id. Empty = Drive root. A dedicated folder is
    /// recommended: Drive-side pruning of old backups only runs when this is set
    /// (never in the root).
    pub fn workflow_backup_drive_folder_id(&self) -> String {
        self.get_str("workflow_backup.drive_folder_id", "")
    }
    /// How many backup files to keep, locally and (when a folder is set) in Drive.
    pub fn workflow_backup_retention(&self) -> i64 {
        self.get_int("workflow_backup.retention", 7).max(1)
    }
    /// Max version snapshots kept per workflow (B1). Labeled snapshots are kept
    /// beyond this cap; only unlabeled ones are pruned oldest-first.
    pub fn retention_workflow_versions_per_workflow(&self) -> i64 {
        self.get_int("retention.workflow_versions_per_workflow", 50)
    }
    /// Throttle (seconds) between version snapshots of the same workflow (B1).
    /// Editor autosaves fire often; a snapshot is skipped when the latest one is
    /// younger than this, so rapid-edit bursts don't each become a version.
    pub fn workflow_version_min_interval_secs(&self) -> i64 {
        self.get_int("workflow.version_min_interval_secs", 30)
    }
    /// B2: byte size above which a node-output string is offloaded to the blob
    /// store instead of being persisted inline in `node_results`. `0` disables.
    pub fn workflow_binary_inline_max_bytes(&self) -> usize {
        self.get_int("workflow.binary_inline_max_bytes", 65536)
            .max(0) as usize
    }
    /// B3: max workflow runs executing concurrently. Read once at startup to size
    /// the run semaphore; changing it takes effect on restart.
    pub fn workflow_max_concurrent_runs(&self) -> i64 {
        self.get_int("workflow.max_concurrent_runs", 10).max(1)
    }
    /// B3: max runs allowed to queue waiting for a permit. Beyond this, new
    /// trigger fires are shed (logged + marked failed) instead of piling up. `0`
    /// disables the cap (unbounded queue, still bounded *execution*).
    pub fn workflow_max_queue_depth(&self) -> i64 {
        self.get_int("workflow.max_queue_depth", 0).max(0)
    }
    /// Operator timezone as whole hours offset from UTC (default +8,
    /// Asia/Manila), clamped to the valid -12..=+14 range.
    pub fn agent_utc_offset_hours(&self) -> i32 {
        self.get_int("agent.utc_offset_hours", 8).clamp(-12, 14) as i32
    }
    /// Operator-local timezone as a fixed UTC offset (default +8, Asia/Manila).
    /// Drives schedule parsing, local→UTC cron conversion, and the agent's
    /// [SYSTEM CLOCK] context.
    pub fn agent_utc_offset(&self) -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(self.agent_utc_offset_hours() * 3600)
            .expect("offset clamped to a valid range")
    }
    /// C1: public base URL used to build the resume/approve/reject links a
    /// Wait-for-webhook or Approval node surfaces. Blank → the node emits
    /// relative paths (`/webhook/resume/<token>`) for the operator to prefix.
    pub fn workflow_public_base_url(&self) -> String {
        self.get_str("workflow.public_base_url", "")
            .trim()
            .trim_end_matches('/')
            .to_string()
    }
    /// C1: default resume-token lifetime (seconds) when an Approval/webhook Wait
    /// node sets no explicit timeout. Defaults to `0` = the token never expires and
    /// the run parks forever (NULL `resume_at`) until someone hits the resume URL —
    /// the documented C1 contract. Set this >0 to give every untimed Approval/webhook
    /// Wait a fallback deadline, which drives the run's `resume_at` so the time poller
    /// fires the timeout branch (approval → Reject) if no one resumes it. A per-node
    /// `timeout` always overrides this.
    pub fn workflow_resume_token_default_ttl_secs(&self) -> i64 {
        self.get_int("workflow.resume_token_default_ttl_secs", 0)
            .max(0)
    }
    /// C2: time window (seconds) for body-hash dedup of generic webhooks that
    /// supply no explicit Idempotency-Key/event_id. `0` (default) disables the
    /// body-hash fallback so identical payloads are never silently dropped;
    /// explicit-key dedup is always on. A positive value buckets the body hash so
    /// retries within the window dedup but a later resend still fires.
    pub fn workflow_webhook_dedup_window_secs(&self) -> i64 {
        self.get_int("workflow.webhook_dedup_window_secs", 0).max(0)
    }
    /// 3.1: how long (seconds) the external-webhook handler holds the HTTP
    /// request open waiting for a Respond to Webhook node to fire before
    /// answering with the default ack instead. The run keeps executing either
    /// way — a timeout only releases the caller, it never cancels the run.
    pub fn workflow_webhook_respond_timeout_secs(&self) -> u64 {
        self.get_int("workflow.webhook_respond_timeout_secs", 30)
            .clamp(1, 600) as u64
    }
    /// C2: days of `trigger_dedup` idempotency keys kept before pruning.
    pub fn retention_trigger_dedup_days(&self) -> i64 {
        self.get_int("retention.trigger_dedup_days", 7).max(1)
    }

    pub fn websearch_enabled(&self) -> bool {
        self.get_bool("websearch.enabled", false)
    }
    pub fn websearch_max_results(&self) -> i64 {
        self.get_int("websearch.max_results", 5)
    }

    // Flat per-model call timeout: a model either answers within this window or
    // the router fails over immediately to the next one. A model may override it
    // via its own `timeout_secs`. Default 30s.
    pub fn model_call_timeout_secs(&self) -> u64 {
        self.get_int("router.model_call_timeout_secs", 30) as u64
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
