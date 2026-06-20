use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba, RgbaImage};

/// Gaussian blur with given sigma
pub fn blur(img: &DynamicImage, sigma: f32) -> DynamicImage {
    img.blur(sigma)
}

/// Sharpen using unsharp mask (sigma = blur radius, threshold = minimum difference to apply)
pub fn sharpen(img: &DynamicImage, sigma: f32, threshold: i32) -> DynamicImage {
    img.unsharpen(sigma, threshold)
}

/// Adjust brightness. Value range: -255 to 255
pub fn brightness(img: &DynamicImage, value: i32) -> DynamicImage {
    img.brighten(value)
}

/// Adjust contrast. Positive values increase contrast, negative decrease it.
pub fn contrast(img: &DynamicImage, value: f32) -> DynamicImage {
    img.adjust_contrast(value)
}

/// Convert image to grayscale (preserves alpha)
pub fn grayscale(img: &DynamicImage) -> DynamicImage {
    img.grayscale()
}

/// Apply sepia tone filter
pub fn sepia(img: &DynamicImage) -> DynamicImage {
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let result: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = rgba.get_pixel(x, y);
        let r = p[0] as f32;
        let g = p[1] as f32;
        let b = p[2] as f32;

        let sr = (r * 0.393 + g * 0.769 + b * 0.189).min(255.0) as u8;
        let sg = (r * 0.349 + g * 0.686 + b * 0.168).min(255.0) as u8;
        let sb = (r * 0.272 + g * 0.534 + b * 0.131).min(255.0) as u8;

        Rgba([sr, sg, sb, p[3]])
    });

    DynamicImage::ImageRgba8(result)
}

/// Invert all color channels (does not affect alpha)
pub fn invert(img: &DynamicImage) -> DynamicImage {
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let result: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = rgba.get_pixel(x, y);
        Rgba([255 - p[0], 255 - p[1], 255 - p[2], p[3]])
    });

    DynamicImage::ImageRgba8(result)
}

/// Adjust saturation. 0.0 = grayscale, 1.0 = original, 2.0 = double saturation
pub fn saturation(img: &DynamicImage, factor: f32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let result: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = rgba.get_pixel(x, y);
        let [r, g, b, a] = p.0;

        let (h_val, s, v) = rgb_to_hsv(r, g, b);
        let new_s = (s * factor).clamp(0.0, 1.0);
        let (nr, ng, nb) = hsv_to_rgb(h_val, new_s, v);

        Rgba([nr, ng, nb, a])
    });

    DynamicImage::ImageRgba8(result)
}

/// Adjust hue rotation in degrees (-360 to 360)
pub fn hue_rotate(img: &DynamicImage, degrees: f32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let result: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = rgba.get_pixel(x, y);
        let [r, g, b, a] = p.0;

        let (h_val, s, v) = rgb_to_hsv(r, g, b);
        let new_h = (h_val + degrees).rem_euclid(360.0);
        let (nr, ng, nb) = hsv_to_rgb(new_h, s, v);

        Rgba([nr, ng, nb, a])
    });

    DynamicImage::ImageRgba8(result)
}

/// Apply a simple box blur (faster than Gaussian for large radii)
pub fn box_blur(img: &DynamicImage, radius: u32) -> DynamicImage {
    if radius == 0 {
        return img.clone();
    }
    // Use Gaussian blur as approximation since imageproc box_filter expects Luma
    img.blur(radius as f32)
}

/// Tint the image with a color (multiply blend mode)
pub fn tint(img: &DynamicImage, color: [u8; 3], strength: f32) -> DynamicImage {
    let strength = strength.clamp(0.0, 1.0);
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let result: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = rgba.get_pixel(x, y);
        Rgba([
            lerp_u8(p[0], multiply(p[0], color[0]), strength),
            lerp_u8(p[1], multiply(p[1], color[1]), strength),
            lerp_u8(p[2], multiply(p[2], color[2]), strength),
            p[3],
        ])
    });

    DynamicImage::ImageRgba8(result)
}

fn multiply(a: u8, b: u8) -> u8 {
    ((a as u16 * b as u16) / 255) as u8
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Apply a pixelation effect (mosaic)
pub fn pixelate(img: &DynamicImage, block_size: u32) -> DynamicImage {
    if block_size <= 1 {
        return img.clone();
    }
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let result: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let bx = (x / block_size) * block_size;
        let by = (y / block_size) * block_size;
        let bx = bx.min(w - 1);
        let by = by.min(h - 1);
        *rgba.get_pixel(bx, by)
    });

    DynamicImage::ImageRgba8(result)
}

/// Posterize: reduce the number of distinct values per channel
pub fn posterize(img: &DynamicImage, levels: u8) -> DynamicImage {
    let levels = levels.max(2);
    let step = 256u32 / levels as u32;
    let (w, h) = img.dimensions();
    let rgba = img.to_rgba8();

    let quantize = |c: u8| -> u8 {
        let level = c as u32 / step;
        (level * step).min(255) as u8
    };

    let result: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let p = rgba.get_pixel(x, y);
        Rgba([quantize(p[0]), quantize(p[1]), quantize(p[2]), p[3]])
    });

    DynamicImage::ImageRgba8(result)
}

// ─── Color space helpers ─────────────────────────────────────────────────────

fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let v = max;
    let s = if max == 0.0 { 0.0 } else { delta / max };

    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };

    (h.rem_euclid(360.0), s, v)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    (
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}
