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

#[derive(Deserialize)]
pub struct KeyRequest {
    /// e.g. ["ctrl", "c"] or ["win", "d"] or ["f5"]
    pub keys: Vec<String>,
    /// How many times to press (default 1)
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

        for _ in 0..req.repeat {
            // Press all modifier keys down
            for key in &keys[..keys.len() - 1] {
                enigo
                    .key(*key, Direction::Press)
                    .map_err(|e| AppError::internal(e.to_string()))?;
            }
            // Click the final key
            let last = *keys.last().unwrap();
            enigo
                .key(last, Direction::Click)
                .map_err(|e| AppError::internal(e.to_string()))?;
            // Release modifiers in reverse
            for key in keys[..keys.len() - 1].iter().rev() {
                enigo
                    .key(*key, Direction::Release)
                    .map_err(|e| AppError::internal(e.to_string()))?;
            }
        }

        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
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
        // Single character keys
        s if s.len() == 1 => s.chars().next().map(Key::Unicode),
        _ => None,
    }
}
