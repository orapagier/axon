use super::context::{AgentEvent, RunContext};
use super::quality::{extract_tool_evidence, quality_check, COMPLETION_SIGNALS, RE_CALL_COLON};
use super::tool_writer::write_temporary_tool;
use crate::memory::compressor::compress_and_store;
use crate::providers::types::{ContentBlock, Message, MessageContent};
use crate::router::{call_llm_with_options, drain_alerts, CallLlmOptions};
use crate::state::AppState;
use crate::tools::runner::run_parallel;
use crate::tools::schema::{ToolDefinition, ToolSource};
use once_cell::sync::Lazy;
use std::sync::Arc;
use tokio::sync::mpsc;

// Internal tool dispatch + handlers were extracted to `agent::internal_tools`.
// Re-exported here so existing `crate::agent::r#loop::execute_internal_tool_from_workflow`
// call sites (e.g. tools::workflow) keep resolving.
pub use crate::agent::internal_tools::execute_internal_tool_from_workflow;
// Error/alert notification dispatch lives in `agent::notifications`; imported so
// the many call sites in `run_inner` resolve without qualification.
use crate::agent::notifications::{
    dispatch_global_error_notification_event, dispatch_router_alert_notifications,
};
use crate::agent::repair::{repair_tool_call, RepairDecision};
use crate::agent::system_context::{build_run_context, RunSystemContext};

// ── Compiled-once regexes ────────────────────────────────────────────────────

static RE_TOOL_TEXT: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?i)\bTool:\s*\w+").unwrap());
static RE_THINKING_BLOCK: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?si)\(Thinking:.*?\)").unwrap());
/// Reasoning models that emit their chain of thought inline (<think>...</think>,
/// e.g. DeepSeek/Qwen via OpenAI-compat gateways) must never leak it to users.
static RE_THINK_TAG: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?si)<think>.*?</think>").unwrap());
static RE_STRIP_TOOL_LINE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?im)^\s*Tool:\s*.*$").unwrap());
static RE_STRIP_PARAMS_LINE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?im)^\s*Parameters:\s*.*$").unwrap());
static RE_STRIP_ACTION_LINE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?im)^\s*Action:\s*.*$").unwrap());
static RE_STRIP_OBSERVATION_LINE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?im)^\s*Observation:\s*.*$").unwrap());
static RE_STRIP_THOUGHT_LINE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?im)^\s*Thought:\s*.*$").unwrap());
static RE_STRIP_API_NAME: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"(?im)^\s*"?api_name"?:\s*.*$"#).unwrap());
static RE_STRIP_PARAMETERS: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r#"(?im)^\s*"?parameters"?:\s*.*$"#).unwrap());
static RE_COLLAPSE_NEWLINES: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"\n{3,}").unwrap());
static RE_STRIP_CALL_COLON: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?im)^\s*call:\w+\{.*\}\s*$").unwrap());
static RE_MUTATION_CLAIM: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(
        r"(?i)\b(i|we|it)\s+(?:have\s+)?(sent|created|scheduled|deleted|updated|uploaded|moved|replied|forwarded|marked|posted|published|completed|added|removed|shared|triggered|ran|executed)\b",
    )
    .unwrap()
});
static RE_DATA_CLAIM: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(
        r"(?i)\b(i checked|i found|i retrieved|i looked up|in your|from your|you have\s+\d+|there\s+(?:is|are)\s+\d+)\b",
    )
    .unwrap()
});
static RE_TIME_SENSITIVE_TASK: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(
        r"(?i)\b(now|current|time|date|day|clock|today|tomorrow|yesterday|tonight|morning|afternoon|evening|weekend|this\s+week|next\s+week|this\s+month|next\s+month|this\s+year|next\s+year|monday|tuesday|wednesday|thursday|friday|saturday|sunday|schedule|scheduled|scheduling|remind|reminder|deadline|due|calendar|meeting|appointment|cron|timezone|utc|pst|est|cst|mst|gmt|in\s+\d+\s+(minute|minutes|hour|hours|day|days|week|weeks|month|months)|at\s+\d{1,2}(?::\d{2})?\s*(am|pm)?)\b",
    )
    .unwrap()
});
/// A response that opens with a promise of future action ("Let me grab...",
/// "I'll fetch...") instead of doing the work. The agent cannot act between
/// turns, so an answer like this with no successful tool call this run is a
/// dead end — the promised work never happens.
static RE_PROMISE_ONLY: Lazy<regex::Regex> = Lazy::new(|| {
    regex::Regex::new(
        r"(?i)^\W*(sure|okay|ok|alright|got it|certainly|of course|no problem|absolutely)?[\s,!.:;\-—–]*(let me|i.ll|i will|i.m going to|i am going to|i.m on it|give me a (moment|sec|second|minute|bit)|one (moment|sec|second|minute)|just a (moment|sec|second|minute)|hold on|hang on|working on (it|that)|right away|will do)\b",
    )
    .unwrap()
});

// ── Shared constants — single source of truth ────────────────────────────────

/// Phrases that indicate the model is refusing to use an available tool.
/// Defined once to prevent the two-location duplication that caused silent drift.
const REFUSAL_PHRASES: &[&str] = &[
    "unable to",
    "cannot",
    "can't",
    "not found",
    "not available",
    "not installed",
    "i don't have",
    "i do not have",
    "i'm unable",
    "don't have access",
    "do not have access",
    "no access",
    "not connected",
    "can not",
    "isn't connected",
    "is not connected",
    "i lack",
    "not possible",
    "not supported",
];

/// Tool name aliases emitted by hallucinating models, mapped to real names.
/// Static so it is never allocated inside the hot loop.
static TOOL_NAME_REMAPS: &[(&str, &str)] = &[
    // Gmail
    ("gmail_tool", "gmail_list"),
    ("gmail_read_email", "gmail_read"),
    ("gmail_check", "gmail_list"),
    ("gmail_inbox", "gmail_list"),
    ("gmail_unread", "gmail_list"),
    ("gmail_list_unread", "gmail_list"),
    ("gmail_send_email", "gmail_send"),
    // Outlook
    ("outlook_tool", "outlook_list_emails"),
    ("outlook_list_unread", "outlook_list_emails"),
    ("outlook_check", "outlook_list_emails"),
    ("outlook_inbox", "outlook_list_emails"),
    ("outlook_unread", "outlook_list_emails"),
    ("outlook_read", "outlook_read_email"),
    ("outlook_send", "outlook_send_email"),
    // Calendar
    ("calendar_tool", "gcal_list_events"),
    ("google_calendar", "gcal_list_events"),
    ("gcal_events", "gcal_list_events"),
    ("ms_calendar", "mscal_list_events"),
    ("microsoft_calendar", "mscal_list_events"),
    ("mscal_events", "mscal_list_events"),
    // Drive
    ("drive_tool", "gdrive_list"),
    ("google_drive", "gdrive_list"),
    ("onedrive_tool", "onedrive_list"),
    // Facebook
    ("facebook_page", "fb_get_page"),
    ("facebook_posts", "fb_list_posts"),
    ("fb_posts", "fb_list_posts"),
    ("facebook_insights", "fb_get_insights"),
    ("fb_insights", "fb_get_insights"),
    ("facebook_chats", "fb_list_messenger_chats"),
    ("fb_chats", "fb_list_messenger_chats"),
    ("facebook_messages", "fb_list_messenger_chats"),
    ("fb_messages", "fb_list_messenger_chats"),
];

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct ToolExecutionReceipt {
    name: String,
    ok: bool,
    is_mutating: bool,
}

/// The single verdict produced by `validate_response`.
/// Computed once per response; drives both token emission and loop control.
enum ValidationDecision {
    /// Response is good — emit token and break (or continue to finalize).
    Pass,
    /// Loop must continue. Inject `message` into history before the next LLM call.
    Retry {
        message: String,
        reason: RetryReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryReason {
    HallucinatedToolCall,
    Refusal,
    ClaimGuard,
    QualityCheck,
    BlankResponse,
    /// Final answer arrived while the run's plan still has open steps.
    PlanIncomplete,
}

/// Per-run counters for observability. Written to the DB at finalization.
#[derive(Default)]
struct GuardCounts {
    nudge_count: u32,
    claim_guard_count: u32,
    qc_correction_count: u32,
}

// ── Small helpers ────────────────────────────────────────────────────────────

fn stable_route_seed(input: &str) -> usize {
    // Stable FNV-1a so routing remains reproducible across process restarts
    // and Rust stdlib hasher changes.
    const FNV_OFFSET_BASIS_64: u64 = 0xcbf29ce484222325;
    const FNV_PRIME_64: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS_64;
    for &b in input.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    hash as usize
}

fn spawn_model_wait_heartbeat(
    tx: Option<mpsc::Sender<AgentEvent>>,
    run_id: String,
    has_candidate_tools: bool,
) -> Option<tokio::task::JoinHandle<()>> {
    let tx = tx?;
    Some(tokio::spawn(async move {
        let mut waited_secs = 0u64;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;
            waited_secs += 4;
            let text = if has_candidate_tools {
                format!(
                    "Waiting for the model to decide the next step... {}s",
                    waited_secs
                )
            } else {
                format!("Waiting for the model response... {}s", waited_secs)
            };
            if tx
                .send(AgentEvent::Thinking {
                    run_id: run_id.clone(),
                    text,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    }))
}

/// Whether an executed tool mutates state. Reads the authoritative `is_mutating`
/// flag from the tool registry (the single source of truth), falling back to
/// name-based derivation only for tools not present in the registry — e.g. a
/// freshly written temp tool or a repaired call whose name isn't registered yet.
///
/// This replaces the old substring-marker scan, which false-positived read
/// tools whose names merely contained a write marker (`fb_list_posts` → `_post`,
/// `fb_get_scheduled_posts` → `_schedule`). Those misclassifications let a
/// successful *read* vouch for a fabricated *write* claim in the claim guard.
/// Deterministic pre-execution check: every required parameter must be present
/// and non-null in the call's input. Returns the missing field names. Catches the
/// most common arg error (an omitted required field) before spending a real tool
/// round-trip — the cheapest possible correction. Unknown tools / empty `required`
/// yield no findings, so the check never blocks a legitimate call.
fn missing_required_args(
    name: &str,
    input: &serde_json::Value,
    all_tools: &[ToolDefinition],
) -> Vec<String> {
    let def = match all_tools.iter().find(|t| t.name == name) {
        Some(d) => d,
        None => return Vec::new(),
    };
    if def.required.is_empty() {
        return Vec::new();
    }
    let obj = input.as_object();
    def.required
        .iter()
        .filter(|r| match obj {
            Some(map) => map.get(r.as_str()).map(|v| v.is_null()).unwrap_or(true),
            None => true,
        })
        .cloned()
        .collect()
}

pub(crate) fn receipt_is_mutating(name: &str, all_tools: &[ToolDefinition]) -> bool {
    all_tools
        .iter()
        .find(|t| t.name == name)
        .map(|t| t.is_mutating)
        .unwrap_or_else(|| crate::tools::schema::derive_is_mutating(name))
}

fn is_explicit_tool_authoring_task(task: &str) -> bool {
    let lower = task.to_ascii_lowercase();
    let phrases = [
        "write a tool",
        "create a tool",
        "build a tool",
        "make a tool",
        "add a tool",
        "temporary tool",
        "temp tool",
        "python tool",
        "new tool",
        "tool named",
        "tool definition",
        "missing tool",
        "plugin",
    ];
    phrases.iter().any(|phrase| lower.contains(phrase))
}

/// Returns true when the task implies a bulk/recurring operation.
/// Uses positive + negative signals to reduce false positives.
/// E.g. "tell me all about Paris" has "all" but is not a bulk task.
pub(crate) fn is_bulk_task(task: &str) -> bool {
    let lower = task.to_lowercase();
    let positives = ["all", "every", "each", "multiple", "recurring", "batch"];
    let negatives = [
        "all about",
        "all in all",
        "after all",
        "above all",
        "overall",
        "tell me all",
        "all the info",
        "all the detail",
    ];
    let has_positive = positives.iter().any(|s| lower.contains(s));
    let has_negative = negatives.iter().any(|s| lower.contains(s));
    has_positive && !has_negative
}

fn deterministic_tool_claim_guard(
    text: &str,
    routed_tools: &[String],
    tool_receipts: &[ToolExecutionReceipt],
) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let any_success = tool_receipts.iter().any(|r| r.ok);
    let any_mutating_success = tool_receipts.iter().any(|r| r.ok && r.is_mutating);
    let successful_tools = tool_receipts
        .iter()
        .filter(|r| r.ok)
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>();
    let routed_or_attempted_tools = !routed_tools.is_empty() || !tool_receipts.is_empty();
    let claims_mutation = RE_MUTATION_CLAIM.is_match(text);
    let claims_tool_backed_data = RE_DATA_CLAIM.is_match(text)
        || (lower.contains("your ") && routed_or_attempted_tools && !any_success);

    if claims_mutation && !any_mutating_success {
        let executed = if successful_tools.is_empty() {
            "none".to_string()
        } else {
            successful_tools.join(", ")
        };
        return Some(format!(
            "You claimed a mutating action was completed, but there is no successful mutating execution receipt for this run. Successful tools so far: {}. You must call the correct tool natively before claiming the action is done, or clearly say it has not been completed yet.",
            executed
        ));
    }

    if routed_or_attempted_tools && !any_success && claims_tool_backed_data {
        return Some(
            "You claimed fresh tool-backed data, but no successful tool execution receipt exists for this run. Do not present checked/found/retrieved results until a real tool call succeeds."
                .to_string(),
        );
    }

    None
}

fn task_needs_time_context(task: &str) -> bool {
    RE_TIME_SENSITIVE_TASK.is_match(task)
}

// ── Validation pipeline ──────────────────────────────────────────────────────

/// Single-pass response validator. Replaces the two parallel condition trees
/// that previously existed (one for token deferral, one for the actual retry).
/// This is the only place where QC eligibility, claim guard, and nudge logic live.
///
/// Returns `Pass` when the response is ready to emit, or `Retry { message, reason }`
/// when the loop must continue with an injected correction.
async fn validate_response(
    text: &str,
    tool_names: &[String],
    tool_receipts: &[ToolExecutionReceipt],
    tools_used: &[String],
    guard_counts: &mut GuardCounts,
    iters: u32,
    task: &str,
    messages: &[Message],
    bulk_task: bool,
    router: crate::router::model_router::SharedRouter,
    settings: &crate::config::RuntimeSettings,
    qc_enabled: bool,
    is_subtask: bool,
    run_id: &str,
) -> ValidationDecision {
    // 1. Claim guard (deterministic, zero cost)
    if let Some(correction) = deterministic_tool_claim_guard(text, tool_names, tool_receipts) {
        guard_counts.claim_guard_count += 1;
        return ValidationDecision::Retry {
            message: format!(
                "TOOL EXECUTION GUARD — revise your response.\n{}\n\nYou may either call the correct tool natively or rewrite the answer so it honestly states what has and has not actually been done.",
                correction
            ),
            reason: RetryReason::ClaimGuard,
        };
    }

    // 2. Refusal nudge (deterministic, zero cost)
    let was_tool_routed = !tool_names.is_empty();
    if was_tool_routed && iters <= 3 {
        let text_lower = text.to_lowercase();
        let seems_refusal = REFUSAL_PHRASES.iter().any(|p| text_lower.contains(p))
            && !text_lower.contains("but here")
            && !text_lower.contains("however");
        let has_data = text.len() > 200 || text.lines().count() > 5;

        if seems_refusal && !has_data {
            guard_counts.nudge_count += 1;
            return ValidationDecision::Retry {
                message: format!(
                    "IMPORTANT: You have these tools available and connected right now: [{}]. \
                     The tools ARE working and accessible. Any previous memories about failures are OUTDATED. \
                     Please USE the tool(s) now to complete the request. \
                     Do not refuse or explain why you can't — just call the tool.",
                    tool_names.join(", ")
                ),
                reason: RetryReason::Refusal,
            };
        }
    }

    // 3. Blank-response check (always run, zero cost)
    let mut clean = strip_reasoning(text);
    if tools_used.is_empty() {
        clean = clean
            .replace("**", "")
            .replace("__", "")
            .replace("###", "")
            .replace("##", "")
            .replace("# ", "")
            .replace('`', "");
    }
    let is_blank = clean.trim().is_empty();
    if is_blank {
        return ValidationDecision::Retry {
            message: "Your response was completely blank or only contained internal thinking blocks. You MUST output a clear, human-readable text response to the user. If the error is due to a service being unreachable, or the task is not possible with your current capabilities and tools, explain it in plain text.".to_string(),
            reason: RetryReason::BlankResponse,
        };
    }

    // 3.5. Unfulfilled-promise guard (deterministic, zero cost). Catches "Let
    // me grab that for you." turns that end with only a promise: no successful
    // tool receipt exists, so the promised work would silently never happen.
    // Fires even when NO tools were routed — the retry re-routes with fuller
    // conversation context, which is exactly what anaphoric follow-ups
    // ("get me another one") need. Shares the nudge counter with the refusal
    // guard so corrections stay bounded.
    {
        let any_success = tool_receipts.iter().any(|r| r.ok);
        let clean_trimmed = clean.trim();
        let is_promise_only = !any_success
            && guard_counts.nudge_count < 2
            && clean_trimmed.len() < 300
            && RE_PROMISE_ONLY.is_match(clean_trimmed)
            && !clean_trimmed.to_lowercase().contains("let me know")
            && !text.contains("<send_file>");
        if is_promise_only {
            guard_counts.nudge_count += 1;
            return ValidationDecision::Retry {
                message: "You ended your turn with only a promise of future action ('let me...', 'I'll...'). You CANNOT perform work between turns — the user only sees this message and nothing would ever happen. Re-read the conversation to resolve what the user is referring to, call the needed tool(s) NOW, and reply with the actual result. If the request is genuinely impossible, say so plainly instead of promising.".to_string(),
                reason: RetryReason::Refusal,
            };
        }
    }

    // 3.7. Plan completion check (deterministic, zero cost, one-shot). A
    // final answer while the run's plan still has open steps usually means
    // the model lost track mid-task. Remind once, listing the open steps;
    // shares the global correction budget so it can't loop.
    if let Some(open) = crate::agent::plan::open_steps(run_id) {
        if !open.is_empty() && crate::agent::plan::mark_reminded(run_id) {
            return ValidationDecision::Retry {
                message: format!(
                    "PLAN CHECK — your plan still has open steps:\n{}\n\nEither complete them now (call the needed tools, then update_plan with status \"done\"), or state plainly in your final answer why each remaining step was skipped.",
                    open.join("\n")
                ),
                reason: RetryReason::PlanIncomplete,
            };
        }
    }

    // 4. Quality check (LLM call — only when actually needed)
    let tools_routed_but_unused = !tool_names.is_empty() && tools_used.is_empty();
    let is_completion_confirm = !tools_used.is_empty()
        && clean.len() < 400
        && !bulk_task
        && COMPLETION_SIGNALS
            .iter()
            .any(|s| clean.to_lowercase().contains(s));

    let trimmed = text.trim();
    let looks_like_raw_tool = trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.contains("```json")
        || trimmed.contains("<tool_call>")
        || trimmed.contains("<function=")
        || RE_CALL_COLON.is_match(trimmed);

    // Fast structural check before burning an LLM token
    if looks_like_raw_tool {
        return ValidationDecision::Retry {
            message: "The response contains raw JSON data or hallucinated tool commands. \
                      If you meant to use a tool, please call it natively using the system's tool format. \
                      If you are responding to the user, summarize the information in clear, natural human-readable plain text.".to_string(),
            reason: RetryReason::HallucinatedToolCall,
        };
    }

    // Scope the (LLM-backed) quality check by action risk. On a free-tier model
    // pool, running a second model on every read-only lookup mostly burns rate
    // limits and latency. `agent.quality_check_mode`:
    //   "all"      — audit every tool-backed answer (legacy behavior)
    //   "mutating" — audit only when the stakes are real: a successful
    //                state-changing action, a routed-but-unused tool (likely a
    //                false refusal), or a blank/hallucinated-success response.
    //                Plain successful reads skip the LLM call (the zero-cost
    //                claim/structural/service-mismatch guards above still run).
    //   "off"      — never (same as agent.quality_check = false)
    let any_mutating_success = tool_receipts.iter().any(|r| r.ok && r.is_mutating);
    let qc_mode = settings.get_str("agent.quality_check_mode", "mutating");
    let qc_scope_ok = match qc_mode.as_str() {
        "off" => false,
        "all" => true,
        _ => any_mutating_success || tools_routed_but_unused || is_blank,
    };

    // One LLM-judged correction per run. The deterministic guards (claim
    // guard, plan check, structural checks) carry the load; with a capable
    // model on complex turns, repeated LLM re-judging mostly burned tokens
    // and oscillated between phrasings.
    let should_qc = qc_enabled
        && qc_scope_ok
        && (!tools_used.is_empty() || is_blank || tools_routed_but_unused)
        && !is_subtask
        && !is_completion_confirm
        && guard_counts.qc_correction_count < 1;

    if should_qc {
        let tool_evidence = extract_tool_evidence(messages);
        if let Some(correction) = quality_check(task, text, &tool_evidence, router, settings).await
        {
            guard_counts.qc_correction_count += 1;
            return ValidationDecision::Retry {
                message: format!(
                    "QUALITY CHECK CORRECTION — please revise your response:\n{}\n\n\
                     Rewrite your response addressing the issues above. \
                     You may use tools to gather correct information if needed, but DO NOT make up inaccurate information.",
                    correction
                ),
                reason: RetryReason::QualityCheck,
            };
        }
    }

    ValidationDecision::Pass
}

// ── Public entry points ──────────────────────────────────────────────────────

pub async fn run_task(task: &str, state: &AppState, ctx: RunContext) -> anyhow::Result<String> {
    let run_id = ctx.run_id.clone();
    let sink = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let result = crate::router::model_router::RUN_TOKEN_SINK
        .scope(sink, run_inner(task, state, ctx, None))
        .await;
    if result.is_err() {
        mark_run_failed_if_running(state, &run_id);
    }
    result
}

pub async fn run_task_streaming(
    task: &str,
    state: &AppState,
    ctx: RunContext,
    tx: mpsc::Sender<AgentEvent>,
) -> anyhow::Result<String> {
    let run_id = ctx.run_id.clone();
    let sink = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let result = crate::router::model_router::RUN_TOKEN_SINK
        .scope(sink, run_inner(task, state, ctx, Some(tx)))
        .await;
    if result.is_err() {
        mark_run_failed_if_running(state, &run_id);
    }
    result
}

fn mark_run_failed_if_running(state: &AppState, run_id: &str) {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE runs SET status='failed', result='Agent task terminated unexpectedly', finished_at=datetime('now') WHERE id=?1 AND status='running'",
            rusqlite::params![run_id],
        );
    }
}

pub(crate) fn strip_router_alert_footer(text: &str) -> String {
    let markers = [
        "\n---\n*Router alerts during this run:*",
        "*Router alerts during this run:*",
        "Router alerts during this run:",
    ];
    for marker in markers {
        if let Some(idx) = text.find(marker) {
            return text[..idx].trim_end().to_string();
        }
    }
    text.to_string()
}

// ── Main agent loop ──────────────────────────────────────────────────────────

pub(crate) async fn run_inner(
    task: &str,
    state: &AppState,
    ctx: RunContext,
    tx: Option<mpsc::Sender<AgentEvent>>,
) -> anyhow::Result<String> {
    let run_id = ctx.run_id.clone();

    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "INSERT INTO runs (id,task,status,platform,session_id,job_id,parent_run_id) VALUES (?1,?2,'running',?3,?4,?5,?6)",
            rusqlite::params![run_id, task, ctx.platform, ctx.session_id, ctx.job_id, ctx.parent_run_id],
        );
    }

    macro_rules! emit {
        ($e:expr) => {
            if let Some(ref t) = tx {
                let _ = t.send($e).await;
            }
        };
    }

    let run_start = tokio::time::Instant::now();
    let run_timeout_secs = state.settings.get_int("agent.run_timeout_secs", 300) as u64;
    let run_deadline = run_start + tokio::time::Duration::from_secs(run_timeout_secs);

    // Derive a stable per-run base seed from run_id.
    // UUID v4 is already random; we just need a deterministic 64-bit mixer.
    let base_route_seed = stable_route_seed(&run_id);

    emit!(AgentEvent::Thinking {
        run_id: run_id.clone(),
        text: "Analyzing request...".into()
    });

    let max_iter = state.settings.max_iterations();
    let max_par = state.settings.max_parallel_tools();
    let allow_write = state.settings.allow_tool_writing();
    let write_retries = state.settings.temp_tool_max_retries();
    let qc_enabled = state.settings.get_bool("agent.quality_check", true);
    // Global correction budget — a single ceiling across ALL retry reasons
    // (claim guard, refusal nudge, blank, hallucinated tool syntax, quality
    // check). Prevents pathological correction loops/oscillation that the
    // per-reason caps don't cover; bounded well under max_iter so we exit
    // gracefully with a best-effort answer instead of churning to timeout.
    let max_corrections = state.settings.get_int("agent.max_corrections", 6) as u32;
    // Hard ceiling on cumulative tokens (input+output) for the whole run. The
    // dominant cost unit is tokens, not iterations — this caps spend even when a
    // few iterations carry huge contexts. Off by default (0).
    let max_total_tokens = state.settings.get_int("agent.max_total_tokens", 0) as u32;
    // Per-run budget for background observation compression. Each compression is
    // an LLM call; capping them bounds the most invisible recurring cost. Master
    // switch agent.compress_observations (default on) and cap
    // agent.max_observations_per_run (default 4). Shared atomic so the cap holds
    // across the parallel internal + sequential external tool paths.
    let obs_budget = {
        let on = state.settings.get_bool("agent.compress_observations", true);
        let cap = state
            .settings
            .get_int("agent.max_observations_per_run", 4)
            .max(0) as u32;
        Arc::new(std::sync::atomic::AtomicU32::new(if on { cap } else { 0 }))
    };
    let memory_enabled = ctx.memory_enabled;
    // Tool scope: "all" (default) exposes every enabled tool to the model on
    // every iteration — the model, not a pre-filter, decides what to use.
    // "routed" restores the legacy regex/embedding/LLM router as a rollback path.
    let tool_scope_all = state.settings.get_str("agent.tool_scope", "all") == "all";
    // Char budget for tool results kept in the model's context; oldest complete
    // tool exchanges are dropped first (see trim_tool_results_by_budget).
    let tool_result_budget = state
        .settings
        .get_int("agent.tool_result_budget_chars", 100_000)
        .max(1_000) as usize;
    let task_is_bulk = is_bulk_task(task);
    let is_conversational = crate::router::tool_router::CONVERSATIONAL.is_match(task);
    let needs_time_context = task_needs_time_context(task);

    let mut base_system = ctx
        .system_prompt
        .clone()
        .unwrap_or_else(|| state.settings.system_prompt());
    if needs_time_context {
        let offset = state.settings.agent_utc_offset();
        let now_local = chrono::Utc::now().with_timezone(&offset);
        base_system = format!(
            "{}\n\n[SYSTEM CLOCK: {}]\n* This is the exact current date & time (operator-local, UTC{}).\n* Use this internally to accurately calculate relative dates (e.g., today, tomorrow, next week, scheduling).\n* Do NOT mention the current time/date to the user unless they asked for it or the task genuinely requires it.",
            base_system,
            now_local.format("%A, %Y-%m-%d %H:%M:%S"),
            offset
        );
    }

    let RunSystemContext {
        sys,
        mut messages,
        filtered_initial,
        tier_initial,
    } = build_run_context(task, state, &ctx, &base_system, needs_time_context).await;

    let mut iters = 0u32;
    let mut tokens = 0u32;
    let mut models_used: Vec<String> = vec![];
    let mut tools_used: Vec<String> = vec![];
    let mut tool_receipts: Vec<ToolExecutionReceipt> = vec![];
    let mut guard_counts = GuardCounts::default();
    // Total corrections issued this run, across every retry reason. Backs the
    // global correction budget (`max_corrections`).
    let mut total_corrections = 0u32;
    let mut token_emitted = false;
    // Track the last model that responded successfully so we can prefer it
    // on the next iteration (sticky routing). Cleared when validation forces
    // a correction — a different model may handle the correction better.
    let mut last_model: Option<String> = None;
    // Consecutive correction counter for exponential backoff.
    #[allow(unused_assignments)]
    let mut consecutive_corrections: u32 = 0;
    // Set after a false-refusal nudge so the *next* model call forces tool use
    // (tool_choice=Required) rather than re-pleading in the prompt. Consumed and
    // cleared at the top of the next iteration.
    let mut force_tool_use_next = false;
    // Stall detection. A model stuck re-issuing the *same* tool call(s) (e.g. the
    // same failing job with identical args) would otherwise churn every remaining
    // iteration and exit via the max-iteration/timeout backstop as a hard failure.
    // Instead we cap identical consecutive tool batches: once the same signature
    // repeats `max_repeated_tool_calls` times we disable tools for one turn and
    // ask the model for a best-effort final answer. 0 disables the guard.
    let max_repeated_tool_calls = state
        .settings
        .get_int("agent.max_repeated_tool_calls", 3)
        .max(0) as u32;
    let mut last_tool_sig: Option<u64> = None;
    let mut repeated_tool_calls: u32 = 0;
    let mut force_final_answer = false;
    let final_text;

    'agent: loop {
        iters += 1;
        let iter_start = std::time::Instant::now();

        // ── Guard: run deadline ──────────────────────────────────────────────
        if tokio::time::Instant::now() >= run_deadline {
            let alerts = drain_alerts(&state.router).await;
            dispatch_router_alert_notifications(
                &alerts,
                state,
                tx.as_ref(),
                &run_id,
                &ctx.platform,
                ctx.chat_id.as_deref(),
                "Run timeout while waiting on model routing",
            )
            .await;
            let msg = format!("Run timeout reached after {}s", run_timeout_secs);
            dispatch_global_error_notification_event(
                state,
                tx.as_ref(),
                &run_id,
                &ctx.platform,
                ctx.chat_id.as_deref(),
                "Agent run timeout",
                &msg,
            )
            .await;
            emit!(AgentEvent::Error {
                run_id: run_id.clone(),
                message: msg.clone()
            });
            finalize(
                state,
                &run_id,
                "failed",
                &msg,
                iters,
                tokens,
                &models_used,
                &tools_used,
                &guard_counts,
            );
            return Err(anyhow::anyhow!(msg));
        }

        // ── Guard: max iterations ────────────────────────────────────────────
        if iters as i64 > max_iter {
            let alerts = drain_alerts(&state.router).await;
            dispatch_router_alert_notifications(
                &alerts,
                state,
                tx.as_ref(),
                &run_id,
                &ctx.platform,
                ctx.chat_id.as_deref(),
                "Agent max-iteration guard triggered",
            )
            .await;
            let msg = format!("Max iterations ({}) reached", max_iter);
            dispatch_global_error_notification_event(
                state,
                tx.as_ref(),
                &run_id,
                &ctx.platform,
                ctx.chat_id.as_deref(),
                "Agent max iterations reached",
                &msg,
            )
            .await;
            emit!(AgentEvent::Error {
                run_id: run_id.clone(),
                message: msg.clone()
            });
            finalize(
                state,
                &run_id,
                "failed",
                &msg,
                iters,
                tokens,
                &models_used,
                &tools_used,
                &guard_counts,
            );
            return Err(anyhow::anyhow!(msg));
        }

        emit!(AgentEvent::Thinking {
            run_id: run_id.clone(),
            text: format!("Iteration {}/{}", iters, max_iter)
        });

        // ── Tool routing ─────────────────────────────────────────────────────
        let all_tools = state.tools.all_enabled_for_agent().await;

        let (filtered, _tier, tool_names) = if force_final_answer {
            // Stall guard tripped last iteration: disable tools so the model must
            // compose a plain-text best-effort answer instead of retrying.
            force_final_answer = false;
            (
                Vec::<ToolDefinition>::new(),
                "final".to_string(),
                Vec::<String>::new(),
            )
        } else if tool_scope_all && ctx.allowed_tools.is_none() {
            // Full scope: every enabled tool, deterministically sorted so the
            // provider-side prompt cache sees a stable prefix. No Tools UI
            // event here — ToolStart events already show live usage, and a
            // full-registry name list per iteration is noise. Conversational
            // turns stay tool-free (cheap small-talk path).
            if is_conversational {
                (
                    Vec::<ToolDefinition>::new(),
                    "conversational".to_string(),
                    Vec::<String>::new(),
                )
            } else {
                let mut f = all_tools.clone();
                f.sort_by(|a, b| a.name.cmp(&b.name));
                let names: Vec<String> = f.iter().map(|t| t.name.clone()).collect();
                (f, "all".to_string(), names)
            }
        } else if iters == 1 {
            let names: Vec<String> = filtered_initial.iter().map(|t| t.name.clone()).collect();
            if !names.is_empty() {
                emit!(AgentEvent::Tools {
                    run_id: run_id.clone(),
                    tools: names.clone(),
                    tier: tier_initial.clone(),
                    parallel: names.len() > 1
                });
            }
            (filtered_initial.clone(), tier_initial.clone(), names)
        } else {
            let (mut f, info) = if let Some(ref allowed) = ctx.allowed_tools {
                let filtered: Vec<_> = all_tools
                    .iter()
                    .filter(|t| allowed.contains(&t.name))
                    .cloned()
                    .collect();
                let mut route_info = serde_json::Map::new();
                route_info.insert(
                    "tier".to_string(),
                    serde_json::Value::String("manual".to_string()),
                );
                (filtered, serde_json::Value::Object(route_info))
            } else {
                state
                    .tool_router
                    .filter_tools(task, &all_tools, &messages)
                    .await
            };

            // Suppress already-called read tools on bulk tasks
            {
                let already_called: std::collections::HashSet<String> = messages
                    .iter()
                    .filter(|m| m.role == "assistant")
                    .flat_map(|m| {
                        if let MessageContent::Blocks(blocks) = &m.content {
                            blocks
                                .iter()
                                .filter_map(|b| {
                                    if let ContentBlock::ToolUse { name, .. } = b {
                                        Some(name.clone())
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                        } else {
                            vec![]
                        }
                    })
                    .collect();
                let read_suffixes = ["_list", "_list_events", "_get", "_search", "_get_freebusy"];
                if task_is_bulk {
                    f.retain(|t| {
                        !(already_called.contains(&t.name)
                            && read_suffixes.iter().any(|s| t.name.ends_with(s)))
                    });
                }
            }

            let names: Vec<String> = f.iter().map(|t| t.name.clone()).collect();
            let t = info
                .get("tier")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            if !names.is_empty() {
                emit!(AgentEvent::Tools {
                    run_id: run_id.clone(),
                    tools: names.clone(),
                    tier: t.clone(),
                    parallel: names.len() > 1
                });
            }
            (f, t, names)
        };

        trim_tool_results_by_budget(&mut messages, tool_result_budget);

        // ── Model selection & call ───────────────────────────────────────────

        // Per-iteration seed: add a golden-ratio-mixed iteration delta to the
        // base seed. This gives each iteration a distinct, uniform
        // starting offset in the model pool without shared mutable state.
        // 0x9e3779b97f4a7c15 is the 64-bit Fibonacci hashing constant.
        let iter_seed =
            base_route_seed.wrapping_add((iters as usize).wrapping_mul(0x9e3779b97f4a7c15_usize));

        // Task-phase-aware role selection.
        // The phase is derived from observable run state — no extra config needed.
        //
        // simple_tasks   → fast cheap model: conversational, no tools, first attempt
        // complex_tasks  → capable model: any tool use, corrections, long runs
        // ""             → general pool: unclassified, let the router decide
        let preferred_role = if filtered.is_empty()
            && is_conversational
            && iters == 1
            && guard_counts.qc_correction_count == 0
        {
            "simple_tasks"
        } else if !filtered.is_empty()
            || iters > 1
            || guard_counts.qc_correction_count > 0
            || guard_counts.claim_guard_count > 0
            || guard_counts.nudge_count > 0
        {
            // Corrections and tool-using iterations always deserve the most capable model.
            "complex_tasks"
        } else {
            ""
        };

        let model_phase = if !filtered.is_empty() && tools_used.is_empty() {
            "Planning next step..."
        } else if !filtered.is_empty() {
            "Reviewing tool results..."
        } else if guard_counts.qc_correction_count > 0 || guard_counts.claim_guard_count > 0 {
            "Refining response..."
        } else if iters > 1 {
            "Refining response..."
        } else {
            "Drafting response..."
        };
        emit!(AgentEvent::Thinking {
            run_id: run_id.clone(),
            text: model_phase.into()
        });

        let model_wait_heartbeat =
            spawn_model_wait_heartbeat(tx.clone(), run_id.clone(), !filtered.is_empty());

        // Low sampling temperature reduces hallucinated tool syntax and
        // correction oscillation. Configurable; default 0.3.
        let temperature = state.settings.get_f64("agent.temperature", 0.3) as f32;
        // Reasoning is ON by default ("medium"). Providers that reject the
        // field degrade gracefully: openai_compat strips it on a 400 and
        // remembers per-model; anthropic sends a thinking param only when the
        // model's thinking_mode opts in. Applied only on the capable/correction
        // phase where deeper planning pays off. Set "off" to disable.
        let reasoning_cfg = state.settings.get_str("agent.reasoning_effort", "medium");
        let reasoning_effort = if !reasoning_cfg.is_empty()
            && reasoning_cfg != "off"
            && preferred_role == "complex_tasks"
        {
            Some(reasoning_cfg)
        } else {
            None
        };
        // After a false refusal, force the model to actually call a tool instead
        // of re-pleading via prompt — but only when tools are available.
        let tool_choice = if force_tool_use_next && !filtered.is_empty() {
            Some(crate::providers::ToolChoice::Required)
        } else {
            None
        };
        force_tool_use_next = false;

        let llm_result = call_llm_with_options(
            &messages,
            &sys,
            &filtered,
            None,
            preferred_role,
            Arc::clone(&state.router),
            &state.settings,
            CallLlmOptions {
                preferred_model_name: ctx.preferred_model.clone(),
                // Sticky: prefer the model that worked last iteration.
                // `last_model` is already cleared to `None` by every correction
                // path (QC, claim-guard, blank, hallucinated, blocked-repair),
                // so no additional guard_counts gate is needed here.
                sticky_model_name: last_model.clone(),
                // Per-iteration failover is bounded only by the overall run
                // deadline — each attempt uses a flat per-model timeout and we
                // move on immediately, so the chain can sweep the whole pool
                // (down to the paid fallback) without an artificial budget cap.
                deadline: Some(run_deadline),
                route_seed: Some(iter_seed),
                temperature: Some(temperature),
                tool_choice,
                reasoning_effort,
                ..CallLlmOptions::default()
            },
        )
        .await;

        if let Some(handle) = model_wait_heartbeat {
            handle.abort();
        }

        let (mut response, model_name, tier) = match llm_result {
            Ok(v) => v,
            Err(e) => {
                let alerts = drain_alerts(&state.router).await;
                dispatch_router_alert_notifications(
                    &alerts,
                    state,
                    tx.as_ref(),
                    &run_id,
                    &ctx.platform,
                    ctx.chat_id.as_deref(),
                    "All model routes failed",
                )
                .await;
                let msg = format!("All models exhausted: {}", e);
                dispatch_global_error_notification_event(
                    state,
                    tx.as_ref(),
                    &run_id,
                    &ctx.platform,
                    ctx.chat_id.as_deref(),
                    "All model routes exhausted",
                    &msg,
                )
                .await;
                emit!(AgentEvent::Error {
                    run_id: run_id.clone(),
                    message: msg.clone()
                });
                finalize(
                    state,
                    &run_id,
                    "failed",
                    &msg,
                    iters,
                    tokens,
                    &models_used,
                    &tools_used,
                    &guard_counts,
                );
                return Err(anyhow::anyhow!(msg));
            }
        };

        tokens += response.usage.total();
        // Fold accumulated auxiliary (tool-router + quality-gate) tokens into the
        // run total so reported cost reflects real spend, not just main calls.
        tokens += crate::router::model_router::RUN_TOKEN_SINK
            .try_with(|s| s.swap(0, std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0) as u32;
        if !models_used.contains(&model_name) {
            models_used.push(model_name.clone());
        }
        // Record for sticky routing on the next iteration.
        last_model = Some(model_name.clone());
        // Reset correction counter on successful model response.
        consecutive_corrections = 0;
        emit!(AgentEvent::Model {
            run_id: run_id.clone(),
            model: model_name.clone(),
            iteration: iters,
            duration_ms: iter_start.elapsed().as_millis() as u64
        });

        let mut text = response.text_content();
        text = strip_router_alert_footer(&text);

        // ── Hallucinated tool call repair ────────────────────────────────────
        if !response.has_tool_calls()
            && (text.contains('{')
                && (text.contains("\"name\"")
                    || text.contains("\"action\"")
                    || text.contains("\"tool\"")
                    || text.contains("\"api_name\""))
                || RE_TOOL_TEXT.is_match(&text)
                || text.contains("<tool_call>")
                || RE_CALL_COLON.is_match(&text))
        {
            match repair_tool_call(&text, response.clone(), &all_tools) {
                RepairDecision::Repaired(repaired) => {
                    response = repaired;
                    text = response.text_content();
                }
                RepairDecision::Blocked(reason) => {
                    emit!(AgentEvent::Thinking {
                        run_id: run_id.clone(),
                        text: "Blocking hallucinated text tool call".into()
                    });
                    messages.push(Message::user(&format!(
                        "TOOL EXECUTION GUARD — your previous response attempted to express a tool call in plain text instead of using the native tool-calling API.\n{}\n\nUse the native tool-calling mechanism only. Do not print JSON, XML, call:tool{{...}}, or Tool:/Parameters: syntax in the message body.",
                        reason
                    )));
                    // Force a fresh model pick on the correction turn.
                    last_model = None;
                    continue 'agent;
                }
                RepairDecision::None => {}
            }
        }

        // Persist assistant turn to message history
        if text.trim().is_empty() && !response.has_tool_calls() {
            messages.push(Message::assistant_with_blocks(vec![ContentBlock::Text {
                text: "(blank response)".to_string(),
            }]));
        } else {
            messages.push(Message::assistant_with_blocks(response.content.clone()));
        }

        if let Ok(conn) = state.db.get() {
            let _ = conn.execute(
                "INSERT INTO run_iterations (id, run_id, iteration, model_name, tokens, tier, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), run_id, iters, model_name.clone(), response.usage.total(), tier.clone(), iter_start.elapsed().as_millis() as i64]
            );
        }

        // ── Guard: per-task token budget ─────────────────────────────────────
        // When cumulative spend trips the ceiling, stop and return the best
        // answer so far with an honest caveat rather than continuing to pay.
        if max_total_tokens > 0 && tokens >= max_total_tokens {
            emit!(AgentEvent::Thinking {
                run_id: run_id.clone(),
                text: "Token budget reached — returning best-effort response".into()
            });
            let mut clean = strip_reasoning(&text);
            if tools_used.is_empty() {
                clean = clean
                    .replace("**", "")
                    .replace("__", "")
                    .replace("###", "")
                    .replace("##", "")
                    .replace("# ", "")
                    .replace('`', "");
            }
            let mut final_output = strip_router_alert_footer(clean.trim());
            if final_output.is_empty() {
                final_output = "I reached my token budget for this request before fully finishing. Please narrow the request or try again.".to_string();
            } else {
                final_output.push_str("\n\n(Note: I reached my token budget for this request, so this is my best-effort answer and some steps may be incomplete.)");
            }
            if !token_emitted {
                emit!(AgentEvent::Token {
                    run_id: run_id.clone(),
                    text: final_output.clone()
                });
            }
            let alerts = drain_alerts(&state.router).await;
            dispatch_router_alert_notifications(
                &alerts,
                state,
                tx.as_ref(),
                &run_id,
                &ctx.platform,
                ctx.chat_id.as_deref(),
                "Model/router alert during run",
            )
            .await;
            final_text = final_output.clone();
            emit!(AgentEvent::Done {
                run_id: run_id.clone(),
                full_text: final_output,
                total_tokens: tokens,
                iterations: iters,
                total_duration_ms: run_start.elapsed().as_millis() as u64,
            });
            finalize(
                state,
                &run_id,
                "completed",
                &final_text,
                iters,
                tokens,
                &models_used,
                &tools_used,
                &guard_counts,
            );
            break 'agent;
        }

        // ── No tool calls → validate and maybe finalize ──────────────────────
        if !response.has_tool_calls() {
            let is_subtask = ctx.depth > 0;

            // Single-pass validation — the only place all guards live
            let decision = validate_response(
                &text,
                &tool_names,
                &tool_receipts,
                &tools_used,
                &mut guard_counts,
                iters,
                task,
                &messages,
                task_is_bulk,
                Arc::clone(&state.router),
                &state.settings,
                qc_enabled,
                is_subtask,
                &run_id,
            )
            .await;

            match decision {
                ValidationDecision::Retry { message, reason } => {
                    // Global correction budget: stop retrying once we've issued
                    // `max_corrections` total corrections across all reasons.
                    // Return the best answer so far with an honest caveat rather
                    // than looping to the iteration/timeout backstop or
                    // oscillating between correction types.
                    total_corrections += 1;
                    if total_corrections > max_corrections {
                        // Fold this turn's quality-gate tokens before exiting.
                        tokens += crate::router::model_router::RUN_TOKEN_SINK
                            .try_with(|s| s.swap(0, std::sync::atomic::Ordering::Relaxed))
                            .unwrap_or(0) as u32;
                        emit!(AgentEvent::Thinking {
                            run_id: run_id.clone(),
                            text: "Correction budget reached — returning best-effort response"
                                .into()
                        });
                        let mut clean = strip_reasoning(&text);
                        if tools_used.is_empty() {
                            clean = clean
                                .replace("**", "")
                                .replace("__", "")
                                .replace("###", "")
                                .replace("##", "")
                                .replace("# ", "")
                                .replace('`', "");
                        }
                        let mut final_output = strip_router_alert_footer(clean.trim());
                        if final_output.is_empty() {
                            final_output = "I wasn't able to fully verify this request after several attempts. Please rephrase or check the logs and try again.".to_string();
                        } else {
                            final_output.push_str("\n\n(Note: I reached my self-correction limit, so this is my best-effort answer and some details may be unverified.)");
                        }

                        if !token_emitted {
                            emit!(AgentEvent::Token {
                                run_id: run_id.clone(),
                                text: final_output.clone()
                            });
                        }

                        let alerts = drain_alerts(&state.router).await;
                        dispatch_router_alert_notifications(
                            &alerts,
                            state,
                            tx.as_ref(),
                            &run_id,
                            &ctx.platform,
                            ctx.chat_id.as_deref(),
                            "Model/router alert during run",
                        )
                        .await;

                        final_text = final_output.clone();
                        emit!(AgentEvent::Done {
                            run_id: run_id.clone(),
                            full_text: final_output,
                            total_tokens: tokens,
                            iterations: iters,
                            total_duration_ms: run_start.elapsed().as_millis() as u64,
                        });
                        finalize(
                            state,
                            &run_id,
                            "completed",
                            &final_text,
                            iters,
                            tokens,
                            &models_used,
                            &tools_used,
                            &guard_counts,
                        );
                        break 'agent;
                    }

                    emit!(AgentEvent::Thinking {
                        run_id: run_id.clone(),
                        text: match reason {
                            RetryReason::Refusal =>
                                format!("Nudging model to use tools: [{}]", tool_names.join(", ")),
                            RetryReason::ClaimGuard =>
                                "Validating tool-backed claims against execution receipts".into(),
                            RetryReason::QualityCheck =>
                                "Quality issue found, refining response...".into(),
                            RetryReason::BlankResponse =>
                                "Response was blank, requesting retry".into(),
                            RetryReason::HallucinatedToolCall =>
                                "Blocking raw tool syntax in response".into(),
                            RetryReason::PlanIncomplete =>
                                "Plan has open steps — asking the model to finish or account for them".into(),
                        }
                    });
                    messages.push(Message::user(&message));

                    // A false refusal means the model has the tools but declined.
                    // Force a real tool call on the retry instead of re-asking.
                    force_tool_use_next = matches!(reason, RetryReason::Refusal);

                    // Clear sticky preference on QC/claim-guard corrections so the
                    // router can pick a fresh model. A model that produced a bad
                    // response shouldn't be the first tried on the correction turn.
                    if matches!(
                        reason,
                        RetryReason::QualityCheck
                            | RetryReason::ClaimGuard
                            | RetryReason::BlankResponse
                            | RetryReason::HallucinatedToolCall
                    ) {
                        last_model = None;
                    }

                    // Exponential backoff on correction-triggered retries to reduce
                    // rate-limit pressure. 400ms × 2^(n-1), capped at 3200ms.
                    // Does not apply on the first nudge (no tools called yet).
                    if reason != RetryReason::Refusal || guard_counts.nudge_count > 1 {
                        consecutive_corrections += 1;
                        let backoff_ms = (400u64).saturating_mul(
                            1u64 << consecutive_corrections.saturating_sub(1).min(3),
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                    }

                    continue 'agent;
                }
                ValidationDecision::Pass => {
                    // Fold this turn's quality-gate tokens (runs after the main
                    // fold above) so the final total is complete.
                    tokens += crate::router::model_router::RUN_TOKEN_SINK
                        .try_with(|s| s.swap(0, std::sync::atomic::Ordering::Relaxed))
                        .unwrap_or(0) as u32;
                    // Memory write (only for useful, tool-backed results)
                    if memory_enabled {
                        match ctx.memory_window {
                            Some(window) => {
                                let _ = state.memory.add_assistant_capped(
                                    &ctx.session_id,
                                    &text,
                                    window,
                                );
                            }
                            None => {
                                let _ = state.memory.add_assistant(&ctx.session_id, &text);
                            }
                        }
                        let text_lower = text.to_lowercase();
                        let is_useful = text.len() > 100
                            && !text_lower.contains("i'm unable")
                            && !text_lower.contains("i cannot")
                            && !text_lower.contains("i don't have access")
                            && !text_lower.starts_with("i'm sorry")
                            && !text_lower.contains("all models exhausted")
                            && !text_lower.contains("max iterations")
                            && !text_lower.contains("router alert")
                            && !tools_used.is_empty();
                        // Isolated runs (Axon nodes) never write to the shared
                        // long-term store — their memory stays node-local.
                        if is_useful && !ctx.isolated_memory {
                            let _ = state
                                .memory
                                .remember(
                                    &format!(
                                        "Task: {task}\nResult: {}",
                                        text.chars().take(500).collect::<String>()
                                    ),
                                    "task_result",
                                    &[],
                                )
                                .await;
                        }
                    }

                    // Clean and finalize output
                    let mut clean = strip_reasoning(&text);
                    if tools_used.is_empty() {
                        clean = clean
                            .replace("**", "")
                            .replace("__", "")
                            .replace("###", "")
                            .replace("##", "")
                            .replace("# ", "")
                            .replace('`', "");
                    }
                    let mut final_output = clean.trim().to_string();
                    final_output = strip_router_alert_footer(&final_output);

                    if final_output.is_empty() {
                        final_output = "I encountered an error and couldn't produce a valid response. Please check the logs or try your request again.".to_string();
                    }

                    // Resolve <send_file> tags for dashboard
                    if ctx.platform == "dashboard" {
                        final_output = resolve_send_file_links(&final_output);
                    }

                    // Emit token (deferred until we know we're passing). Uses the
                    // fully resolved output so dashboard clients don't render raw
                    // <send_file> tags or unstripped markdown before `done` arrives.
                    if !final_output.is_empty() && !token_emitted {
                        emit!(AgentEvent::Token {
                            run_id: run_id.clone(),
                            text: final_output.clone()
                        });
                    }

                    let alerts = drain_alerts(&state.router).await;
                    dispatch_router_alert_notifications(
                        &alerts,
                        state,
                        tx.as_ref(),
                        &run_id,
                        &ctx.platform,
                        ctx.chat_id.as_deref(),
                        "Model/router alert during run",
                    )
                    .await;

                    final_text = final_output.clone();
                    emit!(AgentEvent::Done {
                        run_id: run_id.clone(),
                        full_text: final_output,
                        total_tokens: tokens,
                        iterations: iters,
                        total_duration_ms: run_start.elapsed().as_millis() as u64,
                    });
                    finalize(
                        state,
                        &run_id,
                        "completed",
                        &final_text,
                        iters,
                        tokens,
                        &models_used,
                        &tools_used,
                        &guard_counts,
                    );
                    break 'agent;
                }
            }
        }

        // ── Tool calls → execute ─────────────────────────────────────────────
        token_emitted = false;

        let mut calls = response.tool_calls();

        if tokio::time::Instant::now() >= run_deadline {
            let alerts = drain_alerts(&state.router).await;
            dispatch_router_alert_notifications(
                &alerts,
                state,
                tx.as_ref(),
                &run_id,
                &ctx.platform,
                ctx.chat_id.as_deref(),
                "Run timeout before tool execution",
            )
            .await;
            let msg = format!(
                "Run timeout reached before tool execution after {}s",
                run_timeout_secs
            );
            dispatch_global_error_notification_event(
                state,
                tx.as_ref(),
                &run_id,
                &ctx.platform,
                ctx.chat_id.as_deref(),
                "Agent tool-execution timeout",
                &msg,
            )
            .await;
            emit!(AgentEvent::Error {
                run_id: run_id.clone(),
                message: msg.clone()
            });
            finalize(
                state,
                &run_id,
                "failed",
                &msg,
                iters,
                tokens,
                &models_used,
                &tools_used,
                &guard_counts,
            );
            return Err(anyhow::anyhow!(msg));
        }

        // ── Auto-repair hallucinated tool names ──────────────────────────────
        for call in calls.iter_mut() {
            // SSH exec alias
            if call.name == "ssh_tool" || call.name == "ssh" {
                if let Some(obj) = call.input.as_object_mut() {
                    if obj.get("action").and_then(|v| v.as_str()) == Some("exec") {
                        obj.insert("action".to_string(), serde_json::json!("run"));
                    }
                }
            }
            // Name remaps from static table (never allocated in hot path)
            let name_lower = call.name.to_lowercase();
            if let Some(&(_, right)) = TOOL_NAME_REMAPS
                .iter()
                .find(|(wrong, _)| *wrong == name_lower.as_str())
            {
                tracing::info!(
                    "Auto-remapped hallucinated tool '{}' -> '{}'",
                    call.name,
                    right
                );
                call.name = right.to_string();
            }
        }

        // ── Pre-execution service mismatch correction ────────────────────────
        {
            let task_lower = task.to_lowercase();
            let mut any_fixed = false;
            for pair in crate::router::service_map::SERVICE_PAIRS {
                let a_kw = pair.a_keywords;
                let a_prefix = pair.a_prefix;
                let b_kw = pair.b_keywords;
                let b_prefix = pair.b_prefix;
                let user_wants_a = a_kw.iter().any(|kw| task_lower.contains(kw));
                let user_wants_b = b_kw.iter().any(|kw| task_lower.contains(kw));
                if user_wants_a && user_wants_b {
                    continue;
                }
                for call in calls.iter_mut() {
                    if user_wants_a && call.name.starts_with(b_prefix) {
                        let old = call.name.clone();
                        let candidate = format!("{}{}", a_prefix, &call.name[b_prefix.len()..]);
                        if all_tools.iter().any(|t| t.name == candidate) {
                            call.name = candidate;
                            tracing::warn!(
                                "Service mismatch fix: user said {:?}, remapped '{}' -> '{}'",
                                a_kw[0],
                                old,
                                call.name
                            );
                            any_fixed = true;
                        } else {
                            tracing::warn!(
                                "Service mismatch: would remap '{}' -> '{}' but target not found",
                                old,
                                candidate
                            );
                        }
                    } else if user_wants_b && call.name.starts_with(a_prefix) {
                        let old = call.name.clone();
                        let candidate = format!("{}{}", b_prefix, &call.name[a_prefix.len()..]);
                        if all_tools.iter().any(|t| t.name == candidate) {
                            call.name = candidate;
                            tracing::warn!(
                                "Service mismatch fix: user said {:?}, remapped '{}' -> '{}'",
                                b_kw[0],
                                old,
                                call.name
                            );
                            any_fixed = true;
                        } else {
                            tracing::warn!(
                                "Service mismatch: would remap '{}' -> '{}' but target not found",
                                old,
                                candidate
                            );
                        }
                    }
                }
            }
            if any_fixed {
                emit!(AgentEvent::Thinking {
                    run_id: run_id.clone(),
                    text: "Corrected service mismatch in tool call".into()
                });
            }
        }

        // ── Stall detection: identical consecutive tool batches ──────────────
        // Hash the (post-repair) requested calls. If the exact same batch repeats
        // `max_repeated_tool_calls` times, we've stopped making progress — trip the
        // guard so this turn's error guidance tells the model to give up retrying,
        // and disable tools next turn to force a best-effort answer.
        let stall_triggered = if max_repeated_tool_calls > 0 {
            let mut parts: Vec<String> = calls
                .iter()
                .map(|c| format!("{}\u{1}{}", c.name, c.input))
                .collect();
            parts.sort();
            let sig = stable_route_seed(&parts.join("\n")) as u64;
            if Some(sig) == last_tool_sig {
                repeated_tool_calls += 1;
            } else {
                last_tool_sig = Some(sig);
                repeated_tool_calls = 1;
            }
            repeated_tool_calls >= max_repeated_tool_calls
        } else {
            false
        };
        if stall_triggered {
            emit!(AgentEvent::Thinking {
                run_id: run_id.clone(),
                text: format!(
                    "Same tool call repeated {}× with no progress — disabling tools and composing a best-effort answer",
                    repeated_tool_calls
                )
            });
            // One tool-free turn next, then finalize. A fresh model may summarize
            // better than the one that kept retrying.
            force_final_answer = true;
            last_model = None;
            // Reset so the forced summary turn (and anything after) starts clean.
            repeated_tool_calls = 0;
            last_tool_sig = None;
        }

        // ── Execute internal vs external tools ───────────────────────────────
        let mut result_msgs = vec![];

        use crate::providers::types::ToolCall;
        let (internal, external): (Vec<ToolCall>, Vec<ToolCall>) =
            calls.into_iter().partition(|tc| {
                all_tools
                    .iter()
                    .find(|t| t.name == tc.name)
                    .map(|t| t.source == ToolSource::Internal)
                    .unwrap_or(false)
            });

        let is_parallel = internal.len() > 1;
        let mut futures: Vec<tokio::task::JoinHandle<(String, String, serde_json::Value)>> = vec![];

        for tc in internal {
            let tc = tc.clone();
            let state = state.clone();
            let ctx = ctx.clone();
            let run_id = run_id.clone();
            let tx_clone = tx.clone();
            let obs_budget = Arc::clone(&obs_budget);

            futures.push(tokio::spawn(async move {
                if let Some(ref t) = tx_clone {
                    let _ = t.send(AgentEvent::ToolStart { run_id: run_id.clone(), tool: tc.name.clone(), tool_call_id: tc.id.clone() }).await;
                }
                let t0 = std::time::Instant::now();
                let (ok, val) = match crate::agent::internal_tools::handle_internal(&tc.name, tc.input.clone(), state.clone(), ctx.clone(), run_id.clone()).await {
                    Ok(v) => (true, v),
                    Err(e) => (false, serde_json::json!({"error": e.to_string()})),
                };
                let duration_ms = t0.elapsed().as_millis() as u64;
                if let Some(ref t) = tx_clone {
                    let _ = t.send(AgentEvent::ToolEnd { run_id: run_id.clone(), tool: tc.name.clone(), tool_call_id: tc.id.clone(), duration_ms, ok }).await;
                }
                if let Ok(conn) = state.db.get() {
                    let _ = conn.execute(
                        "INSERT INTO tool_calls (id,run_id,tool_name,args,result,error,duration_ms,parallel) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                        rusqlite::params![uuid::Uuid::new_v4().to_string(), run_id, tc.name, serde_json::to_string(&tc.input).ok(), serde_json::to_string(&val).ok(), if ok { None } else { Some(val.to_string()) }, duration_ms as i64, is_parallel as i32]
                    );
                }
                // Isolated runs (Axon nodes) keep their tool calls node-local:
                // skip the shared observation pool so the main agent never sees
                // which tools an individual node used.
                if !ctx.isolated_memory {
                    spawn_compress(run_id.clone(), tc.name.clone(), tc.input.clone(), val.clone(), Arc::clone(&state.router), Arc::clone(&state.settings), Arc::clone(&state.db), Arc::clone(&obs_budget));
                }
                (tc.id, tc.name, val)
            }));
        }

        let joined_results = futures::future::join_all(futures).await;
        for res in joined_results {
            if let Ok((id, name, val)) = res {
                state
                    .files
                    .register_from_json(&val, Some(name.clone()))
                    .await;
                if !tools_used.contains(&name) {
                    tools_used.push(name.clone());
                }
                let has_error = val.get("error").is_some();
                tool_receipts.push(ToolExecutionReceipt {
                    name: name.clone(),
                    ok: !has_error,
                    is_mutating: receipt_is_mutating(&name, &all_tools),
                });
                if has_error {
                    let guidance = if stall_triggered {
                        "You have repeated this exact call several times without progress. Do NOT retry it. Tools are disabled for your next turn — write your best final answer to the user in plain text using what you already have, and clearly state what could not be completed and why.".to_string()
                    } else {
                        let teach = all_tools
                            .iter()
                            .find(|t| t.name == name)
                            .map(|t| format!("\n{}", t.teaching_block()))
                            .unwrap_or_default();
                        format!(
                            "Tool '{}' returned an error. Review the error message, adjust your arguments, and retry once.{}",
                            name, teach
                        )
                    };
                    let enriched = serde_json::json!({ "result": val, "guidance": guidance });
                    result_msgs.push(Message::tool_result(&id, enriched));
                } else {
                    result_msgs.push(Message::tool_result(&id, val));
                }
            }
        }

        if !external.is_empty() {
            for tc in &external {
                emit!(AgentEvent::ToolStart {
                    run_id: run_id.clone(),
                    tool: tc.name.clone(),
                    tool_call_id: tc.id.clone()
                });
            }
            let mut final_calls = vec![];
            for tc in external {
                let exists = all_tools.iter().any(|t| t.name == tc.name);
                if !exists && allow_write && is_explicit_tool_authoring_task(task) {
                    emit!(AgentEvent::Thinking {
                        run_id: run_id.clone(),
                        text: format!("Tool '{}' missing — writing it...", tc.name)
                    });
                    match write_temporary_tool(
                        &format!(
                            "Tool named '{}': {}",
                            tc.name,
                            tc.input.to_string().chars().take(200).collect::<String>()
                        ),
                        &tc.input,
                        Arc::clone(&state.router),
                        &state.settings,
                        write_retries,
                    )
                    .await
                    {
                        Ok(new_name) => {
                            if let Ok(def) = ToolDefinition::from_python_file(&format!(
                                "tools_temp/{}.py",
                                new_name
                            )) {
                                state.tools.register(def).await;
                            }
                            let mut ntc = tc.clone();
                            ntc.name = new_name;
                            final_calls.push(ntc);
                        }
                        Err(e) => {
                            tracing::warn!("Auto-write failed: {}", e);
                            final_calls.push(tc);
                        }
                    }
                } else {
                    final_calls.push(tc);
                }
            }

            // Pre-execution validation: divert unknown tools and calls missing
            // required args, answering them inline with an error that teaches —
            // the schema and an example beat a blind retry loop.
            let mut valid_calls = Vec::with_capacity(final_calls.len());
            for tc in final_calls {
                let known = all_tools.iter().any(|t| t.name == tc.name);
                if !known {
                    let suggestions =
                        crate::tools::schema::closest_tool_names(&tc.name, &all_tools, 3);
                    emit!(AgentEvent::ToolEnd {
                        run_id: run_id.clone(),
                        tool: tc.name.clone(),
                        tool_call_id: tc.id.clone(),
                        duration_ms: 0,
                        ok: false
                    });
                    tool_receipts.push(ToolExecutionReceipt {
                        name: tc.name.clone(),
                        ok: false,
                        is_mutating: receipt_is_mutating(&tc.name, &all_tools),
                    });
                    result_msgs.push(Message::tool_result(
                        &tc.id,
                        serde_json::json!({
                            "error": format!("No tool named '{}' exists.", tc.name),
                            "guidance": format!(
                                "Closest available tools: {}. Call one of these (with its own schema) or answer without a tool.",
                                suggestions.join(", ")
                            ),
                        }),
                    ));
                    continue;
                }
                let missing = missing_required_args(&tc.name, &tc.input, &all_tools);
                if missing.is_empty() {
                    valid_calls.push(tc);
                    continue;
                }
                emit!(AgentEvent::ToolEnd {
                    run_id: run_id.clone(),
                    tool: tc.name.clone(),
                    tool_call_id: tc.id.clone(),
                    duration_ms: 0,
                    ok: false
                });
                tool_receipts.push(ToolExecutionReceipt {
                    name: tc.name.clone(),
                    ok: false,
                    is_mutating: receipt_is_mutating(&tc.name, &all_tools),
                });
                let teach = all_tools
                    .iter()
                    .find(|t| t.name == tc.name)
                    .map(|t| t.teaching_block())
                    .unwrap_or_default();
                result_msgs.push(Message::tool_result(
                    &tc.id,
                    serde_json::json!({
                        "error": format!("Missing required parameter(s): {}", missing.join(", ")),
                        "guidance": format!(
                            "Re-call the tool with every required field set.\n{}",
                            teach
                        ),
                    }),
                ));
            }

            let results = run_parallel(valid_calls, Arc::new(state.tools.clone()), max_par).await;
            for res in &results {
                emit!(AgentEvent::ToolEnd {
                    run_id: run_id.clone(),
                    tool: res.tool_name.clone(),
                    tool_call_id: res.tool_use_id.clone(),
                    duration_ms: res.duration_ms,
                    ok: res.error.is_none()
                });
                if !tools_used.contains(&res.tool_name) {
                    tools_used.push(res.tool_name.clone());
                }
                tool_receipts.push(ToolExecutionReceipt {
                    name: res.tool_name.clone(),
                    ok: res.error.is_none(),
                    is_mutating: receipt_is_mutating(&res.tool_name, &all_tools),
                });
                if let Ok(conn) = state.db.get() {
                    let _ = conn.execute(
                        "INSERT INTO tool_calls (id,run_id,tool_name,args,result,error,duration_ms,parallel) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                        rusqlite::params![uuid::Uuid::new_v4().to_string(), run_id, res.tool_name,
                            serde_json::to_string(&res.input).ok(),
                            serde_json::to_string(&res.output).ok(), res.error, res.duration_ms as i64, (results.len() > 1) as i32]
                    );
                }
                // Isolated runs (Axon nodes) keep their tool calls node-local:
                // skip the shared observation pool so the main agent never sees
                // which tools an individual node used.
                if !ctx.isolated_memory {
                    spawn_compress(
                        run_id.clone(),
                        res.tool_name.clone(),
                        res.input.clone(),
                        res.output.clone(),
                        Arc::clone(&state.router),
                        Arc::clone(&state.settings),
                        Arc::clone(&state.db),
                        Arc::clone(&obs_budget),
                    );
                }

                if res.error.is_some() {
                    let guidance = if stall_triggered {
                        "You have repeated this exact call several times without progress. Do NOT retry it. Tools are disabled for your next turn — write your best final answer to the user in plain text using what you already have, and clearly state what could not be completed and why.".to_string()
                    } else {
                        let teach = all_tools
                            .iter()
                            .find(|t| t.name == res.tool_name)
                            .map(|t| format!("\n{}", t.teaching_block()))
                            .unwrap_or_default();
                        format!(
                            "Tool '{}' failed. Read the error, fix the arguments, and retry once. If it is an auth/service problem, say so instead of retrying.{}",
                            res.tool_name, teach
                        )
                    };
                    let enriched = serde_json::json!({
                        "error": res.error,
                        "output": res.output,
                        "guidance": guidance
                    });
                    result_msgs.push(Message::tool_result(&res.tool_use_id, enriched));
                } else {
                    result_msgs.push(Message::tool_result(&res.tool_use_id, res.output.clone()));
                }
            }
        }

        messages.extend(result_msgs);
    }

    Ok(final_text)
}

// ── Compression helper ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn spawn_compress(
    run_id: String,
    tool_name: String,
    tool_args: serde_json::Value,
    tool_result: serde_json::Value,
    router: crate::router::model_router::SharedRouter,
    settings: Arc<crate::config::RuntimeSettings>,
    db: Arc<r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>>,
    budget: Arc<std::sync::atomic::AtomicU32>,
) {
    tokio::spawn(async move {
        compress_and_store(
            &run_id,
            &tool_name,
            &tool_args,
            &tool_result,
            router,
            settings,
            db,
            budget,
        )
        .await;
    });
}

// ── DB helpers ───────────────────────────────────────────────────────────────

/// Write the final run record. Guard trigger counts are stored so you can
/// query how often each guard fires in production without log scraping.
fn finalize(
    state: &AppState,
    id: &str,
    status: &str,
    result: &str,
    iters: u32,
    tokens: u32,
    models: &[String],
    tools: &[String],
    guards: &GuardCounts,
) {
    // Drop run-scoped plan state on every exit path.
    crate::agent::plan::clear(id);
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE runs SET status=?1,result=?2,iterations=?3,total_tokens=?4,models_used=?5,tools_used=?6,\
             nudge_count=?7,claim_guard_count=?8,qc_correction_count=?9,finished_at=datetime('now') WHERE id=?10",
            rusqlite::params![
                status, result, iters, tokens,
                serde_json::to_string(models).ok(),
                serde_json::to_string(tools).ok(),
                guards.nudge_count,
                guards.claim_guard_count,
                guards.qc_correction_count,
                id
            ]
        );
    }
}

/// Trim oldest tool exchanges when the total character footprint of all
/// ToolResult blocks exceeds `budget_chars`, keeping the most recent context.
///
/// Correctness rule: a `tool_use` block and its matching `tool_result` are an
/// inseparable pair (matched by id). Providers (Anthropic, OpenAI) reject the
/// whole request with a 400 if either half appears without the other. So we
/// remove complete pairs by id — never a lone result, never a lone use — and
/// then drop any message whose block list became empty. Text blocks that shared
/// an assistant turn with a removed `tool_use` are preserved.
///
/// Using a character budget instead of a fixed count prevents one large tool
/// result from silently consuming the context window alongside many small ones.
fn trim_tool_results_by_budget(messages: &mut Vec<Message>, budget_chars: usize) {
    fn result_chars(messages: &[Message]) -> usize {
        messages
            .iter()
            .map(|m| {
                if let MessageContent::Blocks(blocks) = &m.content {
                    blocks
                        .iter()
                        .map(|b| match b {
                            ContentBlock::ToolResult { content, .. } => content.len(),
                            _ => 0,
                        })
                        .sum()
                } else {
                    0
                }
            })
            .sum()
    }

    let mut total = result_chars(messages);
    if total <= budget_chars {
        return;
    }

    // Weight of each tool_use_id that currently has a result present.
    let mut result_len_by_id: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for m in messages.iter() {
        if let MessageContent::Blocks(blocks) = &m.content {
            for b in blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                } = b
                {
                    *result_len_by_id.entry(tool_use_id.clone()).or_insert(0) += content.len();
                }
            }
        }
    }

    // Oldest-first list of tool_use ids that have a matching result — only these
    // can be removed as complete pairs without orphaning anything.
    let mut ordered_ids: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in messages.iter() {
        if let MessageContent::Blocks(blocks) = &m.content {
            for b in blocks {
                if let ContentBlock::ToolUse { id, .. } = b {
                    if result_len_by_id.contains_key(id) && seen.insert(id.clone()) {
                        ordered_ids.push(id.clone());
                    }
                }
            }
        }
    }

    for id in ordered_ids {
        if total <= budget_chars {
            break;
        }
        let freed = result_len_by_id.get(&id).copied().unwrap_or(0);
        for m in messages.iter_mut() {
            if let MessageContent::Blocks(blocks) = &mut m.content {
                blocks.retain(|b| {
                    !(matches!(b, ContentBlock::ToolUse { id: i, .. } if *i == id)
                        || matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if *tool_use_id == id))
                });
            }
        }
        total = total.saturating_sub(freed);
    }

    // Never send a contentless turn: drop messages whose block list is now empty.
    messages.retain(|m| !matches!(&m.content, MessageContent::Blocks(b) if b.is_empty()));
}

// ── Text cleaning ────────────────────────────────────────────────────────────

/// Rewrite `<send_file>path</send_file>` tags into authorized dashboard
/// download links. Used live before the Token/Done emission, and again when
/// the dashboard rehydrates a stored transcript (`GET /conversations/:id/messages`)
/// — the raw tag is what's persisted, so reloaded chats must resolve it too.
pub fn resolve_send_file_links(text: &str) -> String {
    let mut out = text.to_string();
    while let (Some(s), Some(e)) = (out.find("<send_file>"), out.find("</send_file>")) {
        if s >= e {
            break;
        }
        let path = out[s + 11..e].trim().to_string();
        let filename = std::path::Path::new(&path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let md_link = format!(
            "\n\n📎 **[Download {}](/api/download?path={})**\n\n",
            filename,
            urlencoding::encode(&path)
        );
        out = format!("{}{}{}", &out[..s], md_link, &out[e + 12..]);
    }
    out
}

fn strip_reasoning(text: &str) -> String {
    let mut result = RE_THINK_TAG.replace_all(text, "").to_string();
    result = RE_THINKING_BLOCK.replace_all(&result, "").to_string();
    result = RE_STRIP_TOOL_LINE.replace_all(&result, "").to_string();
    result = RE_STRIP_PARAMS_LINE.replace_all(&result, "").to_string();
    result = RE_STRIP_ACTION_LINE.replace_all(&result, "").to_string();
    result = RE_STRIP_OBSERVATION_LINE
        .replace_all(&result, "")
        .to_string();
    result = RE_STRIP_THOUGHT_LINE.replace_all(&result, "").to_string();
    result = RE_STRIP_API_NAME.replace_all(&result, "").to_string();
    result = RE_STRIP_PARAMETERS.replace_all(&result, "").to_string();
    result = RE_STRIP_CALL_COLON.replace_all(&result, "").to_string();
    result = RE_COLLAPSE_NEWLINES
        .replace_all(&result, "\n\n")
        .to_string();
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promise_only_regex_catches_deferred_work() {
        for s in [
            "Let me grab another PDF from your Google Drive for you.",
            "Sure, I'll fetch that file now.",
            "Okay — give me a moment to look that up.",
            "I'm going to check your calendar.",
        ] {
            assert!(RE_PROMISE_ONLY.is_match(s), "should match: {s}");
        }
        for s in [
            "Here you go: the file is attached.",
            "Done! The PDF has been sent.",
            "Let me know if you need anything else.",
        ] {
            // "Let me know" is excluded by the guard's contains() check, not the
            // regex itself — assert only the clear negatives here.
            if !s.to_lowercase().contains("let me know") {
                assert!(!RE_PROMISE_ONLY.is_match(s), "should not match: {s}");
            }
        }
    }

    #[test]
    fn strip_reasoning_removes_think_tags() {
        let out = strip_reasoning(
            "<think>The user wants X, so I should\ncall the tool.</think>Here is your answer.",
        );
        assert_eq!(out, "Here is your answer.");
        // Unclosed tag is left alone rather than eating the whole message.
        let out = strip_reasoning("<think>partial only");
        assert!(out.contains("partial only"));
    }

    #[test]
    fn resolve_send_file_links_builds_download_link() {
        let out =
            resolve_send_file_links("Here you go: <send_file>data/files/My File.pdf</send_file>");
        assert!(out.contains("[Download My File.pdf]"), "got: {out}");
        assert!(
            out.contains("(/api/download?path=data%2Ffiles%2FMy%20File.pdf)"),
            "got: {out}"
        );
        assert!(!out.contains("<send_file>"));
        // No tag → unchanged
        assert_eq!(resolve_send_file_links("plain text"), "plain text");
    }
}
