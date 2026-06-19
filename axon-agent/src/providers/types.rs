use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct StreamSink {
    callback: Arc<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>,
    started: Arc<AtomicBool>,
}

impl StreamSink {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        StreamSink {
            callback: Arc::new(move |text| Box::pin(callback(text))),
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn send(&self, text: impl Into<String>) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        self.started.store(true, Ordering::Relaxed);
        (self.callback)(text).await;
    }

    pub fn has_started(&self) -> bool {
        self.started.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    Image {
        media_type: String,
        data: String,
    },
}
impl ContentBlock {
    pub fn text(s: impl Into<String>) -> Self {
        ContentBlock::Text { text: s.into() }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentBlock::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: u32,
    pub output_tokens: u32,
}
impl UsageInfo {
    pub fn total(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RateLimitSnapshot {
    pub req_limit_per_min: Option<u64>,
    pub req_remaining_per_min: Option<u64>,
    pub req_reset_per_min: Option<String>,
    pub tokens_limit_per_min: Option<u64>,
    pub tokens_remaining_per_min: Option<u64>,
    pub tokens_reset_per_min: Option<String>,
    pub req_limit: Option<u64>,
    pub req_remaining: Option<u64>,
    pub req_reset: Option<String>,
    pub tokens_limit: Option<u64>,
    pub tokens_remaining: Option<u64>,
    pub last_updated: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: UsageInfo,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
}

impl UnifiedResponse {
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| b.as_text())
            .collect::<Vec<_>>()
            .join("")
    }
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::ToolUse { id, name, input } = b {
                    Some(ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn has_tool_calls(&self) -> bool {
        self.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: MessageContent,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Message {
            role: "user".into(),
            content: MessageContent::Text(text.into()),
        }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Message {
            role: "assistant".into(),
            content: MessageContent::Text(text.into()),
        }
    }
    pub fn tool_result(id: impl Into<String>, result: serde_json::Value) -> Self {
        let s = match &result {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        Message {
            role: "user".into(),
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: s,
            }]),
        }
    }
    pub fn assistant_with_blocks(blocks: Vec<ContentBlock>) -> Self {
        Message {
            role: "assistant".into(),
            content: MessageContent::Blocks(blocks),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRecord {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub api_key: String,
    pub base_url: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    pub priority: i32,
    pub max_tokens: u32,
    pub enabled: bool,
    pub role: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub rate_limit_reset_at: Option<String>,
    #[serde(default)]
    pub consecutive_errors: u32,
    #[serde(default)]
    pub total_calls: u64,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub rl_snapshot: RateLimitSnapshot,
}
impl ModelRecord {
    pub fn is_available(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.status.as_str() {
            "rate_limited" | "unavailable" => self
                .rate_limit_reset_at
                .as_ref()
                .and_then(|r| chrono::DateTime::parse_from_rfc3339(r).ok())
                .map(|r| chrono::Utc::now() > r)
                .unwrap_or(false),
            _ => true,
        }
    }
    pub fn mark_rate_limited(&mut self, cooldown: i64) {
        self.status = "rate_limited".into();
        self.rate_limit_reset_at =
            Some((chrono::Utc::now() + chrono::Duration::minutes(cooldown)).to_rfc3339());
        tracing::error!("{} rate-limited for {}m", self.name, cooldown);
    }
    pub fn mark_error(&mut self, threshold: u32, cooldown: i64) {
        self.consecutive_errors += 1;
        if self.consecutive_errors >= threshold {
            self.status = "unavailable".into();
            self.rate_limit_reset_at =
                Some((chrono::Utc::now() + chrono::Duration::minutes(cooldown)).to_rfc3339());
            tracing::error!(
                "{} unavailable after {} errors; will retry in {}m",
                self.name,
                threshold,
                cooldown
            );
        }
    }
    pub fn mark_success(&mut self, i: u32, o: u32) {
        self.consecutive_errors = 0;
        self.status = "available".into();
        self.rate_limit_reset_at = None;
        self.total_calls += 1;
        self.total_input_tokens += i as u64;
        self.total_output_tokens += o as u64;
    }
}

pub fn normalize_provider_name(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "gemini" => "google".to_string(),
        _ => normalized,
    }
}

pub fn normalize_base_url(base_url: Option<String>) -> Option<String> {
    base_url
        .map(|url| normalize_base_url_str(&url))
        .filter(|url| !url.is_empty())
}

pub fn normalize_base_url_str(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/chat/completions")
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_string()
}

pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", normalize_base_url_str(base_url))
}

pub fn provider_base_url(p: &str) -> Option<&'static str> {
    let normalized = normalize_provider_name(p);
    match normalized.as_str() {
        "google" => Some("https://generativelanguage.googleapis.com/v1beta/openai/"),
        "groq" => Some("https://api.groq.com/openai/v1"),
        "cerebras" => Some("https://api.cerebras.ai/v1"),
        "nvidia" => Some("https://integrate.api.nvidia.com/v1"),
        "openrouter" => Some("https://openrouter.ai/api/v1"),
        "ollama" => Some("http://localhost:11434/v1"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_provider_aliases() {
        assert_eq!(normalize_provider_name("gemini"), "google");
        assert_eq!(
            provider_base_url("gemini"),
            Some("https://generativelanguage.googleapis.com/v1beta/openai/")
        );
    }

    #[test]
    fn strips_chat_completions_suffix_from_base_url() {
        assert_eq!(
            normalize_base_url_str("https://integrate.api.nvidia.com/v1/chat/completions"),
            "https://integrate.api.nvidia.com/v1"
        );
        assert_eq!(
            chat_completions_url("https://integrate.api.nvidia.com/v1/chat/completions"),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
    }
}
