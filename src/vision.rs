use anyhow::{Result, bail};
use image::RgbaImage;
use serde::Deserialize;

use crate::geometry_detector;
use crate::layout::ROI_REFERENCE_H;
use crate::slot::{SlotKind, SlotLayout};

#[derive(Debug, Deserialize)]
pub struct Calibration {
    pub reference: ReferenceSize,
    pub roi_ref: Rect,
    pub scale_axis: ScaleAxis,
    #[serde(default)]
    pub anchor: RoiAnchor,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ReferenceSize {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
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

pub struct RoiFrame {
    pub image: RgbaImage,
    pub screen_x: i32,
    pub screen_y: i32,
}

#[derive(Debug)]
pub struct RoiObservation {
    pub image: RgbaImage,
    pub layout: SlotLayout,
    pub screen_x: i32,
    pub screen_y: i32,
    pub slots: Vec<Slot>,
}

#[derive(Debug, Clone)]
pub struct Slot {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub row: u32,
    pub col: u32,
    pub kind: SlotKind,
    pub classification: Option<Classification>,
}

impl Slot {
    pub fn center(&self) -> (u32, u32) {
        (
            self.x.saturating_add(self.w / 2),
            self.y.saturating_add(self.h / 2),
        )
    }

    pub fn center_f32(&self) -> (f32, f32) {
        (
            self.x as f32 + self.w as f32 * 0.5,
            self.y as f32 + self.h as f32 * 0.5,
        )
    }
}

impl RoiObservation {
    pub fn screen_center(&self, slot: &Slot) -> (i32, i32) {
        let (center_x, center_y) = slot.center();
        (
            self.screen_x + center_x as i32,
            self.screen_y + center_y as i32,
        )
    }

    pub fn scale_y(&self) -> f32 {
        self.image.height() as f32 / ROI_REFERENCE_H as f32
    }
}

#[derive(Debug, Clone)]
pub struct Classification {
    pub item_id: String,
    /// Raw fused template similarity used by the acceptance gate.
    pub match_score: f32,
    /// Difference between the best and second-best template scores.
    pub match_margin: f32,
    /// Normalized surplus above the weaker acceptance gate; not a probability.
    pub gate_quality: f32,
}

pub fn resolve_calibration_roi_for_size(
    image_w: u32,
    image_h: u32,
    calibration: &Calibration,
) -> Result<Rect> {
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
        ScaleAxis::Width => image_w as f32 / reference.w as f32,
        ScaleAxis::Height => image_h as f32 / reference.h as f32,
        ScaleAxis::Fit => (image_w as f32 / reference.w as f32)
            .min(image_h as f32 / reference.h as f32),
    };

    let scaled_reference_w = reference.w as f32 * scale;
    let scaled_reference_h = reference.h as f32 * scale;
    let (offset_x, offset_y) = match calibration.anchor {
        RoiAnchor::TopLeft => (0.0, 0.0),
        RoiAnchor::TopCenter => ((image_w as f32 - scaled_reference_w) * 0.5, 0.0),
        RoiAnchor::Center => (
            (image_w as f32 - scaled_reference_w) * 0.5,
            (image_h as f32 - scaled_reference_h) * 0.5,
        ),
    };

    let left = offset_x + rect.x as f32 * scale;
    let top = offset_y + rect.y as f32 * scale;
    let right = offset_x + (rect.x + rect.w) as f32 * scale;
    let bottom = offset_y + (rect.y + rect.h) as f32 * scale;
    if left < 0.0 || top < 0.0 || right > image_w as f32 || bottom > image_h as f32 {
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

    let x = left.round() as u32;
    let y = top.round() as u32;
    let right = right.round() as u32;
    let bottom = bottom.round() as u32;

    Ok(Rect {
        x,
        y,
        w: right.saturating_sub(x).max(1),
        h: bottom.saturating_sub(y).max(1),
    })
}

pub fn detect_slot_layout(
    frame: RoiFrame,
    expected_layout: SlotLayout,
) -> Result<RoiObservation> {
    let RoiFrame {
        image,
        screen_x,
        screen_y,
    } = frame;
    if image.width() == 0 || image.height() == 0 {
        bail!("cannot detect slots in an empty ROI image");
    }
    let slots = geometry_detector::detect(&image, expected_layout)?;
    Ok(RoiObservation {
        image,
        layout: expected_layout,
        screen_x,
        screen_y,
        slots,
    })
}
