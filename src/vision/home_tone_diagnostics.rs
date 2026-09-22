use anyhow::{Result, ensure};
use image::{GenericImageView, RgbaImage};
use tracing::debug;

use crate::image_rect::ImageRect;

use super::{RoiObservation, SlotKind, SlotLayout};

const GRAY_MIN: usize = 4;
const GRAY_MAX: usize = 160;
const NEUTRAL_CHROMA_MAX: f32 = 0.05;
const MIN_CONSENSUS_SUPPORT: f32 = 0.30;

#[derive(Debug, Clone, Copy)]
struct HomeToneMeasurement {
    pub gray: f32,
    pub rgb: [f32; 3],
    pub consensus_support: f32,
    pub slot_peaks: [u8; 4],
    pub slot_supports: [f32; 4],
}

struct GrayHistogram {
    bins: [u32; 256],
    area: u32,
    samples: Vec<[u8; 3]>,
}

pub(crate) fn log_home_tone(observation: &RoiObservation) {
    match measure_home_tone(observation) {
        Ok(tone) => debug!(
            home_gray = tone.gray,
            home_rgb = ?tone.rgb,
            consensus_support = tone.consensus_support,
            slot_peaks = ?tone.slot_peaks,
            slot_supports = ?tone.slot_supports,
            "normalized Home UI tone measured"
        ),
        Err(error) => debug!(
            error = %format!("{error:#}"),
            "normalized Home UI tone measurement unavailable"
        ),
    }
}

fn measure_home_tone(observation: &RoiObservation) -> Result<HomeToneMeasurement> {
    ensure!(
        observation.layout == SlotLayout::Home,
        "tone measurement requires the loadout home"
    );
    let mut slots = observation
        .slots
        .iter()
        .filter(|slot| matches!(slot.kind, SlotKind::Stratagem | SlotKind::StratagemEmpty))
        .collect::<Vec<_>>();
    slots.sort_by_key(|slot| slot.col);
    ensure!(
        slots.len() == 4,
        "tone measurement requires four home stratagem slots, got {}",
        slots.len()
    );

    let histograms = slots
        .into_iter()
        .map(|slot| gray_histogram(&observation.image, slot.core_rect()))
        .collect::<Result<Vec<_>>>()?;
    estimate_histograms(histograms)
}

fn gray_histogram(image: &RgbaImage, region: ImageRect) -> Result<GrayHistogram> {
    ensure!(
        region.w > 0
            && region.h > 0
            && region.x + region.w <= image.width()
            && region.y + region.h <= image.height(),
        "tone diagnostic region is outside the captured image"
    );
    let mut histogram = GrayHistogram {
        bins: [0; 256],
        area: region.w * region.h,
        samples: Vec::new(),
    };
    for pixel in image.view(region.x, region.y, region.w, region.h).pixels() {
        let [r, g, b, _] = pixel.2.0;
        let minimum = r.min(g).min(b);
        let maximum = r.max(g).max(b);
        let sum = u16::from(r) + u16::from(g) + u16::from(b);
        if sum == 0 || f32::from(maximum - minimum) / f32::from(sum) > NEUTRAL_CHROMA_MAX {
            continue;
        }
        let gray = (sum + 1) / 3;
        let gray = usize::from(gray);
        if (GRAY_MIN..=GRAY_MAX).contains(&gray) {
            histogram.bins[gray] += 1;
            histogram.samples.push([r, g, b]);
        }
    }
    Ok(histogram)
}

fn estimate_histograms(histograms: Vec<GrayHistogram>) -> Result<HomeToneMeasurement> {
    let histograms: [GrayHistogram; 4] = histograms.try_into().map_err(|histograms: Vec<_>| {
        anyhow::anyhow!(
            "tone measurement requires four samples, got {}",
            histograms.len()
        )
    })?;

    let mut peak = GRAY_MIN;
    let mut best_strength = -1.0f32;
    let mut best_exact = -1.0f32;
    for gray in GRAY_MIN..=GRAY_MAX {
        let strength = third_highest(std::array::from_fn(|index| {
            let histogram = &histograms[index];
            (histogram.bins[gray - 1] + 2 * histogram.bins[gray] + histogram.bins[gray + 1]) as f32
                / (4 * histogram.area) as f32
        }));
        let exact = third_highest(std::array::from_fn(|index| {
            histograms[index].bins[gray] as f32 / histograms[index].area as f32
        }));
        if strength > best_strength || (strength == best_strength && exact > best_exact) {
            peak = gray;
            best_strength = strength;
            best_exact = exact;
        }
    }

    let slot_supports = std::array::from_fn(|index| {
        let histogram = &histograms[index];
        histogram.bins[peak - 1..=peak + 1].iter().sum::<u32>() as f32 / histogram.area as f32
    });
    let consensus_support = third_highest(slot_supports);
    ensure!(
        consensus_support >= MIN_CONSENSUS_SUPPORT,
        "home gray has insufficient cross-slot support: {:.1}%",
        consensus_support * 100.0
    );

    let mut combined = [0u32; 256];
    let mut inliers = 0;
    for (histogram, support) in histograms.iter().zip(slot_supports) {
        if support >= MIN_CONSENSUS_SUPPORT {
            inliers += 1;
            for (combined, &count) in combined[peak - 1..=peak + 1]
                .iter_mut()
                .zip(&histogram.bins[peak - 1..=peak + 1])
            {
                *combined += count;
            }
        }
    }
    ensure!(inliers >= 3, "home gray is not shared by three slots");
    let total = combined.iter().sum::<u32>();
    let gray = histogram_median(&combined, total);

    let mut channels = [[0u32; 256]; 3];
    let mut color_samples = 0;
    for (histogram, support) in histograms.iter().zip(slot_supports) {
        if support < MIN_CONSENSUS_SUPPORT {
            continue;
        }
        for &[r, g, b] in &histogram.samples {
            let sample_gray = usize::from((u16::from(r) + u16::from(g) + u16::from(b) + 1) / 3);
            if !(peak - 1..=peak + 1).contains(&sample_gray) {
                continue;
            }
            channels[0][usize::from(r)] += 1;
            channels[1][usize::from(g)] += 1;
            channels[2][usize::from(b)] += 1;
            color_samples += 1;
        }
    }
    ensure!(color_samples > 0, "home gray has no color samples");
    let rgb = std::array::from_fn(|channel| histogram_median(&channels[channel], color_samples));
    let slot_peaks = std::array::from_fn(|index| {
        (GRAY_MIN..=GRAY_MAX)
            .max_by_key(|&gray| histograms[index].bins[gray])
            .unwrap_or(GRAY_MIN) as u8
    });
    Ok(HomeToneMeasurement {
        gray,
        rgb,
        consensus_support,
        slot_peaks,
        slot_supports,
    })
}

fn third_highest(mut values: [f32; 4]) -> f32 {
    values.sort_unstable_by(f32::total_cmp);
    values[1]
}

fn histogram_median(histogram: &[u32; 256], total: u32) -> f32 {
    if total.is_multiple_of(2) {
        (f32::from(histogram_rank(histogram, total / 2 - 1))
            + f32::from(histogram_rank(histogram, total / 2)))
            * 0.5
    } else {
        histogram_rank(histogram, total / 2) as f32
    }
}

fn histogram_rank(histogram: &[u32; 256], rank: u32) -> u8 {
    let mut cumulative = 0;
    for (value, &count) in histogram.iter().enumerate() {
        cumulative += count;
        if cumulative > rank {
            return value as u8;
        }
    }
    unreachable!("histogram rank must be below its total count")
}
