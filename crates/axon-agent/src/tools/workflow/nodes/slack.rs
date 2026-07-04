//! Slack workflow action node.
//!
//! Exposes Slack's Web API send paths (the same ones the agent chat gateway in
//! `messaging::slack` uses) as an explicit-operation workflow node. Two auth
//! modes:
//!   • Bot     — `Authorization: Bearer <token>` against https://slack.com/api,
//!               with post / blocks / update / delete / react operations.
//!   • Webhook — POST to an incoming-webhook URL, no credential required.
//!
//! Credentials are merged into `config` by `interpolate_config` before this
//! runs, so a bot token arrives as `bot_token` (or `access_token`).
//!
//! NOTE: the Slack Web API returns HTTP 200 with `{"ok": false, "error": ...}`
//! on failure (mirrors `messaging::slack`), so success is decided by the `ok`
//! field, not the HTTP status.

use serde_json::{json, Value};

const API_BASE: &str = "https://slack.com/api";

// ── Config helpers ──────────────────────────────────────────────────────────

fn str_val(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        Value::Object(_) | Value::Array(_) => {
            let s = serde_json::to_string(v).unwrap_or_default();
            (!s.is_empty()).then_some(s)
        }
    })
}

fn require(config: &Value, key: &str) -> Result<String, String> {
    match str_val(config, key) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(format!("Missing required field '{key}' in Slack config")),
    }
}

/// Parse the `blocks` textarea (a JSON string) into a Block Kit array. Accepts
/// either a bare array or an object containing a `blocks` array.
fn parse_blocks(config: &Value) -> Result<Value, String> {
    let raw = require(config, "blocks")?;
    let parsed: Value =
        serde_json::from_str(raw.trim()).map_err(|e| format!("Invalid 'blocks' JSON: {e}"))?;
    match parsed {
        Value::Array(_) => Ok(parsed),
        Value::Object(ref o) if o.get("blocks").is_some_and(|b| b.is_array()) => {
            Ok(o.get("blocks").cloned().unwrap())
        }
        _ => Err("'blocks' must be a JSON array of Block Kit blocks".to_string()),
    }
}

// ── HTTP response handling ──────────────────────────────────────────────────

/// Consume a Slack Web API response. Slack signals failure with `ok:false`
/// (HTTP 200), so success is keyed off that field rather than the status code.
async fn finish_api(resp: reqwest::Response) -> Result<Value, String> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Slack: failed to read response: {e}"))?;
    let body: Value =
        serde_json::from_str(&text).map_err(|_| format!("Slack API error {status}: {text}"))?;
    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(format!("Slack API error: {err}"));
    }
    Ok(body)
}

// ── Operations — bot mode ─────────────────────────────────────────────────────

async fn post_message(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel = require(config, "channel")?;
    let text = require(config, "text")?;
    let resp = client
        .post(format!("{API_BASE}/chat.postMessage"))
        .bearer_auth(token)
        .json(&json!({ "channel": channel, "text": text }))
        .send()
        .await
        .map_err(|e| format!("Slack request error: {e}"))?;
    finish_api(resp).await
}

async fn post_blocks(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel = require(config, "channel")?;
    let blocks = parse_blocks(config)?;
    let mut body = serde_json::Map::new();
    body.insert("channel".into(), json!(channel));
    body.insert("blocks".into(), blocks);
    // `text` is the recommended notification/accessibility fallback.
    if let Some(text) = str_val(config, "text") {
        body.insert("text".into(), json!(text));
    }
    let resp = client
        .post(format!("{API_BASE}/chat.postMessage"))
        .bearer_auth(token)
        .json(&Value::Object(body))
        .send()
        .await
        .map_err(|e| format!("Slack request error: {e}"))?;
    finish_api(resp).await
}

async fn update_message(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel = require(config, "channel")?;
    let ts = require(config, "ts")?;
    let text = require(config, "text")?;
    let resp = client
        .post(format!("{API_BASE}/chat.update"))
        .bearer_auth(token)
        .json(&json!({ "channel": channel, "ts": ts, "text": text }))
        .send()
        .await
        .map_err(|e| format!("Slack request error: {e}"))?;
    finish_api(resp).await
}

async fn delete_message(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel = require(config, "channel")?;
    let ts = require(config, "ts")?;
    let resp = client
        .post(format!("{API_BASE}/chat.delete"))
        .bearer_auth(token)
        .json(&json!({ "channel": channel, "ts": ts }))
        .send()
        .await
        .map_err(|e| format!("Slack request error: {e}"))?;
    finish_api(resp).await
}

async fn add_reaction(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel = require(config, "channel")?;
    let timestamp = require(config, "ts")?;
    // Slack reaction names are bare emoji shortcodes without the surrounding `:`.
    let name = require(config, "reaction")?
        .trim()
        .trim_matches(':')
        .to_string();
    let resp = client
        .post(format!("{API_BASE}/reactions.add"))
        .bearer_auth(token)
        .json(&json!({ "channel": channel, "timestamp": timestamp, "name": name }))
        .send()
        .await
        .map_err(|e| format!("Slack request error: {e}"))?;
    finish_api(resp).await
}

// ── Operations — webhook mode ─────────────────────────────────────────────────

async fn execute_webhook(client: &reqwest::Client, config: &Value) -> Result<Value, String> {
    let url = require(config, "webhook_url")?;
    let mut body = serde_json::Map::new();
    // Prefer blocks when supplied; always include text (fallback / plain message).
    if str_val(config, "blocks").is_some() {
        body.insert("blocks".into(), parse_blocks(config)?);
    }
    if let Some(text) = str_val(config, "text") {
        body.insert("text".into(), json!(text));
    }
    if !body.contains_key("text") && !body.contains_key("blocks") {
        return Err("Slack webhook requires 'text' or 'blocks'".to_string());
    }

    // Incoming webhooks return a plain-text `ok` body (HTTP 200) on success.
    let resp = client
        .post(&url)
        .json(&Value::Object(body))
        .send()
        .await
        .map_err(|e| format!("Slack webhook request error: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("Slack webhook error {status}: {text}"));
    }
    Ok(json!({ "success": true }))
}

// ── Public executor ───────────────────────────────────────────────────────────

pub(crate) async fn execute(config: &Value) -> Result<Value, String> {
    let client = crate::http::shared();
    let auth_mode = str_val(config, "auth_mode").unwrap_or_else(|| "bot".to_string());

    if auth_mode.eq_ignore_ascii_case("webhook") {
        return execute_webhook(&client, config).await;
    }

    let token = str_val(config, "bot_token")
        .or_else(|| str_val(config, "access_token"))
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| {
            "Missing Slack bot token. Add a credential (service 'slack') with a 'bot_token' \
             field, or switch auth mode to Webhook."
                .to_string()
        })?;
    let token = token.trim().to_string();

    let operation = str_val(config, "operation").unwrap_or_else(|| "postMessage".to_string());
    match operation.as_str() {
        "postMessage" => post_message(&client, &token, config).await,
        "postBlocks" => post_blocks(&client, &token, config).await,
        "updateMessage" => update_message(&client, &token, config).await,
        "deleteMessage" => delete_message(&client, &token, config).await,
        "addReaction" => add_reaction(&client, &token, config).await,
        other => Err(format!("Unsupported Slack operation '{other}'")),
    }
}
