//! Full pipeline example: renders a quote onto an image, then combines with
//! background music to produce a shareable MP4 video.
//!
//! Usage:
//!   cargo run --example quote_video -- \
//!     --image background.png \
//!     --audio music.mp3 \
//!     --text "In Him was life, and that life was the light of all mankind." \
//!     --attribution "— John 1:4" \
//!     --output output.mp4

use image_processor::{
    text::{LoadedFont, TextAlignment, TextShadow, TextStyle},
    video::{get_audio_duration, image_to_video_from_memory, loop_audio, VideoConfig},
    GradientDirection, ImagePipeline, Result,
};
use std::time::Duration;

struct Config {
    image: String,
    audio: String,
    text: String,
    attribution: String,
    font_path: String,
    output: String,
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str| -> Option<String> {
        args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
    };

    Config {
        image: get("--image").unwrap_or("background.png".into()),
        audio: get("--audio").unwrap_or("audio.mp3".into()),
        text: get("--text").unwrap_or("Trust in the Lord with all your heart.".into()),
        attribution: get("--attribution").unwrap_or("— Proverbs 3:5".into()),
        font_path: get("--font").unwrap_or("/fonts/Playball-Regular.ttf".into()),
        output: get("--output").unwrap_or("output.mp4".into()),
    }
}

fn main() -> Result<()> {
    let cfg = parse_args();

    println!("Loading font...");
    let font = LoadedFont::from_path(&cfg.font_path)?;

    let main_style = TextStyle {
        size: 60.0,
        color: [255, 255, 255, 255],
        alignment: TextAlignment::Center,
        shadow: Some(TextShadow {
            offset_x: 2,
            offset_y: 2,
            color: [0, 0, 0, 180],
        }),
        line_height: 1.45,
        ..Default::default()
    };

    let attr_style = TextStyle {
        size: 32.0,
        color: [220, 220, 220, 255],
        alignment: TextAlignment::Center,
        shadow: Some(TextShadow {
            offset_x: 1,
            offset_y: 1,
            color: [0, 0, 0, 160],
        }),
        line_height: 1.3,
        ..Default::default()
    };

    // ── Step 1: Render the quote onto the image ───────────────────────────────
    println!("Rendering quote image...");
    let rendered = ImagePipeline::from_path(&cfg.image)?
        .resize_fill(1080, 1080)
        .gradient_overlay(
            [0, 0, 0, 40],
            [0, 0, 0, 180],
            GradientDirection::BottomToTop,
        )
        .add_two_texts(
            &cfg.text,
            &font,
            &main_style,
            &cfg.attribution,
            &font,
            &attr_style,
            80,
            80,
        )
        .build();

    // ── Step 2: Prepare audio ─────────────────────────────────────────────────
    // Get audio duration so we can loop music to match
    println!("Preparing audio...");
    let audio_duration = get_audio_duration(&cfg.audio).unwrap_or(30.0);
    println!("  Audio duration: {:.1}s", audio_duration);

    // Loop music to match narration length (with 2s buffer for fade-out)
    let tmp_looped = format!("/tmp/qv_looped_{}.mp3", std::process::id());
    let tmp_mixed = format!("/tmp/qv_mixed_{}.aac", std::process::id());

    loop_audio(
        &cfg.audio,
        Duration::from_secs_f64(audio_duration + 2.0),
        &tmp_looped,
    )?;

    // Optional: mix voice + background music if you have a separate voice track
    // mix_audio("voice.mp3", &tmp_looped, cfg.bg_music_volume, &tmp_mixed)?;
    // For this example we just use the audio directly

    // ── Step 3: Combine image + audio into video ──────────────────────────────
    println!("Encoding video...");
    let video_config = VideoConfig::social_media().with_audio_fade(
        Some(Duration::from_millis(500)), // fade in
        Some(Duration::from_secs(2)),     // fade out
    );

    image_to_video_from_memory(&rendered, &cfg.audio, &cfg.output, &video_config)?;

    // Cleanup temp files
    std::fs::remove_file(&tmp_looped).ok();
    std::fs::remove_file(&tmp_mixed).ok();

    println!("Done → {}", cfg.output);
    Ok(())
}
