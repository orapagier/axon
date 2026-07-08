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
    /// Model reasoning returned by providers with thinking enabled. Kept in
    /// the in-run history so Anthropic can echo it back on multi-turn tool use
    /// (the API requires signed thinking blocks to be replayed verbatim).
    /// Never part of user-visible text (`as_text` skips it) and never
    /// forwarded to providers that don't understand it.
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
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
    pub fn user_with_blocks(blocks: Vec<ContentBlock>) -> Self {
        Message {
            role: "user".into(),
            content: MessageContent::Blocks(blocks),
        }
    }
}

/// How the model is allowed to use tools on a single call. Mapped to each
/// provider's native wire format (`tool_choice` for OpenAI/Anthropic).
///   - `Auto`     : model decides (default when tools are present)
///   - `Required` : model MUST call at least one tool (used to defeat false refusals)
///   - `None`     : model must not call a tool this turn
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    Required,
    None,
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
    /// Anthropic-provider thinking mode (models.toml, optional):
    /// "adaptive" (Claude 4.6+), "budget" (older Claude models that take a
    /// thinking token budget), unset/"off" = never send a thinking param.
    #[serde(default)]
    pub thinking_mode: Option<String>,
    /// Set at runtime when a provider rejected `reasoning_effort` with a 400;
    /// the field is omitted for this model from then on (process lifetime).
    #[serde(default)]
    pub no_reasoning: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub rate_limit_reset_at: Option<String>,
    #[serde(default)]
    pub consecutive_errors: u32,
    /// Consecutive rate-limit (429) hits with no intervening success. Tracked for
    /// telemetry/UI only — cooldown length is now window-based (per-minute/hour/day),
    /// not escalated by this count. Reset to 0 on the next success.
    #[serde(default)]
    pub consecutive_rate_limits: u32,
    #[serde(default)]
    pub total_calls: u64,
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub total_output_tokens: u64,
    #[serde(default)]
    pub rl_snapshot: RateLimitSnapshot,
}
/// Which rate-limit window a 429 most likely refers to, inferred from the error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitWindow {
    PerMinute,
    Hourly,
    Daily,
    Unknown,
}

/// Structured hint extracted from a provider's 429: which window was hit and, when
/// the provider told us, exactly how many seconds until it resets.
#[derive(Debug, Clone)]
pub struct RateLimitHint {
    pub window: RateLimitWindow,
    pub explicit_secs: Option<u64>,
}

/// Parse a duration like `57s`, `1m30s`, `2m`, `1h2m3s`, `0.5s`, `500ms`, or a
/// bare number (seconds) into whole seconds, rounded up.
pub fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    // Bare number → seconds.
    if let Ok(n) = s.parse::<f64>() {
        return (n.is_finite() && n >= 0.0).then_some(n.ceil() as u64);
    }
    let mut total = 0f64;
    let mut num = String::new();
    let mut found = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else if c == 'h' {
            if let Ok(v) = num.parse::<f64>() {
                total += v * 3600.0;
                found = true;
            }
            num.clear();
        } else if c == 'm' {
            if chars.peek() == Some(&'s') {
                chars.next(); // 'ms'
                if let Ok(v) = num.parse::<f64>() {
                    total += v / 1000.0;
                    found = true;
                }
            } else if let Ok(v) = num.parse::<f64>() {
                total += v * 60.0;
                found = true;
            }
            num.clear();
        } else if c == 's' {
            if let Ok(v) = num.parse::<f64>() {
                total += v;
                found = true;
            }
            num.clear();
        } else {
            num.clear();
        }
    }
    found.then_some(total.ceil() as u64)
}

fn substr_between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)? + i;
    Some(&s[i..j])
}

/// Pull an explicit reset delay (seconds) from a lowercased 429 body, most
/// authoritative source first.
fn extract_explicit_reset_secs(lower: &str) -> Option<u64> {
    // 1) Retry-After header we folded in as `[retry-after:VALUE]`.
    if let Some(v) = substr_between(lower, "[retry-after:", "]") {
        if let Some(s) = parse_duration_secs(v).filter(|&s| s > 0) {
            return Some(s);
        }
    }
    // 2) Gemini-style `"retryDelay": "57s"`. After splitting off the key text, the
    // quotes in the remainder are: [0]=key's closing quote, [1]=value open,
    // [2]=value close — so the value sits between quotes 1 and 2.
    if let Some(rest) = lower.split("retrydelay").nth(1) {
        let quotes: Vec<usize> = rest.match_indices('"').map(|(i, _)| i).collect();
        if quotes.len() >= 3 {
            let val = &rest[quotes[1] + 1..quotes[2]];
            if let Some(s) = parse_duration_secs(val).filter(|&s| s > 0) {
                return Some(s);
            }
        }
    }
    // 3) Inline "try again in <dur>" / "retry after <dur>".
    for marker in [
        "try again in ",
        "try again after ",
        "retry after ",
        "please retry after ",
    ] {
        if let Some(idx) = lower.find(marker) {
            let tail = &lower[idx + marker.len()..];
            let end = tail
                .find(|c: char| matches!(c, '.' | ',' | ')' | '\n' | '"'))
                .unwrap_or(tail.len());
            if let Some(s) = parse_duration_secs(&tail[..end]).filter(|&s| s > 0) {
                return Some(s);
            }
        }
    }
    None
}

/// Infer the rate-limit window and any explicit reset delay from a 429 error string.
pub fn parse_rate_limit_hint(err: &str) -> RateLimitHint {
    let lower = err.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|p| lower.contains(p));

    let window = if has(&[
        "per day",
        "per-day",
        "perday",
        "/day",
        "requests per day",
        "tokens per day",
        "daily",
        "rpd",
    ]) {
        RateLimitWindow::Daily
    } else if has(&["per hour", "per-hour", "perhour", "/hour", "hourly", "rph"]) {
        RateLimitWindow::Hourly
    } else if has(&[
        "per minute",
        "per-minute",
        "perminute",
        "/min",
        "per min",
        "rpm",
        "tpm",
    ]) {
        RateLimitWindow::PerMinute
    } else {
        RateLimitWindow::Unknown
    };

    RateLimitHint {
        window,
        explicit_secs: extract_explicit_reset_secs(&lower),
    }
}

/// Seconds from now until the next UTC midnight — a neutral, self-correcting
/// estimate of when a daily free-tier quota rolls over.
pub fn secs_until_next_utc_midnight() -> i64 {
    use chrono::Timelike;
    let secs_into_day = chrono::Utc::now().num_seconds_from_midnight() as i64;
    (86_400 - secs_into_day).clamp(1, 86_400)
}

/// Decide how long (seconds) to quarantine a model after a 429, by window.
///
/// An explicit provider reset (retry-after / retryDelay) always wins when given;
/// otherwise a flat per-window default is used — no exponential escalation:
///
/// - **PerMinute / Unknown:** the explicit delay (+3s buffer), else a flat 60s.
/// - **Hourly:** the explicit delay, else 60 minutes.
/// - **Daily:** the explicit delay if it's at least an hour, else until the next
///   UTC midnight. Provider-specific reset times are honored through the explicit
///   signal; UTC midnight is only the neutral fallback when none is given.
pub fn rate_limit_cooldown_secs(hint: &RateLimitHint, secs_until_utc_midnight: i64) -> i64 {
    const HARD_MAX: i64 = 24 * 3600;
    const FLOOR: i64 = 5;
    const PER_MINUTE_SECS: i64 = 60;
    const HOURLY_SECS: i64 = 3600;
    match hint.window {
        RateLimitWindow::Daily => hint
            .explicit_secs
            .map(|s| s as i64)
            .filter(|&s| s >= HOURLY_SECS)
            .unwrap_or_else(|| secs_until_utc_midnight.max(HOURLY_SECS))
            .clamp(HOURLY_SECS, HARD_MAX),
        RateLimitWindow::Hourly => hint
            .explicit_secs
            .map(|s| s as i64)
            .filter(|&s| s >= 60)
            .unwrap_or(HOURLY_SECS)
            .clamp(60, HARD_MAX),
        RateLimitWindow::PerMinute | RateLimitWindow::Unknown => hint
            .explicit_secs
            .map(|s| (s as i64).saturating_add(3))
            .unwrap_or(PER_MINUTE_SECS)
            .clamp(FLOOR, HARD_MAX),
    }
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
    /// Quarantine a model after a 429. The cooldown comes from the parsed `hint`:
    /// an explicit provider-supplied reset is honored; otherwise a flat per-window
    /// default applies (per-minute ≈ 60s, hourly ≈ 60m, daily until next midnight).
    /// `consecutive_rate_limits` is still tracked for telemetry but no longer
    /// lengthens the cooldown.
    pub fn mark_rate_limited(&mut self, hint: &RateLimitHint) {
        self.consecutive_rate_limits = self.consecutive_rate_limits.saturating_add(1);
        let secs = rate_limit_cooldown_secs(hint, secs_until_next_utc_midnight());
        self.status = "rate_limited".into();
        self.rate_limit_reset_at =
            Some((chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339());
        tracing::error!(
            "{} rate-limited for {}s ({:?})",
            self.name,
            secs,
            hint.window
        );
    }
    /// Record a non-rate-limit error. After `threshold` consecutive errors the
    /// model is parked as unavailable until the next UTC midnight ("rest of the
    /// day"), matching the daily rate-limit policy. A genuine success clears the
    /// counter and reinstates the model immediately via `mark_success`.
    pub fn mark_error(&mut self, threshold: u32) {
        self.consecutive_errors += 1;
        if self.consecutive_errors >= threshold {
            self.status = "unavailable".into();
            let secs = secs_until_next_utc_midnight();
            self.rate_limit_reset_at =
                Some((chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339());
            tracing::error!(
                "{} unavailable after {} consecutive errors; parked until midnight (~{}s)",
                self.name,
                threshold,
                secs
            );
        }
    }
    pub fn mark_success(&mut self, i: u32, o: u32) {
        self.consecutive_errors = 0;
        self.consecutive_rate_limits = 0;
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

/// Canonicalize a model's router role: trim, lowercase, and collapse runs of
/// whitespace/hyphens into single underscores. This way a hand-typed value like
/// `"Quality Checker"` or `"paid-model"` matches the role names the engine
/// requests internally (e.g. `quality_checker`, `paid_model`) instead of
/// silently never matching. Empty/general stays empty.
pub fn normalize_role(role: &str) -> String {
    role.trim()
        .to_ascii_lowercase()
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
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
    fn normalizes_roles() {
        assert_eq!(normalize_role("Quality Checker"), "quality_checker");
        assert_eq!(normalize_role(" paid-model "), "paid_model");
        assert_eq!(normalize_role("complex_tasks"), "complex_tasks");
        assert_eq!(normalize_role("  Router  "), "router");
        assert_eq!(normalize_role(""), "");
        assert_eq!(normalize_role("memory   compressor"), "memory_compressor");
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration_secs("57s"), Some(57));
        assert_eq!(parse_duration_secs("2m30s"), Some(150));
        assert_eq!(parse_duration_secs("1h2m3s"), Some(3723));
        assert_eq!(parse_duration_secs("1.5s"), Some(2)); // rounds up
        assert_eq!(parse_duration_secs("500ms"), Some(1)); // rounds up
        assert_eq!(parse_duration_secs("20"), Some(20)); // bare = seconds
        assert_eq!(parse_duration_secs("  3m "), Some(180));
        assert_eq!(parse_duration_secs("garbage"), None);
        assert_eq!(parse_duration_secs(""), None);
    }

    #[test]
    fn classifies_rate_limit_windows() {
        assert_eq!(
            parse_rate_limit_hint("rate limit: Quota exceeded ... GenerateRequestsPerDay").window,
            RateLimitWindow::Daily
        );
        assert_eq!(
            parse_rate_limit_hint("429 requests per minute exceeded").window,
            RateLimitWindow::PerMinute
        );
        assert_eq!(
            parse_rate_limit_hint("rate limit: 100 per hour").window,
            RateLimitWindow::Hourly
        );
        assert_eq!(
            parse_rate_limit_hint("rate limit: something opaque").window,
            RateLimitWindow::Unknown
        );
    }

    #[test]
    fn extracts_explicit_reset_from_body() {
        // Retry-After header we fold in.
        assert_eq!(
            parse_rate_limit_hint("rate limit [retry-after:42]: too many").explicit_secs,
            Some(42)
        );
        // Gemini retryDelay.
        assert_eq!(
            parse_rate_limit_hint(r#"rate limit: {"retryDelay": "17s"}"#).explicit_secs,
            Some(17)
        );
        // Inline phrasing.
        assert_eq!(
            parse_rate_limit_hint("rate limit: Please try again in 2m30s.").explicit_secs,
            Some(150)
        );
    }

    #[test]
    fn cooldown_honors_window_and_explicit_reset() {
        let until_midnight = 5 * 3600;
        // Per-minute with explicit 30s reset → ~30s (+3s buffer), honored over the flat default.
        let h = RateLimitHint {
            window: RateLimitWindow::PerMinute,
            explicit_secs: Some(30),
        };
        assert_eq!(rate_limit_cooldown_secs(&h, until_midnight), 33);
        // Per-minute with no reset info → flat 60s, no exponential escalation.
        let h = RateLimitHint {
            window: RateLimitWindow::PerMinute,
            explicit_secs: None,
        };
        assert_eq!(rate_limit_cooldown_secs(&h, until_midnight), 60);
        // Hourly with no reset info → a flat 60 minutes.
        let h = RateLimitHint {
            window: RateLimitWindow::Hourly,
            explicit_secs: None,
        };
        assert_eq!(rate_limit_cooldown_secs(&h, until_midnight), 3600);
        // Daily with no explicit reset → wait until UTC midnight (>= 1h).
        let h = RateLimitHint {
            window: RateLimitWindow::Daily,
            explicit_secs: None,
        };
        assert_eq!(rate_limit_cooldown_secs(&h, until_midnight), until_midnight);
        // Daily ignores a bogus short "retry in 30s" and still parks for hours.
        let h = RateLimitHint {
            window: RateLimitWindow::Daily,
            explicit_secs: Some(30),
        };
        assert_eq!(rate_limit_cooldown_secs(&h, until_midnight), until_midnight);
        // Unknown with no reset info → treated like per-minute (flat 60s).
        let h = RateLimitHint {
            window: RateLimitWindow::Unknown,
            explicit_secs: None,
        };
        assert_eq!(rate_limit_cooldown_secs(&h, until_midnight), 60);
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
