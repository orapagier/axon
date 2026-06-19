use crate::error::{ImageProcessorError, Result};
use image::{DynamicImage, GenericImageView, Rgba};
use std::collections::HashMap;
use std::io::BufReader;

/// EXIF metadata extracted from an image file
#[derive(Debug, Default, Clone)]
pub struct ExifData {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub datetime: Option<String>,
    pub orientation: Option<u32>,
    pub gps_latitude: Option<f64>,
    pub gps_longitude: Option<f64>,
    pub iso: Option<u32>,
    pub exposure_time: Option<String>,
    pub f_number: Option<f64>,
    pub software: Option<String>,
}

/// Read EXIF metadata from an image file
pub fn read_exif(path: &str) -> Result<ExifData> {
    let file = std::fs::File::open(path).map_err(|e| ImageProcessorError::IoError(e))?;
    let mut bufreader = BufReader::new(file);

    let exif_reader = exif::Reader::new();
    let exif = exif_reader
        .read_from_container(&mut bufreader)
        .map_err(|e| ImageProcessorError::ExifError(e.to_string()))?;

    let mut data = ExifData::default();

    for field in exif.fields() {
        match field.tag {
            exif::Tag::Make => {
                data.make = Some(field.display_value().to_string());
            }
            exif::Tag::Model => {
                data.model = Some(field.display_value().to_string());
            }
            exif::Tag::DateTime => {
                data.datetime = Some(field.display_value().to_string());
            }
            exif::Tag::Orientation => {
                if let exif::Value::Short(ref v) = field.value {
                    data.orientation = v.first().map(|&x| x as u32);
                }
            }
            exif::Tag::PhotographicSensitivity => {
                if let exif::Value::Short(ref v) = field.value {
                    data.iso = v.first().map(|&x| x as u32);
                }
            }
            exif::Tag::ExposureTime => {
                data.exposure_time = Some(field.display_value().to_string());
            }
            exif::Tag::FNumber => {
                if let exif::Value::Rational(ref v) = field.value {
                    data.f_number = v.first().map(|r| r.num as f64 / r.denom as f64);
                }
            }
            exif::Tag::Software => {
                data.software = Some(field.display_value().to_string());
            }
            exif::Tag::PixelXDimension => {
                if let exif::Value::Long(ref v) = field.value {
                    data.width = v.first().copied();
                }
            }
            exif::Tag::PixelYDimension => {
                if let exif::Value::Long(ref v) = field.value {
                    data.height = v.first().copied();
                }
            }
            exif::Tag::GPSLatitude => {
                if let exif::Value::Rational(ref v) = field.value {
                    if v.len() >= 3 {
                        let deg = v[0].num as f64 / v[0].denom as f64;
                        let min = v[1].num as f64 / v[1].denom as f64;
                        let sec = v[2].num as f64 / v[2].denom as f64;
                        data.gps_latitude = Some(deg + min / 60.0 + sec / 3600.0);
                    }
                }
            }
            exif::Tag::GPSLongitude => {
                if let exif::Value::Rational(ref v) = field.value {
                    if v.len() >= 3 {
                        let deg = v[0].num as f64 / v[0].denom as f64;
                        let min = v[1].num as f64 / v[1].denom as f64;
                        let sec = v[2].num as f64 / v[2].denom as f64;
                        data.gps_longitude = Some(deg + min / 60.0 + sec / 3600.0);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(data)
}

/// Compute the average brightness (luminance) of a rectangular region
/// Returns 0 (black) to 255 (white)
pub fn average_brightness(img: &DynamicImage, x: u32, y: u32, width: u32, height: u32) -> u8 {
    let (iw, ih) = img.dimensions();
    let x_end = (x + width).min(iw);
    let y_end = (y + height).min(ih);
    let rgba = img.to_rgba8();

    let mut total: u64 = 0;
    let mut count: u64 = 0;

    for py in y..y_end {
        for px in x..x_end {
            let p = rgba.get_pixel(px, py);
            // Standard luminance formula
            let lum = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
            total += lum as u64;
            count += 1;
        }
    }

    if count == 0 {
        128
    } else {
        (total / count) as u8
    }
}

/// Choose white or black text color based on background brightness
/// Samples the region where text will be placed
pub fn auto_text_color(img: &DynamicImage, x: u32, y: u32, width: u32, height: u32) -> Rgba<u8> {
    let brightness = average_brightness(img, x, y, width, height);
    if brightness > 128 {
        Rgba([0, 0, 0, 255]) // dark text on light background
    } else {
        Rgba([255, 255, 255, 255]) // white text on dark background
    }
}

/// Compute highly readable text color that fits the background's dominant color dynamically
pub fn auto_text_color_from_dominant(img: &DynamicImage) -> Rgba<u8> {
    let dominant = dominant_color(img);
    let lum = 0.299 * dominant[0] as f64 + 0.587 * dominant[1] as f64 + 0.114 * dominant[2] as f64;

    // Complementary color
    let comp_r = 255 - dominant[0];
    let comp_g = 255 - dominant[1];
    let comp_b = 255 - dominant[2];

    if lum > 190.0 {
        // Bright background -> needs dark text.
        // We use a very deep, dark tint of the complementary color to make it look cohesive.
        Rgba([
            (comp_r as f32 * 0.15) as u8,
            (comp_g as f32 * 0.15) as u8,
            (comp_b as f32 * 0.15) as u8,
            255,
        ])
    } else {
        // Dark/Mixed background -> needs bright text.
        // Keep it very bright (close to white) with just a tiny hint of complementary color
        Rgba([
            255 - ((255 - comp_r as i32) / 12) as u8,
            255 - ((255 - comp_g as i32) / 12) as u8,
            255 - ((255 - comp_b as i32) / 12) as u8,
            255,
        ])
    }
}

/// Extract the dominant color from an image using grid sampling + quantization
pub fn dominant_color(img: &DynamicImage) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    // Sample a grid of pixels (skip transparent ones)
    let step_x = (w / 32).max(1);
    let step_y = (h / 32).max(1);

    // Quantize colors to reduce palette (divide each channel into buckets of 32)
    let mut buckets: HashMap<(u8, u8, u8), u32> = HashMap::new();

    for y in (0..h).step_by(step_y as usize) {
        for x in (0..w).step_by(step_x as usize) {
            let p = rgba.get_pixel(x, y);
            if p[3] < 128 {
                continue; // skip mostly transparent pixels
            }
            let qr = p[0] / 32 * 32;
            let qg = p[1] / 32 * 32;
            let qb = p[2] / 32 * 32;
            *buckets.entry((qr, qg, qb)).or_insert(0) += 1;
        }
    }

    buckets
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|((r, g, b), _)| [r, g, b])
        .unwrap_or([128, 128, 128])
}

/// Check if an image is mostly dark (average brightness < 100)
pub fn is_dark(img: &DynamicImage) -> bool {
    let (w, h) = img.dimensions();
    average_brightness(img, 0, 0, w, h) < 100
}

/// Compute a simple perceptual hash (pHash) for image similarity comparison
/// Returns a 64-bit hash. XOR two hashes and count set bits to get similarity distance.
pub fn phash(img: &DynamicImage) -> u64 {
    // Resize to 8x8, convert to grayscale
    let small = img
        .resize_exact(8, 8, image::imageops::FilterType::Lanczos3)
        .grayscale();
    let pixels: Vec<u8> = small.to_luma8().pixels().map(|p| p[0]).collect();

    // Compute mean
    let mean = pixels.iter().map(|&p| p as u32).sum::<u32>() / pixels.len() as u32;

    // Build hash: 1 if pixel >= mean, 0 otherwise
    let mut hash: u64 = 0;
    for (i, &pixel) in pixels.iter().enumerate() {
        if (pixel as u32) >= mean {
            hash |= 1 << i;
        }
    }

    hash
}

/// Count differing bits between two pHashes (Hamming distance)
/// 0 = identical, <10 = similar, >20 = different images
pub fn phash_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Get pixel dimensions of an image without fully decoding it
pub fn image_dimensions(path: &str) -> Result<(u32, u32)> {
    Ok(image::image_dimensions(path)?)
}

/// Detect if the image has transparency (any pixel with alpha < 255)
pub fn has_transparency(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(buf) => buf.pixels().any(|p| p[3] < 255),
        DynamicImage::ImageRgba16(buf) => buf.pixels().any(|p| p[3] < 65535),
        _ => false,
    }
}

/// Convert RGBA image to JPEG-safe RGB (flatten transparency onto a background color)
pub fn flatten_transparency(img: &DynamicImage, bg: [u8; 3]) -> DynamicImage {
    use image::{ImageBuffer, Rgb};
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let rgb: image::RgbImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = rgba.get_pixel(x, y);
        let alpha = p[3] as f32 / 255.0;
        Rgb([
            (p[0] as f32 * alpha + bg[0] as f32 * (1.0 - alpha)) as u8,
            (p[1] as f32 * alpha + bg[1] as f32 * (1.0 - alpha)) as u8,
            (p[2] as f32 * alpha + bg[2] as f32 * (1.0 - alpha)) as u8,
        ])
    });

    DynamicImage::ImageRgb8(rgb)
}

/// Estimate file size in bytes for a given format and image
pub fn estimate_output_size(
    img: &DynamicImage,
    format: crate::canvas::OutputFormat,
    quality: u8,
) -> usize {
    let (w, h) = img.dimensions();
    let pixels = (w * h) as usize;
    match format {
        crate::canvas::OutputFormat::Png => pixels * 4 / 4, // rough estimate after deflate
        crate::canvas::OutputFormat::Jpeg => pixels * quality as usize / 500,
        crate::canvas::OutputFormat::WebP => pixels * quality as usize / 600,
    }
}
