//! Image/video processing tool for the Axon AI agent.
//!
//! Exposes `image_processor` capabilities as a JSON-driven internal tool.
//! The agent sends JSON args with an "action" field; this module dispatches
//! to the appropriate pipeline operations and returns a JSON result.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use image::GenericImageView;
use serde_json::{json, Value};

use image_processor::video::{AudioCodec, PixelFormat, VideoCodec, VideoConfig, VideoContainer};
use image_processor::{
    GradientDirection, Gravity, ImagePipeline, LoadedFont, OutputFormat, TextAlignment, TextStyle,
};

// Ã¢â€â‚¬Ã¢â€â‚¬ Public entry point Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬

/// Handle an `image_tool` invocation from the agent loop.
pub async fn handle_image(args: Value) -> Result<Value> {
    let action = args["action"].as_str().unwrap_or("").to_string();

    match action.as_str() {
        "process"      => action_process(args).await,
        "quote_image"  => action_quote_image(args).await,
        "filters"      => action_filters(args).await,
        "info"         => action_info(args).await,
        "video"        => action_video(args).await,
        "slideshow"    => action_slideshow(args).await,
        other => anyhow::bail!(
            "Unknown image_tool action: '{}'. Valid: process, quote_image, filters, info, video, slideshow",
            other
        ),
    }
}

// Ã¢â€â‚¬Ã¢â€â‚¬ Action handlers Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬

/// Generic pipeline: load Ã¢â€ â€™ chain of operations Ã¢â€ â€™ save.
///
/// Accepts a JSON array of "steps", each { "op": "...", ... }.
async fn action_process(args: Value) -> Result<Value> {
    let output = clean_path(require_str(&args, "output")?);
    let steps = args["steps"]
        .as_array()
        .context("'steps' must be an array of operations")?;

    let mut pipe =
        ImagePipeline::new(load_image_from_args(&args).context("Failed to load input image")?);

    for (i, step) in steps.iter().enumerate() {
        let op = step["op"]
            .as_str()
            .with_context(|| format!("step[{}] missing 'op'", i))?;

        pipe =
            apply_op(pipe, op, step).with_context(|| format!("step[{}] op='{}' failed", i, op))?;
    }

    let fmt = parse_format(args["format"].as_str());
    match fmt {
        Some(f) => pipe.save_as(&output, f)?,
        None => pipe.save(&output)?,
    }

    Ok(json!({
        "status": "ok",
        "output": output,
        "message": format!("Processed {} steps, saved to {}", steps.len(), output),
    }))
}

/// Create a quote/devotional image with text overlay on a background.
async fn action_quote_image(args: Value) -> Result<Value> {
    let input = clean_path(require_str(&args, "input")?);
    let output = clean_path(require_str(&args, "output")?);
    let text = require_str(&args, "text")?;
    let attribution = args["attribution"].as_str().unwrap_or("").trim();

    let raw_font_path =
        parse_nonempty_str_arg(&args, "font_path").unwrap_or("/fonts/Playball-Regular.ttf");
    let font_path =
        resolve_existing_path(raw_font_path).unwrap_or_else(|| raw_font_path.to_string());
    let main_font = load_font_with_fallback(&font_path)
        .with_context(|| format!("Failed to load font: {}", font_path))?;

    let attr_font_source =
        parse_nonempty_str_arg(&args, "attribution_font_path").unwrap_or(font_path.as_str());
    let attr_font_path =
        resolve_existing_path(attr_font_source).unwrap_or_else(|| attr_font_source.to_string());
    let attr_font = load_font_with_fallback(&attr_font_path)
        .with_context(|| format!("Failed to load attribution font: {}", attr_font_path))?;

    // Load image and get dimensions
    // Uses input_binary (base64) if provided, falls back to file path
    let img =
        load_image_from_args(&args).with_context(|| format!("Failed to open image: {}", input))?;
    let (width, height) = img.dimensions();

    // Optional resize
    let target_w = parse_u32_arg(&args, "width")?.unwrap_or(width);
    let target_h = parse_u32_arg(&args, "height")?.unwrap_or(height);

    // Auto-detect aesthetic text color based on dominant background color
    let text_color = image_processor::utils::auto_text_color_from_dominant(&img);
    let text_color_arr = [text_color[0], text_color[1], text_color[2], 255];
    let main_color = parse_optional_color4_arg(&args, "font_color").unwrap_or(text_color_arr);
    let attr_color =
        parse_optional_color4_arg(&args, "attribution_font_color").unwrap_or(text_color_arr);
    // Dynamic margins: Landscape benefits from wider side margins to force larger fonts
    let is_landscape = target_w > target_h;
    let margin_x = if is_landscape {
        (target_w as f32 * 0.12).max(40.0) as u32
    } else {
        (target_w as f32 * 0.05).max(20.0) as u32
    };
    let margin_y = (target_h as f32 * 0.08).max(40.0) as u32;
    let overlay_margin_x = parse_u32_arg(&args, "overlay_margin_x")?.unwrap_or(margin_x);
    let overlay_margin_y = parse_u32_arg(&args, "overlay_margin_y")?.unwrap_or(margin_y);

    // Aesthetic sizing: diagonal-based scale factor respects all orientations
    let diag = ((target_w * target_w + target_h * target_h) as f32).sqrt();
    let scale_factor = (diag / 1280.0).max(0.4);

    let char_count = text.len();
    let line_count = text.lines().count();
    let (main_size_base, attr_size_base) = adaptive_font_sizes(char_count, line_count);

    // Initial size should be very large to give the binary search fitter headroom
    // to fill the available space. Default to 50% of width if not specified.
    let main_size = parse_f32_arg(&args, "font_size")?
        .unwrap_or((target_w as f32 * 0.5 * scale_factor).min(600.0));

    let attr_size = parse_f32_arg(&args, "attribution_font_size")?
        .unwrap_or(main_size * (attr_size_base as f32 / main_size_base as f32));

    let main_alignment =
        parse_alignment(parse_nonempty_str_arg(&args, "alignment").or(Some("left")));
    let attr_alignment =
        parse_alignment(parse_nonempty_str_arg(&args, "attribution_alignment").or(Some("left")));

    let main_style = TextStyle {
        size: main_size,
        color: main_color,
        alignment: main_alignment,
        shadow: None,
        line_height: 1.2,
        ..Default::default()
    };

    let attr_style = TextStyle {
        size: attr_size,
        color: attr_color,
        alignment: attr_alignment,
        shadow: None,
        line_height: 1.15,
        ..Default::default()
    };

    // Build pipeline
    let mut pipe = ImagePipeline::new(img);

    if target_w != width || target_h != height {
        pipe = pipe.resize_fill(target_w, target_h);
    }

    // Determine if we need a text background box based on text color (white = dark box, dark = white box)
    let is_light_text = text_color[0] > 128 && text_color[1] > 128 && text_color[2] > 128;
    let bg_color = if is_light_text {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    };

    // Check if the user specified a background box preference
    let add_box = args["add_background_box"].as_bool().unwrap_or(true);

    if add_box {
        pipe = pipe.add_text_background(
            margin_x.min(margin_y),
            bg_color,
            0.2, // Subtle 20% opacity
            20,  // 20px padding around the text region
        );
    }

    // "Sandwich" gradients ensure light text is readable on both bright skies and light grass
    pipe = pipe.gradient_overlay([0, 0, 0, 100], [0, 0, 0, 0], GradientDirection::TopToBottom);
    pipe = pipe.gradient_overlay([0, 0, 0, 0], [0, 0, 0, 120], GradientDirection::BottomToTop);

    if !attribution.is_empty() {
        pipe = pipe.add_two_texts(
            &text,
            &main_font,
            &main_style,
            attribution,
            &attr_font,
            &attr_style,
            margin_x,
            margin_y,
        );
    } else {
        pipe = pipe.add_text(&text, &main_font, &main_style, margin_x, margin_y);
    }

    let mut custom_ov_w = None;
    let mut custom_ov_h = None;
    if let Some(size_raw) = parse_nonempty_str_arg(&args, "overlay_image_size") {
        let size_str = size_raw.to_lowercase();
        let parts: Vec<&str> = size_str.split('x').collect();
        if parts.len() == 2 {
            if let (Ok(w), Ok(h)) = (
                parts[0].trim().parse::<u32>(),
                parts[1].trim().parse::<u32>(),
            ) {
                custom_ov_w = Some(w);
                custom_ov_h = Some(h);
            }
        } else if parts.len() == 1 {
            if let Ok(s) = parts[0].trim().parse::<u32>() {
                custom_ov_w = Some(s);
                custom_ov_h = Some(s);
            }
        }
    }

    let overlay_position = parse_gravity_anchor(
        parse_nonempty_str_arg(&args, "overlay_position").unwrap_or("top-right"),
    );

    let mut icon_shares_position_with_text = false;
    for item in collection_entries(&args, "additional_texts") {
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !text.is_empty() {
            let pos = parse_gravity_anchor(
                item.get("position")
                    .and_then(Value::as_str)
                    .unwrap_or("bottom-left"),
            );
            if std::mem::discriminant(&pos) == std::mem::discriminant(&overlay_position) {
                icon_shares_position_with_text = true;
                break;
            }
        }
    }

    if let Some(overlay_image_path) = parse_nonempty_str_arg(&args, "overlay_image_path") {
        if !icon_shares_position_with_text {
            pipe = apply_overlay_image(
                pipe,
                overlay_image_path,
                overlay_position,
                target_w,
                target_h,
                custom_ov_w,
                custom_ov_h,
                overlay_margin_x,
                overlay_margin_y,
            )
            .await?;
        }
    }

    // collect overlay image info so text overlays can stack beside it
    let overlay_icon_info: Option<(image::DynamicImage, Gravity)> =
        if let Some(ov_path) = parse_nonempty_str_arg(&args, "overlay_image_path") {
            let ov_pos = parse_gravity_anchor(
                parse_nonempty_str_arg(&args, "overlay_position").unwrap_or("top-right"),
            );
            let max_icon_w = custom_ov_w.unwrap_or(50);
            let max_icon_h = custom_ov_h.unwrap_or(50);
            if let Ok(local) = ensure_local_file(clean_path(ov_path)).await {
                if let Ok(img) = load_image_robust(&local) {
                    let resized = if custom_ov_w.is_some() || custom_ov_h.is_some() {
                        img.resize_exact(
                            max_icon_w,
                            max_icon_h,
                            image::imageops::FilterType::Lanczos3,
                        )
                    } else {
                        img.thumbnail(max_icon_w, max_icon_h)
                    };
                    Some((resized, ov_pos))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

    pipe = apply_additional_text_overlays(
        pipe,
        &args,
        &main_font,
        text_color_arr,
        target_w,
        target_h,
        scale_factor,
        overlay_icon_info.as_ref(),
    )?;

    let fmt = parse_format(args["format"].as_str());
    match fmt {
        Some(f) => pipe.save_as(&output, f)?,
        None => pipe.save(&output)?,
    }

    Ok(json!({
        "status": "ok",
        "output": output,
        "message": format!("Quote image saved to {}", output),
    }))
}

/// Apply a chain of image filters.
async fn action_filters(args: Value) -> Result<Value> {
    let output = clean_path(require_str(&args, "output")?);
    let filters = args["filters"]
        .as_array()
        .context("'filters' must be an array")?;

    let mut pipe = ImagePipeline::new(load_image_from_args(&args)?);

    for f in filters {
        let name = f["name"].as_str().unwrap_or("");
        pipe = match name {
            "blur" => pipe.blur(f["sigma"].as_f64().unwrap_or(3.0) as f32),
            "sharpen" => pipe.sharpen(
                f["sigma"].as_f64().unwrap_or(3.0) as f32,
                f["threshold"].as_i64().unwrap_or(1) as i32,
            ),
            "brightness" => pipe.brightness(f["value"].as_i64().unwrap_or(20) as i32),
            "contrast" => pipe.contrast(f["value"].as_f64().unwrap_or(20.0) as f32),
            "grayscale" => pipe.grayscale(),
            "sepia" => pipe.sepia(),
            "invert" => pipe.invert(),
            "saturation" => pipe.saturation(f["factor"].as_f64().unwrap_or(1.5) as f32),
            "hue_rotate" => pipe.hue_rotate(f["degrees"].as_f64().unwrap_or(90.0) as f32),
            "vignette" => pipe.vignette(f["strength"].as_f64().unwrap_or(0.6) as f32),
            other => anyhow::bail!("Unknown filter: '{}'", other),
        };
    }

    let fmt = parse_format(args["format"].as_str());
    match fmt {
        Some(f) => pipe.save_as(&output, f)?,
        None => pipe.save(&output)?,
    }
    Ok(json!({
        "status": "ok",
        "output": output,
        "filters_applied": filters.len(),
    }))
}

/// Get image info: dimensions, EXIF, dominant color, brightness.
async fn action_info(args: Value) -> Result<Value> {
    // Resolve the best available file path for EXIF (which needs a real path).
    // Falls back to empty string Ã¢â‚¬â€ EXIF will simply be skipped if unavailable.
    let exif_path = args["binary"]
        .as_object()
        .and_then(|b| b.get("local_path"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| args["input"].as_str().unwrap_or(""));
    let exif_path = clean_path(exif_path);

    let img = load_image_from_args(&args)?;
    let (w, h) = img.dimensions();

    let dominant = image_processor::utils::dominant_color(&img);
    let is_dark = image_processor::utils::is_dark(&img);

    let mut info = json!({
        "width": w,
        "height": h,
        "dominant_color": format!("#{:02x}{:02x}{:02x}", dominant[0], dominant[1], dominant[2]),
        "is_dark": is_dark,
        "has_transparency": image_processor::utils::has_transparency(&img),
    });

    // Try EXIF (may fail on non-jpeg/tiff)
    match image_processor::utils::read_exif(&exif_path) {
        Ok(exif) => {
            info["exif"] = json!({
                "make": exif.make,
                "model": exif.model,
                "datetime": exif.datetime,
                "iso": exif.iso,
                "exposure": exif.exposure_time,
                "f_number": exif.f_number,
                "software": exif.software,
                "gps_lat": exif.gps_latitude,
                "gps_lon": exif.gps_longitude,
            });
        }
        Err(_) => {} // No EXIF, that's fine
    }

    Ok(info)
}

/// Create a video from a still image + audio track (requires ffmpeg).
async fn action_video(args: Value) -> Result<Value> {
    let image_path = clean_path(require_str(&args, "image_path")?);
    let audio_path = clean_path(require_str(&args, "audio_path")?);

    let image_path = ensure_local_file(image_path).await?;
    let audio_path = ensure_local_file(audio_path).await?;

    let (preset, config) = build_video_config(&args)?;

    let output = resolve_video_output(&args, &config)?;

    // Run blocking ffmpeg in a spawn_blocking task
    let image_path_owned = image_path.to_string();
    let audio_path_owned = audio_path.to_string();
    let output_owned = output.clone();
    let config_for_run = config.clone();

    tokio::task::spawn_blocking(move || {
        image_processor::video::image_to_video(
            &image_path_owned,
            &audio_path_owned,
            &output_owned,
            &config_for_run,
        )
    })
    .await
    .context("Video task panicked")?
    .map_err(|e| anyhow::anyhow!("Video creation failed: {}", e))?;

    Ok(json!({
        "status": "ok",
        "output": output,
        "preset": preset,
        "config": video_config_to_json(&config),
    }))
}

/// Create a slideshow video from multiple images.
async fn action_slideshow(args: Value) -> Result<Value> {
    let images = resolve_slideshow_images(&args).await?;

    let mut audio_path = None;
    if let Some(s) = parse_nonempty_str_arg(&args, "audio_path") {
        audio_path = Some(ensure_local_file(clean_path(s)).await?);
    }
    let slide_duration_secs = parse_f64_arg(&args, "slide_duration_secs")?.unwrap_or(5.0);

    let (preset, config) = build_video_config(&args)?;

    let output = resolve_video_output(&args, &config)?;
    let output_owned = output.clone();
    let config_for_run = config.clone();

    let images_count = images.len();
    tokio::task::spawn_blocking(move || {
        image_processor::video::slideshow(
            &images,
            std::time::Duration::from_secs_f64(slide_duration_secs),
            audio_path.as_deref(),
            image_processor::video::Transition::None,
            &output_owned,
            &config_for_run,
        )
    })
    .await
    .context("Slideshow task panicked")?
    .map_err(|e| anyhow::anyhow!("Slideshow creation failed: {}", e))?;

    Ok(json!({
        "status": "ok",
        "output": output,
        "image_count": images_count,
        "preset": preset,
        "config": video_config_to_json(&config),
    }))
}

async fn resolve_slideshow_images(args: &Value) -> Result<Vec<String>> {
    let has_explicit_images = args
        .get("images")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .any(|v| v.as_str().map(|s| !s.trim().is_empty()).unwrap_or(false))
        })
        .unwrap_or(false);

    let should_use_folder = matches!(
        parse_nonempty_str_arg(args, "slideshow_image_source"),
        Some("folder")
    ) || (!has_explicit_images
        && parse_nonempty_str_arg(args, "image_folder").is_some());

    if should_use_folder {
        let folder = parse_nonempty_str_arg(args, "image_folder").unwrap_or(".");
        let images = collect_image_files_from_folder(folder)?;
        if images.is_empty() {
            anyhow::bail!(
                "No images were found in the selected slideshow folder: {}",
                folder
            );
        }
        return Ok(images);
    }

    let raw_images = args["images"]
        .as_array()
        .context("'images' must be an array of file paths or URLs")?;

    let mut images = Vec::new();
    for v in raw_images {
        if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            images.push(ensure_local_file(clean_path(s)).await?);
        }
    }

    if images.is_empty() {
        anyhow::bail!(
            "No slideshow images were provided. Select a folder or choose upstream images."
        );
    }

    Ok(images)
}

async fn apply_overlay_image(
    pipe: ImagePipeline,
    overlay_image_path: &str,
    position: Gravity,
    _base_w: u32,
    _base_h: u32,
    custom_w: Option<u32>,
    custom_h: Option<u32>,
    margin_x: u32,
    margin_y: u32,
) -> Result<ImagePipeline> {
    let overlay_local = ensure_local_file(clean_path(overlay_image_path)).await?;
    let overlay_img = load_image_robust(&overlay_local)
        .with_context(|| format!("Failed to open overlay image: {}", overlay_image_path))?;
    let max_w = custom_w.unwrap_or(50);
    let max_h = custom_h.unwrap_or(50);
    // Use resize_exact if custom sizes are provided to enforce exact bounds, else use thumbnail
    let resized = if custom_w.is_some() || custom_h.is_some() {
        overlay_img.resize_exact(max_w, max_h, image::imageops::FilterType::Lanczos3)
    } else {
        overlay_img.thumbnail(max_w, max_h)
    };

    Ok(pipe.overlay_gravity(&resized, position, margin_x, margin_y))
}

fn apply_additional_text_overlays(
    mut pipe: ImagePipeline,
    args: &Value,
    font: &LoadedFont,
    default_color: [u8; 4],
    base_w: u32,
    base_h: u32,
    scale_factor: f32,
    overlay_icon: Option<&(image::DynamicImage, Gravity)>,
) -> Result<ImagePipeline> {
    let global_margin = ((base_w.min(base_h) as f32) * 0.04).max(24.0) as u32;
    let gap = (global_margin / 3).max(8);
    let transparent = image::Rgba([0u8, 0, 0, 0]);

    for item in collection_entries(args, "additional_texts") {
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if text.is_empty() {
            continue;
        }

        let custom_margin_x = parse_u32_arg(item, "margin_x")?.unwrap_or(global_margin);
        let custom_margin_y = parse_u32_arg(item, "margin_y")?.unwrap_or(global_margin);

        let position = parse_gravity_anchor(
            item.get("position")
                .and_then(Value::as_str)
                .unwrap_or("bottom-left"),
        );
        let region_w = match position {
            Gravity::North | Gravity::South => ((base_w as f32) * 0.72).max(180.0) as u32,
            _ => ((base_w as f32) * 0.42).max(140.0) as u32,
        };
        let region_h = ((base_h as f32) * 0.18).max(64.0) as u32;
        let seed_size = (((base_h as f32) * 0.08) * scale_factor.clamp(0.7, 1.4)).clamp(18.0, 84.0);

        // Check if overlay icon is at same position — if so, shrink text region and stitch
        let icon_share = overlay_icon.filter(|(_, icon_grav)| {
            std::mem::discriminant(icon_grav) == std::mem::discriminant(&position)
        });

        let effective_region_w = if let Some((icon_img, _)) = icon_share {
            region_w.saturating_sub(icon_img.width() + gap)
        } else {
            region_w
        };

        let requested_size = parse_f32_value(item.get("font_size"))?;

        let fitted_size = if let Some(custom_size) = requested_size {
            // User requested an explicit size, let's honor it by giving a massive bounding box
            // so `auto_font_size` won't artificially clamp it unless it's larger than the whole image
            image_processor::text::auto_font_size(
                text,
                font,
                (base_w as f32) * 0.95, // Allow up to 95% of image width
                (base_h as f32) * 0.95, // Allow up to 95% of image height
                custom_size,
                10.0,
                1.15,
            )
        } else {
            // Unspecified size, intelligently restrict to aesthetic corners
            image_processor::text::auto_font_size(
                text,
                font,
                effective_region_w as f32,
                region_h as f32,
                seed_size,
                10.0,
                1.15,
            )
        };

        let style = TextStyle {
            size: fitted_size,
            color: parse_optional_color4_value(item.get("font_color")).unwrap_or(default_color),
            alignment: parse_alignment(
                item.get("alignment")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .or(Some("left")),
            ),
            shadow: None,
            line_height: 1.15,
            ..Default::default()
        };
        let text_img = image::DynamicImage::ImageRgba8(image_processor::text::text_to_image(
            text,
            font,
            &style,
            effective_region_w,
        ));

        // If icon at same anchor: stack [icon | gap | text] horizontally, place as combined
        let composite_dyn: image::DynamicImage = if let Some((icon_img, _)) = icon_share {
            image_processor::composite::stack_horizontal(icon_img, &text_img, gap, transparent)
        } else {
            text_img
        };
        pipe = pipe.overlay_gravity(&composite_dyn, position, custom_margin_x, custom_margin_y);
    }

    Ok(pipe)
}

fn build_video_config(args: &Value) -> Result<(String, VideoConfig)> {
    let preset = parse_nonempty_str_arg(args, "preset").unwrap_or("workflow_default");
    let mut config = match preset {
        "workflow_default" => VideoConfig::workflow_default(),
        "social_media" => VideoConfig::social_media(),
        "instagram_reel" => VideoConfig::instagram_reel(),
        "high_quality" => VideoConfig::high_quality(),
        "web_stream" => VideoConfig::web_stream(),
        other => anyhow::bail!(
            "Unknown video preset '{}'. Valid presets: workflow_default, social_media, instagram_reel, high_quality, web_stream",
            other
        ),
    };

    apply_video_overrides(args, &mut config)?;
    Ok((preset.to_string(), config))
}

fn apply_video_overrides(args: &Value, config: &mut VideoConfig) -> Result<()> {
    if let Some(v) = parse_nonempty_str_arg(args, "video_codec") {
        config.video_codec = parse_video_codec(v)?;
    }
    if let Some(v) = parse_nonempty_str_arg(args, "audio_codec") {
        config.audio_codec = parse_audio_codec(v)?;
    }
    if let Some(v) = parse_nonempty_str_arg(args, "container") {
        config.container = parse_video_container(v)?;
    }
    if let Some(v) = parse_nonempty_str_arg(args, "pixel_format") {
        config.pixel_format = parse_pixel_format(v)?;
    }
    if let Some(v) = parse_nonempty_str_arg(args, "encoder_preset") {
        config.preset = v.to_string();
    }

    if let Some(v) = parse_u32_arg(args, "fps")? {
        if v == 0 {
            anyhow::bail!("'fps' must be greater than 0");
        }
        config.fps = v;
    }
    if let Some(v) = parse_u32_arg(args, "crf")? {
        config.crf = v;
    }
    if let Some(v) = parse_u32_arg(args, "keyframe_interval")? {
        if v == 0 {
            anyhow::bail!("'keyframe_interval' must be greater than 0");
        }
        config.keyframe_interval = Some(v);
    }

    if let Some(v) = parse_optional_string_override(args, "video_bitrate") {
        config.video_bitrate = v;
    }
    if let Some(v) = parse_nonempty_str_arg(args, "audio_bitrate") {
        config.audio_bitrate = v.to_string();
    }
    if let Some(v) = parse_optional_string_override(args, "max_bitrate") {
        config.max_bitrate = v;
    }
    if let Some(v) = parse_optional_string_override(args, "buf_size") {
        config.buf_size = v;
    }

    if let Some(resolution) = parse_nonempty_str_arg(args, "target_resolution") {
        match resolution.to_ascii_lowercase().as_str() {
            "custom" => {}
            "source" | "original" | "none" => {
                config.target_resolution = None;
            }
            _ => {
                config.target_resolution = Some(parse_resolution_pair(resolution)?);
            }
        }
    }

    let target_width = parse_u32_arg(args, "target_width")?.or(parse_u32_arg(args, "width")?);
    let target_height = parse_u32_arg(args, "target_height")?.or(parse_u32_arg(args, "height")?);

    match (target_width, target_height) {
        (Some(w), Some(h)) => {
            if w == 0 || h == 0 {
                anyhow::bail!("'target_width' and 'target_height' must both be greater than 0");
            }
            config.target_resolution = Some((w, h));
        }
        (Some(_), None) | (None, Some(_)) => {
            anyhow::bail!("Provide both width and height when overriding target resolution");
        }
        _ => {}
    }

    Ok(())
}

fn parse_nonempty_str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn parse_optional_string_override(args: &Value, key: &str) -> Option<Option<String>> {
    let value = parse_nonempty_str_arg(args, key)?;
    let normalized = value.to_ascii_lowercase();
    if matches!(normalized.as_str(), "none" | "auto" | "default" | "preset") {
        Some(None)
    } else {
        Some(Some(value.to_string()))
    }
}

fn parse_u32_arg(args: &Value, key: &str) -> Result<Option<u32>> {
    let Some(raw) = args.get(key) else {
        return Ok(None);
    };

    if raw.is_null() {
        return Ok(None);
    }

    if let Some(n) = raw.as_u64() {
        let converted = u32::try_from(n)
            .with_context(|| format!("'{}' is too large (max {})", key, u32::MAX))?;
        return Ok(Some(converted));
    }

    if let Some(n) = raw.as_f64() {
        if n.is_finite() && n >= 0.0 && (n.fract() == 0.0) {
            if n > u32::MAX as f64 {
                anyhow::bail!("'{}' is too large (max {})", key, u32::MAX);
            }
            return Ok(Some(n as u32));
        }
        anyhow::bail!("'{}' must be an integer value", key);
    }

    if let Some(s) = raw.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let parsed = trimmed
            .parse::<u32>()
            .with_context(|| format!("'{}' must be an integer value", key))?;
        return Ok(Some(parsed));
    }

    Ok(None)
}

fn parse_resolution_pair(value: &str) -> Result<(u32, u32)> {
    let trimmed = value.trim();
    let Some(idx) = trimmed.find('x').or_else(|| trimmed.find('X')) else {
        anyhow::bail!(
            "Invalid target_resolution '{}'. Expected format like '1280x720'",
            value
        );
    };

    let (w_raw, h_raw_with_sep) = trimmed.split_at(idx);
    let h_raw = &h_raw_with_sep[1..];
    let width = w_raw
        .trim()
        .parse::<u32>()
        .with_context(|| format!("Invalid target_resolution width '{}'", w_raw.trim()))?;
    let height = h_raw
        .trim()
        .parse::<u32>()
        .with_context(|| format!("Invalid target_resolution height '{}'", h_raw.trim()))?;

    if width == 0 || height == 0 {
        anyhow::bail!("target_resolution width/height must be greater than 0");
    }

    Ok((width, height))
}

fn parse_video_codec(value: &str) -> Result<VideoCodec> {
    match value.trim().to_ascii_lowercase().as_str() {
        "h264" => Ok(VideoCodec::H264),
        "h265" => Ok(VideoCodec::H265),
        "vp9" => Ok(VideoCodec::VP9),
        "copy" => Ok(VideoCodec::Copy),
        _ => anyhow::bail!(
            "Unknown video_codec '{}'. Valid values: h264, h265, vp9, copy",
            value
        ),
    }
}

fn parse_audio_codec(value: &str) -> Result<AudioCodec> {
    match value.trim().to_ascii_lowercase().as_str() {
        "aac" => Ok(AudioCodec::Aac),
        "mp3" => Ok(AudioCodec::Mp3),
        "opus" => Ok(AudioCodec::Opus),
        "copy" => Ok(AudioCodec::Copy),
        _ => anyhow::bail!(
            "Unknown audio_codec '{}'. Valid values: aac, mp3, opus, copy",
            value
        ),
    }
}

fn parse_video_container(value: &str) -> Result<VideoContainer> {
    match value.trim().to_ascii_lowercase().as_str() {
        "mp4" => Ok(VideoContainer::Mp4),
        "webm" => Ok(VideoContainer::WebM),
        "mkv" => Ok(VideoContainer::Mkv),
        "mov" => Ok(VideoContainer::Mov),
        _ => anyhow::bail!(
            "Unknown container '{}'. Valid values: mp4, webm, mkv, mov",
            value
        ),
    }
}

fn parse_pixel_format(value: &str) -> Result<PixelFormat> {
    match value.trim().to_ascii_lowercase().as_str() {
        "yuv420p" => Ok(PixelFormat::Yuv420p),
        "yuv444p" => Ok(PixelFormat::Yuv444p),
        "yuva420p" => Ok(PixelFormat::Yuva420p),
        _ => anyhow::bail!(
            "Unknown pixel_format '{}'. Valid values: yuv420p, yuv444p, yuva420p",
            value
        ),
    }
}

fn video_config_to_json(config: &VideoConfig) -> Value {
    let video_codec = match config.video_codec {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
        VideoCodec::VP9 => "vp9",
        VideoCodec::Copy => "copy",
    };
    let audio_codec = match config.audio_codec {
        AudioCodec::Aac => "aac",
        AudioCodec::Mp3 => "mp3",
        AudioCodec::Opus => "opus",
        AudioCodec::Copy => "copy",
    };
    let container = match config.container {
        VideoContainer::Mp4 => "mp4",
        VideoContainer::WebM => "webm",
        VideoContainer::Mkv => "mkv",
        VideoContainer::Mov => "mov",
    };
    let pixel_format = match config.pixel_format {
        PixelFormat::Yuv420p => "yuv420p",
        PixelFormat::Yuv444p => "yuv444p",
        PixelFormat::Yuva420p => "yuva420p",
    };

    json!({
        "video_codec": video_codec,
        "audio_codec": audio_codec,
        "container": container,
        "fps": config.fps,
        "video_bitrate": config.video_bitrate.clone(),
        "audio_bitrate": config.audio_bitrate.clone(),
        "pixel_format": pixel_format,
        "encoder_preset": config.preset.clone(),
        "crf": config.crf,
        "target_resolution": config.target_resolution.map(|(w, h)| format!("{}x{}", w, h)),
        "keyframe_interval": config.keyframe_interval,
        "max_bitrate": config.max_bitrate.clone(),
        "buf_size": config.buf_size.clone(),
    })
}

/// Resolve the output path for video/slideshow actions.
///
/// Reads `output_filename` (or falls back to legacy `output`) from args,
/// defaults to `"output"` if neither is set.  Saves into `data/files/`.
/// Auto-appends the correct file extension (`.mp4`, `.webm`, Ã¢â‚¬Â¦) based on
/// the `VideoConfig` container if the filename does not already have one.
fn resolve_video_output(
    args: &Value,
    config: &image_processor::video::VideoConfig,
) -> Result<String> {
    let raw_name = args["output_filename"]
        .as_str()
        .or_else(|| args["output"].as_str()); // legacy fallback
    let expected_ext = config.container.extension(); // e.g. "mp4"
    let filename = normalize_video_output_filename(raw_name, expected_ext);

    let dir = app_data_files_dir().context(
        "Could not locate data/files directory. Set AXON_DATA_DIR or ensure data/files/ exists.",
    )?;

    Ok(dir.join(&filename).to_string_lossy().into_owned())
}
fn normalize_video_output_filename(raw_name: Option<&str>, expected_ext: &str) -> String {
    let raw_name = raw_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("output");

    // Strip any directory separators: we only accept a filename, not a path.
    let mut filename = raw_name
        .rsplit(&['/', '\\'][..])
        .next()
        .unwrap_or("output")
        .trim()
        .to_string();

    // Guard against extension-only or otherwise unusable names that can lead
    // to hidden files like ".mp4" in data/files.
    let is_extension_only =
        filename.starts_with('.') && filename.len() > 1 && !filename[1..].contains('.');
    if filename.is_empty() || filename == "." || filename == ".." || is_extension_only {
        filename = "output".to_string();
    }

    // Auto-append the expected extension if missing (or unsupported).
    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let has_supported_ext = matches!(
        ext.as_deref(),
        Some("mp4") | Some("webm") | Some("mkv") | Some("mov")
    );

    if has_supported_ext {
        filename
    } else {
        format!("{}.{}", filename.trim_end_matches('.'), expected_ext)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_video_output_filename;

    #[test]
    fn normalize_video_output_defaults_for_empty_value() {
        let name = normalize_video_output_filename(Some("   "), "mp4");
        assert_eq!(name, "output.mp4");
    }

    #[test]
    fn normalize_video_output_defaults_for_extension_only_value() {
        let name = normalize_video_output_filename(Some(".mp4"), "mp4");
        assert_eq!(name, "output.mp4");
    }

    #[test]
    fn normalize_video_output_trims_directory_segments() {
        let name = normalize_video_output_filename(Some("nested/path/reel"), "mp4");
        assert_eq!(name, "reel.mp4");
    }

    #[test]
    fn normalize_video_output_keeps_supported_extension() {
        let name = normalize_video_output_filename(Some("reel.mov"), "mp4");
        assert_eq!(name, "reel.mov");
    }
}
/// Helper to download HTTP URLs to a local temporary file before FFmpeg processing
async fn ensure_local_file(mut path: String) -> Result<String> {
    if path.starts_with("http://") || path.starts_with("https://") {
        let response = reqwest::get(&path)
            .await
            .with_context(|| format!("Failed to download {}", path))?;
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("Failed to read {}", path))?;

        let ext = path
            .split('/')
            .last()
            .unwrap_or("")
            .split('.')
            .last()
            .unwrap_or("tmp")
            .split('?')
            .next()
            .unwrap_or("tmp");
        let tmp_dir = std::env::temp_dir();
        let fname = format!(
            "axon_dl_{}_{}.{}",
            std::process::id(),
            uuid::Uuid::new_v4().as_simple(),
            ext
        );
        let tmp_path = tmp_dir.join(fname);

        tokio::fs::write(&tmp_path, bytes).await?;
        path = tmp_path.to_string_lossy().into_owned();
    }
    Ok(path)
}

// Ã¢â€â‚¬Ã¢â€â‚¬ Pipeline step dispatch Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬

fn apply_op(pipe: ImagePipeline, op: &str, step: &Value) -> Result<ImagePipeline> {
    Ok(match op {
        "resize" => pipe.resize(
            step["width"].as_u64().context("resize needs 'width'")? as u32,
            step["height"].as_u64().context("resize needs 'height'")? as u32,
        ),
        "resize_fit" => pipe.resize_fit(
            step["max_width"]
                .as_u64()
                .context("resize_fit needs 'max_width'")? as u32,
            step["max_height"]
                .as_u64()
                .context("resize_fit needs 'max_height'")? as u32,
        ),
        "resize_fill" => pipe.resize_fill(
            step["width"]
                .as_u64()
                .context("resize_fill needs 'width'")? as u32,
            step["height"]
                .as_u64()
                .context("resize_fill needs 'height'")? as u32,
        ),
        "crop" => pipe.crop(
            step["x"].as_u64().unwrap_or(0) as u32,
            step["y"].as_u64().unwrap_or(0) as u32,
            step["width"].as_u64().context("crop needs 'width'")? as u32,
            step["height"].as_u64().context("crop needs 'height'")? as u32,
        )?,
        "crop_center" => pipe.crop_center(
            step["width"]
                .as_u64()
                .context("crop_center needs 'width'")? as u32,
            step["height"]
                .as_u64()
                .context("crop_center needs 'height'")? as u32,
        )?,
        "pad" => pipe.pad(
            step["padding"].as_u64().unwrap_or(20) as u32,
            parse_color4(step["color"].as_str()),
        ),
        "rotate" => pipe.rotate(step["degrees"].as_f64().unwrap_or(90.0) as f32)?,
        "flip_horizontal" => pipe.flip_horizontal(),
        "flip_vertical" => pipe.flip_vertical(),
        "blur" => pipe.blur(step["sigma"].as_f64().unwrap_or(3.0) as f32),
        "sharpen" => pipe.sharpen(
            step["sigma"].as_f64().unwrap_or(3.0) as f32,
            step["threshold"].as_i64().unwrap_or(1) as i32,
        ),
        "brightness" => pipe.brightness(step["value"].as_i64().unwrap_or(20) as i32),
        "contrast" => pipe.contrast(step["value"].as_f64().unwrap_or(20.0) as f32),
        "grayscale" => pipe.grayscale(),
        "sepia" => pipe.sepia(),
        "invert" => pipe.invert(),
        "saturation" => pipe.saturation(step["factor"].as_f64().unwrap_or(1.5) as f32),
        "hue_rotate" => pipe.hue_rotate(step["degrees"].as_f64().unwrap_or(90.0) as f32),
        "vignette" => pipe.vignette(step["strength"].as_f64().unwrap_or(0.6) as f32),
        "color_overlay" => pipe.color_overlay(
            parse_color3(step["color"].as_str()),
            step["opacity"].as_f64().unwrap_or(0.3) as f32,
        ),
        "gradient_overlay" => pipe.gradient_overlay(
            parse_color4(step["color_start"].as_str()),
            parse_color4(step["color_end"].as_str()),
            parse_gradient_dir(step["direction"].as_str()),
        ),
        "rounded_corners" => pipe.rounded_corners(step["radius"].as_u64().unwrap_or(20) as u32),
        other => anyhow::bail!("Unknown pipeline op: '{}'", other),
    })
}

// Ã¢â€â‚¬Ã¢â€â‚¬ Helpers Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .with_context(|| format!("Missing required string parameter: '{}'", key))
}

fn load_font_with_fallback(primary_path: &str) -> Result<LoadedFont> {
    if let Ok(font) = LoadedFont::from_path(primary_path) {
        return Ok(font);
    }

    let fallbacks = [
        "C:\\Windows\\Fonts\\arial.ttf",
        "C:\\Windows\\Fonts\\micross.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ];

    for fallback in fallbacks {
        if let Ok(font) = LoadedFont::from_path(fallback) {
            tracing::info!(
                "Using fallback font for image_tool. requested='{}' fallback='{}'",
                primary_path,
                fallback
            );
            return Ok(font);
        }
    }

    anyhow::bail!(
        "Failed to load font '{}' and no system fallbacks were found",
        primary_path
    )
}

/// Locate the app's `data/files` staging directory at runtime.
///
/// Discovery order:
///   1. `AXON_DATA_DIR` env-var  (set this at deploy time, e.g. `/opt/axon/data`)
///   2. Walk up from the running executable looking for a `data/files` sub-dir
///      (covers `target/release/axon` -> `data/files` three levels up)
///   3. Walk up from the current working directory
///
/// Returns `None` only if none of the above locations exist on disk.
pub fn app_data_files_dir() -> Option<std::path::PathBuf> {
    // 1. Explicit env var
    if let Ok(dir) = std::env::var("AXON_DATA_DIR") {
        let base = std::path::PathBuf::from(dir);
        // Accept either  $AXON_DATA_DIR/files  or  $AXON_DATA_DIR  itself
        for candidate in [base.join("files"), base.clone()] {
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }

    // 2. Walk up from the executable (works for both dev and prod layouts)
    if let Ok(exe) = std::env::current_exe() {
        // skip(1) to start from the exe's parent, check up to 5 ancestors
        for ancestor in exe.ancestors().skip(1).take(5) {
            let p = ancestor.join("data").join("files");
            if p.is_dir() {
                return Some(p);
            }
        }
    }

    // 3. Walk up from cwd (useful when running with `cargo run`)
    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors().take(5) {
            let p = ancestor.join("data").join("files");
            if p.is_dir() {
                return Some(p);
            }
        }
    }

    None
}

/// Helper for recursive font discovery
fn collect_fonts_recursively(dir: &std::path::Path, prefix: &str, fonts: &mut Vec<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let dir_name = entry.file_name().to_string_lossy().to_string();
                let new_prefix = if prefix.is_empty() {
                    dir_name
                } else {
                    format!("{}/{}", prefix, dir_name)
                };
                collect_fonts_recursively(&path, &new_prefix, fonts);
            } else if let Ok(name) = entry.file_name().into_string() {
                let lower = name.to_lowercase();
                if lower.ends_with(".ttf") || lower.ends_with(".otf") {
                    let full_name = if prefix.is_empty() {
                        name
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    fonts.push(full_name);
                }
            }
        }
    }
}

/// Dynamically list all available fonts in the data/files/fonts dir for the dropdown menu
pub fn discover_fonts() -> Option<Vec<String>> {
    let base = app_data_files_dir()?;
    let fonts_dir = base.join("fonts");
    let mut fonts = Vec::new();

    collect_fonts_recursively(&fonts_dir, "", &mut fonts);

    if fonts.is_empty() {
        None
    } else {
        Some(fonts)
    }
}

/// Normalize a file path and produce every variant worth trying, in priority order.
///
/// In addition to the standard path normalizations (strip `\\?\`, flip slashes),
/// this also resolves the *filename component* against the app's `data/files`
/// directory.  That way callers can pass the full absolute path they have stored
/// (which may be a Windows legacy path on a Linux deploy, or vice-versa) and the
/// file will still be found as long as it exists in the local staging folder.
fn path_candidates(s: &str) -> Vec<String> {
    // Strip surrounding whitespace and any literal quote characters that the UI
    // may have included (e.g. "C:\..." typed instead of C:\...).
    let s = s.trim().trim_matches('"').trim_matches('\'').trim();
    let mut candidates: Vec<String> = Vec::new();

    // Ã¢â€â‚¬Ã¢â€â‚¬ 1. Exact path as given Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
    candidates.push(s.to_string());

    // Ã¢â€â‚¬Ã¢â€â‚¬ 2. Windows \\?\ extended-path handling Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
    if let Some(stripped) = s.strip_prefix("\\\\?\\") {
        candidates.push(stripped.to_string());
        candidates.push(stripped.replace('\\', "/"));
    } else {
        if s.len() > 2 && s.chars().nth(1) == Some(':') {
            // Bare Windows absolute path Ã¢â‚¬â€ also try with the \\?\ prefix
            candidates.push(format!("\\\\?\\{}", s));
        }
        candidates.push(s.replace('\\', "/"));
    }

    // Ã¢â€â‚¬Ã¢â€â‚¬ 3. Resolve filename against the app's data/files dir Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
    // This is the key fallback: whatever stale/absolute/cross-OS path was
    // stored, if the file exists in our local staging folder we'll find it.
    if let Some(data_dir) = app_data_files_dir() {
        // Try the bare filename (UUID_original.jpg)
        if let Some(fname) = std::path::Path::new(&s.replace('\\', "/")).file_name() {
            candidates.push(data_dir.join(fname).to_string_lossy().into_owned());
            // Also try looking in the fonts sub-directory
            candidates.push(
                data_dir
                    .join("fonts")
                    .join(fname)
                    .to_string_lossy()
                    .into_owned(),
            );
        }

        // Also try any sub-path after a `data/files/` segment, in case the
        // stored path has the right tail but a wrong root.
        let normalised = s.replace('\\', "/");

        // Ã¢â€â‚¬Ã¢â€â‚¬ 4. Try prefixing with fonts/ (Handles recursive dropdowns) Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
        candidates.push(
            data_dir
                .join("fonts")
                .join(&normalised)
                .to_string_lossy()
                .into_owned(),
        );

        for marker in ["data/files/", "data\\files\\"] {
            if let Some(idx) = normalised.find(marker) {
                let tail = &normalised[idx + marker.len()..];
                if !tail.is_empty() {
                    candidates.push(data_dir.join(tail).to_string_lossy().into_owned());
                }
                break;
            }
        }
    }

    // Deduplicate while preserving order
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert(c.clone()));
    candidates
}

/// Convenience wrapper: return the first candidate path (for output paths).
fn clean_path(s: &str) -> String {
    path_candidates(s)
        .into_iter()
        .next()
        .unwrap_or_else(|| s.trim().to_string())
}

/// Find the first generated path candidate that actually maps to an existing file.
fn resolve_existing_path(s: &str) -> Option<String> {
    path_candidates(s)
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
}

/// Try to open an image from a file path, attempting multiple path variants.
///
/// Strategy (in order, per path candidate):
///   1. `std::fs::read()` Ã¢â€ â€™ `image::load_from_memory()` Ã¢â‚¬â€ reads raw bytes first,
///      then sniffs format from magic bytes. This is the most reliable approach for
///      Windows `\\?\` extended paths, OneDrive paths, and extension-less filenames
///      because it bypasses `ImageReader`'s file-extension hints entirely.
///   2. `ImageReader::open()` Ã¢â€ â€™ `with_guessed_format()` Ã¢â€ â€™ `decode()` Ã¢â‚¬â€ kept as a
///      secondary fallback in case the file handle approach works where read() doesn't.
fn load_image_robust(raw_path: &str) -> Result<image::DynamicImage> {
    let candidates = path_candidates(raw_path);
    let mut errors: Vec<String> = Vec::new();

    for candidate in &candidates {
        // Ã¢â€â‚¬Ã¢â€â‚¬ Primary: read bytes Ã¢â€ â€™ decode from memory Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
        // More robust than ImageReader::open() for \\?\ paths and unusual filenames.
        match std::fs::read(candidate) {
            Ok(bytes) => match image::load_from_memory(&bytes) {
                Ok(img) => return Ok(img),
                Err(e) => errors.push(format!(
                    "  '{}' [read+memory]: bytes read OK but decode failed: {}",
                    candidate, e
                )),
            },
            Err(read_err) => {
                // Ã¢â€â‚¬Ã¢â€â‚¬ Secondary: ImageReader fallback Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬Ã¢â€â‚¬
                match image::ImageReader::open(candidate) {
                    Ok(reader) => match reader.with_guessed_format() {
                        Ok(r) => match r.decode() {
                            Ok(img) => return Ok(img),
                            Err(e) => errors.push(format!(
                                "  '{}' [fs::read: {}] [ImageReader decode]: {}",
                                candidate, read_err, e
                            )),
                        },
                        Err(e) => errors.push(format!(
                            "  '{}' [fs::read: {}] [with_guessed_format]: {}",
                            candidate, read_err, e
                        )),
                    },
                    Err(open_err) => errors.push(format!(
                        "  '{}' [fs::read: {}] [open: {}]",
                        candidate, read_err, open_err
                    )),
                }
            }
        }
    }

    anyhow::bail!(
        "Failed to open image from path '{}'. Tried {} variant(s):\n{}",
        raw_path.trim(),
        errors.len(),
        errors.join("\n")
    )
}

/// Decode a base64 string (with optional data-URI prefix) into an image.
fn decode_base64_image(b64: &str) -> Result<image::DynamicImage> {
    let b64_clean = b64.find(',').map_or(b64, |i| &b64[i + 1..]);
    let bytes = BASE64
        .decode(b64_clean.trim())
        .context("Not valid base64")?;
    image::load_from_memory(&bytes).context("Failed to decode image from base64 bytes")
}

/// Return true if `s` looks like raw base64 image data rather than a file path.
/// Checks the leading magic-byte patterns once base64-decoded.
fn looks_like_base64_image(s: &str) -> bool {
    // Common base64 prefixes for image formats:
    //   JPEG  Ã¢â€ â€™ /9j/
    //   PNG   Ã¢â€ â€™ iVBORw
    //   GIF   Ã¢â€ â€™ R0lGOD
    //   WebP  Ã¢â€ â€™ UklGR
    //   BMP   Ã¢â€ â€™ Qk0
    //   TIFF  Ã¢â€ â€™ SUk   (little-endian) / TU0 (big-endian)
    let s = s
        .trim_start_matches("data:image/")
        .find(',')
        .map_or(s, |_| s);
    s.starts_with("/9j/")
        || s.starts_with("iVBOR")
        || s.starts_with("R0lGOD")
        || s.starts_with("UklGR")
        || s.starts_with("Qk0")
        || s.starts_with("SUk")
        || s.starts_with("TU0")
        || s.starts_with("data:image/")
}

/// Load an image from the tool args.
///
/// Resolution order:
///   1. `input_binary`           Ã¢â‚¬â€œ explicit base64 string (data-URI prefix optional)
///   2. `binary.body`            Ã¢â‚¬â€œ base64 embedded in the HTTP node's binary object
///                                 (map: `$node["Synapse 1"].data.binary` Ã¢â€ â€™ `binary`)
///   3. `binary.local_path`      Ã¢â‚¬â€œ file path from the HTTP node's binary object;
///                                 read via `std::fs::read()` for maximum compatibility
///                                 with `\\?\` extended paths
///   4. `input`                  Ã¢â‚¬â€œ explicit file path string; if it looks like base64
///                                 data (accidentally piped from body) it is decoded
///                                 in-memory instead of treated as a path
fn load_image_from_args(args: &Value) -> Result<image::DynamicImage> {
    // 1. Explicit base64 field
    if let Some(b64) = args["input_binary"].as_str().filter(|s| !s.is_empty()) {
        return decode_base64_image(b64).context("input_binary: base64 decode failed");
    }

    // 2 & 3. Full binary object passed in as `binary`
    //   Workflow: map $node["Synapse 1"].data.binary Ã¢â€ â€™ `binary`
    if let Some(bin_obj) = args["binary"].as_object() {
        // Prefer in-memory base64 body (no filesystem required)
        if let Some(b64) = bin_obj
            .get("body")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if looks_like_base64_image(b64) {
                return decode_base64_image(b64).context("binary.body: base64 decode failed");
            }
        }
        // Fallback to local_path in the binary object
        if let Some(lp) = bin_obj
            .get("local_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return load_image_robust(lp)
                .with_context(|| format!("binary.local_path '{}': failed to read file", lp));
        }
    }

    // 4. Explicit `input` field Ã¢â‚¬â€ accept either a file path or accidentally-pasted base64
    let input = args["input"]
        .as_str()
        .context("No image source provided. Supply one of: 'input_binary' (base64), 'binary' (HTTP binary object), or 'input' (file path).")?;

    if looks_like_base64_image(input) {
        // User mapped binary body to the file-path field Ã¢â‚¬â€ handle it gracefully
        return decode_base64_image(input)
            .context("'input' field contained base64 data; decode failed");
    }

    load_image_robust(input)
}

/// Parse "#rrggbbaa" or "#rrggbb" to [u8; 4].
fn parse_color4(s: Option<&str>) -> [u8; 4] {
    let s = s.unwrap_or("#000000ff");
    let s = s.trim_start_matches('#');
    let r = u8::from_str_radix(&s.get(0..2).unwrap_or("00"), 16).unwrap_or(0);
    let g = u8::from_str_radix(&s.get(2..4).unwrap_or("00"), 16).unwrap_or(0);
    let b = u8::from_str_radix(&s.get(4..6).unwrap_or("00"), 16).unwrap_or(0);
    let a = u8::from_str_radix(&s.get(6..8).unwrap_or("ff"), 16).unwrap_or(255);
    [r, g, b, a]
}

/// Parse "#rrggbb" to [u8; 3].
fn parse_color3(s: Option<&str>) -> [u8; 3] {
    let c = parse_color4(s);
    [c[0], c[1], c[2]]
}

fn parse_format(s: Option<&str>) -> Option<OutputFormat> {
    match s {
        Some("png") => Some(OutputFormat::Png),
        Some("jpeg") | Some("jpg") => Some(OutputFormat::Jpeg),
        Some("webp") => Some(OutputFormat::WebP),
        _ => None,
    }
}

fn parse_gradient_dir(s: Option<&str>) -> GradientDirection {
    match s {
        Some("top_to_bottom") => GradientDirection::TopToBottom,
        Some("left_to_right") => GradientDirection::LeftToRight,
        Some("right_to_left") => GradientDirection::RightToLeft,
        _ => GradientDirection::BottomToTop,
    }
}

fn parse_alignment(s: Option<&str>) -> TextAlignment {
    match s {
        Some("left") => TextAlignment::Left,
        Some("right") => TextAlignment::Right,
        _ => TextAlignment::Center,
    }
}

/// Parse a floating-point (f64) argument from JSON args by key.
/// Accepts JSON numbers and numeric strings. Returns `Ok(None)` for missing/null/empty values.
fn parse_f64_arg(args: &Value, key: &str) -> Result<Option<f64>> {
    let Some(raw) = args.get(key) else {
        return Ok(None);
    };
    parse_f64_value(Some(raw))
}

/// Parse a floating-point (f64) value from an optional `&Value`.
/// Accepts JSON numbers and numeric strings. Returns `Ok(None)` for None/null/empty values.
fn parse_f64_value(raw: Option<&Value>) -> Result<Option<f64>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    if let Some(n) = raw.as_f64() {
        return Ok(Some(n));
    }
    if let Some(n) = raw.as_u64() {
        return Ok(Some(n as f64));
    }
    if let Some(n) = raw.as_i64() {
        return Ok(Some(n as f64));
    }
    if let Some(s) = raw.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let parsed = trimmed
            .parse::<f64>()
            .with_context(|| format!("Expected a numeric value, got '{}'", trimmed))?;
        return Ok(Some(parsed));
    }
    Ok(None)
}

/// Parse a floating-point argument from JSON args by key.
/// Accepts JSON numbers and numeric strings. Returns `Ok(None)` for missing/null/empty values.
fn parse_f32_arg(args: &Value, key: &str) -> Result<Option<f32>> {
    let Some(raw) = args.get(key) else {
        return Ok(None);
    };
    parse_f32_value(Some(raw))
}

/// Parse a floating-point value from an optional `&Value`.
/// Accepts JSON numbers and numeric strings. Returns `Ok(None)` for None/null/empty values.
fn parse_f32_value(raw: Option<&Value>) -> Result<Option<f32>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    if let Some(n) = raw.as_f64() {
        return Ok(Some(n as f32));
    }
    if let Some(n) = raw.as_u64() {
        return Ok(Some(n as f32));
    }
    if let Some(n) = raw.as_i64() {
        return Ok(Some(n as f32));
    }
    if let Some(s) = raw.as_str() {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let parsed = trimmed
            .parse::<f32>()
            .with_context(|| format!("Expected a numeric value, got '{}'", trimmed))?;
        return Ok(Some(parsed));
    }
    Ok(None)
}

/// Parse an optional RGBA color from JSON args by key.
/// Returns the parsed `[u8; 4]` if the key exists and is a non-empty color string,
/// otherwise returns `None` so callers can fall back to an auto-detected default.
fn parse_optional_color4_arg(args: &Value, key: &str) -> Option<[u8; 4]> {
    let s = args
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(parse_color4(Some(s)))
}

/// Parse an optional RGBA color from a single `&Value` reference.
/// Same logic as `parse_optional_color4_arg` but accepts a direct `Option<&Value>`.
fn parse_optional_color4_value(raw: Option<&Value>) -> Option<[u8; 4]> {
    let s = raw?.as_str().map(str::trim).filter(|s| !s.is_empty())?;
    Some(parse_color4(Some(s)))
}

/// Map a human-readable anchor string to a `Gravity` variant.
///
/// Accepts strings like `"top-left"`, `"bottom-center"`, `"top-right"`, etc.
/// Falls back to `Gravity::NorthEast` (top-right) for unrecognised input.
fn parse_gravity_anchor(s: &str) -> Gravity {
    match s.trim().to_ascii_lowercase().as_str() {
        "top-left" | "top_left" | "northwest" => Gravity::NorthWest,
        "top-center" | "top_center" | "top" | "north" => Gravity::North,
        "top-right" | "top_right" | "northeast" => Gravity::NorthEast,
        "bottom-left" | "bottom_left" | "southwest" => Gravity::SouthWest,
        "bottom-center" | "bottom_center" | "bottom" | "south" => Gravity::South,
        "bottom-right" | "bottom_right" | "southeast" => Gravity::SouthEast,
        "left" | "west" => Gravity::West,
        "right" | "east" => Gravity::East,
        "center" => Gravity::Center,
        _ => Gravity::NorthEast,
    }
}

/// Iterate over an optional JSON array field.
/// Returns an empty slice if the key is missing, null, or not an array.
fn collection_entries<'a>(args: &'a Value, key: &str) -> &'a [Value] {
    if let Some(val) = args.get(key) {
        if let Some(arr) = val.as_array() {
            return arr.as_slice();
        }
        if let Some(obj) = val.as_object() {
            if let Some(params) = obj.get("parameters").and_then(Value::as_array) {
                return params.as_slice();
            }
        }
    }
    &[]
}

/// Collect all image files from a folder path (relative to `data/files/`).
/// Returns a sorted `Vec<String>` of absolute paths.
fn collect_image_files_from_folder(folder: &str) -> Result<Vec<String>> {
    let base = app_data_files_dir().context("Could not locate data/files directory.")?;
    let search_dir = if folder == "." || folder.is_empty() {
        base.clone()
    } else {
        base.join(folder)
    };

    if !search_dir.is_dir() {
        anyhow::bail!("Folder '{}' does not exist or is not a directory", folder);
    }

    let image_exts = ["jpg", "jpeg", "png", "webp", "bmp", "gif", "tiff", "tif"];
    let mut images = Vec::new();

    let mut dirs_to_visit = vec![search_dir.clone()];

    while let Some(current_dir) = dirs_to_visit.pop() {
        if let Ok(entries) = std::fs::read_dir(&current_dir) {
            for entry_result in entries {
                let Ok(entry) = entry_result else { continue };
                let path = entry.path();
                if path.is_dir() {
                    dirs_to_visit.push(path);
                } else if path.is_file() {
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.to_ascii_lowercase())
                        .unwrap_or_default();
                    if image_exts.contains(&ext.as_str()) {
                        images.push(path.to_string_lossy().into_owned());
                    }
                }
            }
        }
    }

    images.sort();
    Ok(images)
}

/// Discover sub-folders inside `data/files/` for the fovea node's folder dropdown.
/// Returns folder paths relative to `data/files/`.
pub fn discover_image_folders() -> Option<Vec<String>> {
    let base = app_data_files_dir()?;
    let mut folders = vec![".".to_string()]; // root folder

    fn walk(dir: &std::path::Path, prefix: &str, folders: &mut Vec<String>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Skip hidden directories and fonts
                    if name.starts_with('.') || name == "fonts" {
                        continue;
                    }
                    let rel = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{}", prefix, name)
                    };
                    folders.push(rel.clone());
                    walk(&path, &rel, folders);
                }
            }
        }
    }

    walk(&base, "", &mut folders);

    if folders.len() <= 1 {
        // Only "." — no actual sub-folders
        Some(folders)
    } else {
        Some(folders)
    }
}

/// Adaptive font size based on text length (mirrors quote_image example logic).
fn adaptive_font_sizes(char_count: usize, line_count: usize) -> (u32, u32) {
    if char_count < 50 && line_count < 2 {
        (96, 52)
    } else if char_count < 100 && line_count < 3 {
        (82, 44)
    } else if char_count < 150 && line_count < 4 {
        (72, 38)
    } else if char_count < 200 && line_count < 5 {
        (64, 34)
    } else if char_count < 250 && line_count < 6 {
        (56, 30)
    } else if char_count < 300 && line_count < 7 {
        (48, 26)
    } else if char_count < 400 && line_count < 9 {
        (40, 22)
    } else if char_count < 500 {
        (34, 20)
    } else {
        (28, 18)
    }
}
