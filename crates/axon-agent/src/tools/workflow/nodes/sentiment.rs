//! Sentiment node — Task 4.3b. A single, constrained LLM call that scores the
//! polarity of free text along a caller-configurable label set. Reuses
//! Classifier's isolated-session + JSON-extraction + enum-coercion pattern
//! (see classifier.rs) so a Switch/IF node downstream can rely on `label`
//! always being one of the configured options.

use crate::state::AppState;
use serde_json::{json, Value};

const DEFAULT_LABELS: &str = "positive, negative, neutral";

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
            return Err("Sentiment node: input text is empty".to_string());
        }

        let labels = parse_list(config, "labels", DEFAULT_LABELS);

        let extra = config
            .get("instructions")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        let system = build_system_prompt(&labels, extra);
        let stimulus = format!(
            "Analyze the sentiment of the following text. Respond with ONLY the JSON object — no markdown, no prose.\n\n---\n{}\n---",
            truncate(&input, MAX_INPUT_CHARS)
        );

        let selected_model = config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Per-node isolated session, mirroring Classifier, so concurrent runs
        // never cross-contaminate. Memory is off: sentiment scoring is stateless.
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
        // raw-JSON guard would reject the answer (see classifier.rs).
        ctx.expects_structured_output = true;
        ctx.allowed_tools = Some(vec![]);

        let raw = crate::agent::run_task(&stimulus, state, ctx)
            .await
            .map_err(|e| format!("Sentiment agent error: {}", e))?;

        // Same fallback as Classifier: if the model answers in prose instead
        // of JSON, scan the text itself for a configured label rather than
        // failing the node outright.
        let (label, score, reasoning) = match extract_json(&raw) {
            Some(parsed) => {
                let label = coerce_enum(&parsed, "label", &labels);
                let score = parsed
                    .get("score")
                    .and_then(|v| v.as_f64())
                    .map(|f| f.clamp(0.0, 1.0));
                let reasoning = parsed
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                (label, score, reasoning)
            }
            None => {
                let text_lc = raw.to_ascii_lowercase();
                let label = coerce_enum_from_text(&text_lc, &labels);
                (label, None, truncate(raw.trim(), 500))
            }
        };

        Ok(json!({
            "label": label,
            "score": score,
            "reasoning": reasoning,
        }))
    })
}

/// Parse a comma-separated option list from config, falling back to `default`
/// when the field is absent or blank. Identical convention to Classifier's.
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

fn build_system_prompt(labels: &[String], extra: &str) -> String {
    let mut p = String::from(
        "You are a precise sentiment analysis engine. Score the sentiment of the \
         user's text and return a single JSON object.\n\n",
    );
    p.push_str(&format!("label: one of [{}]\n", labels.join(", ")));
    p.push_str("score: a number from 0.0 to 1.0 — your confidence in the chosen label\n");
    p.push_str("reasoning: one short sentence explaining the choice\n\n");
    if !extra.is_empty() {
        p.push_str("Additional instructions:\n");
        p.push_str(extra);
        p.push_str("\n\n");
    }
    p.push_str(
        "Rules: pick EXACTLY one value for label, copied verbatim from the allowed list. \
         Output ONLY the JSON object — no markdown fences, no commentary. \
         Example: {\"label\":\"positive\",\"score\":0.91,\"reasoning\":\"expresses clear satisfaction\"}",
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
/// surrounding prose. Identical convention to Classifier's own `extract_json`.
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

/// Map the model's raw `label` value onto one of the `allowed` options:
/// exact (case-insensitive) first, then a loose substring match, finally the
/// last option as a catch-all so the field is never empty/off-enum. Identical
/// convention to Classifier's own `coerce_enum`.
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
            (a_lc.len() >= 3 && got_lc.contains(&a_lc))
                || (got_lc.len() >= 3 && a_lc.contains(&got_lc))
        }) {
            return m.clone();
        }
    }
    allowed.last().cloned().unwrap_or_default()
}

/// Same last-resort intent as `coerce_enum`, but for when the model didn't
/// return JSON at all. Identical convention to Classifier's own
/// `coerce_enum_from_text`.
fn coerce_enum_from_text(text_lc: &str, allowed: &[String]) -> String {
    allowed
        .iter()
        .find(|a| {
            let a_lc = a.to_ascii_lowercase();
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
        let cfg = json!({ "labels": "  " });
        assert_eq!(
            parse_list(&cfg, "labels", "positive, negative"),
            vec!["positive", "negative"]
        );
        let missing = json!({});
        assert_eq!(parse_list(&missing, "labels", "x,y"), vec!["x", "y"]);
    }

    #[test]
    fn parse_list_trims_and_splits() {
        let cfg = json!({ "labels": " good , bad ,, neutral " });
        assert_eq!(
            parse_list(&cfg, "labels", "d"),
            vec!["good", "bad", "neutral"]
        );
    }

    #[test]
    fn build_system_prompt_lists_labels_and_extra() {
        let labels = vec!["positive".to_string(), "negative".to_string()];
        let p = build_system_prompt(&labels, "Treat sarcasm as negative.");
        assert!(p.contains("label: one of [positive, negative]"));
        assert!(p.contains("Treat sarcasm as negative."));
    }

    #[test]
    fn extract_json_direct() {
        let m = extract_json(r#"{"label":"positive","score":0.9}"#).unwrap();
        assert_eq!(m.get("label").unwrap(), "positive");
    }

    #[test]
    fn extract_json_from_fenced_block() {
        let raw = "Sure!\n```json\n{\"label\":\"negative\",\"reasoning\":\"angry tone\"}\n```";
        let m = extract_json(raw).unwrap();
        assert_eq!(m.get("label").unwrap(), "negative");
    }

    #[test]
    fn extract_json_none_when_absent() {
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn coerce_enum_exact_case_insensitive() {
        let allowed = vec!["Positive".to_string(), "Negative".to_string()];
        let m = map(json!({ "label": "positive" }));
        assert_eq!(coerce_enum(&m, "label", &allowed), "Positive");
    }

    #[test]
    fn coerce_enum_substring_fallback() {
        let allowed = vec!["positive".to_string(), "negative".to_string()];
        let m = map(json!({ "label": "somewhat positive overall" }));
        assert_eq!(coerce_enum(&m, "label", &allowed), "positive");
    }

    #[test]
    fn coerce_enum_last_resort_is_last_option() {
        let allowed = vec!["positive".to_string(), "negative".to_string(), "neutral".to_string()];
        let m = map(json!({}));
        assert_eq!(coerce_enum(&m, "label", &allowed), "neutral");
    }

    #[test]
    fn coerce_enum_from_text_finds_label_in_prose() {
        let allowed = vec!["positive".to_string(), "negative".to_string(), "neutral".to_string()];
        let text = "this message reads as fairly negative overall.".to_ascii_lowercase();
        assert_eq!(coerce_enum_from_text(&text, &allowed), "negative");
    }

    #[test]
    fn coerce_enum_from_text_falls_back_to_last_option() {
        let allowed = vec!["positive".to_string(), "negative".to_string(), "neutral".to_string()];
        let text = "totally unrelated text with no matching option".to_ascii_lowercase();
        assert_eq!(coerce_enum_from_text(&text, &allowed), "neutral");
    }

    #[test]
    fn truncate_is_char_safe() {
        let s = "áéíóú";
        assert_eq!(truncate(s, 10), s);
        assert!(truncate(s, 2).starts_with("áé"));
    }
}
