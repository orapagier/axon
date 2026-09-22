//! oc-bridge — expose opencode's local agent server as an OpenAI-compatible
//! model endpoint for Axon.
//!
//! Axon's model router handles arbitrary OpenAI-compatible providers (any
//! provider name it doesn't special-case routes through `openai_compat`). This
//! binary fronts a locally-run `opencode serve` with the same translation the
//! old Node script did: `chat/completions` requests become opencode
//! session/message calls, so Axon can use opencode's free hosted models with
//! zero API keys. It is a plain compiled binary — no interpreter on the
//! runtime path.
//!
//! Endpoints (all on 127.0.0.1):
//!   GET  /v1/models                        list the free-model catalog
//!   POST /v1/chat/completions              non-streaming, or SSE when asked
//!   GET  /health                           probe for scripts/lib/oc-bridge.sh
//!
//! Env:
//!   OC_BRIDGE_PORT           bridge listen port  (default 4010)
//!   OC_SERVE_PORT            opencode serve port  (default 4123)
//!   OPENCODE_BIN             path to the opencode CLI (default "opencode" on PATH)
//!   OC_SERVE_RESTART_SECS    restart `opencode serve` every N seconds to keep
//!                            its RAM use near startup baseline (default 3600; 0 disables)

use axum::{
    Json,
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const SERVE_HOST: &str = "127.0.0.1";
const MODEL_PROVIDER: &str = "opencode";
const FREE_MODELS: [&str; 8] = [
    "big-pickle",
    "jev-1.13-free",
    "ling-3.0-flash-fin-free",
    "mimo-v2.5-free",
    "muse-spark-1.2-contributor-free",
    "muse-spark-1.3-contributor-free",
    "nemotron-3-ultra-free",
    "nemotron-3.5-lightning-free",
];

struct AppState {
    client: reqwest::Client,
    serve: Arc<Mutex<Option<Child>>>,
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn serve_url() -> String {
    let port = env_or("OC_SERVE_PORT", "4123");
    format!("http://{SERVE_HOST}:{port}")
}

async fn health_check(client: &reqwest::Client, base: &str, path: &str) -> bool {
    let url = format!("{base}{path}");
    client
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn spawn_serve() -> Result<Child, String> {
    let opencode = env_or("OPENCODE_BIN", "opencode");
    let scratch = env::temp_dir().join(format!("oc-bridge-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).map_err(|e| format!("scratch dir: {e}"))?;
    let port = env_or("OC_SERVE_PORT", "4123");
    Command::new(&opencode)
        .args(["serve", "--port", &port, "--hostname", SERVE_HOST])
        .current_dir(&scratch)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn {opencode}: {e}"))
}

async fn wait_for_health(client: &reqwest::Client, base: &str) -> bool {
    for _ in 0..60 {
        if health_check(client, base, "/api/health").await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn ensure_serve(state: &AppState) -> Result<(), String> {
    let base = serve_url();
    if health_check(&state.client, &base, "/api/health").await {
        return Ok(());
    }
    {
        let mut guard = state.serve.lock().await;
        if let Some(child) = guard.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                return Ok(()); // still starting / alive
            }
        }
        *guard = Some(spawn_serve()?);
    }
    if wait_for_health(&state.client, &base).await {
        Ok(())
    } else {
        Err("opencode serve did not come up".to_string())
    }
}

/// SIGTERM the child and wait for it to exit, escalating to SIGKILL after a
/// grace period so sqlite state gets a clean shutdown.
async fn stop_child(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    match tokio::time::timeout(Duration::from_secs(15), child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

/// Replace the current `opencode serve` child with a fresh one, keeping its
/// memory footprint near startup baseline. Run periodically (see
/// OC_SERVE_RESTART_SECS) — the serve process grows steadily over hours.
async fn restart_serve(state: &AppState) -> Result<(), String> {
    let mut guard = state.serve.lock().await;
    let alive = match guard.as_mut() {
        Some(child) => child.try_wait().ok().flatten().is_none(),
        None => false,
    };
    if !alive {
        return Ok(()); // nothing running; ensure_serve will spawn on demand
    }
    if let Some(mut child) = guard.take() {
        stop_child(&mut child).await;
    }
    // Let the old listener release the port before respawning.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    *guard = Some(spawn_serve()?);
    if wait_for_health(&state.client, &serve_url()).await {
        Ok(())
    } else {
        Err("serve restart: new process did not come up".to_string())
    }
}

fn flatten_content(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.trim().to_string();
    }
    if let Some(items) = content.as_array() {
        let mut out = Vec::new();
        for p in items {
            if let Some(s) = p.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            } else if p.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = p.get("text").and_then(Value::as_str) {
                    let t = t.trim();
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
            } else if p.get("type").and_then(Value::as_str) == Some("image_url") {
                out.push("[image]".to_string());
            }
        }
        return out.join("\n").trim().to_string();
    }
    content.as_str().unwrap_or("").trim().to_string()
}

fn flatten_system(messages: &[Value]) -> String {
    let mut parts = Vec::new();
    for m in messages {
        if m.get("role").and_then(Value::as_str) == Some("system") {
            let t = flatten_content(m.get("content").unwrap_or(&Value::Null));
            if !t.is_empty() {
                parts.push(t);
            }
        }
    }
    parts.join("\n\n")
}

fn messages_to_prompt(messages: &[Value]) -> String {
    let mut parts = Vec::new();
    for m in messages {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "tool" => {
                let id = m.get("tool_call_id").and_then(Value::as_str).unwrap_or("");
                let out = flatten_content(m.get("content").unwrap_or(&Value::Null));
                parts.push(format!("[tool result {id}]\n{out}"));
            }
            "assistant" if m.get("tool_calls").is_some() => {
                if let Some(calls) = m.get("tool_calls").and_then(Value::as_array) {
                    for tc in calls {
                        let name = tc.pointer("/function/name").and_then(Value::as_str).unwrap_or("?");
                        let args = tc
                            .pointer("/function/arguments")
                            .map(|v| v.to_string())
                            .unwrap_or_default();
                        parts.push(format!("[tool call {name}: {args}]"));
                    }
                }
            }
            "user" | "assistant" | "system" => {
                let t = flatten_content(m.get("content").unwrap_or(&Value::Null));
                if !t.is_empty() {
                    parts.push(t);
                }
            }
            _ => {}
        }
    }
    parts.join("\n\n").trim().to_string()
}

fn pick_model(model: &Value) -> (String, String) {
    let raw = model.as_str().unwrap_or("big-pickle");
    if let Some((p, m)) = raw.split_once('/') {
        return (p.to_string(), m.to_string());
    }
    (MODEL_PROVIDER.to_string(), raw.to_string())
}

async fn post_json(client: &reqwest::Client, url: &str, body: Value) -> Result<Value, String> {
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request {url}: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("opencode {status} at {url}: {}", truncate(&text, 500)));
    }
    serde_json::from_str(&text).map_err(|e| format!("bad json from {url}: {e}: {}", truncate(&text, 200)))
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        s.chars().take(n).collect()
    } else {
        s.to_string()
    }
}

async fn create_session(client: &reqwest::Client, model_id: &str) -> Result<String, String> {
    let body = json!({
        "title": "axon-bridge",
        "model": { "id": model_id, "providerID": MODEL_PROVIDER },
    });
    let j = post_json(client, &format!("{}/session", serve_url()), body).await?;
    j.get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "session response had no id".to_string())
}

async fn delete_session(client: &reqwest::Client, id: &str) {
    let _ = client
        .delete(&format!("{}/session/{id}", serve_url()))
        .send()
        .await;
}

async fn ask_opencode(
    state: &AppState,
    provider: &str,
    model_id: &str,
    system: &str,
    prompt: &str,
) -> Result<(String, u64, u64), String> {
    let sid = create_session(&state.client, model_id).await?;
    let message_url = format!("{}/session/{sid}/message", serve_url());
    let body = json!({
        "parts": [{ "type": "text", "text": if prompt.trim().is_empty() { "Hello" } else { prompt } }],
        "model": { "providerID": provider, "modelID": model_id },
        "tools": serde_json::Map::new(),
        "system": if system.trim().is_empty() { Value::Null } else { Value::String(system.to_string()) },
    });
    let result = async {
        let j = post_json(&state.client, &message_url, body).await?;
        if let Some(err) = j.get("info").and_then(|i| i.get("error")) {
            let code = err.get("code").and_then(Value::as_str).unwrap_or("opencode_error");
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("unknown");
            return Err(format!("{code}: {msg}"));
        }
        let mut text = String::new();
        if let Some(parts) = j.get("parts").and_then(Value::as_array) {
            for p in parts {
                if p.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                        text.push_str(t);
                    }
                }
            }
        }
        let (mut pin, mut pout) = (0u64, 0u64);
        if let Some(tk) = j.get("info").and_then(|i| i.get("tokens")) {
            if let Some(v) = tk.get("input").and_then(Value::as_u64) {
                pin = v;
            }
            if let Some(v) = tk.get("output").and_then(Value::as_u64) {
                pout = v;
            }
        }
        Ok((text.trim().to_string(), pin, pout))
    }
    .await;
    delete_session(&state.client, &sid).await;
    result
}

fn completion_json(model: &str, text: &str, pin: u64, pout: u64) -> Value {
    json!({
        "id": format!("chatcmpl-{}", now_millis()),
        "object": "chat.completion",
        "created": now_secs(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
        }],
        "usage": { "prompt_tokens": pin, "completion_tokens": pout },
    })
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sse_for(model: &str, text: &str, pin: u64, pout: u64) -> String {
    let mut out = String::new();
    for chunk in char_indices_chunks(text, 48) {
        let part = json!({
            "id": format!("chatcmpl-{}", now_millis()),
            "object": "chat.completion.chunk",
            "created": now_secs(),
            "model": model,
            "choices": [{ "index": 0, "delta": { "content": chunk }, "finish_reason": Value::Null }],
        });
        out.push_str(&format!("data: {part}\n\n"));
    }
    let done = json!({
        "id": format!("chatcmpl-{}", now_millis()),
        "object": "chat.completion.chunk",
        "created": now_secs(),
        "model": model,
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": pin, "completion_tokens": pout },
    });
    out.push_str(&format!("data: {done}\n\n"));
    out.push_str("data: [DONE]\n\n");
    out
}

fn char_indices_chunks(s: &str, n: usize) -> Vec<String> {
    s.chars()
        .collect::<Vec<char>>()
        .chunks(n)
        .map(|c| c.iter().collect())
        .collect()
}

async fn handler_chat(
    State(state): State<Arc<AppState>>,
    body: axum::extract::Json<Value>,
) -> Response {
    if let Err(e) = ensure_serve(&state).await {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": { "message": e, "type": "opencode_bridge_error" } })),
        )
            .into_response();
    }

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (provider, model_id) = pick_model(body.get("model").unwrap_or(&Value::Null));
    let system = flatten_system(&messages);
    let prompt = messages_to_prompt(&messages);

    let (text, pin, pout) = match ask_opencode(&state, &provider, &model_id, &system, &prompt).await {
        Ok(ok) => ok,
        Err(e) => {
            let status = if e.starts_with("429") {
                StatusCode::TOO_MANY_REQUESTS
            } else {
                StatusCode::BAD_GATEWAY
            };
            return (
                status,
                Json(json!({ "error": { "message": e, "type": "opencode_bridge_error" } })),
            )
                .into_response();
        }
    };

    let stream_flag = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let model_str = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(model_id.as_str())
        .to_string();
    if stream_flag {
        let body = sse_for(&model_str, &text, pin, pout);
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            axum::body::Body::from(body),
        )
            .into_response()
    } else {
        Json(completion_json(&model_str, &text, pin, pout)).into_response()
    }
}

async fn handler_health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn handler_models() -> Json<Value> {
    let data: Vec<Value> = FREE_MODELS
        .iter()
        .map(|id| {
            json!({
                "id": format!("{MODEL_PROVIDER}/{id}"),
                "object": "model",
                "owned_by": MODEL_PROVIDER,
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": data }))
}

#[tokio::main]
async fn main() {
    let state = Arc::new(AppState {
        client: reqwest::Client::new(),
        serve: Arc::new(Mutex::new(None)),
    });
    let app = Router::new()
        .route("/health", get(handler_health))
        .route("/v1/models", get(handler_models))
        .route("/v1/chat/completions", post(handler_chat))
        .with_state(state.clone());

    let port = env_or("OC_BRIDGE_PORT", "4010");
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("oc-bridge: cannot bind {addr}: {e}");
            std::process::exit(1);
        });

    println!("oc-bridge listening on http://{addr} (v1/chat/completions)");
    println!("opencode serve on {}", serve_url());

    let restart_secs: u64 = env_or("OC_SERVE_RESTART_SECS", "3600")
        .parse()
        .unwrap_or(3600);
    if restart_secs > 0 {
        let restart_state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(restart_secs)).await;
                match restart_serve(&restart_state).await {
                    Ok(()) => println!(
                        "oc-bridge: opencode serve restarted (interval {restart_secs}s)"
                    ),
                    Err(e) => eprintln!("oc-bridge: serve restart failed: {e}"),
                }
            }
        });
        println!("oc-bridge: scheduling opencode serve restart every {restart_secs}s");
    }

    tokio::select! {
        res = axum::serve(listener, app) => {
            if let Err(e) = res {
                eprintln!("oc-bridge: server error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {}
        _ = shutdown_signal() => {}
    }
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    if let Ok(mut sig) = signal(SignalKind::terminate()) {
        let _ = sig.recv().await;
    } else {
        std::future::pending::<()>().await;
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    std::future::pending::<()>().await;
}