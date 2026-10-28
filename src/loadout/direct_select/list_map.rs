use std::collections::{HashMap, HashSet};

use serde::Serialize;
use tracing::debug;

use crate::item::ItemKind;
use crate::vision::{RoiObservation, ScrollbarObservation, Slot, SlotKind, TemplateMatchCandidate};

use super::ScrollDirection;
use super::click_plan::DirectClickTarget;
use super::page_navigation::{PageSnapshot, SlotSample};

mod evidence;

const PAGE_ALIGNMENT_MIN_SCORE: f32 = 0.65;
const POSITION_TOLERANCE_PX: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub(super) struct SlotId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
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

#[derive(Clone, Copy, Debug, Serialize)]
struct PlacedSlot {
    local: LocalPosition,
    id: SlotId,
}

#[derive(Clone, Debug, Serialize)]
struct PagePlacement {
    offset_y: f32,
    mean_score: f32,
    support: usize,
    slots: Vec<PlacedSlot>,
}

#[derive(Clone, Copy, Serialize)]
struct AlignmentConstraints {
    direction: Option<ScrollDirection>,
    excluded: Option<SlotId>,
}

#[derive(Debug, Serialize)]
struct AlignmentPrediction {
    offset_y: f32,
    radius: f32,
    source: &'static str,
}

#[derive(Debug, Serialize)]
struct AlignmentEvaluation {
    offset_y: f32,
    mean_score: f32,
    support: usize,
    pairs: Vec<AlignmentPair>,
    rejection: Option<AlignmentRejection>,
}

#[derive(Debug, Serialize)]
struct AlignmentPair {
    current: usize,
    mapped: usize,
    cosine: f32,
}

#[derive(Debug, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
enum AlignmentRejection {
    NoGeometryPairs,
    NoVisualPairs,
    WrongDirection,
    GeometryConflict {
        current: usize,
        mapped: usize,
        center_distance: f32,
        overlap: f32,
    },
}

#[derive(Serialize)]
pub(super) struct AlignmentAssessment {
    constraints: AlignmentConstraints,
    current_offset_y: f32,
    required_score: f32,
    reason: &'static str,
    prediction: Option<AlignmentPrediction>,
    evaluation: Option<AlignmentEvaluation>,
    pub(super) placement: Option<LocatedPage>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LocatedPage {
    page: PagePlacement,
    pub(super) vertical_shift: f32,
}

impl LocatedPage {
    pub(super) fn has_moved(&self) -> bool {
        self.vertical_shift.abs() > POSITION_TOLERANCE_PX
    }
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
    scrollbar: Option<ScrollbarObservation>,
    items: HashMap<String, SlotId>,
    candidate_evidence: HashMap<String, HashMap<SlotId, CandidateEvidence>>,
    active_fallbacks: HashMap<String, MappedCandidate>,
    selected: HashSet<SlotId>,
    no_booster_option: Option<SlotId>,
}

impl ListMap {
    pub(super) fn new(
        page: &PageSnapshot,
        item_kind: ItemKind,
        evidence: &crate::evidence::OperationEvidence,
    ) -> Self {
        let mut map = Self {
            item_kind,
            slots: Vec::new(),
            current: PagePlacement {
                offset_y: 0.0,
                mean_score: 1.0,
                support: page.slot_samples.len(),
                slots: Vec::new(),
            },
            scrollbar: None,
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
        evidence::record_commit(evidence, &map, page, "initial");
        map
    }

    pub(super) fn locate_page(
        &self,
        page: &PageSnapshot,
        direction: Option<ScrollDirection>,
    ) -> AlignmentAssessment {
        self.find_placement(
            page,
            AlignmentConstraints {
                direction,
                excluded: None,
            },
        )
    }

    pub(super) fn locate_after_click(
        &self,
        page: &PageSnapshot,
        clicked_id: SlotId,
    ) -> AlignmentAssessment {
        let clicked_y = self.slots[clicked_id.0].content_y;
        let midpoint = self
            .visible_content_y_range()
            .map(|(min, max)| 0.5 * (min + max));
        let direction = midpoint.map(|midpoint| {
            if clicked_y <= midpoint {
                ScrollDirection::Up
            } else {
                ScrollDirection::Down
            }
        });
        let mut assessment = self.find_placement(
            page,
            AlignmentConstraints {
                direction,
                excluded: Some(clicked_id),
            },
        );
        if midpoint.is_none() {
            assessment.reason = "no_visible_map";
            assessment.placement = None;
        }
        assessment
    }

    pub(super) fn record_assessment(
        &self,
        evidence: &crate::evidence::OperationEvidence,
        page: &PageSnapshot,
        assessment: &AlignmentAssessment,
        context: &str,
    ) {
        evidence::record(evidence, self, page, assessment, context);
    }

    pub(super) fn slot_after_placement(
        &self,
        placement: &LocatedPage,
        page: &RoiObservation,
        id: SlotId,
    ) -> Option<Slot> {
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

    pub(super) fn commit(
        &mut self,
        page: &PageSnapshot,
        placement: LocatedPage,
        context: &'static str,
        evidence: &crate::evidence::OperationEvidence,
    ) {
        self.commit_page(page, placement.page, context);
        evidence::record_commit(evidence, self, page, context);
    }

    fn find_placement(
        &self,
        page: &PageSnapshot,
        constraints: AlignmentConstraints,
    ) -> AlignmentAssessment {
        let prediction = self.predict_alignment(page);
        let evaluation = prediction
            .as_ref()
            .map(|prediction| self.evaluate_alignment(page, prediction, constraints));
        let reason = match evaluation.as_ref() {
            None => "scrollbar_unavailable",
            Some(result) if result.rejection.is_some() => "rejected",
            Some(result) if result.mean_score < PAGE_ALIGNMENT_MIN_SCORE => "low_score",
            Some(_) => "accepted",
        };
        let placement = evaluation
            .as_ref()
            .filter(|_| reason == "accepted")
            .map(|result| LocatedPage {
                page: self.placement_at_offset(
                    page,
                    result.offset_y,
                    result.mean_score,
                    result.support,
                ),
                vertical_shift: self.current.offset_y - result.offset_y,
            });
        debug!(
            direction = ?constraints.direction, excluded = ?constraints.excluded,
            current_offset_y = self.current.offset_y,
            predicted_offset_y = prediction.as_ref().map(|prediction| prediction.offset_y),
            offset_y = evaluation.as_ref().map(|result| result.offset_y),
            mean_score = evaluation.as_ref().map(|result| result.mean_score),
            support = evaluation.as_ref().map(|result| result.support),
            required_score = PAGE_ALIGNMENT_MIN_SCORE, reason,
            "local slot map alignment evaluated"
        );
        AlignmentAssessment {
            constraints,
            current_offset_y: self.current.offset_y,
            required_score: PAGE_ALIGNMENT_MIN_SCORE,
            reason,
            placement,
            prediction,
            evaluation,
        }
    }

    fn predict_alignment(&self, page: &PageSnapshot) -> Option<AlignmentPrediction> {
        match (self.scrollbar, page.roi.scrollbar) {
            (Some(before), Some(after)) => {
                let ratio = 0.5 * (before.length_ratio + after.length_ratio);
                let delta = 0.5 * (after.upper_y + after.lower_y - before.upper_y - before.lower_y);
                Some(AlignmentPrediction {
                    offset_y: self.current.offset_y + delta / ratio,
                    // A quarter of the core height stays well inside a half-row:
                    // enough to refine the coarse estimate, not choose another row.
                    radius: (page.slot_samples.first()?.height as f32 * 0.25)
                        .max(POSITION_TOLERANCE_PX),
                    source: "scrollbar",
                })
            }
            (None, None) if self.item_kind == ItemKind::Booster => {
                // A fitting Booster list has no thumb. Test the existing position
                // with geometry AND icon evidence; absence alone confirms nothing.
                Some(AlignmentPrediction {
                    offset_y: self.current.offset_y,
                    radius: POSITION_TOLERANCE_PX,
                    source: "same_position",
                })
            }
            _ => None,
        }
    }

    fn evaluate_alignment(
        &self,
        page: &PageSnapshot,
        prediction: &AlignmentPrediction,
        constraints: AlignmentConstraints,
    ) -> AlignmentEvaluation {
        let mut offsets = page
            .slot_samples
            .iter()
            .filter_map(|current| {
                let index =
                    self.nearest_mapped_slot(current, prediction.offset_y, prediction.radius)?;
                Some(self.slots[index].content_y - current.page_y)
            })
            .collect::<Vec<_>>();
        let mut result = AlignmentEvaluation {
            offset_y: prediction.offset_y,
            mean_score: 0.0,
            support: 0,
            pairs: Vec::new(),
            rejection: None,
        };
        if offsets.is_empty() {
            result.rejection = Some(AlignmentRejection::NoGeometryPairs);
            return result;
        }
        offsets.sort_unstable_by(f32::total_cmp);
        result.offset_y = interpolated_median(&offsets);
        if constraints
            .direction
            .is_some_and(|direction| !self.direction_allows(result.offset_y, direction))
        {
            result.rejection = Some(AlignmentRejection::WrongDirection);
            return result;
        }
        // Excluded/selected cells still constrain geometry. Preserve the first
        // actual conflict, rather than losing it in an Option/filter chain.
        for (current_index, current) in page.slot_samples.iter().enumerate() {
            let content_y = current.page_y + result.offset_y;
            let top = content_y - current.center_y;
            let bottom = top + current.height as f32;
            for (mapped_index, mapped) in self
                .slots
                .iter()
                .enumerate()
                .filter(|(_, mapped)| mapped.col == current.col)
            {
                let center_distance = (mapped.content_y - content_y).abs();
                let mapped_top = mapped.content_y - mapped.sample.center_y;
                let mapped_bottom = mapped_top + mapped.sample.height as f32;
                let overlap = bottom.min(mapped_bottom) - top.max(mapped_top);
                if center_distance > POSITION_TOLERANCE_PX && overlap > POSITION_TOLERANCE_PX {
                    result.rejection = Some(AlignmentRejection::GeometryConflict {
                        current: current_index,
                        mapped: mapped_index,
                        center_distance,
                        overlap,
                    });
                    return result;
                }
            }
        }
        // Geometry chooses the single rigid alignment. Appearance only verifies
        // it; selected, empty and just-clicked cells cannot supply icon evidence.
        for (current_index, current) in page.slot_samples.iter().enumerate() {
            let Some(index) =
                self.nearest_mapped_slot(current, result.offset_y, POSITION_TOLERANCE_PX)
            else {
                continue;
            };
            let id = SlotId(index);
            if self.selected.contains(&id)
                || self.no_booster_option == Some(id)
                || constraints.excluded == Some(id)
            {
                continue;
            }
            if let Some(cosine) = best_shifted_response_cosine(&self.slots[index].sample, current) {
                result.pairs.push(AlignmentPair {
                    current: current_index,
                    mapped: index,
                    cosine,
                });
            }
        }
        result.support = result.pairs.len();
        if result.support == 0 {
            result.rejection = Some(AlignmentRejection::NoVisualPairs);
        } else {
            result.mean_score =
                result.pairs.iter().map(|pair| pair.cosine).sum::<f32>() / result.support as f32;
        }
        result
    }

    fn nearest_mapped_slot(
        &self,
        current: &SlotSample,
        offset_y: f32,
        tolerance: f32,
    ) -> Option<usize> {
        let expected_y = current.page_y + offset_y;
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, mapped)| mapped.col == current.col)
            .map(|(index, mapped)| (index, (mapped.content_y - expected_y).abs()))
            .filter(|(_, distance)| *distance <= tolerance)
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index)
    }

    fn direction_allows(&self, offset_y: f32, direction: ScrollDirection) -> bool {
        let movement = offset_y - self.current.offset_y;
        match direction {
            ScrollDirection::Up => movement <= POSITION_TOLERANCE_PX,
            ScrollDirection::Down => movement >= -POSITION_TOLERANCE_PX,
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
                let index = self.nearest_mapped_slot(current, offset_y, POSITION_TOLERANCE_PX)?;
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
        self.scrollbar = page.roi.scrollbar;
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

    pub(super) fn mark_selected(&mut self, id: SlotId) {
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
    ) -> Option<DirectClickTarget> {
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
        Some(DirectClickTarget {
            item_id: item_id.to_string(),
            slot: slot.clone(),
            match_error: candidate.map_or(1.0, |candidate| candidate.score as f32),
            match_margin: candidate.map_or(0.0, |candidate| candidate.match_margin),
            gate_quality: candidate.map_or(0.0, |candidate| candidate.gate_quality),
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

    pub(super) fn current_slot_id(&self, slot: &Slot) -> Option<SlotId> {
        let local = LocalPosition::of(slot);
        self.current
            .slots
            .iter()
            .find(|placed| placed.local == local)
            .map(|placed| placed.id)
    }
}

fn interpolated_median(values: &[f32]) -> f32 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[middle - 1] + values[middle])
    } else {
        values[middle]
    }
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
