// telegram.rs
// Telegram integration node for the workflow engine.
// Mirrors n8n's Telegram + TelegramTrigger nodes.
//
// Supported operations
// ─────────────────────
// message  : sendMessage, sendPhoto, sendVideo, sendAudio, sendDocument,
//            sendAnimation, sendSticker, sendLocation, sendMediaGroup,
//            editMessageText, deleteMessage, pinChatMessage, unpinChatMessage
// chat     : getChat, getChatAdministrators, getChatMember,
//            setChatTitle, setChatDescription, sendChatAction, leaveChat
// callback : answerQuery   (answerCallbackQuery)
//
// Config keys (all pulled from config: &Value)
// ─────────────────────────────────────────────
//   access_token  – Telegram bot token
//   base_url      – default "https://api.telegram.org"
//   resource      – "message" | "chat" | "callback"
//   operation     – see list above
//   + per-operation keys documented inline

use anyhow::Result;
use base64::Engine;
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

use crate::tools::schema::{ToolDefinition, ToolSource};

// ── Credentials ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct TelegramCredentials {
    access_token: String,
    base_url: String,
}

impl TelegramCredentials {
    fn from_config(config: &Value) -> Result<Self, String> {
        let access_token = str_val(config, "bot_token")
            .or_else(|| str_val(config, "access_token"))
            .ok_or_else(|| {
                "Missing required field 'bot_token' or 'access_token' in config".to_string()
            })?;
        let base_url =
            str_val(config, "base_url").unwrap_or_else(|| "https://api.telegram.org".to_string());
        let base_url = base_url.trim_end_matches('/').to_string();

        Ok(Self {
            access_token,
            base_url,
        })
    }

    fn api_url(&self, endpoint: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.access_token, endpoint)
    }
}

// ── HTTP client ───────────────────────────────────────────────────────────────

struct TelegramClient {
    http: reqwest::Client,
    creds: TelegramCredentials,
}

impl TelegramClient {
    fn new(creds: TelegramCredentials) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build Telegram HTTP client");
        Self { http, creds }
    }

    /// POST a JSON body to a Telegram Bot API endpoint.
    async fn post(&self, endpoint: &str, body: Value) -> Result<Value, String> {
        let url = self.creds.api_url(endpoint);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Telegram request error: {e}"))?;

        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Telegram response: {e}"))?;

        if !status.is_success() {
            let desc = json
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown Telegram API error");
            return Err(format!("Telegram API error {status}: {desc}"));
        }

        // Unwrap the `result` field that Telegram always wraps responses in.
        Ok(json.get("result").cloned().unwrap_or(json))
    }

    /// POST multipart form (for binary file uploads).
    async fn post_multipart(&self, endpoint: &str, form: multipart::Form) -> Result<Value, String> {
        let url = self.creds.api_url(endpoint);
        let resp = self
            .http
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("Telegram multipart error: {e}"))?;

        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Telegram multipart response: {e}"))?;

        if !status.is_success() {
            let desc = json
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown Telegram API error");
            return Err(format!("Telegram API error {status}: {desc}"));
        }

        Ok(json.get("result").cloned().unwrap_or(json))
    }

    /// GET a Telegram Bot API endpoint (e.g. getFile, getChat).
    #[allow(dead_code)]
    async fn get(&self, endpoint: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let url = self.creds.api_url(endpoint);
        let resp = self
            .http
            .get(&url)
            .query(query)
            .send()
            .await
            .map_err(|e| format!("Telegram GET error: {e}"))?;

        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Telegram GET response: {e}"))?;

        if !status.is_success() {
            let desc = json
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown Telegram API error");
            return Err(format!("Telegram API error {status}: {desc}"));
        }

        Ok(json.get("result").cloned().unwrap_or(json))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn str_val(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        // Objects/Arrays: stringify so workflow expressions resolving
        // to structured data still produce a usable string value.
        Value::Object(_) | Value::Array(_) => {
            let s = serde_json::to_string(v).unwrap_or_default();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
    })
}

fn require_str(config: &Value, key: &str) -> Result<String, String> {
    str_val(config, key).ok_or_else(|| format!("Missing required field '{key}' in Telegram config"))
}

fn bool_val(config: &Value, key: &str) -> bool {
    config.get(key).and_then(parse_bool_like).unwrap_or(false)
}

fn bool_val_item(item: &Value, key: &str) -> bool {
    item.get(key).and_then(parse_bool_like).unwrap_or(false)
}

fn parse_bool_like(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::String(s) => {
            let normalized = s.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            }
        }
        Value::Number(n) => {
            if n.as_i64() == Some(1) {
                Some(true)
            } else if n.as_i64() == Some(0) {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn scalar_string_val(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// ── Binary file handling ────────────────────────────────────────────────────
//
// The "binary file object" passed between workflow nodes is NOT standardized.
// Producers in this codebase emit two different shapes:
//   • Telegram trigger download + Myelin → camelCase: localPath / fileName / mimeType
//   • getFile + AttachedFile + UI download → snake_case: local_path / original_name / mime_type
// On top of that a user may type a literal server path string (e.g.
// "/data/files/report.pdf") into the "File" field, or reference a whole upstream
// node output where the file object is nested under `binary` / `data` / `file`.
//
// `extract_file_descriptor` accepts every one of those shapes and returns
// (local_path, file_name?, mime_type?). `binary_descriptor` is the inverse: it
// builds an output object carrying BOTH naming conventions so any downstream
// consumer (old or new) resolves it.

struct ResolvedFile {
    bytes: Vec<u8>,
    file_name: String,
    mime_type: String,
}

/// Pull (local_path, file_name?, mime_type?) out of a `file` config value,
/// tolerating string paths, camelCase, snake_case, and nested wrappers.
/// Shared with the Myelin node so file objects resolve identically everywhere.
pub(crate) fn extract_file_descriptor(
    file_val: &Value,
) -> Option<(String, Option<String>, Option<String>)> {
    // 1. A bare string is the path itself.
    if let Some(s) = file_val.as_str() {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        // A reference that resolved through string-interpolation can arrive as a
        // JSON-stringified object — recover it rather than treating it as a path.
        if s.starts_with('{') && s.ends_with('}') {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                if parsed.is_object() {
                    return extract_file_descriptor(&parsed);
                }
            }
        }
        return Some((s.to_string(), None, None));
    }

    let obj = file_val.as_object()?;

    // 2. The metadata may be nested under a wrapper key when the user references
    //    a whole upstream node output (e.g. {{ $node["Trigger"] }}).
    if let Some(inner) = ["binary", "data", "file"]
        .iter()
        .find_map(|k| obj.get(*k).filter(|v| v.is_object() || v.is_string()))
    {
        if let Some(found) = extract_file_descriptor(inner) {
            return Some(found);
        }
    }

    let pick = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
            .map(|s| s.to_string())
    };

    let local_path = pick(&["local_path", "localPath", "path", "file_path", "filePath"])?;
    let file_name = pick(&["original_name", "fileName", "file_name", "name"]);
    let mime_type = pick(&["mime_type", "mimeType", "mime", "content_type", "contentType"]);
    Some((local_path, file_name, mime_type))
}

/// Resolve and read the binary referenced by `config["file"]`.
/// Returns Ok(None) when no usable `file` value is present (lets callers such as
/// sendDocument fall back to base64 `file_bytes`).
async fn try_read_binary_file(
    config: &Value,
    default_name: &str,
) -> Result<Option<ResolvedFile>, String> {
    let Some((local_path, name, mime)) = config.get("file").and_then(extract_file_descriptor)
    else {
        return Ok(None);
    };

    let bytes = tokio::fs::read(&local_path)
        .await
        .map_err(|e| format!("Failed to read file '{local_path}': {e}"))?;

    // Prefer the supplied name, then the path's basename, then the type default.
    let file_name = name
        .or_else(|| {
            std::path::Path::new(&local_path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| default_name.to_string());

    let mime_type = mime.unwrap_or_else(|| {
        mime_guess::from_path(&file_name)
            .first_or_octet_stream()
            .to_string()
    });

    Ok(Some(ResolvedFile {
        bytes,
        file_name,
        mime_type,
    }))
}

/// Same as `try_read_binary_file` but errors when no `file` is provided.
async fn read_binary_file(config: &Value, default_name: &str) -> Result<ResolvedFile, String> {
    try_read_binary_file(config, default_name)
        .await?
        .ok_or_else(|| {
            "Binary Data is enabled but the 'File' field is empty or unrecognized. Provide a \
             server file path (e.g. /data/files/your-file.pdf) or a binary object from a \
             previous node (containing local_path / localPath)."
                .to_string()
        })
}

/// Build a multipart part with an explicit filename and MIME type.
fn file_part(bytes: Vec<u8>, file_name: String, mime_type: &str) -> Result<multipart::Part, String> {
    multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str(mime_type)
        .map_err(|e| format!("Invalid MIME type '{mime_type}': {e}"))
}

/// Standardized binary-file descriptor emitted by producer operations, carrying
/// both snake_case and camelCase keys so every downstream consumer resolves it.
/// Shared with the Myelin node so stored/retrieved files use the same shape.
pub(crate) fn binary_descriptor(
    local_path: &str,
    file_name: &str,
    mime: &str,
    size: usize,
) -> Value {
    json!({
        "local_path": local_path,
        "localPath": local_path,
        "original_name": file_name,
        "file_name": file_name,
        "fileName": file_name,
        "mime_type": mime,
        "mimeType": mime,
        "size": size,
        "fileSize": size,
    })
}

fn button_string_field(button: &Value, key: &str) -> Option<String> {
    button
        .get(key)
        .and_then(scalar_string_val)
        .or_else(|| {
            button
                .get("additionalFields")
                .and_then(|af| af.get(key))
                .and_then(scalar_string_val)
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn button_route_to_trigger(button: &Value) -> bool {
    bool_val_item(button, "route_to_trigger")
        || button
            .get("additionalFields")
            .map(|af| bool_val_item(af, "route_to_trigger"))
            .unwrap_or(false)
}

pub(crate) fn encode_callback_data(callback_data: &str, route_to_trigger: bool) -> String {
    let trimmed = callback_data.trim();
    let normalized = trimmed
        .strip_prefix("trig:")
        .or_else(|| trimmed.strip_prefix("agent:"))
        .unwrap_or(trimmed);

    if route_to_trigger {
        format!("trig:{normalized}")
    } else {
        format!("agent:{normalized}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InlineKeyboardButtonConfig {
    pub text: Option<String>,
    pub url: Option<String>,
    pub callback_data: Option<String>,
    pub switch_inline_query: Option<String>,
    pub route_to_trigger: bool,
}

fn parse_inline_keyboard_button(button: &Value) -> Option<InlineKeyboardButtonConfig> {
    let parsed = InlineKeyboardButtonConfig {
        text: button_string_field(button, "text"),
        url: button_string_field(button, "url"),
        callback_data: button_string_field(button, "callback_data"),
        switch_inline_query: button_string_field(button, "switch_inline_query"),
        route_to_trigger: button_route_to_trigger(button),
    };

    if parsed.text.is_none()
        && parsed.url.is_none()
        && parsed.callback_data.is_none()
        && parsed.switch_inline_query.is_none()
    {
        None
    } else {
        Some(parsed)
    }
}

pub(crate) fn collect_inline_keyboard_buttons(
    config: &Value,
) -> Vec<Vec<InlineKeyboardButtonConfig>> {
    let Some(ik) = config.get("inline_keyboard") else {
        return Vec::new();
    };

    let ik_obj = if let Some(s) = ik.as_str() {
        serde_json::from_str::<Value>(s).unwrap_or_else(|_| ik.clone())
    } else {
        ik.clone()
    };

    let mut keyboard = Vec::new();

    if let Some(rows) = ik_obj.get("rows").and_then(|v| v.as_array()) {
        for row in rows {
            let Some(buttons) = row
                .get("row")
                .and_then(|r| r.get("buttons"))
                .and_then(|b| b.as_array())
                .or_else(|| row.get("buttons").and_then(|v| v.as_array()))
            else {
                continue;
            };

            let parsed_row: Vec<InlineKeyboardButtonConfig> = buttons
                .iter()
                .filter_map(parse_inline_keyboard_button)
                .collect();
            if !parsed_row.is_empty() {
                keyboard.push(parsed_row);
            }
        }
    } else if let Some(buttons) = ik_obj
        .get("parameters")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .or_else(|| ik_obj.get("buttons").and_then(|v| v.as_array()))
    {
        for button in buttons.iter().filter_map(parse_inline_keyboard_button) {
            keyboard.push(vec![button]);
        }
    }

    keyboard
}

/// Build the standard attribution footer used by n8n.
fn attribution_footer(instance_id: &str) -> String {
    if instance_id.is_empty() {
        format!("\n\n_This message was sent automatically with _[n8n](https://n8n.io/?utm_source=n8n-internal&utm_medium=powered_by&utm_campaign=n8n-nodes-base.telegram)")
    } else {
        format!("\n\n_This message was sent automatically with _[n8n](https://n8n.io/?utm_source=n8n-internal&utm_medium=powered_by&utm_campaign=n8n-nodes-base.telegram_{instance_id})")
    }
}

/// If config has an `additionalFields` sub-object (from the UI collection drawer),
/// promote its keys to the top level so existing lookups work unchanged.
fn flatten_additional_fields(config: &Value) -> Value {
    let mut merged = config.clone();
    if let Value::Object(ref mut map) = merged {
        if let Some(Value::Object(af)) = config.get("additionalFields").cloned() {
            for (k, v) in af {
                // Don't overwrite top-level keys (e.g. chat_id, text)
                if !map.contains_key(&k) {
                    map.insert(k, v);
                }
            }
        }
    }
    merged
}

/// Resolve the effective parse mode, honouring values nested in `additionalFields`.
fn resolve_parse_mode(config: &Value) -> String {
    str_val(&flatten_additional_fields(config), "parse_mode")
        .unwrap_or_else(|| "Markdown".to_string())
}

fn is_html_parse_mode(config: &Value) -> bool {
    resolve_parse_mode(config).eq_ignore_ascii_case("html")
}

/// Tags Telegram's HTML parse mode understands. Anything else that looks like a
/// tag (e.g. an email address `<noreply@example.com>`) gets escaped so the Bot
/// API doesn't reject the whole message with "Unsupported start tag".
const TELEGRAM_HTML_TAGS: &[&str] = &[
    "b",
    "strong",
    "i",
    "em",
    "u",
    "ins",
    "s",
    "strike",
    "del",
    "span",
    "tg-spoiler",
    "tg-emoji",
    "a",
    "code",
    "pre",
    "blockquote",
];

/// Escape stray `<`, `>`, and `&` for Telegram's HTML parse mode while leaving
/// valid Telegram tags and HTML entities intact. Telegram requires that any of
/// these three characters not forming part of a tag/entity be escaped, otherwise
/// it aborts with `can't parse entities`.
fn escape_html_for_telegram(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len() + 16);
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '<' => match valid_tag_end(&chars, i) {
                Some(end) => {
                    out.extend(chars[i..end].iter());
                    i = end;
                }
                None => {
                    out.push_str("&lt;");
                    i += 1;
                }
            },
            '>' => {
                out.push_str("&gt;");
                i += 1;
            }
            '&' => match entity_end(&chars, i) {
                Some(end) => {
                    out.extend(chars[i..end].iter());
                    i = end;
                }
                None => {
                    out.push_str("&amp;");
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// If a valid Telegram HTML tag starts at `start` (where `chars[start] == '<'`),
/// return the index just past its closing `>`; otherwise `None`.
fn valid_tag_end(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start + 1;
    let closing = chars.get(j) == Some(&'/');
    if closing {
        j += 1;
    }
    // Tag name: ASCII letters/digits plus '-' (for tg-spoiler / tg-emoji).
    let name_start = j;
    while matches!(chars.get(j), Some(c) if c.is_ascii_alphanumeric() || *c == '-') {
        j += 1;
    }
    let name: String = chars[name_start..j].iter().collect::<String>().to_lowercase();
    if name.is_empty() || !TELEGRAM_HTML_TAGS.contains(&name.as_str()) {
        return None;
    }

    // Closing tag: optional spaces, then '>'.
    if closing {
        while chars.get(j) == Some(&' ') {
            j += 1;
        }
        return (chars.get(j) == Some(&'>')).then_some(j + 1);
    }

    match chars.get(j) {
        // Opening tag with no attributes.
        Some('>') => Some(j + 1),
        // Attributes are only valid on a subset of tags; scan to the matching
        // '>', skipping over quoted attribute values so a '>' inside a value
        // (e.g. an href) doesn't end the tag early.
        Some(c) if c.is_whitespace() => {
            const ATTR_TAGS: &[&str] = &["a", "span", "code", "pre", "blockquote", "tg-emoji"];
            if !ATTR_TAGS.contains(&name.as_str()) {
                return None;
            }
            let mut quote: Option<char> = None;
            while let Some(&c) = chars.get(j) {
                match quote {
                    Some(q) if c == q => quote = None,
                    Some(_) => {}
                    None => match c {
                        '"' | '\'' => quote = Some(c),
                        '<' => return None,
                        '>' => return Some(j + 1),
                        _ => {}
                    },
                }
                j += 1;
            }
            None
        }
        _ => None,
    }
}

/// If a valid HTML entity starts at `start` (where `chars[start] == '&'`),
/// return the index just past its `;`; otherwise `None`.
fn entity_end(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start + 1;
    if chars.get(j) == Some(&'#') {
        // Numeric: &#123; (decimal) or &#x1F600; (hex).
        j += 1;
        let hex = matches!(chars.get(j), Some('x') | Some('X'));
        if hex {
            j += 1;
        }
        let digits_start = j;
        while matches!(chars.get(j), Some(c) if if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() })
        {
            j += 1;
        }
        if j == digits_start {
            return None;
        }
    } else {
        // Named: &amp; &lt; …
        let name_start = j;
        while matches!(chars.get(j), Some(c) if c.is_ascii_alphanumeric()) {
            j += 1;
        }
        if j == name_start {
            return None;
        }
    }
    (chars.get(j) == Some(&';')).then_some(j + 1)
}

/// Merge `parse_mode`, `disable_web_page_preview`, reply markup, and the
/// optional attribution footer into `body` — mirrors `addAdditionalFields`.
fn apply_additional_fields(body: &mut serde_json::Map<String, Value>, config: &Value) {
    // Flatten "additionalFields" sub-object so legacy field lookups still work
    let config = &flatten_additional_fields(config);
    // parse_mode (default Markdown, same as n8n)
    let parse_mode = str_val(config, "parse_mode").unwrap_or_else(|| "Markdown".to_string());
    if parse_mode.to_lowercase() != "none" {
        body.insert("parse_mode".into(), json!(parse_mode));
    }

    // In HTML parse mode, escape stray markup in captions so the Bot API doesn't
    // reject media messages. (Message `text` is escaped before chunking in
    // send_message / edit_message_text so the length check sees the final text.)
    if parse_mode.eq_ignore_ascii_case("html") {
        if let Some(caption) = body.get("caption").and_then(|v| v.as_str()).map(str::to_string) {
            body.insert("caption".into(), json!(escape_html_for_telegram(&caption)));
        }
    }

    // disable_web_page_preview (default true)
    let dwpp = config
        .get("disable_web_page_preview")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    body.insert("disable_web_page_preview".into(), json!(dwpp));

    // append_attribution
    if bool_val(config, "append_attribution") {
        let instance_id = str_val(config, "instance_id").unwrap_or_default();
        if let Some(text) = body
            .get_mut("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        {
            let new_text = format!("{}{}", text, attribution_footer(&instance_id));
            body.insert("text".into(), json!(new_text));
        }
    }

    // reply_to_message_id
    if let Some(reply_id) = str_val(config, "reply_to_message_id") {
        if !reply_id.is_empty() {
            if let Ok(id) = reply_id.parse::<i64>() {
                body.insert("reply_to_message_id".into(), json!(id));
            }
        }
    }

    // reply_markup: inlineKeyboard | forceReply | replyKeyboardRemove | replyKeyboard
    let markup_type = str_val(config, "reply_markup").unwrap_or_else(|| "none".to_string());
    match markup_type.as_str() {
        "inlineKeyboard" => {
            let mut keyboard: Vec<Vec<Value>> = Vec::new();

            for row in collect_inline_keyboard_buttons(config) {
                let mut btn_vec = Vec::new();
                for button in row {
                    let mut btn = serde_json::Map::new();
                    if let Some(text) = button.text.clone() {
                        btn.insert("text".into(), json!(text));
                    }
                    if let Some(url) = button.url.clone() {
                        btn.insert("url".into(), json!(url));
                    }
                    if let Some(callback_data) = button.callback_data.clone() {
                        let encoded = encode_callback_data(&callback_data, button.route_to_trigger);
                        tracing::info!(
                            "[TELEGRAM] Button '{}': route_to_trigger={} final_callback_data='{}'",
                            button.text.as_deref().unwrap_or("?"),
                            button.route_to_trigger,
                            encoded
                        );
                        btn.insert("callback_data".into(), json!(encoded));
                    }
                    if let Some(switch_inline_query) = button.switch_inline_query.clone() {
                        btn.insert("switch_inline_query".into(), json!(switch_inline_query));
                    }
                    if !btn.is_empty() {
                        btn_vec.push(Value::Object(btn));
                    }
                }
                if !btn_vec.is_empty() {
                    keyboard.push(btn_vec);
                }
            }

            if !keyboard.is_empty() {
                body.insert(
                    "reply_markup".into(),
                    json!({ "inline_keyboard": keyboard }),
                );
            }
        }
        "forceReply" => {
            if let Some(fr) = config.get("force_reply") {
                body.insert("reply_markup".into(), fr.clone());
            }
        }
        "replyKeyboardRemove" => {
            if let Some(rkr) = config.get("reply_keyboard_remove") {
                body.insert("reply_markup".into(), rkr.clone());
            }
        }
        "replyKeyboard" => {
            if let Some(rk) = config.get("reply_keyboard") {
                body.insert("reply_markup".into(), rk.clone());
            }
        }
        _ => {} // "none"
    }
}

// ── Operations — message resource ─────────────────────────────────────────────

/// Telegram's hard limit for a single sendMessage call.
const TELEGRAM_MAX_TEXT: usize = 4096;

async fn send_message(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let mut text = require_str(config, "text")?;

    // In HTML parse mode, escape stray '<', '>', '&' (e.g. "<noreply@x.com>")
    // up front so the length check and chunk boundaries operate on the final,
    // escaped text rather than splitting it after the fact.
    if is_html_parse_mode(config) {
        text = escape_html_for_telegram(&text);
    }

    // Auto-chunk messages exceeding Telegram's 4096-char limit.
    if text.len() > TELEGRAM_MAX_TEXT {
        let chunks = chunk_text(&text, TELEGRAM_MAX_TEXT);
        let mut last_result = json!({});
        for (i, chunk) in chunks.iter().enumerate() {
            let mut body = serde_json::Map::new();
            body.insert("chat_id".into(), json!(chat_id));
            body.insert("text".into(), json!(chunk));
            if i == 0 {
                apply_additional_fields(&mut body, config);
            } else {
                // Inherit parse_mode for continuations (text is already escaped).
                let pm = resolve_parse_mode(config);
                if !pm.eq_ignore_ascii_case("none") {
                    body.insert("parse_mode".into(), json!(pm));
                }
            }
            last_result = client.post("sendMessage", Value::Object(body)).await?;
        }
        return Ok(last_result);
    }

    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("text".into(), json!(text));
    apply_additional_fields(&mut body, config);

    client.post("sendMessage", Value::Object(body)).await
}

/// Split a long message into chunks, preferring to break at newlines.
fn chunk_text(text: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > max {
        // Try to break at the last newline within the limit
        let split_at = remaining[..max]
            .rfind('\n')
            .map(|i| i + 1) // include the newline in the current chunk
            .unwrap_or(max); // fall back to hard cut
        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}

async fn send_photo(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;

    if bool_val(config, "binary_data") {
        let file = read_binary_file(config, "photo.jpg").await?;
        let part = file_part(file.bytes, file.file_name, &file.mime_type)?;

        let mut form = multipart::Form::new()
            .text("chat_id", chat_id)
            .part("photo", part);

        if let Some(caption) = str_val(config, "caption") {
            form = form.text("caption", caption);
        }
        apply_additional_fields_to_form(&mut form, config);

        return client.post_multipart("sendPhoto", form).await;
    }

    let photo = require_str(config, "photo")?; // file_id or URL
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("photo".into(), json!(photo));
    if let Some(caption) = str_val(config, "caption") {
        body.insert("caption".into(), json!(caption));
    }
    apply_additional_fields(&mut body, config);

    client.post("sendPhoto", Value::Object(body)).await
}

async fn send_video(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;

    if bool_val(config, "binary_data") {
        let file = read_binary_file(config, "video.mp4").await?;
        let part = file_part(file.bytes, file.file_name, &file.mime_type)?;

        let mut form = multipart::Form::new()
            .text("chat_id", chat_id)
            .part("video", part);

        for key in &[
            "caption",
            "duration",
            "width",
            "height",
            "supports_streaming",
        ] {
            if let Some(v) = config.get(*key) {
                form = form.text(*key, v.to_string());
            }
        }
        apply_additional_fields_to_form(&mut form, config);

        return client.post_multipart("sendVideo", form).await;
    }

    let video = require_str(config, "video")?;
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("video".into(), json!(video));
    for key in &[
        "caption",
        "duration",
        "width",
        "height",
        "supports_streaming",
    ] {
        if let Some(v) = config.get(*key) {
            body.insert(key.to_string(), v.clone());
        }
    }
    apply_additional_fields(&mut body, config);

    client.post("sendVideo", Value::Object(body)).await
}

async fn send_audio(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;

    if bool_val(config, "binary_data") {
        let file = read_binary_file(config, "audio.mp3").await?;
        let part = file_part(file.bytes, file.file_name, &file.mime_type)?;

        let mut form = multipart::Form::new()
            .text("chat_id", chat_id)
            .part("audio", part);
        for key in &["caption", "duration", "performer", "title"] {
            if let Some(v) = config.get(*key) {
                form = form.text(*key, v.to_string());
            }
        }
        apply_additional_fields_to_form(&mut form, config);
        return client.post_multipart("sendAudio", form).await;
    }

    let audio = require_str(config, "audio")?;
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("audio".into(), json!(audio));
    for key in &["caption", "duration", "performer", "title"] {
        if let Some(v) = config.get(*key) {
            body.insert(key.to_string(), v.clone());
        }
    }
    apply_additional_fields(&mut body, config);
    client.post("sendAudio", Value::Object(body)).await
}

async fn send_document(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;

    if bool_val(config, "binary_data") {
        // Prefer the resolved file path/object; fall back to inline base64 bytes.
        let part = if let Some(file) = try_read_binary_file(config, "document.pdf").await? {
            file_part(file.bytes, file.file_name, &file.mime_type)?
        } else if config.get("file_bytes").is_some() {
            let file_bytes_b64 = require_str(config, "file_bytes")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(file_bytes_b64)
                .map_err(|e| format!("Failed to decode file_bytes: {e}"))?;
            let name = str_val(config, "file_name").unwrap_or_else(|| "document".to_string());
            let mime = str_val(config, "mime_type")
                .unwrap_or_else(|| "application/octet-stream".to_string());
            file_part(bytes, name, &mime)?
        } else {
            return Err(
                "Binary Data is enabled but no file was provided. Set the 'File' field to a \
                 server path (e.g. /data/files/your-file.pdf) or a binary object from a \
                 previous node, or supply base64 'file_bytes'."
                    .to_string(),
            );
        };

        let mut form = multipart::Form::new()
            .text("chat_id", chat_id)
            .part("document", part);
        if let Some(caption) = str_val(config, "caption") {
            form = form.text("caption", caption);
        }
        apply_additional_fields_to_form(&mut form, config);
        return client.post_multipart("sendDocument", form).await;
    }

    let document = require_str(config, "document")?;
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("document".into(), json!(document));
    if let Some(caption) = str_val(config, "caption") {
        body.insert("caption".into(), json!(caption));
    }
    apply_additional_fields(&mut body, config);
    client.post("sendDocument", Value::Object(body)).await
}

async fn send_animation(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;

    if bool_val(config, "binary_data") {
        let file = read_binary_file(config, "animation.gif").await?;
        let part = file_part(file.bytes, file.file_name, &file.mime_type)?;

        let mut form = multipart::Form::new()
            .text("chat_id", chat_id)
            .part("animation", part);
        for key in &["caption", "duration", "width", "height"] {
            if let Some(v) = config.get(*key) {
                form = form.text(*key, v.to_string());
            }
        }
        apply_additional_fields_to_form(&mut form, config);
        return client.post_multipart("sendAnimation", form).await;
    }

    let animation = require_str(config, "animation")?;
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("animation".into(), json!(animation));
    for key in &["caption", "duration", "width", "height"] {
        if let Some(v) = config.get(key) {
            body.insert(key.to_string(), v.clone());
        }
    }
    apply_additional_fields(&mut body, config);
    client.post("sendAnimation", Value::Object(body)).await
}

async fn send_sticker(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;

    if bool_val(config, "binary_data") {
        let file = read_binary_file(config, "sticker.webp").await?;
        let part = file_part(file.bytes, file.file_name, &file.mime_type)?;

        let mut form = multipart::Form::new()
            .text("chat_id", chat_id)
            .part("sticker", part);
        apply_additional_fields_to_form(&mut form, config);
        return client.post_multipart("sendSticker", form).await;
    }

    let sticker = require_str(config, "sticker")?;
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("sticker".into(), json!(sticker));
    apply_additional_fields(&mut body, config);
    client.post("sendSticker", Value::Object(body)).await
}

async fn send_location(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let latitude = config
        .get("latitude")
        .and_then(|v| v.as_f64())
        .ok_or("Missing 'latitude'")?;
    let longitude = config
        .get("longitude")
        .and_then(|v| v.as_f64())
        .ok_or("Missing 'longitude'")?;

    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("latitude".into(), json!(latitude));
    body.insert("longitude".into(), json!(longitude));
    for key in &["live_period", "heading", "proximity_alert_radius"] {
        if let Some(v) = config.get(key) {
            body.insert(key.to_string(), v.clone());
        }
    }
    if let Some(reply_msg) = str_val(config, "reply_to_message_id") {
        if let Ok(id) = reply_msg.parse::<i64>() {
            body.insert("reply_to_message_id".into(), json!(id));
        }
    }

    client.post("sendLocation", Value::Object(body)).await
}

/// sendMediaGroup – send a group of photos or videos as an album.
/// config["media"] should be a JSON array of InputMedia objects, each with:
///   type, media (file_id or URL), and optional caption.
async fn send_media_group(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let media = config
        .get("media")
        .and_then(|v| v.as_array())
        .ok_or("Missing 'media' array for sendMediaGroup")?;

    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("media".into(), json!(media));

    client.post("sendMediaGroup", Value::Object(body)).await
}

async fn edit_message_text(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'message_id' for editMessageText")?;
    let mut text = require_str(config, "text")?;
    if is_html_parse_mode(config) {
        text = escape_html_for_telegram(&text);
    }

    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("message_id".into(), json!(message_id));
    body.insert("text".into(), json!(text));
    apply_additional_fields(&mut body, config);

    client.post("editMessageText", Value::Object(body)).await
}

async fn delete_message(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'message_id' for deleteMessage")?;

    client
        .post(
            "deleteMessage",
            json!({ "chat_id": chat_id, "message_id": message_id }),
        )
        .await
}

async fn pin_chat_message(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'message_id' for pinChatMessage")?;
    let disable_notification = bool_val(config, "disable_notification");

    client
        .post(
            "pinChatMessage",
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "disable_notification": disable_notification,
            }),
        )
        .await
}

async fn unpin_chat_message(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let mut body = json!({ "chat_id": chat_id });
    if let Some(mid) = config.get("message_id").and_then(|v| v.as_i64()) {
        body["message_id"] = json!(mid);
    }
    client.post("unpinChatMessage", body).await
}

// ── Operations — chat resource ────────────────────────────────────────────────

async fn get_chat(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    client.post("getChat", json!({ "chat_id": chat_id })).await
}

async fn get_chat_administrators(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    client
        .post("getChatAdministrators", json!({ "chat_id": chat_id }))
        .await
}

async fn get_chat_member(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let user_id = config
        .get("user_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'user_id'")?;

    client
        .post(
            "getChatMember",
            json!({ "chat_id": chat_id, "user_id": user_id }),
        )
        .await
}

async fn export_chat_invite_link(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    client
        .post("exportChatInviteLink", json!({ "chat_id": chat_id }))
        .await
}

async fn create_chat_invite_link(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));

    for key in &[
        "name",
        "expire_date",
        "member_limit",
        "creates_join_request",
    ] {
        if let Some(v) = config.get(*key) {
            body.insert(key.to_string(), v.clone());
        }
    }

    client
        .post("createChatInviteLink", Value::Object(body))
        .await
}

async fn revoke_chat_invite_link(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let invite_link = config
        .get("invite_link")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'invite_link'")?;

    client
        .post(
            "revokeChatInviteLink",
            json!({ "chat_id": chat_id, "invite_link": invite_link }),
        )
        .await
}

async fn set_chat_photo(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;

    if bool_val(config, "binary_data") {
        let file = read_binary_file(config, "photo.jpg").await?;
        let part = file_part(file.bytes, file.file_name, &file.mime_type)?;

        let form = multipart::Form::new()
            .text("chat_id", chat_id)
            .part("photo", part);

        return client.post_multipart("setChatPhoto", form).await;
    }

    let photo = require_str(config, "photo")?;
    client
        .post(
            "setChatPhoto",
            json!({ "chat_id": chat_id, "photo": photo }),
        )
        .await
}

async fn delete_chat_photo(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    client
        .post("deleteChatPhoto", json!({ "chat_id": chat_id }))
        .await
}

async fn edit_message_caption(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'message_id'")?;
    let caption = str_val(config, "caption").unwrap_or_default();

    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("message_id".into(), json!(message_id));
    if !caption.is_empty() {
        body.insert("caption".into(), json!(caption));
    }
    apply_additional_fields(&mut body, config);

    client.post("editMessageCaption", Value::Object(body)).await
}

async fn edit_message_media(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        })
        .ok_or("Missing 'message_id'")?;

    let media_type = str_val(config, "media_type").unwrap_or_else(|| "photo".to_string());
    let media = str_val(config, "media")
        .or_else(|| str_val(config, "file_id"))
        .or_else(|| str_val(config, "url"))
        .filter(|s| !s.is_empty())
        .ok_or("Missing 'media' (file_id or URL) for editMessageMedia")?;

    let mut input_media = serde_json::Map::new();
    input_media.insert("type".into(), json!(media_type));
    input_media.insert("media".into(), json!(media));
    if let Some(caption) = str_val(config, "caption").filter(|c| !c.is_empty()) {
        input_media.insert("caption".into(), json!(caption));
    }

    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("message_id".into(), json!(message_id));
    body.insert("media".into(), Value::Object(input_media));

    client.post("editMessageMedia", Value::Object(body)).await
}

async fn forward_message(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let from_chat_id = require_str(config, "from_chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'message_id'")?;
    let disable_notification = bool_val(config, "disable_notification");

    client
        .post(
            "forwardMessage",
            json!({
                "chat_id": chat_id,
                "from_chat_id": from_chat_id,
                "message_id": message_id,
                "disable_notification": disable_notification
            }),
        )
        .await
}

async fn copy_message(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let from_chat_id = require_str(config, "from_chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'message_id'")?;
    let disable_notification = bool_val(config, "disable_notification");

    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("from_chat_id".into(), json!(from_chat_id));
    body.insert("message_id".into(), json!(message_id));
    if disable_notification {
        body.insert("disable_notification".into(), json!(true));
    }
    apply_additional_fields(&mut body, config);

    client.post("copyMessage", Value::Object(body)).await
}

async fn stop_poll(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let message_id = config
        .get("message_id")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'message_id'")?;
    let mut body = serde_json::Map::new();
    body.insert("chat_id".into(), json!(chat_id));
    body.insert("message_id".into(), json!(message_id));

    if let Some(reply_markup) = config.get("reply_markup") {
        body.insert("reply_markup".into(), reply_markup.clone());
    }

    client.post("stopPoll", Value::Object(body)).await
}

async fn answer_inline_query(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let inline_query_id = require_str(config, "inline_query_id")?;
    let results = config
        .get("results")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'results' JSON array")?;

    let results: Value =
        serde_json::from_str(results).map_err(|e| format!("Invalid 'results' JSON: {}", e))?;

    let mut body = serde_json::Map::new();
    body.insert("inline_query_id".into(), json!(inline_query_id));
    body.insert("results".into(), results);

    for key in &[
        "cache_time",
        "personal",
        "next_offset",
        "switch_pm_text",
        "switch_pm_parameter",
    ] {
        if let Some(v) = config.get(*key) {
            body.insert(key.to_string(), v.clone());
        }
    }

    client.post("answerInlineQuery", Value::Object(body)).await
}

async fn set_chat_title(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let title = require_str(config, "title")?;
    client
        .post(
            "setChatTitle",
            json!({ "chat_id": chat_id, "title": title }),
        )
        .await
}

async fn set_chat_description(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    let description = str_val(config, "description").unwrap_or_default();
    client
        .post(
            "setChatDescription",
            json!({ "chat_id": chat_id, "description": description }),
        )
        .await
}

async fn send_chat_action(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    // action: typing | upload_photo | record_video | upload_video | record_audio |
    //         upload_audio | upload_document | find_location | record_video_note |
    //         upload_video_note
    let action = str_val(config, "action").unwrap_or_else(|| "typing".to_string());
    client
        .post(
            "sendChatAction",
            json!({ "chat_id": chat_id, "action": action }),
        )
        .await
}

async fn leave_chat(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let chat_id = require_str(config, "chat_id")?;
    client
        .post("leaveChat", json!({ "chat_id": chat_id }))
        .await
}

// ── Operations — callback resource ────────────────────────────────────────────

async fn answer_callback_query(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let callback_query_id = require_str(config, "callback_query_id")?;
    let mut body = serde_json::Map::new();
    body.insert("callback_query_id".into(), json!(callback_query_id));
    for key in &["text", "show_alert", "url", "cache_time"] {
        if let Some(v) = config.get(key) {
            body.insert(key.to_string(), v.clone());
        }
    }
    client
        .post("answerCallbackQuery", Value::Object(body))
        .await
}

// ── Public executor ───────────────────────────────────────────────────────────

/// Drop-in replacement for `execute_http_node` — plug into the workflow engine
/// match arm with `"telegram" => execute_telegram_node(&config).await`.
pub async fn execute_telegram_node(config: &Value) -> Result<Value, String> {
    let creds = TelegramCredentials::from_config(config)?;
    let client = TelegramClient::new(creds);

    let resource = str_val(config, "resource").unwrap_or_else(|| "message".to_string());
    let operation = str_val(config, "operation").unwrap_or_else(|| "sendMessage".to_string());

    match (resource.as_str(), operation.as_str()) {
        // ── message ──────────────────────────────────────────────────────────
        ("message", "sendMessage") => send_message(&client, config).await,
        ("message", "sendPhoto") => send_photo(&client, config).await,
        ("message", "sendVideo") => send_video(&client, config).await,
        ("message", "sendAudio") => send_audio(&client, config).await,
        ("message", "sendDocument") => send_document(&client, config).await,
        ("message", "sendAnimation") => send_animation(&client, config).await,
        ("message", "sendSticker") => send_sticker(&client, config).await,
        ("message", "sendLocation") => send_location(&client, config).await,
        ("message", "sendMediaGroup") => send_media_group(&client, config).await,
        ("message", "forwardMessage") => forward_message(&client, config).await,
        ("message", "copyMessage") => copy_message(&client, config).await,
        ("message", "editMessageText") => edit_message_text(&client, config).await,
        ("message", "editMessageCaption") => edit_message_caption(&client, config).await,
        ("message", "editMessageMedia") => edit_message_media(&client, config).await,
        ("message", "deleteMessage") => delete_message(&client, config).await,
        ("message", "pinChatMessage") => pin_chat_message(&client, config).await,
        ("message", "unpinChatMessage") => unpin_chat_message(&client, config).await,
        ("message", "stopPoll") => stop_poll(&client, config).await,
        ("message", "getFile") => download_message_file(&client, config).await,

        // ── chat ─────────────────────────────────────────────────────────────
        ("chat", "getChat") => get_chat(&client, config).await,
        ("chat", "getChatAdministrators") => get_chat_administrators(&client, config).await,
        ("chat", "getChatMember") => get_chat_member(&client, config).await,
        ("chat", "getFile") => download_message_file(&client, config).await,
        ("chat", "setChatTitle") => set_chat_title(&client, config).await,
        ("chat", "setChatDescription") => set_chat_description(&client, config).await,
        ("chat", "setChatPhoto") => set_chat_photo(&client, config).await,
        ("chat", "deleteChatPhoto") => delete_chat_photo(&client, config).await,
        ("chat", "sendChatAction") => send_chat_action(&client, config).await,
        ("chat", "leaveChat") => leave_chat(&client, config).await,
        ("chat", "exportChatInviteLink") => export_chat_invite_link(&client, config).await,
        ("chat", "createChatInviteLink") => create_chat_invite_link(&client, config).await,
        ("chat", "revokeChatInviteLink") => revoke_chat_invite_link(&client, config).await,

        // ── callback ─────────────────────────────────────────────────────────
        ("callback", "answerQuery") => answer_callback_query(&client, config).await,
        ("callback", "answerInlineQuery") => answer_inline_query(&client, config).await,

        _ => Err(format!(
            "Telegram: unknown resource/operation combination '{resource}/{operation}'"
        )),
    }
}

// ── Telegram Trigger (webhook) ────────────────────────────────────────────────
//
// The trigger is used by your HTTP server layer.  Call `handle_telegram_webhook`
// from your Axum/Actix route handler.
//
// Expected config keys
// ────────────────────
//   access_token          – bot token (used to derive the secret token)
//   workflow_id           – used to build the secret token
//   node_id               – used to build the secret token
//   download              – bool: download attached files
//   image_size            – "small"|"medium"|"large"|"extraLarge"
//   chat_ids              – comma-separated allowed chat IDs (optional)
//   user_ids              – comma-separated allowed user IDs (optional)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_post: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_message: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_query: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_query: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll: Option<Value>,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug)]
pub enum TriggerResult {
    /// Truly rejected — bad secret token, filtered chat/user, etc.
    /// The caller must discard this update entirely (do NOT send to agent).
    Rejected { reason: String },

    /// The button had `route_to_trigger = true` (callback_data was `trig:…`).
    /// Fire this trigger's workflow with the payload (prefix already stripped).
    /// The main agent MUST ignore this update.
    AcceptedForTrigger(Value),

    /// The button had `route_to_trigger = false` (callback_data was `agent:…`),
    /// or this is an ordinary message/update (no routing prefix at all).
    /// The `agent:` prefix has already been stripped from callback_data.
    /// Pass the payload to the main agent; do NOT fire trigger workflows for it.
    AcceptedForAgent(Value),
}

/// Derive the secret token from workflow_id + node_id, stripping non-alphanumeric chars.
/// Mirrors n8n's `getSecretToken`: `${workflowId}_${nodeId}`.
pub fn derive_secret_token(workflow_id: &str, node_id: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    };
    format!("{}_{}", clean(workflow_id), clean(node_id))
}

/// Process an inbound Telegram webhook POST.
///
/// `incoming_secret` — value of the `x-telegram-bot-api-secret-token` header.
/// `body`            — the parsed JSON body from Telegram.
/// `config`          — the node config (see key list above).
pub async fn handle_telegram_webhook(
    incoming_secret: &str,
    mut body: Value,
    config: &Value,
) -> TriggerResult {
    // ── 1. Verify secret token ────────────────────────────────────────────────
    let workflow_id = str_val(config, "workflow_id").unwrap_or_default();
    let node_id = str_val(config, "node_id").unwrap_or_default();

    if !workflow_id.is_empty() && !node_id.is_empty() {
        let expected = derive_secret_token(&workflow_id, &node_id);
        // Constant-time comparison avoids timing attacks (mirrors crypto.timingSafeEqual).
        if !constant_time_eq(incoming_secret.as_bytes(), expected.as_bytes()) {
            return TriggerResult::Rejected {
                reason: "Invalid secret token".into(),
            };
        }
    }

    // ── 2. Callback-query routing ─────────────────────────────────────────────
    //
    // Buttons encoded with `route_to_trigger = true`  → callback_data = "trig:…"
    // Buttons encoded with `route_to_trigger = false` → callback_data = "agent:…"
    //
    // The two paths are mutually exclusive and must never both fire:
    //   • "trig:"  → strip prefix, fall through so filter steps still run,
    //                then return AcceptedForTrigger.  Main agent MUST ignore.
    //   • "agent:" → strip prefix, return AcceptedForAgent immediately.
    //                This trigger's workflow must NOT fire.
    //   • No prefix (plain/legacy data) → treat as agent-routed.
    if let Some(data) = body
        .get("callback_query")
        .and_then(|cbq| cbq.get("data"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
    {
        if let Some(stripped) = data.strip_prefix("trig:") {
            // Trigger-routed: remove prefix in-place, fall through to filters.
            if let Some(cbq_obj) = body
                .get_mut("callback_query")
                .and_then(|v| v.as_object_mut())
            {
                cbq_obj.insert("data".into(), json!(stripped));
            }
        } else {
            // agent:-prefixed OR plain/legacy callback_data → route to agent.
            // Strip the "agent:" prefix so the agent receives a clean instruction.
            let clean = data.strip_prefix("agent:").unwrap_or(&data).to_string();
            if let Some(cbq_obj) = body
                .get_mut("callback_query")
                .and_then(|v| v.as_object_mut())
            {
                cbq_obj.insert("data".into(), json!(clean));
            }
            return TriggerResult::AcceptedForAgent(body);
        }
    }

    let msg_obj = body.get("message").or_else(|| body.get("channel_post"));
    // Fallback for callback_query which also has a message and user
    let from_obj = msg_obj
        .and_then(|m| m.get("from"))
        .or_else(|| body.get("callback_query").and_then(|c| c.get("from")));
    let chat_obj = msg_obj.and_then(|m| m.get("chat")).or_else(|| {
        body.get("callback_query")
            .and_then(|c| c.pointer("/message/chat"))
    });

    // ── 3. Chat ID filter ────────────────────────────────────────────────────
    if let Some(chat_ids_str) = str_val(config, "chat_ids") {
        if !chat_ids_str.is_empty() {
            let allowed: Vec<&str> = chat_ids_str.split(',').map(|s| s.trim()).collect();
            let incoming_chat_id = chat_obj
                .and_then(|c| c.get("id"))
                .and_then(|id| id.as_i64())
                .map(|id| id.to_string())
                .unwrap_or_default();
            if !allowed.contains(&incoming_chat_id.as_str()) {
                return TriggerResult::Rejected {
                    reason: format!("Chat ID '{incoming_chat_id}' not in allowed list"),
                };
            }
        }
    }

    // ── 4. User ID filter ────────────────────────────────────────────────────
    if let Some(user_ids_str) = str_val(config, "user_ids") {
        if !user_ids_str.is_empty() {
            let allowed: Vec<&str> = user_ids_str.split(',').map(|s| s.trim()).collect();
            let incoming_user_id = from_obj
                .and_then(|f| f.get("id"))
                .and_then(|id| id.as_i64())
                .map(|id| id.to_string())
                .unwrap_or_default();
            if !allowed.contains(&incoming_user_id.as_str()) {
                return TriggerResult::Rejected {
                    reason: format!("User ID '{incoming_user_id}' not in allowed list"),
                };
            }
        }
    }

    // ── 5. Standardize file_id ───────────────────────────────────────────────
    let image_size = str_val(config, "image_size").unwrap_or_else(|| "large".to_string());

    // Inject the best file_id directly into the message or channel_post so users can easily drag it
    let extracted_file_id = body
        .get("message")
        .or_else(|| body.get("channel_post"))
        .and_then(|msg| extract_best_file_id(msg, &image_size));

    if let Some(file_id) = extracted_file_id {
        let msg_mut = match body.get_mut("message") {
            Some(msg) => Some(msg),
            None => body.get_mut("channel_post"),
        };
        if let Some(msg_obj) = msg_mut {
            if let Some(map) = msg_obj.as_object_mut() {
                map.insert("file_id".into(), Value::String(file_id));
            }
        }
    }

    // ── 6. Optional file download ─────────────────────────────────────────────
    if bool_val(config, "download") {
        if let Some(msg) = body.get("message").or_else(|| body.get("channel_post")) {
            if let Some(binary) = try_download_attachment(config, msg, &image_size).await {
                let mut result = serde_json::Map::new();
                result.insert("json".into(), body.clone());
                result.insert("binary".into(), binary);
                return TriggerResult::AcceptedForTrigger(Value::Object(result));
            }
        }
    }

    TriggerResult::AcceptedForTrigger(body)
}

fn extract_best_file_id(msg: &Value, image_size: &str) -> Option<String> {
    if let Some(photos) = msg.get("photo").and_then(|p| p.as_array()) {
        if photos.is_empty() {
            return None;
        }
        let idx = match image_size {
            "small" => 0,
            "medium" => photos.len() / 2,
            "extraLarge" | "desktop" => photos.len() - 1,
            _ => photos.len() - 1, // "large" default
        };
        photos[idx.min(photos.len() - 1)]
            .get("file_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else if let Some(video) = msg.get("video") {
        video
            .get("file_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else if let Some(doc) = msg.get("document") {
        doc.get("file_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    }
}

/// Attempt to download photo / video / document from a message.
/// Returns a JSON object describing the downloaded file, or None if
/// the message has no downloadable attachment.
async fn try_download_attachment(config: &Value, msg: &Value, image_size: &str) -> Option<Value> {
    let creds = TelegramCredentials::from_config(config).ok()?;
    let client = TelegramClient::new(creds.clone());

    let file_id = extract_best_file_id(msg, image_size)?;

    // getFile → file_path
    let file_info = client
        .post("getFile", json!({ "file_id": file_id }))
        .await
        .ok()?;
    let file_path = file_info.get("file_path").and_then(|v| v.as_str())?;

    // Download the actual bytes
    let download_url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        creds.access_token, file_path
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .ok()?;
    let bytes = http
        .get(&download_url)
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;

    let file_name = file_path.split('/').last().unwrap_or("telegram_file");
    let mime = mime_guess::from_path(file_name)
        .first_or_octet_stream()
        .to_string();

    // Stage to disk using the same helper used by http.rs
    let staged_path = crate::files::stage_bytes(&bytes, file_name).ok()?;

    Some(json!({
        "binary": binary_descriptor(
            &staged_path.to_string_lossy(),
            file_name,
            &mime,
            bytes.len(),
        )
    }))
}

/// Register / unregister a Telegram webhook.
///
/// `action`      – "create" | "delete" | "check"
/// `webhook_url` – the public HTTPS URL Telegram should POST to
/// `config`      – must contain `access_token`, `workflow_id`, `node_id`
pub async fn manage_webhook(
    action: &str,
    webhook_url: &str,
    allowed_updates: &[&str],
    config: &Value,
) -> Result<Value, String> {
    let creds = TelegramCredentials::from_config(config)?;
    let client = TelegramClient::new(creds.clone());

    let workflow_id = str_val(config, "workflow_id").unwrap_or_default();
    let node_id = str_val(config, "node_id").unwrap_or_default();
    let secret_token = derive_secret_token(&workflow_id, &node_id);

    match action {
        "create" => {
            let updates: Vec<&str> = if allowed_updates.contains(&"*") {
                vec![] // empty = all
            } else {
                allowed_updates.to_vec()
            };
            client
                .post(
                    "setWebhook",
                    json!({
                        "url": webhook_url,
                        "allowed_updates": updates,
                        "secret_token": secret_token,
                    }),
                )
                .await
        }
        "delete" => client.post("deleteWebhook", json!({})).await,
        "check" => {
            let info = client.post("getWebhookInfo", json!({})).await?;
            Ok(json!({
                "registered": info.get("url").and_then(|v| v.as_str()) == Some(webhook_url),
                "info": info,
            }))
        }
        _ => Err(format!("manage_webhook: unknown action '{action}'")),
    }
}

// ── Tool definitions ──────────────────────────────────────────────────────────

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "telegram".to_string(),
        description: "Send messages, photos, documents, and other content via a Telegram bot. \
             Manage chats and respond to callback queries."
            .to_string(),
        parameters: serde_json::json!({
            "access_token": {
                "type": "string",
                "description": "Telegram bot token from @BotFather"
            },
            "base_url": {
                "type": "string",
                "description": "Telegram Bot API base URL (default: https://api.telegram.org)"
            },
            "resource": {
                "type": "string",
                "enum": ["message", "chat", "callback"],
                "description": "Resource group"
            },
            "operation": {
                "type": "string",
                "enum": [
                    "sendMessage", "sendPhoto", "sendVideo", "sendAudio",
                    "sendDocument", "sendAnimation", "sendSticker", "sendLocation",
                    "sendMediaGroup", "editMessageText", "deleteMessage",
                    "pinChatMessage", "unpinChatMessage",
                    "getChat", "getChatAdministrators", "getChatMember",
                    "setChatTitle", "setChatDescription", "sendChatAction", "leaveChat",
                    "answerQuery"
                ],
                "description": "Operation to perform"
            },
            "chat_id": {
                "type": "string",
                "description": "Target chat or channel ID"
            },
            "text": {
                "type": "string",
                "description": "Message text (supports Markdown)"
            },
            "parse_mode": {
                "type": "string",
                "enum": ["Markdown", "HTML", "MarkdownV2"],
                "default": "Markdown"
            },
            "disable_web_page_preview": {
                "type": "boolean",
                "default": true
            },
            "append_attribution": {
                "type": "boolean",
                "description": "Append n8n attribution footer to messages",
                "default": false
            },
            "instance_id": {
                "type": "string",
                "description": "n8n instance ID for attribution link"
            },
            "reply_markup": {
                "type": "string",
                "enum": ["none", "inlineKeyboard", "forceReply", "replyKeyboardRemove"],
                "default": "none"
            },
            "inline_keyboard": {
                "type": "object",
                "description": "Inline keyboard definition ({rows:[{row:{buttons:[{text,additionalFields:{url|callback_data}}]}}]})"
            },
            "photo": { "type": "string", "description": "File ID or URL of photo" },
            "video": { "type": "string", "description": "File ID or URL of video" },
            "audio": { "type": "string", "description": "File ID or URL of audio" },
            "document": { "type": "string", "description": "File ID or URL of document" },
            "animation": { "type": "string", "description": "File ID or URL of animation/GIF" },
            "sticker": { "type": "string", "description": "File ID or URL of sticker" },
            "binary_data": {
                "type": "boolean",
                "description": "Upload file from binary data instead of URL/file_id"
            },
            "file_bytes": {
                "type": "string",
                "description": "Base64-encoded file bytes (requires binary_data=true)"
            },
            "file_name": { "type": "string" },
            "mime_type": { "type": "string" },
            "caption": { "type": "string" },
            "latitude": { "type": "number" },
            "longitude": { "type": "number" },
            "media": {
                "type": "array",
                "description": "InputMedia array for sendMediaGroup"
            },
            "message_id": { "type": "integer" },
            "user_id": { "type": "integer" },
            "title": { "type": "string" },
            "description": { "type": "string" },
            "action": {
                "type": "string",
                "description": "Chat action type for sendChatAction (e.g. 'typing')"
            },
            "callback_query_id": { "type": "string" },
            "show_alert": { "type": "boolean" },
            "disable_notification": { "type": "boolean" },
            "reply_to_message_id": { "type": "string" }
        }),
        required: vec![
            "access_token".to_string(),
            "resource".to_string(),
            "operation".to_string(),
        ],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: true,
    }
}

pub fn trigger_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "telegram_trigger".to_string(),
        description: "Register a Telegram webhook and receive inbound updates (messages, photos, \
             documents, callback queries, etc.)."
            .to_string(),
        parameters: serde_json::json!({
            "access_token": { "type": "string" },
            "base_url":     { "type": "string" },
            "workflow_id":  { "type": "string" },
            "node_id":      { "type": "string" },
            "webhook_url":  { "type": "string", "description": "Public HTTPS URL for the webhook" },
            "updates": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Telegram update types to subscribe to. Use ['*'] for all."
            },
            "download":   { "type": "boolean",  "default": false },
            "image_size": {
                "type": "string",
                "enum": ["small","medium","large","extraLarge"],
                "default": "large"
            },
            "chat_ids": {
                "type": "string",
                "description": "Comma-separated chat IDs to allow"
            },
            "user_ids": {
                "type": "string",
                "description": "Comma-separated user IDs to allow"
            }
        }),
        required: vec![
            "access_token".to_string(),
            "webhook_url".to_string(),
            "workflow_id".to_string(),
            "node_id".to_string(),
        ],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: true,
    }
}

// ── Utility ───────────────────────────────────────────────────────────────────

/// Constant-time byte comparison (avoids timing attacks on secret tokens).
async fn download_message_file(client: &TelegramClient, config: &Value) -> Result<Value, String> {
    let file_id = require_str(config, "file_id")?;
    if file_id.trim().is_empty() {
        return Err("\"File ID\" is required for Get Download Link. \
            Provide the file_id from an upstream Telegram message (e.g. \
            {{ $node[\"Trigger\"].data.document.file_id }})."
            .to_string());
    }
    let file_info = client
        .post("getFile", json!({ "file_id": file_id }))
        .await
        .map_err(|e| format!("Failed to get file info: {e}"))?;

    let file_path = file_info
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or("No file_path in Telegram response")?;
    let download_url = format!(
        "{}/file/bot{}/{}",
        client.creds.base_url, client.creds.access_token, file_path
    );

    let http = reqwest::Client::new();
    let resp = http
        .get(&download_url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read bytes: {e}"))?;

    let default_name = file_path.split('/').last().unwrap_or("file").to_string();
    let original_name = match str_val(config, "file_name") {
        Some(name) if !name.trim().is_empty() => name,
        _ => default_name,
    };

    let staged_path = crate::files::stage_bytes(&bytes, &original_name)
        .map_err(|e| format!("Staging failed: {e}"))?;

    let mime = mime_guess::from_path(&original_name)
        .first_or_octet_stream()
        .to_string();

    Ok(json!({
        "binary": binary_descriptor(
            &staged_path.to_string_lossy(),
            &original_name,
            &mime,
            bytes.len(),
        )
    }))
}

fn apply_additional_fields_to_form(form: &mut multipart::Form, config: &Value) {
    let mut dummy = serde_json::Map::new();
    apply_additional_fields(&mut dummy, config);

    // We need to re-consume and rebuild the form since multipart::Form is not easily mutable via reference
    let mut new_form = std::mem::replace(form, multipart::Form::new());
    for (k, v) in dummy {
        if k == "chat_id"
            || k == "photo"
            || k == "video"
            || k == "audio"
            || k == "document"
            || k == "animation"
            || k == "sticker"
        {
            continue;
        }
        let val_str = if let Some(s) = v.as_str() {
            s.to_string()
        } else {
            v.to_string()
        };
        new_form = new_form.text(k, val_str);
    }
    *form = new_form;
}

// ── Utility ───────────────────────────────────────────────────────────────────

/// Constant-time byte comparison (avoids timing attacks on secret tokens).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_file_descriptor_plain_string_path() {
        let v = json!("/data/files/report.pdf");
        let (path, name, mime) = extract_file_descriptor(&v).unwrap();
        assert_eq!(path, "/data/files/report.pdf");
        assert_eq!(name, None);
        assert_eq!(mime, None);
    }

    #[test]
    fn test_extract_file_descriptor_snake_case() {
        // Shape emitted by getFile / AttachedFile / UI.
        let v = json!({
            "local_path": "/data/files/a.pdf",
            "original_name": "a.pdf",
            "mime_type": "application/pdf",
            "size": 10
        });
        let (path, name, mime) = extract_file_descriptor(&v).unwrap();
        assert_eq!(path, "/data/files/a.pdf");
        assert_eq!(name.as_deref(), Some("a.pdf"));
        assert_eq!(mime.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn test_extract_file_descriptor_camel_case() {
        // Shape emitted by the Telegram trigger download + Myelin — the case
        // that used to fail with "Missing 'local_path' in file object".
        let v = json!({
            "localPath": "/data/files/b.jpg",
            "fileName": "b.jpg",
            "mimeType": "image/jpeg",
            "fileSize": 20
        });
        let (path, name, mime) = extract_file_descriptor(&v).unwrap();
        assert_eq!(path, "/data/files/b.jpg");
        assert_eq!(name.as_deref(), Some("b.jpg"));
        assert_eq!(mime.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn test_extract_file_descriptor_nested_under_binary() {
        // User references the whole upstream node output ({{ $node["Trigger"] }}).
        let v = json!({
            "json": { "message_id": 1 },
            "binary": { "localPath": "/data/files/c.png", "fileName": "c.png" }
        });
        let (path, name, _mime) = extract_file_descriptor(&v).unwrap();
        assert_eq!(path, "/data/files/c.png");
        assert_eq!(name.as_deref(), Some("c.png"));
    }

    #[test]
    fn test_extract_file_descriptor_stringified_object() {
        let v = json!(r#"{"local_path":"/data/files/d.txt","original_name":"d.txt"}"#);
        let (path, name, _mime) = extract_file_descriptor(&v).unwrap();
        assert_eq!(path, "/data/files/d.txt");
        assert_eq!(name.as_deref(), Some("d.txt"));
    }

    #[test]
    fn test_extract_file_descriptor_rejects_empty_and_pathless() {
        assert!(extract_file_descriptor(&json!("")).is_none());
        assert!(extract_file_descriptor(&json!("   ")).is_none());
        // An object with no recognizable path key yields None (caller then gives
        // a helpful error instead of the cryptic "Missing 'local_path'").
        assert!(extract_file_descriptor(&json!({ "file_id": "abc" })).is_none());
    }

    #[test]
    fn test_binary_descriptor_carries_both_conventions() {
        let d = binary_descriptor("/data/files/e.pdf", "e.pdf", "application/pdf", 5);
        // Re-reading it back must succeed regardless of which convention a
        // downstream consumer expects.
        assert_eq!(d["local_path"], json!("/data/files/e.pdf"));
        assert_eq!(d["localPath"], json!("/data/files/e.pdf"));
        let (path, name, mime) = extract_file_descriptor(&d).unwrap();
        assert_eq!(path, "/data/files/e.pdf");
        assert_eq!(name.as_deref(), Some("e.pdf"));
        assert_eq!(mime.as_deref(), Some("application/pdf"));
    }

    #[test]
    fn test_derive_secret_token_strips_invalid_chars() {
        assert_eq!(
            derive_secret_token("workflow#123", "node@123"),
            "workflow123_node123"
        );
    }

    #[test]
    fn test_derive_secret_token_basic() {
        assert_eq!(derive_secret_token("wf1", "n2"), "wf1_n2");
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hi", b"hello"));
    }

    #[test]
    fn test_attribution_footer_with_instance() {
        let footer = attribution_footer("abc123");
        assert!(footer.contains("abc123"));
        assert!(footer.contains("n8n"));
    }

    #[test]
    fn test_apply_additional_fields_appends_attribution() {
        let config = json!({
            "append_attribution": true,
            "instance_id": "myInstance",
            "reply_markup": "none",
        });
        let mut body = serde_json::Map::new();
        body.insert("text".into(), json!("Hello"));
        apply_additional_fields(&mut body, &config);

        let text = body["text"].as_str().unwrap();
        assert!(text.contains("Hello"));
        assert!(text.contains("n8n"));
        assert!(text.contains("myInstance"));
    }

    #[test]
    fn test_apply_additional_fields_inline_keyboard() {
        let config = json!({
            "reply_markup": "inlineKeyboard",
            "inline_keyboard": {
                "rows": [{
                    "row": {
                        "buttons": [{
                            "text": "Click me",
                            "additionalFields": { "url": "https://example.com" }
                        }]
                    }
                }]
            }
        });
        let mut body = serde_json::Map::new();
        body.insert("text".into(), json!("Pick one"));
        apply_additional_fields(&mut body, &config);

        let markup = &body["reply_markup"];
        assert!(markup.get("inline_keyboard").is_some());
        let btn = &markup["inline_keyboard"][0][0];
        assert_eq!(btn["text"], "Click me");
        assert_eq!(btn["url"], "https://example.com");
    }

    #[test]
    fn test_apply_additional_fields_inline_keyboard_route_to_trigger_string_bool() {
        let config = json!({
            "reply_markup": "inlineKeyboard",
            "inline_keyboard": {
                "parameters": [{
                    "text": "Test",
                    "callback_data": "test",
                    "route_to_trigger": "true"
                }]
            }
        });
        let mut body = serde_json::Map::new();
        body.insert("text".into(), json!("Pick one"));
        apply_additional_fields(&mut body, &config);

        let btn = &body["reply_markup"]["inline_keyboard"][0][0];
        assert_eq!(btn["callback_data"], "trig:test");
    }

    #[test]
    fn test_apply_additional_fields_inline_keyboard_route_to_agent_explicit_prefix() {
        let config = json!({
            "reply_markup": "inlineKeyboard",
            "inline_keyboard": {
                "parameters": [{
                    "text": "Test",
                    "callback_data": "Test",
                    "route_to_trigger": false
                }]
            }
        });
        let mut body = serde_json::Map::new();
        body.insert("text".into(), json!("Pick one"));
        apply_additional_fields(&mut body, &config);

        let btn = &body["reply_markup"]["inline_keyboard"][0][0];
        assert_eq!(btn["callback_data"], "agent:Test");
    }

    #[test]
    fn test_apply_additional_fields_inline_keyboard_rows_support_top_level_callback_data() {
        let config = json!({
            "reply_markup": "inlineKeyboard",
            "inline_keyboard": {
                "rows": [{
                    "row": {
                        "buttons": [{
                            "text": "Trigger me",
                            "callback_data": "launch",
                            "additionalFields": { "route_to_trigger": true }
                        }]
                    }
                }]
            }
        });
        let mut body = serde_json::Map::new();
        body.insert("text".into(), json!("Pick one"));
        apply_additional_fields(&mut body, &config);

        let btn = &body["reply_markup"]["inline_keyboard"][0][0];
        assert_eq!(btn["callback_data"], "trig:launch");
    }

    #[tokio::test]
    async fn test_handle_telegram_webhook_agent_routed_returns_accepted_for_agent() {
        let config = json!({});
        let payload = json!({
            "callback_query": {
                "id": "cbq-2",
                "data": "agent:do_something",
                "from": { "id": 123 },
                "message": {
                    "chat": { "id": 456 },
                    "text": "Pick one"
                }
            }
        });

        let result = handle_telegram_webhook("", payload, &config).await;
        match result {
            TriggerResult::AcceptedForAgent(body) => {
                // "agent:" prefix must be stripped so the agent gets a clean instruction.
                assert_eq!(body["callback_query"]["data"], "do_something");
            }
            TriggerResult::AcceptedForTrigger(_) => {
                panic!("agent:-prefixed callback must NOT fire the trigger workflow")
            }
            TriggerResult::Rejected { reason } => {
                panic!("agent:-prefixed callback must not be rejected, got: {reason}")
            }
        }
    }

    #[tokio::test]
    async fn test_handle_telegram_webhook_strips_trigger_prefix_before_accepting() {
        let config = json!({});
        let payload = json!({
            "callback_query": {
                "id": "cbq-1",
                "data": "trig:test",
                "from": { "id": 123 },
                "message": {
                    "chat": { "id": 456 },
                    "text": "Trigger"
                }
            }
        });

        let result = handle_telegram_webhook("", payload, &config).await;
        match result {
            TriggerResult::AcceptedForTrigger(body) => {
                assert_eq!(body["callback_query"]["data"], "test");
            }
            other => panic!("expected AcceptedForTrigger webhook payload, got {other:?}"),
        }
    }

    #[test]
    fn test_escape_html_escapes_stray_angle_brackets() {
        // The reported failure: an email address looks like an unsupported tag.
        assert_eq!(
            escape_html_for_telegram("You have a new email from ChatGPT <noreply@email.openai.com>"),
            "You have a new email from ChatGPT &lt;noreply@email.openai.com&gt;"
        );
    }

    #[test]
    fn test_escape_html_preserves_valid_tags() {
        assert_eq!(
            escape_html_for_telegram("<b>Bold</b> and <i>italic</i>"),
            "<b>Bold</b> and <i>italic</i>"
        );
        assert_eq!(
            escape_html_for_telegram("<a href=\"https://x.com/?a=1&b=2\">link</a>"),
            "<a href=\"https://x.com/?a=1&b=2\">link</a>"
        );
        assert_eq!(
            escape_html_for_telegram("<tg-spoiler>hidden</tg-spoiler>"),
            "<tg-spoiler>hidden</tg-spoiler>"
        );
    }

    #[test]
    fn test_escape_html_mixes_tags_and_stray_markup() {
        assert_eq!(
            escape_html_for_telegram("<b>From</b> ChatGPT <noreply@openai.com> & co"),
            "<b>From</b> ChatGPT &lt;noreply@openai.com&gt; &amp; co"
        );
    }

    #[test]
    fn test_escape_html_keeps_entities_and_escapes_stray_amp() {
        assert_eq!(
            escape_html_for_telegram("Tom &amp; Jerry &#39;hi&#39; AT&T"),
            "Tom &amp; Jerry &#39;hi&#39; AT&amp;T"
        );
    }

    #[test]
    fn test_escape_html_rejects_unknown_and_attributed_plain_tags() {
        // Unknown tag → escaped.
        assert_eq!(escape_html_for_telegram("<div>x</div>"), "&lt;div&gt;x&lt;/div&gt;");
        // A tag that takes no attributes but has trailing text → escaped.
        assert_eq!(escape_html_for_telegram("<i am here>"), "&lt;i am here&gt;");
    }

    #[test]
    fn test_credentials_from_config() {
        let config = json!({
            "access_token": "BOT_TOKEN",
            "base_url": "https://api.telegram.org"
        });
        let creds = TelegramCredentials::from_config(&config).unwrap();
        assert_eq!(
            creds.api_url("sendMessage"),
            "https://api.telegram.org/botBOT_TOKEN/sendMessage"
        );
    }

    #[test]
    fn test_credentials_missing_token() {
        let config = json!({ "base_url": "https://api.telegram.org" });
        assert!(TelegramCredentials::from_config(&config).is_err());
    }
}
