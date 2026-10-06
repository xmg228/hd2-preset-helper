use crate::item::ItemKind;
use crate::vision::{RoiObservation, Slot};

#[derive(Clone)]
pub(super) struct DirectClickTarget {
    pub(super) item_id: String,
    pub(super) match_error: f32,
    pub(super) match_margin: f32,
    pub(super) gate_quality: f32,
    pub(super) slot: Slot,
    #[cfg(feature = "diagnostics")]
    pub(super) fallback: bool,
}

pub(super) fn next_visible_target(
    result: &RoiObservation,
    remaining: &[String],
    item_kind: ItemKind,
    mut is_available: impl FnMut(&Slot) -> bool,
) -> Option<DirectClickTarget> {
    remaining
        .iter()
        .filter_map(|item_id| find_visible_target(result, item_id, item_kind))
        .filter(|target| is_available(&target.slot))
        .min_by(compare_center_then_x)
}

pub(super) fn find_visible_target(
    result: &RoiObservation,
    item_id: &str,
    item_kind: ItemKind,
) -> Option<DirectClickTarget> {
    result
        .slots
        .iter()
        .filter_map(|slot| {
            if !slot.kind.is_selectable_item_for(item_kind) {
                return None;
            }
            let classification = slot.classification.as_ref()?;
            if classification.item_id != item_id {
                return None;
            }
            Some(DirectClickTarget {
                item_id: classification.item_id.clone(),
                match_error: classification.match_error,
                match_margin: classification.match_margin,
                gate_quality: classification.gate_quality,
                slot: slot.clone(),
                #[cfg(feature = "diagnostics")]
                fallback: false,
            })
        })
        .max_by(|left, right| {
            left.gate_quality
                .total_cmp(&right.gate_quality)
                .then_with(|| left.match_margin.total_cmp(&right.match_margin))
        })
}

fn compare_center_then_x(
    left: &DirectClickTarget,
    right: &DirectClickTarget,
) -> std::cmp::Ordering {
    let (left_x, left_y) = left.slot.center_f32();
    let (right_x, right_y) = right.slot.center_f32();
    left_y
        .total_cmp(&right_y)
        .then_with(|| left_x.total_cmp(&right_x))
}
