//! Equipment-page observation. Layout priors and UI semantics stay out of frame scoring.
use anyhow::Result;
use image::{GenericImageView, RgbaImage};
use rayon::prelude::*;
use serde::Serialize;

use super::borders::{
    FixedEdgeProfiles, RowCandidate, SamplingParams, SlotScratch, rgb_at, round_ties_even,
    score_slot_direct, select_rows, supported_columns, tangent_positions, vertical_tangent_offsets,
};
use super::text::{TextGeometry, TextSample};
use super::{
    Calibration, ReferenceSize, ResolvedRoi, RoiAnchor, RoiGeometry, ScaleAxis,
    resolve_calibration_roi_for_size,
};
use crate::{image_rect::ImageRect, item::EquipmentKind};

// Entry cards, category heading, list and item name share one capture and coordinate system.
const ROI: ImageRect = ImageRect {
    x: 48,
    y: 180,
    w: 1562,
    h: 810,
};

#[derive(Clone, Copy)]
pub(crate) enum EquipmentText {
    Name,
    Category,
}

#[derive(Serialize)]
pub struct EquipmentEntry {
    pub rects: [[f64; 4]; 6],
    pub scores: [f32; 6],
}

impl EquipmentEntry {
    pub fn confirmed(&self) -> bool {
        // During the return animation, the bottom row can pass through the top row's position.
        self.scores
            .chunks_exact(3)
            .all(|row| row.iter().any(|&q| q >= 0.05))
    }

    pub fn point(&self, kind: EquipmentKind) -> (u32, u32) {
        let [l, t, r, b] = self.rects[kind.index()];
        (
            ((l + r) * 0.5).round() as u32,
            ((t + b) * 0.5).round() as u32,
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct EquipmentSlot {
    pub row: usize,
    pub col: usize,
    /// Pixel-index coordinates of opposite frame center lines in the captured ROI.
    pub rect: [f64; 4],
    pub frame_score: f32,
    pub focus_response: f32,
    pub hover_response: f32,
    pub equipped: bool,
}

impl EquipmentSlot {
    pub fn center(&self) -> (u32, u32) {
        let [l, t, r, b] = self.rect;
        (
            ((l + r) * 0.5).round() as u32,
            ((t + b) * 0.5).round() as u32,
        )
    }
}

#[derive(Clone)]
pub struct EquipmentObservation {
    pub slots: Vec<EquipmentSlot>,
    pub focus: Option<usize>,
    pub hover: Option<usize>,
    pub name: TextSample,
    /// Displayed heading, also changed by hover; not necessarily the focus's category.
    /// Helmets and capes have no category navigation.
    pub category: Option<TextSample>,
    pub scale: f64,
}

#[derive(Debug, Serialize)]
pub struct PageChange {
    pub geometry_changed: bool,
    pub focus_changed: bool,
    pub name_cosine: f32,
    pub name_changed: bool,
    pub category_cosine: Option<f32>,
    pub category_changed: bool,
}

impl PageChange {
    pub fn observed(&self) -> bool {
        self.geometry_changed || self.focus_changed || self.name_changed || self.category_changed
    }
}

impl EquipmentObservation {
    pub fn ready(&self) -> bool {
        self.focus.is_some() && self.name.has_text()
    }

    pub fn change_from(&self, previous: &Self) -> PageChange {
        // Ignore border sampling jitter; indices are viewport-local, not global rows.
        let moved = |a: &EquipmentSlot, b: &EquipmentSlot| {
            a.col != b.col || (a.rect[1] - b.rect[1]).abs() > 3.0 * self.scale
        };
        let geometry_changed = self.slots.len() != previous.slots.len()
            || self
                .slots
                .iter()
                .zip(&previous.slots)
                .any(|(a, b)| moved(a, b));
        let focus_changed = match (self.focus, previous.focus) {
            (Some(a), Some(b)) => moved(&self.slots[a], &previous.slots[b]),
            _ => self.focus != previous.focus,
        };
        let name_cosine = self.name.native_cosine(&previous.name);
        let category_cosine = self
            .category
            .as_ref()
            .zip(previous.category.as_ref())
            .map(|(current, old)| current.native_cosine(old));
        PageChange {
            geometry_changed,
            focus_changed,
            name_cosine,
            // Provisional temporal-change threshold, deliberately not an identity threshold.
            name_changed: self.name.has_text() && previous.name.has_text() && name_cosine < 0.97,
            category_cosine,
            category_changed: category_cosine.is_some_and(|score| score < 0.97),
        }
    }
}

pub struct EquipmentObserver {
    kind: EquipmentKind,
    geometry: RoiGeometry,
}

impl EquipmentObserver {
    /// All kinds share the same capture ROI; only list layout/semantics change.
    pub fn with_kind(&self, kind: EquipmentKind) -> Self {
        Self {
            kind,
            geometry: self.geometry,
        }
    }

    pub fn resolve(width: u32, height: u32, kind: EquipmentKind) -> Result<(Self, ResolvedRoi)> {
        let resolved = resolve_calibration_roi_for_size(
            width,
            height,
            &Calibration {
                reference: ReferenceSize { w: 1920, h: 1080 },
                roi_ref: ROI,
                scale_axis: ScaleAxis::Fit,
                anchor: RoiAnchor::TopCenter,
            },
        )?;
        Ok((
            Self {
                kind,
                geometry: resolved.geometry,
            },
            resolved,
        ))
    }

    pub fn entry(&self, image: &RgbaImage) -> EquipmentEntry {
        let params = self.sampling_params(128.0, 94.0);
        let offsets = vertical_tangent_offsets(params);
        let mut scratch = SlotScratch::default();
        let rects = std::array::from_fn(|index| {
            let left = self.geometry.logical_origin_x
                + (56.0 + (index % 3) as f64 * 145.0 - ROI.x as f64) * self.geometry.scale
                - 0.5;
            let top = self.geometry.logical_origin_y
                + (718.0 + (index / 3) as f64 * 104.0 - ROI.y as f64) * self.geometry.scale
                - 0.5;
            [left, top, left + params.width, top + params.height]
        });
        let scores = rects.map(|[left, top, _, _]| {
            score_slot_direct(image, left, top, params, &offsets, &mut scratch).q
        });
        EquipmentEntry { rects, scores }
    }

    pub fn observe(&self, image: &RgbaImage) -> EquipmentObservation {
        let slots = match self.kind {
            EquipmentKind::Primary | EquipmentKind::Secondary => {
                self.grid(image, [203.0, 503.0], 282.0, 150.0, 168.0)
            }
            _ => self.grid(image, [203.0, 403.0, 603.0], 182.0, 182.0, 200.0),
        };
        let hover = slots
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.hover_response.total_cmp(&b.hover_response))
            .and_then(|(index, hovered)| {
                let mut baseline = slots
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, slot)| slot.hover_response)
                    .collect::<Vec<_>>();
                if baseline.is_empty() {
                    return None;
                }
                baseline.sort_unstable_by(f32::total_cmp);
                let median = baseline[baseline.len() / 2];
                (hovered.hover_response >= median + 0.10).then_some(index)
            });
        let focus = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.focus_response >= 0.2)
            .max_by(|(_, a), (_, b)| a.focus_response.total_cmp(&b.focus_response))
            .map(|(index, _)| index)
            // Valid on a settled page; navigation must not mistake an old hover for new focus.
            .or(hover);
        let name = self.text(image, EquipmentText::Name);
        // Exclude Z/C key labels and the horizontal separator below the heading.
        let category = (!matches!(self.kind, EquipmentKind::Helmet | EquipmentKind::Cape))
            .then(|| self.text(image, EquipmentText::Category));
        EquipmentObservation {
            slots,
            focus,
            hover,
            name,
            category,
            scale: self.geometry.scale,
        }
    }

    fn text(&self, image: &RgbaImage, field: EquipmentText) -> TextSample {
        let (rect, geometry) = self.text_region(field);
        TextSample::extract(
            image::imageops::crop_imm(image, rect.x, rect.y, rect.w, rect.h).to_image(),
            geometry,
        )
    }

    /// Shared by live crops and native template loading, including fractional crop margins.
    pub(crate) fn text_region(&self, field: EquipmentText) -> (ImageRect, TextGeometry) {
        let bounds = match field {
            EquipmentText::Name => [885.0, 578.0, 1600.0, 615.0],
            // Excludes Z/C labels and the horizontal separator below the heading.
            EquipmentText::Category => [234.0, 192.0, 752.0, 226.0],
        };
        let s = self.geometry.scale;
        let x = |v| self.geometry.logical_origin_x + (v - ROI.x as f64) * s;
        let y = |v| self.geometry.logical_origin_y + (v - ROI.y as f64) * s;
        let left = x(bounds[0]).floor() as u32;
        let top = y(bounds[1]).floor() as u32;
        let rect = ImageRect {
            x: left,
            y: top,
            w: x(bounds[2]).ceil() as u32 - left,
            h: y(bounds[3]).ceil() as u32 - top,
        };
        (
            rect,
            TextGeometry {
                logical_size: (
                    (bounds[2] - bounds[0]) as u32,
                    (bounds[3] - bounds[1]) as u32,
                ),
                crop_offset: [x(bounds[0]) - left as f64, y(bounds[1]) - top as f64],
                scale: s,
            },
        )
    }

    fn sampling_params(&self, width: f64, height: f64) -> SamplingParams {
        let scale = self.geometry.scale;
        SamplingParams {
            width: width * scale,
            height: height * scale,
            trim: round_ties_even(8.0 * scale).max(2),
            step: round_ties_even(3.0 * scale).max(1),
            reference_distance: round_ties_even(4.0 * scale).max(2),
            reference_half_width: round_ties_even(scale).max(0),
        }
    }

    fn grid<const N: usize>(
        &self,
        image: &RgbaImage,
        columns: [f64; N],
        width: f64,
        height: f64,
        pitch: f64,
    ) -> Vec<EquipmentSlot> {
        let scale = self.geometry.scale;
        // Convert continuous pixel-corner coordinates to native pixel indices once.
        let columns =
            columns.map(|x| self.geometry.logical_origin_x + (x - ROI.x as f64) * scale - 0.5);
        let y_min =
            (self.geometry.logical_origin_y + (237.5 - ROI.y as f64) * scale - 0.5).ceil() as i32;
        let y_max = (self.geometry.logical_origin_y + (975.5 - height - ROI.y as f64) * scale - 0.5)
            .floor() as i32;
        let params = self.sampling_params(width, height);
        let profiles = FixedEdgeProfiles::build(image, &columns, y_min, y_max, params);
        let curve = (y_min..=y_max)
            .into_par_iter()
            .map_init(SlotScratch::default, |scratch, y| {
                let slots = std::array::from_fn(|col| profiles.score_slot(col, y, scratch));
                RowCandidate {
                    y,
                    score: slots.iter().map(|s| s.q).fold(0.0, f32::max),
                    slots,
                }
            })
            .collect::<Vec<RowCandidate<N>>>();
        let rows = select_rows(&curve, 10, round_ties_even((pitch - 1.0) * scale).max(1));
        let mut slots = Vec::new();
        for (row, candidate) in rows.iter().enumerate() {
            let supported = supported_columns(&candidate.slots);
            let occupied = supported
                .iter()
                .rposition(|&ok| ok)
                .map_or(0, |col| col + 1);
            for (col, &left) in columns.iter().enumerate().take(occupied) {
                let top = candidate.y as f64;
                let rect = [left, top, left + params.width, top + params.height];
                let [focus_response, hover_response] = border_responses(image, rect, params);
                slots.push(EquipmentSlot {
                    row,
                    col,
                    rect,
                    frame_score: candidate.slots[col].q,
                    focus_response,
                    hover_response,
                    equipped: equipped_marker(image, rect, scale),
                });
            }
        }
        slots
    }
}

fn border_responses(image: &RgbaImage, rect: [f64; 4], params: SamplingParams) -> [f32; 2] {
    let mut responses = [Vec::new(), Vec::new()];
    let mut sample = |x, y| {
        if let Some([r, g, b]) = rgb_at(image, x, y) {
            responses[0].push((r.min(g) - b).max(0.0));
            responses[1].push(r.min(g).min(b));
        }
    };
    for x in tangent_positions(
        rect[0] + params.trim as f64,
        rect[2] - params.trim as f64,
        params.step,
    ) {
        sample(x, round_ties_even(rect[1]));
        sample(x, round_ties_even(rect[3]));
    }
    for dy in vertical_tangent_offsets(params) {
        sample(round_ties_even(rect[0]), round_ties_even(rect[1]) + dy);
        sample(round_ties_even(rect[2]), round_ties_even(rect[1]) + dy);
    }
    responses.map(|mut values| {
        values.sort_unstable_by(f32::total_cmp);
        if values.is_empty() {
            return 0.0;
        }
        let p = (values.len() - 1) as f32 * 0.75;
        let lo = p.floor() as usize;
        values[lo] + (values[p.ceil() as usize] - values[lo]) * p.fract()
    })
}

pub(crate) fn equipped_marker_rect(rect: [f64; 4], scale: f64) -> ImageRect {
    // Dot center: 11 reference pixels inside the frame. Convert pixel-index frame
    // coordinates to pixel-corner coordinates (+0.5), then round only the origin.
    // The patch stays inside the complete detected slot and excludes its border.
    let side = (8.0 * scale).round().max(1.0) as u32;
    let [x, y] = [rect[2], rect[3]]
        .map(|edge| (edge + 0.5 - 11.0 * scale - side as f64 * 0.5).round() as u32);
    ImageRect {
        x,
        y,
        w: side,
        h: side,
    }
}

fn equipped_marker(image: &RgbaImage, rect: [f64; 4], scale: f64) -> bool {
    let patch = equipped_marker_rect(rect, scale);
    let yellow = image
        .view(patch.x, patch.y, patch.w, patch.h)
        .pixels()
        .filter(|(_, _, p)| p[0].min(p[1]) as i32 - p[2] as i32 > 140)
        .count();
    yellow * 4 >= patch.w as usize * patch.h as usize
}
