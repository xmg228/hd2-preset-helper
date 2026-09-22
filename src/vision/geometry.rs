use std::sync::OnceLock;
use std::time::Instant;

use anyhow::{Result, bail};
use image::RgbaImage;
use rayon::prelude::*;
use tracing::debug;

use crate::item::ItemKind;
use crate::vision::color;

use super::{HOME_COLS, LIST_COLS, RoiGeometry, SLOT_SIDE_LOGICAL, Slot, SlotKind, SlotLayout};
const GRID_PITCH_LOGICAL: f64 = 85.0;
const HOME_BOOSTER_X_LOGICAL: f64 = 343.0;
const HOME_Y_LOGICAL: f64 = 478.0;
const HOME_Y_SEARCH_LOGICAL: f64 = 2.0;
const HOME_SCORE_THRESHOLD: f32 = 0.05;
const LIST_Y_MIN_LOGICAL: f64 = 90.0;
const LIST_Y_MAX_LOGICAL: f64 = 525.0;
const LIST_MAX_ROWS: usize = 10;

const FRAME_COLOR_RETAIN_RATIO: f32 = 0.50;
const EDGE_RETAIN_RATIO: f32 = 2.0 / 3.0;
const PAGE_THRESHOLD_RATIO: f32 = 1.0 / 3.0;
const COLUMN_SUPPORT_RATIO: f32 = 0.10;

const AA_RADIUS: i32 = 4;
const AA_MAX_OPPOSITE_DELTA: f64 = 0.75;
const AA_MAX_RESIDUAL: f64 = 1.0;

// Empty home slots have a nearly uniform center. Relative variation keeps the
// filled check stable across global SDR/HDR brightness changes.
const HOME_CONTENT_INSET_RATIO: f32 = 14.0 / 78.0;
const HOME_CONTENT_MEAN_FLOOR: f32 = 0.08;
const HOME_CONTENT_MIN_RELATIVE_STD: f32 = 0.15;

// Conservative concentric sampling regions; actual outer shells vary by icon.
const BOOSTER_HEX_SIDE_LEN: f64 = 0.425;
const BOOSTER_CONTENT_SCALE: f64 = 0.90;
const BOOSTER_RING_INNER_SCALE: f64 = 0.85;
const BOOSTER_CORE_SCALE: f64 = 0.60;
const HOME_BOOSTER_MIN_YELLOW_RATIO: f32 = 0.35;
const HOME_BOOSTER_MIN_RING_YELLOW_RATIO: f32 = 0.80;
const HOME_BOOSTER_MIN_CORE_DARK_RATIO: f32 = 0.10;
const HOME_BOOSTER_MAX_CORE_LUMA: u8 = 90;

#[derive(Clone, Copy, Debug, Default)]
struct EdgeSample {
    contrast: f32,
    color: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default)]
struct SlotEvidence {
    q: f32,
    cross_support: f32,
}

#[derive(Clone)]
struct RowCandidate {
    y: i32,
    score: f32,
    slots: [SlotEvidence; 4],
}

struct DetectedRow {
    y: f64,
    occupied_columns: usize,
}

#[derive(Clone, Copy)]
struct SamplingParams {
    side: f64,
    trim: i32,
    step: i32,
    reference_distance: i32,
    reference_half_width: i32,
    aa_radius: i32,
}

impl SamplingParams {
    fn new(scale: f64) -> Self {
        let sampling_scale = scale * 432.0 / 576.0;
        let edge_envelope = (2.0 * sampling_scale).ceil().max(1.0) as i32;
        Self {
            side: SLOT_SIDE_LOGICAL * scale,
            trim: round_ties_even(10.0 * sampling_scale).max(2),
            step: round_ties_even(4.0 * sampling_scale).max(1),
            reference_distance: round_ties_even(6.0 * sampling_scale).max(edge_envelope + 2),
            reference_half_width: round_ties_even(0.75 * sampling_scale).max(0),
            aa_radius: round_ties_even(AA_RADIUS as f64 * sampling_scale).max(1),
        }
    }
}

struct HorizontalProfile {
    y_min: i32,
    tangents: Vec<i32>,
    samples: Vec<EdgeSample>,
}

impl HorizontalProfile {
    fn build(image: &RgbaImage, left: f64, y_min: i32, y_max: i32, params: SamplingParams) -> Self {
        let tangents = tangent_positions(
            left + params.trim as f64,
            left + params.side - params.trim as f64,
            params.step,
        );
        let height = (y_max - y_min + 1).max(0) as usize;
        let mut samples = Vec::with_capacity(height * tangents.len());
        for y in y_min..=y_max {
            samples.extend(
                tangents
                    .iter()
                    .map(|&x| sample_horizontal_point(image, x, y, params).unwrap_or_default()),
            );
        }
        Self {
            y_min,
            tangents,
            samples,
        }
    }

    fn at(&self, boundary: f64) -> Option<&[EdgeSample]> {
        let row = usize::try_from(round_ties_even(boundary) - self.y_min).ok()?;
        let width = self.tangents.len();
        self.samples.get(row * width..(row + 1) * width)
    }
}

struct FixedEdgeProfiles {
    horizontal: [HorizontalProfile; 4],
    vertical: [Vec<EdgeSample>; 8],
    vertical_offsets: Vec<i32>,
    params: SamplingParams,
}

impl FixedEdgeProfiles {
    fn build(
        image: &RgbaImage,
        columns: [f64; 4],
        y_min: i32,
        y_max: i32,
        params: SamplingParams,
    ) -> Self {
        let started = Instant::now();
        let horizontal_vec = columns
            .par_iter()
            .map(|&left| {
                HorizontalProfile::build(
                    image,
                    left,
                    y_min,
                    round_ties_even(y_max as f64 + params.side),
                    params,
                )
            })
            .collect::<Vec<_>>();
        let horizontal: [HorizontalProfile; 4] = horizontal_vec
            .try_into()
            .unwrap_or_else(|_| unreachable!("list geometry always has four columns"));

        let boundaries: [f64; 8] = std::array::from_fn(|index| {
            let left = columns[index / 2];
            if index.is_multiple_of(2) {
                left
            } else {
                left + params.side
            }
        });
        let vertical_vec = boundaries
            .par_iter()
            .map(|&boundary| {
                (0..image.height() as i32)
                    .map(|y| sample_vertical_point(image, boundary, y, params).unwrap_or_default())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let vertical: [Vec<EdgeSample>; 8] = vertical_vec
            .try_into()
            .unwrap_or_else(|_| unreachable!("four columns always have eight vertical edges"));

        let vertical_offsets = vertical_tangent_offsets(params);
        debug!(
            horizontal_points = horizontal
                .iter()
                .map(|profile| profile.samples.len())
                .sum::<usize>(),
            vertical_points = vertical.iter().map(Vec::len).sum::<usize>(),
            elapsed = ?started.elapsed(),
            "fixed edge profiles built"
        );
        Self {
            horizontal,
            vertical,
            vertical_offsets,
            params,
        }
    }

    fn score_slot(&self, col: usize, y: i32, scratch: &mut SlotScratch) -> SlotEvidence {
        let Some(top) = self.horizontal[col].at(y as f64) else {
            return SlotEvidence::default();
        };
        let Some(bottom) = self.horizontal[col].at(y as f64 + self.params.side) else {
            return SlotEvidence::default();
        };

        scratch.left.clear();
        scratch.right.clear();
        let left_line = &self.vertical[col * 2];
        let right_line = &self.vertical[col * 2 + 1];
        for &offset in &self.vertical_offsets {
            let index = y + offset;
            if let Ok(index) = usize::try_from(index)
                && let (Some(&left), Some(&right)) = (left_line.get(index), right_line.get(index))
            {
                scratch.left.push(left);
                scratch.right.push(right);
            }
        }

        score_edge_sets(
            top,
            bottom,
            &scratch.left,
            &scratch.right,
            &mut scratch.scoring,
        )
    }
}

#[derive(Default)]
struct SlotScratch {
    top: Vec<EdgeSample>,
    bottom: Vec<EdgeSample>,
    left: Vec<EdgeSample>,
    right: Vec<EdgeSample>,
    scoring: ScoringScratch,
}

#[derive(Default)]
struct ScoringScratch {
    samples: Vec<EdgeSample>,
    weighted_values: Vec<(f32, f32)>,
    values: Vec<f32>,
}

#[derive(Clone, Copy)]
struct EdgeQuality {
    q: f32,
    cross_support: f32,
}

pub fn detect(
    screenshot: &RgbaImage,
    geometry: RoiGeometry,
    expected_layout: SlotLayout,
) -> Result<Vec<Slot>> {
    if screenshot.width() == 0 || screenshot.height() == 0 {
        bail!("cannot run geometry detector on an empty image");
    }

    let started = Instant::now();
    let slots = match expected_layout {
        SlotLayout::Home => detect_home(screenshot, geometry),
        SlotLayout::List(item_kind) => detect_list(screenshot, geometry, item_kind),
    };
    debug!(
        target: "hd2_preset_helper::perf",
        ?expected_layout,
        total = ?started.elapsed(),
        detections = slots.len(),
        "geometry detector timing"
    );
    Ok(slots)
}

fn detect_home(image: &RgbaImage, geometry: RoiGeometry) -> Vec<Slot> {
    let params = SamplingParams::new(geometry.scale);
    let columns = home_columns(geometry);
    let nominal_y = geometry.logical_origin_y + HOME_Y_LOGICAL * geometry.scale;
    let search_radius = (HOME_Y_SEARCH_LOGICAL * geometry.scale).ceil() as i32;
    let vertical_offsets = vertical_tangent_offsets(params);
    let mut scratch = SlotScratch::default();
    let mut best: Option<RowCandidate> = None;
    let center = round_ties_even(nominal_y);
    for y in center - search_radius..=center + search_radius {
        let slots = std::array::from_fn(|col| {
            score_slot_direct(
                image,
                columns[col],
                y as f64,
                params,
                &vertical_offsets,
                &mut scratch,
            )
        });
        let score = second_highest(slots.map(|slot| slot.q));
        if best.as_ref().is_none_or(|current| score > current.score) {
            best = Some(RowCandidate { y, score, slots });
        }
    }
    let Some(best) = best.filter(|candidate| candidate.score >= HOME_SCORE_THRESHOLD) else {
        return Vec::new();
    };
    debug!(
        y = best.y,
        home_score = best.score,
        slots = ?best.slots,
        "fixed home frame selected"
    );

    let supported = best.slots.map(|slot| slot.q >= HOME_SCORE_THRESHOLD);
    let top =
        refine_row_aa(image, best.y as f64, &supported, columns, params).unwrap_or(best.y as f64);
    let mut slots = columns
        .into_iter()
        .enumerate()
        .map(|(col, left)| {
            slot_from_native(
                left,
                top,
                params.side,
                0,
                col as u32,
                SlotKind::StratagemEmpty,
            )
        })
        .collect::<Vec<_>>();
    for slot in &mut slots {
        if slot_has_content(image, slot) {
            slot.kind = SlotKind::Stratagem;
        }
    }

    let mut booster = slot_from_native(
        geometry.logical_origin_x + HOME_BOOSTER_X_LOGICAL * geometry.scale,
        top,
        params.side,
        0,
        HOME_COLS.len() as u32,
        SlotKind::HomeBoosterEmpty,
    );
    booster.kind = home_booster_kind(image, &booster);
    slots.push(booster);
    slots
}

fn detect_list(image: &RgbaImage, geometry: RoiGeometry, item_kind: ItemKind) -> Vec<Slot> {
    let params = SamplingParams::new(geometry.scale);
    let columns = list_columns(geometry);
    let y_min = (geometry.logical_origin_y + LIST_Y_MIN_LOGICAL * geometry.scale).ceil() as i32;
    let y_max = (geometry.logical_origin_y + LIST_Y_MAX_LOGICAL * geometry.scale).floor() as i32;
    if y_min > y_max {
        return Vec::new();
    }

    let profiles = FixedEdgeProfiles::build(image, columns, y_min, y_max, params);
    let curve = (y_min..=y_max)
        .into_par_iter()
        .map_init(SlotScratch::default, |scratch, y| {
            let slots = std::array::from_fn(|col| profiles.score_slot(col, y, scratch));
            RowCandidate {
                y,
                score: slots.into_iter().map(|slot| slot.q).fold(0.0, f32::max),
                slots,
            }
        })
        .collect::<Vec<_>>();
    let peaks = local_maxima(&curve);
    let min_gap = round_ties_even(84.0 * geometry.scale).max(1);
    let raw = select_rows_dp_hard(&peaks, LIST_MAX_ROWS, min_gap);
    let page_scale = median_top_three(&raw);
    if page_scale <= f32::EPSILON {
        return Vec::new();
    }
    let threshold = page_scale * PAGE_THRESHOLD_RATIO;
    let eligible = peaks
        .into_iter()
        .filter(|row| row.score >= threshold)
        .collect::<Vec<_>>();
    let selected = select_rows_dp_hard(&eligible, LIST_MAX_ROWS, min_gap);
    debug!(
        page_scale,
        threshold,
        raw_rows = raw.len(),
        final_rows = selected.len(),
        rows = ?selected
            .iter()
            .map(|row| (row.y, row.score, row.slots))
            .collect::<Vec<_>>(),
        "fixed list rows selected"
    );

    let rows = selected
        .into_iter()
        .map(|row| {
            let supported = supported_columns(&row.slots);
            let occupied_columns = supported
                .iter()
                .rposition(|&supported| supported)
                .map_or(1, |col| col + 1);
            let y = refine_row_aa(image, row.y as f64, &supported, columns, params)
                .unwrap_or(row.y as f64);
            DetectedRow {
                y,
                occupied_columns,
            }
        })
        .collect::<Vec<_>>();
    list_rows_to_slots(&rows, geometry, params.side, columns, item_kind)
}

fn supported_columns(slots: &[SlotEvidence; 4]) -> [bool; 4] {
    let strongest = slots
        .iter()
        .map(|slot| slot.cross_support)
        .fold(0.0, f32::max);
    let threshold = strongest * COLUMN_SUPPORT_RATIO;
    slots.map(|slot| slot.cross_support >= threshold)
}

fn list_rows_to_slots(
    rows: &[DetectedRow],
    geometry: RoiGeometry,
    side: f64,
    columns: [f64; 4],
    item_kind: ItemKind,
) -> Vec<Slot> {
    let no_booster_y_max =
        geometry.logical_origin_y + (LIST_Y_MIN_LOGICAL + GRID_PITCH_LOGICAL) * geometry.scale;
    let mut slots = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        for (col, &left) in columns.iter().enumerate().take(row.occupied_columns) {
            let kind = match item_kind {
                ItemKind::Booster if row.y < no_booster_y_max && col == 0 => {
                    SlotKind::NoBoosterOption
                }
                ItemKind::Booster => SlotKind::Booster,
                ItemKind::Stratagem => SlotKind::Stratagem,
            };
            slots.push(slot_from_native(
                left,
                row.y,
                side,
                row_index as u32,
                col as u32,
                kind,
            ));
        }
    }
    slots
}

fn score_slot_direct(
    image: &RgbaImage,
    left: f64,
    top: f64,
    params: SamplingParams,
    vertical_offsets: &[i32],
    scratch: &mut SlotScratch,
) -> SlotEvidence {
    let tangents = tangent_positions(
        left + params.trim as f64,
        left + params.side - params.trim as f64,
        params.step,
    );
    scratch.top.clear();
    scratch.bottom.clear();
    scratch.left.clear();
    scratch.right.clear();
    scratch.top.extend(
        tangents
            .iter()
            .filter_map(|&x| sample_horizontal_point(image, x, round_ties_even(top), params)),
    );
    scratch.bottom.extend(tangents.iter().filter_map(|&x| {
        sample_horizontal_point(image, x, round_ties_even(top + params.side), params)
    }));
    for &offset in vertical_offsets {
        let y = round_ties_even(top + offset as f64);
        if let Some(sample) = sample_vertical_point(image, left, y, params) {
            scratch.left.push(sample);
        }
        if let Some(sample) = sample_vertical_point(image, left + params.side, y, params) {
            scratch.right.push(sample);
        }
    }
    score_edge_sets(
        &scratch.top,
        &scratch.bottom,
        &scratch.left,
        &scratch.right,
        &mut scratch.scoring,
    )
}

fn score_edge_sets(
    top: &[EdgeSample],
    bottom: &[EdgeSample],
    left: &[EdgeSample],
    right: &[EdgeSample],
    scratch: &mut ScoringScratch,
) -> SlotEvidence {
    if top.is_empty() || bottom.is_empty() || left.is_empty() || right.is_empty() {
        return SlotEvidence::default();
    }
    let horizontal_color = robust_frame_color(top, bottom, scratch);
    let vertical_color = robust_frame_color(left, right, scratch);
    let top = edge_quality(top, vertical_color, &mut scratch.values);
    let bottom = edge_quality(bottom, vertical_color, &mut scratch.values);
    let left = edge_quality(left, horizontal_color, &mut scratch.values);
    let right = edge_quality(right, horizontal_color, &mut scratch.values);
    SlotEvidence {
        q: (top.q * bottom.q * left.q * right.q).sqrt().sqrt(),
        cross_support: 0.25
            * (top.cross_support + bottom.cross_support + left.cross_support + right.cross_support),
    }
}

fn robust_frame_color(
    first: &[EdgeSample],
    second: &[EdgeSample],
    scratch: &mut ScoringScratch,
) -> [f32; 3] {
    scratch.samples.clear();
    scratch.samples.extend_from_slice(first);
    scratch.samples.extend_from_slice(second);
    scratch
        .samples
        .sort_unstable_by(|left, right| right.contrast.total_cmp(&left.contrast));
    let retained = ((scratch.samples.len() as f32 * FRAME_COLOR_RETAIN_RATIO).ceil() as usize)
        .clamp(1, scratch.samples.len());
    scratch.samples.truncate(retained);

    std::array::from_fn(|channel| {
        scratch.weighted_values.clear();
        scratch.weighted_values.extend(
            scratch
                .samples
                .iter()
                .map(|sample| (sample.color[channel], sample.contrast)),
        );
        weighted_median(&mut scratch.weighted_values)
    })
}

fn weighted_median(values: &mut [(f32, f32)]) -> f32 {
    values.sort_unstable_by(|left, right| left.0.total_cmp(&right.0));
    let total = values.iter().map(|(_, weight)| *weight).sum::<f32>();
    if total <= f32::EPSILON {
        return median_pairs(values);
    }
    let target = total * 0.5;
    let mut cumulative = 0.0;
    for &(value, weight) in values.iter() {
        cumulative += weight;
        if cumulative >= target {
            return value;
        }
    }
    values.last().map_or(0.0, |&(value, _)| value)
}

fn median_pairs(values: &[(f32, f32)]) -> f32 {
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[mid - 1].0 + values[mid].0)
    } else {
        values[mid].0
    }
}

fn edge_quality(
    samples: &[EdgeSample],
    frame_color: [f32; 3],
    values: &mut Vec<f32>,
) -> EdgeQuality {
    values.clear();
    values.extend(samples.iter().map(|sample| {
        let mismatch = color_distance(sample.color, frame_color);
        sample.contrast * sample.contrast / (sample.contrast + mismatch + 1.0e-9)
    }));
    let cross_support = values.iter().sum::<f32>() / values.len() as f32;
    values.sort_unstable_by(f32::total_cmp);
    let retained =
        ((values.len() as f32 * EDGE_RETAIN_RATIO).ceil() as usize).clamp(1, values.len());
    let robust = values[values.len() - retained..].iter().sum::<f32>() / retained as f32;
    let q25 = quantile_sorted(values, 0.25);
    EdgeQuality {
        q: (robust * q25).max(0.0).sqrt(),
        cross_support,
    }
}

fn quantile_sorted(values: &[f32], quantile: f32) -> f32 {
    if values.len() == 1 {
        return values[0];
    }
    let position = quantile * (values.len() - 1) as f32;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f32;
    values[lower] + (values[upper] - values[lower]) * fraction
}

fn sample_horizontal_point(
    image: &RgbaImage,
    tangent_x: i32,
    normal_y: i32,
    params: SamplingParams,
) -> Option<EdgeSample> {
    let center = rgb_at(image, tangent_x, normal_y)?;
    let outer = mean_rgb_vertical(
        image,
        tangent_x,
        normal_y - params.reference_distance,
        params.reference_half_width,
    )?;
    let inner = mean_rgb_vertical(
        image,
        tangent_x,
        normal_y + params.reference_distance,
        params.reference_half_width,
    )?;
    Some(EdgeSample {
        contrast: color_distance(center, outer).min(color_distance(center, inner)),
        color: center,
    })
}

fn sample_vertical_point(
    image: &RgbaImage,
    normal_x: f64,
    tangent_y: i32,
    params: SamplingParams,
) -> Option<EdgeSample> {
    let normal_x = round_ties_even(normal_x);
    let center = rgb_at(image, normal_x, tangent_y)?;
    let outer = mean_rgb_horizontal(
        image,
        normal_x - params.reference_distance,
        tangent_y,
        params.reference_half_width,
    )?;
    let inner = mean_rgb_horizontal(
        image,
        normal_x + params.reference_distance,
        tangent_y,
        params.reference_half_width,
    )?;
    Some(EdgeSample {
        contrast: color_distance(center, outer).min(color_distance(center, inner)),
        color: center,
    })
}

fn rgb_at(image: &RgbaImage, x: i32, y: i32) -> Option<[f32; 3]> {
    if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
        return None;
    }
    let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}

fn mean_rgb_vertical(image: &RgbaImage, x: i32, y: i32, half_width: i32) -> Option<[f32; 3]> {
    mean_rgb((-half_width..=half_width).filter_map(|offset| rgb_at(image, x, y + offset)))
}

fn mean_rgb_horizontal(image: &RgbaImage, x: i32, y: i32, half_width: i32) -> Option<[f32; 3]> {
    mean_rgb((-half_width..=half_width).filter_map(|offset| rgb_at(image, x + offset, y)))
}

fn mean_rgb(samples: impl Iterator<Item = [f32; 3]>) -> Option<[f32; 3]> {
    let mut sum = [0.0; 3];
    let mut count = 0u32;
    for sample in samples {
        for channel in 0..3 {
            sum[channel] += sample[channel];
        }
        count += 1;
    }
    (count > 0).then(|| sum.map(|value| value / count as f32))
}

fn color_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    left.into_iter()
        .zip(right)
        .map(|(left, right)| (left - right).powi(2))
        .sum::<f32>()
        .sqrt()
}

fn tangent_positions(start: f64, end: f64, step: i32) -> Vec<i32> {
    let first = round_ties_even(start);
    let last = round_ties_even(end);
    (first..=last).step_by(step as usize).collect()
}

fn vertical_tangent_offsets(params: SamplingParams) -> Vec<i32> {
    tangent_positions(
        params.trim as f64,
        params.side - params.trim as f64,
        params.step,
    )
    .into_iter()
    .filter(|&offset| (offset as f64 - params.side * 0.5).abs() > params.side * 0.10)
    .collect()
}

fn local_maxima(curve: &[RowCandidate]) -> Vec<RowCandidate> {
    curve
        .iter()
        .enumerate()
        .filter(|&(index, row)| {
            let left = index
                .checked_sub(1)
                .and_then(|index| curve.get(index))
                .map_or(f32::NEG_INFINITY, |row| row.score);
            let right = curve
                .get(index + 1)
                .map_or(f32::NEG_INFINITY, |row| row.score);
            row.score > left && row.score >= right && row.score > 0.0
        })
        .map(|(_, row)| row.clone())
        .collect()
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
    let mut previous: isize = -1;
    for i in 0..n {
        while (previous + 1) < i as isize
            && candidates[i].y - candidates[(previous + 1) as usize].y >= min_gap
        {
            previous += 1;
        }
        prev[i] = previous;
    }

    let negative = -1.0e30f32;
    let stride = k_max + 1;
    let mut dp = vec![negative; (n + 1) * stride];
    let mut take = vec![false; (n + 1) * stride];
    for i in 0..=n {
        dp[i * stride] = 0.0;
    }
    for i in 1..=n {
        let row = &candidates[i - 1];
        let compatible = (prev[i - 1] + 1) as usize;
        for k in 1..=k_max {
            let index = i * stride + k;
            let skip = dp[(i - 1) * stride + k];
            let use_score = dp[compatible * stride + k - 1] + row.score;
            if use_score > skip + 1.0e-9 {
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
        if score > best_score + 1.0e-9 {
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

fn median_top_three(rows: &[RowCandidate]) -> f32 {
    let mut values = rows.iter().map(|row| row.score).collect::<Vec<_>>();
    values.sort_unstable_by(|left, right| right.total_cmp(left));
    values.truncate(3);
    median_values(&values)
}

fn median_values(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut values = values.to_vec();
    values.sort_unstable_by(f32::total_cmp);
    median_sorted_f32(&values)
}

fn second_highest(mut values: [f32; 4]) -> f32 {
    values.sort_unstable_by(|left, right| right.total_cmp(left));
    values[1]
}

fn refine_row_aa(
    image: &RgbaImage,
    nominal_y: f64,
    supported: &[bool; 4],
    columns: [f64; 4],
    params: SamplingParams,
) -> Option<f64> {
    let raw_y = estimate_row_aa(image, nominal_y, supported, columns, params)?;
    let residual = raw_y - nominal_y;
    if residual.abs() <= AA_MAX_RESIDUAL {
        debug!(
            nominal_y,
            residual,
            reanchored = false,
            "AA row phase refined"
        );
        return Some(raw_y);
    }

    let reanchored_y = nominal_y + residual.signum();
    let verified_y = estimate_row_aa(image, reanchored_y, supported, columns, params)?;
    let verified_residual = verified_y - reanchored_y;
    if verified_residual.abs() > AA_MAX_RESIDUAL
        || round_ties_even(verified_y) != round_ties_even(raw_y)
    {
        return None;
    }

    debug!(
        nominal_y,
        residual = verified_y - nominal_y,
        reanchored = true,
        "AA row phase refined"
    );
    Some(verified_y)
}

fn estimate_row_aa(
    image: &RgbaImage,
    nominal_y: f64,
    supported: &[bool; 4],
    columns: [f64; 4],
    params: SamplingParams,
) -> Option<f64> {
    let mut residuals = Vec::new();
    for (col, &supported) in supported.iter().enumerate() {
        if !supported {
            continue;
        }
        let tangents = tangent_positions(
            columns[col] + params.trim as f64,
            columns[col] + params.side - params.trim as f64,
            params.step,
        );
        let top = aa_horizontal_residual(image, nominal_y, &tangents, params);
        let bottom = aa_horizontal_residual(image, nominal_y + params.side, &tangents, params);
        if let (Some(top), Some(bottom)) = (top, bottom)
            && (top - bottom).abs() <= AA_MAX_OPPOSITE_DELTA
        {
            residuals.extend([top, bottom]);
        }
    }
    if residuals.is_empty() {
        return None;
    }
    residuals.sort_unstable_by(f64::total_cmp);
    let residual = median_sorted_f64(&residuals);
    Some(nominal_y + residual)
}

fn aa_horizontal_residual(
    image: &RgbaImage,
    boundary: f64,
    tangents: &[i32],
    params: SamplingParams,
) -> Option<f64> {
    let profile_len = (2 * params.aa_radius + 1) as usize;
    let anchor = round_ties_even(boundary);
    let mut tangent_profiles = Vec::with_capacity(tangents.len() * profile_len);
    let mut differences = Vec::with_capacity(profile_len);
    for &x in tangents {
        let outer = mean_linear_rgb_vertical(
            image,
            x,
            round_ties_even(boundary - params.reference_distance as f64),
            params.reference_half_width,
        )?;
        let inner = mean_linear_rgb_vertical(
            image,
            x,
            round_ties_even(boundary + params.reference_distance as f64),
            params.reference_half_width,
        )?;
        let background: [f32; 3] =
            std::array::from_fn(|channel| 0.5 * (outer[channel] + inner[channel]));
        differences.clear();
        let mut direction = [0.0; 3];
        let mut direction_energy = 0.0;
        for offset in -params.aa_radius..=params.aa_radius {
            let color = linear_rgb_at(image, x, anchor + offset)?;
            let difference = std::array::from_fn(|channel| color[channel] - background[channel]);
            let energy = difference.iter().map(|value| value * value).sum::<f32>();
            if energy > direction_energy {
                direction_energy = energy;
                direction = difference;
            }
            differences.push(difference);
        }
        if direction_energy <= f32::EPSILON {
            continue;
        }
        tangent_profiles.extend(differences.iter().map(|difference| {
            (difference
                .iter()
                .zip(direction)
                .map(|(left, right)| left * right)
                .sum::<f32>()
                / direction_energy)
                .clamp(0.0, 1.0)
        }));
    }
    if tangent_profiles.is_empty() {
        return None;
    }

    let mut profile = vec![0.0; profile_len];
    let mut values = Vec::with_capacity(tangent_profiles.len() / profile_len);
    for (index, output) in profile.iter_mut().enumerate() {
        values.clear();
        values.extend(
            tangent_profiles
                .chunks_exact(profile_len)
                .map(|profile| profile[index]),
        );
        values.sort_unstable_by(f32::total_cmp);
        *output = median_sorted_f32(&values);
    }
    let floor = profile.iter().copied().fold(f32::INFINITY, f32::min);
    let mut total = 0.0f64;
    let mut moment = 0.0f64;
    for (index, value) in profile.into_iter().enumerate() {
        let value = (value - floor).max(0.0) as f64;
        let offset = index as i32 - params.aa_radius;
        total += value;
        moment += offset as f64 * value;
    }
    if total <= f64::EPSILON {
        return None;
    }
    let boundary_hat = anchor as f64 + moment / total + 0.5;
    Some(boundary_hat - boundary)
}

fn mean_linear_rgb_vertical(
    image: &RgbaImage,
    x: i32,
    y: i32,
    half_width: i32,
) -> Option<[f32; 3]> {
    mean_rgb((-half_width..=half_width).filter_map(|offset| linear_rgb_at(image, x, y + offset)))
}

fn linear_rgb_at(image: &RgbaImage, x: i32, y: i32) -> Option<[f32; 3]> {
    if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
        return None;
    }
    let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
    let table = linear_rgb_table();
    Some([table[r as usize], table[g as usize], table[b as usize]])
}

fn linear_rgb_table() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|index| {
            let value = index as f32 / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        })
    })
}

fn median_sorted_f32(values: &[f32]) -> f32 {
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[mid - 1] + values[mid])
    } else {
        values[mid]
    }
}

fn median_sorted_f64(values: &[f64]) -> f64 {
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[mid - 1] + values[mid])
    } else {
        values[mid]
    }
}

fn slot_from_native(left: f64, top: f64, side: f64, row: u32, col: u32, kind: SlotKind) -> Slot {
    Slot {
        x: left.round() as u32,
        y: top.round() as u32,
        w: side.round().max(1.0) as u32,
        h: side.round().max(1.0) as u32,
        center_x: (left + side * 0.5) as f32,
        center_y: (top + side * 0.5) as f32,
        side,
        row,
        col,
        kind,
        classification: None,
    }
}

fn list_columns(geometry: RoiGeometry) -> [f64; 4] {
    LIST_COLS.map(|x| geometry.logical_origin_x + x as f64 * geometry.scale)
}

fn home_columns(geometry: RoiGeometry) -> [f64; 4] {
    HOME_COLS.map(|x| geometry.logical_origin_x + x as f64 * geometry.scale)
}

fn round_ties_even(value: f64) -> i32 {
    value.round_ties_even() as i32
}

fn slot_has_content(image: &RgbaImage, slot: &Slot) -> bool {
    let inset_x = (slot.w as f32 * HOME_CONTENT_INSET_RATIO).round() as u32;
    let inset_y = (slot.h as f32 * HOME_CONTENT_INSET_RATIO).round() as u32;
    let x0 = slot.x.saturating_add(inset_x);
    let y0 = slot.y.saturating_add(inset_y);
    let x1 = slot
        .x
        .saturating_add(slot.w)
        .saturating_sub(inset_x)
        .min(image.width());
    let y1 = slot
        .y
        .saturating_add(slot.h)
        .saturating_sub(inset_y)
        .min(image.height());
    if x0 >= x1 || y0 >= y1 {
        return false;
    }

    let mut sum = 0.0f32;
    let mut sum_sq = 0.0f32;
    let mut count = 0u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let [r, g, b, _] = image.get_pixel(x, y).0;
            let value = color::luma601_u8(r, g, b) as f32 / 255.0;
            sum += value;
            sum_sq += value * value;
            count += 1;
        }
    }
    let mean = sum / count as f32;
    let std = (sum_sq / count as f32 - mean * mean).max(0.0).sqrt();
    std / mean.max(HOME_CONTENT_MEAN_FLOOR) >= HOME_CONTENT_MIN_RELATIVE_STD
}

fn home_booster_kind(rgba: &RgbaImage, slot: &Slot) -> SlotKind {
    const SQRT_3: f64 = 1.732_050_807_568_877_2;

    let (center_x, center_y) = slot.center_f32();
    let row_span = |y: u32, scale: f64| {
        let dy = (y as f64 + 0.5 - center_y as f64).abs();
        let side = slot.side * BOOSTER_HEX_SIDE_LEN * scale;
        if dy > 0.5 * SQRT_3 * side {
            return 0..0;
        }
        let half_width = side - dy / SQRT_3;
        let width = rgba.width() as f64;
        let left = (center_x as f64 - half_width).floor().clamp(0.0, width) as u32;
        let right = (center_x as f64 + half_width).ceil().clamp(0.0, width) as u32;
        left..right
    };
    let mut yellow_pixels = 0u32;
    let mut content_pixels = 0u32;
    let mut ring_yellow_pixels = 0u32;
    let mut ring_pixels = 0u32;
    let mut core_dark_pixels = 0u32;
    let mut core_pixels = 0u32;

    for y in slot.y..slot.y.saturating_add(slot.h).min(rgba.height()) {
        let content = row_span(y, BOOSTER_CONTENT_SCALE);
        let ring_inner = row_span(y, BOOSTER_RING_INNER_SCALE);
        let core = row_span(y, BOOSTER_CORE_SCALE);
        content_pixels += content.end - content.start;
        for x in content {
            let [r, g, b, _] = rgba.get_pixel(x, y).0;
            let yellow = color::is_booster_yellow(r, g, b);
            if yellow {
                yellow_pixels += 1;
            }
            if !ring_inner.contains(&x) {
                ring_pixels += 1;
                ring_yellow_pixels += u32::from(yellow);
            }
            if core.contains(&x) {
                core_pixels += 1;
                core_dark_pixels +=
                    u32::from(color::luma601_u8(r, g, b) <= HOME_BOOSTER_MAX_CORE_LUMA);
            }
        }
    }

    if content_pixels > 0
        && ring_pixels > 0
        && core_pixels > 0
        && yellow_pixels as f32 >= content_pixels as f32 * HOME_BOOSTER_MIN_YELLOW_RATIO
        && ring_yellow_pixels as f32 >= ring_pixels as f32 * HOME_BOOSTER_MIN_RING_YELLOW_RATIO
        && core_dark_pixels as f32 >= core_pixels as f32 * HOME_BOOSTER_MIN_CORE_DARK_RATIO
    {
        SlotKind::HomeBooster
    } else {
        SlotKind::HomeBoosterEmpty
    }
}
