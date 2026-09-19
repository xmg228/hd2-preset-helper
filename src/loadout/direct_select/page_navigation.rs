use std::time::{Duration, Instant};

use anyhow::Result;
use image::RgbaImage;
use tracing::{debug, debug_span, trace};

use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::vision::{
    RecognizerSession, RoiObservation, SlotKind, SlotLayout, TemplateClassifier,
    TemplateMatchCandidate, slot_core_rect,
};

use super::super::frame::{fingerprint_distance, image_fingerprint};

use super::ScrollDirection;
use super::page_relation::{PAGE_TURN_SHORT_THRESHOLD_RATIO, PageRelation, compare_page_turn};

const PAGE_TURN_NO_MOVEMENT_GRACE: Duration = Duration::from_millis(250);
const PAGE_BOUNDARY_NUDGE_NO_MOVEMENT_GRACE: Duration = Duration::from_millis(200);
const PAGE_TURN_NO_MOVEMENT_FRAMES: usize = 2;
const PAGE_CHANGE_THRESHOLD: f32 = 6.0;
const PAGE_SCROLL_NOTCHES: i32 = 5;
const PAGE_BOUNDARY_PROBE_NOTCHES: i32 = 1;

pub(super) struct PageSnapshot {
    pub(super) roi: RoiObservation,
    pub(super) signature: Vec<u8>,
    pub(super) match_candidates: Vec<TemplateMatchCandidate>,
    pub(super) slot_samples: Vec<SlotSample>,
}

#[derive(Clone)]
pub(super) struct SlotSample {
    pub(super) row: u32,
    pub(super) col: u32,
    pub(super) page_y: f32,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) center_x: f32,
    pub(super) center_y: f32,
    pub(super) response: Vec<u8>,
}

pub(super) struct PageNavigator {
    recognizer: RecognizerSession,
    item_kind: ItemKind,
    classifier: TemplateClassifier,
}

pub(super) enum PageTurnResult {
    Moved {
        page: PageSnapshot,
        short: bool,
        directed_shift: Option<f32>,
    },
    NoMovement(PageSnapshot),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PageTurnInput {
    Full(ScrollDirection),
    Nudge(ScrollDirection),
}

impl PageTurnInput {
    pub(super) const fn direction(self) -> ScrollDirection {
        match self {
            Self::Full(direction) | Self::Nudge(direction) => direction,
        }
    }

    pub(super) const fn is_full(self) -> bool {
        matches!(self, Self::Full(_))
    }

    pub(super) const fn is_nudge(self) -> bool {
        matches!(self, Self::Nudge(_))
    }

    fn scroll_notches(self) -> i32 {
        let magnitude = match self {
            Self::Full(_) => PAGE_SCROLL_NOTCHES,
            Self::Nudge(_) => PAGE_BOUNDARY_PROBE_NOTCHES,
        };
        match self.direction() {
            ScrollDirection::Up => magnitude,
            ScrollDirection::Down => -magnitude,
        }
    }
}

impl PageNavigator {
    pub(super) fn new(
        recognizer: RecognizerSession,
        item_kind: ItemKind,
        classifier: TemplateClassifier,
    ) -> Self {
        Self {
            recognizer,
            item_kind,
            classifier,
        }
    }

    pub(super) fn item_kind(&self) -> ItemKind {
        self.item_kind
    }

    pub(super) fn recognizer(&self) -> RecognizerSession {
        self.recognizer
    }

    pub(super) fn perform_confirmed_semantic_page_turn(
        &self,
        automation: &mut AutomationSession<'_>,
        current_page: &PageSnapshot,
        input: PageTurnInput,
        wheel_attempt: u32,
    ) -> Result<PageTurnResult> {
        let span = debug_span!("confirmed_semantic_page_turn", wheel_attempt, ?input);
        let _guard = span.enter();

        automation.scroll(input.scroll_notches())?;
        self.observe_instant_viewport_change(automation, current_page, input)
    }

    fn observe_instant_viewport_change(
        &self,
        automation: &mut AutomationSession<'_>,
        current_page: &PageSnapshot,
        input: PageTurnInput,
    ) -> Result<PageTurnResult> {
        // Semantic anchors preserve measured page shifts when a target is
        // shared; a changed fresh frame covers pages with no recognized target.
        let start = Instant::now();
        let mut visual_reference = current_page.signature.clone();
        let mut same_viewport_frames = 0usize;
        let mut semantic_observations = 0usize;
        let direction = input.direction();
        let no_movement_grace = if input.is_nudge() {
            PAGE_BOUNDARY_NUDGE_NO_MOVEMENT_GRACE
        } else {
            PAGE_TURN_NO_MOVEMENT_GRACE
        };

        loop {
            let image = automation.capture()?;
            let signature = image_fingerprint(&image);
            let distance = fingerprint_distance(&visual_reference, &signature);
            let elapsed = start.elapsed();
            let no_movement_check = elapsed >= no_movement_grace;
            if distance < PAGE_CHANGE_THRESHOLD && !no_movement_check {
                continue;
            }

            let candidate = self.scan_direct_page(image)?;
            semantic_observations += 1;
            let relation = match compare_page_turn(
                &current_page.roi,
                &candidate.roi,
                self.item_kind,
                direction,
            ) {
                PageRelation::DifferentViewport | PageRelation::Uncertain
                    if distance < PAGE_CHANGE_THRESHOLD =>
                {
                    PageRelation::SameViewport
                }
                relation => relation,
            };
            let elapsed = start.elapsed();
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
                        directed_shift: Some(shift.directed_shift),
                    });
                }
                relation @ (PageRelation::DifferentViewport | PageRelation::Uncertain) => {
                    debug!(
                        target: "hd2_preset_helper::perf",
                        ?relation,
                        distance,
                        "page turn completed from changed frame"
                    );
                    return Ok(PageTurnResult::Moved {
                        page: candidate,
                        short: false,
                        directed_shift: None,
                    });
                }
                PageRelation::SameViewport => {
                    visual_reference = candidate.signature.clone();
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
                }
            }
        }
    }

    pub(super) fn scan_direct_page(&self, image: RgbaImage) -> Result<PageSnapshot> {
        let mut roi = self
            .recognizer
            .detect(image, SlotLayout::List(self.item_kind))?;
        if self.item_kind == ItemKind::Booster {
            // Only the initially opened page is known to be at the top. Later
            // pages let the temporary map identify the global no-booster cell.
            for slot in &mut roi.slots {
                if slot.kind == SlotKind::NoBoosterOption {
                    slot.kind = SlotKind::Booster;
                }
            }
        }
        self.prepare_page(roi)
    }

    pub(super) fn prepare_initial_page(&self, roi: RoiObservation) -> Result<PageSnapshot> {
        self.prepare_page(roi)
    }

    fn prepare_page(&self, mut roi: RoiObservation) -> Result<PageSnapshot> {
        let match_candidates = self.classifier.classify_batch(&mut roi)?;
        self.finish_direct_page(roi, match_candidates)
    }

    fn finish_direct_page(
        &self,
        roi: RoiObservation,
        match_candidates: Vec<TemplateMatchCandidate>,
    ) -> Result<PageSnapshot> {
        let signature = {
            let span = debug_span!("roi_fingerprint");
            let _guard = span.enter();
            image_fingerprint(&roi.image)
        };
        let slot_samples = roi
            .slots
            .iter()
            .filter(|slot| {
                slot.kind.is_selectable_item_for(self.item_kind)
                    || (self.item_kind == ItemKind::Booster
                        && slot.kind == SlotKind::NoBoosterOption)
            })
            .map(|slot| -> Result<_> {
                let core = slot_core_rect(slot);
                let response = if slot.kind == SlotKind::NoBoosterOption {
                    vec![0; (core.w * core.h) as usize]
                } else {
                    self.classifier.alignment_response(&roi.image, slot)?
                };
                let (center_x, center_y) = slot.center_f32();
                Ok(SlotSample {
                    row: slot.row,
                    col: slot.col,
                    page_y: center_y,
                    width: core.w,
                    height: core.h,
                    center_x: center_x - core.x as f32,
                    center_y: center_y - core.y as f32,
                    response,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(PageSnapshot {
            roi,
            signature,
            match_candidates,
            slot_samples,
        })
    }
}
