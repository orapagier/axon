use crate::state::AppState;
use crate::tools::workflow::{
    execute_gmail_trigger, EXTERNAL_TRIGGER_DATA, TELEGRAM_TRIGGER_DATA, WHATSAPP_TRIGGER_DATA,
};
use serde_json::{json, Value};

pub(crate) async fn execute(
    config: &Value,
    state: &AppState,
    trigger_source: &str,
    workflow_id: &str,
) -> Result<Value, String> {
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
    } else if config.get("type").and_then(|v| v.as_str()) == Some("webhook") {
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
