use axum::Json;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Mouse, Settings};
use serde::Deserialize;

use crate::routes::{ActionResponse, AppError};

fn mk_enigo() -> Result<Enigo, AppError> {
    Enigo::new(&Settings::default()).map_err(|e| AppError::internal(e.to_string()))
}

// ──────────────────────────────────────────────────────────────────────────────
// Move
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MoveRequest {
    pub x: i32,
    pub y: i32,
    /// "abs" (default) or "rel" for relative movement
    #[serde(default = "abs_str")]
    pub mode: String,
}
fn abs_str() -> String {
    "abs".to_string()
}

pub async fn move_mouse(
    Json(req): Json<MoveRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut e = mk_enigo()?;
        let coord = if req.mode == "rel" {
            Coordinate::Rel
        } else {
            Coordinate::Abs
        };
        e.move_mouse(req.x, req.y, coord)
            .map_err(|e| AppError::internal(e.to_string()))?;
        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

// ──────────────────────────────────────────────────────────────────────────────
// Click
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ClickRequest {
    /// Move to this position before clicking (optional)
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// "left" (default), "right", or "middle"
    #[serde(default = "left_str")]
    pub button: String,
    /// Double-click if true
    #[serde(default)]
    pub double: bool,
    /// Delay in ms before clicking (useful for UI settle time)
    #[serde(default)]
    pub delay_ms: u64,
}
fn left_str() -> String {
    "left".to_string()
}

pub async fn click_mouse(
    Json(req): Json<ClickRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut e = mk_enigo()?;

        if let (Some(x), Some(y)) = (req.x, req.y) {
            e.move_mouse(x, y, Coordinate::Abs)
                .map_err(|e| AppError::internal(e.to_string()))?;
        }

        if req.delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(req.delay_ms));
        }

        let btn = parse_button(&req.button);
        e.button(btn, Direction::Click)
            .map_err(|e| AppError::internal(e.to_string()))?;

        if req.double {
            std::thread::sleep(std::time::Duration::from_millis(50));
            e.button(btn, Direction::Click)
                .map_err(|e| AppError::internal(e.to_string()))?;
        }

        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

// ──────────────────────────────────────────────────────────────────────────────
// Scroll
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ScrollRequest {
    /// Vertical scroll amount (positive = down, negative = up)
    #[serde(default)]
    pub y: i32,
    /// Horizontal scroll amount (positive = right, negative = left)
    #[serde(default)]
    pub x: i32,
    /// Move mouse here before scrolling (optional)
    pub mouse_x: Option<i32>,
    pub mouse_y: Option<i32>,
}

pub async fn scroll_mouse(
    Json(req): Json<ScrollRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut e = mk_enigo()?;

        if let (Some(mx), Some(my)) = (req.mouse_x, req.mouse_y) {
            e.move_mouse(mx, my, Coordinate::Abs)
                .map_err(|e| AppError::internal(e.to_string()))?;
        }

        if req.y != 0 {
            e.scroll(req.y, Axis::Vertical)
                .map_err(|e| AppError::internal(e.to_string()))?;
        }
        if req.x != 0 {
            e.scroll(req.x, Axis::Horizontal)
                .map_err(|e| AppError::internal(e.to_string()))?;
        }

        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

// ──────────────────────────────────────────────────────────────────────────────
// Drag
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DragRequest {
    pub from_x: i32,
    pub from_y: i32,
    pub to_x: i32,
    pub to_y: i32,
    #[serde(default = "left_str")]
    pub button: String,
}

pub async fn drag_mouse(
    Json(req): Json<DragRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut e = mk_enigo()?;
        let btn = parse_button(&req.button);

        e.move_mouse(req.from_x, req.from_y, Coordinate::Abs)
            .map_err(|e| AppError::internal(e.to_string()))?;
        e.button(btn, Direction::Press)
            .map_err(|e| AppError::internal(e.to_string()))?;

        // Smooth drag in steps
        let steps = 20;
        for i in 1..=steps {
            let x = req.from_x + (req.to_x - req.from_x) * i / steps;
            let y = req.from_y + (req.to_y - req.from_y) * i / steps;
            e.move_mouse(x, y, Coordinate::Abs)
                .map_err(|e| AppError::internal(e.to_string()))?;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        e.button(btn, Direction::Release)
            .map_err(|e| AppError::internal(e.to_string()))?;

        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

fn parse_button(s: &str) -> Button {
    match s {
        "right" => Button::Right,
        "middle" => Button::Middle,
        _ => Button::Left,
    }
}
