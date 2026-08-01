use crate::item::ItemKind;
use crate::vision::RoiObservation;

const LIST_SAFE_BOTTOM_CENTER_Y_REF: f32 = 627.0;
const LIST_SAFE_Y_TOLERANCE_REF: f32 = 2.0;

pub(super) struct DirectClickTarget {
    pub(super) item_id: String,
    pub(super) match_score: f32,
    pub(super) match_margin: f32,
    pub(super) gate_quality: f32,
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) center_y_roi: f32,
}

pub(super) struct ClickPlan {
    pub(super) immediate: Vec<DirectClickTarget>,
    pub(super) terminal_bottom: Option<DirectClickTarget>,
}

pub(super) fn build(
    result: &RoiObservation,
    remaining: &[String],
    item_kind: ItemKind,
    list_end_confirmed: bool,
) -> ClickPlan {
    let roi_scale_y = result.scale_y();
    let safe_bottom_y = LIST_SAFE_BOTTOM_CENTER_Y_REF * roi_scale_y;
    let safe_y_tolerance = LIST_SAFE_Y_TOLERANCE_REF * roi_scale_y;

    let mut immediate = Vec::new();
    let mut bottom = Vec::new();

    for item_id in remaining {
        let Some(target) = best_visible_target(result, item_id, item_kind) else {
            continue;
        };

        if list_end_confirmed || target.center_y_roi <= safe_bottom_y + safe_y_tolerance {
            immediate.push(target);
        } else {
            bottom.push(target);
        }
    }

    immediate.sort_by(compare_center_then_x);

    ClickPlan {
        immediate,
        terminal_bottom: bottom.into_iter().min_by(compare_center_then_x),
    }
}

fn best_visible_target(
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
                center_y_roi: slot.y as f32 + slot.h as f32 * 0.5,
            })
        })
        .max_by(|left, right| {
            left.gate_quality
                .total_cmp(&right.gate_quality)
                .then_with(|| left.match_margin.total_cmp(&right.match_margin))
                .then_with(|| left.match_score.total_cmp(&right.match_score))
        })
}

fn compare_center_y(left: &DirectClickTarget, right: &DirectClickTarget) -> std::cmp::Ordering {
    left.center_y_roi.total_cmp(&right.center_y_roi)
}

fn compare_center_then_x(
    left: &DirectClickTarget,
    right: &DirectClickTarget,
) -> std::cmp::Ordering {
    compare_center_y(left, right).then_with(|| left.x.cmp(&right.x))
}
