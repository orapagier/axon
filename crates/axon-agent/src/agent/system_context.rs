//! Per-run system-context builder, extracted from `agent::r#loop::run_inner`.
//!
//! `build_run_context` assembles everything the agent loop needs before its
//! first model call: short-term history (with stale alert footers stripped),
//! a parallel memory search + initial tool routing, long-term observations, and
//! the composed system prompt with the time/memory/observation/file context
//! blocks. The result is consumed (by destructuring) at the top of `run_inner`.

use crate::agent::r#loop::strip_router_alert_footer;
use crate::agent::RunContext;
use crate::memory::compressor::search_recent_observations;
use crate::providers::types::{Message, MessageContent};
use crate::state::AppState;
use crate::tools::schema::ToolDefinition;

pub(crate) struct RunSystemContext {
    pub(crate) sys: String,
    pub(crate) messages: Vec<Message>,
    pub(crate) filtered_initial: Vec<ToolDefinition>,
    pub(crate) tier_initial: String,
}

pub(crate) async fn build_run_context(
    task: &str,
    state: &AppState,
    ctx: &RunContext,
    base_system: &str,
    needs_time_context: bool,
) -> RunSystemContext {
    let top_k = state.settings.long_term_top_k();
    let memory_enabled = ctx.memory_enabled;
    // Isolated runs (Axon workflow nodes) keep ONLY their own short-term window;
    // they never reach into the global long-term store or observation log.
    let isolated = ctx.isolated_memory;
    let is_conversational = crate::router::tool_router::CONVERSATIONAL.is_match(task);
    let should_search_memory = memory_enabled && !isolated && !is_conversational && task.len() > 10;

    // Load short-term history. A per-run memory_window (set by the Axon node)
    // bounds how many recent messages feed the model; otherwise use the full
    // session (already capped by the global short-term limit on write).
    let mut messages: Vec<Message> = if memory_enabled {
        match ctx.memory_window {
            Some(window) => state
                .memory
                .short
                .to_messages_limited(&ctx.session_id, window)
                .unwrap_or_default(),
            None => {
                // Dashboard chats retain a long transcript (bounded by
                // short_term_max_msgs) but only feed the model the newest few
                // turns, so per-message context stays cheap even in a long
                // thread. Other platforms keep the full session window.
                let ctx_window = state.settings.get_int("memory.dashboard_context_window", 5);
                if ctx.platform == "dashboard" && ctx_window > 0 {
                    state
                        .memory
                        .short
                        .to_messages_limited(&ctx.session_id, ctx_window as usize)
                        .unwrap_or_default()
                } else {
                    state
                        .memory
                        .short
                        .to_messages(&ctx.session_id)
                        .unwrap_or_default()
                }
            }
        }
    } else {
        Vec::new()
    };

    // Strip stale alert footers from old history rows
    for msg in &mut messages {
        if msg.role == "assistant" {
            if let MessageContent::Text(text) = &mut msg.content {
                *text = strip_router_alert_footer(text);
            }
        }
    }
    // A bounded window can begin mid-thread on an assistant turn; providers
    // (e.g. Anthropic) require the first message to be a user turn, so drop any
    // leading assistant messages before the current task is appended.
    while messages
        .first()
        .map(|m| m.role == "assistant")
        .unwrap_or(false)
    {
        messages.remove(0);
    }
    if messages.last().map(|m| m.role != "user").unwrap_or(true) {
        messages.push(Message::user(task));
    }
    if memory_enabled {
        match ctx.memory_window {
            Some(window) => {
                let _ = state.memory.add_user_capped(&ctx.session_id, task, window);
            }
            None => {
                let _ = state.memory.add_user(&ctx.session_id, task);
            }
        }
    }

    // Parallel: memory search + initial tool routing
    let (memories_res, routing_res) = tokio::join!(
        async {
            if should_search_memory {
                let exclude = if ctx.session_id == "owner" {
                    Some("scheduler")
                } else {
                    None
                };
                state.memory.search(task, top_k, exclude).await
            } else {
                Ok(vec![])
            }
        },
        async {
            let all_tools = state.tools.all_enabled_for_agent().await;
            if let Some(ref allowed) = ctx.allowed_tools {
                let filtered: Vec<_> = all_tools
                    .into_iter()
                    .filter(|t| allowed.contains(&t.name))
                    .collect();
                let mut info = serde_json::Map::new();
                info.insert(
                    "tier".to_string(),
                    serde_json::Value::String("manual".to_string()),
                );
                (filtered, serde_json::Value::Object(info))
            } else {
                state.tool_router.filter_tools(task, &all_tools, &[]).await
            }
        }
    );

    let memories = memories_res.unwrap_or_default();
    let (filtered_initial, route_info) = routing_res;
    let tier_initial = route_info
        .get("tier")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Long-term observations (skipped for isolated runs)
    let observations = if memory_enabled && !isolated {
        search_recent_observations(task, 5, &state.db)
    } else {
        Vec::new()
    };

    // Build context strings
    let time_ctx = if needs_time_context {
        let now = ctx.user_time.clone().unwrap_or_else(|| {
            chrono::Utc::now()
                .with_timezone(&state.settings.agent_utc_offset())
                .format("%A, %B %e, %Y, %H:%M:%S")
                .to_string()
        });
        format!("- Local time/date: {}\n", now)
    } else {
        String::new()
    };

    let mem_ctx = if memories.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n[Relevant memories — these are HINTS only, always verify with tools]\n{}",
            memories
                .iter()
                .map(|m| format!("- ({}) {}", m.created_at, m.content))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let obs_ctx = if observations.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n[Recent tool observations — for context only, always re-verify]\n{}",
            observations.join("\n")
        )
    };

    let files_ctx = if ctx.attached_files.is_empty() {
        String::new()
    } else {
        let lines: Vec<String> = ctx
            .attached_files
            .iter()
            .map(|f| {
                format!(
                    "  - '{}' ({}, {} bytes) at: {}",
                    f.original_name, f.mime_type, f.size, f.local_path
                )
            })
            .collect();
        format!("\n\n[User attached files]\n{}", lines.join("\n"))
    };

    // A tool-free run (explicit empty allow-list, e.g. the Classifier node) is a
    // single structured completion whose own system prompt fully defines the
    // output contract. The conversational scaffolding below — tool-calling
    // CRITICAL RULES + FILE HANDLING — would fight that contract: rule #4
    // ("PLAIN TEXT ONLY") in particular makes the model paraphrase its JSON as
    // prose. So for these runs we keep base_system + context blocks only.
    let tool_free = matches!(ctx.allowed_tools.as_deref(), Some([]));
    let context_block = format!("{}{}{}{}", time_ctx, mem_ctx, obs_ctx, files_ctx);

    let sys = if tool_free {
        if context_block.is_empty() {
            base_system.to_string()
        } else {
            format!("{}\n\n[Context]\n{}", base_system, context_block)
        }
    } else {
        format!(
            "{}\n\n[Context]\n{}\n\n\
CRITICAL RULES:\n\
1. ALWAYS call the relevant tool to get current data. NEVER answer from memories or past observations alone.\n\
2. Memories and observations above are HINTS for context — they may be outdated or wrong. Always verify by calling tools.\n\
3. If a tool exists for the request (email, calendar, search, etc.), you MUST call it — do NOT claim you cannot access it.\n\
4. Provide responses in PLAIN TEXT ONLY. Do not use Markdown formatting.\n\
5. If your previous chat history contains errors, outages, or broken services, DO NOT bring them up again when the user says 'hi' or greets you. Assume problems may have been resolved in the background. Only discuss past errors if the user explicitly asks.\n\
6. Do NOT mention the current time/date unless the user asked for it or the task genuinely requires it.\n\n\
FILE HANDLING:\n\
- To send a file back to the user, include <send_file>/path/to/local/file</send_file> in your final answer.\n\
- When user attached files are listed above, use the local path directly with upload tools.\n\
- SSH upload: action='upload_file', local_path=<path>, remote_path=<destination>\n\
- Google Drive upload: gdrive_upload_binary, local_path=<path>, name=<filename>\n\
- OneDrive upload: onedrive_upload_binary, local_path=<path>, name=<filename>\n\
- Gmail with attachment: gmail_send_with_attachment, local_path=<path>\n\
- Outlook with attachment: outlook_send_with_attachment, local_path=<path>\n\
- SSH download saves to local path; use <send_file> to deliver it to user.\n\
- GDrive/OneDrive download tools return local path; use <send_file> to deliver.",
            base_system, context_block
        )
    };

    RunSystemContext {
        sys,
        messages,
        filtered_initial,
        tier_initial,
    }
}
