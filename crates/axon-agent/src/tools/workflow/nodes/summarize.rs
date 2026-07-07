//! Summarize node — Task 4.3a. A single, constrained LLM call that condenses
//! free text to a caller-chosen length/style. Thin preset over the Cortex
//! path, reusing Classifier/Extractor's isolated-session pattern. Unlike
//! those two, the output is prose (not JSON), so `expects_structured_output`
//! stays at its default (false) — there is no raw-JSON guard to defeat here.

use crate::state::AppState;
use serde_json::{json, Value};

/// Max characters of input text handed to the model — same budget as Classifier.
const MAX_INPUT_CHARS: usize = 8000;

pub(crate) fn execute<'a>(
    config: &'a Value,
    state: &'a AppState,
    workflow_id: &'a str,
    node_id: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>> {
    Box::pin(async move {
        // Input may arrive as a String (the common case) or, when an expression
        // resolves to an object/array, as structured JSON — stringify those.
        let input = match config.get("input") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        if input.is_empty() {
            return Err("Summarize node: input text is empty".to_string());
        }

        let length = config
            .get("length")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("medium");
        let style = config
            .get("style")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("paragraph");
        let focus = config
            .get("focus")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        let system = build_system_prompt(length, style, focus);
        let stimulus = format!(
            "Summarize the following text.\n\n---\n{}\n---",
            truncate(&input, MAX_INPUT_CHARS)
        );

        let selected_model = config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Per-node isolated session, mirroring Classifier, so concurrent runs
        // never cross-contaminate. Memory is off: summarization is stateless.
        let session = format!("wf:{}:node:{}", workflow_id, node_id);
        let mut ctx = crate::agent::RunContext::new(
            &stimulus,
            "workflow",
            Some(&session),
            None,
            None,
            None,
            Some(system.as_str()),
        );
        ctx.preferred_model = selected_model;
        ctx.memory_enabled = false;
        ctx.isolated_memory = true;
        ctx.allowed_tools = Some(vec![]);

        let raw = crate::agent::run_task(&stimulus, state, ctx)
            .await
            .map_err(|e| format!("Summarize agent error: {}", e))?;

        Ok(json!({ "summary": raw.trim() }))
    })
}

fn build_system_prompt(length: &str, style: &str, focus: &str) -> String {
    let mut p =
        String::from("You are a precise summarization engine. Summarize the user's text.\n\n");
    p.push_str(match length {
        "short" => "Target length: 1-2 sentences.\n",
        "long" => "Target length: 3+ paragraphs, covering all key points in depth.\n",
        _ => "Target length: one short paragraph (3-5 sentences).\n",
    });
    if style == "bullets" {
        p.push_str(
            "Format the summary as a bullet list (one point per line, starting with \"- \").\n",
        );
    } else {
        p.push_str("Format the summary as flowing prose (no bullet points).\n");
    }
    if !focus.is_empty() {
        p.push_str("Focus especially on: ");
        p.push_str(focus);
        p.push('\n');
    }
    p.push_str(
        "Output ONLY the summary text — no preamble, no \"Summary:\" label, no markdown fences.",
    );
    p
}

/// Truncate by characters (not bytes) so multibyte input never panics.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}…[truncated]", t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_system_prompt_short_paragraph_default() {
        let p = build_system_prompt("short", "paragraph", "");
        assert!(p.contains("1-2 sentences"));
        assert!(p.contains("flowing prose"));
        assert!(!p.contains("Focus especially on"));
    }

    #[test]
    fn build_system_prompt_long_bullets_with_focus() {
        let p = build_system_prompt("long", "bullets", "action items");
        assert!(p.contains("3+ paragraphs"));
        assert!(p.contains("bullet list"));
        assert!(p.contains("Focus especially on: action items"));
    }

    #[test]
    fn build_system_prompt_unknown_length_falls_back_to_medium() {
        let p = build_system_prompt("bogus", "paragraph", "");
        assert!(p.contains("one short paragraph"));
    }

    #[test]
    fn truncate_is_char_safe() {
        let s = "áéíóú";
        assert_eq!(truncate(s, 10), s);
        assert!(truncate(s, 2).starts_with("áé"));
    }

    #[test]
    fn truncate_noop_under_limit() {
        assert_eq!(truncate("short", 100), "short");
    }
}
