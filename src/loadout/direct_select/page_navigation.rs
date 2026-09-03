use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use image::RgbaImage;
use tracing::{debug, debug_span, trace};

use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::vision::{RecognizerRuntime, RoiObservation, SlotLayout};

use super::super::frame::{fingerprint_distance, image_fingerprint};

use super::page_relation::{PAGE_TURN_SHORT_THRESHOLD_RATIO, PageRelation, compare_page_turn};

const PAGE_TURN_NO_MOVEMENT_GRACE: Duration = Duration::from_millis(700);
const PAGE_TURN_DECISION_TIMEOUT: Duration = Duration::from_millis(1200);
const PAGE_TURN_HARD_TIMEOUT: Duration = Duration::from_secs(4);
const PAGE_TURN_MIN_SEMANTIC_OBSERVATIONS: usize = 3;
const PAGE_TURN_NO_MOVEMENT_FRAMES: usize = 3;
const PAGE_CHANGE_THRESHOLD: f32 = 6.0;
const PAGE_WHEEL_DELTA: i32 = -600;
const PAGE_END_PROBE_DELTA: i32 = -120;

pub(super) struct PageSnapshot {
    pub(super) roi: RoiObservation,
    pub(super) signature: Vec<u8>,
}

pub(super) struct PageNavigator<'a> {
    runtime: &'a RecognizerRuntime,
    item_kind: ItemKind,
}

pub(super) enum PageTurnResult {
    Moved { page: PageSnapshot, short: bool },
    NoMovement(PageSnapshot),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PageTurnInput {
    Full,
    EndProbe,
}

impl PageTurnInput {
    fn wheel_delta(self) -> i32 {
        match self {
            Self::Full => PAGE_WHEEL_DELTA,
            Self::EndProbe => PAGE_END_PROBE_DELTA,
        }
    }
}

impl<'a> PageNavigator<'a> {
    pub(super) fn new(runtime: &'a RecognizerRuntime, item_kind: ItemKind) -> Self {
        Self { runtime, item_kind }
    }

    pub(super) fn detect_home(&self, image: RgbaImage) -> Result<RoiObservation> {
        self.runtime.detect(image, SlotLayout::Home)
    }

    pub(super) fn perform_confirmed_semantic_page_turn(
        &self,
        automation: &mut AutomationSession<'_>,
        current_page: PageSnapshot,
        input: PageTurnInput,
        wheel_attempt: u32,
    ) -> Result<PageTurnResult> {
        let span = debug_span!("confirmed_semantic_page_turn", wheel_attempt, ?input);
        let _guard = span.enter();

        automation.scroll(input.wheel_delta())?;
        self.observe_instant_viewport_change(automation, &current_page)
    }

    fn observe_instant_viewport_change(
        &self,
        automation: &mut AutomationSession<'_>,
        current_page: &PageSnapshot,
    ) -> Result<PageTurnResult> {
        // Fingerprints trigger classification; semantic anchor displacement
        // determines whether the explicit page input moved the viewport.
        let start = Instant::now();
        let mut visual_reference = current_page.signature.clone();
        let mut pending_different: Option<PageSnapshot> = None;
        let mut same_viewport_frames = 0usize;
        let mut semantic_observations = 0usize;

        loop {
            let image = automation.capture()?;
            let signature = image_fingerprint(&image);
            let distance = fingerprint_distance(&visual_reference, &signature);
            let elapsed = start.elapsed();
            let decision_time_elapsed = elapsed >= PAGE_TURN_DECISION_TIMEOUT;
            let no_movement_check = elapsed >= PAGE_TURN_NO_MOVEMENT_GRACE;
            if distance < PAGE_CHANGE_THRESHOLD && !no_movement_check && !decision_time_elapsed {
                continue;
            }

            let candidate = self.scan_direct_page(image)?;
            semantic_observations += 1;
            let relation = compare_page_turn(&current_page.roi, &candidate.roi, self.item_kind);
            let elapsed = start.elapsed();
            let decision_timed_out = elapsed >= PAGE_TURN_DECISION_TIMEOUT
                && semantic_observations >= PAGE_TURN_MIN_SEMANTIC_OBSERVATIONS;
            let hard_timed_out = elapsed >= PAGE_TURN_HARD_TIMEOUT;
            trace!(
                relation = ?relation,
                semantic_observations,
                elapsed = ?elapsed,
                "page semantic relation"
            );

            match relation {
                PageRelation::Shifted(shift) => {
                    let short = shift.shift_ratio < PAGE_TURN_SHORT_THRESHOLD_RATIO;
                    debug!(
                        target: "hd2_preset_helper::perf",
                        directed_shift = shift.directed_shift,
                        shift_ratio = shift.shift_ratio,
                        short_threshold_ratio = PAGE_TURN_SHORT_THRESHOLD_RATIO,
                        short,
                        "page turn completed"
                    );
                    return Ok(PageTurnResult::Moved {
                        page: candidate,
                        short,
                    });
                }
                PageRelation::DifferentViewport => {
                    same_viewport_frames = 0;
                    let mut had_pending_different = false;
                    if let Some(previous_candidate) = pending_different.take() {
                        had_pending_different = true;
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
                            debug!(
                                target: "hd2_preset_helper::perf",
                                relation = ?PageRelation::DifferentViewport,
                                "page turn completed"
                            );
                            return Ok(PageTurnResult::Moved {
                                page: candidate,
                                short: false,
                            });
                        }
                    }
                    if hard_timed_out || (decision_timed_out && had_pending_different) {
                        bail!(
                            "page input reached a different viewport but it was not confirmed after {} semantic observations over {:?}",
                            semantic_observations,
                            elapsed
                        );
                    }
                    pending_different = Some(candidate);
                }
                PageRelation::SameViewport => {
                    visual_reference = candidate.signature.clone();
                    pending_different = None;
                    same_viewport_frames += 1;
                    if no_movement_check && same_viewport_frames >= PAGE_TURN_NO_MOVEMENT_FRAMES {
                        debug!(
                            target: "hd2_preset_helper::perf",
                            elapsed = ?start.elapsed(),
                            same_viewport_frames,
                            "page turn produced no movement"
                        );
                        return Ok(PageTurnResult::NoMovement(candidate));
                    }
                    if hard_timed_out {
                        bail!(
                            "page input collected only {} of {} stable same-viewport frames after {} semantic observations over {:?}",
                            same_viewport_frames,
                            PAGE_TURN_NO_MOVEMENT_FRAMES,
                            semantic_observations,
                            elapsed
                        );
                    }
                }
                PageRelation::Uncertain => {
                    same_viewport_frames = 0;
                    pending_different = None;
                    if decision_timed_out || hard_timed_out {
                        bail!(
                            "page input produced no confirmed viewport transition after {} semantic observations over {:?}",
                            semantic_observations,
                            elapsed
                        );
                    }
                }
            }
        }
    }

    pub(super) fn scan_direct_page(&self, image: RgbaImage) -> Result<PageSnapshot> {
        let roi = self
            .runtime
            .recognize(image, SlotLayout::List(self.item_kind))?;
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
