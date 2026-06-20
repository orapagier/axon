use crate::tools::schema::{ToolDefinition, ToolSource};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResult {
    pub success: bool,
    pub content: serde_json::Value,
}

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};
use tokio_stream::StreamExt;

pub struct McpClient {
    server_name: String,
    api_key: Option<String>,
    http: reqwest::Client,
    tools_cache: RwLock<Vec<ToolDefinition>>,

    // Standard MCP SSE fields
    post_url: RwLock<String>,
    pending: Arc<RwLock<HashMap<i64, oneshot::Sender<serde_json::Value>>>>,
    next_id: Arc<AtomicI64>,
}

impl McpClient {
    fn rpc_timeout(method: &str) -> std::time::Duration {
        let default_secs = if method == "tools/call" { 15 * 60 } else { 60 };
        let env_key = if method == "tools/call" {
            "AXON_MCP_TOOLS_CALL_TIMEOUT_SECS"
        } else {
            "AXON_MCP_RPC_TIMEOUT_SECS"
        };
        let secs = std::env::var(env_key)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default_secs);
        std::time::Duration::from_secs(secs)
    }

    pub async fn new(name: &str, url: &str, api_key: Option<String>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();

        let mut req_builder = http.get(url).header("Accept", "text/event-stream");
        if let Some(key) = &api_key {
            req_builder = req_builder.header("Authorization", format!("Bearer {}", key));
        }

        let mut es = reqwest_eventsource::EventSource::new(req_builder).unwrap();
        let mut post_url = format!("{}/mcp", url.trim_end_matches('/')); // fallback legacy URL

        // Wait for the endpoint event or fallback quickly
        let mut sse_found = false;
        if let Ok(Some(event)) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match es.next().await {
                    Some(Ok(reqwest_eventsource::Event::Open)) => continue,
                    Some(Ok(reqwest_eventsource::Event::Message(msg))) => {
                        if msg.event == "endpoint" {
                            break Some(msg);
                        }
                    }
                    _ => break None,
                }
            }
        })
        .await
        {
            let endpoint: String = if event.data.starts_with('"') {
                serde_json::from_str(&event.data).unwrap_or(event.data.clone())
            } else {
                event.data.clone()
            };

            // Resolve the absolute URL
            if endpoint.starts_with("http") {
                post_url = endpoint;
            } else {
                let parsed = reqwest::Url::parse(url)?;
                if endpoint.starts_with('/') {
                    post_url = format!(
                        "{}://{}{}",
                        parsed.scheme(),
                        parsed.host_str().unwrap_or(""),
                        endpoint
                    );
                } else {
                    let base = parsed.path().trim_end_matches('/');
                    post_url = format!(
                        "{}://{}{}/{}",
                        parsed.scheme(),
                        parsed.host_str().unwrap_or(""),
                        base,
                        endpoint
                    );
                }
            }
            sse_found = true;
        }

        let pending: Arc<RwLock<HashMap<i64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let pending_clone = pending.clone();

        if sse_found {
            // Spawn task to handle incoming SSE `message` events
            tokio::spawn(async move {
                use tokio_stream::StreamExt;
                while let Some(event) = es.next().await {
                    if let Ok(reqwest_eventsource::Event::Message(msg)) = event {
                        if msg.event == "message" {
                            if let Ok(rpc) = serde_json::from_str::<serde_json::Value>(&msg.data) {
                                if let Some(id) = rpc.get("id").and_then(|i| i.as_i64()) {
                                    if let Some(sender) = pending_clone.write().await.remove(&id) {
                                        let _ = sender.send(rpc);
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }

        Ok(McpClient {
            server_name: name.to_string(),
            api_key,
            http,
            tools_cache: RwLock::new(vec![]),
            post_url: RwLock::new(post_url),
            pending,
            next_id: Arc::new(AtomicI64::new(1)),
        })
    }

    async fn send_rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let rpc_timeout = Self::rpc_timeout(method);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let post_url = self.post_url.read().await.clone();

        let (tx, rx) = oneshot::channel();
        self.pending.write().await.insert(id, tx);

        let mut req = self
            .http
            .post(&post_url)
            .timeout(rpc_timeout)
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params
            }));

        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req.send().await.context(format!("MCP {}", method))?;
        let status = resp.status();

        // If it's 202 accepted, it's async SSE, we must wait for the SSE stream response
        if status == reqwest::StatusCode::ACCEPTED {
            match tokio::time::timeout(rpc_timeout, rx).await {
                Ok(Ok(body)) => {
                    if let Some(err) = body.get("error") {
                        anyhow::bail!("MCP error: {}", err);
                    }
                    Ok(body.get("result").cloned().unwrap_or(serde_json::json!({})))
                }
                _ => {
                    self.pending.write().await.remove(&id);
                    anyhow::bail!(
                        "MCP {} timed out waiting for SSE response after {}s",
                        method,
                        rpc_timeout.as_secs()
                    )
                }
            }
        } else if status.is_success() {
            // It's a synchronous response (legacy Axon format)
            self.pending.write().await.remove(&id);
            let body: serde_json::Value = resp.json().await.context(format!(
                "Failed to parse JSON response for MCP method {}",
                method
            ))?;
            if let Some(err) = body.get("error") {
                anyhow::bail!("MCP error: {}", err);
            }
            Ok(body.get("result").cloned().unwrap_or(serde_json::json!({})))
        } else {
            self.pending.write().await.remove(&id);
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("MCP returned {}: {}", status, body);
        }
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<ToolDefinition>> {
        let result = self.send_rpc("tools/list", serde_json::json!({})).await?;

        #[derive(Deserialize)]
        struct L {
            tools: Vec<T>,
        }
        #[derive(Deserialize)]
        struct T {
            name: String,
            description: Option<String>,
            #[serde(rename = "inputSchema")]
            input_schema: Option<serde_json::Value>,
        }

        let body: L = serde_json::from_value(result).context("parse MCP tools")?;
        let tools: Vec<ToolDefinition> = body
            .tools
            .into_iter()
            .map(|t| {
                let (params, required) = t
                    .input_schema
                    .as_ref()
                    .map(|s| {
                        let p = s
                            .get("properties")
                            .cloned()
                            .unwrap_or(serde_json::json!({}));
                        let r: Vec<String> = s
                            .get("required")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default();
                        (p, r)
                    })
                    .unwrap_or((serde_json::json!({}), vec![]));
                ToolDefinition {
                    name: t.name.clone(),
                    is_mutating: crate::tools::schema::derive_is_mutating(&t.name),
                    description: t.description.unwrap_or_else(|| "MCP tool".into()),
                    parameters: params,
                    required,
                    source: ToolSource::Mcp {
                        server_name: self.server_name.clone(),
                        tool_name: t.name,
                    },
                    enabled: true,
                }
            })
            .collect();
        *self.tools_cache.write().await = tools.clone();
        tracing::info!("MCP '{}': {} tools", self.server_name, tools.len());
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let result = self
            .send_rpc(
                "tools/call",
                serde_json::json!({
                    "name": name,
                    "arguments": args
                }),
            )
            .await?;
        Ok(normalize_mcp_output(result))
    }

    pub async fn cached_tools(&self) -> Vec<ToolDefinition> {
        self.tools_cache.read().await.clone()
    }
}

pub fn normalize_mcp_output(raw: serde_json::Value) -> serde_json::Value {
    if raw
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let msg = raw
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|i| i.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("MCP error");
        return serde_json::json!({"error":true,"message":msg});
    }
    let Some(arr) = raw.get("content").and_then(|c| c.as_array()) else {
        return serde_json::json!({"output":raw.to_string()});
    };
    let mut parts: Vec<serde_json::Value> = vec![];
    for item in arr {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                let text = item.get("text").and_then(|t| t.as_str()).unwrap_or("");
                parts.push(serde_json::from_str(text).unwrap_or(serde_json::json!({"text":text})));
            }
            Some("image") => parts.push(serde_json::json!({
                "type":"image",
                "mime_type": item.get("mimeType").and_then(|m| m.as_str()).unwrap_or("image/png"),
                "data_base64": item.get("data").and_then(|d| d.as_str()).unwrap_or(""),
            })),
            _ => parts.push(item.clone()),
        }
    }
    if parts.len() == 1 {
        parts.remove(0)
    } else {
        serde_json::json!(parts)
    }
}

/// Convert an MCP tool (as JSON, either from an SSE `tools/list` reply or from
/// serializing an in-process `rmcp::model::Tool`) into the agent's
/// `ToolDefinition`. Shared by the SSE client and the in-process provider so
/// both produce identical tool definitions.
pub fn tool_def_from_json(v: &serde_json::Value, server_name: &str) -> Option<ToolDefinition> {
    let name = v.get("name").and_then(|n| n.as_str())?.to_string();
    let description = v
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());
    let schema = v.get("inputSchema").or_else(|| v.get("input_schema"));
    let (params, required) = schema
        .map(|s| {
            let p = s
                .get("properties")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            let r: Vec<String> = s
                .get("required")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            (p, r)
        })
        .unwrap_or((serde_json::json!({}), vec![]));
    Some(ToolDefinition {
        name: name.clone(),
        is_mutating: crate::tools::schema::derive_is_mutating(&name),
        description: description.unwrap_or_else(|| "MCP tool".into()),
        parameters: params,
        required,
        source: ToolSource::Mcp {
            server_name: server_name.to_string(),
            tool_name: name,
        },
        enabled: true,
    })
}

/// A connected MCP backend: either a remote server over SSE, or the built-in
/// integration services running in-process (no separate process).
pub enum McpBackend {
    Sse(McpClient),
    InProcess(crate::mcp::inprocess::InProcessMcp),
}

impl McpBackend {
    async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        match self {
            McpBackend::Sse(c) => c.call_tool(name, args).await,
            McpBackend::InProcess(p) => p.call_tool(name, args).await,
        }
    }

    async fn cached_tools(&self) -> Vec<ToolDefinition> {
        match self {
            McpBackend::Sse(c) => c.cached_tools().await,
            McpBackend::InProcess(p) => p.cached_tools(),
        }
    }
}

pub struct McpManager {
    clients: tokio::sync::RwLock<HashMap<String, McpBackend>>,
}

impl McpManager {
    pub fn new() -> Self {
        McpManager {
            clients: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    pub async fn connect(
        &self,
        name: &str,
        url: &str,
        api_key: Option<String>,
    ) -> anyhow::Result<Vec<ToolDefinition>> {
        let client = McpClient::new(name, url, api_key).await?;
        let tools = client.list_tools().await?;
        self.clients
            .write()
            .await
            .insert(name.to_string(), McpBackend::Sse(client));
        Ok(tools)
    }

    /// Register the built-in integration services as an in-process backend.
    /// Tools are sourced as `Mcp { server_name: name }` so the rest of the
    /// agent (registry, OAuth handlers, workflows) dispatches to them as plain
    /// async function calls — no separate process and no SSE hop.
    pub async fn connect_inprocess(&self, name: &str) -> anyhow::Result<Vec<ToolDefinition>> {
        let provider = crate::mcp::inprocess::InProcessMcp::new(name).await?;
        let tools = provider.cached_tools();
        self.clients
            .write()
            .await
            .insert(name.to_string(), McpBackend::InProcess(provider));
        Ok(tools)
    }

    /// The shared `axon_core::AppState` of the in-process backend, if present.
    /// Used by the agent's media route to resolve temporary media files.
    pub async fn inprocess_state(&self) -> Option<std::sync::Arc<axon_core::AppState>> {
        let clients = self.clients.read().await;
        for backend in clients.values() {
            if let McpBackend::InProcess(p) = backend {
                return Some(p.mcp_state());
            }
        }
        None
    }

    pub async fn disconnect(&self, name: &str) {
        self.clients.write().await.remove(name);
    }

    pub async fn call(
        &self,
        server: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let clients = self.clients.read().await;
        let client = clients
            .get(server)
            .with_context(|| format!("MCP '{}' not connected", server))?;
        client.call_tool(tool, args).await
    }

    // FIX: sequential .await — no block_on deadlock
    pub async fn all_tools(&self) -> Vec<ToolDefinition> {
        let clients = self.clients.read().await;
        let mut all = vec![];
        for c in clients.values() {
            all.extend(c.cached_tools().await);
        }
        all
    }

    pub async fn server_names(&self) -> Vec<String> {
        self.clients.read().await.keys().cloned().collect()
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}
