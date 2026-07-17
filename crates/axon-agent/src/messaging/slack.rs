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
            client: crate::http::shared(),
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
                let mut text = event
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Voice input: a Slack voice clip arrives as a `files` entry
                // with an audio mimetype and no text. When STT is configured
                // (same stt.* settings as the dashboard mic), the transcript
                // becomes the message text.
                if text.is_empty() {
                    if let Some(t) = self.transcribe_voice_clip(event, &state).await {
                        text = t;
                    }
                }
                if !text.is_empty() {
                    let ctx = crate::agent::RunContext::new(
                        &text,
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
                    let t2 = text.clone();

                    tokio::spawn(async move {
                        let _ = crate::agent::run_task_streaming(&t2, &*s2, ctx, tx).await;
                    });

                    let _ = super::streaming::stream_to_gateway(rx, g2, c2).await;
                }
            }
        }
        Ok(serde_json::json!({"ok":true}))
    }

    /// If a message event carries an audio file (Slack voice clips land in
    /// `files` with an `audio/*` mimetype), download it with the bot token
    /// (`files:read` scope) and run it through the configured STT. `None` when
    /// STT is unconfigured, no audio file is attached, or transcription fails
    /// (failures are logged, and the message falls back to text-only handling).
    async fn transcribe_voice_clip(
        &self,
        event: &serde_json::Value,
        state: &crate::state::AppState,
    ) -> Option<String> {
        let cfg = crate::stt::config_from_settings(&state.settings)?;
        let files = event.get("files")?.as_array()?;
        let audio = files.iter().find(|f| {
            f.get("mimetype")
                .and_then(|v| v.as_str())
                .map(|m| m.starts_with("audio/"))
                .unwrap_or(false)
        })?;
        let url = audio
            .get("url_private_download")
            .or_else(|| audio.get("url_private"))
            .and_then(|v| v.as_str())?;
        let name = audio
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("voice_message.m4a");
        let mime = audio
            .get("mimetype")
            .and_then(|v| v.as_str())
            .unwrap_or("audio/mp4");

        // Slack allows files up to 1 GB; anything past the STT cap would be
        // rejected by transcribe() anyway, so don't buffer it into memory in
        // the first place. The event's `size` field catches it pre-download,
        // Content-Length pre-body as a fallback.
        if audio
            .get("size")
            .and_then(|v| v.as_u64())
            .is_some_and(|s| s as usize > crate::stt::MAX_AUDIO_BYTES)
        {
            tracing::warn!("[SLACK] voice file exceeds the 25 MB transcription cap — skipping");
            return None;
        }

        let resp = match self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("[SLACK] voice file download failed: {}", e);
                return None;
            }
        };
        if !resp.status().is_success() {
            tracing::warn!("[SLACK] voice file download failed: {}", resp.status());
            return None;
        }
        if resp
            .content_length()
            .is_some_and(|len| len as usize > crate::stt::MAX_AUDIO_BYTES)
        {
            tracing::warn!("[SLACK] voice file exceeds the 25 MB transcription cap — skipping");
            return None;
        }
        let bytes = resp.bytes().await.ok()?.to_vec();

        match crate::stt::transcribe(&cfg, bytes, name, mime).await {
            Ok(t) if !t.is_empty() => {
                tracing::info!(
                    "[SLACK] voice message transcribed ({} chars)",
                    t.chars().count()
                );
                Some(t)
            }
            Ok(_) => {
                tracing::warn!("[SLACK] voice transcription returned empty text");
                None
            }
            Err(e) => {
                tracing::warn!("[SLACK] voice transcription failed: {:#}", e);
                None
            }
        }
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
