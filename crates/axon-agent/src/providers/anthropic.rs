use super::types::*;
use super::ProviderCallOptions;
use crate::tools::schema::ToolDefinition;
use anyhow::Context;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Serialize)]
struct AnthReq<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<AnthMsg>,
    // String, or a structured text-block array carrying a cache_control
    // breakpoint. Null when empty so serde skips it.
    #[serde(skip_serializing_if = "Value::is_null")]
    system: Value,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
}
#[derive(Serialize, Deserialize)]
struct AnthMsg {
    role: String,
    content: Value,
}
#[derive(Deserialize)]
struct AnthResp {
    content: Vec<AnthBlock>,
    stop_reason: Option<String>,
    usage: AnthUsage,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}
#[derive(Deserialize)]
struct AnthUsage {
    input_tokens: u32,
    output_tokens: u32,
}

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("axon-agent/1.0")
        .build()
        .expect("build shared Anthropic HTTP client")
});

pub async fn call(
    model: &mut ModelRecord,
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: u32,
    options: ProviderCallOptions,
) -> anyhow::Result<UnifiedResponse> {
    let msgs: Vec<AnthMsg> = messages.iter().map(|m| {
        let content = match &m.content {
            MessageContent::Text(t) => json!(t),
            MessageContent::Blocks(b) => json!(b.iter().map(|blk| match blk {
                ContentBlock::Text { text }       => json!({"type":"text","text":text}),
                ContentBlock::ToolUse { id,name,input } => json!({"type":"tool_use","id":id,"name":name,"input":input}),
                ContentBlock::ToolResult { tool_use_id, content } => json!({"type":"tool_result","tool_use_id":tool_use_id,"content":content}),
                ContentBlock::Image { media_type, data } => json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}}),
            }).collect::<Vec<_>>()),
        };
        AnthMsg { role: m.role.clone(), content }
    }).collect();

    let mut tool_defs: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name":t.name,"description":t.description,
                "input_schema":{"type":"object","properties":t.parameters,"required":t.required}
            })
        })
        .collect();

    // Prompt caching: the system prompt + tool schemas are byte-identical across
    // every iteration of a run (and across runs), so cache that stable prefix.
    // A breakpoint on the last tool caches the whole tools block; a breakpoint on
    // the system block caches tools+system. Anthropic ignores breakpoints below
    // the minimum cacheable size, so this is always safe. (api.anthropic.com is
    // hardcoded below, so this path is always genuine Anthropic.)
    if let Some(last) = tool_defs.last_mut() {
        if let Some(obj) = last.as_object_mut() {
            obj.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
        }
    }
    let system_field = if system.is_empty() {
        Value::Null
    } else {
        json!([{
            "type": "text",
            "text": system,
            "cache_control": {"type": "ephemeral"}
        }])
    };

    // tool_choice only applies when tools are present. Auto is Anthropic's
    // default, so we only emit an explicit value for Required/None.
    let tool_choice = if tool_defs.is_empty() {
        None
    } else {
        match options.tool_choice {
            Some(ToolChoice::Required) => Some(json!({"type": "any"})),
            Some(ToolChoice::None) => Some(json!({"type": "none"})),
            _ => None,
        }
    };

    let resp = HTTP_CLIENT
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &model.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&AnthReq {
            model: &model.model_id,
            max_tokens,
            messages: msgs,
            system: system_field,
            tools: tool_defs,
            temperature: options.temperature,
            tool_choice,
        })
        .send()
        .await
        .context("Anthropic request")?;

    let rl = super::openai_compat::parse_rl_headers("anthropic", resp.headers());
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
        anyhow::bail!("anthropic {}: {}", status, body);
    }
    let body: AnthResp = resp.json().await.context("parse anthropic response")?;
    let mut blocks = vec![];
    for b in body.content {
        match b {
            AnthBlock::Text { text } => blocks.push(ContentBlock::text(text)),
            AnthBlock::ToolUse { id, name, input } => {
                blocks.push(ContentBlock::ToolUse { id, name, input })
            }
        }
    }
    let stop = match body.stop_reason.as_deref() {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    };
    Ok(UnifiedResponse {
        content: blocks,
        stop_reason: stop,
        usage: UsageInfo {
            input_tokens: body.usage.input_tokens,
            output_tokens: body.usage.output_tokens,
        },
    })
}
