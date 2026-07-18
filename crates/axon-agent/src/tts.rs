//! Shared text-to-speech powering spoken agent replies on the dashboard Chat
//! page. Configured via the `tts.*` settings (mirroring the `stt.*`
//! voice-input group):
//!   * Groq   — https://api.groq.com/openai/v1 + playai-tts + voice Fritz-PlayAI
//!   * OpenAI — https://api.openai.com/v1 + gpt-4o-mini-tts + voice alloy
//!   * Gemini — https://generativelanguage.googleapis.com/v1beta/openai +
//!     gemini-2.5-flash-preview-tts + voice Kore. Google exposes no
//!     `/audio/speech` route (not even on its OpenAI-compat layer), so Gemini
//!     base URLs are detected and served through the native
//!     `models/{model}:generateContent` speech API instead, with the returned
//!     PCM wrapped in a WAV header for the browser.
//!   * Piper  — `tts.base_url = "piper"` selects a local, offline engine
//!     instead of an HTTP host: `tts.model` names a voice (e.g.
//!     `en_US-lessac-medium`) installed under [`piper_models_dir`], synthesis
//!     runs as a subprocess of the `piper` binary at [`piper_bin_path`]. No
//!     API key, no network round-trip, zero marginal cost.
//!
//! Also owns the TTS model listing that feeds the Settings page `tts.model`
//! dropdown: `GET {base_url}/models` filtered down to speech-synthesis ids
//! for HTTP hosts, or the installed `.onnx` voices for Piper. Cached in
//! `provider_model_cache` under the provider key `"tts"`.
//!
//! TTS is strictly best-effort: every caller falls back to its non-audio
//! behavior (the browser's speechSynthesis, or plain text) when this module
//! reports "not configured" or any upstream failure — rate limits included.

use crate::config::RuntimeSettings;
use crate::providers::ModelChoice;
use anyhow::Context;
use once_cell::sync::Lazy;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

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

/// Synthesized speech ready to send to the browser. OpenAI-shaped hosts stream
/// as they synthesize; the Gemini path buffers, because the audio arrives as
/// one base64 blob inside a JSON envelope.
pub enum SpeechAudio {
    /// Upstream response confirmed 2xx — pipe its byte stream through.
    Streamed(reqwest::Response),
    /// Fully assembled audio (Gemini PCM wrapped as WAV).
    Buffered {
        content_type: &'static str,
        bytes: Vec<u8>,
    },
}

/// Native-API root for Google's Gemini endpoint, or `None` for every other
/// host. Accepts whatever the user pasted — the OpenAI-compat URL
/// (`…/v1beta/openai`), a versioned root (`…/v1beta`), or the bare domain —
/// and normalizes to the versioned root that `models/{m}:generateContent`
/// hangs off.
fn gemini_native_root(base_url: &str) -> Option<String> {
    if !base_url.contains("generativelanguage.googleapis.com") {
        return None;
    }
    let root = base_url
        .trim_end_matches('/')
        .trim_end_matches("/openai")
        .trim_end_matches('/');
    if root.ends_with("/v1beta") || root.ends_with("/v1alpha") || root.ends_with("/v1") {
        Some(root.to_string())
    } else {
        Some(format!("{}/v1beta", root))
    }
}

/// Synthesize speech, picking the engine from the base URL: `"piper"` runs the
/// local offline binary, Gemini hosts go through the native `generateContent`
/// speech API, everything else POSTs the OpenAI-shaped `{base_url}/audio/speech`.
/// For the OpenAI path `voice` is omitted when blank so hosts that require one
/// answer with their own explicit 400 (which the caller treats like any other
/// failure: log and fall back).
pub async fn speak(cfg: &TtsConfig, text: &str) -> anyhow::Result<SpeechAudio> {
    let input = clamp_input(text.trim());
    if input.is_empty() {
        anyhow::bail!("no text to speak");
    }

    if is_piper(&cfg.base_url) {
        return speak_piper(cfg, &input).await;
    }

    if let Some(root) = gemini_native_root(&cfg.base_url) {
        return speak_gemini(cfg, &root, &input).await;
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
    Ok(SpeechAudio::Streamed(resp))
}

/// Gemini's speech API: `models/{model}:generateContent` with an AUDIO
/// response modality. The audio comes back base64-encoded inside the JSON
/// (16-bit PCM, mono, rate declared in the part's mimeType) — decoded here and
/// wrapped in a WAV header, since browsers won't play headerless PCM.
async fn speak_gemini(cfg: &TtsConfig, root: &str, input: &str) -> anyhow::Result<SpeechAudio> {
    // Google catalogues list ids as "models/gemini-…"; the URL path adds its
    // own "models/" segment.
    let model = cfg.model.trim_start_matches("models/");
    // A voice name is provider-specific, so a blank setting gets a known-good
    // Gemini prebuilt voice rather than being omitted.
    let voice = if cfg.voice.is_empty() {
        "Kore"
    } else {
        cfg.voice.as_str()
    };
    let body = serde_json::json!({
        "contents": [{"parts": [{"text": input}]}],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {"voiceConfig": {"prebuiltVoiceConfig": {"voiceName": voice}}}
        }
    });

    let url = format!("{}/models/{}:generateContent", root, model);
    let mut req = HTTP_CLIENT.post(&url).json(&body);
    if !cfg.api_key.is_empty() {
        req = req.header("x-goog-api-key", cfg.api_key.clone());
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("speech request to {}", url))?;
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = body_text.chars().take(300).collect();
        anyhow::bail!("speech synthesis failed ({}): {}", status, snippet);
    }

    let parsed: serde_json::Value =
        serde_json::from_str(&body_text).context("parse Gemini speech response")?;
    let Some((mime, data_b64)) = find_inline_audio(&parsed) else {
        let snippet: String = body_text.chars().take(300).collect();
        anyhow::bail!("no audio in Gemini response: {}", snippet);
    };
    use base64::Engine as _;
    let pcm = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .context("decode Gemini audio payload")?;
    let rate = pcm_rate_from_mime(&mime).unwrap_or(24_000);
    Ok(SpeechAudio::Buffered {
        content_type: "audio/wav",
        bytes: wav_from_pcm16(&pcm, rate, 1),
    })
}

/// `tts.base_url` value that selects the local Piper engine instead of an
/// HTTP host.
const PIPER_SENTINEL: &str = "piper";

fn is_piper(base_url: &str) -> bool {
    base_url.trim().eq_ignore_ascii_case(PIPER_SENTINEL)
}

/// Where the Piper binary lives — the release archive from
/// https://github.com/rhasspy/piper/releases extracted as-is (the exe needs
/// its sibling `.dll`/`.so` files and `espeak-ng-data/` alongside it, so this
/// directory holds the whole extracted tree, not just the binary).
fn piper_bin_path() -> PathBuf {
    let exe = if cfg!(windows) { "piper.exe" } else { "piper" };
    axon_core::data_dir().join("piper").join(exe)
}

/// Where Piper voice models (`<id>.onnx` + `<id>.onnx.json`) live.
fn piper_models_dir() -> PathBuf {
    axon_core::data_dir().join("piper").join("models")
}

/// Synthesize via a local Piper subprocess: `piper -m <voice>.onnx -f - -q`,
/// text piped to stdin, a complete WAV read back from stdout. Stdin is
/// written from a separate task so a long reply can't deadlock against Piper
/// filling its stdout pipe before we finish writing.
async fn speak_piper(cfg: &TtsConfig, input: &str) -> anyhow::Result<SpeechAudio> {
    let bin = piper_bin_path();
    if !bin.is_file() {
        anyhow::bail!(
            "Piper binary not found at {} — download it from https://github.com/rhasspy/piper/releases and extract it there",
            bin.display()
        );
    }
    let voice = cfg.model.trim().trim_end_matches(".onnx");
    let model_path = piper_models_dir().join(format!("{voice}.onnx"));
    if !model_path.is_file() {
        anyhow::bail!(
            "Piper voice '{}' not found at {} — download the .onnx + .onnx.json from https://huggingface.co/rhasspy/piper-voices into that folder",
            voice,
            model_path.display()
        );
    }

    let mut child = tokio::process::Command::new(&bin)
        .arg("-m")
        .arg(&model_path)
        .arg("-f")
        .arg("-")
        .arg("-q")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn piper at {}", bin.display()))?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let text = input.to_string();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(text.as_bytes()).await;
        // Dropping closes the pipe, signalling EOF so piper starts synthesis.
        drop(stdin);
    });

    let output = child
        .wait_with_output()
        .await
        .context("run piper synthesis")?;
    let _ = writer.await;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("piper exited with {}: {}", output.status, stderr.trim());
    }
    Ok(SpeechAudio::Buffered {
        content_type: "audio/wav",
        bytes: output.stdout,
    })
}

/// Voices installed under `dir`: every `<id>.onnx` with a matching
/// `<id>.onnx.json` sidecar, alphabetical. Split from [`piper_models_dir`]'s
/// caller so it's testable against a temp directory.
fn piper_voices_in(dir: &Path) -> Vec<ModelChoice> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("onnx") {
                return None;
            }
            let sidecar = PathBuf::from(format!("{}.json", path.display()));
            if !sidecar.is_file() {
                return None;
            }
            path.file_stem()?.to_str().map(str::to_string)
        })
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|id| ModelChoice { id, label: None })
        .collect()
}

/// First inline audio blob in a `generateContent` response:
/// `candidates[0].content.parts[*].inlineData.{mimeType,data}`. Both camelCase
/// (REST) and snake_case spellings are accepted.
fn find_inline_audio(v: &serde_json::Value) -> Option<(String, String)> {
    let parts = v
        .get("candidates")?
        .get(0)?
        .get("content")?
        .get("parts")?
        .as_array()?;
    for part in parts {
        let Some(inline) = part.get("inlineData").or_else(|| part.get("inline_data")) else {
            continue;
        };
        let mime = inline
            .get("mimeType")
            .or_else(|| inline.get("mime_type"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        if let Some(data) = inline.get("data").and_then(|d| d.as_str()) {
            return Some((mime.to_string(), data.to_string()));
        }
    }
    None
}

/// Sample rate from a Gemini audio mimeType like
/// `audio/L16;codec=pcm;rate=24000`.
fn pcm_rate_from_mime(mime: &str) -> Option<u32> {
    mime.split(';')
        .find_map(|p| p.trim().strip_prefix("rate="))
        .and_then(|r| r.parse().ok())
}

/// Wrap raw 16-bit little-endian PCM in a canonical 44-byte WAV header.
fn wav_from_pcm16(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let byte_rate = sample_rate * u32::from(channels) * 2;
    let block_align = channels * 2;
    let data_len = pcm.len() as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
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
    if is_piper(base_url) {
        return Ok(piper_voices_in(&piper_models_dir()));
    }
    // Google's native root serves no OpenAI-shaped catalogue; its compat layer
    // at {root}/openai does, so Gemini listings go through there whichever
    // Google URL the user pasted.
    let list_url = match gemini_native_root(base_url) {
        Some(root) => format!("{}/openai", root),
        None => base_url.to_string(),
    };
    let all = crate::providers::list_available_models("openai", Some(&list_url), api_key).await?;
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
    fn tts_filter_keeps_gemini_speech_models() {
        let all = vec![
            choice("models/gemini-2.0-flash"),
            choice("models/gemini-2.5-flash-preview-tts"),
            choice("models/gemini-2.5-pro-preview-tts"),
        ];
        let ids: Vec<String> = filter_tts_models(all).into_iter().map(|c| c.id).collect();
        assert_eq!(
            ids,
            vec![
                "models/gemini-2.5-flash-preview-tts",
                "models/gemini-2.5-pro-preview-tts"
            ]
        );
    }

    #[test]
    fn gemini_root_detected_from_any_google_url_shape() {
        for url in [
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "https://generativelanguage.googleapis.com/v1beta",
            "https://generativelanguage.googleapis.com",
        ] {
            assert_eq!(
                gemini_native_root(url).as_deref(),
                Some("https://generativelanguage.googleapis.com/v1beta"),
                "for {url}"
            );
        }
        assert_eq!(gemini_native_root("https://api.groq.com/openai/v1"), None);
        assert_eq!(gemini_native_root("https://api.openai.com/v1"), None);
    }

    #[test]
    fn inline_audio_extracted_from_gemini_response() {
        let resp = serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"text": "ignored preamble"},
                {"inlineData": {"mimeType": "audio/L16;codec=pcm;rate=24000", "data": "AAEC"}}
            ]}}]
        });
        let (mime, data) = find_inline_audio(&resp).expect("audio part");
        assert_eq!(mime, "audio/L16;codec=pcm;rate=24000");
        assert_eq!(data, "AAEC");
        assert_eq!(pcm_rate_from_mime(&mime), Some(24_000));
        assert_eq!(pcm_rate_from_mime("audio/mpeg"), None);
        assert_eq!(
            find_inline_audio(&serde_json::json!({"candidates": []})),
            None
        );
    }

    #[test]
    fn wav_wrapper_writes_canonical_header() {
        let pcm = [0u8; 480]; // 10ms of 24kHz mono s16le
        let wav = wav_from_pcm16(&pcm, 24_000, 1);
        assert_eq!(wav.len(), 44 + 480);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 24_000);
        // byte rate = rate * channels * 2
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 48_000);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 480);
    }

    #[test]
    fn piper_sentinel_is_case_and_whitespace_insensitive() {
        assert!(is_piper("piper"));
        assert!(is_piper("  Piper  "));
        assert!(is_piper("PIPER"));
        assert!(!is_piper("https://api.groq.com/openai/v1"));
        assert!(!is_piper(""));
    }

    #[test]
    fn piper_voices_lists_only_onnx_with_json_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "axon-piper-voices-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // Complete voice: both files present.
        std::fs::write(dir.join("en_US-lessac-medium.onnx"), b"fake").unwrap();
        std::fs::write(dir.join("en_US-lessac-medium.onnx.json"), b"{}").unwrap();
        // Onnx with no sidecar config — download interrupted, must be excluded.
        std::fs::write(dir.join("en_US-amy-medium.onnx"), b"fake").unwrap();
        // Unrelated file — must be ignored.
        std::fs::write(dir.join("README.txt"), b"notes").unwrap();

        let voices = piper_voices_in(&dir);
        let ids: Vec<&str> = voices.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["en_US-lessac-medium"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn piper_voices_empty_when_dir_missing() {
        let dir = std::env::temp_dir().join("axon-piper-voices-does-not-exist");
        assert!(piper_voices_in(&dir).is_empty());
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
