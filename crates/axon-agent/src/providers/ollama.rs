use super::types::*;
use super::ProviderCallOptions;
use crate::tools::schema::ToolDefinition;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Serialize)]
struct OllamaReq {
    model: String,
    messages: Vec<OllamaMsg>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Value>,
    /// Ollama reasoning toggle. Thinking output arrives in the response's
    /// `message.thinking` field, which we ignore — it never reaches the user.
    /// Non-reasoning models ignore the flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OllamaMsg {
    role: String,
    #[serde(default)]
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaTc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OllamaTc {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    function: OllamaFn,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OllamaFn {
    name: String,
    arguments: Value,
}

#[derive(Debug, Deserialize)]
struct OllamaResp {
    message: OllamaMsg,
    prompt_eval_count: Option<u32>,
    eval_count: Option<u32>,
}

fn to_ollama_msgs(messages: &[Message], system: &str) -> Vec<OllamaMsg> {
    let mut out = vec![];
    if !system.is_empty() {
        out.push(OllamaMsg {
            role: "system".into(),
            content: system.to_string(),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    for m in messages {
        match &m.content {
            MessageContent::Text(t) => out.push(OllamaMsg {
                role: m.role.clone(),
                content: t.clone(),
                tool_calls: None,
                tool_call_id: None,
            }),
            MessageContent::Blocks(blocks) => {
                let text = blocks
                    .iter()
                    .filter_map(|b| b.as_text())
                    .collect::<Vec<_>>()
                    .join("");
                let tcs: Vec<OllamaTc> = blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::ToolUse {
                            id, name, input, ..
                        } = b
                        {
                            Some(OllamaTc {
                                id: Some(id.clone()),
                                function: OllamaFn {
                                    name: name.clone(),
                                    arguments: input.clone(),
                                },
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                let m_out = OllamaMsg {
                    role: m.role.clone(),
                    content: text,
                    tool_calls: if tcs.is_empty() { None } else { Some(tcs) },
                    tool_call_id: None,
                };

                // If it's a tool result turn, Ollama expects individual messages for each result
                let mut tool_results = vec![];
                for b in blocks {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } = b
                    {
                        tool_results.push(OllamaMsg {
                            role: "tool".into(),
                            content: content.clone(),
                            tool_calls: None,
                            tool_call_id: Some(tool_use_id.clone()),
                        });
                    }
                }

                if !tool_results.is_empty() {
                    out.extend(tool_results);
                } else {
                    out.push(m_out);
                }
            }
        }
    }
    out
}

pub async fn call(
    model: &mut ModelRecord,
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    _max_tokens: u32,
    options: ProviderCallOptions,
) -> anyhow::Result<UnifiedResponse> {
    let base = model
        .base_url
        .as_deref()
        .unwrap_or("http://localhost:11434");
    let url = format!("{}/api/chat", base.trim_end_matches('/'));

    let ollama_tools = if tools.is_empty() {
        None
    } else {
        Some(
            tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": {
                                "type": "object",
                                "properties": t.parameters,
                                "required": t.required
                            }
                        }
                    })
                })
                .collect(),
        )
    };

    let ollama_opts = options.temperature.map(|t| json!({ "temperature": t }));

    // Omit for models that already rejected the field once this process
    // (see the 400 handling below — mirrors openai_compat.rs's no_reasoning).
    let think = if model.no_reasoning {
        None
    } else {
        options.reasoning_effort.as_ref().map(|_| true)
    };

    let payload = OllamaReq {
        model: model.model_id.clone(),
        messages: to_ollama_msgs(messages, system),
        stream: false,
        tools: ollama_tools,
        options: ollama_opts,
        think,
    };

    let mut client_builder = reqwest::Client::builder();
    client_builder = client_builder.user_agent("axon-agent/1.0");

    let client = client_builder
        .build()
        .context("Failed to build HTTP client")?;
    let mut request = client.post(&url);

    if !model.api_key.is_empty() && !model.api_key.starts_with("${") {
        request = request.header("Authorization", format!("Bearer {}", model.api_key));
    }

    let resp = request
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("Ollama Cloud HTTP request failed for {}", url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "Unavailable".to_string());

        // Some models (e.g. custom/fine-tuned GGUFs without a thinking-capable
        // template) reject the `think` field outright with a 400. Strip it,
        // remember this model can't take it, and retry once (recursion is
        // bounded: no_reasoning makes the retried payload omit the field, so
        // this branch can't re-trigger).
        if status.as_u16() == 400 && think.is_some() && body.to_lowercase().contains("thinking") {
            tracing::info!(
                "Model '{}' rejected think param; retrying without it (flagged no_reasoning)",
                model.name
            );
            model.no_reasoning = true;
            return Box::pin(call(
                model,
                messages,
                system,
                tools,
                _max_tokens,
                ProviderCallOptions {
                    stream_sink: None,
                    temperature: options.temperature,
                    tool_choice: options.tool_choice,
                    reasoning_effort: None,
                },
            ))
            .await;
        }

        anyhow::bail!("Ollama Cloud API error ({}): {}", status, body);
    }

    let body: OllamaResp = resp
        .json()
        .await
        .with_context(|| "Failed to parse Ollama Cloud response JSON")?;
    let mut blocks = vec![];

    let content = body.message.content;
    if !content.is_empty() {
        blocks.push(ContentBlock::text(content));
    }

    let mut stop = StopReason::EndTurn;
    if let Some(tcs) = body.message.tool_calls {
        for tc in tcs {
            blocks.push(ContentBlock::ToolUse {
                id: tc
                    .id
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()[..8].to_string()),
                name: tc.function.name,
                input: tc.function.arguments,
                signature: None,
            });
        }
        stop = StopReason::ToolUse;
    }

    Ok(UnifiedResponse {
        content: blocks,
        stop_reason: stop,
        usage: UsageInfo {
            input_tokens: body.prompt_eval_count.unwrap_or(0),
            output_tokens: body.eval_count.unwrap_or(0),
        },
    })
}
