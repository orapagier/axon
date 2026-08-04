pub mod agent;
pub mod clipboard;
pub mod files;
pub mod keyboard;
pub mod mouse;
pub mod notification;
pub mod registry;
pub mod screenshot;
pub mod shell;
pub mod system;
pub mod window;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};

// ──────────────────────────────────────────────────────────────────────────────
// Shared success response
// ──────────────────────────────────────────────────────────────────────────────

/// Standard success response shared across all route modules.
#[derive(Serialize, Clone)]
pub struct ActionResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ActionResponse {
    pub fn ok() -> Json<Self> {
        Json(Self {
            success: true,
            message: None,
        })
    }

    pub fn with_message(msg: impl Into<String>) -> Json<Self> {
        Json(Self {
            success: true,
            message: Some(msg.into()),
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Structured error type
// ──────────────────────────────────────────────────────────────────────────────

/// Structured JSON error response for the API.
pub struct AppError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl AppError {
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "INTERNAL_ERROR",
            message: msg.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "BAD_REQUEST",
            message: msg.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "NOT_FOUND",
            message: msg.into(),
        }
    }

    pub fn timeout(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::REQUEST_TIMEOUT,
            code: "TIMEOUT",
            message: msg.into(),
        }
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "UNAUTHORIZED",
            message: msg.into(),
        }
    }

    /// Used by the `cfg(not(windows))` stubs in `window.rs`. Those branches are
    /// never compiled on Windows, so a missing constructor here went unnoticed
    /// while still breaking any non-Windows build.
    #[allow(dead_code)]
    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            code: "NOT_IMPLEMENTED",
            message: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": self.message,
            "code": self.code,
        });
        (self.status, Json(body)).into_response()
    }
}

impl From<tokio::task::JoinError> for AppError {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::internal(e.to_string())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Health check
// ──────────────────────────────────────────────────────────────────────────────

/// Health check — no auth required
pub async fn ping() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "version": env!("CARGO_PKG_VERSION"),
            "service": "win-automation-api"
        })),
    )
}
