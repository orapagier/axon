use axum::{extract::{Query, State}, Json};
use base64::{engine::general_purpose::STANDARD, Engine};
use image::{ImageBuffer, Rgba};
use screenshots::Screen;
use serde::{Deserialize, Serialize};

use crate::{routes::AppError, server::AppState};

#[derive(Deserialize, Clone)]
pub struct ScreenshotQuery {
    /// Which monitor to capture (0 = primary). Defaults to 0.
    #[serde(default)]
    pub screen: usize,
    /// Quality: 1-100 (only affects JPEG if format=jpeg). Default 90.
    #[allow(dead_code)]
    #[serde(default = "default_quality")]
    pub quality: u8,
    /// "png" (default) or "jpeg"
    #[serde(default = "png_str")]
    pub format: String,
    /// Crop region (optional): x, y, width, height
    pub crop_x: Option<u32>,
    pub crop_y: Option<u32>,
    pub crop_w: Option<u32>,
    pub crop_h: Option<u32>,
}

fn default_quality() -> u8 { 90 }
fn png_str() -> String { "png".to_string() }

/// A JSON body wins over query parameters when both are supplied. `Option<Json<T>>`
/// yields `None` for a GET, a bodyless POST, or a body that fails to parse, so
/// query-only callers keep working exactly as before.
fn merge_options(
    query: ScreenshotQuery,
    body: Option<Json<ScreenshotQuery>>,
) -> ScreenshotQuery {
    match body {
        Some(Json(b)) => b,
        None => query,
    }
}

#[derive(Serialize)]
pub struct ScreenshotResponse {
    /// Base64-encoded image — the /agent proxy automatically converts this to
    /// a public download URL via save_base64_fields(). Callers through /agent
    /// will see { "image": { "url": "...", "note": "..." } } instead of raw base64.
    pub image: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub screen_index: usize,
    pub screen_count: usize,
}

#[derive(Serialize)]
pub struct SaveResponse {
    pub filename: String,
    pub path: String,
    pub url: String,
}

/// Captures a screenshot and returns it as a base64-encoded image.
/// When called through /agent, the proxy automatically intercepts the base64
/// image field and replaces it with a { url, note } object pointing to a
/// temporary public file. No manual base64 decoding needed by the caller.
///
/// Options may arrive either as query parameters (`GET /screenshot?screen=1`)
/// or as a JSON body (`POST /screenshot {"screen": 1}`). The body form matters:
/// the /agent proxy only ever forwards a JSON body, so with query parameters
/// alone every agent-driven capture silently fell back to a full primary-screen
/// PNG regardless of what the caller asked for.
pub async fn capture(
    Query(query): Query<ScreenshotQuery>,
    body: Option<Json<ScreenshotQuery>>,
) -> Result<Json<ScreenshotResponse>, AppError> {
    let query = merge_options(query, body);
    tokio::task::spawn_blocking(move || {
        let screens = Screen::all().map_err(|e| AppError::internal(e.to_string()))?;

        let screen_count = screens.len();
        let screen = screens
            .get(query.screen)
            .ok_or_else(|| AppError::not_found(format!("Screen {} not found", query.screen)))?;

        let captured = screen
            .capture()
            .map_err(|e| AppError::internal(e.to_string()))?;

        let mut img: image::DynamicImage = {
            let raw = captured.as_raw().to_vec();
            let buf: ImageBuffer<Rgba<u8>, Vec<u8>> =
                ImageBuffer::from_raw(captured.width(), captured.height(), raw)
                    .ok_or_else(|| AppError::internal("Image buffer error"))?;
            image::DynamicImage::ImageRgba8(buf)
        };

        if let (Some(cx), Some(cy), Some(cw), Some(ch)) =
            (query.crop_x, query.crop_y, query.crop_w, query.crop_h)
        {
            img = img.crop_imm(cx, cy, cw, ch);
        }

        let width = img.width();
        let height = img.height();

        let mut buf = std::io::Cursor::new(Vec::new());
        match query.format.as_str() {
            "jpeg" | "jpg" => img
                .write_to(&mut buf, image::ImageFormat::Jpeg)
                .map_err(|e| AppError::internal(e.to_string()))?,
            _ => img
                .write_to(&mut buf, image::ImageFormat::Png)
                .map_err(|e| AppError::internal(e.to_string()))?,
        }

        let encoded = STANDARD.encode(buf.into_inner());

        Ok(Json(ScreenshotResponse {
            image: encoded,
            format: query.format,
            width,
            height,
            screen_index: query.screen,
            screen_count,
        }))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

/// Captures a screenshot, saves it directly to the public/ folder, and returns
/// the filename, local path, and public URL — no base64 round trip needed.
///
/// Uses config.public_url (same source as files.rs and agent.rs) for consistent URLs.
pub async fn capture_and_save(
    State(state): State<AppState>,
    Query(query): Query<ScreenshotQuery>,
    body: Option<Json<ScreenshotQuery>>,
) -> Result<Json<SaveResponse>, AppError> {
    // Extract public_url before entering spawn_blocking (AppState is not Send-safe to move)
    let public_url = state.config.public_url.clone();
    let query = merge_options(query, body);

    tokio::task::spawn_blocking(move || {
        let screens = Screen::all().map_err(|e| AppError::internal(e.to_string()))?;

        let screen = screens
            .get(query.screen)
            .ok_or_else(|| AppError::not_found(format!("Screen {} not found", query.screen)))?;

        let captured = screen
            .capture()
            .map_err(|e| AppError::internal(e.to_string()))?;

        let mut img: image::DynamicImage = {
            let raw = captured.as_raw().to_vec();
            let buf: ImageBuffer<Rgba<u8>, Vec<u8>> =
                ImageBuffer::from_raw(captured.width(), captured.height(), raw)
                    .ok_or_else(|| AppError::internal("Image buffer error"))?;
            image::DynamicImage::ImageRgba8(buf)
        };

        if let (Some(cx), Some(cy), Some(cw), Some(ch)) =
            (query.crop_x, query.crop_y, query.crop_w, query.crop_h)
        {
            img = img.crop_imm(cx, cy, cw, ch);
        }

        let ext = match query.format.as_str() {
            "jpeg" | "jpg" => "jpg",
            _ => "png",
        };

        // Timestamped filename keeps ordering obvious; the random suffix stops
        // the unauthenticated /public route from being enumerated by guessing
        // recent timestamps, and removes the collision risk within one second.
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filename = format!(
            "screenshot_{}_{}.{}",
            timestamp,
            crate::server::random_token(),
            ext
        );

        // Save directly into public/ — same dir the static file server reads from
        let public = crate::server::public_dir();
        std::fs::create_dir_all(&public).map_err(|e| AppError::internal(e.to_string()))?;
        let file_path = public.join(&filename);

        let fmt = if ext == "jpg" {
            image::ImageFormat::Jpeg
        } else {
            image::ImageFormat::Png
        };
        img.save_with_format(&file_path, fmt)
            .map_err(|e| AppError::internal(e.to_string()))?;

        // Use config.public_url — consistent with files.rs and agent.rs
        let base = if public_url.trim().is_empty() {
            tracing::warn!("public_url is not configured; screenshot URL will be relative");
            String::new()
        } else {
            public_url.trim_end_matches('/').to_string()
        };
        let url = format!("{}/public/{}", base, filename);

        Ok(Json(SaveResponse {
            filename,
            path: file_path.to_string_lossy().to_string(),
            url,
        }))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}
