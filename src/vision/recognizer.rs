use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use image::RgbaImage;
use tracing::{debug, debug_span};

use crate::assets::{IconCatalog, default_calibration, default_icon_manifest, parse_json_asset};

use super::classifier::TemplateClassifier;
use super::{Calibration, RoiObservation, SlotLayout, detect_slot_layout};

const TEMPLATE_THRESHOLD: f32 = 0.70;
const TEMPLATE_MIN_MARGIN: f32 = 0.035;

pub struct RecognizerRuntime {
    calibration: Calibration,
    icons: Arc<IconCatalog>,
    classifier: TemplateClassifier,
}

impl RecognizerRuntime {
    pub fn load() -> Result<Self> {
        let load_time = Instant::now();
        let calibration = parse_json_asset(default_calibration())?;
        let icons = Arc::new(IconCatalog::load(default_icon_manifest())?);
        let classifier = TemplateClassifier::load(&icons, TEMPLATE_THRESHOLD, TEMPLATE_MIN_MARGIN)?;

        debug!(elapsed = ?load_time.elapsed(), "recognizer runtime ready");

        Ok(Self {
            calibration,
            icons,
            classifier,
        })
    }

    pub fn calibration(&self) -> &Calibration {
        &self.calibration
    }

    pub fn icon_catalog(&self) -> &Arc<IconCatalog> {
        &self.icons
    }

    pub fn detect(&self, image: RgbaImage, expected_layout: SlotLayout) -> Result<RoiObservation> {
        let span = debug_span!("detect_layout");
        let _guard = span.enter();

        detect_slot_layout(image, expected_layout)
    }

    pub fn classify(&self, result: &mut RoiObservation) -> Result<()> {
        let span = debug_span!("classify_layout");
        let _guard = span.enter();

        self.classifier
            .classify_batch(&result.image, &mut result.slots, result.layout)
    }

    pub fn recognize(
        &self,
        image: RgbaImage,
        expected_layout: SlotLayout,
    ) -> Result<RoiObservation> {
        let span = debug_span!("recognize_roi");
        let _guard = span.enter();

        let mut result = self.detect(image, expected_layout)?;
        self.classify(&mut result)?;
        Ok(result)
    }
}
