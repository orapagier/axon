//! Run-scoped trigger staging.
//!
//! A trigger's payload (webhook body, Telegram/WhatsApp event, gmail batch,
//! sub-workflow input, error-workflow failure description, …) is staged here
//! keyed by RUN id before the run starts and consumed (removed) by the trigger
//! node inside that run. Keying by run id — not workflow id, as the old
//! per-source maps did — means two concurrent fires of the SAME workflow can
//! never swap or lose each other's payloads.
//!
//! The entry-node pin (manual play button on one Stimulus, or a sub-workflow's
//! chosen entry trigger) lives here too, under the same key.
//!
//! Plain std Mutexes on purpose: entries are staged from the sync spawn path
//! (`run_in_background_inner`, before the run task exists) or a quick async
//! insert, and every accessor locks briefly without ever holding the guard
//! across an await.

use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

static RUN_TRIGGER_DATA: Lazy<Mutex<HashMap<String, Value>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// The single trigger node a run starts from (absent ⇒ start from every
/// trigger). Set for manual play-button runs and pinned sub-workflow entries.
static RUN_ENTRY_NODE: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn payloads() -> std::sync::MutexGuard<'static, HashMap<String, Value>> {
    RUN_TRIGGER_DATA.lock().unwrap_or_else(|p| p.into_inner())
}

fn entries() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
    RUN_ENTRY_NODE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Stage the trigger payload for a run that is about to start.
pub(crate) fn stage(run_id: &str, payload: Value) {
    payloads().insert(run_id.to_string(), payload);
}

/// Consume the staged payload (single use — the trigger node calls this once).
pub(crate) fn take(run_id: &str) -> Option<Value> {
    payloads().remove(run_id)
}

/// Pin the sole entry trigger node for a run that is about to start.
pub(crate) fn stage_entry_node(run_id: &str, node_id: String) {
    entries().insert(run_id.to_string(), node_id);
}

/// Consume the entry-node pin (single use — `run_inner` calls this while
/// building the start queue).
pub(crate) fn take_entry_node(run_id: &str) -> Option<String> {
    entries().remove(run_id)
}

/// Drop anything still staged for `run_id`. Called when a run ends by any path
/// (and from the queue-shed path, where the run task never starts), so a run
/// that never reaches its trigger node — early load failure, cancelled child,
/// shed fire — cannot leak entries.
pub(crate) fn discard(run_id: &str) {
    payloads().remove(run_id);
    entries().remove(run_id);
}

/// RAII guard: discards a run's staged entries on drop, whatever the exit path.
/// Bind it near the top of `run_inner` (like `CancellationCleanup`).
pub(crate) struct StagedCleanup(String);

impl StagedCleanup {
    pub(crate) fn new(run_id: &str) -> Self {
        Self(run_id.to_string())
    }
}

impl Drop for StagedCleanup {
    fn drop(&mut self) {
        discard(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn concurrent_runs_of_same_workflow_keep_their_own_payloads() {
        // The pre-fix bug: maps were keyed by workflow_id, so run B's payload
        // overwrote run A's. Run-id keys make the payloads independent.
        stage("run-a", json!({"body": "first"}));
        stage("run-b", json!({"body": "second"}));

        assert_eq!(take("run-a"), Some(json!({"body": "first"})));
        assert_eq!(take("run-b"), Some(json!({"body": "second"})));
        // Consumed once — a second read finds nothing.
        assert_eq!(take("run-a"), None);
    }

    #[test]
    fn entry_node_pins_are_per_run() {
        stage_entry_node("run-c", "node-1".into());
        stage_entry_node("run-d", "node-2".into());
        assert_eq!(take_entry_node("run-c").as_deref(), Some("node-1"));
        assert_eq!(take_entry_node("run-d").as_deref(), Some("node-2"));
        assert_eq!(take_entry_node("run-c"), None);
    }

    #[test]
    fn cleanup_guard_discards_unconsumed_entries() {
        // A run that errors before its trigger node executes (bad child id,
        // failed workflow load) must not leak its staged payload.
        stage("run-e", json!({"leak": true}));
        stage_entry_node("run-e", "node-x".into());
        {
            let _guard = StagedCleanup::new("run-e");
            // run dies here without consuming anything
        }
        assert_eq!(take("run-e"), None);
        assert_eq!(take_entry_node("run-e"), None);
    }
}
