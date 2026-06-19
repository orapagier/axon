//! Temporary local-media serving for Instagram publishing.
//!
//! When the Instagram service stages media for a publish, Meta's API must fetch
//! it from a public URL. The in-process MCP state registers each file under a
//! short-lived token; these routes serve that file (with HTTP range support).
//! Public (unauthenticated) — Meta cannot present the dashboard master key.
//!
//! Ported from the former axon-mcp HTTP server now that the integration
//! services run in-process.

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

use crate::state::AppState;

fn parse_byte_range(range_header: &str, total_len: usize) -> Option<(usize, usize)> {
    if total_len == 0 {
        return None;
    }
    let value = range_header
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?
        .trim();
    if value.is_empty() {
        return None;
    }
    let (start_raw, end_raw) = value.split_once('-')?;

    if start_raw.is_empty() {
        // Suffix range: bytes=-N
        let suffix_len: usize = end_raw.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let len = suffix_len.min(total_len);
        return Some((total_len - len, total_len.saturating_sub(1)));
    }

    let start: usize = start_raw.parse().ok()?;
    if start >= total_len {
        return None;
    }
    let end = if end_raw.is_empty() {
        total_len - 1
    } else {
        end_raw.parse::<usize>().ok()?.min(total_len - 1)
    };
    if start > end {
        return None;
    }
    Some((start, end))
}

async fn serve_token(
    token: &str,
    state: &AppState,
    range_header: Option<&str>,
    head_only: bool,
) -> Response {
    let Some(mcp_state) = state.mcp.inprocess_state().await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(file) = mcp_state.resolve_temp_media_file(token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let total_len = match tokio::fs::metadata(&file.path).await {
        Ok(meta) => meta.len() as usize,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response()
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let bytes = match tokio::fs::read(&file.path).await {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return StatusCode::NOT_FOUND.into_response()
        }
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let mut headers = HeaderMap::new();
    if let Some(ct) = file.content_type {
        if let Ok(v) = ct.parse() {
            headers.insert(header::CONTENT_TYPE, v);
        }
    }
    if let Ok(v) = "public, max-age=7200".parse() {
        headers.insert(header::CACHE_CONTROL, v);
    }
    if let Ok(v) = "bytes".parse() {
        headers.insert(header::ACCEPT_RANGES, v);
    }

    let range = range_header.and_then(|v| parse_byte_range(v, total_len));
    let (status, content_range, body_bytes) = if let Some((start, end)) = range {
        (
            StatusCode::PARTIAL_CONTENT,
            Some(format!("bytes {}-{}/{}", start, end, total_len)),
            bytes[start..=end].to_vec(),
        )
    } else {
        (StatusCode::OK, None, bytes)
    };

    if let Ok(v) = body_bytes.len().to_string().parse() {
        headers.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(cr) = content_range {
        if let Ok(v) = cr.parse() {
            headers.insert(header::CONTENT_RANGE, v);
        }
    }

    if head_only {
        return (status, headers, axum::body::Body::empty()).into_response();
    }
    (status, headers, body_bytes).into_response()
}

fn range_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string())
}

pub async fn local_media(
    Path(token): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    serve_token(&token, &state, range_of(&headers).as_deref(), false).await
}

pub async fn local_media_head(
    Path(token): Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    serve_token(&token, &state, range_of(&headers).as_deref(), true).await
}

pub async fn local_media_named(
    Path((token, _name)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    serve_token(&token, &state, range_of(&headers).as_deref(), false).await
}

pub async fn local_media_named_head(
    Path((token, _name)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    serve_token(&token, &state, range_of(&headers).as_deref(), true).await
}
