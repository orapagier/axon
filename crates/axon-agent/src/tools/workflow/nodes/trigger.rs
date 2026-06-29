use crate::state::AppState;
use crate::tools::workflow::{
    execute_gmail_trigger, EXTERNAL_TRIGGER_DATA, SUBFLOW_TRIGGER_DATA, TELEGRAM_TRIGGER_DATA,
    WHATSAPP_TRIGGER_DATA,
};
use serde_json::{json, Value};

pub(crate) async fn execute(
    config: &Value,
    state: &AppState,
    trigger_source: &str,
    workflow_id: &str,
) -> Result<Value, String> {
    // Invoked as a sub-workflow: the parent's input payload is injected here
    // regardless of the configured trigger type, so a reusable workflow can read
    // its caller's data from the trigger node like any other source.
    if trigger_source == "subflow" {
        let mut data = SUBFLOW_TRIGGER_DATA.lock().await;
        if let Some(val) = data.remove(workflow_id) {
            return Ok(val);
        }
        return Ok(json!({"trigger": "subflow"}));
    }
    if config.get("type").and_then(|v| v.as_str()) == Some("gmail") {
        match execute_gmail_trigger(config, state, workflow_id).await {
            Ok(data) => Ok(data),
            Err(e) => {
                tracing::warn!("Gmail trigger fetch failed: {}", e);
                Ok(json!({"trigger": trigger_source, "gmail_error": e}))
            }
        }
    } else if config.get("type").and_then(|v| v.as_str()) == Some("whatsapp") {
        let mut data = WHATSAPP_TRIGGER_DATA.lock().await;
        if let Some(val) = data.remove(workflow_id) {
            Ok(val)
        } else {
            Ok(json!({"trigger": trigger_source}))
        }
    } else if config.get("type").and_then(|v| v.as_str()) == Some("telegram") {
        let mut data = TELEGRAM_TRIGGER_DATA.lock().await;
        if let Some(val) = data.remove(workflow_id) {
            Ok(val)
        } else {
            Ok(json!({"trigger": trigger_source}))
        }
    } else if config.get("type").and_then(|v| v.as_str()) == Some("webhook")
        || config.get("type").and_then(|v| v.as_str()) == Some("github")
        || config.get("type").and_then(|v| v.as_str()) == Some("facebook")
    {
        // GitHub deliveries and Facebook webhook events share the external-trigger
        // map (keyed by workflow_id). For Facebook the payload is the event object
        // {event_type, from_name, from_id, message, object_id, page_id, ...} pushed
        // by `webhook::facebook::fb_event`.
        let mut data = EXTERNAL_TRIGGER_DATA.lock().await;
        if let Some(val) = data.remove(workflow_id) {
            Ok(val)
        } else {
            Ok(json!({"trigger": trigger_source}))
        }
    } else {
        Ok(json!({"trigger": trigger_source}))
    }
}
