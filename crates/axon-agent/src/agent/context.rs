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
    /// When true, this run's memory is fully isolated to its own short-term
    /// session window: it does NOT read the global long-term memory or recent
    /// tool observations, and does NOT write its results into long-term memory.
    /// Used by the Axon workflow node so each node starts blank and only ever
    /// knows its own last N interactions — never the rest of the system.
    #[serde(default)]
    pub isolated_memory: bool,
    /// A node-PRIVATE long-term memory partition (Cortex node with Long-term
    /// Memory enabled): recall searches only memories stored under this scope,
    /// and useful results are written back to it. Complements
    /// `isolated_memory` — the run still never touches the shared global
    /// long-term store or the observation log.
    #[serde(default)]
    pub memory_scope: Option<String>,
    /// When true, the run is EXPECTED to answer with machine-readable output
    /// (a bare JSON object, etc.), so the response validators must not treat
    /// that as a "raw dump" and inject a rewrite-in-prose correction — doing so
    /// pollutes the conversation and the model ends up answering the correction
    /// instead of the task. Set by structured workflow nodes (Classifier).
    #[serde(default)]
    pub expects_structured_output: bool,
    /// Forces model routing to a specific role for the ENTIRE run, bypassing
    /// the agent loop's own general-pool routing. Set by workflow nodes that
    /// need a specific model class end-to-end (the Cortex node's Image mode
    /// sets this to `Some("image_model".into())` so a text-only general/paid
    /// model can never silently receive an image it can't read).
    #[serde(default)]
    pub preferred_role: Option<String>,
    /// Pre-resolved image content to prepend to the first user turn (Cortex
    /// node, Image mode). Built by the node executor from the `media` config
    /// field before `run_task` is called.
    #[serde(default)]
    pub image_content: Option<crate::providers::types::ContentBlock>,
    /// True when the request came in by voice (dashboard mic / wake word /
    /// push-to-talk) and its reply will be read aloud. The system prompt then
    /// gets a SPOKEN REPLY hint so the agent answers with a short, conversational
    /// summary instead of dumping long lists/records a listener can't follow.
    #[serde(default)]
    pub voice: bool,
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
            isolated_memory: false,
            memory_scope: None,
            expects_structured_output: false,
            preferred_role: None,
            image_content: None,
            voice: false,
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
