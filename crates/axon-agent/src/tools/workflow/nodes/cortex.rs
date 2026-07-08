use crate::providers::types::ContentBlock;
use crate::state::AppState;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use once_cell::sync::Lazy;
use serde_json::Value;

static MEDIA_HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("axon-agent/1.0")
        .build()
        .expect("build shared media-fetch HTTP client")
});

/// Resolve the Cortex node's `media` config field (already expression-
/// resolved by the workflow engine) into a provider-ready `ContentBlock::Image`.
/// Accepts a literal http(s) URL, a raw/data-URI base64 string, a local file
/// path, or the HTTP node's binary object shape `{ body, local_path, mime_type }`.
async fn resolve_media_to_image_block(media: &Value) -> Result<ContentBlock, String> {
    let (raw, declared_mime): (String, Option<String>) = match media {
        Value::String(s) if !s.trim().is_empty() => (s.trim().to_string(), None),
        Value::Object(obj) => {
            let mime = obj
                .get("mime_type")
                .and_then(|v| v.as_str())
                .map(String::from);
            if let Some(body) = obj
                .get("body")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                (body.to_string(), mime)
            } else if let Some(lp) = obj
                .get("local_path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                (lp.to_string(), mime)
            } else {
                return Err("Media resolved to an object with neither 'body' nor 'local_path' — expected an image URL, base64 string, or an upstream node's binary output.".to_string());
            }
        }
        _ => {
            return Err(
                "Media is empty. Provide an image URL or a reference to an upstream node's output (e.g. {{ $node[\"HttpNode\"].data.binary }})."
                    .to_string(),
            )
        }
    };

    if raw.starts_with("http://") || raw.starts_with("https://") {
        let resp = MEDIA_HTTP_CLIENT
            .get(&raw)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch media URL '{}': {}", raw, e))?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).to_string());
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("Failed to read media response body: {}", e))?
            .to_vec();
        image::load_from_memory(&bytes)
            .map_err(|e| format!("Media at '{}' does not look like a valid image: {}", raw, e))?;
        let media_type = content_type
            .or_else(|| image::guess_format(&bytes).ok().map(|f| f.to_mime_type().to_string()))
            .unwrap_or_else(|| "application/octet-stream".to_string());
        return Ok(ContentBlock::Image {
            media_type,
            data: BASE64.encode(&bytes),
        });
    }

    if crate::tools::image_tool::looks_like_base64_image(&raw) {
        let b64_clean = raw
            .find(',')
            .map_or(raw.as_str(), |i| &raw[i + 1..])
            .trim()
            .to_string();
        let bytes = BASE64
            .decode(&b64_clean)
            .map_err(|e| format!("Media base64 decode failed: {}", e))?;
        image::load_from_memory(&bytes)
            .map_err(|e| format!("Media does not look like a valid image: {}", e))?;
        let media_type = declared_mime
            .or_else(|| image::guess_format(&bytes).ok().map(|f| f.to_mime_type().to_string()))
            .unwrap_or_else(|| "image/jpeg".to_string());
        return Ok(ContentBlock::Image {
            media_type,
            data: b64_clean,
        });
    }

    // Otherwise: a local file path (e.g. binary.local_path from an upstream
    // HTTP/staging node).
    let bytes = std::fs::read(&raw).map_err(|e| format!("Failed to read media file '{}': {}", raw, e))?;
    image::load_from_memory(&bytes)
        .map_err(|e| format!("Media file '{}' does not look like a valid image: {}", raw, e))?;
    let media_type = declared_mime
        .or_else(|| image::guess_format(&bytes).ok().map(|f| f.to_mime_type().to_string()))
        .unwrap_or_else(|| "image/jpeg".to_string());
    Ok(ContentBlock::Image {
        media_type,
        data: BASE64.encode(&bytes),
    })
}

pub(crate) fn execute_cortex_node<'a>(
    config: &'a Value,
    state: &'a AppState,
    workflow_id: &'a str,
    node_id: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>> {
    Box::pin(async move {
        // Extract stimulus — handle any value type since interpolation may return Object/Array
        let stimulus = match config.get("stimulus") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        };

        // Extract cortex — same resilient extraction
        let cortex = match config.get("cortex") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        };

        if stimulus.is_empty() {
            return Err("Cortex node: stimulus (User Prompt) is empty".to_string());
        }

        // Extract optional model selection
        let selected_model = config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Per-node memory toggle (default ON for backwards compatibility).
        let memory_enabled = config
            .get("memory_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // Per-node memory size, expressed as conversation INTERACTIONS (a pair of
        // one user message + one assistant reply), matching n8n Simple Memory.
        // Accept a number or a numeric string (interpolation may stringify it).
        // Defaults to 10 pairs; clamped to >= 1.
        let memory_pairs = config
            .get("memory_window")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            })
            .map(|n| n.max(1) as usize)
            .unwrap_or(10);

        // Extract optional tools selection
        let selected_tools: Vec<String> = config
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Build system prompt with optional tool injection
        let mut system_prompt_text = cortex.clone();
        if !selected_tools.is_empty() {
            let tool_section = format!(
                "\n\n[TOOL RESTRICTION]\nYou have access ONLY to the following tools: {}. \
                 Use only these tools to fulfill the user's request. Do not attempt to use any other tools.",
                selected_tools.join(", ")
            );
            system_prompt_text.push_str(&tool_section);
        }

        if let Some(ref model) = selected_model {
            tracing::info!(
                "Cortex node: user selected model '{}' for workflow {}",
                model,
                workflow_id
            );
        }

        // Mode: "text" (default) or "image". Video is not supported — no
        // configured provider path can accept video input today.
        let mode = config
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();

        let image_content: Option<ContentBlock> = if mode == "image" {
            let Some(model_name) = selected_model.as_deref() else {
                return Err("Cortex node (Image mode): a Model is required — pick a model tagged role=\"image_model\" (auto-select is not available for images).".to_string());
            };
            match crate::router::model_router::model_role_by_name(&state.router, model_name).await
            {
                Some(role) if role == "image_model" => {}
                Some(other) => {
                    return Err(format!(
                        "Cortex node (Image mode): model '{}' is tagged role=\"{}\", not \"image_model\". Tag it role=\"image_model\" on the Models page, or pick a different model.",
                        model_name,
                        if other.is_empty() { "<general>" } else { &other }
                    ));
                }
                None => {
                    return Err(format!(
                        "Cortex node (Image mode): model '{}' was not found.",
                        model_name
                    ))
                }
            }
            let media_val = config.get("media").cloned().unwrap_or(Value::Null);
            Some(
                resolve_media_to_image_block(&media_val)
                    .await
                    .map_err(|e| format!("Cortex node (Image mode): {}", e))?,
            )
        } else {
            None
        };

        let system_prompt = if system_prompt_text.is_empty() {
            None
        } else {
            Some(system_prompt_text.as_str())
        };

        // Isolated session PER NODE (not per workflow) so each Cortex node keeps
        // its own dedicated, persistent conversation memory — multiple Cortex nodes
        // in the same workflow no longer share one history.
        let session = format!("wf:{}:node:{}", workflow_id, node_id);
        let mut ctx = crate::agent::RunContext::new(
            &stimulus,
            "workflow",
            Some(&session),
            None,
            None,
            None,
            system_prompt,
        );
        ctx.preferred_model = selected_model;
        ctx.memory_enabled = memory_enabled;
        // Short-term memory caps individual messages, so a pair == 2 rows.
        ctx.memory_window = Some(memory_pairs.saturating_mul(2));
        // Each Cortex node's memory is fully isolated: it starts blank and only
        // ever sees its own sliding window of recent turns. Never the global
        // long-term memory or the system-wide tool-observation log, and it does
        // not write its outputs back into that shared long-term store.
        ctx.isolated_memory = true;

        if mode == "image" {
            ctx.preferred_role = Some("image_model".to_string());
            ctx.image_content = image_content;
        }

        if !selected_tools.is_empty() {
            ctx.allowed_tools = Some(selected_tools);
        }

        let agent_response = crate::agent::run_task(&stimulus, state, ctx)
            .await
            .map_err(|e| format!("Cortex agent error: {}", e))?;

        Ok(serde_json::json!({
            "output": agent_response
        }))
    })
}
