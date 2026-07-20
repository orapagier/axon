use crate::config::RuntimeSettings;
use crate::error_reporting::send_global_error_notification;
use crate::messaging::{MessageGateway, MessagingHub};
use crate::providers::types::Message;
use crate::router::{call_llm, drain_alerts, format_alerts, SharedRouter};
use crate::tools::ToolRegistry;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    pub id: String,
    pub service: String, // preset: gmail, outlook, facebook, gcal, mscal — or "custom"
    pub tool_name: String, // resolved MCP/registry tool to call (e.g. "gmail_list")
    pub tool_args: String, // JSON arguments string (e.g. '{"max_results": 5}')
    pub label: String,   // human label for notifications (e.g. "Gmail", "My CRM")
    pub enabled: bool,
    pub poll_mins: f64,
    pub last_check: Option<String>,
    pub last_seen_ids: Vec<String>,
    pub trigger_condition: String, // "always", "on_change"
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct NewItem {
    pub service: String,
    pub id: String,
    pub raw_id: String, // original API ID for content fetching (without composite suffixes)
    pub summary: String,
}

// ── Engine ────────────────────────────────────────────────────────────────────

pub struct WatcherEngine {
    db: Arc<Pool<SqliteConnectionManager>>,
    router: SharedRouter,
    settings: Arc<RuntimeSettings>,
    messaging: Arc<MessagingHub>,
    memory: Arc<crate::memory::MemoryStore>,
    tools: Arc<Mutex<ToolRegistry>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    state: Arc<Mutex<Option<crate::state::AppState>>>,
}

impl WatcherEngine {
    pub fn new(
        db: Arc<Pool<SqliteConnectionManager>>,
        router: SharedRouter,
        settings: Arc<RuntimeSettings>,
        messaging: Arc<MessagingHub>,
        memory: Arc<crate::memory::MemoryStore>,
        tools: ToolRegistry,
    ) -> Self {
        Self {
            db,
            router,
            settings,
            messaging,
            memory,
            tools: Arc::new(Mutex::new(tools)),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            state: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn start(self: &Arc<Self>, state: crate::state::AppState) {
        {
            let mut lock = self.state.lock().await;
            *lock = Some(state);
        }
        if self.running.swap(true, std::sync::atomic::Ordering::SeqCst) {
            tracing::info!("Watcher already running");
            return;
        }

        let engine = Arc::clone(self);
        tokio::spawn(async move {
            tracing::info!("Watcher engine started");
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            let mut _tick_count: u64 = 0;

            loop {
                interval.tick().await;
                _tick_count += 1;

                // Master switch
                let enabled = engine.settings.get_bool("watcher.enabled", false);
                if !enabled {
                    if tokio::time::Instant::now().elapsed().as_secs() % 3600 < 15 {
                        tracing::info!(
                            "Watcher engine is currently DISABLED via settings ('watcher.enabled')"
                        );
                    }
                    continue;
                }

                let notify_chat_id = engine.settings.get_str("watcher.notify_chat_id", "");
                if notify_chat_id.is_empty() {
                    if tokio::time::Instant::now().elapsed().as_secs() % 3600 < 15 {
                        tracing::warn!("Watcher engine: 'watcher.notify_chat_id' is NOT configured. Notifications will be skipped.");
                    }
                }

                // Load watchers from DB
                let watchers = match engine.load_watchers() {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!("Watcher load error: {}", e);
                        continue;
                    }
                };

                if watchers.is_empty() {
                    continue;
                }

                let now = chrono::Utc::now();
                let mut all_new_items: Vec<NewItem> = Vec::new();

                for watcher in &watchers {
                    if !watcher.enabled {
                        continue;
                    }

                    // Check if it's time to poll (poll_mins supports fractions: 0.25 = 15s)
                    let secs_since_last = watcher
                        .last_check
                        .as_ref()
                        .map(|lc| {
                            chrono::DateTime::parse_from_rfc3339(lc)
                                .map(|dt| (now - dt.with_timezone(&chrono::Utc)).num_seconds())
                                .unwrap_or(99999)
                        })
                        .unwrap_or(99999);
                    let poll_secs = (watcher.poll_mins * 60.0) as i64;

                    if secs_since_last < poll_secs {
                        continue;
                    }

                    // Check quiet hours
                    if engine.is_quiet_hours(&now) {
                        if secs_since_last >= poll_secs {
                            tracing::info!("Watcher {}: skip poll (quiet hours)", watcher.id);
                        }
                        continue;
                    }

                    tracing::debug!("Polling watcher: {} ({})", watcher.id, watcher.service);

                    let items = engine.poll_service(watcher, false).await;
                    let new_count = items.len();

                    // Update last_check regardless of whether we found items
                    let _ = engine.update_last_check(&watcher.id, &now, &items);

                    let is_first_poll =
                        watcher.last_seen_ids.is_empty() && watcher.last_check.is_none();

                    if new_count > 0 {
                        if is_first_poll {
                            tracing::info!(
                                "Watcher {}: first poll — stored {} baseline items (silent)",
                                watcher.id,
                                new_count
                            );
                        } else {
                            tracing::info!(
                                "Watcher {}: {} new items from {}",
                                watcher.id,
                                new_count,
                                watcher.service
                            );
                            all_new_items.extend(items);
                        }
                    }

                    // Log the poll
                    let _ = engine.log_poll(&watcher.id, new_count);
                }

                // Always drain router alerts accumulated during the polling phase and
                // push them immediately. This catches model errors that occurred in
                // background agent tasks (loop.rs) before triage_and_notify runs —
                // those tasks drain the alert buffer themselves, so triage would
                // otherwise find it empty and silently drop the alerts.
                let poll_alerts = drain_alerts(&engine.router).await;
                if !poll_alerts.is_empty() {
                    let alert_msg = format_alerts(&poll_alerts);
                    tracing::warn!("Watcher: model errors during polling phase: {}", alert_msg);
                    engine
                        .dispatch_router_alert_global(
                            "Watcher polling captured model/router errors",
                            &alert_msg,
                        )
                        .await;
                }

                // If we have new items, run triage
                if !all_new_items.is_empty() {
                    tracing::info!(
                        "Watcher triage: {} new items across services",
                        all_new_items.len()
                    );
                    engine.triage_and_notify(all_new_items).await;
                }
            }
        });
    }

    // ── Built-in service presets ────────────────────────────────────────────────

    /// Resolve a preset service name to (tool_name, tool_args).
    /// Returns None if the service is "custom" (user-supplied tool_name/tool_args).
    fn resolve_preset(service: &str) -> Option<(&'static str, serde_json::Value)> {
        match service {
            // Email watchers — always fetch exactly 1 new item per poll
            "gmail" => Some((
                "gmail_list",
                serde_json::json!({"max_results": 1, "query": "is:unread in:inbox"}),
            )),
            "outlook" => Some((
                "outlook_list_emails",
                serde_json::json!({"max_results": 1, "filter": "isRead eq false"}),
            )),
            // "command" is handled by poll_command_service()
            // Facebook is handled via webhook path (poll_webhook_events)
            // gcal / mscal / custom → covered by Task Watcher (command)
            "command" => None,
            _ => None,
        }
    }

    // ── Service Polling (generic) ─────────────────────────────────────────────

    pub(crate) async fn poll_service(
        &self,
        watcher: &WatcherConfig,
        ignore_seen: bool,
    ) -> Vec<NewItem> {
        // Raw Command: run a general task & treat result as an item
        if watcher.service == "command" {
            return self.poll_command_service(watcher).await;
        }

        // Facebook: use webhook events instead of API polling
        if watcher.service == "facebook" || watcher.service == "facebook_comments" {
            return self.poll_webhook_events("facebook", ignore_seen);
        }

        // Determine which tool + args to call
        let (tool, args) = if let Some((t, a)) = Self::resolve_preset(&watcher.service) {
            // Override preset args if user provided custom tool_args
            let args = if watcher.tool_args.is_empty() || watcher.tool_args == "{}" {
                a
            } else {
                serde_json::from_str(&watcher.tool_args).unwrap_or(a)
            };
            (t.to_string(), args)
        } else {
            // Custom watcher — use user-supplied tool_name and tool_args
            if watcher.tool_name.is_empty() {
                tracing::warn!(
                    "Watcher '{}' has custom service but no tool_name",
                    watcher.id
                );
                return Vec::new();
            }
            let args: serde_json::Value =
                serde_json::from_str(&watcher.tool_args).unwrap_or(serde_json::json!({}));
            (watcher.tool_name.clone(), args)
        };

        tracing::info!("Watcher '{}': calling tool '{}'", watcher.label, tool);

        let tools = self.tools.lock().await;
        let result = tools.run(&tool, args).await;
        drop(tools);

        match result {
            Ok(data) => {
                // Log raw response keys for debugging
                if let Some(obj) = data.as_object() {
                    let keys: Vec<&String> = obj.keys().collect();
                    tracing::info!("Watcher '{}' tool response keys: {:?}", watcher.label, keys);
                } else if data.is_array() {
                    tracing::info!(
                        "Watcher '{}' tool response: array with {} items",
                        watcher.label,
                        data.as_array().map(|a| a.len()).unwrap_or(0)
                    );
                }
                let mut items = self.extract_new_items(watcher, &data, ignore_seen);

                // Auto-fetch detailed content for each new item
                if !items.is_empty() {
                    self.enrich_items(&mut items, watcher).await;
                }

                tracing::info!(
                    "Watcher '{}': total {} new items (known IDs: {})",
                    watcher.label,
                    items.len(),
                    watcher.last_seen_ids.len()
                );
                items
            }
            Err(e) => {
                let err_msg = format!("Watcher poll '{}' ({}) failed: {}", watcher.label, tool, e);
                tracing::warn!("{}", err_msg);
                // Notify the user about the error so they can fix it (e.g., re-authenticate)
                self.send_notification(&format!("Axon watcher error: {} - please check if the service needs re-authentication or if the tool is available.", err_msg), Some(&watcher.id)).await;
                Vec::new()
            }
        }
    }

    // ── Webhook-backed polling (Facebook) ─────────────────────────────────────

    fn poll_webhook_events(&self, source: &str, _ignore_seen: bool) -> Vec<NewItem> {
        let conn = match self.db.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Watcher webhook poll: DB error: {}", e);
                return Vec::new();
            }
        };

        let mut items = Vec::new();
        let mut event_ids: Vec<i64> = Vec::new();

        // Scoped block so `stmt` is dropped before we use `conn` for UPDATE
        {
            let mut stmt = match conn.prepare(
                "SELECT id, event_type, from_name, from_id, object_id, parent_id, message, permalink, created_at
                 FROM webhook_events WHERE source=?1 AND read=0
                 ORDER BY created_at ASC LIMIT 20"
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Watcher webhook poll: query error: {}", e);
                    return Vec::new();
                }
            };

            let rows = stmt.query_map(rusqlite::params![source], |row| {
                Ok((
                    row.get::<_, i64>(0)?,                       // id
                    row.get::<_, String>(1)?,                    // event_type
                    row.get::<_, String>(2).unwrap_or_default(), // from_name
                    row.get::<_, String>(3).unwrap_or_default(), // from_id
                    row.get::<_, String>(4).unwrap_or_default(), // object_id
                    row.get::<_, String>(5).unwrap_or_default(), // parent_id
                    row.get::<_, String>(6).unwrap_or_default(), // message
                    row.get::<_, String>(7).unwrap_or_default(), // permalink
                    row.get::<_, String>(8)?,                    // created_at
                ))
            });

            if let Ok(rows) = rows {
                for row in rows.flatten() {
                    let (
                        id,
                        event_type,
                        from_name,
                        _from_id,
                        object_id,
                        _parent_id,
                        message,
                        _permalink,
                        _created_at,
                    ) = row;
                    event_ids.push(id);

                    let display_name = if from_name.is_empty() {
                        "Someone".to_string()
                    } else {
                        from_name
                    };

                    let summary = match event_type.as_str() {
                        "comment" => {
                            let msg_preview: String = message.chars().take(300).collect();
                            format!(
                                "Facebook comment from {} on post {}: \"{}{}\" [comment:{}]",
                                display_name,
                                object_id,
                                msg_preview,
                                if message.len() > 300 { "..." } else { "" },
                                object_id
                            )
                        }
                        "message" => {
                            let msg_preview: String = message.chars().take(300).collect();
                            format!(
                                "Facebook message from {}: \"{}{}\"",
                                display_name,
                                msg_preview,
                                if message.len() > 300 { "..." } else { "" },
                            )
                        }
                        "reaction" => {
                            format!("Facebook reaction from {} on {}", display_name, object_id)
                        }
                        _ => format!("Facebook {} from {}: {}", event_type, display_name, message),
                    };

                    items.push(NewItem {
                        service: "facebook".to_string(),
                        id: format!("wh_{}", id),
                        raw_id: object_id,
                        summary,
                    });
                }
            }
        } // stmt dropped here

        // Mark as read so they don't re-trigger
        if !event_ids.is_empty() {
            let placeholders: String = event_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE webhook_events SET read=1 WHERE id IN ({})",
                placeholders
            );
            let _ = conn.execute(&sql, rusqlite::params_from_iter(event_ids.iter()));
            tracing::info!(
                "Watcher: processed {} Facebook webhook events",
                event_ids.len()
            );
        }

        items
    }

    /// Command/Task Watcher Polling.
    ///
    /// Runs the configured natural-language task through the full agent loop,
    /// saves the result to `watcher_command_results` in the database, then
    /// compares it against the previous result:
    ///
    /// * Different result  → return as a new `NewItem` so the user gets notified.
    /// * Same result       → return a `__NO_CHANGE__` marker so `triage_and_notify`
    ///                       sends a short friendly "nothing new" message instead of
    ///                       going completely silent.
    /// * trigger_condition "always" → always report, never deduplicate.
    async fn poll_command_service(&self, watcher: &WatcherConfig) -> Vec<NewItem> {
        let command = if !watcher.tool_name.is_empty() {
            &watcher.tool_name
        } else {
            &watcher.label
        };

        if command.is_empty() {
            return Vec::new();
        }

        tracing::info!(
            "Watcher '{}': executing command task: '{}'",
            watcher.label,
            command
        );

        // Run the agent task
        let state_opt = self.state.lock().await;
        let Some(state) = state_opt.as_ref() else {
            tracing::warn!(
                "Watcher command '{}' failed: AppState not available",
                watcher.label
            );
            return Vec::new();
        };

        let context = crate::agent::RunContext::new(
            command,
            "watcher",
            Some("watcher"),
            None,
            Some(&watcher.id),
            None,
            None,
        );

        let result = crate::agent::run_task(command, state, context).await;
        drop(state_opt); // release the lock before any DB work

        match result {
            Ok(text) => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    return Vec::new();
                }

                // ── Compute hash of the new result ────────────────────────────
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut hasher = DefaultHasher::new();
                text.hash(&mut hasher);
                let new_hash = format!("{:x}", hasher.finish());

                // ── Load previous result hash from DB ─────────────────────────
                let prev_hash: Option<String> = self.db.get().ok().and_then(|conn| {
                    conn.query_row(
                        "SELECT result_hash FROM watcher_command_results \
                         WHERE watcher_id = ?1 ORDER BY id DESC LIMIT 1",
                        rusqlite::params![watcher.id],
                        |row| row.get(0),
                    )
                    .ok()
                });

                // ── Always store the new result ───────────────────────────────
                if let Ok(conn) = self.db.get() {
                    let _ = conn.execute(
                        "INSERT INTO watcher_command_results \
                         (watcher_id, watcher_label, result, result_hash) VALUES (?1, ?2, ?3, ?4)",
                        rusqlite::params![watcher.id, watcher.label, text, new_hash],
                    );
                    // Keep only last 50 results per watcher to prevent unbounded growth
                    let _ = conn.execute(
                        "DELETE FROM watcher_command_results \
                         WHERE watcher_id = ?1 \
                         AND id NOT IN (
                             SELECT id FROM watcher_command_results
                             WHERE watcher_id = ?1 ORDER BY id DESC LIMIT 50
                         )",
                        rusqlite::params![watcher.id],
                    );
                }

                let item_id = format!("cmd_{}_{}", watcher.id, new_hash);

                // ── trigger_condition = "always": always report ───────────────
                if watcher.trigger_condition == "always" {
                    tracing::info!(
                        "Watcher '{}': trigger=always, reporting result",
                        watcher.label
                    );
                    return vec![NewItem {
                        service: "command".to_string(),
                        id: format!(
                            "cmd_{}_{}",
                            watcher.id,
                            chrono::Utc::now().timestamp_millis()
                        ),
                        raw_id: watcher.id.clone(),
                        summary: text,
                    }];
                }

                // ── Default "on_change": DB dedup ─────────────────────────────
                match prev_hash {
                    Some(prev) if prev == new_hash => {
                        // Result is identical to last time → send "nothing new" marker
                        tracing::info!(
                            "Watcher '{}': result unchanged (hash {}), marking __NO_CHANGE__",
                            watcher.label,
                            new_hash
                        );
                        vec![NewItem {
                            service: "command".to_string(),
                            id: item_id,
                            raw_id: watcher.id.clone(),
                            summary: "__NO_CHANGE__".to_string(),
                        }]
                    }
                    _ => {
                        // New or changed result → report it
                        tracing::info!("Watcher '{}': result changed, reporting", watcher.label);
                        vec![NewItem {
                            service: "command".to_string(),
                            id: item_id,
                            raw_id: watcher.id.clone(),
                            summary: text,
                        }]
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Watcher command '{}' failed: {}", watcher.label, e);
                Vec::new()
            }
        }
    }

    /// Helper: coerce a JSON value (string, number, or other) into a String ID.
    fn value_to_id(v: &serde_json::Value) -> String {
        match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => v.to_string(),
        }
    }

    fn extract_new_items(
        &self,
        watcher: &WatcherConfig,
        data: &serde_json::Value,
        ignore_seen: bool,
    ) -> Vec<NewItem> {
        let mut items = Vec::new();
        let known: std::collections::HashSet<&str> =
            watcher.last_seen_ids.iter().map(|s| s.as_str()).collect();

        // Try to extract items from various response formats
        let entries = if let Some(arr) = data.as_array() {
            arr.clone()
        } else if let Some(arr) = data.get("messages").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = data.get("data").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = data.get("items").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = data.get("emails").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = data.get("events").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = data.get("conversations").and_then(|v| v.as_array()) {
            arr.clone()
        } else if let Some(arr) = data.get("value").and_then(|v| v.as_array()) {
            // Microsoft Graph API format
            arr.clone()
        } else {
            tracing::warn!(
                "Watcher '{}': could not find iterable array in response. Top-level type: {}",
                watcher.label,
                if data.is_object() {
                    "object"
                } else if data.is_array() {
                    "array"
                } else {
                    "other"
                }
            );
            return items;
        };

        tracing::info!(
            "Watcher '{}': found {} entries in response",
            watcher.label,
            entries.len()
        );

        for entry in &entries {
            // Extract ID — service-aware ID generation
            // For services like Facebook where the same conversation persists,
            // we use a composite key so new messages are detected.
            let base_id = entry
                .get("id")
                .or_else(|| entry.get("message_id"))
                .or_else(|| entry.get("event_id"))
                .map(Self::value_to_id)
                .unwrap_or_default();

            if base_id.is_empty() {
                continue;
            }

            let raw_id = base_id.clone(); // preserve for content fetching
            let id = match watcher.service.as_str() {
                // Facebook: same conversation ID, but updated_time/message_count changes
                "facebook" => {
                    let updated = entry
                        .get("updated_time")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let msg_count = entry
                        .get("message_count")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    if !updated.is_empty() {
                        format!("{}_{}", base_id, updated)
                    } else {
                        format!("{}_{}", base_id, msg_count)
                    }
                }
                // GCal: events may update; use updated timestamp
                "gcal" | "mscal" => {
                    let updated = entry
                        .get("updated")
                        .or_else(|| entry.get("lastModifiedDateTime"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !updated.is_empty() {
                        format!("{}_{}", base_id, updated)
                    } else {
                        base_id
                    }
                }
                // Gmail, Outlook: email ID is unique per message
                _ => base_id,
            };

            if !ignore_seen && known.contains(id.as_str()) {
                continue;
            }

            // Build a readable summary (used as a placeholder before enrich_items enriches it)
            let summary = match watcher.service.as_str() {
                "gmail" | "outlook" => {
                    let from = entry
                        .get("from")
                        .or_else(|| entry.get("sender"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let subject = entry
                        .get("subject")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no subject)");
                    let snippet = entry
                        .get("snippet")
                        .or_else(|| entry.get("preview"))
                        .or_else(|| entry.get("bodyPreview"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    format!("Email from {} - Subject: {} - {}", from, subject, snippet)
                }
                // Facebook (webhook path) summaries are built in poll_webhook_events
                _ => format!("{}: {}", watcher.service, entry),
            };

            items.push(NewItem {
                service: watcher.service.clone(),
                id,
                raw_id,
                summary,
            });
        }

        items
    }

    /// Extract new comment items from Facebook posts.
    /// Tracks composite ID `{post_id}_comments_{total_count}` so a new comment
    /// shows up as a change in total_count.
    /// NOTE: Kept as fallback — Facebook now uses webhook-backed polling via poll_webhook_events.
    #[allow(dead_code)]
    fn extract_fb_comment_items(
        &self,
        watcher: &WatcherConfig,
        posts_data: &serde_json::Value,
    ) -> Vec<NewItem> {
        let known: std::collections::HashSet<&str> =
            watcher.last_seen_ids.iter().map(|s| s.as_str()).collect();
        let mut items = Vec::new();

        let entries = posts_data
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for entry in &entries {
            let post_id = entry.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            if post_id.is_empty() {
                continue;
            }

            // comments.summary.total_count
            let comment_count = entry
                .get("comments")
                .and_then(|c| c.get("summary"))
                .and_then(|s| s.get("total_count"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            // Composite ID: changes when comment count changes
            let id = format!("{}_comments_{}", post_id, comment_count);

            if known.contains(id.as_str()) || comment_count == 0 {
                continue;
            }

            let post_msg = entry
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(a post)")
                .chars()
                .take(80)
                .collect::<String>();

            let summary = format!(
                "Facebook post comment — your post \"{}{}\" now has {} comments",
                post_msg,
                if post_msg.len() >= 80 { "..." } else { "" },
                comment_count
            );

            items.push(NewItem {
                service: "facebook".to_string(),
                id,
                raw_id: post_id.to_string(),
                summary,
            });
        }
        items
    }

    // ── Auto-fetch content for each new item ─────────────────────────────────

    async fn enrich_items(&self, items: &mut Vec<NewItem>, _watcher: &WatcherConfig) {
        let tools = self.tools.lock().await;

        for item in items.iter_mut() {
            match item.service.as_str() {
                "gmail" | "outlook" => {
                    let tool_name = if item.service == "gmail" {
                        "gmail_get"
                    } else {
                        "outlook_get_email"
                    };
                    let args = if item.service == "gmail" {
                        serde_json::json!({"id": item.raw_id})
                    } else {
                        serde_json::json!({"message_id": item.raw_id})
                    };

                    match tools.run(tool_name, args).await {
                        Ok(email) => {
                            // ── Sender name extraction ────────────────────────────────────
                            // Gmail: "from" is a flat string like "John Doe <john@example.com>"
                            // Outlook: "from" or "sender" is a nested object:
                            //   {"emailAddress": {"name": "John Doe", "address": "john@example.com"}}
                            let extract_sender_name = |v: &serde_json::Value| -> String {
                                // 1. Try nested emailAddress (Standard MS Graph)
                                if let Some(ea) = v.get("emailAddress") {
                                    if let Some(name) = ea
                                        .get("name")
                                        .and_then(|n| n.as_str())
                                        .filter(|s| !s.is_empty())
                                    {
                                        return name.to_string();
                                    }
                                    if let Some(addr) = ea
                                        .get("address")
                                        .and_then(|a| a.as_str())
                                        .filter(|s| !s.is_empty())
                                    {
                                        return addr.to_string();
                                    }
                                }
                                // 2. Try direct properties (Simplified/Other)
                                if let Some(name) = v
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .filter(|s| !s.is_empty())
                                {
                                    return name.to_string();
                                }
                                if let Some(addr) = v
                                    .get("address")
                                    .and_then(|a| a.as_str())
                                    .filter(|s| !s.is_empty())
                                {
                                    return addr.to_string();
                                }
                                // 3. Gmail fallback (Flat string: "Name <email>")
                                if let Some(s) = v.as_str() {
                                    if let Some(name_end) = s.find('<') {
                                        let name = s[..name_end].trim().trim_matches('"');
                                        if !name.is_empty() {
                                            return name.to_string();
                                        }
                                    }
                                    return s.to_string();
                                }
                                "Unknown".to_string()
                            };

                            let from_val = email
                                .get("from")
                                .or_else(|| email.get("sender"))
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            let from_name = extract_sender_name(&from_val);

                            let from_address = from_val
                                .get("emailAddress")
                                .and_then(|ea| ea.get("address"))
                                .and_then(|a| a.as_str())
                                .or_else(|| from_val.as_str())
                                .unwrap_or("");

                            let subject = email
                                .get("subject")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(no subject)");

                            // ── Body extraction ───────────────────────────────────────────
                            // Prefer full body, fall back to snippet/preview
                            let body = email
                                .get("body")
                                .or_else(|| email.get("text"))
                                .or_else(|| email.get("snippet"))
                                .or_else(|| email.get("bodyPreview"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let body_preview: String = body.chars().take(600).collect();

                            // ── Thread/message IDs for reply capability ───────────────────
                            let thread_id = email
                                .get("thread_id")
                                .or_else(|| email.get("threadId"))
                                .or_else(|| email.get("conversationId"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            let date_info = {
                                if let Some(idate) =
                                    email.get("internalDate").and_then(|v| v.as_str())
                                {
                                    // Gmail internalDate is ms timestamp
                                    if let Ok(ms) = idate.parse::<i64>() {
                                        let dt = chrono::DateTime::from_timestamp_millis(ms)
                                            .unwrap_or_else(|| chrono::Utc::now().into());
                                        dt.with_timezone(
                                            &chrono::FixedOffset::east_opt(8 * 3600).unwrap(),
                                        )
                                        .format("%B %e, %Y at %l:%M %p")
                                        .to_string()
                                    } else {
                                        idate.to_string()
                                    }
                                } else if let Some(rdt) = email
                                    .get("receivedDateTime")
                                    .or_else(|| email.get("date"))
                                    .and_then(|v| v.as_str())
                                {
                                    // Outlook receivedDateTime is ISO8601
                                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rdt) {
                                        dt.with_timezone(
                                            &chrono::FixedOffset::east_opt(8 * 3600).unwrap(),
                                        )
                                        .format("%B %e, %Y at %l:%M %p")
                                        .to_string()
                                    } else {
                                        rdt.to_string()
                                    }
                                } else {
                                    "Unknown".to_string()
                                }
                            };

                            // ── Attachments ──────────────────────────────────────────────
                            let has_attachments = email
                                .get("has_attachments")
                                .and_then(|v| v.as_bool())
                                .or_else(|| {
                                    email
                                        .get("attachments")
                                        .and_then(|v| v.as_array())
                                        .map(|a| !a.is_empty())
                                })
                                .unwrap_or(false);

                            // ── DB Store & Dedup ──────────────────────────────────────────────────────
                            let db = self.db.clone();
                            let is_reported = match db.get() {
                                Ok(conn) => {
                                    let mut reported = false;
                                    let stmt = conn.prepare(
                                        "SELECT reported FROM watcher_emails WHERE email_id = ?1",
                                    );
                                    if let Ok(mut stmt) = stmt {
                                        if let Ok(mut rows) = stmt.query([&item.raw_id]) {
                                            if let Ok(Some(row)) = rows.next() {
                                                reported = row.get::<_, i64>(0).unwrap_or(0) == 1;
                                                if !reported {
                                                    let _ = conn.execute("UPDATE watcher_emails SET reported = 1 WHERE email_id = ?1", [&item.raw_id]);
                                                }
                                            } else {
                                                let _ = conn.execute(
                                                    "INSERT INTO watcher_emails (service, email_id, thread_id, sender_name, sender_email, subject, body, date_received, has_attachments, reported) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
                                                    rusqlite::params![item.service, item.raw_id, thread_id, from_name, from_address, subject, body, date_info, if has_attachments { 1 } else { 0 }]
                                                );
                                            }
                                        }
                                    }
                                    reported
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to acquire DB connection for watcher emails: {}",
                                        e
                                    );
                                    false
                                }
                            };

                            if is_reported {
                                tracing::info!("Watcher: email '{}' already reported, marking as __ALREADY_REPORTED__", item.raw_id);
                                item.summary = "__ALREADY_REPORTED__".to_string();
                                continue;
                            }

                            // ── Local spam/promo pre-filter ───────────────────────────────
                            // Catch obvious automated/promo senders before sending to LLM.
                            // The LLM prompt also filters, but this saves a token for clear cases.
                            let is_likely_spam = {
                                let addr_lower = from_address.to_lowercase();
                                let subj_lower = subject.to_lowercase();
                                let body_lower = body.to_lowercase();
                                addr_lower.contains("noreply")
                                    || addr_lower.contains("no-reply")
                                    || addr_lower.contains("donotreply")
                                    || addr_lower.contains("notifications@")
                                    || addr_lower.contains("newsletter")
                                    || addr_lower.contains("marketing")
                                    || addr_lower.contains("promo")
                                    || addr_lower.contains("deals@")
                                    || addr_lower.contains("offers@")
                                    || addr_lower.contains("digest@")
                                    || addr_lower.contains("updates@")
                                    || addr_lower.contains("github.com")
                                    || addr_lower.contains("gitlab.com")
                                    || addr_lower.contains("linkedin.com")
                                    || addr_lower.contains("medium.com")
                                    || addr_lower.contains("substack.com")
                                    || addr_lower.contains("quora.com")
                                    || addr_lower.contains("jira")
                                    || addr_lower.contains("atlassian")
                                    || subj_lower.contains("unsubscribe")
                                    || subj_lower.starts_with("[github]")
                                    || subj_lower.starts_with("[jira]")
                                    || body_lower.contains("unsubscribe from this list")
                                    || body_lower.contains("you're receiving this because")
                            };

                            if is_likely_spam {
                                tracing::info!(
                                    "Watcher: pre-filtered spam/promo email from '{}': '{}'",
                                    from_name,
                                    subject
                                );
                                // Mark as seen so it doesn't re-trigger, but don't report it
                                item.summary = "__SKIP__".to_string();
                            } else {
                                // Build the structured summary that the LLM will use.
                                // Include metadata tokens so it can be acted on later.
                                item.summary = format!(
                                    "From: {from_name} | Date: {date} | Subject: {subject} | Body: {body}{ellipsis} \
                                    {attachment_note}\
                                    [email_id:{msg_id}|thread_id:{tid}|from_name:{from_name}|email_address:{from_address}]",
                                    from_name = from_name,
                                    date = date_info,
                                    subject = subject,
                                    body = body_preview,
                                    ellipsis = if body.len() > 600 { "..." } else { "" },
                                    attachment_note = if has_attachments { "(Note: This email has attachments) " } else { "" },
                                    msg_id = item.raw_id,
                                    tid = thread_id,
                                    from_address = from_address,
                                );
                                tracing::info!(
                                    "Watcher: enriched email from '{}' ({}): '{}'",
                                    from_name,
                                    date_info,
                                    subject
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to enrich email {}: {}", item.raw_id, e);
                        }
                    }
                }
                // Facebook webhook items arrive pre-summarised from poll_webhook_events.
                // Command results are fully handled inside poll_command_service.
                _ => {}
            }
        }
    }

    // ── Triage with LLM ────────────────────────────────────────────────────────

    pub(crate) async fn triage_and_notify(&self, items: Vec<NewItem>) {
        // Drop pre-filtered spam/promo items (marked __SKIP__ during enrichment)
        let items: Vec<NewItem> = items
            .into_iter()
            .filter(|i| i.summary != "__SKIP__")
            .collect();

        if items.is_empty() {
            tracing::info!("Watcher triage: all items were pre-filtered, nothing to notify");
            return;
        }

        // ── Short-circuit: every item is already-known (no real news) ──────────
        // Covers two cases:
        //   • Email watchers:   summary == "__ALREADY_REPORTED__"  (DB email dedup)
        //   • Command watchers: summary == "__NO_CHANGE__"          (DB result dedup)
        // When ALL items fall into either bucket, skip the heavy triage prompt and
        // send one short, warm, conversational "nothing new" sentence instead.
        let is_no_news =
            |i: &NewItem| i.summary == "__ALREADY_REPORTED__" || i.summary == "__NO_CHANGE__";
        let all_no_news = items.iter().all(is_no_news);
        if all_no_news {
            tracing::info!(
                "Watcher triage: all {} items already known — sending 'nothing new' message",
                items.len()
            );
            let user_name = self.settings.get_str("watcher.user_name", "Jelmar");
            let services: Vec<String> = {
                let mut seen = std::collections::HashSet::new();
                items
                    .iter()
                    .filter(|i| seen.insert(i.service.clone()))
                    .map(|i| i.service.clone())
                    .collect()
            };
            let service_label = if services.is_empty() {
                "your inbox".to_string()
            } else {
                services.join(" and ")
            };
            let now_manila =
                chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
            let now_str = now_manila.format("%A, %Y-%m-%d %H:%M:%S").to_string();
            let nothing_new_system = format!(
                "[CURRENT TIME: {now_str} (Asia/Manila)]\n\
                You are Axon, a personal AI assistant. Write ONE short, warm, conversational sentence \
                telling {name} you just checked {service} and there is nothing new since the last check. \
                End with a brief friendly note (e.g. \"All clear!\", \"You're good!\"). \
                Vary the phrasing — never repeat the same opener. Plain text only, no markdown.",
                now_str = now_str,
                name = user_name,
                service = service_label,
            );
            let nothing_new_messages =
                vec![Message::user("Generate the 'nothing new' message now.")];
            match call_llm(
                &nothing_new_messages,
                &nothing_new_system,
                &[],
                None,
                "watcher",
                Arc::clone(&self.router),
                &self.settings,
                None,
            )
            .await
            {
                Ok((response, _model, _tier)) => {
                    let alerts = drain_alerts(&self.router).await;
                    let alert_msg = format_alerts(&alerts);
                    tracing::debug!(
                        "Watcher's call_llm succeeded, drained {} alerts: {:?}",
                        alerts.len(),
                        alerts
                    );
                    if !alerts.is_empty() {
                        tracing::warn!("Watcher: model errors during triage: {:?}", alerts);
                    }
                    let txt = response.text_content().to_string();
                    let txt = txt.trim();

                    if txt.is_empty() {
                        let alerts2 = drain_alerts(&self.router).await;
                        let alert_msg2 = format_alerts(&alerts2);
                        tracing::warn!("Watcher triage LLM returned empty text, using fallback");
                        let fallback = format!(
                            "Watcher: {} new items (triage unavailable):\n{}",
                            items.len(),
                            items
                                .iter()
                                .map(|i| format!("• {}", i.summary))
                                .collect::<Vec<_>>()
                                .join("\n")
                        );
                        self.send_notification(&fallback, None).await;
                        self.dispatch_router_alert_global(
                            "Watcher triage fallback captured model/router errors",
                            &alert_msg2,
                        )
                        .await;
                        return;
                    }

                    if txt == "NOTHING_IMPORTANT" || txt.contains("NOTHING_IMPORTANT") {
                        tracing::info!("Watcher triage: nothing important, skipping notification");
                        return;
                    }

                    tracing::info!("Watcher triage result: {}", txt);
                    self.send_notification(txt, None).await;
                    self.dispatch_router_alert_global(
                        "Watcher triage captured model/router errors",
                        &alert_msg,
                    )
                    .await;
                }
                Err(e) => {
                    let alerts = drain_alerts(&self.router).await;
                    let alert_msg = format_alerts(&alerts);
                    tracing::warn!("Watcher 'nothing new' LLM call failed: {}", e);
                    self.send_notification(
                        &format!(
                            "Just checked {} — nothing new since last time. All clear!",
                            service_label
                        ),
                        None,
                    )
                    .await;
                    self.dispatch_router_alert_global(
                        "Watcher 'nothing new' path captured model/router errors",
                        &alert_msg,
                    )
                    .await;
                }
            }
            return;
        }

        let items_text = items
            .iter()
            .enumerate()
            .map(|(i, item)| match item.summary.as_str() {
                "__ALREADY_REPORTED__" => format!(
                    "{}. Checked {} (id: {}) — already reported, no new updates.",
                    i + 1,
                    item.service,
                    item.raw_id
                ),
                "__NO_CHANGE__" => format!(
                    "{}. Task watcher '{}' ran — result is the same as last time, nothing new.",
                    i + 1,
                    item.raw_id
                ),
                _ => format!("{}. {}", i + 1, item.summary),
            })
            .collect::<Vec<_>>()
            .join("\n");

        let user_name = self.settings.get_str("watcher.user_name", "Jelmar");
        let user_title = self.settings.get_str("watcher.user_title", "");

        let address_options = {
            let mut opts = vec![format!("\"Hey {}\"", user_name)];
            opts.push("\"Hey boss\"".to_string());
            if !user_title.is_empty() {
                opts.push(format!("\"Hi, {}\"", user_title));
            }
            opts.push("\"Hey\"".to_string());
            opts.push("\"Hi there\"".to_string());
            opts.join(", ")
        };

        let now_manila =
            chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
        let now_str = now_manila.format("%A, %Y-%m-%d %H:%M:%S").to_string();

        let system = format!(
            r#"[CURRENT TIME: {now_str} (Asia/Manila)]

You are Axon, a personal AI assistant notifying your boss about items that need attention.

ABOUT YOUR BOSS:
- Name: {name}
- Title: {title}
- Address them as: {options}
- Vary your greeting naturally. Don't use the same opener twice in a row.
  - Casual: "Hey {name}" or "Hey boss"
  - Important: skip the greeting, lead with the news
  - Multiple items: "Hey {name}, a few things came in"

YOUR PERSONALITY: Warm, natural, human. You speak like a real person — not a robot or a template.

SKIP ENTIRELY (say nothing about these):
- Promotional/marketing emails, newsletters, product digests, unsubscribe emails
- Automated CI/CD, GitHub, GitLab, Jira, Atlassian notifications (unless it's a failure)
- Social media likes, follows, shares
- Spam, phishing, order/login confirmations, app updates

REPORT (these matter):
- Personal emails from real people
- Client or business emails
- Direct messages from customers or church members
- Facebook comments or messages from people
- Payment, billing, or financial alerts
- Security alerts
- Calendar events coming up soon
- Command or Custom task outputs (e.g., news summaries, search results). THESE ARE EXPLICITLY REQUESTED BY THE USER AND MUST ALWAYS BE REPORTED.
- Items marked as "no new updates found since last check". (For these, just tell the user in a short friendly conversational paragraph that you checked and there's nothing new).
- Anything urgent or time-sensitive

HOW TO WRITE EACH EMAIL NOTIFICATION:
- Write it as a natural paragraph — NOT as a list of fields.
- Mention the sender by name, what the email is about, and what they're asking or saying.
- Keep each item to 2-3 sentences max.
- End with a natural offer to act: "Want me to reply?", "Should I take care of it?"
- Do NOT use labels like "From:", "Subject:", "Body:" in your response. Weave it into prose.
- If there's nothing new, just say you checked and there's nothing new, in a warm paragraph.
- Good example: "John Santos just emailed you — he's asking about the Sunday event schedule and wants to know if there's a registration link he can share. Want me to reply to him?"
- Bad example: "From: John | Subject: Sunday Event | Body: asking about registration link"

METADATA TOKENS (VERY IMPORTANT):
- Each item contains metadata like [email_id:xxx|thread_id:xxx|from_name:xxx] or [comment:xxx] or [conv:xxx]
- You MUST keep these tokens in your response, EXACTLY as given. Do NOT remove or modify them.
- Weave them naturally at the end of your sentence: "...Want me to reply? [email_id:xxx|thread_id:xxx|from_name:John Santos]"

OTHER FORMATTING RULES:
- Plain text only. No markdown, no bullet points, no asterisks, no numbered lists.
- If multiple items from the same person or thread, group them naturally in one paragraph.
- If ALL items should be skipped (and there are no "no new updates"), respond with EXACTLY: NOTHING_IMPORTANT
"#,
            name = user_name,
            title = if user_title.is_empty() {
                "Pastor"
            } else {
                &user_title
            },
            options = address_options
        );

        let messages = vec![Message::user(&format!(
            "Here are {} new notifications:\n\n{}",
            items.len(),
            items_text
        ))];

        let triage_result = call_llm(
            &messages,
            &system,
            &[],
            None,
            "watcher",
            Arc::clone(&self.router),
            &self.settings,
            None,
        )
        .await;

        match triage_result {
            Ok((response, _model, _tier)) => {
                let alerts = drain_alerts(&self.router).await;
                let alert_msg = format_alerts(&alerts);
                tracing::debug!(
                    "Watcher's call_llm succeeded, drained {} alerts: {:?}",
                    alerts.len(),
                    alerts
                );
                if !alerts.is_empty() {
                    tracing::warn!("Watcher: model errors during triage: {:?}", alerts);
                }
                let text = response.text_content().to_string();
                let txt = text.trim();

                if txt.is_empty() {
                    tracing::warn!("Watcher triage LLM returned empty text, using fallback");
                    let fallback = format!(
                        "Watcher: {} new items (triage unavailable):\n{}",
                        items.len(),
                        items
                            .iter()
                            .map(|i| format!("• {}", i.summary))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    self.send_notification(&fallback, None).await;
                    self.dispatch_router_alert_global(
                        "Watcher triage fallback captured model/router errors",
                        &alert_msg,
                    )
                    .await;
                    return;
                }

                if txt == "NOTHING_IMPORTANT" || txt.contains("NOTHING_IMPORTANT") {
                    tracing::info!("Watcher triage: nothing important, skipping notification");
                    self.dispatch_router_alert_global(
                        "Watcher triage captured model/router errors",
                        &alert_msg,
                    )
                    .await;
                    return;
                }

                tracing::info!("Watcher triage result: {}", txt);
                self.send_notification(txt, None).await;
                self.dispatch_router_alert_global(
                    "Watcher triage captured model/router errors",
                    &alert_msg,
                )
                .await;

                // Store a memory entry for each email so Axon can resolve
                // "reply to John's email" later in conversation.
                let chat_id = self.settings.get_str("watcher.notify_chat_id", "");
                if !chat_id.is_empty() {
                    for item in &items {
                        if item.service == "gmail" || item.service == "outlook" {
                            if item.summary == "__ALREADY_REPORTED__" {
                                continue;
                            }
                            // Extract from_name and email_id from the summary metadata token
                            let tag = if let (Some(s), Some(e)) =
                                (item.summary.find("[email_id:"), item.summary.rfind(']'))
                            {
                                item.summary[s..=e].to_string()
                            } else {
                                String::new()
                            };
                            if !tag.is_empty() {
                                let mem_text = format!(
                                    "Watcher email notification: {} {}",
                                    item.summary
                                        .split_once(" | Body")
                                        .map(|(h, _)| h)
                                        .unwrap_or(&item.summary),
                                    tag
                                );
                                // Tag by service and sender name for easy retrieval
                                let from_name = item
                                    .summary
                                    .find("from_name:")
                                    .and_then(|s| item.summary[s + 10..].split(']').next())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let _ = self
                                    .memory
                                    .remember(
                                        &mem_text,
                                        &item.service,
                                        &[
                                            &item.service,
                                            "watcher_email",
                                            &from_name.to_lowercase(),
                                        ],
                                    )
                                    .await;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                let alerts = drain_alerts(&self.router).await;
                let alert_msg = format_alerts(&alerts);
                tracing::warn!("Watcher triage LLM call failed: {}", e);
                let fallback = format!(
                    "Watcher: {} new items (triage unavailable):\n{}",
                    items.len(),
                    items
                        .iter()
                        .map(|i| format!("• {}", i.summary))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                self.send_notification(&fallback, None).await;
                self.dispatch_router_alert_global(
                    "Watcher triage failure captured model/router errors",
                    &alert_msg,
                )
                .await;
            }
        }
    }

    async fn dispatch_router_alert_global(&self, summary: &str, details: &str) {
        let details_trimmed = details.trim();
        if details_trimmed.is_empty() {
            return;
        }

        let state = self.state.lock().await.clone();
        if let Some(app_state) = state {
            if let Err(e) = send_global_error_notification(
                &app_state,
                "watcher.router",
                summary,
                details_trimmed,
                None,
                None,
            )
            .await
            {
                tracing::warn!("Watcher global alert notification failed: {}", e);
            }
            return;
        }

        tracing::warn!(
            "Watcher global alert notification skipped (AppState unavailable): {}",
            summary
        );
    }

    /// Mirror a watcher hit into the dashboard bell. Best-effort and independent
    /// of messaging delivery, so a hit stays visible (and reload-safe) even when
    /// no chat target is configured or the gateway is down.
    async fn notify_dashboard(&self, level: &str, title: &str, message: &str) {
        // Clone the handle out and release the lock before emitting — `emit`
        // awaits a blocking DB insert, and the state mutex is on the hot path of
        // every watcher tick (same pattern as dispatch_router_alert_global).
        let state = self.state.lock().await.clone();
        if let Some(state) = state {
            state.notify.emit("watcher", level, title, message).await;
        }
    }

    async fn send_notification(&self, text: &str, watcher_id: Option<&str>) {
        let platform = self.settings.get_str("watcher.notify_platform", "telegram");
        let chat_id = self.settings.get_str("watcher.notify_chat_id", "");
        let title = watcher_id.unwrap_or("triage").to_string();

        if chat_id.is_empty() {
            tracing::warn!("Watcher: No notify_chat_id configured, skipping notification");
            // Still surface the hit itself — dropping it entirely is the bug
            // this hub exists to fix.
            self.notify_dashboard("warning", &title, text).await;
            return;
        }

        let prefixed = format!("📬 {}", text);

        // Record to short-term memory with an isolated session per watcher
        let session = watcher_id
            .map(|id| format!("watcher:{}", id))
            .unwrap_or_else(|| "watcher:triage".to_string());
        if let Err(e) = self
            .memory
            .short
            .store_message(&session, "assistant", text, None)
        {
            tracing::warn!(
                "Watcher: Failed to store notification in short-term memory: {}",
                e
            );
        }

        match platform.as_str() {
            "telegram" => {
                let tg = self.messaging.telegram.lock().await;
                if let Some(gw) = tg.as_ref() {
                    if let Err(e) = gw.send_text(&chat_id, &prefixed).await {
                        tracing::warn!("Watcher telegram send failed: {}", e);
                    }
                } else {
                    tracing::warn!("Watcher: Telegram not connected");
                }
            }
            "discord" => {
                let dc = self.messaging.discord.lock().await;
                if let Some(gw) = dc.as_ref() {
                    if let Err(e) = gw.send_text(&chat_id, &prefixed).await {
                        tracing::warn!("Watcher discord send failed: {}", e);
                    }
                } else {
                    tracing::warn!("Watcher: Discord not connected");
                }
            }
            "slack" => {
                let sl = self.messaging.slack.lock().await;
                if let Some(gw) = sl.as_ref() {
                    if let Err(e) = gw.send_text(&chat_id, &prefixed).await {
                        tracing::warn!("Watcher slack send failed: {}", e);
                    }
                } else {
                    tracing::warn!("Watcher: Slack not connected");
                }
            }
            _ => tracing::warn!("Unknown watcher notification platform: {}", platform),
        }

        // The bell mirrors every hit regardless of how messaging delivery went,
        // so the triage text survives a page reload and is readable after the
        // moment of the toast.
        self.notify_dashboard("info", &title, text).await;
    }

    // ── Database ───────────────────────────────────────────────────────────────

    fn load_watchers(&self) -> anyhow::Result<Vec<WatcherConfig>> {
        let conn = self.db.get()?;
        let mut stmt = conn.prepare(
            "SELECT id, service, enabled, poll_mins, last_check, last_seen_ids, tool_name, tool_args, label, trigger_condition FROM watchers",
        )?;
        let rows = stmt.query_map([], |row| {
            let seen_json: String = row.get::<_, String>(5).unwrap_or_else(|_| "[]".to_string());
            let seen: Vec<String> = serde_json::from_str(&seen_json).unwrap_or_default();
            let service: String = row.get(1)?;
            let tool_name: String = row.get::<_, String>(6).unwrap_or_default();
            let tool_args: String = row.get::<_, String>(7).unwrap_or_else(|_| "{}".to_string());
            let raw_label: String = row.get::<_, String>(8).unwrap_or_default();
            let label = if raw_label.is_empty() {
                service.clone()
            } else {
                raw_label
            };
            let trigger_condition: String = row
                .get::<_, String>(9)
                .unwrap_or_else(|_| "on_change".to_string());
            Ok(WatcherConfig {
                id: row.get(0)?,
                service,
                tool_name,
                tool_args,
                label,
                enabled: row.get::<_, i32>(2)? != 0,
                poll_mins: row.get::<_, f64>(3)?,
                last_check: row.get(4)?,
                last_seen_ids: seen,
                trigger_condition,
            })
        })?;

        let mut watchers = Vec::new();
        for row in rows {
            if let Ok(w) = row {
                watchers.push(w);
            }
        }
        Ok(watchers)
    }

    pub(crate) fn update_last_check(
        &self,
        watcher_id: &str,
        now: &chrono::DateTime<chrono::Utc>,
        new_items: &[NewItem],
    ) -> anyhow::Result<()> {
        let conn = self.db.get()?;

        // Merge new IDs with existing (keep last 50 to prevent unbounded growth)
        let mut stmt = conn.prepare("SELECT last_seen_ids FROM watchers WHERE id = ?1")?;
        let existing_json: String = stmt
            .query_row(rusqlite::params![watcher_id], |r| r.get(0))
            .unwrap_or_else(|_| "[]".to_string());
        let mut existing: Vec<String> = serde_json::from_str(&existing_json).unwrap_or_default();

        // new_items usually comes newest-first. Reverse it to append oldest-first.
        for item in new_items.iter().rev() {
            if !existing.contains(&item.id) {
                existing.push(item.id.clone());
            }
        }
        // Keep only last 200 bounds to prevent unbounded db growth
        if existing.len() > 200 {
            existing = existing.split_off(existing.len() - 200);
        }

        let seen_json = serde_json::to_string(&existing)?;
        conn.execute(
            "UPDATE watchers SET last_check = ?1, last_seen_ids = ?2 WHERE id = ?3",
            rusqlite::params![now.to_string(), seen_json, watcher_id],
        )?;
        Ok(())
    }

    pub fn log_poll(&self, watcher_id: &str, new_count: usize) -> anyhow::Result<()> {
        let conn = self.db.get()?;
        conn.execute(
            "INSERT INTO watcher_log (watcher_id, new_count) VALUES (?1, ?2)",
            rusqlite::params![watcher_id, new_count as i32],
        )?;
        // Keep only last 200 log entries per watcher
        conn.execute(
            "DELETE FROM watcher_log WHERE watcher_id = ?1 AND id NOT IN (SELECT id FROM watcher_log WHERE watcher_id = ?1 ORDER BY created_at DESC LIMIT 200)",
            rusqlite::params![watcher_id],
        )?;
        Ok(())
    }

    fn is_quiet_hours(&self, now: &chrono::DateTime<chrono::Utc>) -> bool {
        let start = self.settings.get_str("watcher.quiet_hours_start", "");
        let end = self.settings.get_str("watcher.quiet_hours_end", "04:00");

        if start.is_empty() || end.is_empty() {
            return false;
        }

        // Parse HH:MM
        let parse_hm = |s: &str| -> Option<(u32, u32)> {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() == 2 {
                Some((parts[0].parse().ok()?, parts[1].parse().ok()?))
            } else {
                None
            }
        };

        let Some((sh, sm)) = parse_hm(&start) else {
            return false;
        };
        let Some((eh, em)) = parse_hm(&end) else {
            return false;
        };

        let tz_offset_hrs = self.settings.get_int("watcher.timezone_offset_hours", 8); // default +8 (PHT)
        let local_hour = ((now.hour() as i64 + tz_offset_hrs) % 24 + 24) % 24;
        let local_min = now.minute();
        let current = local_hour as u32 * 60 + local_min;
        let s = sh * 60 + sm;
        let e = eh * 60 + em;

        if s <= e {
            current >= s && current < e // e.g., 22:00 to 23:00
        } else {
            current >= s || current < e // e.g., 22:00 to 07:00 (wraps midnight)
        }
    }
}

use chrono::Timelike;
