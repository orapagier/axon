use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;

use crate::config::RuntimeSettings;
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::router::{SharedRouter, ToolRouter};
use crate::scheduler::SchedulerEngine;
use crate::tools::{FileHandler, ToolRegistry};

pub struct WorkflowCompletion {
    pub workflow_id: String,
    pub description: String,
    pub output: serde_json::Value,
}

#[derive(Clone)]
pub struct AppState {
    pub router: SharedRouter,
    pub tool_router: Arc<ToolRouter>,
    pub tools: ToolRegistry,
    pub memory: Arc<MemoryStore>,
    pub scheduler: Arc<SchedulerEngine>,
    pub mcp: Arc<McpManager>,
    pub files: Arc<FileHandler>,
    pub messaging: Arc<crate::messaging::MessagingHub>,
    pub settings: Arc<RuntimeSettings>,
    pub db: Arc<Pool<SqliteConnectionManager>>,
    pub workflow_tx: tokio::sync::mpsc::UnboundedSender<WorkflowCompletion>,
    pub workflow_cancellations: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
}
