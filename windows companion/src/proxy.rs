//! Forwards desktop routes from the service (Plane A) to the desktop agent
//! (Plane B) over loopback.
//!
//! Callers see one API on one port. Internally, anything that needs a window
//! station — capture, clipboard, synthetic input, window management — takes a
//! second hop into the interactive session, because a session-0 service
//! physically cannot do those things.

use axum::{
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::server::AppState;

/// Matches the service's own body cap. Screenshots come back through here, but
/// they travel B→A as a response, not a request.
const MAX_FORWARD_BODY: usize = 10 * 1024 * 1024;

pub async fn forward(State(state): State<AppState>, req: Request) -> Response {
    let Some(agent) = state.agent.clone() else {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "NO_AGENT",
            "This process has no desktop agent configured. Desktop routes are only \
             available from the service.",
        );
    };

    if !agent.is_ready() {
        return no_desktop_session();
    }

    let (parts, body) = req.into_parts();

    let bytes = match axum::body::to_bytes(body, MAX_FORWARD_BODY).await {
        Ok(b) => b,
        Err(e) => {
            return problem(
                StatusCode::BAD_REQUEST,
                "BAD_BODY",
                &format!("Could not read request body: {e}"),
            )
        }
    };

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let url = format!("{}{}", agent.base_url(), path_and_query);

    let mut outbound = state
        .http
        .request(parts.method.clone(), &url)
        .header("Authorization", format!("Bearer {}", agent.token));

    if let Some(ct) = parts.headers.get(header::CONTENT_TYPE) {
        outbound = outbound.header(header::CONTENT_TYPE, ct.clone());
    }

    // A GET with a body makes axum's Query extractors on the far side behave
    // inconsistently — routes/agent.rs already avoids this for the same reason.
    if parts.method != Method::GET && !bytes.is_empty() {
        outbound = outbound.body(bytes.to_vec());
    }

    match outbound.send().await {
        Ok(resp) => {
            let status =
                StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            match resp.bytes().await {
                Ok(body) => {
                    let mut response = (status, body).into_response();
                    if let Some(ct) = content_type {
                        if let Ok(v) = header::HeaderValue::from_str(&ct) {
                            response.headers_mut().insert(header::CONTENT_TYPE, v);
                        }
                    }
                    response
                }
                Err(e) => problem(
                    StatusCode::BAD_GATEWAY,
                    "AGENT_READ_FAILED",
                    &format!("Desktop agent response could not be read: {e}"),
                ),
            }
        }
        Err(e) => {
            // The agent was marked ready but is not answering — it most likely
            // died between the check and this call. Wake the supervisor so it
            // relaunches instead of waiting out the liveness poll.
            tracing::warn!("Desktop agent unreachable at {}: {}", url, e);
            agent.kick();
            problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "AGENT_UNREACHABLE",
                &format!(
                    "The desktop agent is not responding on loopback ({e}). It is being \
                     relaunched — retry in a few seconds."
                ),
            )
        }
    }
}

/// The honest answer when the machine is at the login screen. Worth being
/// explicit: this is the one thing the two-plane split cannot paper over, and a
/// caller seeing it should know the shell and file routes still work.
fn no_desktop_session() -> Response {
    problem(
        StatusCode::SERVICE_UNAVAILABLE,
        "NO_DESKTOP_SESSION",
        "No interactive desktop session is available — nobody is logged on, or the \
         session has not finished starting. Screenshot, clipboard, keyboard, mouse and \
         window routes need a logged-in session. /shell, /files/*, /system/*, \
         /processes and /registry/* are unaffected and still work.",
    )
}

fn problem(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": message,
            "code": code,
        })),
    )
        .into_response()
}
