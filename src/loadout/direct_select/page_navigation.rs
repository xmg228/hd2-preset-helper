use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use image::RgbaImage;
use tracing::{debug, debug_span, trace, warn};

use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::vision::{
    ItemAvailability, RecognizerSession, RoiObservation, Slot, SlotKind, SlotLayout,
    TemplateClassifier, TemplateMatchCandidate,
};

use super::ScrollDirection;
use super::list_map::{ListMap, LocatedPage};

const PAGE_TURN_NO_MOVEMENT_GRACE: Duration = Duration::from_millis(200);
const PAGE_BOUNDARY_NUDGE_NO_MOVEMENT_GRACE: Duration = Duration::from_millis(150);
const PAGE_TURN_NO_MOVEMENT_FRAMES: usize = 2;
const PAGE_TURN_ALIGNMENT_TIMEOUT: Duration = Duration::from_millis(400);
const SCROLLBAR_MOVEMENT_TOLERANCE_PX: f32 = 0.75;
const PAGE_SCROLL_NOTCHES: i32 = 5;
const PAGE_BOUNDARY_PROBE_NOTCHES: i32 = 1;
const PAGE_ALIGNMENT_RECOVERY_NOTCHES: i32 = 2;

pub(super) struct PageSnapshot {
    pub(super) roi: RoiObservation,
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
    Moved(PageSnapshot),
    NoMovement(PageSnapshot),
    Recovered(PageSnapshot),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PageTurnInput {
    Full(ScrollDirection),
    Nudge(ScrollDirection),
    Recovery(ScrollDirection),
}

impl PageTurnInput {
    pub(super) const fn direction(self) -> ScrollDirection {
        match self {
            Self::Full(direction) | Self::Nudge(direction) | Self::Recovery(direction) => direction,
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
            Self::Recovery(_) => PAGE_ALIGNMENT_RECOVERY_NOTCHES,
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

    pub(super) fn item_availability(
        &self,
        image: &RgbaImage,
        slot: &Slot,
        item_id: &str,
    ) -> Result<ItemAvailability> {
        self.classifier.item_availability(image, slot, item_id)
    }

    pub(super) fn turn_page(
        &self,
        automation: &mut AutomationSession<'_>,
        list_map: &mut ListMap,
        current_page: &PageSnapshot,
        input: PageTurnInput,
        wheel_attempt: u32,
    ) -> Result<PageTurnResult> {
        let span = debug_span!("turn_page", wheel_attempt, ?input);
        let _guard = span.enter();

        automation.scroll(input.scroll_notches())?;
        let (page, placement) = self.observe_page_turn(
            automation,
            list_map,
            current_page,
            input,
            Some(input.direction()),
        )?;
        if let Some(placement) = placement {
            let moved = placement.has_moved();
            list_map.commit(&page, placement, "page-turn", automation.evidence());
            return Ok(if moved {
                PageTurnResult::Moved(page)
            } else {
                PageTurnResult::NoMovement(page)
            });
        }

        warn!(
            ?input,
            "page turn remained unconfirmed; scrolling back for recovery"
        );
        let recovery = PageTurnInput::Recovery(input.direction().opposite());
        automation.scroll(recovery.scroll_notches())?;
        // The rejected page was never committed. Recovery may land on either
        // side of the last confirmed page, or return to that same position.
        let (page, placement) =
            self.observe_page_turn(automation, list_map, &page, recovery, None)?;
        let placement = placement.context(
            "neither the page turn nor the recovery scroll could be confirmed in the temporary list map",
        )?;
        list_map.commit(&page, placement, "recovery", automation.evidence());
        Ok(PageTurnResult::Recovered(page))
    }

    fn observe_page_turn(
        &self,
        automation: &mut AutomationSession<'_>,
        list_map: &ListMap,
        current_page: &PageSnapshot,
        input: PageTurnInput,
        direction: Option<ScrollDirection>,
    ) -> Result<(PageSnapshot, Option<LocatedPage>)> {
        let start = Instant::now();
        let scrollbar_reference = current_page.roi.scrollbar;
        let mut same_viewport_frames = 0usize;
        let mut observations = 0usize;
        let no_movement_grace = if input.is_full() {
            PAGE_TURN_NO_MOVEMENT_GRACE
        } else {
            PAGE_BOUNDARY_NUDGE_NO_MOVEMENT_GRACE
        };

        loop {
            let image = automation.capture()?;
            let observed_elapsed = start.elapsed();
            let scrollbar = self.recognizer.detect_scrollbar(&image);
            let input_observed =
                scrollbar_reference
                    .zip(scrollbar)
                    .is_some_and(|(before, after)| {
                        let delta =
                            0.5 * (after.upper_y + after.lower_y - before.upper_y - before.lower_y);
                        match input.direction() {
                            ScrollDirection::Up => delta < -SCROLLBAR_MOVEMENT_TOLERANCE_PX,
                            ScrollDirection::Down => delta > SCROLLBAR_MOVEMENT_TOLERANCE_PX,
                        }
                    });
            let at_boundary = scrollbar.is_some_and(|bar| match input.direction() {
                ScrollDirection::Up => bar.at_top,
                ScrollDirection::Down => bar.at_bottom,
            });
            // Observe the input relative to its preceding frame (also during
            // recovery); map placement still uses the last committed page.
            if !input_observed && !at_boundary && observed_elapsed < no_movement_grace {
                continue;
            }

            let page = self.scan_direct_page(image)?;
            observations += 1;
            let mut assessment = list_map.locate_page(&page, direction);
            trace!(
                observations,
                located = assessment.placement.is_some(),
                elapsed_s = start.elapsed().as_secs_f64(),
                "page map observation"
            );

            if let Some(placement) = assessment.placement.as_ref() {
                let moved = placement.has_moved();
                // A visible thumb away from the requested endpoint disproves
                // a boundary. Only no-thumb Booster lists need a static probe.
                let static_booster = self.item_kind == ItemKind::Booster
                    && scrollbar_reference.is_none()
                    && scrollbar.is_none();
                let confirmed = if moved {
                    same_viewport_frames = 0;
                    input_observed || direction.is_none()
                } else {
                    same_viewport_frames += 1;
                    at_boundary
                        || ((direction.is_none() || static_booster)
                            && observed_elapsed >= no_movement_grace
                            && same_viewport_frames >= PAGE_TURN_NO_MOVEMENT_FRAMES)
                };
                if confirmed {
                    list_map.record_assessment(
                        automation.evidence(),
                        &page,
                        &assessment,
                        if moved {
                            "page-turn-moved"
                        } else {
                            "page-turn-no-movement"
                        },
                    );
                    debug!(
                        target: "hd2_preset_helper::perf",
                        vertical_shift = placement.vertical_shift,
                        elapsed_s = start.elapsed().as_secs_f64(),
                        same_viewport_frames,
                        "page turn confirmed by scrollbar and map evidence"
                    );
                    return Ok((page, assessment.placement.take()));
                }
            } else {
                same_viewport_frames = 0;
            }
            if start.elapsed() >= PAGE_TURN_ALIGNMENT_TIMEOUT && observations >= 2 {
                debug!(
                    observations,
                    slots = page.slot_samples.len(),
                    ?scrollbar_reference,
                    ?scrollbar,
                    input_observed,
                    elapsed_s = start.elapsed().as_secs_f64(),
                    timeout_s = PAGE_TURN_ALIGNMENT_TIMEOUT.as_secs_f64(),
                    "page turn remained unconfirmed by scrollbar and map evidence"
                );
                list_map.record_assessment(
                    automation.evidence(),
                    &page,
                    &assessment,
                    "page-turn-unconfirmed",
                );
                return Ok((page, None));
            }
        }
    }

    pub(super) fn scan_direct_page(
        &self,
        image: crate::capture::CapturedFrame,
    ) -> Result<PageSnapshot> {
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

    pub(super) fn prepare_page(&self, mut roi: RoiObservation) -> Result<PageSnapshot> {
        let match_candidates = self.classifier.classify_batch(&mut roi)?;
        let slot_samples = roi
            .slots
            .iter()
            .filter(|slot| {
                slot.kind.is_selectable_item_for(self.item_kind)
                    || (self.item_kind == ItemKind::Booster
                        && slot.kind == SlotKind::NoBoosterOption)
            })
            .map(|slot| -> Result<_> {
                let core = slot.core_rect();
                let response = if slot.kind == SlotKind::NoBoosterOption {
                    vec![0; (core.w * core.h) as usize]
                } else {
                    self.classifier
                        .alignment_response(&roi.image, slot)
                        .with_context(|| {
                            format!(
                                "failed to sample {} slot row {} col {} for map alignment",
                                self.item_kind.label(),
                                slot.row,
                                slot.col
                            )
                        })?
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
            match_candidates,
            slot_samples,
        })
    }
}
