use std::time::Instant;

use anyhow::{Result, bail};
use fast_image_resize as fir;
use fir::{ResizeAlg, ResizeOptions, Resizer};
use image::{GrayImage, RgbaImage};
use rayon::prelude::*;
use tracing::debug;

use crate::layout::{
    HOME_BOOSTER_X, HOME_COLS, LIST_COLS, ROI_REFERENCE_H, ROI_REFERENCE_H_F32, ROI_REFERENCE_W,
    ROI_REFERENCE_W_F32, SLOT_SIZE_I32,
};
use crate::icon_color;
use crate::item::ItemKind;
use crate::slot::{SlotKind, SlotLayout};
use crate::vision::{Rect, Slot};

// Window sizes for the three-band luma edge response.
const H_WIN: i32 = 48;
const H_LINE_H: i32 = 3;
const H_SIDE_H: i32 = 7;
const V_WIN: i32 = 48;
const V_LINE_W: i32 = 3;
const V_SIDE_W: i32 = 7;
const Z_EPS: f32 = 0.12;

const H_RESPONSE_THR: f32 = 0.18;
const V_RESPONSE_THR: f32 = 0.16;
const H_RESPONSE_HI: f32 = 0.75;
const V_RESPONSE_HI: f32 = 0.70;

// Candidate x/y are fixed slot anchors. Each edge is measured with a small
// thickness band rather than shifting the edge to maximize its response.
// Horizontal edges prefer the theoretical center pixel and use the strongest
// adjacent pixel only as thickness / sampling support. Vertical edges retain the
// more tolerant top-k reduction because column x is not scanned independently.
const EDGE_BAND: i32 = 1;
const H_EDGE_CENTER_WEIGHT: f32 = 2.0;
const EDGE_BAND_TOPK: usize = 2;
const SEGMENT_BINS: usize = 10;
const V_SEGMENT_SKIP_CENTER_BINS: usize = 2;

const H_SEGMENT_SAMPLE_STEP: i32 = 1;
const SEGMENT_MIN_BIN_SCORE: f32 = 0.22;

const MIN_SLOT_SCORE: f32 = 0.24;
const MIN_HORIZONTAL_EDGE: f32 = 0.22;
const MIN_SIDE: f32 = 0.16;

// Robust frame-luma consistency. A slot contributes 36 border segments:
// ten each on top/bottom and eight each on the intentionally gapped sides.
const BORDER_SEGMENTS: usize =
    4 * SEGMENT_BINS - 2 * V_SEGMENT_SKIP_CENTER_BINS;
const BORDER_UNIFORMITY_TRIM: usize = 8;
const BORDER_UNIFORMITY_RETAINED: usize = BORDER_SEGMENTS - BORDER_UNIFORMITY_TRIM;
const BORDER_UNIFORMITY_LUMA_FLOOR: f32 = 0.08;
const BORDER_UNIFORMITY_GOOD: f32 = 0.10;
const BORDER_UNIFORMITY_BAD: f32 = 0.30;
const BORDER_UNIFORMITY_MAX_PENALTY: f32 = 0.60;

// Slot score weights: emphasize top/bottom pairing so a single strong line
// cannot dominate a slot score.
const TB_MIN_WEIGHT: f32 = 0.55;
const TB_MEAN_WEIGHT: f32 = 0.25;
const SIDE_MEAN_WEIGHT: f32 = 0.15;
const SIDE_BEST_WEIGHT: f32 = 0.05;

const ROW_THRESHOLD: f32 = 0.16;
// Empty home slots have a nearly uniform center. Relative variation keeps the
// filled check stable across global SDR/HDR brightness changes.
const HOME_CONTENT_INSET: i32 = 19;
const HOME_CONTENT_MEAN_FLOOR: f32 = 0.08;
const HOME_CONTENT_MIN_RELATIVE_STD: f32 = 0.15;

// Use the central 90% of the calibrated home-booster hex to avoid its border.
const BOOSTER_HEX_CENTER_X: f32 = 0.5000;
const BOOSTER_HEX_CENTER_Y: f32 = 0.5048;
const BOOSTER_CONTENT_SIDE_LEN: f32 = 0.4250 * 0.90;
const HOME_BOOSTER_MIN_YELLOW_RATIO: f32 = 0.40;
const ROW_MIN_DIST: i32 = 113;

// Detection runs in the canonical ROI coordinate system. Input ROIs with the
// same aspect ratio are normalized before scoring, then boxes are mapped back to
// source coordinates.

#[derive(Clone, Copy)]
struct Profile {
    cols: [i32; 4],
    y_min: i32,
    y_max: i32,
    max_rows: usize,
    min_slots: usize,
}

const LIST_PROFILE: Profile = Profile {
    cols: LIST_COLS,
    y_min: 120,
    y_max: 720,
    max_rows: 10,
    min_slots: 1,
};
const HOME_PROFILE: Profile = Profile {
    cols: HOME_COLS,
    y_min: 620,
    y_max: 652,
    max_rows: 1,
    min_slots: HOME_COLS.len(),
};

#[derive(Clone)]
struct SlotCandidate {
    x: i32,
    col: u32,
}

#[derive(Clone)]
struct RowCandidate {
    y: i32,
    score: f32,
    slots: Vec<SlotCandidate>,
}

struct ProfileLookup {
    h_lines: Vec<LineScores>,
    h_x_to_index: Vec<Option<usize>>,
    v_x_to_index: Vec<Option<usize>>,
    v_lines: Vec<LineScores>,
}

struct LineScores {
    pos: i32,
    start: i32,
    scores: Vec<f32>,
}

struct IntegralImage {
    width: usize,
    height: usize,
    sum: Vec<f32>,
    sum_sq: Vec<f32>,
}

pub fn detect(screenshot: &RgbaImage, expected_layout: SlotLayout) -> Result<Vec<Slot>> {
    if screenshot.width() == 0 || screenshot.height() == 0 {
        bail!("cannot run geometry detector on an empty image");
    }

    let started = Instant::now();
    let luma = luma_canonical(screenshot)?;
    let luma_time = started.elapsed();
    let integral = IntegralImage::from_luma(&luma);
    let integral_time = started.elapsed() - luma_time;

    let detections = match expected_layout {
        SlotLayout::Home => detect_home(screenshot, &integral),
        SlotLayout::List(item_kind) => detect_list(
            screenshot.width(),
            screenshot.height(),
            &integral,
            item_kind,
        ),
    };

    debug!(
        target: "hd2_preset_helper::perf",
        ?expected_layout,
        luma = ?luma_time,
        integral = ?integral_time,
        total = ?started.elapsed(),
        detections = detections.len(),
        "geometry detector timing"
    );
    Ok(detections)
}

fn detect_home(rgba: &RgbaImage, integral: &IntegralImage) -> Vec<Slot> {
    let lookup = ProfileLookup::new(integral, &HOME_PROFILE);
    let rows = scan_profile(&lookup, integral, HOME_PROFILE);
    home_rows_to_slots(rgba, integral, &rows)
}

fn detect_list(
    image_w: u32,
    image_h: u32,
    integral: &IntegralImage,
    item_kind: ItemKind,
) -> Vec<Slot> {
    let lookup = ProfileLookup::new(integral, &LIST_PROFILE);
    let rows = scan_profile(&lookup, integral, LIST_PROFILE);
    match item_kind {
        ItemKind::Booster => booster_list_rows_to_slots(image_w, image_h, &LIST_PROFILE, &rows),
        ItemKind::Stratagem => rows_to_slots_by(image_w, image_h, &rows, |_, _| {
            SlotKind::Stratagem
        }),
    }
}

fn home_rows_to_slots(
    rgba: &RgbaImage,
    integral: &IntegralImage,
    rows: &[RowCandidate],
) -> Vec<Slot> {
    let Some(row) = rows.first() else {
        return Vec::new();
    };

    let mut slots: Vec<Slot> = row
        .slots
        .iter()
        .map(|candidate| {
            let kind = if slot_has_content(integral, candidate.x, row.y) {
                SlotKind::Stratagem
            } else {
                SlotKind::StratagemEmpty
            };
            slot_from_rect(
                slot_rect(rgba.width(), rgba.height(), candidate.x, row.y),
                0,
                candidate.col,
                kind,
            )
        })
        .collect();

    if slots.len() != HOME_COLS.len() {
        return Vec::new();
    }

    let rect = slot_rect(rgba.width(), rgba.height(), HOME_BOOSTER_X, row.y);
    slots.push(slot_from_rect(
        rect,
        0,
        HOME_COLS.len() as u32,
        home_booster_kind(rgba, rect),
    ));

    slots
}

fn booster_list_rows_to_slots(
    image_w: u32,
    image_h: u32,
    profile: &Profile,
    rows: &[RowCandidate],
) -> Vec<Slot> {
    let first_row_y_max = profile.y_min + ROW_MIN_DIST;
    rows_to_slots_by(image_w, image_h, rows, |row, slot| {
        if row.y < first_row_y_max && slot.col == 0 {
            SlotKind::NoBoosterOption
        } else {
            SlotKind::Booster
        }
    })
}

fn rows_to_slots_by(
    image_w: u32,
    image_h: u32,
    rows: &[RowCandidate],
    mut kind_for_slot: impl FnMut(&RowCandidate, &SlotCandidate) -> SlotKind,
) -> Vec<Slot> {
    let mut slots = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        for candidate in &row.slots {
            slots.push(slot_from_rect(
                slot_rect(image_w, image_h, candidate.x, row.y),
                row_index as u32,
                candidate.col,
                kind_for_slot(row, candidate),
            ));
        }
    }
    slots
}

fn slot_from_rect(rect: Rect, row: u32, col: u32, kind: SlotKind) -> Slot {
    Slot {
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        row,
        col,
        kind,
        classification: None,
    }
}

fn slot_rect(image_w: u32, image_h: u32, x: i32, y: i32) -> Rect {
    let sx = image_w as f32 / ROI_REFERENCE_W_F32;
    let sy = image_h as f32 / ROI_REFERENCE_H_F32;
    Rect {
        x: (x as f32 * sx).round() as u32,
        y: (y as f32 * sy).round() as u32,
        w: (SLOT_SIZE_I32 as f32 * sx).round().max(1.0) as u32,
        h: (SLOT_SIZE_I32 as f32 * sy).round().max(1.0) as u32,
    }
}

fn scan_profile(
    lookup: &ProfileLookup,
    integral: &IntegralImage,
    profile: Profile,
) -> Vec<RowCandidate> {
    let mut candidates = Vec::new();

    for y in profile.y_min..=profile.y_max {
        if let Some(row) = score_row_at(lookup, integral, &profile, y) {
            candidates.push(row);
        }
    }

    // Each integer y has already been scored, so DP itself is the final joint
    // optimization over row scores and the hard spacing constraint.
    select_rows_dp_hard(&candidates, profile.max_rows, ROW_MIN_DIST)
}

fn select_rows_dp_hard(
    candidates: &[RowCandidate],
    max_rows: usize,
    min_gap: i32,
) -> Vec<RowCandidate> {
    if candidates.is_empty() || max_rows == 0 {
        return Vec::new();
    }

    let n = candidates.len();
    let k_max = max_rows.min(n);

    let mut prev = vec![-1isize; n];
    let mut j: isize = -1;
    for i in 0..n {
        while (j + 1) < i as isize
            && candidates[i].y - candidates[(j + 1) as usize].y >= min_gap
        {
            j += 1;
        }
        prev[i] = j;
    }

    let neg = -1.0e30f32;
    let stride = k_max + 1;
    let mut dp = vec![neg; (n + 1) * stride];
    let mut take = vec![false; (n + 1) * stride];
    for i in 0..=n {
        dp[i * stride] = 0.0;
    }

    for i in 1..=n {
        let row = &candidates[i - 1];
        let p = (prev[i - 1] + 1) as usize;
        for k in 1..=k_max {
            let index = i * stride + k;
            let skip = dp[(i - 1) * stride + k];
            let use_score = dp[p * stride + k - 1] + row.score;
            if use_score > skip + 1e-9 {
                dp[index] = use_score;
                take[index] = true;
            } else {
                dp[index] = skip;
            }
        }
    }

    let mut best_k = 0usize;
    let mut best_score = dp[n * stride];
    for k in 1..=k_max {
        let score = dp[n * stride + k];
        if score > best_score + 1e-9 {
            best_score = score;
            best_k = k;
        }
    }

    let mut selected = Vec::new();
    let mut i = n;
    let mut k = best_k;
    while i > 0 && k > 0 {
        if take[i * stride + k] {
            selected.push(candidates[i - 1].clone());
            i = (prev[i - 1] + 1) as usize;
            k -= 1;
        } else {
            i -= 1;
        }
    }
    selected.reverse();
    selected
}

fn score_row_at(
    lookup: &ProfileLookup,
    integral: &IntegralImage,
    profile: &Profile,
    y: i32,
) -> Option<RowCandidate> {
    let mut slots: [Option<SlotCandidate>; 4] = std::array::from_fn(|_| None);
    let mut slot_count = 0usize;
    let mut score = 0.0f32;
    for (col, x) in profile.cols.into_iter().enumerate() {
        if let Some((slot, slot_score)) = score_slot_at(lookup, integral, x, y, col as u32) {
            score = score.max(slot_score);
            slots[col] = Some(slot);
            slot_count += 1;
        }
    }

    // Rank a row by its strongest complete slot so a valid single-slot final row
    // is not diluted by empty columns. Only allocate the retained slot vector for
    // rows that pass both gates.
    if slot_count < profile.min_slots || score < ROW_THRESHOLD {
        return None;
    }

    Some(RowCandidate {
        y,
        score: score.clamp(0.0, 1.0),
        slots: slots.into_iter().flatten().collect(),
    })
}

fn score_slot_at(
    lookup: &ProfileLookup,
    integral: &IntegralImage,
    x: i32,
    y: i32,
    col: u32,
) -> Option<(SlotCandidate, f32)> {
    let sw = SLOT_SIZE_I32;
    let sh = SLOT_SIZE_I32;
    let top = lookup.horizontal_edge_score(x, y);
    let bottom = lookup.horizontal_edge_score(x, y + sh);
    let left = lookup.vertical_edge_score(x, y);
    let right = lookup.vertical_edge_score(x + sw, y);

    let tb_min = top.min(bottom);
    let tb_mean = 0.5 * (top + bottom);
    let side_mean = 0.5 * (left + right);
    let side_best = left.max(right);
    let base_score = TB_MIN_WEIGHT * tb_min
        + TB_MEAN_WEIGHT * tb_mean
        + SIDE_MEAN_WEIGHT * side_mean
        + SIDE_BEST_WEIGHT * side_best;

    // Keep the established geometry gates first. Uniformity is evaluated only
    // for candidates that could otherwise become real slots.
    if top < MIN_HORIZONTAL_EDGE
        || bottom < MIN_HORIZONTAL_EDGE
        || side_best < MIN_SIDE
        || base_score < MIN_SLOT_SCORE
    {
        return None;
    }

    let score = base_score * border_uniformity_factor(integral, x, y);
    if score < MIN_SLOT_SCORE {
        return None;
    }

    Some((SlotCandidate { x, col }, score))
}

fn border_uniformity_factor(integral: &IntegralImage, x: i32, y: i32) -> f32 {
    let width = SLOT_SIZE_I32;
    let height = SLOT_SIZE_I32;
    let mut values = [0.0; BORDER_SEGMENTS];
    let mut index = 0;
    let top = y - H_LINE_H / 2;
    let bottom = y + height - H_LINE_H / 2;
    let left = x - V_LINE_W / 2;
    let right = x + width - V_LINE_W / 2;

    for bin in 0..SEGMENT_BINS {
        let (start, end) = segment_bounds(width, bin);
        values[index] = integral.mean_rect(x + start, top, x + end, top + H_LINE_H);
        values[index + 1] =
            integral.mean_rect(x + start, bottom, x + end, bottom + H_LINE_H);
        index += 2;
    }

    let skip_start = (SEGMENT_BINS - V_SEGMENT_SKIP_CENTER_BINS) / 2;
    let skip_end = skip_start + V_SEGMENT_SKIP_CENTER_BINS;
    for bin in 0..SEGMENT_BINS {
        if (skip_start..skip_end).contains(&bin) {
            continue;
        }
        let (start, end) = segment_bounds(height, bin);
        values[index] = integral.mean_rect(left, y + start, left + V_LINE_W, y + end);
        values[index + 1] =
            integral.mean_rect(right, y + start, right + V_LINE_W, y + end);
        index += 2;
    }
    debug_assert_eq!(index, BORDER_SEGMENTS);

    // Once ordered by luma, the segments nearest the median form one contiguous
    // range. Trim the farther end eight times instead of sorting by deviation.
    values.sort_unstable_by(f32::total_cmp);
    let median = median_sorted(&values);
    let mut first = 0;
    let mut last = values.len();
    for _ in 0..BORDER_UNIFORMITY_TRIM {
        if median - values[first] > values[last - 1] - median {
            first += 1;
        } else {
            last -= 1;
        }
    }

    let retained = &values[first..last];
    debug_assert_eq!(retained.len(), BORDER_UNIFORMITY_RETAINED);
    let center = median_sorted(retained).max(BORDER_UNIFORMITY_LUMA_FLOOR);
    let (sum, sum_sq) = retained
        .iter()
        .fold((0.0, 0.0), |(sum, sum_sq), &value| {
            (sum + value, sum_sq + value * value)
        });
    let count = retained.len() as f32;
    let mean = sum / count;
    let relative_std = (sum_sq / count - mean * mean).max(0.0).sqrt() / center;
    let t = ((relative_std - BORDER_UNIFORMITY_GOOD)
        / (BORDER_UNIFORMITY_BAD - BORDER_UNIFORMITY_GOOD))
        .clamp(0.0, 1.0);
    let penalty = t * t * (3.0 - 2.0 * t);
    1.0 - BORDER_UNIFORMITY_MAX_PENALTY * penalty
}

fn median_sorted(values: &[f32]) -> f32 {
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        0.5 * (values[mid - 1] + values[mid])
    } else {
        values[mid]
    }
}

fn segment_bounds(length: i32, bin: usize) -> (i32, i32) {
    let scale = length as f32 / SEGMENT_BINS as f32;
    (
        (bin as f32 * scale).round() as i32,
        ((bin + 1) as f32 * scale).round() as i32,
    )
}

impl ProfileLookup {
    fn new(integral: &IntegralImage, profile: &Profile) -> Self {
        let started = Instant::now();
        let width = integral.width;
        let h_y_min = profile.y_min - EDGE_BAND;
        let h_y_max = profile.y_max + SLOT_SIZE_I32 + EDGE_BAND;
        let h_lines: Vec<LineScores> = profile
            .cols
            .par_iter()
            .map(|x| {
                build_horizontal_line_scores(integral, *x, h_y_min, h_y_max)
            })
            .collect();

        let mut h_x_to_index = vec![None; width];
        for (index, line) in h_lines.iter().enumerate() {
            h_x_to_index[line.pos as usize] = Some(index);
        }

        let mut v_positions = Vec::new();
        for x in profile.cols {
            for edge_x in [x, x + SLOT_SIZE_I32] {
                v_positions.extend(edge_x - EDGE_BAND..=edge_x + EDGE_BAND);
            }
        }
        v_positions.sort_unstable();
        v_positions.dedup();

        let v_lines: Vec<LineScores> = v_positions
            .par_iter()
            .map(|x| build_vertical_line_scores(integral, *x, profile))
            .collect();
        let mut v_x_to_index = vec![None; width];
        for (index, line) in v_lines.iter().enumerate() {
            v_x_to_index[line.pos as usize] = Some(index);
        }

        debug!(
            h_lines = h_lines.len(),
            v_lines = v_lines.len(),
            elapsed = ?started.elapsed(),
            "geometry profile lookup built"
        );

        Self {
            h_lines,
            h_x_to_index,
            v_x_to_index,
            v_lines,
        }
    }

    fn horizontal_edge_score(&self, x: i32, y: i32) -> f32 {
        let Some(Some(index)) = self.h_x_to_index.get(x as usize) else {
            return 0.0;
        };
        let line = &self.h_lines[*index];
        let center = line.score_at(y);
        let neighbor = line.score_at(y - EDGE_BAND).max(line.score_at(y + EDGE_BAND));
        center_biased_band_score(center, neighbor, H_EDGE_CENTER_WEIGHT)
    }

    fn vertical_edge_score(&self, x: i32, y: i32) -> f32 {
        let mut best = f32::NEG_INFINITY;
        let mut second = f32::NEG_INFINITY;
        let mut count = 0usize;
        for xx in x - EDGE_BAND..=x + EDGE_BAND {
            let Some(Some(index)) = self.v_x_to_index.get(xx as usize) else {
                continue;
            };
            push_top2(self.v_lines[*index].score_at(y), &mut best, &mut second);
            count += 1;
        }
        topk2_mean(best, second, count).unwrap_or(0.0)
    }
}

fn horizontal_score_at(integral: &IntegralImage, x: i32, y: i32) -> f32 {
    response_to_score(
        horizontal_response(integral, x, y),
        H_RESPONSE_THR,
        H_RESPONSE_HI,
    )
}

fn vertical_score_at(integral: &IntegralImage, x: i32, y: i32) -> f32 {
    response_to_score(
        vertical_response(integral, x, y),
        V_RESPONSE_THR,
        V_RESPONSE_HI,
    )
}

impl LineScores {
    fn score_at(&self, position: i32) -> f32 {
        let index = position - self.start;
        if index < 0 {
            return 0.0;
        }
        self.scores.get(index as usize).copied().unwrap_or(0.0)
    }
}

fn build_horizontal_line_scores(
    integral: &IntegralImage,
    x: i32,
    y_min: i32,
    y_max: i32,
) -> LineScores {
    let mut scores = Vec::with_capacity((y_max - y_min + 1).max(0) as usize);
    for y in y_min..=y_max {
        scores.push(segmented_score_by_sampling_step(
            SLOT_SIZE_I32,
            H_SEGMENT_SAMPLE_STEP,
            |offset| {
                let px = x + offset;
                horizontal_score_at(integral, px, y)
            },
        ));
    }

    LineScores {
        pos: x,
        start: y_min,
        scores,
    }
}

fn build_vertical_line_scores(
    integral: &IntegralImage,
    x: i32,
    profile: &Profile,
) -> LineScores {
    let y_min = profile.y_min;
    let y_max = profile.y_max;
    let response_y_max = y_max + SLOT_SIZE_I32;
    let mut response_scores = Vec::with_capacity((response_y_max - y_min + 1).max(0) as usize);
    for y in y_min..=response_y_max {
        response_scores.push(vertical_score_at(integral, x, y));
    }

    let mut prefix = Vec::with_capacity(response_scores.len() + 1);
    prefix.push(0.0);
    for score in response_scores {
        prefix.push(prefix.last().copied().unwrap_or(0.0) + score);
    }

    let mut scores = Vec::with_capacity((y_max - y_min + 1).max(0) as usize);
    for y in y_min..=y_max {
        scores.push(segmented_score_from_prefix_masked(
            &prefix,
            y_min,
            y,
            SLOT_SIZE_I32,
            V_SEGMENT_SKIP_CENTER_BINS,
        ));
    }

    LineScores {
        pos: x,
        start: y_min,
        scores,
    }
}

fn center_biased_band_score(center: f32, neighbor: f32, center_weight: f32) -> f32 {
    (center_weight * center + neighbor) / (center_weight + 1.0)
}

fn push_top2(value: f32, best: &mut f32, second: &mut f32) {
    if value > *best {
        *second = *best;
        *best = value;
    } else if value > *second {
        *second = value;
    }
}

fn topk2_mean(best: f32, second: f32, count: usize) -> Option<f32> {
    if count == 0 {
        None
    } else if EDGE_BAND_TOPK <= 1 || count == 1 {
        Some(best)
    } else {
        Some((best + second) * 0.5)
    }
}

fn segmented_score_by_sampling_step<F>(length: i32, step: i32, mut sample: F) -> f32
where
    F: FnMut(i32) -> f32,
{
    let step = step.max(1);
    segmented_score(length, |start, end| {
        let mut sum = 0.0;
        let mut count = 0usize;
        for offset in (start..end).step_by(step as usize) {
            sum += sample(offset);
            count += 1;
        }
        if count == 0 {
            0.0
        } else {
            sum / count as f32
        }
    })
}

fn segmented_score_from_prefix_masked(
    prefix: &[f32],
    base: i32,
    start: i32,
    length: i32,
    skip_center_bins: usize,
) -> f32 {
    segmented_score_masked(length, skip_center_bins, |bin_start, bin_end| {
        let y0 = (start + bin_start - base).max(0) as usize;
        let y1 = (start + bin_end - base).clamp(0, prefix.len().saturating_sub(1) as i32) as usize;
        if y1 <= y0 {
            return 0.0;
        }
        (prefix[y1] - prefix[y0]) / (y1 - y0) as f32
    })
}

fn segmented_score<F>(length: i32, bin_mean: F) -> f32
where
    F: FnMut(i32, i32) -> f32,
{
    segmented_score_masked(length, 0, bin_mean)
}

fn segmented_score_masked<F>(length: i32, skip_center_bins: usize, mut bin_mean: F) -> f32
where
    F: FnMut(i32, i32) -> f32,
{
    if length <= 0 {
        return 0.0;
    }

    let mut mean_score = 0.0;
    let mut active = 0usize;
    let mut used = 0usize;
    let skip = skip_center_bins.min(SEGMENT_BINS.saturating_sub(1));
    let skip_start = (SEGMENT_BINS - skip) / 2;
    let skip_end = skip_start + skip;
    for bin in 0..SEGMENT_BINS {
        if skip > 0 && (skip_start..skip_end).contains(&bin) {
            continue;
        }
        let (a, b) = segment_bounds(length, bin);
        if b <= a {
            continue;
        }

        let mean = bin_mean(a, b);
        mean_score += mean;
        used += 1;
        if mean >= SEGMENT_MIN_BIN_SCORE {
            active += 1;
        }
    }

    let used = used.max(1);
    let mean_score = mean_score / used as f32;
    let active_ratio = active as f32 / used as f32;
    (0.65 * mean_score + 0.35 * active_ratio).clamp(0.0, 1.0)
}

fn resize_l8_lanczos(src: GrayImage, dst_w: u32, dst_h: u32) -> Result<GrayImage> {
    if src.width() == dst_w && src.height() == dst_h {
        return Ok(src);
    }

    let mut destination = GrayImage::new(dst_w, dst_h);
    let options =
        ResizeOptions::new().resize_alg(ResizeAlg::Convolution(fir::FilterType::Lanczos3));
    Resizer::new()
        .resize(&src, &mut destination, &options)
        .map_err(|e| anyhow::anyhow!("failed to FIR-resize L8 image: {e}"))?;
    Ok(destination)
}

fn luma_canonical(rgba: &RgbaImage) -> Result<GrayImage> {
    let width = rgba.width() as usize;
    let mut raw = vec![0; width * rgba.height() as usize];
    let (pixels, remainder) = rgba.as_raw().as_chunks::<4>();
    debug_assert!(remainder.is_empty());
    for (pixel, luma) in pixels.iter().zip(&mut raw) {
        *luma = icon_color::luma601_u8(pixel[0], pixel[1], pixel[2]);
    }

    let gray = GrayImage::from_raw(rgba.width(), rgba.height(), raw)
        .ok_or_else(|| anyhow::anyhow!("failed to create grayscale ROI image"))?;
    resize_l8_lanczos(gray, ROI_REFERENCE_W, ROI_REFERENCE_H)
}

fn horizontal_response(integral: &IntegralImage, x: i32, y: i32) -> f32 {
    let side_offset = (H_LINE_H + H_SIDE_H) / 2;
    let core = integral.mean_centered(x, y, H_WIN, H_LINE_H);
    let above = integral.mean_centered(x, y - side_offset, H_WIN, H_SIDE_H);
    let below = integral.mean_centered(x, y + side_offset, H_WIN, H_SIDE_H);
    let (_, std) = integral.mean_std_centered(x, y, H_WIN, H_LINE_H + 2 * H_SIDE_H);
    ((core - above).abs() + (core - below).abs()) / (2.0 * (std + Z_EPS))
}

fn vertical_response(integral: &IntegralImage, x: i32, y: i32) -> f32 {
    let side_offset = (V_LINE_W + V_SIDE_W) / 2;
    let core = integral.mean_centered(x, y, V_LINE_W, V_WIN);
    let left = integral.mean_centered(x - side_offset, y, V_SIDE_W, V_WIN);
    let right = integral.mean_centered(x + side_offset, y, V_SIDE_W, V_WIN);
    let (_, std) = integral.mean_std_centered(x, y, V_LINE_W + 2 * V_SIDE_W, V_WIN);
    ((core - left).abs() + (core - right).abs()) / (2.0 * (std + Z_EPS))
}

fn response_to_score(response: f32, threshold: f32, high: f32) -> f32 {
    ((response - threshold) / (high - threshold).max(1e-6)).clamp(0.0, 1.0)
}

impl IntegralImage {
    fn from_luma(luma: &GrayImage) -> Self {
        let width = luma.width() as usize;
        let height = luma.height() as usize;
        let values = luma.as_raw();
        let stride = width + 1;
        let scale = 1.0 / 255.0;
        let mut sum = vec![0.0; (width + 1) * (height + 1)];
        let mut sum_sq = vec![0.0; (width + 1) * (height + 1)];

        for (y, row) in values.chunks_exact(width).enumerate() {
            let mut row_sum = 0.0;
            let mut row_sum_sq = 0.0;
            for (x, &value) in row.iter().enumerate() {
                let value = value as f32 * scale;
                row_sum += value;
                row_sum_sq += value * value;
                let idx = (y + 1) * stride + x + 1;
                sum[idx] = sum[y * stride + x + 1] + row_sum;
                sum_sq[idx] = sum_sq[y * stride + x + 1] + row_sum_sq;
            }
        }

        Self {
            width,
            height,
            sum,
            sum_sq,
        }
    }

    fn mean_centered(&self, x: i32, y: i32, width: i32, height: i32) -> f32 {
        let (sum, count) = self.rect_sum_centered(&self.sum, x, y, width, height);
        sum / count.max(1) as f32
    }

    fn mean_std_centered(&self, x: i32, y: i32, width: i32, height: i32) -> (f32, f32) {
        let (sum, count) = self.rect_sum_centered(&self.sum, x, y, width, height);
        let (sum_sq, _) = self.rect_sum_centered(&self.sum_sq, x, y, width, height);
        let count = count.max(1) as f32;
        let mean = sum / count;
        let variance = (sum_sq / count - mean * mean).max(0.0);
        (mean, variance.sqrt())
    }

    fn mean_rect(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> f32 {
        let (sum, count) = self.rect_sum(&self.sum, x0, y0, x1, y1);
        sum / count.max(1) as f32
    }

    fn rect_sum_centered(
        &self,
        table: &[f32],
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> (f32, u32) {
        let x0 = (x - width / 2).clamp(0, self.width as i32);
        let y0 = (y - height / 2).clamp(0, self.height as i32);
        self.rect_sum(table, x0, y0, x0 + width, y0 + height)
    }

    fn rect_sum(
        &self,
        table: &[f32],
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> (f32, u32) {
        let x0 = x0.clamp(0, self.width as i32) as usize;
        let y0 = y0.clamp(0, self.height as i32) as usize;
        let x1 = x1.clamp(0, self.width as i32) as usize;
        let y1 = y1.clamp(0, self.height as i32) as usize;
        if x1 <= x0 || y1 <= y0 {
            return (0.0, 0);
        }

        let stride = self.width + 1;
        let sum = table[y1 * stride + x1] - table[y0 * stride + x1]
            - table[y1 * stride + x0]
            + table[y0 * stride + x0];
        (sum, ((x1 - x0) * (y1 - y0)) as u32)
    }
}

fn slot_has_content(integral: &IntegralImage, x: i32, y: i32) -> bool {
    let content_size = SLOT_SIZE_I32 - 2 * HOME_CONTENT_INSET;
    let center_offset = SLOT_SIZE_I32 / 2;
    let (mean, std) = integral.mean_std_centered(
        x + center_offset,
        y + center_offset,
        content_size,
        content_size,
    );
    std / mean.max(HOME_CONTENT_MEAN_FLOOR) >= HOME_CONTENT_MIN_RELATIVE_STD
}

fn home_booster_kind(rgba: &RgbaImage, rect: Rect) -> SlotKind {
    let width = rect.w.min(rgba.width().saturating_sub(rect.x));
    let height = rect.h.min(rgba.height().saturating_sub(rect.y));
    let mut yellow_pixels = 0u32;
    let mut mask_pixels = 0u32;

    for local_y in 0..height {
        let span = booster_hex_row_span(local_y, width, height);
        mask_pixels += span.end - span.start;
        for local_x in span {
            let [r, g, b, _] = rgba.get_pixel(rect.x + local_x, rect.y + local_y).0;
            if icon_color::booster_yellow_likeness(r, g, b) >= 0.5 {
                yellow_pixels += 1;
            }
        }
    }

    if mask_pixels > 0
        && (yellow_pixels as f32) >= mask_pixels as f32 * HOME_BOOSTER_MIN_YELLOW_RATIO
    {
        SlotKind::HomeBooster
    } else {
        SlotKind::HomeBoosterEmpty
    }
}

fn booster_hex_row_span(local_y: u32, width: u32, height: u32) -> std::ops::Range<u32> {
    const SQRT_3: f32 = 1.732_050_8;

    let y = (local_y as f32 + 0.5) / height as f32;
    let dy = (y - BOOSTER_HEX_CENTER_Y).abs();
    if dy > 0.5 * SQRT_3 * BOOSTER_CONTENT_SIDE_LEN {
        return 0..0;
    }

    let half_width = BOOSTER_CONTENT_SIDE_LEN - dy / SQRT_3;
    let width = width as f32;
    let left = ((BOOSTER_HEX_CENTER_X - half_width) * width)
        .floor()
        .clamp(0.0, width) as u32;
    let right = ((BOOSTER_HEX_CENTER_X + half_width) * width)
        .ceil()
        .clamp(0.0, width) as u32;
    left..right
}
