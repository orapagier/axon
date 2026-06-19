//! Tool-call repair, extracted from `agent::r#loop`.
//!
//! Some models emit a tool call as *text* (a JSON object, an XML `<tool_call>`
//! block, a `call:tool{...}` line, or a plain "Tool: x / Parameters: {...}"
//! block) instead of using the native tool-calling mechanism. `repair_tool_call`
//! recognizes those shapes and, for non-mutating tools, rewrites the response
//! into a real `ToolUse` block. Mutating tools are blocked so the executor
//! always produces a genuine execution receipt.

use crate::agent::quality::RE_CALL_COLON;
use crate::agent::r#loop::receipt_is_mutating;
use crate::providers::types::ContentBlock;
use crate::tools::schema::ToolDefinition;
use once_cell::sync::Lazy;

static RE_REPAIR_TOOL_PLAIN: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?i)(?:^|\n)\s*Tool:\s*(\w+)\s*(?:\n|$)").unwrap());
static RE_REPAIR_PARAMS: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?i)Parameters:\s*(\{[^}]*\}|None|none|null)").unwrap());
static RE_XML_FUNC: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"<function=([^>]+)>").unwrap());
static RE_XML_PARAM: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"(?s)<parameter=([^>]+)>(.*?)</parameter>").unwrap());

pub(crate) enum RepairDecision {
    Repaired(crate::providers::types::UnifiedResponse),
    Blocked(String),
    None,
}

pub(crate) fn repair_tool_call(
    text: &str,
    resp: crate::providers::types::UnifiedResponse,
    available_tools: &[ToolDefinition],
) -> RepairDecision {
    let finalize = |mut resp: crate::providers::types::UnifiedResponse,
                    name: String,
                    input: serde_json::Value|
     -> RepairDecision {
        if !available_tools.iter().any(|t| t.name == name) {
            return RepairDecision::Blocked(format!(
                "The tool '{}' is not a real registered tool. Do not invent tool names.",
                name
            ));
        }
        if receipt_is_mutating(&name, available_tools) {
            return RepairDecision::Blocked(format!(
                "The tool '{}' appears mutating. Mutating actions must use the native tool-calling mechanism so the executor can produce a real execution receipt.",
                name
            ));
        }
        resp.content
            .retain(|b| !matches!(b, ContentBlock::Text { .. }));
        resp.content.push(ContentBlock::ToolUse {
            id: format!("repair-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            name,
            input,
        });
        RepairDecision::Repaired(resp)
    };

    // JSON-based repair
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                let json_str = &text[start..=end];
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let parsed = if let Some(n) = val.get("name").and_then(|v| v.as_str()) {
                        let args = if let Some(a) = val.get("arguments") {
                            if a.is_string() {
                                serde_json::from_str(a.as_str().unwrap()).unwrap_or(a.clone())
                            } else {
                                a.clone()
                            }
                        } else if let Some(i) = val.get("input") {
                            i.clone()
                        } else {
                            serde_json::json!({})
                        };
                        Some((n.to_string(), args))
                    } else if let Some(n) = val.get("tool").and_then(|v| v.as_str()) {
                        let args = val.get("args").cloned().unwrap_or_else(|| {
                            val.get("input").cloned().unwrap_or_else(|| {
                                val.get("arguments")
                                    .cloned()
                                    .unwrap_or(serde_json::json!({}))
                            })
                        });
                        Some((n.to_string(), args))
                    } else if let Some(action) = val.get("action").and_then(|v| v.as_str()) {
                        let mut input = val.clone();
                        if let Some(obj) = input.as_object_mut() {
                            obj.remove("action");
                        }
                        let tool_name = match action {
                            "run" | "exec" | "ssh" => "ssh_tool",
                            "search" | "google" => "web_search_tool",
                            "read" | "write" | "edit" => "file_tool",
                            _ => return RepairDecision::None,
                        };
                        Some((tool_name.to_string(), input))
                    } else if let Some(n) = val.get("api_name").and_then(|v| v.as_str()) {
                        let args = val
                            .get("parameters")
                            .cloned()
                            .unwrap_or(serde_json::json!({}));
                        Some((n.to_string(), args))
                    } else {
                        None
                    };

                    if let Some((name, input)) = parsed {
                        return finalize(resp, name, input);
                    }
                }
            }
        }
    }

    // XML <tool_call> format
    if let (Some(start), Some(end)) = (text.find("<tool_call>"), text.find("</tool_call>")) {
        let block = &text[start..end + 12];
        if let Some(caps) = RE_XML_FUNC.captures(block) {
            if let Some(m) = caps.get(1) {
                let tool_name = m.as_str().to_string();
                let mut input = serde_json::Map::new();
                for caps_param in RE_XML_PARAM.captures_iter(block) {
                    if let (Some(k), Some(v)) = (caps_param.get(1), caps_param.get(2)) {
                        input.insert(
                            k.as_str().to_string(),
                            serde_json::Value::String(v.as_str().trim().to_string()),
                        );
                    }
                }
                return finalize(resp, tool_name, serde_json::Value::Object(input));
            }
        }
    }

    // call:tool{...} format
    if let Some(caps) = RE_CALL_COLON.captures(text) {
        if let (Some(t), Some(a)) = (caps.get(1), caps.get(2)) {
            let tool_name = t.as_str().to_string();
            let raw_args = a.as_str().trim();
            let input = if let Ok(val) =
                serde_json::from_str::<serde_json::Value>(&format!("{{{}}}", raw_args))
            {
                val
            } else {
                let mut map = serde_json::Map::new();
                for pair in raw_args.split(',') {
                    if let Some((k, v)) = pair.split_once(':') {
                        map.insert(
                            k.trim().to_string(),
                            serde_json::Value::String(v.trim().to_string()),
                        );
                    }
                }
                serde_json::Value::Object(map)
            };
            tracing::info!(
                "Repaired call:colon tool call: {} -> {:?}",
                tool_name,
                input
            );
            return finalize(resp, tool_name, input);
        }
    }

    // Plain-text "Tool: xxx" format
    if let Some(caps) = RE_REPAIR_TOOL_PLAIN.captures(text) {
        if let Some(m) = caps.get(1) {
            let tool_name = m.as_str().to_string();
            let input = if let Some(pcaps) = RE_REPAIR_PARAMS.captures(text) {
                if let Some(raw) = pcaps.get(1) {
                    let r = raw.as_str();
                    if r.eq_ignore_ascii_case("none") || r.eq_ignore_ascii_case("null") {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(r).unwrap_or(serde_json::json!({}))
                    }
                } else {
                    return RepairDecision::None;
                }
            } else {
                serde_json::json!({})
            };
            return finalize(resp, tool_name, input);
        }
    }

    RepairDecision::None
}
