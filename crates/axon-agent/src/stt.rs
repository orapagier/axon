//! Shared speech-to-text: one OpenAI-compatible `/audio/transcriptions` client
//! used by every voice-input surface — the dashboard chat microphone
//! (`dashboard::api::audio`) and the messaging gateways (Telegram voice notes,
//! Slack audio clips). Configured via the `stt.*` settings:
//!   * Groq   — https://api.groq.com/openai/v1 + whisper-large-v3-turbo
//!   * OpenAI — https://api.openai.com/v1 + gpt-4o-mini-transcribe / whisper-1
//!
//! Also owns the STT model listing that feeds the Settings page `stt.model`
//! dropdown: `GET {base_url}/models` filtered down to transcription-capable
//! ids, cached in `provider_model_cache` under the provider key `"stt"`.

use crate::config::RuntimeSettings;
use crate::providers::ModelChoice;
use anyhow::Context;
use once_cell::sync::Lazy;
use serde_json::Value;

/// Whisper-style endpoints cap uploads at 25 MB; reject earlier with a clear
/// message instead of relaying an opaque provider 413.
pub const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

/// Cache key under which STT catalogues live in `provider_model_cache`. A
/// synthetic provider name — never a real chat provider, so the ModelsPage
/// dropdowns and the STT dropdown can share the table without collisions.
pub const STT_CACHE_PROVIDER: &str = "stt";

// Separate client from the streaming chat client: transcription is a single
// blocking upload whose latency scales with clip length, so it gets its own
// generous-but-bounded timeout.
static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("axon-agent/1.0")
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("build STT HTTP client")
});

/// Resolved `stt.*` settings. `None` from [`config_from_settings`] means voice
/// input is not configured (no base URL or no model).
#[derive(Debug, Clone)]
pub struct SttConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub language: String,
}

/// Read and `${VAR}`-resolve the `stt.*` settings. Returns `None` when
/// `stt.base_url` or `stt.model` is blank — callers treat that as "voice input
/// disabled" and keep their non-voice behavior.
pub fn config_from_settings(settings: &RuntimeSettings) -> Option<SttConfig> {
    let base_url = settings.resolve(&settings.get_str("stt.base_url", ""));
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    let model = settings.resolve(&settings.get_str("stt.model", ""));
    let model = model.trim().to_string();
    if base_url.is_empty() || model.is_empty() {
        return None;
    }
    let api_key = settings.resolve(&settings.get_str("stt.api_key", ""));
    let language = settings.resolve(&settings.get_str("stt.language", ""));
    Some(SttConfig {
        base_url,
        model,
        api_key: api_key.trim().to_string(),
        language: language.trim().to_string(),
    })
}

/// Pull the transcript out of a `/audio/transcriptions` JSON response
/// (`{"text": …}` — same shape across OpenAI, Groq, and other compat hosts).
fn extract_transcript(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("text")?
        .as_str()
        .map(|s| s.trim().to_string())
}

/// POST one audio clip to `{base_url}/audio/transcriptions` and return the
/// transcript. `filename`'s extension is how Whisper-style endpoints detect the
/// container; `mime` is a bonus hint attached only when it parses as valid.
pub async fn transcribe(
    cfg: &SttConfig,
    bytes: Vec<u8>,
    filename: &str,
    mime: &str,
) -> anyhow::Result<String> {
    if bytes.is_empty() {
        anyhow::bail!("no audio data");
    }
    if bytes.len() > MAX_AUDIO_BYTES {
        anyhow::bail!(
            "recording too large ({} MB) — transcription endpoints accept up to 25 MB",
            bytes.len() / (1024 * 1024)
        );
    }

    let mime_ok = mime.parse::<mime_guess::mime::Mime>().is_ok();
    let mut part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_string());
    if mime_ok {
        part = part.mime_str(mime).expect("mime validated above");
    }
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", cfg.model.clone())
        .text("response_format", "json");
    if !cfg.language.is_empty() {
        form = form.text("language", cfg.language.clone());
    }

    let url = format!("{}/audio/transcriptions", cfg.base_url);
    let mut req = HTTP_CLIENT.post(&url).multipart(form);
    if !cfg.api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", cfg.api_key));
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("transcription request to {}", url))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = body.chars().take(300).collect();
        anyhow::bail!("transcription failed ({}): {}", status, snippet);
    }
    extract_transcript(&body).ok_or_else(|| {
        anyhow::anyhow!(
            "unexpected transcription response shape: {}",
            body.chars().take(200).collect::<String>()
        )
    })
}

/// True when a model id looks like a speech-to-text model. Heuristic over the
/// naming conventions of the OpenAI-compatible hosts we know (whisper-*,
/// *-transcribe, distil-whisper, voxtral, parakeet, canary, …); TTS ids are
/// explicitly excluded so `playai-tts` and friends never land in an STT
/// dropdown.
fn looks_like_stt_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    if id.contains("tts") || id.contains("text-to-speech") {
        return false;
    }
    [
        "whisper",
        "transcribe",
        "voxtral",
        "parakeet",
        "canary",
        "-asr",
        "speech-to-text",
        "-stt",
    ]
    .iter()
    .any(|k| id.contains(k))
}

/// Filter a provider catalogue down to STT-capable models. When the heuristic
/// recognizes nothing (an unknown platform's naming scheme), the full list is
/// returned instead — a long dropdown beats an empty one.
fn filter_stt_models(all: Vec<ModelChoice>) -> Vec<ModelChoice> {
    let filtered: Vec<ModelChoice> = all
        .iter()
        .filter(|c| looks_like_stt_model(&c.id))
        .cloned()
        .collect();
    if filtered.is_empty() {
        all
    } else {
        filtered
    }
}

/// Fetch the transcription models a host exposes: `GET {base_url}/models`
/// (the OpenAI-compatible shape every `/audio/transcriptions` host also
/// speaks), filtered to STT-looking ids. Used by the Settings dropdown's live
/// fallback and the daily prefetch sweep.
pub async fn list_stt_models(base_url: &str, api_key: &str) -> anyhow::Result<Vec<ModelChoice>> {
    // Provider "openai" routes to the shared OpenAI-compat lister with our
    // explicit base_url, which is exactly the request shape we need.
    let all = crate::providers::list_available_models("openai", Some(base_url), api_key).await?;
    Ok(filter_stt_models(all))
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
    fn transcript_parses_and_trims() {
        assert_eq!(
            extract_transcript(r#"{"text":" hello world \n"}"#).as_deref(),
            Some("hello world")
        );
        // verbose_json-style extras around `text` don't break extraction
        assert_eq!(
            extract_transcript(r#"{"task":"transcribe","text":"hi","duration":1.2}"#).as_deref(),
            Some("hi")
        );
        assert_eq!(extract_transcript("not json"), None);
        assert_eq!(extract_transcript(r#"{"no_text":true}"#), None);
    }

    #[test]
    fn media_recorder_mime_with_codecs_is_valid() {
        // The exact strings browsers put on MediaRecorder blobs must pass the
        // validity gate, or the part would silently lose its content type.
        for m in ["audio/webm;codecs=opus", "audio/webm", "audio/mp4"] {
            assert!(m.parse::<mime_guess::mime::Mime>().is_ok(), "{m}");
        }
    }

    #[test]
    fn stt_filter_keeps_transcription_models_and_drops_tts() {
        // A realistic Groq catalogue slice: chat models, TTS, and whisper.
        let all = vec![
            choice("llama-3.3-70b-versatile"),
            choice("whisper-large-v3-turbo"),
            choice("distil-whisper-large-v3-en"),
            choice("playai-tts"),
            choice("gpt-4o-mini-transcribe"),
        ];
        let got = filter_stt_models(all);
        let ids: Vec<&str> = got.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "whisper-large-v3-turbo",
                "distil-whisper-large-v3-en",
                "gpt-4o-mini-transcribe"
            ]
        );
    }

    #[test]
    fn stt_filter_falls_back_to_full_list_when_nothing_matches() {
        // An unknown platform whose naming the heuristic doesn't recognize:
        // return everything rather than an empty dropdown.
        let all = vec![choice("acme-audio-1"), choice("acme-audio-2")];
        assert_eq!(filter_stt_models(all).len(), 2);
    }
}
