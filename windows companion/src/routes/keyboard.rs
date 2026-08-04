use axum::Json;
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use serde::Deserialize;

use crate::routes::{ActionResponse, AppError};

// ──────────────────────────────────────────────────────────────────────────────
// Type text
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TypeRequest {
    /// Text to type as if from a keyboard
    pub text: String,
    /// Optional delay in ms between characters (default 0)
    #[serde(default)]
    pub delay_ms: u64,
}

pub async fn type_text(Json(req): Json<TypeRequest>) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut enigo =
            Enigo::new(&Settings::default()).map_err(|e| AppError::internal(e.to_string()))?;

        if req.delay_ms > 0 {
            for ch in req.text.chars() {
                enigo
                    .text(&ch.to_string())
                    .map_err(|e| AppError::internal(e.to_string()))?;
                std::thread::sleep(std::time::Duration::from_millis(req.delay_ms));
            }
        } else {
            enigo
                .text(&req.text)
                .map_err(|e| AppError::internal(e.to_string()))?;
        }

        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

// ──────────────────────────────────────────────────────────────────────────────
// Key press / hotkey
// ──────────────────────────────────────────────────────────────────────────────

/// Upper bound on `repeat` — without it a single request can pin a blocking
/// thread for hours while hammering the focused window with keystrokes.
const MAX_REPEAT: u32 = 1000;

#[derive(Deserialize)]
pub struct KeyRequest {
    /// e.g. ["ctrl", "c"] or ["win", "d"] or ["f5"]
    pub keys: Vec<String>,
    /// How many times to press (default 1, max 1000)
    #[serde(default = "one")]
    pub repeat: u32,
}

fn one() -> u32 {
    1
}

pub async fn press_key(Json(req): Json<KeyRequest>) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut enigo =
            Enigo::new(&Settings::default()).map_err(|e| AppError::internal(e.to_string()))?;

        let keys: Vec<Key> = req.keys.iter().filter_map(|k| parse_key(k)).collect();

        if keys.is_empty() {
            return Err(AppError::bad_request(format!(
                "No recognized keys in: {:?}",
                req.keys
            )));
        }

        if req.repeat > MAX_REPEAT {
            return Err(AppError::bad_request(format!(
                "repeat is {} — maximum is {}",
                req.repeat, MAX_REPEAT
            )));
        }

        let (modifiers, last) = keys.split_at(keys.len() - 1);
        let last = last[0];

        for _ in 0..req.repeat {
            // Press modifiers, click the final key, then ALWAYS release the
            // modifiers — including when the click fails. Returning early
            // between press and release leaves Ctrl/Alt/Win physically stuck
            // down system-wide, which the user cannot clear from the API.
            let mut result = press_all(&mut enigo, modifiers);

            if result.is_ok() {
                result = enigo
                    .key(last, Direction::Click)
                    .map_err(|e| AppError::internal(e.to_string()));
            }

            let release_result = release_all(&mut enigo, modifiers);
            result?;
            release_result?;
        }

        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

/// Press modifiers in order. On failure, releases the ones already pressed so
/// the keyboard is never left in a half-held state.
fn press_all(enigo: &mut Enigo, modifiers: &[Key]) -> Result<(), AppError> {
    for (i, key) in modifiers.iter().enumerate() {
        if let Err(e) = enigo.key(*key, Direction::Press) {
            let _ = release_all(enigo, &modifiers[..i]);
            return Err(AppError::internal(e.to_string()));
        }
    }
    Ok(())
}

/// Release modifiers in reverse order. Every key is attempted even if an
/// earlier one fails — a stuck modifier is worse than a lost error message.
fn release_all(enigo: &mut Enigo, modifiers: &[Key]) -> Result<(), AppError> {
    let mut first_err = None;
    for key in modifiers.iter().rev() {
        if let Err(e) = enigo.key(*key, Direction::Release) {
            first_err.get_or_insert(AppError::internal(e.to_string()));
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn parse_key(s: &str) -> Option<Key> {
    match s.to_lowercase().as_str() {
        "ctrl" | "control" => Some(Key::Control),
        "alt" => Some(Key::Alt),
        "shift" => Some(Key::Shift),
        "win" | "super" | "meta" | "windows" => Some(Key::Meta),
        "enter" | "return" => Some(Key::Return),
        "tab" => Some(Key::Tab),
        "esc" | "escape" => Some(Key::Escape),
        "space" => Some(Key::Space),
        "backspace" => Some(Key::Backspace),
        "delete" | "del" => Some(Key::Delete),
        "insert" | "ins" => Some(Key::Insert),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" | "pgup" => Some(Key::PageUp),
        "pagedown" | "pgdn" => Some(Key::PageDown),
        "up" => Some(Key::UpArrow),
        "down" => Some(Key::DownArrow),
        "left" => Some(Key::LeftArrow),
        "right" => Some(Key::RightArrow),
        "f1" => Some(Key::F1),
        "f2" => Some(Key::F2),
        "f3" => Some(Key::F3),
        "f4" => Some(Key::F4),
        "f5" => Some(Key::F5),
        "f6" => Some(Key::F6),
        "f7" => Some(Key::F7),
        "f8" => Some(Key::F8),
        "f9" => Some(Key::F9),
        "f10" => Some(Key::F10),
        "f11" => Some(Key::F11),
        "f12" => Some(Key::F12),
        "printscreen" | "prtsc" => Some(Key::Print),
        "capslock" => Some(Key::CapsLock),
        "numlock" => Some(Key::Numlock),
        "scrolllock" => Some(Key::Scroll),
        // Single character keys. Count chars, not bytes — "é" and "ü" are
        // multi-byte, so a byte-length check silently rejected them.
        s if s.chars().count() == 1 => s.chars().next().map(Key::Unicode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_and_single_char_keys() {
        assert!(matches!(parse_key("ctrl"), Some(Key::Control)));
        assert!(matches!(parse_key("F5"), Some(Key::F5)));
        assert!(matches!(parse_key("a"), Some(Key::Unicode('a'))));
    }

    #[test]
    fn parses_multibyte_single_characters() {
        assert!(matches!(parse_key("é"), Some(Key::Unicode('é'))));
        assert!(matches!(parse_key("ü"), Some(Key::Unicode('ü'))));
    }

    #[test]
    fn rejects_unknown_multi_char_keys() {
        assert!(parse_key("nope").is_none());
        assert!(parse_key("").is_none());
    }
}
