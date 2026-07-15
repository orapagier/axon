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
                    let images: Vec<&ContentBlock> = blocks
                        .iter()
                        .filter(|b| matches!(b, ContentBlock::Image { .. }))
                        .collect();
                    let tcs: Vec<OaiTc> = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolUse {
                                id, name, input, ..
                            } = b
                            {
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
                    let content = if images.is_empty() {
                        if text.is_empty() {
                            Some(serde_json::Value::Null)
                        } else {
                            Some(json!(text))
                        }
                    } else {
                        // Vision format: `content` becomes an array of parts
                        // instead of a plain string — an optional text part
                        // followed by one `image_url` part per image, sent as
                        // a `data:` URI so no provider-side fetch is needed.
                        let mut parts: Vec<serde_json::Value> = Vec::new();
                        if !text.is_empty() {
                            parts.push(json!({"type": "text", "text": text}));
                        }
                        for blk in &images {
                            if let ContentBlock::Image { media_type, data } = blk {
                                parts.push(json!({
                                    "type": "image_url",
                                    "image_url": { "url": format!("data:{};base64,{}", media_type, data) }
                                }));
                            }
                        }
                        Some(json!(parts))
                    };
                    out.push(OaiMsg {
                        role: m.role.clone(),
                        content,
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

/// JSON-Schema keywords we forward to model providers. Any other key found at a
/// schema-node level is dropped before the request is built.
///
/// Strict providers (Google Gemini/Gemma) reject unknown fields with
/// `400 INVALID_ARGUMENT` ("Unknown name ... Cannot find field"), while lenient
/// OpenAI-shaped providers silently ignore them. Axon's own tool schemas carry
/// non-schema UI hints (n8n's `displayOptions`), and MCP tools forward
/// `inputSchema` verbatim from external servers — so arbitrary keys (`$ref`,
/// `$schema`, `additionalProperties`, future n8n-isms) can appear. Whitelisting
/// standard keywords keeps the payload portable across every provider and model;
/// a blocklist could never enumerate what an MCP server might send.
const ALLOWED_SCHEMA_KEYS: &[&str] = &[
    // core / annotations
    "type",
    "description",
    "title",
    "default",
    "enum",
    "const",
    "format",
    "nullable",
    // object
    "properties",
    "required",
    "additionalProperties",
    "minProperties",
    "maxProperties",
    // array
    "items",
    "prefixItems",
    "minItems",
    "maxItems",
    "uniqueItems",
    // string
    "minLength",
    "maxLength",
    "pattern",
    // number
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    // combinators
    "anyOf",
    "allOf",
    "oneOf",
    "not",
];

const VALID_SCHEMA_TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "object", "array", "null",
];

/// Sanitize a JSON-Schema *properties map* (property-name -> subschema) in place
/// so it is safe to send to any model provider. Shared with `providers::google`
/// (Gemini's native adapter needs the same treatment).
///
/// The input is the map of parameter properties, so its keys are property
/// *names* (`job_id`, `action`, ...) and must never be filtered — only the
/// subschema *values* are cleaned. See [`ALLOWED_SCHEMA_KEYS`] for the rules.
pub fn sanitize_schema(props: &mut Value) {
    if let Value::Object(map) = props {
        for subschema in map.values_mut() {
            sanitize_schema_node(subschema);
        }
    }
}

/// Sanitize a single schema node: coerce non-standard `type` values, drop keys
/// that aren't standard JSON-Schema, and recurse into nested schemas.
fn sanitize_schema_node(v: &mut Value) {
    let Value::Object(obj) = v else { return };

    // Drop everything that isn't a recognized JSON-Schema keyword. Property
    // *names* are never inspected here — they live one level up as the keys of a
    // `properties` map handled by `sanitize_schema`.
    obj.retain(|k, _| ALLOWED_SCHEMA_KEYS.contains(&k.as_str()));

    // Coerce non-standard types (e.g. n8n's "any") to "string".
    if let Some(t) = obj.get("type").and_then(Value::as_str) {
        if !VALID_SCHEMA_TYPES.contains(&t) {
            obj.insert("type".to_string(), json!("string"));
        }
    }

    // Recurse into every position that holds a nested schema.
    if let Some(nested) = obj.get_mut("properties") {
        sanitize_schema(nested);
    }
    if let Some(items) = obj.get_mut("items") {
        match items {
            Value::Array(arr) => arr.iter_mut().for_each(sanitize_schema_node),
            other => sanitize_schema_node(other),
        }
    }
    if let Some(ap) = obj.get_mut("additionalProperties") {
        if ap.is_object() {
            sanitize_schema_node(ap);
        }
    }
    for key in ["anyOf", "allOf", "oneOf", "prefixItems"] {
        if let Some(Value::Array(arr)) = obj.get_mut(key) {
            arr.iter_mut().for_each(sanitize_schema_node);
        }
    }
    if let Some(not) = obj.get_mut("not") {
        sanitize_schema_node(not);
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
            signature: None,
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

        for data in drain_sse_frames(&mut buffer) {
            // "[DONE]" is OpenAI-specific and has no equivalent in the shared
            // frame-splitting helper, so it's filtered here rather than there.
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

/// Split accumulated SSE bytes into complete event frames, returning each
/// frame's joined `data:` payload and leaving any trailing partial event in
/// `buffer` for the next chunk. Provider-agnostic — shared with
/// `providers::google`'s streaming path. The OpenAI-specific `"[DONE]"`
/// sentinel has no equivalent in Gemini's native SSE, so it stays filtered
/// by each caller rather than here.
pub fn drain_sse_frames(buffer: &mut String) -> Vec<String> {
    let mut frames = Vec::new();
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

        frames.push(data_lines.join("\n"));
    }
    frames
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
        // re-trigger). Providers word the rejection differently — Groq-hosted
        // Gemma says "thinking level is not supported for this model" rather
        // than mentioning "reasoning" at all — so check both terms.
        if status.as_u16() == 400 && payload.reasoning_effort.is_some() && {
            let lower = body.to_lowercase();
            lower.contains("reasoning") || lower.contains("thinking")
        } {
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

#[derive(Debug, Deserialize)]
struct OaiImageResp {
    #[serde(default)]
    data: Vec<OaiImageDatum>,
    usage: Option<OaiImageUsage>,
}
#[derive(Debug, Deserialize)]
struct OaiImageDatum {
    b64_json: Option<String>,
    url: Option<String>,
    revised_prompt: Option<String>,
}
/// gpt-image-1 reports image-token usage; DALL·E and most compatible hosts
/// omit the field entirely.
#[derive(Debug, Deserialize)]
struct OaiImageUsage {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

/// Best-effort MIME type for raw image bytes; generation endpoints default to
/// PNG when they don't say.
fn sniff_image_mime(bytes: &[u8]) -> String {
    image::guess_format(bytes)
        .ok()
        .map(|f| f.to_mime_type().to_string())
        .unwrap_or_else(|| "image/png".to_string())
}

/// Generate an image via the OpenAI-compatible `/images/generations`
/// endpoint. `response_format` is deliberately omitted: gpt-image-1 rejects
/// the parameter (it always returns base64) while DALL·E-era models default
/// to a short-lived URL — so both response shapes are handled and a URL is
/// fetched immediately.
pub async fn generate_image(
    model: &mut ModelRecord,
    prompt: &str,
) -> anyhow::Result<GeneratedImage> {
    let provider = normalize_provider_name(&model.provider);
    let base = model
        .base_url
        .as_deref()
        .or_else(|| provider_base_url(&provider))
        .unwrap_or("https://api.openai.com/v1");
    let url = format!("{}/images/generations", normalize_base_url_str(base));

    let payload = json!({
        "model": model.model_id,
        "prompt": prompt,
        "n": 1,
    });
    let resp = HTTP_CLIENT
        .post(&url)
        .header("Authorization", format!("Bearer {}", model.api_key))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;
    model.rl_snapshot = parse_rl_headers(&provider, resp.headers());
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
        if status.as_u16() == 404 {
            anyhow::bail!(
                "provider error 404 at {}: this host does not expose /images/generations \
                 (not all OpenAI-compatible providers support image generation): {}",
                url,
                body
            );
        }
        anyhow::bail!("provider error {} at {}: {}", status, url, body);
    }

    let body: OaiImageResp = resp.json().await.context("parse image response")?;
    let usage = UsageInfo {
        input_tokens: body
            .usage
            .as_ref()
            .and_then(|u| u.input_tokens)
            .unwrap_or(0),
        output_tokens: body
            .usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0),
    };
    let datum = body
        .data
        .into_iter()
        .next()
        .context("image response contained no data entries")?;
    let text = datum.revised_prompt.unwrap_or_default();

    if let Some(b64) = datum.b64_json.filter(|s| !s.is_empty()) {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        let bytes = BASE64
            .decode(b64.trim())
            .map_err(|e| anyhow::anyhow!("image base64 decode failed: {}", e))?;
        let mime_type = sniff_image_mime(&bytes);
        return Ok(GeneratedImage {
            bytes,
            mime_type,
            text,
            usage,
        });
    }
    if let Some(img_url) = datum.url.filter(|s| !s.is_empty()) {
        let img_resp = HTTP_CLIENT
            .get(&img_url)
            .send()
            .await
            .with_context(|| format!("fetch generated image from {}", img_url))?;
        if !img_resp.status().is_success() {
            anyhow::bail!(
                "fetching generated image from {} failed with {}",
                img_url,
                img_resp.status()
            );
        }
        let content_type = img_resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
            .filter(|s| s.starts_with("image/"));
        let bytes = img_resp
            .bytes()
            .await
            .context("read generated image body")?
            .to_vec();
        let mime_type = content_type.unwrap_or_else(|| sniff_image_mime(&bytes));
        return Ok(GeneratedImage {
            bytes,
            mime_type,
            text,
            usage,
        });
    }
    anyhow::bail!("image response entry had neither b64_json nor url");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_sse_frames_splits_multiple_events_and_partial_buffer() {
        let mut buffer =
            String::from("data: {\"a\":1}\n\ndata: {\"a\":2}\n\ndata: partial-no-terminator-yet");
        let frames = drain_sse_frames(&mut buffer);
        assert_eq!(
            frames,
            vec!["{\"a\":1}".to_string(), "{\"a\":2}".to_string()]
        );
        // The unterminated trailing event stays in the buffer for the next chunk.
        assert_eq!(buffer, "data: partial-no-terminator-yet");
    }

    // Both wire shapes /images/generations can answer with: gpt-image-1 style
    // (b64_json + usage) and DALL·E style (url, no usage).
    #[test]
    fn image_response_parses_b64_and_url_shapes() {
        let b64: OaiImageResp = serde_json::from_str(
            r#"{"created":1,"data":[{"b64_json":"QUJD","revised_prompt":"a nicer fox"}],"usage":{"input_tokens":10,"output_tokens":4000}}"#,
        )
        .unwrap();
        assert_eq!(b64.data[0].b64_json.as_deref(), Some("QUJD"));
        assert_eq!(b64.data[0].revised_prompt.as_deref(), Some("a nicer fox"));
        assert_eq!(b64.usage.as_ref().unwrap().output_tokens, Some(4000));

        let url: OaiImageResp =
            serde_json::from_str(r#"{"created":1,"data":[{"url":"https://cdn.example/img.png"}]}"#)
                .unwrap();
        assert_eq!(
            url.data[0].url.as_deref(),
            Some("https://cdn.example/img.png")
        );
        assert!(url.data[0].b64_json.is_none());
        assert!(url.usage.is_none());
    }

    #[test]
    fn sniffs_png_magic_bytes_and_defaults_to_png() {
        let png_header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
        assert_eq!(sniff_image_mime(png_header), "image/png");
        assert_eq!(sniff_image_mime(b"not an image"), "image/png");
    }

    #[test]
    fn sanitize_schema_strips_non_schema_keys_but_keeps_property_names() {
        // A properties map: keys are parameter *names* (must survive), values
        // are subschemas carrying non-standard keys that strict providers 400 on.
        let mut props = json!({
            // n8n UI hint on a scalar param — the actual Gemini/Gemma 400 cause.
            "job_id": {
                "type": "string",
                "description": "keep me",
                "displayOptions": {"show": {"action": ["edit"]}}
            },
            // non-standard type coerced + arbitrary MCP-style junk dropped.
            "blob": {"type": "any", "$ref": "#/x", "additionalKey": 1},
            // nested object: junk one level deeper must also be stripped.
            "cfg": {
                "type": "object",
                "displayOptions": {"show": {"action": ["create"]}},
                "properties": {
                    "inner": {"type": "string", "typeOptions": {"rows": 4}}
                }
            },
            // array items schema is also cleaned.
            "items_param": {
                "type": "array",
                "items": {"type": "string", "displayOptions": {}}
            }
        });
        sanitize_schema(&mut props);

        // Property names are preserved.
        assert!(props.get("job_id").is_some());
        assert!(props.get("blob").is_some());
        assert!(props.get("cfg").is_some());

        // Non-schema keys are gone at every depth...
        assert!(props.pointer("/job_id/displayOptions").is_none());
        assert!(props.pointer("/blob/$ref").is_none());
        assert!(props.pointer("/blob/additionalKey").is_none());
        assert!(props.pointer("/cfg/displayOptions").is_none());
        assert!(props.pointer("/cfg/properties/inner/typeOptions").is_none());
        assert!(props.pointer("/items_param/items/displayOptions").is_none());

        // ...while real schema keywords and values are untouched.
        assert_eq!(
            props
                .pointer("/job_id/description")
                .and_then(|v| v.as_str()),
            Some("keep me")
        );
        // Non-standard "any" type is coerced to "string".
        assert_eq!(
            props.pointer("/blob/type").and_then(|v| v.as_str()),
            Some("string")
        );
        assert_eq!(
            props
                .pointer("/cfg/properties/inner/type")
                .and_then(|v| v.as_str()),
            Some("string")
        );
        assert_eq!(
            props
                .pointer("/items_param/items/type")
                .and_then(|v| v.as_str()),
            Some("string")
        );
    }
}
