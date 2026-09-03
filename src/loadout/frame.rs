use anyhow::Result;
use image::RgbaImage;

use crate::capture::{CaptureRegion, CaptureSource};
use crate::vision::{Calibration, luma601_u8, resolve_calibration_roi_for_size};

const ROI_FINGERPRINT_SAMPLES: u32 = 32;

pub fn bind_loadout_region<'a>(
    capture: &'a mut CaptureSource,
    calibration: &Calibration,
) -> Result<CaptureRegion<'a>> {
    let (image_w, image_h) = capture.output_size();
    let roi = resolve_calibration_roi_for_size(image_w, image_h, calibration)?;
    Ok(capture.region(roi))
}

pub fn image_fingerprint(rgba: &RgbaImage) -> Vec<u8> {
    let samples = ROI_FINGERPRINT_SAMPLES;
    let mut fingerprint = Vec::with_capacity((samples * samples) as usize);

    for row in 0..samples {
        for col in 0..samples {
            let x = ((col as f32 + 0.5) * rgba.width() as f32 / samples as f32) as u32;
            let y = ((row as f32 + 0.5) * rgba.height() as f32 / samples as f32) as u32;
            let x = x.min(rgba.width().saturating_sub(1));
            let y = y.min(rgba.height().saturating_sub(1));

            let [r, g, b, _] = rgba.get_pixel(x, y).0;
            let luma = luma601_u8(r, g, b);
            fingerprint.push(luma);
        }
    }

    fingerprint
}

pub fn fingerprint_distance(left: &[u8], right: &[u8]) -> f32 {
    if left.is_empty() || left.len() != right.len() {
        return f32::INFINITY;
    }

    let total: u32 = left
        .iter()
        .zip(right)
        .map(|(left, right)| left.abs_diff(*right) as u32)
        .sum();
    total as f32 / left.len() as f32
}
