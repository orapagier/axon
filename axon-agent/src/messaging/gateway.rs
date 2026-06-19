use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingMessage {
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub username: Option<String>,
    pub text: Option<String>,
    pub files: Vec<IncomingFile>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingFile {
    pub filename: String,
    pub mime_type: Option<String>,
    pub url: Option<String>,
    pub data: Option<Vec<u8>>,
    pub size: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingMessage {
    pub text: Option<String>,
    pub files: Vec<OutgoingFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingFile {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

impl OutgoingMessage {
    pub fn text(s: impl Into<String>) -> Self {
        OutgoingMessage {
            text: Some(s.into()),
            files: vec![],
        }
    }
}

#[async_trait]
pub trait MessageGateway: Send + Sync {
    fn platform_name(&self) -> &str;
    fn is_connected(&self) -> bool;
    /// Sends text and returns the platform-specific message ID
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<String>;
    async fn send_file(&self, chat_id: &str, file: OutgoingFile) -> Result<()>;
    /// Sends a full message and returns the platform-specific message ID (of the text part if any)
    async fn send_message(&self, chat_id: &str, msg: OutgoingMessage) -> Result<String>;
    /// Edits an existing message's text
    async fn edit_text(&self, chat_id: &str, message_id: &str, text: &str) -> Result<()>;
}
