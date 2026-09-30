//! Rectangular frame evidence and fixed-column row detection, independent of item identity.
use image::RgbaImage;
use rayon::prelude::*;
use std::time::Instant;
use tracing::debug;

const FRAME_COLOR_RETAIN_RATIO: f32 = 0.50;
const EDGE_RETAIN_RATIO: f32 = 2.0 / 3.0;
const PAGE_THRESHOLD_RATIO: f32 = 1.0 / 3.0;
const COLUMN_SUPPORT_RATIO: f32 = 0.10;

#[derive(Clone, Copy, Debug, Default)]
struct EdgeSample {
    contrast: f32,
    color: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SlotEvidence {
    pub(super) q: f32,
    pub(super) cross_support: f32,
}

#[derive(Clone)]
pub(super) struct RowCandidate<const N: usize> {
    pub(super) y: i32,
    pub(super) score: f32,
    pub(super) slots: [SlotEvidence; N],
}

#[derive(Clone, Copy)]
pub(super) struct SamplingParams {
    pub(super) width: f64,
    pub(super) height: f64,
    pub(super) trim: i32,
    pub(super) step: i32,
    pub(super) reference_distance: i32,
    pub(super) reference_half_width: i32,
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
            left + params.width - params.trim as f64,
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

pub(super) struct FixedEdgeProfiles {
    horizontal: Vec<HorizontalProfile>,
    vertical: Vec<Vec<EdgeSample>>,
    vertical_offsets: Vec<i32>,
    params: SamplingParams,
}

impl FixedEdgeProfiles {
    pub(super) fn build(
        image: &RgbaImage,
        columns: &[f64],
        y_min: i32,
        y_max: i32,
        params: SamplingParams,
    ) -> Self {
        let started = Instant::now();
        let horizontal = columns
            .par_iter()
            .map(|&left| {
                HorizontalProfile::build(
                    image,
                    left,
                    y_min,
                    round_ties_even(y_max as f64 + params.height),
                    params,
                )
            })
            .collect::<Vec<_>>();
        let boundaries = columns
            .iter()
            .flat_map(|&left| [left, left + params.width])
            .collect::<Vec<_>>();
        let vertical = boundaries
            .par_iter()
            .map(|&boundary| {
                (0..image.height() as i32)
                    .map(|y| sample_vertical_point(image, boundary, y, params).unwrap_or_default())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let vertical_offsets = vertical_tangent_offsets(params);
        debug!(
            horizontal_points = horizontal
                .iter()
                .map(|profile| profile.samples.len())
                .sum::<usize>(),
            vertical_points = vertical.iter().map(Vec::len).sum::<usize>(),
            elapsed_s = started.elapsed().as_secs_f64(),
            "fixed edge profiles built"
        );
        Self {
            horizontal,
            vertical,
            vertical_offsets,
            params,
        }
    }

    pub(super) fn score_slot(&self, col: usize, y: i32, scratch: &mut SlotScratch) -> SlotEvidence {
        let Some(top) = self.horizontal[col].at(y as f64) else {
            return SlotEvidence::default();
        };
        let Some(bottom) = self.horizontal[col].at(y as f64 + self.params.height) else {
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
pub(super) struct SlotScratch {
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

pub(super) fn score_slot_direct(
    image: &RgbaImage,
    left: f64,
    top: f64,
    params: SamplingParams,
    vertical_offsets: &[i32],
    scratch: &mut SlotScratch,
) -> SlotEvidence {
    let tangents = tangent_positions(
        left + params.trim as f64,
        left + params.width - params.trim as f64,
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
        sample_horizontal_point(image, x, round_ties_even(top + params.height), params)
    }));
    for &offset in vertical_offsets {
        let y = round_ties_even(top + offset as f64);
        if let Some(sample) = sample_vertical_point(image, left, y, params) {
            scratch.left.push(sample);
        }
        if let Some(sample) = sample_vertical_point(image, left + params.width, y, params) {
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

pub(super) fn rgb_at(image: &RgbaImage, x: i32, y: i32) -> Option<[f32; 3]> {
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

pub(super) fn mean_rgb(samples: impl Iterator<Item = [f32; 3]>) -> Option<[f32; 3]> {
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

pub(super) fn tangent_positions(start: f64, end: f64, step: i32) -> Vec<i32> {
    let first = round_ties_even(start);
    let last = round_ties_even(end);
    (first..=last).step_by(step as usize).collect()
}

pub(super) fn vertical_tangent_offsets(params: SamplingParams) -> Vec<i32> {
    tangent_positions(
        params.trim as f64,
        params.height - params.trim as f64,
        params.step,
    )
    .into_iter()
    .filter(|&offset| (offset as f64 - params.height * 0.5).abs() > params.height * 0.10)
    .collect()
}

fn local_maxima<const N: usize>(curve: &[RowCandidate<N>]) -> Vec<RowCandidate<N>> {
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

fn select_rows_dp_hard<const N: usize>(
    candidates: &[RowCandidate<N>],
    max_rows: usize,
    min_gap: i32,
) -> Vec<RowCandidate<N>> {
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

pub(super) fn select_rows<const N: usize>(
    curve: &[RowCandidate<N>],
    max_rows: usize,
    min_gap: i32,
) -> Vec<RowCandidate<N>> {
    let peaks = local_maxima(curve);
    let raw = select_rows_dp_hard(&peaks, max_rows, min_gap);
    let page_scale = median_top_three(&raw);
    if page_scale <= f32::EPSILON {
        return Vec::new();
    }
    let threshold = page_scale * PAGE_THRESHOLD_RATIO;
    let eligible = peaks
        .into_iter()
        .filter(|row| row.score >= threshold)
        .collect::<Vec<_>>();
    let selected = select_rows_dp_hard(&eligible, max_rows, min_gap);
    debug!(
        page_scale,
        threshold,
        raw_rows = raw.len(),
        final_rows = selected.len(),
        rows = %format_args!("{:.3?}", selected
            .iter()
            .map(|row| (row.y, row.score, row.slots))
            .collect::<Vec<_>>()),
        "fixed list rows selected"
    );

    selected
}

pub(super) fn supported_columns<const N: usize>(slots: &[SlotEvidence; N]) -> [bool; N] {
    let strongest = slots
        .iter()
        .map(|slot| slot.cross_support)
        .fold(0.0, f32::max);
    let threshold = strongest * COLUMN_SUPPORT_RATIO;
    slots.map(|slot| slot.cross_support >= threshold)
}

fn median_top_three<const N: usize>(rows: &[RowCandidate<N>]) -> f32 {
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

fn median_sorted_f32(values: &[f32]) -> f32 {
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[mid - 1] + values[mid])
    } else {
        values[mid]
    }
}

pub(super) fn round_ties_even(value: f64) -> i32 {
    value.round_ties_even() as i32
}
