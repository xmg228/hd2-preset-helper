use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use image::RgbaImage;
use tracing::{debug, trace};

use crate::capture::CaptureSource;
use crate::icon_color::luma601_u8;
use crate::page_sync::capture_latest_roi_frame;
use crate::vision::{Calibration, Slot};

const SEGMENT_BINS: usize = 10;
const VERTICAL_CENTER_SKIP_BINS: usize = 2;
const BORDER_SEGMENTS: usize = 4 * SEGMENT_BINS - 2 * VERTICAL_CENTER_SKIP_BINS;

const HOVER_TIMEOUT: Duration = Duration::from_millis(700);
const HOVER_STABLE_DURATION: Duration = Duration::from_millis(30);
const HOVER_STABLE_SCORE_DELTA: f32 = 3.0;
const NORMAL_HISTORY_LIMIT: usize = 128;
const CHROMA_PENALTY: f32 = 0.75;

// The absolute baseline is used only before clicking, to ensure that the
// cursor is resting on the intended slot.
const HOVER_SEGMENT_DELTA: f32 = 8.0;
const MIN_SEGMENT_SUPPORT: f32 = 0.30;

const SELECTED_MIN_SCORE_DROP: f32 = 18.0;
const SELECTED_SCORE_DROP_RATIO: f32 = 0.12;

pub(super) struct HoverSample {
    target_score: f32,
}

impl HoverSample {
    pub(super) fn is_dimmer_than(&self, unselected: &Self) -> bool {
        let score_drop = unselected.target_score - self.target_score;
        let minimum_score_drop =
            (unselected.target_score * SELECTED_SCORE_DROP_RATIO).max(SELECTED_MIN_SCORE_DROP);
        score_drop >= minimum_score_drop
    }

    pub(super) fn target_score(&self) -> f32 {
        self.target_score
    }
}

struct HoverEvidence {
    target_score: f32,
    baseline: f32,
    support: f32,
}

impl HoverEvidence {
    fn confirmed(&self) -> bool {
        self.support >= MIN_SEGMENT_SUPPORT
    }

    fn sample(&self) -> HoverSample {
        HoverSample {
            target_score: self.target_score,
        }
    }
}

#[derive(Default)]
pub(super) struct HoverVerifier {
    normal_history: VecDeque<f32>,
}

impl HoverVerifier {
    pub(super) fn sample_current_frame(
        &self,
        image: &RgbaImage,
        target: &Slot,
    ) -> Result<HoverSample> {
        let segments = border_center_line_scores(image, target)?;
        Ok(HoverSample {
            target_score: median(&segments),
        })
    }

    pub(super) fn wait_at_current_position(
        &mut self,
        capture: &mut CaptureSource,
        calibration: &Calibration,
        slots: &[Slot],
        target: &Slot,
    ) -> Result<HoverSample> {
        let started = Instant::now();
        let mut stable_since = None;
        let mut previous_score = None;

        loop {
            let frame = capture_latest_roi_frame(capture, calibration)?;
            let sample = self.evaluate(&frame.image, slots, target)?;

            trace!(
                target_row = target.row,
                target_col = target.col,
                target_score = sample.target_score,
                baseline = sample.baseline,
                hover_support = sample.support,
                elapsed = ?started.elapsed(),
                hover_confirmed = sample.confirmed(),
                "target hover evidence"
            );

            if sample.confirmed() {
                let score_is_stable = previous_score.is_some_and(|previous: f32| {
                    (previous - sample.target_score).abs() <= HOVER_STABLE_SCORE_DELTA
                });
                if score_is_stable {
                    if stable_since
                        .is_some_and(|since: Instant| since.elapsed() >= HOVER_STABLE_DURATION)
                    {
                        debug!(
                            target_row = target.row,
                            target_col = target.col,
                            target_score = sample.target_score,
                            baseline = sample.baseline,
                            hover_support = sample.support,
                            elapsed = ?started.elapsed(),
                            "target hover state confirmed"
                        );
                        return Ok(sample.sample());
                    }
                } else {
                    stable_since = Some(Instant::now());
                }
                previous_score = Some(sample.target_score);
            } else {
                stable_since = None;
                previous_score = None;
            }

            if started.elapsed() >= HOVER_TIMEOUT {
                bail!(
                    "hover was not confirmed for target slot row {} col {} after {:?} (target={:.1}, baseline={:.1}, support={:.0}%)",
                    target.row,
                    target.col,
                    started.elapsed(),
                    sample.target_score,
                    sample.baseline,
                    sample.support * 100.0,
                );
            }
        }
    }

    fn evaluate(
        &mut self,
        image: &RgbaImage,
        slots: &[Slot],
        target: &Slot,
    ) -> Result<HoverEvidence> {
        let mut scored = Vec::with_capacity(slots.len());
        for slot in slots {
            let segments = border_center_line_scores(image, slot)?;
            let score = median(&segments);
            scored.push((slot, segments, score));
        }

        let target_index = scored
            .iter()
            .position(|(slot, _, _)| same_slot(slot, target))
            .ok_or_else(|| anyhow::anyhow!("hover target slot is absent from the page geometry"))?;
        let highest_index = scored
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.2.total_cmp(&right.2))
            .map(|(index, _)| index);

        let mut current_normal = Vec::new();
        for (index, (_, _, score)) in scored.iter().enumerate() {
            if Some(index) != highest_index {
                current_normal.push(*score);
            }
        }

        let mut baseline_samples = Vec::with_capacity(
            current_normal
                .len()
                .saturating_add(self.normal_history.len()),
        );
        baseline_samples.extend(current_normal.iter().copied());
        baseline_samples.extend(self.normal_history.iter().copied());
        if baseline_samples.is_empty() {
            bail!("not enough non-hover border samples to establish a hover baseline");
        }

        let baseline = median(&baseline_samples);
        let (_, target_segments, target_score) = &scored[target_index];
        let support = segment_support(target_segments, baseline + HOVER_SEGMENT_DELTA);

        for value in current_normal {
            self.normal_history.push_back(value);
        }
        while self.normal_history.len() > NORMAL_HISTORY_LIMIT {
            self.normal_history.pop_front();
        }

        Ok(HoverEvidence {
            target_score: *target_score,
            baseline,
            support,
        })
    }
}

fn segment_support(segments: &[f32], threshold: f32) -> f32 {
    segments.iter().filter(|score| **score >= threshold).count() as f32 / BORDER_SEGMENTS as f32
}

fn border_center_line_scores(image: &RgbaImage, slot: &Slot) -> Result<Vec<f32>> {
    if slot.w == 0 || slot.h == 0 {
        bail!("cannot sample a zero-size slot border");
    }
    let right = slot.x.saturating_add(slot.w);
    let bottom = slot.y.saturating_add(slot.h);
    if right >= image.width() || bottom >= image.height() {
        bail!(
            "slot border ({},{})-({},{}) lies outside ROI image {}x{}",
            slot.x,
            slot.y,
            right,
            bottom,
            image.width(),
            image.height()
        );
    }

    let mut values = Vec::with_capacity(BORDER_SEGMENTS);
    for bin in 0..SEGMENT_BINS {
        let (start, end) = segment_bounds(slot.w, bin);
        values.push(horizontal_segment_score(
            image,
            slot.x + start,
            slot.x + end,
            slot.y,
        ));
        values.push(horizontal_segment_score(
            image,
            slot.x + start,
            slot.x + end,
            bottom,
        ));
    }

    let skip_start = (SEGMENT_BINS - VERTICAL_CENTER_SKIP_BINS) / 2;
    let skip_end = skip_start + VERTICAL_CENTER_SKIP_BINS;
    for bin in 0..SEGMENT_BINS {
        if (skip_start..skip_end).contains(&bin) {
            continue;
        }
        let (start, end) = segment_bounds(slot.h, bin);
        values.push(vertical_segment_score(
            image,
            slot.x,
            slot.y + start,
            slot.y + end,
        ));
        values.push(vertical_segment_score(
            image,
            right,
            slot.y + start,
            slot.y + end,
        ));
    }
    debug_assert_eq!(values.len(), BORDER_SEGMENTS);
    Ok(values)
}

fn horizontal_segment_score(image: &RgbaImage, start: u32, end: u32, y: u32) -> f32 {
    let values = (start..end)
        .map(|x| white_response(image.get_pixel(x, y).0))
        .collect::<Vec<_>>();
    median(&values)
}

fn vertical_segment_score(image: &RgbaImage, x: u32, start: u32, end: u32) -> f32 {
    let values = (start..end)
        .map(|y| white_response(image.get_pixel(x, y).0))
        .collect::<Vec<_>>();
    median(&values)
}

fn white_response([r, g, b, _]: [u8; 4]) -> f32 {
    let luma = luma601_u8(r, g, b) as f32;
    let max_channel = r.max(g).max(b) as f32;
    let min_channel = r.min(g).min(b) as f32;
    (luma - CHROMA_PENALTY * (max_channel - min_channel)).max(0.0)
}

fn segment_bounds(length: u32, bin: usize) -> (u32, u32) {
    let start = (bin as u32 * length) / SEGMENT_BINS as u32;
    let end = (((bin + 1) as u32 * length) / SEGMENT_BINS as u32).max(start + 1);
    (start, end)
}

fn same_slot(left: &Slot, right: &Slot) -> bool {
    left.row == right.row && left.col == right.col && left.x == right.x && left.y == right.y
}

fn median(values: &[f32]) -> f32 {
    debug_assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_unstable_by(f32::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        0.5 * (sorted[mid - 1] + sorted[mid])
    } else {
        sorted[mid]
    }
}
