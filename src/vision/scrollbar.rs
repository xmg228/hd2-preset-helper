//! Scrollbar evidence for the shared Stratagem/Booster ROI, independent of navigation.
use image::RgbaImage;
use serde::Serialize;

use super::RoiGeometry;

// Reference ROI coordinates. Sample inside the 7 px wide thumb, not its edges.
const CENTER_X: f64 = 413.5;
const TRACK_TOP: f64 = 183.0;
const TRACK_BOTTOM: f64 = 711.0;
const YELLOW_NOISE_FLOOR: f64 = 16.0;
const CAP_RESPONSE_THRESHOLD: f64 = 40.0;
const ENDPOINT_TOLERANCE_PX: f64 = 0.75;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScrollbarObservation {
    /// Yellow cap centers in native ROI pixels, like the observed slot centers.
    pub upper_y: f32,
    pub lower_y: f32,
    pub at_top: bool,
    pub at_bottom: bool,
}

struct Row {
    yellow: f64,
    gray: bool,
}

struct Cap {
    start: usize,
    end: usize,
    center: f64,
}

/// Missing or ambiguous evidence is unknown, including lists with no scrollbar.
pub(super) fn detect(image: &RgbaImage, geometry: RoiGeometry) -> Option<ScrollbarObservation> {
    let scale = geometry.scale;
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let left = (geometry.logical_origin_x + (CENTER_X - 1.5) * scale).floor();
    let right = (geometry.logical_origin_x + (CENTER_X + 1.5) * scale).ceil();
    let top = geometry.logical_origin_y + TRACK_TOP * scale;
    let bottom = geometry.logical_origin_y + TRACK_BOTTOM * scale;
    let first = (top - 3.0 * scale).floor();
    let last = (bottom + 3.0 * scale).ceil();
    if left < 0.0 || first < 0.0 || right > image.width() as f64 || last > image.height() as f64 {
        return None;
    }

    let rows = (first as u32..last as u32)
        .map(|y| {
            let mut yellow = 0.0;
            let mut gray = 0;
            for x in left as u32..right as u32 {
                let [r, g, b, _] = image.get_pixel(x, y).0.map(f64::from);
                yellow += (r.min(g) - b - YELLOW_NOISE_FLOOR).max(0.0);
                gray += usize::from(
                    r.max(g).max(b) - r.min(g).min(b) <= 16.0 && r.min(g).min(b) >= 64.0,
                );
            }
            let width = right - left;
            Row {
                yellow: yellow / width,
                gray: gray as f64 >= width * 0.8,
            }
        })
        .collect::<Vec<_>>();

    let mut caps = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        if rows[index].yellow < CAP_RESPONSE_THRESHOLD {
            index += 1;
            continue;
        }
        let start = index;
        while index < rows.len() && rows[index].yellow >= CAP_RESPONSE_THRESHOLD {
            index += 1;
        }
        let end = index;
        if (end - start) as f64 > 4.0 * scale + 1.0 {
            return None;
        }
        // Include partial-coverage pixels adjacent to the detected cap.
        let mut mass = 0.0;
        let mut moment = 0.0;
        for (i, row) in rows
            .iter()
            .enumerate()
            .take((end + 1).min(rows.len()))
            .skip(start.saturating_sub(1))
        {
            mass += row.yellow;
            moment += (first + i as f64 + 0.5) * row.yellow;
        }
        caps.push(Cap {
            start,
            end,
            center: moment / mass,
        });
    }
    let [upper, lower] = caps.as_slice() else {
        return None;
    };
    let body = rows.get(upper.end..lower.start)?;
    if (body.len() as f64) < 6.0 * scale
        || (body.iter().filter(|row| row.gray).count() as f64) < body.len() as f64 * 0.8
        || upper.center < top - ENDPOINT_TOLERANCE_PX
        || lower.center > bottom + ENDPOINT_TOLERANCE_PX
    {
        return None;
    }
    Some(ScrollbarObservation {
        upper_y: upper.center as f32,
        lower_y: lower.center as f32,
        at_top: (upper.center - top).abs() <= ENDPOINT_TOLERANCE_PX,
        at_bottom: (lower.center - bottom).abs() <= ENDPOINT_TOLERANCE_PX,
    })
}
