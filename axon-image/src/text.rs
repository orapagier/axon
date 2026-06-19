use crate::error::{ImageProcessorError, Result};
use ab_glyph::{Font, FontRef, FontVec, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_text_mut, text_size};
use serde::{Deserialize, Serialize};

/// Horizontal text alignment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlignment {
    Left,
    Center,
    Right,
}

/// Shadow configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextShadow {
    pub offset_x: i32,
    pub offset_y: i32,
    pub color: [u8; 4], // RGBA
}

/// Outline (stroke) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextOutline {
    pub width: u32,
    pub color: [u8; 4], // RGBA
}

/// Full text style configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextStyle {
    /// Font size in pixels
    pub size: f32,
    /// Text fill color [R, G, B, A]
    pub color: [u8; 4],
    /// Text alignment
    pub alignment: TextAlignment,
    /// Optional drop shadow
    pub shadow: Option<TextShadow>,
    /// Optional stroke outline
    pub outline: Option<TextOutline>,
    /// Line height multiplier (1.0 = tight, 1.4 = comfortable)
    pub line_height: f32,
    /// Letter spacing in pixels (can be negative)
    pub letter_spacing: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            size: 48.0,
            color: [255, 255, 255, 255],
            alignment: TextAlignment::Center,
            shadow: Some(TextShadow {
                offset_x: 2,
                offset_y: 2,
                color: [0, 0, 0, 180],
            }),
            outline: None,
            line_height: 1.4,
            letter_spacing: 0.0,
        }
    }
}

impl TextStyle {
    pub fn new(size: f32, color: [u8; 4]) -> Self {
        Self {
            size,
            color,
            ..Default::default()
        }
    }

    pub fn with_alignment(mut self, alignment: TextAlignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn with_shadow(mut self, shadow: TextShadow) -> Self {
        self.shadow = Some(shadow);
        self
    }

    pub fn without_shadow(mut self) -> Self {
        self.shadow = None;
        self
    }

    pub fn with_outline(mut self, outline: TextOutline) -> Self {
        self.outline = Some(outline);
        self
    }

    pub fn with_line_height(mut self, line_height: f32) -> Self {
        self.line_height = line_height;
        self
    }
}

/// A loaded font that can be used for rendering
pub enum LoadedFont {
    Borrowed(FontRef<'static>),
    Owned(FontVec),
}

impl LoadedFont {
    /// Load font from file path
    pub fn from_path(path: &str) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        FontVec::try_from_vec(bytes)
            .map(LoadedFont::Owned)
            .map_err(|e| {
                ImageProcessorError::FontError(format!("Failed to parse font at {path}: {e:?}"))
            })
    }

    /// Load font from embedded bytes (e.g., include_bytes!)
    pub fn from_bytes(bytes: &'static [u8]) -> Result<Self> {
        FontRef::try_from_slice(bytes)
            .map(LoadedFont::Borrowed)
            .map_err(|e| {
                ImageProcessorError::FontError(format!("Failed to parse font bytes: {e:?}"))
            })
    }

    /// Measure the pixel width of a string at the given scale
    pub fn measure_width(&self, text: &str, scale: PxScale) -> f32 {
        match self {
            LoadedFont::Borrowed(f) => measure_width_inner(f, text, scale),
            LoadedFont::Owned(f) => measure_width_inner(f, text, scale),
        }
    }

    /// Measure the pixel height of a string at the given scale
    pub fn measure_height(&self, scale: PxScale) -> f32 {
        match self {
            LoadedFont::Borrowed(f) => {
                let sf = f.as_scaled(scale);
                sf.ascent() - sf.descent()
            }
            LoadedFont::Owned(f) => {
                let sf = f.as_scaled(scale);
                sf.ascent() - sf.descent()
            }
        }
    }

    /// Draw text onto an RGBA image using imageproc
    pub fn draw(
        &self,
        img: &mut RgbaImage,
        color: Rgba<u8>,
        x: i32,
        y: i32,
        scale: PxScale,
        text: &str,
    ) {
        match self {
            LoadedFont::Borrowed(f) => draw_text_mut(img, color, x, y, scale, f, text),
            LoadedFont::Owned(f) => draw_text_mut(img, color, x, y, scale, f, text),
        }
    }

    /// Measure full text size using imageproc's text_size
    pub fn text_size(&self, scale: PxScale, text: &str) -> (u32, u32) {
        match self {
            LoadedFont::Borrowed(f) => text_size(scale, f, text),
            LoadedFont::Owned(f) => text_size(scale, f, text),
        }
    }
}

fn measure_width_inner<F: Font>(font: &F, text: &str, scale: PxScale) -> f32 {
    let sf = font.as_scaled(scale);
    let mut width = 0.0f32;
    let mut last_glyph_id = None;
    for ch in text.chars() {
        let glyph_id = sf.glyph_id(ch);
        if let Some(last) = last_glyph_id {
            width += sf.kern(last, glyph_id);
        }
        width += sf.h_advance(glyph_id);
        last_glyph_id = Some(glyph_id);
    }
    width
}

/// Wrap text into lines that fit within max_width pixels at the given scale
pub fn wrap_text(text: &str, font: &LoadedFont, scale: PxScale, max_width: f32) -> Vec<String> {
    let mut lines = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current_line = String::new();
        let mut current_width = 0.0f32;
        let space_width = font.measure_width(" ", scale);

        for word in paragraph.split_whitespace() {
            let word_width = font.measure_width(word, scale);

            if current_line.is_empty() {
                current_line = word.to_string();
                current_width = word_width;
            } else if current_width + space_width + word_width <= max_width {
                current_line.push(' ');
                current_line.push_str(word);
                current_width += space_width + word_width;
            } else {
                lines.push(current_line);
                current_line = word.to_string();
                current_width = word_width;
            }
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }

    lines
}

/// Find the largest font size at which all text fits within the given pixel area
pub fn auto_font_size(
    text: &str,
    font: &LoadedFont,
    max_width: f32,
    max_height: f32,
    initial_size: f32,
    min_size: f32,
    line_height: f32,
) -> f32 {
    let mut low = min_size;
    let mut high = initial_size.max(min_size + 1.0);
    let mut best_size = min_size;

    // Use binary search for efficiency and precision
    for _ in 0..15 {
        // 15 iterations = ~0.03 error for 1000px range
        if high - low < 0.5 {
            break;
        }

        let size = (low + high) / 2.0;
        let scale = PxScale::from(size);
        let lines = wrap_text(text, font, scale, max_width);

        let line_h = font.measure_height(scale) * line_height;
        let total_h = line_h * lines.len() as f32;

        // Also check if any single word is still too wide (wrap_text puts them on their own line)
        let mut overflow_x = false;
        for line in &lines {
            if font.measure_width(line, scale) > max_width {
                overflow_x = true;
                break;
            }
        }

        if total_h <= max_height && !overflow_x {
            best_size = size;
            low = size;
        } else {
            high = size;
        }
    }

    best_size
}

/// Auto-size two blocks of text together to fix within a region
pub fn auto_font_size_two_blocks(
    main_text: &str,
    main_font: &LoadedFont,
    main_initial_size: f32,
    last_text: &str,
    last_font: &LoadedFont,
    last_initial_size: f32,
    max_width: f32,
    max_height: f32,
    min_main_size: f32,
    main_line_height: f32,
    last_line_height: f32,
) -> (f32, f32) {
    let mut low = min_main_size;
    let mut high = main_initial_size.max(min_main_size + 1.0);
    let ratio = last_initial_size / main_initial_size;
    let mut best_main_size = min_main_size;

    for _ in 0..15 {
        if high - low < 0.5 {
            break;
        }

        let main_size = (low + high) / 2.0;
        let last_size = main_size * ratio;
        let main_scale = PxScale::from(main_size);
        let last_scale = PxScale::from(last_size);

        let main_line_h = main_font.measure_height(main_scale) * main_line_height;
        let last_line_h = last_font.measure_height(last_scale) * last_line_height;

        let main_lines = wrap_text(main_text, main_font, main_scale, max_width);
        let last_lines = wrap_text(last_text, last_font, last_scale, max_width);

        let total_h = main_line_h * main_lines.len() as f32
            + (main_line_h * 0.5) // gap between blocks (tighter: 0.5 instead of 0.7)
            + last_line_h * last_lines.len() as f32;

        let mut overflow_x = false;
        for line in &main_lines {
            if main_font.measure_width(line, main_scale) > max_width {
                overflow_x = true;
                break;
            }
        }
        if !overflow_x {
            for line in &last_lines {
                if last_font.measure_width(line, last_scale) > max_width {
                    overflow_x = true;
                    break;
                }
            }
        }

        if total_h <= max_height && !overflow_x {
            best_main_size = main_size;
            low = main_size;
        } else {
            high = main_size;
        }
    }

    (best_main_size, best_main_size * ratio)
}

/// Compute the total pixel height of wrapped text at given scale and line height multiplier
pub fn text_block_height(
    text: &str,
    font: &LoadedFont,
    scale: PxScale,
    max_width: f32,
    line_height_multiplier: f32,
) -> f32 {
    let lines = wrap_text(text, font, scale, max_width);
    let line_h = font.measure_height(scale) * line_height_multiplier;
    line_h * lines.len() as f32
}

/// Render a text block onto an RGBA image
///
/// # Parameters
/// - `img`: target image (modified in place)
/// - `text`: the text to render
/// - `font`: loaded font to use
/// - `style`: text styling (color, shadow, outline, alignment, etc.)
/// - `region_x`, `region_y`: top-left of the text bounding region
/// - `region_w`, `region_h`: size of the text bounding region
pub fn render_text(
    img: &mut RgbaImage,
    text: &str,
    font: &LoadedFont,
    style: &TextStyle,
    region_x: u32,
    region_y: u32,
    region_w: u32,
    region_h: u32,
) {
    let scale = PxScale::from(style.size);
    let lines = wrap_text(text, font, scale, region_w as f32);
    let line_h = (font.measure_height(scale) * style.line_height) as i32;
    let total_h = line_h * lines.len() as i32;

    // Vertical centering within region
    let start_y = region_y as i32 + ((region_h as i32 - total_h) / 2).max(0);

    for (i, line) in lines.iter().enumerate() {
        let line_w = font.measure_width(line, scale) as i32;
        let y = start_y + i as i32 * line_h;

        let x = match style.alignment {
            TextAlignment::Left => region_x as i32,
            TextAlignment::Center => region_x as i32 + (region_w as i32 - line_w) / 2,
            TextAlignment::Right => region_x as i32 + region_w as i32 - line_w,
        };

        draw_line(img, line, font, style, x, y, scale);
    }
}

/// Render two separate text blocks (e.g., quote body + attribution) onto the image
pub fn render_two_blocks(
    img: &mut RgbaImage,
    main_text: &str,
    main_font: &LoadedFont,
    main_style: &TextStyle,
    last_text: &str,
    last_font: &LoadedFont,
    last_style: &TextStyle,
    region_x: u32,
    region_y: u32,
    region_w: u32,
    region_h: u32,
) {
    let main_scale = PxScale::from(main_style.size);
    let last_scale = PxScale::from(last_style.size);

    let main_line_h = (main_font.measure_height(main_scale) * main_style.line_height) as i32;
    let last_line_h = (last_font.measure_height(last_scale) * last_style.line_height) as i32;

    let main_lines = wrap_text(main_text, main_font, main_scale, region_w as f32);
    let last_lines = wrap_text(last_text, last_font, last_scale, region_w as f32);

    let total_h = main_line_h * main_lines.len() as i32
        + (main_line_h / 2) // gap between blocks
        + last_line_h * last_lines.len() as i32;

    let start_y = region_y as i32 + ((region_h as i32 - total_h) / 2).max(0);

    // Draw main text
    for (i, line) in main_lines.iter().enumerate() {
        let line_w = main_font.measure_width(line, main_scale) as i32;
        let y = start_y + i as i32 * main_line_h;
        let x = align_x(region_x, region_w, line_w, main_style.alignment);
        draw_line(img, line, main_font, main_style, x, y, main_scale);
    }

    // Draw last line block below with gap
    let last_start_y = start_y + main_line_h * main_lines.len() as i32 + main_line_h / 2;
    for (i, line) in last_lines.iter().enumerate() {
        let line_w = last_font.measure_width(line, last_scale) as i32;
        let y = last_start_y + i as i32 * last_line_h;
        let x = align_x(region_x, region_w, line_w, last_style.alignment);
        draw_line(img, line, last_font, last_style, x, y, last_scale);
    }
}

fn align_x(region_x: u32, region_w: u32, line_w: i32, alignment: TextAlignment) -> i32 {
    match alignment {
        TextAlignment::Left => region_x as i32,
        TextAlignment::Center => region_x as i32 + (region_w as i32 - line_w) / 2,
        TextAlignment::Right => region_x as i32 + region_w as i32 - line_w,
    }
}

fn draw_line(
    img: &mut RgbaImage,
    text: &str,
    font: &LoadedFont,
    style: &TextStyle,
    x: i32,
    y: i32,
    scale: PxScale,
) {
    // 1. Draw shadow (lowest layer)
    if let Some(ref shadow) = style.shadow {
        let sc = Rgba(shadow.color);
        font.draw(
            img,
            sc,
            x + shadow.offset_x,
            y + shadow.offset_y,
            scale,
            text,
        );
    }

    // 2. Draw outline (middle layer) — draw in 8 directions + diagonals
    if let Some(ref outline) = style.outline {
        let oc = Rgba(outline.color);
        let w = outline.width as i32;
        for dx in -w..=w {
            for dy in -w..=w {
                if dx == 0 && dy == 0 {
                    continue;
                }
                font.draw(img, oc, x + dx, y + dy, scale, text);
            }
        }
    }

    // 3. Draw main text (top layer)
    let fill = Rgba(style.color);
    font.draw(img, fill, x, y, scale, text);
}

/// Build a standalone text image with transparent background
pub fn text_to_image(
    text: &str,
    font: &LoadedFont,
    style: &TextStyle,
    max_width: u32,
) -> RgbaImage {
    let scale = PxScale::from(style.size);
    let lines = wrap_text(text, font, scale, max_width as f32);

    let mut actual_max_w: f32 = 0.0;
    for line in &lines {
        let w = font.measure_width(line, scale);
        if w > actual_max_w {
            actual_max_w = w;
        }
    }
    let actual_max_width = (actual_max_w.ceil() as u32).max(1).min(max_width);

    let line_h = (font.measure_height(scale) * style.line_height) as u32;
    let height = line_h * lines.len() as u32 + line_h / 2;

    let mut img: RgbaImage =
        image::ImageBuffer::from_pixel(actual_max_width, height.max(1), Rgba([0, 0, 0, 0]));

    render_text(&mut img, text, font, style, 0, 0, actual_max_width, height);
    img
}
