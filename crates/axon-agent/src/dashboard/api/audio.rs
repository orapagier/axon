//! Dashboard voice endpoints.
//!
//! * `POST /api/audio/transcribe` — takes a recorded clip (multipart `file`
//!   field, as produced by the browser's MediaRecorder) and returns
//!   `{ok, text}` from the configured OpenAI-compatible `/audio/transcriptions`
//!   endpoint. The actual client lives in `crate::stt`, shared with the
//!   messaging gateways' voice-message handling.
//! * `POST /api/audio/speech` — spoken agent replies: `{text}` in, synthesized
//!   audio out via `crate::tts` (streamed through from OpenAI-compatible
//!   `/audio/speech` hosts; buffered WAV from Gemini's native speech API).
//!   Any non-2xx here means "no TTS" — the Chat page falls back to browser
//!   speech synthesis.
//! * `POST /api/audio/models` — the `stt.model`/`tts.model` dropdown feed:
//!   audio models available at a given base URL (`{kind: "stt"|"tts"}`).
//!   Served from the `provider_model_cache` (daily prefetch sweep) with a
//!   live-fetch fallback, mirroring `/api/models/available` for chat models.

use super::*;
use axum::body::Body;
use axum::extract::Multipart;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

pub async fn transcribe_audio(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Json<Value> {
    let Some(cfg) = crate::stt::config_from_settings(&state.settings) else {
        return Json(json!({
            "error": "Speech-to-text is not configured — set stt.base_url and stt.model under Settings → Voice Input (e.g. Groq: https://api.groq.com/openai/v1 + whisper-large-v3-turbo)."
        }));
    };

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

    match crate::stt::transcribe(&cfg, bytes, &filename, &mime).await {
        Ok(text) => Json(json!({"ok": true, "text": text})),
        Err(e) => {
            tracing::warn!("STT transcription failed ({}): {:#}", cfg.model, e);
            Json(json!({"error": format!("{:#}", e)}))
        }
    }
}

/// Speak an agent reply: `{text}` in, audio out. The upstream body is streamed
/// through as it synthesizes — never buffered here. Non-2xx statuses carry a
/// JSON `{error}` and mean "no server TTS for this reply": 503 when the `tts.*`
/// settings are incomplete, 502 when the provider errored or rate-limited.
/// The Chat page treats every failure the same way — fall back to the
/// browser's built-in speech synthesis (the pre-TTS behavior).
pub async fn speak_text(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let text = payload
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "no text to speak"})),
        )
            .into_response();
    }
    let Some(cfg) = crate::tts::config_from_settings(&state.settings) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "Text-to-speech is not configured — set tts.base_url and tts.model under Settings → Voice Replies (e.g. Groq: https://api.groq.com/openai/v1 + playai-tts + voice Fritz-PlayAI)."
            })),
        )
            .into_response();
    };

    match crate::tts::speak(&cfg, &text).await {
        Ok(crate::tts::SpeechAudio::Streamed(upstream)) => {
            let content_type = upstream
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("audio/mpeg")
                .to_string();
            (
                [
                    (header::CONTENT_TYPE, content_type),
                    // Replies are one-shot and per-conversation; never cache.
                    (header::CACHE_CONTROL, "no-store".to_string()),
                ],
                Body::from_stream(upstream.bytes_stream()),
            )
                .into_response()
        }
        // Gemini's native API answers with one buffered WAV instead of a
        // stream; same headers, ready-made body.
        Ok(crate::tts::SpeechAudio::Buffered {
            content_type,
            bytes,
        }) => (
            [
                (header::CONTENT_TYPE, content_type.to_string()),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!("TTS synthesis failed ({}): {:#}", cfg.model, e);
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("{:#}", e)})),
            )
                .into_response()
        }
    }
}

/// Audio models available at a base URL, for the Settings page `stt.model` and
/// `tts.model` dropdowns. Body: `{kind?, base_url?, api_key?}` — `kind` picks
/// the catalogue ("stt" default, "tts" for speech synthesis); the page sends
/// its current (possibly unsaved) drafts, blanks fall back to the stored
/// settings of that kind, and `${VAR}` placeholders resolve settings-then-env
/// either way.
///
/// Fast path is the `provider_model_cache` row set the daily sweep maintains
/// under the synthetic providers `"stt"`/`"tts"`; a miss (e.g. a base URL just
/// typed into the field) does one live catalogue fetch and warms the cache.
/// An empty list means "nothing listable"; the UI falls back to free text.
pub async fn get_audio_models(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let is_tts = payload
        .get("kind")
        .and_then(|v| v.as_str())
        .is_some_and(|k| k.eq_ignore_ascii_case("tts"));
    let (cache_provider, base_url_key, api_key_key) = if is_tts {
        (
            crate::tts::TTS_CACHE_PROVIDER,
            "tts.base_url",
            "tts.api_key",
        )
    } else {
        (
            crate::stt::STT_CACHE_PROVIDER,
            "stt.base_url",
            "stt.api_key",
        )
    };

    let settings = &state.settings;
    let field = |name: &str, key: &str| -> String {
        let from_payload = payload
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let raw = from_payload.unwrap_or_else(|| settings.get_str(key, ""));
        settings.resolve(&raw).trim().to_string()
    };
    let base_url = field("base_url", base_url_key)
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return Json(json!({"ok": true, "models": []}));
    }
    let api_key = field("api_key", api_key_key);

    // Piper's "catalogue" is a local directory scan — instant and always
    // current, so never serve it stale from the daily-swept cache (a voice
    // installed today wouldn't appear until tomorrow's sweep otherwise).
    let piper = is_tts && crate::tts::is_piper(&base_url);

    // Fast path: the daily-swept cache.
    if !piper {
        if let Ok(conn) = state.db.get() {
            let cached = crate::model_cache::read_cached(&conn, cache_provider, Some(&base_url));
            if !cached.is_empty() {
                return Json(json!({"ok": true, "models": cached}));
            }
        }
    }

    // Cache miss → one live fetch so a just-typed base URL still lists.
    let fetched = if is_tts {
        crate::tts::list_tts_models(&base_url, &api_key).await
    } else {
        crate::stt::list_stt_models(&base_url, &api_key).await
    };
    match fetched {
        Ok(choices) if !choices.is_empty() => {
            if !piper {
                if let Ok(conn) = state.db.get() {
                    let _ =
                        crate::model_cache::store(&conn, cache_provider, Some(&base_url), &choices);
                }
            }
            Json(json!({"ok": true, "models": choices}))
        }
        Ok(_) => Json(json!({"ok": true, "models": []})),
        Err(e) => {
            tracing::warn!("audio/models live fetch for '{}' failed: {:#}", base_url, e);
            Json(json!({"ok": true, "models": []}))
        }
    }
}
