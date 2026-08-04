use axum::{
    extract::{DefaultBodyLimit, Path},
    http::{header, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::{auth, config::Config, proxy, routes, session::SessionAgent};

/// Request body cap for every endpoint except /files/upload.
const BODY_LIMIT: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// The desktop agent to forward capture/clipboard/input routes to.
    /// `None` inside the desktop agent itself, which serves those routes
    /// directly and has nothing further to forward to.
    pub agent: Option<Arc<SessionAgent>>,
    /// Shared client for the loopback hop. Rebuilding it per request would
    /// discard the connection pool.
    pub http: reqwest::Client,
}

pub async fn start(
    config: Arc<Config>,
    agent: Arc<SessionAgent>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let state = AppState {
        config: config.clone(),
        agent: Some(agent),
        http: reqwest::Client::new(),
    };

    // Ensure public dir exists at startup — must exist before we serve from it
    let public = public_dir();
    if let Err(e) = std::fs::create_dir_all(&public) {
        fatal(&format!(
            "Failed to create public directory {}: {}",
            public.display(),
            e
        ));
        return;
    }
    info!("Public files directory: {}", public.display());

    // Protected routes — require Bearer token.
    //
    // ROUTING STRATEGY:
    // The /agent proxy always dispatches internally using whatever method the
    // caller specifies. Our Python tool always sends method:"POST", so every
    // route that the AI needs to read must accept POST in addition to GET.
    //
    // Read-only routes with no body extractors: registered as get+post — the
    // handler ignores the method and has no body extractor, so POST with an
    // empty body works identically to GET.
    //
    // Dual-purpose routes (read OR write depending on body):
    //   /system/env   — POST body:{} = list;  POST body:{key,value} = use /system/env/set
    //   /clipboard    — POST body:{} = read;  POST body:{text} = use /clipboard/set
    // These are split into separate endpoints to avoid the ambiguity of an
    // empty-body POST hitting a handler that requires a JSON body extractor.
    // ── Desktop plane ────────────────────────────────────────────────────────
    // Everything here needs a window station and therefore cannot run in the
    // service's session 0. These are forwarded verbatim to the desktop agent,
    // which answers 503 NO_DESKTOP_SESSION when nobody is logged on.
    let desktop = Router::new()
        .route("/keyboard/type", post(proxy::forward))
        .route("/keyboard/key", post(proxy::forward))
        .route("/mouse/move", post(proxy::forward))
        .route("/mouse/click", post(proxy::forward))
        .route("/mouse/scroll", post(proxy::forward))
        .route("/mouse/drag", post(proxy::forward))
        // Screenshot: GET returns base64 (agent auto-converts to URL); POST same handler.
        // Options may be supplied as query params (GET) or a JSON body (POST).
        .route("/screenshot", get(proxy::forward).post(proxy::forward))
        // Screenshot/save: saves directly to public/, returns {filename,path,url}
        .route("/screenshot/save", get(proxy::forward).post(proxy::forward))
        // ShellExecute from session 0 would open the target on an invisible
        // desktop nobody can see, so /open belongs to the desktop plane too.
        .route("/open", post(proxy::forward))
        .route("/clipboard", get(proxy::forward).post(proxy::forward))
        .route("/clipboard/set", post(proxy::forward))
        .route("/windows", get(proxy::forward).post(proxy::forward))
        .route("/windows/focus", post(proxy::forward))
        .route("/windows/close", post(proxy::forward))
        .route("/windows/minimize", post(proxy::forward))
        .route("/windows/maximize", post(proxy::forward))
        .route("/windows/resize", post(proxy::forward))
        .route("/notify", post(proxy::forward));

    // ── Service plane ────────────────────────────────────────────────────────
    // Served in-process as LocalSystem. These keep working at the lock screen,
    // at the login screen, and with no user logged on at all.
    let protected = Router::new()
        .merge(desktop)
        // /shell picks its plane from run_as: "system" (default) runs here,
        // "user" is forwarded to the desktop agent.
        .route("/shell", post(routes::shell::dispatch))
        .route("/files/read", post(routes::files::read_file))
        .route("/files/write", post(routes::files::write_file))
        .route("/files/list", post(routes::files::list_dir))
        .route("/files/delete", post(routes::files::delete_file))
        .route("/files/exists", post(routes::files::file_exists))
        .route("/files/link", post(routes::files::link_file))
        .route("/files/search", post(routes::files::search_files))
        // system/info: no body extractor — GET and POST both work
        .route("/system/info", get(routes::system::system_info).post(routes::system::system_info))
        .route("/system/power", post(routes::system::power_action))
        // system/env: GET or POST(empty body) = list all; /system/env/set = write
        .route("/system/env", get(routes::system::list_env).post(routes::system::list_env))
        .route("/system/env/set", post(routes::system::set_env))
        // processes: no body extractor — GET and POST both work
        .route("/processes", get(routes::system::list_processes).post(routes::system::list_processes))
        .route("/processes/kill", post(routes::system::kill_process))
        .route("/registry/read", post(routes::registry::read_key))
        .route("/registry/write", post(routes::registry::write_key))
        .route("/agent", post(routes::agent::proxy_endpoint))
        // Tool-RPC surface consumed by axon-agent's DeviceTool. Registered here
        // so it sits behind the same bearer auth as everything else.
        .route(
            "/agent/tools",
            get(routes::agent_tools::list_tools).post(routes::agent_tools::list_tools),
        )
        .route("/agent/tool", post(routes::agent_tools::call_tool))
        .route("/status", get(status).post(status))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // Build the public file router separately so it never gets tangled with
    // the auth middleware applied to `protected` via route_layer.
    let public_router = Router::new().route("/public/:filename", get(serve_public_file));

    // /files/upload is registered on its own router so it can opt out of BOTH
    // body limits and accept arbitrarily large uploads:
    //
    //   1. tower-http's RequestBodyLimitLayer, applied to `limited` below.
    //   2. axum's OWN DefaultBodyLimit, which caps every body-consuming
    //      extractor (including Multipart) at 2 MB unless explicitly disabled.
    //      Skipping DefaultBodyLimit::disable() here silently caps uploads at
    //      2 MB no matter what the tower-http layer says.
    //
    // Still requires auth like the rest of `protected`.
    let upload_router = Router::new()
        .route("/files/upload", post(routes::files::upload_file))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .layer(DefaultBodyLimit::disable());

    // Both limits are set to the same value for everything else, so the cap is
    // BODY_LIMIT regardless of which extractor a handler uses.
    let limited = Router::new()
        .route("/ping", get(routes::ping))
        .merge(public_router)
        .merge(protected)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(BODY_LIMIT));

    // NOTE: no CORS layer. Every client of this API is server-side (the Axon
    // agent, n8n) and sends an Authorization header directly. A permissive
    // Access-Control-Allow-Origin would let any web page the user visits probe
    // this port and read unauthenticated routes (/ping, /public/*) from the
    // browser. Links under /public are opened as top-level navigations, which
    // CORS does not govern, so nothing here needs it.
    let app = Router::new()
        .merge(limited)
        .merge(upload_router)
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let addr = format!("127.0.0.1:{}", config.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            fatal(&format!(
                "Failed to bind {}: {}. Is another copy already running, \
                 or is the port in use by something else?",
                addr, e
            ));
            return;
        }
    };

    info!("API server listening on http://{}", addr);

    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
            info!("Server shutting down gracefully...");
        })
        .await;

    if let Err(e) = served {
        fatal(&format!("Server stopped with an error: {}", e));
    }
}

/// Which half of the API is currently answering.
///
/// Worth checking before blaming a workflow: a 503 from /screenshot at 3am is
/// almost always "the laptop is at the login screen", not a bug.
async fn status(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::Json<serde_json::Value> {
    let desktop_ready = state.agent.as_ref().map(|a| a.is_ready()).unwrap_or(false);
    let console_session = crate::session::active_console_session();
    let tunnel = crate::tunnel::snapshot();

    axum::Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        // Only meaningful when read over the loopback port: if you are reading
        // this through the tunnel, the tunnel is up by definition. That is the
        // point — it lets a local watchdog see an outage the outside cannot.
        "tunnel": tunnel,
        "service_plane": {
            "ready": true,
            "routes": ["/shell", "/files/*", "/system/*", "/processes", "/registry/*"],
            "available_at_lockscreen": true,
        },
        "desktop_plane": {
            "ready": desktop_ready,
            "console_session": console_session,
            "routes": ["/screenshot", "/clipboard", "/keyboard/*", "/mouse/*", "/windows/*", "/notify", "/open"],
            "available_at_lockscreen": false,
            "detail": if desktop_ready {
                "Desktop agent is running in the interactive session."
            } else {
                "No desktop agent. Nobody is logged on, or the agent is restarting."
            },
        }
    }))
}

/// Report a startup failure. In release builds there is no console, so the
/// message also goes to the on-disk error log — otherwise the process would
/// just vanish with no explanation.
fn fatal(msg: &str) {
    error!("{}", msg);
    crate::log_error_to_file(msg);
}

/// Serves a single file from the public/ directory by filename.
/// No auth required — files are short-lived, and their names carry a random
/// component so they cannot be enumerated by guessing timestamps.
async fn serve_public_file(Path(filename): Path<String>) -> Response {
    if !is_safe_filename(&filename) {
        return (StatusCode::BAD_REQUEST, "Invalid filename").into_response();
    }

    let file_path = public_dir().join(&filename);

    match tokio::fs::read(&file_path).await {
        Ok(bytes) => {
            let content_type = mime_from_filename(&filename);
            let mut response = (StatusCode::OK, bytes).into_response();
            let headers = response.headers_mut();
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("inline"),
            );
            headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            response
        }
        Err(e) => {
            tracing::warn!("serve_public_file: could not read {:?}: {}", file_path, e);
            (StatusCode::NOT_FOUND, "File not found").into_response()
        }
    }
}

/// A public filename must be a plain name: no directory separators, no parent
/// references, no drive letters, no leading dot. Anything else could escape the
/// public directory once joined onto it.
fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && !name.contains("..")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.contains('\0')
        && std::path::Path::new(name).components().count() == 1
}

fn mime_from_filename(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Returns the public/ directory next to the running executable.
/// Must match exe_relative_path("public") in agent.rs and exe_public_dir() in files.rs exactly.
pub fn public_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("Cannot resolve executable path");
    exe.parent()
        .expect("Executable has no parent directory")
        .join("public")
}

/// 32 bytes of cryptographic randomness, hex encoded — the shared secret for
/// the loopback hop between the service and the desktop agent.
///
/// Deliberately not `random_token()`. `RandomState::new()` caches its keys in a
/// thread-local and merely increments them between calls, so successive values
/// are related rather than independent. That is acceptable for unguessable
/// public filenames; it is not acceptable for a token that authorises synthetic
/// keyboard and mouse input.
pub fn loopback_token() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };

        let mut buf = [0u8; 32];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };

        if status == 0 {
            return buf.iter().map(|b| format!("{:02x}", b)).collect();
        }

        // Never expected: BCryptGenRandom with the system-preferred RNG does not
        // fail in practice. Loud, because the fallback is materially weaker.
        error!("BCryptGenRandom failed with status {status:#x} — using a weaker fallback token");
    }

    (0..4).map(|_| random_token()).collect()
}

/// 16 hex characters of OS-seeded randomness, used to make /public filenames
/// unguessable. `RandomState` is seeded from the operating system, so this
/// avoids taking a dependency on `rand` for a single use.
pub fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_usize(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as usize);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_filenames() {
        for bad in [
            "",
            "..",
            "../secret.txt",
            "..\\secret.txt",
            "sub/dir.png",
            "sub\\dir.png",
            "C:file.png",
            ".hidden",
        ] {
            assert!(!is_safe_filename(bad), "should have rejected {:?}", bad);
        }
    }

    #[test]
    fn accepts_generated_filenames() {
        assert!(is_safe_filename("screenshot_1740000000_a1b2c3d4e5f60718.png"));
        assert!(is_safe_filename("notes_1740000000_deadbeefcafe0001.txt"));
    }

    #[test]
    fn random_token_is_hex_and_varies() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "tokens should not repeat");
    }
}
