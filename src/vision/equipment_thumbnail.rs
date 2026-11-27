//! Optional card artwork from two views of the same equipment slot.

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::{Rgba, RgbaImage, imageops};

use crate::image_rect::ImageRect;

const MAX_THUMBNAIL_EDGE: u32 = 128;
const OPAQUE_COLOR_DIFF: u8 = 1;
const CLEAR_COLOR_DIFF: u8 = 18;
const SIGNIFICANT_CHANGE: u8 = 8;

fn rgb_difference(a: &Rgba<u8>, b: &Rgba<u8>) -> u8 {
    a[0].abs_diff(b[0])
        .max(a[1].abs_diff(b[1]))
        .max(a[2].abs_diff(b[2]))
}

pub(crate) enum IgnoreRegion {
    /// Coordinates normalized to the cropped card interior.
    Rect {
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    },
    /// Both center and radius are in the captured frame's pixels.
    Circle { center: [f64; 2], radius: f64 },
}

impl IgnoreRegion {
    fn contains(&self, bounds: ImageRect, x: u32, y: u32) -> bool {
        match *self {
            Self::Rect {
                left,
                top,
                right,
                bottom,
            } => {
                let u = (x as f32 + 0.5) / bounds.w as f32;
                let v = (y as f32 + 0.5) / bounds.h as f32;
                u >= left && u < right && v >= top && v < bottom
            }
            Self::Circle { center, radius } => {
                let dx = f64::from(bounds.x + x) + 0.5 - center[0];
                let dy = f64::from(bounds.y + y) + 0.5 - center[1];
                dx * dx + dy * dy <= radius * radius
            }
        }
    }
}

pub(crate) struct EquipmentCard {
    bounds: ImageRect,
    image: RgbaImage,
    yellow: bool,
}

impl EquipmentCard {
    /// Record native pixels at fixed screen coordinates for both stability and matting.
    pub fn capture(frame: &RgbaImage, rect: [f64; 4], yellow: bool) -> Option<Self> {
        let [left, top, right, bottom] = rect;
        let inset = 0.035 * (right - left).min(bottom - top);
        let [x0, y0, x1, y1] = [
            (left + inset).ceil(),
            (top + inset).ceil(),
            (right - inset).floor(),
            (bottom - inset).floor(),
        ];
        if !(inset > 0.0
            && x0 >= 0.0
            && y0 >= 0.0
            && x1 <= f64::from(frame.width())
            && y1 <= f64::from(frame.height())
            && x1 > x0
            && y1 > y0)
        {
            return None;
        }
        let bounds = ImageRect {
            x: x0 as u32,
            y: y0 as u32,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        };
        Some(Self {
            bounds,
            image: imageops::crop_imm(frame, bounds.x, bounds.y, bounds.w, bounds.h).to_image(),
            yellow,
        })
    }

    /// The caller has confirmed this is another frame with the same focus state.
    pub fn stable_in_frame(&self, frame: &RgbaImage) -> bool {
        let (width, height) = self.image.dimensions();
        let side = (width.min(height) * 31 / 100).max(1);
        let left = (width - side) / 2;
        let top = (height - side) / 2;
        // Small loading-arc motion should not be diluted by the rest of the card.
        let limit = (side * side / 100).max(1);
        let mut changed = 0;
        for y in top..top + side {
            for x in left..left + side {
                let Some(current) = frame.get_pixel_checked(self.bounds.x + x, self.bounds.y + y)
                else {
                    return false;
                };
                let reference = self.image.get_pixel(x, y);
                if rgb_difference(current, reference) > SIGNIFICANT_CHANGE {
                    changed += 1;
                    if changed > limit {
                        return false;
                    }
                }
            }
        }
        true
    }

    pub fn thumbnail(normal: &Self, yellow: &Self, ignored: &[IgnoreRegion]) -> Option<RgbaImage> {
        if normal.yellow || !yellow.yellow || normal.bounds != yellow.bounds {
            return None;
        }
        let (width, height) = normal.image.dimensions();
        let mut image = RgbaImage::new(width, height);
        let mut valid = 0;
        let mut clear = 0;
        let mut has_foreground = false;
        let span = u16::from(CLEAR_COLOR_DIFF - OPAQUE_COLOR_DIFF);
        for (x, y, output) in image.enumerate_pixels_mut() {
            if ignored
                .iter()
                .any(|region| region.contains(normal.bounds, x, y))
            {
                continue;
            }
            valid += 1;
            let n = normal.image.get_pixel(x, y);
            let f = yellow.image.get_pixel(x, y);
            let difference = rgb_difference(n, f);
            if difference >= CLEAR_COLOR_DIFF {
                clear += 1;
                continue;
            }
            has_foreground |= difference <= OPAQUE_COLOR_DIFF;
            // Leave intermediate differences partially opaque to retain soft edges.
            let alpha = (u16::from(CLEAR_COLOR_DIFF - difference.max(OPAQUE_COLOR_DIFF)) * 255
                + span / 2)
                / span;
            *output = Rgba([n[0], n[1], n[2], alpha as u8]);
        }
        // Require evidence of both foreground and background, regardless of the item's shape.
        if !has_foreground || clear * 20 < valid {
            return None;
        }
        let edge = width.max(height);
        if edge <= MAX_THUMBNAIL_EDGE {
            return Some(image);
        }
        let scaled = |side: u32| ((side * MAX_THUMBNAIL_EDGE + edge / 2) / edge).max(1);
        let mut thumbnail = RgbaImage::new(scaled(width), scaled(height));
        // The resizer filters using premultiplied colors and returns straight RGBA.
        Resizer::new()
            .resize(
                &image,
                &mut thumbnail,
                &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Hamming)),
            )
            .ok()?;
        Some(thumbnail)
    }
}
