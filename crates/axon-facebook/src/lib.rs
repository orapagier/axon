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
            Tool { name: "facebook_auth_url".into(),           description: "Get the Facebook OAuth URL. Open it in a browser to grant Page access.".into(),       input_schema: schema!({}, []) },
            Tool { name: "facebook_instagram_auth_url".into(), description: "Get the Instagram OAuth URL. Open it in a browser to grant Instagram access.".into(),       input_schema: schema!({}, []) },
            Tool { name: "facebook_exchange_code".into(),      description: "Exchange Facebook/Instagram OAuth code for a long-lived token.".into(),                    input_schema: schema!({"code":{"type":"string"},"service":{"type":"string"}}, ["code"]) },
            Tool { name: "facebook_connect_url".into(),        description: "Get the OAuth URL for connecting a Page as a reusable credential (multi-account).".into(),    input_schema: schema!({}, []) },
            Tool { name: "facebook_exchange_code_pages".into(),description: "Exchange an OAuth code for the list of Pages the user manages, each with its own Page token.".into(), input_schema: schema!({"code":{"type":"string"}}, ["code"]) },
            Tool { name: "facebook_auth_status".into(),        description: "Check Facebook and Instagram authentication status.".into(),                                        input_schema: schema!({}, []) },
            Tool { name: "facebook_revoke".into(),        description: "Delete stored Facebook and Instagram tokens.".into(),                                input_schema: schema!({}, []) },
            Tool { name: "facebook_debug_token".into(),   description: "Inspect the current token (expiry, scopes, app_id).".into(),                 input_schema: schema!({}, []) },

            // Page
            Tool { name: "fb_get_page".into(),    description: "Get Facebook Page info: name, about, fans, website, contact, hours.".into(),                  input_schema: schema!({}, []) },
            Tool { name: "fb_update_page".into(), description: "Update Facebook Page fields (about, description, phone, website).".into(),                    input_schema: schema!({"about":{"type":"string"},"description":{"type":"string"},"phone":{"type":"string"},"website":{"type":"string"}}, []) },

            // Posts
            Tool { name: "fb_list_posts".into(),                  description: "List published posts on the Facebook Page.".into(),                          input_schema: schema!({"limit":{"type":"integer","default":10},"after":{"type":"string","description":"Pagination cursor"}}, []) },
            Tool { name: "fb_get_post".into(),                    description: "Get a single Facebook post with engagement stats.".into(),                   input_schema: schema!({"post_id":{"type":"string"}}, ["post_id"]) },
            Tool { name: "fb_create_post".into(),                 description: "Create a text post on the Facebook Page.".into(),                            input_schema: schema!({"message":{"type":"string"},"link":{"type":"string"}}, ["message"]) },
            Tool { name: "fb_create_post_with_image".into(),      description: "Create a Facebook post with an image from an absolute local file path or public HTTP URL.".into(),      input_schema: schema!({"message":{"type":"string"},"image_url_or_path":{"type":"string"}}, ["message","image_url_or_path"]) },
            Tool { name: "fb_create_post_with_video".into(),      description: "Create a Facebook post with a video from an absolute local file path or public HTTP URL.".into(),       input_schema: schema!({"message":{"type":"string"},"video_url_or_path":{"type":"string"}}, ["message","video_url_or_path"]) },
            Tool { name: "fb_update_post".into(),                 description: "Edit the message of an existing Facebook post.".into(),                      input_schema: schema!({"post_id":{"type":"string"},"message":{"type":"string"}}, ["post_id","message"]) },
            Tool { name: "fb_delete_post".into(),                 description: "Delete a Facebook post.".into(),                                             input_schema: schema!({"post_id":{"type":"string"}}, ["post_id"]) },
            Tool { name: "fb_get_scheduled_posts".into(),         description: "List scheduled (unpublished) posts on the Facebook Page.".into(),            input_schema: schema!({"limit":{"type":"integer","default":10}}, []) },
            Tool { name: "fb_schedule_post".into(),               description: "Create a scheduled post. publish_time is a Unix timestamp.".into(),          input_schema: schema!({"message":{"type":"string"},"publish_time":{"type":"integer","description":"Unix timestamp (future)"}}, ["message","publish_time"]) },

            // Comments
            Tool { name: "fb_list_comments".into(),   description: "List comments on a post, photo, or video.".into(),                                       input_schema: schema!({"object_id":{"type":"string"},"limit":{"type":"integer","default":10}}, ["object_id"]) },
            Tool { name: "fb_recent_comments".into(), description: "Get recent comments across your latest posts. No post ID needed — checks your most recent posts automatically.".into(), input_schema: schema!({"post_count":{"type":"integer","default":5,"description":"Number of recent posts to check"},"comment_limit":{"type":"integer","default":10,"description":"Max comments per post"}}, []) },
            Tool { name: "fb_reply_to_comment".into(),description: "Reply to a Facebook comment.".into(),                                                    input_schema: schema!({"comment_id":{"type":"string"},"message":{"type":"string"}}, ["comment_id","message"]) },
            Tool { name: "fb_get_comment".into(),     description: "Get a Facebook comment by ID.".into(),                                                   input_schema: schema!({"comment_id":{"type":"string"}}, ["comment_id"]) },
            Tool { name: "fb_delete_comment".into(),  description: "Delete a Facebook comment.".into(),                                                      input_schema: schema!({"comment_id":{"type":"string"}}, ["comment_id"]) },
            Tool { name: "fb_hide_comment".into(),    description: "Hide or unhide a Facebook comment.".into(),                                              input_schema: schema!({"comment_id":{"type":"string"},"hide":{"type":"boolean","default":true}}, ["comment_id"]) },
            Tool { name: "fb_like_object".into(),     description: "Like a Facebook post, comment, or photo.".into(),                                        input_schema: schema!({"object_id":{"type":"string"}}, ["object_id"]) },
            Tool { name: "fb_react_object".into(),     description: "React to a Facebook object.".into(),                                                     input_schema: schema!({"object_id":{"type":"string"},"reaction_type":{"type":"string","description":"Reaction type: like, love, haha, wow, sad, angry"}}, ["object_id"]) },
            Tool { name: "fb_unreact_object".into(),  description: "Remove reaction from a Facebook post, comment, or photo.".into(),    input_schema: schema!({"object_id":{"type":"string"},"reaction_type":{"type":"string","description":"Reaction type to remove: like, love, haha, wow, sad, angry"}}, ["object_id"]) },

            // Insights
            Tool { name: "fb_page_insights".into(), description: "Get Page analytics: views, engagement, follows. period: day|week|days_28.".into(), input_schema: schema!({"metric":{"type":"string","default":"page_views,page_post_engagements,page_daily_follows"},"period":{"type":"string","default":"day"},"since":{"type":"string","description":"YYYY-MM-DD"},"until":{"type":"string","description":"YYYY-MM-DD"}}, []) },
            Tool { name: "fb_post_insights".into(), description: "Get analytics for a specific Facebook post.".into(),                                       input_schema: schema!({"post_id":{"type":"string"}}, ["post_id"]) },

            // Messaging
            Tool { name: "fb_list_messenger_chats".into(),       description: "List Facebook Messenger inbox chats for the Page.".into(),                             input_schema: schema!({"limit":{"type":"integer","default":10},"unread_only":{"type":"boolean","default":true,"description":"Only return chats that have unread messages"}}, []) },
            Tool { name: "fb_get_messenger_chat".into(),         description: "Get the message history inside a specific Messenger chat.".into(),                          input_schema: schema!({"conversation_id":{"type":"string"},"limit":{"type":"integer","default":10}}, ["conversation_id"]) },
            Tool { name: "fb_send_message".into(),             description: "Send a Messenger text message to a user by their PSID.".into(),                 input_schema: schema!({"recipient_id":{"type":"string","description":"User PSID"},"message":{"type":"string"}}, ["recipient_id","message"]) },
            Tool { name: "fb_send_message_image".into(),       description: "Send an image via Messenger. MUST be a public HTTP URL, local files are NOT supported here.".into(),                                           input_schema: schema!({"recipient_id":{"type":"string"},"image_url":{"type":"string"}}, ["recipient_id","image_url"]) },
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
