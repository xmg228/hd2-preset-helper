use std::collections::BTreeMap;

use crate::item::ItemKind;
use crate::layout::{ROI_REFERENCE_H, ROI_REFERENCE_W};
use crate::vision::{RoiObservation, Slot};

const PAGE_RELATION_MIN_MATCHES: usize = 4;
const PAGE_RELATION_MIN_INLIER_RATIO: f32 = 0.70;
// Explicit page turns start from a clean pre-input page, so only a small
// geometry jitter allowance is needed to distinguish zero movement.
const PAGE_SHIFT_JITTER_PX: f32 = 2.0;
// A full downward list turn moves content 338 px upward in the 832 px
// reference ROI.
const PAGE_TURN_SHIFT_REFERENCE_PX: f32 = 338.0;
pub(super) const PAGE_TURN_FULL_SHIFT_MIN_RATIO: f32 = 0.80;

#[derive(Debug)]
pub(super) enum PageRelation {
    SameViewport,
    Shifted(PageShift),
    DifferentViewport,
    Uncertain,
}

#[derive(Debug)]
pub(super) struct PageShift {
    pub(super) directed_shift: f32,
    pub(super) expected_full_shift: f32,
    pub(super) shift_ratio: f32,
    pub(super) reached_end: bool,
}

pub(super) fn compare_page_turn(
    previous: &RoiObservation,
    current: &RoiObservation,
    item_kind: ItemKind,
) -> PageRelation {
    let previous_identity_count = identity_slots(previous, item_kind).count();
    let current_identity_count = identity_slots(current, item_kind).count();
    if previous_identity_count < PAGE_RELATION_MIN_MATCHES
        || current_identity_count < PAGE_RELATION_MIN_MATCHES
    {
        return PageRelation::Uncertain;
    }

    let row_pitch = estimate_row_pitch(previous, item_kind);
    let x_tolerance = 10.0 * previous.image.width() as f32 / ROI_REFERENCE_W as f32;
    let dy_tolerance =
        (row_pitch * 0.12).max(8.0 * previous.image.height() as f32 / ROI_REFERENCE_H as f32);
    let mut used_current = vec![false; current.slots.len()];
    let mut shifts = Vec::new();

    for previous_slot in identity_slots(previous, item_kind) {
        let Some(previous_classification) = previous_slot.classification.as_ref() else {
            continue;
        };
        let (previous_x, previous_y) = previous_slot.center_f32();

        let best = current
            .slots
            .iter()
            .enumerate()
            .filter(|(index, slot)| !used_current[*index] && is_identity_slot(slot, item_kind))
            .filter_map(|(index, slot)| {
                let classification = slot.classification.as_ref()?;
                if classification.item_id != previous_classification.item_id
                    || previous_slot.col != slot.col
                {
                    return None;
                }
                let (current_x, current_y) = slot.center_f32();
                let dx = current_x - previous_x;
                if dx.abs() > x_tolerance {
                    return None;
                }
                Some((index, dx.abs(), current_y - previous_y))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1));

        if let Some((index, _, dy)) = best {
            used_current[index] = true;
            shifts.push(dy);
        }
    }

    let matches = shifts.len();
    if matches < PAGE_RELATION_MIN_MATCHES {
        if previous_identity_count >= 8 && current_identity_count >= 8 && matches <= 1 {
            return PageRelation::DifferentViewport;
        }
        return PageRelation::Uncertain;
    }

    let median_dy = interpolated_median(&mut shifts).unwrap_or(0.0);
    let inliers = shifts
        .iter()
        .filter(|dy| (**dy - median_dy).abs() <= dy_tolerance)
        .count();
    if inliers as f32 / (matches as f32) < PAGE_RELATION_MIN_INLIER_RATIO {
        return PageRelation::Uncertain;
    }
    if median_dy.abs() <= PAGE_SHIFT_JITTER_PX {
        return PageRelation::SameViewport;
    }
    if median_dy >= -PAGE_SHIFT_JITTER_PX {
        return PageRelation::Uncertain;
    }

    let directed_shift = -median_dy;
    let expected_full_shift =
        PAGE_TURN_SHIFT_REFERENCE_PX * previous.image.height() as f32 / ROI_REFERENCE_H as f32;
    let shift_ratio = directed_shift / expected_full_shift;
    PageRelation::Shifted(PageShift {
        directed_shift,
        expected_full_shift,
        shift_ratio,
        reached_end: shift_ratio < PAGE_TURN_FULL_SHIFT_MIN_RATIO,
    })
}

fn identity_slots<'a>(
    result: &'a RoiObservation,
    item_kind: ItemKind,
) -> impl Iterator<Item = &'a Slot> + 'a {
    result
        .slots
        .iter()
        .filter(move |slot| is_identity_slot(slot, item_kind))
}

fn is_identity_slot(slot: &Slot, item_kind: ItemKind) -> bool {
    slot.kind.is_selectable_item_for(item_kind) && slot.classification.is_some()
}

fn estimate_row_pitch(result: &RoiObservation, item_kind: ItemKind) -> f32 {
    let mut rows: BTreeMap<u32, Vec<f32>> = BTreeMap::new();
    for slot in result
        .slots
        .iter()
        .filter(|slot| is_identity_slot(slot, item_kind))
    {
        rows.entry(slot.row).or_default().push(slot.center_f32().1);
    }

    let mut centers: Vec<f32> = rows
        .into_values()
        .filter_map(|mut values| interpolated_median(&mut values))
        .collect();
    centers.sort_by(f32::total_cmp);
    let mut differences: Vec<f32> = centers
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|difference| *difference > 1.0)
        .collect();

    interpolated_median(&mut differences)
        .unwrap_or(113.0 * result.image.height() as f32 / ROI_REFERENCE_H as f32)
}

fn interpolated_median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[middle - 1] + values[middle]) * 0.5)
    } else {
        Some(values[middle])
    }
}
