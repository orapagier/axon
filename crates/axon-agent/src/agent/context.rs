use crate::files::AttachedFile;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn default_memory_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunContext {
    pub run_id: String,
    pub parent_run_id: Option<String>,
    pub session_id: String,
    pub task: String,
    pub platform: String,
    pub chat_id: Option<String>,
    pub job_id: Option<String>,
    pub user_time: Option<String>,
    pub depth: u8,
    pub attached_files: Vec<AttachedFile>,
    pub system_prompt: Option<String>,
    pub preferred_model: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default = "default_memory_enabled")]
    pub memory_enabled: bool,
    /// Max number of recent short-term messages to read/retain for this run's
    /// session. `None` falls back to the global short-term cap. Used by the Axon
    /// workflow node to give each node its own sliding-window memory size.
    #[serde(default)]
    pub memory_window: Option<usize>,
}

impl RunContext {
    pub fn new(
        task: &str,
        platform: &str,
        session_id: Option<&str>,
        chat_id: Option<&str>,
        job_id: Option<&str>,
        user_time: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Self {
        RunContext {
            run_id: Uuid::new_v4().to_string(),
            parent_run_id: None,
            session_id: session_id
                .map(|s| s.to_string())
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            task: task.to_string(),
            platform: platform.to_string(),
            chat_id: chat_id.map(|s| s.to_string()),
            job_id: job_id.map(|s| s.to_string()),
            user_time: user_time.map(|s| s.to_string()),
            depth: 0,
            attached_files: vec![],
            system_prompt: system_prompt.map(|s| s.to_string()),
            preferred_model: None,
            allowed_tools: None,
            memory_enabled: true,
            memory_window: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Thinking {
        run_id: String,
        text: String,
    },
    Model {
        run_id: String,
        model: String,
        iteration: u32,
        duration_ms: u64,
    },
    Tools {
        run_id: String,
        tools: Vec<String>,
        tier: String,
        parallel: bool,
    },
    ToolStart {
        run_id: String,
        tool: String,
        tool_call_id: String,
    },
    ToolEnd {
        run_id: String,
        tool: String,
        tool_call_id: String,
        duration_ms: u64,
        ok: bool,
    },
    Token {
        run_id: String,
        text: String,
    },
    Done {
        run_id: String,
        full_text: String,
        total_tokens: u32,
        iterations: u32,
        total_duration_ms: u64,
    },
    Error {
        run_id: String,
        message: String,
    },
    Notification {
        run_id: String,
        level: String,
        title: String,
        message: String,
    },
    MemoryHit {
        run_id: String,
        count: usize,
    },
}
