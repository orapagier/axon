use axum::{
    extract::Path,
    http::{header, HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tokio::sync::watch;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::{auth, config::Config, routes};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
}

pub async fn start(config: Arc<Config>, mut shutdown_rx: watch::Receiver<bool>) {
    let state = AppState {
        config: config.clone(),
    };

    // Ensure public dir exists at startup — must exist before we serve from it
    let public = public_dir();
    std::fs::create_dir_all(&public).expect("Failed to create public directory");
    info!("Public files directory: {}", public.display());

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_headers(Any)
        .allow_methods(Any);

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
    let protected = Router::new()
        .route("/shell", post(routes::shell::run_command))
        .route("/keyboard/type", post(routes::keyboard::type_text))
        .route("/keyboard/key", post(routes::keyboard::press_key))
        .route("/mouse/move", post(routes::mouse::move_mouse))
        .route("/mouse/click", post(routes::mouse::click_mouse))
        .route("/mouse/scroll", post(routes::mouse::scroll_mouse))
        .route("/mouse/drag", post(routes::mouse::drag_mouse))
        // Screenshot: GET returns base64 (agent auto-converts to URL); POST same handler
        .route("/screenshot", get(routes::screenshot::capture).post(routes::screenshot::capture))
        // Screenshot/save: saves directly to public/, returns {filename,path,url}
        .route("/screenshot/save", get(routes::screenshot::capture_and_save).post(routes::screenshot::capture_and_save))
        .route("/open", post(routes::files::open_target))
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
        // clipboard: GET or POST(empty body) = read; /clipboard/set = write
        .route("/clipboard", get(routes::clipboard::get_clipboard).post(routes::clipboard::get_clipboard))
        .route("/clipboard/set", post(routes::clipboard::set_clipboard))
        // windows: no body extractor — GET and POST both work
        .route("/windows", get(routes::window::list_windows).post(routes::window::list_windows))
        .route("/windows/focus", post(routes::window::focus_window))
        .route("/windows/close", post(routes::window::close_window))
        .route("/windows/minimize", post(routes::window::minimize_window))
        .route("/windows/maximize", post(routes::window::maximize_window))
        .route("/windows/resize", post(routes::window::resize_window))
        .route("/notify", post(routes::notification::send_notification))
        .route("/registry/read", post(routes::registry::read_key))
        .route("/registry/write", post(routes::registry::write_key))
        .route("/agent", post(routes::agent::proxy_endpoint))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    // Build the public file router separately so it never gets tangled with
    // the auth middleware applied to `protected` via route_layer.
    let public_router = Router::new().route("/public/:filename", get(serve_public_file));

    // /files/upload is registered on its own router, merged in AFTER the
    // RequestBodyLimitLayer below is applied to everything else, so uploads
    // are not capped at 10MB. Still requires auth like the rest of `protected`.
    let upload_router = Router::new()
        .route("/files/upload", post(routes::files::upload_file))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let limited = Router::new()
        .route("/ping", get(routes::ping))
        .merge(public_router)
        .merge(protected)
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            10 * 1024 * 1024,
        ));

    let app = Router::new()
        .merge(limited)
        .merge(upload_router)
        .with_state(state)
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    let addr = format!("127.0.0.1:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind to port");

    info!("API server listening on http://{}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
            info!("Server shutting down gracefully...");
        })
        .await
        .unwrap();
}

/// Serves a single file from the public/ directory by filename.
/// No auth required — files are short-lived and contain no sensitive data.
async fn serve_public_file(Path(filename): Path<String>) -> Response {
    // Block path traversal attacks
    if filename.contains("..") || filename.contains('/') || filename.contains('\\') {
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
