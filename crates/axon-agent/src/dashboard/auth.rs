use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use std::env;

pub async fn require_auth(req: Request, next: Next) -> Result<Response, StatusCode> {
    let master_key = env::var("AXON_MASTER_KEY").unwrap_or_default();

    // If no key is configured, allow access (for backward compatibility / local dev)
    if master_key.is_empty() {
        return Ok(next.run(req).await);
    }

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok());

    let query_key = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|p| p.strip_prefix("api_key="))
            // The browser sends the key via encodeURIComponent (see axon-ui ws.js),
            // so it must be percent-decoded before comparison. Keys containing
            // characters such as '+', '/', '=' or spaces — common in base64/random
            // secrets — would otherwise never match, breaking WebSocket auth even
            // though REST (which sends the raw key in the Bearer header) still works.
            .map(|raw| {
                urlencoding::decode(raw)
                    .map(|d| d.into_owned())
                    .unwrap_or_else(|_| raw.to_string())
            })
    });

    let provided = if let Some(h) = auth_header {
        h.strip_prefix("Bearer ").unwrap_or(h).to_string()
    } else if let Some(q) = query_key {
        q
    } else {
        "".to_string()
    };

    if provided == master_key {
        Ok(next.run(req).await)
    } else {
        tracing::warn!("Unauthorized access attempt to dashboard (invalid master key)");
        Err(StatusCode::UNAUTHORIZED)
    }
}
