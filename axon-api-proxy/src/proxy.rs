use axum::{
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use tracing::{info, warn};

use crate::{error::ApiErr, AppState};

pub async fn list_models(State(s): State<AppState>) -> impl IntoResponse {
    let pool = s.pool.read().await.clone();
    let mut seen = HashSet::new();
    let models: Vec<Value> = pool
        .slots
        .iter()
        .filter(|slot| seen.insert(slot.model_id.clone()))
        .map(|slot| {
            json!({
                "id": slot.model_id,
                "object": "model",
                "created": 0,
                "owned_by": slot.provider,
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": models }))
}

const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

pub async fn proxy_handler(
    State(s): State<AppState>,
    req: Request<Body>,
) -> Result<Response, ApiErr> {
    // Proxy-level auth
    if !s.proxy_secret.is_empty() {
        let auth = req
            .headers()
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if auth.trim_start_matches("Bearer ").trim() != s.proxy_secret {
            return Err(ApiErr(
                StatusCode::UNAUTHORIZED,
                "Invalid or missing AXON_API_KEY — this is the proxy's own auth, \
                 not an upstream provider error. Check your client configuration."
                    .into(),
            ));
        }
    }

    let pool = s.pool.read().await.clone();
    let n = pool.slots.len();
    if n == 0 {
        return Err(ApiErr(
            StatusCode::SERVICE_UNAVAILABLE,
            "No active slots in pool".into(),
        ));
    }

    let available = pool.available_count();
    if available == 0 {
        return Err(ApiErr(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "All {} slot(s) are in cooldown. Try again in a few seconds.",
                n
            ),
        ));
    }

    let path = req.uri().path().to_string();
    let query = req.uri().query().map(|q| q.to_string());
    let method = req.method().clone();
    let base_headers = {
        let mut h = req.headers().clone();
        for hdr in &[
            "host",
            "authorization",
            "x-api-key",
            "transfer-encoding",
            "connection",
            "content-length",
        ] {
            h.remove(*hdr);
        }
        h
    };

    let raw_body = axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|e| {
            ApiErr(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Request body too large (max {} MB): {}",
                    MAX_BODY_BYTES / 1024 / 1024,
                    e
                ),
            )
        })?;

    let parsed_json: Option<Value> = if raw_body.is_empty() {
        warn!("-> received an empty body (0 bytes) from client!");
        None
    } else {
        serde_json::from_slice::<Value>(&raw_body).ok()
    };

    let mut last_err: Option<String> = None;

    for attempt in 0..n {
        let (slot_idx, slot) = match pool.next() {
            Some(s) => s,
            None => break,
        };
        let slot = slot.clone();

        let sub = path
            .strip_prefix("/v1/")
            .or_else(|| path.strip_prefix("/v1"))
            .unwrap_or(&path)
            .trim_start_matches('/');
        let upstream = match &query {
            Some(q) => format!("{}/{}?{}", slot.base_url, sub, q),
            None => format!("{}/{}", slot.base_url, sub),
        };

        let mut headers = base_headers.clone();
        if !slot.api_key.is_empty() {
            match slot.auth_style.as_str() {
                "x-api-key" => match HeaderValue::from_str(&slot.api_key) {
                    Ok(v) => { headers.insert("x-api-key", v); }
                    Err(e) => {
                        warn!("x '{}' invalid API key: {} -- skipping", slot.name, e);
                        pool.mark_failed(slot_idx, 0);
                        last_err = Some(format!("invalid API key header for '{}'", slot.name));
                        continue;
                    }
                },
                _ => match HeaderValue::from_str(&format!("Bearer {}", slot.api_key)) {
                    Ok(v) => { headers.insert("authorization", v); }
                    Err(e) => {
                        warn!("x '{}' invalid API key: {} -- skipping", slot.name, e);
                        pool.mark_failed(slot_idx, 0);
                        last_err = Some(format!("invalid API key header for '{}'", slot.name));
                        continue;
                    }
                },
            }
        }

        let body = if let Some(ref parsed) = parsed_json {
            let mut v = parsed.clone();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("model".to_string(), Value::String(slot.model_id.clone()));
            }
            let b = serde_json::to_vec(&v).unwrap_or_else(|_| raw_body.to_vec());
            info!(
                "-> body preview: {}",
                String::from_utf8_lossy(&b[..b.len().min(300)])
            );
            b
        } else if !raw_body.is_empty() {
            warn!("-> body is not JSON (len={}), passing through unchanged", raw_body.len());
            raw_body.to_vec()
        } else {
            Vec::new()
        };

        headers.insert(
            "content-length",
            HeaderValue::from_str(&body.len().to_string()).unwrap(),
        );

        info!(
            "-> [attempt {}/{}] [{}] {} via '{}' (timeout={}s, {}/{} slots available)",
            attempt + 1, n,
            slot.provider, upstream, slot.name,
            slot.timeout_secs,
            pool.available_count(), n,
        );

        // ── Per-slot timeout ──────────────────────────────────────────────
        // Build a one-shot client with this slot's timeout so fast providers
        // (Cerebras, Gemini) fail-fast and we move to the next slot quickly,
        // rather than blocking the agent's router for 120s.
        let per_slot_client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(slot.timeout_secs))
            .pool_max_idle_per_host(10)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                warn!("x '{}' failed to build client: {}", slot.name, e);
                last_err = Some(e.to_string());
                continue;
            }
        };

        let res = per_slot_client
            .request(method.clone(), &upstream)
            .headers(headers)
            .body(body)
            .send()
            .await;

        match res {
            Err(e) => {
                if e.is_timeout() {
                    warn!("x '{}' timed out after {}s -- cooling slot", slot.name, slot.timeout_secs);
                    pool.mark_timeout(slot_idx);
                    last_err = Some(format!("'{}' timed out after {}s", slot.name, slot.timeout_secs));
                } else {
                    warn!("x '{}' network error: {} -- cooling slot", slot.name, e);
                    pool.mark_failed(slot_idx, 0);
                    last_err = Some(e.to_string());
                }
                continue;
            }
            Ok(upstream_res) => {
                let status = upstream_res.status();

                if !status.is_success() {
                    let err_body = upstream_res.text().await.unwrap_or_default();
                    warn!(
                        "x '{}' returned {} -- cooling slot. Body: {}",
                        slot.name, status, &err_body
                    );

                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("error.log")
                    {
                        use std::io::Write;
                        let _ = writeln!(
                            f,
                            "[{}] {} - {}: {}",
                            chrono::Utc::now().to_rfc3339(),
                            slot.name,
                            status,
                            err_body
                        );
                    }

                    pool.mark_failed(slot_idx, status.as_u16());
                    last_err = Some(format!("[{}] {}: {}", slot.name, status, &err_body));
                    continue;
                }

                // ── Success — stream the response back ────────────────────
                let res_hdrs = upstream_res.headers().clone();
                let stream = upstream_res
                    .bytes_stream()
                    .map(|c| c.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
                let mut resp = Response::new(Body::from_stream(stream));
                *resp.status_mut() = status;
                for (k, v) in &res_hdrs {
                    if k != "transfer-encoding" && k != "connection" {
                        resp.headers_mut().insert(k, v.clone());
                    }
                }
                if let Ok(hv) = HeaderValue::from_str(&slot.name) {
                    resp.headers_mut().insert("x-axon-proxy-slot", hv);
                }
                if let Ok(hv) = HeaderValue::from_str(&pool.available_count().to_string()) {
                    resp.headers_mut().insert("x-axon-slots-available", hv);
                }
                info!("ok '{}' succeeded ({})", slot.name, status);
                return Ok(resp);
            }
        }
    }

    Err(ApiErr(
        StatusCode::BAD_GATEWAY,
        format!(
            "All {} slot(s) exhausted. Last error: {}",
            n,
            last_err.unwrap_or_default()
        ),
    ))
}
