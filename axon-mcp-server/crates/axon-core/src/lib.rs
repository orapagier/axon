pub mod oauth;
pub mod storage;

pub use oauth::*;
pub use storage::*;

use anyhow::Result;
use chrono::Utc;
use reqwest::{header, Client};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::RwLock;

// ── Shared application state ──────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub client: Client,
    pub storage: Arc<RwLock<Storage>>,
    pub temp_media_files: Arc<RwLock<HashMap<String, TempMediaFile>>>,
    pub temp_media_counter: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
pub struct TempMediaFile {
    pub path: PathBuf,
    pub content_type: Option<String>,
    pub expires_at_ms: i64,
}

impl AppState {
    pub async fn new() -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::USER_AGENT, "axon-mcp/2.0".parse()?);

        let client = Client::builder()
            .default_headers(headers)
            .use_rustls_tls()
            .build()?;

        let storage = Storage::load()?;
        Ok(Self {
            client,
            storage: Arc::new(RwLock::new(storage)),
            temp_media_files: Arc::new(RwLock::new(HashMap::new())),
            temp_media_counter: Arc::new(AtomicU64::new(1)),
        })
    }

    pub async fn register_temp_media_file(
        &self,
        path: PathBuf,
        content_type: Option<String>,
        ttl_secs: u64,
    ) -> String {
        self.purge_expired_temp_media_files().await;

        let now_ms = Utc::now().timestamp_millis();
        let expires_at_ms = now_ms + (ttl_secs as i64 * 1000);
        let nonce = self.temp_media_counter.fetch_add(1, Ordering::Relaxed);

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        expires_at_ms.hash(&mut hasher);
        nonce.hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        let hash = hasher.finish();

        let token = format!("{hash:016x}{nonce:016x}");
        let entry = TempMediaFile {
            path,
            content_type,
            expires_at_ms,
        };

        self.temp_media_files
            .write()
            .await
            .insert(token.clone(), entry);
        token
    }

    pub async fn resolve_temp_media_file(&self, token: &str) -> Option<TempMediaFile> {
        self.purge_expired_temp_media_files().await;
        let entry = self.temp_media_files.read().await.get(token).cloned();
        match entry {
            Some(file) if file.path.is_file() => Some(file),
            Some(_) => {
                self.temp_media_files.write().await.remove(token);
                None
            }
            None => None,
        }
    }

    pub async fn purge_expired_temp_media_files(&self) {
        let now_ms = Utc::now().timestamp_millis();
        self.temp_media_files
            .write()
            .await
            .retain(|_, v| v.expires_at_ms > now_ms);
    }

    /// Convenience: GET with Bearer token.
    pub async fn get(&self, token: &str, url: &str) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Convenience: GET with Bearer token + query params.
    pub async fn get_q(
        &self,
        token: &str,
        url: &str,
        params: &[(&str, &str)],
    ) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(url)
            .bearer_auth(token)
            .query(params)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// Convenience: POST JSON with Bearer token.
    pub async fn post(
        &self,
        token: &str,
        url: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let resp = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;

        // Some endpoints return 204 No Content
        if resp.status().as_u16() == 204 {
            return Ok(serde_json::json!({ "success": true }));
        }
        Ok(resp.json().await?)
    }

    /// Convenience: PATCH JSON with Bearer token.
    pub async fn patch(
        &self,
        token: &str,
        url: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let resp = self
            .client
            .patch(url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        if resp.status().as_u16() == 204 {
            return Ok(serde_json::json!({ "success": true }));
        }
        Ok(resp.json().await?)
    }

    /// Convenience: DELETE with Bearer token.
    pub async fn delete(&self, token: &str, url: &str) -> Result<serde_json::Value> {
        self.client
            .delete(url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?;
        Ok(serde_json::json!({ "success": true }))
    }
}

// ── MCP tool helpers ──────────────────────────────────────────────────────────

/// Build a `Tool` input_schema map from a JSON literal.
#[macro_export]
macro_rules! schema {
    ($props:tt, [$($req:expr),*]) => {{
        use std::sync::Arc;
        Arc::new(
            serde_json::json!({
                "type": "object",
                "properties": $props,
                "required": [$($req),*]
            })
            .as_object()
            .unwrap()
            .clone()
        )
    }};
}

/// Construct a simple text `CallToolResult`.
pub fn ok_json(v: impl serde::Serialize) -> rmcp::model::CallToolResult {
    let text =
        serde_json::to_string_pretty(&v).unwrap_or_else(|e| format!("serialization error: {e}"));
    rmcp::model::CallToolResult {
        content: vec![rmcp::model::Content::text(text)],
        is_error: None,
    }
}

/// Construct an error `CallToolResult`.
pub fn err_json(msg: impl std::fmt::Display) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult {
        content: vec![rmcp::model::Content::text(format!("❌ Error: {msg}"))],
        is_error: Some(true),
    }
}
