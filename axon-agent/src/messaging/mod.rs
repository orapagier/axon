pub mod discord;
pub mod gateway;
pub mod slack;
pub mod streaming;
pub mod telegram;

pub use discord::DiscordGateway;
pub use gateway::{IncomingFile, IncomingMessage, MessageGateway, OutgoingFile, OutgoingMessage};
pub use slack::SlackGateway;
pub use telegram::TelegramGateway;

use std::sync::Arc;
use tokio::sync::Mutex;

pub struct MessagingHub {
    pub telegram: Mutex<Option<Arc<TelegramGateway>>>,
    pub discord: Mutex<Option<Arc<DiscordGateway>>>,
    pub slack: Mutex<Option<Arc<SlackGateway>>>,
}

impl MessagingHub {
    pub fn new() -> Self {
        Self {
            telegram: Mutex::new(None),
            discord: Mutex::new(None),
            slack: Mutex::new(None),
        }
    }

    pub async fn get_status(&self) -> serde_json::Value {
        let tg = self.telegram.lock().await;
        let dc = self.discord.lock().await;
        let sl = self.slack.lock().await;

        serde_json::json!({
            "telegram": { "connected": tg.as_ref().map(|g| g.is_connected()).unwrap_or(false) },
            "discord":  { "connected": dc.as_ref().map(|g| g.is_connected()).unwrap_or(false) },
            "slack":    { "connected": sl.as_ref().map(|g| g.is_connected()).unwrap_or(false) },
        })
    }

    /// Send a notification to the active messaging platform.
    ///
    /// Tries the `preferred` platform first (if set and connected), then
    /// auto-detects the first connected gateway. Returns the message ID on
    /// success. Locks are held only long enough to clone the `Arc<Gateway>`,
    /// so I/O happens outside the lock.
    pub async fn send_to_active_platform(
        &self,
        chat_id: &str,
        text: &str,
        preferred: &str,
    ) -> Result<String, String> {
        let platforms: Vec<&str> = if preferred.is_empty() {
            vec!["telegram", "discord", "slack"]
        } else {
            let mut v = vec![preferred];
            for p in &["telegram", "discord", "slack"] {
                if *p != preferred {
                    v.push(p);
                }
            }
            v
        };

        for platform in platforms {
            match platform {
                "telegram" => {
                    let gw = {
                        let tg = self.telegram.lock().await;
                        tg.as_ref()
                            .filter(|g| g.is_connected())
                            .map(|g| Arc::clone(g))
                    };
                    if let Some(gw) = gw {
                        if let Ok(id) = gw.send_text(chat_id, text).await {
                            return Ok(id);
                        }
                    }
                }
                "discord" => {
                    let gw = {
                        let dc = self.discord.lock().await;
                        dc.as_ref()
                            .filter(|g| g.is_connected())
                            .map(|g| Arc::clone(g))
                    };
                    if let Some(gw) = gw {
                        if let Ok(id) = gw.send_text(chat_id, text).await {
                            return Ok(id);
                        }
                    }
                }
                "slack" => {
                    let gw = {
                        let sl = self.slack.lock().await;
                        sl.as_ref()
                            .filter(|g| g.is_connected())
                            .map(|g| Arc::clone(g))
                    };
                    if let Some(gw) = gw {
                        if let Ok(id) = gw.send_text(chat_id, text).await {
                            return Ok(id);
                        }
                    }
                }
                _ => {}
            }
        }

        Err("No messaging platform connected".into())
    }
}
