//! Navigate between the Stratagem and equipment loadout pages.
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};

use super::{UiState, bind_loadout_region, detect_ui_state};
use crate::{
    capture::CaptureSource,
    game_settings::GameColorSettings,
    input::{InputSession, Key},
    item::EquipmentKind,
    vision::{RecognizerRuntime, SlotLayout, equipment::EquipmentObserver},
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
    let mut confirmed = false;
    loop {
        target.ensure_input_target()?;
        let visible = page_visible(capture, runtime, colors, page)?;
        if visible && confirmed {
            tracing::debug!(?page, elapsed = ?started.elapsed(), "loadout page switched");
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
    match page {
        Page::Stratagems => {
            let mut bound = bind_loadout_region(capture, runtime.calibration(), colors)?;
            let home = runtime
                .bind(bound.geometry)
                .detect(bound.region.capture()?, SlotLayout::Home)?;
            Ok(matches!(
                detect_ui_state(&home),
                UiState::HomeEmpty | UiState::HomeFilled | UiState::HomeMixed
            ))
        }
        Page::Equipment => {
            let (width, height) = capture.output_size();
            let (observer, resolved) =
                EquipmentObserver::resolve(width, height, EquipmentKind::Helmet)?;
            let frame = capture.region(resolved.rect, colors)?.capture()?;
            Ok(observer.entry(&frame).confirmed())
        }
    }
}
