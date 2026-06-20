use crate::error::{ImageProcessorError, Result};
use image::{
    imageops::FilterType, DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Rgba, RgbaImage,
};
use std::io::Cursor;

/// Gravity determines where content is anchored when extending or overlaying
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gravity {
    NorthWest,
    North,
    NorthEast,
    West,
    Center,
    East,
    SouthWest,
    South,
    SouthEast,
}

/// Output image format
#[derive(Debug, Clone, Copy)]
pub enum OutputFormat {
    Png,
    Jpeg,
    WebP,
}

impl OutputFormat {
    pub fn to_image_format(self) -> ImageFormat {
        match self {
            OutputFormat::Png => ImageFormat::Png,
            OutputFormat::Jpeg => ImageFormat::Jpeg,
            OutputFormat::WebP => ImageFormat::WebP,
        }
    }

    pub fn extension(&self) -> &str {
        match self {
            OutputFormat::Png => "png",
            OutputFormat::Jpeg => "jpg",
            OutputFormat::WebP => "webp",
        }
    }
}

/// Resize image to exact dimensions (may distort)
pub fn resize(img: &DynamicImage, width: u32, height: u32, filter: FilterType) -> DynamicImage {
    img.resize_exact(width, height, filter)
}

/// Resize image while preserving aspect ratio, fitting within given bounds
pub fn resize_fit(
    img: &DynamicImage,
    max_width: u32,
    max_height: u32,
    filter: FilterType,
) -> DynamicImage {
    img.resize(max_width, max_height, filter)
}

/// Resize image preserving aspect ratio, then crop to fill exact dimensions
pub fn resize_fill(
    img: &DynamicImage,
    width: u32,
    height: u32,
    filter: FilterType,
) -> DynamicImage {
    let (orig_w, orig_h) = img.dimensions();

    let scale_w = width as f64 / orig_w as f64;
    let scale_h = height as f64 / orig_h as f64;
    let scale = scale_w.max(scale_h);

    let scaled_w = (orig_w as f64 * scale).ceil() as u32;
    let scaled_h = (orig_h as f64 * scale).ceil() as u32;

    let scaled = img.resize_exact(scaled_w, scaled_h, filter);
    crop_center(&scaled, width, height).unwrap_or(scaled)
}

/// Generate a thumbnail fitting within max dimensions
pub fn thumbnail(img: &DynamicImage, max_width: u32, max_height: u32) -> DynamicImage {
    img.thumbnail(max_width, max_height)
}

/// Crop to a specific rectangle
pub fn crop(img: &DynamicImage, x: u32, y: u32, width: u32, height: u32) -> Result<DynamicImage> {
    let (iw, ih) = img.dimensions();
    if x + width > iw || y + height > ih {
        return Err(ImageProcessorError::InvalidParameter(format!(
            "Crop region ({x},{y},{width},{height}) exceeds image dimensions ({iw},{ih})"
        )));
    }
    Ok(img.crop_imm(x, y, width, height))
}

/// Crop from the center
pub fn crop_center(img: &DynamicImage, width: u32, height: u32) -> Result<DynamicImage> {
    let (iw, ih) = img.dimensions();
    let x = iw.saturating_sub(width) / 2;
    let y = ih.saturating_sub(height) / 2;
    let w = width.min(iw);
    let h = height.min(ih);
    Ok(img.crop_imm(x, y, w, h))
}

/// Crop anchored to a gravity point
pub fn crop_gravity(
    img: &DynamicImage,
    width: u32,
    height: u32,
    gravity: Gravity,
) -> Result<DynamicImage> {
    let (iw, ih) = img.dimensions();
    let w = width.min(iw);
    let h = height.min(ih);

    let x = match gravity {
        Gravity::NorthWest | Gravity::West | Gravity::SouthWest => 0,
        Gravity::North | Gravity::Center | Gravity::South => iw.saturating_sub(w) / 2,
        Gravity::NorthEast | Gravity::East | Gravity::SouthEast => iw.saturating_sub(w),
    };

    let y = match gravity {
        Gravity::NorthWest | Gravity::North | Gravity::NorthEast => 0,
        Gravity::West | Gravity::Center | Gravity::East => ih.saturating_sub(h) / 2,
        Gravity::SouthWest | Gravity::South | Gravity::SouthEast => ih.saturating_sub(h),
    };

    Ok(img.crop_imm(x, y, w, h))
}

/// Add uniform padding on all sides
pub fn pad_uniform(img: &DynamicImage, padding: u32, color: Rgba<u8>) -> DynamicImage {
    pad(img, padding, padding, padding, padding, color)
}

/// Add padding with individual side control (top, right, bottom, left)
pub fn pad(
    img: &DynamicImage,
    top: u32,
    right: u32,
    bottom: u32,
    left: u32,
    color: Rgba<u8>,
) -> DynamicImage {
    let (iw, ih) = img.dimensions();
    let new_w = iw + left + right;
    let new_h = ih + top + bottom;

    let mut canvas: RgbaImage = ImageBuffer::from_pixel(new_w, new_h, color);
    image::imageops::overlay(&mut canvas, &img.to_rgba8(), left as i64, top as i64);
    DynamicImage::ImageRgba8(canvas)
}

/// Extend canvas to given dimensions, placing original image at gravity position
pub fn extend_canvas(
    img: &DynamicImage,
    new_width: u32,
    new_height: u32,
    gravity: Gravity,
    color: Rgba<u8>,
) -> Result<DynamicImage> {
    let (iw, ih) = img.dimensions();
    if new_width < iw || new_height < ih {
        return Err(ImageProcessorError::InvalidParameter(
            "New canvas dimensions must be >= original image dimensions".into(),
        ));
    }

    let x = match gravity {
        Gravity::NorthWest | Gravity::West | Gravity::SouthWest => 0,
        Gravity::North | Gravity::Center | Gravity::South => (new_width - iw) / 2,
        Gravity::NorthEast | Gravity::East | Gravity::SouthEast => new_width - iw,
    };

    let y = match gravity {
        Gravity::NorthWest | Gravity::North | Gravity::NorthEast => 0,
        Gravity::West | Gravity::Center | Gravity::East => (new_height - ih) / 2,
        Gravity::SouthWest | Gravity::South | Gravity::SouthEast => new_height - ih,
    };

    let mut canvas: RgbaImage = ImageBuffer::from_pixel(new_width, new_height, color);
    image::imageops::overlay(&mut canvas, &img.to_rgba8(), x as i64, y as i64);
    Ok(DynamicImage::ImageRgba8(canvas))
}

/// Rotate image by degrees (supports 90, 180, 270; others require interpolation)
pub fn rotate(img: &DynamicImage, degrees: f32) -> Result<DynamicImage> {
    let normalized = ((degrees % 360.0) + 360.0) % 360.0;
    Ok(match normalized as u32 {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        0 => img.clone(),
        _ => {
            // For arbitrary angles, rotate using imageproc
            let rgba = img.to_rgba8();
            let rotated = imageproc::geometric_transformations::rotate_about_center(
                &rgba,
                degrees.to_radians(),
                imageproc::geometric_transformations::Interpolation::Bilinear,
                Rgba([0, 0, 0, 0]),
            );
            DynamicImage::ImageRgba8(rotated)
        }
    })
}

/// Flip image horizontally
pub fn flip_horizontal(img: &DynamicImage) -> DynamicImage {
    img.fliph()
}

/// Flip image vertically
pub fn flip_vertical(img: &DynamicImage) -> DynamicImage {
    img.flipv()
}

/// Encode image to bytes in the specified format
pub fn to_bytes(img: &DynamicImage, format: OutputFormat) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, format.to_image_format())?;
    Ok(buf.into_inner())
}

/// Load image from file path
pub fn load(path: &str) -> Result<DynamicImage> {
    let img = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?;
    Ok(img)
}

/// Load image from raw bytes
pub fn from_bytes(bytes: &[u8]) -> Result<DynamicImage> {
    Ok(image::load_from_memory(bytes)?)
}

/// Save image to file path, format inferred from extension
pub fn save(img: &DynamicImage, path: &str) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(img.save(path)?)
}

/// Save image to file path with explicit format
pub fn save_as(img: &DynamicImage, path: &str, format: OutputFormat) -> Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(img.save_with_format(path, format.to_image_format())?)
}

/// Compute position (x, y) of an overlay image given gravity and margin
pub fn gravity_position(
    base_width: u32,
    base_height: u32,
    overlay_width: u32,
    overlay_height: u32,
    gravity: Gravity,
    margin_x: u32,
    margin_y: u32,
) -> (i64, i64) {
    let x = match gravity {
        Gravity::NorthWest | Gravity::West | Gravity::SouthWest => margin_x as i64,
        Gravity::North | Gravity::Center | Gravity::South => {
            ((base_width as i64 - overlay_width as i64) / 2).max(0)
        }
        Gravity::NorthEast | Gravity::East | Gravity::SouthEast => {
            (base_width as i64 - overlay_width as i64 - margin_x as i64).max(0)
        }
    };

    let y = match gravity {
        Gravity::NorthWest | Gravity::North | Gravity::NorthEast => margin_y as i64,
        Gravity::West | Gravity::Center | Gravity::East => {
            ((base_height as i64 - overlay_height as i64) / 2).max(0)
        }
        Gravity::SouthWest | Gravity::South | Gravity::SouthEast => {
            (base_height as i64 - overlay_height as i64 - margin_y as i64).max(0)
        }
    };

    (x, y)
}
