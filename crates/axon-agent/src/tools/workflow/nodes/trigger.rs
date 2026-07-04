use crate::state::AppState;
use crate::tools::workflow::{execute_crm_trigger, execute_gmail_trigger, trigger_data};
use serde_json::{json, Value};

pub(crate) async fn execute(
    config: &Value,
    state: &AppState,
    trigger_source: &str,
    workflow_id: &str,
    run_id: &str,
) -> Result<Value, String> {
    // Invoked as a sub-workflow: the parent's input payload was staged keyed by
    // THIS run id, so a reusable workflow can read its caller's data from the
    // trigger node like any other source.
    if trigger_source == "subflow" {
        if let Some(val) = trigger_data::take(run_id) {
            return Ok(val);
        }
        return Ok(json!({"trigger": "subflow"}));
    }
    // Error handler run (A3): the failure description from the workflow that
    // failed was staged for this run. Surfaced like any other trigger payload
    // so the handler can branch on failed_node/error.
    if trigger_source == "error" {
        if let Some(val) = trigger_data::take(run_id) {
            return Ok(val);
        }
        return Ok(json!({"trigger": "error"}));
    }
    let trigger_type = config.get("type").and_then(|v| v.as_str());
    if trigger_type == Some("gmail") {
        match execute_gmail_trigger(config, state, workflow_id, run_id).await {
            Ok(data) => Ok(data),
            Err(e) => {
                tracing::warn!("Gmail trigger fetch failed: {}", e);
                Ok(json!({"trigger": trigger_source, "gmail_error": e}))
            }
        }
    } else if matches!(
        trigger_type,
        // GitHub deliveries and Facebook webhook events ride the same staging
        // as generic webhooks. For Facebook the payload is the event object
        // {event_type, from_name, from_id, message, object_id, page_id, ...}
        // pushed by `webhook::facebook::fb_event`.
        Some("whatsapp") | Some("telegram") | Some("webhook") | Some("github") | Some("facebook")
    ) {
        if let Some(val) = trigger_data::take(run_id) {
            Ok(val)
        } else {
            Ok(json!({"trigger": trigger_source}))
        }
    } else {
        Ok(json!({"trigger": trigger_source}))
    }
}
