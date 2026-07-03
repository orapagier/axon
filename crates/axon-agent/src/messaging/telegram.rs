use super::gateway::{MessageGateway, OutgoingFile, OutgoingMessage};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

const WORKFLOW_RUN_POLL_MAX_SECONDS: u64 = 300;
const WORKFLOW_RUN_POLL_INTERVAL_SECONDS: u64 = 2;
const TELEGRAM_MAX_COMMANDS_PER_SCOPE: usize = 100;

#[derive(Debug, Clone)]
struct WorkflowMenuItem {
    id: String,
    name: String,
}

#[derive(Debug, Clone)]
struct WorkflowRunSummary {
    status: String,
    finished_at: Option<String>,
    node_results: Value,
}

#[derive(Debug, Clone)]
struct WorkflowSlashCommand {
    command: String,
    workflow_id: String,
    workflow_name: String,
}

pub struct TelegramGateway {
    token: String,
    connected: Arc<AtomicBool>,
    // Set by a superseding gateway (see reconnect_messaging) so this
    // instance's polling loop exits instead of running forever alongside
    // the new one — two live pollers on the same bot token race for
    // getUpdates and occasionally both win a cycle, producing duplicate
    // replies to a single incoming message.
    stopped: Arc<AtomicBool>,
    client: reqwest::Client,
}

fn normalized_equals(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(a), Some(b)) => !a.trim().is_empty() && a.trim() == b.trim(),
        _ => false,
    }
}

/// True if a telegram trigger node's `chat_ids` config matches `chat_id`.
/// `chat_ids` may be a comma/space-separated string or an array. An empty or
/// missing `chat_ids` means "listen to every chat" → always matches.
fn chat_ids_match(chat_ids: Option<&Value>, chat_id: &str) -> bool {
    let target = chat_id.trim();
    if target.is_empty() {
        return false;
    }
    let sep = |c: char| matches!(c, ',' | ';' | ' ' | '\n' | '\t');
    match chat_ids {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => {
            let s = s.trim();
            s.is_empty()
                || s.split(sep)
                    .map(|p| p.trim())
                    .filter(|p| !p.is_empty())
                    .any(|p| p == target)
        }
        Some(Value::Array(arr)) => {
            arr.is_empty()
                || arr.iter().any(|v| match v {
                    Value::String(s) => s.trim() == target,
                    Value::Number(n) => n.to_string() == target,
                    _ => false,
                })
        }
        _ => false,
    }
}

fn score_saved_callback_route(
    config: &Value,
    callback_data: &str,
    chat_id: &str,
    message_text: Option<&str>,
    message_caption: Option<&str>,
) -> Option<(bool, usize)> {
    let mut best_match: Option<(bool, usize)> = None;

    for row in crate::tools::telegram::collect_inline_keyboard_buttons(config) {
        for button in row {
            let Some(button_callback_data) = button.callback_data.as_deref() else {
                continue;
            };
            if !button_callback_data.eq_ignore_ascii_case(callback_data) {
                continue;
            }

            let mut score = 10usize;

            if normalized_equals(
                config.get("chat_id").and_then(|v| v.as_str()),
                Some(chat_id),
            ) {
                score += 3;
            }
            if normalized_equals(config.get("text").and_then(|v| v.as_str()), message_text) {
                score += 5;
            }
            if normalized_equals(
                config.get("caption").and_then(|v| v.as_str()),
                message_caption,
            ) {
                score += 5;
            }

            match best_match {
                Some((_, best_score)) if best_score >= score => {}
                _ => best_match = Some((button.route_to_trigger, score)),
            }
        }
    }

    best_match
}

fn infer_callback_route_from_configs(
    configs: &[Value],
    callback_data: &str,
    chat_id: &str,
    message_text: Option<&str>,
    message_caption: Option<&str>,
) -> Option<bool> {
    let mut best_match: Option<(bool, usize)> = None;
    let mut ambiguous = false;

    for config in configs {
        let Some(candidate) = score_saved_callback_route(
            config,
            callback_data,
            chat_id,
            message_text,
            message_caption,
        ) else {
            continue;
        };

        match best_match {
            None => {
                best_match = Some(candidate);
                ambiguous = false;
            }
            Some((_, best_score)) if candidate.1 > best_score => {
                best_match = Some(candidate);
                ambiguous = false;
            }
            Some((best_route, best_score)) if candidate.1 == best_score => {
                if candidate.0 != best_route {
                    ambiguous = true;
                }
            }
            _ => {}
        }
    }

    if ambiguous {
        None
    } else {
        best_match.map(|(route_to_trigger, _)| route_to_trigger)
    }
}

impl TelegramGateway {
    pub fn new(token: String) -> Self {
        let ok = !token.is_empty();
        TelegramGateway {
            token,
            connected: Arc::new(AtomicBool::new(ok)),
            stopped: Arc::new(AtomicBool::new(false)),
            client: reqwest::Client::new(),
        }
    }
    fn api(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    /// Signal this gateway's `start_polling` loop to exit on its next cycle.
    /// Called on a superseded gateway when a new one takes over the same
    /// bot token, so only one poller is ever fetching updates at a time.
    pub fn stop_polling(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    pub async fn start_polling(self: Arc<Self>, state: Arc<crate::state::AppState>) {
        if self.token.is_empty() {
            return;
        }
        if let Err(e) = self.ensure_default_commands(&state).await {
            tracing::warn!("Telegram setMyCommands failed: {}", e);
        }
        tracing::info!("Telegram polling started");
        // Resume from the last acknowledged update instead of 0. Starting
        // from 0 after every restart makes Telegram redeliver every update
        // it still considers unacked (anything fetched but not yet
        // superseded by a later offset) — the tail of that replay lands on
        // an agent run that already completed and replied once, producing
        // a second, independently-generated reply to the same message.
        let mut offset: i64 = state
            .settings
            .get_int("messaging.telegram_update_offset", 0);
        let mut consecutive_errors: u32 = 0;
        loop {
            if self.stopped.load(Ordering::Relaxed) {
                tracing::info!("Telegram polling stopped (superseded by a newer gateway)");
                return;
            }
            match Arc::clone(&self)
                .poll_once(offset, Arc::clone(&state))
                .await
            {
                Ok(n) => {
                    if n != offset {
                        offset = n;
                        if let Ok(conn) = state.db.get() {
                            let _ = conn.execute(
                                "INSERT INTO settings (key,value,value_type,category) VALUES ('messaging.telegram_update_offset',?1,'int','messaging') \
                                 ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=datetime('now')",
                                rusqlite::params![offset.to_string()],
                            );
                        }
                    }
                    consecutive_errors = 0;
                }
                Err(e) => {
                    consecutive_errors = consecutive_errors.saturating_add(1);
                    let wait = poll_backoff_secs(consecutive_errors);
                    tracing::warn!("Telegram poll: {} (retry in {}s)", e, wait);
                    tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                }
            }
        }
    }

    async fn ensure_default_commands(&self, state: &crate::state::AppState) -> Result<()> {
        // ── Clear stale command menu cache from previous bot instances ──
        let delete_scopes = vec![
            json!({}),
            json!({ "scope": { "type": "all_private_chats" } }),
            json!({ "scope": { "type": "all_group_chats" } }),
            json!({ "scope": { "type": "all_chat_administrators" } }),
        ];
        let mut delete_ok = 0u32;
        for body in &delete_scopes {
            if self.call_api("deleteMyCommands", body).await.is_ok() {
                delete_ok += 1;
            }
        }
        tracing::info!(
            "Telegram deleteMyCommands: cleared {} / {} scopes",
            delete_ok,
            delete_scopes.len()
        );

        let mut commands: Vec<Value> = vec![
            json!({ "command": "start", "description": "Start and show quick actions" }),
            json!({ "command": "workflows", "description": "Browse & run workflows" }),
            json!({ "command": "run", "description": "Run a workflow by name or ID" }),
            json!({ "command": "help", "description": "Show available commands" }),
            json!({ "command": "settings", "description": "Show bot settings help" }),
        ];

        let wf_specs = Self::build_workflow_slash_commands(state);
        let wf_count = wf_specs.len();
        if wf_count == 0 {
            tracing::warn!(
                "Telegram workflow commands: no enabled workflows found; only built-in slash commands will be registered"
            );
        }
        for wf in wf_specs {
            if commands.len() >= TELEGRAM_MAX_COMMANDS_PER_SCOPE {
                break;
            }
            let mut desc = format!("Run workflow: {}", wf.workflow_name);
            if desc.chars().count() > 64 {
                desc = desc.chars().take(64).collect();
            }
            commands.push(json!({
                "command": wf.command,
                "description": desc
            }));
        }

        tracing::info!(
            "Telegram setMyCommands: registering {} total commands ({} built-in + {} workflows)",
            commands.len(),
            5,
            wf_count
        );

        let set_scopes = vec![
            json!({ "commands": commands }),
            json!({ "commands": commands, "scope": { "type": "all_private_chats" } }),
            json!({ "commands": commands, "scope": { "type": "all_group_chats" } }),
            json!({ "commands": commands, "scope": { "type": "all_chat_administrators" } }),
        ];
        for body in &set_scopes {
            if let Err(e) = self.call_api("setMyCommands", body).await {
                tracing::warn!("Telegram setMyCommands scope failed: {}", e);
            }
        }
        Ok(())
    }

    async fn call_api(&self, method: &str, body: &Value) -> Result<Value> {
        let resp = self.client.post(self.api(method)).json(body).send().await?;
        let payload: Value = resp.json().await?;
        let ok = payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        if !ok {
            let desc = payload
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("Telegram API request failed");
            anyhow::bail!("{}", desc);
        }
        Ok(payload.get("result").cloned().unwrap_or(json!({})))
    }

    fn parse_slash_command(text: &str) -> Option<(String, String)> {
        let trimmed = text.trim();
        // Some clients can prepend invisible directional marks before slash commands.
        let normalized = trimmed.trim_start_matches(|c: char| {
            c.is_whitespace()
                || c == '\u{200E}' // LRM
                || c == '\u{200F}' // RLM
                || c == '\u{2066}' // LRI
                || c == '\u{2067}' // RLI
                || c == '\u{2068}' // FSI
                || c == '\u{2069}' // PDI
        });
        if !normalized.starts_with('/') {
            return None;
        }

        let mut splitter = normalized.splitn(2, char::is_whitespace);
        let token = splitter.next().unwrap_or_default();
        let args = splitter.next().unwrap_or("").trim().to_string();
        let command_token = token.trim_start_matches('/');
        if command_token.is_empty() {
            return None;
        }
        let raw_command = command_token.split('@').next().unwrap_or("").trim();
        let command: String = raw_command
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect::<String>()
            .to_ascii_lowercase();
        if command.is_empty() {
            return None;
        }
        Some((command, args))
    }

    fn fallback_parse_known_command(text: &str) -> Option<(String, String)> {
        let trimmed = text.trim();
        let normalized = trimmed.trim_start_matches(|c: char| {
            c.is_whitespace()
                || c == '\u{200E}'
                || c == '\u{200F}'
                || c == '\u{2066}'
                || c == '\u{2067}'
                || c == '\u{2068}'
                || c == '\u{2069}'
        });
        let lower = normalized.to_ascii_lowercase();
        for name in ["workflows", "run", "help", "start", "settings"] {
            let prefix = format!("/{}", name);
            if lower == prefix
                || lower.starts_with(&(prefix.clone() + " "))
                || lower.starts_with(&(prefix + "@"))
            {
                let args = normalized
                    .splitn(2, char::is_whitespace)
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                return Some((name.to_string(), args));
            }
        }
        None
    }

    fn build_help_text() -> String {
        [
            "Available commands:",
            "",
            "/workflows — Browse & run workflows (tap to run)",
            "/workflows <keyword> — Filter workflows by name",
            "/run <name_or_id> — Run a workflow directly",
            "/help — Show this help",
            "/settings — Bot settings info",
        ]
        .join("\n")
    }

    fn format_workflow_output(node_results: &Value) -> String {
        let maybe_last = node_results.as_array().and_then(|arr| arr.last());
        if let Some(last) = maybe_last {
            if let Some(output) = last.get("output") {
                if output.is_string() {
                    let text = output.as_str().unwrap_or("").trim();
                    if !text.is_empty() {
                        return Self::truncate_text(text, 3600);
                    }
                }
                if !output.is_null() {
                    if let Ok(pretty) = serde_json::to_string_pretty(output) {
                        return Self::truncate_text(&pretty, 3600);
                    }
                }
            }
            if let Some(err) = last.get("error").and_then(|v| v.as_str()) {
                if !err.trim().is_empty() {
                    return Self::truncate_text(err.trim(), 3600);
                }
            }
        }
        "Workflow finished with no final output.".to_string()
    }

    fn truncate_text(text: &str, max_chars: usize) -> String {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() <= max_chars {
            return text.to_string();
        }
        let mut truncated: String = chars.into_iter().take(max_chars).collect();
        truncated.push_str("\n...(truncated)");
        truncated
    }

    fn parse_allowlist(raw: &str) -> HashSet<String> {
        raw.split(',')
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .collect()
    }

    fn workflow_access_allowed(
        state: &crate::state::AppState,
        chat_id: &str,
        user_id: Option<&str>,
    ) -> bool {
        let allowed_chat_ids_raw = state
            .settings
            .get_str("messaging.workflow_runner_chat_ids", "");
        let allowed_user_ids_raw = state
            .settings
            .get_str("messaging.workflow_runner_user_ids", "");

        let allowed_chats = Self::parse_allowlist(&allowed_chat_ids_raw);
        let allowed_users = Self::parse_allowlist(&allowed_user_ids_raw);

        let chat_ok = allowed_chats.is_empty() || allowed_chats.contains(chat_id);
        let user_ok = if allowed_users.is_empty() {
            true
        } else if let Some(uid) = user_id {
            allowed_users.contains(uid)
        } else {
            false
        };

        chat_ok && user_ok
    }

    fn fetch_enabled_workflows(
        state: &crate::state::AppState,
    ) -> anyhow::Result<Vec<WorkflowMenuItem>> {
        let conn = state.db.get()?;
        let mut stmt = conn
            .prepare("SELECT id, name FROM workflows WHERE enabled = 1 ORDER BY LOWER(name) ASC")?;
        let rows = stmt.query_map([], |r| {
            Ok(WorkflowMenuItem {
                id: r.get::<_, String>(0)?,
                name: r.get::<_, String>(1)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    fn slug_command_fragment(raw: &str) -> String {
        let mut out = String::new();
        let mut prev_underscore = false;
        for ch in raw.chars() {
            let c = ch.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() {
                out.push(c);
                prev_underscore = false;
            } else if (c == ' ' || c == '-' || c == '_') && !prev_underscore {
                out.push('_');
                prev_underscore = true;
            }
        }
        out.trim_matches('_').to_string()
    }

    fn build_workflow_slash_commands(state: &crate::state::AppState) -> Vec<WorkflowSlashCommand> {
        let workflows = Self::fetch_enabled_workflows(state).unwrap_or_default();
        let mut used: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for wf in workflows {
            let mut base = Self::slug_command_fragment(&wf.name);
            if base.is_empty() {
                base = Self::slug_command_fragment(&wf.id);
            }
            if base.is_empty() {
                continue;
            }
            let max_base_len = 32usize.saturating_sub(3); // "wf_" prefix
            if base.len() > max_base_len {
                base = base.chars().take(max_base_len).collect();
            }

            let mut cmd = format!("wf_{}", base);
            if used.contains(&cmd) {
                for n in 2..=999 {
                    let suffix = format!("_{}", n);
                    let allowed = 32usize.saturating_sub(3).saturating_sub(suffix.len());
                    let mut candidate_base = base.clone();
                    if candidate_base.len() > allowed {
                        candidate_base = candidate_base.chars().take(allowed).collect();
                    }
                    let candidate = format!("wf_{}{}", candidate_base, suffix);
                    if !used.contains(&candidate) {
                        cmd = candidate;
                        break;
                    }
                }
            }

            used.insert(cmd.clone());
            out.push(WorkflowSlashCommand {
                command: cmd,
                workflow_id: wf.id,
                workflow_name: wf.name,
            });
        }
        out
    }

    fn resolve_workflow_from_command(
        state: &crate::state::AppState,
        command: &str,
    ) -> Option<(String, String)> {
        let specs = Self::build_workflow_slash_commands(state);
        specs
            .into_iter()
            .find(|s| s.command == command)
            .map(|s| (s.workflow_id, s.workflow_name))
    }

    async fn send_workflow_command_list(
        &self,
        state: &crate::state::AppState,
        chat_id: &str,
        filter: Option<&str>,
    ) -> Result<()> {
        let workflows = match Self::fetch_enabled_workflows(state) {
            Ok(wf) => {
                tracing::info!(
                    "Telegram /workflows: fetched {} enabled workflows from DB",
                    wf.len()
                );
                wf
            }
            Err(e) => {
                tracing::error!("Telegram /workflows: DB fetch failed: {}", e);
                let _ = self
                    .send_text(chat_id, "Failed to load workflows from database.")
                    .await;
                return Ok(());
            }
        };

        let needle = filter.unwrap_or("").trim().to_ascii_lowercase();
        let filtered: Vec<WorkflowMenuItem> = if needle.is_empty() {
            workflows
        } else {
            workflows
                .into_iter()
                .filter(|w| {
                    w.name.to_ascii_lowercase().contains(&needle)
                        || w.id.to_ascii_lowercase().contains(&needle)
                })
                .collect()
        };

        if filtered.is_empty() {
            let text = if needle.is_empty() {
                "No enabled workflows available.".to_string()
            } else {
                format!(
                    "No workflows matched '{}'. Try /workflows without a filter.",
                    filter.unwrap_or("").trim()
                )
            };
            let _ = self.send_text(chat_id, &text).await;
            return Ok(());
        }

        tracing::info!(
            "Telegram /workflows: showing {} workflows (filter={:?})",
            filtered.len(),
            filter
        );

        // Build inline keyboard buttons — one workflow per row, tap to run.
        let max_buttons = 50usize;
        let total = filtered.len();
        let buttons: Vec<Value> = filtered
            .iter()
            .take(max_buttons)
            .map(|wf| {
                json!([{
                    "text": format!("▶ {}", wf.name),
                    "callback_data": format!("wf:run:{}", wf.id)
                }])
            })
            .collect();

        let mut text = "⚡ Select a workflow to run:".to_string();
        if total > max_buttons {
            text.push_str(&format!(
                "\n\nShowing {} of {}. Use /workflows <keyword> to filter.",
                max_buttons, total
            ));
        }

        let body = json!({
            "chat_id": chat_id,
            "text": text,
            "reply_markup": {
                "inline_keyboard": buttons
            }
        });

        match self.call_api("sendMessage", &body).await {
            Ok(_) => {
                tracing::info!("Telegram /workflows: inline keyboard sent successfully");
            }
            Err(e) => {
                tracing::error!(
                    "Telegram /workflows: inline keyboard failed: {} — falling back to text list",
                    e
                );
                // Fallback: send as plain text list so user always gets something
                let mut lines = vec!["Select a workflow to run:".to_string(), "".to_string()];
                for wf in filtered.iter().take(max_buttons) {
                    lines.push(format!("• {} (ID: {})", wf.name, wf.id));
                }
                lines.push("".to_string());
                lines.push("Use /run <name_or_id> to execute.".to_string());
                let fallback_text = lines.join("\n");
                let _ = self
                    .send_text(chat_id, &Self::truncate_text(&fallback_text, 3900))
                    .await;
            }
        }

        Ok(())
    }

    fn resolve_workflow_id_by_name_or_id(
        state: &crate::state::AppState,
        needle: &str,
    ) -> anyhow::Result<Option<(String, String)>> {
        let query = needle.trim();
        if query.is_empty() {
            return Ok(None);
        }
        let conn = state.db.get()?;

        if let Ok((id, name)) = conn.query_row(
            "SELECT id, name FROM workflows WHERE enabled = 1 AND (id = ?1 OR name = ?1) LIMIT 1",
            rusqlite::params![query],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ) {
            return Ok(Some((id, name)));
        }

        if let Ok((id, name)) = conn.query_row(
            "SELECT id, name FROM workflows WHERE enabled = 1 AND LOWER(name) = LOWER(?1) LIMIT 1",
            rusqlite::params![query],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        ) {
            return Ok(Some((id, name)));
        }

        Ok(None)
    }

    /// True if `workflow_id` has an *enabled* telegram trigger (stimulus) node
    /// whose configured chat_ids match `chat_id` (empty chat_ids = any chat).
    /// Used to gate reply-to-message routing: a reply is only re-fed into the
    /// sending workflow when that workflow can actually receive it on a telegram
    /// trigger for this chat — otherwise re-running it would just restart the
    /// workflow from its real entry node (e.g. a Gmail search), not "reprocess".
    fn workflow_has_telegram_trigger_for_chat(
        state: &crate::state::AppState,
        workflow_id: &str,
        chat_id: &str,
    ) -> bool {
        let Ok(conn) = state.db.get() else {
            return false;
        };
        let Ok(mut stmt) = conn.prepare(
            "SELECT config FROM workflow_nodes \
             WHERE workflow_id = ?1 AND node_type = 'stimulus' AND enabled = 1",
        ) else {
            return false;
        };
        let Ok(rows) = stmt.query_map([workflow_id], |r| r.get::<_, String>(0)) else {
            return false;
        };
        for cfg_str in rows.flatten() {
            let Ok(cfg) = serde_json::from_str::<Value>(&cfg_str) else {
                continue;
            };
            if cfg.get("type").and_then(|v| v.as_str()) != Some("telegram") {
                continue;
            }
            if chat_ids_match(cfg.get("chat_ids"), chat_id) {
                return true;
            }
        }
        false
    }

    /// If `message_id` (in `chat_id`) was sent by a workflow, return its
    /// workflow id. Populated by the workflow engine whenever a telegram node
    /// sends a message. A `None` here means the replied-to message did not come
    /// from a workflow (e.g. it was an agent message) → handle normally.
    fn lookup_reply_route(
        state: &crate::state::AppState,
        chat_id: &str,
        message_id: i64,
    ) -> Option<String> {
        let conn = state.db.get().ok()?;
        conn.query_row(
            "SELECT workflow_id FROM telegram_reply_routes WHERE chat_id = ?1 AND message_id = ?2 LIMIT 1",
            rusqlite::params![chat_id, message_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
    }

    fn fetch_workflow_run_summary(
        state: &crate::state::AppState,
        run_id: &str,
    ) -> anyhow::Result<Option<WorkflowRunSummary>> {
        let conn = state.db.get()?;
        let row = conn.query_row(
            "SELECT status, finished_at, node_results FROM workflow_runs WHERE id = ?1 LIMIT 1",
            rusqlite::params![run_id],
            |r| {
                let status: String = r.get(0)?;
                let finished_at: Option<String> = r.get(1)?;
                let node_results_str: String = r.get(2)?;
                // B2: rehydrate any offloaded binary descriptors so the reply
                // shows real output, never a `{_axon_binary}` placeholder.
                let mut node_results =
                    serde_json::from_str::<Value>(&node_results_str).unwrap_or(json!([]));
                crate::tools::workflow::binary::rehydrate_value(&mut node_results);
                Ok(WorkflowRunSummary {
                    status,
                    finished_at,
                    node_results,
                })
            },
        );

        match row {
            Ok(run) => Ok(Some(run)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn run_workflow_and_report(
        self: Arc<Self>,
        state: Arc<crate::state::AppState>,
        chat_id: String,
        workflow_id: String,
        workflow_name: String,
        trigger_payload: Option<serde_json::Value>,
    ) {
        let start_text = format!("Running workflow: {}", workflow_name);
        let status_mid = self
            .send_text(&chat_id, &start_text)
            .await
            .unwrap_or_default();

        // Kept on the "manual" source (chat-invoked run: all triggers eligible,
        // pins apply), but any Telegram event context rides along staged for
        // this run so a telegram-type stimulus node can read it.
        let run_id = match crate::tools::workflow::WorkflowEngine::run_in_background_with_payload(
            &workflow_id,
            &state,
            "manual",
            None,
            trigger_payload,
        ) {
            Ok(id) => id,
            Err(e) => {
                let _ = self
                    .send_text(
                        &chat_id,
                        &format!("Failed to start workflow '{}': {}", workflow_name, e),
                    )
                    .await;
                return;
            }
        };

        let deadline =
            std::time::Instant::now() + Duration::from_secs(WORKFLOW_RUN_POLL_MAX_SECONDS);
        loop {
            if std::time::Instant::now() >= deadline {
                let _ = self
                    .send_text(
                        &chat_id,
                        &format!(
                            "Workflow '{}' is still running (run_id: {}). Check workflow history for final status.",
                            workflow_name, run_id
                        ),
                    )
                    .await;
                break;
            }

            match Self::fetch_workflow_run_summary(&state, &run_id) {
                Ok(Some(run)) => {
                    if run.status != "running" {
                        let output = Self::format_workflow_output(&run.node_results);
                        let finished_stamp =
                            run.finished_at.unwrap_or_else(|| "unknown".to_string());
                        let final_text = format!(
                            "Workflow '{}' completed.\nStatus: {}\nFinished: {}\nRun ID: {}\n\n{}",
                            workflow_name, run.status, finished_stamp, run_id, output
                        );
                        let final_text = Self::truncate_text(&final_text, 3900);
                        if !status_mid.is_empty() {
                            let _ = self.edit_text(&chat_id, &status_mid, &final_text).await;
                        } else {
                            let _ = self.send_text(&chat_id, &final_text).await;
                        }
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = self
                        .send_text(
                            &chat_id,
                            &format!(
                                "Workflow '{}' started (run_id: {}), but status polling failed: {}",
                                workflow_name, run_id, e
                            ),
                        )
                        .await;
                    break;
                }
            }

            sleep(Duration::from_secs(WORKFLOW_RUN_POLL_INTERVAL_SECONDS)).await;
        }
    }

    async fn handle_slash_command(
        self: Arc<Self>,
        state: Arc<crate::state::AppState>,
        chat_id: String,
        user_id: Option<String>,
        command: &str,
        args: &str,
    ) -> bool {
        tracing::info!(
            "Telegram intercepted slash command: /{} with args: '{}' from chat_id: {}",
            command,
            args,
            chat_id
        );

        if let Some((workflow_id, workflow_name)) =
            Self::resolve_workflow_from_command(&state, command)
        {
            if !Self::workflow_access_allowed(&state, &chat_id, user_id.as_deref()) {
                let _ = self
                    .send_text(
                        &chat_id,
                        "You are not authorized to run workflows from this chat.",
                    )
                    .await;
                return true;
            }
            let this = Arc::clone(&self);
            let state2 = Arc::clone(&state);
            let cid = chat_id.clone();
            tokio::spawn(async move {
                this.run_workflow_and_report(state2, cid, workflow_id, workflow_name, None)
                    .await;
            });
            return true;
        }

        match command {
            "start" | "help" => {
                let _ = self.send_text(&chat_id, &Self::build_help_text()).await;
                // Also show workflow buttons for quick access
                if Self::workflow_access_allowed(&state, &chat_id, user_id.as_deref()) {
                    let _ = self
                        .send_workflow_command_list(&state, &chat_id, None)
                        .await;
                }
                true
            }
            "settings" => {
                let msg = [
                    "Bot settings tip:",
                    "- Configure Telegram token in Services -> Messaging.",
                    "- Use /workflows to run saved workflows from chat.",
                    "- Use /run <workflow_name_or_id> for direct execution.",
                ]
                .join("\n");
                let _ = self.send_text(&chat_id, &msg).await;
                true
            }
            "workflows" => {
                tracing::info!(
                    "Telegram: /workflows command received from chat_id={}",
                    chat_id
                );
                if !Self::workflow_access_allowed(&state, &chat_id, user_id.as_deref()) {
                    tracing::warn!(
                        "Telegram: /workflows blocked — access denied for chat_id={} user_id={:?}",
                        chat_id,
                        user_id
                    );
                    let _ = self
                        .send_text(
                            &chat_id,
                            "You are not authorized to browse/run workflows from this chat.",
                        )
                        .await;
                    return true;
                }
                // Refresh the / command menu in the background
                if let Err(e) = self.ensure_default_commands(&state).await {
                    tracing::warn!("Telegram: ensure_default_commands failed: {}", e);
                }
                let filter = args.trim();
                let filter_opt = if filter.is_empty() {
                    None
                } else {
                    Some(filter)
                };
                if let Err(e) = self
                    .send_workflow_command_list(&state, &chat_id, filter_opt)
                    .await
                {
                    tracing::error!("Telegram: send_workflow_command_list failed: {}", e);
                    let _ = self
                        .send_text(&chat_id, "Failed to load workflow list.")
                        .await;
                }
                true
            }
            "run" => {
                if !Self::workflow_access_allowed(&state, &chat_id, user_id.as_deref()) {
                    let _ = self
                        .send_text(
                            &chat_id,
                            "You are not authorized to run workflows from this chat.",
                        )
                        .await;
                    return true;
                }
                if args.trim().is_empty() {
                    let _ = self
                        .send_text(&chat_id, "Usage: /run <workflow_name_or_id>")
                        .await;
                    let _ = self
                        .send_workflow_command_list(&state, &chat_id, None)
                        .await;
                    return true;
                }

                match Self::resolve_workflow_id_by_name_or_id(&state, args.trim()) {
                    Ok(Some((workflow_id, workflow_name))) => {
                        let this = Arc::clone(&self);
                        let state2 = Arc::clone(&state);
                        let cid = chat_id.clone();
                        tokio::spawn(async move {
                            this.run_workflow_and_report(
                                state2,
                                cid,
                                workflow_id,
                                workflow_name,
                                None,
                            )
                            .await;
                        });
                        true
                    }
                    Ok(None) => {
                        let _ = self
                            .send_text(
                                &chat_id,
                                "Workflow not found or not enabled. Use /workflows to browse available workflows.",
                            )
                            .await;
                        true
                    }
                    Err(e) => {
                        let _ = self
                            .send_text(&chat_id, &format!("Could not resolve workflow: {}", e))
                            .await;
                        true
                    }
                }
            }
            _ => false,
        }
    }

    async fn poll_once(
        self: Arc<Self>,
        offset: i64,
        state: Arc<crate::state::AppState>,
    ) -> Result<i64> {
        let resp = self
            .client
            .get(self.api("getUpdates"))
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", "30".into()),
                ("allowed_updates", r#"["message", "callback_query"]"#.into()),
            ])
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;
        let updates = resp
            .get("result")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let mut next = offset;
        for u in &updates {
            let uid = u.get("update_id").and_then(|v| v.as_i64()).unwrap_or(0);
            next = next.max(uid + 1);

            // Handle callback_query FIRST to ensure priority routing
            if let Some(cbq) = u.get("callback_query").cloned() {
                let this = Arc::clone(&self);
                let st = Arc::clone(&state);
                tokio::spawn(async move { this.handle_callback_query(&cbq, st).await });
            } else if let Some(msg) = u.get("message").cloned() {
                let this = Arc::clone(&self);
                let st = Arc::clone(&state);
                tokio::spawn(async move { this.handle_update(&msg, st).await });
            }
        }
        Ok(next)
    }

    fn infer_saved_callback_route(
        state: &crate::state::AppState,
        callback_data: &str,
        chat_id: &str,
        message_text: Option<&str>,
        message_caption: Option<&str>,
    ) -> Option<bool> {
        let Ok(conn) = state.db.get() else {
            return None;
        };

        let configs: Vec<Value> = conn
            .prepare(
                "SELECT wn.config
                 FROM workflow_nodes wn
                 INNER JOIN workflows w ON w.id = wn.workflow_id
                 WHERE w.enabled = 1 AND wn.node_type = 'telegram'",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, String>(0))
                    .map(|rows| {
                        rows.filter_map(|row| {
                            row.ok()
                                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                        })
                        .collect()
                    })
            })
            .unwrap_or_default();

        infer_callback_route_from_configs(
            &configs,
            callback_data,
            chat_id,
            message_text,
            message_caption,
        )
    }

    async fn handle_callback_query(
        self: Arc<Self>,
        cbq: &serde_json::Value,
        state: Arc<crate::state::AppState>,
    ) {
        let chat_id = cbq
            .pointer("/message/chat/id")
            .and_then(|v| v.as_i64())
            .map(|id| id.to_string())
            .unwrap_or_default();
        let raw_data = cbq
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let (explicit_route_to_trigger, data) =
            if let Some(stripped) = raw_data.strip_prefix("trig:") {
                (Some(true), stripped.to_string())
            } else if let Some(stripped) = raw_data.strip_prefix("agent:") {
                (Some(false), stripped.to_string())
            } else {
                (None, raw_data.clone())
            };
        let cbq_id = cbq
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        tracing::info!(
            "[TELEGRAM] Callback Query data raw='{}' normalized='{}' explicit_route={:?} chat_id={}",
            raw_data,
            data,
            explicit_route_to_trigger,
            chat_id
        );
        let user_id = cbq
            .pointer("/from/id")
            .and_then(|v| v.as_i64())
            .map(|v| v.to_string());
        let msg = cbq.get("message");
        let text = msg
            .and_then(|m| m.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let caption = msg
            .and_then(|m| m.get("caption"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if data.is_empty() || chat_id.is_empty() {
            return;
        }

        // Answer the callback query first to stop Telegram's loading spinner.
        if !cbq_id.is_empty() {
            let _ = self
                .client
                .post(self.api("answerCallbackQuery"))
                .json(&serde_json::json!({"callback_query_id": cbq_id}))
                .send()
                .await;
        }

        if data == "wf:noop" {
            return;
        }

        if explicit_route_to_trigger != Some(false)
            && data.starts_with("wf:")
            && !Self::workflow_access_allowed(&state, &chat_id, user_id.as_deref())
        {
            let _ = self
                .send_text(
                    &chat_id,
                    "You are not authorized to browse/run workflows from this chat.",
                )
                .await;
            return;
        }

        if explicit_route_to_trigger != Some(false) {
            if let Some(workflow_id) = data.strip_prefix("wf:run:") {
                let resolved = Self::resolve_workflow_id_by_name_or_id(&state, workflow_id)
                    .ok()
                    .flatten();
                if let Some((wf_id, wf_name)) = resolved {
                    let this = Arc::clone(&self);
                    let state2 = Arc::clone(&state);
                    let cid = chat_id.clone();
                    tokio::spawn(async move {
                        this.run_workflow_and_report(state2, cid, wf_id, wf_name, None)
                            .await;
                    });
                } else {
                    let _ = self
                        .send_text(
                            &chat_id,
                            "That workflow is unavailable. Use /workflows to refresh the list.",
                        )
                        .await;
                }
                return;
            }
        }

        // Toggle ON (trig: prefix) → run_workflow tool with callback data as the workflow name.
        let route_to_trigger = explicit_route_to_trigger.unwrap_or_else(|| {
            Self::infer_saved_callback_route(
                &state,
                &data,
                &chat_id,
                Some(text.as_str()),
                Some(caption.as_str()),
            )
            .unwrap_or(false)
        });

        if route_to_trigger {
            // Resolve the workflow by name or ID, store trigger data so the stimulus node
            // can read the full callback context, then run the workflow directly.
            tracing::info!(
                "[TELEGRAM] trig: button → resolving and running workflow '{}'",
                data
            );
            let state_clone = Arc::clone(&state);
            let wf_name = data.clone();
            let trigger_data = json!({
                "trigger": "telegram",
                "events": [{
                    "type": "callback_query",
                    "chat_id": chat_id,
                    "data": data,
                    "from": cbq.get("from").cloned().unwrap_or(json!({})),
                    "message": msg.cloned().unwrap_or(json!({}))
                }]
            });
            tokio::spawn(async move {
                match Self::resolve_workflow_id_by_name_or_id(&state_clone, &wf_name) {
                    Ok(Some((wf_id, _))) => {
                        // The full callback context rides the spawn call so the
                        // stimulus node of THIS run receives it.
                        if let Err(e) =
                            crate::tools::workflow::WorkflowEngine::run_in_background_with_payload(
                                &wf_id,
                                &state_clone,
                                "telegram",
                                None,
                                Some(trigger_data),
                            )
                        {
                            tracing::error!(
                                "[TELEGRAM] WorkflowEngine failed for '{}' (id={}): {}",
                                wf_name,
                                wf_id,
                                e
                            );
                        }
                    }
                    Ok(None) => {
                        tracing::error!(
                            "[TELEGRAM] trig: workflow named '{}' not found or not enabled",
                            wf_name
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "[TELEGRAM] trig: workflow resolution error for '{}': {}",
                            wf_name,
                            e
                        );
                    }
                }
            });
            return;
        }

        let file_id = msg
            .and_then(|m| m.get("document"))
            .and_then(|d| d.get("file_id"))
            .or_else(|| {
                msg.and_then(|m| m.get("photo"))
                    .and_then(|a| a.as_array())
                    .and_then(|a| a.last())
                    .and_then(|p| p.get("file_id"))
            })
            .or_else(|| {
                msg.and_then(|m| m.get("audio"))
                    .and_then(|d| d.get("file_id"))
            })
            .or_else(|| {
                msg.and_then(|m| m.get("video"))
                    .and_then(|d| d.get("file_id"))
            })
            .or_else(|| {
                msg.and_then(|m| m.get("voice"))
                    .and_then(|d| d.get("file_id"))
            })
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let original_filename = msg
            .and_then(|m| m.get("document"))
            .and_then(|d| d.get("file_name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if msg.and_then(|m| m.get("photo")).is_some() {
                    "attachment.jpg".to_string()
                } else if msg.and_then(|m| m.get("video")).is_some() {
                    "attachment.mp4".to_string()
                } else if msg.and_then(|m| m.get("audio")).is_some()
                    || msg.and_then(|m| m.get("voice")).is_some()
                {
                    "attachment.ogg".to_string()
                } else {
                    "attachment.bin".to_string()
                }
            });

        let mut attached = Vec::new();
        if let Some(fid) = &file_id {
            match self.download_to_staging(fid, &original_filename).await {
                Ok(af) => attached.push(af),
                Err(e) => tracing::warn!("Failed to download Telegram file: {}", e),
            }
        }

        // Toggle OFF (agent: prefix or default) → send callback data as instruction to the main agent.
        // The "(Button Clicked):" prefix is essential — without it the agent mistakes the
        // callback data for a workflow/agent name and tries to look it up instead of acting on it.
        tracing::info!(
            "[TELEGRAM] agent: button → task '{}' (chat={})",
            data,
            chat_id
        );

        let mut effective_text = format!("(Button Clicked):\n{}", data);
        if !text.is_empty() {
            effective_text.push_str(&format!("\n\nOriginal Text:\n{}", text));
        }
        if !caption.is_empty() {
            effective_text.push_str(&format!("\n\nOriginal Caption:\n{}", caption));
        }

        if let Some(first_file) = attached.first() {
            effective_text.push_str(&format!(
                "\n\n[SYSTEM HINT: The exact local path of the newly attached media is: '{}'. You MUST use this path in your tools rather than any old ones mentioned above.]",
                first_file.local_path
            ));
        }

        let mut ctx = crate::agent::RunContext::new(
            &effective_text,
            "telegram",
            Some(&chat_id),
            Some(&chat_id),
            None,
            None,
            None,
        );
        ctx.attached_files = attached;

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let s2 = Arc::clone(&state);
        let self2 = Arc::clone(&self);
        let chat_id2 = chat_id.clone();
        let t2 = effective_text.clone();

        tokio::spawn(async move {
            let _ = crate::agent::run_task_streaming(&t2, &*s2, ctx, tx).await;
        });

        let _ = super::streaming::stream_to_gateway(rx, self2, chat_id2).await;
    }

    async fn handle_update(
        self: Arc<Self>,
        msg: &serde_json::Value,
        state: Arc<crate::state::AppState>,
    ) {
        let chat_id = msg
            .pointer("/chat/id")
            .and_then(|v| v.as_i64())
            .map(|id| id.to_string())
            .unwrap_or_default();
        let text = msg
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let caption = msg
            .get("caption")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let user_id = msg
            .pointer("/from/id")
            .and_then(|v| v.as_i64())
            .map(|v| v.to_string());

        tracing::info!(
            "[LOCAL_BOT_V2] Received message: '{}' from chat_id={}",
            text,
            chat_id
        );

        // Hard-coded override for /workflows to prevent any fallthrough to AI agent
        let trimmed_lower = text.trim().to_ascii_lowercase();
        if trimmed_lower == "/workflows" || trimmed_lower.starts_with("/workflows ") {
            let args = if trimmed_lower.len() > 10 {
                text.trim()[10..].trim().to_string()
            } else {
                "".to_string()
            };
            tracing::info!(
                "[LOCAL_BOT_V2] Forced interception of /workflows with args: '{}'",
                args
            );
            let _ = Arc::clone(&self)
                .handle_slash_command(
                    Arc::clone(&state),
                    chat_id.clone(),
                    user_id.clone(),
                    "workflows",
                    &args,
                )
                .await;
            return;
        }

        if let Some((command, args)) = Self::parse_slash_command(&text) {
            tracing::debug!("Telegram slash command: /{} args='{}'", command, args);
            let handled = Arc::clone(&self)
                .handle_slash_command(
                    Arc::clone(&state),
                    chat_id.clone(),
                    user_id.clone(),
                    &command,
                    &args,
                )
                .await;
            if handled {
                return;
            }
            // Safety guard: never let built-in or workflow commands fall through to the AI agent.
            let is_builtin = matches!(
                command.as_str(),
                "start" | "help" | "settings" | "workflows" | "run"
            );
            if is_builtin || command.starts_with("wf_") {
                tracing::warn!(
                    "Slash command /{} was not marked as handled — blocking agent fallthrough",
                    command
                );
                return;
            }
        }

        if let Some((command, args)) = Self::fallback_parse_known_command(&text) {
            tracing::debug!(
                "Telegram fallback slash command parse: /{} args='{}'",
                command,
                args
            );
            let handled = Arc::clone(&self)
                .handle_slash_command(
                    Arc::clone(&state),
                    chat_id.clone(),
                    user_id.clone(),
                    &command,
                    &args,
                )
                .await;
            if handled {
                return;
            }
            return;
        }

        // ── Reply-to-workflow routing ──────────────────────────────────────
        // If this message is a "reply to" a Telegram message that a workflow
        // sent, deliver the reply (plus the original replied-to message) to that
        // workflow's telegram trigger node and stop — do NOT fall through to the
        // main agent. Slash commands were already handled above, so they are
        // unaffected. Replies to non-workflow messages have no route entry and
        // continue to the normal handling below (ending at the main agent).
        //
        // Set when a reply targets a workflow-sent message but that workflow has
        // no telegram trigger node able to receive it — carries the workflow name
        // so the agent fall-through can explain why nothing was reprocessed.
        let mut reply_workflow_hint: Option<String> = None;
        if let Some(reply) = msg.get("reply_to_message") {
            let replied_msg_id = reply
                .get("message_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if replied_msg_id > 0 {
                let route = Self::lookup_reply_route(&state, &chat_id, replied_msg_id);
                if route.is_none() {
                    tracing::debug!(
                        "[TELEGRAM] Reply to message {} in chat {} has no recorded workflow route — handling as a normal message (agent)",
                        replied_msg_id,
                        chat_id
                    );
                }
                if let Some(stored_wf) = route {
                    // Confirm the workflow still exists & is enabled, and the
                    // chat is allowed to run workflows; otherwise fall through.
                    if let Ok(Some((wf_id, wf_name))) =
                        Self::resolve_workflow_id_by_name_or_id(&state, &stored_wf)
                    {
                        if Self::workflow_access_allowed(&state, &chat_id, user_id.as_deref()) {
                            // Only re-feed the reply into the workflow if it can
                            // actually receive it: it must have an enabled telegram
                            // trigger (stimulus) node for this chat. Otherwise the
                            // reply is meant for "reprocessing" but there is nothing
                            // to receive it — fall through to the agent with a hint.
                            if Self::workflow_has_telegram_trigger_for_chat(
                                &state, &wf_id, &chat_id,
                            ) {
                                let replied_text = reply
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| reply.get("caption").and_then(|v| v.as_str()))
                                    .unwrap_or("");
                                let trigger_data = json!({
                                    "trigger": "telegram",
                                    "events": [{
                                        "type": "reply",
                                        "chat_id": chat_id,
                                        "text": text.clone(),
                                        "caption": caption.clone(),
                                        "from": msg.get("from").cloned().unwrap_or_else(|| json!({})),
                                        "message": msg.clone(),
                                        "reply_to_message": reply.clone(),
                                        "replied_text": replied_text,
                                    }]
                                });
                                tracing::info!(
                                    "[TELEGRAM] Reply to workflow message → routing to workflow '{}' (id={})",
                                    wf_name,
                                    wf_id
                                );
                                let state_clone = Arc::clone(&state);
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        crate::tools::workflow::WorkflowEngine::run_in_background_with_payload(
                                            &wf_id,
                                            &state_clone,
                                            "telegram",
                                            None,
                                            Some(trigger_data),
                                        )
                                    {
                                        tracing::error!(
                                            "[TELEGRAM] reply→workflow run failed (id={}): {}",
                                            wf_id,
                                            e
                                        );
                                    }
                                });
                                return;
                            } else {
                                tracing::info!(
                                    "[TELEGRAM] Reply targets workflow '{}' (id={}) which has no enabled telegram trigger node for chat {} — falling through to agent with hint",
                                    wf_name,
                                    wf_id,
                                    chat_id
                                );
                                reply_workflow_hint = Some(wf_name.clone());
                            }
                        }
                    }
                }
            }
        }

        // Extract file_id from document, photo, audio, video, or voice
        let file_id = msg
            .get("document")
            .and_then(|d| d.get("file_id"))
            .or_else(|| {
                msg.get("photo")
                    .and_then(|a| a.as_array())
                    .and_then(|a| a.last())
                    .and_then(|p| p.get("file_id"))
            })
            .or_else(|| msg.get("audio").and_then(|d| d.get("file_id")))
            .or_else(|| msg.get("video").and_then(|d| d.get("file_id")))
            .or_else(|| msg.get("voice").and_then(|d| d.get("file_id")))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let original_filename = msg
            .get("document")
            .and_then(|d| d.get("file_name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                if msg.get("photo").is_some() {
                    "attachment.jpg".to_string()
                } else if msg.get("video").is_some() {
                    "attachment.mp4".to_string()
                } else if msg.get("audio").is_some() || msg.get("voice").is_some() {
                    "attachment.ogg".to_string()
                } else {
                    "attachment.bin".to_string()
                }
            });

        // Download attached file to staging directory
        let mut attached = Vec::new();
        if let Some(fid) = &file_id {
            match self.download_to_staging(fid, &original_filename).await {
                Ok(af) => attached.push(af),
                Err(e) => tracing::warn!("Failed to download Telegram file: {}", e),
            }
        }

        let query_text = if !text.is_empty() {
            text.trim()
        } else {
            caption.trim()
        };

        if !query_text.is_empty() {
            if let Ok(Some((wf_id, wf_name))) =
                Self::resolve_workflow_id_by_name_or_id(&state, query_text)
            {
                tracing::info!(
                    "[TELEGRAM] Exact match to workflow name '{}' → running workflow instead of agent.",
                    wf_name
                );

                if !Self::workflow_access_allowed(&state, &chat_id, user_id.as_deref()) {
                    let _ = self
                        .send_text(
                            &chat_id,
                            "You are not authorized to run workflows from this chat.",
                        )
                        .await;
                    return;
                }

                let state_clone = Arc::clone(&state);
                let chat_id_clone = chat_id.clone();
                let wf_id_clone = wf_id.clone();
                let self_clone = Arc::clone(&self);

                let trigger_data = serde_json::json!({
                    "trigger": "telegram",
                    "events": [{
                        "type": "message",
                        "chat_id": chat_id_clone,
                        "text": text,
                        "caption": caption,
                        "from": msg.get("from").cloned().unwrap_or_else(|| serde_json::json!({})),
                        "message": msg.clone()
                    }]
                });

                tokio::spawn(async move {
                    self_clone
                        .run_workflow_and_report(
                            state_clone,
                            chat_id_clone,
                            wf_id_clone,
                            wf_name,
                            Some(trigger_data),
                        )
                        .await;
                });
                return;
            }
        }

        let mut effective_text = if !text.is_empty() {
            text
        } else if !caption.is_empty() {
            caption
        } else if !attached.is_empty() {
            "User sent a file.".to_string()
        } else {
            return;
        };

        let mut target_id = None;
        if let Some(reply) = msg.get("reply_to_message") {
            let replied_text = reply.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let replied_msg_id = reply
                .get("message_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            if replied_msg_id > 0 {
                if let Ok(conn) = state.db.get() {
                    let search_str = format!("%\"telegram_msg_id\":\"{}\"%", replied_msg_id);
                    if let Ok(mut stmt) = conn.prepare("SELECT metadata FROM memory_short WHERE session_id = ?1 AND metadata LIKE ?2 ORDER BY id DESC LIMIT 1") {
                        if let Ok(mut rows) = stmt.query(rusqlite::params![chat_id, search_str]) {
                            if let Ok(Some(row)) = rows.next() {
                                if let Ok(Some(meta)) = row.get::<_, Option<String>>(0) {
                                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&meta) {
                                        if let Some(tid) = json.get("target_id").and_then(|v| v.as_str()) {
                                            target_id = Some(tid.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if !replied_text.is_empty() {
                if let Some(tid) = target_id {
                    effective_text = format!(
                        "(In reply to your previous message [hidden Context ID: {}]: \"{}\")\n\n{}",
                        tid, replied_text, effective_text
                    );
                } else {
                    effective_text = format!(
                        "(In reply to your previous message: \"{}\")\n\n{}",
                        replied_text, effective_text
                    );
                }
            }
        }

        if let Some(first_file) = attached.first() {
            effective_text = format!(
                "{}\n\n[SYSTEM HINT: The exact local path of the newly attached media is: '{}'. You MUST use this path rather than any old ones mentioned above.]",
                effective_text, first_file.local_path
            );
        }

        // The reply was meant for a workflow that has no telegram trigger node to
        // receive it. Don't act on the content — instruct the agent to explain and
        // tell the user to add a telegram trigger (stimulus) node.
        if let Some(wf_name) = &reply_workflow_hint {
            effective_text = format!(
                "[SYSTEM INSTRUCTION — Do NOT act on or process the user's message content below. \
The user replied to a Telegram message that was sent by the workflow \"{wf}\", expecting that \
workflow to reprocess or edit it. That workflow has no enabled Telegram trigger (stimulus) node \
for this chat, so it cannot receive the reply. Reply briefly: tell the user their reply to the \
\"{wf}\" workflow could not be processed because it has no Telegram trigger (stimulus) node set \
for this chat, and that they should add a Telegram trigger (stimulus) node (configured for this \
chat) to the \"{wf}\" workflow so it can receive replies. Do not attempt the task itself.]\n\n\
User's reply was:\n{body}",
                wf = wf_name,
                body = effective_text,
            );
        }

        let mut ctx = crate::agent::RunContext::new(
            &effective_text,
            "telegram",
            Some(&chat_id), // Use chat_id as session ID to isolate contexts
            Some(&chat_id),
            None,
            None,
            None,
        );
        ctx.attached_files = attached;

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let s2 = Arc::clone(&state);
        let self2 = Arc::clone(&self);
        let chat_id2 = chat_id.clone();
        let t2 = effective_text.clone();

        tokio::spawn(async move {
            let _ = crate::agent::run_task_streaming(&t2, &*s2, ctx, tx).await;
        });

        let _ = super::streaming::stream_to_gateway(rx, self2, chat_id2).await;
    }

    /// Download a Telegram file to the staging directory and return AttachedFile metadata.
    async fn download_to_staging(
        &self,
        file_id: &str,
        filename: &str,
    ) -> anyhow::Result<crate::files::AttachedFile> {
        let resp: serde_json::Value = self
            .client
            .get(self.api("getFile"))
            .query(&[("file_id", file_id)])
            .send()
            .await?
            .json()
            .await?;

        let file_path = resp
            .pointer("/result/file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("No file_path in getFile response"))?;

        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.token, file_path
        );

        let bytes = self.client.get(&download_url).send().await?.bytes().await?;
        let size = bytes.len() as u64;
        let mime = mime_guess::from_path(filename)
            .first_or_octet_stream()
            .to_string();
        let staged_path = crate::files::stage_bytes(&bytes, filename)?;

        Ok(crate::files::AttachedFile {
            original_name: filename.to_string(),
            local_path: staged_path.display().to_string(),
            mime_type: mime,
            size,
        })
    }

    async fn send_chunked(&self, chat_id: &str, text: &str) -> Result<String> {
        let mut last_id = String::new();
        for chunk in text.as_bytes().chunks(4096) {
            last_id = self
                .send_text(chat_id, &String::from_utf8_lossy(chunk))
                .await?;
        }
        Ok(last_id)
    }
}

#[async_trait]
impl MessageGateway for TelegramGateway {
    fn platform_name(&self) -> &str {
        "telegram"
    }
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String> {
        let resp = self
            .client
            .post(self.api("sendMessage"))
            .json(&serde_json::json!({"chat_id":chat_id,"text":text}))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Telegram: {}", resp.text().await.unwrap_or_default());
        }
        let body: serde_json::Value = resp.json().await?;
        let msg_id = body
            .pointer("/result/message_id")
            .and_then(|v| v.as_i64())
            .map(|id| id.to_string())
            .unwrap_or_default();
        Ok(msg_id)
    }
    async fn send_file(&self, chat_id: &str, file: OutgoingFile) -> Result<()> {
        let part = reqwest::multipart::Part::bytes(file.data)
            .file_name(file.filename)
            .mime_str(&file.mime_type)?;
        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);
        let resp = self
            .client
            .post(self.api("sendDocument"))
            .multipart(form)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Telegram file: {}", resp.text().await.unwrap_or_default());
        }
        Ok(())
    }
    async fn send_message(&self, chat_id: &str, msg: OutgoingMessage) -> Result<String> {
        let mut last_id = String::new();
        if let Some(t) = &msg.text {
            last_id = self.send_chunked(chat_id, t).await?;
        }
        for f in msg.files {
            self.send_file(chat_id, f).await?;
        }
        Ok(last_id)
    }
    async fn edit_text(&self, chat_id: &str, message_id: &str, text: &str) -> Result<()> {
        let resp = self
            .client
            .post(self.api("editMessageText"))
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "message_id": message_id.parse::<i64>().unwrap_or(0),
                "text": text
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Telegram edit: {}", resp.text().await.unwrap_or_default());
        }
        Ok(())
    }
}

/// Backoff for consecutive long-poll failures: 5s doubling to a 5-minute cap
/// (5, 10, 20, 40, 80, 160, 300, 300, …). A sustained outage or 429 rate limit
/// is no longer hammered every 5 seconds; one success resets the counter.
fn poll_backoff_secs(consecutive_errors: u32) -> u64 {
    let shift = consecutive_errors.saturating_sub(1).min(6);
    (5u64 << shift).min(300)
}

#[cfg(test)]
mod tests {
    use super::chat_ids_match;
    use super::infer_callback_route_from_configs;
    use super::poll_backoff_secs;
    use super::TelegramGateway;
    use serde_json::json;

    #[test]
    fn poll_backoff_doubles_and_caps() {
        assert_eq!(poll_backoff_secs(1), 5);
        assert_eq!(poll_backoff_secs(2), 10);
        assert_eq!(poll_backoff_secs(3), 20);
        assert_eq!(poll_backoff_secs(6), 160);
        assert_eq!(poll_backoff_secs(7), 300);
        assert_eq!(poll_backoff_secs(100), 300);
        assert_eq!(poll_backoff_secs(u32::MAX), 300);
    }

    #[test]
    fn chat_ids_match_single_string() {
        let cfg = json!({ "chat_ids": "6967671873" });
        assert!(chat_ids_match(cfg.get("chat_ids"), "6967671873"));
        assert!(!chat_ids_match(cfg.get("chat_ids"), "123"));
    }

    #[test]
    fn chat_ids_match_comma_separated() {
        let cfg = json!({ "chat_ids": "111, 6967671873 ,222" });
        assert!(chat_ids_match(cfg.get("chat_ids"), "6967671873"));
        assert!(chat_ids_match(cfg.get("chat_ids"), "222"));
        assert!(!chat_ids_match(cfg.get("chat_ids"), "333"));
    }

    #[test]
    fn chat_ids_match_array_of_numbers_and_strings() {
        let cfg = json!({ "chat_ids": [111, "6967671873"] });
        assert!(chat_ids_match(cfg.get("chat_ids"), "111"));
        assert!(chat_ids_match(cfg.get("chat_ids"), "6967671873"));
        assert!(!chat_ids_match(cfg.get("chat_ids"), "222"));
    }

    #[test]
    fn chat_ids_match_empty_means_any_chat() {
        let empty_str = json!({ "chat_ids": "" });
        let empty_arr = json!({ "chat_ids": [] });
        let missing = json!({});
        assert!(chat_ids_match(empty_str.get("chat_ids"), "6967671873"));
        assert!(chat_ids_match(empty_arr.get("chat_ids"), "6967671873"));
        assert!(chat_ids_match(missing.get("chat_ids"), "6967671873"));
    }

    #[test]
    fn chat_ids_match_blank_target_never_matches() {
        let cfg = json!({ "chat_ids": "" });
        assert!(!chat_ids_match(cfg.get("chat_ids"), "  "));
    }

    #[test]
    fn parse_slash_command_basic() {
        let parsed = TelegramGateway::parse_slash_command("/workflows");
        assert_eq!(parsed, Some(("workflows".to_string(), "".to_string())));
    }

    #[test]
    fn parse_slash_command_with_args() {
        let parsed = TelegramGateway::parse_slash_command("/run Amazing Facts");
        assert_eq!(
            parsed,
            Some(("run".to_string(), "Amazing Facts".to_string()))
        );
    }

    #[test]
    fn parse_slash_command_with_bot_suffix() {
        let parsed = TelegramGateway::parse_slash_command("/workflows@my_bot");
        assert_eq!(parsed, Some(("workflows".to_string(), "".to_string())));
    }

    #[test]
    fn parse_slash_command_with_invisible_prefix() {
        let parsed = TelegramGateway::parse_slash_command("\u{200E}/workflows");
        assert_eq!(parsed, Some(("workflows".to_string(), "".to_string())));
    }

    #[test]
    fn fallback_parse_known_command() {
        let parsed = TelegramGateway::fallback_parse_known_command("\u{200F}/workflows test");
        assert_eq!(parsed, Some(("workflows".to_string(), "test".to_string())));
    }

    #[test]
    fn infer_callback_route_prefers_matching_message_context() {
        let configs = vec![
            json!({
                "chat_id": "6967671873",
                "text": "Agent Version",
                "inline_keyboard": {
                    "parameters": [{
                        "text": "Test",
                        "callback_data": "test",
                        "route_to_trigger": false
                    }]
                }
            }),
            json!({
                "chat_id": "6967671873",
                "text": "Trigger Version",
                "inline_keyboard": {
                    "parameters": [{
                        "text": "Test",
                        "callback_data": "test",
                        "route_to_trigger": "true"
                    }]
                }
            }),
        ];

        assert_eq!(
            infer_callback_route_from_configs(
                &configs,
                "test",
                "6967671873",
                Some("Trigger Version"),
                None,
            ),
            Some(true)
        );
    }

    #[test]
    fn infer_callback_route_returns_none_for_ambiguous_matches() {
        let configs = vec![
            json!({
                "chat_id": "6967671873",
                "text": "Same Text",
                "inline_keyboard": {
                    "parameters": [{
                        "text": "Test",
                        "callback_data": "test",
                        "route_to_trigger": false
                    }]
                }
            }),
            json!({
                "chat_id": "6967671873",
                "text": "Same Text",
                "inline_keyboard": {
                    "parameters": [{
                        "text": "Test",
                        "callback_data": "test",
                        "route_to_trigger": true
                    }]
                }
            }),
        ];

        assert_eq!(
            infer_callback_route_from_configs(
                &configs,
                "test",
                "6967671873",
                Some("Same Text"),
                None,
            ),
            None
        );
    }
}
