// http.rs
// Native HTTP request tool for Axon
// Provides functionality similar to n8n's HTTP Request node

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:124.0) Gecko/20100101 Firefox/124.0",
];

const REFERERS: &[&str] = &[
    "https://www.google.com/",
    "https://www.bing.com/",
    "https://duckduckgo.com/",
    "https://t.co/",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpRequestParams {
    pub method: String,
    pub url: String,
    pub headers: Option<serde_json::Value>,
    pub query: Option<serde_json::Value>,
    pub body: Option<serde_json::Value>,
    pub auth: Option<HttpAuth>,
    pub timeout_seconds: Option<u64>,
    pub response_format: Option<String>,
    pub limit: Option<usize>,
    pub proxy: Option<String>,
    pub send_binary_data: Option<bool>,
    pub binary_property: Option<String>,
    pub body_content_type: Option<String>, // "json", "form-data", "multipart", "raw"
    pub stealth_headers: Option<bool>,
    pub raw_content_type: Option<String>, // "text/plain", "application/json", etc.
    pub allow_unauthorized_certs: Option<bool>,
    pub full_response: Option<bool>,
    pub data_cleaner: Option<bool>,
    // When data_cleaner is on, preserve <a> links as Markdown [text](absolute-url).
    pub keep_links: Option<bool>,
    pub always_output_binary: Option<bool>,
    // Synapse UI fields: jsonBody is a raw JSON string, specify_body controls how body is sent
    pub json_body: Option<String>,
    pub specify_body: Option<String>,
    pub header_parameters: Option<serde_json::Value>, // {"parameters": [{"name":..,"value":..}]}
    // Redirect handling
    pub follow_redirects: Option<bool>, // default true
    pub max_redirects: Option<usize>,   // default 10
    // Retry on failure (network errors, 5xx, 429)
    pub retry_on_fail: Option<bool>,
    pub max_tries: Option<u32>,         // total attempts incl. the first
    pub retry_interval_ms: Option<u64>, // wait between attempts
    // When true, a 4xx/5xx response is turned into an Err instead of a normal
    // result, so the node's continue-on-fail / error-workflow machinery fires.
    pub fail_on_error_status: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpAuth {
    pub auth_type: String, // "basic", "header"
    pub user: Option<String>,
    pub password: Option<String>,
    pub header_name: Option<String>,
    pub header_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: serde_json::Value,
    pub body: serde_json::Value,
    pub text_content: Option<String>,
    pub binary: Option<crate::files::AttachedFile>,
}

pub struct HttpRequestTool {
    client: reqwest::Client,
}

impl HttpRequestTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .cookie_store(true)
            .referer(true)
            // Default redirect limit aligned with the UI default so the common
            // case can reuse this pooled client (keeps connections warm).
            .redirect(reqwest::redirect::Policy::limited(21))
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    fn generate_stealth_headers(&self, ua: &str) -> HeaderMap {
        let mut hmap = HeaderMap::new();
        let referer = REFERERS[rand::random::<usize>() % REFERERS.len()];

        hmap.insert("User-Agent", HeaderValue::from_str(ua).unwrap());
        hmap.insert("Accept", HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"));
        hmap.insert(
            "Accept-Language",
            HeaderValue::from_static("en-US,en;q=0.9"),
        );
        hmap.insert(
            "Accept-Encoding",
            HeaderValue::from_static("gzip, deflate, br"),
        );
        hmap.insert("Upgrade-Insecure-Requests", HeaderValue::from_static("1"));
        hmap.insert("Sec-Fetch-Dest", HeaderValue::from_static("document"));
        hmap.insert("Sec-Fetch-Mode", HeaderValue::from_static("navigate"));
        hmap.insert("Sec-Fetch-Site", HeaderValue::from_static("none"));
        hmap.insert("Sec-Fetch-User", HeaderValue::from_static("?1"));
        hmap.insert("Referer", HeaderValue::from_str(referer).unwrap());

        // Chrome-specific client hints (if it looks like Chrome)
        if ua.contains("Chrome") {
            hmap.insert(
                "Sec-Ch-Ua",
                HeaderValue::from_static(
                    "\"Google Chrome\";v=\"123\", \"Not:A-Brand\";v=\"8\", \"Chromium\";v=\"123\"",
                ),
            );
            hmap.insert("Sec-Ch-Ua-Mobile", HeaderValue::from_static("?0"));
            hmap.insert(
                "Sec-Ch-Ua-Platform",
                HeaderValue::from_static("\"Windows\""),
            );
        }

        hmap
    }

    pub async fn request(&self, params: HttpRequestParams) -> Result<HttpResponse> {
        let method = match params.method.to_uppercase().as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "PATCH" => reqwest::Method::PATCH,
            "DELETE" => reqwest::Method::DELETE,
            "HEAD" => reqwest::Method::HEAD,
            "OPTIONS" => reqwest::Method::OPTIONS,
            _ => anyhow::bail!("Unsupported HTTP method: {}", params.method),
        };

        let ua = USER_AGENTS[rand::random::<usize>() % USER_AGENTS.len()];
        let timeout = params.timeout_seconds.unwrap_or(30);
        let allow_unauthorized = params.allow_unauthorized_certs.unwrap_or(false);

        // Avoid logging the proxy URL — it may embed credentials (user:pass@host).
        tracing::debug!(
            "HTTP {} {} (proxy={}, allow_unauthorized={})",
            method,
            params.url,
            params.proxy.is_some(),
            allow_unauthorized
        );

        // Redirect handling — the pooled client already follows up to 21 redirects,
        // so only build a request-scoped client when the policy actually differs.
        let follow_redirects = params.follow_redirects.unwrap_or(true);
        let redirect_limit = params.max_redirects.unwrap_or(21);
        let custom_redirect = !follow_redirects || redirect_limit != 21;

        // A proxy, relaxed TLS, or a non-default redirect policy each force a
        // request-scoped client (these can't be set per-request on the pool).
        let client = if params.proxy.is_some() || allow_unauthorized || custom_redirect {
            let redirect_policy = if !follow_redirects {
                reqwest::redirect::Policy::none()
            } else {
                reqwest::redirect::Policy::limited(redirect_limit)
            };
            let mut builder = reqwest::Client::builder()
                .timeout(Duration::from_secs(timeout))
                .cookie_store(true)
                .danger_accept_invalid_certs(allow_unauthorized)
                .redirect(redirect_policy)
                .referer(true)
                .gzip(true)
                .brotli(true)
                .deflate(true);

            if let Some(proxy_url) = &params.proxy {
                tracing::debug!("Configuring outbound proxy");
                let proxy = reqwest::Proxy::all(proxy_url).context("Invalid proxy URL")?;
                builder = builder.proxy(proxy);
            }

            builder
                .build()
                .context("Failed to build custom HTTP client")?
        } else {
            self.client.clone()
        };

        let mut headers = if params.stealth_headers.unwrap_or(false) {
            self.generate_stealth_headers(ua)
        } else {
            HeaderMap::new()
        };

        // Handle Query Parameters
        let mut final_url = params.url.trim().to_string();
        if final_url.is_empty() {
            anyhow::bail!("URL is required");
        }
        if let Some(ref query_val) = params.query {
            if let Some(query_obj) = query_val.as_object() {
                if !query_obj.is_empty() {
                    // The query string must sit before any URL fragment, so split it
                    // off and re-attach it after appending params.
                    let (mut base, fragment) = match final_url.split_once('#') {
                        Some((b, f)) => (b.to_string(), Some(f.to_string())),
                        None => (final_url.clone(), None),
                    };
                    let mut connector = if base.contains('?') { "&" } else { "?" };
                    for (k, v) in query_obj {
                        let v_str = match v {
                            serde_json::Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        };
                        base.push_str(connector);
                        base.push_str(&urlencoding::encode(k));
                        base.push('=');
                        base.push_str(&urlencoding::encode(&v_str));
                        connector = "&";
                    }
                    if let Some(frag) = fragment {
                        base.push('#');
                        base.push_str(&frag);
                    }
                    final_url = base;
                }
            }
        }

        // Add default Accept header if not present (n8n style)
        let has_accept = params
            .headers
            .as_ref()
            .and_then(|h| h.as_object())
            .map(|obj| obj.keys().any(|k| k.to_lowercase() == "accept"))
            .unwrap_or(false);

        if !has_accept {
            let accept_val = match params.response_format.as_deref() {
                Some("json") => "application/json,text/*;q=0.99",
                Some("text") => {
                    "application/json,text/html,application/xhtml+xml,application/xml,text/*;q=0.9, */*;q=0.1"
                }
                _ => "application/json,text/html,application/xhtml+xml,application/xml,text/*;q=0.9, image/*;q=0.8, */*;q=0.7",
            };
            headers.insert("Accept", HeaderValue::from_static(accept_val));
        }

        // Handle Authentication
        let mut basic_auth = None;
        if let Some(ref auth) = params.auth {
            match auth.auth_type.as_str() {
                "basicAuth" => {
                    basic_auth = Some((
                        auth.user.clone().unwrap_or_default(),
                        auth.password.clone().unwrap_or_default(),
                    ));
                }
                "headerAuth" => {
                    let name = auth
                        .header_name
                        .clone()
                        .unwrap_or_else(|| "Authorization".to_string());
                    let value = auth.header_value.clone().unwrap_or_default();
                    if let (Ok(hname), Ok(hval)) = (
                        HeaderName::from_bytes(name.as_bytes()),
                        HeaderValue::from_str(&value),
                    ) {
                        headers.insert(hname, hval);
                    }
                }
                "bearerAuth" => {
                    // header_value carries the raw token; normalize so we never
                    // double-prefix a value the user already pasted as "Bearer …".
                    let token = auth.header_value.clone().unwrap_or_default();
                    let token = token.trim();
                    let value = if token.to_lowercase().starts_with("bearer ") {
                        token.to_string()
                    } else {
                        format!("Bearer {token}")
                    };
                    if let Ok(hval) = HeaderValue::from_str(&value) {
                        headers.insert(reqwest::header::AUTHORIZATION, hval);
                    }
                }
                _ => {}
            }
        }

        if let Some(ref headers_val) = params.headers {
            if let Some(headers_obj) = headers_val.as_object() {
                for (k, v) in headers_obj {
                    let v_str = match v {
                        Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    if let (Ok(hname), Ok(hval)) = (
                        HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_str(&v_str),
                    ) {
                        headers.insert(hname, hval);
                    }
                }
            }
        }

        let mut rb = client.request(method, &final_url);

        if let Some((u, p)) = basic_auth {
            rb = rb.basic_auth(u, Some(p));
        }

        if let Some(t) = params.timeout_seconds {
            rb = rb.timeout(Duration::from_secs(t));
        }

        // Resolve Synapse UI fields: json_body (string) → body (Value)
        // Also merge header_parameters into params.headers
        let params = {
            let mut p = params;

            // Merge header_parameters (array format) into headers object
            if let Some(hp) = &p.header_parameters {
                if let Some(arr) = hp.get("parameters").and_then(|v| v.as_array()) {
                    let mut hobj = p
                        .headers
                        .as_ref()
                        .and_then(|h| h.as_object())
                        .cloned()
                        .unwrap_or_default();
                    for param in arr {
                        if let (Some(name), Some(val)) = (
                            param.get("name").and_then(|v| v.as_str()),
                            param.get("value").and_then(|v| v.as_str()),
                        ) {
                            if !name.is_empty() {
                                hobj.insert(name.to_string(), serde_json::json!(val));
                            }
                        }
                    }
                    p.headers = Some(serde_json::Value::Object(hobj));
                }
            }

            // If body is None/empty but json_body has content, parse and use it
            let body_is_empty = p
                .body
                .as_ref()
                .map(|b| b.is_null() || b.as_str().map(|s| s.trim().is_empty()).unwrap_or(false))
                .unwrap_or(true);

            if body_is_empty {
                if let Some(ref jb) = p.json_body {
                    let trimmed = jb.trim();
                    if !trimmed.is_empty() {
                        match serde_json::from_str::<serde_json::Value>(trimmed) {
                            Ok(parsed) => {
                                p.body = Some(parsed);
                                p.body_content_type = Some("json".to_string());
                            }
                            Err(_) => {
                                // Not valid JSON — send as raw string
                                p.body = Some(serde_json::Value::String(trimmed.to_string()));
                                p.body_content_type = Some("raw".to_string());
                            }
                        }
                    }
                }
            }
            p
        };

        if params.send_binary_data.unwrap_or(false) {
            if let Some(path) = &params.binary_property {
                if !path.is_empty() {
                    let bytes = tokio::fs::read(path)
                        .await
                        .context(format!("Failed to read binary body file at {}", path))?;
                    rb = rb.body(bytes);
                }
            }
        } else if let Some(body_val) = params.body {
            let explicit_type = params.body_content_type.as_deref().unwrap_or("json");

            match explicit_type {
                "form-urlencoded" => {
                    if let Some(obj) = body_val.as_object() {
                        let mut pairs = Vec::new();
                        for (k, v) in obj {
                            let v_str = match v {
                                Value::String(s) => s.clone(),
                                _ => v.to_string(),
                            };
                            pairs.push((k, v_str));
                        }
                        rb = rb.form(&pairs);
                    } else if let Value::String(s) = body_val {
                        rb = rb.header("Content-Type", "application/x-www-form-urlencoded");
                        rb = rb.body(s);
                    } else {
                        rb = rb.form(&body_val);
                    }
                }
                "multipart-form-data" => {
                    let mut form = reqwest::multipart::Form::new();
                    if let Some(obj) = body_val.as_object() {
                        for (k, v) in obj {
                            if let Some(file_obj) = v.as_object() {
                                if let Some(path_val) = file_obj.get("_axon_file_path") {
                                    if let Some(path_str) = path_val.as_str() {
                                        if let Ok(bytes) = tokio::fs::read(path_str).await {
                                            let file_name = std::path::Path::new(path_str)
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy()
                                                .to_string();
                                            let part = reqwest::multipart::Part::bytes(bytes)
                                                .file_name(file_name);
                                            form = form.part(k.clone(), part);
                                            continue;
                                        }
                                    }
                                }
                            }
                            let v_str = match v {
                                Value::String(s) => s.clone(),
                                _ => v.to_string(),
                            };
                            form = form.text(k.clone(), v_str);
                        }
                    }
                    rb = rb.multipart(form);
                }
                "raw" => {
                    if let Some(raw_ct) = &params.raw_content_type {
                        rb = rb.header("Content-Type", raw_ct);
                    }
                    let b_str = match body_val {
                        Value::String(s) => s,
                        _ => body_val.to_string(),
                    };
                    rb = rb.body(b_str);
                }
                _ => {
                    // Default: JSON
                    rb = rb.json(&body_val);
                }
            }
        }

        // Apply finalized headers
        rb = rb.headers(headers);

        // Retry on transient failures (network errors, 5xx, 429) when enabled.
        let max_tries = if params.retry_on_fail.unwrap_or(false) {
            params.max_tries.unwrap_or(3).max(1)
        } else {
            1
        };
        let retry_interval = Duration::from_millis(params.retry_interval_ms.unwrap_or(1000));

        // Retrying requires re-issuing the request, which needs a cloneable body
        // (streaming/multipart bodies can't be cloned — those run once).
        let response = if max_tries <= 1 || rb.try_clone().is_none() {
            rb.send().await.context("HTTP request failed")?
        } else {
            let mut resp = None;
            let mut last_err: Option<reqwest::Error> = None;
            for attempt in 1..=max_tries {
                let attempt_rb = rb.try_clone().expect("body is cloneable");
                match attempt_rb.send().await {
                    Ok(r) => {
                        let retryable = r.status().is_server_error() || r.status().as_u16() == 429;
                        if retryable && attempt < max_tries {
                            tracing::debug!(
                                "HTTP retry {}/{} after status {}",
                                attempt,
                                max_tries,
                                r.status()
                            );
                            tokio::time::sleep(retry_interval).await;
                            continue;
                        }
                        resp = Some(r);
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        if attempt < max_tries {
                            tracing::debug!(
                                "HTTP retry {}/{} after transport error",
                                attempt,
                                max_tries
                            );
                            tokio::time::sleep(retry_interval).await;
                            continue;
                        }
                    }
                }
            }
            match resp {
                Some(r) => r,
                None => {
                    return Err(anyhow::Error::from(
                        last_err.expect("a transport error was recorded"),
                    ))
                    .context("HTTP request failed after retries")
                }
            }
        };
        let status = response.status().as_u16();
        let headers = response.headers().clone();

        // Extract headers
        let mut resp_headers = serde_json::Map::new();
        for (name, value) in &headers {
            if let Ok(val_str) = value.to_str() {
                resp_headers.insert(name.to_string(), serde_json::json!(val_str));
            }
        }

        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = response
            .bytes()
            .await
            .context("Failed to read response body")?;

        // Detect binary types - allow common JSON/XML sub-types
        let is_binary = !content_type.contains("text/")
            && !content_type.contains("application/json")
            && !content_type.contains("application/xml")
            && !content_type.contains("+json")
            && !content_type.contains("+xml")
            && !content_type.is_empty();

        let force_binary = params.always_output_binary.unwrap_or(false)
            || params.response_format.as_deref() == Some("file");

        let (body, text_content, binary) =
            if !force_binary && content_type.contains("application/json") {
                let json_body: serde_json::Value = serde_json::from_slice(&bytes)
                    .unwrap_or_else(|_| serde_json::json!("(failed to parse JSON body)"));
                (json_body, None, None)
            } else if is_binary || force_binary {
                // Stage the file
                // 1. Try Content-Disposition header for a proper filename (includes extension).
                let cd_filename = resp_headers
                    .get("content-disposition")
                    .and_then(|v| v.as_str())
                    .and_then(|cd| {
                        cd.split(';').find_map(|part| {
                            let part = part.trim();
                            if part.to_lowercase().starts_with("filename*=") {
                                // RFC 5987: charset'lang'encoded-value — take the last token
                                let val = &part["filename*=".len()..];
                                val.split('\'').last().map(|s| {
                                    urlencoding::decode(s)
                                        .map(|d| d.into_owned())
                                        .unwrap_or_else(|_| s.to_string())
                                        .trim_matches('"')
                                        .to_string()
                                })
                            } else if part.to_lowercase().starts_with("filename=") {
                                Some(
                                    part["filename=".len()..]
                                        .trim_matches('"')
                                        .trim()
                                        .to_string(),
                                )
                            } else {
                                None
                            }
                        })
                    })
                    .filter(|s| !s.is_empty());

                // 2. Fall back to the last URL path segment (strip query string).
                let url_filename = {
                    let raw_segment = final_url
                        .split('#')
                        .next()
                        .unwrap_or(&final_url)
                        .rsplit('/')
                        .next()
                        .unwrap_or("download")
                        .split('?')
                        .next()
                        .unwrap_or("download");
                    let decoded = urlencoding::decode(raw_segment)
                        .map(|d| d.into_owned())
                        .unwrap_or_else(|_| raw_segment.to_string());
                    let trimmed = decoded.trim();
                    if trimmed.is_empty() {
                        "download".to_string()
                    } else {
                        trimmed.to_string()
                    }
                };

                let mut filename =
                    crate::files::sanitize_filename(&cd_filename.unwrap_or(url_filename));

                // 3. If the chosen filename still has no extension, infer one from Content-Type.
                if std::path::Path::new(&filename)
                    .extension()
                    .map(|e| e.is_empty())
                    .unwrap_or(true)
                {
                    let ext = match content_type
                        .split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_lowercase()
                        .as_str()
                    {
                        "image/jpeg" | "image/jpg" => "jpg",
                        "image/png" => "png",
                        "image/webp" => "webp",
                        "image/gif" => "gif",
                        "image/bmp" => "bmp",
                        "image/tiff" => "tiff",
                        "image/svg+xml" => "svg",
                        "image/avif" => "avif",
                        "application/pdf" => "pdf",
                        "video/mp4" => "mp4",
                        "video/webm" => "webm",
                        "video/quicktime" => "mov",
                        "audio/mpeg" => "mp3",
                        "audio/ogg" => "ogg",
                        "audio/wav" => "wav",
                        "application/zip" => "zip",
                        _ => "bin",
                    };
                    filename = format!("{}.{}", filename, ext);
                }

                filename = crate::files::sanitize_filename(&filename);

                let path = crate::files::stage_bytes(&bytes, &filename)
                    .context("Failed to stage binary response")?;

                let attached = crate::files::AttachedFile {
                    original_name: filename.clone(),
                    local_path: path.to_string_lossy().into_owned(),
                    mime_type: content_type.to_string(),
                    size: bytes.len() as u64,
                };

                let body_b64 = BASE64.encode(&bytes);
                (
                    serde_json::json!({
                        "filename": filename,
                        "size": bytes.len(),
                        "mime_type": content_type,
                        // base64-encoded bytes — usable as input_binary for image_tool
                        // if you prefer a zero-filesystem approach
                        "body": body_b64,
                    }),
                    None,
                    Some(attached),
                )
            } else {
                let text = String::from_utf8_lossy(&bytes).into_owned();

                if params.data_cleaner.unwrap_or(false) {
                    // Readability-style main-content extraction (Mozilla
                    // readability.js port): scores the DOM to find the actual
                    // article container and drops nav/footer/ads, instead of
                    // uniformly stripping every tag. Markdown text mode keeps
                    // links as [label](url) when Keep Links is on; Formatted
                    // mode yields plain paragraphs otherwise. A page dom_smoothie
                    // can't parse (e.g. a non-HTML body) falls back to the raw
                    // text rather than failing the node.
                    let text_mode = if params.keep_links.unwrap_or(false) {
                        dom_smoothie::TextMode::Markdown
                    } else {
                        dom_smoothie::TextMode::Formatted
                    };
                    let cfg = dom_smoothie::Config {
                        text_mode,
                        ..Default::default()
                    };
                    let cleaned =
                        dom_smoothie::Readability::new(text.clone(), Some(&final_url), Some(cfg))
                            .and_then(|mut r| r.parse())
                            .map(|article| article.text_content.to_string())
                            .unwrap_or_else(|_| text.clone());

                    (serde_json::json!(cleaned.clone()), Some(cleaned), None)
                } else {
                    (serde_json::json!(text.clone()), Some(text), None)
                }
            };

        // Fail-on-error: surface 4xx/5xx as an Err so the caller's
        // continue-on-fail / error-workflow path runs, instead of passing an
        // error body downstream as if it were data. Include a short snippet of
        // the response so the failure is diagnosable.
        if params.fail_on_error_status.unwrap_or(false) && status >= 400 {
            let snippet: String = match &body {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }
            .chars()
            .take(500)
            .collect();
            return Err(anyhow::anyhow!("HTTP {status}: {snippet}"));
        }

        // Return based on requested format
        let mut final_body = body;
        if params.full_response.unwrap_or(false) {
            let mut full = serde_json::Map::new();
            full.insert("body".to_string(), final_body);
            full.insert(
                "headers".to_string(),
                serde_json::Value::Object(resp_headers.clone()),
            );
            full.insert("statusCode".to_string(), serde_json::json!(status));
            final_body = serde_json::Value::Object(full);
        }

        match params.response_format.as_deref() {
            Some("text") => {
                let mut content = text_content.unwrap_or_else(|| {
                    if params.full_response.unwrap_or(false) {
                        final_body
                            .get("body")
                            .and_then(|b| b.as_str())
                            .unwrap_or_default()
                            .to_string()
                    } else {
                        final_body.as_str().unwrap_or_default().to_string()
                    }
                });

                // If limit is set, take first N lines/entries
                if let Some(limit) = params.limit {
                    let lines: Vec<&str> = content
                        .lines()
                        .map(|l| l.trim())
                        .filter(|l| !l.is_empty())
                        .take(limit)
                        .collect();
                    content = lines.join("\n");
                }

                Ok(HttpResponse {
                    status,
                    headers: serde_json::json!({}), // Skip headers for text mode to save tokens
                    body: serde_json::json!(content),
                    text_content: None,
                    binary,
                })
            }
            _ => Ok(HttpResponse {
                status,
                headers: serde_json::Value::Object(resp_headers),
                body: final_body,
                text_content,
                binary,
            }),
        }
    }
}

use crate::tools::schema::{ToolDefinition, ToolSource};

pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "synapse".to_string(),
        description: "Execute a raw HTTP request (GET, POST, etc). Use this for ad-hoc API calls or when no specific tool exists for a service.".to_string(),
        parameters: serde_json::json!({
            "url": {"type":"string"},
            "method": {"type":"string", "enum":["GET","POST","PUT","DELETE","PATCH","HEAD","OPTIONS"], "default":"GET"},
            "headers": {"type":"object", "description":"JSON object of headers"},
            "body": {"type":"object", "description":"JSON body for POST/PUT"},
            "timeout_seconds": {"type":"integer", "default":30},
            "response_format": {"type":"string", "enum":["full","text","file"], "default":"text"},
            "limit": {"type":"integer", "description":"Max results for text mode"},
            "proxy": {"type":"string", "description":"Optional proxy URL"},
            "data_cleaner": {"type":"boolean", "description":"Whether to extract only textual content from HTML responses"}
        }),
        required: vec!["url".to_string()],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: true,
    }
}

pub fn list_saved_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "list_synapses".to_string(),
        description: "List all Synapses (saved HTTP requests) from the database. Use this to discover existing API integrations.".to_string(),
        parameters: serde_json::json!({}),
        required: vec![],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: false,
    }
}

pub fn run_saved_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "run_synapse".to_string(),
        description: "Run one of the Synapses (saved HTTP requests) from the database. You can override parameters like the body or query.".to_string(),
        parameters: serde_json::json!({
            "name_or_id": { "type":"string", "description":"The name or ID of the saved Synapse" },
            "body_override": { "type":"object", "description":"Optional JSON body to replace the saved one" },
            "header_overrides": { "type":"object", "description":"Optional headers to merge/replace" }
        }),
        required: vec!["name_or_id".to_string()],
        source: ToolSource::Internal,
        enabled: true,
        is_mutating: true,
    }
}

#[cfg(test)]
mod data_cleaner_tests {
    use super::HttpRequestTool;

    // Readability extraction drops nav/footer boilerplate and keeps the
    // article body — the thing the old uniform tag-stripper couldn't do.
    #[test]
    fn readability_drops_boilerplate_keeps_article() {
        let html = r#"<html><body>
            <nav><a href="/">Home</a> <a href="/about">About</a></nav>
            <article>
                <h1>Headline</h1>
                <p>This is the real article content, long enough for the
                readability scorer to confidently pick it as the main
                candidate over the surrounding navigation and footer chrome
                that every page on this site repeats verbatim.</p>
            </article>
            <footer>Copyright 2026. All rights reserved. Contact us.</footer>
        </body></html>"#;

        let mut r =
            dom_smoothie::Readability::new(html, Some("https://example.com/post"), None).unwrap();
        let article = r.parse().unwrap();
        let text = article.text_content.to_string();

        assert!(text.contains("real article content"), "got: {text}");
        assert!(!text.contains("Copyright 2026"), "footer leaked: {text}");
    }

    // Keep Links (Markdown text mode) preserves the link as [label](url).
    #[test]
    fn markdown_mode_keeps_links() {
        let html = r#"<html><body><article>
            <h1>Headline</h1>
            <p>Read more in <a href="/deep-dive">our deep dive</a> about this
            topic, which has enough surrounding text for readability to treat
            this paragraph as the article body rather than noise.</p>
        </article></body></html>"#;

        let cfg = dom_smoothie::Config {
            text_mode: dom_smoothie::TextMode::Markdown,
            ..Default::default()
        };
        let mut r =
            dom_smoothie::Readability::new(html, Some("https://example.com/post"), Some(cfg))
                .unwrap();
        let article = r.parse().unwrap();
        let text = article.text_content.to_string();

        assert!(
            text.contains("[our deep dive](https://example.com/deep-dive)"),
            "got: {text}"
        );
    }

    // A body dom_smoothie can't parse as an article (e.g. no real content)
    // must not panic the caller — http.rs falls back to the raw text.
    #[test]
    fn unparsable_body_is_handled_by_caller_fallback() {
        let text = "not html at all, just plain text".to_string();
        let cleaned = dom_smoothie::Readability::new(text.clone(), None, None)
            .and_then(|mut r| r.parse())
            .map(|a| a.text_content.to_string())
            .unwrap_or_else(|_| text.clone());
        assert!(!cleaned.is_empty());
    }

    #[test]
    fn tool_constructs() {
        let _ = HttpRequestTool::new();
    }
}
