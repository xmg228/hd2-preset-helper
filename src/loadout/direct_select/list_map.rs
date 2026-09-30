use std::collections::{HashMap, HashSet};

use tracing::debug;

use crate::item::ItemKind;
use crate::vision::{ItemAvailability, RoiObservation, Slot, SlotKind, TemplateMatchCandidate};

use super::ScrollDirection;
use super::page_navigation::{PageSnapshot, PageTurnInput, SlotSample};

#[cfg(feature = "diagnostics")]
#[path = "list_map_diagnostics.rs"]
mod diagnostics;

const PAGE_ALIGNMENT_MIN_MARGIN: f32 = 0.04;
const POSITION_TOLERANCE_PX: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SlotId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LocalPosition {
    row: u32,
    col: u32,
}

impl LocalPosition {
    const fn of(slot: &Slot) -> Self {
        Self {
            row: slot.row,
            col: slot.col,
        }
    }

    const fn of_sample(slot: &SlotSample) -> Self {
        Self {
            row: slot.row,
            col: slot.col,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct MappedCandidate {
    slot_id: SlotId,
    score: f64,
    match_margin: f32,
    gate_quality: f32,
    availability: ItemAvailability,
}

#[derive(Clone, Copy, Debug)]
struct CandidateEvidence {
    best: MappedCandidate,
    score_sum: f64,
    observations: u32,
}

impl CandidateEvidence {
    fn new(candidate: MappedCandidate) -> Self {
        Self {
            best: candidate,
            score_sum: candidate.score,
            observations: 1,
        }
    }

    fn record(&mut self, candidate: MappedCandidate) {
        self.score_sum += candidate.score;
        self.observations += 1;
        if candidate.score < self.best.score {
            self.best = candidate;
        }
    }

    fn candidate(self) -> MappedCandidate {
        MappedCandidate {
            score: self.score_sum / f64::from(self.observations),
            ..self.best
        }
    }
}

struct MapSlot {
    col: u32,
    content_y: f32,
    sample: SlotSample,
    edge_clearance: f32,
}

#[derive(Clone, Copy, Debug)]
struct PlacedSlot {
    local: LocalPosition,
    id: SlotId,
}

#[derive(Clone, Debug)]
struct PagePlacement {
    offset_y: f32,
    mean_score: f32,
    support: usize,
    slots: Vec<PlacedSlot>,
}

#[derive(Clone, Copy)]
struct AlignmentConstraints {
    direction: ScrollDirection,
    expected_offset_y: Option<f32>,
    allow_stationary: bool,
    excluded: Option<SlotId>,
}

#[derive(Clone, Copy, Debug)]
struct AlignmentCandidate {
    offset_y: f32,
    mean_score: f32,
    support: usize,
}

#[derive(Clone, Debug)]
pub(super) struct PostClickPlacement {
    page: PagePlacement,
    pub(super) vertical_shift: f32,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum NavigationHint {
    Unmapped,
    Scroll(ScrollDirection),
    ExpectedVisible,
}

pub(super) struct ListMap {
    item_kind: ItemKind,
    slots: Vec<MapSlot>,
    current: PagePlacement,
    items: HashMap<String, SlotId>,
    candidate_evidence: HashMap<String, HashMap<SlotId, CandidateEvidence>>,
    active_fallbacks: HashMap<String, MappedCandidate>,
    selected: HashSet<SlotId>,
    no_booster_option: Option<SlotId>,
}

impl ListMap {
    pub(super) fn new(page: &PageSnapshot, item_kind: ItemKind) -> Self {
        let mut map = Self {
            item_kind,
            slots: Vec::new(),
            current: PagePlacement {
                offset_y: 0.0,
                mean_score: 1.0,
                support: page.slot_samples.len(),
                slots: Vec::new(),
            },
            items: HashMap::new(),
            candidate_evidence: HashMap::new(),
            active_fallbacks: HashMap::new(),
            selected: HashSet::new(),
            no_booster_option: None,
        };
        let initial = map.placement_at_offset(page, 0.0, 1.0, page.slot_samples.len());
        map.commit_page(page, initial, "initial");
        map.no_booster_option = page
            .roi
            .slots
            .iter()
            .find(|slot| slot.kind == SlotKind::NoBoosterOption)
            .and_then(|slot| map.current_slot_id(slot));
        map
    }

    pub(super) fn advance(
        &mut self,
        page: &PageSnapshot,
        input: PageTurnInput,
        directed_shift: Option<f32>,
    ) -> bool {
        let direction = input.direction();
        let expected_offset_y = directed_shift.map(|shift| match direction {
            ScrollDirection::Down => self.current.offset_y + shift,
            ScrollDirection::Up => self.current.offset_y - shift,
        });
        let Some(placement) = self.find_placement(
            page,
            AlignmentConstraints {
                direction,
                expected_offset_y,
                allow_stationary: false,
                excluded: None,
            },
            "page-turn",
        ) else {
            return false;
        };
        self.commit_page(page, placement, "page-turn");
        true
    }

    pub(super) fn locate_after_click(
        &self,
        page: &PageSnapshot,
        clicked_slot: &Slot,
    ) -> Option<PostClickPlacement> {
        let clicked_id = self.current_slot_id(clicked_slot)?;
        let clicked_y = self.slots[clicked_id.0].content_y;
        let (visible_min, visible_max) = self.visible_content_y_range()?;
        let visible_midpoint = 0.5 * (visible_min + visible_max);
        let expected_direction = if clicked_y <= visible_midpoint {
            ScrollDirection::Up
        } else {
            ScrollDirection::Down
        };
        let placement = self.find_placement(
            page,
            AlignmentConstraints {
                direction: expected_direction,
                expected_offset_y: None,
                allow_stationary: true,
                excluded: Some(clicked_id),
            },
            "post-click",
        )?;
        let vertical_shift = self.current.offset_y - placement.offset_y;
        debug!(
            offset_y = placement.offset_y,
            vertical_shift,
            ?expected_direction,
            support = placement.support,
            mean_score = placement.mean_score,
            "post-click page located in temporary list map"
        );
        Some(PostClickPlacement {
            page: placement,
            vertical_shift,
        })
    }

    pub(super) fn slot_after_placement(
        &self,
        placement: &PostClickPlacement,
        page: &RoiObservation,
        previous_slot: &Slot,
    ) -> Option<Slot> {
        let id = self.current_slot_id(previous_slot)?;
        let local = placement
            .page
            .slots
            .iter()
            .find(|placed| placed.id == id)?
            .local;
        page.slots
            .iter()
            .find(|slot| LocalPosition::of(slot) == local)
            .cloned()
    }

    pub(super) fn commit_after_click(
        &mut self,
        page: &PageSnapshot,
        placement: PostClickPlacement,
    ) {
        self.commit_page(page, placement.page, "post-click");
    }

    fn find_placement(
        &self,
        page: &PageSnapshot,
        constraints: AlignmentConstraints,
        context: &'static str,
    ) -> Option<PagePlacement> {
        let candidates = self.alignment_candidates(page, constraints);
        let best = candidates.first().copied();
        let best = best?;
        let runner_up = candidates.get(1).copied();
        let margin = runner_up.map_or(1.0, |runner| best.mean_score - runner.mean_score);
        let accepted = runner_up.is_none_or(|runner| {
            best.support > runner.support || margin >= PAGE_ALIGNMENT_MIN_MARGIN
        });
        debug!(
            context,
            direction = ?constraints.direction,
            current_offset_y = self.current.offset_y,
            expected_offset_y = constraints.expected_offset_y,
            best_offset_y = best.offset_y,
            vertical_shift = self.current.offset_y - best.offset_y,
            algorithm = "semantic_cosine",
            best_mean_score = best.mean_score,
            best_support = best.support,
            runner_up_mean_score = runner_up.map(|candidate| candidate.mean_score),
            runner_up_support = runner_up.map(|candidate| candidate.support),
            margin,
            accepted,
            candidates = %format_args!("{:.3?}", candidates.iter().take(8).collect::<Vec<_>>()),
            "continuous slot map alignment evaluated"
        );
        accepted
            .then(|| self.placement_at_offset(page, best.offset_y, best.mean_score, best.support))
    }

    fn alignment_candidates(
        &self,
        page: &PageSnapshot,
        constraints: AlignmentConstraints,
    ) -> Vec<AlignmentCandidate> {
        let offsets = page.slot_samples.iter().flat_map(|current| {
            self.slots
                .iter()
                .enumerate()
                .filter(move |(index, mapped)| {
                    let id = SlotId(*index);
                    mapped.col == current.col
                        && !self.selected.contains(&id)
                        && self.no_booster_option != Some(id)
                        && constraints.excluded != Some(id)
                })
                .map(move |(_, mapped)| mapped.content_y - current.page_y)
                .filter(move |offset_y| {
                    constraints.expected_offset_y.is_none_or(|expected| {
                        (*offset_y - expected).abs() <= POSITION_TOLERANCE_PX
                    })
                })
        });
        let mut candidates = offset_hypotheses(offsets)
            .into_iter()
            .filter(|&offset_y| {
                self.direction_allows(
                    offset_y,
                    constraints.direction,
                    constraints.allow_stationary,
                )
            })
            .filter_map(|offset_y| self.evaluate_alignment(page, offset_y, constraints.excluded))
            .collect::<Vec<_>>();

        // Prefer page-level evidence. A single overlapping slot remains a
        // fallback only when no candidate contains two or more observations.
        if candidates.iter().any(|candidate| candidate.support >= 2) {
            candidates.retain(|candidate| candidate.support >= 2);
        }
        candidates.sort_by(compare_alignment);
        candidates
    }

    fn evaluate_alignment(
        &self,
        page: &PageSnapshot,
        offset_y: f32,
        excluded: Option<SlotId>,
    ) -> Option<AlignmentCandidate> {
        let mut scores = Vec::new();
        let mut offsets = Vec::new();

        for current in &page.slot_samples {
            let expected_y = current.page_y + offset_y;
            let Some((mapped, mapped_offset)) = self
                .slots
                .iter()
                .enumerate()
                .filter(|(index, mapped)| {
                    let id = SlotId(*index);
                    mapped.col == current.col
                        && !self.selected.contains(&id)
                        && self.no_booster_option != Some(id)
                        && excluded != Some(id)
                })
                .map(|(_, mapped)| {
                    (
                        mapped,
                        mapped.content_y - current.page_y,
                        (mapped.content_y - expected_y).abs(),
                    )
                })
                .filter(|(_, _, distance)| *distance <= POSITION_TOLERANCE_PX)
                .min_by(|left, right| left.2.total_cmp(&right.2))
                .map(|(mapped, mapped_offset, _)| (mapped, mapped_offset))
            else {
                continue;
            };
            let Some(score) = best_shifted_response_cosine(&mapped.sample, current) else {
                continue;
            };
            scores.push(score);
            offsets.push(mapped_offset);
        }

        if scores.is_empty() {
            return None;
        }
        offsets.sort_unstable_by(f32::total_cmp);
        Some(AlignmentCandidate {
            offset_y: interpolated_median(&offsets),
            mean_score: scores.iter().sum::<f32>() / scores.len() as f32,
            support: scores.len(),
        })
    }

    fn direction_allows(
        &self,
        offset_y: f32,
        direction: ScrollDirection,
        allow_stationary: bool,
    ) -> bool {
        let movement = offset_y - self.current.offset_y;
        match direction {
            ScrollDirection::Up => {
                movement
                    <= if allow_stationary {
                        POSITION_TOLERANCE_PX
                    } else {
                        -POSITION_TOLERANCE_PX
                    }
            }
            ScrollDirection::Down => {
                movement
                    >= if allow_stationary {
                        -POSITION_TOLERANCE_PX
                    } else {
                        POSITION_TOLERANCE_PX
                    }
            }
        }
    }

    fn placement_at_offset(
        &self,
        page: &PageSnapshot,
        offset_y: f32,
        mean_score: f32,
        support: usize,
    ) -> PagePlacement {
        let slots = page
            .slot_samples
            .iter()
            .filter_map(|current| {
                let content_y = current.page_y + offset_y;
                let (index, _) = self
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(_, mapped)| mapped.col == current.col)
                    .map(|(index, mapped)| (index, (mapped.content_y - content_y).abs()))
                    .filter(|(_, distance)| *distance <= POSITION_TOLERANCE_PX)
                    .min_by(|left, right| left.1.total_cmp(&right.1))?;
                Some(PlacedSlot {
                    local: LocalPosition::of_sample(current),
                    id: SlotId(index),
                })
            })
            .collect();
        PagePlacement {
            offset_y,
            mean_score,
            support,
            slots,
        }
    }

    fn commit_page(
        &mut self,
        page: &PageSnapshot,
        mut placement: PagePlacement,
        context: &'static str,
    ) {
        let min_y = page
            .slot_samples
            .iter()
            .map(|slot| slot.page_y)
            .min_by(f32::total_cmp)
            .unwrap_or(0.0);
        let max_y = page
            .slot_samples
            .iter()
            .map(|slot| slot.page_y)
            .max_by(f32::total_cmp)
            .unwrap_or(0.0);

        for current in &page.slot_samples {
            let local = LocalPosition::of_sample(current);
            let edge_clearance = (current.page_y - min_y).min(max_y - current.page_y);
            if let Some(placed) = placement.slots.iter().find(|placed| placed.local == local) {
                let mapped = &mut self.slots[placed.id.0];
                if !self.selected.contains(&placed.id) && edge_clearance > mapped.edge_clearance {
                    mapped.sample = current.clone();
                    mapped.edge_clearance = edge_clearance;
                }
                continue;
            }

            let id = SlotId(self.slots.len());
            self.slots.push(MapSlot {
                col: current.col,
                content_y: current.page_y + placement.offset_y,
                sample: current.clone(),
                edge_clearance,
            });
            placement.slots.push(PlacedSlot { local, id });
        }

        self.current = placement;
        self.record_current(&page.roi);
        self.record_candidates(&page.match_candidates);
        debug!(
            context,
            offset_y = self.current.offset_y,
            mean_score = self.current.mean_score,
            support = self.current.support,
            mapped_slots = self.slots.len(),
            "temporary list map placed in continuous list coordinates"
        );
    }

    pub(super) fn record_page(&mut self, page: &PageSnapshot) {
        let placement = self.placement_at_offset(
            page,
            self.current.offset_y,
            self.current.mean_score,
            self.current.support,
        );
        self.commit_page(page, placement, "same-viewport");
    }

    fn record_current(&mut self, page: &RoiObservation) {
        for slot in page
            .slots
            .iter()
            .filter(|slot| slot.kind.is_selectable_item_for(self.item_kind))
        {
            let Some(classification) = &slot.classification else {
                continue;
            };
            let Some(id) = self.current_slot_id(slot) else {
                continue;
            };
            if self.no_booster_option != Some(id) && !self.selected.contains(&id) {
                self.items.insert(classification.item_id.clone(), id);
            }
        }
    }

    pub(super) fn mark_selected(&mut self, slot: &Slot) {
        let Some(id) = self.current_slot_id(slot) else {
            return;
        };
        self.selected.insert(id);
        self.items.retain(|_, mapped| *mapped != id);
        self.candidate_evidence.retain(|_, candidates| {
            candidates.remove(&id);
            !candidates.is_empty()
        });
        self.active_fallbacks
            .retain(|_, candidate| candidate.slot_id != id);
    }

    pub(super) fn can_select_slot(&self, slot: &Slot) -> bool {
        self.current_slot_id(slot)
            .is_some_and(|id| self.no_booster_option != Some(id) && !self.selected.contains(&id))
    }

    pub(super) fn contains_item(&self, item_id: &str) -> bool {
        self.items.contains_key(item_id)
    }

    fn record_candidates(&mut self, candidates: &[TemplateMatchCandidate]) {
        for candidate in candidates {
            let Some(slot_id) = self.current_slot_id(&candidate.slot) else {
                continue;
            };
            let mapped = MappedCandidate {
                slot_id,
                score: candidate.score,
                match_margin: candidate.match_margin,
                gate_quality: candidate.gate_quality,
                availability: candidate.availability,
            };
            if self.no_booster_option == Some(slot_id) || self.selected.contains(&slot_id) {
                continue;
            }
            self.candidate_evidence
                .entry(candidate.item_id.clone())
                .or_default()
                .entry(slot_id)
                .and_modify(|evidence| evidence.record(mapped))
                .or_insert_with(|| CandidateEvidence::new(mapped));
        }
    }

    pub(super) fn activate_best_candidate(&mut self, item_id: &str) -> Option<f64> {
        let candidates = self.candidate_evidence.get(item_id)?;
        let (candidate, observations) = candidates
            .values()
            .filter(|evidence| !self.selected.contains(&evidence.best.slot_id))
            .map(|evidence| (evidence.candidate(), evidence.observations))
            .min_by(|(left, _), (right, _)| left.score.total_cmp(&right.score))?;
        self.items.insert(item_id.to_string(), candidate.slot_id);
        self.active_fallbacks.insert(item_id.to_string(), candidate);
        let mapped = &self.slots[candidate.slot_id.0];
        debug!(
            item_id,
            content_y = mapped.content_y,
            col = mapped.col,
            score = candidate.score,
            observations,
            candidate_positions = candidates.len(),
            "Top-1 fallback selected from accumulated slot evidence"
        );
        Some(candidate.score)
    }

    pub(super) fn visible_mapped_target(
        &self,
        item_id: &str,
        page: &RoiObservation,
    ) -> Option<TemplateMatchCandidate> {
        let id = *self.items.get(item_id)?;
        if self.selected.contains(&id) {
            return None;
        }
        let local = self
            .current
            .slots
            .iter()
            .find(|placed| placed.id == id)?
            .local;
        let slot = page.slots.iter().find(|slot| {
            slot.kind.is_selectable_item_for(self.item_kind) && LocalPosition::of(slot) == local
        })?;
        let candidate = self.active_fallbacks.get(item_id).copied().or_else(|| {
            self.candidate_evidence
                .get(item_id)?
                .get(&id)
                .copied()
                .map(CandidateEvidence::candidate)
        });
        Some(TemplateMatchCandidate {
            item_id: item_id.to_string(),
            slot: slot.clone(),
            score: candidate.map_or(1.0, |candidate| candidate.score),
            match_margin: candidate.map_or(0.0, |candidate| candidate.match_margin),
            gate_quality: candidate.map_or(0.0, |candidate| candidate.gate_quality),
            availability: candidate.map_or(ItemAvailability::Available, |candidate| {
                candidate.availability
            }),
        })
    }

    pub(super) fn navigation_hint(&self, item_id: &str) -> NavigationHint {
        let Some(&target_id) = self.items.get(item_id) else {
            return NavigationHint::Unmapped;
        };
        if self
            .current
            .slots
            .iter()
            .any(|placed| placed.id == target_id)
        {
            return NavigationHint::ExpectedVisible;
        }

        let target_y = self.slots[target_id.0].content_y;
        let Some((visible_min, visible_max)) = self.visible_content_y_range() else {
            return NavigationHint::ExpectedVisible;
        };

        if target_y < visible_min {
            NavigationHint::Scroll(ScrollDirection::Up)
        } else if target_y > visible_max {
            NavigationHint::Scroll(ScrollDirection::Down)
        } else {
            NavigationHint::ExpectedVisible
        }
    }

    fn visible_content_y_range(&self) -> Option<(f32, f32)> {
        let mut visible = self
            .current
            .slots
            .iter()
            .map(|placed| self.slots[placed.id.0].content_y);
        let first = visible.next()?;
        Some(visible.fold((first, first), |(min, max), y| (min.min(y), max.max(y))))
    }

    fn current_slot_id(&self, slot: &Slot) -> Option<SlotId> {
        let local = LocalPosition::of(slot);
        self.current
            .slots
            .iter()
            .find(|placed| placed.local == local)
            .map(|placed| placed.id)
    }
}

fn offset_hypotheses(offsets: impl Iterator<Item = f32>) -> Vec<f32> {
    let mut offsets = offsets.collect::<Vec<_>>();
    offsets.sort_unstable_by(f32::total_cmp);
    let mut hypotheses = Vec::new();
    let mut start = 0;
    while start < offsets.len() {
        let mut end = start + 1;
        while end < offsets.len() && offsets[end] - offsets[end - 1] <= POSITION_TOLERANCE_PX {
            end += 1;
        }
        hypotheses.push(interpolated_median(&offsets[start..end]));
        start = end;
    }
    hypotheses
}

fn interpolated_median(values: &[f32]) -> f32 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[middle - 1] + values[middle])
    } else {
        values[middle]
    }
}

fn compare_alignment(left: &AlignmentCandidate, right: &AlignmentCandidate) -> std::cmp::Ordering {
    right
        .mean_score
        .total_cmp(&left.mean_score)
        .then_with(|| right.support.cmp(&left.support))
}

fn best_shifted_response_cosine(left: &SlotSample, right: &SlotSample) -> Option<f32> {
    (-1..=1)
        .filter_map(|dy| shifted_response_cosine(left, right, dy))
        .max_by(f32::total_cmp)
}

fn shifted_response_cosine(left: &SlotSample, right: &SlotSample, dy: i32) -> Option<f32> {
    let offset_x = (right.center_x - left.center_x).round() as i32;
    let offset_y = (right.center_y - left.center_y).round() as i32 + dy;
    let mut count = 0usize;
    let mut dot = 0.0f64;
    let mut left_sq = 0.0f64;
    let mut right_sq = 0.0f64;

    for left_y in 0..left.height as i32 {
        let right_y = left_y + offset_y;
        if !(0..right.height as i32).contains(&right_y) {
            continue;
        }
        for left_x in 0..left.width as i32 {
            let right_x = left_x + offset_x;
            if !(0..right.width as i32).contains(&right_x) {
                continue;
            }
            let left_value =
                left.response[left_y as usize * left.width as usize + left_x as usize] as f64;
            let right_value =
                right.response[right_y as usize * right.width as usize + right_x as usize] as f64;
            count += 1;
            dot += left_value * right_value;
            left_sq += left_value * left_value;
            right_sq += right_value * right_value;
        }
    }

    if count < 64 {
        return None;
    }
    let denominator = (left_sq * right_sq).sqrt();
    (denominator > 1e-8).then(|| (dot / denominator).clamp(0.0, 1.0) as f32)
}
