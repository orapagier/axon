use axum::{extract::State, http::Method, Json};
use base64::{engine::general_purpose, Engine};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::fs;

use crate::{routes::AppError, server::AppState};

/// How long (in minutes) before old files are cleaned up
const FILE_TTL_MINUTES: u64 = 30;

#[derive(Deserialize)]
pub struct ProxyRequest {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: Value,
}

pub async fn proxy_endpoint(
    State(state): State<AppState>,
    Json(req): Json<ProxyRequest>,
) -> Result<Json<Value>, AppError> {
    let client = Client::new();
    let port = state.config.port;
    let token = &state.config.api_secret;
    let public_url = state.config.public_url.clone();

    // Ensure the path starts with /
    let path = if req.path.starts_with('/') {
        req.path.clone()
    } else {
        format!("/{}", req.path)
    };

    // Reject recursive proxy
    if path == "/agent" || path == "/agent/" || path.starts_with("/public") {
        return Err(AppError::bad_request("Recursive or forbidden proxy path"));
    }

    let url = format!("http://127.0.0.1:{}{}", port, path);

    // Save method string before moving into reqwest builder so we can check it later
    let method_upper = req.method.to_uppercase();
    let method = match method_upper.as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "DELETE" => Method::DELETE,
        "PATCH" => Method::PATCH,
        _ => {
            return Err(AppError::bad_request(format!(
                "Unsupported HTTP method: {}",
                req.method
            )))
        }
    };

    let mut request = client.request(method, &url);
    if !token.is_empty() {
        request = request.header("Authorization", format!("Bearer {}", token));
    }

    // GET requests must NOT send a body — axum Query extractors ignore it and
    // some handlers return 422 if Content-Type: application/json is present
    // without a matching body extractor. Only attach a JSON body for methods
    // that semantically carry one (POST, PUT, PATCH, DELETE with body).
    let has_body = method_upper != "GET"
        && !req.body.is_null()
        && req.body != json!({});

    let response = if has_body {
        request.json(&req.body).send().await
    } else {
        request.send().await
    }
    .map_err(|e| AppError::internal(format!("Proxy network error: {}", e)))?;

    let status = response.status();
    // Always read as text first — never call .json() directly to avoid parse
    // failures on plain-text, empty, or HTML error bodies.
    let text = response.text().await.unwrap_or_default();

    let resp_body: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            if text.trim().is_empty() {
                json!({ "result": "Success" })
            } else {
                json!({ "result": text.trim() })
            }
        }
    };

    if !status.is_success() {
        let error_msg = resp_body
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| resp_body.to_string());
        return Err(AppError {
            status,
            code: "PROXY_ERROR",
            message: error_msg,
        });
    }

    // Convert any base64 blobs in the response to downloadable file URLs,
    // exactly as screenshots are handled.
    let resp_body = save_base64_fields(resp_body, &path, &public_url).await;

    Ok(Json(resp_body))
}

/// Recursively walks response JSON.  Any string field that looks like a base64
/// blob is decoded, written to the public directory, and replaced with a
/// `{ "url": "...", "note": "..." }` object — identical to how screenshots work.
fn save_base64_fields<'a>(
    val: Value,
    endpoint_path: &'a str,
    public_url: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
    Box::pin(async move {
        match val {
            Value::Object(map) => {
                let mut new_map = serde_json::Map::new();
                for (key, v) in map {
                    let new_val = match &v {
                        Value::String(s) if is_base64_blob(s) => {
                            match save_blob(s, &key, endpoint_path, public_url).await {
                                Ok(url) => json!({
                                    "url": url,
                                    "note": "File saved for 30 minutes. Download via the URL."
                                }),
                                Err(_) => v,
                            }
                        }
                        Value::Object(_) | Value::Array(_) => {
                            save_base64_fields(v, endpoint_path, public_url).await
                        }
                        _ => v,
                    };
                    new_map.insert(key, new_val);
                }
                Value::Object(new_map)
            }
            // Recurse into arrays so files inside list responses are also converted
            Value::Array(arr) => {
                let mut new_arr = Vec::with_capacity(arr.len());
                for item in arr {
                    new_arr.push(save_base64_fields(item, endpoint_path, public_url).await);
                }
                Value::Array(new_arr)
            }
            other => other,
        }
    })
}

/// Returns true if the string looks like a base64 blob (long enough and
/// only contains valid base64 characters in the first sample window).
fn is_base64_blob(s: &str) -> bool {
    if s.len() < 256 {
        return false;
    }
    s.chars()
        .take(64)
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
}

/// Decodes a base64 string, saves to `public/{stem}_{timestamp}.{ext}`, returns URL.
async fn save_blob(
    b64: &str,
    field_name: &str,
    endpoint_path: &str,
    public_url: &str,
) -> anyhow::Result<String> {
    let bytes = general_purpose::STANDARD.decode(b64)?;

    let ext = guess_extension(&bytes, endpoint_path);
    let stem = field_name_to_stem(field_name, endpoint_path);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let filename = format!("{}_{}.{}", stem, ts, ext);

    let public_dir = crate::server::public_dir();
    fs::create_dir_all(&public_dir).await?;
    fs::write(public_dir.join(&filename), &bytes).await?;

    // Clean up old files in the background
    tokio::spawn(cleanup_old_files(public_dir, FILE_TTL_MINUTES));

    // Fall back to a relative URL if public_url is not configured so the link
    // is at least usable locally (e.g. through the tunnel's base domain).
    let base = if public_url.trim().is_empty() {
        // Try to derive from the tunnel — caller should really set public_url
        tracing::warn!("public_url is not configured; file URL will be relative");
        String::new()
    } else {
        public_url.trim_end_matches('/').to_string()
    };

    Ok(format!("{}/public/{}", base, filename))
}

/// Guess file extension from magic bytes, falling back to endpoint context.
fn guess_extension(bytes: &[u8], endpoint_path: &str) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        return "png";
    }
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        return "jpg";
    }
    if bytes.starts_with(b"GIF") {
        return "gif";
    }
    if bytes.get(8..12) == Some(b"WEBP") {
        return "webp";
    }
    if bytes.starts_with(b"%PDF") {
        return "pdf";
    }
    if bytes.starts_with(b"PK") {
        return "zip";
    }
    if endpoint_path.contains("screenshot") {
        return "png";
    }
    if endpoint_path.contains("camera") || endpoint_path.contains("photo") {
        return "jpg";
    }
    "bin"
}

/// Convert field/endpoint name to a clean file stem.
fn field_name_to_stem<'a>(field_name: &'a str, endpoint_path: &'a str) -> &'a str {
    if endpoint_path.contains("screenshot") {
        return "screenshot";
    }
    if endpoint_path.contains("camera") {
        return "photo";
    }
    match field_name {
        "image" | "data" | "content" | "result" => "file",
        other => other,
    }
}

/// Delete files older than `ttl_minutes` from the public directory.
async fn cleanup_old_files(dir: PathBuf, ttl_minutes: u64) {
    let ttl = std::time::Duration::from_secs(ttl_minutes * 60);
    let Ok(mut entries) = fs::read_dir(&dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(meta) = entry.metadata().await {
            let time = meta
                .created()
                .unwrap_or_else(|_| meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH));
            if let Ok(age) = time.elapsed() {
                if age > ttl {
                    let _ = fs::remove_file(entry.path()).await;
                }
            }
        }
    }
}
