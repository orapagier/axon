use crate::config::RuntimeSettings;
use crate::memory::embeddings::{cosine_similarity, Embedder};
use crate::providers::types::Message;
use crate::router::model_router::SharedRouter;
use crate::tools::schema::ToolDefinition;
use anyhow::Context;
use once_cell::sync::Lazy;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use regex::Regex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub static CONVERSATIONAL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
    r"(?i)^(hi+|hello|hey|thanks|thank\s+you|ok(ay)?|sure|yes|no|nope|yep|yeah|yup|bye|goodbye|good\s+(morning|evening|night|day)|how\s+are\s+you|what.s\s+up|got\s+it|sounds\s+good|perfect|great|cool|nice|lol|haha|hmm+|alright|nevermind|nvm|noted|understood|what\s+day\s+is\s+it|what\s+time\s+is\s+it|what\s+is\s+the\s+date|today.s\s+date|what\s+date\s+is\s+it|current\s+time|date\s*time\s*now|date\s+now|time\s+now|clock)\W*$"
).unwrap()
});

pub static MULTISTEP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
    r"(?i)\b(then|and\s+then|after\s+that|afterwards|next\s*[,;]?|and\s+also|followed\s+by|once\s+(that.?s?\s+)?done|first\s*[,;]|second\s*[,;]|finally\s*[,;]?|additionally|as\s+well\s+as)\b"
).unwrap()
});

/// Anaphoric follow-up markers: the request leans on earlier turns for its
/// meaning ("get me another one", "do it again", "same for the other file").
/// Such messages carry no service keywords themselves, so routing them
/// requires folding recent history into the routing text.
static FOLLOWUP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
    r"(?i)\b(another|again|one\s+more|more\s+of|the\s+same|same\s+(one|thing|file|way)|that\s+(one|file|again)|this\s+one|those|these|them|it|previous|last\s+one|next\s+one|the\s+other|others?|as\s+well|too|also|retry|redo|resend|repeat|continue|keep\s+going)\b"
).unwrap()
});

/// Intent-aware static routes: maps (service keywords) → (read_tools, write_tools, all_tools).
/// When the user mentions a service, we check intent (read vs write) and inject ONLY relevant tools.
/// This prevents routing noise (e.g., send tools for "check new gmail") and ensures coverage for
/// services like Facebook that weren't previously routed at all.
struct StaticRoute {
    pattern: Regex,
    read_tools: Vec<&'static str>, // check, list, read, new, unread, inbox, get, show
    write_tools: Vec<&'static str>, // send, compose, write, create, post, reply, upload, share
}

static READ_INTENT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(check|list|read|new|unread|inbox|get|show|see|view|fetch|retrieve|latest|recent|upcoming|look\s+at|what.s|what\s+are|any\s+new|download)\b").unwrap()
});

static WRITE_INTENT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(send|compose|write|create|post|reply|upload|share|forward|schedule|draft|attach|make|publish|update|edit|delete|remove|hide|unhide|like|unlike|set|add|remind|reminder|email|mail|notify|message|dm|text)\b").unwrap()
});

static STATIC_ROUTES: Lazy<Vec<StaticRoute>> = Lazy::new(|| {
    vec![
        // ── Facebook ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(facebook\s+chat|fb\s+chat|messenger|facebook\s+message|fb\s+message|facebook\s+dm)s?\b").unwrap(),
            read_tools: vec!["fb_list_messenger_chats", "fb_get_messenger_chat"],
            write_tools: vec!["fb_send_message", "fb_send_message_image"],
        },
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(facebook\s+comment|fb\s+comment)s?\b").unwrap(),
            read_tools: vec!["fb_list_comments", "fb_recent_comments"],
            write_tools: vec!["fb_reply_to_comment", "fb_delete_comment", "fb_hide_comment", "fb_react_object", "fb_unreact_object"],
        },
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(facebook\s+post|fb\s+post)s?\b").unwrap(),
            read_tools: vec!["fb_list_posts", "fb_get_post", "fb_get_scheduled_posts"],
            write_tools: vec!["fb_create_post", "fb_create_post_with_image", "fb_create_post_with_video", "fb_update_post", "fb_delete_post", "fb_schedule_post"],
        },
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(facebook\s+page|fb\s+page|facebook\s+insight|fb\s+insight|facebook\s+analytics|fb\s+analytics|facebook\s+fans|fb\s+fans|facebook\s+reach|fb\s+reach)s?\b").unwrap(),
            read_tools: vec!["fb_get_page", "fb_page_insights", "fb_post_insights"],
            write_tools: vec!["fb_update_page"],
        },
        // Catch-all "facebook" — route to most common read tools
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(facebook|fb)\b").unwrap(),
            read_tools: vec!["fb_list_messenger_chats", "fb_list_posts", "fb_recent_comments", "fb_get_page"],
            write_tools: vec!["fb_create_post", "fb_create_post_with_image", "fb_create_post_with_video", "fb_send_message", "fb_reply_to_comment"],
        },
        // ── Gmail ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gmail|google\s+email|google\s+mail)\b").unwrap(),
            read_tools: vec!["gmail_list", "gmail_get", "gmail_search", "gmail_list_labels", "gmail_download_attachment", "gmail_list_drafts", "gmail_get_draft"],
            write_tools: vec!["gmail_send", "gmail_send_with_attachment", "gmail_reply", "gmail_trash", "gmail_mark_read", "gmail_mark_unread", "gmail_add_label", "gmail_remove_label", "gmail_forward", "gmail_delete", "gmail_untrash", "gmail_create_draft", "gmail_update_draft", "gmail_send_draft", "gmail_delete_draft"],
        },
        // ── Outlook ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(outlook|outlook\s+email|microsoft\s+email|ms\s+email)\b").unwrap(),
            read_tools: vec!["outlook_list_emails", "outlook_read_email", "outlook_search", "outlook_download_attachment"],
            write_tools: vec!["outlook_send_email", "outlook_reply_email", "outlook_forward_email", "outlook_send_with_attachment", "outlook_move_email", "outlook_delete_email"],
        },
        // ── Microsoft Calendar ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(mscal|microsoft\s+calendar|outlook\s+calendar|ms\s+cal)\b").unwrap(),
            read_tools: vec!["mscal_list_events", "mscal_get_event"],
            write_tools: vec!["mscal_create_event", "mscal_update_event", "mscal_delete_event"],
        },
        // ── Google Calendar ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gcal|google\s+calendar)\b").unwrap(),
            read_tools: vec![
                "gcal_list_events",
                "gcal_list_calendars",
                "gcal_get_event",
                "gcal_get_freebusy",
            ],
            write_tools: vec![
                "gcal_create_event",
                "gcal_quick_add",
                "gcal_update_event",
                "gcal_delete_event",
                "gcal_move_event",
            ],
        },
        // ── OneDrive ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(onedrive|one\s+drive|microsoft\s+drive|ms\s+drive)\b").unwrap(),
            read_tools: vec!["onedrive_list", "onedrive_search", "onedrive_download"],
            write_tools: vec!["onedrive_upload_binary", "onedrive_share", "onedrive_move_file", "onedrive_delete"],
        },
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gdrive|google\s+drive)\b").unwrap(),
            read_tools: vec![
                "gdrive_list",
                "gdrive_search",
                "gdrive_download_binary",
            ],
            write_tools: vec![
                "gdrive_upload_binary",
                "gdrive_upload_folder",
                "gdrive_share",
                "gdrive_move_file",
                "gdrive_delete",
            ],
        },
        // ── Google Contacts ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gcon|google\s+contacts?|google\s+people)\b").unwrap(),
            read_tools: vec!["gcon_list_contacts", "gcon_get_contact", "gcon_search_contacts"],
            write_tools: vec!["gcon_create_contact", "gcon_update_contact", "gcon_delete_contact"],
        },
        // ── Microsoft Contacts ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(mscontacts|microsoft\s+contacts?|outlook\s+contacts?)\b").unwrap(),
            read_tools: vec!["mscontacts_list_contacts", "mscontacts_get_contact"],
            write_tools: vec!["mscontacts_create_contact", "mscontacts_update_contact", "mscontacts_delete_contact"],
        },
        // ── Google Tasks ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gtasks|google\s+tasks?|to-do|todo)\b").unwrap(),
            read_tools: vec!["gtasks_list_lists", "gtasks_list_tasks"],
            write_tools: vec!["gtasks_create_task", "gtasks_complete_task"],
        },
        // ── Google Meet ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gmeet|google\s+meet|conference|transcript)\b").unwrap(),
            read_tools: vec!["gmeet_list_records", "gmeet_get_full_transcript"],
            write_tools: vec![],
        },
        // ── Google Docs ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gdocs|google\s+docs?)\b").unwrap(),
            read_tools: vec!["gdocs_get_text"],
            write_tools: vec!["gdocs_create", "gdocs_append_text"],
        },
        // ── Google Sheets ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gsheets|google\s+sheets?|spreadsheet)s?\b").unwrap(),
            read_tools: vec![
                "gsheets_list", "gsheets_get", "gsheets_read_range", "gsheets_batch_read", "gsheets_find",
            ],
            write_tools: vec![
                "gsheets_create", "gsheets_write_range", "gsheets_batch_write",
                "gsheets_append_rows", "gsheets_clear_range",
                "gsheets_add_sheet", "gsheets_delete_sheet", "gsheets_rename_sheet",
                "gsheets_duplicate_sheet", "gsheets_copy_sheet_to",
                "gsheets_insert_dimension", "gsheets_delete_dimension",
                "gsheets_sort_range", "gsheets_create_filter", "gsheets_clear_filter",
                "gsheets_merge_cells", "gsheets_unmerge_cells",
                "gsheets_bold_row", "gsheets_freeze_rows", "gsheets_auto_resize",
                "gsheets_format_cells", "gsheets_add_conditional_format",
                "gsheets_clear_conditional_formats", "gsheets_batch_update",
            ],
        },
        // ── Google Slides ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gslides|google\s+slides?|presentation)\b").unwrap(),
            read_tools: vec![],
            write_tools: vec!["gslides_create", "gslides_replace_text"],
        },
        // ── Google Chat ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\b(gchat|google\s+chat)\b").unwrap(),
            read_tools: vec!["gchat_list_spaces"],
            write_tools: vec!["gchat_send_message"],
        },
        // ── Workflows ──
        StaticRoute {
            pattern: Regex::new(r"(?i)\bworkflows?\b").unwrap(),
            read_tools: vec!["list_workflows"],
            write_tools: vec!["run_workflow"],
        },
    ]
});

/// Recent-history text used to route anaphoric follow-ups that don't name a
/// service themselves: the last couple of user turns plus the last assistant
/// turn, skipping the trailing copy of the current message (the caller appends
/// the in-flight task to history before routing). Truncated per-message so a
/// long earlier answer can't drown the routing signal.
fn history_context_text(msg: &str, history: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut users = 0usize;
    let mut assistants = 0usize;
    let mut skipped_current = false;
    for m in history.iter().rev() {
        let text = match &m.content {
            crate::providers::types::MessageContent::Text(t) => t,
            _ => continue,
        };
        if !skipped_current && m.role == "user" && text.trim() == msg {
            skipped_current = true;
            continue;
        }
        match m.role.as_str() {
            "user" if users < 2 => {
                users += 1;
                parts.push(text.chars().take(200).collect());
            }
            "assistant" if assistants < 1 => {
                assistants += 1;
                parts.push(text.chars().take(200).collect());
            }
            _ => {}
        }
        if users >= 2 && assistants >= 1 {
            break;
        }
    }
    parts.reverse();
    parts.join("\n")
}

/// Select tools from a StaticRoute based on user intent
fn select_by_intent(route: &StaticRoute, msg: &str) -> Vec<String> {
    let is_read = READ_INTENT.is_match(msg);
    let is_write = WRITE_INTENT.is_match(msg);
    if is_read && !is_write {
        route.read_tools.iter().map(|s| s.to_string()).collect()
    } else if is_write && !is_read {
        route.write_tools.iter().map(|s| s.to_string()).collect()
    } else {
        // Ambiguous or both — return all
        let mut all: Vec<String> = route.read_tools.iter().map(|s| s.to_string()).collect();
        all.extend(route.write_tools.iter().map(|s| s.to_string()));
        all
    }
}

pub struct ToolRouter {
    patterns: Arc<RwLock<HashMap<String, Vec<Regex>>>>,
    db: Arc<Pool<SqliteConnectionManager>>,
    router: SharedRouter,
    settings: Arc<RuntimeSettings>,
    /// Optional embedder for the zero-LLM semantic tool tier. Built from the
    /// `embedder.*` settings (legacy fallback: VOYAGE_API_KEY); `None` when
    /// unconfigured — the router then falls back to the LLM tier.
    embedder: Option<Embedder>,
    /// Cache of tool-description embeddings, keyed by tool name. Filled lazily
    /// (and batched) the first time each tool is a routing candidate.
    tool_embeddings: Arc<RwLock<HashMap<String, Vec<f32>>>>,
}

impl ToolRouter {
    pub fn new(
        db: Arc<Pool<SqliteConnectionManager>>,
        router: SharedRouter,
        settings: Arc<RuntimeSettings>,
    ) -> Self {
        let embedder = Embedder::from_settings(&settings);
        ToolRouter {
            patterns: Arc::new(RwLock::new(HashMap::new())),
            db,
            router,
            settings,
            embedder,
            tool_embeddings: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn load_patterns(&self) -> anyhow::Result<()> {
        let rows: Vec<(String, String)> = {
            let conn = self.db.get().context("DB pool")?;
            let mut s =
                conn.prepare("SELECT tool_name, pattern FROM tool_patterns WHERE enabled=1")?;
            let res = s
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            res
        };
        let mut map: HashMap<String, Vec<Regex>> = HashMap::new();
        for (tool, pat) in rows {
            match Regex::new(&format!("(?i){}", pat)) {
                Ok(re) => map.entry(tool).or_default().push(re),
                Err(e) => tracing::warn!("Invalid pattern '{}': {}", pat, e),
            }
        }
        let count: usize = map.values().map(|v| v.len()).sum();
        *self.patterns.write().await = map;
        tracing::info!("Tool router: {} patterns loaded", count);
        Ok(())
    }

    // FIX: async fn using .read().await — never calls block_on
    // Returns (matched tools, confident, service_hit). `service_hit` is exposed
    // so `filter_tools` can tell "no service named at all" (retry with history
    // context) apart from "matched but not confidently".
    async fn tier1(&self, msg: &str) -> (Vec<String>, bool, bool) {
        let p = self.patterns.read().await;
        let mut matched: Vec<String> = p
            .iter()
            .filter(|(_, pats)| pats.iter().any(|re| re.is_match(msg)))
            .map(|(n, _)| n.clone())
            .collect();

        // Intent-aware static routes: inject the right tools for the mentioned services,
        // filtered by user intent (read vs write) to avoid routing noise
        let mut service_hit = false;
        for route in STATIC_ROUTES.iter() {
            if route.pattern.is_match(msg) {
                for tool in select_by_intent(route, msg) {
                    if !matched.contains(&tool) {
                        matched.push(tool);
                    }
                }
                service_hit = true;
            }
        }

        let confident =
            service_hit || (!matched.is_empty() && matched.len() <= 3 && !MULTISTEP.is_match(msg));
        (matched, confident, service_hit)
    }

    /// Ensure every candidate tool has a cached description embedding. Misses are
    /// batched (chunked to stay within provider per-request input limits) and the
    /// write lock is only taken after all network calls complete.
    async fn ensure_tool_embeddings(&self, candidates: &[ToolDefinition], embedder: &Embedder) {
        let missing: Vec<(String, String)> = {
            let cache = self.tool_embeddings.read().await;
            candidates
                .iter()
                .filter(|t| !cache.contains_key(&t.name))
                .map(|t| (t.name.clone(), format!("{}: {}", t.name, t.description)))
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let mut fresh: Vec<(String, Vec<f32>)> = Vec::with_capacity(missing.len());
        for chunk in missing.chunks(96) {
            let texts: Vec<&str> = chunk.iter().map(|(_, txt)| txt.as_str()).collect();
            match embedder.embed(&texts).await {
                Ok(embs) if embs.len() == chunk.len() => {
                    for ((name, _), emb) in chunk.iter().zip(embs) {
                        fresh.push((name.clone(), emb));
                    }
                }
                Ok(_) => tracing::warn!("embed router: tool-embedding count mismatch"),
                Err(e) => {
                    tracing::debug!("embed router: tool embed failed: {}", e);
                    return;
                }
            }
        }
        if !fresh.is_empty() {
            let mut cache = self.tool_embeddings.write().await;
            for (name, emb) in fresh {
                cache.insert(name, emb);
            }
        }
    }

    /// Zero-LLM tool selection: embed the request once and cosine-match it
    /// against cached tool-description embeddings, returning the top-K tools
    /// above a similarity floor. Returns `None` (so the caller falls back to the
    /// LLM tier) when embeddings are disabled/unavailable or nothing clears the
    /// floor. This replaces a full chat-completion per routing turn with a single
    /// cheap embedding call.
    async fn tier_embed(
        &self,
        msg: &str,
        candidates: &[ToolDefinition],
    ) -> Option<(Vec<String>, serde_json::Value)> {
        if !self.settings.get_bool("router.use_embeddings", true) {
            return None;
        }
        let embedder = self.embedder.as_ref()?;
        if candidates.is_empty() {
            return None;
        }
        let t0 = std::time::Instant::now();
        self.ensure_tool_embeddings(candidates, embedder).await;
        let qv = match embedder.embed_one(msg).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("embed router: query embed failed: {}", e);
                return None;
            }
        };
        let top_k = self.settings.get_int("router.embed_top_k", 5).max(1) as usize;
        let floor = self.settings.get_f64("router.embed_floor", 0.45) as f32;
        let mut scored: Vec<(f32, String)> = {
            let cache = self.tool_embeddings.read().await;
            candidates
                .iter()
                .filter_map(|t| {
                    cache
                        .get(&t.name)
                        .map(|emb| (cosine_similarity(&qv, emb), t.name.clone()))
                })
                .collect()
        };
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let top_score = scored.first().map(|(s, _)| *s).unwrap_or(0.0);
        let hits: Vec<String> = scored
            .iter()
            .take_while(|(s, _)| *s >= floor)
            .take(top_k)
            .map(|(_, n)| n.clone())
            .collect();
        if hits.is_empty() {
            return None;
        }
        Some((
            hits,
            serde_json::json!({
                "tier": "embed",
                "duration_ms": t0.elapsed().as_millis(),
                "top_score": top_score,
            }),
        ))
    }

    async fn tier2(
        &self,
        msg: &str,
        candidates: &[ToolDefinition],
        history: &[Message],
        multi: bool,
    ) -> (Vec<String>, serde_json::Value) {
        let names: std::collections::HashSet<String> =
            candidates.iter().map(|t| t.name.clone()).collect();
        let tool_list = candidates
            .iter()
            .map(|t| {
                format!(
                    "- {}: {}",
                    t.name,
                    t.description.chars().take(120).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let prior: Vec<String> = history
            .iter()
            .rev()
            .filter(|m| m.role == "user")
            .take(2)
            .filter_map(|m| match &m.content {
                crate::providers::types::MessageContent::Text(t) => {
                    Some(t.chars().take(120).collect())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        // Both prompts are seeded (visible in the Settings page); a blank value
        // means "use the built-in" — router.user_prompt is seeded blank because
        // its default embeds the generated disambiguation block below.
        let mut sys_prompt = self.settings.get_str("router.system_prompt", "");
        if sys_prompt.trim().is_empty() {
            sys_prompt = "You are a routing proxy. Reply ONLY with comma-separated names of the tools needed, or exactly NONE. Do not use quotes or backticks.".to_string();
        }
        // The SERVICE DISAMBIGUATION block is generated from the single
        // `service_map::SERVICE_PAIRS` registry so it can never drift from the
        // pre-execution corrector and quality gate in the agent loop.
        let default_user_tmpl = format!(
            "You are a strict tool router. Based on the request, select the necessary tools from the list.\nTools:\n{{tool_list}}\n{{prior}}{{multi}}{}\nRequest: {{msg}}\n\nRULES:\n1. Output strictly a comma-separated list of tool names.\n2. Do NOT output anything else (no markdown, no quotes, no conversational text).\n3. If NO tools are needed, output exactly: NONE\n4. NEVER mix Google and Microsoft tools for the same request.\n\nTools needed:",
            crate::router::service_map::router_disambiguation_block()
        );
        let mut user_tmpl = self.settings.get_str("router.user_prompt", "");
        if user_tmpl.trim().is_empty() {
            user_tmpl = default_user_tmpl;
        }
        let prior_str = if prior.is_empty() {
            String::new()
        } else {
            format!("Prior: {}\n", prior.join(" -> "))
        };
        let multi_str = if multi {
            "Multi-step: list ALL tools needed.\n"
        } else {
            ""
        };
        let prompt = user_tmpl
            .replace("{tool_list}", &tool_list)
            .replace("{prior}", &prior_str)
            .replace("{multi}", &multi_str)
            .replace("{msg}", msg);

        let t0 = std::time::Instant::now();
        match crate::router::model_router::call_llm_with_options(
            &[Message::user(&prompt)],
            &sys_prompt,
            &[],
            None,
            "router",
            Arc::clone(&self.router),
            &self.settings,
            crate::router::model_router::CallLlmOptions {
                // Tool routing is a deterministic classification — pin temp to 0
                // for stable, reproducible selections.
                temperature: Some(0.0),
                ..Default::default()
            },
        )
        .await
        {
            Ok((resp, model, tier)) => {
                crate::router::model_router::record_aux_tokens(resp.usage.total());
                let raw = resp.text_content().trim().to_uppercase();
                let dur = t0.elapsed().as_millis();
                if raw == "NONE" || raw.is_empty() {
                    return (
                        vec![],
                        serde_json::json!({"tier": tier, "model": model, "duration_ms": dur}),
                    );
                }
                let mut used = std::collections::HashSet::new();
                let sel: Vec<String> = raw
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s: &String| {
                        if names.contains(s.as_str()) && !used.contains(s) {
                            used.insert(s.clone());
                            true
                        } else {
                            false
                        }
                    })
                    .collect();
                (
                    sel,
                    serde_json::json!({"tier": tier, "model": model, "duration_ms": dur}),
                )
            }
            Err(e) => {
                tracing::warn!("Tier2 router failed: {}", e);
                (vec![], serde_json::json!({"tier":"binary_llm_failed"}))
            }
        }
    }

    pub async fn filter_tools(
        &self,
        msg: &str,
        all_tools: &[ToolDefinition],
        history: &[Message],
    ) -> (Vec<ToolDefinition>, serde_json::Value) {
        self.filter_tools_impl(msg, all_tools, history, true).await
    }

    /// Hybrid tool scope: pattern + embedding tiers only. When neither is
    /// confident, returns the (possibly empty) pattern hits instead of paying
    /// an LLM routing call — the agent's `search_tools` meta-tool covers
    /// anything the cheap tiers miss.
    pub async fn filter_tools_cheap(
        &self,
        msg: &str,
        all_tools: &[ToolDefinition],
        history: &[Message],
    ) -> (Vec<ToolDefinition>, serde_json::Value) {
        self.filter_tools_impl(msg, all_tools, history, false).await
    }

    async fn filter_tools_impl(
        &self,
        msg: &str,
        all_tools: &[ToolDefinition],
        history: &[Message],
        allow_llm_tier: bool,
    ) -> (Vec<ToolDefinition>, serde_json::Value) {
        let msg = msg.trim();
        let by_name: HashMap<String, &ToolDefinition> =
            all_tools.iter().map(|t| (t.name.clone(), t)).collect();
        if CONVERSATIONAL.is_match(msg) {
            return (vec![], serde_json::json!({"tier":"simple_tasks"}));
        }
        let multi = MULTISTEP.is_match(msg);
        let (mut hits, mut confident, service_hit) = self.tier1(msg).await;
        let mut tier1_label = "regex";

        // Context-aware follow-up routing: when the message names no service
        // itself and reads like an anaphoric follow-up (or is just short),
        // re-run the pattern tier over recent history + the message. "get me
        // another one" then inherits the gdrive route from the previous turn.
        // The combined text also feeds the embedding tier below, so semantic
        // matching sees the same context.
        let mut route_msg = msg.to_string();
        if !service_hit
            && !history.is_empty()
            && (FOLLOWUP.is_match(msg) || msg.split_whitespace().count() <= 12)
        {
            let ctx_text = history_context_text(msg, history);
            if !ctx_text.is_empty() {
                let combined = format!("{}\n{}", ctx_text, msg);
                let (ctx_hits, ctx_confident, _) = self.tier1(&combined).await;
                if !ctx_hits.is_empty() {
                    for h in ctx_hits {
                        if !hits.contains(&h) {
                            hits.push(h);
                        }
                    }
                    confident = confident || ctx_confident;
                    tier1_label = "regex_context";
                }
                route_msg = combined;
            }
        }

        if confident {
            let tools = hits
                .iter()
                .filter_map(|n| by_name.get(n).map(|t| (*t).clone()))
                .collect();
            return (
                tools,
                serde_json::json!({"tier": tier1_label, "duration_ms": 0}),
            );
        }
        let candidates: Vec<ToolDefinition> = if multi || hits.is_empty() {
            all_tools.to_vec()
        } else {
            hits.iter()
                .filter_map(|n| by_name.get(n).map(|t| (*t).clone()))
                .collect()
        };

        // Zero-LLM embedding tier (when a Voyage key is configured). Replaces a
        // chat-completion per routing turn with one cheap embedding call. Falls
        // through to the LLM tier only when embeddings are unavailable or nothing
        // clears the similarity floor.
        if let Some((emb_hits, telem)) = self.tier_embed(&route_msg, &candidates).await {
            let merged = if multi && !hits.is_empty() {
                let mut v = hits.clone();
                for h in &emb_hits {
                    if !v.contains(h) {
                        v.push(h.clone());
                    }
                }
                v
            } else {
                emb_hits
            };
            let tools = merged
                .iter()
                .filter_map(|n| by_name.get(n).map(|t| (*t).clone()))
                .collect();
            return (tools, telem);
        }

        if !allow_llm_tier {
            // Cheap mode: unconfident routing returns the pattern hits as-is
            // (possibly empty) rather than paying an LLM call per iteration.
            let tools = hits
                .iter()
                .filter_map(|n| by_name.get(n).map(|t| (*t).clone()))
                .collect();
            return (tools, serde_json::json!({"tier": "pattern_unconfident"}));
        }

        let (llm_hits, telem) = self.tier2(msg, &candidates, history, multi).await;
        if telem.get("tier").and_then(|v| v.as_str()) != Some("binary_llm_failed") {
            let merged = if multi && !hits.is_empty() {
                let mut v = hits.clone();
                for h in &llm_hits {
                    if !v.contains(h) {
                        v.push(h.clone());
                    }
                }
                v
            } else {
                llm_hits
            };
            let tools = merged
                .iter()
                .filter_map(|n| by_name.get(n).map(|t| (*t).clone()))
                .collect();
            return (tools, telem);
        }
        (
            all_tools.to_vec(),
            serde_json::json!({"tier":"passthrough"}),
        )
    }

    pub async fn all_patterns(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let conn = self.db.get()?;
        let mut s = conn.prepare("SELECT id, tool_name, pattern, description, enabled FROM tool_patterns ORDER BY tool_name")?;
        let res: Vec<serde_json::Value> = s
            .query_map([], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, i64>(0)?,
                    "tool_name": r.get::<_, String>(1)?,
                    "pattern": r.get::<_, String>(2)?,
                    "description": r.get::<_, Option<String>>(3)?,
                    "enabled": r.get::<_, bool>(4)?,
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(res)
    }

    pub async fn update_bulk_patterns(&self, items: Vec<serde_json::Value>) -> anyhow::Result<()> {
        {
            let mut conn = self.db.get()?;
            let tx = conn.transaction()?;
            // Keep MCP patterns untouched by filtering out enabled=1 where it might contain "mcp" (?)
            // Actually, easiest is just delete all and assume user JSON provides them, OR wipe everything and since MCP patterns are seeded on startup, we just let them stay or be recreated.
            // Let's wipe everything because main.rs "INSERT OR IGNORE" handles MCP patterns on boot.
            tx.execute("DELETE FROM tool_patterns", [])?;
            let mut stmt = tx.prepare("INSERT INTO tool_patterns (tool_name, pattern, description, enabled) VALUES (?1, ?2, ?3, ?4)")?;
            for raw in items {
                let tool_name = raw.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
                let pattern = raw.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
                let description = raw.get("description").and_then(|v| v.as_str());
                let enabled = raw.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

                if tool_name.is_empty() || pattern.is_empty() {
                    continue;
                }
                stmt.execute(rusqlite::params![tool_name, pattern, description, enabled])?;
            }
            drop(stmt);
            tx.commit()?;
        }
        let _ = self.load_patterns().await;
        Ok(())
    }

    pub async fn add_pattern(
        &self,
        tool: &str,
        pat: &str,
        desc: Option<&str>,
    ) -> anyhow::Result<i64> {
        let conn = self.db.get()?;
        conn.execute("INSERT INTO tool_patterns (tool_name, pattern, description, enabled) VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![tool, pat, desc])?;
        let id = conn.last_insert_rowid();
        let _ = self.load_patterns().await;
        Ok(id)
    }

    pub async fn set_enabled(&self, id: i64, enabled: bool) -> anyhow::Result<()> {
        let conn = self.db.get()?;
        conn.execute(
            "UPDATE tool_patterns SET enabled=?1 WHERE id=?2",
            rusqlite::params![enabled, id],
        )?;
        let _ = self.load_patterns().await;
        Ok(())
    }

    pub async fn delete_pattern(&self, id: i64) -> anyhow::Result<()> {
        let conn = self.db.get()?;
        conn.execute("DELETE FROM tool_patterns WHERE id=?1", [id])?;
        let _ = self.load_patterns().await;
        Ok(())
    }
}
