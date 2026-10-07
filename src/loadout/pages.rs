//! Navigate between the Stratagem and equipment loadout pages.
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use image::RgbaImage;

use super::{UiState, bind_loadout_region, detect_ui_state};
use crate::{
    capture::{CaptureRegion, CaptureSource},
    game_settings::GameColorSettings,
    input::{InputSession, Key},
    item::{EquipmentKind, ItemKind},
    vision::{RecognizerRuntime, RecognizerSession, SlotLayout, equipment::EquipmentObserver},
    window::WindowTarget,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Stratagems,
    Equipment,
}

/// Show the requested home and return the originating loadout page.
pub(crate) fn show_page(
    capture: &mut CaptureSource,
    runtime: &RecognizerRuntime,
    target: &WindowTarget,
    colors: GameColorSettings,
    page: Page,
) -> Result<Page> {
    if page_visible(capture, runtime, colors, page)? {
        return Ok(page);
    }
    let other = match page {
        Page::Stratagems => Page::Equipment,
        Page::Equipment => Page::Stratagems,
    };
    let origin = if page_visible(capture, runtime, colors, other)? {
        other
    } else {
        let mut origin = None;
        // Equipment's focus and text evidence take precedence over generic icon geometry.
        for candidate in [Page::Equipment, Page::Stratagems] {
            let (mut region, detector) = bind_page(capture, runtime, colors, candidate)?;
            if detector.list_visible(region.capture()?)? {
                origin = Some(candidate);
                break;
            }
        }
        let origin =
            origin.context("open a Stratagem or equipment loadout screen before using a preset")?;
        InputSession::new(target.clone())?.tap_key(Key::Escape, 45)?;
        wait_for_page(capture, runtime, target, colors, origin)?;
        origin
    };
    if origin != page {
        InputSession::new(target.clone())?.tap_key(Key::R, 45)?;
        wait_for_page(capture, runtime, target, colors, page)?;
    }
    Ok(origin)
}

fn wait_for_page(
    capture: &mut CaptureSource,
    runtime: &RecognizerRuntime,
    target: &WindowTarget,
    colors: GameColorSettings,
    page: Page,
) -> Result<()> {
    let started = Instant::now();
    let (mut region, detector) = bind_page(capture, runtime, colors, page)?;
    let mut confirmed = false;
    loop {
        target.ensure_input_target()?;
        let visible = detector.visible(region.capture()?)?;
        if visible && confirmed {
            tracing::debug!(
                ?page,
                elapsed_s = started.elapsed().as_secs_f64(),
                "loadout home confirmed after navigation"
            );
            return Ok(());
        }
        confirmed = visible;
        ensure!(
            started.elapsed() < Duration::from_millis(1500),
            "could not confirm the {page:?} home after navigation; no retry was sent"
        );
    }
}

fn page_visible(
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

    fn list_visible(&self, frame: RgbaImage) -> Result<bool> {
        match self {
            Self::Stratagems(recognizer) => {
                let mut observation = recognizer.detect(frame, SlotLayout::Home)?;
                if detect_ui_state(&observation) == UiState::Home {
                    return Ok(false);
                }
                for kind in [ItemKind::Stratagem, ItemKind::Booster] {
                    observation = recognizer.detect(observation.image, SlotLayout::List(kind))?;
                    if detect_ui_state(&observation) == UiState::List(kind) {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Self::Equipment(observer) => Ok(!observer.entry(&frame).confirmed()
                && [EquipmentKind::Helmet, EquipmentKind::Primary]
                    .into_iter()
                    .any(|kind| observer.with_kind(kind).observe(&frame).ready())),
        }
    }
}
