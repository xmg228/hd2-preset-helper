use anyhow::{Result, ensure};
use image::RgbaImage;

use super::matcher::SemanticImage;
use super::semantic_extractor::SemanticExtraction;
use super::{ImageSample, Slot, crop_slot_sample};

pub(super) const INTERIOR_HALF_WIDTH: f32 = 0.425;
pub(super) const MATCH_THRESHOLD: f64 = 0.35;
pub(super) const AVAILABLE_BRIGHTNESS_RATIO: f32 = 0.80;
const REGULAR_HEX_HALF_HEIGHT_RATIO: f32 = 0.866_025_4;
const PALETTE_SEED_FRACTION: usize = 10;
const PALETTE_ITERATIONS: usize = 4;

pub(crate) fn crop_sample(
    image: &RgbaImage,
    slot: &Slot,
    physical_size: f32,
) -> Result<ImageSample> {
    let mut sample = crop_slot_sample(image, slot, physical_size)?;
    let center_x = sample.geometry.center_x;
    let center_y = sample.geometry.center_y;
    for (x, y, pixel) in sample.image.enumerate_pixels_mut() {
        let normalized_x = (x as f32 + 0.5 - center_x) / physical_size;
        let normalized_y = (y as f32 + 0.5 - center_y) / physical_size;
        pixel[3] = if interior_contains(normalized_x, normalized_y) {
            255
        } else {
            0
        };
    }

    Ok(sample)
}

pub(super) fn extract(sample: &ImageSample) -> Result<SemanticExtraction> {
    let width = sample.image.width() as usize;
    let height = sample.image.height() as usize;
    ensure!(width > 0 && height > 0, "booster sample is empty");

    let valid = sample
        .image
        .pixels()
        .filter(|pixel| pixel[3] > 0)
        .map(|pixel| {
            [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
            ]
        })
        .collect::<Vec<_>>();
    ensure!(valid.len() >= 64, "booster interior is unexpectedly small");

    let (dark_endpoint, yellow_endpoint) = estimate_palette(&valid);
    let direction = subtract(yellow_endpoint, dark_endpoint);
    let norm = dot(direction, direction);
    ensure!(norm > 1.0e-4, "booster interior has insufficient contrast");

    let mut glyph_mass = 0.0f64;
    let pixels = sample
        .image
        .pixels()
        .map(|pixel| {
            if pixel[3] == 0 {
                return [0.0, 0.0];
            }
            let rgb = [
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
            ];
            let yellow = (dot(subtract(rgb, dark_endpoint), direction) / norm).clamp(0.0, 1.0);
            let glyph = 1.0 - yellow;
            glyph_mass += glyph as f64;
            // The matcher stores its high-reliability channel second ([CLASS, WHITE]).
            [0.0, glyph]
        })
        .collect::<Vec<_>>();
    let image = SemanticImage::new(width, height, pixels)?
        .with_center(sample.geometry.center_x, sample.geometry.center_y);

    Ok(SemanticExtraction {
        image,
        mode: "booster_glyph",
        primary_endpoint: dark_endpoint,
        secondary_endpoint: yellow_endpoint,
        primary_mass: glyph_mass as f32,
        secondary_mass: 0.0,
    })
}

pub(super) fn glyph_response(sample: &ImageSample) -> Result<Vec<u8>> {
    Ok(extract(sample)?.image.secondary_response_u8())
}

pub(super) fn yellow_luma(extraction: &SemanticExtraction) -> f32 {
    luma(extraction.secondary_endpoint)
}

fn estimate_palette(pixels: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut ordered = pixels.to_vec();
    ordered.sort_unstable_by(|left, right| luma(*left).total_cmp(&luma(*right)));
    let seed_count = (ordered.len() / PALETTE_SEED_FRACTION).max(1);
    let mut dark = mean_rgb(&ordered[..seed_count]);
    let mut yellow = mean_rgb(&ordered[ordered.len() - seed_count..]);

    for _ in 0..PALETTE_ITERATIONS {
        let mut sums = [[0.0f64; 3]; 2];
        let mut counts = [0u32; 2];
        for &pixel in pixels {
            let cluster =
                usize::from(squared_distance(pixel, yellow) < squared_distance(pixel, dark));
            counts[cluster] += 1;
            for channel in 0..3 {
                sums[cluster][channel] += pixel[channel] as f64;
            }
        }
        if counts[0] > 0 {
            dark = std::array::from_fn(|channel| (sums[0][channel] / counts[0] as f64) as f32);
        }
        if counts[1] > 0 {
            yellow = std::array::from_fn(|channel| (sums[1][channel] / counts[1] as f64) as f32);
        }
    }

    if luma(dark) > luma(yellow) {
        (yellow, dark)
    } else {
        (dark, yellow)
    }
}

fn interior_contains(x: f32, y: f32) -> bool {
    let half_height = INTERIOR_HALF_WIDTH * REGULAR_HEX_HALF_HEIGHT_RATIO;
    let vertical = y.abs() / half_height;
    let row_half_width = INTERIOR_HALF_WIDTH * (1.0 - 0.5 * vertical);
    vertical <= 1.0 && x.abs() <= row_half_width
}

fn mean_rgb(pixels: &[[f32; 3]]) -> [f32; 3] {
    std::array::from_fn(|channel| {
        pixels.iter().map(|pixel| pixel[channel]).sum::<f32>() / pixels.len() as f32
    })
}

fn luma(rgb: [f32; 3]) -> f32 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|channel| left[channel] - right[channel])
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn squared_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    dot(subtract(left, right), subtract(left, right))
}
