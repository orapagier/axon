//! Dashboard voice input endpoints.
//!
//! * `POST /api/audio/transcribe` — takes a recorded clip (multipart `file`
//!   field, as produced by the browser's MediaRecorder) and returns
//!   `{ok, text}` from the configured OpenAI-compatible `/audio/transcriptions`
//!   endpoint. The actual client lives in `crate::stt`, shared with the
//!   messaging gateways' voice-message handling.
//! * `POST /api/audio/models` — the `stt.model` dropdown feed: transcription
//!   models available at a given `stt.base_url`. Served from the
//!   `provider_model_cache` (daily prefetch sweep) with a live-fetch fallback,
//!   mirroring `/api/models/available` for chat models.

use super::*;
use axum::extract::Multipart;

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

/// Transcription models available at an STT base URL, for the Settings page
/// `stt.model` dropdown. Body: `{base_url?, api_key?}` — the page sends its
/// current (possibly unsaved) drafts; blanks fall back to the stored `stt.*`
/// settings, and `${VAR}` placeholders resolve settings-then-env either way.
///
/// Fast path is the `provider_model_cache` row set the daily sweep maintains
/// under the synthetic provider `"stt"`; a miss (e.g. a base URL just typed
/// into the field) does one live catalogue fetch and warms the cache with it.
/// An empty list means "nothing listable"; the UI falls back to free text.
pub async fn get_stt_models(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
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
    let base_url = field("base_url", "stt.base_url")
        .trim_end_matches('/')
        .to_string();
    if base_url.is_empty() {
        return Json(json!({"ok": true, "models": []}));
    }
    let api_key = field("api_key", "stt.api_key");

    // Fast path: the daily-swept cache.
    if let Ok(conn) = state.db.get() {
        let cached =
            crate::model_cache::read_cached(&conn, crate::stt::STT_CACHE_PROVIDER, Some(&base_url));
        if !cached.is_empty() {
            return Json(json!({"ok": true, "models": cached}));
        }
    }

    // Cache miss → one live fetch so a just-typed base URL still lists.
    match crate::stt::list_stt_models(&base_url, &api_key).await {
        Ok(choices) if !choices.is_empty() => {
            if let Ok(conn) = state.db.get() {
                let _ = crate::model_cache::store(
                    &conn,
                    crate::stt::STT_CACHE_PROVIDER,
                    Some(&base_url),
                    &choices,
                );
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
