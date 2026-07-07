//! Extractor (Information Extractor) — Task 4.1. A single, constrained LLM call
//! that pulls a caller-defined set of fields out of free text and returns them
//! as one JSON object. Where Classifier tags text along three fixed axes, this
//! extracts arbitrary named fields the workflow author defines per node.
//!
//! Reuses the Classifier/Cortex LLM path exactly: per-node isolated session
//! (memory off), no tools, and `expects_structured_output` so the agent loop's
//! raw-JSON guard doesn't reject the bare-JSON answer and inject a
//! rewrite-in-prose correction (see classifier.rs for the same pattern).

use crate::state::AppState;
use serde_json::{json, Map, Value};

/// Max characters of input text handed to the model — same budget as Classifier.
const MAX_INPUT_CHARS: usize = 8000;

struct Attribute {
    name: String,
    attr_type: String,
    description: String,
    required: bool,
}

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
            return Err("Extractor node: input text is empty".to_string());
        }

        let attrs = parse_attributes(config);
        if attrs.is_empty() {
            return Err(
                "Extractor node: no attributes configured — add at least one field to extract"
                    .to_string(),
            );
        }

        let extra = config
            .get("instructions")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();

        let system = build_system_prompt(&attrs, extra);
        let stimulus = format!(
            "Extract fields from the following text. Respond with ONLY the JSON object — no markdown, no prose.\n\n---\n{}\n---",
            truncate(&input, MAX_INPUT_CHARS)
        );

        let selected_model = config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Per-node isolated session, mirroring Classifier, so concurrent runs never
        // cross-contaminate. Memory is off: extraction is stateless.
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
        ctx.expects_structured_output = true;
        ctx.allowed_tools = Some(vec![]);

        let raw = crate::agent::run_task(&stimulus, state, ctx)
            .await
            .map_err(|e| format!("Extractor agent error: {}", e))?;

        // Build the output strictly from the configured attributes — a field the
        // model omits (or invents extra) never changes the node's output shape,
        // so downstream expressions like {{ $node["Extractor"].data.amount }}
        // always resolve.
        let parsed = extract_json(&raw).unwrap_or_default();
        let mut out = Map::new();
        for a in &attrs {
            let value = parsed.get(&a.name).cloned().unwrap_or(Value::Null);
            out.insert(a.name.clone(), coerce_type(value, &a.attr_type));
        }
        Ok(Value::Object(out))
    })
}

/// Parse the `attributes` fixedCollection (`{ parameters: [...] }` envelope, same
/// convention as Aggregate's `aggregations`). Rows with a blank name are dropped —
/// an unnamed field has no JSON key to land under.
fn parse_attributes(config: &Value) -> Vec<Attribute> {
    config
        .get("attributes")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let name = a
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())?;
                    Some(Attribute {
                        name: name.to_string(),
                        attr_type: a
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("string")
                            .to_string(),
                        description: a
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        required: a.get("required").and_then(|v| v.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn build_system_prompt(attrs: &[Attribute], extra: &str) -> String {
    let mut p = String::from(
        "You are a precise information extraction engine. Extract the following fields \
         from the user's text and return a single JSON object with EXACTLY these keys.\n\n",
    );
    for a in attrs {
        let req = if a.required { "required" } else { "optional" };
        if a.description.is_empty() {
            p.push_str(&format!("- {} ({}, {})\n", a.name, a.attr_type, req));
        } else {
            p.push_str(&format!(
                "- {} ({}, {}): {}\n",
                a.name, a.attr_type, req, a.description
            ));
        }
    }
    p.push('\n');
    if !extra.is_empty() {
        p.push_str("Additional instructions:\n");
        p.push_str(extra);
        p.push_str("\n\n");
    }
    p.push_str(
        "Rules: use null for any field genuinely not present in the text — never invent or \
         guess a value. Match the requested type exactly (numbers unquoted, booleans as \
         true/false, not strings). Output ONLY the JSON object — no markdown fences, no \
         commentary.",
    );
    p
}

/// Coerce the model's raw value for one attribute onto its configured JSON type.
/// A value that can't be coerced (wrong shape entirely) becomes `Null` rather than
/// smuggling a mistyped value downstream; `null` always stays `null`. `array`/
/// `object`/anything else passes through as the model returned it.
fn coerce_type(value: Value, attr_type: &str) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    match attr_type {
        "number" => match &value {
            Value::Number(_) => value,
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            _ => Value::Null,
        },
        "boolean" => match &value {
            Value::Bool(_) => value,
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Value::Bool(true),
                "false" | "no" | "0" => Value::Bool(false),
                _ => Value::Null,
            },
            _ => Value::Null,
        },
        "string" => match &value {
            Value::String(_) => value,
            Value::Number(n) => Value::String(n.to_string()),
            Value::Bool(b) => Value::String(b.to_string()),
            other => Value::String(serde_json::to_string(other).unwrap_or_default()),
        },
        _ => value,
    }
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
/// surrounding prose by falling back to the outermost `{ … }` span. Identical
/// convention to Classifier's own `extract_json`.
fn extract_json(raw: &str) -> Option<Map<String, Value>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(name: &str, t: &str, desc: &str, required: bool) -> Value {
        json!({ "name": name, "type": t, "description": desc, "required": required })
    }

    fn cfg(attrs: Vec<Value>) -> Value {
        json!({ "attributes": { "parameters": attrs } })
    }

    #[test]
    fn parse_attributes_reads_all_fields() {
        let c = cfg(vec![attr("amount", "number", "the total charged", true)]);
        let parsed = parse_attributes(&c);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "amount");
        assert_eq!(parsed[0].attr_type, "number");
        assert_eq!(parsed[0].description, "the total charged");
        assert!(parsed[0].required);
    }

    #[test]
    fn parse_attributes_drops_blank_names() {
        let c = cfg(vec![
            attr("", "string", "no name", false),
            attr("  ", "string", "blank name", false),
            attr("email", "string", "", false),
        ]);
        let parsed = parse_attributes(&c);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "email");
    }

    #[test]
    fn parse_attributes_defaults_type_to_string() {
        let c = cfg(vec![json!({ "name": "note" })]);
        let parsed = parse_attributes(&c);
        assert_eq!(parsed[0].attr_type, "string");
        assert!(!parsed[0].required);
    }

    #[test]
    fn parse_attributes_empty_when_missing() {
        assert!(parse_attributes(&json!({})).is_empty());
    }

    #[test]
    fn build_system_prompt_lists_each_attribute() {
        let attrs = vec![
            Attribute {
                name: "amount".to_string(),
                attr_type: "number".to_string(),
                description: "the total".to_string(),
                required: true,
            },
            Attribute {
                name: "note".to_string(),
                attr_type: "string".to_string(),
                description: String::new(),
                required: false,
            },
        ];
        let p = build_system_prompt(&attrs, "");
        assert!(p.contains("amount (number, required): the total"));
        assert!(p.contains("note (string, optional)"));
    }

    #[test]
    fn build_system_prompt_appends_extra_instructions() {
        let p = build_system_prompt(&[], "Treat dollar amounts as USD.");
        assert!(p.contains("Additional instructions:"));
        assert!(p.contains("Treat dollar amounts as USD."));
    }

    #[test]
    fn coerce_type_number_passthrough_and_string_parse() {
        assert_eq!(coerce_type(json!(42), "number"), json!(42));
        assert_eq!(coerce_type(json!("42.5"), "number"), json!(42.5));
        assert_eq!(coerce_type(json!("not a number"), "number"), Value::Null);
        assert_eq!(coerce_type(json!(true), "number"), Value::Null);
    }

    #[test]
    fn coerce_type_boolean_from_variants() {
        assert_eq!(coerce_type(json!(true), "boolean"), json!(true));
        assert_eq!(coerce_type(json!("yes"), "boolean"), json!(true));
        assert_eq!(coerce_type(json!("FALSE"), "boolean"), json!(false));
        assert_eq!(coerce_type(json!("0"), "boolean"), json!(false));
        assert_eq!(coerce_type(json!("maybe"), "boolean"), Value::Null);
        assert_eq!(coerce_type(json!(3), "boolean"), Value::Null);
    }

    #[test]
    fn coerce_type_string_from_variants() {
        assert_eq!(coerce_type(json!("hi"), "string"), json!("hi"));
        assert_eq!(coerce_type(json!(7), "string"), json!("7"));
        assert_eq!(coerce_type(json!(true), "string"), json!("true"));
        assert_eq!(
            coerce_type(json!([1, 2]), "string"),
            json!(serde_json::to_string(&json!([1, 2])).unwrap())
        );
    }

    #[test]
    fn coerce_type_array_and_object_pass_through() {
        assert_eq!(coerce_type(json!([1, 2, 3]), "array"), json!([1, 2, 3]));
        assert_eq!(
            coerce_type(json!({ "a": 1 }), "object"),
            json!({ "a": 1 })
        );
    }

    #[test]
    fn coerce_type_null_always_stays_null() {
        assert_eq!(coerce_type(Value::Null, "number"), Value::Null);
        assert_eq!(coerce_type(Value::Null, "string"), Value::Null);
    }

    #[test]
    fn extract_json_direct() {
        let m = extract_json(r#"{"amount":42}"#).unwrap();
        assert_eq!(m.get("amount").unwrap(), 42);
    }

    #[test]
    fn extract_json_from_fenced_block() {
        let raw = "Sure!\n```json\n{\"amount\":42,\"note\":\"paid\"}\n```";
        let m = extract_json(raw).unwrap();
        assert_eq!(m.get("note").unwrap(), "paid");
    }

    #[test]
    fn extract_json_none_when_absent() {
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn truncate_is_char_safe() {
        let s = "áéíóú";
        assert_eq!(truncate(s, 10), s);
        assert!(truncate(s, 2).starts_with("áé"));
    }
}
