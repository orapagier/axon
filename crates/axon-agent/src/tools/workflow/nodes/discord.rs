//! Discord workflow action node.
//!
//! Exposes Discord's REST send paths (the same ones the agent chat gateway in
//! `messaging::discord` uses) as an explicit-operation workflow node. Two auth
//! modes:
//!   • Bot     — `Authorization: Bot <token>` against https://discord.com/api/v10,
//!               with send / embed / edit / delete / react / fetch operations.
//!   • Webhook — POST to a webhook URL, no credential required.
//!
//! Credentials are merged into `config` by `interpolate_config` before this
//! runs, so a bot token arrives as `bot_token` (or `access_token`). Error and
//! JSON handling mirror `tools::telegram`.

use crate::tools::workflow::str_val;
use serde_json::{json, Value};

const API_BASE: &str = "https://discord.com/api/v10";
/// Discord's hard per-message content limit.
const DISCORD_MAX_CHARS: usize = 2000;

// ── Config helpers ──────────────────────────────────────────────────────────

/// Like `str_val` but treats missing/empty/whitespace as a hard error.
fn require(config: &Value, key: &str) -> Result<String, String> {
    match str_val(config, key) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(format!("Missing required field '{key}' in Discord config")),
    }
}

/// Parse a Discord embed color from `#rrggbb`, `0xRRGGBB`, or a decimal integer.
fn parse_color(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix('#') {
        i64::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else {
        t.parse::<i64>().ok()
    }
}

/// Split content into chunks no longer than `max` characters (Discord counts
/// characters, not bytes), preferring to break at a newline within the limit.
fn chunk_chars(text: &str, max: usize) -> Vec<String> {
    if text.chars().count() <= max {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    let mut last_newline: Option<usize> = None;
    for ch in text.chars() {
        current.push(ch);
        count += 1;
        if ch == '\n' {
            last_newline = Some(current.len());
        }
        if count >= max {
            // Prefer breaking at the most recent newline so we don't split lines.
            if let Some(byte_idx) = last_newline.filter(|&i| i < current.len()) {
                let rest = current.split_off(byte_idx);
                chunks.push(std::mem::replace(&mut current, rest));
            } else {
                chunks.push(std::mem::take(&mut current));
            }
            count = current.chars().count();
            last_newline = None;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Build a Discord embed object from `embed_*` config fields. Errors when no
/// field is set, since Discord rejects an empty embed.
fn build_embed(config: &Value) -> Result<Value, String> {
    let mut embed = serde_json::Map::new();
    if let Some(t) = str_val(config, "embed_title") {
        embed.insert("title".into(), json!(t));
    }
    if let Some(d) = str_val(config, "embed_description") {
        embed.insert("description".into(), json!(d));
    }
    if let Some(u) = str_val(config, "embed_url") {
        embed.insert("url".into(), json!(u));
    }
    if let Some(c) = str_val(config, "embed_color").and_then(|s| parse_color(&s)) {
        embed.insert("color".into(), json!(c));
    }
    if embed.is_empty() {
        return Err("Discord embed requires at least a title or description".to_string());
    }
    Ok(Value::Object(embed))
}

// ── HTTP response handling ──────────────────────────────────────────────────

/// Consume a Discord HTTP response: 204 / empty body → `{success:true}`,
/// non-2xx → `Err(<body text>)`, otherwise the parsed JSON.
async fn finish(resp: reqwest::Response) -> Result<Value, String> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Discord: failed to read response: {e}"))?;
    if !status.is_success() {
        return Err(format!("Discord API error {status}: {text}"));
    }
    if text.trim().is_empty() {
        return Ok(json!({ "success": true }));
    }
    serde_json::from_str(&text).map_err(|e| format!("Discord: failed to parse response: {e}"))
}

// ── Operations — bot mode ─────────────────────────────────────────────────────

async fn send_message(
    client: &reqwest::Client,
    auth: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel_id = require(config, "channel_id")?;
    let content = require(config, "content")?;

    let mut last = json!({ "success": true });
    for chunk in chunk_chars(&content, DISCORD_MAX_CHARS) {
        let resp = client
            .post(format!("{API_BASE}/channels/{channel_id}/messages"))
            .header("Authorization", auth)
            .json(&json!({ "content": chunk }))
            .send()
            .await
            .map_err(|e| format!("Discord request error: {e}"))?;
        last = finish(resp).await?;
    }
    Ok(last)
}

async fn send_embed(client: &reqwest::Client, auth: &str, config: &Value) -> Result<Value, String> {
    let channel_id = require(config, "channel_id")?;
    let embed = build_embed(config)?;
    let mut body = serde_json::Map::new();
    body.insert("embeds".into(), json!([embed]));
    if let Some(content) = str_val(config, "content") {
        body.insert("content".into(), json!(content));
    }
    let resp = client
        .post(format!("{API_BASE}/channels/{channel_id}/messages"))
        .header("Authorization", auth)
        .json(&Value::Object(body))
        .send()
        .await
        .map_err(|e| format!("Discord request error: {e}"))?;
    finish(resp).await
}

async fn edit_message(
    client: &reqwest::Client,
    auth: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel_id = require(config, "channel_id")?;
    let message_id = require(config, "message_id")?;
    let content = require(config, "content")?;
    let resp = client
        .patch(format!(
            "{API_BASE}/channels/{channel_id}/messages/{message_id}"
        ))
        .header("Authorization", auth)
        .json(&json!({ "content": content }))
        .send()
        .await
        .map_err(|e| format!("Discord request error: {e}"))?;
    finish(resp).await
}

async fn delete_message(
    client: &reqwest::Client,
    auth: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel_id = require(config, "channel_id")?;
    let message_id = require(config, "message_id")?;
    let resp = client
        .delete(format!(
            "{API_BASE}/channels/{channel_id}/messages/{message_id}"
        ))
        .header("Authorization", auth)
        .send()
        .await
        .map_err(|e| format!("Discord request error: {e}"))?;
    finish(resp).await
}

async fn add_reaction(
    client: &reqwest::Client,
    auth: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel_id = require(config, "channel_id")?;
    let message_id = require(config, "message_id")?;
    let emoji = require(config, "emoji")?;
    // Unicode emoji and custom `name:id` must be percent-encoded in the path.
    let encoded = urlencoding::encode(emoji.trim());
    let resp = client
        .put(format!(
            "{API_BASE}/channels/{channel_id}/messages/{message_id}/reactions/{encoded}/@me"
        ))
        .header("Authorization", auth)
        .send()
        .await
        .map_err(|e| format!("Discord request error: {e}"))?;
    finish(resp).await
}

async fn get_messages(
    client: &reqwest::Client,
    auth: &str,
    config: &Value,
) -> Result<Value, String> {
    let channel_id = require(config, "channel_id")?;
    let limit = str_val(config, "limit")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(50)
        .clamp(1, 100);
    let resp = client
        .get(format!("{API_BASE}/channels/{channel_id}/messages"))
        .header("Authorization", auth)
        .query(&[("limit", limit.to_string())])
        .send()
        .await
        .map_err(|e| format!("Discord request error: {e}"))?;
    finish(resp).await
}

// ── Operations — webhook mode ─────────────────────────────────────────────────

async fn execute_webhook(client: &reqwest::Client, config: &Value) -> Result<Value, String> {
    let url = require(config, "webhook_url")?;
    let mut body = serde_json::Map::new();
    if let Some(content) = str_val(config, "content") {
        body.insert("content".into(), json!(content));
    }
    if let Some(username) = str_val(config, "username") {
        body.insert("username".into(), json!(username));
    }
    if let Some(avatar) = str_val(config, "avatar_url") {
        body.insert("avatar_url".into(), json!(avatar));
    }
    if let Ok(embed) = build_embed(config) {
        body.insert("embeds".into(), json!([embed]));
    }
    if !body.contains_key("content") && !body.contains_key("embeds") {
        return Err(
            "Discord webhook requires 'content' or an embed (title/description)".to_string(),
        );
    }

    // `?wait=true` makes Discord return the created message JSON instead of 204.
    let resp = client
        .post(&url)
        .query(&[("wait", "true")])
        .json(&Value::Object(body))
        .send()
        .await
        .map_err(|e| format!("Discord webhook request error: {e}"))?;
    finish(resp).await
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
            "Missing Discord bot token. Add a credential (service 'discord') with a 'bot_token' \
             field, or switch auth mode to Webhook."
                .to_string()
        })?;
    let auth = format!("Bot {}", token.trim());

    let operation = str_val(config, "operation").unwrap_or_else(|| "sendMessage".to_string());
    match operation.as_str() {
        "sendMessage" => send_message(&client, &auth, config).await,
        "sendEmbed" => send_embed(&client, &auth, config).await,
        "editMessage" => edit_message(&client, &auth, config).await,
        "deleteMessage" => delete_message(&client, &auth, config).await,
        "addReaction" => add_reaction(&client, &auth, config).await,
        "getMessages" => get_messages(&client, &auth, config).await,
        other => Err(format!("Unsupported Discord operation '{other}'")),
    }
}
