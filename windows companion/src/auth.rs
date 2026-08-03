use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::routes::AppError;
use crate::server::AppState;

/// Constant-time byte comparison to prevent timing attacks on auth tokens.
/// Returns `true` only if both slices have the same length and identical contents.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

pub async fn require_auth(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match token {
        Some(t) => {
            if constant_time_eq(t.as_bytes(), state.config.api_secret.as_bytes()) {
                tracing::info!("Auth successful");
                next.run(request).await
            } else {
                // SECURITY: never log the provided or expected token value —
                // doing so would leak secrets into log files / stdout.
                tracing::warn!("Auth failed: Bearer token mismatch");
                AppError::unauthorized("Invalid or missing Bearer token").into_response()
            }
        }
        _ => {
            tracing::warn!("Auth failed: Authorization header missing or malformed");
            AppError::unauthorized("Invalid or missing Bearer token").into_response()
        }
    }
}
