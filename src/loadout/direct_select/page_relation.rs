use crate::item::ItemKind;
use crate::vision::{ROI_REFERENCE_H, RoiObservation, Slot};

use super::ScrollDirection;

// Explicit page turns start from a clean pre-input page, so only a small
// geometry jitter allowance is needed to distinguish zero movement.
const PAGE_SHIFT_JITTER_PX: f32 = 2.0;
// A full list turn moves content about 338 px in the 832 px reference ROI.
const PAGE_TURN_SHIFT_REFERENCE_PX: f32 = 338.0;
pub(super) const PAGE_TURN_SHORT_THRESHOLD_RATIO: f32 = 0.80;

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
    pub(super) shift_ratio: f32,
}

pub(super) fn compare_page_turn(
    previous: &RoiObservation,
    current: &RoiObservation,
    item_kind: ItemKind,
    direction: ScrollDirection,
) -> PageRelation {
    if identity_slots(&previous.slots, item_kind).next().is_none()
        || identity_slots(&current.slots, item_kind).next().is_none()
    {
        return PageRelation::Uncertain;
    }

    let Some(median_dy) = common_identity_vertical_shift(&previous.slots, current, item_kind)
    else {
        return PageRelation::DifferentViewport;
    };

    if median_dy.abs() <= PAGE_SHIFT_JITTER_PX {
        return PageRelation::SameViewport;
    }
    let directed_shift = match direction {
        ScrollDirection::Down => -median_dy,
        ScrollDirection::Up => median_dy,
    };
    if directed_shift <= PAGE_SHIFT_JITTER_PX {
        return PageRelation::Uncertain;
    }

    let expected_full_shift =
        PAGE_TURN_SHIFT_REFERENCE_PX * previous.image.height() as f32 / ROI_REFERENCE_H as f32;
    let shift_ratio = directed_shift / expected_full_shift;
    PageRelation::Shifted(PageShift {
        directed_shift,
        shift_ratio,
    })
}

pub(super) fn common_identity_vertical_shift(
    previous_slots: &[Slot],
    current: &RoiObservation,
    item_kind: ItemKind,
) -> Option<f32> {
    let mut used_current = vec![false; current.slots.len()];
    let mut shifts = Vec::new();

    for previous_slot in identity_slots(previous_slots, item_kind) {
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
                Some((index, dx.abs(), current_y - previous_y))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1));

        if let Some((index, _, dy)) = best {
            used_current[index] = true;
            shifts.push(dy);
        }
    }

    interpolated_median(&mut shifts)
}

fn identity_slots<'a>(
    slots: &'a [Slot],
    item_kind: ItemKind,
) -> impl Iterator<Item = &'a Slot> + 'a {
    slots
        .iter()
        .filter(move |slot| is_identity_slot(slot, item_kind))
}

fn is_identity_slot(slot: &Slot, item_kind: ItemKind) -> bool {
    slot.kind.is_selectable_item_for(item_kind) && slot.classification.is_some()
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
