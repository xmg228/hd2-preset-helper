//! Resolve reference UI coordinates into a native capture region without losing crop phase.
use crate::image_rect::ImageRect;
use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Calibration {
    pub reference: ReferenceSize,
    pub roi_ref: ImageRect,
    pub scale_axis: ScaleAxis,
    #[serde(default)]
    pub anchor: RoiAnchor,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ReferenceSize {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleAxis {
    Width,
    Height,
    Fit,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoiAnchor {
    #[default]
    TopLeft,
    TopCenter,
    Center,
}

#[derive(Debug, Clone, Copy)]
pub struct RoiGeometry {
    pub scale: f64,
    pub logical_origin_x: f64,
    pub logical_origin_y: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct ResolvedRoi {
    pub rect: ImageRect,
    pub geometry: RoiGeometry,
}

pub fn resolve_calibration_roi_for_size(
    image_w: u32,
    image_h: u32,
    calibration: &Calibration,
) -> Result<ResolvedRoi> {
    let reference = calibration.reference;
    let rect = calibration.roi_ref;
    if reference.w == 0 || reference.h == 0 {
        bail!(
            "reference width and height must be greater than zero, got {}x{}",
            reference.w,
            reference.h
        );
    }
    if rect.w == 0 || rect.h == 0 {
        bail!("roi_ref width and height must be greater than zero");
    }
    if rect.x + rect.w > reference.w || rect.y + rect.h > reference.h {
        bail!(
            "roi_ref ({},{},{},{}) is outside reference {}x{}",
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            reference.w,
            reference.h
        );
    }

    let scale = match calibration.scale_axis {
        ScaleAxis::Width => image_w as f64 / reference.w as f64,
        ScaleAxis::Height => image_h as f64 / reference.h as f64,
        ScaleAxis::Fit => {
            (image_w as f64 / reference.w as f64).min(image_h as f64 / reference.h as f64)
        }
    };

    let scaled_reference_w = reference.w as f64 * scale;
    let scaled_reference_h = reference.h as f64 * scale;
    let (offset_x, offset_y) = match calibration.anchor {
        RoiAnchor::TopLeft => (0.0, 0.0),
        RoiAnchor::TopCenter => ((image_w as f64 - scaled_reference_w) * 0.5, 0.0),
        RoiAnchor::Center => (
            (image_w as f64 - scaled_reference_w) * 0.5,
            (image_h as f64 - scaled_reference_h) * 0.5,
        ),
    };

    let left = offset_x + rect.x as f64 * scale;
    let top = offset_y + rect.y as f64 * scale;
    let right = offset_x + (rect.x + rect.w) as f64 * scale;
    let bottom = offset_y + (rect.y + rect.h) as f64 * scale;
    if left < 0.0 || top < 0.0 || right > image_w as f64 || bottom > image_h as f64 {
        bail!(
            "scaled ROI ({:.1},{:.1},{:.1},{:.1}) is outside image {}x{}",
            left,
            top,
            right - left,
            bottom - top,
            image_w,
            image_h
        );
    }

    // Keep the complete continuous Page ROI inside the integer capture rect.
    // The fractional offset is preserved in RoiGeometry for native sampling.
    let x = left.floor() as u32;
    let y = top.floor() as u32;
    let right = right.ceil() as u32;
    let bottom = bottom.ceil() as u32;

    Ok(ResolvedRoi {
        rect: ImageRect {
            x,
            y,
            w: right.saturating_sub(x).max(1),
            h: bottom.saturating_sub(y).max(1),
        },
        geometry: RoiGeometry {
            scale,
            logical_origin_x: left - x as f64,
            logical_origin_y: top - y as f64,
        },
    })
}
