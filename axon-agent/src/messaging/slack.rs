use super::gateway::{MessageGateway, OutgoingFile, OutgoingMessage};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct SlackGateway {
    bot_token: String,
    connected: Arc<AtomicBool>,
    client: reqwest::Client,
}

impl SlackGateway {
    pub fn new(token: String) -> Self {
        let ok = !token.is_empty();
        SlackGateway {
            bot_token: token,
            connected: Arc::new(AtomicBool::new(ok)),
            client: reqwest::Client::new(),
        }
    }
    pub async fn handle_event(
        &self,
        payload: serde_json::Value,
        state: Arc<crate::state::AppState>,
    ) -> Result<serde_json::Value> {
        if let Some(challenge) = payload.get("challenge").and_then(|v| v.as_str()) {
            return Ok(serde_json::json!({"challenge":challenge}));
        }
        if let Some(event) = payload.get("event") {
            if event.get("type").and_then(|v| v.as_str()) == Some("message")
                && event.get("bot_id").is_none()
            {
                let channel = event.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if !text.is_empty() {
                    let ctx = crate::agent::RunContext::new(
                        text,
                        "slack",
                        Some(channel), // Use channel as session ID to isolate contexts
                        Some(channel),
                        None,
                        None,
                        None,
                    );
                    let (tx, rx) = tokio::sync::mpsc::channel(100);
                    let s2 = state.clone();
                    let g2 = Arc::new(crate::messaging::SlackGateway::new(self.bot_token.clone()));
                    let c2 = channel.to_string();
                    let t2 = text.to_string();

                    tokio::spawn(async move {
                        let _ = crate::agent::run_task_streaming(&t2, &*s2, ctx, tx).await;
                    });

                    let _ = super::streaming::stream_to_gateway(rx, g2, c2).await;
                }
            }
        }
        Ok(serde_json::json!({"ok":true}))
    }
}

#[async_trait]
impl MessageGateway for SlackGateway {
    fn platform_name(&self) -> &str {
        "slack"
    }
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
    async fn send_text(&self, channel: &str, text: &str) -> Result<String> {
        let resp = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&serde_json::json!({"channel":channel,"text":text}))
            .send()
            .await?;
        let body: serde_json::Value = resp.json().await?;
        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            anyhow::bail!(
                "Slack: {}",
                body.get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            );
        }
        let ts = body
            .get("ts")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(ts)
    }
    async fn send_file(&self, channel: &str, file: OutgoingFile) -> Result<()> {
        let part = reqwest::multipart::Part::bytes(file.data)
            .file_name(file.filename.clone())
            .mime_str(&file.mime_type)?;
        let form = reqwest::multipart::Form::new()
            .text("channels", channel.to_string())
            .text("filename", file.filename)
            .part("file", part);
        let resp = self
            .client
            .post("https://slack.com/api/files.upload")
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .multipart(form)
            .send()
            .await?;
        if resp
            .json::<serde_json::Value>()
            .await?
            .get("ok")
            .and_then(|v| v.as_bool())
            != Some(true)
        {
            anyhow::bail!("Slack file upload failed");
        }
        Ok(())
    }
    async fn send_message(&self, channel: &str, msg: OutgoingMessage) -> Result<String> {
        let mut last_id = String::new();
        if let Some(t) = &msg.text {
            last_id = self.send_text(channel, t).await?;
        }
        for f in msg.files {
            self.send_file(channel, f).await?;
        }
        Ok(last_id)
    }
    async fn edit_text(&self, channel: &str, message_id: &str, text: &str) -> Result<()> {
        let resp = self
            .client
            .post("https://slack.com/api/chat.update")
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .json(&serde_json::json!({"channel":channel,"ts":message_id,"text":text}))
            .send()
            .await?;
        let body_json: serde_json::Value = resp.json().await?;
        if body_json.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            anyhow::bail!(
                "Slack edit failed: {}",
                body_json
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
            );
        }
        Ok(())
    }
}
