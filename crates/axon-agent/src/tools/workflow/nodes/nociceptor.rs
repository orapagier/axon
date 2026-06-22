use crate::error_reporting::send_global_error_notification;
use crate::state::AppState;
use crate::tools::workflow::NodeResult;
use serde_json::Value;

pub(crate) async fn execute_nociceptor_node(
    state: &AppState,
    previous_results: &[NodeResult],
) -> Result<Value, String> {
    use crate::router::model_router::{drain_alerts, format_alerts};

    // 1. Drain recent infrastructure alerts from the Model Router
    let model_alerts = drain_alerts(&state.router).await;
    let model_alert_text = format_alerts(&model_alerts);

    // 2. Identify failures in the current workflow's nodes
    let node_failures: Vec<_> = previous_results
        .iter()
        .filter(|r| r.status == "error")
        .collect();

    if model_alerts.is_empty() && node_failures.is_empty() {
        return Ok(serde_json::json!({
            "status": "No alerts or node failures found",
            "model_alerts": 0,
            "node_failures": 0,
            "dispatched": false
        }));
    }

    // 3. Construct a unified report
    let mut report = String::new();

    if !model_alert_text.is_empty() {
        report.push_str(&model_alert_text);
        report.push_str("\n\n");
    }

    if !node_failures.is_empty() {
        report.push_str("Nociceptor Alert!\n");
        for f in &node_failures {
            report.push_str(&format!(
                "- ❌ Node: '{}' ({}) failed: {}\n",
                f.node_name,
                f.node_type,
                f.error.as_deref().unwrap_or("No error details")
            ));
        }
    }

    let alert_count = model_alerts.len() + node_failures.len();

    match send_global_error_notification(
        state,
        "workflow.nociceptor",
        "Nociceptor captured workflow errors",
        &report,
        None,
        None,
    )
    .await
    {
        Ok(_) => Ok(serde_json::json!({
            "status": format!("Dispatched report with {} items", alert_count),
            "alert_count": alert_count,
            "dispatched": true
        })),
        Err(e) => Ok(serde_json::json!({
            "status": format!("Errors found but global notification failed: {}", e),
            "alert_count": alert_count,
            "dispatched": false
        })),
    }
}
