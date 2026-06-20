pub mod anthropic;
pub mod ollama;
pub mod openai_compat;
pub mod types;
use crate::tools::schema::ToolDefinition;
pub use types::*;

#[derive(Clone, Default)]
pub struct ProviderCallOptions {
    pub stream_sink: Option<StreamSink>,
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
    match model.provider.as_str() {
        "anthropic" => anthropic::call(model, messages, system, tools, max_tokens, options).await,
        "ollama" => ollama::call(model, messages, system, tools, max_tokens, options).await,
        _ => openai_compat::call(model, messages, system, tools, max_tokens, options).await,
    }
}
