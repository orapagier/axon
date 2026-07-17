//! Shared text-to-speech: one OpenAI-compatible `/audio/speech` client powering
//! spoken agent replies on the dashboard Chat page. Configured via the `tts.*`
//! settings (mirroring the `stt.*` voice-input group):
//!   * Groq   — https://api.groq.com/openai/v1 + playai-tts + voice Fritz-PlayAI
//!   * OpenAI — https://api.openai.com/v1 + gpt-4o-mini-tts + voice alloy
//!
//! Also owns the TTS model listing that feeds the Settings page `tts.model`
//! dropdown: `GET {base_url}/models` filtered down to speech-synthesis ids,
//! cached in `provider_model_cache` under the provider key `"tts"`.
//!
//! TTS is strictly best-effort: every caller falls back to its non-audio
//! behavior (the browser's speechSynthesis, or plain text) when this module
//! reports "not configured" or any upstream failure — rate limits included.

use crate::config::RuntimeSettings;
use crate::providers::ModelChoice;
use anyhow::Context;
use once_cell::sync::Lazy;

/// Hosted speech endpoints cap the `input` field (OpenAI: 4096 chars). Long
/// agent replies are truncated to this many characters rather than erroring —
/// a spoken lead-in beats no audio at all.
pub const MAX_TTS_CHARS: usize = 4000;

/// Cache key under which TTS catalogues live in `provider_model_cache`.
/// Synthetic, like `stt::STT_CACHE_PROVIDER` — never a real chat provider.
pub const TTS_CACHE_PROVIDER: &str = "tts";

// Own client rather than the streaming chat client: synthesis latency scales
// with reply length, so it gets the same generous-but-bounded timeout as STT.
static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("axon-agent/1.0")
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("build TTS HTTP client")
});

/// Resolved `tts.*` settings. `None` from [`config_from_settings`] means voice
/// replies are not configured (no base URL or no model).
#[derive(Debug, Clone)]
pub struct TtsConfig {
    pub base_url: String,
    pub model: String,
    pub voice: String,
    pub api_key: String,
}

/// Read and `${VAR}`-resolve the `tts.*` settings. Returns `None` when
/// `tts.base_url` or `tts.model` is blank — callers treat that as "voice
/// replies disabled" and keep their non-audio behavior.
pub fn config_from_settings(settings: &RuntimeSettings) -> Option<TtsConfig> {
    let base_url = settings.resolve(&settings.get_str("tts.base_url", ""));
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    let model = settings.resolve(&settings.get_str("tts.model", ""));
    let model = model.trim().to_string();
    if base_url.is_empty() || model.is_empty() {
        return None;
    }
    let voice = settings.resolve(&settings.get_str("tts.voice", ""));
    let api_key = settings.resolve(&settings.get_str("tts.api_key", ""));
    Some(TtsConfig {
        base_url,
        model,
        voice: voice.trim().to_string(),
        api_key: api_key.trim().to_string(),
    })
}

/// Clamp speech input to [`MAX_TTS_CHARS`] on a char boundary.
fn clamp_input(text: &str) -> String {
    text.chars().take(MAX_TTS_CHARS).collect()
}

/// POST `{base_url}/audio/speech` and return the upstream response once its
/// status is confirmed OK — the caller streams the audio body through to the
/// browser without buffering it here. `voice` is omitted when blank so hosts
/// that require one answer with their own explicit 400 (which the caller
/// treats like any other failure: log and fall back).
pub async fn speak(cfg: &TtsConfig, text: &str) -> anyhow::Result<reqwest::Response> {
    let input = clamp_input(text.trim());
    if input.is_empty() {
        anyhow::bail!("no text to speak");
    }

    let mut body = serde_json::json!({
        "model": cfg.model,
        "input": input,
        "response_format": "mp3",
    });
    if !cfg.voice.is_empty() {
        body["voice"] = serde_json::Value::String(cfg.voice.clone());
    }

    let url = format!("{}/audio/speech", cfg.base_url);
    let mut req = HTTP_CLIENT.post(&url).json(&body);
    if !cfg.api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", cfg.api_key));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("speech request to {}", url))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(300).collect();
        anyhow::bail!("speech synthesis failed ({}): {}", status, snippet);
    }
    Ok(resp)
}

/// True when a model id looks like a text-to-speech model. Mirror image of
/// `stt::looks_like_stt_model`: transcription ids are explicitly excluded so
/// `whisper-large-v3` never lands in a TTS dropdown, then we match the naming
/// conventions of the OpenAI-compatible hosts we know (tts-1, gpt-4o-mini-tts,
/// playai-tts, speecht5, …).
fn looks_like_tts_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    if [
        "whisper",
        "transcribe",
        "speech-to-text",
        "-stt",
        "-asr",
        "voxtral",
        "parakeet",
        "canary",
    ]
    .iter()
    .any(|k| id.contains(k))
    {
        return false;
    }
    ["tts", "text-to-speech", "speech"]
        .iter()
        .any(|k| id.contains(k))
}

/// Filter a provider catalogue down to TTS-capable models. When the heuristic
/// recognizes nothing (an unknown platform's naming scheme), the full list is
/// returned instead — a long dropdown beats an empty one.
fn filter_tts_models(all: Vec<ModelChoice>) -> Vec<ModelChoice> {
    let filtered: Vec<ModelChoice> = all
        .iter()
        .filter(|c| looks_like_tts_model(&c.id))
        .cloned()
        .collect();
    if filtered.is_empty() {
        all
    } else {
        filtered
    }
}

/// Fetch the speech models a host exposes: `GET {base_url}/models` filtered to
/// TTS-looking ids. Used by the Settings dropdown's live fallback and the
/// daily prefetch sweep.
pub async fn list_tts_models(base_url: &str, api_key: &str) -> anyhow::Result<Vec<ModelChoice>> {
    let all = crate::providers::list_available_models("openai", Some(base_url), api_key).await?;
    Ok(filter_tts_models(all))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str) -> ModelChoice {
        ModelChoice {
            id: id.into(),
            label: None,
        }
    }

    #[test]
    fn tts_filter_keeps_speech_models_and_drops_stt() {
        // A realistic mixed catalogue: chat models, whisper, and TTS.
        let all = vec![
            choice("llama-3.3-70b-versatile"),
            choice("whisper-large-v3-turbo"),
            choice("playai-tts"),
            choice("playai-tts-arabic"),
            choice("gpt-4o-mini-tts"),
            choice("tts-1-hd"),
            choice("gpt-4o-mini-transcribe"),
        ];
        let got = filter_tts_models(all);
        let ids: Vec<&str> = got.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "playai-tts",
                "playai-tts-arabic",
                "gpt-4o-mini-tts",
                "tts-1-hd"
            ]
        );
    }

    #[test]
    fn tts_filter_falls_back_to_full_list_when_nothing_matches() {
        let all = vec![choice("acme-audio-1"), choice("acme-audio-2")];
        assert_eq!(filter_tts_models(all).len(), 2);
    }

    #[test]
    fn input_clamps_on_char_boundary() {
        // Multi-byte chars near the cut must not panic or split.
        let long = "héllo ".repeat(2000); // 12000 chars
        let clamped = clamp_input(&long);
        assert_eq!(clamped.chars().count(), MAX_TTS_CHARS);
        // Short input passes through untouched.
        assert_eq!(clamp_input("hi"), "hi");
    }
}
