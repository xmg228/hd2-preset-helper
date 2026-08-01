use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tracing::{debug, debug_span, trace};

use crate::capture::CaptureSource;
use crate::input;
use crate::item::ItemKind;
use crate::page_sync::{capture_latest_roi_frame, image_fingerprint};
use crate::runtime::RecognizerRuntime;
use crate::slot::SlotLayout;
use crate::vision::{RoiFrame, RoiObservation};
use crate::visual_fingerprint::distance as fingerprint_distance;

use super::page_relation::{PAGE_TURN_FULL_SHIFT_MIN_RATIO, PageRelation, compare_page_turn};

const PAGE_TURN_NO_MOVEMENT_GRACE: Duration = Duration::from_millis(100);
const PAGE_TURN_TIMEOUT: Duration = Duration::from_millis(350);
const PAGE_CHANGE_THRESHOLD: f32 = 6.0;
const PAGE_WHEEL_DELTA: i32 = -600;

pub(super) struct PageSnapshot {
    pub(super) roi: RoiObservation,
    pub(super) signature: Vec<u8>,
}

pub(super) struct PageNavigator<'a> {
    runtime: &'a RecognizerRuntime,
    item_kind: ItemKind,
}

pub(super) enum PageTurnResult {
    Moved {
        page: PageSnapshot,
        reached_end: bool,
    },
    NoMovement(PageSnapshot),
}

impl<'a> PageNavigator<'a> {
    pub(super) fn new(runtime: &'a RecognizerRuntime, item_kind: ItemKind) -> Self {
        Self { runtime, item_kind }
    }

    pub(super) fn perform_confirmed_semantic_page_turn(
        &self,
        capture: &mut CaptureSource,
        current_page: PageSnapshot,
        next_page_index: u32,
    ) -> Result<PageTurnResult> {
        let span = debug_span!("confirmed_semantic_page_turn", next_page_index);
        let _guard = span.enter();

        input::wheel_with_boundary(PAGE_WHEEL_DELTA, || capture.sync_to_latest())?;

        let (page, relation) = self.observe_instant_viewport_change(capture, &current_page)?;

        match relation {
            PageRelation::Shifted(shift) => {
                debug!(
                    target: "hd2_preset_helper::perf",
                    directed_shift = shift.directed_shift,
                    expected_full_shift = shift.expected_full_shift,
                    shift_ratio = shift.shift_ratio,
                    full_shift_min_ratio = PAGE_TURN_FULL_SHIFT_MIN_RATIO,
                    reached_end = shift.reached_end,
                    "page turn completed"
                );
                Ok(PageTurnResult::Moved {
                    page,
                    reached_end: shift.reached_end,
                })
            }
            PageRelation::DifferentViewport => {
                debug!(
                    target: "hd2_preset_helper::perf",
                    relation = ?PageRelation::DifferentViewport,
                    "page turn completed"
                );
                Ok(PageTurnResult::Moved {
                    page,
                    reached_end: false,
                })
            }
            PageRelation::SameViewport | PageRelation::Uncertain => {
                Ok(PageTurnResult::NoMovement(page))
            }
        }
    }

    fn observe_instant_viewport_change(
        &self,
        capture: &mut CaptureSource,
        current_page: &PageSnapshot,
    ) -> Result<(PageSnapshot, PageRelation)> {
        // Fingerprints trigger classification; semantic anchor displacement
        // determines whether the explicit page input moved the viewport.
        let start = Instant::now();
        let mut visual_reference = current_page.signature.clone();
        let mut last_same_page: Option<PageSnapshot> = None;
        let mut pending_different: Option<PageSnapshot> = None;
        let mut no_movement_checked = false;

        loop {
            let frame = capture_latest_roi_frame(capture, self.runtime.calibration())?;
            let signature = image_fingerprint(&frame.image);
            let distance = fingerprint_distance(&visual_reference, &signature);
            let elapsed = start.elapsed();
            let grace_check = !no_movement_checked
                && pending_different.is_none()
                && elapsed >= PAGE_TURN_NO_MOVEMENT_GRACE;
            if grace_check {
                no_movement_checked = true;
                if let Some(same_page) = last_same_page.take() {
                    debug!(
                        target: "hd2_preset_helper::perf",
                        elapsed = ?elapsed,
                        reason = "same_viewport_after_grace",
                        "page turn produced no movement"
                    );
                    return Ok((same_page, PageRelation::SameViewport));
                }
            }

            let timed_out = elapsed >= PAGE_TURN_TIMEOUT;
            if distance < PAGE_CHANGE_THRESHOLD && !grace_check && !timed_out {
                continue;
            }

            let candidate = self.scan_direct_page(frame)?;
            let relation = compare_page_turn(&current_page.roi, &candidate.roi, self.item_kind);
            trace!(
                relation = ?relation,
                elapsed = ?start.elapsed(),
                "page semantic relation"
            );

            match relation {
                relation @ PageRelation::Shifted(_) => {
                    return Ok((candidate, relation));
                }
                relation @ PageRelation::DifferentViewport => {
                    if let Some(previous_candidate) = pending_different.take() {
                        let confirmation = compare_page_turn(
                            &previous_candidate.roi,
                            &candidate.roi,
                            self.item_kind,
                        );
                        let confirmed = matches!(confirmation, PageRelation::SameViewport);
                        trace!(
                            confirmed,
                            elapsed = ?start.elapsed(),
                            "different viewport confirmation"
                        );
                        if confirmed {
                            return Ok((candidate, relation));
                        }
                    }
                    if timed_out {
                        bail!(
                            "page input reached a different viewport but it was not confirmed by a second semantic frame after {:?}",
                            start.elapsed()
                        );
                    }
                    pending_different = Some(candidate);
                }
                relation @ PageRelation::SameViewport => {
                    visual_reference = candidate.signature.clone();
                    pending_different = None;
                    if elapsed >= PAGE_TURN_NO_MOVEMENT_GRACE {
                        let reason = if timed_out {
                            "same_viewport_at_timeout"
                        } else {
                            "same_viewport_after_grace"
                        };
                        debug!(
                            target: "hd2_preset_helper::perf",
                            elapsed = ?start.elapsed(),
                            reason,
                            "page turn produced no movement"
                        );
                        return Ok((candidate, relation));
                    }
                    last_same_page = Some(candidate);
                }
                PageRelation::Uncertain => {
                    if timed_out {
                        bail!(
                            "page input produced no semantically confirmed viewport transition after {:?}",
                            start.elapsed()
                        );
                    }
                }
            }
        }
    }

    pub(super) fn scan_direct_page(&self, frame: RoiFrame) -> Result<PageSnapshot> {
        let roi = self
            .runtime
            .recognize(frame, SlotLayout::List(self.item_kind))?;
        Ok(Self::finish_direct_page(roi))
    }

    pub(super) fn prepare_direct_page(&self, mut roi: RoiObservation) -> Result<PageSnapshot> {
        self.runtime.classify(&mut roi)?;
        Ok(Self::finish_direct_page(roi))
    }

    fn finish_direct_page(roi: RoiObservation) -> PageSnapshot {
        let signature = {
            let span = debug_span!("roi_fingerprint");
            let _guard = span.enter();
            image_fingerprint(&roi.image)
        };

        PageSnapshot { roi, signature }
    }
}
