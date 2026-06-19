//! # image_processor
//!
//! A full-featured image processing library built for AI agent pipelines.
//!
//! ## Modules
//! - [`canvas`] — resize, crop, rotate, pad, format conversion
//! - [`text`] — TTF font loading, text wrapping, auto-sizing, shadow, outline
//! - [`composite`] — layering, overlays, watermark, gradient, blend, vignette
//! - [`filters`] — blur, sharpen, brightness, contrast, grayscale, sepia, hue, saturation
//! - [`utils`] — EXIF reading, dominant color, brightness analysis, pHash
//! - [`pipeline`] — fluent builder for chaining all operations
//! - [`batch`] — parallel processing of image collections
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use image_processor::{
//!     pipeline::ImagePipeline,
//!     text::{LoadedFont, TextStyle, TextAlignment},
//!     canvas::{Gravity, OutputFormat},
//!     composite::GradientDirection,
//! };
//!
//! fn main() -> image_processor::error::Result<()> {
//!     let font = LoadedFont::from_path("/fonts/Playball-Regular.ttf")?;
//!     let style = TextStyle {
//!         size: 60.0,
//!         color: [255, 255, 255, 255],
//!         alignment: TextAlignment::Center,
//!         ..Default::default()
//!     };
//!
//!     ImagePipeline::from_path("background.jpg")?
//!         .resize_fill(1080, 1080)
//!         .gradient_overlay(
//!             [0, 0, 0, 60],
//!             [0, 0, 0, 200],
//!             GradientDirection::BottomToTop,
//!         )
//!         .add_text("God is love.", &font, &style, 80)
//!         .save("output.png")?;
//!
//!     Ok(())
//! }
//! ```

pub mod batch;
pub mod canvas;
pub mod composite;
pub mod error;
pub mod filters;
pub mod pipeline;
pub mod text;
pub mod utils;
pub mod video;

// Re-export the most commonly used types at the crate root
pub use canvas::{Gravity, OutputFormat};
pub use composite::GradientDirection;
pub use error::{ImageProcessorError, Result};
pub use pipeline::ImagePipeline;
pub use text::{LoadedFont, TextAlignment, TextOutline, TextShadow, TextStyle};
