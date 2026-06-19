use anyhow::Result;
use async_trait::async_trait;
use axon_business::BusinessService;
use axon_core::AppState;
use axon_crm::CrmService;
use axon_facebook::FacebookService;
use axon_google::GoogleService;
use axon_instagram::InstagramService;
use axon_microsoft::MicrosoftService;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use rmcp::{
    model::{
        CallToolRequestParam, CallToolResult, ErrorCode, ErrorData, Implementation,
        ListToolsResult, PaginatedRequestParam, ProtocolVersion, ServerCapabilities, ServerInfo,
        ServerResult,
    },
    service::RequestContext,
    Error as McpError, RoleServer, ServerHandler,
};
use serde::Deserialize;
use std::future::Future;
use std::sync::Arc;
use tokio::io::{stdin, stdout};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

// ── MCP Server ────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AxonServer {
    state: Arc<AppState>,
    google: Arc<GoogleService>,
    microsoft: Arc<MicrosoftService>,
    facebook: Arc<FacebookService>,
    instagram: Arc<InstagramService>,
    business: Arc<BusinessService>,
    crm: Arc<CrmService>,
    sse_sessions: Arc<
        tokio::sync::RwLock<
            std::collections::HashMap<String, mpsc::Sender<Result<SseEvent, axum::Error>>>,
        >,
    >,
}

impl AxonServer {
    async fn new(state: Arc<AppState>) -> Result<Self> {
        Ok(Self {
            state: state.clone(),
            google: Arc::new(GoogleService::new(state.clone())),
            microsoft: Arc::new(MicrosoftService::new(state.clone())),
            facebook: Arc::new(FacebookService::new(state.clone())),
            instagram: Arc::new(InstagramService::new(state.clone())),
            business: Arc::new(BusinessService::new(state.clone())),
            crm: Arc::new(CrmService::new(state.clone()).await?),
            sse_sessions: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        })
    }

    async fn do_list_tools(&self) -> ListToolsResult {
        let mut tools = Vec::new();
        tools.extend(GoogleService::tool_list());
        tools.extend(MicrosoftService::tool_list());
        tools.extend(FacebookService::tool_list());
        tools.extend(InstagramService::tool_list());
        tools.extend(BusinessService::tool_list());
        tools.extend(CrmService::tool_list());
        ListToolsResult {
            tools,
            next_cursor: None,
        }
    }

    async fn do_call_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<CallToolResult, anyhow::Error> {
        if name.starts_with("google_")
            || name.starts_with("gmail_")
            || name.starts_with("gcal_")
            || name.starts_with("gdrive_")
            || name.starts_with("gdocs_")
            || name.starts_with("gsheets_")
            || name.starts_with("gcon_")
            || name.starts_with("gmeet_")
            || name.starts_with("gtasks_")
            || name.starts_with("gslides_")
            || name.starts_with("gforms_")
            || name.starts_with("gchat_")
        {
            self.google.call(name, args).await
        } else if name.starts_with("microsoft_")
            || name.starts_with("outlook_")
            || name.starts_with("mscal_")
            || name.starts_with("onedrive_")
            || name.starts_with("teams_")
            || name.starts_with("mscontacts_")
        {
            self.microsoft.call(name, args).await
        } else if name.starts_with("facebook_") || name.starts_with("fb_") {
            self.facebook.call(name, args).await
        } else if name.starts_with("instagram_") || name.starts_with("ig_") {
            self.instagram.call(name, args).await
        } else if name.starts_with("crm_") {
            self.crm.call(name, args).await
        } else {
            self.business.call(name, args).await
        }
    }
}

#[async_trait]
impl ServerHandler for AxonServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            server_info: Implementation {
                name: "axon-mcp".into(),
                version: "2.0.0".into(),
            },
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }

    fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async move { Ok(self.do_list_tools().await) }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let name = &request.name;
            let args = request.arguments.unwrap_or_default();
            self.do_call_tool(name, args)
                .await
                .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))
        }
    }
}

// ── HTTP Server Handlers ──────────────────────────────────────────────────────

async fn root_handler() -> impl IntoResponse {
    Html(
        r#"
        <!DOCTYPE html>
        <html>
        <head>
            <title>Axon MCP Status</title>
            <style>
                body { font-family: sans-serif; display: flex; align-items: center; justify-content: center; height: 100vh; margin: 0; background: #f9fafb; font-size: 1.1rem; }
                .card { background: white; padding: 3rem; border-radius: 1.5rem; box-shadow: 0 10px 15px -3px rgb(0 0 0 / 0.1); text-align: center; max-width: 600px; }
                h1 { color: #111827; margin-bottom: 0.5rem; }
                p { color: #4b5563; }
                .status { display: inline-block; padding: 0.25rem 1rem; background: #dcfce7; color: #166534; border-radius: 9999px; font-weight: bold; margin-bottom: 2rem; }
                .endpoint { background: #f3f4f6; padding: 1rem; border-radius: 0.5rem; text-align: left; margin-bottom: 1rem; border: 1px solid #e5e7eb; }
                code { color: #8b5cf6; font-weight: bold; }
                .hint { font-size: 0.9rem; color: #9ca3af; margin-top: 2rem; }
            </style>
        </head>
        <body>
            <div class="card">
                <h1>Axon MCP Server</h1>
                <div class="status">● Running</div>
                <p>Use the following URL in your Axon Agent:</p>
                <div class="endpoint">
                    <code>http://127.0.0.1:8080</code>
                </div>
                <p style="margin-top: 2rem; font-weight: bold;">Endpoints:</p>
                <div style="text-align: left;">
                    <div class="endpoint"><b>GET</b> <code>/sse</code> — MCP Standard SSE Connection</div>
                    <div class="endpoint"><b>POST</b> <code>/message?session_id=...</code> — MCP Standard Event Endpoint</div>
                    <div class="endpoint"><b>POST</b> <code>/mcp</code> — Legacy Stateless MCP Endpoint</div>
                    <div class="endpoint"><b>GET</b> <code>/auth/:service/callback</code> — OAuth Callbacks</div>
                </div>
                <p class="hint">Tokens stored in <code>~/.local/share/axon-mcp/tokens.json</code></p>
            </div>
        </body>
        </html>
    "#,
    )
}

async fn mcp_get_handler() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Html(error_page(
            "The /mcp endpoint requires a POST request with JSON-RPC 2.0 payload.",
        )),
    )
}

// ── Standard MCP SSE Transport ───────────────────────────────────────────────

async fn sse_handler(
    State(server): State<AxonServer>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, axum::Error>>> {
    let session_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = mpsc::channel(100);

    let post_url = format!("/message?session_id={}", session_id);
    let endpoint_event = SseEvent::default().event("endpoint").data(post_url.clone());

    let _ = tx.send(Ok(endpoint_event)).await;
    server.sse_sessions.write().await.insert(session_id, tx);

    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::new())
}

#[derive(Deserialize)]
struct MessageQuery {
    session_id: String,
}

async fn message_post_handler(
    State(server): State<AxonServer>,
    Query(query): Query<MessageQuery>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let rpc: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let Some(tx) = server
        .sse_sessions
        .read()
        .await
        .get(&query.session_id)
        .cloned()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let method = rpc
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = rpc.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let params = rpc.get("params").cloned();

    // Spawn processing in background to immediately return 202 Accepted
    tokio::spawn(async move {
        let result = match method.as_str() {
            "tools/list" => Ok(ServerResult::ListToolsResult(server.do_list_tools().await)),
            "tools/call" => {
                let args = params
                    .clone()
                    .and_then(|p| p.get("arguments").cloned())
                    .and_then(|a| a.as_object().cloned())
                    .unwrap_or_default();
                let name = params
                    .and_then(|p| p.get("name").cloned())
                    .and_then(|n| n.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();

                if name.is_empty() {
                    Err(ErrorData::new(
                        ErrorCode::INVALID_PARAMS,
                        "Missing tool name",
                        None,
                    ))
                } else {
                    server
                        .do_call_tool(&name, args)
                        .await
                        .map(ServerResult::CallToolResult)
                        .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))
                }
            }
            _ => Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("Method not found: {}", method),
                None,
            )),
        };

        let response_payload = match result {
            Ok(res) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": res }),
            Err(e) => {
                serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code.0, "message": e.message, "data": e.data } })
            }
        };

        let event = SseEvent::default()
            .event("message")
            .data(serde_json::to_string(&response_payload).unwrap());

        // Since SSE protocol expects valid json in message event, the spec usually requires `event("message")`
        // with the whole json-rpc response as data.
        let _ = tx.send(Ok(event)).await;
    });

    StatusCode::ACCEPTED.into_response()
}

// ── Legacy Stateless MCP Endpoint ─────────────────────────────────────────────

async fn mcp_handler(
    State(server): State<AxonServer>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    info!("Incoming MCP request: POST /mcp");
    info!("Headers: {:?}", headers);

    let rpc: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse MCP request body as JSON: {}", e);
            let raw_body = String::from_utf8_lossy(&body);
            warn!("Raw body received: {}", raw_body);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    }
                })),
            )
                .into_response();
        }
    };

    let method = rpc.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = rpc.get("id").cloned().unwrap_or(serde_json::Value::Null);

    info!("MCP Method Invoked: {}", method);

    let result: Result<ServerResult, ErrorData> = match method {
        "tools/list" => Ok(ServerResult::ListToolsResult(server.do_list_tools().await)),
        "tools/call" => {
            let args = rpc
                .get("params")
                .and_then(|p| p.get("arguments"))
                .and_then(|a| a.as_object())
                .cloned()
                .unwrap_or_default();
            let name = rpc
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");

            if name.is_empty() {
                Err(ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    "Missing tool name",
                    None,
                ))
            } else {
                server
                    .do_call_tool(name, args)
                    .await
                    .map(ServerResult::CallToolResult)
                    .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))
            }
        }
        _ => {
            warn!("Method not found: {}", method);
            Err(ErrorData::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("Method not found: {}", method),
                None,
            ))
        }
    };

    match result {
        Ok(res) => Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": res
        }))
        .into_response(),
        Err(e) => Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": e.code.0,
                "message": e.message,
                "data": e.data
            }
        }))
        .into_response(),
    }
}

#[derive(Deserialize)]
struct OAuthQuery {
    code: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn oauth_callback_handler(
    Path(service): Path<String>,
    Query(query): Query<OAuthQuery>,
    State(server): State<AxonServer>,
) -> impl IntoResponse {
    let code = query.code;
    let error = query.error_description.or(query.error);

    match (code, error) {
        (Some(code), _) => {
            let result = match service.as_str() {
                "google" => {
                    server.google.call("google_exchange_code", map_of("code", &code))
                        .await
                }
                "microsoft" => {
                    server.microsoft.call("microsoft_exchange_code", map_of("code", &code))
                        .await
                }
                "facebook" | "instagram" => {
                    server
                        .facebook
                        .call(
                            "facebook_exchange_code",
                            map_of_two("code", &code, "service", &service),
                        )
                        .await
                }
                _ => Err(anyhow::anyhow!("unknown service: {}", service)),
            };

            match result {
                Ok(_) => (StatusCode::OK, Html(success_page(&service))),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html(error_page(&e.to_string())),
                ),
            }
        }
        (None, Some(err)) => (StatusCode::BAD_REQUEST, Html(error_page(&err))),
        _ => (
            StatusCode::BAD_REQUEST,
            Html(error_page("Missing code parameter. Did you navigate here manually? This URL is for OAuth callbacks only.")),
        ),
    }
}

fn parse_byte_range(range_header: &str, total_len: usize) -> Option<(usize, usize)> {
    if total_len == 0 {
        return None;
    }

    let raw = range_header.trim();
    let value = raw.strip_prefix("bytes=")?.split(',').next()?.trim();

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
        let start = total_len - len;
        let end = total_len.saturating_sub(1);
        return Some((start, end));
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

async fn serve_local_media_token(
    token: &str,
    server: &AxonServer,
    range_header: Option<&str>,
    head_only: bool,
) -> axum::response::Response {
    let Some(file) = server.state.resolve_temp_media_file(&token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let total_len = match tokio::fs::metadata(&file.path).await {
        Ok(meta) => meta.len() as usize,
        Err(e) => {
            warn!(
                "Failed to stat local media file {}: {}",
                file.path.display(),
                e
            );
            return if e.kind() == std::io::ErrorKind::NotFound {
                StatusCode::NOT_FOUND.into_response()
            } else {
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            };
        }
    };

    let bytes = match tokio::fs::read(&file.path).await {
        Ok(data) => data,
        Err(e) => {
            warn!(
                "Failed to read local media file {}: {}",
                file.path.display(),
                e
            );
            return if e.kind() == std::io::ErrorKind::NotFound {
                StatusCode::NOT_FOUND.into_response()
            } else {
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            };
        }
    };

    let mut headers = HeaderMap::new();
    if let Some(content_type) = file.content_type {
        if let Ok(value) = content_type.parse() {
            headers.insert(axum::http::header::CONTENT_TYPE, value);
        }
    }
    if let Ok(value) = "public, max-age=7200".parse() {
        headers.insert(axum::http::header::CACHE_CONTROL, value);
    }
    if let Ok(value) = "bytes".parse() {
        headers.insert(header::ACCEPT_RANGES, value);
    }

    let range = range_header.and_then(|v| parse_byte_range(v, total_len));
    let (status, body_len, content_range, body_bytes): (
        StatusCode,
        usize,
        Option<String>,
        Vec<u8>,
    ) = if let Some((start, end)) = range {
        (
            StatusCode::PARTIAL_CONTENT,
            end - start + 1,
            Some(format!("bytes {}-{}/{}", start, end, total_len)),
            bytes[start..=end].to_vec(),
        )
    } else {
        (StatusCode::OK, bytes.len(), None, bytes)
    };

    if let Ok(value) = body_len.to_string().parse() {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    if let Some(cr) = content_range {
        if let Ok(value) = cr.parse() {
            headers.insert(header::CONTENT_RANGE, value);
        }
    }

    if head_only {
        return (status, headers, axum::body::Body::empty()).into_response();
    }

    (status, headers, body_bytes).into_response()
}

async fn local_media_handler(
    Path(token): Path<String>,
    headers: HeaderMap,
    State(server): State<AxonServer>,
) -> impl IntoResponse {
    let range = headers.get(header::RANGE).and_then(|h| h.to_str().ok());
    serve_local_media_token(&token, &server, range, false).await
}

async fn local_media_named_handler(
    Path((token, _name)): Path<(String, String)>,
    headers: HeaderMap,
    State(server): State<AxonServer>,
) -> impl IntoResponse {
    let range = headers.get(header::RANGE).and_then(|h| h.to_str().ok());
    serve_local_media_token(&token, &server, range, false).await
}

async fn local_media_head_handler(
    Path(token): Path<String>,
    headers: HeaderMap,
    State(server): State<AxonServer>,
) -> impl IntoResponse {
    let range = headers.get(header::RANGE).and_then(|h| h.to_str().ok());
    serve_local_media_token(&token, &server, range, true).await
}

async fn local_media_named_head_handler(
    Path((token, _name)): Path<(String, String)>,
    headers: HeaderMap,
    State(server): State<AxonServer>,
) -> impl IntoResponse {
    let range = headers.get(header::RANGE).and_then(|h| h.to_str().ok());
    serve_local_media_token(&token, &server, range, true).await
}

async fn fallback_handler(method: Method, path: axum::extract::OriginalUri) -> impl IntoResponse {
    warn!("404 Not Found: {} {:?}", method, path);
    (
        StatusCode::NOT_FOUND,
        Html(error_page(&format!(
            "Endpoint <code>{} {:?}</code> not found. Check your URL.",
            method, path
        ))),
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn map_of(key: &str, value: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert(key.to_owned(), serde_json::Value::String(value.to_owned()));
    m
}

fn map_of_two(
    k1: &str,
    v1: &str,
    k2: &str,
    v2: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert(k1.to_owned(), serde_json::Value::String(v1.to_owned()));
    m.insert(k2.to_owned(), serde_json::Value::String(v2.to_owned()));
    m
}

fn success_page(service: &str) -> String {
    let label = match service {
        "google" => "Google Workspace",
        "microsoft" => "Microsoft 365",
        "facebook" => "Facebook Page & Instagram",
        "instagram" => "Instagram",
        _ => service,
    };
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>axon-mcp</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#f0fdf4}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#16a34a;margin:0 0 12px}}p{{color:#6b7280;margin:0;line-height:1.5}}</style>
<script>setTimeout(() => {{ window.close(); }}, 3000);</script></head>
<body><div class="card"><h1>✅ {label} Authenticated</h1>
<p>Tokens saved successfully. This tab will close automatically in 3 seconds...</p></div></body></html>"#
    )
}

fn error_page(msg: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>axon-mcp error</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#fef2f2}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#dc2626;margin:0 0 12px}}p{{color:#6b7280;margin:0;word-break:break-word;line-height:1.5}}
code{{background:#f3f4f6;padding:0.2rem 0.4rem;border-radius:0.3rem;font-family:monospace;color:#111827}}</style></head>
<body><div class="card"><h1>❌ Error</h1><p>{msg}</p></div></body></html>"#
    )
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("axon_mcp=info".parse()?)
                .add_directive("rmcp=warn".parse()?),
        )
        .init();

    dotenvy::dotenv().ok();
    let cb_host = std::env::var("AXON_CALLBACK_HOST")
        .or_else(|_| std::env::var("AXON_PUBLIC_BASE_URL"))
        .or_else(|_| std::env::var("instagram.public_base_url"))
        .unwrap_or_else(|_| "http://localhost:8080".to_string());
    info!("axon-mcp v2.0.0 starting. Callback Host: {}", cb_host);

    let state = Arc::new(AppState::new().await?);
    let server = AxonServer::new(state.clone()).await?;

    // Build Router
    let app = Router::new()
        .route("/", get(root_handler))
        .route("/sse", get(sse_handler))
        .route("/message", post(message_post_handler))
        .route("/mcp", post(mcp_handler).get(mcp_get_handler))
        .route("/auth/:service/callback", get(oauth_callback_handler))
        .route(
            "/media/local/:token",
            get(local_media_handler).head(local_media_head_handler),
        )
        .route(
            "/media/local/:token/:name",
            get(local_media_named_handler).head(local_media_named_head_handler),
        )
        .fallback(fallback_handler)
        .layer(CorsLayer::permissive())
        .with_state(server.clone());

    let bind_addr = std::env::var("AXON_BIND_ADDR")
        .or_else(|_| std::env::var("instagram.bind_addr"))
        .unwrap_or_else(|_| format!("127.0.0.1:{}", axon_core::oauth::CALLBACK_PORT));
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("Running HTTP server on http://{}", bind_addr);

    // Also support stdio in a background task
    let server_stdio = server.clone();
    tokio::spawn(async move {
        info!("Starting MCP stdio transport...");
        match rmcp::service::serve_server(server_stdio, (stdin(), stdout())).await {
            Ok(service) => {
                if let Err(e) = service.waiting().await {
                    warn!("MCP stdio transport closed: {}", e);
                }
            }
            Err(e) => warn!("Failed to start MCP stdio transport: {}", e),
        }
    });

    info!("Axon MCP Server is ready and listening.");
    axum::serve(listener, app).await?;

    Ok(())
}
