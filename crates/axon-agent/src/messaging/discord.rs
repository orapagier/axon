use super::gateway::{MessageGateway, OutgoingFile, OutgoingMessage};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct DiscordGateway {
    token: String,
    connected: Arc<AtomicBool>,
    client: reqwest::Client,
}

impl DiscordGateway {
    pub fn new(token: String) -> Self {
        let ok = !token.is_empty();
        DiscordGateway {
            token,
            connected: Arc::new(AtomicBool::new(ok)),
            client: crate::http::shared(),
        }
    }
    fn auth(&self) -> String {
        format!("Bot {}", self.token)
    }
    pub async fn start_gateway(self: Arc<Self>, state: Arc<crate::state::AppState>) {
        if self.token.is_empty() {
            return;
        }
        tracing::info!("Discord gateway started (HTTP interaction mode)");
        // Note: Full Discord Gateway WebSocket support requires IDENTIFY + heartbeat.
        // For now, Discord works as send-only (notifications).
        // Messages can be processed via the handle_message method when integrated with
        // Discord Interactions or a future WebSocket gateway implementation.
        let _ = state;
    }

    /// Process an incoming Discord message through the agent pipeline.
    /// Called by Discord Interactions or a future WebSocket gateway handler.
    pub async fn handle_message(
        self: Arc<Self>,
        channel_id: &str,
        text: &str,
        state: Arc<crate::state::AppState>,
    ) {
        if text.is_empty() {
            return;
        }

        let ctx = crate::agent::RunContext::new(
            text,
            "discord",
            Some(channel_id), // Use channel_id as session ID to isolate contexts
            Some(channel_id),
            None,
            None,
            None,
        );

        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let s2 = Arc::clone(&state);
        let self2 = Arc::clone(&self);
        let c2 = channel_id.to_string();
        let t2 = text.to_string();

        tokio::spawn(async move {
            let _ = crate::agent::run_task_streaming(&t2, &*s2, ctx, tx).await;
        });

        let _ = super::streaming::stream_to_gateway(rx, self2, c2).await;
    }
}

#[async_trait]
impl MessageGateway for DiscordGateway {
    fn platform_name(&self) -> &str {
        "discord"
    }
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
    async fn send_text(&self, channel_id: &str, text: &str) -> Result<String> {
        let mut last_id = String::new();
        for chunk in text.as_bytes().chunks(1900) {
            let resp = self
                .client
                .post(format!(
                    "https://discord.com/api/v10/channels/{}/messages",
                    channel_id
                ))
                .header("Authorization", self.auth())
                .json(&serde_json::json!({"content": String::from_utf8_lossy(chunk)}))
                .send()
                .await?;
            if !resp.status().is_success() {
                anyhow::bail!("Discord: {}", resp.text().await.unwrap_or_default());
            }
            let body: serde_json::Value = resp.json().await?;
            last_id = body
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
        }
        Ok(last_id)
    }
    async fn send_file(&self, channel_id: &str, file: OutgoingFile) -> Result<()> {
        let part = reqwest::multipart::Part::bytes(file.data)
            .file_name(file.filename)
            .mime_str(&file.mime_type)?;
        let resp = self
            .client
            .post(format!(
                "https://discord.com/api/v10/channels/{}/messages",
                channel_id
            ))
            .header("Authorization", self.auth())
            .multipart(reqwest::multipart::Form::new().part("files[0]", part))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Discord file: {}", resp.text().await.unwrap_or_default());
        }
        Ok(())
    }
    async fn send_message(&self, channel_id: &str, msg: OutgoingMessage) -> Result<String> {
        let mut last_id = String::new();
        if let Some(t) = &msg.text {
            last_id = self.send_text(channel_id, t).await?;
        }
        for f in msg.files {
            self.send_file(channel_id, f).await?;
        }
        Ok(last_id)
    }
    async fn edit_text(&self, channel_id: &str, message_id: &str, text: &str) -> Result<()> {
        let resp = self
            .client
            .patch(format!(
                "https://discord.com/api/v10/channels/{}/messages/{}",
                channel_id, message_id
            ))
            .header("Authorization", self.auth())
            .json(&serde_json::json!({"content": text}))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Discord edit: {}", resp.text().await.unwrap_or_default());
        }
        Ok(())
    }
}
