use std::{fmt::Write as _, fs, io::Cursor};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, GrayImage, ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::super::page_navigation::{PageSnapshot, SlotSample};
use super::{AlignmentCandidate, AlignmentConstraints, ListMap, SlotId};

const CELL_PADDING: u32 = 2;
const CELL_BORDER: Rgba<u8> = Rgba([64, 64, 64, 255]);
const SELECTED_BORDER: Rgba<u8> = Rgba([255, 220, 32, 255]);
const BACKGROUND: Rgba<u8> = Rgba([8, 8, 8, 255]);

pub(super) fn save_alignment_case(
    map: &ListMap,
    page: &PageSnapshot,
    constraints: AlignmentConstraints,
    candidates: &[AlignmentCandidate],
    reason: &str,
) {
    let result = (|| -> Result<()> {
        let directory = crate::vision::create_list_map_case_directory(map.item_kind, reason)?;
        page.roi.image.save(directory.join("page.png"))?;
        if let Some(image) = render(map) {
            image.save(directory.join("map.png"))?;
        }
        let mut top_pairs = Vec::new();
        let comparisons = candidates
            .iter()
            .take(2)
            .enumerate()
            .map(|(rank, candidate)| {
                (
                    if rank == 0 { "best" } else { "runner-up" },
                    rank + 1,
                    candidate,
                )
            });
        for (name, rank, candidate) in comparisons {
            let pairs = alignment_pairs(map, page, constraints, candidate);
            fs::write(
                directory.join(format!("{name}.svg")),
                render_pairs(map, page, candidate, &pairs)?,
            )?;
            top_pairs.push(json!({ "rank": rank, "name": name, "pairs": pairs }));
        }
        let report = json!({
            "reason": reason,
            "direction": format!("{:?}", constraints.direction),
            "excluded_map_id": constraints.excluded.map(|id| id.0),
            "current_offset_y": map.current.offset_y,
            "candidates": candidates.iter().map(candidate_record).collect::<Vec<_>>(),
            "map_slots": map.slots.iter().enumerate().map(|(id, slot)| json!({
                "id": id, "col": slot.col, "content_y": slot.content_y,
                "sample_row": slot.sample.row, "sample_page_y": slot.sample.page_y,
                "width": slot.sample.width, "height": slot.sample.height,
                "center_x": slot.sample.center_x, "center_y": slot.sample.center_y,
                "selected": map.selected.contains(&SlotId(id)),
                "edge_clearance": slot.edge_clearance,
            })).collect::<Vec<_>>(),
            "current_slots": page.slot_samples.iter().map(|slot| json!({
                "row": slot.row, "col": slot.col, "page_y": slot.page_y,
                "width": slot.width, "height": slot.height,
                "center_x": slot.center_x, "center_y": slot.center_y,
            })).collect::<Vec<_>>(),
            "top_pairs": top_pairs,
        });
        fs::write(
            directory.join("candidates.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        debug!(path = %directory.display(), reason, "page alignment case saved");
        Ok(())
    })();
    if let Err(error) = result {
        warn!(reason, error = %format!("{error:#}"), "failed to save page alignment case");
    }
}

fn candidate_record(candidate: &AlignmentCandidate) -> Value {
    json!({
        "hypothesis_y": candidate.hypothesis_y, "offset_y": candidate.offset_y,
        "mean_score": candidate.mean_score, "support": candidate.support,
    })
}

fn alignment_pairs(
    map: &ListMap,
    page: &PageSnapshot,
    constraints: AlignmentConstraints,
    candidate: &AlignmentCandidate,
) -> Vec<Value> {
    page.slot_samples
        .iter()
        .enumerate()
        .filter_map(|(index, current)| {
            let id =
                map.nearest_mapped_slot(current, candidate.hypothesis_y, constraints.excluded)?;
            let mapped = &map.slots[id];
            Some(json!({
                "current_index": index, "row": current.row, "col": current.col,
                "page_y": current.page_y, "map_id": id, "map_content_y": mapped.content_y,
                "distance": (mapped.content_y - current.page_y - candidate.hypothesis_y).abs(),
                "cosine": super::best_shifted_response_cosine(&mapped.sample, current),
            }))
        })
        .collect()
}

fn render_pairs(
    map: &ListMap,
    page: &PageSnapshot,
    candidate: &AlignmentCandidate,
    pairs: &[Value],
) -> Result<String> {
    let row_height = page
        .slot_samples
        .iter()
        .map(|slot| slot.height)
        .chain(map.slots.iter().map(|slot| slot.sample.height))
        .max()
        .unwrap_or(64)
        .max(64)
        + 28;
    let height = 64 + row_height * pairs.len() as u32;
    let cell_width = page
        .slot_samples
        .iter()
        .map(|slot| slot.width)
        .chain(map.slots.iter().map(|slot| slot.sample.width))
        .max()
        .unwrap_or(64)
        + 16;
    let mapped_x = 12 + cell_width;
    let label_x = mapped_x + cell_width;
    let width = (label_x + 500).max(800);
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><rect width=\"100%\" height=\"100%\" fill=\"#111\"/><g fill=\"#eee\" font-family=\"Segoe UI,Arial,sans-serif\" font-size=\"14\"><text x=\"12\" y=\"22\">hypothesis={:.3} offset={:.3} mean={:.6} support={}</text><text x=\"12\" y=\"46\">current</text><text x=\"{mapped_x}\" y=\"46\">mapped</text>",
        candidate.hypothesis_y, candidate.offset_y, candidate.mean_score, candidate.support,
    );
    for (row, pair) in pairs.iter().enumerate() {
        let current = &page.slot_samples[pair["current_index"].as_u64().unwrap() as usize];
        let id = pair["map_id"].as_u64().unwrap() as usize;
        let mapped = &map.slots[id];
        let y = 60 + row as u32 * row_height;
        for (x, sample) in [(12, current), (mapped_x, &mapped.sample)] {
            let data = response_png(sample)?;
            write!(
                svg,
                "<image x=\"{x}\" y=\"{y}\" width=\"{}\" height=\"{}\" href=\"data:image/png;base64,{data}\"/>",
                sample.width, sample.height
            )?;
        }
        let cosine = pair["cosine"]
            .as_f64()
            .map_or_else(|| "unscored".into(), |score| format!("{score:.6}"));
        write!(
            svg,
            "<text x=\"{label_x}\" y=\"{}\">r{} c{} page_y={:.3}</text><text x=\"{label_x}\" y=\"{}\">mapID={} content_y={:.3}</text><text x=\"{label_x}\" y=\"{}\">raw cosine={cosine} distance={:.3}</text>",
            y + 16,
            current.row,
            current.col,
            current.page_y,
            y + 36,
            id,
            mapped.content_y,
            y + 56,
            pair["distance"].as_f64().unwrap()
        )?;
    }
    svg.push_str("</g></svg>");
    Ok(svg)
}

fn response_png(sample: &SlotSample) -> Result<String> {
    let image = GrayImage::from_raw(sample.width, sample.height, sample.response.clone())
        .context("alignment response has an invalid size")?;
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(image).write_to(&mut bytes, ImageFormat::Png)?;
    Ok(STANDARD.encode(bytes.into_inner()))
}

impl Drop for ListMap {
    fn drop(&mut self) {
        let Some(image) = render(self) else {
            return;
        };
        match crate::vision::save_list_map_image(self.item_kind, &image) {
            Ok(path) => debug!(
                item_kind = self.item_kind.label(),
                path = %path.display(),
                mapped_slots = self.slots.len(),
                "temporary list map image saved"
            ),
            Err(error) => warn!(
                item_kind = self.item_kind.label(),
                error = %format!("{error:#}"),
                "failed to save temporary list map image"
            ),
        }
    }
}

fn render(map: &ListMap) -> Option<RgbaImage> {
    let min_col = map.slots.iter().map(|slot| slot.col).min()?;
    let max_col = map.slots.iter().map(|slot| slot.col).max()?;
    let min_top = map
        .slots
        .iter()
        .map(|slot| slot.content_y - slot.sample.center_y)
        .min_by(f32::total_cmp)?;
    let max_bottom = map
        .slots
        .iter()
        .map(|slot| slot.content_y - slot.sample.center_y + slot.sample.height as f32)
        .max_by(f32::total_cmp)?;
    let cell_width = map.slots.iter().map(|slot| slot.sample.width).max()? + 2 * CELL_PADDING;
    let columns = max_col - min_col + 1;
    let height = (max_bottom - min_top).ceil().max(1.0) as u32 + 2 * CELL_PADDING;
    let mut image = RgbaImage::from_pixel(columns * cell_width, height, BACKGROUND);

    for (index, mapped) in map.slots.iter().enumerate() {
        let sample = &mapped.sample;
        let cell_x = (mapped.col - min_col) * cell_width;
        let cell_y = (mapped.content_y - sample.center_y - min_top)
            .round()
            .max(0.0) as u32;
        let x = cell_x + (cell_width - sample.width) / 2;
        let y = cell_y + CELL_PADDING;
        for sample_y in 0..sample.height {
            for sample_x in 0..sample.width {
                let index = sample_y as usize * sample.width as usize + sample_x as usize;
                let value = sample.response[index];
                image.put_pixel(x + sample_x, y + sample_y, Rgba([value, value, value, 255]));
            }
        }
        let border = if map.selected.contains(&SlotId(index)) {
            SELECTED_BORDER
        } else {
            CELL_BORDER
        };
        draw_border(
            &mut image,
            cell_x,
            cell_y,
            cell_width,
            sample.height + 2 * CELL_PADDING,
            border,
        );
    }
    Some(image)
}

fn draw_border(image: &mut RgbaImage, x: u32, y: u32, width: u32, height: u32, color: Rgba<u8>) {
    for px in x..x + width {
        image.put_pixel(px, y, color);
        image.put_pixel(px, y + height - 1, color);
    }
    for py in y..y + height {
        image.put_pixel(x, py, color);
        image.put_pixel(x + width - 1, py, color);
    }
}
