pub mod anthropic;
pub mod google;
pub mod ollama;
pub mod openai_compat;
pub mod types;
use crate::tools::schema::ToolDefinition;
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
