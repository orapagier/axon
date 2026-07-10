//! Python code node (n8n gap-closure — and then some: n8n's Python runs in a
//! sandboxed Pyodide/WASM build with no native packages; this runs REAL
//! CPython on the host, so pandas/requests/whatever-is-pip-installed all work.
//! Same trust model as the Shell node: self-hosted, operator-owned machine).
//!
//! The script executes via the system interpreter with a JSON bridge:
//!
//!   - `_json`    — the current input (per-item inside for_each, like `$json`)
//!   - `_item` / `_index` — per-item fan-out context (None outside loops)
//!   - `_node`    — dict of prior results by node name AND id → `{name, id,
//!                  type, json}`  (Python-flavored mirror of `$node`)
//!   - `_results` — ordered list of prior results
//!   - assign to `result` — that value (JSON-serialized) becomes the node's
//!                  output; `print()` output is captured into `logs` and only
//!                  returned when `result` stays None.
//!
//! Interpreter: `pythonBin` config → `AXON_PYTHON` env → `python3`/`python`
//! (`python`/`py` on Windows), first one that spawns. Timeout: `timeout`
//! seconds (default 30) — the process is killed past it.

use crate::tools::workflow::{NodeResult, WorkflowNode, ITEM_CONTEXT_KEY};
use serde_json::{json, Map, Value};

const PY_SCRIPT_MAX_BYTES: usize = 200_000;
const PY_OUTPUT_MAX_BYTES: usize = 4_000_000;

/// Bootstrap run via `python -c`: reads `{script, globals}` JSON on stdin,
/// exec()s the script with the bridge globals, prints `{result, logs}` JSON on
/// stdout. `default=str` keeps non-JSON values (datetime, Decimal) stringy
/// instead of crashing the bridge.
const BOOTSTRAP: &str = r#"
import sys, json, io, contextlib
_payload = json.load(sys.stdin)
_g = dict(_payload.get('globals') or {})
_g['result'] = None
_buf = io.StringIO()
try:
    with contextlib.redirect_stdout(_buf):
        exec(compile(_payload['script'], '<axon-python>', 'exec'), _g)
    _out = {'ok': True, 'result': _g.get('result'), 'logs': _buf.getvalue().splitlines()[:200]}
except Exception as e:
    _out = {'ok': False, 'error': '%s: %s' % (type(e).__name__, e), 'logs': _buf.getvalue().splitlines()[:200]}
sys.stdout.write(json.dumps(_out, default=str))
"#;

/// Interpreter candidates, most specific first.
fn interpreter_candidates(config: &Value) -> Vec<String> {
    let mut c = Vec::new();
    if let Some(bin) = config
        .get("pythonBin")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        c.push(bin.to_string());
    }
    if let Ok(bin) = std::env::var("AXON_PYTHON") {
        if !bin.trim().is_empty() {
            c.push(bin.trim().to_string());
        }
    }
    if cfg!(windows) {
        c.push("python".to_string());
        c.push("py".to_string());
    } else {
        c.push("python3".to_string());
        c.push("python".to_string());
    }
    c
}

/// The bridge globals mirroring the JS node's context, Python-legal names.
fn build_globals(results: &[NodeResult]) -> Map<String, Value> {
    // Per-item context ($item) rides in the results list under the reserved
    // key — split it out exactly like the JS side does.
    let item_ctx = results
        .iter()
        .find(|r| r.node_id == ITEM_CONTEXT_KEY)
        .map(|r| r.output.clone());
    let real: Vec<&NodeResult> = results
        .iter()
        .filter(|r| r.node_id != ITEM_CONTEXT_KEY)
        .collect();

    let mut node_map = Map::new();
    let mut results_list = Vec::with_capacity(real.len());
    for r in &real {
        let entry = json!({
            "name": r.node_name, "id": r.node_id, "type": r.node_type,
            "json": r.output, "data": r.output, "error": r.error,
        });
        node_map.insert(r.node_id.clone(), entry.clone());
        node_map.insert(r.node_name.clone(), entry.clone());
        results_list.push(entry);
    }

    let last_output = real.last().map(|r| r.output.clone()).unwrap_or(Value::Null);
    let (item, index, json_binding) = match &item_ctx {
        Some(ctx) => {
            let item = ctx.get("item").cloned().unwrap_or(Value::Null);
            let index = ctx.get("index").cloned().unwrap_or(Value::Null);
            let override_json = ctx
                .get("override_json")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let jb = if override_json { item.clone() } else { last_output };
            (item, index, jb)
        }
        None => (Value::Null, Value::Null, last_output),
    };

    let mut g = Map::new();
    g.insert("_json".to_string(), json_binding);
    g.insert("_item".to_string(), item);
    g.insert("_index".to_string(), index);
    g.insert("_node".to_string(), Value::Object(node_map));
    g.insert("_results".to_string(), Value::Array(results_list));
    g
}

pub(crate) async fn execute(
    raw_script: &str,
    node: &WorkflowNode,
    results: &[NodeResult],
    config: &Value,
) -> Result<Value, String> {
    if raw_script.trim().is_empty() {
        return Err("No script specified".to_string());
    }
    if raw_script.len() > PY_SCRIPT_MAX_BYTES {
        return Err("Script too large".to_string());
    }
    let timeout_secs = config
        .get("timeout")
        .and_then(|v| v.as_u64())
        .filter(|t| *t > 0)
        .unwrap_or(30)
        .min(600);

    let payload = serde_json::to_vec(&json!({
        "script": raw_script,
        "globals": build_globals(results),
    }))
    .map_err(|e| format!("Payload build error: {e}"))?;

    // First candidate that spawns wins; spawn failure (not found) tries the next.
    let mut child = None;
    let mut tried = Vec::new();
    for bin in interpreter_candidates(config) {
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.arg("-c")
            .arg(BOOTSTRAP)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        match cmd.spawn() {
            Ok(c) => {
                child = Some((bin, c));
                break;
            }
            Err(_) => tried.push(bin),
        }
    }
    let (bin, mut child) = child.ok_or_else(|| {
        format!(
            "No Python interpreter found (tried: {}). Install Python or set 'pythonBin' / AXON_PYTHON.",
            tried.join(", ")
        )
    })?;

    // Feed the payload and collect output under the timeout.
    use tokio::io::AsyncWriteExt;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(&payload)
            .await
            .map_err(|e| format!("Python stdin write failed: {e}"))?;
    }
    drop(child.stdin.take());

    let output = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(format!("Python ({bin}) failed to run: {e}")),
        Err(_) => {
            return Err(format!(
                "Python node '{}' timed out after {timeout_secs}s",
                node.name
            ));
        }
    };

    if output.stdout.len() > PY_OUTPUT_MAX_BYTES {
        return Err("Python output too large".to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bridge: Value = match serde_json::from_str(stdout.trim()) {
        Ok(v) => v,
        Err(_) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Python ({bin}) did not return bridge JSON (exit {:?}). stderr: {}",
                output.status.code(),
                stderr.chars().take(2000).collect::<String>()
            ));
        }
    };

    if bridge.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let logs = bridge.get("logs").cloned().unwrap_or(json!([]));
        let error = bridge
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown Python error");
        return Err(format!(
            "Python error: {error}{}",
            if logs.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                format!(" | logs: {logs}")
            } else {
                String::new()
            }
        ));
    }

    let result = bridge.get("result").cloned().unwrap_or(Value::Null);
    if result.is_null() {
        // No `result` assigned — surface the prints so the node still shows
        // something useful.
        let logs = bridge.get("logs").cloned().unwrap_or(json!([]));
        return Ok(json!({ "logs": logs }));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nr(id: &str, name: &str, position: i64, output: Value) -> NodeResult {
        NodeResult {
            node_id: id.to_string(),
            node_name: name.to_string(),
            node_type: "test".to_string(),
            position,
            status: "success".to_string(),
            output,
            duration_ms: 0,
            error: None,
            attempts: 1,
        }
    }

    fn test_node() -> WorkflowNode {
        WorkflowNode {
            id: "py1".to_string(),
            workflow_id: "wf".to_string(),
            position: 1,
            position_x: 0.0,
            position_y: 0.0,
            node_type: "python".to_string(),
            name: "Python".to_string(),
            config: serde_json::json!({}),
            enabled: true,
            continue_on_fail: false,
            retries: 0,
            retry_wait_ms: 0,
            retry_backoff: "fixed".to_string(),
            pinned_data: None,
        }
    }

    /// Skip tests gracefully on machines without Python.
    fn python_available() -> bool {
        interpreter_candidates(&serde_json::json!({})).iter().any(|bin| {
            std::process::Command::new(bin)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    }

    // The bridge globals mirror the JS context with Python-legal names.
    #[test]
    fn globals_bind_json_node_results() {
        let results = vec![
            nr("a", "First", 0, serde_json::json!({ "x": 1 })),
            nr("b", "Second", 1, serde_json::json!({ "y": 2 })),
        ];
        let g = build_globals(&results);
        assert_eq!(g["_json"], serde_json::json!({ "y": 2 }));
        assert_eq!(g["_node"]["First"]["json"], serde_json::json!({ "x": 1 }));
        assert_eq!(g["_node"]["b"]["json"], serde_json::json!({ "y": 2 }));
        assert_eq!(g["_results"].as_array().unwrap().len(), 2);
        assert_eq!(g["_item"], Value::Null);
    }

    // Per-item context: _item/_index bind and _json overrides on for_each.
    #[test]
    fn globals_bind_item_context() {
        let mut results = vec![nr("a", "First", 0, serde_json::json!({ "x": 1 }))];
        results.push(nr(
            ITEM_CONTEXT_KEY,
            ITEM_CONTEXT_KEY,
            i64::MAX / 2,
            serde_json::json!({ "item": { "sku": "A" }, "index": 2, "override_json": true }),
        ));
        let g = build_globals(&results);
        assert_eq!(g["_item"], serde_json::json!({ "sku": "A" }));
        assert_eq!(g["_index"], serde_json::json!(2));
        assert_eq!(g["_json"], serde_json::json!({ "sku": "A" }));
        // The synthetic entry never leaks into _node/_results.
        assert!(g["_node"].get(ITEM_CONTEXT_KEY).is_none());
        assert_eq!(g["_results"].as_array().unwrap().len(), 1);
    }

    // End-to-end: a real interpreter runs the script and returns `result`.
    #[tokio::test]
    async fn runs_real_python_when_available() {
        if !python_available() {
            eprintln!("skipping: no python interpreter on PATH");
            return;
        }
        let results = vec![nr("a", "Data", 0, serde_json::json!({ "n": 20 }))];
        let out = execute(
            "result = {'doubled': _json['n'] * 2, 'from': _node['Data']['json']['n']}",
            &test_node(),
            &results,
            &serde_json::json!({}),
        )
        .await
        .unwrap();
        assert_eq!(out["doubled"], serde_json::json!(40));
        assert_eq!(out["from"], serde_json::json!(20));
    }

    // A Python exception surfaces as a node error with the exception text.
    #[tokio::test]
    async fn python_exception_is_node_error() {
        if !python_available() {
            eprintln!("skipping: no python interpreter on PATH");
            return;
        }
        let err = execute(
            "raise ValueError('boom')",
            &test_node(),
            &[],
            &serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("ValueError: boom"), "got: {err}");
    }

    // print() output lands in logs when no result is assigned.
    #[tokio::test]
    async fn prints_become_logs() {
        if !python_available() {
            eprintln!("skipping: no python interpreter on PATH");
            return;
        }
        let out = execute(
            "print('hello from py')",
            &test_node(),
            &[],
            &serde_json::json!({}),
        )
        .await
        .unwrap();
        assert_eq!(out["logs"][0], serde_json::json!("hello from py"));
    }
}
