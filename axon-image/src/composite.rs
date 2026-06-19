use crate::canvas::{gravity_position, Gravity};
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgba, RgbaImage};

/// Direction for gradient overlays
#[derive(Debug, Clone, Copy)]
pub enum GradientDirection {
    TopToBottom,
    BottomToTop,
    LeftToRight,
    RightToLeft,
    TopLeftToBottomRight,
    BottomLeftToTopRight,
}

/// Overlay one image on top of another at a specific position
pub fn overlay(base: &DynamicImage, overlay: &DynamicImage, x: i64, y: i64) -> DynamicImage {
    let mut result = base.to_rgba8();
    image::imageops::overlay(&mut result, &overlay.to_rgba8(), x, y);
    DynamicImage::ImageRgba8(result)
}

/// Overlay image using gravity and margin for positioning
pub fn overlay_gravity(
    base: &DynamicImage,
    overlay_img: &DynamicImage,
    gravity: Gravity,
    margin_x: u32,
    margin_y: u32,
) -> DynamicImage {
    let (bw, bh) = base.dimensions();
    let (ow, oh) = overlay_img.dimensions();
    let (x, y) = gravity_position(bw, bh, ow, oh, gravity, margin_x, margin_y);
    overlay(base, overlay_img, x, y)
}

/// Blend two images together at a given alpha (0.0 = full base, 1.0 = full overlay)
pub fn blend(base: &DynamicImage, top: &DynamicImage, alpha: f32) -> DynamicImage {
    let (w, h) = base.dimensions();
    let base_rgba = base.to_rgba8();
    let top_rgba = top
        .resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let alpha = alpha.clamp(0.0, 1.0);

    let blended: RgbaImage = ImageBuffer::from_fn(w, h, |x, y| {
        let bp = base_rgba.get_pixel(x, y);
        let tp = top_rgba.get_pixel(x, y);
        Rgba([
            lerp_u8(bp[0], tp[0], alpha),
            lerp_u8(bp[1], tp[1], alpha),
            lerp_u8(bp[2], tp[2], alpha),
            lerp_u8(bp[3], tp[3], alpha),
        ])
    });

    DynamicImage::ImageRgba8(blended)
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

/// Draw a semi-transparent filled rectangle on the image
pub fn semi_transparent_rect(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: [u8; 3],
    opacity: f32,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    let (iw, ih) = img.dimensions();
    let x_end = (x + width).min(iw);
    let y_end = (y + height).min(ih);

    for py in y..y_end {
        for px in x..x_end {
            let orig = img.get_pixel(px, py);
            let blended = Rgba([
                lerp_u8(orig[0], color[0], opacity),
                lerp_u8(orig[1], color[1], opacity),
                lerp_u8(orig[2], color[2], opacity),
                orig[3],
            ]);
            img.put_pixel(px, py, blended);
        }
    }
}

/// Add a semi-transparent color overlay to the entire image
pub fn color_overlay(img: &DynamicImage, color: [u8; 3], opacity: f32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let mut result = img.to_rgba8();
    semi_transparent_rect(&mut result, 0, 0, w, h, color, opacity);
    DynamicImage::ImageRgba8(result)
}

/// Add a linear gradient overlay to the image
pub fn gradient_overlay(
    img: &DynamicImage,
    color_start: [u8; 4],
    color_end: [u8; 4],
    direction: GradientDirection,
) -> DynamicImage {
    let (w, h) = img.dimensions();
    let mut result = img.to_rgba8();

    for y in 0..h {
        for x in 0..w {
            let t = match direction {
                GradientDirection::TopToBottom => y as f32 / h as f32,
                GradientDirection::BottomToTop => 1.0 - y as f32 / h as f32,
                GradientDirection::LeftToRight => x as f32 / w as f32,
                GradientDirection::RightToLeft => 1.0 - x as f32 / w as f32,
                GradientDirection::TopLeftToBottomRight => {
                    (x as f32 / w as f32 + y as f32 / h as f32) / 2.0
                }
                GradientDirection::BottomLeftToTopRight => {
                    (x as f32 / w as f32 + (1.0 - y as f32 / h as f32)) / 2.0
                }
            };

            let overlay_color = Rgba([
                lerp_u8(color_start[0], color_end[0], t),
                lerp_u8(color_start[1], color_end[1], t),
                lerp_u8(color_start[2], color_end[2], t),
                lerp_u8(color_start[3], color_end[3], t),
            ]);

            let orig = result.get_pixel(x, y);
            let alpha = overlay_color[3] as f32 / 255.0;
            let blended = Rgba([
                lerp_u8(orig[0], overlay_color[0], alpha),
                lerp_u8(orig[1], overlay_color[1], alpha),
                lerp_u8(orig[2], overlay_color[2], alpha),
                orig[3],
            ]);
            result.put_pixel(x, y, blended);
        }
    }

    DynamicImage::ImageRgba8(result)
}

/// Stamp a watermark onto an image with given gravity, margin, and opacity
pub fn watermark(
    base: &DynamicImage,
    mark: &DynamicImage,
    gravity: Gravity,
    margin_x: u32,
    margin_y: u32,
    opacity: f32,
) -> DynamicImage {
    let opacity = opacity.clamp(0.0, 1.0);
    let (bw, bh) = base.dimensions();
    let (mw, mh) = mark.dimensions();

    // Apply opacity to watermark
    let mut mark_rgba = mark.to_rgba8();
    for pixel in mark_rgba.pixels_mut() {
        pixel[3] = (pixel[3] as f32 * opacity) as u8;
    }

    let (x, y) = gravity_position(bw, bh, mw, mh, gravity, margin_x, margin_y);

    let mut result = base.to_rgba8();
    image::imageops::overlay(&mut result, &mark_rgba, x, y);
    DynamicImage::ImageRgba8(result)
}

/// Add a text watermark (rendered externally) at gravity position with opacity
pub fn watermark_image_at(
    base: &mut RgbaImage,
    mark: &RgbaImage,
    gravity: Gravity,
    margin_x: u32,
    margin_y: u32,
    opacity: f32,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    let (bw, bh) = base.dimensions();
    let (mw, mh) = mark.dimensions();
    let (ox, oy) = gravity_position(bw, bh, mw, mh, gravity, margin_x, margin_y);

    for (mx, my, mp) in mark.enumerate_pixels() {
        let bx = ox as u32 + mx;
        let by = oy as u32 + my;

        if bx >= bw || by >= bh {
            continue;
        }

        let src_alpha = (mp[3] as f32 * opacity) / 255.0;
        if src_alpha == 0.0 {
            continue;
        }

        let dst = base.get_pixel_mut(bx, by);
        dst[0] = lerp_u8(dst[0], mp[0], src_alpha);
        dst[1] = lerp_u8(dst[1], mp[1], src_alpha);
        dst[2] = lerp_u8(dst[2], mp[2], src_alpha);
    }
}

/// Create a dark vignette border effect
pub fn vignette(img: &DynamicImage, strength: f32) -> DynamicImage {
    let strength = strength.clamp(0.0, 1.0);
    let (w, h) = img.dimensions();
    let mut result = img.to_rgba8();

    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let max_dist = (cx * cx + cy * cy).sqrt();

    for (x, y, pixel) in result.enumerate_pixels_mut() {
        let dx = x as f32 - cx;
        let dy = y as f32 - cy;
        let dist = (dx * dx + dy * dy).sqrt() / max_dist;
        let factor = 1.0 - (dist * strength).min(1.0);

        pixel[0] = (pixel[0] as f32 * factor) as u8;
        pixel[1] = (pixel[1] as f32 * factor) as u8;
        pixel[2] = (pixel[2] as f32 * factor) as u8;
    }

    DynamicImage::ImageRgba8(result)
}

/// Stack two images vertically
pub fn stack_vertical(
    top: &DynamicImage,
    bottom: &DynamicImage,
    gap: u32,
    gap_color: Rgba<u8>,
) -> DynamicImage {
    let (tw, th) = top.dimensions();
    let (bw, bh) = bottom.dimensions();
    let new_w = tw.max(bw);
    let new_h = th + gap + bh;

    let mut canvas: RgbaImage = ImageBuffer::from_pixel(new_w, new_h, gap_color);
    image::imageops::overlay(&mut canvas, &top.to_rgba8(), 0, 0);
    image::imageops::overlay(&mut canvas, &bottom.to_rgba8(), 0, (th + gap) as i64);
    DynamicImage::ImageRgba8(canvas)
}

/// Stack two images horizontally
pub fn stack_horizontal(
    left: &DynamicImage,
    right: &DynamicImage,
    gap: u32,
    gap_color: Rgba<u8>,
) -> DynamicImage {
    let (lw, lh) = left.dimensions();
    let (rw, rh) = right.dimensions();
    let new_w = lw + gap + rw;
    let new_h = lh.max(rh);

    let mut canvas: RgbaImage = ImageBuffer::from_pixel(new_w, new_h, gap_color);
    image::imageops::overlay(&mut canvas, &left.to_rgba8(), 0, 0);
    image::imageops::overlay(&mut canvas, &right.to_rgba8(), (lw + gap) as i64, 0);
    DynamicImage::ImageRgba8(canvas)
}

/// Apply a rounded corner mask (alpha cutout) to an image
pub fn rounded_corners(img: &DynamicImage, radius: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    let mut result = img.to_rgba8();
    let r = radius as f32;

    for y in 0..h {
        for x in 0..w {
            let in_corner = is_outside_rounded_rect(x, y, w, h, r);
            if in_corner {
                result.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            }
        }
    }

    DynamicImage::ImageRgba8(result)
}

fn is_outside_rounded_rect(x: u32, y: u32, w: u32, h: u32, r: f32) -> bool {
    let fx = x as f32;
    let fy = y as f32;
    let fw = w as f32;
    let fh = h as f32;

    let corners = [
        (r, r),                       // top-left
        (fw - r - 1.0, r),            // top-right
        (r, fh - r - 1.0),            // bottom-left
        (fw - r - 1.0, fh - r - 1.0), // bottom-right
    ];

    for (cx, cy) in corners {
        if fx < cx + r && fy < cy + r && fx > cx - r && fy > cy - r {
            let dx = fx - cx;
            let dy = fy - cy;
            if dx * dx + dy * dy > r * r {
                return true;
            }
        }
    }

    false
}
