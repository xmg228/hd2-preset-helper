use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use image::RgbaImage;
use tracing::{debug, trace};

use crate::capture::CaptureSource;
use crate::icon_color::luma601_u8;
use crate::page_sync::capture_latest_roi_frame;
use crate::vision::{Calibration, Slot};

const HOVER_TIMEOUT: Duration = Duration::from_millis(700);
const HOVER_STABLE_DURATION: Duration = Duration::from_millis(15);
const HOVER_STABLE_SCORE_DELTA: f32 = 3.0;
const CHROMA_PENALTY: f32 = 0.75;

const HOVER_MIN_SCORE_GAP: f32 = 24.0;
const HOVER_MAD_MULTIPLIER: f32 = 3.0;
const MAD_NORMAL_SCALE: f32 = 1.4826;

const SELECTED_MIN_SCORE_DROP: f32 = 14.0;
const SELECTED_SCORE_DROP_RATIO: f32 = 0.10;

pub(super) struct HoverSample {
    target_score: f32,
}

impl HoverSample {
    pub(super) fn is_dimmer_than(&self, unselected: &Self) -> bool {
        let score_drop = unselected.target_score - self.target_score;
        score_drop >= unselected.required_score_drop()
    }

    pub(super) fn target_score(&self) -> f32 {
        self.target_score
    }

    pub(super) fn required_score_drop(&self) -> f32 {
        (self.target_score * SELECTED_SCORE_DROP_RATIO).max(SELECTED_MIN_SCORE_DROP)
    }
}

struct HoverEvidence {
    target_score: f32,
    baseline: f32,
    required_gap: f32,
}

impl HoverEvidence {
    fn confirmed(&self) -> bool {
        self.target_score >= self.baseline + self.required_gap
    }

    fn sample(&self) -> HoverSample {
        HoverSample {
            target_score: self.target_score,
        }
    }
}

pub(super) struct HoverVerifier;

impl HoverVerifier {
    pub(super) fn sample_current_frame(image: &RgbaImage, target: &Slot) -> Result<HoverSample> {
        Ok(HoverSample {
            target_score: border_center_line_score(image, target)?,
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
                score_gap = sample.target_score - sample.baseline,
                required_gap = sample.required_gap,
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
                            score_gap = sample.target_score - sample.baseline,
                            required_gap = sample.required_gap,
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
                    "hover was not confirmed for target slot row {} col {} after {:?} (target={:.1}, baseline={:.1}, gap={:.1}/{:.1})",
                    target.row,
                    target.col,
                    started.elapsed(),
                    sample.target_score,
                    sample.baseline,
                    sample.target_score - sample.baseline,
                    sample.required_gap,
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
            scored.push((slot, border_center_line_score(image, slot)?));
        }

        let target_index = scored
            .iter()
            .position(|(slot, _)| same_slot(slot, target))
            .ok_or_else(|| anyhow::anyhow!("hover target slot is absent from the page geometry"))?;
        let mut baseline_samples = scored
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != target_index)
            .map(|(_, (_, score))| *score)
            .collect::<Vec<_>>();
        if baseline_samples.len() > 1 {
            let highest_index = baseline_samples
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| left.total_cmp(right))
                .map(|(index, _)| index)
                .expect("checked non-empty baseline samples");
            baseline_samples.swap_remove(highest_index);
        }
        if baseline_samples.is_empty() {
            bail!("not enough non-hover border samples to establish a hover baseline");
        }

        let baseline = median(&baseline_samples);
        let deviations = baseline_samples
            .iter()
            .map(|score| (score - baseline).abs())
            .collect::<Vec<_>>();
        let dispersion = MAD_NORMAL_SCALE * median(&deviations);
        let required_gap = HOVER_MIN_SCORE_GAP.max(HOVER_MAD_MULTIPLIER * dispersion);
        let target_score = scored[target_index].1;

        Ok(HoverEvidence {
            target_score,
            baseline,
            required_gap,
        })
    }
}

fn border_center_line_score(image: &RgbaImage, slot: &Slot) -> Result<f32> {
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

    let mut values = Vec::with_capacity((2 * slot.w + 2 * slot.h) as usize);
    for x in slot.x..right {
        values.push(white_response(image.get_pixel(x, slot.y).0));
        values.push(white_response(image.get_pixel(x, bottom).0));
    }
    for y in slot.y..bottom {
        values.push(white_response(image.get_pixel(slot.x, y).0));
        values.push(white_response(image.get_pixel(right, y).0));
    }
    Ok(upper_tertile(&values))
}

fn white_response([r, g, b, _]: [u8; 4]) -> f32 {
    let luma = luma601_u8(r, g, b) as f32;
    let max_channel = r.max(g).max(b) as f32;
    let min_channel = r.min(g).min(b) as f32;
    (luma - CHROMA_PENALTY * (max_channel - min_channel)).max(0.0)
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

fn upper_tertile(values: &[f32]) -> f32 {
    debug_assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_unstable_by(f32::total_cmp);
    sorted[(sorted.len() - 1) * 2 / 3]
}
