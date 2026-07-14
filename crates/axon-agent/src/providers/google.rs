use super::types::*;
use super::ProviderCallOptions;
use crate::tools::schema::ToolDefinition;
use anyhow::Context;
use futures::StreamExt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GenReq {
    contents: Vec<GenContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GenContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GenTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_config: Option<GenToolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenGenerationConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GenContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default)]
    parts: Vec<GenPart>,
}

/// Flat/untagged wire shape: exactly one of the optional fields is populated
/// per part. `thought`/`thought_signature` only appear when Gemini's native
/// `includeThoughts` is requested (we never send it) but are parsed
/// defensively anyway; `thought_signature` sits directly on the same part as
/// `function_call` — no wrapper envelope needed, unlike the OpenAI-compat
/// shim's `extra_content.google.thought_signature` workaround.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct GenPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<GenInlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<GenFnCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<GenFnResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GenInlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GenFnCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    name: String,
    args: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct GenFnResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    name: String,
    response: Value,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GenTool {
    function_declarations: Vec<GenFnDecl>,
}

#[derive(Debug, Serialize, Clone)]
struct GenFnDecl {
    name: String,
    description: String,
    /// `None` for zero-parameter tools: Gemini's native API rejects
    /// `{"type":"object","properties":{}}` with 400 INVALID_ARGUMENT
    /// ("properties: should be non-empty for OBJECT type") — the field must
    /// be omitted entirely, unlike the OpenAI-compat shim which tolerated it.
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Value>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GenToolConfig {
    function_calling_config: GenFnCallingConfig,
}

#[derive(Debug, Serialize, Clone)]
struct GenFnCallingConfig {
    mode: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GenGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_config: Option<GenThinkingConfig>,
    /// `["TEXT","IMAGE"]` for image-generation models (e.g.
    /// gemini-2.5-flash-image); text-only models reject the field, so it is
    /// only ever set on the dedicated image-generation path.
    #[serde(skip_serializing_if = "Option::is_none")]
    response_modalities: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct GenThinkingConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_level: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenResp {
    #[serde(default)]
    candidates: Vec<GenCandidate>,
    usage_metadata: Option<GenUsage>,
    /// Mid-stream errors (e.g. quota exhausted after the 200 was committed)
    /// arrive as an `{"error": {...}}` SSE frame. Without this field the
    /// frame would deserialize as an empty-but-valid response (`candidates`
    /// is `#[serde(default)]`) and the failure would be silently swallowed.
    error: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenCandidate {
    content: Option<GenContent>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenUsage {
    prompt_token_count: Option<u32>,
    candidates_token_count: Option<u32>,
    /// Thinking tokens are reported separately from `candidatesTokenCount`
    /// but bill as output — both are summed into `output_tokens`.
    thoughts_token_count: Option<u32>,
}

impl GenUsage {
    fn output_total(&self) -> u32 {
        self.candidates_token_count.unwrap_or(0) + self.thoughts_token_count.unwrap_or(0)
    }
}

/// Request payload for the Imagen `:predict` endpoint. Imagen models (unlike the
/// Gemini image models) are served only on `:predict` with this Vertex-style
/// `instances`/`parameters` envelope — never `generateContent`.
#[derive(Debug, Serialize)]
struct ImagenReq {
    instances: Vec<ImagenInstance>,
    parameters: ImagenParams,
}

#[derive(Debug, Serialize)]
struct ImagenInstance {
    prompt: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImagenParams {
    sample_count: u32,
}

/// Response from `:predict`: one entry per requested sample, each carrying the
/// generated image as base64. No token/usage metadata is returned.
#[derive(Debug, Deserialize)]
struct ImagenResp {
    #[serde(default)]
    predictions: Vec<ImagenPrediction>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImagenPrediction {
    #[serde(default)]
    bytes_base64_encoded: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
}

/// Imagen models (`imagen-3.0-*`, `imagen-4.0-*`) are served only on the
/// `:predict` endpoint; the Gemini image models (`gemini-*-image`) use
/// `:generateContent`. Hitting the wrong endpoint returns 404 NOT_FOUND.
fn is_imagen_model(model_id: &str) -> bool {
    model_id.to_ascii_lowercase().contains("imagen")
}

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("axon-agent/1.0")
        .build()
        .expect("build shared Google HTTP client")
});

/// Pre-scan every `ToolUse` block across the whole conversation so tool
/// results (which carry only an id) can be resolved to the function name
/// Gemini's `functionResponse.name` requires.
fn tool_names_by_id(messages: &[Message]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for m in messages {
        if let MessageContent::Blocks(blocks) = &m.content {
            for b in blocks {
                if let ContentBlock::ToolUse { id, name, .. } = b {
                    out.insert(id.clone(), name.clone());
                }
            }
        }
    }
    out
}

fn message_to_parts(m: &Message, id_to_name: &HashMap<String, String>) -> Vec<GenPart> {
    match &m.content {
        MessageContent::Text(t) => {
            if t.is_empty() {
                vec![]
            } else {
                vec![GenPart {
                    text: Some(t.clone()),
                    ..Default::default()
                }]
            }
        }
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => {
                    if text.is_empty() {
                        None
                    } else {
                        Some(GenPart {
                            text: Some(text.clone()),
                            ..Default::default()
                        })
                    }
                }
                ContentBlock::ToolUse {
                    id,
                    name,
                    input,
                    signature,
                } => Some(GenPart {
                    function_call: Some(GenFnCall {
                        id: Some(id.clone()),
                        name: name.clone(),
                        args: input.clone(),
                    }),
                    thought_signature: signature.clone(),
                    ..Default::default()
                }),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                } => {
                    let name = id_to_name.get(tool_use_id).cloned().unwrap_or_else(|| {
                        tracing::warn!(
                            "google: no matching ToolUse for tool_use_id '{}', using fallback function name",
                            tool_use_id
                        );
                        "unknown_function".to_string()
                    });
                    Some(GenPart {
                        function_response: Some(GenFnResponse {
                            id: Some(tool_use_id.clone()),
                            name,
                            response: json!({"result": content}),
                        }),
                        ..Default::default()
                    })
                }
                ContentBlock::Image { media_type, data } => Some(GenPart {
                    inline_data: Some(GenInlineData {
                        mime_type: media_type.clone(),
                        data: data.clone(),
                    }),
                    ..Default::default()
                }),
                // Dropped for cross-provider-failover tolerance: thinking
                // blocks captured under another provider are never replayed
                // to Gemini.
                ContentBlock::Thinking { .. } => None,
            })
            .collect(),
    }
}

/// One `GenContent` per `Message`, then adjacent same-role entries are
/// merged. This merge is load-bearing, not defensive padding: `agent/loop.rs`
/// emits parallel tool results as N separate `Message`s (one
/// `Message::tool_result()` call per result), but Gemini requires all of a
/// turn's tool results batched into a single `Content{role:"user"}` with
/// multiple `functionResponse` parts — this pass is what produces that shape.
fn contents_from_messages(messages: &[Message]) -> Vec<GenContent> {
    let id_to_name = tool_names_by_id(messages);
    let mut out: Vec<GenContent> = Vec::new();
    for m in messages {
        let role = if m.role == "assistant" {
            "model"
        } else {
            "user"
        };
        let parts = message_to_parts(m, &id_to_name);
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if last.role.as_deref() == Some(role) {
                last.parts.extend(parts);
                continue;
            }
        }
        out.push(GenContent {
            role: Some(role.to_string()),
            parts,
        });
    }
    out
}

fn to_gen_tools(tools: &[ToolDefinition]) -> Vec<GenTool> {
    let declarations = tools
        .iter()
        .map(|t| {
            let mut params = t.parameters.clone();
            // Robustness: unwrap an already-full object schema (common if
            // copy-pasted) to avoid doubled "type":"object" nesting.
            if let Some(obj) = params.as_object() {
                if obj.get("type").and_then(|v| v.as_str()) == Some("object") {
                    if let Some(props) = obj.get("properties") {
                        params = props.clone();
                    }
                }
            }
            if !params.is_object() {
                params = json!({});
            }
            super::openai_compat::sanitize_schema(&mut params);
            let has_properties = params.as_object().is_some_and(|o| !o.is_empty());
            GenFnDecl {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: has_properties.then(|| {
                    json!({
                        "type": "object",
                        "properties": params,
                        "required": t.required
                    })
                }),
            }
        })
        .collect();
    vec![GenTool {
        function_declarations: declarations,
    }]
}

fn build_tool_config(tool_choice: Option<ToolChoice>) -> Option<GenToolConfig> {
    let mode = match tool_choice {
        Some(ToolChoice::Required) => "ANY",
        Some(ToolChoice::None) => "NONE",
        _ => return None,
    };
    Some(GenToolConfig {
        function_calling_config: GenFnCallingConfig {
            mode: mode.to_string(),
        },
    })
}

/// Thinking control: opt-in per model via `thinking_mode` in models.toml —
/// "level" (Gemini 3.x `thinkingLevel`: low/medium/high) or "budget"
/// (Gemini 2.5-era `thinkingBudget`, same 2048/8192/16384 effort scale and
/// `max_tokens.saturating_sub(1024)` floor-drop Anthropic's "budget" mode
/// uses). `model.no_reasoning` (flipped after a 400 rejection) suppresses the
/// field entirely regardless of `thinking_mode`. Unlike Anthropic, temperature
/// is never force-dropped when thinking is active.
fn build_generation_config(
    model: &ModelRecord,
    max_tokens: u32,
    options: &ProviderCallOptions,
) -> Option<GenGenerationConfig> {
    let thinking_config = if model.no_reasoning {
        None
    } else {
        match (
            model.thinking_mode.as_deref(),
            options.reasoning_effort.as_deref(),
        ) {
            (Some("level"), Some(effort)) => {
                let level = match effort {
                    "low" => "low",
                    "high" => "high",
                    _ => "medium",
                };
                Some(GenThinkingConfig {
                    thinking_budget: None,
                    thinking_level: Some(level.to_string()),
                })
            }
            (Some("budget"), Some(effort)) => {
                let budget: i32 = match effort {
                    "low" => 2048,
                    "high" => 16384,
                    _ => 8192,
                };
                let budget = budget.min(max_tokens.saturating_sub(1024) as i32);
                if budget >= 1024 {
                    Some(GenThinkingConfig {
                        thinking_budget: Some(budget),
                        thinking_level: None,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    };
    Some(GenGenerationConfig {
        max_output_tokens: Some(max_tokens),
        temperature: options.temperature,
        thinking_config,
        response_modalities: None,
    })
}

/// Gemini always returns `finishReason:"STOP"` whether the turn produced text
/// or a function call — there is no distinct "tool_calls" reason like OpenAI.
/// `ToolUse` must be inferred from whether any function-call blocks were
/// actually parsed, never from `finish_reason` directly.
fn infer_stop_reason(tool_blocks: &[ContentBlock], finish_reason: Option<&str>) -> StopReason {
    if !tool_blocks.is_empty() {
        return StopReason::ToolUse;
    }
    match finish_reason {
        Some("MAX_TOKENS") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

fn parts_to_blocks(parts: Vec<GenPart>) -> (String, Vec<ContentBlock>) {
    let mut text = String::new();
    let mut blocks = Vec::new();
    for part in parts {
        // Never shown to the user — same anti-leak posture as the `<thought>`
        // tag fix applied to the OpenAI-compat shim this session.
        if part.thought == Some(true) {
            continue;
        }
        if let Some(t) = part.text {
            text.push_str(&t);
        }
        if let Some(fc) = part.function_call {
            blocks.push(ContentBlock::ToolUse {
                id: fc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: fc.name,
                input: fc.args,
                signature: part.thought_signature,
            });
        }
    }
    (text, blocks)
}

/// Deliberately not shared with `openai_compat::build_unified_response_from_parts`:
/// that one expects string `arguments` needing a JSON-parse round trip, while
/// Gemini's `args` arrive as already-parsed `Value` — sharing would force a
/// pointless string round trip.
fn finalize_response(
    text: String,
    tool_blocks: Vec<ContentBlock>,
    finish_reason: Option<&str>,
    usage: UsageInfo,
) -> UnifiedResponse {
    let stop_reason = infer_stop_reason(&tool_blocks, finish_reason);
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(ContentBlock::text(text));
    }
    content.extend(tool_blocks);
    UnifiedResponse {
        content,
        stop_reason,
        usage,
    }
}

/// Real SSE: each frame is a *complete* `GenerateContentResponse`, and
/// `functionCall.args` always arrives whole in one frame (Gemini doesn't
/// token-stream call arguments) — no fragment-merge needed, unlike OpenAI's
/// `BTreeMap<usize, PartialToolCall>` accumulation. No `[DONE]` sentinel
/// either; the connection just ends.
async fn call_streaming(
    model: &mut ModelRecord,
    url: &str,
    payload: &GenReq,
    stream_sink: StreamSink,
) -> anyhow::Result<UnifiedResponse> {
    let resp = HTTP_CLIENT
        .post(url)
        .header("x-goog-api-key", &model.api_key)
        .header("Content-Type", "application/json")
        .json(payload)
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;

    let rl = super::openai_compat::parse_rl_headers("google", resp.headers());
    model.rl_snapshot = rl;
    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = super::openai_compat::retry_after_header(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            let suffix = retry_after
                .map(|ra| format!(" [retry-after:{}]", ra))
                .unwrap_or_default();
            anyhow::bail!("rate limit{}: {}", suffix, body);
        }
        anyhow::bail!("provider error {} at {}: {}", status, url, body);
    }

    let mut text = String::new();
    let mut tool_blocks: Vec<ContentBlock> = Vec::new();
    let mut usage = UsageInfo::default();
    let mut finish_reason: Option<String> = None;
    let mut buffer = String::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read streaming chunk")?;
        buffer.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));

        for data in super::openai_compat::drain_sse_frames(&mut buffer) {
            let parsed: GenResp =
                serde_json::from_str(&data).with_context(|| "parse streaming response chunk")?;

            if let Some(err) = parsed.error {
                anyhow::bail!("provider error mid-stream at {}: {}", url, err);
            }

            if let Some(u) = parsed.usage_metadata {
                usage.input_tokens = u.prompt_token_count.unwrap_or(usage.input_tokens);
                let out = u.output_total();
                if out > 0 {
                    usage.output_tokens = out;
                }
            }

            if let Some(candidate) = parsed.candidates.into_iter().next() {
                if candidate.finish_reason.is_some() {
                    finish_reason = candidate.finish_reason;
                }
                let parts = candidate.content.map(|c| c.parts).unwrap_or_default();
                let (chunk_text, chunk_tools) = parts_to_blocks(parts);
                if !chunk_text.is_empty() {
                    text.push_str(&chunk_text);
                    stream_sink.send(chunk_text).await;
                }
                tool_blocks.extend(chunk_tools);
            }
        }
    }

    // A healthy stream always ends with a candidate carrying a finishReason.
    // Seeing none (and no content) means the prompt was blocked — Gemini
    // returns 200 with a promptFeedback-only frame and no candidates — or the
    // stream died; surface it instead of returning an empty success (the
    // pre-token fallback in `call` will then retry non-streaming, where the
    // same condition produces a proper "empty candidates" error).
    if finish_reason.is_none() && text.is_empty() && tool_blocks.is_empty() {
        anyhow::bail!(
            "stream at {} ended with no candidates (prompt possibly blocked)",
            url
        );
    }

    Ok(finalize_response(
        text,
        tool_blocks,
        finish_reason.as_deref(),
        usage,
    ))
}

pub async fn call(
    model: &mut ModelRecord,
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: u32,
    options: ProviderCallOptions,
) -> anyhow::Result<UnifiedResponse> {
    let base = model
        .base_url
        .as_deref()
        .or_else(|| provider_base_url("google"))
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta");
    let base = normalize_base_url_str(base);
    // Runtime-added DB rows are never refreshed by the boot sync, so one may
    // still carry the old OpenAI-compat shim base (".../v1beta/openai/") from
    // before this native adapter existed. That suffix can never be right for
    // generateContent URLs — strip it rather than 404 every call.
    let base = base.strip_suffix("/openai").unwrap_or(&base).to_string();

    let system_instruction = if system.is_empty() {
        None
    } else {
        Some(GenContent {
            role: None,
            parts: vec![GenPart {
                text: Some(system.to_string()),
                ..Default::default()
            }],
        })
    };
    let gen_tools = if tools.is_empty() {
        None
    } else {
        Some(to_gen_tools(tools))
    };
    // tool_config only applies when tools are present.
    let tool_config = gen_tools
        .as_ref()
        .and_then(|_| build_tool_config(options.tool_choice));
    let generation_config = build_generation_config(model, max_tokens, &options);

    let payload = GenReq {
        contents: contents_from_messages(messages),
        system_instruction,
        tools: gen_tools,
        tool_config,
        generation_config,
    };

    let model_id = model.model_id.clone();
    let gen_url = format!("{}/models/{}:generateContent", base, model_id);
    let stream_url = format!("{}/models/{}:streamGenerateContent?alt=sse", base, model_id);

    if let Some(stream_sink) = options.stream_sink.clone() {
        match call_streaming(model, &stream_url, &payload, stream_sink.clone()).await {
            Ok(resp) => return Ok(resp),
            Err(e) if !stream_sink.has_started() => {
                tracing::warn!(
                    "Streaming failed before any tokens for provider 'google' model '{}', retrying non-streaming: {}",
                    model.model_id,
                    e
                );
            }
            Err(e) => return Err(e),
        }
    }

    let resp = HTTP_CLIENT
        .post(&gen_url)
        .header("x-goog-api-key", &model.api_key)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("HTTP to {}", gen_url))?;

    let rl = super::openai_compat::parse_rl_headers("google", resp.headers());
    model.rl_snapshot = rl;
    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = super::openai_compat::retry_after_header(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            let suffix = retry_after
                .map(|ra| format!(" [retry-after:{}]", ra))
                .unwrap_or_default();
            anyhow::bail!("rate limit{}: {}", suffix, body);
        }

        // A model that rejects thinkingConfig 400s the whole request. Strip
        // it, remember this model can't take it, and retry once (recursion
        // is bounded: no_reasoning makes build_generation_config omit the
        // field on the retried payload, so this branch's precondition —
        // thinking_config being present in the payload we just sent — can't
        // re-trigger).
        if status.as_u16() == 400
            && payload
                .generation_config
                .as_ref()
                .is_some_and(|g| g.thinking_config.is_some())
            && {
                let lower = body.to_lowercase();
                lower.contains("reasoning") || lower.contains("thinking")
            }
        {
            tracing::info!(
                "Model '{}' rejected thinking config; retrying without it (flagged no_reasoning)",
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
                    reasoning_effort: options.reasoning_effort.clone(),
                },
            ))
            .await;
        }

        anyhow::bail!("provider error {} at {}: {}", status, gen_url, body);
    }

    let body: GenResp = resp.json().await.context("parse response")?;
    let candidate = body
        .candidates
        .into_iter()
        .next()
        .context("empty candidates")?;
    let parts = candidate.content.map(|c| c.parts).unwrap_or_default();
    let (text, tool_blocks) = parts_to_blocks(parts);
    let usage = UsageInfo {
        input_tokens: body
            .usage_metadata
            .as_ref()
            .and_then(|u| u.prompt_token_count)
            .unwrap_or(0),
        output_tokens: body
            .usage_metadata
            .as_ref()
            .map(|u| u.output_total())
            .unwrap_or(0),
    };
    Ok(finalize_response(
        text,
        tool_blocks,
        candidate.finish_reason.as_deref(),
        usage,
    ))
}

/// Request payload for image generation: a single user turn (prompt plus an
/// optional reference image to edit/restyle) with `responseModalities`
/// requesting an image back. No system instruction, tools, or thinking —
/// Gemini image models reject or ignore all three.
fn build_image_gen_request(prompt: &str, input_image: Option<&ContentBlock>) -> GenReq {
    let mut parts = vec![GenPart {
        text: Some(prompt.to_string()),
        ..Default::default()
    }];
    if let Some(ContentBlock::Image { media_type, data }) = input_image {
        parts.push(GenPart {
            inline_data: Some(GenInlineData {
                mime_type: media_type.clone(),
                data: data.clone(),
            }),
            ..Default::default()
        });
    }
    GenReq {
        contents: vec![GenContent {
            role: Some("user".to_string()),
            parts,
        }],
        system_instruction: None,
        tools: None,
        tool_config: None,
        generation_config: Some(GenGenerationConfig {
            max_output_tokens: None,
            temperature: None,
            thinking_config: None,
            response_modalities: Some(vec!["TEXT".to_string(), "IMAGE".to_string()]),
        }),
    }
}

/// Pull the first inline image (plus any narration text) out of a response's
/// parts. No image part at all means the model refused or isn't an
/// image-generation model — surface its text so the user sees why.
fn extract_generated_image(
    parts: Vec<GenPart>,
    usage: UsageInfo,
    model_id: &str,
) -> anyhow::Result<GeneratedImage> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    let mut text = String::new();
    let mut image: Option<(String, String)> = None;
    for part in parts {
        if part.thought == Some(true) {
            continue;
        }
        if let Some(t) = part.text {
            text.push_str(&t);
        }
        if image.is_none() {
            if let Some(inline) = part.inline_data {
                image = Some((inline.mime_type, inline.data));
            }
        }
    }
    let Some((mime_type, data)) = image else {
        let detail = if text.is_empty() {
            String::new()
        } else {
            format!(
                " Model said: {}",
                text.chars().take(300).collect::<String>()
            )
        };
        anyhow::bail!(
            "model '{}' returned no image — make sure it is an image-generation model \
             (e.g. gemini-2.5-flash-image).{}",
            model_id,
            detail
        );
    };
    let bytes = BASE64
        .decode(data.trim())
        .map_err(|e| anyhow::anyhow!("image base64 decode failed: {}", e))?;
    Ok(GeneratedImage {
        bytes,
        mime_type,
        text,
        usage,
    })
}

/// Generate an image with a Google image model, dispatching on model family:
/// Imagen models (`imagen-*`) use the `:predict` endpoint, while the Gemini
/// image models (`gemini-*-image`) use `:generateContent` with an IMAGE
/// response modality. `input_image` optionally supplies a reference image so
/// the prompt can edit or restyle it (nano-banana-style editing) — supported
/// only by the Gemini image models.
pub async fn generate_image(
    model: &mut ModelRecord,
    prompt: &str,
    input_image: Option<&ContentBlock>,
) -> anyhow::Result<GeneratedImage> {
    let base = model
        .base_url
        .as_deref()
        .or_else(|| provider_base_url("google"))
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta");
    let base = normalize_base_url_str(base);
    // Same legacy-shim guard as `call`: strip an OpenAI-compat "/openai" base.
    let base = base.strip_suffix("/openai").unwrap_or(&base).to_string();

    if is_imagen_model(&model.model_id) {
        return generate_image_imagen(model, &base, prompt, input_image).await;
    }
    generate_image_gemini(model, &base, prompt, input_image).await
}

/// Text-to-image via an Imagen model's `:predict` endpoint. Imagen has no
/// reference-image editing on this API, so a supplied `input_image` is a hard
/// error pointing at the Gemini image models instead.
async fn generate_image_imagen(
    model: &mut ModelRecord,
    base: &str,
    prompt: &str,
    input_image: Option<&ContentBlock>,
) -> anyhow::Result<GeneratedImage> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    if input_image.is_some() {
        anyhow::bail!(
            "model '{}' is an Imagen model, which is text-to-image only and cannot edit a \
             reference image — leave Media empty, or use a Gemini image model \
             (e.g. gemini-2.5-flash-image) for editing",
            model.model_id
        );
    }
    let url = format!("{}/models/{}:predict", base, model.model_id);
    let payload = ImagenReq {
        instances: vec![ImagenInstance {
            prompt: prompt.to_string(),
        }],
        parameters: ImagenParams { sample_count: 1 },
    };
    let resp = HTTP_CLIENT
        .post(&url)
        .header("x-goog-api-key", &model.api_key)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;

    model.rl_snapshot = super::openai_compat::parse_rl_headers("google", resp.headers());
    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = super::openai_compat::retry_after_header(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            let suffix = retry_after
                .map(|ra| format!(" [retry-after:{}]", ra))
                .unwrap_or_default();
            anyhow::bail!("rate limit{}: {}", suffix, body);
        }
        anyhow::bail!("provider error {} at {}: {}", status, url, body);
    }

    let body: ImagenResp = resp.json().await.context("parse response")?;
    let pred =
        body.predictions.into_iter().next().context(
            "Imagen returned no predictions (prompt possibly blocked by safety filters)",
        )?;
    let data = pred
        .bytes_base64_encoded
        .context("Imagen prediction contained no image bytes")?;
    let bytes = BASE64
        .decode(data.trim())
        .map_err(|e| anyhow::anyhow!("image base64 decode failed: {}", e))?;
    let mime_type = pred.mime_type.unwrap_or_else(|| "image/png".to_string());
    Ok(GeneratedImage {
        bytes,
        mime_type,
        text: String::new(),
        usage: UsageInfo::default(),
    })
}

/// Image generation via a Gemini image model's `generateContent` with an IMAGE
/// response modality. `input_image` optionally supplies a reference image to
/// edit or restyle.
async fn generate_image_gemini(
    model: &mut ModelRecord,
    base: &str,
    prompt: &str,
    input_image: Option<&ContentBlock>,
) -> anyhow::Result<GeneratedImage> {
    let url = format!("{}/models/{}:generateContent", base, model.model_id);

    let payload = build_image_gen_request(prompt, input_image);
    let resp = HTTP_CLIENT
        .post(&url)
        .header("x-goog-api-key", &model.api_key)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("HTTP to {}", url))?;

    model.rl_snapshot = super::openai_compat::parse_rl_headers("google", resp.headers());
    if !resp.status().is_success() {
        let status = resp.status();
        let retry_after = super::openai_compat::retry_after_header(resp.headers());
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            let suffix = retry_after
                .map(|ra| format!(" [retry-after:{}]", ra))
                .unwrap_or_default();
            anyhow::bail!("rate limit{}: {}", suffix, body);
        }
        anyhow::bail!("provider error {} at {}: {}", status, url, body);
    }

    let body: GenResp = resp.json().await.context("parse response")?;
    let usage = UsageInfo {
        input_tokens: body
            .usage_metadata
            .as_ref()
            .and_then(|u| u.prompt_token_count)
            .unwrap_or(0),
        output_tokens: body
            .usage_metadata
            .as_ref()
            .map(|u| u.output_total())
            .unwrap_or(0),
    };
    let candidate = body
        .candidates
        .into_iter()
        .next()
        .context("empty candidates (prompt possibly blocked)")?;
    let parts = candidate.content.map(|c| c.parts).unwrap_or_default();
    extract_generated_image(parts, usage, &model.model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_model(thinking_mode: Option<&str>) -> ModelRecord {
        ModelRecord {
            name: "test-google".into(),
            provider: "google".into(),
            model_id: "gemini-test".into(),
            api_key: "k".into(),
            base_url: None,
            timeout_secs: None,
            priority: 1,
            max_tokens: 8192,
            enabled: true,
            role: "".into(),
            thinking_mode: thinking_mode.map(|s| s.to_string()),
            no_reasoning: false,
            status: "available".into(),
            rate_limit_reset_at: None,
            consecutive_errors: 0,
            consecutive_rate_limits: 0,
            total_calls: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            rl_snapshot: Default::default(),
        }
    }

    #[test]
    fn maps_assistant_to_model_and_user_stays_user() {
        let messages = vec![Message::user("hi"), Message::assistant("hello")];
        let contents = contents_from_messages(&messages);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role.as_deref(), Some("user"));
        assert_eq!(contents[1].role.as_deref(), Some("model"));
    }

    #[test]
    fn merges_parallel_tool_results_into_one_user_content() {
        // The actual shape agent/loop.rs produces: parallel tool results are
        // N separate Message::tool_result() calls, not one message with N
        // blocks. Gemini requires them batched into a single Content.
        let messages = vec![
            Message::assistant_with_blocks(vec![
                ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "get_weather".into(),
                    input: json!({"city": "Tokyo"}),
                    signature: None,
                },
                ContentBlock::ToolUse {
                    id: "call-2".into(),
                    name: "get_time".into(),
                    input: json!({"tz": "JST"}),
                    signature: None,
                },
            ]),
            Message::tool_result("call-1", json!({"temp_c": 21})),
            Message::tool_result("call-2", json!({"time": "10:00"})),
        ];
        let contents = contents_from_messages(&messages);
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0].role.as_deref(), Some("model"));
        assert_eq!(contents[0].parts.len(), 2);
        assert_eq!(contents[1].role.as_deref(), Some("user"));
        assert_eq!(contents[1].parts.len(), 2);
        assert!(contents[1]
            .parts
            .iter()
            .all(|p| p.function_response.is_some()));
    }

    #[test]
    fn resolves_function_response_name_via_id_lookup() {
        let messages = vec![
            Message::assistant_with_blocks(vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "get_weather".into(),
                input: json!({}),
                signature: None,
            }]),
            Message::tool_result("call-1", json!({"temp_c": 21})),
        ];
        let contents = contents_from_messages(&messages);
        let fr = contents[1].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr.name, "get_weather");
        assert_eq!(fr.id.as_deref(), Some("call-1"));
    }

    #[test]
    fn thinking_only_message_produces_zero_contents() {
        let messages = vec![Message::assistant_with_blocks(vec![
            ContentBlock::Thinking {
                thinking: "pondering...".into(),
                signature: None,
            },
        ])];
        let contents = contents_from_messages(&messages);
        assert!(contents.is_empty());
    }

    #[test]
    fn to_gen_tools_wraps_all_declarations_in_one_tool_and_sanitizes_schema() {
        let tools = vec![
            ToolDefinition::internal("tool_a", "does a", json!({"x": {"type": "any"}}), vec![]),
            ToolDefinition::internal("tool_b", "does b", json!({}), vec![]),
        ];
        let gen_tools = to_gen_tools(&tools);
        assert_eq!(gen_tools.len(), 1);
        assert_eq!(gen_tools[0].function_declarations.len(), 2);
        let params_a = gen_tools[0].function_declarations[0]
            .parameters
            .as_ref()
            .unwrap();
        assert_eq!(
            params_a
                .pointer("/properties/x/type")
                .and_then(|v| v.as_str()),
            Some("string")
        );
        // Zero-parameter tools must omit `parameters` entirely — Gemini 400s
        // on an empty-properties OBJECT schema.
        assert!(gen_tools[0].function_declarations[1].parameters.is_none());
    }

    #[test]
    fn infer_stop_reason_prefers_tool_use_over_stop_finish_reason() {
        let tool_blocks = vec![ContentBlock::ToolUse {
            id: "1".into(),
            name: "f".into(),
            input: json!({}),
            signature: None,
        }];
        assert_eq!(
            infer_stop_reason(&tool_blocks, Some("STOP")),
            StopReason::ToolUse
        );
        assert_eq!(
            infer_stop_reason(&[], Some("MAX_TOKENS")),
            StopReason::MaxTokens
        );
        assert_eq!(infer_stop_reason(&[], Some("STOP")), StopReason::EndTurn);
        assert_eq!(infer_stop_reason(&[], None), StopReason::EndTurn);
    }

    #[test]
    fn build_generation_config_level_mode() {
        let model = test_model(Some("level"));
        let options = ProviderCallOptions {
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let cfg = build_generation_config(&model, 8192, &options).unwrap();
        let tc = cfg.thinking_config.unwrap();
        assert_eq!(tc.thinking_level.as_deref(), Some("high"));
        assert_eq!(tc.thinking_budget, None);
    }

    #[test]
    fn build_generation_config_budget_mode() {
        let model = test_model(Some("budget"));
        let options = ProviderCallOptions {
            reasoning_effort: Some("low".into()),
            ..Default::default()
        };
        let cfg = build_generation_config(&model, 8192, &options).unwrap();
        let tc = cfg.thinking_config.unwrap();
        assert_eq!(tc.thinking_budget, Some(2048));
        assert_eq!(tc.thinking_level, None);
    }

    #[test]
    fn build_generation_config_drops_budget_under_floor() {
        let model = test_model(Some("budget"));
        // max_tokens - 1024 leaves less than 1024 headroom for a "low" (2048) budget.
        let options = ProviderCallOptions {
            reasoning_effort: Some("low".into()),
            ..Default::default()
        };
        let cfg = build_generation_config(&model, 1500, &options).unwrap();
        assert!(cfg.thinking_config.is_none());
    }

    #[test]
    fn error_frames_deserialize_with_error_field_set() {
        // Without GenResp.error, an `{"error": {...}}` SSE frame would parse
        // as an empty-but-valid response and the failure would be swallowed.
        let parsed: GenResp =
            serde_json::from_str(r#"{"error":{"code":429,"message":"quota"}}"#).unwrap();
        assert!(parsed.error.is_some());
        assert!(parsed.candidates.is_empty());
    }

    #[test]
    fn usage_output_total_includes_thinking_tokens() {
        let u = GenUsage {
            prompt_token_count: Some(10),
            candidates_token_count: Some(100),
            thoughts_token_count: Some(250),
        };
        assert_eq!(u.output_total(), 350);
        let u = GenUsage {
            prompt_token_count: Some(10),
            candidates_token_count: Some(100),
            thoughts_token_count: None,
        };
        assert_eq!(u.output_total(), 100);
    }

    #[test]
    fn image_gen_request_asks_for_image_modality() {
        let req = build_image_gen_request("a red fox", None);
        assert_eq!(req.contents.len(), 1);
        assert_eq!(req.contents[0].role.as_deref(), Some("user"));
        assert_eq!(req.contents[0].parts.len(), 1);
        assert_eq!(req.contents[0].parts[0].text.as_deref(), Some("a red fox"));
        assert!(req.system_instruction.is_none());
        assert!(req.tools.is_none());
        let gc = req.generation_config.unwrap();
        assert_eq!(
            gc.response_modalities,
            Some(vec!["TEXT".to_string(), "IMAGE".to_string()])
        );
        assert!(gc.thinking_config.is_none());
    }

    #[test]
    fn image_gen_request_attaches_reference_image() {
        let reference = ContentBlock::Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        };
        let req = build_image_gen_request("make it night time", Some(&reference));
        let parts = &req.contents[0].parts;
        assert_eq!(parts.len(), 2);
        let inline = parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline.mime_type, "image/png");
        assert_eq!(inline.data, "AAAA");
    }

    #[test]
    fn extract_generated_image_decodes_inline_data_and_keeps_text() {
        use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
        let parts = vec![
            GenPart {
                text: Some("Here is your fox.".into()),
                ..Default::default()
            },
            GenPart {
                inline_data: Some(GenInlineData {
                    mime_type: "image/png".into(),
                    data: BASE64.encode(b"fake-png-bytes"),
                }),
                ..Default::default()
            },
        ];
        let img = extract_generated_image(parts, UsageInfo::default(), "gemini-img").unwrap();
        assert_eq!(img.bytes, b"fake-png-bytes");
        assert_eq!(img.mime_type, "image/png");
        assert_eq!(img.text, "Here is your fox.");
    }

    #[test]
    fn is_imagen_model_routes_only_imagen_ids_to_predict() {
        assert!(is_imagen_model("imagen-4.0-generate-001"));
        assert!(is_imagen_model("imagen-4.0-ultra-generate-001"));
        assert!(is_imagen_model("imagen-3.0-generate-002"));
        assert!(is_imagen_model("IMAGEN-4.0-FAST-GENERATE-001"));
        // Gemini image models stay on generateContent.
        assert!(!is_imagen_model("gemini-2.5-flash-image"));
        assert!(!is_imagen_model("gemini-3.1-flash-image"));
        assert!(!is_imagen_model("gemini-2.5-flash"));
    }

    #[test]
    fn imagen_response_decodes_base64_prediction() {
        let body: ImagenResp = serde_json::from_str(
            r#"{"predictions":[{"mimeType":"image/png","bytesBase64Encoded":"AAAA"}]}"#,
        )
        .unwrap();
        assert_eq!(body.predictions.len(), 1);
        assert_eq!(body.predictions[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(
            body.predictions[0].bytes_base64_encoded.as_deref(),
            Some("AAAA")
        );
    }

    #[test]
    fn extract_generated_image_without_image_part_surfaces_model_text() {
        let parts = vec![GenPart {
            text: Some("I cannot draw that.".into()),
            ..Default::default()
        }];
        let err = extract_generated_image(parts, UsageInfo::default(), "gemini-img")
            .unwrap_err()
            .to_string();
        assert!(err.contains("returned no image"), "got: {err}");
        assert!(err.contains("gemini-img"), "got: {err}");
        assert!(err.contains("I cannot draw that."), "got: {err}");
    }

    #[test]
    fn build_generation_config_suppressed_by_no_reasoning() {
        let mut model = test_model(Some("level"));
        model.no_reasoning = true;
        let options = ProviderCallOptions {
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let cfg = build_generation_config(&model, 8192, &options).unwrap();
        assert!(cfg.thinking_config.is_none());
    }
}
