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

/// Shared HTTP client. Building a `reqwest::Client` per request throws away the
/// connection pool and re-initialises the TLS/certificate machinery every call.
static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();

fn client() -> &'static Client {
    CLIENT.get_or_init(Client::new)
}

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
    let client = client();
    let port = state.config.port;
    let token = &state.config.api_secret;
    let public_url = state.config.public_url.clone();

    // Ensure the path starts with /
    let path = if req.path.starts_with('/') {
        req.path.clone()
    } else {
        format!("/{}", req.path)
    };

    // Reject recursive or forbidden proxying.
    //
    // The guard compares the ROUTE, not the raw string: axum matches on path
    // only, so "/agent?x=1" and "/agent#f" both reach this same handler while
    // comparing unequal to "/agent". Strip the query and fragment first, then
    // normalise trailing slashes and duplicate separators.
    let route = normalize_route(&path);
    if route == "/agent" || route.starts_with("/public") {
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

    // Convert base64 blobs in the response to downloadable file URLs, exactly
    // as screenshots are handled — but never for endpoints whose payload is
    // user text (see returns_user_text).
    let resp_body = if returns_user_text(&route) {
        resp_body
    } else {
        save_base64_fields(resp_body, &path, &public_url).await
    };

    Ok(Json(resp_body))
}

/// Reduce a caller-supplied path to the route axum will actually match:
/// drop the query string and fragment, collapse repeated slashes, and strip a
/// trailing slash. Used only for the security guard — the original path is
/// still what gets proxied.
fn normalize_route(path: &str) -> String {
    let no_query = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');

    let mut out = String::with_capacity(no_query.len() + 1);
    let mut last_was_slash = false;
    for c in no_query.chars() {
        if c == '/' {
            if last_was_slash {
                continue;
            }
            last_was_slash = true;
        } else {
            last_was_slash = false;
        }
        out.push(c);
    }

    if out.is_empty() {
        "/".to_string()
    } else {
        out
    }
}

/// Endpoints whose response carries text the user asked for verbatim.
///
/// Base64 rewriting must never touch these: a text file, clipboard entry, or
/// shell output that happens to be a valid base64 string would otherwise be
/// deleted and replaced with a download link, losing the content the caller
/// actually requested.
fn returns_user_text(route: &str) -> bool {
    matches!(
        route,
        "/files/read"
            | "/files/list"
            | "/files/search"
            | "/clipboard"
            | "/shell"
            | "/system/env"
            | "/registry/read"
            | "/windows"
            | "/processes"
    )
}

/// Field names that carry binary payloads. Rewriting is restricted to these so
/// an ordinary long string is never mistaken for a blob. Deliberately excludes
/// ambiguous names like `content`, `data`, and `result`.
fn is_binary_field(name: &str) -> bool {
    matches!(
        name,
        "image"
            | "photo"
            | "screenshot"
            | "thumbnail"
            | "blob"
            | "bytes"
            | "audio"
            | "video"
            | "attachment"
    )
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
                        Value::String(s) if is_binary_field(&key) && is_base64_blob(s) => {
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
    // The random component matters: /public/* is unauthenticated, so a purely
    // timestamped name could be enumerated by guessing recent seconds.
    let filename = format!(
        "{}_{}_{}.{}",
        stem,
        ts,
        crate::server::random_token(),
        ext
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    fn is_blocked(path: &str) -> bool {
        let route = normalize_route(path);
        route == "/agent" || route.starts_with("/public")
    }

    #[test]
    fn blocks_plain_recursive_paths() {
        assert!(is_blocked("/agent"));
        assert!(is_blocked("/agent/"));
        assert!(is_blocked("/public/x.png"));
    }

    #[test]
    fn blocks_recursion_disguised_by_query_or_fragment() {
        // axum routes on the path alone, so these all reach /agent.
        assert!(is_blocked("/agent?x=1"));
        assert!(is_blocked("/agent#frag"));
        assert!(is_blocked("/agent/?x=1"));
        assert!(is_blocked("//agent"));
        assert!(is_blocked("/agent//"));
    }

    #[test]
    fn allows_normal_endpoints() {
        assert!(!is_blocked("/shell"));
        assert!(!is_blocked("/screenshot?screen=1"));
        assert!(!is_blocked("/files/read"));
        // Not the /public route — a different path that merely starts similarly
        // is still blocked by the prefix rule, which is the conservative choice.
        assert!(!is_blocked("/processes"));
    }

    #[test]
    fn text_endpoints_are_exempt_from_base64_rewriting() {
        assert!(returns_user_text("/files/read"));
        assert!(returns_user_text("/clipboard"));
        assert!(returns_user_text("/shell"));
        assert!(!returns_user_text("/screenshot"));
    }

    #[test]
    fn only_binary_field_names_are_rewritten() {
        assert!(is_binary_field("image"));
        assert!(is_binary_field("screenshot"));
        // `content` is the /files/read text field — rewriting it destroyed the
        // file contents the caller asked for.
        assert!(!is_binary_field("content"));
        assert!(!is_binary_field("data"));
        assert!(!is_binary_field("result"));
        assert!(!is_binary_field("stdout"));
    }

    #[test]
    fn base64_detection_needs_length_and_charset() {
        assert!(!is_base64_blob("short"));
        assert!(!is_base64_blob(&"hello world ".repeat(40)));
        assert!(is_base64_blob(&"QUJDRA".repeat(60)));
    }
}
