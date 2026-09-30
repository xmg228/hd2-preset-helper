//! Navigate between the Stratagem and equipment loadout pages.
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use image::RgbaImage;

use super::{UiState, bind_loadout_region, detect_ui_state};
use crate::{
    capture::{CaptureRegion, CaptureSource},
    game_settings::GameColorSettings,
    input::{InputSession, Key},
    item::EquipmentKind,
    vision::{RecognizerRuntime, RecognizerSession, SlotLayout, equipment::EquipmentObserver},
    window::WindowTarget,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Stratagems,
    Equipment,
}

pub(crate) fn show_page(
    capture: &mut CaptureSource,
    runtime: &RecognizerRuntime,
    target: &WindowTarget,
    colors: GameColorSettings,
    page: Page,
) -> Result<()> {
    if page_visible(capture, runtime, colors, page)? {
        return Ok(());
    }
    let other = match page {
        Page::Stratagems => Page::Equipment,
        Page::Equipment => Page::Stratagems,
    };
    ensure!(
        page_visible(capture, runtime, colors, other)?,
        "return to the Stratagem or equipment home before using a preset"
    );
    let started = Instant::now();
    InputSession::new(target.clone())?.tap_key(Key::R, 45)?;
    let (mut region, detector) = bind_page(capture, runtime, colors, page)?;
    let mut confirmed = false;
    loop {
        target.ensure_input_target()?;
        let visible = detector.visible(region.capture()?)?;
        if visible && confirmed {
            tracing::debug!(
                ?page,
                elapsed_s = started.elapsed().as_secs_f64(),
                "loadout page switched"
            );
            return Ok(());
        }
        confirmed = visible;
        ensure!(
            started.elapsed() < Duration::from_millis(1500),
            "could not confirm the {page:?} home after switching; no retry was sent"
        );
    }
}

pub(crate) fn page_visible(
    capture: &mut CaptureSource,
    runtime: &RecognizerRuntime,
    colors: GameColorSettings,
    page: Page,
) -> Result<bool> {
    let (mut region, detector) = bind_page(capture, runtime, colors, page)?;
    detector.visible(region.capture()?)
}

enum PageDetector {
    Stratagems(RecognizerSession),
    Equipment(EquipmentObserver),
}

// Keep the ROI and its detector bound throughout a page transition.
fn bind_page<'a>(
    capture: &'a mut CaptureSource,
    runtime: &RecognizerRuntime,
    colors: GameColorSettings,
    page: Page,
) -> Result<(CaptureRegion<'a>, PageDetector)> {
    match page {
        Page::Stratagems => {
            let bound = bind_loadout_region(capture, runtime.calibration(), colors)?;
            let detector = PageDetector::Stratagems(runtime.bind(bound.geometry));
            Ok((bound.region, detector))
        }
        Page::Equipment => {
            let (width, height) = capture.output_size();
            let (observer, resolved) =
                EquipmentObserver::resolve(width, height, EquipmentKind::Helmet)?;
            let region = capture.region(resolved.rect, colors)?;
            Ok((region, PageDetector::Equipment(observer)))
        }
    }
}

impl PageDetector {
    fn visible(&self, frame: RgbaImage) -> Result<bool> {
        match self {
            Self::Stratagems(recognizer) => {
                let home = recognizer.detect(frame, SlotLayout::Home)?;
                Ok(detect_ui_state(&home) == UiState::Home)
            }
            Self::Equipment(observer) => Ok(observer.entry(&frame).confirmed()),
        }
    }
}
