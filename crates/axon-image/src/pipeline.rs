use crate::{
    canvas::{self, Gravity, OutputFormat},
    composite::{self, GradientDirection},
    error::Result,
    filters,
    text::{self, LoadedFont, TextStyle},
};
use image::{imageops::FilterType, DynamicImage, Rgba};

/// Fluent builder for chaining image processing operations.
///
/// # Example
/// ```rust,ignore
/// let result = ImagePipeline::from_path("input.jpg")?
///     .resize_fill(1080, 1080)
///     .brightness(-20)
///     .gradient_overlay(
///         [0, 0, 0, 0],
///         [0, 0, 0, 180],
///         GradientDirection::BottomToTop,
///     )
///     .add_text("Your quote here", &font, &style, 50)
///     .save("output.png")?;
/// ```
pub struct ImagePipeline {
    img: DynamicImage,
}

impl ImagePipeline {
    /// Start pipeline with an existing DynamicImage
    pub fn new(img: DynamicImage) -> Self {
        Self { img }
    }

    /// Load image from file path and start pipeline
    pub fn from_path(path: &str) -> Result<Self> {
        Ok(Self {
            img: canvas::load(path)?,
        })
    }

    /// Load image from bytes and start pipeline
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            img: canvas::from_bytes(bytes)?,
        })
    }

    /// Get current image dimensions
    pub fn dimensions(&self) -> (u32, u32) {
        use image::GenericImageView;
        self.img.dimensions()
    }

    // ─── Canvas Operations ────────────────────────────────────────────────────

    /// Resize to exact dimensions (may distort)
    pub fn resize(self, width: u32, height: u32) -> Self {
        Self {
            img: canvas::resize(&self.img, width, height, FilterType::Lanczos3),
        }
    }

    /// Resize preserving aspect ratio, fitting within bounds
    pub fn resize_fit(self, max_width: u32, max_height: u32) -> Self {
        Self {
            img: canvas::resize_fit(&self.img, max_width, max_height, FilterType::Lanczos3),
        }
    }

    /// Resize and crop to fill exact dimensions (no distortion)
    pub fn resize_fill(self, width: u32, height: u32) -> Self {
        Self {
            img: canvas::resize_fill(&self.img, width, height, FilterType::Lanczos3),
        }
    }

    /// Crop to a specific rectangle
    pub fn crop(self, x: u32, y: u32, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            img: canvas::crop(&self.img, x, y, width, height)?,
        })
    }

    /// Crop from center
    pub fn crop_center(self, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            img: canvas::crop_center(&self.img, width, height)?,
        })
    }

    /// Add uniform padding on all sides
    pub fn pad(self, padding: u32, color: [u8; 4]) -> Self {
        Self {
            img: canvas::pad_uniform(&self.img, padding, Rgba(color)),
        }
    }

    /// Extend canvas to given size, placing image at gravity position
    pub fn extend_canvas(
        self,
        width: u32,
        height: u32,
        gravity: Gravity,
        color: [u8; 4],
    ) -> Result<Self> {
        Ok(Self {
            img: canvas::extend_canvas(&self.img, width, height, gravity, Rgba(color))?,
        })
    }

    /// Rotate image by degrees
    pub fn rotate(self, degrees: f32) -> Result<Self> {
        Ok(Self {
            img: canvas::rotate(&self.img, degrees)?,
        })
    }

    /// Flip horizontally
    pub fn flip_horizontal(self) -> Self {
        Self {
            img: canvas::flip_horizontal(&self.img),
        }
    }

    /// Flip vertically
    pub fn flip_vertical(self) -> Self {
        Self {
            img: canvas::flip_vertical(&self.img),
        }
    }

    // ─── Filter Operations ────────────────────────────────────────────────────

    /// Gaussian blur
    pub fn blur(self, sigma: f32) -> Self {
        Self {
            img: filters::blur(&self.img, sigma),
        }
    }

    /// Sharpen with unsharp mask
    pub fn sharpen(self, sigma: f32, threshold: i32) -> Self {
        Self {
            img: filters::sharpen(&self.img, sigma, threshold),
        }
    }

    /// Adjust brightness (-255 to 255)
    pub fn brightness(self, value: i32) -> Self {
        Self {
            img: filters::brightness(&self.img, value),
        }
    }

    /// Adjust contrast
    pub fn contrast(self, value: f32) -> Self {
        Self {
            img: filters::contrast(&self.img, value),
        }
    }

    /// Convert to grayscale
    pub fn grayscale(self) -> Self {
        Self {
            img: filters::grayscale(&self.img),
        }
    }

    /// Apply sepia tone
    pub fn sepia(self) -> Self {
        Self {
            img: filters::sepia(&self.img),
        }
    }

    /// Invert colors
    pub fn invert(self) -> Self {
        Self {
            img: filters::invert(&self.img),
        }
    }

    /// Adjust saturation (0.0 = gray, 1.0 = original, 2.0 = vivid)
    pub fn saturation(self, factor: f32) -> Self {
        Self {
            img: filters::saturation(&self.img, factor),
        }
    }

    /// Hue rotation in degrees
    pub fn hue_rotate(self, degrees: f32) -> Self {
        Self {
            img: filters::hue_rotate(&self.img, degrees),
        }
    }

    /// Apply vignette effect (darkened edges)
    pub fn vignette(self, strength: f32) -> Self {
        Self {
            img: composite::vignette(&self.img, strength),
        }
    }

    /// Tint with a color
    pub fn tint(self, color: [u8; 3], strength: f32) -> Self {
        Self {
            img: filters::tint(&self.img, color, strength),
        }
    }

    // ─── Composite Operations ─────────────────────────────────────────────────

    /// Overlay another image at an absolute position
    pub fn overlay(self, other: &DynamicImage, x: i64, y: i64) -> Self {
        Self {
            img: composite::overlay(&self.img, other, x, y),
        }
    }

    /// Overlay another image at a gravity position
    pub fn overlay_gravity(
        self,
        other: &DynamicImage,
        gravity: Gravity,
        margin_x: u32,
        margin_y: u32,
    ) -> Self {
        Self {
            img: composite::overlay_gravity(&self.img, other, gravity, margin_x, margin_y),
        }
    }

    /// Add a semi-transparent color overlay to the entire image
    pub fn color_overlay(self, color: [u8; 3], opacity: f32) -> Self {
        Self {
            img: composite::color_overlay(&self.img, color, opacity),
        }
    }

    /// Add a gradient overlay
    pub fn gradient_overlay(
        self,
        color_start: [u8; 4],
        color_end: [u8; 4],
        direction: GradientDirection,
    ) -> Self {
        Self {
            img: composite::gradient_overlay(&self.img, color_start, color_end, direction),
        }
    }

    /// Add a watermark image at gravity position with given opacity
    pub fn watermark(
        self,
        mark: &DynamicImage,
        gravity: Gravity,
        margin_x: u32,
        margin_y: u32,
        opacity: f32,
    ) -> Self {
        Self {
            img: composite::watermark(&self.img, mark, gravity, margin_x, margin_y, opacity),
        }
    }

    /// Blend with another image (0.0 = base only, 1.0 = other only)
    pub fn blend(self, other: &DynamicImage, alpha: f32) -> Self {
        Self {
            img: composite::blend(&self.img, other, alpha),
        }
    }

    /// Apply rounded corners
    pub fn rounded_corners(self, radius: u32) -> Self {
        Self {
            img: composite::rounded_corners(&self.img, radius),
        }
    }

    // ─── Text Operations ──────────────────────────────────────────────────────

    /// Add a text block to the image, auto-sized to fit within a margin
    pub fn add_text(
        self,
        text: &str,
        font: &LoadedFont,
        style: &TextStyle,
        margin_x: u32,
        margin_y: u32,
    ) -> Self {
        use image::GenericImageView;
        let (iw, ih) = self.img.dimensions();
        let region_w = iw.saturating_sub(2 * margin_x);
        let region_h = ih.saturating_sub(2 * margin_y);

        // Auto-fit font size
        let fitted_size = text::auto_font_size(
            text,
            font,
            region_w as f32,
            region_h as f32,
            style.size,
            12.0,
            style.line_height,
        );

        let fitted_style = TextStyle {
            size: fitted_size,
            ..style.clone()
        };

        let mut rgba = self.img.to_rgba8();
        text::render_text(
            &mut rgba,
            text,
            font,
            &fitted_style,
            margin_x,
            margin_y,
            region_w,
            region_h,
        );
        Self {
            img: DynamicImage::ImageRgba8(rgba),
        }
    }

    /// Add two text blocks (e.g., quote + attribution) auto-sized together
    pub fn add_two_texts(
        self,
        main_text: &str,
        main_font: &LoadedFont,
        main_style: &TextStyle,
        last_text: &str,
        last_font: &LoadedFont,
        last_style: &TextStyle,
        margin_x: u32,
        margin_y: u32,
    ) -> Self {
        use image::GenericImageView;
        let (iw, ih) = self.img.dimensions();
        let region_w = iw.saturating_sub(2 * margin_x);
        let region_h = ih.saturating_sub(2 * margin_y);

        let (fitted_main_size, fitted_last_size) = text::auto_font_size_two_blocks(
            main_text,
            main_font,
            main_style.size,
            last_text,
            last_font,
            last_style.size,
            region_w as f32,
            region_h as f32,
            12.0,
            main_style.line_height,
            last_style.line_height,
        );

        let fitted_main_style = TextStyle {
            size: fitted_main_size,
            ..main_style.clone()
        };
        let fitted_last_style = TextStyle {
            size: fitted_last_size,
            ..last_style.clone()
        };

        let mut rgba = self.img.to_rgba8();
        text::render_two_blocks(
            &mut rgba,
            main_text,
            main_font,
            &fitted_main_style,
            last_text,
            last_font,
            &fitted_last_style,
            margin_x,
            margin_y,
            region_w,
            region_h,
        );
        Self {
            img: DynamicImage::ImageRgba8(rgba),
        }
    }

    /// Add a semi-transparent box behind where text will be placed
    pub fn add_text_background(
        self,
        margin: u32,
        color: [u8; 3],
        opacity: f32,
        padding: u32,
    ) -> Self {
        use image::GenericImageView;
        let (iw, ih) = self.img.dimensions();
        let mut rgba = self.img.to_rgba8();
        let x = margin.saturating_sub(padding);
        let y = margin.saturating_sub(padding);
        let w = iw.saturating_sub(2 * margin) + 2 * padding;
        let h = ih.saturating_sub(2 * margin) + 2 * padding;
        composite::semi_transparent_rect(&mut rgba, x, y, w, h, color, opacity);
        Self {
            img: DynamicImage::ImageRgba8(rgba),
        }
    }

    // ─── Output ───────────────────────────────────────────────────────────────

    /// Consume the pipeline and return the final image
    pub fn build(self) -> DynamicImage {
        self.img
    }

    /// Save to file, format inferred from extension
    pub fn save(self, path: &str) -> Result<()> {
        canvas::save(&self.img, path)
    }

    /// Save to file with explicit format
    pub fn save_as(self, path: &str, format: OutputFormat) -> Result<()> {
        canvas::save_as(&self.img, path, format)
    }

    /// Encode to bytes in the given format
    pub fn to_bytes(self, format: OutputFormat) -> Result<Vec<u8>> {
        canvas::to_bytes(&self.img, format)
    }
}
