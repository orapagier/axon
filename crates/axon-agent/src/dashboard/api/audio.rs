//! Dashboard voice input: `POST /api/audio/transcribe` takes a recorded clip
//! (multipart `file` field, as produced by the browser's MediaRecorder) and
//! returns `{ok, text}` from an OpenAI-compatible `/audio/transcriptions`
//! endpoint. Like the embedder, one code path covers every provider that
//! speaks that shape, so switching providers is a settings change:
//!   * Groq   — https://api.groq.com/openai/v1 + whisper-large-v3-turbo
//!   * OpenAI — https://api.openai.com/v1 + gpt-4o-mini-transcribe / whisper-1
//! Configured via `stt.base_url` / `stt.model` / `stt.api_key` / `stt.language`.

use super::*;
use axum::extract::Multipart;
use once_cell::sync::Lazy;

/// Whisper-style endpoints cap uploads at 25 MB; reject earlier with a clear
/// message instead of relaying an opaque provider 413.
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

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

/// Pull the transcript out of a `/audio/transcriptions` JSON response
/// (`{"text": …}` — same shape across OpenAI, Groq, and other compat hosts).
fn extract_transcript(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("text")?
        .as_str()
        .map(|s| s.trim().to_string())
}

pub async fn transcribe_audio(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Json<Value> {
    let settings = &state.settings;
    let base_url = settings.resolve(&settings.get_str("stt.base_url", ""));
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    let model = settings.resolve(&settings.get_str("stt.model", ""));
    let model = model.trim().to_string();
    if base_url.is_empty() || model.is_empty() {
        return Json(json!({
            "error": "Speech-to-text is not configured — set stt.base_url and stt.model under Settings → Voice Input (e.g. Groq: https://api.groq.com/openai/v1 + whisper-large-v3-turbo)."
        }));
    }
    let api_key = settings.resolve(&settings.get_str("stt.api_key", ""));
    let language = settings.resolve(&settings.get_str("stt.language", ""));

    // First non-empty field is the clip; the browser sends exactly one.
    let mut audio: Option<(Vec<u8>, String, String)> = None;
    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field.file_name().unwrap_or("recording.webm").to_string();
        let mime = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        if let Ok(bytes) = field.bytes().await {
            if !bytes.is_empty() {
                audio = Some((bytes.to_vec(), filename, mime));
                break;
            }
        }
    }
    let Some((bytes, filename, mime)) = audio else {
        return Json(json!({"error": "no audio data received"}));
    };
    if bytes.len() > MAX_AUDIO_BYTES {
        return Json(json!({
            "error": format!("recording too large ({} MB) — transcription endpoints accept up to 25 MB", bytes.len() / (1024 * 1024))
        }));
    }

    // Whisper detects the container from the part's filename extension; the
    // mime is a bonus hint. MediaRecorder mimes carry parameters
    // ("audio/webm;codecs=opus") which parse fine — only attach when valid.
    let mime_ok = mime.parse::<mime_guess::mime::Mime>().is_ok();
    let mut part = reqwest::multipart::Part::bytes(bytes).file_name(filename);
    if mime_ok {
        part = part.mime_str(&mime).expect("mime validated above");
    }
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.clone())
        .text("response_format", "json");
    let language = language.trim().to_string();
    if !language.is_empty() {
        form = form.text("language", language);
    }

    let url = format!("{}/audio/transcriptions", base_url);
    let mut req = HTTP_CLIENT.post(&url).multipart(form);
    let api_key = api_key.trim();
    if !api_key.is_empty() {
        req = req.header("Authorization", format!("Bearer {}", api_key));
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("STT request to {} failed: {}", url, e);
            return Json(json!({"error": format!("transcription request failed: {}", e)}));
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = body.chars().take(300).collect();
        tracing::warn!("STT provider error {} ({}): {}", status, model, snippet);
        return Json(json!({"error": format!("transcription failed ({}): {}", status, snippet)}));
    }
    match extract_transcript(&body) {
        Some(text) => Json(json!({"ok": true, "text": text})),
        None => {
            tracing::warn!(
                "unexpected STT response shape: {}",
                body.chars().take(200).collect::<String>()
            );
            Json(json!({"error": "unexpected transcription response shape"}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
