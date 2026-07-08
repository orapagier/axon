//! Respond to Webhook — Task 3.1. Lets a workflow answer the live HTTP request
//! that triggered it with a custom status / headers / body, turning a workflow
//! into an API endpoint instead of a fire-and-forget receiver.
//!
//! Mechanics: when an external webhook delivery targets a workflow that contains
//! this node, the HTTP handler (`webhook/external.rs`) registers a oneshot
//! sender here keyed by RUN id — registered inside `run_in_background_inner`
//! BEFORE the run task spawns, the same ordering invariant `trigger_data`
//! staging relies on, so the node can never race ahead of the registration —
//! and holds the request open. When this node executes it takes that sender and
//! fires the response through it. One shot by construction: a second respond
//! node (or a loop iteration) finds no sender and reports `responded: false`
//! instead of erroring, so manual editor runs and re-runs stay harmless.
//!
//! The run-end RAII guard (`ChannelCleanup`, bound in `run_inner` next to
//! `StagedCleanup`) drops an unfired sender on ANY exit path — respond node on a
//! not-taken branch, run error, durable-wait suspend — which closes the channel
//! and lets the waiting handler fall back to the default ack immediately
//! instead of sitting out its timeout.

use crate::tools::workflow::{cfg_usize, val_to_string};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::oneshot;

// ── The response that crosses the channel ────────────────────────────────────

/// What the respond node sends to the HTTP handler holding the request open.
#[derive(Debug)]
pub(crate) struct WebhookHttpResponse {
    pub(crate) status: u16,
    /// Author-supplied headers, in order. May override the default content-type.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: ResponseBody,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResponseBody {
    Json(Value),
    Text(String),
    Empty,
}

// ── Run-scoped responder registry (mirrors trigger_data's staging maps) ──────

static RESPONDERS: Lazy<Mutex<HashMap<String, oneshot::Sender<WebhookHttpResponse>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn responders(
) -> std::sync::MutexGuard<'static, HashMap<String, oneshot::Sender<WebhookHttpResponse>>> {
    RESPONDERS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Register the waiting HTTP handler's sender for a run that is about to start.
pub(crate) fn register(run_id: &str, tx: oneshot::Sender<WebhookHttpResponse>) {
    responders().insert(run_id.to_string(), tx);
}

/// Take the sender for this run (single use — first respond node wins).
fn take_sender(run_id: &str) -> Option<oneshot::Sender<WebhookHttpResponse>> {
    responders().remove(run_id)
}

/// Drop any unfired sender for `run_id`, closing the channel so the waiting
/// handler unblocks. Called from the run-end guard and the queue-shed path.
pub(crate) fn discard(run_id: &str) {
    responders().remove(run_id);
}

/// RAII guard: discards a run's unfired responder on drop, whatever the exit
/// path. Bind it near the top of `run_inner` (like `StagedCleanup`).
pub(crate) struct ChannelCleanup(String);

impl ChannelCleanup {
    pub(crate) fn new(run_id: &str) -> Self {
        Self(run_id.to_string())
    }
}

impl Drop for ChannelCleanup {
    fn drop(&mut self) {
        discard(&self.0);
    }
}

// ── Pure response assembly (table-tested) ────────────────────────────────────

/// "First incoming item" under the list convention: an array input responds
/// with its first element, anything else responds with the value itself.
fn first_item(input: &Value) -> Value {
    match input {
        Value::Array(a) => a.first().cloned().unwrap_or(json!({})),
        Value::Null => json!({}),
        other => other.clone(),
    }
}

/// Build the HTTP response from the node config + primary input. Pure — all
/// channel/side-effect handling stays in `execute`.
fn build_response(config: &Value, input: &Value) -> Result<WebhookHttpResponse, String> {
    let mode = config
        .get("respondWith")
        .and_then(|v| v.as_str())
        .unwrap_or("firstIncomingItem");

    let body = match mode {
        "firstIncomingItem" => ResponseBody::Json(first_item(input)),
        "json" => {
            let raw = config.get("responseBody").cloned().unwrap_or(Value::Null);
            match raw {
                // An expression that resolved to a real object/array rides as-is.
                v @ (Value::Object(_) | Value::Array(_)) => ResponseBody::Json(v),
                Value::String(s) => {
                    let t = s.trim();
                    if t.is_empty() {
                        ResponseBody::Json(json!({}))
                    } else {
                        match serde_json::from_str::<Value>(t) {
                            Ok(v) => ResponseBody::Json(v),
                            Err(e) => {
                                return Err(format!("Response Body is not valid JSON: {}", e))
                            }
                        }
                    }
                }
                Value::Null => ResponseBody::Json(json!({})),
                other => ResponseBody::Json(other), // bare number/bool is valid JSON
            }
        }
        "text" => {
            let raw = config.get("responseBody").cloned().unwrap_or(Value::Null);
            ResponseBody::Text(match raw {
                Value::Null => String::new(),
                other => val_to_string(&other),
            })
        }
        "noData" => ResponseBody::Empty,
        other => return Err(format!("Unknown Respond With mode: {}", other)),
    };

    let status = match cfg_usize(config, "statusCode") {
        None => 200,
        Some(n) if (100..=599).contains(&n) => n as u16,
        Some(n) => return Err(format!("Status Code {} is not a valid HTTP status", n)),
    };

    // fixedCollection envelope: { "parameters": [ { name, value }, ... ] }.
    let headers: Vec<(String, String)> = config
        .get("responseHeaders")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let name = row.get("name").and_then(|v| v.as_str())?.trim();
                    if name.is_empty() {
                        return None;
                    }
                    let value = row.get("value").map(val_to_string).unwrap_or_default();
                    Some((name.to_string(), value))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(WebhookHttpResponse {
        status,
        headers,
        body,
    })
}

/// Node-result preview of the body (what the canvas shows / downstream reads).
fn body_preview(body: &ResponseBody) -> Value {
    match body {
        ResponseBody::Json(v) => v.clone(),
        ResponseBody::Text(s) => Value::String(s.clone()),
        ResponseBody::Empty => Value::Null,
    }
}

// ── Executor ──────────────────────────────────────────────────────────────────

pub(crate) fn execute(config: &Value, input: &Value, run_id: &str) -> Result<Value, String> {
    let response = build_response(config, input)?;
    let preview = body_preview(&response.body);
    let status = response.status;

    match take_sender(run_id) {
        Some(tx) => match tx.send(response) {
            Ok(()) => Ok(json!({
                "responded": true,
                "statusCode": status,
                "body": preview,
            })),
            // Receiver already gone: the handler timed out (or the caller hung
            // up). The workflow keeps going — this is a note, not a failure.
            Err(_) => Ok(json!({
                "responded": false,
                "statusCode": status,
                "body": preview,
                "note": "The webhook caller stopped waiting before the response was ready (respond timeout). The run continued normally.",
            })),
        },
        // No live request: a manual/editor run, a non-webhook trigger, or a
        // second respond node after one already fired. Preview, don't fail.
        None => Ok(json!({
            "responded": false,
            "statusCode": status,
            "body": preview,
            "note": "No live webhook request was waiting — the response is a preview. It is only sent when the run is started by an external webhook delivery.",
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Default mode: respond with the first incoming item; bare object = itself.
    #[test]
    fn default_mode_responds_with_first_incoming_item() {
        let r = build_response(&json!({}), &json!({"ok": 1})).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, ResponseBody::Json(json!({"ok": 1})));
        assert!(r.headers.is_empty());
    }

    // List convention: an array input responds with its first element.
    #[test]
    fn first_incoming_item_takes_first_array_element() {
        let input = json!([{"a": 1}, {"a": 2}]);
        let r = build_response(&json!({"respondWith": "firstIncomingItem"}), &input).unwrap();
        assert_eq!(r.body, ResponseBody::Json(json!({"a": 1})));
        // Empty array / null input degrade to an empty object, not an error.
        let r = build_response(&json!({}), &json!([])).unwrap();
        assert_eq!(r.body, ResponseBody::Json(json!({})));
        let r = build_response(&json!({}), &Value::Null).unwrap();
        assert_eq!(r.body, ResponseBody::Json(json!({})));
    }

    // json mode parses a string body; an expression-resolved object rides as-is.
    #[test]
    fn json_mode_parses_string_and_passes_objects() {
        let cfg = json!({"respondWith": "json", "responseBody": "{\"msg\": \"hi\"}"});
        let r = build_response(&cfg, &Value::Null).unwrap();
        assert_eq!(r.body, ResponseBody::Json(json!({"msg": "hi"})));

        let cfg = json!({"respondWith": "json", "responseBody": {"already": "obj"}});
        let r = build_response(&cfg, &Value::Null).unwrap();
        assert_eq!(r.body, ResponseBody::Json(json!({"already": "obj"})));
    }

    #[test]
    fn json_mode_rejects_invalid_json() {
        let cfg = json!({"respondWith": "json", "responseBody": "{nope"});
        let err = build_response(&cfg, &Value::Null).unwrap_err();
        assert!(err.contains("not valid JSON"), "got: {err}");
    }

    // Blank/missing json body → {} (a respond node dropped in unconfigured
    // still answers something sensible).
    #[test]
    fn json_mode_blank_body_is_empty_object() {
        let cfg = json!({"respondWith": "json", "responseBody": ""});
        let r = build_response(&cfg, &Value::Null).unwrap();
        assert_eq!(r.body, ResponseBody::Json(json!({})));
    }

    // text mode coerces non-strings (an expression may resolve to a number).
    #[test]
    fn text_mode_coerces_to_string() {
        let cfg = json!({"respondWith": "text", "responseBody": "done"});
        let r = build_response(&cfg, &Value::Null).unwrap();
        assert_eq!(r.body, ResponseBody::Text("done".into()));

        let cfg = json!({"respondWith": "text", "responseBody": 42});
        let r = build_response(&cfg, &Value::Null).unwrap();
        assert_eq!(r.body, ResponseBody::Text("42".into()));
    }

    #[test]
    fn no_data_mode_sends_empty_body() {
        let cfg = json!({"respondWith": "noData", "statusCode": 204});
        let r = build_response(&cfg, &Value::Null).unwrap();
        assert_eq!(r.body, ResponseBody::Empty);
        assert_eq!(r.status, 204);
    }

    // Status codes: default, custom (incl. string form via cfg_usize), invalid.
    #[test]
    fn status_code_validation() {
        let r = build_response(&json!({"statusCode": 201}), &json!({})).unwrap();
        assert_eq!(r.status, 201);
        let r = build_response(&json!({"statusCode": "404"}), &json!({})).unwrap();
        assert_eq!(r.status, 404);
        assert!(build_response(&json!({"statusCode": 42}), &json!({})).is_err());
        assert!(build_response(&json!({"statusCode": 9000}), &json!({})).is_err());
    }

    // Headers ride the fixedCollection envelope; blank names are skipped.
    #[test]
    fn headers_parse_from_fixed_collection() {
        let cfg = json!({
            "responseHeaders": { "parameters": [
                { "name": "X-Request-Id", "value": "abc" },
                { "name": "  ", "value": "skipped" },
                { "name": "Retry-After", "value": 30 },
            ]}
        });
        let r = build_response(&cfg, &json!({})).unwrap();
        assert_eq!(
            r.headers,
            vec![
                ("X-Request-Id".to_string(), "abc".to_string()),
                ("Retry-After".to_string(), "30".to_string()),
            ]
        );
    }

    #[test]
    fn unknown_mode_errors() {
        assert!(build_response(&json!({"respondWith": "streamed"}), &json!({})).is_err());
    }

    // Registry: the registered sender receives the node's response.
    #[test]
    fn execute_fires_registered_channel_once() {
        let (tx, mut rx) = oneshot::channel();
        register("run-resp-1", tx);

        let out = execute(
            &json!({"statusCode": 202}),
            &json!({"ok": true}),
            "run-resp-1",
        )
        .unwrap();
        assert_eq!(out["responded"], json!(true));
        assert_eq!(out["statusCode"], json!(202));

        let sent = rx.try_recv().expect("response should have been sent");
        assert_eq!(sent.status, 202);
        assert_eq!(sent.body, ResponseBody::Json(json!({"ok": true})));

        // Second respond in the same run: sender consumed → preview, not error.
        let out = execute(&json!({}), &json!({}), "run-resp-1").unwrap();
        assert_eq!(out["responded"], json!(false));
    }

    // No waiter (manual editor run) → responded:false preview, never an error.
    #[test]
    fn execute_without_waiter_previews() {
        let out = execute(&json!({}), &json!({"x": 1}), "run-resp-none").unwrap();
        assert_eq!(out["responded"], json!(false));
        assert_eq!(out["body"], json!({"x": 1}));
        assert!(out["note"].as_str().unwrap().contains("preview"));
    }

    // The run-end guard drops an unfired sender so the handler unblocks.
    #[test]
    fn cleanup_guard_closes_channel() {
        let (tx, mut rx) = oneshot::channel::<WebhookHttpResponse>();
        register("run-resp-2", tx);
        {
            let _guard = ChannelCleanup::new("run-resp-2");
            // run ends here without the respond node firing
        }
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }
}
