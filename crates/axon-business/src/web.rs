use anyhow::Result;
use axon_core::AppState;
use reqwest::{
    header::{HeaderName, HeaderValue},
    Method,
};
use serde_json::{Map, Value};
use std::sync::Arc;

pub async fn request(
    state: &Arc<AppState>,
    url: &str,
    method: &str,
    headers: Option<&Map<String, Value>>,
    query: Option<&Map<String, Value>>,
    body: Option<&Value>,
) -> Result<Value> {
    let method = match method.to_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "DELETE" => Method::DELETE,
        "PATCH" => Method::PATCH,
        "HEAD" => Method::HEAD,
        _ => Method::GET,
    };

    let mut req = state.client.request(method, url);

    // Add query params
    if let Some(params) = query {
        req = req.query(params);
    }

    // Add headers
    if let Some(h_map) = headers {
        for (k, v) in h_map {
            if let Some(s) = v.as_str() {
                if let (Ok(name), Ok(val)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(s),
                ) {
                    req = req.header(name, val);
                }
            }
        }
    }

    // Add body
    if let Some(b) = body {
        req = req.json(b);
    }

    let resp = req.send().await?;
    let status = resp.status();

    // Check if it's JSON
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type.contains("application/json") {
        let json: Value = resp.json().await?;
        Ok(serde_json::json!({
            "status": status.as_u16(),
            "data": json
        }))
    } else {
        let text = resp.text().await?;
        Ok(serde_json::json!({
            "status": status.as_u16(),
            "data": text
        }))
    }
}
