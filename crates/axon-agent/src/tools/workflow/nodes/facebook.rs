//! Facebook Page workflow action node.
//!
//! Performs outbound Page actions — replying to comments, replying to Messenger
//! chats, posting, liking/hiding/deleting comments — using a Page access token.
//!
//! Multi-account: the node selects a `credential_id` (service "facebook"), and
//! `interpolate_config` merges that credential's data into `config` before this
//! runs, so the Page token arrives as `page_access_token` and the Page id as
//! `page_id`. Each credential is created by the "Connect a Page" OAuth flow.
//! Error/JSON handling mirrors `nodes::discord`.

use serde_json::{json, Value};

const FB_API: &str = "https://graph.facebook.com/v25.0";

/// Default field projection for a Page post/feed read.
const POST_FIELDS: &str = "id,message,story,created_time,full_picture,permalink_url,\
     likes.summary(true),comments.summary(true),shares";

// ── Config helpers ──────────────────────────────────────────────────────────

/// Read a scalar config value as a string. Objects/arrays (from resolved
/// expressions) are stringified; null/missing → None.
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

/// Like `str_val` but treats missing/empty/whitespace as a hard error.
fn require(config: &Value, key: &str) -> Result<String, String> {
    match str_val(config, key) {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(format!("Missing required field '{key}' in Facebook config")),
    }
}

/// The Graph API `fields` projection for a read: an explicit `fields` config
/// value (power users requesting exactly what they want) overrides the
/// comprehensive `default`. Graph rejects the whole call if any requested field
/// is inaccessible, so `default` stays to the broadly-safe set for a Page token.
fn fields_or(config: &Value, default: &str) -> String {
    str_val(config, "fields")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Parse the `limit` config as a page size clamped to Graph's 1–100 range.
fn limit_or(config: &Value, default: u32) -> u32 {
    str_val(config, "limit")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(default)
        .clamp(1, 100)
}

/// Resolve the Page access token from merged credential data.
fn page_token(config: &Value) -> Result<String, String> {
    str_val(config, "page_access_token")
        .or_else(|| str_val(config, "access_token"))
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| {
            "Missing Facebook Page token. Pick a credential (service 'facebook') in this node, \
             or click Connect to add one."
                .to_string()
        })
}

// ── HTTP response handling ──────────────────────────────────────────────────

/// Consume a Graph API response: non-2xx → `Err(<body text>)`; empty / "true"
/// (like/delete endpoints) → `{success:true}`; otherwise the parsed JSON.
async fn finish(resp: reqwest::Response) -> Result<Value, String> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Facebook: failed to read response: {e}"))?;
    if !status.is_success() {
        return Err(format!("Facebook API error {status}: {}", text.trim()));
    }
    if text.trim().is_empty() || text.trim() == "true" {
        return Ok(json!({ "success": true }));
    }
    serde_json::from_str(&text).map_err(|e| format!("Facebook: failed to parse response: {e}"))
}

// ── Operations ──────────────────────────────────────────────────────────────

/// Reply to a comment (creates a child comment under `comment_id`).
async fn reply_comment(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let comment_id = require(config, "comment_id")?;
    let message = require(config, "message")?;
    let resp = client
        .post(format!("{FB_API}/{comment_id}/comments"))
        .bearer_auth(token)
        .form(&[("message", message.as_str())])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Comment on a post/object directly (not a reply).
async fn comment(client: &reqwest::Client, token: &str, config: &Value) -> Result<Value, String> {
    let object_id = require(config, "object_id")?;
    let message = require(config, "message")?;
    let resp = client
        .post(format!("{FB_API}/{object_id}/comments"))
        .bearer_auth(token)
        .form(&[("message", message.as_str())])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Send a Messenger text reply to a user PSID via the Page.
async fn send_message(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let recipient_id = require(config, "recipient_id")?;
    let message = require(config, "message")?;
    let resp = client
        .post(format!("{FB_API}/{page_id}/messages"))
        .bearer_auth(token)
        .json(&json!({
            "recipient": { "id": recipient_id },
            "message": { "text": message },
            "messaging_type": "RESPONSE",
        }))
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Create a text post (optionally with a link) on the Page feed.
async fn create_post(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let message = require(config, "message")?;
    let mut form: Vec<(&str, String)> = vec![("message", message)];
    if let Some(link) = str_val(config, "link").filter(|s| !s.trim().is_empty()) {
        form.push(("link", link));
    }
    apply_schedule(config, &mut form);
    let resp = client
        .post(format!("{FB_API}/{page_id}/feed"))
        .bearer_auth(token)
        .query(&[("fields", "id,permalink_url")])
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// When `scheduled_publish_time` (a future unix timestamp) is set, flip the feed/
/// photo create into a scheduled draft: Graph needs `published=false` alongside it.
fn apply_schedule(config: &Value, form: &mut Vec<(&str, String)>) {
    if let Some(ts) = str_val(config, "scheduled_publish_time").filter(|s| !s.trim().is_empty()) {
        form.push(("published", "false".to_string()));
        form.push(("scheduled_publish_time", ts));
    }
}

/// Like a post/comment/photo.
async fn like_object(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let object_id = require(config, "object_id")?;
    let resp = client
        .post(format!("{FB_API}/{object_id}/likes"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Hide or unhide a comment.
async fn hide_comment(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let comment_id = require(config, "comment_id")?;
    let hide = config
        .get("hide")
        .and_then(|v| v.as_bool())
        .or_else(|| str_val(config, "hide").map(|s| s.eq_ignore_ascii_case("true")))
        .unwrap_or(true);
    let resp = client
        .post(format!("{FB_API}/{comment_id}"))
        .bearer_auth(token)
        .form(&[("is_hidden", if hide { "true" } else { "false" })])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Delete a comment.
async fn delete_comment(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let comment_id = require(config, "comment_id")?;
    let resp = client
        .delete(format!("{FB_API}/{comment_id}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// List comments on a post/object.
async fn get_comments(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let object_id = require(config, "object_id")?;
    let limit = limit_or(config, 25);
    let fields = fields_or(
        config,
        "id,message,from{id,name},created_time,like_count,comment_count,parent{id},\
         attachment,permalink_url,is_hidden,is_private,can_hide,can_remove,\
         can_reply_privately,user_likes,message_tags,reactions.summary(true)",
    );
    let resp = client
        .get(format!("{FB_API}/{object_id}/comments"))
        .bearer_auth(token)
        .query(&[
            ("limit", limit.to_string()),
            ("order", "reverse_chronological".to_string()),
            ("fields", fields),
        ])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Fetch a Messenger conversation thread by user PSID — participant names and
/// recent messages. Queries the Page `conversations` edge filtered by `user_id`,
/// expanding `participants` (names) and `messages` (the thread history).
async fn get_thread(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let recipient_id = require(config, "recipient_id")?;
    let limit = limit_or(config, 25);
    // A `fields` override controls the entire projection (including the messages
    // subfield and its own limit). The default expands the thread metadata and
    // pulls full message objects — text, sender, attachments, stickers, shares.
    let fields = fields_or(
        config,
        &format!(
            "id,link,snippet,updated_time,message_count,unread_count,can_reply,\
             is_subscribed,participants,senders,former_participants,\
             messages.limit({limit}){{id,message,from,to,created_time,attachments,sticker,shares,tags}}"
        ),
    );
    let resp = client
        .get(format!("{FB_API}/{page_id}/conversations"))
        .bearer_auth(token)
        .query(&[
            ("user_id", recipient_id),
            ("platform", "messenger".to_string()),
            ("fields", fields),
        ])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Create a photo post from an image URL (`message` becomes the caption).
async fn create_photo_post(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let image_url = require(config, "image_url")?;
    let mut form: Vec<(&str, String)> = vec![("url", image_url)];
    if let Some(caption) = str_val(config, "message").filter(|s| !s.trim().is_empty()) {
        form.push(("caption", caption));
    }
    apply_schedule(config, &mut form);
    let resp = client
        .post(format!("{FB_API}/{page_id}/photos"))
        .bearer_auth(token)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Create a video post from a hosted video URL (`message` becomes the description).
async fn create_video_post(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let video_url = require(config, "video_url")?;
    let mut form: Vec<(&str, String)> = vec![("file_url", video_url)];
    if let Some(desc) = str_val(config, "message").filter(|s| !s.trim().is_empty()) {
        form.push(("description", desc));
    }
    let resp = client
        .post(format!("{FB_API}/{page_id}/videos"))
        .bearer_auth(token)
        .query(&[("fields", "id,permalink_url")])
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Edit the text of an existing post.
async fn update_post(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let post_id = require(config, "post_id")?;
    let message = require(config, "message")?;
    let resp = client
        .post(format!("{FB_API}/{post_id}"))
        .bearer_auth(token)
        .form(&[("message", message.as_str())])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Delete a post.
async fn delete_post(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let post_id = require(config, "post_id")?;
    let resp = client
        .delete(format!("{FB_API}/{post_id}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// List recent posts on the Page feed.
async fn get_posts(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let limit = limit_or(config, 25);
    let fields = fields_or(config, POST_FIELDS);
    let resp = client
        .get(format!("{FB_API}/{page_id}/feed"))
        .bearer_auth(token)
        .query(&[("limit", limit.to_string()), ("fields", fields)])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Fetch a single post by id.
async fn get_post(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let post_id = require(config, "post_id")?;
    let fields = fields_or(config, POST_FIELDS);
    let resp = client
        .get(format!("{FB_API}/{post_id}"))
        .bearer_auth(token)
        .query(&[("fields", fields)])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// List scheduled (not-yet-published) posts.
async fn get_scheduled_posts(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let limit = limit_or(config, 25);
    let resp = client
        .get(format!("{FB_API}/{page_id}/feed"))
        .bearer_auth(token)
        .query(&[
            ("limit", limit.to_string()),
            ("is_published", "false".to_string()),
            (
                "fields",
                "id,message,scheduled_publish_time,permalink_url".to_string(),
            ),
        ])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Remove the Page's like from a post/comment/photo.
async fn unlike_object(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let object_id = require(config, "object_id")?;
    let resp = client
        .delete(format!("{FB_API}/{object_id}/likes"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// React to an object as the Page. LIKE goes through the `likes` edge; any other
/// type through `reactions`. ANGRY is blocked as a guardrail (mirrors the agent).
async fn react_object(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let object_id = require(config, "object_id")?;
    let reaction = str_val(config, "reaction_type")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "LIKE".to_string())
        .to_uppercase();
    if reaction == "ANGRY" {
        return Err("ANGRY reaction is not allowed (guardrail)".to_string());
    }
    let req = if reaction == "LIKE" {
        client.post(format!("{FB_API}/{object_id}/likes"))
    } else {
        client
            .post(format!("{FB_API}/{object_id}/reactions"))
            .query(&[("type", reaction)])
    };
    let resp = req
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// List the reactions on an object.
async fn get_reactions(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let object_id = require(config, "object_id")?;
    let limit = limit_or(config, 25);
    let resp = client
        .get(format!("{FB_API}/{object_id}/reactions"))
        .bearer_auth(token)
        .query(&[
            ("limit", limit.to_string()),
            ("summary", "true".to_string()),
            ("fields", fields_or(config, "id,name,type")),
        ])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// List the likes on an object.
async fn get_likes(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let object_id = require(config, "object_id")?;
    let limit = limit_or(config, 25);
    let resp = client
        .get(format!("{FB_API}/{object_id}/likes"))
        .bearer_auth(token)
        .query(&[
            ("limit", limit.to_string()),
            ("summary", "true".to_string()),
        ])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Fetch a single comment by id.
async fn get_comment(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let comment_id = require(config, "comment_id")?;
    let fields = fields_or(
        config,
        "id,message,from{id,name},created_time,like_count,comment_count,parent{id},\
         attachment,permalink_url,is_hidden,is_private,can_hide,can_remove,\
         can_reply_privately,user_likes,message_tags,reactions.summary(true)",
    );
    let resp = client
        .get(format!("{FB_API}/{comment_id}"))
        .bearer_auth(token)
        .query(&[("fields", fields)])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Send a private (Messenger) reply to a public comment, addressed by comment id.
async fn private_reply(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let comment_id = require(config, "comment_id")?;
    let message = require(config, "message")?;
    let resp = client
        .post(format!("{FB_API}/{page_id}/messages"))
        .bearer_auth(token)
        .json(&json!({
            "recipient": { "comment_id": comment_id },
            "message": { "text": message },
        }))
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Send a Messenger image to a PSID via an image attachment URL.
async fn send_image(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let recipient_id = require(config, "recipient_id")?;
    let image_url = require(config, "image_url")?;
    let resp = client
        .post(format!("{FB_API}/{page_id}/messages"))
        .bearer_auth(token)
        .json(&json!({
            "recipient": { "id": recipient_id },
            "message": {
                "attachment": {
                    "type": "image",
                    "payload": { "url": image_url, "is_reusable": true },
                },
            },
            "messaging_type": "RESPONSE",
        }))
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Send a Messenger sender action to a PSID (typing_on / typing_off / mark_seen).
async fn send_action(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let recipient_id = require(config, "recipient_id")?;
    let action = str_val(config, "sender_action")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "typing_on".to_string());
    let resp = client
        .post(format!("{FB_API}/{page_id}/messages"))
        .bearer_auth(token)
        .json(&json!({
            "recipient": { "id": recipient_id },
            "sender_action": action,
        }))
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// List Messenger conversations for the Page.
async fn get_conversations(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let limit = limit_or(config, 25);
    let fields = fields_or(
        config,
        "id,participants,updated_time,message_count,unread_count,snippet,can_reply",
    );
    let resp = client
        .get(format!("{FB_API}/{page_id}/conversations"))
        .bearer_auth(token)
        .query(&[
            ("platform", "messenger".to_string()),
            ("limit", limit.to_string()),
            ("fields", fields),
        ])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Fetch the Page's profile/metadata.
async fn get_page_info(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let fields = fields_or(
        config,
        "id,name,username,about,category,fan_count,followers_count,link,website,\
         phone,emails,hours,location,rating_count,overall_star_rating,\
         verification_status,description,cover,picture,is_published",
    );
    let resp = client
        .get(format!("{FB_API}/{page_id}"))
        .bearer_auth(token)
        .query(&[("fields", fields)])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Update editable Page profile fields (about / description / phone / website).
async fn update_page(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let mut form: Vec<(&str, String)> = Vec::new();
    for key in ["about", "description", "phone", "website"] {
        if let Some(v) = str_val(config, key).filter(|s| !s.trim().is_empty()) {
            form.push((key, v));
        }
    }
    if form.is_empty() {
        return Err(
            "Provide at least one Page field to update (about, description, phone, website)."
                .to_string(),
        );
    }
    let resp = client
        .post(format!("{FB_API}/{page_id}"))
        .bearer_auth(token)
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Page-level insights for one or more metrics over a period.
async fn get_page_insights(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let page_id = require(config, "page_id")?;
    let metric = str_val(config, "metric")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "page_impressions,page_post_engagements,page_daily_follows".to_string());
    let period = str_val(config, "period")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "day".to_string());
    let resp = client
        .get(format!("{FB_API}/{page_id}/insights"))
        .bearer_auth(token)
        .query(&[("metric", metric), ("period", period)])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

/// Post-level insights (impressions, engaged users, clicks, reactions).
async fn get_post_insights(
    client: &reqwest::Client,
    token: &str,
    config: &Value,
) -> Result<Value, String> {
    let post_id = require(config, "post_id")?;
    let metric = str_val(config, "metric")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            "post_impressions,post_impressions_unique,post_engaged_users,post_clicks,\
             post_reactions_by_type_total"
                .to_string()
        });
    let resp = client
        .get(format!("{FB_API}/{post_id}/insights"))
        .bearer_auth(token)
        .query(&[("metric", metric)])
        .send()
        .await
        .map_err(|e| format!("Facebook request error: {e}"))?;
    finish(resp).await
}

// ── Public executor ───────────────────────────────────────────────────────────

pub(crate) async fn execute(config: &Value) -> Result<Value, String> {
    let client = reqwest::Client::new();
    let token = page_token(config)?;
    let operation = str_val(config, "operation").unwrap_or_else(|| "replyComment".to_string());

    match operation.as_str() {
        "replyComment" => reply_comment(&client, &token, config).await,
        "comment" => comment(&client, &token, config).await,
        "sendMessage" => send_message(&client, &token, config).await,
        "createPost" => create_post(&client, &token, config).await,
        "likeObject" => like_object(&client, &token, config).await,
        "hideComment" => hide_comment(&client, &token, config).await,
        "deleteComment" => delete_comment(&client, &token, config).await,
        "getComments" => get_comments(&client, &token, config).await,
        "getThread" => get_thread(&client, &token, config).await,
        other => Err(format!("Unsupported Facebook operation '{other}'")),
    }
}
