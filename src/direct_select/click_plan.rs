use crate::item::ItemKind;
use crate::vision::{RoiObservation, Slot};

pub(super) struct DirectClickTarget {
    pub(super) item_id: String,
    pub(super) match_score: f32,
    pub(super) match_margin: f32,
    pub(super) gate_quality: f32,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) slot: Slot,
}

pub(super) fn next_visible_target(
    result: &RoiObservation,
    remaining: &[String],
    item_kind: ItemKind,
) -> Option<DirectClickTarget> {
    remaining
        .iter()
        .filter_map(|item_id| find_visible_target(result, item_id, item_kind))
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
            let (x, y) = result.screen_center(slot);
            Some(DirectClickTarget {
                item_id: classification.item_id.clone(),
                match_score: classification.match_score,
                match_margin: classification.match_margin,
                gate_quality: classification.gate_quality,
                x,
                y,
                slot: slot.clone(),
            })
        })
        .max_by(|left, right| {
            left.gate_quality
                .total_cmp(&right.gate_quality)
                .then_with(|| left.match_margin.total_cmp(&right.match_margin))
                .then_with(|| left.match_score.total_cmp(&right.match_score))
        })
}

fn compare_center_then_x(
    left: &DirectClickTarget,
    right: &DirectClickTarget,
) -> std::cmp::Ordering {
    let left_center_y = left.slot.y as f32 + left.slot.h as f32 * 0.5;
    let right_center_y = right.slot.y as f32 + right.slot.h as f32 * 0.5;
    left_center_y
        .total_cmp(&right_center_y)
        .then_with(|| left.x.cmp(&right.x))
}
