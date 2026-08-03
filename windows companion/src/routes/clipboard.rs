use axum::Json;
use serde::{Deserialize, Serialize};

use crate::routes::{ActionResponse, AppError};

#[derive(Serialize)]
pub struct ClipboardResponse {
    pub text: String,
}

#[derive(Deserialize)]
pub struct ClipboardRequest {
    pub text: String,
}

pub async fn get_clipboard() -> Result<Json<ClipboardResponse>, AppError> {
    tokio::task::spawn_blocking(|| {
        let text = clipboard_win::get_clipboard_string()
            .map_err(|e| AppError::internal(e.to_string()))?;
        Ok(Json(ClipboardResponse { text }))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

pub async fn set_clipboard(
    Json(req): Json<ClipboardRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        clipboard_win::set_clipboard_string(&req.text)
            .map_err(|e| AppError::internal(e.to_string()))?;
        Ok(ActionResponse::ok())
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}
