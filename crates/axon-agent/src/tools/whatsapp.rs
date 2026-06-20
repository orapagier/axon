// whatsapp.rs
// WhatsApp Business Cloud API integration node for the workflow engine.
// Mirrors the structure of telegram.rs — same credential/client/operation pattern.
//
// Supported operations
// ─────────────────────
// message  : sendText, sendTemplate, sendImage, sendDocument, sendAudio,
//            sendVideo, sendSticker, sendLocation, sendInteractive,
//            markRead
// profile  : getProfile, updateProfile
//
// Config keys (all pulled from config: &Value)
// ─────────────────────────────────────────────
//   access_token      – Meta permanent / system user token
//   phone_number_id   – WhatsApp Business phone number ID
//   base_url          – default "https://graph.facebook.com/v19.0"
//   resource          – "message" | "profile"
//   operation         – see list above
//   + per-operation keys documented inline
//
// Trigger config keys
// ────────────────────
//   verify_token      – any secret string you set in the Meta webhook config
//   workflow_id       – used to derive a per-node verify token (optional)
//   node_id           – used to derive a per-node verify token (optional)

use anyhow::Result;
use reqwest::multipart;
use serde_json::{json, Value};
use std::time::Duration;

use crate::tools::schema::{ToolDefinition, ToolSource};

// ── Credentials ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct WhatsAppCredentials {
    access_token: String,
    phone_number_id: String,
    base_url: String,
}

impl WhatsAppCredentials {
    fn from_config(config: &Value) -> Result<Self, String> {
        let access_token = str_val(config, "access_token")
            .or_else(|| get_facebook_page_token())
            .ok_or_else(|| {
                "Missing Facebook/WhatsApp access token. Please run Facebook OAuth in Axon MCP."
                    .to_string()
            })?;

        let phone_number_id = str_val(config, "phone_number_id")
            .ok_or_else(|| "Missing required field 'phone_number_id' in config".to_string())?;

        let base_url = str_val(config, "base_url")
            .unwrap_or_else(|| "https://graph.facebook.com/v19.0".to_string());
        let base_url = base_url.trim_end_matches('/').to_string();

        Ok(Self {
            access_token,
            phone_number_id,
            base_url,
        })
    }

    /// Endpoint for sending messages: POST /{phone_number_id}/messages
    fn messages_url(&self) -> String {
        format!("{}/{}/messages", self.base_url, self.phone_number_id)
    }

    /// Endpoint for the business profile: GET|POST /{phone_number_id}/whatsapp_business_profile
    fn profile_url(&self) -> String {
        format!(
            "{}/{}/whatsapp_business_profile",
            self.base_url, self.phone_number_id
        )
    }

    /// Endpoint for uploading media: POST /{phone_number_id}/media
    fn media_url(&self) -> String {
        format!("{}/{}/media", self.base_url, self.phone_number_id)
    }
}

/// Base directory holding the integration services' on-disk token files
/// (`tokens.json`, `credentials.json`). Honors the `AXON_MCP_DATA_DIR` override for deployments
/// that stage these files elsewhere; otherwise the platform local-data dir,
/// matching `main.rs`. Tests also use the override to make the token fallback
/// hermetic instead of depending on whatever happens to be on the host.
fn axon_mcp_data_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("AXON_MCP_DATA_DIR") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir);
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("axon-mcp")
}

fn get_facebook_page_token() -> Option<String> {
    let data_dir = axon_mcp_data_dir();

    // Try tokens.json first
    let tokens_path = data_dir.join("tokens.json");
    if let Ok(raw) = std::fs::read_to_string(&tokens_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(t) = v
                .get("facebook")
                .and_then(|f| f.get("page_access_token"))
                .and_then(|s| s.as_str())
            {
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }

    // Fallback to credentials.json static token
    let creds_path = data_dir.join("credentials.json");
    if let Ok(raw) = std::fs::read_to_string(&creds_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(t) = v
                .get("facebook")
                .and_then(|f| f.get("page_access_token"))
                .and_then(|s| s.as_str())
            {
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

// ── HTTP client ───────────────────────────────────────────────────────────────

struct WhatsAppClient {
    http: reqwest::Client,
    creds: WhatsAppCredentials,
}

impl WhatsAppClient {
    fn new(creds: WhatsAppCredentials) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build WhatsApp HTTP client");
        Self { http, creds }
    }

    /// POST a JSON body. Adds Bearer auth header automatically.
    async fn post(&self, url: &str, body: Value) -> Result<Value, String> {
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.creds.access_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("WhatsApp request error: {e}"))?;

        self.parse_response(resp).await
    }

    /// GET request (used for profile reads).
    async fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.creds.access_token)
            .query(query)
            .send()
            .await
            .map_err(|e| format!("WhatsApp GET error: {e}"))?;

        self.parse_response(resp).await
    }

    /// POST multipart form (binary media upload).
    async fn post_multipart(&self, url: &str, form: multipart::Form) -> Result<Value, String> {
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.creds.access_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("WhatsApp multipart error: {e}"))?;

        self.parse_response(resp).await
    }

    async fn parse_response(&self, resp: reqwest::Response) -> Result<Value, String> {
        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse WhatsApp response: {e}"))?;

        if !status.is_success() {
            // Meta error shape: { "error": { "message": "...", "code": 190 } }
            let msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown WhatsApp API error");
            let code = json
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i64())
                .unwrap_or(0);
            return Err(format!("WhatsApp API error {status} (code {code}): {msg}"));
        }

        Ok(json)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn str_val(config: &Value, key: &str) -> Option<String> {
    config.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
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
    str_val(config, key).ok_or_else(|| format!("Missing required field '{key}' in WhatsApp config"))
}

fn bool_val(config: &Value, key: &str) -> bool {
    config.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Build the base message envelope that every send operation shares.
fn base_message(to: &str, msg_type: &str) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("messaging_product".into(), json!("whatsapp"));
    m.insert("recipient_type".into(), json!("individual"));
    m.insert("to".into(), json!(to));
    m.insert("type".into(), json!(msg_type));
    m
}

// ── Operations — message resource ─────────────────────────────────────────────

/// sendText — plain text message (with optional URL preview toggle).
///
/// Config keys:
///   to                – recipient phone number (E.164, e.g. "+639171234567")
///   text              – message body
///   preview_url       – bool, default false
async fn send_text(client: &WhatsAppClient, config: &Value) -> Result<Value, String> {
    let to = require_str(config, "to")?;
    let text = require_str(config, "text")?;
    let preview_url = bool_val(config, "preview_url");

    let mut body = base_message(&to, "text");
    body.insert(
        "text".into(),
        json!({ "body": text, "preview_url": preview_url }),
    );

    client
        .post(&client.creds.messages_url(), Value::Object(body))
        .await
}

/// sendTemplate — send a pre-approved WhatsApp template.
///
/// Config keys:
///   to                – recipient phone number
///   template_name     – approved template name
///   language_code     – e.g. "en_US", "fil" (default "en_US")
///   components        – JSON array of template component objects (optional)
///
/// Template components example:
/// [
///   { "type": "body", "parameters": [{ "type": "text", "text": "John" }] }
/// ]
async fn send_template(client: &WhatsAppClient, config: &Value) -> Result<Value, String> {
    let to = require_str(config, "to")?;
    let template_name = require_str(config, "template_name")?;
    let language_code = str_val(config, "language_code").unwrap_or_else(|| "en_US".to_string());

    let mut template_obj = serde_json::Map::new();
    template_obj.insert("name".into(), json!(template_name));
    template_obj.insert("language".into(), json!({ "code": language_code }));

    // Inline components or raw JSON string
    if let Some(raw) = config.get("components") {
        let components = if let Some(s) = raw.as_str() {
            serde_json::from_str::<Value>(s)
                .map_err(|e| format!("Invalid 'components' JSON: {e}"))?
        } else {
            raw.clone()
        };
        template_obj.insert("components".into(), components);
    }

    let mut body = base_message(&to, "template");
    body.insert("template".into(), Value::Object(template_obj));

    client
        .post(&client.creds.messages_url(), Value::Object(body))
        .await
}

/// sendInteractive — send buttons or list menus.
///
/// Config keys:
///   to                – recipient phone number
///   interactive_type  – "button" | "list"
///   body_text         – main message text
///   buttons           – JSON array for button type:
///                       [{ "id": "btn_yes", "title": "Yes" }, ...]  (max 3)
///   button_text       – header text for list type (the button label)
///   sections          – JSON array for list type:
///                       [{ "title": "Options", "rows": [{ "id": "r1", "title": "Row 1" }] }]
///   header_text       – optional header text
///   footer_text       – optional footer text
async fn send_interactive(client: &WhatsAppClient, config: &Value) -> Result<Value, String> {
    let to = require_str(config, "to")?;
    let interactive_type =
        str_val(config, "interactive_type").unwrap_or_else(|| "button".to_string());
    let body_text = require_str(config, "body_text")?;

    let mut interactive = serde_json::Map::new();
    interactive.insert("type".into(), json!(interactive_type));
    interactive.insert("body".into(), json!({ "text": body_text }));

    // Optional header
    if let Some(header) = str_val(config, "header_text") {
        interactive.insert("header".into(), json!({ "type": "text", "text": header }));
    }

    // Optional footer
    if let Some(footer) = str_val(config, "footer_text") {
        interactive.insert("footer".into(), json!({ "text": footer }));
    }

    match interactive_type.as_str() {
        "button" => {
            // buttons: [{ "id": "...", "title": "..." }]
            let raw_buttons = config
                .get("buttons")
                .ok_or("Missing 'buttons' for interactive button message")?;
            let buttons: Vec<Value> = if let Some(s) = raw_buttons.as_str() {
                serde_json::from_str(s).map_err(|e| format!("Invalid 'buttons' JSON: {e}"))?
            } else if let Some(arr) = raw_buttons.as_array() {
                arr.clone()
            } else if let Some(wrapped) = raw_buttons.get("parameters").and_then(|v| v.as_array()) {
                wrapped.clone()
            } else {
                return Err("'buttons' must be an array or a fixedCollection object".to_string());
            };

            // Meta expects: { "buttons": [{ "type": "reply", "reply": { "id": "...", "title": "..." } }] }
            let meta_buttons: Vec<Value> = buttons
                .iter()
                .map(|b| {
                    json!({
                        "type": "reply",
                        "reply": {
                            "id": b.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                            "title": b.get("title").and_then(|v| v.as_str()).unwrap_or("")
                        }
                    })
                })
                .collect();

            interactive.insert("action".into(), json!({ "buttons": meta_buttons }));
        }
        "list" => {
            let button_text =
                str_val(config, "button_text").unwrap_or_else(|| "Options".to_string());
            let raw_sections = config
                .get("sections")
                .ok_or("Missing 'sections' for interactive list message")?;
            let sections: Value = if let Some(s) = raw_sections.as_str() {
                serde_json::from_str(s).map_err(|e| format!("Invalid 'sections' JSON: {e}"))?
            } else {
                raw_sections.clone()
            };
            interactive.insert(
                "action".into(),
                json!({ "button": button_text, "sections": sections }),
            );
        }
        other => return Err(format!("Unknown interactive_type '{other}'")),
    }

    let mut body = base_message(&to, "interactive");
    body.insert("interactive".into(), Value::Object(interactive));

    client
        .post(&client.creds.messages_url(), Value::Object(body))
        .await
}

/// sendImage / sendDocument / sendAudio / sendVideo — media messages.
///
/// Config keys (shared):
///   to                – recipient phone number
///   media_type        – "image" | "document" | "audio" | "video" | "sticker"
///   link              – public URL of the media file (use this OR media_id)
///   media_id          – pre-uploaded media ID (use this OR link)
///   caption           – optional caption (image/document/video only)
///   filename          – optional filename (document only)
///   binary_data       – bool: upload file from disk first, then send by media_id
///   file              – { local_path, original_name, mime_type } (requires binary_data=true)
async fn send_media(
    client: &WhatsAppClient,
    config: &Value,
    media_type: &str,
) -> Result<Value, String> {
    let to = require_str(config, "to")?;

    // If binary_data, upload to Meta first to get a media_id.
    let media_id = if bool_val(config, "binary_data") {
        Some(upload_media(client, config).await?)
    } else {
        str_val(config, "media_id")
    };

    let mut media_obj = serde_json::Map::new();

    if let Some(id) = media_id {
        media_obj.insert("id".into(), json!(id));
    } else if let Some(link) = str_val(config, "link") {
        media_obj.insert("link".into(), json!(link));
    } else {
        return Err(format!(
            "Either 'link', 'media_id', or 'binary_data' + 'file' must be provided for {media_type}"
        ));
    }

    if let Some(caption) = str_val(config, "caption") {
        media_obj.insert("caption".into(), json!(caption));
    }
    if let Some(filename) = str_val(config, "filename") {
        media_obj.insert("filename".into(), json!(filename));
    }

    let mut body = base_message(&to, media_type);
    body.insert(media_type.into(), Value::Object(media_obj));

    client
        .post(&client.creds.messages_url(), Value::Object(body))
        .await
}

/// sendLocation — send a pin on the map.
///
/// Config keys:
///   to                – recipient phone number
///   latitude          – float
///   longitude         – float
///   name              – optional place name
///   address           – optional address string
async fn send_location(client: &WhatsAppClient, config: &Value) -> Result<Value, String> {
    let to = require_str(config, "to")?;
    let latitude = config
        .get("latitude")
        .and_then(|v| v.as_f64())
        .ok_or("Missing 'latitude'")?;
    let longitude = config
        .get("longitude")
        .and_then(|v| v.as_f64())
        .ok_or("Missing 'longitude'")?;

    let mut location = serde_json::Map::new();
    location.insert("latitude".into(), json!(latitude));
    location.insert("longitude".into(), json!(longitude));
    if let Some(name) = str_val(config, "name") {
        location.insert("name".into(), json!(name));
    }
    if let Some(address) = str_val(config, "address") {
        location.insert("address".into(), json!(address));
    }

    let mut body = base_message(&to, "location");
    body.insert("location".into(), Value::Object(location));

    client
        .post(&client.creds.messages_url(), Value::Object(body))
        .await
}

/// markRead — mark a received message as read (shows blue ticks).
///
/// Config keys:
///   message_id        – the wamid of the received message (from webhook payload)
async fn mark_read(client: &WhatsAppClient, config: &Value) -> Result<Value, String> {
    let message_id = require_str(config, "message_id")?;

    let body = json!({
        "messaging_product": "whatsapp",
        "status": "read",
        "message_id": message_id
    });

    client.post(&client.creds.messages_url(), body).await
}

// ── Operations — profile resource ─────────────────────────────────────────────

/// getProfile — retrieve the WhatsApp Business profile fields.
///
/// Config keys:
///   fields            – comma-separated fields (default: "about,address,description,email,profile_picture_url,websites,vertical")
async fn get_profile(client: &WhatsAppClient, config: &Value) -> Result<Value, String> {
    let fields = str_val(config, "fields").unwrap_or_else(|| {
        "about,address,description,email,profile_picture_url,websites,vertical".to_string()
    });
    client
        .get(&client.creds.profile_url(), &[("fields", fields.as_str())])
        .await
}

/// updateProfile — update WhatsApp Business profile fields.
///
/// Config keys (all optional, supply only what you want to change):
///   about             – status text
///   address           – business address
///   description       – business description
///   email             – business email
///   vertical          – business vertical (e.g. "RETAIL")
///   websites          – JSON array of URL strings (max 2)
async fn update_profile(client: &WhatsAppClient, config: &Value) -> Result<Value, String> {
    let mut profile = serde_json::Map::new();
    profile.insert("messaging_product".into(), json!("whatsapp"));

    for key in &["about", "address", "description", "email", "vertical"] {
        if let Some(v) = str_val(config, key) {
            profile.insert(key.to_string(), json!(v));
        }
    }

    if let Some(raw) = config.get("websites") {
        let websites: Value = if let Some(s) = raw.as_str() {
            serde_json::from_str(s).map_err(|e| format!("Invalid 'websites' JSON: {e}"))?
        } else {
            raw.clone()
        };
        profile.insert("websites".into(), websites);
    }

    client
        .post(&client.creds.profile_url(), Value::Object(profile))
        .await
}

// ── Media upload helper ───────────────────────────────────────────────────────

/// Upload a local file to Meta's media endpoint and return the media_id.
/// Used internally when binary_data=true.
async fn upload_media(client: &WhatsAppClient, config: &Value) -> Result<String, String> {
    let file_val = config
        .get("file")
        .ok_or("Missing 'file' object for binary upload")?;

    let local_path = file_val
        .as_str()
        .or_else(|| file_val.get("local_path").and_then(|v| v.as_str()))
        .ok_or("Missing 'local_path' in file object")?;

    let file_name = file_val
        .get("original_name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            std::path::Path::new(local_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string()
        });

    let mime = file_val
        .get("mime_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            mime_guess::from_path(local_path)
                .first_or_octet_stream()
                .to_string()
        });

    let bytes = tokio::fs::read(local_path)
        .await
        .map_err(|e| format!("Failed to read file: {e}"))?;

    let part = multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str(&mime)
        .map_err(|e| format!("Invalid MIME type: {e}"))?;

    let form = multipart::Form::new()
        .text("messaging_product", "whatsapp")
        .part("file", part);

    let result = client
        .post_multipart(&client.creds.media_url(), form)
        .await?;

    result
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Meta media upload did not return an 'id'".to_string())
}

// ── Public executor ───────────────────────────────────────────────────────────

/// Drop-in replacement for other node executors.
/// Wire up with: `"whatsapp" => execute_whatsapp_node(&config).await`
pub async fn execute_whatsapp_node(config: &Value) -> Result<Value, String> {
    let creds = WhatsAppCredentials::from_config(config)?;
    let client = WhatsAppClient::new(creds);

    let resource = str_val(config, "resource").unwrap_or_else(|| "message".to_string());
    let operation = str_val(config, "operation").unwrap_or_else(|| "sendText".to_string());

    match (resource.as_str(), operation.as_str()) {
        // ── message ──────────────────────────────────────────────────────────
        ("message", "sendText") => send_text(&client, config).await,
        ("message", "sendTemplate") => send_template(&client, config).await,
        ("message", "sendInteractive") => send_interactive(&client, config).await,
        ("message", "sendImage") => send_media(&client, config, "image").await,
        ("message", "sendDocument") => send_media(&client, config, "document").await,
        ("message", "sendAudio") => send_media(&client, config, "audio").await,
        ("message", "sendVideo") => send_media(&client, config, "video").await,
        ("message", "sendSticker") => send_media(&client, config, "sticker").await,
        ("message", "sendLocation") => send_location(&client, config).await,
        ("message", "markRead") => mark_read(&client, config).await,

        // ── profile ──────────────────────────────────────────────────────────
        ("profile", "getProfile") => get_profile(&client, config).await,
        ("profile", "updateProfile") => update_profile(&client, config).await,

        _ => Err(format!(
            "WhatsApp: unknown resource/operation combination '{resource}/{operation}'"
        )),
    }
}

// ── WhatsApp Trigger (webhook) ────────────────────────────────────────────────
//
// Meta sends TWO kinds of requests to your webhook URL:
//
//   1. GET  — webhook verification challenge (one-time, when you register the webhook)
//   2. POST — inbound message/status update events
//
// Call `verify_whatsapp_webhook` from your GET route handler.
// Call `handle_whatsapp_webhook` from your POST route handler.
//
// Expected trigger config keys
// ─────────────────────────────
//   verify_token  – the secret you set in the Meta Developer Console
//   workflow_id   – optional, used to derive a per-node verify token
//   node_id       – optional, used to derive a per-node verify token

#[derive(Debug)]
pub enum WebhookVerifyResult {
    /// Return this string as the HTTP 200 response body.
    Challenge(String),
    /// Return HTTP 403.
    Forbidden { reason: String },
}

/// Handle the GET verification handshake from Meta.
///
/// `hub_mode`         – value of `hub.mode` query param  (must be "subscribe")
/// `hub_challenge`    – value of `hub.challenge` query param (echo back on success)
/// `hub_verify_token` – value of `hub.verify_token` query param
/// `config`           – trigger node config
pub fn verify_whatsapp_webhook(
    hub_mode: &str,
    hub_challenge: &str,
    hub_verify_token: &str,
    config: &Value,
) -> WebhookVerifyResult {
    if hub_mode != "subscribe" {
        return WebhookVerifyResult::Forbidden {
            reason: format!("Unexpected hub.mode: '{hub_mode}'"),
        };
    }

    let expected = expected_verify_token(config);

    // Constant-time compare to avoid timing attacks.
    if !constant_time_eq(hub_verify_token.as_bytes(), expected.as_bytes()) {
        return WebhookVerifyResult::Forbidden {
            reason: "Invalid verify_token".into(),
        };
    }

    WebhookVerifyResult::Challenge(hub_challenge.to_string())
}

#[derive(Debug)]
pub enum TriggerResult {
    /// Ignored (e.g. status updates when the node only wants messages).
    Ignored { reason: String },
    /// Accepted — carry each entry forward into the workflow.
    Accepted(Vec<Value>),
}

/// Parse an inbound POST from Meta and extract the message entries.
///
/// `body`   – parsed JSON body from Meta
/// `config` – trigger node config
///
/// Config filter keys:
///   event_types  – comma-separated list of event types to accept:
///                  "message" | "status" | "all" (default "message")
pub async fn handle_whatsapp_webhook(body: Value, config: &Value) -> TriggerResult {
    // Meta wraps everything in: { "object": "whatsapp_business_account", "entry": [...] }
    if body.get("object").and_then(|v| v.as_str()) != Some("whatsapp_business_account") {
        return TriggerResult::Ignored {
            reason: "Not a WhatsApp Business Account event".into(),
        };
    }

    let event_types = str_val(config, "event_types").unwrap_or_else(|| "message".to_string());
    let accept_messages = event_types.contains("message") || event_types.contains("all");
    let accept_statuses = event_types.contains("status") || event_types.contains("all");

    let entries = match body.get("entry").and_then(|v| v.as_array()) {
        Some(e) => e,
        None => {
            return TriggerResult::Ignored {
                reason: "No 'entry' array in webhook payload".into(),
            }
        }
    };

    let mut accepted: Vec<Value> = Vec::new();

    for entry in entries {
        let changes = match entry.get("changes").and_then(|v| v.as_array()) {
            Some(c) => c,
            None => continue,
        };

        for change in changes {
            if change.get("field").and_then(|v| v.as_str()) != Some("messages") {
                continue;
            }

            let value = match change.get("value") {
                Some(v) => v,
                None => continue,
            };

            // Inbound messages (only process actual messages to prevent geometric explosion loops
            // from delivery statuses triggering Agent replies in an infinite cycle).
            if accept_messages {
                if let Some(messages) = value.get("messages").and_then(|v| v.as_array()) {
                    for msg in messages {
                        // Enrich with metadata (phone_number_id, display_phone_number)
                        let mut enriched = msg.clone();
                        if let Some(meta) = value.get("metadata") {
                            enriched["_metadata"] = meta.clone();
                        }
                        // Attach sender profile if present
                        if let Some(contacts) = value.get("contacts").and_then(|v| v.as_array()) {
                            if let Some(contact) = contacts.first() {
                                enriched["_contact"] = contact.clone();
                            }
                        }
                        accepted.push(enriched);
                    }
                }
            }

            // Delivery / read status updates
            // (Disabled by default from triggering standard agent workflows to strictly prevent
            // infinite cascade loops. If you need to process delivery receipts, do so via a
            // separate read-only workflow path that does not contain response nodes).
            if accept_statuses {
                if let Some(statuses) = value.get("statuses").and_then(|v| v.as_array()) {
                    if !accept_messages {
                        // If they specifically requested ONLY statuses, let them have it,
                        // assuming they know what they are doing.
                        for status in statuses {
                            accepted.push(status.clone());
                        }
                    } else {
                        tracing::debug!("Ignored status webhook to prevent loop: {:?}", statuses);
                    }
                }
            }
        }
    }

    if accepted.is_empty() {
        TriggerResult::Ignored {
            reason: "No accepted events found in payload".into(),
        }
    } else {
        TriggerResult::Accepted(accepted)
    }
}

/// Derive the expected verify token.
/// If `verify_token` is set directly, use it.
/// Otherwise fall back to "{workflow_id}_{node_id}" (same pattern as Telegram).
fn expected_verify_token(config: &Value) -> String {
    if let Some(token) = str_val(config, "verify_token") {
        return token;
    }
    let workflow_id = str_val(config, "workflow_id").unwrap_or_default();
    let node_id = str_val(config, "node_id").unwrap_or_default();
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect()
    };
    format!("{}_{}", clean(&workflow_id), clean(&node_id))
}

// ── Tool definitions ──────────────────────────────────────────────────────────

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "whatsapp".to_string(),
        description: "Send messages, templates, interactive buttons, media, and locations \
             via the WhatsApp Business Cloud API. Manage your business profile."
            .to_string(),
        parameters: serde_json::json!({
            "phone_number_id": {
                "type": "string",
                "description": "WhatsApp Business phone number ID from Meta Developer Console"
            },
            "base_url": {
                "type": "string",
                "description": "Meta Graph API base URL (default: https://graph.facebook.com/v19.0)"
            },
            "resource": {
                "type": "string",
                "enum": ["message", "profile"],
                "description": "Resource group"
            },
            "operation": {
                "type": "string",
                "enum": [
                    "sendText", "sendTemplate", "sendInteractive",
                    "sendImage", "sendDocument", "sendAudio", "sendVideo", "sendSticker",
                    "sendLocation", "markRead",
                    "getProfile", "updateProfile"
                ],
                "description": "Operation to perform"
            },
            "to": {
                "type": "string",
                "description": "Recipient phone number in E.164 format (e.g. +639171234567)"
            },
            "text": {
                "type": "string",
                "description": "Message text for sendText"
            },
            "preview_url": {
                "type": "boolean",
                "default": false,
                "description": "Enable URL previews in text messages"
            },
            "template_name": {
                "type": "string",
                "description": "Approved template name for sendTemplate"
            },
            "language_code": {
                "type": "string",
                "default": "en_US",
                "description": "Template language code (e.g. 'en_US', 'fil')"
            },
            "components": {
                "type": "array",
                "description": "Template component parameters array (JSON array or string)"
            },
            "interactive_type": {
                "type": "string",
                "enum": ["button", "list"],
                "default": "button",
                "description": "Interactive message sub-type"
            },
            "body_text": {
                "type": "string",
                "description": "Body text for interactive messages"
            },
            "header_text": { "type": "string" },
            "footer_text": { "type": "string" },
            "buttons": {
                "type": "array",
                "description": "Button definitions for interactive button messages: [{\"id\":\"...\",\"title\":\"...\"}] (max 3)"
            },
            "button_text": {
                "type": "string",
                "description": "Label for the list menu button (interactive list only)"
            },
            "sections": {
                "type": "array",
                "description": "List sections for interactive list messages: [{\"title\":\"...\",\"rows\":[{\"id\":\"...\",\"title\":\"...\"}]}]"
            },
            "link": {
                "type": "string",
                "description": "Public URL of the media file"
            },
            "media_id": {
                "type": "string",
                "description": "Pre-uploaded Meta media ID"
            },
            "caption": { "type": "string" },
            "filename": {
                "type": "string",
                "description": "Display filename for document messages"
            },
            "binary_data": {
                "type": "boolean",
                "description": "Upload file from disk before sending"
            },
            "file": {
                "type": "object",
                "description": "{ local_path, original_name, mime_type } (requires binary_data=true)"
            },
            "latitude": { "type": "number" },
            "longitude": { "type": "number" },
            "name": { "type": "string", "description": "Place name for location messages" },
            "address": { "type": "string", "description": "Address string for location messages" },
            "message_id": {
                "type": "string",
                "description": "wamid of the message to mark as read"
            },
            "fields": {
                "type": "string",
                "description": "Comma-separated profile fields to retrieve (getProfile)"
            },
            "about": { "type": "string" },
            "description": { "type": "string" },
            "email": { "type": "string" },
            "vertical": { "type": "string" },
            "websites": {
                "type": "array",
                "description": "Array of website URLs (max 2) for updateProfile"
            }
        }),
        required: vec![
            "phone_number_id".to_string(),
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
        name: "whatsapp_trigger".to_string(),
        description: "Receive inbound WhatsApp messages and status updates via the Meta \
             webhook. Handles the GET verification challenge automatically."
            .to_string(),
        parameters: serde_json::json!({
            "verify_token": {
                "type": "string",
                "description": "Secret token you configured in the Meta Developer Console"
            },
            "workflow_id": { "type": "string" },
            "node_id":     { "type": "string" },
            "event_types": {
                "type": "string",
                "description": "Comma-separated event types to accept: 'message', 'status', or 'all' (default 'message')"
            }
        }),
        required: vec!["verify_token".to_string()],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: true,
    }
}

// ── Utility ───────────────────────────────────────────────────────────────────

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
    fn test_credentials_from_config() {
        let config = json!({
            "access_token": "EAAabc123",
            "phone_number_id": "12345678"
        });
        let creds = WhatsAppCredentials::from_config(&config).unwrap();
        assert_eq!(
            creds.messages_url(),
            "https://graph.facebook.com/v19.0/12345678/messages"
        );
    }

    #[test]
    fn test_credentials_missing_token() {
        // Point the Facebook-token fallback at a non-existent dir so the result
        // doesn't depend on whether a real token file exists on the host (which
        // would otherwise satisfy the fallback and make this assertion flaky).
        let empty_dir = std::env::temp_dir().join(format!(
            "axon-wa-no-token-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::env::set_var("AXON_MCP_DATA_DIR", &empty_dir);
        let config = json!({ "phone_number_id": "12345678" });
        let result = WhatsAppCredentials::from_config(&config);
        std::env::remove_var("AXON_MCP_DATA_DIR");

        assert!(
            result.is_err(),
            "expected missing-token error when no access_token and no on-disk fallback"
        );
    }

    #[test]
    fn test_credentials_missing_phone_number_id() {
        let config = json!({ "access_token": "EAAabc123" });
        assert!(WhatsAppCredentials::from_config(&config).is_err());
    }

    #[test]
    fn test_verify_webhook_success() {
        let config = json!({ "verify_token": "my_secret" });
        match verify_whatsapp_webhook("subscribe", "challenge_xyz", "my_secret", &config) {
            WebhookVerifyResult::Challenge(c) => assert_eq!(c, "challenge_xyz"),
            other => panic!("Expected Challenge, got {:?}", other),
        }
    }

    #[test]
    fn test_verify_webhook_wrong_token() {
        let config = json!({ "verify_token": "my_secret" });
        match verify_whatsapp_webhook("subscribe", "challenge_xyz", "wrong_token", &config) {
            WebhookVerifyResult::Forbidden { .. } => {}
            other => panic!("Expected Forbidden, got {:?}", other),
        }
    }

    #[test]
    fn test_verify_webhook_wrong_mode() {
        let config = json!({ "verify_token": "my_secret" });
        match verify_whatsapp_webhook("unsubscribe", "challenge_xyz", "my_secret", &config) {
            WebhookVerifyResult::Forbidden { .. } => {}
            other => panic!("Expected Forbidden, got {:?}", other),
        }
    }

    #[test]
    fn test_expected_verify_token_direct() {
        let config = json!({ "verify_token": "direct_secret" });
        assert_eq!(expected_verify_token(&config), "direct_secret");
    }

    #[test]
    fn test_expected_verify_token_derived() {
        let config = json!({ "workflow_id": "wf#1", "node_id": "node@2" });
        assert_eq!(expected_verify_token(&config), "wf1_node2");
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hi", b"hello"));
    }

    #[test]
    fn test_base_message_shape() {
        let msg = base_message("+639171234567", "text");
        assert_eq!(msg["messaging_product"], "whatsapp");
        assert_eq!(msg["to"], "+639171234567");
        assert_eq!(msg["type"], "text");
    }
}
