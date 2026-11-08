//! Native scrollbar measurements from confirmed scan steps; encoding happens after scanning.
use std::{collections::BTreeMap, time::Instant};

use anyhow::Result;
use image::{GenericImageView, ImageEncoder, RgbaImage, codecs::png::PngEncoder};
use serde::Serialize;
use serde_json::json;

use super::{Navigation, Session};
use crate::{
    image_rect::ImageRect,
    item::EquipmentKind,
    vision::{ScrollbarObservation, equipment::EquipmentSlot},
};

const POSITION_BINS: usize = 16;
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Serialize)]
struct Reading {
    kind: EquipmentKind,
    phase: &'static str,
    frame: u64,
    elapsed_s: f64,
    scale: f64,
    output_size: (u32, u32),
    capture_roi: ImageRect,
    strip_roi: ImageRect,
    track_top: f64,
    track_bottom: f64,
    scrollbar: Option<ScrollbarObservation>,
    input: serde_json::Value,
    location: serde_json::Value,
    focus: Option<usize>,
    slots: Vec<EquipmentSlot>,
}

struct Sample {
    reading: usize,
    image: RgbaImage,
}

pub(crate) struct ScanDiagnostics {
    started: Instant,
    readings: Vec<Reading>,
    // Keep the first and latest strip in each position bin, including missing detections.
    samples: BTreeMap<(usize, usize, bool), Sample>,
    image_bytes: usize,
    omitted_images: usize,
}

impl Default for ScanDiagnostics {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            readings: Vec::new(),
            samples: BTreeMap::new(),
            image_bytes: 0,
            omitted_images: 0,
        }
    }
}

impl ScanDiagnostics {
    pub fn is_empty(&self) -> bool {
        self.readings.is_empty()
    }

    pub(super) fn observe(
        &mut self,
        session: &Session<'_>,
        kind: EquipmentKind,
        phase: &'static str,
        navigation: Option<Navigation>,
        row: Option<(usize, bool)>,
    ) {
        let frame = &session.frame;
        if self
            .readings
            .last()
            .is_some_and(|last| last.kind == kind && last.frame == frame.id)
        {
            return;
        }
        let Some(diagnostic) = session.observer.scrollbar_diagnostic(frame) else {
            return;
        };
        let reading = self.readings.len();
        self.readings.push(Reading {
            kind,
            phase,
            frame: frame.id,
            elapsed_s: frame
                .captured_at
                .saturating_duration_since(self.started)
                .as_secs_f64(),
            scale: session.page.scale,
            output_size: frame.output_size,
            capture_roi: frame.rect,
            strip_roi: diagnostic.region,
            track_top: diagnostic.track_top,
            track_bottom: diagnostic.track_bottom,
            scrollbar: session.page.scrollbar,
            input: navigation
                .map(|nav| {
                    json!({
                        "action": nav.action.name(), "column": nav.action.column(),
                        "retried": nav.retried, "wrapped": nav.wrapped,
                    })
                })
                .unwrap_or_default(),
            location: row
                .map(|(row, boundary_before)| {
                    json!({
                        "row": row, "boundary_before": boundary_before,
                    })
                })
                .unwrap_or_default(),
            focus: session.page.focus,
            slots: session.page.slots.clone(),
        });
        let bin = session.page.scrollbar.map_or(POSITION_BINS, |bar| {
            let travel = diagnostic.track_bottom
                - diagnostic.track_top
                - f64::from(bar.lower_y - bar.upper_y);
            let position = (f64::from(bar.upper_y) - diagnostic.track_top) / travel.max(1.0);
            (position.clamp(0.0, 1.0) * (POSITION_BINS - 1) as f64).round() as usize
        });
        let first = !self.samples.contains_key(&(kind.index(), bin, true));
        let key = (kind.index(), bin, first);
        let replaced_bytes = self
            .samples
            .get(&key)
            .map_or(0, |sample| sample.image.len());
        let region = diagnostic.region;
        let bytes = region.w as usize * region.h as usize * 4;
        if self.image_bytes - replaced_bytes + bytes > MAX_IMAGE_BYTES {
            self.omitted_images += 1;
            return;
        }
        let image = frame
            .view(region.x, region.y, region.w, region.h)
            .to_image();
        self.samples.insert(key, Sample { reading, image });
        self.image_bytes = self.image_bytes - replaced_bytes + bytes;
    }

    pub fn write_files(&self, mut write: impl FnMut(&str, &[u8]) -> Result<()>) -> Result<()> {
        let mut images = Vec::new();
        let mut png = Vec::new();
        for sample in self.samples.values() {
            let reading = &self.readings[sample.reading];
            let path = format!(
                "scrollbar/{}/{:04}.png",
                reading.kind.label(),
                sample.reading
            );
            png.clear();
            PngEncoder::new(&mut png).write_image(
                sample.image.as_raw(),
                sample.image.width(),
                sample.image.height(),
                image::ExtendedColorType::Rgba8,
            )?;
            write(&path, &png)?;
            images.push(json!({"path": path, "reading": sample.reading}));
        }
        let summaries = EquipmentKind::ALL.into_iter().filter_map(|kind| {
            let readings = self.readings.iter().filter(|r| r.kind == kind).collect::<Vec<_>>();
            if readings.is_empty() { return None; }
            let mut heights = readings.iter().filter_map(|r| r.scrollbar.map(|bar| {
                f64::from(bar.lower_y - bar.upper_y) / r.scale
            })).collect::<Vec<_>>();
            heights.sort_by(f64::total_cmp);
            Some(json!({
                "kind": kind, "observations": readings.len(), "detected": heights.len(),
                "cap_distance_reference_px": {
                    "min": heights.first(), "median": heights.get(heights.len() / 2), "max": heights.last(),
                },
            }))
        }).collect::<Vec<_>>();
        write(
            "scrollbar/measurements.json",
            &serde_json::to_vec_pretty(&json!({
                "pixels": "native normalized UI RGBA8; lossless PNG, no resizing or video encoding",
                "coordinates": "ROI pixel coordinates; strip_roi is relative to capture_roi; divide distances by scale for reference pixels",
                "sampling": "one reading per confirmed scan step; first and latest image per position bin",
                "omitted_images": self.omitted_images,
                "summary": summaries, "images": images, "readings": self.readings,
            }))?,
        )
    }
}
