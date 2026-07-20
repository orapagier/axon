use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;

use crate::config::RuntimeSettings;
use crate::mcp::McpManager;
use crate::memory::MemoryStore;
use crate::notify::NotifyHub;
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
    /// Dashboard-facing notification fan-out: persists to `notifications` and
    /// broadcasts to every connected WS client. Additive — messaging delivery
    /// (Telegram/Discord/Slack) still happens through `messaging`.
    pub notify: Arc<NotifyHub>,
    pub settings: Arc<RuntimeSettings>,
    pub db: Arc<Pool<SqliteConnectionManager>>,
    pub workflow_tx: tokio::sync::mpsc::UnboundedSender<WorkflowCompletion>,
    pub workflow_cancellations: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    /// B3: bounds how many workflow runs execute concurrently. Sized at startup
    /// from `workflow.max_concurrent_runs`. Background runs acquire a permit
    /// before executing and release it on completion or durable-wait suspend.
    pub run_semaphore: Arc<tokio::sync::Semaphore>,
    /// B3 gauges: runs currently executing, and runs queued waiting for a permit.
    /// Read by observability (C3) and used to enforce `workflow.max_queue_depth`.
    pub active_runs: Arc<std::sync::atomic::AtomicI64>,
    pub run_queue_depth: Arc<std::sync::atomic::AtomicI64>,
}
