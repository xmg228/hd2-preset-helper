use anyhow::Result;

use crate::capture::{CaptureRegion, CaptureSource};
use crate::game_settings::GameColorSettings;
use crate::vision::{Calibration, RoiGeometry, resolve_calibration_roi_for_size};

pub struct BoundLoadoutRegion<'a> {
    pub region: CaptureRegion<'a>,
    pub geometry: RoiGeometry,
}

pub fn bind_loadout_region<'a>(
    capture: &'a mut CaptureSource,
    calibration: &Calibration,
    color_settings: GameColorSettings,
) -> Result<BoundLoadoutRegion<'a>> {
    let (image_w, image_h) = capture.output_size();
    let resolved = resolve_calibration_roi_for_size(image_w, image_h, calibration)?;
    Ok(BoundLoadoutRegion {
        region: capture.region(resolved.rect, color_settings)?,
        geometry: resolved.geometry,
    })
}
