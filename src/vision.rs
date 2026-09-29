mod booster;
mod borders;
mod color;
#[cfg(feature = "diagnostics")]
mod diagnostics;
pub(crate) mod equipment;
mod geometry;
#[cfg(feature = "diagnostics")]
mod home_tone_diagnostics;
mod matcher;
mod recognizer;
mod roi;
mod semantic_extractor;
mod template_classifier;
pub(crate) mod text;

use anyhow::{Result, bail};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::image_rect::ImageRect;
use crate::item::ItemKind;

pub(crate) use booster::crop_sample as crop_booster_sample;
pub use color::{icon_likeness, luma601_u8};
#[cfg(feature = "diagnostics")]
pub use diagnostics::init as init_diagnostics;
#[cfg(feature = "diagnostics")]
pub use diagnostics::save_fallback_slot;
#[cfg(feature = "diagnostics")]
pub use diagnostics::save_list_map_image;
#[cfg(feature = "diagnostics")]
pub(crate) use home_tone_diagnostics::log_home_tone;
pub use recognizer::{RecognizerRuntime, RecognizerSession};
pub use roi::{
    Calibration, ReferenceSize, ResolvedRoi, RoiAnchor, RoiGeometry, ScaleAxis,
    resolve_calibration_roi_for_size,
};
pub use semantic_extractor::crop_slot_sample;
pub use template_classifier::{TemplateClassifier, TemplateMatchCandidate};

pub const ROI_REFERENCE_H: u32 = 624;

// Geometry in the 1920x1080 reference UI. The core excludes the slot frame;
// icon sizes describe texture scale, not the size of the saved crop.
const SLOT_SIDE_LOGICAL: f64 = 78.0;
const CORE_INSET_LOGICAL: f64 = 4.0;
pub(crate) const HOME_ICON_SIZE_LOGICAL: f32 = 70.0;
const LIST_ICON_SIZE_LOGICAL: f32 = 51.0;

const LIST_COLS: [i32; 4] = [58, 143, 228, 313];
const HOME_COLS: [i32; 4] = [8, 93, 178, 263];

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
    center_x: f32,
    center_y: f32,
    /// Continuous distance between opposite frame center lines, in native pixels.
    side: f64,
    pub row: u32,
    pub col: u32,
    pub kind: SlotKind,
    pub classification: Option<Classification>,
}

impl Slot {
    pub fn center(&self) -> (u32, u32) {
        (self.center_x.round() as u32, self.center_y.round() as u32)
    }

    pub fn center_f32(&self) -> (f32, f32) {
        (self.center_x, self.center_y)
    }

    /// Fixed integer crop size per UI scale, positioned around the continuous
    /// center. The sample retains its fractional center relative to this rect.
    pub(crate) fn core_rect(&self) -> ImageRect {
        let scale = self.side / SLOT_SIDE_LOGICAL;
        let side = ((SLOT_SIDE_LOGICAL - 2.0 * CORE_INSET_LOGICAL) * scale)
            .round_ties_even()
            .max(1.0) as u32;
        ImageRect {
            x: (self.center_x as f64 - side as f64 * 0.5)
                .round_ties_even()
                .max(0.0) as u32,
            y: (self.center_y as f64 - side as f64 * 0.5)
                .round_ties_even()
                .max(0.0) as u32,
            w: side,
            h: side,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct SampleGeometry {
    pub center_x: f32,
    pub center_y: f32,
    pub physical_size: f32,
}

pub struct ImageSample {
    pub image: RgbaImage,
    pub geometry: SampleGeometry,
}

#[derive(Debug, Clone)]
pub struct Classification {
    pub item_id: String,
    /// Raw template-matching error used by the acceptance gate; lower is better.
    pub match_error: f32,
    /// Difference between the best and second-best template errors.
    pub match_margin: f32,
    /// Normalized surplus above the weaker acceptance gate; not a probability.
    pub gate_quality: f32,
    pub availability: ItemAvailability,
}

#[derive(Debug, Clone, Copy)]
pub enum ItemAvailability {
    Available,
    Unavailable { brightness_ratio: f32 },
}

pub fn detect_slot_layout(
    image: RgbaImage,
    geometry: RoiGeometry,
    expected_layout: SlotLayout,
) -> Result<RoiObservation> {
    if image.width() == 0 || image.height() == 0 {
        bail!("cannot detect slots in an empty ROI image");
    }
    let slots = geometry::detect(&image, geometry, expected_layout)?;
    Ok(RoiObservation {
        image,
        layout: expected_layout,
        slots,
    })
}
