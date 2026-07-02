use crate::state::AppState;
use crate::tools::workflow::val_to_datetime;
use serde_json::{json, Value};

/// Waits longer than this suspend the run to the database instead of blocking an
/// in-process sleep, so a multi-day wait survives an agent restart (see the
/// engine's `__axon_wait_suspend` handling). Shorter waits sleep in-process so
/// the editor's live run animation keeps flowing for quick "wait a few seconds"
/// steps and so test/partial runs never silently background themselves.
const DURABLE_WAIT_THRESHOLD_SECS: f64 = 60.0;

/// Sentinel key the Wait node sets on its output to tell the engine "suspend
/// this run until `resume_at`" rather than treating the node as complete. The
/// engine strips it before persisting the visible node result.
pub(crate) const SUSPEND_MARKER: &str = "__axon_wait_suspend";

pub(crate) async fn execute(
    config: &Value,
    state: &AppState,
    workflow_id: &str,
    run_id: &str,
    // When false (test/partial runs, or a Wait iterated inside a Loop body), the
    // node always sleeps in-process — it can't durably suspend because there is
    // no single run to resume. The engine sets this.
    durable_allowed: bool,
) -> Result<Value, String> {
    let mode = config
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("interval");

    // C1: human-in-the-loop. "webhook"/"approval" don't wait on a clock — they
    // suspend the run durably until an external caller hits a tokenized resume
    // URL. The engine mints the token, persists the suspend, and surfaces the
    // links (it has the node id + DB); here we just emit the marker. An optional
    // `timeout` (amount+unit) becomes a hard deadline the engine mirrors into
    // resume_at so the time poller can fire a timeout branch.
    if mode == "webhook" || mode == "approval" {
        if !durable_allowed {
            return Err(format!(
                "Wait node ({mode} mode) needs a real run to suspend — it can't \
                 run inside a Loop body or a single-step/test run"
            ));
        }
        let expires_seconds = wait_timeout_seconds(config);
        return Ok(json!({
            SUSPEND_MARKER: { "mode": mode, "expires_seconds": expires_seconds },
            "mode": mode,
            "waiting": true,
        }));
    }

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

    // Resume time, for the UI/run log and for any downstream node that reads it.
    let resume_at = (chrono::Utc::now()
        + chrono::Duration::milliseconds((seconds * 1000.0) as i64))
    .to_rfc3339();

    // Durable path: hand the wait back to the engine, which persists resume_at
    // and frees this task instead of holding a (possibly days-long) sleep. The
    // engine recomputes the stored wake time from `seconds` at suspend instant,
    // so a brief scheduling delay before this returns can't drift the deadline.
    if durable_allowed && seconds > DURABLE_WAIT_THRESHOLD_SECS {
        return Ok(json!({
            SUSPEND_MARKER: { "seconds": seconds, "resume_at": resume_at, "mode": mode },
            "mode": mode,
            "resume_at": resume_at,
            "waiting": true,
        }));
    }

    // Short / non-durable path: sleep in-process in short slices so workflow
    // cancellation takes effect promptly instead of after the full wait.
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

/// C1: optional hard timeout for a webhook/approval Wait, in seconds. Reads
/// `timeout_amount` + `timeout_unit` (same units as the interval form) or a bare
/// `timeout_seconds`. Returns `None` (engine falls back to the configured default
/// TTL) when no positive timeout is set.
fn wait_timeout_seconds(config: &Value) -> Option<f64> {
    if let Some(s) = config.get("timeout_seconds").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    }) {
        return (s > 0.0).then_some(s);
    }
    let amount = config.get("timeout_amount").and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
    })?;
    if amount <= 0.0 {
        return None;
    }
    let unit = config
        .get("timeout_unit")
        .and_then(|v| v.as_str())
        .unwrap_or("hours");
    Some(match unit {
        "seconds" | "s" => amount,
        "minutes" => amount * 60.0,
        "hours" => amount * 3600.0,
        "days" => amount * 86400.0,
        "weeks" => amount * 604_800.0,
        _ => amount * 3600.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_none_when_unset_or_zero() {
        assert_eq!(wait_timeout_seconds(&json!({})), None);
        assert_eq!(wait_timeout_seconds(&json!({ "timeout_amount": 0 })), None);
        assert_eq!(wait_timeout_seconds(&json!({ "timeout_seconds": 0 })), None);
    }

    #[test]
    fn timeout_unit_conversion_and_default() {
        assert_eq!(
            wait_timeout_seconds(&json!({ "timeout_amount": 2, "timeout_unit": "minutes" })),
            Some(120.0)
        );
        // Bare timeout_seconds wins; string numbers parse.
        assert_eq!(
            wait_timeout_seconds(&json!({ "timeout_seconds": "90" })),
            Some(90.0)
        );
        // Default unit is hours.
        assert_eq!(
            wait_timeout_seconds(&json!({ "timeout_amount": 1 })),
            Some(3600.0)
        );
    }
}
