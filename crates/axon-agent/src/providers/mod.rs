pub mod anthropic;
pub mod google;
pub mod list;
pub mod ollama;
pub mod openai_compat;
pub mod types;
use crate::tools::schema::ToolDefinition;
pub use list::{list_available_models, ModelChoice};
pub use types::*;

#[derive(Clone, Default)]
pub struct ProviderCallOptions {
    pub stream_sink: Option<StreamSink>,
    /// Sampling temperature. `None` = leave provider default. Deterministic
    /// sub-tasks (routing, quality gate) pass `Some(0.0)`.
    pub temperature: Option<f32>,
    /// Force/suppress tool use. `None` = `Auto` when tools are present.
    pub tool_choice: Option<ToolChoice>,
    /// Reasoning effort for reasoning-capable models ("low"|"medium"|"high").
    /// `None` = omit the field entirely (safe for non-reasoning providers).
    pub reasoning_effort: Option<String>,
}

pub async fn call_provider(
    model: &mut ModelRecord,
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: u32,
) -> anyhow::Result<UnifiedResponse> {
    call_provider_with_options(
        model,
        messages,
        system,
        tools,
        max_tokens,
        ProviderCallOptions::default(),
    )
    .await
}

/// Dispatch an image-generation request to the model's provider. Google goes
/// through `generateContent` with image response modality (and accepts an
/// optional reference image to edit); everything else is treated as
/// OpenAI-compatible and uses `/images/generations` (text prompt only).
pub async fn generate_image_with_provider(
    model: &mut ModelRecord,
    prompt: &str,
    input_image: Option<&ContentBlock>,
) -> anyhow::Result<GeneratedImage> {
    match normalize_provider_name(&model.provider).as_str() {
        "google" => google::generate_image(model, prompt, input_image).await,
        "anthropic" | "ollama" => anyhow::bail!(
            "provider '{}' cannot generate images; use a Google (Gemini) image model or an \
             OpenAI-compatible host with /images/generations",
            model.provider
        ),
        _ => {
            if input_image.is_some() {
                anyhow::bail!(
                    "a reference image (Media) is only supported with Google (Gemini) image \
                     models; leave Media empty for OpenAI-compatible providers"
                );
            }
            openai_compat::generate_image(model, prompt).await
        }
    }
}

pub async fn call_provider_with_options(
    model: &mut ModelRecord,
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: u32,
    options: ProviderCallOptions,
) -> anyhow::Result<UnifiedResponse> {
    match normalize_provider_name(&model.provider).as_str() {
        "anthropic" => anthropic::call(model, messages, system, tools, max_tokens, options).await,
        "google" => google::call(model, messages, system, tools, max_tokens, options).await,
        "ollama" => ollama::call(model, messages, system, tools, max_tokens, options).await,
        _ => openai_compat::call(model, messages, system, tools, max_tokens, options).await,
    }
}
