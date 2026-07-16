pub mod auth;
pub mod comments;
pub mod insights;
pub mod messaging;
pub mod page;
pub mod posts;

use anyhow::Result;
use axon_core::{err_json, ok_json, schema, AppState};
use rmcp::model::{CallToolResult, Tool};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct FacebookService(pub Arc<AppState>);

impl FacebookService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self(state)
    }

    pub fn tool_list() -> Vec<Tool> {
        vec![
            // Auth
            Tool::new("facebook_auth_url", "Get the Facebook OAuth URL. Open it in a browser to grant Page access.", schema!({}, [])),
            Tool::new("facebook_instagram_auth_url", "Get the Instagram OAuth URL. Open it in a browser to grant Instagram access.", schema!({}, [])),
            Tool::new("facebook_exchange_code", "Exchange Facebook/Instagram OAuth code for a long-lived token.", schema!({"code":{"type":"string"},"service":{"type":"string"}}, ["code"])),
            Tool::new("facebook_connect_url", "Get the OAuth URL for connecting a Page as a reusable credential (multi-account).", schema!({}, [])),
            Tool::new("facebook_exchange_code_pages", "Exchange an OAuth code for the list of Pages the user manages, each with its own Page token.", schema!({"code":{"type":"string"}}, ["code"])),
            Tool::new("facebook_auth_status", "Check Facebook and Instagram authentication status.", schema!({}, [])),
            Tool::new("facebook_revoke", "Delete stored Facebook and Instagram tokens.", schema!({}, [])),
            Tool::new("facebook_debug_token", "Inspect the current token (expiry, scopes, app_id).", schema!({}, [])),
            Tool::new("facebook_get_app_credentials", "Get the configured Facebook App credentials (app_id, page_id, verify_token; app_secret is only reported as set/unset, never returned).", schema!({}, [])),
            Tool::new("facebook_set_app_credentials", "Update the Facebook App credentials (app_id, app_secret, verify_token, page_id) used for OAuth and webhook verification.", schema!({"app_id":{"type":"string"},"app_secret":{"type":"string","description":"Leave blank to keep the existing secret"},"verify_token":{"type":"string"},"page_id":{"type":"string"}}, ["app_id","verify_token","page_id"])),

            // Page
            Tool::new("fb_get_page", "Get Facebook Page info: name, about, fans, website, contact, hours.", schema!({}, [])),
            Tool::new("fb_update_page", "Update Facebook Page fields (about, description, phone, website).", schema!({"about":{"type":"string"},"description":{"type":"string"},"phone":{"type":"string"},"website":{"type":"string"}}, [])),

            // Posts
            Tool::new("fb_list_posts", "List published posts on the Facebook Page. Use this when asked what is posted on the Page.", schema!({"limit":{"type":"integer","default":10},"after":{"type":"string","description":"Pagination cursor"}}, [])),
            Tool::new("fb_get_post", "Get a single Facebook post with engagement stats.", schema!({"post_id":{"type":"string"}}, ["post_id"])),
            Tool::new("fb_create_post", "Create a text post on the Facebook Page.", schema!({"message":{"type":"string"},"link":{"type":"string"}}, ["message"])),
            Tool::new("fb_create_post_with_image", "Create a Facebook post with an image from an absolute local file path or public HTTP URL.", schema!({"message":{"type":"string"},"image_url_or_path":{"type":"string"}}, ["message","image_url_or_path"])),
            Tool::new("fb_create_post_with_video", "Create a Facebook post with a video from an absolute local file path or public HTTP URL.", schema!({"message":{"type":"string"},"video_url_or_path":{"type":"string"}}, ["message","video_url_or_path"])),
            Tool::new("fb_update_post", "Edit the message of an existing Facebook post.", schema!({"post_id":{"type":"string"},"message":{"type":"string"}}, ["post_id","message"])),
            Tool::new("fb_delete_post", "Delete a Facebook post.", schema!({"post_id":{"type":"string"}}, ["post_id"])),
            Tool::new("fb_get_scheduled_posts", "List scheduled (unpublished) posts on the Facebook Page.", schema!({"limit":{"type":"integer","default":10}}, [])),
            Tool::new("fb_schedule_post", "Create a scheduled post. publish_time is a Unix timestamp.", schema!({"message":{"type":"string"},"publish_time":{"type":"integer","description":"Unix timestamp (future)"}}, ["message","publish_time"])),

            // Comments
            Tool::new("fb_list_comments", "List comments on a post, photo, or video.", schema!({"object_id":{"type":"string"},"limit":{"type":"integer","default":10}}, ["object_id"])),
            Tool::new("fb_recent_comments", "Get recent comments across your latest posts. No post ID needed — checks your most recent posts automatically.", schema!({"post_count":{"type":"integer","default":5,"description":"Number of recent posts to check"},"comment_limit":{"type":"integer","default":10,"description":"Max comments per post"}}, [])),
            Tool::new("fb_reply_to_comment", "Reply to a Facebook comment.", schema!({"comment_id":{"type":"string"},"message":{"type":"string"}}, ["comment_id","message"])),
            Tool::new("fb_get_comment", "Get a Facebook comment by ID.", schema!({"comment_id":{"type":"string"}}, ["comment_id"])),
            Tool::new("fb_delete_comment", "Delete a Facebook comment.", schema!({"comment_id":{"type":"string"}}, ["comment_id"])),
            Tool::new("fb_hide_comment", "Hide or unhide a Facebook comment.", schema!({"comment_id":{"type":"string"},"hide":{"type":"boolean","default":true}}, ["comment_id"])),
            Tool::new("fb_like_object", "Like a Facebook post, comment, or photo.", schema!({"object_id":{"type":"string"}}, ["object_id"])),
            Tool::new("fb_react_object", "React to a Facebook object.", schema!({"object_id":{"type":"string"},"reaction_type":{"type":"string","description":"Reaction type: like, love, haha, wow, sad, angry"}}, ["object_id"])),
            Tool::new("fb_unreact_object", "Remove reaction from a Facebook post, comment, or photo.", schema!({"object_id":{"type":"string"},"reaction_type":{"type":"string","description":"Reaction type to remove: like, love, haha, wow, sad, angry"}}, ["object_id"])),

            // Insights
            Tool::new("fb_page_insights", "Get Page analytics: views, engagement, follows. period: day|week|days_28.", schema!({"metric":{"type":"string","default":"page_views,page_post_engagements,page_daily_follows"},"period":{"type":"string","default":"day"},"since":{"type":"string","description":"YYYY-MM-DD"},"until":{"type":"string","description":"YYYY-MM-DD"}}, [])),
            Tool::new("fb_post_insights", "Get analytics for a specific Facebook post.", schema!({"post_id":{"type":"string"}}, ["post_id"])),

            // Messaging
            Tool::new("fb_list_messenger_chats", "List Facebook Messenger inbox chats for the Page. Use this when asked to check Messenger or the Page inbox.", schema!({"limit":{"type":"integer","default":10},"unread_only":{"type":"boolean","default":true,"description":"Only return chats that have unread messages"}}, [])),
            Tool::new("fb_get_messenger_chat", "Get the message history inside a specific Messenger chat.", schema!({"conversation_id":{"type":"string"},"limit":{"type":"integer","default":10}}, ["conversation_id"])),
            Tool::new("fb_send_message", "Send a Messenger text message to a user by their PSID.", schema!({"recipient_id":{"type":"string","description":"User PSID"},"message":{"type":"string"}}, ["recipient_id","message"])),
            Tool::new("fb_send_message_image", "Send an image via Messenger. MUST be a public HTTP URL, local files are NOT supported here.", schema!({"recipient_id":{"type":"string"},"image_url":{"type":"string"}}, ["recipient_id","image_url"])),
        ]
    }

    pub async fn call(&self, name: &str, args: Map<String, Value>) -> Result<CallToolResult> {
        let a = &args;
        let s = str!(a);
        let n = num!(a);
        let b = boo!(a);

        let result: Result<Value> = match name {
            "facebook_auth_url" => auth::auth_url(&self.0).await,
            "facebook_instagram_auth_url" => auth::instagram_auth_url(&self.0).await,
            "facebook_exchange_code" => {
                auth::exchange_code(
                    &self.0,
                    s("code")?,
                    a.get("service").and_then(|v| v.as_str()),
                )
                .await
            }
            "facebook_connect_url" => auth::connect_url(&self.0).await,
            "facebook_exchange_code_pages" => auth::exchange_code_pages(&self.0, s("code")?).await,
            "facebook_auth_status" => auth::auth_status(&self.0).await,
            "facebook_revoke" => auth::revoke(&self.0).await,
            "facebook_debug_token" => auth::debug_token(&self.0).await,
            "facebook_get_app_credentials" => auth::get_app_credentials(&self.0).await,
            "facebook_set_app_credentials" => {
                auth::set_app_credentials(
                    &self.0,
                    s("app_id")?,
                    a.get("app_secret").and_then(|v| v.as_str()),
                    s("verify_token")?,
                    s("page_id")?,
                )
                .await
            }

            "fb_get_page" => page::get_page(&self.0).await,
            "fb_update_page" => page::update_page(&self.0, a).await,

            "fb_list_posts" => {
                posts::list(
                    &self.0,
                    n("limit", 10.0).min(10.0) as u32,
                    a.get("after").and_then(|v| v.as_str()),
                )
                .await
            }
            "fb_get_post" => posts::get(&self.0, s("post_id")?).await,
            "fb_create_post" => {
                posts::create(
                    &self.0,
                    s("message")?,
                    a.get("link").and_then(|v| v.as_str()),
                    None,
                )
                .await
            }
            "fb_create_post_with_image" => {
                posts::create_with_image(&self.0, s("message")?, s("image_url_or_path")?).await
            }
            "fb_create_post_with_video" => {
                posts::create_with_video(&self.0, s("message")?, s("video_url_or_path")?).await
            }
            "fb_update_post" => posts::update(&self.0, s("post_id")?, s("message")?).await,
            "fb_delete_post" => posts::delete(&self.0, s("post_id")?).await,
            "fb_get_scheduled_posts" => {
                posts::get_scheduled(&self.0, n("limit", 10.0).min(10.0) as u32).await
            }
            "fb_schedule_post" => {
                posts::create(
                    &self.0,
                    s("message")?,
                    None,
                    Some(n("publish_time", 0.0) as u64),
                )
                .await
            }

            "fb_list_comments" => {
                comments::list(&self.0, s("object_id")?, n("limit", 10.0).min(10.0) as u32).await
            }
            "fb_recent_comments" => {
                comments::recent_comments(
                    &self.0,
                    n("post_count", 5.0) as u32,
                    n("comment_limit", 10.0).min(10.0) as u32,
                )
                .await
            }
            "fb_reply_to_comment" => {
                comments::reply(&self.0, s("comment_id")?, s("message")?).await
            }
            "fb_delete_comment" => comments::delete(&self.0, s("comment_id")?).await,
            "fb_get_comment" => comments::get(&self.0, s("comment_id")?).await,
            "fb_hide_comment" => {
                comments::set_hidden(&self.0, s("comment_id")?, b("hide", true)).await
            }
            "fb_react_object" => {
                let r_type = a.get("reaction_type").and_then(|v| v.as_str());
                if let Some(r) = r_type {
                    if r.eq_ignore_ascii_case("ANGRY") {
                        return Err(anyhow::anyhow!(
                            "ANGRY reaction is blocked by safety guardrail"
                        ));
                    }
                }
                comments::react(&self.0, s("object_id")?, r_type).await
            }
            "fb_like_object" => comments::like(&self.0, s("object_id")?).await,
            "fb_unreact_object" => {
                comments::unreact(
                    &self.0,
                    s("object_id")?,
                    a.get("reaction_type").and_then(|v| v.as_str()),
                )
                .await
            }

            "fb_page_insights" => {
                insights::page_insights(
                    &self.0,
                    a.get("metric")
                        .and_then(|v| v.as_str())
                        .unwrap_or("page_views,page_post_engagements,page_daily_follows"),
                    a.get("period").and_then(|v| v.as_str()).unwrap_or("day"),
                    a.get("since").and_then(|v| v.as_str()),
                    a.get("until").and_then(|v| v.as_str()),
                )
                .await
            }
            "fb_get_post_insights" | "fb_post_insights" => {
                insights::post_insights(&self.0, s("post_id")?).await
            }

            "fb_list_messenger_chats" => {
                messaging::list_conversations(
                    &self.0,
                    n("limit", 10.0).min(10.0) as u32,
                    a.get("unread_only")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                )
                .await
            }
            "fb_get_messenger_chat" => {
                messaging::get_conversation(
                    &self.0,
                    s("conversation_id")?,
                    n("limit", 10.0).min(10.0) as u32,
                )
                .await
            }
            "fb_send_message" => {
                messaging::send_text(&self.0, s("recipient_id")?, s("message")?).await
            }
            "fb_send_message_image" => {
                messaging::send_image(&self.0, s("recipient_id")?, s("image_url")?).await
            }

            other => Err(anyhow::anyhow!("Unknown Facebook tool: {other}")),
        };

        Ok(match result {
            Ok(v) => ok_json(v),
            Err(e) => err_json(e),
        })
    }
}

macro_rules! str {
    ($args:expr) => {
        |key: &str| -> Result<&str> {
            $args
                .get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))
        }
    };
}
macro_rules! num {
    ($args:expr) => {
        |key: &str, default: f64| -> f64 {
            $args.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
        }
    };
}
macro_rules! boo {
    ($args:expr) => {
        |key: &str, default: bool| -> bool {
            $args.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
        }
    };
}
use boo;
use num;
use str;
