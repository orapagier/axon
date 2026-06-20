//! Output quality gate, split out of the agent loop.
//!
//! `extract_tool_evidence` summarizes the actual tool calls/results in a run,
//! and `quality_check` is the optional LLM-backed audit that runs after the
//! cheap deterministic guards (see `validate_response` in the loop). The two
//! shared constants live here because both the loop's structural guards and the
//! quality gate use them; the loop imports them back.

use once_cell::sync::Lazy;

use crate::providers::types::{ContentBlock, Message, MessageContent};
use crate::router::call_llm;

/// Detects the `call:tool{args}` hallucination format some weak models emit.
pub(crate) static RE_CALL_COLON: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?m)^\s*call:(\w+)\{(.+)\}\s*$").unwrap());

/// Short action-completion phrases used to skip QC on simple confirmations.
pub(crate) const COMPLETION_SIGNALS: &[&str] = &[
    "posted",
    "published",
    "sent",
    "created",
    "scheduled",
    "deleted",
    "updated",
    "uploaded",
    "moved",
    "replied",
    "forwarded",
    "marked",
    "done",
    "completed",
    "success",
];

pub(crate) fn extract_tool_evidence(messages: &[Message]) -> String {
    let mut evidence = Vec::new();
    for msg in messages {
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                match block {
                    ContentBlock::ToolUse { name, input, .. } => {
                        let args_str = serde_json::to_string(input).unwrap_or_default();
                        let args_short: String = args_str.chars().take(200).collect();
                        evidence.push(format!("[CALLED] {} ({})", name, args_short));
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        let result_str = serde_json::to_string(content).unwrap_or_default();
                        let result_short: String = result_str.chars().take(800).collect();
                        let truncated = if result_str.len() > 800 {
                            "... [truncated]"
                        } else {
                            ""
                        };
                        evidence.push(format!("[RESULT] {}{}", result_short, truncated));
                    }
                    _ => {}
                }
            }
        }
    }
    if evidence.is_empty() {
        String::new()
    } else {
        evidence.join("\n")
    }
}

/// LLM-based quality gate. Only called for responses that pass the cheaper
/// deterministic checks. Gracefully degrades to None if no model is available.
pub(crate) async fn quality_check(
    original_task: &str,
    proposed_response: &str,
    tool_evidence: &str,
    router: crate::router::model_router::SharedRouter,
    settings: &crate::config::RuntimeSettings,
) -> Option<String> {
    // Skip very short responses — confirmations, greetings
    if proposed_response.len() < 120 {
        return None;
    }

    // Fast-pass: short action-completion confirm with real tool evidence
    {
        let lower = proposed_response.to_lowercase();
        let has_completion = COMPLETION_SIGNALS.iter().any(|s| lower.contains(s));
        if has_completion && proposed_response.len() < 400 && !tool_evidence.is_empty() {
            return None;
        }
    }

    // Fast structural check (no LLM needed)
    let trimmed = proposed_response.trim();
    if trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.contains("```json")
        || trimmed.contains("``` json")
        || trimmed.contains("<tool_call>")
        || trimmed.contains("<function=")
        || RE_CALL_COLON.is_match(trimmed)
    {
        return Some(
            "The response contains raw JSON data or hallucinated tool commands. \
             If you meant to use a tool, please call it natively using the system's tool format. \
             If you are responding to the user, summarize the information in clear, natural human-readable plain text."
                .to_string(),
        );
    }

    // Fast service mismatch check
    {
        let task_lower = original_task.to_lowercase();
        let evidence_lower = tool_evidence.to_lowercase();
        for (keywords, wrong_prefix, correction) in crate::router::service_map::mismatch_rules() {
            if keywords.iter().any(|kw| task_lower.contains(kw))
                && evidence_lower.contains(wrong_prefix)
            {
                return Some(format!(
                    "SERVICE MISMATCH: The user asked for {} but you used the wrong service. \
                     Please use {}. Call the correct tool and respond with its data.",
                    keywords[0], correction
                ));
            }
        }
    }

    let prompt = format!(
        "You are a quality gate for an AI agent. Your job is to catch CRITICAL errors only.\n\n\
         REQUEST: {}\n\n\
         TOOL EVIDENCE (ground truth from actual tool calls):\n{}\n\n\
         RESPONSE:\n{}\n\n\
         Only flag the response if it has a CRITICAL problem:\n\
         1. FABRICATION: The response states facts that are NOT in the tool evidence (making up data)\n\
         2. WRONG QUESTION: The response answers a completely different question than what was asked\n\
         3. RAW DUMP: The response contains raw JSON data, ```json blocks, dictionary variables, or hallucinated tool commands instead of a natural human-readable answer\n\
         4. WRONG SERVICE: User asked for Microsoft (mscal/outlook/onedrive) but got Google (gcal/gmail/gdrive) data, or vice versa\n\
         5. FALSE REFUSAL: The response says it cannot do something (e.g. \"I cannot create events\") even though the tool evidence shows you have tools for it. You MUST use tools instead of refusing.\n\
         6. HALLUCINATED SUCCESS: Tool evidence shows \"(no tools called)\" but the response claims to have completed a task that requires tool access.\n\n\
         Do NOT flag for:\n\
         - Minor omissions or style preferences\n\
         - Slightly imperfect wording or formatting\n\
         - Missing optional details that weren't explicitly asked for\n\
         - The response being shorter than you'd prefer\n\n\
         WHEN IN DOUBT, PASS. A good-enough answer is better than a retry that might make things worse.\n\n\
         Respond with exactly: PASS\n\
         Or if there is a CRITICAL issue: one sentence describing what is critically wrong.",
        original_task,
        if tool_evidence.is_empty() { "(no tools called)" } else { tool_evidence },
        proposed_response.chars().take(1500).collect::<String>()
    );

    let sys = "You are a quality gate. Output ONLY 'PASS' or ONE sentence about a critical issue. Nothing else. When in doubt, output PASS.";

    if !crate::router::has_available_role(&router, "quality_checker").await {
        return None;
    }

    match call_llm(
        &[Message::user(&prompt)],
        sys,
        &[],
        Some(150),
        "quality_checker",
        router,
        settings,
        None,
    )
    .await
    {
        Ok((resp, model, _)) => {
            let verdict = resp.text_content().trim().to_string();
            tracing::info!(
                "Quality check [{}]: {}",
                model,
                verdict.chars().take(80).collect::<String>()
            );
            if verdict.to_uppercase().starts_with("PASS") {
                None
            } else {
                Some(verdict)
            }
        }
        Err(e) => {
            tracing::warn!("Quality check skipped (model unavailable): {}", e);
            None
        }
    }
}
