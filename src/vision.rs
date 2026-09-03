mod classifier;
mod color;
mod geometry;
mod recognizer;

use anyhow::{Result, bail};
use image::RgbaImage;
use serde::Deserialize;

use crate::image_rect::ImageRect;
use crate::item::ItemKind;

pub use color::{icon_likeness, luma601_u8};
pub use recognizer::RecognizerRuntime;

const ROI_REFERENCE_W: u32 = 576;
pub const ROI_REFERENCE_H: u32 = 832;
const ROI_REFERENCE_W_F32: f32 = 576.0;
const ROI_REFERENCE_H_F32: f32 = 832.0;

const SLOT_SIZE_I32: i32 = 104;

const LIST_COLS: [i32; 4] = [77, 190, 304, 417];
const HOME_COLS: [i32; 4] = [11, 124, 237, 350];
const HOME_BOOSTER_X: i32 = 457;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Stratagem,
    StratagemEmpty,
    Booster,
    /// Special "no booster" list cell; occupies the grid but has no item template.
    NoBoosterOption,
    /// Filled booster slot on the loadout home screen.
    HomeBooster,
    /// Empty booster slot on the loadout home screen.
    HomeBoosterEmpty,
}

impl SlotKind {
    pub const fn classification_kind(self) -> Option<ItemKind> {
        match self {
            Self::Stratagem => Some(ItemKind::Stratagem),
            Self::Booster | Self::HomeBooster => Some(ItemKind::Booster),
            Self::StratagemEmpty | Self::NoBoosterOption | Self::HomeBoosterEmpty => None,
        }
    }

    pub const fn is_selectable_item_for(self, item_kind: ItemKind) -> bool {
        matches!(
            (item_kind, self),
            (ItemKind::Stratagem, Self::Stratagem) | (ItemKind::Booster, Self::Booster)
        )
    }

    pub const fn is_home_booster(self) -> bool {
        matches!(self, Self::HomeBooster | Self::HomeBoosterEmpty)
    }
}

/// Page-level layout expected by the detector and attached to each observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotLayout {
    Home,
    List(ItemKind),
}

impl SlotLayout {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::List(ItemKind::Stratagem) => "stratagem_list",
            Self::List(ItemKind::Booster) => "booster_list",
        }
    }
}

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

#[derive(Debug)]
pub struct RoiObservation {
    pub image: RgbaImage,
    pub layout: SlotLayout,
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
) -> Result<ImageRect> {
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
        ScaleAxis::Fit => {
            (image_w as f32 / reference.w as f32).min(image_h as f32 / reference.h as f32)
        }
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

    Ok(ImageRect {
        x,
        y,
        w: right.saturating_sub(x).max(1),
        h: bottom.saturating_sub(y).max(1),
    })
}

pub fn detect_slot_layout(image: RgbaImage, expected_layout: SlotLayout) -> Result<RoiObservation> {
    if image.width() == 0 || image.height() == 0 {
        bail!("cannot detect slots in an empty ROI image");
    }
    let slots = geometry::detect(&image, expected_layout)?;
    Ok(RoiObservation {
        image,
        layout: expected_layout,
        slots,
    })
}
