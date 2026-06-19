//! Video generation from static images and audio.
//!
//! This module wraps FFmpeg via `std::process::Command` for maximum compatibility,
//! with rich Rust-typed configuration. FFmpeg must be installed on the system.
//!
//! For embedding into an agent binary without a system FFmpeg dependency, see
//! the `ffmpeg-sidecar` crate which bundles a static FFmpeg binary.

use std::process::Command;
use std::time::Duration;

use image::DynamicImage;

use crate::canvas;
use crate::error::{ImageProcessorError, Result};

// â”€â”€â”€ Constants â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Duration of the cross-dissolve overlap for `Transition::CrossFade`, in seconds.
const CROSSFADE_DURATION_SECS: f64 = 0.5;

// â”€â”€â”€ Configuration Types â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Video codec for output
#[derive(Debug, Clone, Copy)]
pub enum VideoCodec {
    /// H.264 â€” widest compatibility (iOS, Android, web, social media)
    H264,
    /// H.265 â€” ~50% smaller files, less compatible
    H265,
    /// VP9 â€” open codec, good for WebM/web. Uses `-quality`/`-speed`, not `-preset`.
    VP9,
    /// Copy existing video stream without re-encoding
    Copy,
}

impl VideoCodec {
    fn ffmpeg_name(&self) -> &str {
        match self {
            VideoCodec::H264 => "libx264",
            VideoCodec::H265 => "libx265",
            VideoCodec::VP9 => "libvpx-vp9",
            VideoCodec::Copy => "copy",
        }
    }
}

/// Audio codec for output
#[derive(Debug, Clone, Copy)]
pub enum AudioCodec {
    /// AAC â€” standard for MP4, widest compatibility
    Aac,
    /// MP3 â€” universal playback
    Mp3,
    /// Opus â€” best quality/size ratio, ideal for WebM
    Opus,
    /// Copy existing audio stream without re-encoding
    Copy,
}

impl AudioCodec {
    fn ffmpeg_name(&self) -> &str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::Mp3 => "libmp3lame",
            AudioCodec::Opus => "libopus",
            AudioCodec::Copy => "copy",
        }
    }
}

/// Container format for the output file
#[derive(Debug, Clone, Copy)]
pub enum VideoContainer {
    Mp4,
    WebM,
    Mkv,
    Mov,
}

impl VideoContainer {
    /// File extension (without leading dot) for this container.
    pub fn extension(&self) -> &str {
        match self {
            VideoContainer::Mp4 => "mp4",
            VideoContainer::WebM => "webm",
            VideoContainer::Mkv => "mkv",
            VideoContainer::Mov => "mov",
        }
    }

    /// Whether this container benefits from `-movflags +faststart`.
    fn needs_faststart(&self) -> bool {
        matches!(self, VideoContainer::Mp4 | VideoContainer::Mov)
    }
}

/// Pixel format â€” yuv420p is required for most players and platforms
#[derive(Debug, Clone, Copy)]
pub enum PixelFormat {
    /// Most compatible â€” required for H.264 on iOS/Android/social media
    Yuv420p,
    /// Higher quality, less compatible
    Yuv444p,
    /// With alpha channel (MOV/ProRes)
    Yuva420p,
}

impl PixelFormat {
    fn ffmpeg_name(&self) -> &str {
        match self {
            PixelFormat::Yuv420p => "yuv420p",
            PixelFormat::Yuv444p => "yuv444p",
            PixelFormat::Yuva420p => "yuva420p",
        }
    }
}

/// Audio fade in/out configuration
#[derive(Debug, Clone)]
pub struct AudioFade {
    /// Fade in duration at the start
    pub fade_in: Option<Duration>,
    /// Fade out duration at the end
    pub fade_out: Option<Duration>,
}

/// Transition type between slides in a slideshow
#[derive(Debug, Clone, Copy)]
pub enum Transition {
    /// Hard cut (no transition)
    None,
    /// Fade to black then fade in on each slide
    FadeBlack,
    /// Cross-dissolve between slides using FFmpeg's `xfade=dissolve` filter.
    /// Overlap duration is [`CROSSFADE_DURATION_SECS`].
    CrossFade,
}

/// Configuration for image-to-video conversion
#[derive(Debug, Clone)]
pub struct VideoConfig {
    /// Output video codec
    pub video_codec: VideoCodec,
    /// Output audio codec
    pub audio_codec: AudioCodec,
    /// Output container format
    pub container: VideoContainer,
    /// Frames per second (use 1 for static images â€” saves space)
    pub fps: u32,
    /// Average video bitrate (e.g., "2M", "500k") â€” `None` = let FFmpeg choose.
    /// For VBV capping, also set `max_bitrate` + `buf_size`.
    pub video_bitrate: Option<String>,
    /// Audio bitrate (e.g., "192k")
    pub audio_bitrate: String,
    /// Pixel format
    pub pixel_format: PixelFormat,
    /// Audio fade configuration
    pub audio_fade: Option<AudioFade>,
    /// H.264/H.265 encoding preset (ultrafast â€¦ veryslow). Ignored for VP9.
    /// Slower = smaller file + better quality.
    pub preset: String,
    /// CRF quality (0 = lossless, 23 = H.264 default, 51 = worst). Lower = better.
    pub crf: u32,
    /// Target output resolution (width, height). Content is scaled down to fit
    /// and padded with black bars to reach the exact dimensions. Required for
    /// platform compliance:
    /// - Instagram Reels / TikTok â†’ (1080, 1920)
    /// - YouTube / Facebook landscape â†’ (1920, 1080)
    pub target_resolution: Option<(u32, u32)>,
    /// Keyframe interval in frames. Social platforms require dense keyframes for
    /// seeking; use `fps * 2` (e.g., 60 at 30 fps). `None` = FFmpeg default (~250).
    pub keyframe_interval: Option<u32>,
    /// Peak bitrate cap for VBV buffering (e.g., "3500k"). `-b:v` alone only
    /// sets the average; `-maxrate` clamps burst spikes above platform limits.
    pub max_bitrate: Option<String>,
    /// VBV buffer size (e.g., "7000k"). Typically 2Ã— `max_bitrate`.
    pub buf_size: Option<String>,
    /// Additional FFmpeg arguments (escape hatch for advanced users)
    pub extra_args: Vec<String>,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            video_codec: VideoCodec::H264,
            audio_codec: AudioCodec::Aac,
            container: VideoContainer::Mp4,
            fps: 1,
            video_bitrate: None,
            audio_bitrate: "192k".into(),
            pixel_format: PixelFormat::Yuv420p,
            audio_fade: None,
            preset: "medium".into(),
            crf: 23,
            target_resolution: None,
            keyframe_interval: None,
            max_bitrate: None,
            buf_size: None,
            extra_args: vec![],
        }
    }
}

impl VideoConfig {
    // â”€â”€ Presets â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Preset for generic landscape social media (YouTube / Facebook, 16:9, 1920Ã—1080).
    /// H.264 + AAC, 30 fps, 3500 kbps VBV cap, keyframe every 2 s, faststart.
    pub fn social_media() -> Self {
        Self {
            video_codec: VideoCodec::H264,
            audio_codec: AudioCodec::Aac,
            container: VideoContainer::Mp4,
            fps: 30,
            video_bitrate: Some("3500k".into()),
            audio_bitrate: "192k".into(),
            pixel_format: PixelFormat::Yuv420p,
            preset: "fast".into(),
            crf: 23,
            target_resolution: Some((1920, 1080)),
            keyframe_interval: Some(60), // 2 s at 30 fps
            max_bitrate: Some("3500k".into()),
            buf_size: Some("7000k".into()),
            ..Default::default()
        }
    }

    /// Workflow default tuned for genuinely fast workflow renders.
    ///
    /// This intentionally trades visual quality for speed:
    /// - 720p output
    /// - lower frame rate (15 fps) to reduce frames to encode
    /// - `ultrafast` x264 preset
    /// - lighter bitrate targets
    ///
    /// For higher quality delivery, use `social_media` / `high_quality`.
    pub fn workflow_default() -> Self {
        Self {
            fps: 15,
            preset: "ultrafast".into(),
            crf: 28,
            video_bitrate: Some("1800k".into()),
            max_bitrate: Some("1800k".into()),
            buf_size: Some("3600k".into()),
            target_resolution: Some((1280, 720)),
            keyframe_interval: Some(30), // 2 s at 15 fps
            audio_bitrate: "128k".into(),
            ..Self::social_media()
        }
    }

    /// Preset for Instagram Reels and TikTok (9:16 portrait, 1080x1920).
    /// All platform requirements enforced: faststart, VBV cap, dense keyframes.
    pub fn instagram_reel() -> Self {
        Self {
            video_codec: VideoCodec::H264,
            audio_codec: AudioCodec::Aac,
            container: VideoContainer::Mp4,
            fps: 30,
            video_bitrate: Some("3500k".into()),
            audio_bitrate: "192k".into(),
            pixel_format: PixelFormat::Yuv420p,
            preset: "fast".into(),
            crf: 23,
            target_resolution: Some((1080, 1920)), // 9:16 portrait
            keyframe_interval: Some(60),           // 2 s at 30 fps
            max_bitrate: Some("3500k".into()),
            buf_size: Some("7000k".into()),
            ..Default::default()
        }
    }

    /// Preset for high-quality archival (slow encode, CRF 18, 320 kbps audio).
    pub fn high_quality() -> Self {
        Self {
            video_codec: VideoCodec::H264,
            audio_codec: AudioCodec::Aac,
            container: VideoContainer::Mp4,
            fps: 30,
            pixel_format: PixelFormat::Yuv420p,
            preset: "slow".into(),
            crf: 18,
            audio_bitrate: "320k".into(),
            keyframe_interval: Some(60),
            ..Default::default()
        }
    }

    /// Preset for web streaming (WebM + VP9 + Opus).
    ///
    /// **Not** compatible with Instagram, TikTok, or Facebook â€” use
    /// [`instagram_reel()`] / [`social_media()`] for those platforms.
    ///
    /// VP9 ignores the `preset` field. Quality and speed are controlled
    /// internally by [`apply_encoder_args`] via `-quality good -speed 2`.
    pub fn web_stream() -> Self {
        Self {
            video_codec: VideoCodec::VP9,
            audio_codec: AudioCodec::Opus,
            container: VideoContainer::WebM,
            fps: 30,
            audio_bitrate: "128k".into(),
            pixel_format: PixelFormat::Yuv420p,
            crf: 31,
            // `preset` is intentionally left as default â€” VP9 ignores it.
            // apply_encoder_args() uses -quality/-speed for VP9 instead.
            ..Default::default()
        }
    }

    // â”€â”€ Builder helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    pub fn with_audio_fade(
        mut self,
        fade_in: Option<Duration>,
        fade_out: Option<Duration>,
    ) -> Self {
        self.audio_fade = Some(AudioFade { fade_in, fade_out });
        self
    }

    // â”€â”€ Internal FFmpeg argument builders â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

    /// Apply video encoder arguments, correctly handling VP9 vs. H.264/H.265.
    ///
    /// `libvpx-vp9` does **not** support `-preset` â€” passing it causes an FFmpeg
    /// warning and falls through to undefined default behaviour. VP9 uses
    /// `-quality good` / `-speed 0-5` instead, and **requires** `-b:v 0` to
    /// enable pure-CRF mode.
    fn apply_encoder_args(&self, cmd: &mut Command) {
        match self.video_codec {
            VideoCodec::VP9 => {
                cmd.args(["-crf", &self.crf.to_string()])
                    .args(["-b:v", "0"]) // required for VP9 CRF mode
                    .args(["-quality", "good"])
                    .args(["-speed", "2"]);
            }
            VideoCodec::H264 | VideoCodec::H265 => {
                cmd.args(["-crf", &self.crf.to_string()])
                    .args(["-preset", &self.preset]);
            }
            VideoCodec::Copy => {}
        }
    }

    /// Apply average bitrate and VBV peak-rate capping.
    ///
    /// `-b:v` alone sets only the *average* bitrate â€” burst peaks can far exceed
    /// platform limits. `-maxrate` + `-bufsize` (VBV) enforce per-frame limits
    /// the way broadcast and social encoders expect.
    ///
    /// For VP9 CRF mode, `-b:v 0` is already set by [`apply_encoder_args`], so
    /// the `video_bitrate` field is skipped to avoid overriding it.
    fn apply_bitrate_args(&self, cmd: &mut Command) {
        if !matches!(self.video_codec, VideoCodec::VP9) {
            if let Some(ref vbr) = self.video_bitrate {
                cmd.args(["-b:v", vbr]);
            }
        }
        if let Some(ref maxr) = self.max_bitrate {
            cmd.args(["-maxrate", maxr]);
        }
        if let Some(ref bufs) = self.buf_size {
            cmd.args(["-bufsize", bufs]);
        }
    }

    /// Apply the keyframe interval (`-g`).
    ///
    /// Without this, libx264 defaults to 250 frames (~8 s at 30 fps), which is
    /// too sparse for social-platform scrubbing. YouTube recommends 1 keyframe
    /// per 2 s; Instagram and Facebook require it for reliable seeking.
    fn apply_keyframe_interval(&self, cmd: &mut Command) {
        if let Some(g) = self.keyframe_interval {
            cmd.args(["-g", &g.to_string()]);
        }
    }

    /// Apply `-movflags +faststart` for MP4/MOV containers.
    ///
    /// By default, FFmpeg writes the moov atom (metadata index) at the *end* of
    /// the file. `+faststart` relocates it to the front so platforms can begin
    /// processing and streaming before the full upload completes. Instagram,
    /// YouTube, and Facebook all explicitly require this; omitting it causes
    /// silent upload failures or rejection.
    fn apply_faststart(&self, cmd: &mut Command) {
        if self.container.needs_faststart() {
            cmd.args(["-movflags", "+faststart"]);
        }
    }

    /// Build a `-vf` filter string for single-input (non-xfade) pipelines.
    ///
    /// If `target_resolution` is set, scales content to fit while preserving
    /// the aspect ratio, then pads with black bars to the exact target size.
    fn build_vf(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if let Some((w, h)) = self.target_resolution {
            parts.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black"
            ));
        } else {
            // Ensure width and height are divisible by 2 (required for yuv420p).
            parts.push("scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string());
        }

        parts.push(format!("fps={}", self.fps));
        parts.push(format!("format={}", self.pixel_format.ffmpeg_name()));
        parts.join(",")
    }

    /// Build the per-input scale/fps/format chain used inside `filter_complex`
    /// for the xfade slideshow path. Each input must share the same resolution
    /// and frame rate before being fed to `xfade`.
    fn build_xfade_input_filter(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if let Some((w, h)) = self.target_resolution {
            parts.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black"
            ));
        } else {
            parts.push("scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string());
        }

        parts.push(format!("fps={}", self.fps));
        parts.push(format!("format={}", self.pixel_format.ffmpeg_name()));
        parts.join(",")
    }
}

// â”€â”€â”€ Core Functions â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Convert a static image + audio file into a video.
/// The video duration matches the audio duration exactly.
///
/// # Parameters
/// - `image_path`: path to the background image (PNG/JPEG/WebP)
/// - `audio_path`: path to the audio file (MP3/AAC/WAV/OGG)
/// - `output_path`: path for the output video
/// - `config`: encoding configuration
///
/// # Example
/// ```rust
/// image_to_video(
///     "quote.png",
///     "background_music.mp3",
///     "output.mp4",
///     &VideoConfig::instagram_reel(),
/// )?;
/// ```
pub fn image_to_video(
    image_path: &str,
    audio_path: &str,
    output_path: &str,
    config: &VideoConfig,
) -> Result<()> {
    check_ffmpeg()?;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y") // overwrite output without asking
        // Input: looping static image
        .args(["-loop", "1"])
        .args(["-framerate", &config.fps.to_string()])
        .args(["-i", image_path])
        // Input: audio
        .args(["-i", audio_path])
        // Video codec
        .args(["-c:v", config.video_codec.ffmpeg_name()]);

    // Codec-specific quality/speed args â€” handles VP9 vs. H.264/H.265 difference.
    config.apply_encoder_args(&mut cmd);

    // Video filter: resolution enforcement + fps normalisation + pixel format.
    cmd.args(["-vf", &config.build_vf()]);

    // Belt-and-suspenders pix_fmt after vf (some codecs reset it).
    cmd.args(["-pix_fmt", config.pixel_format.ffmpeg_name()]);

    // Tune for still images only when fps == 1.
    // Do NOT apply this for social/reel uploads â€” it lowers bitrate and causes
    // Instagram to reject the file.
    if matches!(config.video_codec, VideoCodec::H264) && config.fps == 1 {
        cmd.args(["-tune", "stillimage"]);
    }

    // Keyframe interval for platform scrubbing compatibility.
    config.apply_keyframe_interval(&mut cmd);

    // Average bitrate + VBV peak cap.
    config.apply_bitrate_args(&mut cmd);

    // Audio codec + bitrate.
    cmd.args(["-c:a", config.audio_codec.ffmpeg_name()]);
    cmd.args(["-b:a", &config.audio_bitrate]);

    // Audio fade filters.
    if let Some(ref fade) = config.audio_fade {
        let audio_duration = get_audio_duration(audio_path).unwrap_or(0.0);
        let mut filters = Vec::new();

        if let Some(fade_in) = fade.fade_in {
            filters.push(format!("afade=t=in:d={:.2}", fade_in.as_secs_f64()));
        }
        if let Some(fade_out) = fade.fade_out {
            let start = (audio_duration - fade_out.as_secs_f64()).max(0.0);
            filters.push(format!(
                "afade=t=out:st={:.2}:d={:.2}",
                start,
                fade_out.as_secs_f64()
            ));
        }

        if !filters.is_empty() {
            cmd.args(["-af", &filters.join(",")]);
        }
    }

    // Match video length to audio length.
    cmd.args(["-shortest"]);

    // CRITICAL: move moov atom to front so platforms can process before full upload.
    config.apply_faststart(&mut cmd);

    // Extra user-defined args.
    for arg in &config.extra_args {
        cmd.arg(arg);
    }

    cmd.arg(output_path);
    run_ffmpeg(cmd)
}

/// Convert an in-memory `DynamicImage` + audio file into a video.
/// Writes the image to a temp file, runs FFmpeg, then cleans up.
pub fn image_to_video_from_memory(
    img: &DynamicImage,
    audio_path: &str,
    output_path: &str,
    config: &VideoConfig,
) -> Result<()> {
    let tmp_img = format!("/tmp/img_proc_tmp_{}.png", std::process::id());
    canvas::save(img, &tmp_img)?;

    let result = image_to_video(&tmp_img, audio_path, output_path, config);
    std::fs::remove_file(&tmp_img).ok();

    result
}

/// Create a slideshow video from multiple images + an optional audio track.
/// Each image is displayed for `slide_duration`.
///
/// `Transition::CrossFade` produces a true cross-dissolve using FFmpeg's
/// `xfade=transition=dissolve` filter. The previous implementation produced an
/// identical hard-cut to `Transition::None` â€” this is now fixed.
///
/// Total duration for CrossFade =
/// `N * slide_secs âˆ’ (N âˆ’ 1) * CROSSFADE_DURATION_SECS`.
///
/// # Parameters
/// - `image_paths`: ordered list of image paths
/// - `slide_duration`: how long each image is displayed (includes any overlap)
/// - `audio_path`: background audio (optional)
/// - `transition`: transition style between slides
/// - `output_path`: output video path
/// - `config`: encoding configuration
pub fn slideshow(
    image_paths: &[String],
    slide_duration: Duration,
    audio_path: Option<&str>,
    transition: Transition,
    output_path: &str,
    config: &VideoConfig,
) -> Result<()> {
    check_ffmpeg()?;

    if image_paths.is_empty() {
        return Err(ImageProcessorError::InvalidParameter(
            "At least one image is required for a slideshow".into(),
        ));
    }

    // CrossFade with 2+ slides requires the xfade filter_complex path.
    if matches!(transition, Transition::CrossFade) && image_paths.len() > 1 {
        return slideshow_crossfade(image_paths, slide_duration, audio_path, output_path, config);
    }

    // â”€â”€ Concat-based path (None, FadeBlack, or single-image CrossFade) â”€â”€â”€â”€â”€â”€â”€â”€

    let concat_path = format!("/tmp/img_proc_concat_{}.txt", std::process::id());
    let slide_secs = slide_duration.as_secs_f64();

    let concat_content: String = image_paths
        .iter()
        .map(|p| format!("file '{}'\nduration {:.3}\n", p, slide_secs))
        .collect();

    std::fs::write(&concat_path, concat_content).map_err(ImageProcessorError::IoError)?;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .args(["-f", "concat"])
        .args(["-safe", "0"])
        .args(["-i", &concat_path]);

    if let Some(audio) = audio_path {
        cmd.args(["-i", audio]);
    }

    let vf = match transition {
        Transition::None | Transition::CrossFade /* single-image fallback */ => {
            config.build_vf()
        }
        Transition::FadeBlack => {
            let scale = if let Some((w, h)) = config.target_resolution {
                format!(
                    "scale={w}:{h}:force_original_aspect_ratio=decrease,\
                     pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black"
                )
            } else {
                "scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string()
            };
            format!(
                "{scale},fps={fps},\
                 fade=t=in:st=0:d=0.5,\
                 fade=t=out:st={out_start:.2}:d=0.5,\
                 format={pix}",
                fps = config.fps,
                out_start = (slide_secs - 0.5).max(0.0),
                pix = config.pixel_format.ffmpeg_name(),
            )
        }
    };

    cmd.args(["-vf", &vf])
        .args(["-c:v", config.video_codec.ffmpeg_name()]);

    config.apply_encoder_args(&mut cmd);

    if matches!(config.video_codec, VideoCodec::H264) && config.fps == 1 {
        cmd.args(["-tune", "stillimage"]);
    }

    config.apply_keyframe_interval(&mut cmd);
    config.apply_bitrate_args(&mut cmd);

    if audio_path.is_some() {
        cmd.args(["-c:a", config.audio_codec.ffmpeg_name()])
            .args(["-b:a", &config.audio_bitrate])
            .args(["-shortest"]);
    }

    // CRITICAL: faststart for platform compatibility.
    config.apply_faststart(&mut cmd);

    for arg in &config.extra_args {
        cmd.arg(arg);
    }

    cmd.arg(output_path);

    let result = run_ffmpeg(cmd);
    std::fs::remove_file(&concat_path).ok();
    result
}

/// Inner implementation for `Transition::CrossFade` with 2+ slides.
///
/// Uses `xfade=transition=dissolve` chained through `filter_complex`.
/// Each image is fed as an individual `-loop 1 -t <duration>` input â€” the concat
/// demuxer cannot be used here because xfade requires separate streams.
///
/// Offset formula: `offset_i = i * (slide_secs - CROSSFADE_DURATION_SECS)`
/// where `i` is zero-indexed. This ensures each dissolve begins exactly when
/// the previous slide's effective display time elapses.
fn slideshow_crossfade(
    image_paths: &[String],
    slide_duration: Duration,
    audio_path: Option<&str>,
    output_path: &str,
    config: &VideoConfig,
) -> Result<()> {
    let slide_secs = slide_duration.as_secs_f64();
    let trans = CROSSFADE_DURATION_SECS;

    if slide_secs <= trans {
        return Err(ImageProcessorError::InvalidParameter(format!(
            "slide_duration ({slide_secs:.2}s) must be greater than the \
             cross-fade overlap duration ({trans:.2}s)"
        )));
    }

    let n = image_paths.len();
    let input_filter = config.build_xfade_input_filter();

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y");

    // One looping input per image, held for exactly `slide_secs`.
    for path in image_paths {
        cmd.args(["-loop", "1"])
            .args(["-t", &format!("{slide_secs:.3}")])
            .args(["-i", path]);
    }

    // Optional audio (input index = n).
    if let Some(audio) = audio_path {
        cmd.args(["-i", audio]);
    }

    // Build filter_complex:
    //   1. Scale/fps/format each input  â†’ [v0]..[vN-1]
    //   2. Chain xfade dissolves        â†’ [x1]..[out]
    let effective = slide_secs - trans; // net visible time per slide before next dissolve
    let mut fc: Vec<String> = Vec::with_capacity(n * 2);

    for i in 0..n {
        fc.push(format!("[{i}:v]{input_filter}[v{i}]"));
    }

    if n == 2 {
        fc.push(format!(
            "[v0][v1]xfade=transition=dissolve:\
             duration={trans:.3}:offset={effective:.3}[out]"
        ));
    } else {
        // First xfade: [v0][v1] â†’ [x1]
        fc.push(format!(
            "[v0][v1]xfade=transition=dissolve:\
             duration={trans:.3}:offset={effective:.3}[x1]"
        ));
        // Middle xfades: [xi][vi+1] â†’ [xi+1]
        for i in 2..n - 1 {
            let offset = i as f64 * effective;
            let prev = format!("x{}", i - 1);
            fc.push(format!(
                "[{prev}][v{i}]xfade=transition=dissolve:\
                 duration={trans:.3}:offset={offset:.3}[x{i}]"
            ));
        }
        // Last xfade â†’ [out]
        let last = n - 1;
        let last_offset = last as f64 * effective;
        let prev = format!("x{}", last - 1);
        fc.push(format!(
            "[{prev}][v{last}]xfade=transition=dissolve:\
             duration={trans:.3}:offset={last_offset:.3}[out]"
        ));
    }

    cmd.args(["-filter_complex", &fc.join(";")])
        .args(["-map", "[out]"])
        .args(["-c:v", config.video_codec.ffmpeg_name()]);

    config.apply_encoder_args(&mut cmd);
    config.apply_keyframe_interval(&mut cmd);
    config.apply_bitrate_args(&mut cmd);

    if audio_path.is_some() {
        let audio_idx = n;
        cmd.args(["-map", &format!("{audio_idx}:a")])
            .args(["-c:a", config.audio_codec.ffmpeg_name()])
            .args(["-b:a", &config.audio_bitrate])
            .args(["-shortest"]);
    }

    config.apply_faststart(&mut cmd);

    for arg in &config.extra_args {
        cmd.arg(arg);
    }

    cmd.arg(output_path);
    run_ffmpeg(cmd)
}

/// Loop a short audio clip to fill a target duration.
/// Useful when your background track is shorter than the intended video.
///
/// # Example
/// ```rust
/// loop_audio("short_music.mp3", Duration::from_secs(60), "looped.mp3")?;
/// image_to_video("image.png", "looped.mp3", "output.mp4", &VideoConfig::default())?;
/// ```
pub fn loop_audio(audio_path: &str, target_duration: Duration, output_path: &str) -> Result<()> {
    check_ffmpeg()?;

    let secs = target_duration.as_secs_f64();
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .args(["-stream_loop", "-1"])
        .args(["-i", audio_path])
        .args(["-t", &format!("{secs:.3}")])
        .args(["-c", "copy"])
        .arg(output_path);

    run_ffmpeg(cmd)
}

/// Mix two audio files together (e.g., voice-over + background music).
///
/// # Parameters
/// - `primary_path`: main audio (voice/narration) â€” always at volume 1.0
/// - `bg_path`: background audio
/// - `bg_volume`: background volume (0.0 = silent, 1.0 = full, 0.3 = typical under voice)
/// - `output_path`: mixed audio output path
/// - `audio_codec`: output codec (pass `AudioCodec::Aac` for MP4 pipelines,
///   `AudioCodec::Opus` for WebM)
/// - `audio_bitrate`: output bitrate string (e.g., `"192k"`, `"128k"`)
pub fn mix_audio(
    primary_path: &str,
    bg_path: &str,
    bg_volume: f32,
    output_path: &str,
    audio_codec: AudioCodec,
    audio_bitrate: &str,
) -> Result<()> {
    check_ffmpeg()?;

    let filter = format!(
        "[0:a]aformat=fltp,volume=1.0[a0];\
         [1:a]aformat=fltp,volume={bg_volume:.2}[a1];\
         [a0][a1]amix=inputs=2:duration=first[aout]"
    );

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .args(["-i", primary_path])
        .args(["-i", bg_path])
        .args(["-filter_complex", &filter])
        .args(["-map", "[aout]"])
        .args(["-c:a", audio_codec.ffmpeg_name()])
        .args(["-b:a", audio_bitrate])
        .arg(output_path);

    run_ffmpeg(cmd)
}

/// Trim audio to a specific start/end time with sample-accurate re-encoding.
///
/// The previous implementation used `-c copy`, which snaps cuts to the nearest
/// keyframe â€” causing audible pops or drift of hundreds of milliseconds.
/// Re-encoding at the cut point produces frame-accurate trims at the cost of
/// a small quality loss (negligible at 192 kbps AAC).
pub fn trim_audio(
    audio_path: &str,
    start: Duration,
    end: Duration,
    output_path: &str,
) -> Result<()> {
    check_ffmpeg()?;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .args(["-i", audio_path])
        .args(["-ss", &format!("{:.3}", start.as_secs_f64())])
        .args(["-to", &format!("{:.3}", end.as_secs_f64())])
        // Re-encode for sample-accurate cuts; -c copy would snap to keyframes.
        .args(["-c:a", "aac"])
        .args(["-b:a", "192k"])
        .args(["-vn"]) // discard any attached video/cover-art stream
        .arg(output_path);

    run_ffmpeg(cmd)
}

/// Add audio to an existing video, replacing its current audio track.
pub fn add_audio_to_video(
    video_path: &str,
    audio_path: &str,
    output_path: &str,
    config: &VideoConfig,
) -> Result<()> {
    check_ffmpeg()?;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .args(["-i", video_path])
        .args(["-i", audio_path])
        .args(["-c:v", "copy"]) // stream-copy video; no re-encode needed
        .args(["-c:a", config.audio_codec.ffmpeg_name()])
        .args(["-b:a", &config.audio_bitrate])
        .args(["-map", "0:v"])
        .args(["-map", "1:a"])
        .args(["-shortest"]);

    // Ensure the remuxed MP4 is also faststart-enabled.
    config.apply_faststart(&mut cmd);

    cmd.arg(output_path);
    run_ffmpeg(cmd)
}

/// Get the duration of an audio/video file in seconds.
pub fn get_audio_duration(path: &str) -> Option<f64> {
    let output = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(["-show_entries", "format=duration"])
        .args(["-of", "default=noprint_wrappers=1:nokey=1"])
        .arg(path)
        .output()
        .ok()?;

    let s = String::from_utf8(output.stdout).ok()?;
    s.trim().parse::<f64>().ok()
}

/// Get video metadata (duration, width, height, fps).
///
/// Uses `ffprobe -of default=noprint_wrappers=1` (stable `key=value` pairs)
/// instead of `-of csv`, which assumed a fixed column order and produced wrong
/// values on files with unusual stream orderings or missing fields.
pub fn get_video_info(path: &str) -> Result<VideoInfo> {
    check_ffmpeg()?;

    let output = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(["-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height,r_frame_rate"])
        // key=value is stable regardless of field order or missing entries.
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path)
        .output()
        .map_err(ImageProcessorError::IoError)?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut fps: f64 = 0.0;

    for line in stdout.lines() {
        let mut iter = line.splitn(2, '=');
        let key = iter.next().unwrap_or("").trim();
        let val = iter.next().unwrap_or("").trim();

        match key {
            "width" => width = val.parse().unwrap_or(0),
            "height" => height = val.parse().unwrap_or(0),
            "r_frame_rate" => {
                // Rational format: "30/1", "2997/100", "60000/1001", etc.
                let parts: Vec<&str> = val.split('/').collect();
                let num: f64 = parts.first().and_then(|x| x.parse().ok()).unwrap_or(1.0);
                let den: f64 = parts.get(1).and_then(|x| x.parse().ok()).unwrap_or(1.0);
                if den != 0.0 {
                    fps = num / den;
                }
            }
            _ => {}
        }
    }

    let duration_secs = get_audio_duration(path).unwrap_or(0.0);

    Ok(VideoInfo {
        width,
        height,
        fps,
        duration_secs,
    })
}

/// Basic metadata about a video file.
#[derive(Debug)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_secs: f64,
}

// â”€â”€â”€ Internal Helpers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

fn check_ffmpeg() -> Result<()> {
    let status = Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(ImageProcessorError::InvalidParameter(
            "FFmpeg not found. Install with: apt install ffmpeg (Ubuntu) \
             or brew install ffmpeg (macOS)"
                .into(),
        )),
    }
}

fn run_ffmpeg(mut cmd: Command) -> Result<()> {
    let output = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(ImageProcessorError::IoError)?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().filter(|l| !l.is_empty()).collect();
        // Return the last 5 meaningful lines from ffmpeg stderr for diagnosis.
        let last_error = if lines.is_empty() {
            "Unknown ffmpeg error".to_string()
        } else {
            lines
                .into_iter()
                .rev()
                .take(5)
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        };
        Err(ImageProcessorError::InvalidParameter(format!(
            "FFmpeg error:\n{last_error}"
        )))
    }
}
