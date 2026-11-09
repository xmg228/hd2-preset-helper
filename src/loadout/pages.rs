//! Navigate between the Stratagem and equipment loadout pages.
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use super::{UiState, bind_loadout_region, detect_ui_state};
use crate::{
    automation::{AutomationSession, Cancellation},
    capture::{CaptureSource, CapturedFrame},
    evidence::OperationEvidence,
    game_settings::GameColorSettings,
    input::Key,
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
    evidence: &OperationEvidence,
    cancellation: &Cancellation,
) -> Result<Page> {
    if page_visible(
        capture,
        runtime,
        target,
        colors,
        page,
        evidence,
        cancellation,
    )? {
        return Ok(page);
    }
    let other = match page {
        Page::Stratagems => Page::Equipment,
        Page::Equipment => Page::Stratagems,
    };
    let origin = if page_visible(
        capture,
        runtime,
        target,
        colors,
        other,
        evidence,
        cancellation,
    )? {
        other
    } else {
        let mut origin = None;
        // Equipment's focus and text evidence take precedence over generic icon geometry.
        for candidate in [Page::Equipment, Page::Stratagems] {
            let (mut region, detector) = bind_page(
                capture,
                runtime,
                target,
                colors,
                candidate,
                evidence,
                cancellation,
            )?;
            let frame = region.capture()?;
            let confirmed = detector.list_visible(frame.clone())?;
            evidence.record(
                "list-detection",
                Some(&frame),
                serde_json::json!({"page": format!("{candidate:?}"), "confirmed": confirmed}),
            );
            if confirmed {
                origin = Some(candidate);
                break;
            }
        }
        let origin =
            origin.context("open a Stratagem or equipment loadout screen before using a preset")?;
        tracing::debug!(
            ?origin,
            key = "Escape",
            "returning from item list to loadout home"
        );
        let (mut automation, _) = bind_page(
            capture,
            runtime,
            target,
            colors,
            origin,
            evidence,
            cancellation,
        )?;
        automation.tap_key(Key::Escape, 45)?;
        wait_for_page(
            capture,
            runtime,
            target,
            colors,
            origin,
            evidence,
            cancellation,
        )?;
        origin
    };
    if origin != page {
        tracing::debug!(?origin, destination = ?page, key = "R", "switching loadout home");
        let (mut automation, _) = bind_page(
            capture,
            runtime,
            target,
            colors,
            origin,
            evidence,
            cancellation,
        )?;
        automation.tap_key(Key::R, 45)?;
        wait_for_page(
            capture,
            runtime,
            target,
            colors,
            page,
            evidence,
            cancellation,
        )?;
    }
    Ok(origin)
}

fn wait_for_page(
    capture: &mut CaptureSource,
    runtime: &RecognizerRuntime,
    target: &WindowTarget,
    colors: GameColorSettings,
    page: Page,
    evidence: &OperationEvidence,
    cancellation: &Cancellation,
) -> Result<()> {
    let started = Instant::now();
    let (mut region, detector) = bind_page(
        capture,
        runtime,
        target,
        colors,
        page,
        evidence,
        cancellation,
    )?;
    let mut confirmed = false;
    loop {
        target.ensure_input_target()?;
        let frame = region.capture()?;
        let detection = detector.home(frame.clone())?;
        let visible = detection.confirmed();
        if visible && confirmed || started.elapsed() >= Duration::from_millis(1500) {
            evidence.record(
                "page-transition",
                Some(&frame),
                serde_json::json!({
                    "page": format!("{page:?}"), "detection": detection, "previous_visible": confirmed,
                    "elapsed_s": started.elapsed().as_secs_f64(),
                }),
            );
        }
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
    target: &WindowTarget,
    colors: GameColorSettings,
    page: Page,
    evidence: &OperationEvidence,
    cancellation: &Cancellation,
) -> Result<bool> {
    let (mut region, detector) = bind_page(
        capture,
        runtime,
        target,
        colors,
        page,
        evidence,
        cancellation,
    )?;
    let frame = region.capture()?;
    let detection = detector.home(frame.clone())?;
    let visible = detection.confirmed();
    evidence.record(
        "page-detection",
        Some(&frame),
        serde_json::json!({"page": format!("{page:?}"), "detection": detection}),
    );
    Ok(visible)
}

enum PageDetector {
    Stratagems(RecognizerSession),
    Equipment(EquipmentObserver),
}

#[derive(serde::Serialize)]
#[serde(tag = "page", rename_all = "snake_case")]
enum HomeDetection {
    Stratagems {
        confirmed: bool,
        slots: Vec<crate::vision::Slot>,
    },
    Equipment {
        confirmed: bool,
        entry: crate::vision::equipment::EquipmentEntry,
    },
}

impl HomeDetection {
    fn confirmed(&self) -> bool {
        match self {
            Self::Stratagems { confirmed, .. } | Self::Equipment { confirmed, .. } => *confirmed,
        }
    }
}

// Keep the ROI and its detector bound throughout a page transition.
fn bind_page<'a>(
    capture: &'a mut CaptureSource,
    runtime: &RecognizerRuntime,
    target: &WindowTarget,
    colors: GameColorSettings,
    page: Page,
    evidence: &'a OperationEvidence,
    cancellation: &Cancellation,
) -> Result<(AutomationSession<'a>, PageDetector)> {
    let (automation, detector) = match page {
        Page::Stratagems => {
            let bound = bind_loadout_region(capture, runtime.calibration(), colors)?;
            let detector = PageDetector::Stratagems(runtime.bind(bound.geometry));
            (
                AutomationSession::new(
                    bound.region,
                    target.clone(),
                    evidence,
                    cancellation.clone(),
                )?,
                detector,
            )
        }
        Page::Equipment => {
            let (width, height) = capture.output_size();
            let (observer, resolved) =
                EquipmentObserver::resolve(width, height, EquipmentKind::Helmet)?;
            let region = capture.region(resolved.rect, colors)?;
            (
                AutomationSession::new(region, target.clone(), evidence, cancellation.clone())?,
                PageDetector::Equipment(observer),
            )
        }
    };
    Ok((automation, detector))
}

impl PageDetector {
    fn home(&self, frame: CapturedFrame) -> Result<HomeDetection> {
        match self {
            Self::Stratagems(recognizer) => {
                let home = recognizer.detect(frame, SlotLayout::Home)?;
                Ok(HomeDetection::Stratagems {
                    confirmed: detect_ui_state(&home) == UiState::Home,
                    slots: home.slots,
                })
            }
            Self::Equipment(observer) => {
                let entry = observer.entry(&frame);
                Ok(HomeDetection::Equipment {
                    confirmed: entry.confirmed(),
                    entry,
                })
            }
        }
    }

    fn list_visible(&self, frame: CapturedFrame) -> Result<bool> {
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
