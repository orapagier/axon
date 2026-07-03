use super::types::*;
use super::ProviderCallOptions;
use crate::tools::schema::ToolDefinition;
use anyhow::Context;
use futures::StreamExt;
use once_cell::sync::Lazy;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Serialize, Clone)]
struct OaiReq {
    model: String,
    messages: Vec<OaiMsg>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OaiMsg {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaiTc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<OaiFn>,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
struct OaiTc {
    id: String,
    r#type: String,
    function: OaiFn,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
struct OaiFn {
    name: String,
    arguments: String,
}
#[derive(Debug, Deserialize)]
struct OaiResp {
    choices: Vec<OaiChoice>,
    usage: Option<OaiUsage>,
}
#[derive(Debug, Deserialize)]
struct OaiChoice {
    message: OaiMsg,
    finish_reason: Option<String>,
}
#[derive(Debug, Deserialize)]
struct OaiUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OaiStreamResp {
    choices: Vec<OaiStreamChoice>,
    usage: Option<OaiUsage>,
}

#[derive(Debug, Deserialize)]
struct OaiStreamChoice {
    delta: Option<OaiDelta>,
    message: Option<OaiMsg>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OaiDelta {
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    tool_calls: Option<Vec<OaiTcDelta>>,
    #[serde(default)]
    function_call: Option<OaiFnDelta>,
}

#[derive(Debug, Deserialize)]
struct OaiTcDelta {
    index: Option<usize>,
    id: Option<String>,
    #[serde(rename = "type")]
    r#type: Option<String>,
    function: Option<OaiFnDelta>,
}

#[derive(Debug, Deserialize)]
struct OaiFnDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct PartialToolCall {
    id: String,
    r#type: String,
    name: String,
    arguments: String,
}

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("axon-agent/1.0")
        .build()
        .expect("build shared OpenAI-compatible HTTP client")
});

fn to_oai_msgs(messages: &[Message], system: &str) -> Vec<OaiMsg> {
    let mut out = vec![];
    if !system.is_empty() {
        out.push(OaiMsg {
            role: "system".into(),
            content: Some(json!(system)),
            tool_calls: None,
            tool_call_id: None,
            function_call: None,
        });
    }
    for m in messages {
        match &m.content {
            MessageContent::Text(t) => out.push(OaiMsg {
                role: m.role.clone(),
                content: Some(json!(t)),
                tool_calls: None,
                tool_call_id: None,
                function_call: None,
            }),
            MessageContent::Blocks(blocks) => {
                let trs: Vec<_> = blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                        } = b
                        {
                            Some((tool_use_id.clone(), content.clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                if !trs.is_empty() {
                    for (id, c) in trs {
                        out.push(OaiMsg {
                            role: "tool".into(),
                            content: Some(json!(c)),
                            tool_calls: None,
                            tool_call_id: Some(id),
                            function_call: None,
                        });
                    }
                } else {
                    let text = blocks
                        .iter()
                        .filter_map(|b| b.as_text())
                        .collect::<Vec<_>>()
                        .join("");
                    let tcs: Vec<OaiTc> = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolUse { id, name, input } = b {
                                Some(OaiTc {
                                    id: id.clone(),
                                    r#type: "function".into(),
                                    function: OaiFn {
                                        name: name.clone(),
                                        arguments: serde_json::to_string(input).unwrap_or_default(),
                                    },
                                })
                            } else {
                                None
                            }
                        })
                        .collect();
                    out.push(OaiMsg {
                        role: m.role.clone(),
                        content: if text.is_empty() {
                            Some(serde_json::Value::Null)
                        } else {
                            Some(json!(text))
                        },
                        tool_calls: if tcs.is_empty() { None } else { Some(tcs) },
                        tool_call_id: None,
                        function_call: None,
                    });
                }
            }
        }
    }
    out
}

/// Recursively sanitize JSON Schema values: replace non-standard types like "any"
/// with "string" so providers like Google Gemini don't reject the schema.
fn sanitize_schema(v: &mut Value) {
    match v {
        Value::Object(obj) => {
            if let Some(t) = obj
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
            {
                let valid = [
                    "string", "number", "integer", "boolean", "object", "array", "null",
                ];
                if !valid.contains(&t.as_str()) {
                    obj.insert("type".to_string(), json!("string"));
                }
            }
            for val in obj.values_mut() {
                sanitize_schema(val);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                sanitize_schema(item);
            }
        }
        _ => {}
    }
}

fn to_oai_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let mut params = t.parameters.clone();

            // Robustness: If the tool parameters are already a full object schema (common if copy-pasted),
            // extract just the properties to avoid doubled "type": "object" nesting.
            if let Some(obj) = params.as_object() {
                if obj.get("type").and_then(|v| v.as_str()) == Some("object") {
                    if let Some(props) = obj.get("properties") {
                        params = props.clone();
                    }
                }
            }

            // Ensure params is an object for properties
            if !params.is_object() {
                params = json!({});
            }

            // Sanitize non-standard schema types (e.g. "any") that providers like
            // Google Gemini reject with INVALID_ARGUMENT errors.
            sanitize_schema(&mut params);

            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": {
                        "type": "object",
                        "properties": params,
                        "required": t.required
                    }
                }
            })
        })
        .collect()
}

fn extract_text_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| {
                    if let Some(obj) = part.as_object() {
                        if obj.get("type").and_then(|v| v.as_str()) == Some("text") {
                            return obj
                                .get("text")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                        }
                    }
                    None
                })
                .collect::<Vec<_>>()
                .join(""),
        ),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn build_unified_response_from_parts(
    text: String,
    tool_calls: Vec<PartialToolCall>,
    stop_reason: StopReason,
    usage: UsageInfo,
) -> UnifiedResponse {
    let mut blocks = vec![];
    if !text.is_empty() {
        blocks.push(ContentBlock::text(text));
    }

    for tc in tool_calls {
        let input: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
        blocks.push(ContentBlock::ToolUse {
            id: if tc.id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                tc.id
            },
            name: tc.name,
            input,
        });
    }

    UnifiedResponse {
        content: blocks,
        stop_reason,
        usage,
    }
}

async fn call_streaming(
    model: &mut ModelRecord,
    url: &str,
    provider: &str,
    payload: OaiReq,
    stream_sink: StreamSink,
) -> anyhow::Result<UnifiedResponse> {
    let resp = HTTP_CLIENT
        .post(url)
        .header("Authorization", format!("Bearer {}", model.api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;

    let rl = parse_rl_headers(provider, resp.headers());
    model.rl_snapshot = rl;
    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = retry_after_header(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            let suffix = retry_after
                .map(|ra| format!(" [retry-after:{}]", ra))
                .unwrap_or_default();
            anyhow::bail!("rate limit{}: {}", suffix, body);
        }

        // FIX: Groq aggressively throws 400 if the model hallucinates a tool when tools were empty.
        // We intercept this and mock a tool use response, so the agent loop can correct the hallucination gracefully.
        if status.as_u16() == 400 && body.contains("Tool choice is none, but model called a tool") {
            if let Ok(json_err) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(failed_gen) = json_err
                    .pointer("/error/failed_generation")
                    .and_then(|v| v.as_str())
                {
                    if let Ok(tc_json) = serde_json::from_str::<serde_json::Value>(failed_gen) {
                        return Ok(build_unified_response_from_parts(
                            String::new(),
                            vec![PartialToolCall {
                                id: uuid::Uuid::new_v4().to_string(),
                                r#type: "function".to_string(),
                                name: tc_json
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown_tool")
                                    .to_string(),
                                arguments: tc_json
                                    .get("arguments")
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "{}".to_string()),
                            }],
                            StopReason::ToolUse,
                            UsageInfo::default(),
                        ));
                    }
                }
            }
            // Fallback if parsing failed: return a completely empty response to force the model to try again natively.
            return Ok(build_unified_response_from_parts(
                "I must not use tools right now.".to_string(),
                vec![],
                StopReason::EndTurn,
                UsageInfo::default(),
            ));
        }

        anyhow::bail!("provider error {} at {}: {}", status, url, body);
    }

    let mut text = String::new();
    let mut partial_tools: std::collections::BTreeMap<usize, PartialToolCall> =
        std::collections::BTreeMap::new();
    let mut usage = UsageInfo::default();
    let mut stop_reason = StopReason::EndTurn;
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read streaming chunk")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));

        while let Some(split_at) = buffer.find("\n\n") {
            let event_block = buffer[..split_at].to_string();
            buffer.drain(..split_at + 2);

            let data_lines: Vec<&str> = event_block
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect();

            if data_lines.is_empty() {
                continue;
            }

            let data = data_lines.join("\n");
            if data == "[DONE]" {
                continue;
            }

            let chunk: OaiStreamResp =
                serde_json::from_str(&data).with_context(|| "parse streaming response chunk")?;

            if let Some(usage_chunk) = chunk.usage {
                usage.input_tokens = usage_chunk.prompt_tokens.unwrap_or(usage.input_tokens);
                usage.output_tokens = usage_chunk.completion_tokens.unwrap_or(usage.output_tokens);
            }

            for choice in chunk.choices {
                if let Some(finish_reason) = choice.finish_reason.as_deref() {
                    stop_reason = match finish_reason {
                        "tool_calls" | "function_call" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        _ => StopReason::EndTurn,
                    };
                }

                if let Some(delta) = choice.delta {
                    if let Some(content) = delta.content.as_ref().and_then(extract_text_value) {
                        text.push_str(&content);
                        stream_sink.send(content).await;
                    }

                    if let Some(tool_calls) = delta.tool_calls {
                        for tc in tool_calls {
                            let idx = tc.index.unwrap_or(0);
                            let entry = partial_tools.entry(idx).or_default();
                            if let Some(id) = tc.id {
                                entry.id = id;
                            }
                            if let Some(kind) = tc.r#type {
                                entry.r#type = kind;
                            }
                            if let Some(function) = tc.function {
                                if let Some(name) = function.name {
                                    entry.name = name;
                                }
                                if let Some(arguments) = function.arguments {
                                    entry.arguments.push_str(&arguments);
                                }
                            }
                        }
                    }

                    if let Some(function_call) = delta.function_call {
                        let entry = partial_tools.entry(0).or_default();
                        entry.r#type = "function".to_string();
                        if let Some(name) = function_call.name {
                            entry.name = name;
                        }
                        if let Some(arguments) = function_call.arguments {
                            entry.arguments.push_str(&arguments);
                        }
                    }
                } else if let Some(message) = choice.message {
                    if let Some(content) = message.content.as_ref().and_then(extract_text_value) {
                        text.push_str(&content);
                        stream_sink.send(content).await;
                    }
                    if let Some(tool_calls) = message.tool_calls {
                        for (idx, tc) in tool_calls.into_iter().enumerate() {
                            let entry = partial_tools.entry(idx).or_default();
                            entry.id = tc.id;
                            entry.r#type = tc.r#type;
                            entry.name = tc.function.name;
                            entry.arguments = tc.function.arguments;
                        }
                    } else if let Some(fc) = message.function_call {
                        let entry = partial_tools.entry(0).or_default();
                        entry.id = uuid::Uuid::new_v4().to_string();
                        entry.r#type = "function".to_string();
                        entry.name = fc.name;
                        entry.arguments = fc.arguments;
                    }
                }
            }
        }
    }

    let tool_calls = partial_tools.into_values().collect::<Vec<_>>();
    Ok(build_unified_response_from_parts(
        text,
        tool_calls,
        stop_reason,
        usage,
    ))
}

/// The `Retry-After` header value (raw string — usually integer seconds, sometimes
/// an HTTP date) if present. Captured at 429 time and folded into the error so the
/// router can honor the provider's own reset timing.
pub fn retry_after_header(h: &HeaderMap) -> Option<String> {
    h.get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn parse_rl_headers(provider: &str, h: &HeaderMap) -> RateLimitSnapshot {
    let get = |k: &str| -> Option<u64> { h.get(k)?.to_str().ok()?.parse().ok() };
    let gets = |k: &str| -> Option<String> { Some(h.get(k)?.to_str().ok()?.to_string()) };
    let mut s = RateLimitSnapshot::default();
    s.last_updated = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    match provider {
        "anthropic" => {
            s.req_limit = get("anthropic-ratelimit-requests-limit");
            s.req_remaining = get("anthropic-ratelimit-requests-remaining");
            s.req_reset = gets("anthropic-ratelimit-requests-reset");
            s.tokens_limit = get("anthropic-ratelimit-tokens-limit");
            s.tokens_remaining = get("anthropic-ratelimit-tokens-remaining");
        }
        _ => {
            s.req_limit_per_min = get("x-ratelimit-limit-requests");
            s.req_remaining_per_min = get("x-ratelimit-remaining-requests");
            s.req_reset_per_min = gets("x-ratelimit-reset-requests");
            s.tokens_limit_per_min = get("x-ratelimit-limit-tokens");
            s.tokens_remaining_per_min = get("x-ratelimit-remaining-tokens");
            s.tokens_reset_per_min = gets("x-ratelimit-reset-tokens");
        }
    }
    s
}

pub async fn call(
    model: &mut ModelRecord,
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: u32,
    options: ProviderCallOptions,
) -> anyhow::Result<UnifiedResponse> {
    let provider = normalize_provider_name(&model.provider);
    let base = model
        .base_url
        .as_deref()
        .or_else(|| provider_base_url(&provider))
        .unwrap_or("https://api.openai.com/v1");
    let url = chat_completions_url(base);
    let oai_tools = if tools.is_empty() {
        None
    } else {
        Some(to_oai_tools(tools))
    };
    // tool_choice only applies when tools are present; map our enum to the
    // OpenAI wire value, defaulting to "auto".
    let tool_choice = oai_tools.as_ref().map(|_| match options.tool_choice {
        Some(ToolChoice::Required) => "required".to_string(),
        Some(ToolChoice::None) => "none".to_string(),
        _ => "auto".to_string(),
    });
    let payload = OaiReq {
        model: model.model_id.clone(),
        messages: to_oai_msgs(messages, system),
        max_tokens,
        stream: options.stream_sink.as_ref().map(|_| true),
        stream_options: None,
        tool_choice,
        tools: oai_tools,
        temperature: options.temperature,
        // Omit for models that already rejected the field once this process.
        reasoning_effort: if model.no_reasoning {
            None
        } else {
            options.reasoning_effort.clone()
        },
    };

    if let Some(stream_sink) = options.stream_sink {
        match call_streaming(model, &url, &provider, payload.clone(), stream_sink.clone()).await {
            Ok(resp) => return Ok(resp),
            Err(e) if !stream_sink.has_started() => {
                tracing::warn!(
                    "Streaming failed before any tokens for provider '{}' model '{}', retrying non-streaming: {}",
                    provider,
                    model.model_id,
                    e
                );
            }
            Err(e) => return Err(e),
        }
    }

    let payload = OaiReq {
        stream: None,
        stream_options: None,
        ..payload
    };

    let resp = HTTP_CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", model.api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;
    let rl = parse_rl_headers(&provider, resp.headers());
    model.rl_snapshot = rl;
    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = retry_after_header(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            let suffix = retry_after
                .map(|ra| format!(" [retry-after:{}]", ra))
                .unwrap_or_default();
            anyhow::bail!("rate limit{}: {}", suffix, body);
        }

        // FIX: Groq aggressively throws 400 if the model hallucinates a tool when tools were empty.
        // We intercept this and mock a tool use response, so the agent loop can correct the hallucination gracefully.
        if status.as_u16() == 400 && body.contains("Tool choice is none, but model called a tool") {
            if let Ok(json_err) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(failed_gen) = json_err
                    .pointer("/error/failed_generation")
                    .and_then(|v| v.as_str())
                {
                    if let Ok(tc_json) = serde_json::from_str::<serde_json::Value>(failed_gen) {
                        return Ok(build_unified_response_from_parts(
                            String::new(),
                            vec![PartialToolCall {
                                id: uuid::Uuid::new_v4().to_string(),
                                r#type: "function".to_string(),
                                name: tc_json
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown_tool")
                                    .to_string(),
                                arguments: tc_json
                                    .get("arguments")
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|| "{}".to_string()),
                            }],
                            StopReason::ToolUse,
                            UsageInfo::default(),
                        ));
                    }
                }
            }
            return Ok(build_unified_response_from_parts(
                "I must not use tools right now.".to_string(),
                vec![],
                StopReason::EndTurn,
                UsageInfo::default(),
            ));
        }

        // A provider that doesn't understand `reasoning_effort` rejects the
        // whole request with a 400. Strip the field, remember this model
        // can't take it, and retry once (recursion is bounded: no_reasoning
        // makes the retried payload omit the field, so this branch can't
        // re-trigger).
        if status.as_u16() == 400
            && payload.reasoning_effort.is_some()
            && body.to_lowercase().contains("reasoning")
        {
            tracing::info!(
                "Model '{}' rejected reasoning_effort; retrying without it (flagged no_reasoning)",
                model.name
            );
            model.no_reasoning = true;
            return Box::pin(call(
                model,
                messages,
                system,
                tools,
                max_tokens,
                ProviderCallOptions {
                    stream_sink: None,
                    temperature: options.temperature,
                    tool_choice: options.tool_choice,
                    reasoning_effort: None,
                },
            ))
            .await;
        }

        anyhow::bail!("provider error {} at {}: {}", status, url, body);
    }
    let body: OaiResp = resp.json().await.context("parse response")?;
    let choice = body.choices.into_iter().next().context("empty choices")?;
    let mut partial_tools = Vec::new();
    if let Some(tcs) = &choice.message.tool_calls {
        for tc in tcs {
            partial_tools.push(PartialToolCall {
                id: tc.id.clone(),
                r#type: tc.r#type.clone(),
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            });
        }
    } else if let Some(fc) = &choice.message.function_call {
        partial_tools.push(PartialToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            r#type: "function".to_string(),
            name: fc.name.clone(),
            arguments: fc.arguments.clone(),
        });
    }
    let stop_reason = match choice.finish_reason.as_deref() {
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    };
    Ok(build_unified_response_from_parts(
        choice
            .message
            .content
            .as_ref()
            .and_then(extract_text_value)
            .unwrap_or_default(),
        partial_tools,
        stop_reason,
        UsageInfo {
            input_tokens: body
                .usage
                .as_ref()
                .and_then(|u| u.prompt_tokens)
                .unwrap_or(0),
            output_tokens: body
                .usage
                .as_ref()
                .and_then(|u| u.completion_tokens)
                .unwrap_or(0),
        },
    ))
}
