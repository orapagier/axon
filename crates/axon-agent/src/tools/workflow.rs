use crate::error_reporting::send_global_error_notification;
use crate::state::AppState;
use boa_engine::{Context, JsString, JsValue, NativeFunction, Source};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

// Per-node executors live in submodules; the engine's `execute_node_by_type`
// dispatches to them. `nodes` is a child module so it can reach this module's
// shared helpers/statics (exposed as `pub(crate)`).
pub(crate) mod nodes;
// B2: large/binary payload offloading for persisted node_results.
pub(crate) mod binary;
// Run-scoped trigger payload + entry-node staging, keyed by RUN id so
// concurrent fires of the same workflow never swap or lose payloads.
pub(crate) mod trigger_data;
// n8n-compatible condition/operator engine (IF / Switch / Filter).
pub(crate) mod conditions;
// Boa (JS) execution + `{{ }}` expression resolution + config interpolation.
pub(crate) mod expressions;

pub(crate) use conditions::*;
pub(crate) use expressions::*;

tokio::task_local! {
    // Call stack of workflow ids currently executing as nested sub-workflows.
    // Used by the Sub-workflow node to bound recursion depth and reject cycles.
    // Unset at the top level (a normal trigger/manual run), where it reads as an
    // empty stack via `try_with(..).unwrap_or_default()`.
    pub(crate) static SUBFLOW_STACK: Vec<String>;
}

// ── Constants ─────────────────────────────────────────────────────────────────

const JS_SCRIPT_MAX_BYTES: usize = 64 * 1024;
const JS_LOG_MAX_LINES: usize = 200;
const JS_TIMEOUT: Duration = Duration::from_secs(10);

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub trigger_type: String,
    pub trigger_config: Value,
    pub last_run_at: Option<String>,
    pub last_status: String,
    pub created_at: String,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
    #[serde(default)]
    pub edges: Vec<WorkflowEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowNode {
    pub id: String,
    pub workflow_id: String,
    pub position: i64,
    #[serde(default)]
    pub position_x: f64,
    #[serde(default)]
    pub position_y: f64,
    pub node_type: String,
    pub name: String,
    pub config: Value,
    pub enabled: bool,
    #[serde(default)]
    pub continue_on_fail: bool,
    /// Retry-on-fail: number of additional attempts after the first failure
    /// (0 = no retry). Triggers, Wait, Loop and branch nodes never retry.
    #[serde(default)]
    pub retries: u32,
    /// Milliseconds to wait between retry attempts.
    #[serde(default)]
    pub retry_wait_ms: u64,
    /// "fixed" (default) or "exponential" — doubles the wait each attempt.
    #[serde(default)]
    pub retry_backoff: String,
    /// Pinned output (A4): on manual/editor runs the node is NOT executed — this
    /// saved value is routed downstream as its result so building/testing is
    /// deterministic and side-effects don't fire. `None` = not pinned. Ignored on
    /// production/trigger/scheduled runs.
    #[serde(default)]
    pub pinned_data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEdge {
    pub id: String,
    pub workflow_id: String,
    pub source_id: String,
    pub target_id: String,
    pub source_handle: Option<String>,
    pub target_handle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeResult {
    pub node_id: String,
    pub node_name: String,
    pub node_type: String,
    pub position: i64,
    pub status: String, // "success" | "error"
    pub output: Value,
    pub duration_ms: u64,
    pub error: Option<String>,
    /// A1: total times the node body was invoked (1 = succeeded first try, N>1 =
    /// retried). 0 for nodes that never executed (disabled/skipped/pinned). For an
    /// iterated (loop-body) node it's the max attempts any single unit took;
    /// per-unit counts also ride along in each `errors[]` entry.
    #[serde(default)]
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunResult {
    pub run_id: String,
    pub workflow_id: String,
    pub status: String,
    pub node_results: Vec<NodeResult>,
    pub final_output: Value,
    pub total_duration_ms: u64,
}

// ── Node executors ────────────────────────────────────────────────────────────

// ── Telegram reply-route registry ─────────────────────────────────────────────

/// Record that `workflow_id` sent a Telegram message, keyed by (chat_id,
/// message_id), so a later "reply to" that message can be routed back into this
/// workflow's telegram trigger. Called after any `telegram` node succeeds; the
/// node output is the Telegram `Message` result containing `message_id` + `chat`.
/// No-ops for operations that return no message (e.g. deleteMessage).
fn record_telegram_reply_route(state: &AppState, workflow_id: &str, output: &Value) {
    // A telegram node's output is usually a single Message, but sendMediaGroup
    // returns an *array* of Messages. Record every message so a reply to any of
    // them routes back to this workflow (previously an array output matched
    // neither `message_id` nor `/chat/id` and was silently dropped).
    let messages: Vec<&Value> = match output {
        Value::Array(arr) => arr.iter().collect(),
        other => vec![other],
    };

    let Ok(conn) = state.db.get() else {
        return;
    };

    let mut recorded = 0u32;
    for msg in messages {
        let (Some(message_id), Some(chat_id)) = (
            msg.get("message_id").and_then(|v| v.as_i64()),
            msg.pointer("/chat/id").and_then(|v| v.as_i64()),
        ) else {
            continue;
        };
        match conn.execute(
            "INSERT INTO telegram_reply_routes (chat_id, message_id, workflow_id, created_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(chat_id, message_id)
             DO UPDATE SET workflow_id = excluded.workflow_id, created_at = excluded.created_at",
            rusqlite::params![chat_id.to_string(), message_id, workflow_id],
        ) {
            Ok(_) => {
                recorded += 1;
                tracing::info!(
                    "[TELEGRAM] Recorded reply route chat_id={} message_id={} -> workflow={}",
                    chat_id,
                    message_id,
                    workflow_id
                );
            }
            Err(e) => tracing::warn!("Failed to record telegram reply route: {}", e),
        }
    }

    if recorded > 0 {
        // Opportunistic TTL prune so the table can't grow unbounded.
        let _ = conn.execute(
            "DELETE FROM telegram_reply_routes WHERE created_at < datetime('now','-30 days')",
            [],
        );
    }
}

// ── Gmail Trigger Executor ────────────────────────────────────────────────────

/// Map a Gmail Stimulus "Label" selection to a proper Gmail search query.
///
/// Mirrors n8n's Gmail trigger: filter by folder/label WITHOUT forcing
/// `is:unread`, so already-read mail is still listed. Whether a message counts
/// as "new" is decided by ID de-duplication (background poll) or a plain listing
/// (manual Execute Step) — never by its read state. System folders use the
/// canonical `in:`/`is:` operators; anything else falls back to `label:`.
fn gmail_query_for_label(label: &str) -> String {
    match label.trim().to_ascii_uppercase().as_str() {
        "" | "INBOX" => "in:inbox".to_string(),
        "UNREAD" => "is:unread".to_string(),
        "STARRED" => "is:starred".to_string(),
        "IMPORTANT" => "is:important".to_string(),
        "SENT" => "in:sent".to_string(),
        "SPAM" => "in:spam".to_string(),
        "TRASH" => "in:trash".to_string(),
        other => format!("label:{}", other.to_lowercase().replace(' ', "-")),
    }
}

/// Build the full Gmail search query for a trigger: the label clause plus any
/// optional subject/body text filters. Empty filters are skipped, so the trigger
/// fires on every new email in the label (no narrowing) — same as before these
/// fields existed. Filters are literal plain text (a trigger's config can't take
/// `{{ }}` expressions): the subject filter scopes to the subject line, while the
/// body filter is a bare term and therefore matches anywhere in the message
/// (subject or body). Multi-word input is grouped in parens, so each word must
/// match (Gmail AND semantics) rather than only the first.
fn gmail_trigger_query(config: &Value) -> String {
    let label = config
        .get("gmail_label")
        .and_then(|v| v.as_str())
        .unwrap_or("INBOX");
    let mut query = gmail_query_for_label(label);

    if let Some(subject) = config
        .get("gmail_subject_query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        query.push_str(&format!(" subject:({})", subject));
    }

    if let Some(body) = config
        .get("gmail_body_query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        query.push_str(&format!(" ({})", body));
    }

    query
}

pub(crate) async fn execute_gmail_trigger(
    config: &Value,
    state: &AppState,
    workflow_id: &str,
    run_id: &str,
) -> Result<Value, String> {
    // Check for pre-fetched data from the background Gmail poller, staged for
    // THIS run (only new, not-previously-seen emails, set by check_and_trigger_gmail()).
    if let Some(trigger_data) = trigger_data::take(run_id) {
        tracing::info!(
            "Gmail trigger: using pre-fetched new email data for workflow {}",
            workflow_id
        );
        return Ok(trigger_data);
    }

    // Fallback: manual "Execute Step" click — do a live fetch of all unread emails
    let label = config
        .get("gmail_label")
        .and_then(|v| v.as_str())
        .unwrap_or("INBOX");
    let max_results = config
        .get("gmail_max_results")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(10);

    let query = gmail_trigger_query(config);
    let args = json!({
        "query": query,
        "max_results": max_results,
    });

    // Use ToolRegistry::run() — the same proven path the watcher engine uses.
    // It automatically handles MCP server resolution via the registered tool source.
    match state.tools.run("gmail_list", args).await {
        Ok(data) => {
            tracing::info!(
                "Gmail trigger (manual): fetched emails from label={} (q='{}'), max={}",
                label,
                query,
                max_results
            );

            // Flatten to a top-level array so manual "Execute Step" emits the
            // EXACT same shape as the background poller (check_and_trigger_gmail).
            // gmail_list returns { messages: [...] }; older paths returned a bare
            // array — accept both so downstream `data.emails[0]` always works.
            let emails: Vec<Value> = data
                .as_array()
                .or_else(|| data.get("messages").and_then(|v| v.as_array()))
                .or_else(|| data.get("emails").and_then(|v| v.as_array()))
                .cloned()
                .unwrap_or_default();

            // Enrich each row with full decoded + decomposed body, parsed sender
            // and richer attachments. The "Download Attachments" toggle also saves
            // every file locally and attaches the paths. Same shape as the poller.
            let download = config
                .get("gmail_download_attachments")
                .and_then(|v| {
                    v.as_bool()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(false);
            let emails = enrich_gmail_emails(state, emails, download).await;

            // A manual "Execute Step" is normally a non-destructive test fetch
            // (like n8n's "Fetch Test Event"). But if the user explicitly enabled
            // "Mark as read", honor it here too — otherwise the toggle silently
            // does nothing when testing the node, which is what it looks like to
            // the user. The label query (e.g. `in:inbox`) lists read mail as well,
            // so the same emails still appear on a re-run.
            let mark_read = config
                .get("gmail_mark_read")
                .and_then(|v| {
                    v.as_bool()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
                .unwrap_or(false);
            if mark_read {
                let ids: Vec<String> = emails
                    .iter()
                    .filter_map(|e| {
                        e.get("id")
                            .or_else(|| e.get("message_id"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                if !ids.is_empty() {
                    if let Err(e) = state
                        .tools
                        .run("gmail_mark_read", json!({ "ids": ids }))
                        .await
                    {
                        tracing::warn!("Gmail trigger (manual): mark-as-read failed: {}", e);
                    }
                }
            }

            Ok(json!({
                "trigger": "gmail",
                "label": label,
                "new_email_count": emails.len(),
                "emails": emails,
            }))
        }
        Err(e) => Err(format!("Gmail trigger failed: {}", e)),
    }
}

/// Turn lightweight `gmail_list` rows into the rich per-message objects the smart
/// Gmail node promises: a full decoded + decomposed body (main text, signature,
/// quoted reply thread), parsed sender, links/contacts and richer attachment
/// metadata — via `gmail_get`. When `download` is set, also persist every
/// attachment to local storage and attach the resulting `files` paths.
/// Best-effort: an email whose detail fetch fails is passed through unchanged so
/// the workflow still runs.
async fn enrich_gmail_emails(state: &AppState, emails: Vec<Value>, download: bool) -> Vec<Value> {
    let mut enriched = Vec::with_capacity(emails.len());
    for email in emails {
        let id = email
            .get("id")
            .or_else(|| email.get("message_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let Some(id) = id else {
            enriched.push(email);
            continue;
        };
        let mut full = match state.tools.run("gmail_get", json!({ "id": id })).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Gmail enrich: gmail_get failed for {}: {}", id, e);
                enriched.push(email);
                continue;
            }
        };
        if download {
            match state
                .tools
                .run(
                    "gmail_download_all_attachments",
                    json!({ "message_id": id }),
                )
                .await
            {
                Ok(res) => {
                    if let (Some(obj), Some(files)) =
                        (full.as_object_mut(), res.get("files").cloned())
                    {
                        obj.insert("files".to_string(), files);
                    }
                }
                Err(e) => tracing::warn!("Gmail enrich: download failed for {}: {}", id, e),
            }
        }
        enriched.push(full);
    }
    enriched
}

/// Parse a positive integer from a config field that may arrive as a JSON
/// number or a string (the UI emits both depending on the widget).
pub(crate) fn cfg_usize(config: &Value, key: &str) -> Option<usize> {
    config.get(key).and_then(|v| {
        v.as_u64()
            .map(|n| n as usize)
            .or_else(|| v.as_f64().map(|f| f as usize))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<usize>().ok()))
    })
}

pub(crate) fn extract_items_for_loop(
    raw_items: &Value,
    array_path: Option<&str>,
) -> Result<Vec<Value>, String> {
    if let Some(arr) = raw_items.as_array() {
        return Ok(arr.clone());
    }

    if let Some(obj) = raw_items.as_object() {
        if let Some(path) = array_path {
            if !path.trim().is_empty() {
                if let Some(v) = Value::Object(obj.clone()).pointer(&parse_path_pointer(path)) {
                    if let Some(arr) = v.as_array() {
                        return Ok(arr.clone());
                    }
                }
            }
        }

        if let Some(v) = obj.get("items").and_then(|v| v.as_array()) {
            return Ok(v.clone());
        }
        if let Some(v) = obj.get("files").and_then(|v| v.as_array()) {
            return Ok(v.clone());
        }
        if let Some(v) = obj.get("data").and_then(|v| v.as_array()) {
            return Ok(v.clone());
        }
        if let Some(v) = obj.values().find_map(|v| v.as_array()) {
            return Ok(v.clone());
        }
    }

    if raw_items.is_null() {
        return Ok(Vec::new());
    }

    Err("Loop node expects an array (or an object containing an array)".to_string())
}

fn find_iteration_source_node_id(
    current_node_id: &str,
    edges: &[WorkflowEdge],
    node_results: &std::collections::HashMap<String, NodeResult>,
) -> Option<String> {
    for edge in edges.iter().filter(|e| e.target_id == current_node_id) {
        let source_id = &edge.source_id;
        let has_loop_marker = node_results
            .get(source_id)
            .and_then(|r| r.output.get("_axon_loop"))
            .and_then(|v| v.as_object())
            .is_some();
        let has_items = node_results
            .get(source_id)
            .and_then(|r| r.output.get("items"))
            .and_then(|v| v.as_array())
            .is_some();
        if has_loop_marker && has_items {
            return Some(source_id.clone());
        }
    }
    None
}

async fn execute_node_dispatch(
    node: &WorkflowNode,
    config: &Value,
    state: &AppState,
    trigger_source: &str,
    workflow_id: &str,
    run_id: &str,
    node_results: &std::collections::HashMap<String, NodeResult>,
    // Whether a Wait node here may durably suspend the whole run (vs sleeping
    // in-process). False inside Loop iterations and for test/partial runs.
    durable_allowed: bool,
) -> Result<Value, String> {
    match node.node_type.as_str() {
        "trigger" | "circadian" | "stimulus" => {
            nodes::trigger::execute(config, state, trigger_source, workflow_id, run_id).await
        }
        "synapse" => nodes::synapse::execute_http_node(config).await,
        "myelin" => crate::tools::myelin::execute_myelin_node(state, config).await,
        "telegram" => crate::tools::telegram::execute_telegram_node(config).await,
        "whatsapp" => crate::tools::whatsapp::execute_whatsapp_node(config).await,
        "discord" => nodes::discord::execute(config).await,
        "slack" => nodes::slack::execute(config).await,
        "github" => nodes::github::execute(config).await,
        "database" => nodes::database::execute(config).await,
        "facebook" => nodes::facebook::execute(config).await,
        "shell" => nodes::shell::execute(config).await,
        "javascript" => {
            // Sort by position for deterministic $results[N] ordering.
            // HashMap iteration order is random, which caused the JS node
            // to intermittently error when scripts accessed $results by index.
            let mut vec: Vec<_> = node_results.values().cloned().collect();
            vec.sort_by_key(|r| r.position);

            // Use the RAW script from node.config (not the interpolated config)
            // because interpolate_config mangles {{ }} expressions by converting
            // complex objects to strings. The JS engine handles {{ }} natively
            // by stripping wrappers and using $node as a real JS variable.
            let raw_script = node
                .config
                .get("script")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            execute_js_node(raw_script, node, &vec, workflow_id, run_id).await
        }
        "cortex" => nodes::cortex::execute_cortex_node(config, state, workflow_id, &node.id).await,
        "classifier" => nodes::classifier::execute(config, state, workflow_id, &node.id).await,
        "fovea" => nodes::fovea::execute(config, state).await,
        t if t == "mcp" || t.starts_with("mcp_") => nodes::mcp::execute(config, state).await,
        "wait" => nodes::wait::execute(config, state, workflow_id, run_id, durable_allowed).await,
        "approval" => {
            // C1: Approval is a Wait preset that suspends for a human decision and
            // routes Approve→output 0 / Reject→output 1. Force approval mode so it
            // behaves correctly regardless of saved config.
            let mut cfg = config.clone();
            if let Some(o) = cfg.as_object_mut() {
                o.insert("mode".to_string(), json!("approval"));
            }
            nodes::wait::execute(&cfg, state, workflow_id, run_id, durable_allowed).await
        }
        "soma" => {
            // Soma's "Include Other Input Fields" merges over the incoming item.
            // Use the same primary-input convention as $json: the most recent
            // predecessor by position.
            let mut vec: Vec<_> = node_results.values().cloned().collect();
            vec.sort_by_key(|r| r.position);
            let input = vec.last().map(|r| r.output.clone()).unwrap_or(Value::Null);
            nodes::soma::execute(config, &input)
        }
        "engram" => nodes::engram::execute(config, state).await,
        "homeostasis" => nodes::homeostasis::execute(config, state).await,
        "ifCondition" => nodes::condition::execute_if_condition_node(config),
        "switch" => nodes::condition::execute_switch_node(config),
        "loop" => nodes::iterate::execute(config),
        "subflow" | "workflow" => {
            nodes::subflow::execute(config, state, workflow_id, run_id, node_results).await
        }
        _ => Err(format!("Unknown type: {}", node.node_type)),
    }
}

/// Wait before retry attempt `attempt` (1-based). Floors the base at 1ms so a
/// misconfigured 0ms still yields a real (cancellation-checking) pause, and
/// doubles per attempt under exponential backoff. Saturating throughout so a
/// large attempt count can't overflow.
fn compute_retry_wait_ms(base_ms: u64, attempt: u32, backoff: &str) -> u64 {
    let base = base_ms.max(1);
    if backoff == "exponential" {
        base.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)))
    } else {
        base
    }
}

/// Per-node entry point: wraps `execute_node_dispatch` with retry-on-fail. On
/// failure the node is re-executed up to `node.retries` times, waiting
/// `retry_wait_ms` between attempts (doubling each time when
/// `retry_backoff == "exponential"`). The wait is cancellation-aware so a Stop
/// request takes effect promptly. Trigger, Wait, Loop and branch nodes never
/// retry — re-running them has no transient-failure semantics (they suspend,
/// fan out, or route) and could double side effects. Every engine call site
/// goes through here, so retry applies uniformly to single nodes and loop units.
#[allow(clippy::too_many_arguments)]
async fn execute_node_by_type(
    node: &WorkflowNode,
    config: &Value,
    state: &AppState,
    trigger_source: &str,
    workflow_id: &str,
    run_id: &str,
    node_results: &std::collections::HashMap<String, NodeResult>,
    durable_allowed: bool,
) -> (Result<Value, String>, u32) {
    let no_retry = matches!(
        node.node_type.as_str(),
        "trigger"
            | "circadian"
            | "stimulus"
            | "wait"
            | "approval"
            | "loop"
            | "ifCondition"
            | "switch"
    );
    let max_attempts = if no_retry { 0 } else { node.retries };

    // `attempt` is the 0-based retry index; `attempts_made` counts every dispatch
    // invocation (the value reported to the UI via NodeResult.attempts).
    let mut attempt: u32 = 0;
    let mut attempts_made: u32 = 0;
    loop {
        attempts_made += 1;
        match execute_node_dispatch(
            node,
            config,
            state,
            trigger_source,
            workflow_id,
            run_id,
            node_results,
            durable_allowed,
        )
        .await
        {
            Ok(v) => return (Ok(v), attempts_made),
            Err(e) => {
                if attempt >= max_attempts {
                    return (Err(e), attempts_made);
                }
                attempt += 1;
                crate::observability::record_node_retry(&node.node_type);
                let wait_ms =
                    compute_retry_wait_ms(node.retry_wait_ms, attempt, &node.retry_backoff);
                tracing::warn!(
                    "Node '{}' ({}) attempt {}/{} failed: {} — retrying in {}ms",
                    node.name,
                    node.id,
                    attempt,
                    max_attempts,
                    e,
                    wait_ms
                );
                // Cancellation-aware sleep in <=1s slices (mirrors wait.rs).
                let deadline =
                    tokio::time::Instant::now() + tokio::time::Duration::from_millis(wait_ms);
                loop {
                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        break;
                    }
                    let slice = (deadline - now).min(tokio::time::Duration::from_secs(1));
                    tokio::time::sleep(slice).await;
                    let cancelled = {
                        let c = state.workflow_cancellations.lock().await;
                        c.contains(workflow_id) || c.contains(run_id)
                    };
                    if cancelled {
                        return (
                            Err("Workflow cancelled during retry backoff".to_string()),
                            attempts_made,
                        );
                    }
                }
            }
        }
    }
}

// ── Workflow Engine Impl ──────────────────────────────────────────────────────

/// Clears a run's cancellation flags from the shared set when a workflow run
/// finishes by ANY path — success, error propagated via `?`, or the early
/// "cancelled" return. Without this, a Stop request leaves an entry in the set
/// forever; with workflow_id-keyed cancellation that silently cancels every
/// subsequent run, poisoning the workflow until the process restarts.
struct CancellationCleanup {
    set: std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    keys: Vec<String>,
}
impl Drop for CancellationCleanup {
    fn drop(&mut self) {
        let set = self.set.clone();
        let keys = std::mem::take(&mut self.keys);
        // The set is behind an async mutex; remove on a detached task. Each
        // run_id is unique, so a slightly-delayed removal can never affect a
        // different run.
        tokio::spawn(async move {
            let mut guard = set.lock().await;
            for k in keys {
                guard.remove(&k);
            }
        });
    }
}

/// Persist a run-state UPDATE whose failure must not abort the run but must
/// not vanish either: disk-full/locked errors here previously disappeared into
/// `let _ =`, leaving the UI showing stale results with no operator-visible
/// trace. Logs a warning carrying the run id instead.
fn persist_run_update<P: rusqlite::Params>(
    conn: &rusqlite::Connection,
    sql: &str,
    params: P,
    run_id: &str,
    what: &str,
) {
    if let Err(e) = conn.execute(sql, params) {
        tracing::warn!("Run {run_id}: failed to persist {what}: {e}");
    }
}

/// Merge a single-node run's fresh results onto the previous run's full chain,
/// replacing matching nodes in place and preserving order. Lets an "Execute
/// Step" save keep every upstream node's data instead of just the one it ran.
fn merge_single_node_results(prior: &[NodeResult], fresh: &[NodeResult]) -> Vec<NodeResult> {
    let mut merged: Vec<NodeResult> = prior.to_vec();
    for nr in fresh {
        if let Some(slot) = merged.iter_mut().find(|p| p.node_id == nr.node_id) {
            *slot = nr.clone();
        } else {
            merged.push(nr.clone());
        }
    }
    merged
}

/// Re-stamp each loaded result's `node_name`/`node_type` from the CURRENT graph,
/// keyed by the stable `node_id`.
///
/// Cached upstream results (loaded for targeted / "Execute Step" runs, and reused
/// verbatim by `reuse_cached_upstream`) carry the name the node had when that
/// prior run executed. Because `$node["Name"]` references resolve by `node_name`,
/// a node RENAMED since that run would no longer be found by its current name —
/// the reference silently resolves to null (e.g. a renamed "Axon 2" caption comes
/// through empty) even though the editor's preview still shows a value (the
/// preview keys by current label → id → result, and `node_id` is rename-stable).
///
/// Re-stamping from the live `nodes` by id makes backend name resolution agree
/// with the current graph and the preview. It also keeps the JS node's `$node`
/// map (which is built from `node_name`) consistent for reused upstream nodes.
fn restamp_result_identities(
    results: &mut std::collections::HashMap<String, NodeResult>,
    prior_ordered: &mut [NodeResult],
    ordered_results: &mut [NodeResult],
    nodes: &[WorkflowNode],
) {
    let id_to_node: std::collections::HashMap<&str, &WorkflowNode> =
        nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for r in results
        .values_mut()
        .chain(prior_ordered.iter_mut())
        .chain(ordered_results.iter_mut())
    {
        if let Some(n) = id_to_node.get(r.node_id.as_str()) {
            r.node_name = n.name.clone();
            r.node_type = n.node_type.clone();
        }
    }
}

/// Fold one prior run's results into the upstream cache being assembled for a
/// targeted ("Execute Step") run / expression fallback.
///
/// The newest run (`is_newest`) is mirrored verbatim so the snapshot matches the
/// last run exactly. Older runs only BACKFILL nodes the newer runs dropped, and
/// only with that node's most recent *successful* result for a node that still
/// exists in the graph. This is what keeps a one-shot Telegram/Gmail/WhatsApp
/// Stimulus payload alive when the immediately-previous run was a partial run on
/// an unrelated node (which persists a node_results array without the trigger):
/// without backfill the trigger would be absent from the cache, fail the
/// single_node_ready and reuse_cached_upstream gates, and re-run under
/// trigger_source="manual", overwriting its real payload with {"trigger":"manual"}.
///
/// Returns true once every current node has a cached result, so the caller can
/// stop reading older runs (keeping the healthy path at a single parse).
fn fold_prior_run_into_cache(
    node_results: &mut std::collections::HashMap<String, NodeResult>,
    prior_ordered: &mut Vec<NodeResult>,
    run_results: Vec<NodeResult>,
    is_newest: bool,
    current_node_ids: &std::collections::HashSet<&str>,
) -> bool {
    if is_newest {
        for r in &run_results {
            node_results.insert(r.node_id.clone(), r.clone());
        }
        *prior_ordered = run_results;
    } else {
        for r in run_results {
            if r.status == "success"
                && !node_results.contains_key(&r.node_id)
                && current_node_ids.contains(r.node_id.as_str())
            {
                node_results.insert(r.node_id.clone(), r.clone());
                prior_ordered.push(r);
            }
        }
    }
    !current_node_ids.is_empty()
        && current_node_ids
            .iter()
            .all(|id| node_results.contains_key(*id))
}

/// State handed to the engine when resuming a run that a durable Wait suspended.
/// `results` are the nodes that already ran in this run (including the Wait);
/// `completed` is their id set, used to replay-not-re-execute them on resume.
struct ResumeState {
    completed: std::collections::HashSet<String>,
    results: Vec<NodeResult>,
}

/// B3: RAII guard holding a run's concurrency permit. Dropping it releases the
/// semaphore permit (so the next queued run can start) and decrements the
/// active-runs gauge. A durably-suspended run drops this when its task returns,
/// freeing the slot while it waits; the resume path acquires a fresh one.
struct RunSlot {
    _permit: tokio::sync::OwnedSemaphorePermit,
    active: std::sync::Arc<std::sync::atomic::AtomicI64>,
}
impl Drop for RunSlot {
    fn drop(&mut self) {
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// B3: acquire a concurrency slot for a background run. Returns `None` when the
/// wait queue is already at `workflow.max_queue_depth` (caller sheds the run) or
/// the semaphore is closed. Otherwise awaits a permit, counting the wait in the
/// queue-depth gauge.
async fn acquire_run_slot(state: &AppState) -> Option<RunSlot> {
    use std::sync::atomic::Ordering;
    let cap = state.settings.workflow_max_queue_depth();
    let depth = state.run_queue_depth.fetch_add(1, Ordering::SeqCst) + 1;
    if cap > 0 && depth > cap {
        state.run_queue_depth.fetch_sub(1, Ordering::SeqCst);
        return None;
    }
    let permit = state.run_semaphore.clone().acquire_owned().await.ok();
    state.run_queue_depth.fetch_sub(1, Ordering::SeqCst);
    let permit = permit?;
    state.active_runs.fetch_add(1, Ordering::SeqCst);
    Some(RunSlot {
        _permit: permit,
        active: state.active_runs.clone(),
    })
}

pub struct WorkflowEngine;
impl WorkflowEngine {
    pub async fn run(
        workflow_id: &str,
        state: &AppState,
        target_node_id: Option<String>,
        run_id: Option<String>,
    ) -> anyhow::Result<WorkflowRunResult> {
        Self::run_with_trigger(workflow_id, state, "manual", target_node_id, false, run_id).await
    }

    pub async fn run_with_trigger(
        workflow_id: &str,
        state: &AppState,
        trigger_source: &str,
        target_node_id: Option<String>,
        single_node: bool,
        external_run_id: Option<String>,
    ) -> anyhow::Result<WorkflowRunResult> {
        Self::run_inner(
            workflow_id,
            state,
            trigger_source,
            target_node_id,
            single_node,
            external_run_id,
            None,
        )
        .await
    }

    /// Core engine. `resume` is `Some` only when re-entering a run that a durable
    /// Wait node suspended: it carries the nodes already completed in that run so
    /// they are replayed (edges released) but not re-executed, letting the BFS
    /// continue from the node after the Wait.
    #[allow(clippy::too_many_arguments)]
    async fn run_inner(
        workflow_id: &str,
        state: &AppState,
        trigger_source: &str,
        target_node_id: Option<String>,
        single_node: bool,
        external_run_id: Option<String>,
        resume: Option<ResumeState>,
    ) -> anyhow::Result<WorkflowRunResult> {
        tracing::info!(
            "Starting workflow run for {} (source: {}, resume: {})",
            workflow_id,
            trigger_source,
            resume.is_some()
        );
        let start = std::time::Instant::now();
        let run_id = external_run_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Clear this run's cancellation flags on every exit path so a Stop press
        // never lingers to cancel a future run. Bound to `_cancel_cleanup` (not
        // `_`) so it lives for the whole function.
        let _cancel_cleanup = CancellationCleanup {
            set: state.workflow_cancellations.clone(),
            keys: vec![run_id.clone(), workflow_id.to_string()],
        };

        // Discard any staged trigger payload / entry pin this run never consumed
        // (early load failure, error before the trigger node, cancelled child) so
        // the staging maps can't leak entries. Keys are unique run ids, so this
        // can never touch another run's data.
        let _staged_cleanup = trigger_data::StagedCleanup::new(&run_id);

        let (workflow_name, nodes, edges) = {
            let conn = state.db.get()?;
            let name: String = conn.query_row(
                "SELECT name FROM workflows WHERE id = ?1",
                [workflow_id],
                |r| r.get(0),
            )?;

            let mut s = conn.prepare("SELECT id, workflow_id, position, position_x, position_y, node_type, name, config, enabled, continue_on_fail, retries, retry_wait_ms, retry_backoff, pinned_data FROM workflow_nodes WHERE workflow_id = ?")?;
            let nodes: Vec<WorkflowNode> = s
                .query_map([workflow_id], |r| {
                    Ok(WorkflowNode {
                        id: r.get(0)?,
                        workflow_id: r.get(1)?,
                        position: r.get(2)?,
                        position_x: r.get(3)?,
                        position_y: r.get(4)?,
                        node_type: r.get(5)?,
                        name: r.get(6)?,
                        config: serde_json::from_str::<Value>(&r.get::<_, String>(7)?)
                            .unwrap_or(json!({})),
                        enabled: r.get::<_, i32>(8)? != 0,
                        continue_on_fail: r.get::<_, i32>(9)? != 0,
                        retries: r.get::<_, i64>(10).unwrap_or(0).max(0) as u32,
                        retry_wait_ms: r.get::<_, i64>(11).unwrap_or(0).max(0) as u64,
                        retry_backoff: r.get::<_, Option<String>>(12)?.unwrap_or_default(),
                        // NULL/blank/unparseable ⇒ not pinned.
                        pinned_data: r
                            .get::<_, Option<String>>(13)?
                            .filter(|s| !s.trim().is_empty())
                            .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            let edges = conn.prepare("SELECT id, workflow_id, source_id, target_id, source_handle, target_handle FROM workflow_edges WHERE workflow_id = ?")?
                .query_map([workflow_id], |r| Ok(WorkflowEdge {
                    id: r.get(0)?, workflow_id: r.get(1)?, source_id: r.get(2)?, target_id: r.get(3)?,
                    source_handle: r.get(4).ok(), target_handle: r.get(5).ok(),
                }))?.filter_map(|r| r.ok()).collect::<Vec<_>>();

            // INSERT OR IGNORE: if run_in_background already pre-created this record, skip the insert.
            conn.execute("INSERT OR IGNORE INTO workflow_runs (id, workflow_id, status, started_at, node_results) VALUES (?1, ?2, 'running', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), '[]')", [run_id.clone(), workflow_id.to_string()])?;
            (name, nodes, edges)
        };

        let mut node_results = std::collections::HashMap::new();
        // Ordered copy of the previous run's results. Kept so a single-node run
        // can persist the whole chain (prior nodes + the re-executed one) instead
        // of collapsing the saved run down to just the one node it actually ran.
        let mut prior_ordered: Vec<NodeResult> = Vec::new();
        let mut ordered_results = Vec::new();
        // B2: byte cap above which node-output strings are offloaded to the blob
        // store before each DB persist (in-memory results stay full).
        let bin_threshold = state.settings.workflow_binary_inline_max_bytes();

        // Nodes already completed *in this run* before a durable Wait suspended
        // it. On resume these are replayed for edge-routing only (never re-run),
        // so triggers don't re-fire and the BFS flows straight to the Wait's
        // downstream nodes. Empty for a normal (non-resumed) run.
        let resumed_completed: std::collections::HashSet<String> = match resume {
            Some(rs) => {
                for r in &rs.results {
                    node_results.insert(r.node_id.clone(), r.clone());
                }
                // Seed the persisted chain so polling/the final save keep every
                // pre-suspend node, not just the ones run after resume.
                ordered_results = rs.results;
                // Flip status back to 'running' and clear the wake fields, so a
                // concurrent poller tick can't claim this run twice.
                if let Ok(conn) = state.db.get() {
                    persist_run_update(
                        &conn,
                        "UPDATE workflow_runs SET status = 'running', resume_at = NULL, resume_node_id = NULL WHERE id = ?1",
                        [run_id.clone()],
                        &run_id,
                        "resume status flip",
                    );
                }
                rs.completed
            }
            None => {
                // Load cached results from prior runs as the upstream snapshot for
                // targeted ("Execute Step") runs and as an expression fallback for
                // skipped/unconnected nodes.
                //
                // We deliberately do NOT trust a single "latest run". A targeted run
                // on a node that is NOT a descendant of the trigger — a different
                // branch, a disconnected/since-deleted node, or one that errored
                // before its branch reached the trigger — persists a node_results
                // array containing only that node, with the trigger ABSENT (verified
                // in production: partial runs save e.g. just `[mcp_gmail]`). If that
                // partial run were the sole cache source, the next Execute Step on a
                // trigger-descendant would find no cached trigger result, fail both
                // the single_node_ready and reuse_cached_upstream gates, and re-run
                // the one-shot Telegram/Gmail/WhatsApp Stimulus under
                // trigger_source="manual" — overwriting its real captured payload
                // with {"trigger":"manual"}. That is the intermittent "trigger flips
                // to manual" bug, and it is intermittent precisely because it only
                // bites when the *previous* run happened to be such a partial run.
                //
                // Fix: seed from the newest finished run, then BACKFILL any node
                // missing from it with that node's most recent *successful* result
                // from older runs. In the healthy case the newest run already has
                // every node, the early-exit fires after one parse, and behavior is
                // unchanged; only when a recent partial run dropped a node (like the
                // trigger) do we recover its last good payload — so the trigger keeps
                // its data across Execute Step clicks on unrelated nodes.
                if let Ok(conn) = state.db.get() {
                    if let Ok(mut stmt) = conn.prepare(
                        "SELECT node_results FROM workflow_runs \
                         WHERE workflow_id = ?1 AND id != ?2 AND status IN ('success','error') \
                         ORDER BY started_at DESC LIMIT 25",
                    ) {
                        let rows = stmt
                            .query_map(rusqlite::params![workflow_id, run_id], |r| {
                                r.get::<_, String>(0)
                            })
                            .map(|m| m.filter_map(|x| x.ok()).collect::<Vec<String>>())
                            .unwrap_or_default();

                        let current_ids: std::collections::HashSet<&str> =
                            nodes.iter().map(|n| n.id.as_str()).collect();
                        // The first run that parses seeds the snapshot verbatim;
                        // later runs only backfill nodes it dropped. Driven off a
                        // `seeded` flag (not the row index) so a single unparseable
                        // newest row doesn't demote the next run to backfill-only.
                        let mut seeded = false;
                        for results_str in &rows {
                            let Ok(mut results) =
                                serde_json::from_str::<Vec<NodeResult>>(results_str)
                            else {
                                continue;
                            };
                            binary::rehydrate_results(&mut results);
                            let complete = fold_prior_run_into_cache(
                                &mut node_results,
                                &mut prior_ordered,
                                results,
                                !seeded,
                                &current_ids,
                            );
                            seeded = true;
                            // Every current node already has a cached result — older
                            // runs can add nothing, so stop parsing (keeps the healthy
                            // path at a single parse).
                            if complete {
                                break;
                            }
                        }
                    }
                }
                std::collections::HashSet::new()
            }
        };

        // Align cached/loaded result identities with the CURRENT graph so that
        // $node["Name"] references (resolved by node_name) still find a node that
        // was renamed after the cached run produced its result. Without this, a
        // reused upstream node keeps its stale stored name and references to its
        // current name resolve to null — e.g. a Telegram caption that reads
        // $node["Axon 2"].data.output comes through empty even though the editor
        // preview shows the value.
        restamp_result_identities(
            &mut node_results,
            &mut prior_ordered,
            &mut ordered_results,
            &nodes,
        );

        let mut in_degree = std::collections::HashMap::new();
        // Counts how many *taken* (non-skipped-branch) inputs each node has
        // received. A node whose in-degree reaches 0 with no live inputs sits
        // entirely on not-taken branches and is skipped instead of executed.
        let mut live_inputs: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut workflow_status = "success".to_string();
        for n in &nodes {
            in_degree.insert(n.id.clone(), 0);
        }
        for e in &edges {
            *in_degree.entry(e.target_id.clone()).or_insert(0) += 1;
        }

        // Single-node mode ("Execute Step" when upstream nodes already have data):
        // run ONLY the target, feeding it the cached results loaded into
        // node_results above. Only honoured when every immediate upstream node
        // actually has a cached result — otherwise the node would run with stale
        // or missing inputs, so we fall back to the full ancestor run.
        let single_node_ready = single_node
            && target_node_id.as_ref().is_some_and(|tid| {
                edges
                    .iter()
                    .filter(|e| &e.target_id == tid)
                    .all(|e| node_results.contains_key(&e.source_id))
            });

        // Logic for partial run: Identification of ancestors
        let mut required_node_ids = std::collections::HashSet::new();
        if let Some(ref target_id) = target_node_id {
            if single_node_ready {
                required_node_ids.insert(target_id.clone());
                tracing::info!(
                    "Single-node run: executing only {} (using cached upstream data)",
                    target_id
                );
            } else {
                let mut stack = vec![target_id.clone()];
                while let Some(current) = stack.pop() {
                    if required_node_ids.insert(current.clone()) {
                        // Find all sources that lead to this node
                        for e in edges.iter().filter(|e| e.target_id == current) {
                            stack.push(e.source_id.clone());
                        }
                    }
                }
                tracing::info!(
                    "Partial run: Required nodes for {}: {:?}",
                    target_id,
                    required_node_ids
                );
            }
        }

        let has_triggers = nodes
            .iter()
            .any(|n| matches!(n.node_type.as_str(), "trigger" | "circadian" | "stimulus"));

        // When a run is initiated by a specific event source (a Telegram reply/
        // callback, a Gmail poll, a Circadian/cron tick, etc.), only start from
        // trigger nodes OF THAT TYPE. This isolates trigger branches in a
        // multi-trigger workflow: e.g. a Circadian tick must NOT also fire a
        // Telegram trigger sitting in the same workflow, and a Telegram reply must
        // NOT fire a Gmail trigger. The source strings here line up with a
        // Stimulus node's `config.type` ("cron" is the Circadian type). Only a
        // genuinely untyped run — "manual" (the Run button) or a "subflow" entry
        // (narrowed separately by the pin below) — starts from every trigger node.
        let entry_trigger_type: Option<&str> = match trigger_source {
            "telegram" | "gmail" | "whatsapp" | "webhook" | "github" | "facebook" | "cron"
            | "crm" => Some(trigger_source),
            // An error run (A3) starts ONLY from error-type trigger nodes; a normal
            // run never does (handled by `is_error_trigger` exclusion below).
            "error" => Some("error"),
            _ => None,
        };

        // A run may pin a single entry trigger to start from (its downstream chain
        // runs, sibling triggers stay dormant). Two sources set this pin, both
        // staged keyed by THIS run id: a sub-workflow call choosing one entry of
        // a multi-trigger child, and a manual "Run" click on a specific Stimulus
        // node's play button (set in `run_in_background_inner`).
        // Consumed here so a later unpinned run falls back to every trigger.
        let entry_node: Option<String> = trigger_data::take_entry_node(&run_id);

        let mut queue: std::collections::VecDeque<_> = nodes
            .iter()
            .filter(|n| {
                let deg = *in_degree.get(&n.id).unwrap_or(&0) == 0;
                if single_node_ready {
                    // Force just the target into the queue regardless of in-degree;
                    // its inputs come from cached upstream results, not a fresh run.
                    target_node_id.as_deref() == Some(n.id.as_str())
                } else if target_node_id.is_some() {
                    deg && required_node_ids.contains(&n.id)
                } else if has_triggers {
                    // Strict pipeline definition: Only start from Trigger nodes if they exist.
                    // This prevents separated, orphaned subgraphs from running accidentally.
                    // When the run is source-scoped, also require the trigger node's
                    // type to match so other trigger branches stay dormant.
                    // A pinned sub-workflow entry narrows it further to that one node.
                    //
                    // Effective kind: a unified Stimulus node carries its kind in
                    // `config.type`; a legacy bare `circadian` node predates that and
                    // stores no `config.type` but means a cron trigger. Falling back
                    // to the node_type keeps those firing on a scheduled run instead
                    // of silently matching nothing.
                    let node_trigger_kind =
                        n.config.get("type").and_then(|v| v.as_str()).unwrap_or(
                            match n.node_type.as_str() {
                                "circadian" => "cron",
                                _ => "manual",
                            },
                        );
                    deg && matches!(n.node_type.as_str(), "trigger" | "circadian" | "stimulus")
                        && entry_trigger_type.map_or(true, |want| node_trigger_kind == want)
                        // Error triggers (A3) are eligible ONLY on an error run; a
                        // normal/manual run must never start from one (it's a
                        // failure handler, not a regular entry point).
                        && (trigger_source == "error" || node_trigger_kind != "error")
                        && entry_node
                            .as_deref()
                            .map_or(true, |chosen| n.id == chosen)
                } else {
                    deg
                }
            })
            .map(|n| n.id.clone())
            .collect();

        tracing::info!("Initial queue with {} nodes", queue.len());

        while let Some(current_id) = queue.pop_front() {
            tracing::debug!("Processing node {}", current_id);
            // Check for cancellation — ensure guard is dropped before any .await
            let is_cancelled = {
                let cancellations = state.workflow_cancellations.lock().await;
                cancellations.contains(workflow_id) || cancellations.contains(&run_id)
            };

            if is_cancelled {
                tracing::info!("Workflow run {} cancelled", run_id);
                {
                    let conn = state.db.get()?;
                    conn.execute("UPDATE workflow_runs SET status = 'cancelled', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?", [&run_id])?;
                }
                crate::observability::record_run_complete(
                    "cancelled",
                    start.elapsed().as_secs_f64(),
                );
                return Ok(WorkflowRunResult {
                    run_id,
                    workflow_id: workflow_id.to_string(),
                    status: "cancelled".to_string(),
                    node_results: node_results.into_values().collect(),
                    final_output: json!({}),
                    total_duration_ms: start.elapsed().as_millis() as u64,
                });
            }

            let node = match nodes.iter().find(|n| n.id == current_id) {
                Some(n) => n,
                None => continue,
            };
            if !node.enabled {
                // On resume this disabled node already emitted its skip; replay
                // only its pass-through routing so the chain isn't duplicated.
                if !resumed_completed.contains(&current_id) {
                    // Emit a "skipped" result so the frontend can properly transition
                    // the animation instead of leaving this node stuck in waiting state.
                    let nr = NodeResult {
                        node_id: current_id.clone(),
                        node_name: node.name.clone(),
                        node_type: node.node_type.clone(),
                        position: node.position,
                        status: "skipped".to_string(),
                        output: json!({"skipped": true, "reason": "Node is disabled"}),
                        duration_ms: 0,
                        error: None,
                        attempts: 0,
                    };
                    node_results.insert(current_id.clone(), nr.clone());
                    ordered_results.push(nr);

                    // Incremental DB update so the frontend poll sees it immediately
                    let res_json = binary::results_to_db_json(&ordered_results, bin_threshold);
                    if let Ok(conn) = state.db.get() {
                        persist_run_update(
                            &conn,
                            "UPDATE workflow_runs SET node_results = ? WHERE id = ?",
                            rusqlite::params![res_json, run_id.clone()],
                            &run_id,
                            "incremental node_results",
                        );
                    }
                }

                for e in edges.iter().filter(|e| e.source_id == current_id) {
                    let deg = in_degree.entry(e.target_id.clone()).or_insert(1);
                    if *deg > 0 {
                        *deg -= 1;
                    }
                    // A disabled node passes through: downstream still runs.
                    *live_inputs.entry(e.target_id.clone()).or_insert(0) += 1;
                    if *deg == 0 {
                        queue.push_back(e.target_id.clone());
                    }
                }
                continue;
            }

            // In a targeted run, reuse a one-shot TRIGGER's cached payload instead
            // of re-executing it: a Telegram/Gmail/WhatsApp Stimulus consumes
            // (removes) its live event the first time it's read, so re-running it
            // under the 'manual' source would overwrite the real payload with
            // {"trigger":"manual"} (the "trigger flips to manual" bug).
            //
            // Regular action nodes are deliberately NOT reused here. The single-node
            // "Execute Step" button never queues ancestors at all — it resolves them
            // from the cached snapshot — so this branch only fires for the "run node
            // + dependencies" play button, whose whole purpose is to re-run the chain
            // up to the target. Reusing a cached action node on that path silently
            // froze external-IO producers: e.g. a Google Sheets PDF export node never
            // re-ran, the file on disk was never refreshed, and the downstream
            // Telegram send shipped the stale/first file. Re-executing them keeps
            // downstream consumers in sync with current external state. Cached errors
            // are never reused — they re-run for a fresh attempt (matches the
            // frontend's `!r.error` "Has Data" gate).
            let is_oneshot_trigger = matches!(
                node.node_type.as_str(),
                "trigger" | "stimulus" | "circadian"
            );
            let reuse_cached_upstream = target_node_id.is_some()
                && target_node_id.as_deref() != Some(current_id.as_str())
                && is_oneshot_trigger
                && node_results
                    .get(&current_id)
                    .is_some_and(|r| r.status == "success");
            if reuse_cached_upstream {
                // Keep the reused result in this run's persisted chain so a
                // non-single-node save (which writes ordered_results directly,
                // unmerged) doesn't drop the upstream node's data.
                if let Some(cached) = node_results.get(&current_id).cloned() {
                    if !ordered_results.iter().any(|r| r.node_id == current_id) {
                        ordered_results.push(cached);
                    }
                }
            }

            // Pinned data (A4): on a manual/editor run ONLY, a node that has
            // saved pinned output is not executed — its pin is routed downstream
            // as the result so building/testing is deterministic and external
            // side-effects (sends, writes) don't fire while iterating. Any
            // non-"manual" source (telegram/gmail/webhook/cron/subflow/error…) is
            // a production/trigger run and ignores pins entirely. A node replayed
            // on resume keeps its already-stored result rather than its pin.
            let use_pin = trigger_source == "manual"
                && !resumed_completed.contains(&current_id)
                && !reuse_cached_upstream
                && node.pinned_data.is_some();
            if use_pin {
                let pinned = node.pinned_data.clone().unwrap_or_else(|| json!({}));
                tracing::info!(
                    "Node '{}' ({}) using pinned data — skipping execution (manual run)",
                    node.name,
                    current_id
                );
                let nr = NodeResult {
                    node_id: current_id.clone(),
                    node_name: node.name.clone(),
                    node_type: node.node_type.clone(),
                    position: node.position,
                    status: "success".to_string(),
                    output: pinned,
                    duration_ms: 0,
                    error: None,
                    attempts: 0,
                };
                node_results.insert(current_id.clone(), nr.clone());
                ordered_results.push(nr);
            }

            // Replay-only on resume: a node already completed in THIS run keeps
            // its stored result and just releases its edges below — it is never
            // re-executed, so triggers don't re-fire and side effects (Telegram
            // sends, file registration) don't repeat. Freshly-reached nodes run
            // normally. The block is closed right before edge routing.
            if !resumed_completed.contains(&current_id) && !reuse_cached_upstream && !use_pin {
                let n_start = std::time::Instant::now();
                // Upstream node ids of the node about to run — used so a
                // `$node["Name"]` reference whose name collides with another node
                // (e.g. legacy workflows predating unique-naming) resolves toward
                // this node's actual upstream instead of a random HashMap match.
                let node_ancestors = ancestor_node_ids(&current_id, &edges);
                let iteration_source_id =
                    find_iteration_source_node_id(&current_id, &edges, &node_results);
                // "Execute Once" (n8n's "Run Once for All Items"): when set, the node
                // does NOT fan out over a loop collection — it runs a single time with
                // the full `items` array visible via {{ $node["Loop"].items }}. This is
                // the clean aggregation/"collect after loop" boundary the old engine
                // lacked, letting a JS/HTTP/Axon node reduce a loop's results.
                let execute_once = node
                    .config
                    .get("execute_once")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let can_iterate = !execute_once
                    && !matches!(
                        node.node_type.as_str(),
                        "loop"
                            | "ifCondition"
                            | "switch"
                            | "trigger"
                            | "circadian"
                            | "stimulus"
                            | "subflow"
                            | "workflow"
                    );

                let (result, attempts): (Result<Value, String>, u32) = if can_iterate {
                    if let Some(source_node_id) = iteration_source_id {
                        if let Some(source_result) = node_results.get(&source_node_id) {
                            if let Some(items) =
                                source_result.output.get("items").and_then(|v| v.as_array())
                            {
                                // Concurrency/batch knobs the Loop node embedded in its
                                // marker (defaults: sequential, one item per iteration).
                                let loop_meta = source_result.output.get("_axon_loop");
                                let parallelism = loop_meta
                                    .and_then(|m| m.get("parallelism"))
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n as usize)
                                    .unwrap_or(1)
                                    .max(1);
                                let batch_size = loop_meta
                                    .and_then(|m| m.get("batch_size"))
                                    .and_then(|v| v.as_u64())
                                    .map(|n| n as usize)
                                    .unwrap_or(1)
                                    .max(1);

                                // Work units: one per item, or one per batch slice when
                                // batch_size > 1 (n8n SplitInBatches style — each unit's
                                // `current` is then the array of items in that batch).
                                let units: Vec<(usize, Value)> = if batch_size > 1 {
                                    items
                                        .chunks(batch_size)
                                        .enumerate()
                                        .map(|(i, c)| (i, Value::Array(c.to_vec())))
                                        .collect()
                                } else {
                                    items
                                        .iter()
                                        .enumerate()
                                        .map(|(i, it)| (i, it.clone()))
                                        .collect()
                                };
                                let unit_count = units.len();
                                let run_id_ref = run_id.as_str();

                                // Inject loop context onto the iteration source's result
                                // so the body can read {{ $node["Loop"].current/index/... }}.
                                let build_unit = |idx: usize, current: &Value| {
                                    let mut temp_results = node_results.clone();
                                    if let Some(source_mut) = temp_results.get_mut(&source_node_id)
                                    {
                                        if let Some(out_obj) = source_mut.output.as_object_mut() {
                                            out_obj.insert("current".to_string(), current.clone());
                                            out_obj.insert("index".to_string(), json!(idx));
                                            out_obj.insert("is_first".to_string(), json!(idx == 0));
                                            out_obj.insert(
                                                "is_last".to_string(),
                                                json!(idx + 1 == unit_count),
                                            );
                                            out_obj.insert("total".to_string(), json!(unit_count));
                                        }
                                    }
                                    let item_config = interpolate_config(
                                        &node.config,
                                        &temp_results,
                                        state,
                                        Some(&node_ancestors),
                                        &run_id,
                                    );
                                    (item_config, temp_results)
                                };

                                let mut iteration_outputs = Vec::new();
                                let mut iteration_errors = Vec::new();
                                // A1: worst-case retry count across units becomes the
                                // body node's reported attempts; per-unit counts also
                                // ride along in each errors[] entry.
                                let mut max_unit_attempts: u32 = 0;

                                if parallelism > 1 {
                                    // Concurrent fan-out — a real edge over n8n's
                                    // single-threaded executor. buffered() preserves
                                    // input order, so outputs stay item-aligned.
                                    use futures::StreamExt;
                                    let futs = units.into_iter().map(|(idx, current)| {
                                        let (item_config, temp_results) = build_unit(idx, &current);
                                        async move {
                                            let (r, a) = execute_node_by_type(
                                                node,
                                                &item_config,
                                                state,
                                                trigger_source,
                                                workflow_id,
                                                run_id_ref,
                                                &temp_results,
                                                // A Wait inside a Loop body can't durably
                                                // suspend — it sleeps in-process per item.
                                                false,
                                            )
                                            .await;
                                            (idx, current, r, a)
                                        }
                                    });
                                    let collected: Vec<(usize, Value, Result<Value, String>, u32)> =
                                        futures::stream::iter(futs)
                                            .buffered(parallelism)
                                            .collect()
                                            .await;
                                    for (idx, item, r, a) in collected {
                                        max_unit_attempts = max_unit_attempts.max(a);
                                        match r {
                                        Ok(v) => iteration_outputs.push(v),
                                        Err(e) => iteration_errors.push(json!({
                                            "index": idx, "item": item, "error": e, "attempts": a
                                        })),
                                    }
                                    }
                                } else {
                                    // Sequential: honours stop-on-first-error (n8n parity)
                                    // unless continue_on_fail is set.
                                    for (idx, current) in units {
                                        let (item_config, temp_results) = build_unit(idx, &current);
                                        let (r, a) = execute_node_by_type(
                                            node,
                                            &item_config,
                                            state,
                                            trigger_source,
                                            workflow_id,
                                            run_id_ref,
                                            &temp_results,
                                            // Iterated Wait: in-process sleep per item.
                                            false,
                                        )
                                        .await;
                                        max_unit_attempts = max_unit_attempts.max(a);
                                        match r {
                                            Ok(v) => iteration_outputs.push(v),
                                            Err(e) => {
                                                iteration_errors.push(json!({
                                                "index": idx, "item": current, "error": e, "attempts": a
                                            }));
                                                if !node.continue_on_fail {
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }

                                let loop_result =
                                    if !iteration_errors.is_empty() && !node.continue_on_fail {
                                        Err(format!(
                                            "Iteration failed in node '{}' ({} errors)",
                                            node.name,
                                            iteration_errors.len()
                                        ))
                                    } else {
                                        Ok(json!({
                                            "_axon_loop": {
                                                "enabled": true,
                                                "count": unit_count,
                                                "source_node_id": source_node_id,
                                                "parallelism": parallelism,
                                                "batch_size": batch_size
                                            },
                                            "items": iteration_outputs,
                                            "count": unit_count,
                                            "total": unit_count,
                                            "error_count": iteration_errors.len(),
                                            "errors": iteration_errors
                                        }))
                                    };
                                (loop_result, max_unit_attempts.max(1))
                            } else {
                                let config = interpolate_config(
                                    &node.config,
                                    &node_results,
                                    state,
                                    Some(&node_ancestors),
                                    &run_id,
                                );
                                execute_node_by_type(
                                    node,
                                    &config,
                                    state,
                                    trigger_source,
                                    workflow_id,
                                    &run_id,
                                    &node_results,
                                    target_node_id.is_none(),
                                )
                                .await
                            }
                        } else {
                            let config = interpolate_config(
                                &node.config,
                                &node_results,
                                state,
                                Some(&node_ancestors),
                                &run_id,
                            );
                            execute_node_by_type(
                                node,
                                &config,
                                state,
                                trigger_source,
                                workflow_id,
                                &run_id,
                                &node_results,
                                target_node_id.is_none(),
                            )
                            .await
                        }
                    } else {
                        let config = interpolate_config(
                            &node.config,
                            &node_results,
                            state,
                            Some(&node_ancestors),
                            &run_id,
                        );
                        execute_node_by_type(
                            node,
                            &config,
                            state,
                            trigger_source,
                            workflow_id,
                            &run_id,
                            &node_results,
                            target_node_id.is_none(),
                        )
                        .await
                    }
                } else {
                    let config = interpolate_config(
                        &node.config,
                        &node_results,
                        state,
                        Some(&node_ancestors),
                        &run_id,
                    );
                    execute_node_by_type(
                        node,
                        &config,
                        state,
                        trigger_source,
                        workflow_id,
                        &run_id,
                        &node_results,
                        target_node_id.is_none(),
                    )
                    .await
                };
                let duration = n_start.elapsed().as_millis() as u64;
                crate::observability::record_node_exec(&node.node_type, duration as f64 / 1000.0);
                let (status, output, error) = match result {
                    Ok(v) => ("success".to_string(), v, None),
                    Err(e) => ("error".to_string(), json!({}), Some(e)),
                };

                tracing::info!(
                    "Node '{}' ({}, type={}) completed in {}ms — status={}",
                    node.name,
                    current_id,
                    node.node_type,
                    duration,
                    status
                );

                let mut nr = NodeResult {
                    node_id: current_id.clone(),
                    node_name: node.name.clone(),
                    node_type: node.node_type.clone(),
                    position: node.position,
                    status: status.clone(),
                    output,
                    duration_ms: duration,
                    error,
                    attempts,
                };

                // Durable Wait suspension: a long Wait returns a sentinel instead of
                // blocking an in-process sleep. Persist the chain so far plus WHEN and
                // WHERE to resume, mark the run 'waiting', and hand the task back. A
                // background poller re-enters the workflow once resume_at passes, so
                // the pause survives an agent restart.
                if let Some(marker) = nr.output.get(nodes::wait::SUSPEND_MARKER).cloned() {
                    let suspend_mode = marker
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("interval")
                        .to_string();

                    // Drop the internal sentinel from the visible result but keep the
                    // node marked 'waiting' so the editor shows the node paused.
                    if let Some(obj) = nr.output.as_object_mut() {
                        obj.remove(nodes::wait::SUSPEND_MARKER);
                    }

                    // Compute the wake deadline. A timed Wait anchors it to the suspend
                    // instant. A webhook/approval Wait (C1) instead parks until an
                    // external caller hits its node+run-scoped resume URL; any timeout
                    // becomes the deadline (NULL = wait forever, only a resume URL
                    // wakes it).
                    let resume_at_db: Option<String> = if suspend_mode == "webhook"
                        || suspend_mode == "approval"
                    {
                        // No token minted: the node id addresses the parked node and
                        // the (unguessable UUIDv4) run id scopes + secures the wake, so
                        // a leaked link can't touch any other run and dies the instant
                        // this one resumes. `resume_by_node` locates the run via
                        // resume_node_id. A timeout still mirrors into resume_at so the
                        // poller can fire the timeout branch; NULL = wait forever.
                        let ttl = match marker.get("expires_seconds").and_then(|v| v.as_f64()) {
                            Some(s) if s > 0.0 => s,
                            _ => state.settings.workflow_resume_token_default_ttl_secs() as f64,
                        };
                        let expires_at: Option<String> = (ttl > 0.0).then(|| {
                            (chrono::Utc::now()
                                + chrono::Duration::milliseconds((ttl * 1000.0) as i64))
                            .format("%Y-%m-%dT%H:%M:%SZ")
                            .to_string()
                        });
                        let base = state.settings.workflow_public_base_url();
                        let link = |p: &str| {
                            if base.is_empty() {
                                p.to_string()
                            } else {
                                format!("{base}{p}")
                            }
                        };
                        // Both ids are known now, so the links surfaced on the node
                        // output are fully run-scoped — what a dashboard operator
                        // clicks. Automation that notifies from an UPSTREAM node builds
                        // the same URL from the sidebar template + `{{ $execution.runId }}`.
                        let resume_path = format!("/webhook/resume/{current_id}/{run_id}");
                        if let Some(obj) = nr.output.as_object_mut() {
                            obj.insert("resume_path".into(), json!(resume_path));
                            obj.insert("resume_url".into(), json!(link(&resume_path)));
                            if suspend_mode == "approval" {
                                let approve_path =
                                    format!("/webhook/approve/{current_id}/{run_id}");
                                let reject_path = format!("/webhook/reject/{current_id}/{run_id}");
                                obj.insert("approve_path".into(), json!(approve_path));
                                obj.insert("approve_url".into(), json!(link(&approve_path)));
                                obj.insert("reject_path".into(), json!(reject_path));
                                obj.insert("reject_url".into(), json!(link(&reject_path)));
                            }
                            // Read by the time poller to pick the timeout branch if the
                            // deadline fires before anyone resumes (see resume path).
                            obj.insert("__axon_resume".into(), json!({ "mode": suspend_mode }));
                        }
                        expires_at
                    } else {
                        let seconds = marker
                            .get("seconds")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        Some(
                            (chrono::Utc::now()
                                + chrono::Duration::milliseconds((seconds * 1000.0) as i64))
                            .format("%Y-%m-%dT%H:%M:%SZ")
                            .to_string(),
                        )
                    };

                    nr.status = "waiting".to_string();
                    node_results.insert(current_id.clone(), nr.clone());
                    ordered_results.push(nr.clone());

                    let chain_json = binary::results_to_db_json(&ordered_results, bin_threshold);
                    {
                        let conn = state.db.get()?;
                        conn.execute(
                            "UPDATE workflow_runs SET status = 'waiting', resume_at = ?1, \
                         resume_node_id = ?2, trigger_type = ?3, node_results = ?4 WHERE id = ?5",
                            rusqlite::params![
                                resume_at_db,
                                current_id,
                                trigger_source,
                                chain_json,
                                run_id
                            ],
                        )?;
                    }
                    tracing::info!(
                        "Workflow run {} suspended at node '{}' ({} mode){} (durable)",
                        run_id,
                        node.name,
                        suspend_mode,
                        match &resume_at_db {
                            Some(t) => format!(" until {t}"),
                            None => String::new(),
                        }
                    );
                    return Ok(WorkflowRunResult {
                        run_id,
                        workflow_id: workflow_id.to_string(),
                        status: "waiting".to_string(),
                        node_results: ordered_results,
                        final_output: nr.output,
                        total_duration_ms: start.elapsed().as_millis() as u64,
                    });
                }

                node_results.insert(current_id.clone(), nr.clone());
                ordered_results.push(nr.clone());

                // Register workflow-sent Telegram messages so replies can be routed
                // back to this workflow's telegram trigger.
                if nr.status == "success" && nr.node_type == "telegram" {
                    record_telegram_reply_route(state, workflow_id, &nr.output);
                }

                // Halting logic: if stop on error and node failed, break the whole workflow.
                if status == "error" && !node.continue_on_fail {
                    tracing::warn!(
                        "Workflow execution halted due to error in node '{}' ({})",
                        node.name,
                        current_id
                    );
                    workflow_status = "error".to_string();
                    break;
                }

                // Scan for files in the node result to auto-register in DB/UI
                let reg_start = std::time::Instant::now();
                state
                    .files
                    .register_from_json(&nr.output, Some(node.name.clone()))
                    .await;
                let reg_ms = reg_start.elapsed().as_millis();
                if reg_ms > 100 {
                    tracing::warn!("File registration for '{}' took {}ms", node.name, reg_ms);
                }
            } // end: execute fresh node (skipped for replayed-on-resume nodes)

            let mut skip_stack: Vec<String> = Vec::new();
            for e in edges.iter().filter(|e| e.source_id == current_id) {
                // If this is a partial run, only continue down required paths
                if target_node_id.is_some() && !required_node_ids.contains(&e.target_id) {
                    continue;
                }

                // Branch routing for IF/Switch nodes: only follow matching output
                // handle(s). A Switch in "all" mode reports several active outputs
                // (outputIndices); an edge is live if its handle matches ANY of them.
                let mut live = true;
                if node.node_type == "ifCondition"
                    || node.node_type == "switch"
                    || node.node_type == "approval"
                {
                    if let Some(nr) = node_results.get(&current_id) {
                        // Prefer outputIndices (multi-match); fall back to the single
                        // outputIndex for IF and legacy results.
                        let active: Vec<i64> = nr
                            .output
                            .get("outputIndices")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| {
                                        x.as_i64().or_else(|| {
                                            x.as_str().and_then(|s| s.parse::<i64>().ok())
                                        })
                                    })
                                    .collect()
                            })
                            .filter(|v: &Vec<i64>| !v.is_empty())
                            .unwrap_or_else(|| {
                                vec![nr
                                    .output
                                    .get("outputIndex")
                                    .and_then(|v| {
                                        v.as_i64().or_else(|| {
                                            v.as_str().and_then(|s| s.parse::<i64>().ok())
                                        })
                                    })
                                    .unwrap_or(0)]
                            });

                        // A branch edge MUST be gated even when its handle is
                        // missing/empty: an ungated edge let the NOT-taken branch
                        // run (e.g. the False branch firing while the condition is
                        // True). A handle-less edge defaults to the first output
                        // (index 0 / "true"), matching the editor — which renders
                        // and persists a bare edge as `output_main_0`.
                        let sh = e
                            .source_handle
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("output_main_0");
                        let lower = sh.to_lowercase();
                        let matches = active.iter().any(|&oi| {
                            format!("output_main_{}", oi) == sh
                                || (node.node_type == "ifCondition"
                                    && ((oi == 0 && lower == "true")
                                        || (oi == 1 && lower == "false")))
                        });

                        if !matches {
                            tracing::info!(
                                "Branch node {}: skipping edge to {} (handle '{}' not in active outputs {:?})",
                                current_id,
                                e.target_id,
                                sh,
                                active
                            );
                            live = false;
                        } else {
                            tracing::info!(
                                "Branch node {}: following edge to {} (handle '{}')",
                                current_id,
                                e.target_id,
                                sh
                            );
                        }
                    }
                }

                // Even a not-taken branch edge must release the target's
                // in-degree — otherwise a merge node fed by both branches
                // deadlocks and skipped-branch nodes hang forever.
                let deg = in_degree.entry(e.target_id.clone()).or_insert(1);
                if *deg > 0 {
                    *deg -= 1;
                }
                if live {
                    *live_inputs.entry(e.target_id.clone()).or_insert(0) += 1;
                }
                if *deg == 0 {
                    if live_inputs.get(&e.target_id).copied().unwrap_or(0) > 0 {
                        queue.push_back(e.target_id.clone());
                    } else {
                        skip_stack.push(e.target_id.clone());
                    }
                }
            }

            // Propagate skips: a node whose inputs were all not-taken branches
            // never executes. Emit an explicit 'skipped' result (so the UI can
            // settle) and release its downstream edges in turn.
            while let Some(skip_id) = skip_stack.pop() {
                if let Some(sn) = nodes.iter().find(|n| n.id == skip_id) {
                    let nr = NodeResult {
                        node_id: skip_id.clone(),
                        node_name: sn.name.clone(),
                        node_type: sn.node_type.clone(),
                        position: sn.position,
                        status: "skipped".to_string(),
                        output: json!({"skipped": true, "reason": "Branch not taken"}),
                        duration_ms: 0,
                        error: None,
                        attempts: 0,
                    };
                    // Keep any preloaded last-run result for expression
                    // fallback; only record the skip for this run's sequence.
                    node_results.entry(skip_id.clone()).or_insert(nr.clone());
                    ordered_results.push(nr);
                }
                for e in edges.iter().filter(|e| e.source_id == skip_id) {
                    if target_node_id.is_some() && !required_node_ids.contains(&e.target_id) {
                        continue;
                    }
                    let deg = in_degree.entry(e.target_id.clone()).or_insert(1);
                    if *deg > 0 {
                        *deg -= 1;
                    }
                    if *deg == 0 {
                        if live_inputs.get(&e.target_id).copied().unwrap_or(0) > 0 {
                            queue.push_back(e.target_id.clone());
                        } else {
                            skip_stack.push(e.target_id.clone());
                        }
                    }
                }
            }

            // Incremental Progress Update for Polling (Sync the ordered sequence).
            // Single-node runs persist the merged chain so polling never wipes the
            // upstream nodes' data off the editor mid-step.
            let res_json = if single_node_ready {
                binary::results_to_db_json(
                    &merge_single_node_results(&prior_ordered, &ordered_results),
                    bin_threshold,
                )
            } else {
                binary::results_to_db_json(&ordered_results, bin_threshold)
            };
            if let Ok(conn) = state.db.get() {
                persist_run_update(
                    &conn,
                    "UPDATE workflow_runs SET node_results = ? WHERE id = ?",
                    rusqlite::params![res_json, run_id.clone()],
                    &run_id,
                    "final node_results",
                );
            }
        }

        let results_vec = ordered_results;
        let total_ms = start.elapsed().as_millis() as u64;
        let status =
            if workflow_status == "error" || results_vec.iter().any(|r| r.status == "error") {
                "error"
            } else {
                "success"
            };
        let final_output = results_vec
            .last()
            .map(|r| r.output.clone())
            .unwrap_or(json!({}));

        {
            let conn = state.db.get()?;
            // Single-node runs save the merged chain (prior cached results + the
            // node we just re-ran) so reloads, expression fallbacks, and later
            // steps keep every upstream node's data.
            let res_json = if single_node_ready {
                binary::results_to_db_json(
                    &merge_single_node_results(&prior_ordered, &results_vec),
                    bin_threshold,
                )
            } else {
                binary::results_to_db_json(&results_vec, bin_threshold)
            };
            conn.execute("UPDATE workflow_runs SET status = ?, finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), node_results = ? WHERE id = ?", [status, &res_json, &run_id])?;
            conn.execute("UPDATE workflows SET last_status = ?, last_run_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?", [status, workflow_id])?;
        }
        // C3: terminal run metric (success/error). 'waiting' suspends return early
        // above, so they're never double-counted here.
        crate::observability::record_run_complete(status, total_ms as f64 / 1000.0);

        let failed_nodes: Vec<&NodeResult> =
            results_vec.iter().filter(|r| r.status == "error").collect();
        if !failed_nodes.is_empty() {
            let mut detail_lines = vec![
                format!("workflow_id={}", workflow_id),
                format!("run_id={}", run_id),
            ];
            for failed in failed_nodes.iter().take(5) {
                detail_lines.push(format!(
                    "- node='{}' type='{}' error={}",
                    failed.node_name,
                    failed.node_type,
                    failed.error.as_deref().unwrap_or("unknown")
                ));
            }
            if failed_nodes.len() > 5 {
                detail_lines.push(format!(
                    "- ... and {} additional node errors",
                    failed_nodes.len() - 5
                ));
            }

            if let Err(e) = send_global_error_notification(
                state,
                "workflow.engine",
                &format!("Workflow '{}' reported execution errors", workflow_name),
                &detail_lines.join("\n"),
                None,
                None,
            )
            .await
            {
                tracing::warn!("Workflow global error notification failed: {}", e);
            }

            // Error workflow (A3): hand off the failure to a designated handler so
            // the operator can notify/compensate (the n8n "Error Trigger" pattern).
            // Resolution: this workflow's `error_workflow_id` → global default
            // `workflow.default_error_workflow_id`. Loop-guarded: never fired from
            // an error run, never targets this same workflow, target must exist and
            // be enabled.
            //
            // Two n8n-parity guards, both required:
            //   * Only a TERMINAL failure fires it (`workflow_status == "error"`: a
            //     node failed with continue_on_fail OFF and halted the run). A node
            //     that errored under continue_on_fail leaves `workflow_status` at
            //     "success" — the run handled it by design — so it must not trip the
            //     Error Trigger.
            //   * Only AUTOMATIC runs fire it. "manual" is the editor Run/Execute
            //     Step (and agent/`/run` explicit invocations); n8n never runs the
            //     Error Trigger for those, so a failing test run while building can't
            //     spam the production error handler. Real event triggers now carry
            //     their own source (telegram/webhook/gmail/…), not "manual".
            // The global notification above still fires for any errored node.
            if trigger_source != "error" && trigger_source != "manual" && workflow_status == "error"
            {
                // Resolve the handler id (workflow-level, then global default).
                // Each DB read is scoped so no pooled connection is held across
                // an await.
                let configured: Option<String> = {
                    let level = state
                        .db
                        .get()
                        .ok()
                        .and_then(|conn| {
                            conn.query_row(
                                "SELECT error_workflow_id FROM workflows WHERE id = ?1",
                                [workflow_id],
                                |r| r.get::<_, Option<String>>(0),
                            )
                            .ok()
                            .flatten()
                        })
                        .filter(|s| !s.trim().is_empty());
                    level.or_else(|| {
                        let d = state
                            .settings
                            .get_str("workflow.default_error_workflow_id", "");
                        (!d.trim().is_empty()).then_some(d)
                    })
                };

                if let Some(error_wf_id) = configured {
                    let error_wf_id = error_wf_id.trim().to_string();
                    // Target must be a different, enabled workflow.
                    let eligible = error_wf_id != workflow_id
                        && state
                            .db
                            .get()
                            .ok()
                            .and_then(|conn| {
                                conn.query_row(
                                    "SELECT enabled FROM workflows WHERE id = ?1",
                                    [&error_wf_id],
                                    |r| r.get::<_, i64>(0),
                                )
                                .ok()
                            })
                            .map(|enabled| enabled != 0)
                            .unwrap_or(false);

                    if eligible {
                        // The node that actually halted the run is the LAST errored
                        // node in execution order (the run breaks on a terminal
                        // failure), not the first — an earlier continue_on_fail error
                        // may sit ahead of it in `failed_nodes`.
                        let culprit = failed_nodes.last();
                        // Bound the error string so a giant node error can't bloat
                        // the handler's trigger payload.
                        let err_str: String = culprit
                            .and_then(|f| f.error.clone())
                            .unwrap_or_default()
                            .chars()
                            .take(2000)
                            .collect();
                        let payload = json!({
                            "trigger": "error",
                            "workflow": { "id": workflow_id, "name": workflow_name },
                            "run_id": run_id,
                            "failed_node": culprit.map(|f| json!({
                                "id": f.node_id,
                                "name": f.node_name,
                                "type": f.node_type,
                            })),
                            "error": err_str,
                            "trigger_type": trigger_source,
                            "ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                        });
                        // The payload rides the spawn call and is staged keyed by
                        // the child's run id, so a failed spawn stages nothing.
                        match Self::run_in_background_with_payload(
                            &error_wf_id,
                            state,
                            "error",
                            None,
                            Some(payload),
                        ) {
                            Ok(child) => tracing::info!(
                                "Workflow '{}' failed — spawned error workflow {} (run {})",
                                workflow_id,
                                error_wf_id,
                                child
                            ),
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to spawn error workflow {}: {}",
                                    error_wf_id,
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }

        Ok(WorkflowRunResult {
            run_id,
            workflow_id: workflow_id.to_string(),
            status: status.to_string(),
            node_results: results_vec,
            final_output,
            total_duration_ms: total_ms,
        })
    }

    /// Spawn a workflow run (or single-node run) in the background and return
    /// the run_id immediately.  The HTTP handler can respond right away so the
    /// frontend can start polling while the backend is still executing, giving
    /// truly live edge/node animations instead of a post-run replay.
    ///
    /// Usage in route handlers:
    ///
    ///   // Full workflow run
    ///   let run_id = Workflow::run_in_background(&wf_id, &state, None).await?;
    ///   return Json(json!({ "ok": true, "run_id": run_id }));
    ///
    ///   // Single-node run (play button / Execute Step)
    ///   let run_id = Workflow::run_in_background(&wf_id, &state, Some(node_id)).await?;
    ///   return Json(json!({ "ok": true, "run_id": run_id }));
    pub fn run_in_background(
        workflow_id: &str,
        state: &AppState,
        target_node_id: Option<String>,
    ) -> anyhow::Result<String> {
        Self::run_in_background_inner(
            workflow_id,
            state,
            "manual",
            target_node_id,
            false,
            None,
            None,
        )
    }

    /// Like `run_in_background` but tags the run with a specific trigger source
    /// (e.g. "telegram") so the engine starts ONLY from trigger nodes of that
    /// type — isolating trigger branches in a multi-trigger workflow.
    pub fn run_in_background_with_source(
        workflow_id: &str,
        state: &AppState,
        trigger_source: &str,
        target_node_id: Option<String>,
    ) -> anyhow::Result<String> {
        Self::run_in_background_inner(
            workflow_id,
            state,
            trigger_source,
            target_node_id,
            false,
            None,
            None,
        )
    }

    /// Like `run_in_background_with_source` but also carries the trigger's
    /// payload (webhook body, Telegram event, error description, …). The
    /// payload is staged keyed by the new RUN id before the task spawns and
    /// consumed by the run's trigger node — so concurrent fires of the same
    /// workflow each see exactly their own event.
    pub fn run_in_background_with_payload(
        workflow_id: &str,
        state: &AppState,
        trigger_source: &str,
        target_node_id: Option<String>,
        trigger_payload: Option<Value>,
    ) -> anyhow::Result<String> {
        Self::run_in_background_inner(
            workflow_id,
            state,
            trigger_source,
            target_node_id,
            false,
            None,
            trigger_payload,
        )
    }

    /// Manual "Run" (play button) on a single Stimulus/trigger node: start a full
    /// downstream run but from ONLY that entry node, leaving sibling triggers (and
    /// their branches) dormant. Distinct from `run_node_in_background`, which runs
    /// a node's *ancestors* up to it; here `node_id` is an entry point with no
    /// ancestors, so we pin it as the sole start node and let its whole chain run.
    pub fn run_from_entry_node(
        workflow_id: &str,
        state: &AppState,
        node_id: String,
    ) -> anyhow::Result<String> {
        Self::run_in_background_inner(
            workflow_id,
            state,
            "manual",
            None,
            false,
            Some(node_id),
            None,
        )
    }

    /// Single-node variant of `run_in_background`: when `single_node` is true,
    /// run ONLY `node_id` using cached upstream results from the previous run
    /// (the "Execute Step" button once upstream nodes already have data) instead
    /// of re-running its ancestors. Falls back to a full ancestor run if any
    /// immediate upstream node has no cached result.
    pub fn run_node_in_background(
        workflow_id: &str,
        state: &AppState,
        node_id: String,
        single_node: bool,
    ) -> anyhow::Result<String> {
        Self::run_in_background_inner(
            workflow_id,
            state,
            "manual",
            Some(node_id),
            single_node,
            None,
            None,
        )
    }

    fn run_in_background_inner(
        workflow_id: &str,
        state: &AppState,
        trigger_source: &str,
        target_node_id: Option<String>,
        single_node: bool,
        entry_node_id: Option<String>,
        trigger_payload: Option<Value>,
    ) -> anyhow::Result<String> {
        let run_id = uuid::Uuid::new_v4().to_string();

        // Pre-create the run record as 'running' so the very first frontend poll
        // can find it immediately, even before the spawned task starts executing.
        // This prevents the poll from seeing the previous run and incorrectly
        // setting backendDone=true before our new run appears.
        {
            let conn = state.db.get()?;
            conn.execute(
                "INSERT INTO workflow_runs (id, workflow_id, status, started_at, node_results) \
                 VALUES (?1, ?2, 'running', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), '[]') \
                 ON CONFLICT(id) DO NOTHING",
                [run_id.clone(), workflow_id.to_string()],
            )?;
        }

        // Stage the run's entry-node pin (manual play button on a Stimulus) and
        // trigger payload, both keyed by run_id and consumed inside the run.
        // Staged AFTER the row insert (so an insert failure can't leak them) and
        // BEFORE spawning (so the run task always sees them).
        if let Some(node_id) = entry_node_id {
            trigger_data::stage_entry_node(&run_id, node_id);
        }
        if let Some(payload) = trigger_payload {
            trigger_data::stage(&run_id, payload);
        }

        let s = state.clone();
        let wf_id = workflow_id.to_string();
        let rid = run_id.clone();
        let src = trigger_source.to_string();

        tokio::spawn(async move {
            // B3: acquire a concurrency slot before executing. Held for the run's
            // duration; released on completion or durable-wait suspend (task end).
            // A full queue sheds the run rather than piling up unbounded tasks.
            let Some(_slot) = acquire_run_slot(&s).await else {
                tracing::warn!(
                    "Workflow run {} shed: run queue full (workflow.max_queue_depth)",
                    rid
                );
                // The run task never reaches `run_inner`, so drop whatever it
                // staged (entry pin / trigger payload, keyed by this run_id).
                trigger_data::discard(&rid);
                if let Ok(conn) = s.db.get() {
                    // Record *why* it failed so run history (and the Telegram
                    // report) show a reason instead of an empty failed run.
                    let reason = serde_json::json!([{
                        "node_id": "__queue__",
                        "node_name": "Concurrency queue",
                        "node_type": "system",
                        "position": 0,
                        "status": "error",
                        "output": Value::Null,
                        "duration_ms": 0,
                        "error": "Run shed: concurrency queue full — raise workflow.max_concurrent_runs / workflow.max_queue_depth",
                    }])
                    .to_string();
                    persist_run_update(
                        &conn,
                        "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), node_results = ?2 WHERE id = ?1",
                        rusqlite::params![&rid, reason],
                        &rid,
                        "shed status",
                    );
                }
                return;
            };

            // Pass the pre-created run_id so run_with_trigger reuses it rather
            // than inserting a duplicate record.
            if let Err(e) = Self::run_with_trigger(
                &wf_id,
                &s,
                &src,
                target_node_id,
                single_node,
                Some(rid.clone()),
            )
            .await
            {
                tracing::error!("Background workflow run failed: {}", e);
                if let Ok(conn) = s.db.get() {
                    persist_run_update(
                        &conn,
                        "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                        [&rid],
                        &rid,
                        "failed status",
                    );
                }
                let details = format!("workflow_id={}\nrun_id={}\nerror={}", wf_id, rid, e);
                if let Err(notify_err) = send_global_error_notification(
                    &s,
                    "workflow.engine",
                    "Workflow runtime crashed before completion",
                    &details,
                    None,
                    None,
                )
                .await
                {
                    tracing::warn!(
                        "Background workflow crash notification failed: {}",
                        notify_err
                    );
                }
            }
        });

        tracing::info!(
            "Workflow '{}' spawned in background, run_id={}",
            workflow_id,
            run_id
        );
        Ok(run_id)
    }

    fn trigger_priority(trigger_type: &str) -> u8 {
        match trigger_type {
            "gmail" | "crm" => 3,
            "cron" | "watcher" => 2,
            _ => 1,
        }
    }

    fn is_workflow_run_active(state: &AppState, workflow_id: &str) -> bool {
        let Ok(conn) = state.db.get() else {
            return false;
        };

        // Ignore stale 'running' rows (e.g. process killed mid-run): without the
        // time bound a single orphaned row blocks scheduled triggers forever.
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM workflow_runs WHERE workflow_id = ?1 AND status = 'running' \
             AND started_at > strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-6 hours'))",
            rusqlite::params![workflow_id],
            |r| r.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .unwrap_or(false)
    }

    /// Re-enter any durably-suspended runs whose wake time has arrived. Called on
    /// each background tick; the first tick fires at startup, so waits that came
    /// due while the agent was restarting resume promptly.
    async fn resume_due_waiting_runs(state: &AppState) {
        // Read the due rows first, then claim each with a status-guarded UPDATE
        // ('waiting' -> 'running'), so a second tick racing us claims 0 rows and
        // never resumes the same run twice.
        let due: Vec<(String, String, String, String, String)> = {
            let Ok(conn) = state.db.get() else {
                return;
            };
            let Ok(mut stmt) = conn.prepare(
                "SELECT id, workflow_id, COALESCE(trigger_type, 'manual'), \
                        COALESCE(resume_node_id, ''), node_results \
                 FROM workflow_runs \
                 WHERE status = 'waiting' AND resume_at IS NOT NULL \
                   AND resume_at <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now') \
                 ORDER BY resume_at ASC LIMIT 50",
            ) else {
                return;
            };
            stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map(|i| i.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        };

        for (run_id, workflow_id, trigger_source, resume_node_id, results_json) in due {
            let claimed = {
                let Ok(conn) = state.db.get() else {
                    continue;
                };
                conn.execute(
                    "UPDATE workflow_runs SET status = 'running' WHERE id = ?1 AND status = 'waiting'",
                    [&run_id],
                )
                .unwrap_or(0)
            };
            if claimed != 1 {
                continue; // another tick already claimed it
            }

            let mut results: Vec<NodeResult> = match serde_json::from_str(&results_json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        "Resume {}: corrupt node_results ({}); failing run",
                        run_id,
                        e
                    );
                    if let Ok(conn) = state.db.get() {
                        persist_run_update(
                            &conn,
                            "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                            [&run_id],
                            &run_id,
                            "failed status",
                        );
                    }
                    continue;
                }
            };

            // B2: restore any offloaded payloads before the resumed run reads them.
            binary::rehydrate_results(&mut results);

            // The node we paused on is stored as 'waiting'; flip it to 'success'
            // now the run continues past it. A webhook/approval node (C1) that
            // reaches the time poller did so by TIMEOUT — a genuine resume claims
            // the run first — so route it down its timeout branch: approval →
            // reject (output 1), webhook → continue with a `timed_out` flag.
            for r in results.iter_mut() {
                if r.node_id == resume_node_id || r.status == "waiting" {
                    r.status = "success".to_string();
                }
                if r.node_id == resume_node_id {
                    let rmode = r
                        .output
                        .get("__axon_resume")
                        .and_then(|m| m.get("mode"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if let Some(rmode) = rmode {
                        if let Some(obj) = r.output.as_object_mut() {
                            obj.remove("__axon_resume");
                            obj.insert("timed_out".to_string(), json!(true));
                            obj.insert("outcome".to_string(), json!("timeout"));
                            if rmode == "approval" {
                                obj.insert("approved".to_string(), json!(false));
                                obj.insert("outputIndex".to_string(), json!(1));
                            }
                        }
                    }
                }
            }
            let completed: std::collections::HashSet<String> =
                results.iter().map(|r| r.node_id.clone()).collect();
            let resume = ResumeState { completed, results };

            let s = state.clone();
            let wf = workflow_id.clone();
            let src = trigger_source.clone();
            let rid = run_id.clone();
            tracing::info!("Resuming durably-suspended workflow run {}", run_id);
            tokio::spawn(async move {
                // B3: a resumed run re-acquires a concurrency slot (the original
                // was released when the run suspended). If the queue is full it
                // stays 'waiting' and a later tick retries — never shed mid-flight.
                let _slot = match acquire_run_slot(&s).await {
                    Some(slot) => slot,
                    None => {
                        tracing::warn!("Resume of {} deferred: run queue full", rid);
                        if let Ok(conn) = s.db.get() {
                            persist_run_update(
                                &conn,
                                "UPDATE workflow_runs SET status = 'waiting' WHERE id = ?1",
                                [&rid],
                                &rid,
                                "deferred-resume status",
                            );
                        }
                        return;
                    }
                };
                if let Err(e) =
                    Self::run_inner(&wf, &s, &src, None, false, Some(rid.clone()), Some(resume))
                        .await
                {
                    tracing::error!("Resumed workflow run {} failed: {}", rid, e);
                    if let Ok(conn) = s.db.get() {
                        persist_run_update(
                            &conn,
                            "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                            [&rid],
                            &rid,
                            "failed status",
                        );
                    }
                }
            });
        }
    }

    /// C1: resume a run durably suspended at a Wait-for-webhook / Approval node,
    /// driven by an external hit on its
    /// `/webhook/{resume,approve,reject}/<node_id>/<run_id>` URL. The node id
    /// addresses the parked node; the run id (an unguessable UUIDv4) scopes the
    /// wake to exactly one run. `outcome` is one of `"resumed"` | `"approved"` |
    /// `"rejected"`; `payload` is the request body (attached to the resumed node
    /// so downstream nodes read it as `$json`). The run continues in the
    /// background; this returns once the resume is committed.
    pub async fn resume_by_node(
        state: &AppState,
        node_id: &str,
        run_id: &str,
        outcome: &str,
        payload: Value,
    ) -> Result<Value, String> {
        let node_id = node_id.to_string();
        let run_id = run_id.to_string();

        // 1. Claim the run: it must still be 'waiting' AND parked on exactly this
        //    node. Verifying node+run together means a link for one node/run can
        //    never resume a different one; the atomic UPDATE makes the claim
        //    single-winner so two racing clicks can't both wake it. A timed-out
        //    run is no longer 'waiting' (the poller already advanced it), so an
        //    expired link fails here naturally — no separate expiry check needed.
        let (workflow_id, trigger_source, results_json): (String, String, String) = {
            let conn = state.db.get().map_err(|e| e.to_string())?;
            conn.query_row(
                "SELECT workflow_id, COALESCE(trigger_type, 'manual'), node_results \
                 FROM workflow_runs \
                 WHERE id = ?1 AND resume_node_id = ?2 AND status = 'waiting'",
                rusqlite::params![run_id, node_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|_| {
                "no run is waiting at this step — it may have already been resumed, \
                 timed out, finished, or the link is for a different node or run"
                    .to_string()
            })?
        };
        let claimed = {
            let conn = state.db.get().map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE workflow_runs SET status = 'running' WHERE id = ?1 AND status = 'waiting'",
                [&run_id],
            )
            .map_err(|e| e.to_string())?
        };
        if claimed != 1 {
            return Err("run already resumed, finished, or cancelled".to_string());
        }

        // 2. Rebuild the chain and patch the resumed node with the payload +
        //    decision so downstream nodes see it and approval branches route.
        let mut results: Vec<NodeResult> = serde_json::from_str(&results_json).map_err(|e| {
            if let Ok(conn) = state.db.get() {
                persist_run_update(
                    &conn,
                    "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                    [&run_id],
                    &run_id,
                    "failed status",
                );
            }
            format!("corrupt node_results on resume: {e}")
        })?;
        binary::rehydrate_results(&mut results);

        // 3. Recover the suspend mode (webhook vs approval) from what the node
        //    itself recorded at suspend, so approve/reject routing is authoritative
        //    without a second query.
        let mode = results
            .iter()
            .find(|r| r.node_id == node_id)
            .and_then(|r| r.output.get("__axon_resume"))
            .and_then(|m| m.get("mode"))
            .and_then(|v| v.as_str())
            .unwrap_or("webhook")
            .to_string();

        let approved = mode == "approval" && outcome != "rejected";
        let now_iso = chrono::Utc::now().to_rfc3339();
        for r in results.iter_mut() {
            if r.node_id == node_id || r.status == "waiting" {
                r.status = "success".to_string();
            }
            if r.node_id == node_id {
                if let Some(obj) = r.output.as_object_mut() {
                    obj.remove("__axon_resume");
                    // Spread an object body so `$json.<field>` works, then stamp
                    // the reserved decision keys (they win over body fields).
                    if let Some(body) = payload.as_object() {
                        for (k, v) in body {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                    obj.insert("data".to_string(), payload.clone());
                    obj.insert("resumed".to_string(), json!(true));
                    // Clear the stale suspend-time flag: the run is no longer
                    // parked, so `$json.waiting` must read false downstream.
                    obj.insert("waiting".to_string(), json!(false));
                    obj.insert("outcome".to_string(), json!(outcome));
                    obj.insert("resumed_at".to_string(), json!(now_iso));
                    if mode == "approval" {
                        obj.insert("approved".to_string(), json!(approved));
                        obj.insert(
                            "outputIndex".to_string(),
                            json!(if approved { 0 } else { 1 }),
                        );
                    }
                }
            }
        }

        let completed: std::collections::HashSet<String> =
            results.iter().map(|r| r.node_id.clone()).collect();
        // Serialized patched chain, used only to re-park if the queue is full:
        // the run was already claimed off 'waiting', so a wait-forever run must
        // fall back to the time poller (which reads node_results from the DB) to
        // retry once a slot frees up.
        let patched_json = serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string());
        let resume = ResumeState { completed, results };

        let s = state.clone();
        let wf = workflow_id.clone();
        let src = trigger_source.clone();
        let rid = run_id.clone();
        tracing::info!(
            "Resuming run {} ({} mode, outcome={})",
            run_id,
            mode,
            outcome
        );
        tokio::spawn(async move {
            let _slot = match acquire_run_slot(&s).await {
                Some(slot) => slot,
                None => {
                    // Queue full: persist the patched chain and re-park on a
                    // now-deadline so the time poller retries — the consumed token
                    // can't, and a wait-forever run would otherwise stick.
                    tracing::warn!("Resume of {} deferred: run queue full", rid);
                    if let Ok(conn) = s.db.get() {
                        persist_run_update(
                            &conn,
                            "UPDATE workflow_runs SET status = 'waiting', node_results = ?1, \
                             resume_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?2",
                            rusqlite::params![patched_json, rid],
                            &rid,
                            "deferred-resume state",
                        );
                    }
                    return;
                }
            };
            if let Err(e) =
                Self::run_inner(&wf, &s, &src, None, false, Some(rid.clone()), Some(resume)).await
            {
                tracing::error!("Resumed workflow run {} failed: {}", rid, e);
                if let Ok(conn) = s.db.get() {
                    persist_run_update(
                        &conn,
                        "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                        [&rid],
                        &rid,
                        "failed status",
                    );
                }
            }
        });

        Ok(json!({
            "ok": true,
            "run_id": run_id,
            "workflow_id": workflow_id,
            "node_id": node_id,
            "outcome": outcome,
            "approved": approved,
        }))
    }

    pub async fn start_background_loop(state: AppState) {
        let state = std::sync::Arc::new(state);
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        tracing::info!("Workflow background loop started (60s interval)");
        loop {
            interval.tick().await;

            // Durable Wait: wake any runs whose suspend deadline has passed
            // (including ones that came due while the agent was restarting).
            Self::resume_due_waiting_runs(&state).await;

            let workflows = {
                let Ok(conn) = state.db.get() else {
                    tracing::warn!("Workflow loop: failed to get DB connection");
                    continue;
                };
                // JOIN with workflow_nodes to detect cron triggers stored in circadian/trigger node configs,
                // not just the workflow-level trigger_type (which may be stale or incorrectly set to 'manual')
                conn.prepare(
                    "SELECT id, name, trigger_type, trigger_config, last_run_at FROM (
                        SELECT DISTINCT w.id, w.name,
                            COALESCE(
                                json_extract(wn.config, '$.type'),
                                CASE WHEN wn.node_type IN ('circadian', 'stimulus') THEN 'cron' END,
                                w.trigger_type
                            ) as trigger_type,
                            COALESCE(wn.config, w.trigger_config) as trigger_config,
                            w.last_run_at
                         FROM workflows w
                         LEFT JOIN workflow_nodes wn ON wn.workflow_id = w.id AND wn.node_type IN ('trigger', 'circadian', 'stimulus')
                         WHERE w.enabled = 1
                    ) WHERE trigger_type IN ('cron', 'watcher', 'gmail', 'crm')"
                )
                    .and_then(|mut s| s.query_map([], |r| Ok(Workflow {
                        id: r.get(0)?, name: r.get(1)?, description: String::new(), enabled: true,
                        trigger_type: r.get::<_, String>(2)?, trigger_config: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or(json!({})),
                        last_run_at: r.get(4)?, last_status: String::new(), created_at: String::new(),
                        nodes: vec![], edges: vec![],
                    })).map(|i| i.filter_map(|r| r.ok()).collect::<Vec<_>>())).unwrap_or_default()
            };

            // The SQL join can return duplicate rows per workflow when multiple
            // trigger-like nodes exist. Keep one row per workflow id.
            let mut deduped: std::collections::HashMap<String, Workflow> =
                std::collections::HashMap::new();
            for wf in workflows {
                match deduped.entry(wf.id.clone()) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(wf);
                    }
                    std::collections::hash_map::Entry::Occupied(mut existing) => {
                        if Self::trigger_priority(&wf.trigger_type)
                            > Self::trigger_priority(&existing.get().trigger_type)
                        {
                            existing.insert(wf);
                        }
                    }
                }
            }
            let workflows: Vec<Workflow> = deduped.into_values().collect();

            if !workflows.is_empty() {
                tracing::info!(
                    "Workflow loop: found {} cron/watcher workflow(s): {}",
                    workflows.len(),
                    workflows
                        .iter()
                        .map(|w| format!("{}({})", w.name, w.trigger_type))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            for wf in workflows {
                if wf.trigger_type == "gmail" {
                    // Gmail trigger: poll-first watcher pattern
                    // Only trigger if there are genuinely NEW emails since last check
                    if !should_trigger(&wf, state.settings.agent_utc_offset_hours()) {
                        continue;
                    }
                    if Self::is_workflow_run_active(state.as_ref(), &wf.id) {
                        tracing::info!(
                            "Workflow '{}' ({}) already running; skip duplicate gmail trigger",
                            wf.name,
                            wf.id
                        );
                        continue;
                    }

                    let s = state.clone();
                    let wf_id = wf.id.clone();
                    let wf_name = wf.name.clone();
                    tokio::spawn(async move {
                        match check_and_trigger_gmail(&wf_id, &wf_name, &wf.trigger_config, &s)
                            .await
                        {
                            Ok(true) => tracing::info!(
                                "Gmail trigger '{}': new emails found, workflow triggered",
                                wf_name
                            ),
                            Ok(false) => {
                                tracing::debug!("Gmail trigger '{}': no new emails", wf_name)
                            }
                            Err(e) => tracing::warn!("Gmail trigger '{}' failed: {}", wf_name, e),
                        }
                    });
                } else if wf.trigger_type == "crm" {
                    // CRM trigger: same poll-first watcher pattern as Gmail, but
                    // over the crm_changes_since feed with a stored cursor.
                    if !should_trigger(&wf, state.settings.agent_utc_offset_hours()) {
                        continue;
                    }
                    if Self::is_workflow_run_active(state.as_ref(), &wf.id) {
                        tracing::info!(
                            "Workflow '{}' ({}) already running; skip duplicate CRM trigger",
                            wf.name,
                            wf.id
                        );
                        continue;
                    }

                    let s = state.clone();
                    let wf_id = wf.id.clone();
                    let wf_name = wf.name.clone();
                    tokio::spawn(async move {
                        match check_and_trigger_crm(&wf_id, &wf_name, &wf.trigger_config, &s).await
                        {
                            Ok(true) => tracing::info!(
                                "CRM trigger '{}': changes found, workflow triggered",
                                wf_name
                            ),
                            Ok(false) => {
                                tracing::debug!("CRM trigger '{}': no changes", wf_name)
                            }
                            Err(e) => tracing::warn!("CRM trigger '{}' failed: {}", wf_name, e),
                        }
                    });
                } else {
                    let triggered = should_trigger(&wf, state.settings.agent_utc_offset_hours());
                    if triggered {
                        if Self::is_workflow_run_active(state.as_ref(), &wf.id) {
                            tracing::info!(
                                "Workflow '{}' ({}) already running; skip duplicate scheduled trigger",
                                wf.name,
                                wf.id
                            );
                            continue;
                        }
                        tracing::info!(
                            "Workflow '{}' ({}) → TRIGGERED, spawning run",
                            wf.name,
                            wf.id
                        );
                        // B3: route scheduled/watcher fires through the bounded
                        // background spawner so they honor the run-concurrency cap
                        // (raw run_with_trigger bypassed the semaphore). The run row
                        // is pre-created, so the is_workflow_run_active guard above
                        // still de-dupes while a run waits for a slot.
                        if let Err(e) = Self::run_in_background_with_source(
                            &wf.id,
                            state.as_ref(),
                            &wf.trigger_type,
                            None,
                        ) {
                            tracing::warn!("Scheduled run of '{}' failed to spawn: {}", wf.name, e);
                        }
                    }
                }
            }
        }
    }
}

/// Gmail watcher: polls for new emails, compares against stored seen IDs,
/// and only triggers the workflow when genuinely new messages arrive.
/// Stores new email data so the stimulus node can inject it as trigger output.
/// C2: record an event-sourced trigger's idempotency key and report whether it
/// was already seen. Returns `true` when the `(source, event_key)` pair already
/// existed (the caller should skip firing). An empty key, or a DB error, returns
/// `false` (fail-open: never drop a real event because dedup itself failed).
pub fn trigger_dedup_seen(state: &AppState, source: &str, event_key: &str) -> bool {
    if event_key.is_empty() {
        return false;
    }
    let Ok(conn) = state.db.get() else {
        return false;
    };
    match conn.execute(
        "INSERT OR IGNORE INTO trigger_dedup (source, event_key) VALUES (?1, ?2)",
        rusqlite::params![source, event_key],
    ) {
        Ok(0) => true,  // row already present → duplicate event
        Ok(_) => false, // freshly inserted → first time seen
        Err(e) => {
            tracing::warn!("trigger_dedup insert failed ({source}): {e}");
            false
        }
    }
}

async fn check_and_trigger_gmail(
    workflow_id: &str,
    workflow_name: &str,
    trigger_config: &Value,
    state: &AppState,
) -> Result<bool, String> {
    let label = trigger_config
        .get("gmail_label")
        .and_then(|v| v.as_str())
        .unwrap_or("INBOX");
    let max_results = trigger_config
        .get("gmail_max_results")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(10);

    let query = gmail_trigger_query(trigger_config);
    let args = json!({
        "query": query,
        "max_results": max_results,
    });

    // Use ToolRegistry::run() — same proven path as the watcher engine
    let data = state
        .tools
        .run("gmail_list", args)
        .await
        .map_err(|e| e.to_string())?;

    // Extract email entries from the response
    let emails = data
        .as_array()
        .or_else(|| data.get("messages").and_then(|v| v.as_array()))
        .or_else(|| data.get("emails").and_then(|v| v.as_array()))
        .cloned()
        .unwrap_or_default();

    // Extract all current message IDs
    let current_ids: Vec<String> = emails
        .iter()
        .filter_map(|e| {
            e.get("id")
                .or_else(|| e.get("message_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    // Load previously seen IDs from the DB
    let seen_ids: Vec<String> = {
        let conn = state.db.get().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT json_extract(trigger_config, '$.gmail_last_seen_ids') FROM workflows WHERE id = ?1",
            rusqlite::params![workflow_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
    };

    let seen_set: std::collections::HashSet<&str> = seen_ids.iter().map(|s| s.as_str()).collect();

    // Find genuinely new emails (not in seen_ids)
    let new_emails: Vec<&Value> = emails
        .iter()
        .filter(|e| {
            e.get("id")
                .or_else(|| e.get("message_id"))
                .and_then(|v| v.as_str())
                .map(|id| !seen_set.contains(id))
                .unwrap_or(false)
        })
        .collect();

    // Update the seen IDs in the DB (keep last 200 to prevent unbounded growth)
    let mut updated_ids = seen_ids.clone();
    for id in &current_ids {
        if !updated_ids.contains(id) {
            updated_ids.push(id.clone());
        }
    }
    // Keep only the last 200 seen IDs
    if updated_ids.len() > 200 {
        updated_ids = updated_ids.split_off(updated_ids.len() - 200);
    }

    {
        let conn = state.db.get().map_err(|e| e.to_string())?;
        let ids_json = serde_json::to_string(&updated_ids).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "UPDATE workflows SET trigger_config = json_set(trigger_config, '$.gmail_last_seen_ids', json(?1)) WHERE id = ?2",
            rusqlite::params![ids_json, workflow_id],
        )
        .map_err(|e| e.to_string())?;
    }

    if new_emails.is_empty() {
        // First poll (no seen IDs stored yet): baseline — don't trigger
        if seen_ids.is_empty() && !current_ids.is_empty() {
            tracing::info!(
                "Gmail trigger '{}': first poll — stored {} baseline IDs (silent)",
                workflow_name,
                current_ids.len()
            );
        }
        return Ok(false);
    }

    tracing::info!(
        "Gmail trigger '{}': {} new email(s) detected (out of {} total)",
        workflow_name,
        new_emails.len(),
        emails.len()
    );

    // Enrich each new email with its full decoded + decomposed body (main text,
    // signature, quoted thread), parsed sender and richer attachments. The
    // "Download Attachments" toggle additionally saves every file locally and
    // attaches the paths. Best-effort; `new_emails` (used below for mark-as-read)
    // is left intact.
    let download = trigger_config
        .get("gmail_download_attachments")
        .and_then(|v| {
            v.as_bool()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(false);
    let new_owned: Vec<Value> = new_emails.iter().map(|e| (*e).clone()).collect();
    let enriched = enrich_gmail_emails(state, new_owned, download).await;
    let enriched_count = enriched.len();

    // Stage the new email data keyed by a pre-generated run id so THIS run's
    // execute_gmail_trigger picks it up (a concurrent run of the same workflow
    // can't consume it). run_inner's cleanup guard discards it if unconsumed.
    let run_id = uuid::Uuid::new_v4().to_string();
    trigger_data::stage(
        &run_id,
        json!({
            "trigger": "gmail",
            "label": label,
            "new_email_count": enriched_count,
            "emails": enriched,
        }),
    );

    // Trigger the workflow. B3: hold a concurrency slot across the inline run so
    // gmail-triggered runs honor the same cap as other triggers. The seen-ids were
    // already committed above, so a queue-full *shed* would silently drop these
    // emails — in that rare case fall back to running unbounded rather than losing
    // them. The mark-as-read below still runs afterward.
    let _slot = acquire_run_slot(state).await;
    if _slot.is_none() {
        tracing::warn!(
            "Gmail trigger '{}': run queue full; running unbounded to avoid dropping {} email(s)",
            workflow_name,
            enriched_count
        );
    }
    WorkflowEngine::run_with_trigger(workflow_id, state, "gmail", None, false, Some(run_id))
        .await
        .map_err(|e| e.to_string())?;

    // Mark as read if configured
    let mark_read = trigger_config
        .get("gmail_mark_read")
        .and_then(|v| {
            v.as_bool()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(false);
    if mark_read {
        let ids: Vec<&str> = new_emails
            .iter()
            .filter_map(|e| {
                e.get("id")
                    .or_else(|| e.get("message_id"))
                    .and_then(|v| v.as_str())
            })
            .collect();
        if !ids.is_empty() {
            if let Err(e) = state
                .tools
                .run("gmail_mark_read", json!({ "ids": ids }))
                .await
            {
                tracing::warn!(
                    "Gmail trigger '{}': mark-as-read failed: {}",
                    workflow_name,
                    e
                );
            }
        }
    }

    Ok(true)
}

// ── CRM Trigger (GHL-style automation) ────────────────────────────────────────

/// Decide which `crm_changes_since` rows fire a CRM trigger, and maintain the
/// deal_id → stage map used for "Deal Stage Changed" detection. Pure so it's
/// unit-testable: the caller owns loading/persisting the map.
///
/// Stage-change semantics: a deal whose stage differs from the recorded one
/// fires (with `previous_stage` attached); a deal not in the map yet — brand
/// new, or pre-existing but first seen by this trigger — is recorded silently
/// so its NEXT stage change fires. A non-stage edit never fires.
fn filter_crm_hits(
    event: &str,
    changes: &[Value],
    known_stages: &mut serde_json::Map<String, Value>,
) -> Vec<Value> {
    let mut hits = Vec::new();
    for ch in changes {
        let change_kind = ch.get("change").and_then(|v| v.as_str()).unwrap_or("");
        let entity_type = ch.get("entity_type").and_then(|v| v.as_str()).unwrap_or("");
        match event {
            "lead_created" => {
                if entity_type == "lead" && change_kind == "created" {
                    hits.push(ch.clone());
                }
            }
            "deal_created" => {
                if entity_type == "deal" && change_kind == "created" {
                    hits.push(ch.clone());
                }
            }
            "deal_stage_changed" => {
                if entity_type != "deal" {
                    continue;
                }
                let (Some(id), Some(stage)) = (
                    ch.get("id").and_then(Value::as_str),
                    ch.get("stage").and_then(Value::as_str),
                ) else {
                    continue;
                };
                if let Some(prev) = known_stages.get(id).and_then(Value::as_str) {
                    if prev != stage {
                        let mut hit = ch.clone();
                        if let Value::Object(ref mut map) = hit {
                            map.insert("previous_stage".to_owned(), Value::from(prev));
                        }
                        hits.push(hit);
                    }
                }
                known_stages.insert(id.to_owned(), Value::from(stage));
            }
            "any_change" => hits.push(ch.clone()),
            other => {
                tracing::warn!("CRM trigger: unknown crm_event '{}', not firing", other);
            }
        }
    }
    hits
}

/// CRM watcher: polls the `crm_changes_since` feed against a cursor stored in
/// `workflows.trigger_config` (`crm_cursor`, plus `crm_known_stages` for stage
/// detection — same home as `gmail_last_seen_ids`), and triggers the workflow
/// when matching changes arrive. The hits are staged keyed by the new RUN id
/// (per-RUN pattern, never workflow-id-keyed) for `execute_crm_trigger`.
async fn check_and_trigger_crm(
    workflow_id: &str,
    workflow_name: &str,
    trigger_config: &Value,
    state: &AppState,
) -> Result<bool, String> {
    let event = trigger_config
        .get("crm_event")
        .and_then(|v| v.as_str())
        .unwrap_or("lead_created");

    // Trigger state lives in the workflows row — the passed trigger_config may
    // be the (read-only snapshot of the) Stimulus node config.
    let (cursor, stages_json): (Option<String>, Option<String>) = {
        let conn = state.db.get().map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT json_extract(trigger_config, '$.crm_cursor'),
                    json_extract(trigger_config, '$.crm_known_stages')
             FROM workflows WHERE id = ?1",
            rusqlite::params![workflow_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?
    };
    let mut known_stages: serde_json::Map<String, Value> = stages_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let persist = |cursor: &str, stages: &serde_json::Map<String, Value>| -> Result<(), String> {
        let conn = state.db.get().map_err(|e| e.to_string())?;
        let stages_text =
            serde_json::to_string(&Value::Object(stages.clone())).map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE workflows SET trigger_config = json_set(COALESCE(trigger_config, '{}'),
                 '$.crm_cursor', ?1, '$.crm_known_stages', json(?2)) WHERE id = ?3",
            rusqlite::params![cursor, stages_text, workflow_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    };

    let entity_types = match event {
        "lead_created" => json!(["lead"]),
        "deal_created" | "deal_stage_changed" => json!(["deal"]),
        _ => json!(["lead", "deal", "org"]),
    };

    // First poll: baseline silently (like gmail's seen-ids baseline). Seed the
    // stage map so pre-existing deals have a known previous stage and their
    // first REAL change fires — without seeding it would be swallowed as
    // "unknown deal, record only".
    let Some(cursor) = cursor else {
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        if event == "deal_stage_changed" {
            match state.tools.run("crm_deal_list", json!({ "limit": 200 })).await {
                Ok(deals) => {
                    for d in deals
                        .get("deals")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                    {
                        if let (Some(id), Some(stage)) = (
                            d.get("id").and_then(Value::as_str),
                            d.get("stage").and_then(Value::as_str),
                        ) {
                            known_stages.insert(id.to_owned(), Value::from(stage));
                        }
                    }
                }
                Err(e) => tracing::warn!(
                    "CRM trigger '{}': stage-map seed failed ({}); pre-existing deals fire from their second change",
                    workflow_name,
                    e
                ),
            }
        }
        persist(&now, &known_stages)?;
        tracing::info!(
            "CRM trigger '{}': first poll — baseline cursor stored (silent)",
            workflow_name
        );
        return Ok(false);
    };

    let data = state
        .tools
        .run(
            "crm_changes_since",
            json!({ "since": cursor, "entity_types": entity_types, "limit": 200 }),
        )
        .await
        .map_err(|e| e.to_string())?;

    let changes = data
        .get("changes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let next_cursor = data
        .get("cursor")
        .and_then(|v| v.as_str())
        .unwrap_or(&cursor)
        .to_string();

    let hits = filter_crm_hits(event, &changes, &mut known_stages);

    // Bound the stage map so trigger_config can't grow without limit. Closed
    // (Won/Lost) deals stop changing, so they are evicted first; only if none
    // remain does an open deal go (serde_json::Map is key-ordered, so there is
    // no true "oldest" — an evicted open deal simply re-records on its next
    // edit and fires from the one after).
    while known_stages.len() > 1000 {
        let key = known_stages
            .iter()
            .find(|(_, stage)| matches!(stage.as_str(), Some("Won") | Some("Lost")))
            .map(|(id, _)| id.clone())
            .or_else(|| known_stages.keys().next().cloned());
        let Some(key) = key else {
            break;
        };
        known_stages.remove(&key);
    }

    // Advance the cursor BEFORE firing (same order as gmail's seen-ids commit):
    // a crash mid-run must not re-fire the same changes forever.
    persist(&next_cursor, &known_stages)?;

    if hits.is_empty() {
        return Ok(false);
    }

    tracing::info!(
        "CRM trigger '{}': {} matching change(s) for '{}' (out of {} in window)",
        workflow_name,
        hits.len(),
        event,
        changes.len()
    );

    // Stage the hits keyed by a pre-generated run id so THIS run's
    // execute_crm_trigger picks them up; run_inner's cleanup guard discards
    // them if the run dies before its trigger node.
    let run_id = uuid::Uuid::new_v4().to_string();
    trigger_data::stage(
        &run_id,
        json!({
            "trigger": "crm",
            "event": event,
            "change_count": hits.len(),
            "changes": hits,
        }),
    );

    // Same slot handling as gmail: the cursor is already committed, so a
    // queue-full shed would silently drop these changes — run unbounded instead.
    let _slot = acquire_run_slot(state).await;
    if _slot.is_none() {
        tracing::warn!(
            "CRM trigger '{}': run queue full; running unbounded to avoid dropping {} change(s)",
            workflow_name,
            hits.len()
        );
    }
    WorkflowEngine::run_with_trigger(workflow_id, state, "crm", None, false, Some(run_id))
        .await
        .map_err(|e| e.to_string())?;

    Ok(true)
}

/// CRM Stimulus executor. Background fires consume the payload staged by
/// `check_and_trigger_crm`; a manual "Execute Step" live-fetches recent
/// changes (last 24 h, widening to 30 days when quiet) so the user has real
/// rows to map fields against — same spirit as the Gmail manual fetch.
pub(crate) async fn execute_crm_trigger(
    config: &Value,
    state: &AppState,
    workflow_id: &str,
    run_id: &str,
) -> Result<Value, String> {
    if let Some(trigger_data) = trigger_data::take(run_id) {
        tracing::info!(
            "CRM trigger: using pre-fetched change data for workflow {}",
            workflow_id
        );
        return Ok(trigger_data);
    }

    let event = config
        .get("crm_event")
        .and_then(|v| v.as_str())
        .unwrap_or("lead_created");
    let entity_types = match event {
        "lead_created" => json!(["lead"]),
        "deal_created" | "deal_stage_changed" => json!(["deal"]),
        _ => json!(["lead", "deal", "org"]),
    };

    let mut changes: Vec<Value> = Vec::new();
    for hours in [24i64, 720] {
        let since = (chrono::Utc::now() - chrono::Duration::hours(hours))
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let data = state
            .tools
            .run(
                "crm_changes_since",
                json!({ "since": since, "entity_types": entity_types.clone(), "limit": 10 }),
            )
            .await
            .map_err(|e| format!("CRM trigger fetch failed: {}", e))?;
        changes = data
            .get("changes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !changes.is_empty() {
            break;
        }
    }

    // Approximate the production shape: creation events prefer freshly created
    // rows (fall back to whatever changed if none), and stage-change rows carry
    // an explicit null previous_stage — a manual fetch has no stage history.
    match event {
        "lead_created" | "deal_created" => {
            let created: Vec<Value> = changes
                .iter()
                .filter(|c| c.get("change").and_then(|v| v.as_str()) == Some("created"))
                .cloned()
                .collect();
            if !created.is_empty() {
                changes = created;
            }
        }
        "deal_stage_changed" => {
            for ch in &mut changes {
                if let Value::Object(ref mut map) = ch {
                    map.entry("previous_stage".to_owned())
                        .or_insert(Value::Null);
                }
            }
        }
        _ => {}
    }

    Ok(json!({
        "trigger": "crm",
        "event": event,
        "change_count": changes.len(),
        "changes": changes,
    }))
}

#[cfg(test)]
mod crm_trigger_tests {
    use super::filter_crm_hits;
    use serde_json::{json, Map, Value};

    fn deal(id: &str, stage: &str, change: &str) -> Value {
        json!({ "entity_type": "deal", "id": id, "stage": stage, "change": change })
    }

    fn lead(id: &str, change: &str) -> Value {
        json!({ "entity_type": "lead", "id": id, "change": change })
    }

    #[test]
    fn lead_created_fires_only_on_created_leads() {
        let mut stages = Map::new();
        let hits = filter_crm_hits(
            "lead_created",
            &[
                lead("l1", "created"),
                lead("l2", "updated"),
                deal("d1", "Won", "created"),
            ],
            &mut stages,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["id"], json!("l1"));
    }

    #[test]
    fn stage_change_fires_with_previous_stage() {
        let mut stages = Map::new();
        stages.insert("d1".into(), json!("Prospecting"));
        let hits = filter_crm_hits(
            "deal_stage_changed",
            &[deal("d1", "Won", "updated")],
            &mut stages,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["previous_stage"], json!("Prospecting"));
        assert_eq!(stages["d1"], json!("Won"), "map advances to the new stage");
    }

    #[test]
    fn non_stage_edit_does_not_fire() {
        let mut stages = Map::new();
        stages.insert("d1".into(), json!("Won"));
        // An edit that left the stage alone (e.g. notes) → updated row, same stage.
        let hits = filter_crm_hits(
            "deal_stage_changed",
            &[deal("d1", "Won", "updated")],
            &mut stages,
        );
        assert!(hits.is_empty());
    }

    #[test]
    fn unknown_deal_is_recorded_silently_then_fires_on_next_change() {
        let mut stages = Map::new();
        // First sighting (pre-existing deal, or a brand-new one): record only.
        assert!(filter_crm_hits(
            "deal_stage_changed",
            &[deal("d2", "Proposal", "updated")],
            &mut stages,
        )
        .is_empty());
        assert_eq!(stages["d2"], json!("Proposal"));
        // Its next stage change fires.
        let hits = filter_crm_hits(
            "deal_stage_changed",
            &[deal("d2", "Negotiation", "updated")],
            &mut stages,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["previous_stage"], json!("Proposal"));
    }

    #[test]
    fn deal_created_batch_keeps_every_new_deal() {
        let mut stages = Map::new();
        let hits = filter_crm_hits(
            "deal_created",
            &[
                deal("d1", "Prospecting", "created"),
                deal("d2", "Qualified", "created"),
                deal("d3", "Won", "updated"),
            ],
            &mut stages,
        );
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn any_change_fires_on_everything_and_unknown_event_never_fires() {
        let mut stages = Map::new();
        let rows = [lead("l1", "created"), deal("d1", "Won", "updated")];
        assert_eq!(filter_crm_hits("any_change", &rows, &mut stages).len(), 2);
        assert!(filter_crm_hits("no_such_event", &rows, &mut stages).is_empty());
    }
}

fn should_trigger(wf: &Workflow, utc_offset_hours: i32) -> bool {
    use std::str::FromStr;

    let now = chrono::Utc::now();

    // Robust last_run_at parsing: try RFC3339 first, then NaiveDateTime fallback
    let last_run_dt = wf
        .last_run_at
        .as_ref()
        .filter(|l| !l.trim().is_empty())
        .and_then(|l| {
            // Try RFC3339 first (e.g. "2026-04-16T20:50:39Z" or "2026-04-16T20:50:39+00:00")
            chrono::DateTime::parse_from_rfc3339(l)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .ok()
                .or_else(|| {
                    // Fallback: NaiveDateTime without timezone (assume UTC)
                    chrono::NaiveDateTime::parse_from_str(l, "%Y-%m-%dT%H:%M:%S")
                        .or_else(|_| chrono::NaiveDateTime::parse_from_str(l, "%Y-%m-%d %H:%M:%S"))
                        .ok()
                        .map(|naive| naive.and_utc())
                })
        })
        .unwrap_or_else(|| {
            tracing::debug!(
                "Workflow '{}': no valid last_run_at (raw={:?}), treating as never-run",
                wf.name,
                wf.last_run_at
            );
            now - chrono::Duration::days(365)
        });

    let schedules_val = wf.trigger_config.get("schedules").or_else(|| {
        wf.trigger_config
            .get("config")
            .and_then(|c| c.get("schedules"))
    });

    if let Some(schedules) = schedules_val
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
    {
        for s in schedules {
            let mut eval_cron = String::new();
            if let Some(mode) = s.get("mode").and_then(|v| v.as_str()) {
                let val = s
                    .get("value")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                    })
                    .unwrap_or(1)
                    .max(1);
                let dow = s.get("dayOfWeek").and_then(|v| v.as_str()).unwrap_or("MON");
                let hod = s
                    .get("hourOfDay")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                    })
                    .unwrap_or(9);
                let moh = s
                    .get("minuteOfHour")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
                    })
                    .unwrap_or(0);
                let custom = s
                    .get("customCron")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0 * * * * *");
                match mode {
                    "minutes" => eval_cron = format!("0 */{} * * * *", val),
                    "hours" => eval_cron = format!("0 {} */{} * * *", moh, val),
                    "days" => eval_cron = format!("0 {} {} */{} * *", moh, hod, val),
                    "weekly" => eval_cron = format!("0 {} {} * * {}", moh, hod, dow),
                    _ => eval_cron = custom.to_string(),
                }
                tracing::debug!(
                    "Workflow '{}': mode={} → local cron='{}'",
                    wf.name,
                    mode,
                    eval_cron
                );
            } else if let Some(cron_expr) = s.get("cron").and_then(|v| v.as_str()) {
                eval_cron = cron_expr.to_string();
            }

            if !eval_cron.is_empty() {
                // Convert operator-local cron to UTC
                let utc_cron =
                    crate::scheduler::engine::local_cron_to_utc(&eval_cron, utc_offset_hours);
                tracing::debug!(
                    "Workflow '{}': local='{}' → utc='{}'",
                    wf.name,
                    eval_cron,
                    utc_cron
                );
                match cron::Schedule::from_str(&utc_cron) {
                    Ok(schedule) => {
                        if let Some(next_fire) = schedule.after(&last_run_dt).next() {
                            let should = next_fire <= now;
                            tracing::info!(
                                "Workflow '{}': last_run={}, next_fire={}, now={}, trigger={}",
                                wf.name,
                                last_run_dt,
                                next_fire,
                                now,
                                should
                            );
                            if should {
                                return true;
                            }
                        } else {
                            tracing::warn!(
                                "Workflow '{}': cron '{}' produced no next fire time after {}",
                                wf.name,
                                utc_cron,
                                last_run_dt
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Workflow '{}': INVALID cron expression '{}' (from manila '{}'): {}",
                            wf.name,
                            utc_cron,
                            eval_cron,
                            e
                        );
                    }
                }
            }
        }
        // An explicit schedules array was provided. Whether it is empty, its
        // entries failed to produce a valid cron, or a valid cron simply isn't
        // due yet, do NOT fall back to legacy interval polling — that would
        // silently fire on a default interval the user never configured. A
        // cron-mode trigger with no due schedule is inactive this tick.
        return false;
    } else {
        tracing::debug!(
            "Workflow '{}': no schedules found in trigger_config, falling back to interval",
            wf.name
        );
    }

    // 2. Fallback to legacy interval-based polling
    let mut mins = if wf.trigger_type == "gmail" || wf.trigger_type == "crm" {
        // Gmail/CRM triggers use poll_interval from config (default 5 min)
        wf.trigger_config
            .get("poll_interval")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(5)
    } else if wf.trigger_type == "watcher" {
        15
    } else {
        60
    };

    if let Some(m) = wf
        .trigger_config
        .get("interval_mins")
        .and_then(|v| v.as_u64())
    {
        mins = m;
    }

    let elapsed = (now - last_run_dt).num_minutes();
    let should = elapsed >= mins as i64;
    tracing::info!(
        "Workflow '{}': interval fallback — elapsed={}min, threshold={}min, trigger={}",
        wf.name,
        elapsed,
        mins,
        should
    );
    should
}

#[cfg(test)]
mod retry_tests {
    use super::compute_retry_wait_ms as w;

    #[test]
    fn fixed_backoff_is_constant() {
        assert_eq!(w(100, 1, "fixed"), 100);
        assert_eq!(w(100, 3, "fixed"), 100);
        assert_eq!(w(250, 5, ""), 250); // empty backoff == fixed
    }

    #[test]
    fn exponential_backoff_doubles_per_attempt() {
        assert_eq!(w(100, 1, "exponential"), 100);
        assert_eq!(w(100, 2, "exponential"), 200);
        assert_eq!(w(100, 3, "exponential"), 400);
        assert_eq!(w(100, 4, "exponential"), 800);
    }

    #[test]
    fn wait_is_floored_at_one_ms() {
        // A 0ms config still yields a >=1ms (cancellation-checking) pause.
        assert_eq!(w(0, 1, "fixed"), 1);
        assert_eq!(w(0, 3, "exponential"), 4);
    }

    #[test]
    fn huge_attempt_count_saturates_without_overflow() {
        // Must not panic on overflow for a pathological attempt number.
        let _ = w(1_000_000, 100, "exponential");
        assert_eq!(w(u64::MAX, 5, "exponential"), u64::MAX);
    }
}

#[cfg(test)]
mod upstream_cache_tests {
    use super::{fold_prior_run_into_cache, NodeResult};
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};

    fn nr(node_id: &str, status: &str, output: Value) -> NodeResult {
        NodeResult {
            node_id: node_id.to_string(),
            node_name: node_id.to_string(),
            node_type: "test".to_string(),
            position: 0,
            status: status.to_string(),
            output,
            duration_ms: 0,
            error: if status == "error" {
                Some("boom".into())
            } else {
                None
            },
            attempts: 1,
        }
    }

    fn build(
        runs_newest_first: Vec<Vec<NodeResult>>,
        ids: &HashSet<&str>,
    ) -> (HashMap<String, NodeResult>, Vec<NodeResult>) {
        let mut node_results = HashMap::new();
        let mut prior_ordered = Vec::new();
        let mut seeded = false;
        for run in runs_newest_first {
            let complete =
                fold_prior_run_into_cache(&mut node_results, &mut prior_ordered, run, !seeded, ids);
            seeded = true;
            if complete {
                break;
            }
        }
        (node_results, prior_ordered)
    }

    // THE bug: the immediately-previous run was a partial Execute Step on an
    // unrelated node, so the newest finished run's node_results is just
    // `[mcp_gmail]` — the Telegram trigger is absent. Backfill from the older
    // full run must recover the trigger's real payload, so a following Execute
    // Step finds it cached and never re-runs the trigger under "manual".
    #[test]
    fn partial_newest_run_does_not_strand_trigger() {
        let ids: HashSet<&str> = ["trigger", "synapse", "telegram"].into_iter().collect();
        let real_payload = json!({ "message": "hi", "chat": { "id": 42 } });
        let (cache, prior) = build(
            vec![
                // newest: partial run on a since-removed/other-branch node
                vec![nr("mcp_gmail", "error", json!({}))],
                // older: the full run that captured the live Telegram payload
                vec![
                    nr("trigger", "success", real_payload.clone()),
                    nr("synapse", "success", json!({ "body": "<html/>" })),
                    nr("telegram", "success", json!({ "ok": true })),
                ],
            ],
            &ids,
        );
        // Trigger recovered with its REAL payload, not {"trigger":"manual"}.
        let t = cache.get("trigger").expect("trigger backfilled");
        assert_eq!(t.status, "success");
        assert_eq!(t.output, real_payload);
        // And it lands in prior_ordered so a merged single-node save re-persists it.
        assert!(prior.iter().any(|r| r.node_id == "trigger"));
    }

    // The newest run wins: a node present in the newest run is never overwritten
    // by an older run's value for the same node.
    #[test]
    fn newest_value_is_not_overwritten_by_older() {
        let ids: HashSet<&str> = ["trigger"].into_iter().collect();
        let (cache, _) = build(
            vec![
                vec![nr("trigger", "success", json!({ "v": "new" }))],
                vec![nr("trigger", "success", json!({ "v": "old" }))],
            ],
            &ids,
        );
        assert_eq!(cache["trigger"].output, json!({ "v": "new" }));
    }

    // Backfill is conservative: never resurrect an errored result, and never
    // revive a node that no longer exists in the current graph.
    #[test]
    fn backfill_skips_errors_and_deleted_nodes() {
        let ids: HashSet<&str> = ["trigger"].into_iter().collect();
        let (cache, _) = build(
            vec![
                vec![nr("other", "success", json!({}))], // newest, missing trigger
                vec![
                    nr("trigger", "error", json!({})), // errored -> not backfilled
                    nr("deleted", "success", json!({ "x": 1 })), // not in graph -> skipped
                ],
            ],
            &ids,
        );
        assert!(!cache.contains_key("trigger"));
        assert!(!cache.contains_key("deleted"));
    }

    // Healthy path: when the newest run already covers every current node, the
    // fold reports completion after one run so the caller stops parsing.
    #[test]
    fn complete_after_newest_when_all_present() {
        let ids: HashSet<&str> = ["a", "b"].into_iter().collect();
        let mut node_results = HashMap::new();
        let mut prior_ordered = Vec::new();
        let complete = fold_prior_run_into_cache(
            &mut node_results,
            &mut prior_ordered,
            vec![nr("a", "success", json!({})), nr("b", "success", json!({}))],
            true,
            &ids,
        );
        assert!(complete);
        assert_eq!(prior_ordered.len(), 2);
    }
}
