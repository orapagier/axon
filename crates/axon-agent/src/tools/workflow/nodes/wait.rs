use crate::state::AppState;
use crate::tools::workflow::val_to_datetime;
use serde_json::{json, Value};

pub(crate) async fn execute(
    config: &Value,
    state: &AppState,
    workflow_id: &str,
    run_id: &str,
) -> Result<Value, String> {
    let mode = config
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("interval");

    // How long to sleep, in seconds.
    let seconds = if mode == "until" {
        // Absolute resume time (n8n "At Specified Time"). A time already in
        // the past resolves to zero wait rather than erroring.
        let until_raw = config
            .get("until")
            .or_else(|| config.get("datetime"))
            .or_else(|| config.get("resume_at"))
            .cloned()
            .unwrap_or(Value::Null);
        match val_to_datetime(&until_raw) {
            Some(dt) => {
                let now = chrono::Utc::now().fixed_offset();
                (((dt - now).num_milliseconds()) as f64 / 1000.0).max(0.0)
            }
            None => {
                return Err("Wait node: 'until' is not a valid date/time \
                     (use ISO 8601, e.g. 2026-06-23T15:30:00Z)"
                    .to_string())
            }
        }
    } else {
        let amount = config
            .get("amount")
            .and_then(|v| {
                if let Some(n) = v.as_f64() {
                    Some(n)
                } else if let Some(s) = v.as_str() {
                    s.trim().parse::<f64>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(1.0);
        let unit = config
            .get("unit")
            .and_then(|v| v.as_str())
            .unwrap_or("seconds");

        (match unit {
            "milliseconds" | "ms" => amount / 1000.0,
            "minutes" => amount * 60.0,
            "hours" => amount * 3600.0,
            "days" => amount * 86400.0,
            "weeks" => amount * 604_800.0,
            _ => amount,
        })
        .max(0.0)
    };

    // Surface the computed resume time so the UI/run log can show when the
    // workflow will continue (and so a downstream node can read it).
    let resume_at = (chrono::Utc::now()
        + chrono::Duration::milliseconds((seconds * 1000.0) as i64))
    .to_rfc3339();

    // Sleep in short slices so workflow cancellation takes effect
    // promptly instead of after the full (possibly days-long) wait.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs_f64(seconds);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let slice = (deadline - now).min(tokio::time::Duration::from_secs(1));
        tokio::time::sleep(slice).await;

        let cancelled = {
            let cancellations = state.workflow_cancellations.lock().await;
            cancellations.contains(workflow_id) || cancellations.contains(run_id)
        };
        if cancelled {
            return Err("Workflow cancelled during wait".to_string());
        }
    }
    Ok(json!({ "waited_seconds": seconds, "resume_at": resume_at, "mode": mode }))
}
