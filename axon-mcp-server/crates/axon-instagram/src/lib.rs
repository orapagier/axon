use anyhow::{Context, Result};
use axon_core::{err_json, ok_json, schema, AppState};
use axon_facebook::auth::{instagram_id, page_token, FB_API};
use rmcp::model::{CallToolResult, Tool};
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use url::Url;

pub struct InstagramService(pub Arc<AppState>);

impl InstagramService {
    const MAX_PAGE_LIMIT: f64 = 50.0;

    pub fn new(state: Arc<AppState>) -> Self {
        Self(state)
    }

    pub fn tool_list() -> Vec<Tool> {
        vec![
            Tool {
                name: "ig_get_account".into(),
                description:
                    "Get Instagram Business Account info (username, bio, followers, etc.).".into(),
                input_schema: schema!({}, []),
            },
            Tool {
                name: "ig_list_media".into(),
                description: "List media (posts, videos) on Instagram.".into(),
                input_schema: schema!({"limit":{"type":"integer","default":10}}, []),
            },
            Tool {
                name: "ig_create_image_post".into(),
                description:
                    "Create an Instagram image post with an image URL or local image path.".into(),
                input_schema: schema!({"image_url":{"type":"string"},"image_path":{"type":"string"},"caption":{"type":"string"}}, ["caption"]),
            },
            Tool {
                name: "ig_create_video_reel".into(),
                description: "Create an Instagram video reel with a video URL or local video path."
                    .into(),
                input_schema: schema!({"video_url":{"type":"string"},"video_path":{"type":"string"},"caption":{"type":"string"}}, ["caption"]),
            },
            Tool {
                name: "ig_list_comments".into(),
                description: "List comments on an Instagram media object.".into(),
                input_schema: schema!({"media_id":{"type":"string"},"limit":{"type":"integer","default":10}}, ["media_id"]),
            },
            Tool {
                name: "ig_reply_to_comment".into(),
                description: "Reply to an Instagram comment.".into(),
                input_schema: schema!({"comment_id":{"type":"string"},"message":{"type":"string"}}, ["comment_id","message"]),
            },
            Tool {
                name: "ig_get_insights".into(),
                description: "Get Instagram account insights (impressions, reach, etc.).".into(),
                input_schema: schema!({}, []),
            },
            Tool {
                name: "ig_list_conversations".into(),
                description: "List Instagram DM conversations.".into(),
                input_schema: schema!({"limit":{"type":"integer","default":10}}, []),
            },
            Tool {
                name: "ig_get_conversation".into(),
                description: "Get message history for an Instagram conversation.".into(),
                input_schema: schema!({"conversation_id":{"type":"string"},"limit":{"type":"integer","default":10}}, ["conversation_id"]),
            },
            Tool {
                name: "ig_send_message".into(),
                description: "Send an Instagram DM text message to a user by their PSID/IGID."
                    .into(),
                input_schema: schema!({"recipient_id":{"type":"string"},"message":{"type":"string"}}, ["recipient_id","message"]),
            },
        ]
    }

    pub async fn call(&self, name: &str, args: Map<String, Value>) -> Result<CallToolResult> {
        let a = &args;
        let s = |key: &str| -> Result<&str> {
            a.get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing required param '{key}'"))
        };
        let n = |key: &str, default: f64| -> f64 {
            a.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
        };
        let opt = |key: &str| {
            a.get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };

        let result: Result<Value> = match name {
            "ig_get_account" => self.get_account().await,
            "ig_list_media" => {
                self.list_media(n("limit", 10.0).min(Self::MAX_PAGE_LIMIT) as u32)
                    .await
            }
            "ig_create_image_post" => {
                let caption = s("caption")?;
                let img = self
                    .resolve_media_input(opt("image_url"), opt("image_path"), "image", false)
                    .await?;
                if img.is_none() {
                    Err(anyhow::anyhow!(
                        "Either image_url or image_path must be provided"
                    ))
                } else {
                    self.create_post(img.as_deref(), None, caption).await
                }
            }
            "ig_create_video_reel" => {
                let caption = s("caption")?;
                let vid = self
                    .resolve_media_input(opt("video_url"), opt("video_path"), "video", true)
                    .await?;
                if vid.is_none() {
                    Err(anyhow::anyhow!(
                        "Either video_url or video_path must be provided"
                    ))
                } else {
                    self.create_post(None, vid.as_deref(), caption).await
                }
            }
            // Backward-compatible alias for older workflow nodes.
            "ig_create_post" => {
                let caption = s("caption")?;
                let img = self
                    .resolve_media_input(opt("image_url"), opt("image_path"), "image", false)
                    .await?;
                let vid = self
                    .resolve_media_input(opt("video_url"), opt("video_path"), "video", true)
                    .await?;

                self.create_post(img.as_deref(), vid.as_deref(), caption)
                    .await
            }
            "ig_list_comments" => {
                self.list_comments(
                    s("media_id")?,
                    n("limit", 10.0).min(Self::MAX_PAGE_LIMIT) as u32,
                )
                .await
            }
            "ig_reply_to_comment" => self.reply_to_comment(s("comment_id")?, s("message")?).await,
            "ig_get_insights" => self.get_insights().await,
            "ig_list_conversations" => {
                self.list_conversations(n("limit", 10.0).min(Self::MAX_PAGE_LIMIT) as u32)
                    .await
            }
            "ig_get_conversation" => {
                self.get_conversation(
                    s("conversation_id")?,
                    n("limit", 10.0).min(Self::MAX_PAGE_LIMIT) as u32,
                )
                .await
            }
            "ig_send_message" => self.send_message(s("recipient_id")?, s("message")?).await,
            _ => Err(anyhow::anyhow!("Unknown Instagram tool: {name}")),
        };

        Ok(match result {
            Ok(v) => ok_json(v),
            Err(e) => err_json(e),
        })
    }

    async fn resolve_media_input(
        &self,
        media_url: Option<&str>,
        media_path: Option<&str>,
        kind: &str,
        is_video: bool,
    ) -> Result<Option<String>> {
        match (media_url, media_path) {
            (Some(_), Some(_)) => Err(anyhow::anyhow!(
                "Provide either {kind}_url or {kind}_path, not both"
            )),
            (Some(url), None) => Ok(Some(url.to_owned())),
            (None, Some(path)) => Ok(Some(self.local_path_to_public_url(path, is_video).await?)),
            (None, None) => Ok(None),
        }
    }

    async fn local_path_to_public_url(&self, raw_path: &str, is_video: bool) -> Result<String> {
        let path = Self::parse_local_path(raw_path)?;
        let canonical = std::fs::canonicalize(&path)
            .with_context(|| format!("Local media path not found: {}", path.display()))?;

        let metadata = std::fs::metadata(&canonical)
            .with_context(|| format!("Cannot access local media file: {}", canonical.display()))?;
        if !metadata.is_file() {
            return Err(anyhow::anyhow!(
                "Local media path is not a file: {}",
                canonical.display()
            ));
        }
        if !Self::is_supported_local_media(&canonical, is_video) {
            return Err(anyhow::anyhow!(
                "Unsupported local {} file extension for Instagram: {}",
                if is_video { "video" } else { "image" },
                canonical.display()
            ));
        }

        let ttl_secs = Self::runtime_config_u64("AXON_MEDIA_URL_TTL_SECS", 2 * 60 * 60);

        let token = self
            .0
            .register_temp_media_file(
                canonical.clone(),
                Some(Self::guess_content_type(&canonical, is_video).to_owned()),
                ttl_secs,
            )
            .await;

        let base = Self::public_media_base_url()?;
        let ext = Self::media_extension(&canonical)
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| {
                if is_video {
                    "mp4".to_string()
                } else {
                    "jpg".to_string()
                }
            });
        // Include a filename hint to improve downstream media-type sniffing compatibility.
        Ok(format!("{base}/media/local/{token}/media.{ext}"))
    }

    fn media_extension(path: &Path) -> Option<String> {
        if let Some(ext) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
        {
            return Some(ext);
        }

        // Handle extension-only hidden filenames such as ".mp4".
        let filename = path.file_name().and_then(|n| n.to_str())?;
        let suffix = filename.strip_prefix('.')?;
        if suffix.is_empty() || suffix.contains('.') {
            return None;
        }
        Some(suffix.to_ascii_lowercase())
    }

    fn parse_local_path(raw_path: &str) -> Result<std::path::PathBuf> {
        let value = Self::normalize_local_path_input(raw_path);
        if value.is_empty() {
            return Err(anyhow::anyhow!("Local media path is empty"));
        }
        if value.starts_with("http://") || value.starts_with("https://") {
            return Err(anyhow::anyhow!(
                "Expected local file path, got URL: {value}"
            ));
        }

        if value.starts_with("file://") {
            let url = Url::parse(&value).with_context(|| format!("Invalid file URL: {value}"))?;
            return url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("Unsupported file URL path: {value}"));
        }

        Ok(std::path::PathBuf::from(value))
    }

    fn normalize_local_path_input(raw_path: &str) -> String {
        let mut value = raw_path
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();

        loop {
            let stripped = value
                .strip_suffix("\\r\\n")
                .or_else(|| value.strip_suffix("\\n"))
                .or_else(|| value.strip_suffix("\\r"))
                .or_else(|| value.strip_suffix("%0D%0A"))
                .or_else(|| value.strip_suffix("%0A"))
                .or_else(|| value.strip_suffix("%0D"));

            let Some(next) = stripped else {
                break;
            };

            value = next.trim_end().to_string();
        }

        value.trim().to_string()
    }

    fn public_media_base_url() -> Result<String> {
        let raw = Self::runtime_config("AXON_PUBLIC_BASE_URL")
            .or_else(|| Self::runtime_config("AXON_CALLBACK_HOST"))
            .unwrap_or_else(|| "http://localhost:8080".to_string());

        let base = if raw.starts_with("http://") || raw.starts_with("https://") {
            raw
        } else {
            format!("http://{raw}")
        };

        let parsed = Url::parse(&base)
            .with_context(|| format!("Invalid AXON_PUBLIC_BASE_URL/AXON_CALLBACK_HOST: {base}"))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("Public media URL is missing host: {base}"))?
            .to_ascii_lowercase();

        if host == "localhost" || host == "127.0.0.1" || host == "0.0.0.0" || host == "::1" {
            return Err(anyhow::anyhow!(
                "Local media paths require a public server URL. Set AXON_PUBLIC_BASE_URL to your public base URL (for example: https://mcp.example.com). Current value resolves to '{base}'."
            ));
        }

        Ok(base.trim_end_matches('/').to_string())
    }

    fn guess_content_type(path: &Path, is_video: bool) -> &'static str {
        let ext = Self::media_extension(path).unwrap_or_default();

        match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            "mp4" => "video/mp4",
            "mov" => "video/quicktime",
            "m4v" => "video/x-m4v",
            "webm" => "video/webm",
            _ if is_video => "video/mp4",
            _ => "image/jpeg",
        }
    }

    fn is_supported_local_media(path: &Path, is_video: bool) -> bool {
        let ext = Self::media_extension(path).unwrap_or_default();

        if is_video {
            matches!(ext.as_str(), "mp4" | "mov" | "m4v" | "webm")
        } else {
            matches!(
                ext.as_str(),
                "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp"
            )
        }
    }

    fn runtime_config(key: &str) -> Option<String> {
        for candidate in
            std::iter::once(key).chain(Self::runtime_config_aliases(key).iter().copied())
        {
            if let Some(v) = Self::runtime_config_from_file(candidate) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
            if let Ok(v) = std::env::var(candidate) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        None
    }

    fn runtime_config_u64(key: &str, default: u64) -> u64 {
        Self::runtime_config(key)
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    }

    fn runtime_config_from_file(key: &str) -> Option<String> {
        let path = Self::runtime_env_path()?;
        let content = std::fs::read_to_string(path).ok()?;
        let parsed = Self::parse_env_like(&content);
        parsed.get(key).cloned()
    }

    fn runtime_config_aliases(key: &str) -> &'static [&'static str] {
        match key {
            "AXON_PUBLIC_BASE_URL" => &["instagram.public_base_url"],
            "AXON_CALLBACK_HOST" => &["instagram.public_base_url"],
            "AXON_MEDIA_URL_TTL_SECS" => &["instagram.media_url_ttl_secs"],
            "AXON_IG_IMAGE_POLL_INTERVAL_SECS" => &["instagram.image_poll_interval_secs"],
            "AXON_IG_IMAGE_POLL_TIMEOUT_SECS" => &["instagram.image_poll_timeout_secs"],
            "AXON_IG_VIDEO_POLL_INTERVAL_SECS" => &["instagram.video_poll_interval_secs"],
            "AXON_IG_VIDEO_POLL_TIMEOUT_SECS" => &["instagram.video_poll_timeout_secs"],
            _ => &[],
        }
    }

    fn runtime_env_path() -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();

        if let Ok(path) = std::env::var("AXON_ENV_FILE") {
            candidates.push(PathBuf::from(path));
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join(".env"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join(".env"));
            }
        }

        candidates.into_iter().find(|p| p.exists())
    }

    fn parse_env_like(raw: &str) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for line in raw.lines() {
            let mut s = line.trim();
            if s.is_empty() || s.starts_with('#') {
                continue;
            }
            if let Some(rest) = s.strip_prefix("export ") {
                s = rest.trim_start();
            }

            let Some((k, v)) = s.split_once('=') else {
                continue;
            };
            let key = k.trim();
            if key.is_empty() {
                continue;
            }
            let mut val = v.trim().to_string();
            if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                val = val[1..val.len() - 1]
                    .replace("\\\"", "\"")
                    .replace("\\\\", "\\");
            } else if val.len() >= 2 && val.starts_with('\'') && val.ends_with('\'') {
                val = val[1..val.len() - 1].to_string();
            }
            out.insert(key.to_string(), val);
        }
        out
    }

    async fn wait_for_container_ready(
        &self,
        token: &str,
        creation_id: &str,
        interval_secs: u64,
        timeout_secs: u64,
    ) -> Result<()> {
        if timeout_secs == 0 {
            return Ok(());
        }

        let interval_secs = interval_secs.max(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(interval_secs)).await;

            let status_resp = self
                .0
                .client
                .get(format!("{FB_API}/{creation_id}"))
                .bearer_auth(token)
                // Only `status_code` is a valid field on IG media containers.
                // `status`, `error`, and `error_message` do not exist and cause
                // a 400 OAuthException (#100) that aborts polling immediately.
                .query(&[("fields", "status_code")])
                .send()
                .await?;
            if !status_resp.status().is_success() {
                let code = status_resp.status();
                let body = status_resp.text().await.unwrap_or_default();
                return Err(anyhow::anyhow!(
                    "Instagram media status API error {code}: {body}"
                ));
            }
            let status: Value = status_resp.json().await?;

            let raw_code = status["status_code"]
                .as_str()
                .or_else(|| status["status"].as_str())
                .unwrap_or("UNKNOWN");
            let code = raw_code.to_ascii_uppercase();

            if matches!(code.as_str(), "FINISHED" | "PUBLISHED") {
                return Ok(());
            }
            if matches!(code.as_str(), "ERROR" | "EXPIRED") {
                return Err(anyhow::anyhow!(
                    "Instagram media processing failed with status: {status}"
                ));
            }

            if std::time::Instant::now() >= deadline {
                return Err(anyhow::anyhow!(
                    "Timeout waiting for Instagram media processing. Last status: {status}"
                ));
            }
        }
    }

    fn is_local_media_url(url: &str) -> bool {
        if let Ok(parsed) = Url::parse(url) {
            return parsed.path().contains("/media/local/");
        }
        url.contains("/media/local/")
    }

    async fn verify_local_media_url(&self, media_url: &str, is_video: bool) -> Result<()> {
        let resp = self
            .0
            .client
            .get(media_url)
            .header("Range", "bytes=0-1023")
            .send()
            .await
            .with_context(|| {
                format!("Failed to reach media URL from axon-mcp: {media_url}. Check AXON_PUBLIC_BASE_URL and reverse proxy routing for /media/local/*")
            })?;

        let status = resp.status();
        if !(status.is_success() || status.as_u16() == 206) {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Local media URL is not reachable ({status}) at {media_url}. Ensure this path is publicly accessible and routed to axon-mcp /media/local/:token. Response: {body}"
            ));
        }

        if let Some(content_type) = resp
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
        {
            let ct = content_type.to_ascii_lowercase();
            let expected = if is_video { "video/" } else { "image/" };
            if !ct.starts_with(expected) {
                return Err(anyhow::anyhow!(
                    "Local media URL returned unexpected content-type '{content_type}' for {media_url}. Expected {expected}*."
                ));
            }
        }

        Ok(())
    }

    async fn get_account(&self) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let ig_id = instagram_id(&self.0).await?;
        let resp: Value = self
            .0
            .client
            .get(format!("{FB_API}/{ig_id}"))
            .bearer_auth(&token)
            .query(&[(
                "fields",
                "id,username,name,biography,followers_count,media_count,profile_picture_url",
            )])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    async fn list_media(&self, limit: u32) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let ig_id = instagram_id(&self.0).await?;
        let resp: Value = self
            .0
            .client
            .get(format!("{FB_API}/{ig_id}/media"))
            .bearer_auth(&token)
            .query(&[
                (
                    "fields",
                    "id,caption,media_type,media_url,timestamp,permalink,like_count,comments_count",
                ),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    async fn create_post(
        &self,
        image_url: Option<&str>,
        video_url: Option<&str>,
        caption: &str,
    ) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let ig_id = instagram_id(&self.0).await?;
        let mut query = vec![("caption", caption)];
        if let Some(v) = video_url {
            if Self::is_local_media_url(v) {
                self.verify_local_media_url(v, true).await?;
            }
            query.push(("video_url", v));
            query.push(("media_type", "REELS"));
            query.push(("share_to_feed", "true"));
        } else if let Some(i) = image_url {
            if Self::is_local_media_url(i) {
                self.verify_local_media_url(i, false).await?;
            }
            query.push(("image_url", i));
        } else {
            return Err(anyhow::anyhow!(
                "Either image_url or video_url must be provided"
            ));
        }

        let container_resp = self
            .0
            .client
            .post(format!("{FB_API}/{ig_id}/media"))
            .bearer_auth(&token)
            .form(&query)
            .send()
            .await?;
        if !container_resp.status().is_success() {
            let code = container_resp.status();
            let body = container_resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Instagram media container API error {code}: {body}"
            ));
        }
        let container: Value = container_resp.json().await?;

        let creation_id = container["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Failed to create Instagram media container"))?;

        if video_url.is_some() {
            let interval = Self::runtime_config_u64("AXON_IG_VIDEO_POLL_INTERVAL_SECS", 10);
            let timeout = Self::runtime_config_u64("AXON_IG_VIDEO_POLL_TIMEOUT_SECS", 600);
            self.wait_for_container_ready(&token, creation_id, interval, timeout)
                .await?;
        } else {
            let interval = Self::runtime_config_u64("AXON_IG_IMAGE_POLL_INTERVAL_SECS", 10);
            let timeout = Self::runtime_config_u64("AXON_IG_IMAGE_POLL_TIMEOUT_SECS", 120);
            self.wait_for_container_ready(&token, creation_id, interval, timeout)
                .await?;
        }

        let publish_resp = self
            .0
            .client
            .post(format!("{FB_API}/{ig_id}/media_publish"))
            .bearer_auth(&token)
            .form(&[("creation_id", creation_id)])
            .send()
            .await?;
        if !publish_resp.status().is_success() {
            let code = publish_resp.status();
            let body = publish_resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Instagram media publish API error {code}: {body}"
            ));
        }
        let publish: Value = publish_resp.json().await?;
        Ok(publish)
    }

    async fn list_comments(&self, media_id: &str, limit: u32) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let resp: Value = self
            .0
            .client
            .get(format!("{FB_API}/{media_id}/comments"))
            .bearer_auth(&token)
            .query(&[
                ("fields", "id,text,timestamp,username,like_count,replies"),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    async fn reply_to_comment(&self, comment_id: &str, message: &str) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let resp: Value = self
            .0
            .client
            .post(format!("{FB_API}/{comment_id}/replies"))
            .bearer_auth(&token)
            .form(&[("message", message)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    async fn get_insights(&self) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let ig_id = instagram_id(&self.0).await?;
        let resp = self
            .0
            .client
            .get(format!("{FB_API}/{ig_id}/insights"))
            .bearer_auth(&token)
            .query(&[("metric", "views,reach,profile_views"), ("period", "day")])
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Instagram Insights API error {status}: {body}"
            ));
        }
        Ok(resp.json().await?)
    }

    async fn list_conversations(&self, limit: u32) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let ig_id = instagram_id(&self.0).await?;
        let resp: Value = self
            .0
            .client
            .get(format!("{FB_API}/{ig_id}/conversations"))
            .bearer_auth(&token)
            .query(&[
                (
                    "fields",
                    "id,participants,updated_time,message_count,unread_count,snippet",
                ),
                ("limit", &limit.to_string()),
                ("platform", "instagram"),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    async fn get_conversation(&self, conversation_id: &str, limit: u32) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let resp: Value = self
            .0
            .client
            .get(format!("{FB_API}/{conversation_id}/messages"))
            .bearer_auth(&token)
            .query(&[
                ("fields", "id,message,from,to,created_time"),
                ("limit", &limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    async fn send_message(&self, recipient_id: &str, text: &str) -> Result<Value> {
        let token = page_token(&self.0).await?;
        let ig_id = instagram_id(&self.0).await?;
        let resp: Value = self
            .0
            .client
            .post(format!("{FB_API}/{ig_id}/messages"))
            .bearer_auth(&token)
            .json(&json!({
                "recipient": { "id": recipient_id },
                "message": { "text": text }
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::InstagramService;
    use std::path::Path;

    #[test]
    fn media_extension_handles_hidden_extension_only_name() {
        let ext = InstagramService::media_extension(Path::new("/tmp/.mp4"));
        assert_eq!(ext.as_deref(), Some("mp4"));
    }
}
