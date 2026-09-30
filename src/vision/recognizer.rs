use std::time::Instant;

use anyhow::Result;
use image::RgbaImage;
use tracing::{debug, debug_span};

use crate::assets::{default_calibration, parse_json_asset};

use super::{Calibration, RoiGeometry, RoiObservation, SlotLayout, detect_slot_layout};

pub struct RecognizerRuntime {
    calibration: Calibration,
}

#[derive(Clone, Copy)]
pub struct RecognizerSession {
    geometry: RoiGeometry,
}

impl RecognizerRuntime {
    pub fn load() -> Result<Self> {
        let load_time = Instant::now();
        let calibration = parse_json_asset(default_calibration())?;

        debug!(
            elapsed_s = load_time.elapsed().as_secs_f64(),
            "recognizer runtime ready"
        );

        Ok(Self { calibration })
    }

    pub fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    pub fn bind(&self, geometry: RoiGeometry) -> RecognizerSession {
        debug!(
            ui_scale = geometry.scale,
            logical_origin_x = geometry.logical_origin_x,
            logical_origin_y = geometry.logical_origin_y,
            "recognizer geometry bound"
        );
        RecognizerSession { geometry }
    }
}

impl RecognizerSession {
    pub fn ui_scale(self) -> f32 {
        self.geometry.scale as f32
    }

    pub fn detect(&self, image: RgbaImage, expected_layout: SlotLayout) -> Result<RoiObservation> {
        let span = debug_span!("detect_layout");
        let _guard = span.enter();

        detect_slot_layout(image, self.geometry, expected_layout)
    }
}
