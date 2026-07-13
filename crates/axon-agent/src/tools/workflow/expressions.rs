//! Expression & interpolation layer, split out of `workflow.rs`: the Boa
//! (JS) execution infrastructure, `{{ }}` resolution against node results,
//! and config interpolation. Call sites are unchanged — the parent module
//! re-exports everything via `pub(crate) use expressions::*;`.

use super::*;

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

pub(crate) fn js_value_to_json(val: &JsValue, context: &mut Context) -> Value {
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
pub(crate) fn eval_jmespath(data: &Value, expr: &str) -> Value {
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
pub(crate) fn expression_env_json() -> String {
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
pub(crate) fn register_expression_natives(context: &mut Context) {
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

// ── Per-item context ($item / $index / $ancestor) ────────────────────────────

/// Reserved node-id under which the engine injects a synthetic "current item"
/// result during per-item fan-out (Loop bodies and `for_each` nodes). It is
/// filtered out of `$node` / `$results` / `$items` / `$prevNode`; scripts and
/// `{{ }}` expressions see it as `$item` / `$index` (plus `$json`/`$input`
/// overrides when the context sets `override_json`, the `for_each` case).
pub(crate) const ITEM_CONTEXT_KEY: &str = "__item__";

/// Split the synthetic item-context entry (if any) out of a result list,
/// returning its output wrapper. The list keeps only real node results.
fn take_item_context(results: &mut Vec<NodeResult>) -> Option<Value> {
    let idx = results.iter().position(|r| r.node_id == ITEM_CONTEXT_KEY)?;
    Some(results.remove(idx).output)
}

/// JS prelude defining the per-item globals and the `$ancestor()` lineage
/// helper. Without an item context the globals are null and `$ancestor(name)`
/// degrades to the named node's whole output. `$ancestor` prefers an explicit
/// `__idx` stamp on the current item (survives Filter/Sort reshaping — see
/// Split Out's `stampIndex` and for_each's `for_each_stamp_index`), falling
/// back to the iteration index (positional alignment).
fn item_bindings_script(ctx: Option<&Value>) -> String {
    let bindings = match ctx {
        Some(c) => {
            let item = serde_json::to_string(c.get("item").unwrap_or(&Value::Null))
                .unwrap_or_else(|_| "null".to_string());
            let index = c.get("index").and_then(|v| v.as_i64()).unwrap_or(-1);
            let is_first = c.get("is_first").and_then(|v| v.as_bool()).unwrap_or(false);
            let is_last = c.get("is_last").and_then(|v| v.as_bool()).unwrap_or(false);
            let total = c.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
            let override_json = c
                .get("override_json")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let overrides = if override_json {
                "$input = $item; $json = $item;"
            } else {
                ""
            };
            format!(
                "var $item = {item}; var $index = {index}; var $isFirst = {is_first}; \
                 var $isLast = {is_last}; var $total = {total}; {overrides}"
            )
        }
        None => "var $item = null; var $index = null; var $isFirst = null; \
                 var $isLast = null; var $total = null;"
            .to_string(),
    };
    format!(
        r#"{bindings}
function $ancestor(name) {{
    var n = $node[name];
    if (n === undefined && typeof name === 'string') n = $node[name.toLowerCase()];
    if (n === undefined || n === null) return null;
    var out = (n.data === undefined) ? null : n.data;
    var idx = null;
    if ($item !== null && typeof $item === 'object' && !Array.isArray($item) && $item.__idx !== undefined) idx = $item.__idx;
    else if (typeof $index === 'number' && $index >= 0) idx = $index;
    if (idx === null || out === null) return out;
    if (Array.isArray(out)) return (out[idx] === undefined) ? null : out[idx];
    if (typeof out === 'object' && Array.isArray(out.items)) return (out.items[idx] === undefined) ? null : out.items[idx];
    return out;
}}"#
    )
}

/// Strip `{{ expression }}` wrappers from a JS script so that dragged-in
/// expressions become plain JavaScript references.
/// E.g. `const item = {{ $node["Gmail"].data }};`
///   →  `const item = $node["Gmail"].data;`
pub(crate) fn strip_expression_wrappers(script: &str) -> String {
    // (?s) so expressions spanning multiple lines are also unwrapped
    static RE: once_cell::sync::Lazy<Regex> =
        once_cell::sync::Lazy::new(|| Regex::new(r"(?s)\{\{\s*(.+?)\s*\}\}").unwrap());
    RE.replace_all(script, "$1").to_string()
}

pub(crate) async fn execute_js_node(
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

    // Per-item fan-out context (Loop body / for_each): surfaced as $item/$index,
    // never as a $node/$results entry.
    let mut results_copy = results.to_vec();
    let item_ctx = take_item_context(&mut results_copy);
    let node_id = node.id.clone();
    let node_name = node.name.clone();
    let wf_id = workflow_id.to_string();
    let run_id = run_id.to_string();
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let logs_for_thread = logs.clone();

    let task = tokio::task::spawn_blocking(move || {
        let _guard = JsLogGuard::install(logs_for_thread);
        let mut context = Context::default();

        // Hard interpreter limits: tokio::time::timeout only abandons the
        // blocking task — it cannot stop boa. Without these, an infinite
        // loop in a user script leaks a blocking thread forever.
        context
            .runtime_limits_mut()
            .set_loop_iteration_limit(5_000_000);
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

        // ── $item / $index / $ancestor (per-item fan-out context) ─────────
        context
            .eval(Source::from_bytes(
                item_bindings_script(item_ctx.as_ref()).as_bytes(),
            ))
            .map_err(|e| format!("Item binding injection error: {}", e))?;

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

// ── Interpolation ─────────────────────────────────────────────────────────────

// Evaluate a full JS expression using boa_engine. Inject $node representing the results map.
pub(crate) fn evaluate_js_expression(
    expression: &str,
    results: &std::collections::HashMap<String, NodeResult>,
    run_id: &str,
) -> Option<Value> {
    let mut context = boa_engine::Context::default();
    register_expression_natives(&mut context);

    // Per-item fan-out context — bound as $item/$index below, hidden from $node.
    let item_ctx = results.get(ITEM_CONTEXT_KEY).map(|r| r.output.clone());

    let mut nodes_map = serde_json::Map::new();
    for (key, res) in results {
        if res.node_id == ITEM_CONTEXT_KEY {
            continue;
        }
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
    let mut ordered: Vec<&NodeResult> = results
        .values()
        .filter(|r| r.node_id != ITEM_CONTEXT_KEY)
        .collect();
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
    if context
        .eval(boa_engine::Source::from_bytes(
            item_bindings_script(item_ctx.as_ref()).as_bytes(),
        ))
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
pub(crate) fn ancestor_node_ids(
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
pub(crate) fn lookup_node<'a>(
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
pub(crate) fn resolve_value(
    s: &str,
    results: &std::collections::HashMap<String, NodeResult>,
) -> Value {
    resolve_value_scoped(s, results, None, "")
}

/// Same as [`resolve_value`] but scoped to the executing node's upstream
/// `ancestors` (node ids), so same-named references prefer the upstream node.
pub(crate) fn resolve_value_scoped(
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
    static RE_BARE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\$?node\[['"](.+?)['"]\]\.([a-zA-Z0-9_\-\.\[\]]+)"#).unwrap());
    static RE_BARE_DOT: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"\$node\.([a-zA-Z0-9_\-]+)\.([a-zA-Z0-9_\-\.\[\]]+)"#).unwrap());
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

pub(crate) fn extract_field(res: &NodeResult, field: &str) -> String {
    match field {
        // "json" is exposed as an alias in JS nodes; honor it here too so
        // {{ $node["X"].json.field }} doesn't silently resolve to "".
        "data" | "output" | "json" => res.output.to_string(),
        "error" => res.error.clone().unwrap_or_default(),
        _ if field.starts_with("data.")
            || field.starts_with("output.")
            || field.starts_with("json.") =>
        {
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

pub(crate) fn get_raw_field(res: &NodeResult, field: &str) -> Value {
    match field {
        "data" | "output" | "json" => res.output.clone(),
        "error" => json!(res.error),
        _ if field.starts_with("data.")
            || field.starts_with("output.")
            || field.starts_with("json.") =>
        {
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
pub(crate) fn interpolate_value(
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

pub(crate) fn interpolate_config(
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

pub(crate) fn parse_path_pointer(path: &str) -> String {
    format!(
        "/{}",
        path.replace("[", "/").replace("]", "").replace(".", "/")
    )
}

#[cfg(test)]
mod resolve_tests {
    use super::{resolve_value, resolve_value_scoped, NodeResult, ITEM_CONTEXT_KEY};
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

    /// Synthetic per-item context, shaped like `item_context_result` in the
    /// engine (workflow.rs).
    fn with_item(
        mut m: HashMap<String, NodeResult>,
        item: Value,
        index: usize,
        override_json: bool,
    ) -> HashMap<String, NodeResult> {
        let mut ctx = node(
            ITEM_CONTEXT_KEY,
            json!({
                "item": item, "index": index, "total": 3,
                "is_first": index == 0, "is_last": index == 2,
                "override_json": override_json,
            }),
        );
        ctx.node_id = ITEM_CONTEXT_KEY.to_string();
        ctx.position = i64::MAX / 2;
        m.insert(ITEM_CONTEXT_KEY.to_string(), ctx);
        m
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

    // ── Per-item context: $item / $index / $json override / $ancestor ────────

    // $item and $index resolve to the current fan-out item and its index.
    #[test]
    fn item_and_index_bind_from_context() {
        let m = with_item(results(), json!({ "sku": "A-7" }), 1, true);
        assert_eq!(
            resolve_value("{{ $item.sku }}", &m),
            Value::String("A-7".into())
        );
        assert_eq!(resolve_value("{{ $index }}", &m), json!(1));
    }

    // for_each (override_json=true): $json IS the current item — n8n semantics.
    #[test]
    fn json_overridden_to_item_when_for_each() {
        let m = with_item(results(), json!({ "sku": "A-7" }), 0, true);
        assert_eq!(
            resolve_value("{{ $json.sku }}", &m),
            Value::String("A-7".into())
        );
    }

    // Loop bodies (override_json=false): $json keeps its historical meaning;
    // only $item points at the current item.
    #[test]
    fn json_not_overridden_in_loop_context() {
        let m = with_item(results(), json!({ "sku": "A-7" }), 0, false);
        assert_eq!(resolve_value("{{ $json.sku }}", &m), Value::Null);
        assert_eq!(
            resolve_value("{{ $item.sku }}", &m),
            Value::String("A-7".into())
        );
    }

    // The synthetic context never leaks into $node.
    #[test]
    fn item_context_hidden_from_node_map() {
        let m = with_item(results(), json!({ "sku": "A-7" }), 0, true);
        assert_eq!(resolve_value("{{ $node['__item__'] }}", &m), Value::Null);
    }

    // $ancestor joins by the item's explicit __idx stamp when present…
    #[test]
    fn ancestor_prefers_idx_stamp() {
        let mut base = results();
        let src = node(
            "Orders",
            json!([{ "customer": "ana" }, { "customer": "bo" }]),
        );
        base.insert(src.node_id.clone(), src);
        // Iteration index says 0, but the __idx stamp says 1 (e.g. a Filter
        // upstream dropped item 0) — the stamp must win.
        let m = with_item(base, json!({ "sku": "A-7", "__idx": 1 }), 0, true);
        assert_eq!(
            resolve_value("{{ $ancestor('Orders').customer }}", &m),
            Value::String("bo".into())
        );
    }

    // …and falls back to positional alignment ($index) without a stamp.
    #[test]
    fn ancestor_falls_back_to_position() {
        let mut base = results();
        let src = node(
            "Orders",
            json!([{ "customer": "ana" }, { "customer": "bo" }]),
        );
        base.insert(src.node_id.clone(), src);
        let m = with_item(base, json!({ "sku": "A-7" }), 1, true);
        assert_eq!(
            resolve_value("{{ $ancestor('Orders').customer }}", &m),
            Value::String("bo".into())
        );
    }

    // Outside any per-item context $ancestor degrades to the whole output.
    #[test]
    fn ancestor_without_context_returns_whole_output() {
        let mut base = results();
        let src = node("Orders", json!([{ "customer": "ana" }]));
        base.insert(src.node_id.clone(), src);
        assert_eq!(
            resolve_value("{{ $ancestor('Orders') }}", &base),
            json!([{ "customer": "ana" }])
        );
    }

    // An {items:[…]} wrapper (loop/for_each aggregate output) is unwrapped.
    #[test]
    fn ancestor_unwraps_items_wrapper() {
        let mut base = results();
        let src = node("Quotes", json!({ "items": [{ "fee": 5 }, { "fee": 9 }] }));
        base.insert(src.node_id.clone(), src);
        let m = with_item(base, json!({ "sku": "A-7" }), 1, true);
        assert_eq!(resolve_value("{{ $ancestor('Quotes').fee }}", &m), json!(9));
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
