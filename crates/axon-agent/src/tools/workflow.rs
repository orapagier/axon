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

// Gmail trigger data: holds new email data between the background poll check
// and the actual workflow execution so the stimulus node can inject it.
pub(crate) static GMAIL_TRIGGER_DATA: once_cell::sync::Lazy<
    tokio::sync::Mutex<std::collections::HashMap<String, Value>>,
> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

pub(crate) static WHATSAPP_TRIGGER_DATA: once_cell::sync::Lazy<
    tokio::sync::Mutex<std::collections::HashMap<String, Value>>,
> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

pub(crate) static TELEGRAM_TRIGGER_DATA: once_cell::sync::Lazy<
    tokio::sync::Mutex<std::collections::HashMap<String, Value>>,
> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

pub(crate) static EXTERNAL_TRIGGER_DATA: once_cell::sync::Lazy<
    tokio::sync::Mutex<std::collections::HashMap<String, Value>>,
> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

// Sub-workflow input: the parent's payload handed to a child workflow's trigger
// node when it runs under trigger_source "subflow". Keyed by child workflow_id
// and consumed (removed) by the trigger node on first read.
pub(crate) static SUBFLOW_TRIGGER_DATA: once_cell::sync::Lazy<
    tokio::sync::Mutex<std::collections::HashMap<String, Value>>,
> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

// Error-trigger payload (A3): the structured failure description handed to an
// error workflow's trigger node when it runs under trigger_source "error".
// Keyed by the error workflow's id and consumed (removed) by the trigger node.
pub(crate) static ERROR_TRIGGER_DATA: once_cell::sync::Lazy<
    tokio::sync::Mutex<std::collections::HashMap<String, Value>>,
> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

// Sub-workflow entry trigger: the id of the single trigger node a parent chose to
// start a child from when that child has multiple triggers. Keyed by child
// workflow_id and consumed (removed) as the entry queue is built, so only that
// trigger's chain runs. Absent ⇒ start from every trigger (the default).
pub(crate) static SUBFLOW_ENTRY_NODE: once_cell::sync::Lazy<
    tokio::sync::Mutex<std::collections::HashMap<String, String>>,
> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

// Manual entry trigger: the id of the single Stimulus/trigger node the user
// clicked "Run" (play button) on, so a manual run starts ONLY from that node's
// chain instead of every trigger in the workflow. Keyed by RUN id (not workflow
// id) so concurrent manual runs never consume each other's pin, and consumed
// (removed) as the entry queue is built. Absent ⇒ start from every trigger.
// A plain std Mutex: it's set from the sync `run_in_background_inner` before the
// run task is spawned, and read (removed, never held across an await) in
// `run_inner`.
pub(crate) static MANUAL_ENTRY_NODE: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

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


// ── JS Execution Infrastructure ───────────────────────────────────────────────

thread_local! {
    static JS_LOG_BUFFER: std::cell::RefCell<Option<std::sync::Arc<std::sync::Mutex<Vec<String>>>>>
        = std::cell::RefCell::new(None);
}

struct JsLogGuard;
impl JsLogGuard {
    fn install(logs: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Self {
        JS_LOG_BUFFER.with(|b| *b.borrow_mut() = Some(logs));
        JsLogGuard
    }
}
impl Drop for JsLogGuard {
    fn drop(&mut self) {
        JS_LOG_BUFFER.with(|b| *b.borrow_mut() = None);
    }
}

fn js_value_to_json(val: &JsValue, context: &mut Context) -> Value {
    if val.is_null() || val.is_undefined() {
        Value::Null
    } else if let Some(b) = val.as_boolean() {
        json!(b)
    } else if let Some(n) = val.as_number() {
        if n.is_nan() || n.is_infinite() {
            Value::Null
        } else if n.fract() == 0.0 {
            json!(n as i64)
        } else {
            json!(n)
        }
    } else if let Some(s) = val.as_string() {
        json!(s.to_std_string_escaped())
    } else if let Some(obj) = val.as_object() {
        if obj.is_array() {
            let len = obj
                .get(JsString::from("length"), context)
                .ok()
                .and_then(|v| v.as_number())
                .unwrap_or(0.0) as usize;
            let mut arr = Vec::with_capacity(len);
            for i in 0..len {
                arr.push(js_value_to_json(
                    &obj.get(i, context).unwrap_or_default(),
                    context,
                ));
            }
            Value::Array(arr)
        } else {
            let keys = obj.own_property_keys(context).unwrap_or_default();
            let mut map = serde_json::Map::new();
            for key in keys {
                let key_str = match &key {
                    boa_engine::property::PropertyKey::String(s) => s.to_std_string_escaped(),
                    boa_engine::property::PropertyKey::Index(i) => i.get().to_string(),
                    _ => continue,
                };
                map.insert(
                    key_str,
                    js_value_to_json(&obj.get(key, context).unwrap_or_default(), context),
                );
            }
            Value::Object(map)
        }
    } else {
        Value::Null
    }
}

// ── D2: expression helper library ─────────────────────────────────────────────

/// Run a JMESPath query over a JSON value. Compile/search errors resolve to
/// `null` (never a run failure) with a debug log — matching the expression
/// library's forgiving contract.
fn eval_jmespath(data: &Value, expr: &str) -> Value {
    if expr.trim().is_empty() {
        return Value::Null;
    }
    let compiled = match jmespath::compile(expr) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("$jmespath compile error for '{expr}': {e}");
            return Value::Null;
        }
    };
    let data_str = serde_json::to_string(data).unwrap_or_else(|_| "null".to_string());
    let var = match jmespath::Variable::from_json(&data_str) {
        Ok(v) => v,
        Err(_) => return Value::Null,
    };
    match compiled.search(jmespath::Rcvar::new(var)) {
        Ok(result) => serde_json::to_value(&*result).unwrap_or(Value::Null),
        Err(e) => {
            tracing::debug!("$jmespath search error for '{expr}': {e}");
            Value::Null
        }
    }
}

/// Build the `$env` object exposed to expressions. Fail-closed: only names
/// explicitly listed in the `AXON_EXPR_ENV` allowlist (comma-separated) are
/// exposed, and `AXON_MASTER_KEY` is hard-blocked regardless. Returns a JS
/// object literal (e.g. `{"REGION":"us"}`).
fn expression_env_json() -> String {
    let whitelist = std::env::var("AXON_EXPR_ENV").unwrap_or_default();
    let mut map = serde_json::Map::new();
    for name in whitelist.split(',') {
        let name = name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("AXON_MASTER_KEY") {
            continue;
        }
        if let Ok(val) = std::env::var(name) {
            map.insert(name.to_string(), Value::String(val));
        }
    }
    serde_json::to_string(&Value::Object(map)).unwrap_or_else(|_| "{}".to_string())
}

/// Register the native `$jmespath(obj, expr)` helper on a Boa context. Shared by
/// the Code node and the inline `{{ }}` evaluator so both see the same helper.
fn register_expression_natives(context: &mut Context) {
    let _ = context.register_global_builtin_callable(
        JsString::from("$jmespath"),
        2,
        NativeFunction::from_copy_closure(|_this, args, ctx| {
            let obj = args.first().cloned().unwrap_or(JsValue::Null);
            let expr = args
                .get(1)
                .and_then(|v| v.as_string())
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_default();
            let data_json = js_value_to_json(&obj, ctx);
            let result_json = eval_jmespath(&data_json, &expr);
            Ok(boa_engine::JsValue::from_json(&result_json, ctx).unwrap_or(JsValue::Null))
        }),
    );
}

/// Strip `{{ expression }}` wrappers from a JS script so that dragged-in
/// expressions become plain JavaScript references.
/// E.g. `const item = {{ $node["Gmail"].data }};`
///   →  `const item = $node["Gmail"].data;`
fn strip_expression_wrappers(script: &str) -> String {
    // (?s) so expressions spanning multiple lines are also unwrapped
    static RE: once_cell::sync::Lazy<Regex> =
        once_cell::sync::Lazy::new(|| Regex::new(r"(?s)\{\{\s*(.+?)\s*\}\}").unwrap());
    RE.replace_all(script, "$1").to_string()
}

async fn execute_js_node(
    raw_script: &str,
    node: &WorkflowNode,
    results: &[NodeResult],
    workflow_id: &str,
    run_id: &str,
) -> Result<Value, String> {
    if raw_script.is_empty() {
        return Err("No script specified".to_string());
    }
    if raw_script.len() > JS_SCRIPT_MAX_BYTES {
        return Err("Script too large".to_string());
    }

    // Strip {{ }} wrappers so dragged expressions become valid JS.
    // $node is injected as a native JS variable, so $node["Name"].data works.
    let script = strip_expression_wrappers(raw_script);

    let results_copy = results.to_vec();
    let node_id = node.id.clone();
    let node_name = node.name.clone();
    let wf_id = workflow_id.to_string();
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let logs_for_thread = logs.clone();

    let task = tokio::task::spawn_blocking(move || {
        let _guard = JsLogGuard::install(logs_for_thread);
        let mut context = Context::default();

        // Hard interpreter limits: tokio::time::timeout only abandons the
        // blocking task — it cannot stop boa. Without these, an infinite
        // loop in a user script leaks a blocking thread forever.
        context.runtime_limits_mut().set_loop_iteration_limit(5_000_000);
        context.runtime_limits_mut().set_recursion_limit(512);

        // ── console.log / print ───────────────────────────────────────────
        context
            .register_global_builtin_callable(
                JsString::from("print"),
                1,
                NativeFunction::from_copy_closure(|_this, args, _ctx| {
                    let msg = args.first().cloned().unwrap_or_default();
                    JS_LOG_BUFFER.with(|b| {
                        if let Some(logs) = b.borrow().as_ref() {
                            if let Ok(mut lock) = logs.lock() {
                                if lock.len() < JS_LOG_MAX_LINES {
                                    lock.push(msg.display().to_string());
                                }
                            }
                        }
                    });
                    Ok(JsValue::undefined())
                }),
            )
            .map_err(|e| e.to_string())?;

        context
            .eval(Source::from_bytes(
                b"var console = { log: print, warn: print, error: print, info: print, debug: print };",
            ))
            .map_err(|e| e.to_string())?;

        // ── D2: native expression helpers ($jmespath) ─────────────────────
        register_expression_natives(&mut context);

        // ── $results (ordered array of all previous results) ──────────────
        let results_json =
            serde_json::to_string(&results_copy).unwrap_or_else(|_| "[]".to_string());
        context
            .eval(Source::from_bytes(
                format!("var $results = {};", results_json).as_bytes(),
            ))
            .map_err(|e| e.to_string())?;

        // ── $node map: $node["NodeName"].data.field ───────────────────────
        let mut nodes_map = serde_json::Map::new();
        for r in &results_copy {
            let mut node_obj = serde_json::Map::new();
            node_obj.insert("output".to_string(), r.output.clone());
            node_obj.insert("data".to_string(), r.output.clone());
            node_obj.insert("json".to_string(), r.output.clone());
            node_obj.insert("error".to_string(), serde_json::json!(r.error));
            node_obj.insert("name".to_string(), serde_json::json!(r.node_name));
            node_obj.insert("id".to_string(), serde_json::json!(r.node_id));
            node_obj.insert("type".to_string(), serde_json::json!(r.node_type));
            let val = serde_json::Value::Object(node_obj);
            // Index by both ID and name (case-insensitive alias too)
            nodes_map.insert(r.node_id.clone(), val.clone());
            nodes_map.insert(r.node_name.clone(), val.clone());
            // Lowercase alias so users don't have to worry about case
            let lower = r.node_name.to_lowercase();
            if lower != r.node_name {
                nodes_map.entry(lower).or_insert(val);
            }
        }
        let node_json = serde_json::to_string(&serde_json::Value::Object(nodes_map))
            .unwrap_or_else(|_| "{}".to_string());
        context
            .eval(Source::from_bytes(
                format!("var $node = {};", node_json).as_bytes(),
            ))
            .map_err(|e| e.to_string())?;

        // ── n8n-style convenience helpers ─────────────────────────────────
        // $input  — the most recent predecessor node's output (what n8n calls $input.first().json)
        // $json   — alias for $input
        // $prevNode — metadata about the previous node
        // $items  — array of all previous node outputs
        // $now    — current ISO timestamp
        // $today  — current date YYYY-MM-DD
        // $execution — workflow context
        // $nodeId / $nodeName — current JS node identity
        let prev = results_copy.last();
        let input_json = prev
            .map(|r| serde_json::to_string(&r.output).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "{}".to_string());
        let prev_node_json = prev
            .map(|r| {
                serde_json::to_string(&serde_json::json!({
                    "name": r.node_name,
                    "id": r.node_id,
                    "type": r.node_type,
                    "output": r.output,
                    "data": r.output,
                    "json": r.output,
                }))
                .unwrap_or_else(|_| "{}".to_string())
            })
            .unwrap_or_else(|| "{}".to_string());

        // $items: array of {json, name, id, type} for all preceding nodes
        let items_arr: Vec<Value> = results_copy
            .iter()
            .map(|r| {
                serde_json::json!({
                    "json": r.output,
                    "data": r.output,
                    "name": r.node_name,
                    "id": r.node_id,
                    "type": r.node_type,
                })
            })
            .collect();
        let items_json = serde_json::to_string(&items_arr).unwrap_or_else(|_| "[]".to_string());

        let now = chrono::Utc::now();
        let helpers = format!(
            r#"
var $input = {input};
var $json = $input;
var $prevNode = {prev_node};
var $items = {items};
var $now = "{now_iso}";
var $today = "{today}";
var $execution = {{ "workflowId": "{wf_id}", "runId": "{run_id}" }};
var $workflow = {{ "id": "{wf_id}" }};
var $nodeId = "{node_id}";
var $nodeName = "{node_name}";
var $env = {env};
"#,
            input = input_json,
            prev_node = prev_node_json,
            items = items_json,
            now_iso = now.to_rfc3339(),
            today = now.format("%Y-%m-%d"),
            wf_id = wf_id,
            run_id = run_id,
            node_id = node_id,
            node_name = node_name,
            env = expression_env_json(),
        );
        context
            .eval(Source::from_bytes(helpers.as_bytes()))
            .map_err(|e| format!("Helper injection error: {}", e))?;

        // ── Execute the user script ───────────────────────────────────────
        let wrapped = format!("(function() {{\n{}\n}})()", script);
        match context.eval(Source::from_bytes(wrapped.as_bytes())) {
            Ok(res) => Ok(js_value_to_json(&res, &mut context)),
            Err(e) => {
                let err_str = e.to_string();
                // Include the first few lines of the processed script to help debug
                let preview: String = script.lines().take(5).collect::<Vec<_>>().join("\n");
                Err(format!(
                    "JS Error: {}\n--- Script preview ---\n{}",
                    err_str, preview
                ))
            }
        }
    });

    match tokio::time::timeout(JS_TIMEOUT, task).await {
        Ok(Ok(Ok(val))) => Ok(val),
        Ok(Ok(Err(e))) => Err(format!("JS Error: {}", e)),
        Ok(Err(e)) => Err(format!("JS panic: {}", e)),
        Err(_) => Err("JS timeout (10s limit exceeded)".to_string()),
    }
}



// Helper to try parsing a string as JSON if it looks like an array or object
pub(crate) fn try_parse_json_value(val: Value) -> Value {
    if let Value::String(ref s) = val {
        let trimmed = s.trim();

        // Already JSON?
        if (trimmed.starts_with('[') && trimmed.ends_with(']'))
            || (trimmed.starts_with('{') && trimmed.ends_with('}'))
        {
            if let Ok(parsed) = serde_json::from_str(trimmed) {
                return parsed;
            }
        }

        // Try Number (Prioritize Integer)
        if let Ok(n) = trimmed.parse::<i64>() {
            return json!(n);
        }
        if let Ok(n) = trimmed.parse::<f64>() {
            return json!(n);
        }

        // Try Boolean
        if trimmed.to_lowercase() == "true" {
            return json!(true);
        }
        if trimmed.to_lowercase() == "false" {
            return json!(false);
        }

        // Try Comma-separated array (Smart Detection)
        // Only apply for short, single-line strings that look like actual CSV lists
        // (e.g. "en,fr,de" or "1,2,3"), NOT for natural language prose containing commas.
        if trimmed.contains(',')
            && !trimmed.contains('{')
            && !trimmed.contains('}')
            && !trimmed.contains('\n')
            && !trimmed.contains('\r')
            && trimmed.len() <= 200
            && !trimmed.contains(", ")
        // prose uses ", " while CSV uses ","
        {
            let parts: Vec<Value> = trimmed
                .split(',')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .map(|p| try_parse_json_value(json!(p)))
                .collect();
            if !parts.is_empty() {
                return Value::Array(parts);
            }
        }

        // Try simplified array syntax: [en] -> ["en"]
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let interior = trimmed[1..trimmed.len() - 1].trim();
            if !interior.is_empty() && !interior.contains(',') {
                return Value::Array(vec![try_parse_json_value(json!(interior))]);
            }
        }
    }
    val
}

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
) -> Result<Value, String> {
    // Check for pre-fetched data from the background Gmail poller.
    // This contains ONLY new emails (not previously seen), set by check_and_trigger_gmail().
    {
        let data = GMAIL_TRIGGER_DATA.lock().await;
        if let Some(trigger_data) = data.get(workflow_id) {
            tracing::info!(
                "Gmail trigger: using pre-fetched new email data for workflow {}",
                workflow_id
            );
            return Ok(trigger_data.clone());
        }
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
                .and_then(|v| v.as_bool().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
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
                .and_then(|v| v.as_bool().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
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
                    if let Err(e) = state.tools.run("gmail_mark_read", json!({ "ids": ids })).await {
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
                .run("gmail_download_all_attachments", json!({ "message_id": id }))
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

// ── Interpolation ─────────────────────────────────────────────────────────────

// Evaluate a full JS expression using boa_engine. Inject $node representing the results map.
fn evaluate_js_expression(
    expression: &str,
    results: &std::collections::HashMap<String, NodeResult>,
    run_id: &str,
) -> Option<Value> {
    let mut context = boa_engine::Context::default();
    register_expression_natives(&mut context);

    let mut nodes_map = serde_json::Map::new();
    for (key, res) in results {
        let mut node_obj = serde_json::Map::new();
        node_obj.insert("output".to_string(), res.output.clone());
        node_obj.insert("data".to_string(), res.output.clone());
        // Mirror the alias set exposed by the JS node so complex expressions
        // that fall through to full JS evaluation see the same shape.
        node_obj.insert("json".to_string(), res.output.clone());
        node_obj.insert("name".to_string(), serde_json::json!(res.node_name));
        node_obj.insert("id".to_string(), serde_json::json!(res.node_id));
        node_obj.insert("type".to_string(), serde_json::json!(res.node_type));
        node_obj.insert("error".to_string(), serde_json::json!(res.error));
        let val = Value::Object(node_obj);
        nodes_map.insert(key.clone(), val.clone());
        nodes_map.insert(res.node_name.clone(), val.clone());
        // Lowercase alias so $node["My Node"] works regardless of case.
        let lower = res.node_name.to_lowercase();
        if lower != res.node_name {
            nodes_map.entry(lower).or_insert(val);
        }
    }
    let nodes_json =
        serde_json::to_string(&Value::Object(nodes_map)).unwrap_or_else(|_| "{}".to_string());

    // Convenience helpers mirroring the JavaScript node, so inline {{ }} field
    // expressions and full JS-node scripts see the same globals. Identity-bound
    // helpers ($nodeId/$nodeName/$execution) are intentionally omitted — a field
    // expression has no "current node" context.
    let mut ordered: Vec<&NodeResult> = results.values().collect();
    ordered.sort_by_key(|r| r.position);
    let prev = ordered.last();
    let input_json = prev
        .map(|r| serde_json::to_string(&r.output).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_else(|| "{}".to_string());
    let prev_node_json = prev
        .map(|r| {
            serde_json::to_string(&serde_json::json!({
                "name": r.node_name, "id": r.node_id, "type": r.node_type,
                "output": r.output, "data": r.output, "json": r.output,
            }))
            .unwrap_or_else(|_| "{}".to_string())
        })
        .unwrap_or_else(|| "{}".to_string());
    let items_json = serde_json::to_string(
        &ordered
            .iter()
            .map(|r| {
                serde_json::json!({
                    "json": r.output, "data": r.output, "name": r.node_name,
                    "id": r.node_id, "type": r.node_type,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string());
    let now = chrono::Utc::now();

    let setup_script = format!(
        "var $node = {nodes};\
         var $input = {input};\
         var $json = $input;\
         var $items = {items};\
         var $prevNode = {prev_node};\
         var $now = \"{now_iso}\";\
         var $today = \"{today}\";\
         var $workflow = {{}};\
         var $execution = {{ \"runId\": \"{run_id}\" }};\
         var $env = {env};",
        nodes = nodes_json,
        input = input_json,
        items = items_json,
        prev_node = prev_node_json,
        now_iso = now.to_rfc3339(),
        today = now.format("%Y-%m-%d"),
        run_id = run_id,
        env = expression_env_json(),
    );

    if context
        .eval(boa_engine::Source::from_bytes(setup_script.as_bytes()))
        .is_err()
    {
        return None;
    }

    let wrapped = format!("(function() {{ return {}; }})()", expression);
    match context.eval(boa_engine::Source::from_bytes(wrapped.as_bytes())) {
        Ok(res) => Some(js_value_to_json(&res, &mut context)),
        Err(e) => {
            tracing::debug!("JS evaluation failed for {}: {}", expression, e);
            None
        }
    }
}

// Resolves a single value against node results. Preserves types for full matches.
/// All transitive upstream node ids of `node_id`, walking `edges` backwards.
/// Lets same-named `$node["Name"]` references be disambiguated toward the
/// reference's actual upstream, matching the editor preview's scoping.
fn ancestor_node_ids(
    node_id: &str,
    edges: &[WorkflowEdge],
) -> std::collections::HashSet<String> {
    let mut ancestors = std::collections::HashSet::new();
    let mut stack = vec![node_id.to_string()];
    while let Some(cur) = stack.pop() {
        for e in edges.iter().filter(|e| e.target_id == cur) {
            if ancestors.insert(e.source_id.clone()) {
                stack.push(e.source_id.clone());
            }
        }
    }
    ancestors
}

/// Resolve a `$node["identifier"]` reference to a single result.
///
/// Exact node-id matches win. For a name match, when several nodes share the
/// name we prefer one that is in `ancestors` (an upstream node of the node being
/// resolved) — the same scoping the editor preview uses — and otherwise fall
/// back to a deterministic pick (lowest node id). Previously the match fell out
/// of `HashMap::values()` in random order, so a name collision (e.g. a legacy
/// workflow with two "Post Bible Verse" nodes) resolved correctly on some runs
/// and to empty on others.
fn lookup_node<'a>(
    results: &'a std::collections::HashMap<String, NodeResult>,
    ancestors: Option<&std::collections::HashSet<String>>,
    identifier: &str,
) -> Option<&'a NodeResult> {
    if let Some(r) = results.get(identifier) {
        return Some(r);
    }
    let id_lower = identifier.to_lowercase();
    let mut matches: Vec<&NodeResult> = results
        .values()
        .filter(|r| r.node_name.to_lowercase() == id_lower)
        .collect();
    if matches.len() > 1 {
        if let Some(anc) = ancestors {
            let mut upstream: Vec<&NodeResult> = matches
                .iter()
                .copied()
                .filter(|r| anc.contains(&r.node_id))
                .collect();
            if !upstream.is_empty() {
                upstream.sort_by(|a, b| a.node_id.cmp(&b.node_id));
                return upstream.into_iter().next();
            }
        }
        matches.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    }
    matches.into_iter().next()
}

/// Convenience wrapper used by tests (production passes an ancestor scope via
/// [`resolve_value_scoped`]).
#[cfg(test)]
fn resolve_value(s: &str, results: &std::collections::HashMap<String, NodeResult>) -> Value {
    resolve_value_scoped(s, results, None, "")
}

/// Same as [`resolve_value`] but scoped to the executing node's upstream
/// `ancestors` (node ids), so same-named references prefer the upstream node.
fn resolve_value_scoped(
    s: &str,
    results: &std::collections::HashMap<String, NodeResult>,
    ancestors: Option<&std::collections::HashSet<String>>,
    run_id: &str,
) -> Value {
    use once_cell::sync::Lazy;
    // Compiled once: this function runs for every string in every node config.
    static RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"\{\{\s*\$?node\[['"](.+?)['"]\]\.([a-zA-Z0-9_\-\.\[\]]+)\s*\}\}"#).unwrap()
    });
    static RE_DOT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"\{\{\s*\$?node\.([a-zA-Z0-9_\-]+)\.([a-zA-Z0-9_\-\.\[\]]+)\s*\}\}"#).unwrap()
    });
    // Pure-expression regexes (no {{ }}). Anchored with ^...$ so they only
    // match when the WHOLE trimmed field value is a single expression — this
    // is the form the drag-and-drop now emits (n8n-style, no brackets).
    static RE_PURE: Lazy<Regex> = Lazy::new(|| {
        // Identifier class excludes quotes and `]` on purpose: with a permissive
        // `(.+?)`, the `^...$` anchor would backtrack and let the identifier span
        // TWO `$node[...]` refs in one field (e.g. `…routeOrigin to …routeDestination`),
        // producing a bogus identifier that resolves to Null and silently writes
        // nothing. Restricting it makes RE_PURE match only a genuine single
        // expression; multi-ref values fall through to prose interpolation below.
        Regex::new(r#"^\$?node\[['"]([^"'\]]+?)['"]\]\.([a-zA-Z0-9_\-\.\[\]]+)$"#).unwrap()
    });
    static RE_PURE_DOT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"^\$?node\.([a-zA-Z0-9_\-]+)\.([a-zA-Z0-9_\-\.\[\]]+)$"#).unwrap()
    });
    static RE_ANY: Lazy<Regex> = Lazy::new(|| Regex::new(r#"(?s)\{\{(.+?)\}\}"#).unwrap());
    // Bare (unwrapped) references embedded inside a larger string — the form
    // drag-and-drop emits when dropped into prose. NOT anchored, so they match
    // mid-string. The bracketed form (node["Name"].field) is distinctive enough
    // to be safe in prose; the dot form requires a leading $ so it never
    // clobbers ordinary text like "file.name.ext".
    static RE_BARE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"\$?node\[['"](.+?)['"]\]\.([a-zA-Z0-9_\-\.\[\]]+)"#).unwrap()
    });
    static RE_BARE_DOT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"\$node\.([a-zA-Z0-9_\-]+)\.([a-zA-Z0-9_\-\.\[\]]+)"#).unwrap()
    });
    let re = &*RE;
    let re_dot = &*RE_DOT;

    // Resolve a bare or unwrapped pure expression to its raw Value.
    // Returns Some(value) on a hit; Some(Null) on a recognized-but-missing
    // reference; None when the expression isn't a $node reference at all.
    let resolve_pure = |expression: &str| -> Option<Value> {
        // Bracketed form: $node["Name"].field
        if let Some(cap) = RE_PURE.captures(expression) {
            let identifier = &cap[1];
            let field = &cap[2];
            let res = lookup_node(results, ancestors, identifier);
            return Some(match res {
                // Preserve null so downstream nodes see the real value.
                // Previously this converted null -> "" which hid missing data.
                Some(res) => get_raw_field(res, field),
                // Recognized reference but node not run / wrong branch: emit
                // Null instead of leaking the literal token into the request.
                None => Value::Null,
            });
        }

        // Dot form: $node.id.field
        if let Some(cap) = RE_PURE_DOT.captures(expression) {
            let identifier = &cap[1];
            let field = &cap[2];
            let res = lookup_node(results, ancestors, identifier);
            return Some(match res {
                Some(res) => get_raw_field(res, field),
                None => Value::Null,
            });
        }
        None
    };

    let trimmed = s.trim();

    // Wrapped pure expression: {{ $node["Name"].field }} (whole field is one expr)
    if trimmed.starts_with("{{") && trimmed.ends_with("}}") && s.matches("{{").count() == 1 {
        let expression = &trimmed[2..trimmed.len() - 2].trim();
        if let Some(val) = resolve_pure(expression) {
            return val;
        }
        // Fallback: full JS evaluation of whatever is inside {{ }}
        if let Some(val) = evaluate_js_expression(expression, results, run_id) {
            return val;
        }
    }

    // Bare pure expression: $node["Name"].field (no {{ }}, n8n drag style).
    // Only resolve when the ENTIRE field value is the expression so we never
    // accidentally rewrite plain prose that happens to mention "$node".
    if !trimmed.contains("{{") {
        if let Some(val) = resolve_pure(trimmed) {
            return val;
        }
    }

    // Mixed string interpolation - requires stringifying results
    let mut result = s.to_string();

    // Bracketed form: {{ $node["Name"].field }}
    for cap in re.captures_iter(s) {
        let identifier = &cap[1];
        let field = &cap[2];
        let res = lookup_node(results, ancestors, identifier);
        if let Some(res) = res {
            let val = extract_field(res, field);
            result = result.replace(&cap[0], &val);
        }
    }

    // Dot form: {{ $node.id.field }}
    let result_clone = result.clone();
    for cap in re_dot.captures_iter(&result_clone) {
        let identifier = &cap[1];
        let field = &cap[2];
        let res = lookup_node(results, ancestors, identifier);
        if let Some(res) = res {
            let val = extract_field(res, field);
            result = result.replace(&cap[0], &val);
        }
    }

    // JS Fallback for ANY remaining {{ ... }} blocks that weren't matched purely!
    let result_cleanup = result.clone();
    for cap in RE_ANY.captures_iter(&result_cleanup) {
        let expression = cap[1].trim();
        if let Some(val) = evaluate_js_expression(expression, results, run_id) {
            let val_str = match &val {
                Value::String(s) => s.clone(),
                Value::Number(n) => {
                    if let Some(f) = n.as_f64() {
                        if f.fract() == 0.0 {
                            (f as i64).to_string()
                        } else {
                            n.to_string()
                        }
                    } else {
                        n.to_string()
                    }
                }
                Value::Bool(b) => b.to_string(),
                Value::Null | Value::Array(_) | Value::Object(_) => {
                    serde_json::to_string(&val).unwrap_or_default()
                }
            };
            // Only replace if it doesn't leave "null" incorrectly... wait!
            let final_str = if val.is_null() {
                "".to_string()
            } else {
                val_str
            };
            result = result.replace(&cap[0], &final_str);
        } else {
            // A {{ }} block that fails to evaluate (JS error, bad syntax, or a
            // reference to a node that didn't run) used to leak its literal text
            // into the output, where it looked like a value that silently
            // "didn't resolve". Resolve to empty instead — consistent with how
            // missing bare references already resolve — and warn so the failure
            // is visible in logs rather than silent in the rendered field.
            tracing::warn!(
                "workflow expression failed to evaluate, resolving to empty: {{ {} }}",
                expression
            );
            result = result.replace(&cap[0], "");
        }
    }

    // Bare references embedded in prose: $node["Name"].field and $node.id.field
    // (no {{ }}). The whole-field bare form already returned above; this handles
    // the drag-into-text case so a downstream node receives the value, not the
    // literal token. Mirrors the editor's live preview (resolveExpression).
    for re_bare in [&*RE_BARE, &*RE_BARE_DOT] {
        let snapshot = result.clone();
        for cap in re_bare.captures_iter(&snapshot) {
            let whole = &cap[0];
            let identifier = &cap[1];
            // Drop any trailing '.' the greedy field class swept up from prose
            // (e.g. a sentence-ending period), and shrink the token to match so
            // we don't also eat the punctuation out of the surrounding text.
            let field = cap[2].trim_end_matches('.');
            if field.is_empty() {
                continue;
            }
            let token = &whole[..whole.len() - (cap[2].len() - field.len())];
            let res = lookup_node(results, ancestors, identifier);
            // Found -> stringified value; recognized but missing (not yet run /
            // wrong branch) -> empty, so the literal token never leaks downstream.
            let replacement = match res {
                Some(res) => extract_field(res, field),
                None => String::new(),
            };
            result = result.replace(token, &replacement);
        }
    }

    Value::String(result)
}

fn extract_field(res: &NodeResult, field: &str) -> String {
    match field {
        // "json" is exposed as an alias in JS nodes; honor it here too so
        // {{ $node["X"].json.field }} doesn't silently resolve to "".
        "data" | "output" | "json" => res.output.to_string(),
        "error" => res.error.clone().unwrap_or_default(),
        _ if field.starts_with("data.")
            || field.starts_with("output.")
            || field.starts_with("json.") => {
            let path = &field[field.find('.').unwrap() + 1..];
            res.output
                .pointer(&format!(
                    "/{}",
                    path.replace("[", "/").replace("]", "").replace(".", "/")
                ))
                .map(|v| {
                    if v.is_string() {
                        v.as_str().unwrap().to_string()
                    } else if let Some(n) = v.as_f64() {
                        if n.fract() == 0.0 {
                            (n as i64).to_string()
                        } else {
                            v.to_string()
                        }
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_default()
        }
        _ if field.starts_with("binary.") => {
            let path = &field[field.find('.').unwrap() + 1..];
            res.output
                .get("binary")
                .and_then(|f| {
                    f.pointer(&format!(
                        "/{}",
                        path.replace("[", "/").replace("]", "").replace(".", "/")
                    ))
                })
                .map(|v| {
                    if v.is_string() {
                        v.as_str().unwrap().to_string()
                    } else if let Some(n) = v.as_f64() {
                        if n.fract() == 0.0 {
                            (n as i64).to_string()
                        } else {
                            v.to_string()
                        }
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_default()
        }
        _ if field.starts_with("file.") => {
            let path = &field[field.find('.').unwrap() + 1..];
            res.output
                .get("binary") // look in binary first as it's the new standard
                .or_else(|| res.output.get("file"))
                .and_then(|f| {
                    f.pointer(&format!(
                        "/{}",
                        path.replace("[", "/").replace("]", "").replace(".", "/")
                    ))
                })
                .map(|v| {
                    if v.is_string() {
                        v.as_str().unwrap().to_string()
                    } else if let Some(n) = v.as_f64() {
                        if n.fract() == 0.0 {
                            (n as i64).to_string()
                        } else {
                            v.to_string()
                        }
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_default()
        }
        // Generic fallback: treat any other field as a direct path into the
        // node's output (e.g. {{ $node["Loop"].current }}, {{ $node["X"].body }}).
        // Without this, unknown fields silently stringified to "".
        _ => res
            .output
            .pointer(&parse_path_pointer(field))
            .map(|v| {
                if let Some(s) = v.as_str() {
                    s.to_string()
                } else if let Some(n) = v.as_f64() {
                    if n.fract() == 0.0 {
                        (n as i64).to_string()
                    } else {
                        v.to_string()
                    }
                } else {
                    v.to_string()
                }
            })
            .unwrap_or_default(),
    }
}

fn get_raw_field(res: &NodeResult, field: &str) -> Value {
    match field {
        "data" | "output" | "json" => res.output.clone(),
        "error" => json!(res.error),
        _ if field.starts_with("data.")
            || field.starts_with("output.")
            || field.starts_with("json.") => {
            let path = &field[field.find('.').unwrap() + 1..];
            res.output
                .pointer(&format!(
                    "/{}",
                    path.replace("[", "/").replace("]", "").replace(".", "/")
                ))
                .cloned()
                .unwrap_or(Value::Null)
        }
        "binary" => res.output.get("binary").cloned().unwrap_or(Value::Null),
        _ if field.starts_with("binary.") => {
            let path = &field[field.find('.').unwrap() + 1..];
            res.output
                .get("binary")
                .and_then(|f| {
                    f.pointer(&format!(
                        "/{}",
                        path.replace("[", "/").replace("]", "").replace(".", "/")
                    ))
                })
                .cloned()
                .unwrap_or(Value::Null)
        }
        _ if field.starts_with("file.") => {
            let path = &field[field.find('.').unwrap() + 1..];
            res.output
                .get("binary")
                .or_else(|| res.output.get("file"))
                .and_then(|f| {
                    f.pointer(&format!(
                        "/{}",
                        path.replace("[", "/").replace("]", "").replace(".", "/")
                    ))
                })
                .cloned()
                .unwrap_or(Value::Null)
        }
        // Generic fallback: treat any other field as a direct path into the
        // node's output so natural expressions resolve to the real value
        // (preserving type) instead of silently yielding null.
        _ => res
            .output
            .pointer(&parse_path_pointer(field))
            .cloned()
            .unwrap_or(Value::Null),
    }
}

// Recursively walk the JSON tree and interpolate string values directly
fn interpolate_value(
    val: &Value,
    results: &std::collections::HashMap<String, NodeResult>,
    ancestors: Option<&std::collections::HashSet<String>>,
    run_id: &str,
) -> Value {
    match val {
        Value::String(s) => {
            // Resolve expressions but do NOT re-parse the result through
            // try_parse_json_value — that caused double-parsing where strings
            // like "123" became numbers, "true" became bools, and comma-
            // containing strings became arrays.
            resolve_value_scoped(s, results, ancestors, run_id)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), interpolate_value(v, results, ancestors, run_id)))
                .collect(),
        ),
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| interpolate_value(v, results, ancestors, run_id))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn interpolate_config(
    config: &Value,
    results: &std::collections::HashMap<String, NodeResult>,
    state: &AppState,
    ancestors: Option<&std::collections::HashSet<String>>,
    run_id: &str,
) -> Value {
    let mut interpolated = interpolate_value(config, results, ancestors, run_id);

    // Inject credentials if specified
    if let Value::Object(ref mut map) = interpolated {
        if let Some(cred_id) = map.get("credential_id").and_then(|v| v.as_str()) {
            if !cred_id.is_empty() {
                if let Ok(conn) = state.db.get() {
                    if let Ok(data_str) = conn.query_row(
                        "SELECT data FROM credentials WHERE id = ?1",
                        [cred_id],
                        |r| r.get::<_, String>(0),
                    ) {
                        // Encrypted at rest; decrypt_key passes legacy plaintext through.
                        let data_str = crate::crypto::decrypt_key(&data_str);
                        if let Ok(Value::Object(cred_map)) = serde_json::from_str(&data_str) {
                            for (k, v) in cred_map {
                                map.insert(k, v);
                            }
                        }
                    }
                }
            }
        }
    }
    interpolated
}

fn parse_path_pointer(path: &str) -> String {
    format!(
        "/{}",
        path.replace("[", "/").replace("]", "").replace(".", "/")
    )
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

// ── n8n-compatible condition engine ─────────────────────────────────────────
//
// Mirrors n8n's Filter/IF/Switch operator set across every data type
// (string, number, boolean, dateTime, array, object) plus the universal
// existence/emptiness operators. Values arrive already expression-resolved,
// so each side may be any JSON type; we coerce per the chosen data type
// (n8n "loose" type validation) before comparing.

/// Map legacy/aliased operator ids to their canonical n8n id.
fn canonical_op(op: &str) -> &str {
    match op {
        "isEmpty" => "empty",
        "isNotEmpty" => "notEmpty",
        "isTrue" => "true",
        "isFalse" => "false",
        "greater" | "larger" => "gt",
        "less" | "smaller" => "lt",
        "greaterEqual" | "largerEqual" | "greaterThanOrEqual" => "gte",
        "lessEqual" | "smallerEqual" | "lessThanOrEqual" => "lte",
        "matches" => "regex",
        "notMatches" | "doesNotMatch" => "notRegex",
        other => other,
    }
}

fn val_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 && f.abs() < 1e15 {
                    return (f as i64).to_string();
                }
            }
            n.to_string()
        }
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}

fn val_to_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

fn val_to_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" | "" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// n8n "empty": null/undefined, "", [], {} are empty.
fn val_is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

pub(crate) fn val_to_datetime(v: &Value) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
    match v {
        Value::Number(n) => {
            let f = n.as_f64()?;
            // Heuristic: large magnitudes are epoch millis, otherwise seconds.
            let (secs, nsecs) = if f.abs() >= 1e11 {
                ((f / 1000.0).trunc() as i64, ((f as i64 % 1000) * 1_000_000) as u32)
            } else {
                (f as i64, 0)
            };
            Utc.timestamp_opt(secs, nsecs)
                .single()
                .map(|dt| dt.fixed_offset())
        }
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return None;
            }
            if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
                return Some(dt);
            }
            if let Ok(dt) = DateTime::parse_from_rfc2822(t) {
                return Some(dt);
            }
            for fmt in [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%dT%H:%M",
                "%Y-%m-%d %H:%M",
            ] {
                if let Ok(ndt) = NaiveDateTime::parse_from_str(t, fmt) {
                    return Some(Utc.from_utc_datetime(&ndt).fixed_offset());
                }
            }
            if let Ok(nd) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
                if let Some(ndt) = nd.and_hms_opt(0, 0, 0) {
                    return Some(Utc.from_utc_datetime(&ndt).fixed_offset());
                }
            }
            if let Ok(secs) = t.parse::<i64>() {
                return Utc.timestamp_opt(secs, 0).single().map(|dt| dt.fixed_offset());
            }
            None
        }
        _ => None,
    }
}

/// Compile a regex, supporting n8n's `/pattern/flags` form and case-insensitive
/// matching. Returns None on an invalid pattern (treated as no-match).
fn compile_regex(pattern: &str, case_insensitive: bool) -> Option<Regex> {
    let mut pat = pattern.to_string();
    let mut ci = case_insensitive;
    let mut multiline = false;
    let mut dotall = false;
    // /body/flags  →  extract body + flags
    if pat.len() >= 2 && pat.starts_with('/') {
        if let Some(close) = pat.rfind('/') {
            if close > 0 {
                let flags = pat[close + 1..].to_string();
                let body = pat[1..close].to_string();
                if flags.chars().all(|c| matches!(c, 'i' | 'm' | 's' | 'g' | 'u' | 'y')) {
                    ci = ci || flags.contains('i');
                    multiline = flags.contains('m');
                    dotall = flags.contains('s');
                    pat = body;
                }
            }
        }
    }
    regex::RegexBuilder::new(&pat)
        .case_insensitive(ci)
        .multi_line(multiline)
        .dot_matches_new_line(dotall)
        .build()
        .ok()
}

fn values_loosely_equal(a: &Value, b: &Value, case_sensitive: bool) -> bool {
    if a == b {
        return true;
    }
    if let (Some(x), Some(y)) = (val_to_number(a), val_to_number(b)) {
        if (x - y).abs() < f64::EPSILON {
            return true;
        }
    }
    let mut sa = val_to_string(a);
    let mut sb = val_to_string(b);
    if !case_sensitive {
        sa = sa.to_lowercase();
        sb = sb.to_lowercase();
    }
    sa == sb
}

fn num_cmp(a: &Value, b: &Value, f: impl Fn(f64, f64) -> bool) -> bool {
    match (val_to_number(a), val_to_number(b)) {
        (Some(x), Some(y)) => f(x, y),
        _ => false,
    }
}

/// Evaluate a single n8n-style condition. `left` is the tested value, `right`
/// the comparison value (ignored by unary operators).
pub(crate) fn evaluate_condition_typed(
    data_type: &str,
    op_raw: &str,
    left: &Value,
    right: &Value,
    case_sensitive: bool,
) -> bool {
    let op = canonical_op(op_raw);

    // Universal operators — valid for every data type.
    match op {
        "exists" => return !left.is_null(),
        "notExists" => return left.is_null(),
        "empty" => return val_is_empty(left),
        "notEmpty" => return !val_is_empty(left),
        _ => {}
    }

    match data_type {
        "number" => match op {
            "equals" => num_cmp(left, right, |a, b| (a - b).abs() < f64::EPSILON),
            "notEquals" => !num_cmp(left, right, |a, b| (a - b).abs() < f64::EPSILON),
            "gt" => num_cmp(left, right, |a, b| a > b),
            "lt" => num_cmp(left, right, |a, b| a < b),
            "gte" => num_cmp(left, right, |a, b| a >= b),
            "lte" => num_cmp(left, right, |a, b| a <= b),
            _ => false,
        },
        "boolean" => {
            let l = val_to_bool(left);
            match op {
                "true" => l == Some(true),
                "false" => l == Some(false),
                "equals" => l.is_some() && l == val_to_bool(right),
                "notEquals" => l != val_to_bool(right),
                _ => false,
            }
        }
        "dateTime" => {
            let l = val_to_datetime(left);
            let r = val_to_datetime(right);
            match (l, r) {
                (Some(a), Some(b)) => match op {
                    "equals" => a == b,
                    "notEquals" => a != b,
                    "after" => a > b,
                    "before" => a < b,
                    "afterOrEquals" => a >= b,
                    "beforeOrEquals" => a <= b,
                    _ => false,
                },
                // Unparseable on either side: only "notEquals" can be true.
                _ => op == "notEquals",
            }
        }
        "array" => {
            let arr = left.as_array();
            match op {
                "contains" => arr
                    .map(|a| a.iter().any(|el| values_loosely_equal(el, right, case_sensitive)))
                    .unwrap_or(false),
                "notContains" => !arr
                    .map(|a| a.iter().any(|el| values_loosely_equal(el, right, case_sensitive)))
                    .unwrap_or(false),
                "lengthEquals" | "lengthNotEquals" | "lengthGt" | "lengthLt" | "lengthGte"
                | "lengthLte" => {
                    let len = arr.map(|a| a.len() as f64).unwrap_or(0.0);
                    let r = val_to_number(right).unwrap_or(0.0);
                    match op {
                        "lengthEquals" => (len - r).abs() < f64::EPSILON,
                        "lengthNotEquals" => (len - r).abs() >= f64::EPSILON,
                        "lengthGt" => len > r,
                        "lengthLt" => len < r,
                        "lengthGte" => len >= r,
                        "lengthLte" => len <= r,
                        _ => false,
                    }
                }
                _ => false,
            }
        }
        "object" => false, // only existence/emptiness apply (handled above)
        // string (default)
        _ => {
            if op == "regex" {
                return compile_regex(&val_to_string(right), !case_sensitive)
                    .map(|re| re.is_match(&val_to_string(left)))
                    .unwrap_or(false);
            }
            if op == "notRegex" {
                return !compile_regex(&val_to_string(right), !case_sensitive)
                    .map(|re| re.is_match(&val_to_string(left)))
                    .unwrap_or(false);
            }
            // Numeric comparisons are also offered on string fields (n8n loose).
            match op {
                "gt" => return num_cmp(left, right, |a, b| a > b),
                "lt" => return num_cmp(left, right, |a, b| a < b),
                "gte" => return num_cmp(left, right, |a, b| a >= b),
                "lte" => return num_cmp(left, right, |a, b| a <= b),
                _ => {}
            }
            let mut l = val_to_string(left);
            let mut r = val_to_string(right);
            if !case_sensitive {
                l = l.to_lowercase();
                r = r.to_lowercase();
            }
            match op {
                "equals" => l == r,
                "notEquals" => l != r,
                "contains" => l.contains(&r),
                "notContains" => !l.contains(&r),
                "startsWith" => l.starts_with(&r),
                "notStartsWith" => !l.starts_with(&r),
                "endsWith" => l.ends_with(&r),
                "notEndsWith" => !l.ends_with(&r),
                _ => l == r,
            }
        }
    }
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
            nodes::trigger::execute(config, state, trigger_source, workflow_id).await
        }
        "synapse" => nodes::synapse::execute_http_node(config).await,
        "myelin" => crate::tools::myelin::execute_myelin_node(state, config).await,
        "telegram" => crate::tools::telegram::execute_telegram_node(config).await,
        "whatsapp" => crate::tools::whatsapp::execute_whatsapp_node(config).await,
        "discord" => nodes::discord::execute(config).await,
        "slack" => nodes::slack::execute(config).await,
        "github" => nodes::github::execute(config).await,
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
        "nociceptor" => {
            let vec: Vec<_> = node_results.values().cloned().collect();
            nodes::nociceptor::execute_nociceptor_node(state, &vec).await
        }
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
                let deadline = tokio::time::Instant::now()
                    + tokio::time::Duration::from_millis(wait_ms);
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
        self.active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
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
                    let _ = conn.execute(
                        "UPDATE workflow_runs SET status = 'running', resume_at = NULL, resume_node_id = NULL WHERE id = ?1",
                        [run_id.clone()],
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
            "telegram" | "gmail" | "whatsapp" | "webhook" | "github" | "facebook" | "cron" => {
                Some(trigger_source)
            }
            // An error run (A3) starts ONLY from error-type trigger nodes; a normal
            // run never does (handled by `is_error_trigger` exclusion below).
            "error" => Some("error"),
            _ => None,
        };

        // A run may pin a single entry trigger to start from (its downstream chain
        // runs, sibling triggers stay dormant). Two sources set this pin:
        //   • a sub-workflow call choosing one entry of a multi-trigger child
        //     (keyed by child workflow id), and
        //   • a manual "Run" click on a specific Stimulus node's play button
        //     (keyed by run id, set in `run_in_background_inner`).
        // Consumed here so a later unpinned run falls back to every trigger.
        let entry_node: Option<String> = match trigger_source {
            "subflow" => SUBFLOW_ENTRY_NODE.lock().await.remove(workflow_id),
            "manual" => MANUAL_ENTRY_NODE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&run_id),
            _ => None,
        };

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
                    let node_trigger_kind = n
                        .config
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or(match n.node_type.as_str() {
                            "circadian" => "cron",
                            _ => "manual",
                        });
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
                        let _ = conn.execute(
                            "UPDATE workflow_runs SET node_results = ? WHERE id = ?",
                            rusqlite::params![res_json, run_id.clone()],
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
            let is_oneshot_trigger =
                matches!(node.node_type.as_str(), "trigger" | "stimulus" | "circadian");
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
                    "loop" | "ifCondition" | "switch" | "trigger" | "circadian" | "stimulus"
                        | "subflow" | "workflow"
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
                                if let Some(source_mut) = temp_results.get_mut(&source_node_id) {
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

                            let loop_result = if !iteration_errors.is_empty()
                                && !node.continue_on_fail
                            {
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
                            let config = interpolate_config(&node.config, &node_results, state, Some(&node_ancestors), &run_id);
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
                        let config = interpolate_config(&node.config, &node_results, state, Some(&node_ancestors), &run_id);
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
                    let config = interpolate_config(&node.config, &node_results, state, Some(&node_ancestors), &run_id);
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
                let config = interpolate_config(&node.config, &node_results, state, Some(&node_ancestors), &run_id);
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
                            let approve_path = format!("/webhook/approve/{current_id}/{run_id}");
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
                    let seconds = marker.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
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
                let _ = conn.execute(
                    "UPDATE workflow_runs SET node_results = ? WHERE id = ?",
                    rusqlite::params![res_json, run_id.clone()],
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
            if trigger_source != "error"
                && trigger_source != "manual"
                && workflow_status == "error"
            {
                // Resolve the handler id (workflow-level, then global default).
                // Each DB read is scoped so no pooled connection is held across an
                // await on the ERROR_TRIGGER_DATA mutex.
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
                        ERROR_TRIGGER_DATA
                            .lock()
                            .await
                            .insert(error_wf_id.clone(), payload);
                        match Self::run_in_background_with_source(
                            &error_wf_id,
                            state,
                            "error",
                            None,
                        ) {
                            Ok(child) => tracing::info!(
                                "Workflow '{}' failed — spawned error workflow {} (run {})",
                                workflow_id,
                                error_wf_id,
                                child
                            ),
                            Err(e) => {
                                // Don't leave a stale payload if the spawn failed.
                                ERROR_TRIGGER_DATA.lock().await.remove(&error_wf_id);
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
    pub async fn set_whatsapp_trigger_data(workflow_id: String, v: Value) {
        WHATSAPP_TRIGGER_DATA.lock().await.insert(workflow_id, v);
    }
    pub async fn set_telegram_trigger_data(workflow_id: String, v: Value) {
        TELEGRAM_TRIGGER_DATA.lock().await.insert(workflow_id, v);
    }
    pub async fn set_external_trigger_data(workflow_id: String, v: Value) {
        EXTERNAL_TRIGGER_DATA.lock().await.insert(workflow_id, v);
    }

    pub fn run_in_background(
        workflow_id: &str,
        state: &AppState,
        target_node_id: Option<String>,
    ) -> anyhow::Result<String> {
        Self::run_in_background_inner(workflow_id, state, "manual", target_node_id, false, None)
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
        Self::run_in_background_inner(workflow_id, state, trigger_source, target_node_id, false, None)
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
        Self::run_in_background_inner(workflow_id, state, "manual", None, false, Some(node_id))
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
        Self::run_in_background_inner(workflow_id, state, "manual", Some(node_id), single_node, None)
    }

    fn run_in_background_inner(
        workflow_id: &str,
        state: &AppState,
        trigger_source: &str,
        target_node_id: Option<String>,
        single_node: bool,
        entry_node_id: Option<String>,
    ) -> anyhow::Result<String> {
        let run_id = uuid::Uuid::new_v4().to_string();

        // Pin the sole entry node for this run (manual play button on a Stimulus).
        // Keyed by run_id and consumed in `run_inner` as the start queue is built,
        // so only this node's chain runs. Set BEFORE spawning so the run task
        // always sees it.
        if let Some(node_id) = entry_node_id {
            MANUAL_ENTRY_NODE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(run_id.clone(), node_id);
        }

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
                // The run task never reaches `run_inner`, so drop any entry-node
                // pin it staged (keyed by this run_id) to avoid a stale entry.
                MANUAL_ENTRY_NODE
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&rid);
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
                    let _ = conn.execute(
                        "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), node_results = ?2 WHERE id = ?1",
                        rusqlite::params![&rid, reason],
                    );
                }
                return;
            };

            // Pass the pre-created run_id so run_with_trigger reuses it rather
            // than inserting a duplicate record.
            if let Err(e) =
                Self::run_with_trigger(&wf_id, &s, &src, target_node_id, single_node, Some(rid.clone()))
                    .await
            {
                tracing::error!("Background workflow run failed: {}", e);
                if let Ok(conn) = s.db.get() {
                    let _ = conn.execute(
                        "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                        [&rid],
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
            "gmail" => 3,
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
                    tracing::error!("Resume {}: corrupt node_results ({}); failing run", run_id, e);
                    if let Ok(conn) = state.db.get() {
                        let _ = conn.execute(
                            "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                            [&run_id],
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
            // The run is no longer waiting: drop any resume tokens so a late URL
            // hit returns 410 rather than racing a finished run.
            if let Ok(conn) = state.db.get() {
                let _ = conn.execute(
                    "DELETE FROM workflow_resume_tokens WHERE run_id = ?1",
                    [&run_id],
                );
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
                            let _ = conn.execute(
                                "UPDATE workflow_runs SET status = 'waiting' WHERE id = ?1",
                                [&rid],
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
                        let _ = conn.execute(
                            "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                            [&rid],
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
                let _ = conn.execute(
                    "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                    [&run_id],
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
                    obj.insert("outcome".to_string(), json!(outcome));
                    obj.insert("resumed_at".to_string(), json!(now_iso));
                    if mode == "approval" {
                        obj.insert("approved".to_string(), json!(approved));
                        obj.insert("outputIndex".to_string(), json!(if approved { 0 } else { 1 }));
                    }
                }
            }
        }

        let completed: std::collections::HashSet<String> =
            results.iter().map(|r| r.node_id.clone()).collect();
        // Serialized patched chain, used only to re-park if the queue is full:
        // the token is already consumed, so a wait-forever run must fall back to
        // the time poller (which reads node_results from the DB) to retry.
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
                    tracing::warn!("Token resume of {} deferred: run queue full", rid);
                    if let Ok(conn) = s.db.get() {
                        let _ = conn.execute(
                            "UPDATE workflow_runs SET status = 'waiting', node_results = ?1, \
                             resume_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?2",
                            rusqlite::params![patched_json, rid],
                        );
                    }
                    return;
                }
            };
            if let Err(e) =
                Self::run_inner(&wf, &s, &src, None, false, Some(rid.clone()), Some(resume)).await
            {
                tracing::error!("Token-resumed workflow run {} failed: {}", rid, e);
                if let Ok(conn) = s.db.get() {
                    let _ = conn.execute(
                        "UPDATE workflow_runs SET status = 'failed', finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?1",
                        [&rid],
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
                    ) WHERE trigger_type IN ('cron', 'watcher', 'gmail')"
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
                    if !should_trigger(&wf) {
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
                } else {
                    let triggered = should_trigger(&wf);
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
                            tracing::warn!(
                                "Scheduled run of '{}' failed to spawn: {}",
                                wf.name,
                                e
                            );
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
        .and_then(|v| v.as_bool().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(false);
    let new_owned: Vec<Value> = new_emails.iter().map(|e| (*e).clone()).collect();
    let enriched = enrich_gmail_emails(state, new_owned, download).await;
    let enriched_count = enriched.len();

    // Store the new email data in a global map so execute_gmail_trigger can pick it up
    {
        let data = json!({
            "trigger": "gmail",
            "label": label,
            "new_email_count": enriched_count,
            "emails": enriched,
        });
        GMAIL_TRIGGER_DATA
            .lock()
            .await
            .insert(workflow_id.to_string(), data);
    }

    // Trigger the workflow. B3: hold a concurrency slot across the inline run so
    // gmail-triggered runs honor the same cap as other triggers. The seen-ids were
    // already committed above, so a queue-full *shed* would silently drop these
    // emails — in that rare case fall back to running unbounded rather than losing
    // them. The cleanup / mark-as-read below still run afterward.
    let _slot = acquire_run_slot(state).await;
    if _slot.is_none() {
        tracing::warn!(
            "Gmail trigger '{}': run queue full; running unbounded to avoid dropping {} email(s)",
            workflow_name,
            enriched_count
        );
    }
    WorkflowEngine::run_with_trigger(workflow_id, state, "gmail", None, false, None)
        .await
        .map_err(|e| e.to_string())?;

    // Clean up the trigger data after run
    GMAIL_TRIGGER_DATA.lock().await.remove(workflow_id);

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
            if let Err(e) = state.tools.run("gmail_mark_read", json!({ "ids": ids })).await {
                tracing::warn!("Gmail trigger '{}': mark-as-read failed: {}", workflow_name, e);
            }
        }
    }

    Ok(true)
}

fn should_trigger(wf: &Workflow) -> bool {
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
                    "Workflow '{}': mode={} → manila cron='{}'",
                    wf.name,
                    mode,
                    eval_cron
                );
            } else if let Some(cron_expr) = s.get("cron").and_then(|v| v.as_str()) {
                eval_cron = cron_expr.to_string();
            }

            if !eval_cron.is_empty() {
                // Convert manila cron to UTC
                let utc_cron = crate::scheduler::engine::manila_cron_to_utc(&eval_cron);
                tracing::debug!(
                    "Workflow '{}': manila='{}' → utc='{}'",
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
    let mut mins = if wf.trigger_type == "gmail" {
        // Gmail triggers use poll_interval from config (default 5 min)
        wf.trigger_config
            .get("poll_interval")
            .and_then(|v| v.as_u64())
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
mod condition_tests {
    use super::evaluate_condition_typed as ev;
    use serde_json::json;

    #[test]
    fn string_ops() {
        assert!(ev("string", "equals", &json!("hi"), &json!("hi"), true));
        assert!(!ev("string", "equals", &json!("Hi"), &json!("hi"), true));
        assert!(ev("string", "equals", &json!("Hi"), &json!("hi"), false)); // case-insensitive
        assert!(ev("string", "contains", &json!("hello world"), &json!("lo wo"), true));
        assert!(ev("string", "notContains", &json!("abc"), &json!("z"), true));
        assert!(ev("string", "startsWith", &json!("abcdef"), &json!("abc"), true));
        assert!(ev("string", "notStartsWith", &json!("abcdef"), &json!("xyz"), true));
        assert!(ev("string", "endsWith", &json!("abcdef"), &json!("def"), true));
        assert!(ev("string", "notEndsWith", &json!("abcdef"), &json!("abc"), true));
        assert!(ev("string", "regex", &json!("user@x.com"), &json!(r"^\S+@\S+\.\S+$"), true));
        assert!(ev("string", "regex", &json!("HELLO"), &json!("hello"), false)); // ci regex
        assert!(ev("string", "notRegex", &json!("abc"), &json!(r"^\d+$"), true));
        // legacy aliases still work
        assert!(ev("string", "isEmpty", &json!(""), &json!(null), true));
        assert!(ev("string", "isNotEmpty", &json!("x"), &json!(null), true));
    }

    #[test]
    fn number_ops() {
        assert!(ev("number", "equals", &json!(5), &json!("5"), true)); // loose coercion
        assert!(ev("number", "notEquals", &json!(5), &json!(6), true));
        assert!(ev("number", "gt", &json!(10), &json!(3), true));
        assert!(ev("number", "lt", &json!(2), &json!(3), true));
        assert!(ev("number", "gte", &json!(3), &json!(3), true));
        assert!(ev("number", "lte", &json!(3), &json!(4), true));
        // legacy aliases
        assert!(ev("number", "greater", &json!(10), &json!(3), true));
        assert!(ev("number", "lessEqual", &json!(3), &json!(3), true));
    }

    #[test]
    fn boolean_ops() {
        assert!(ev("boolean", "true", &json!(true), &json!(null), true));
        assert!(ev("boolean", "false", &json!(false), &json!(null), true));
        assert!(ev("boolean", "true", &json!("yes"), &json!(null), true)); // coerce
        assert!(ev("boolean", "equals", &json!(true), &json!("true"), true));
        assert!(ev("boolean", "notEquals", &json!(true), &json!(false), true));
        // legacy
        assert!(ev("boolean", "isTrue", &json!(1), &json!(null), true));
        assert!(ev("boolean", "isFalse", &json!(0), &json!(null), true));
    }

    #[test]
    fn datetime_ops() {
        let a = json!("2024-01-01T00:00:00Z");
        let b = json!("2024-06-01T00:00:00Z");
        assert!(ev("dateTime", "before", &a, &b, true));
        assert!(ev("dateTime", "after", &b, &a, true));
        assert!(ev("dateTime", "equals", &json!("2024-01-01"), &json!("2024-01-01T00:00:00Z"), true));
        assert!(ev("dateTime", "afterOrEquals", &a, &a, true));
        assert!(ev("dateTime", "beforeOrEquals", &a, &b, true));
        // cross-offset equality compares the instant
        assert!(ev("dateTime", "equals", &json!("2024-01-01T00:00:00+00:00"), &json!("2024-01-01T01:00:00+01:00"), true));
    }

    #[test]
    fn array_ops() {
        let arr = json!([1, 2, 3]);
        assert!(ev("array", "contains", &arr, &json!(2), true));
        assert!(ev("array", "contains", &arr, &json!("2"), true)); // loose element match
        assert!(ev("array", "notContains", &arr, &json!(9), true));
        assert!(ev("array", "lengthEquals", &arr, &json!(3), true));
        assert!(ev("array", "lengthGt", &arr, &json!(2), true));
        assert!(ev("array", "lengthLte", &arr, &json!(3), true));
        assert!(ev("array", "lengthNotEquals", &arr, &json!(5), true));
    }

    #[test]
    fn universal_ops() {
        assert!(ev("string", "exists", &json!("x"), &json!(null), true));
        assert!(ev("string", "notExists", &json!(null), &json!(null), true));
        assert!(ev("array", "empty", &json!([]), &json!(null), true));
        assert!(ev("object", "empty", &json!({}), &json!(null), true));
        assert!(ev("object", "notEmpty", &json!({"a": 1}), &json!(null), true));
        assert!(ev("number", "empty", &json!(null), &json!(null), true));
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::{resolve_value, resolve_value_scoped, NodeResult};
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};

    fn node(name: &str, output: Value) -> NodeResult {
        NodeResult {
            node_id: format!("id_{name}"),
            node_name: name.to_string(),
            node_type: "test".to_string(),
            position: 0,
            status: "success".to_string(),
            output,
            duration_ms: 0,
            error: None,
            attempts: 1,
        }
    }

    fn results() -> HashMap<String, NodeResult> {
        let mut m = HashMap::new();
        let get = node(
            "Get Email",
            json!({ "from": "alice@example.com", "body": "Hello there" }),
        );
        m.insert(get.node_id.clone(), get);
        let trig = node(
            "When clicked",
            json!({ "emails": [ { "id": "msg_1" }, { "id": "msg_2" } ] }),
        );
        m.insert(trig.node_id.clone(), trig);
        m
    }

    // The original bug: a dragged-in $node[...] reference sitting inside prose
    // was sent verbatim instead of being resolved.
    #[test]
    fn bare_reference_in_prose_resolves() {
        let out = resolve_value(
            "Boss, new email from $node[\"Get Email\"].data.from\n\n$node[\"Get Email\"].data.body",
            &results(),
        );
        assert_eq!(
            out,
            Value::String("Boss, new email from alice@example.com\n\nHello there".to_string())
        );
    }

    // A sentence-ending period must survive — only the reference is replaced.
    #[test]
    fn bare_reference_keeps_trailing_punctuation() {
        let out = resolve_value("from $node[\"Get Email\"].data.from. Thanks", &results());
        assert_eq!(
            out,
            Value::String("from alice@example.com. Thanks".to_string())
        );
    }

    // Whole-field bare reference still returns the raw typed value (not a string),
    // so downstream nodes can index arrays/objects.
    #[test]
    fn whole_field_bare_preserves_type() {
        let out = resolve_value("$node[\"When clicked\"].data.emails[0].id", &results());
        assert_eq!(out, json!("msg_1"));
    }

    // Legacy {{ }}-wrapped form is unaffected.
    #[test]
    fn wrapped_form_still_works() {
        let out = resolve_value("Hi {{ $node[\"Get Email\"].data.from }} !", &results());
        assert_eq!(out, Value::String("Hi alice@example.com !".to_string()));
    }

    // A reference to a node that didn't run resolves to empty — never leaks the token.
    #[test]
    fn missing_node_does_not_leak_token() {
        let out = resolve_value("x $node[\"Ghost\"].data.foo y", &results());
        assert_eq!(out, Value::String("x  y".to_string()));
    }

    // Regression: two whole-field $node[...] refs with text between them must BOTH
    // resolve. The old anchored RE_PURE backtracked its `(.+?)` identifier across
    // both refs, captured a bogus node name, and returned Null — so e.g. a Sheets
    // batch_write cell silently wrote nothing while the action reported success.
    #[test]
    fn two_bare_references_both_resolve() {
        let route = node(
            "JavaScript 1",
            json!({ "routeOrigin": "Manila", "routeDestination": "Cebu" }),
        );
        let mut m = HashMap::new();
        m.insert(route.node_id.clone(), route);
        let out = resolve_value(
            "$node[\"JavaScript 1\"].data.routeOrigin to $node[\"JavaScript 1\"].data.routeDestination",
            &m,
        );
        assert_eq!(out, Value::String("Manila to Cebu".to_string()));
    }

    // Regression: two nodes share the name "Post Bible Verse" (a legacy workflow
    // created before unique-naming was enforced). Only the Facebook one — the
    // actual upstream of the Telegram node — has `permalink_url`; the Instagram
    // one does not. Scoped resolution must prefer the upstream ancestor so the
    // link resolves on every run, instead of falling out of HashMap iteration
    // order and intermittently hitting the Instagram node (-> empty "view it at ").
    #[test]
    fn duplicate_name_prefers_upstream_ancestor() {
        let mut fb = node(
            "Post Bible Verse",
            json!({ "id": "1_2", "permalink_url": "https://fb/p/2" }),
        );
        fb.node_id = "node_fb".to_string();
        let mut ig = node("Post Bible Verse", json!({ "id": "ig_1" }));
        ig.node_id = "node_ig".to_string();

        let mut m = HashMap::new();
        m.insert(fb.node_id.clone(), fb);
        m.insert(ig.node_id.clone(), ig);

        let text = "view it at $node[\"Post Bible Verse\"].data.permalink_url";

        // The Facebook-branch Telegram node's only upstream is the Facebook node.
        let fb_anc: HashSet<String> = ["node_fb".to_string()].into_iter().collect();
        let out = resolve_value_scoped(text, &m, Some(&fb_anc), "");
        assert_eq!(out, Value::String("view it at https://fb/p/2".to_string()));

        // A node scoped to the Instagram branch sees no permalink -> empty,
        // and never silently borrows the Facebook node's value.
        let ig_anc: HashSet<String> = ["node_ig".to_string()].into_iter().collect();
        let out_ig = resolve_value_scoped(text, &m, Some(&ig_anc), "");
        assert_eq!(out_ig, Value::String("view it at ".to_string()));
    }

    // Repro of the empty-caption bug: a cached/reused upstream result carries the
    // node_name it had on a PRIOR run; after the node was renamed to "Axon 2",
    // $node["Axon 2"] no longer matches by name and the caption resolves to null.
    // restamp_result_identities re-stamps the name from the current graph (by the
    // stable node_id), after which the reference resolves again — matching what
    // the editor preview shows.
    #[test]
    fn renamed_upstream_caption_resolves_after_restamp() {
        use super::{restamp_result_identities, WorkflowNode};
        use serde_json::json as j;

        let node_id = "node_ai".to_string();

        // Cached result stored under the node's OLD name ("Axon").
        let mut stale = node("Axon", j!({ "output": "Boss Cham, here is the draft." }));
        stale.node_id = node_id.clone();
        let mut results = HashMap::new();
        results.insert(node_id.clone(), stale);

        // Before re-stamping, the current name does not resolve.
        let before = resolve_value("$node[\"Axon 2\"].data.output", &results);
        assert_eq!(before, Value::Null);

        // Current graph: the same node id is now named "Axon 2".
        let nodes = vec![WorkflowNode {
            id: node_id.clone(),
            workflow_id: "wf".into(),
            position: 0,
            position_x: 0.0,
            position_y: 0.0,
            node_type: "cortex".into(),
            name: "Axon 2".into(),
            config: j!({}),
            enabled: true,
            continue_on_fail: false,
            retries: 0,
            retry_wait_ms: 0,
            retry_backoff: String::new(),
            pinned_data: None,
        }];
        restamp_result_identities(&mut results, &mut [], &mut [], &nodes);

        let after = resolve_value("$node[\"Axon 2\"].data.output", &results);
        assert_eq!(
            after,
            Value::String("Boss Cham, here is the draft.".to_string())
        );
    }

    // Generality: the fix is not Sheets- or node-specific. Any field on any node
    // can combine 2+ references from *different* nodes with arbitrary text between
    // them. Three refs across three node types, one field.
    #[test]
    fn multiple_references_across_different_nodes() {
        let mut m = HashMap::new();
        for n in [
            node("HTTP Request", json!({ "city": "Tokyo" })),
            node("Set 2", json!({ "temp": "18" })),
            node("Code", json!({ "unit": "C" })),
        ] {
            m.insert(n.node_id.clone(), n);
        }
        let out = resolve_value(
            "$node[\"HTTP Request\"].data.city: $node[\"Set 2\"].data.temp°$node[\"Code\"].data.unit",
            &m,
        );
        assert_eq!(out, Value::String("Tokyo: 18°C".to_string()));
    }

    // Inline JS: a {{ }} block runs through the boa engine with $node injected,
    // so any field on any node can transform/clean data with real JavaScript.
    #[test]
    fn inline_js_transform_in_any_field() {
        let mut m = HashMap::new();
        let js = node("JavaScript 1", json!({ "routeOrigin": "  manila, ph " }));
        m.insert(js.node_id.clone(), js);

        // method chain
        let out = resolve_value(
            "{{ $node[\"JavaScript 1\"].data.routeOrigin.trim().toUpperCase() }}",
            &m,
        );
        assert_eq!(out, Value::String("MANILA, PH".to_string()));

        // split + transform, embedded in surrounding text
        let out2 = resolve_value(
            "From: {{ $node[\"JavaScript 1\"].data.routeOrigin.split(\",\")[0].trim() }}",
            &m,
        );
        assert_eq!(out2, Value::String("From: manila".to_string()));
    }

    // Inline expressions get the same convenience globals as the JS node:
    // $json/$input (previous node), $items, $now, $today, $env.
    #[test]
    fn inline_js_helpers_match_js_node() {
        let mut m = HashMap::new();
        let prev = node("HTTP Request", json!({ "routeOrigin": "manila" }));
        m.insert(prev.node_id.clone(), prev);

        // $json points at the previous node's output.
        let out = resolve_value("{{ $json.routeOrigin.toUpperCase() }}", &m);
        assert_eq!(out, Value::String("MANILA".to_string()));

        // $today is injected and shaped YYYY-MM-DD.
        let today = resolve_value("{{ $today }}", &m);
        let today = today.as_str().unwrap();
        assert_eq!(today.len(), 10);
        assert_eq!(today.matches('-').count(), 2);

        // $now is a non-empty ISO timestamp.
        let now = resolve_value("{{ $now }}", &m);
        assert!(now.as_str().unwrap().contains('T'));
    }

    // (a) A {{ }} expression that errors resolves to empty instead of leaking
    // its literal text — no more confusing `{{ ... }}` showing up in output.
    #[test]
    fn failed_expression_resolves_to_empty() {
        let mut m = HashMap::new();
        let js = node("X", json!({ "present": "ok" }));
        m.insert(js.node_id.clone(), js);

        // .trim() on an undefined field throws in JS -> empty, not literal.
        let out = resolve_value("v={{ $node[\"X\"].data.missing.trim() }}", &m);
        assert_eq!(out, Value::String("v=".to_string()));

        // Reference to a node that didn't run, wrapped in {{ }} -> empty.
        let out2 = resolve_value("A {{ $node[\"Ghost\"].data.x }} B", &m);
        assert_eq!(out2, Value::String("A  B".to_string()));
        assert!(!out2.as_str().unwrap().contains("{{"));
    }

    // D2: the native $jmespath helper works as a pure function.
    #[test]
    fn jmespath_helper_queries_json() {
        let data = json!({ "items": [{ "n": 1 }, { "n": 2 }, { "n": 3 }] });
        assert_eq!(super::eval_jmespath(&data, "items[*].n"), json!([1, 2, 3]));
        assert_eq!(super::eval_jmespath(&data, "items[1].n"), json!(2));
        // Malformed expression → null, never a panic.
        assert_eq!(super::eval_jmespath(&data, "items[["), Value::Null);
        // Empty expression → null.
        assert_eq!(super::eval_jmespath(&data, ""), Value::Null);
    }

    // D2: $jmespath is reachable from inline {{ }} expressions and returns a
    // typed value (array), not a stringified blob.
    #[test]
    fn jmespath_helper_available_inline() {
        let mut m = HashMap::new();
        let n = node("API", json!({ "users": [{ "id": 7 }, { "id": 9 }] }));
        m.insert(n.node_id.clone(), n);
        let out = resolve_value("{{ $jmespath($node[\"API\"].data, \"users[*].id\") }}", &m);
        assert_eq!(out, json!([7, 9]));
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
            let complete = fold_prior_run_into_cache(
                &mut node_results,
                &mut prior_ordered,
                run,
                !seeded,
                ids,
            );
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
                    nr("trigger", "error", json!({})),        // errored -> not backfilled
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
