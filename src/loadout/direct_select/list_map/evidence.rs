//! Render existing alignment evidence; never evaluate another match here.
use image::{Rgba, RgbaImage};
use serde_json::{Value, json};

use super::super::page_navigation::{PageSnapshot, SlotSample};
use super::{AlignmentAssessment, ListMap};
use crate::evidence::OperationEvidence;

pub(super) fn record(
    evidence: &OperationEvidence,
    map: &ListMap,
    page: &PageSnapshot,
    assessment: &AlignmentAssessment,
    context: &str,
) {
    // A bounded contact sheet preserves the exact map samples referenced by the
    // top two hypotheses, even after later commits replace those samples.
    let pairs = assessment
        .candidates
        .iter()
        .filter(|candidate| !candidate.pairs.is_empty())
        .take(2)
        .flat_map(|candidate| &candidate.pairs)
        .collect::<Vec<_>>();
    let width = pairs
        .iter()
        .map(|pair| {
            page.slot_samples[pair.current]
                .width
                .max(map.slots[pair.mapped].sample.width)
        })
        .max()
        .unwrap_or(0);
    let height = pairs
        .iter()
        .map(|pair| {
            page.slot_samples[pair.current]
                .height
                .max(map.slots[pair.mapped].sample.height)
        })
        .max()
        .unwrap_or(0);
    let current_slots = page
        .slot_samples
        .iter()
        .map(|slot| {
            json!({
                "row": slot.row, "col": slot.col, "page_y": slot.page_y,
                "width": slot.width, "height": slot.height,
                "center_x": slot.center_x, "center_y": slot.center_y,
            })
        })
        .collect::<Vec<_>>();
    let matches = page
        .match_candidates
        .iter()
        .map(|candidate| {
            json!({
                "item": candidate.item_id, "row": candidate.slot.row, "col": candidate.slot.col,
                "match_error": candidate.score, "margin": candidate.match_margin,
            })
        })
        .collect::<Vec<_>>();
    let pair_sheet = (width > 0 && height > 0).then(|| json!({
        "file": format!("evidence/alignment-{}.png", page.roi.image.id),
            "cell_width": width, "cell_height": height, "left": "current", "right": "mapped",
            "rows": pairs.iter().map(|pair| json!({"current": pair.current, "mapped": pair.mapped, "cosine": pair.cosine})).collect::<Vec<_>>()
    }));
    evidence.record(context, Some(&page.roi.image), json!({
        "alignment": assessment, "map_slots": map_slots(map), "current_slots": current_slots, "matches": matches,
        "pair_sheet": pair_sheet,
    }));
    if width == 0 || height == 0 {
        return;
    }
    let mut image =
        RgbaImage::from_pixel(2 * width, pairs.len() as u32 * height, Rgba([0, 0, 0, 255]));
    for (row, pair) in pairs.iter().enumerate() {
        draw_sample(
            &mut image,
            &page.slot_samples[pair.current],
            0,
            row as u32 * height,
        );
        draw_sample(
            &mut image,
            &map.slots[pair.mapped].sample,
            width,
            row as u32 * height,
        );
    }
    evidence.image(&format!("alignment-{}", page.roi.image.id), image);
}

pub(super) fn record_commit(
    evidence: &OperationEvidence,
    map: &ListMap,
    page: &PageSnapshot,
    context: &str,
) {
    evidence.record(
        "list-map-commit",
        Some(&page.roi.image),
        json!({
            "context": context, "placement": map.current, "map_slots": map_slots(map),
        }),
    );
}

fn map_slots(map: &ListMap) -> Vec<Value> {
    map.slots
        .iter()
        .enumerate()
        .map(|(id, slot)| {
            json!({
                "id": id, "col": slot.col, "content_y": slot.content_y,
                "width": slot.sample.width, "height": slot.sample.height,
                "center_x": slot.sample.center_x, "center_y": slot.sample.center_y,
                "selected": map.selected.contains(&super::SlotId(id)),
                "no_booster_option": map.no_booster_option == Some(super::SlotId(id)),
            })
        })
        .collect()
}

fn draw_sample(image: &mut RgbaImage, sample: &SlotSample, x: u32, y: u32) {
    for sy in 0..sample.height {
        for sx in 0..sample.width {
            let value = sample.response[(sy * sample.width + sx) as usize];
            image.put_pixel(x + sx, y + sy, Rgba([value, value, value, 255]));
        }
    }
}
