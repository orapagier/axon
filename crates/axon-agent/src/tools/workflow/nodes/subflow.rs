//! Sub-workflow ("Execute Workflow") node.
//!
//! Runs another workflow by id or name, hands it this node's input, waits for it
//! to finish, and returns its output so downstream nodes consume it. Reuses the
//! same engine path (`WorkflowEngine::run_with_trigger`) as a normal run, so the
//! child gets a real `workflow_runs` row (history/observability see it) linked to
//! the calling run via `parent_run_id`.
//!
//! Recursion is bounded two ways: a hard depth cap and a per-call cycle check,
//! both carried in the `SUBFLOW_STACK` task-local across the inline child run.
//!
//! Returns a boxed future (not an `async fn`) to break the async-recursion cycle
//! engine → dispatch → sub-workflow → engine, the same pattern `cortex` and
//! `classifier` use.

use crate::state::AppState;
use crate::tools::workflow::{
    NodeResult, WorkflowEngine, SUBFLOW_ENTRY_NODE, SUBFLOW_STACK, SUBFLOW_TRIGGER_DATA,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Hard cap on nested sub-workflow depth. Bounds runaway recursion even when a
/// per-call cycle check can't see an indirect loop in a single hop.
const MAX_SUBFLOW_DEPTH: usize = 8;

pub(crate) fn execute<'a>(
    config: &'a Value,
    state: &'a AppState,
    parent_workflow_id: &'a str,
    parent_run_id: &'a str,
    node_results: &'a HashMap<String, NodeResult>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>> {
    Box::pin(async move {
        let target = config
            .get("workflow_id")
            .and_then(|v| v.as_str())
            .or_else(|| config.get("workflow").and_then(|v| v.as_str()))
            .unwrap_or("")
            .trim()
            .to_string();
        if target.is_empty() {
            return Err("Execute Workflow node: no target workflow selected".to_string());
        }

        // Resolve by id first, then by name (same lookup as the agent run_workflow tool).
        let child_id = {
            let conn = state.db.get().map_err(|e| format!("DB error: {e}"))?;
            conn.query_row(
                "SELECT id FROM workflows WHERE id = ?1 OR name = ?1 LIMIT 1",
                rusqlite::params![target],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| format!("Execute Workflow: '{target}' not found"))?
        };

        if child_id == parent_workflow_id {
            return Err("Execute Workflow node: a workflow cannot call itself".to_string());
        }

        // Recursion / cycle guard via the task-local call stack. The top-level run
        // leaves it unset, so seed it with the calling workflow on the first hop.
        let mut stack = SUBFLOW_STACK.try_with(|s| s.clone()).unwrap_or_default();
        if stack.is_empty() {
            stack.push(parent_workflow_id.to_string());
        }
        if stack.len() >= MAX_SUBFLOW_DEPTH {
            return Err(format!(
                "Execute Workflow node: maximum nesting depth ({MAX_SUBFLOW_DEPTH}) exceeded"
            ));
        }
        if stack.contains(&child_id) {
            return Err(format!(
                "Execute Workflow node: cycle detected — workflow '{child_id}' is already \
                 running in this call chain"
            ));
        }
        stack.push(child_id.clone());

        // Input payload: the explicit `input` field if wired, otherwise the primary
        // upstream item (most recent predecessor by position), else an empty object.
        // An empty/blank string counts as "not wired" (the UI seeds input = "").
        let explicit_input = config.get("input").and_then(|v| match v {
            Value::Null => None,
            Value::String(s) if s.trim().is_empty() => None,
            other => Some(other.clone()),
        });
        let input = explicit_input.unwrap_or_else(|| {
            let mut vec: Vec<_> = node_results.values().cloned().collect();
            vec.sort_by_key(|r| r.position);
            vec.last()
                .map(|r| r.output.clone())
                .unwrap_or_else(|| json!({}))
        });
        SUBFLOW_TRIGGER_DATA
            .lock()
            .await
            .insert(child_id.clone(), input);

        // Run the child inline (same task) so the task-local stack scopes its whole
        // execution and any nested sub-workflows see the updated call chain.
        let fut = WorkflowEngine::run_with_trigger(&child_id, state, "subflow", None, false, None);
        let result = SUBFLOW_STACK
            .scope(stack, fut)
            .await
            .map_err(|e| format!("Execute Workflow run failed: {e}"))?;

        // Link the child run to this parent run for history/observability.
        if let Ok(conn) = state.db.get() {
            let _ = conn.execute(
                "UPDATE workflow_runs SET parent_run_id = ?1 WHERE id = ?2",
                rusqlite::params![parent_run_id, result.run_id],
            );
        }

        if result.status == "error" {
            return Err(format!(
                "Execute Workflow: '{target}' finished with errors (run {})",
                result.run_id
            ));
        }
        if result.status == "waiting" {
            // The child durably suspended at a Wait node. v1 does not chain the
            // parent's suspension to the child; surface it as pending rather than
            // silently reporting completion.
            return Ok(json!({
                "subflow": { "workflow_id": child_id, "run_id": result.run_id, "status": "waiting" },
                "waiting": true,
                "data": result.final_output,
            }));
        }

        let data = result.final_output.clone();
        let items = data.get("items").cloned().unwrap_or_else(|| data.clone());
        Ok(json!({
            "subflow": { "workflow_id": child_id, "run_id": result.run_id, "status": result.status },
            "data": data,
            "items": items,
        }))
    })
}
