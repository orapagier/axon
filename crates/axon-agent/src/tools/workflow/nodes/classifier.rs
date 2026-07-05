//! Classifier node: a single, constrained LLM call that tags arbitrary text
//! along three axes — `category`, `priority`, `intent` — using user-configurable
//! enums, and emits clean structured fields for downstream IF/Switch routing.
//!
//! Composes directly with the smart Gmail trigger (which already strips
//! signatures and quoted threads): feed it `{{ trigger.email.subject }}` +
//! `body_main` and branch the workflow on the result. Unlike the general Axon
//! node, this guarantees the output value for each axis is one of the allowed
//! options (coerced/clamped here), so a Switch can rely on it.

use crate::state::AppState;
use serde_json::{json, Value};

const DEFAULT_CATEGORIES: &str = "support, sales, billing, spam, personal, other";
const DEFAULT_PRIORITIES: &str = "urgent, high, normal, low";
const DEFAULT_INTENTS: &str = "question, request, complaint, feedback, fyi, other";

/// Max characters of input text handed to the model. Keeps a runaway body
/// (newsletter, huge quoted thread) from blowing the prompt budget; the smart
/// Gmail trigger usually trims this to `body_main` already.
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
            return Err("Classifier node: input text is empty".to_string());
        }

        let categories = parse_list(config, "categories", DEFAULT_CATEGORIES);
        let priorities = parse_list(config, "priorities", DEFAULT_PRIORITIES);
        let intents = parse_list(config, "intents", DEFAULT_INTENTS);

        let extra = config
            .get("instructions")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        let system = build_system_prompt(&categories, &priorities, &intents, extra);
        let stimulus = format!(
            "Classify the following text. Respond with ONLY the JSON object — no markdown, no prose.\n\n---\n{}\n---",
            truncate(&input, MAX_INPUT_CHARS)
        );

        let selected_model = config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Per-node isolated session, mirroring the Axon node, so concurrent runs
        // never cross-contaminate. Memory is off: classification is stateless.
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
        // The node demands a bare JSON object; without this flag the loop's
        // raw-JSON guard would reject the answer and inject a rewrite-in-prose
        // correction, which the model then classifies instead of the input.
        ctx.expects_structured_output = true;
        // Empty allow-list => the agent loop filters down to zero tools, so this
        // is a single tool-free completion (see agent::loop filter at the
        // `allowed_tools` branch).
        ctx.allowed_tools = Some(vec![]);

        let raw = crate::agent::run_task(&stimulus, state, ctx)
            .await
            .map_err(|e| format!("Classifier agent error: {}", e))?;

        // Constrain each axis to its configured enum so downstream Switch/IF
        // nodes can match on a known set of values. Models occasionally ignore
        // the "JSON only" instruction and answer in prose instead — rather
        // than trust a retry to fix that, fall back to scanning the raw text
        // itself for whichever configured option it mentions, so the node
        // always produces a structured result from a single model call.
        let (category, priority, intent, confidence, reasoning) = match extract_json(&raw) {
            Some(parsed) => {
                let category = coerce_enum(&parsed, "category", &categories);
                let priority = coerce_enum(&parsed, "priority", &priorities);
                let intent = coerce_enum(&parsed, "intent", &intents);
                let confidence = parsed
                    .get("confidence")
                    .and_then(|v| v.as_f64())
                    .map(|f| f.clamp(0.0, 1.0));
                let reasoning = parsed
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (category, priority, intent, confidence, reasoning)
            }
            None => {
                let text_lc = raw.to_ascii_lowercase();
                let category = coerce_enum_from_text(&text_lc, &categories);
                let priority = coerce_enum_from_text(&text_lc, &priorities);
                let intent = coerce_enum_from_text(&text_lc, &intents);
                (category, priority, intent, None, truncate(raw.trim(), 500))
            }
        };

        Ok(json!({
            "category": category,
            "priority": priority,
            "intent": intent,
            "confidence": confidence,
            "reasoning": reasoning,
        }))
    })
}

/// Parse a comma-separated option list from config, falling back to `default`
/// when the field is absent or blank.
fn parse_list(config: &Value, key: &str, default: &str) -> Vec<String> {
    let raw = config
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default);
    let list: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if list.is_empty() {
        default.split(',').map(|s| s.trim().to_string()).collect()
    } else {
        list
    }
}

fn build_system_prompt(
    categories: &[String],
    priorities: &[String],
    intents: &[String],
    extra: &str,
) -> String {
    let mut p = String::from(
        "You are a precise text classifier. Classify the user's text along three axes \
         and return a single JSON object.\n\n",
    );
    p.push_str(&format!("category: one of [{}]\n", categories.join(", ")));
    p.push_str(&format!("priority: one of [{}]\n", priorities.join(", ")));
    p.push_str(&format!("intent: one of [{}]\n", intents.join(", ")));
    p.push_str("confidence: a number from 0.0 to 1.0\n");
    p.push_str("reasoning: one short sentence explaining the choice\n\n");
    if !extra.is_empty() {
        p.push_str("Additional instructions:\n");
        p.push_str(extra);
        p.push_str("\n\n");
    }
    p.push_str(
        "Rules: pick EXACTLY one value per axis, copied verbatim from the allowed list. \
         If unsure, choose the closest match (use a catch-all like \"other\" only as a last resort). \
         Output ONLY the JSON object — no markdown fences, no commentary. \
         Example: {\"category\":\"support\",\"priority\":\"normal\",\"intent\":\"question\",\"confidence\":0.82,\"reasoning\":\"asks how to reset password\"}",
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

/// Pull a JSON object out of a model response, tolerating markdown fences or
/// surrounding prose by falling back to the outermost `{ … }` span.
fn extract_json(raw: &str) -> Option<serde_json::Map<String, Value>> {
    if let Ok(Value::Object(m)) = serde_json::from_str::<Value>(raw.trim()) {
        return Some(m);
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    match serde_json::from_str::<Value>(&raw[start..=end]) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

/// Map the model's raw value for `key` onto one of the `allowed` options:
/// exact (case-insensitive) first, then a loose substring match, finally the
/// last option as a catch-all so the field is never empty/off-enum.
fn coerce_enum(parsed: &serde_json::Map<String, Value>, key: &str, allowed: &[String]) -> String {
    let got = parsed
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if let Some(m) = allowed.iter().find(|a| a.eq_ignore_ascii_case(got)) {
        return m.clone();
    }
    if !got.is_empty() {
        let got_lc = got.to_ascii_lowercase();
        if let Some(m) = allowed.iter().find(|a| {
            let a_lc = a.to_ascii_lowercase();
            // Require >=3 chars on the matched token: a 1-2 char option like "a"
            // is a substring of almost any text and would match spuriously.
            (a_lc.len() >= 3 && got_lc.contains(&a_lc))
                || (got_lc.len() >= 3 && a_lc.contains(&got_lc))
        }) {
            return m.clone();
        }
    }
    allowed.last().cloned().unwrap_or_default()
}

/// Same last-resort intent as `coerce_enum`, but for when the model didn't
/// return JSON at all: find the first configured option that appears
/// anywhere in the (already-lowercased) free-form text, so a plain-prose
/// reply like "this is a sales question, normal priority" still yields a
/// usable classification instead of failing the node.
fn coerce_enum_from_text(text_lc: &str, allowed: &[String]) -> String {
    allowed
        .iter()
        .find(|a| {
            let a_lc = a.to_ascii_lowercase();
            // Same >=3 char guard as coerce_enum: skip short options like "a"
            // that would match almost any text.
            a_lc.len() >= 3 && text_lc.contains(&a_lc)
        })
        .cloned()
        .unwrap_or_else(|| allowed.last().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn parse_list_defaults_when_blank() {
        let cfg = json!({ "categories": "  " });
        assert_eq!(
            parse_list(&cfg, "categories", "a, b, c"),
            vec!["a", "b", "c"]
        );
        let missing = json!({});
        assert_eq!(parse_list(&missing, "categories", "x,y"), vec!["x", "y"]);
    }

    #[test]
    fn parse_list_trims_and_splits() {
        let cfg = json!({ "p": " urgent , normal ,, low " });
        assert_eq!(parse_list(&cfg, "p", "d"), vec!["urgent", "normal", "low"]);
    }

    #[test]
    fn extract_json_direct() {
        let m = extract_json(r#"{"category":"sales"}"#).unwrap();
        assert_eq!(m.get("category").unwrap(), "sales");
    }

    #[test]
    fn extract_json_from_fenced_block() {
        let raw = "Sure!\n```json\n{\"category\":\"support\",\"priority\":\"high\"}\n```";
        let m = extract_json(raw).unwrap();
        assert_eq!(m.get("priority").unwrap(), "high");
    }

    #[test]
    fn extract_json_none_when_absent() {
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn coerce_enum_from_text_finds_each_axis_in_prose() {
        let categories = vec!["support".to_string(), "sales".to_string(), "other".to_string()];
        let priorities = vec!["urgent".to_string(), "normal".to_string(), "low".to_string()];
        let intents = vec!["question".to_string(), "complaint".to_string(), "other".to_string()];
        let text = "the text was classified as a sales-related question with normal priority."
            .to_ascii_lowercase();
        assert_eq!(coerce_enum_from_text(&text, &categories), "sales");
        assert_eq!(coerce_enum_from_text(&text, &priorities), "normal");
        assert_eq!(coerce_enum_from_text(&text, &intents), "question");
    }

    #[test]
    fn coerce_enum_from_text_falls_back_to_last_option() {
        let allowed = vec!["a".to_string(), "b".to_string(), "other".to_string()];
        let text = "totally unrelated text with no matching option".to_ascii_lowercase();
        assert_eq!(coerce_enum_from_text(&text, &allowed), "other");
    }

    #[test]
    fn coerce_enum_exact_case_insensitive() {
        let allowed = vec!["Urgent".to_string(), "Low".to_string()];
        let m = map(json!({ "priority": "urgent" }));
        assert_eq!(coerce_enum(&m, "priority", &allowed), "Urgent");
    }

    #[test]
    fn coerce_enum_substring_fallback() {
        let allowed = vec!["support".to_string(), "billing".to_string()];
        let m = map(json!({ "category": "customer support request" }));
        assert_eq!(coerce_enum(&m, "category", &allowed), "support");
    }

    #[test]
    fn coerce_enum_last_resort_is_last_option() {
        let allowed = vec!["a".to_string(), "b".to_string(), "other".to_string()];
        let m = map(json!({ "category": "totally-unrelated-zzz" }));
        assert_eq!(coerce_enum(&m, "category", &allowed), "other");
        let empty = map(json!({}));
        assert_eq!(coerce_enum(&empty, "category", &allowed), "other");
    }

    #[test]
    fn truncate_is_char_safe() {
        let s = "áéíóú";
        assert_eq!(truncate(s, 10), s);
        assert!(truncate(s, 2).starts_with("áé"));
    }
}
