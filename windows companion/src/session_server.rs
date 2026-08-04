//! Plane B — the desktop agent's HTTP server.
//!
//! Runs inside the logged-on user's session on `WinSta0\Default`, which is the
//! only place capture, clipboard and `SendInput` actually work. Binds loopback
//! only and is never exposed through the tunnel; the service is its sole client.
//!
//! It deliberately does not read `config.toml`. Everything it needs arrives at
//! launch — the port and public URL on the command line, the shared token on
//! stdin — so `api_secret` never leaves the service's address space.

use axum::{
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info};

use crate::{auth, config::Config, routes, server::AppState};

const BODY_LIMIT: usize = 10 * 1024 * 1024;

pub async fn start(
    port: u16,
    token: String,
    public_url: String,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // A synthetic config carrying only what the desktop routes read.
    // `api_secret` here is the loopback token, not the public API secret: the
    // service authenticates to us with it, and nothing else ever should.
    let config = Arc::new(Config {
        tunnel_token: String::new(),
        api_secret: token,
        port,
        public_url,
        session_port: None,
    });

    let state = AppState {
        config,
        agent: None,
        http: reqwest::Client::new(),
    };

    // Screenshots saved via /screenshot/save land here, and the service's
    // indexer expires them. Both processes resolve this from the same exe path.
    let public = crate::server::public_dir();
    if let Err(e) = std::fs::create_dir_all(&public) {
        error!("Desktop agent could not create {}: {}", public.display(), e);
        return;
    }

    let protected = Router::new()
        .route("/shell", post(routes::shell::run_command))
        .route("/keyboard/type", post(routes::keyboard::type_text))
        .route("/keyboard/key", post(routes::keyboard::press_key))
        .route("/mouse/move", post(routes::mouse::move_mouse))
        .route("/mouse/click", post(routes::mouse::click_mouse))
        .route("/mouse/scroll", post(routes::mouse::scroll_mouse))
        .route("/mouse/drag", post(routes::mouse::drag_mouse))
        .route(
            "/screenshot",
            get(routes::screenshot::capture).post(routes::screenshot::capture),
        )
        .route(
            "/screenshot/save",
            get(routes::screenshot::capture_and_save).post(routes::screenshot::capture_and_save),
        )
        .route("/open", post(routes::files::open_target))
        .route(
            "/clipboard",
            get(routes::clipboard::get_clipboard).post(routes::clipboard::get_clipboard),
        )
        .route("/clipboard/set", post(routes::clipboard::set_clipboard))
        .route(
            "/windows",
            get(routes::window::list_windows).post(routes::window::list_windows),
        )
        .route("/windows/focus", post(routes::window::focus_window))
        .route("/windows/close", post(routes::window::close_window))
        .route("/windows/minimize", post(routes::window::minimize_window))
        .route("/windows/maximize", post(routes::window::maximize_window))
        .route("/windows/resize", post(routes::window::resize_window))
        .route("/notify", post(routes::notification::send_notification))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let app = Router::new()
        .route("/ping", get(routes::ping))
        .merge(protected)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(BODY_LIMIT))
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            let msg = format!(
                "Desktop agent failed to bind {}: {}. Set a free `session_port` in \
                 config.toml if something else holds it.",
                addr, e
            );
            error!("{}", msg);
            crate::log_error_to_file(&msg);
            return;
        }
    };

    info!("Desktop agent listening on http://{}", addr);

    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.changed().await;
            info!("Desktop agent shutting down");
        })
        .await;

    if let Err(e) = served {
        error!("Desktop agent stopped with an error: {}", e);
    }
}
