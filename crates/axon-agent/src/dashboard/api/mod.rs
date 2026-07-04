use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::{http::HeaderMap, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// Unwraps a DB/statement `Result` inside a `Json<Value>`-returning handler.
/// A failure (schema drift, poisoned pool, disk error) becomes a logged
/// `{"error": …}` response instead of a panic that kills the request task.
macro_rules! try_json {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Dashboard API DB error: {e}");
                return Json(json!({ "error": format!("internal error: {e}") }));
            }
        }
    };
}

mod channels;
mod chat_memory;
mod credentials;
mod crm;
mod google;
mod infra;
mod jobs;
mod mcp_files;
mod models;
mod oauth;
mod runs;
mod settings;
mod tools_router;
mod watchers;
mod workflows;

pub use channels::*;
pub use chat_memory::*;
pub use credentials::*;
pub use crm::*;
pub use google::*;
pub use infra::*;
pub use jobs::*;
pub use mcp_files::*;
pub use models::*;
pub use oauth::*;
pub use runs::*;
pub use settings::*;
pub use tools_router::*;
pub use watchers::*;
pub use workflows::*;
