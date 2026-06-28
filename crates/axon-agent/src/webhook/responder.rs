// The built-in Facebook auto-reply pipeline is disabled — replies are now driven
// by Stimulus→Facebook-node workflows (see `webhook::facebook::fb_event`). The
// pipeline is kept here, unwired, so it can be re-enabled later if needed.
#![allow(dead_code)]

use crate::config::RuntimeSettings;
use crate::memory::MemoryStore;
use crate::messaging::{MessageGateway, MessagingHub};
use crate::providers::types::Message;
use crate::router::{call_llm, SharedRouter};
use crate::tools::ToolRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Debug logger — writes to plain-text file readable over SSH
fn dbg_log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/autoreply_debug.log")
    {
        let now_manila =
            chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
        let _ = writeln!(f, "[{}] {}", now_manila.format("%H:%M:%S"), msg);
    }
}

/// Structured event data extracted from raw webhook payload (code-based, no LLM).
#[derive(Debug, Clone)]
pub struct FacebookEvent {
    pub event_type: String, // "comment" or "message"
    pub from_name: String,
    pub from_id: String, // PSID for messages, user ID for comments
    pub message: String,
    pub object_id: String, // comment_id or conversation_id
    pub parent_id: String, // post_id for comments
    pub post_id: String,   // actual root post ID
    pub permalink: String,
    pub timestamp: String,
}

/// Send a "LIKE" reaction to a Facebook object (comment or post)
pub async fn handle_fb_like(object_id: String, tools: ToolRegistry) {
    dbg_log(&format!(
        "SENDING REACTION (INDEPENDENT): LIKE for {}",
        object_id
    ));
    match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tools.run(
            "fb_like_object",
            serde_json::json!({
                "object_id": object_id
            }),
        ),
    )
    .await
    {
        Ok(Ok(res)) => {
            let s = serde_json::to_string(&res).unwrap_or_default();
            tracing::info!("FB like success: {}", s);
        }
        Ok(Err(e)) => {
            tracing::error!("FB like error: {}", e);
        }
        Err(_) => {
            tracing::warn!("FB like timed out");
        }
    }
}

/// Send a polite fallback reply when the LLM fails, is rate-limited, or times out.
pub async fn handle_fallback_reply(
    event: FacebookEvent,
    tools: ToolRegistry,
    settings: Arc<crate::config::RuntimeSettings>,
    messaging: Arc<crate::messaging::MessagingHub>,
    memory: Arc<crate::memory::MemoryStore>,
) {
    if event.event_type != "comment" && event.event_type != "message" {
        return;
    }

    dbg_log(&format!(
        "TRIGGERING FALLBACK for '{}' ({})",
        event.from_name, event.event_type
    ));

    let name_parts: Vec<&str> = event.from_name.split_whitespace().collect();
    let first_name = name_parts.first().unwrap_or(&"there");

    let fallback_msg = format!(
        "Hi {}, thank you for your engagement with our ministry page! 🙏 We're so glad you're part of our community. We pray that all is well with you and your loved ones. God bless and keep you! ✨",
        first_name
    );

    let send_result = match event.event_type.as_str() {
        "comment" => {
            tools
                .run(
                    "fb_reply_to_comment",
                    serde_json::json!({
                        "comment_id": event.object_id,
                        "message": fallback_msg
                    }),
                )
                .await
        }
        "message" => {
            tools
                .run(
                    "fb_send_message",
                    serde_json::json!({
                        "recipient_id": event.from_id,
                        "message": fallback_msg
                    }),
                )
                .await
        }
        _ => return,
    };

    match send_result {
        Ok(ref val)
            if val.get("error").and_then(|e| e.as_object()).is_some()
                || val.get("error").and_then(|e| e.as_bool()).unwrap_or(false) =>
        {
            let (msg, code) = if let Some(err_obj) = val.get("error").and_then(|e| e.as_object()) {
                let m = err_obj
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                let c = err_obj.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                (m, c)
            } else {
                let m = val
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                (m, 0)
            };

            dbg_log(&format!("FALLBACK API ERROR {}: {}", code, msg));
            tracing::error!("FB fallback reply failed (API error): {}", msg);

            // If the fallback also fails, we notify the owner that we are likely totally blocked.
            if settings.get_bool("webhook.fb_notify_replies", true) {
                let greeting = if event.object_id.chars().count() % 2 == 0 {
                    "Jelmar"
                } else {
                    "Pastor"
                };
                let msg_note = format!(
                    "Hey {}, it looks like we are completely blocked from commenting on Facebook right now. I even tried a generic fallback reply for {}, but that failed too. I did successfully 'Like' the comment, though! 👍\n\n(Error: {})",
                    greeting, event.from_name, msg
                );
                notify_owner(
                    &settings,
                    &messaging,
                    &memory,
                    &msg_note,
                    Some(&event.object_id),
                )
                .await;
            }
            return;
        }
        Ok(res) => {
            let s = serde_json::to_string(&res).unwrap_or_default();
            dbg_log(&format!("FALLBACK SUCCESS: {}", s));
            tracing::info!("FB fallback reply sent successfully: {}", s);
        }
        Err(e) => {
            dbg_log(&format!("FALLBACK TRANSPORT ERROR: {}", e));
            tracing::error!("FB fallback reply failed: {}", e);

            // If the fallback also fails, we notify the owner that we are likely totally blocked.
            if settings.get_bool("webhook.fb_notify_replies", true) {
                let greeting = if event.object_id.chars().count() % 2 == 0 {
                    "Jelmar"
                } else {
                    "Pastor"
                };
                let msg = format!(
                    "Hey {}, it looks like we are completely blocked from commenting on Facebook right now. I even tried a generic fallback reply for {}, but that failed too. I did successfully 'Like' the comment, though! 👍",
                    greeting, event.from_name
                );
                notify_owner(&settings, &messaging, &memory, &msg, Some(&event.object_id)).await;
            }
            return;
        }
    }

    // 1. Log interaction to memory
    let user_tag = format!("fb_user:{}", event.from_id);
    let memory_content = match event.event_type.as_str() {
        "comment" => format!(
            "[{}] Facebook comment from {} (ID:{}): \"{}\" → Fallback Replied: \"{}\"",
            event.timestamp,
            event.from_name,
            event.from_id,
            event.message.chars().take(200).collect::<String>(),
            fallback_msg.chars().take(200).collect::<String>()
        ),
        "message" => format!(
            "[{}] Facebook message from {} (PSID:{}): \"{}\" → Fallback Replied: \"{}\"",
            event.timestamp,
            event.from_name,
            event.from_id,
            event.message.chars().take(200).collect::<String>(),
            fallback_msg.chars().take(200).collect::<String>()
        ),
        _ => String::new(),
    };
    if !memory_content.is_empty() {
        let _ = memory
            .remember(
                &memory_content,
                "facebook_webhook",
                &[&user_tag, "facebook", &event.event_type],
            )
            .await;
    }

    // 2. Sync to short-term memory (for Agent context)
    let chat_id = settings.get_str("watcher.notify_chat_id", "");
    if !chat_id.is_empty() {
        let incoming_text = match event.event_type.as_str() {
            "message" => format!(
                "[Facebook message from {}] (PSID: {}): {}",
                event.from_name, event.from_id, event.message
            ),
            _ => format!(
                "[Facebook {} from {}] (ID: {}, Post ID: {}): {}",
                event.event_type, event.from_name, event.object_id, event.parent_id, event.message
            ),
        };
        let _ = memory
            .short
            .store_message(&chat_id, "user", &incoming_text, None);
        let _ = memory
            .short
            .store_message(&chat_id, "assistant", &fallback_msg, None);
    }

    // 3. Notify owner
    if settings.get_bool("webhook.fb_notify_replies", true) {
        let greeting = if event.object_id.chars().count() % 2 == 0 {
            "Hi Pastor"
        } else {
            "Hello Jelmar"
        };
        let notification = format!(
            "{}, Facebook seems to be rate-limited or the AI process timed out, so I just sent a warm fallback reply to {} to make sure they feel acknowledged. 🙏✨",
            greeting, event.from_name
        );
        notify_owner(
            &settings,
            &messaging,
            &memory,
            &notification,
            if event.object_id.is_empty() {
                None
            } else {
                Some(&event.object_id)
            },
        )
        .await;
    }
}

/// Run the auto-reply pipeline: fetch context → LLM → reply → notify owner
pub async fn handle_auto_reply(
    event: FacebookEvent,
    tools: ToolRegistry,
    router: SharedRouter,
    settings: Arc<RuntimeSettings>,
    messaging: Arc<MessagingHub>,
    memory: Arc<MemoryStore>,
) {
    // Check master switch
    if !settings.get_bool("webhook.fb_auto_reply", true) {
        tracing::debug!("FB auto-reply disabled via settings");
        dbg_log("DISABLED: webhook.fb_auto_reply is false");
        return;
    }

    // Skip events from page itself (don't reply to own actions)
    let page_id = crate::webhook::facebook::load_fb_creds().page_id.clone();
    if !page_id.is_empty() && event.from_id == page_id {
        tracing::debug!("FB auto-reply: skipping own page action");
        return;
    }

    dbg_log(&format!(
        "PIPELINE START type={} from='{}' msg='{}'",
        event.event_type,
        event.from_name,
        event.message.chars().take(60).collect::<String>()
    ));

    tracing::info!(
        "FB auto-reply: processing {} from '{}': {}",
        event.event_type,
        event.from_name,
        event.message.chars().take(80).collect::<String>()
    );

    // 1. Fetch context
    let context = match event.event_type.as_str() {
        "comment" => fetch_comment_context(&tools, &event).await,
        "message" => fetch_message_context(&tools, &event).await,
        _ => {
            tracing::debug!("FB auto-reply: skipping event type '{}'", event.event_type);
            return;
        }
    };
    dbg_log(&format!("CONTEXT FETCHED type={}", event.event_type));

    // 2. Memory interaction is now tool-driven (handled in the LLM loop below)
    let user_tag = format!("fb_user:{}", event.from_id);
    dbg_log("MEMORY SEARCH BYPASS (NOW TOOL-DRIVEN)");

    // 3. Get the system prompt
    let mut system_prompt = settings.get_str("webhook.fb_reply_prompt", DEFAULT_FB_REPLY_PROMPT);
    let now_manila =
        chrono::Utc::now().with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
    system_prompt.push_str(&format!(
        "\n\n[CURRENT TIME: {} (Asia/Manila)]",
        now_manila.format("%A, %Y-%m-%d %H:%M:%S")
    ));

    // 4. Build message for LLM
    let user_msg = match event.event_type.as_str() {
        "comment" => format!(
            "You received a Facebook comment from {name}.\n\n\
             [POST THEY COMMENTED ON]\n\
             {post}\n\n\
             {context}\n\
             ---------------------\n\
             CURRENT COMMENT TO REPLY TO:\n\
             \"{comment}\"\n\
             ---------------------\n\n\
             Instruction: Write your direct reply to {name} now. Be warmly pastoral, natural, and human. \
             Use the current time to add a subtle pastoral touch (e.g., \"blessed morning\", \"peaceful evening\") if it fits the flow. \
             If you need past context about this user, use the agent_memory_tool with tag '{user_tag}'.",
            post = context
                .get("post_text")
                .and_then(|v| v.as_str())
                .unwrap_or("(unavailable)"),
            user_tag = user_tag,
            name = event.from_name,
            comment = event.message,
            context = format_context(&context),
        ),
            "message" => format!(
            "You received a Facebook message from {name}.\n\n\
             {context}\n\
             ---------------------\n\
             CURRENT MESSAGE TO REPLY TO:\n\
             \"{msg}\"\n\
             ---------------------\n\n\
             Instruction: Write your direct reply to {name} now. Play along naturally. \
             Be warmly pastoral and human. Use the current time to add a subtle pastoral touch (e.g., \"blessed afternoon\", \"blessed day\") if it feels right. \
             If you need past context about this user, use the agent_memory_tool with tag '{user_tag}'. \
             If they sent a short casual greeting, reply with a warm pastoral greeting using their name {name}. \
             DO NOT over-explain, do NOT analyze the chat history, and do NOT make robotic transition statements.",
            name = context
                .get("sender_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&event.from_name),
            msg = event.message,
            context = format_context(&context),
            user_tag = user_tag,
        ),
        _ => return,
    };

    // 5. Call LLM (Loop to support tools like memory search)
    let mut messages = vec![Message::user(&user_msg)];
    let mut tool_defs = tools.all_enabled().await;

    // Filter tools to only include those relevant for auto-reply to avoid noise
    tool_defs.retain(|t| t.name == "agent_memory_tool" || t.name == "web_search");

    let mut final_reply = String::new();
    let mut iterations = 0;

    while iterations < 3 {
        iterations += 1;
        dbg_log(&format!(
            "LLM CALLING (Iter {}) role=watcher prompt_len={}",
            iterations,
            user_msg.len()
        ));

        match call_llm(
            &messages,
            &system_prompt,
            &tool_defs,
            None,
            "watcher",
            Arc::clone(&router),
            &settings,
            None,
        )
        .await
        {
            Ok((response, model, _tier)) => {
                let text = response.text_content();
                if !response.has_tool_calls() {
                    dbg_log(&format!("LLM OK model={} reply_len={}", model, text.len()));
                    final_reply = text.to_string();
                    break;
                }

                // Append assistant message with its content (including any tool calls)
                messages.push(Message::assistant_with_blocks(response.content.clone()));

                // Handle tool calls
                for tcall in response.tool_calls() {
                    dbg_log(&format!("TOOL CALL: {}", tcall.name));
                    let result = if tcall.name == "agent_memory_tool" {
                        // Handle internal memory tool manually here
                        let action = tcall
                            .input
                            .get("action")
                            .and_then(|a| a.as_str())
                            .unwrap_or("");
                        match action {
                            "search" => {
                                let query = tcall
                                    .input
                                    .get("content")
                                    .and_then(|c| c.as_str())
                                    .unwrap_or("");
                                let results =
                                    memory.search(query, 5, None).await.unwrap_or_default();
                                let summaries: Vec<String> =
                                    results.iter().map(|e| e.content.clone()).collect();
                                serde_json::json!({ "results": summaries })
                            }
                            _ => {
                                serde_json::json!({ "error": "Only 'search' action is currently supported in auto-reply context" })
                            }
                        }
                    } else {
                        tools
                            .run(&tcall.name, tcall.input.clone())
                            .await
                            .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }))
                    };

                    messages.push(Message::tool_result(tcall.id, result));
                }
            }
            Err(e) => {
                dbg_log(&format!("LLM FAILED: {}", e));
                tracing::error!("FB auto-reply LLM failed: {}", e);
                handle_fallback_reply(event, tools, settings, messaging, memory).await;
                return;
            }
        }
    }

    let reply_text = final_reply;

    if reply_text.is_empty() || reply_text.contains("SKIP") || reply_text.contains("DO_NOT_REPLY") {
        dbg_log(&format!(
            "LLM SKIPPED reply='{}'",
            reply_text.chars().take(30).collect::<String>()
        ));
        tracing::info!("FB auto-reply: LLM decided not to reply");
        return;
    }

    let final_reply = reply_text.clone();

    dbg_log(&format!(
        "SEND type={} object_id='{}' from='{}' reply_len={}",
        event.event_type,
        event.object_id,
        event.from_name,
        reply_text.len()
    ));

    // Typing delay removed — the reply queue in facebook.rs enforces a 15-30s
    // gap before each job (covering both the "human noticed it" wait and typing).
    // Keeping this delay here too was stacking them to 23-54s total.

    // 6. Send reply via MCP
    let send_result = match event.event_type.as_str() {
        "comment" => {
            tools
                .run(
                    "fb_reply_to_comment",
                    serde_json::json!({
                        "comment_id": event.object_id.clone(),
                        "message": final_reply.clone()
                    }),
                )
                .await
        }
        "message" => {
            tools
                .run(
                    "fb_send_message",
                    serde_json::json!({
                        "recipient_id": event.from_id.clone(),
                        "message": final_reply.clone()
                    }),
                )
                .await
        }
        _ => return,
    };

    match send_result {
        Ok(ref val)
            if val.get("error").and_then(|e| e.as_object()).is_some()
                || val.get("error").and_then(|e| e.as_bool()).unwrap_or(false) =>
        {
            let (msg, code) = if let Some(err_obj) = val.get("error").and_then(|e| e.as_object()) {
                let m = err_obj
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                let c = err_obj.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                (m, c)
            } else {
                let m = val
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                (m, 0)
            };

            let is_rate_limit = code == 368 || msg.contains("rate limit") || msg.contains("spam");
            dbg_log(&format!("API ERROR {}: {}", code, msg));
            tracing::error!(
                "FB auto-reply send failed (API error{}): {}",
                if is_rate_limit { " (rate limited)" } else { "" },
                msg
            );

            if settings.get_bool("webhook.fb_notify_replies", true) {
                notify_error_to_owner(
                    &event,
                    &msg,
                    is_rate_limit,
                    &settings,
                    &messaging,
                    &memory,
                    &router,
                )
                .await;
            }
            return;
        }
        Ok(ref val) => {
            dbg_log(&format!(
                "SUCCESS val={}",
                serde_json::to_string(val).unwrap_or_default()
            ));
            tracing::info!("FB auto-reply sent successfully to {}", event.from_name);
        }
        Err(ref e) => {
            dbg_log(&format!("TRANSPORT ERROR: {}", e));
            tracing::error!("FB auto-reply send failed: {}", e);

            if settings.get_bool("webhook.fb_notify_replies", true) {
                notify_error_to_owner(
                    &event,
                    &e.to_string(),
                    false, // transport error is typically not a rate limit
                    &settings,
                    &messaging,
                    &memory,
                    &router,
                )
                .await;
            }
            return;
        }
    }

    // 7. Log interaction to memory
    let memory_content = match event.event_type.as_str() {
        "comment" => format!(
            "[{}] Facebook comment from {} (ID:{}): \"{}\" → Liked & Replied: \"{}\"",
            event.timestamp,
            event.from_name,
            event.from_id,
            event.message.chars().take(200).collect::<String>(),
            final_reply.chars().take(200).collect::<String>()
        ),
        "message" => format!(
            "[{}] Facebook message from {} (PSID:{}): \"{}\" → Replied: \"{}\"",
            event.timestamp,
            event.from_name,
            event.from_id,
            event.message.chars().take(200).collect::<String>(),
            final_reply.chars().take(200).collect::<String>()
        ),
        _ => String::new(),
    };
    if !memory_content.is_empty() {
        match memory
            .remember(
                &memory_content,
                "facebook_webhook",
                &[&user_tag, "facebook", &event.event_type],
            )
            .await
        {
            Ok(id) => tracing::info!("FB auto-reply: stored interaction in memory (id={})", id),
            Err(e) => tracing::warn!("FB auto-reply: failed to store memory: {}", e),
        }
    }

    // 8. Notify owner
    if settings.get_bool("webhook.fb_notify_replies", true) {
        let prompt = format!(
            "You are an energetic, helpful human-like assistant named Axon managing a Facebook page for your boss. You just replied to a message or comment over Facebook.\n\n\
             EVENT TYPE: {}\n\
             SENDER NAME: {}\n\
             THEIR ORIGINAL MESSAGE: \"{}\"\n\
             YOUR RESPONSE: \"{}\"\n\n\
             Write a very quick, friendly, and natural 1-2 sentence message to notify your boss about what you just did.\n\
             \n\
             IMPORTANT GREETING RULES:\n\
             - Do NOT use 'Hi boss' or 'Hey boss' constantly.\n\
             - Address him naturally occasionally using one of: Jelmar, Pastor Jelmar, or Pastor.\n\
             - E.g., 'Hey Pastor, I just replied...' or 'Jelmar, a comment came in from...'\n\
             \n\
             IMPORTANT TONE RULES:\n\
             - Speak in the first person using personal pronouns like 'I', 'me', and 'my'.\n\
             - NEVER use words like 'auto-reply', 'automated response', 'programmed', or 'bot'.\n\
             - Sound like a real human secretary/assistant who personally handled the task (e.g., 'I just took care of a comment from...', 'I responded to...', 'I just jumped in to answer...').\n\
             - Add 1-2 relevant, colorful emojis natively in your text to make the message lively and conversational.",
            event.event_type,
            event.from_name,
            event.message.chars().take(200).collect::<String>(),
            reply_text.chars().take(300).collect::<String>()
        );

        let notification = match call_llm(
            &[Message::user(&prompt)],
            "You are Axon, a helpful, conversational, and energetic AI assistant.",
            &[],
            Some(150), // Increase max_tokens to prevent truncation
            "watcher",
            Arc::clone(&router),
            &settings,
            None,
        )
        .await
        {
            Ok((resp, _, _)) => {
                let txt = resp.text_content().to_string();
                if txt.trim().is_empty() {
                    tracing::warn!(
                        "FB auto-reply notification LLM returned empty text, using fallback"
                    );
                    format!(
                        "Hey! Just letting you know I swiftly replied to a {} from {}. They said: \"{}\" and I answered with: \"{}\"",
                        event.event_type,
                        event.from_name,
                        event.message.chars().take(100).collect::<String>(),
                        reply_text.chars().take(150).collect::<String>()
                    )
                } else {
                    txt
                }
            }
            Err(e) => {
                tracing::warn!("FB auto-reply notification LLM fallback: {}", e);
                // Fallback to a natural static template if the LLM fails
                format!(
                    "Hey! Just letting you know I swiftly replied to a {} from {}. They said: \"{}\" and I answered with: \"{}\"",
                    event.event_type,
                    event.from_name,
                    event.message.chars().take(100).collect::<String>(),
                    reply_text.chars().take(150).collect::<String>()
                )
            }
        };

        // ── SYNC TO SHORT-TERM MEMORY ──────────────────────────────────────
        // We store both the incoming message and our auto-reply in the
        // Telegram chat's history so the Agent knows the full context if
        // the user replies to the notification.
        let chat_id = settings.get_str("watcher.notify_chat_id", "");
        if !chat_id.is_empty() {
            let incoming_text = match event.event_type.as_str() {
                "message" => format!(
                    "[Facebook message from {}] (PSID: {}): {}",
                    event.from_name, event.from_id, event.message
                ),
                _ => format!(
                    "[Facebook {} from {}] (ID: {}, Post ID: {}): {}",
                    event.event_type,
                    event.from_name,
                    event.object_id,
                    event.parent_id,
                    event.message
                ),
            };
            if let Err(e) = memory
                .short
                .store_message(&chat_id, "user", &incoming_text, None)
            {
                tracing::warn!(
                    "Failed to store incoming FB message in short-term memory: {}",
                    e
                );
            }
            if let Err(e) = memory
                .short
                .store_message(&chat_id, "assistant", &reply_text, None)
            {
                tracing::warn!("Failed to store FB auto-reply in short-term memory: {}", e);
            }
        }

        notify_owner(
            &settings,
            &messaging,
            &memory,
            &notification,
            if event.object_id.is_empty() {
                None
            } else {
                Some(&event.object_id)
            },
        )
        .await;
    }
}

// ── Context Fetching ─────────────────────────────────────────────────────────

/// Score relevance of a comment against a reference string using keyword overlap.
/// Ignores common English and Filipino stop words to reduce noise.
fn keyword_overlap_score(candidate: &str, reference: &str) -> usize {
    const STOP_WORDS: &[&str] = &[
        // English
        "a", "an", "the", "and", "or", "but", "is", "are", "was", "were", "i", "you", "he", "she",
        "we", "they", "it", "to", "of", "in", "on", "at", "for", "with", "this", "that", "what",
        "how", "why", "be", "do", "did", "has", "have", "had", "will", "can", "not",
        // Filipino
        "ang", "ng", "sa", "na", "mga", "ay", "ko", "mo", "ka", "po", "nga", "ba", "naman", "lang",
        "din", "rin", "si", "ni", "kayo", "siya", "sila", "kami", "tayo", "ito", "yan", "yun",
    ];
    let tokenize = |text: &str| -> std::collections::HashSet<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty() && w.len() > 2 && !STOP_WORDS.contains(w))
            .map(|w| w.to_string())
            .collect()
    };
    let cand_words = tokenize(candidate);
    let ref_words = tokenize(reference);
    cand_words.intersection(&ref_words).count()
}

async fn fetch_comment_context(tools: &ToolRegistry, event: &FacebookEvent) -> Value {
    let mut ctx = serde_json::json!({});

    // Fetch the original post content using actual POST ID
    let root_post = if !event.post_id.is_empty() {
        &event.post_id
    } else {
        &event.parent_id
    };
    if !root_post.is_empty() {
        match tools
            .run("fb_get_post", serde_json::json!({"post_id": root_post}))
            .await
        {
            Ok(data) => {
                let post_text = data.get("message").and_then(|m| m.as_str()).unwrap_or("");
                ctx["post_text"] = serde_json::json!(post_text);
            }
            Err(_) => {
                ctx["post_text"] = serde_json::json!("(could not fetch post)");
            }
        }
    }

    // Determine thread context:
    // If parent_id != post_id and parent_id is not empty, it's a nested reply!
    let is_nested_reply = !event.parent_id.is_empty()
        && event.parent_id != event.post_id
        && !event.post_id.is_empty();

    if is_nested_reply {
        // Nested reply: fetch the parent comment so Axon knows what comment is being replied to
        match tools
            .run(
                "fb_get_comment",
                serde_json::json!({"comment_id": event.parent_id}),
            )
            .await
        {
            Ok(data) => {
                let name = data
                    .get("from")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("Someone");
                let msg = data.get("message").and_then(|m| m.as_str()).unwrap_or("");
                ctx["parent_comment"] = serde_json::json!(format!("{}: \"{}\"", name, msg));
            }
            Err(e) => {
                tracing::warn!("Failed to fetch parent comment: {}", e);
            }
        }

        // Fetch up to 10 sibling replies from the API (single call).
        // • Thread has ≤ 5 replies → pass the 3 most recent directly (fast path, no filtering).
        // • Thread has > 5 replies  → apply keyword overlap scoring to keep the 3 most
        //   relevant to the triggering comment + parent comment (saves tokens on busy threads).
        match tools
            .run(
                "fb_list_comments",
                serde_json::json!({"object_id": event.parent_id, "limit": 10}),
            )
            .await
        {
            Ok(data) => {
                let comments = data
                    .get("comments")
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();

                // Build reference text for keyword scoring:
                // triggering comment + parent comment text (if we fetched it)
                let parent_text = ctx
                    .get("parent_comment")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let reference = format!("{} {}", event.message, parent_text);

                let selected: Vec<String> = if comments.len() <= 5 {
                    // Small thread — take the 3 most recent directly
                    comments
                        .iter()
                        .take(3)
                        .filter_map(|c| {
                            let name = c
                                .get("commenter_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("Someone");
                            let msg = c.get("message").and_then(|m| m.as_str()).unwrap_or("");
                            if msg.is_empty() {
                                None
                            } else {
                                Some(format!("  ↳ {}: \"{}\"", name, msg))
                            }
                        })
                        .collect()
                } else {
                    // Busy thread — score by keyword overlap and keep top 3
                    tracing::debug!(
                        "FB auto-reply: thread has {} replies, applying keyword filter",
                        comments.len()
                    );
                    let mut scored: Vec<(usize, String)> = comments
                        .iter()
                        .filter_map(|c| {
                            let name = c
                                .get("commenter_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("Someone");
                            let msg = c.get("message").and_then(|m| m.as_str()).unwrap_or("");
                            if msg.is_empty() {
                                None
                            } else {
                                let score = keyword_overlap_score(msg, &reference);
                                Some((score, format!("  ↳ {}: \"{}\"", name, msg)))
                            }
                        })
                        .collect();
                    // Sort descending by relevance score
                    scored.sort_by(|a, b| b.0.cmp(&a.0));
                    scored.into_iter().take(3).map(|(_, s)| s).collect()
                };

                if !selected.is_empty() {
                    ctx["sibling_replies"] = serde_json::json!(selected);
                }
            }
            Err(e) => tracing::warn!("Failed to fetch sibling replies: {}", e),
        }
    }
    // For top-level comments, the post content alone is sufficient context.
    // Axon doesn't need to see other people's comments on the post.

    ctx
}

async fn fetch_message_context(tools: &ToolRegistry, event: &FacebookEvent) -> Value {
    let mut ctx = serde_json::json!({});

    // Get previous messages from this sender's conversation
    // We need to find the conversation first — use sender PSID
    match tools
        .run("fb_list_messenger_chats", serde_json::json!({"limit": 5}))
        .await
    {
        Ok(conversations) => {
            // Find the conversation with this sender
            let convs = conversations
                .get("data")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default();

            for conv in &convs {
                let participants = conv
                    .get("participants")
                    .and_then(|p| p.get("data"))
                    .and_then(|d| d.as_array())
                    .cloned()
                    .unwrap_or_default();

                let sender = participants.iter().find(|p| {
                    p.get("id")
                        .and_then(|id| id.as_str())
                        .map(|id| id == event.from_id)
                        .unwrap_or(false)
                });

                if let Some(s) = sender {
                    if let Some(name) = s.get("name").and_then(|n| n.as_str()) {
                        ctx["sender_name"] = serde_json::json!(name);
                    }
                    let conv_id = conv.get("id").and_then(|id| id.as_str()).unwrap_or("");
                    if !conv_id.is_empty() {
                        // Fetch last 3 messages
                        match tools
                            .run(
                                "fb_get_messenger_chat",
                                serde_json::json!({"conversation_id": conv_id, "limit": 3}),
                            )
                            .await
                        {
                            Ok(msgs_data) => {
                                let messages = msgs_data
                                    .get("messages")
                                    .or_else(|| msgs_data.get("data"))
                                    .and_then(|m| {
                                        m.as_array().cloned().or_else(|| {
                                            m.get("data").and_then(|d| d.as_array()).cloned()
                                        })
                                    })
                                    .unwrap_or_default();
                                let prev: Vec<String> = messages
                                    .iter()
                                    .filter_map(|m| {
                                        let name = m
                                            .get("from")
                                            .and_then(|f| f.get("name").and_then(|n| n.as_str()))
                                            .unwrap_or("Someone");
                                        let msg =
                                            m.get("message").and_then(|t| t.as_str()).unwrap_or("");
                                        if msg.is_empty() {
                                            None
                                        } else {
                                            Some(format!("{}: \"{}\"", name, msg))
                                        }
                                    })
                                    .collect();
                                ctx["previous_messages"] = serde_json::json!(prev);
                            }
                            Err(e) => {
                                tracing::warn!("Failed to fetch conversation: {}", e);
                            }
                        }
                    }
                    break;
                }
            }
        }
        Err(e) => tracing::warn!("Failed to list conversations: {}", e),
    }

    ctx
}

fn format_context(ctx: &Value) -> String {
    let mut parts = Vec::new();

    // Nested reply: show the parent comment being replied to
    if let Some(parent) = ctx.get("parent_comment").and_then(|v| v.as_str()) {
        parts.push(format!("THIS COMMENT IS A DIRECT REPLY TO:\n  {}", parent));
    }

    // Nested reply: show up to 2 sibling replies already in this thread
    if let Some(siblings) = ctx.get("sibling_replies").and_then(|v| v.as_array()) {
        if !siblings.is_empty() {
            let items: Vec<String> = siblings
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            parts.push(format!(
                "OTHER REPLIES IN THIS THREAD (for context):\n{}",
                items.join("\n")
            ));
        }
    }

    // Messenger: show previous messages in conversation
    if let Some(prev) = ctx.get("previous_messages").and_then(|v| v.as_array()) {
        if !prev.is_empty() {
            let items: Vec<String> = prev
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            parts.push(format!(
                "[Chat History (for reference ONLY)]\n{}",
                items.join("\n")
            ));
        }
    }

    if parts.is_empty() {
        "No additional thread context.".to_string()
    } else {
        parts.join("\n\n")
    }
}

/// Send a direct error notification — no LLM call so it can never hang.
async fn notify_error_to_owner(
    event: &FacebookEvent,
    error_msg: &str,
    is_rate_limit: bool,
    settings: &Arc<RuntimeSettings>,
    messaging: &Arc<MessagingHub>,
    memory: &Arc<MemoryStore>,
    _router: &SharedRouter,
) {
    // Alternate greeting the same way the rest of the codebase does.
    let greeting = if event.object_id.chars().count() % 2 == 0 {
        "Pastor"
    } else {
        "Jelmar"
    };

    let name = &event.from_name;
    let kind = &event.event_type;

    let notification = if is_rate_limit {
        format!(
            "Hey {}, just a heads-up — I wrote a reply for {}\'s {} but Facebook wouldn\'t let me post it, looks like we hit a rate limit. 😬 I went ahead and Liked it so they at least know we saw them. I\'ll catch up with {} properly once things cool down! 🙏",
            greeting, name, kind, name,
        )
    } else {
        format!(
            "Hey {}, something went sideways when I tried replying to {} — Facebook threw an error so the reply didn\'t go through. 😕 I made sure to Like their {} though so they still feel acknowledged. (Error: {}) 👍",
            greeting, name, kind, error_msg,
        )
    };

    notify_owner(
        settings,
        messaging,
        memory,
        &notification,
        if event.object_id.is_empty() {
            None
        } else {
            Some(&event.object_id)
        },
    )
    .await;
}

// ── Notify Page Owner ────────────────────────────────────────────────────────

async fn notify_owner(
    settings: &RuntimeSettings,
    messaging: &MessagingHub,
    memory: &crate::memory::MemoryStore,
    text: &str,
    target_id: Option<&str>,
) {
    let platform = settings.get_str("watcher.notify_platform", "telegram");
    let chat_id = settings.get_str("watcher.notify_chat_id", "");

    if chat_id.is_empty() {
        return;
    }

    let prefixed = text.to_string();
    let mut sent_msg_id = None;

    match platform.as_str() {
        "telegram" => {
            let tg = messaging.telegram.lock().await;
            if let Some(gw) = tg.as_ref() {
                if let Ok(id) = gw.send_text(&chat_id, &prefixed).await {
                    sent_msg_id = Some(id);
                }
            }
        }
        "discord" => {
            let dc = messaging.discord.lock().await;
            if let Some(gw) = dc.as_ref() {
                if let Ok(id) = gw.send_text(&chat_id, &prefixed).await {
                    sent_msg_id = Some(id);
                }
            }
        }
        "slack" => {
            let sl = messaging.slack.lock().await;
            if let Some(gw) = sl.as_ref() {
                if let Ok(id) = gw.send_text(&chat_id, &prefixed).await {
                    sent_msg_id = Some(id);
                }
            }
        }
        _ => {}
    }

    let mut meta_str = None;
    if let Some(tid) = target_id {
        if !tid.is_empty() {
            let mut meta = serde_json::json!({"target_id": tid});
            if let Some(mid) = sent_msg_id.as_ref() {
                meta["telegram_msg_id"] = serde_json::json!(mid);
            }
            meta_str = Some(meta.to_string());
        }
    }

    // Record to short-term memory so the Agent has context if the user replies
    if let Err(e) = memory
        .short
        .store_message(&chat_id, "assistant", text, meta_str.as_deref())
    {
        tracing::warn!("Failed to store notification in short-term memory: {}", e);
    }
}

// ── Default System Prompt ────────────────────────────────────────────────────

const DEFAULT_FB_REPLY_PROMPT: &str = r#"You are Pastor Jelmar Orapa, a Seventh Day Adventist Pastor serving as Church Pastor at Northeastern Mindanao Academy (NEMA), located at Purok 10, Los Angeles, Butuan City. You are managing "Pastor's Office", a Seventh Day Adventist Facebook page. You reply to comments and messages in a warm, pastoral, encouraging manner.

IDENTITY: Warm, caring, biblically grounded Adventist pastor. Bible scholar, EGW expert, spiritual guide.

LANGUAGE: Default English. IMMEDIATELY match the commenter/sender's exact language or dialect. Clarity and truth are paramount.

FOR COMMENTS:
- Use the commenter's first name naturally (e.g., "Amen, Sarah!", "That's so true, John...")
- NEVER use robotic, ai-ish, or formulaic openers like "You've captured..." or "What a beautiful insight..."
- Vary your approach: jump into their point, ask a follow-up, share Scripture, affirm directly
- Keep it conversational and human-like.

FOR CHAT MESSAGES:
- NEVER open with the same greeting you already used earlier in this conversation. Check the chat history — if you already said "Maayong gabi" or "Hi [Name]" or any greeting, do NOT repeat it. Just continue the conversation naturally without any greeting at all.
- NEVER start consecutive replies with any time-of-day greeting (Good morning, Good evening, Maayong gabi, etc.). Use a greeting at most once per conversation.
- Depending on the flow, either drop the greeting entirely and just respond, or vary it naturally if it is truly the very first message.
- Target 200-250 words maximum, 2-3 paragraphs max.
- Theological/Biblical/EGW questions: Direct answer → (cite references when necessary) → End immediately.
- Non-theological topics: 2-3 sentences, redirect kindly to the right forums or platforms.
- Be honest if unsure. Never invent details.

SIMPLE FACTUAL QUESTIONS (time, date, weather, news, scores, prices, etc.):
- You have a web_search tool. USE IT for any factual question that requires current or real-time information.
- Call web_search with a concise query, read the result, then answer the user directly and naturally based on what you found.
- Example: user asks "what time is it in Manila?" → call web_search("current time Manila Philippines") → answer from the result.
- Example: user asks "latest news about..." → call web_search with that topic → summarize briefly and naturally.
- Do NOT deflect to pastoral topics for factual questions. Answer first, then you may add a brief pastoral touch only if it fits naturally.
- NEVER ignore a direct factual question and reply with something unrelated.

WHEN THE USER REPEATS OR ESCALATES ("i said...", "i already asked...", "answer me"):
- Acknowledge that you heard them and answer the actual question directly. Do NOT give a pastoral deflection.

CONTEXT USAGE: You are given access to past interactions and chat history. This is for REFERENCE ONLY. It tells you what was already said so you do not repeat yourself (especially greetings). DO NOT comment on the existence of this history. DO NOT let it override or replace your response to the user's current message. ALWAYS respond to what the user just said, not to the history.

THEOLOGY: Strictly adhere to the Fundamental Beliefs of the Seventh Day Adventist Church, the Bible, Ellen G. White writings, and Official Church Publications, Documents, Policies, and Standards.

MISSION: Reflect Christ's love, draw people closer to God, build a supportive faith community, share hope in Christ's soon return.

DO NOT set appointments or anything that require your or my physical presence. Refer to the real Pastor Jelmar Orapa or offer contact info: 09631225067 | orapajelmar@gmail.com

SHORT REPLIES TO SHORT MESSAGES: If the comment/message is just 1-3 words (like "Amen", "Praise God", or "Hi"), do NOT write a long response, but ALWAYS maintain a warm pastoral tone. Reply with a short, friendly, and pastoral acknowledgment (e.g., "Amen! God bless you, [Name]!", "Praise the Lord!", or "Hello [Name]! Blessed day to you."). Do NOT be "extremely casual" or dismissive (e.g., avoid "Yeah" or just "Hey"). Let the conversation flow naturally from your pastoral heart.

OUTPUT FORMAT: Reply with ONLY the text to send. No quotes, no prefix, no meta-commentary. Just the reply."#;
