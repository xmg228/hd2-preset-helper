use std::sync::OnceLock;

use anyhow::{Result, ensure};
use fast_image_resize::{
    FilterType, ResizeAlg, ResizeOptions, Resizer, images::TypedImage, pixels::F32,
};
use image::{GrayImage, RgbaImage};
use serde::{Deserialize, Serialize};

const WHITE_SEED_MIN: u8 = 200;
const WHITE_SEED_CHROMA_MAX: u8 = 5;
const TEXT_CHROMA_MAX: u8 = 10;

/// Continuous logical text region inside its integer native crop.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub(crate) struct TextGeometry {
    pub logical_size: (u32, u32),
    pub crop_offset: [f64; 2],
    pub scale: f64,
}

/// Native image/response for extraction and temporal checks; logical signature for identity.
#[derive(Clone)]
pub(crate) struct TextSample {
    pub image: RgbaImage,
    pub response: Vec<f32>,
    pub geometry: TextGeometry,
    signature: OnceLock<TextSignature>,
}

/// Lightly smoothed foreground on the fixed logical grid.
#[derive(Clone)]
pub(crate) struct TextSignature {
    size: (u32, u32),
    response: Vec<f32>,
}

impl TextSignature {
    pub fn cosine(&self, other: &Self) -> f32 {
        if self.size != other.size {
            return 0.0;
        }
        response_cosine(&self.response, &other.response)
    }
}

impl TextSample {
    pub fn from_image(image: RgbaImage, geometry: TextGeometry) -> Result<Self> {
        let (width, height) = image.dimensions();
        ensure!(
            width > 0
                && height > 0
                && geometry.scale.is_finite()
                && geometry.scale > 0.0
                && geometry.logical_size.0 > 0
                && geometry.logical_size.1 > 0
                && geometry.logical_size.0 <= 4096
                && geometry.logical_size.1 <= 512
                && geometry
                    .crop_offset
                    .iter()
                    .all(|v| v.is_finite() && *v >= 0.0)
                && geometry.crop_offset[0] < width as f64
                && geometry.crop_offset[1] < height as f64
                && geometry.crop_offset[0] + geometry.logical_size.0 as f64 * geometry.scale
                    <= width as f64 + 1e-6
                && geometry.crop_offset[1] + geometry.logical_size.1 as f64 * geometry.scale
                    <= height as f64 + 1e-6,
            "text image contains invalid sampling geometry"
        );
        Ok(Self::extract(image, geometry))
    }

    pub fn retain_left_group(mut self) -> Self {
        let gap = (self.geometry.logical_size.1 as f64 * self.geometry.scale)
            .round_ties_even()
            .max(1.0) as usize;
        retain_left_group(&mut self.response, self.image.width() as usize, gap);
        self.signature.take();
        self
    }

    pub fn signature(&self) -> &TextSignature {
        self.signature.get_or_init(|| {
            let TextGeometry {
                logical_size: size,
                crop_offset: [x, y],
                scale,
            } = self.geometry;
            let (width, height) = self.image.dimensions();
            let mut response = if size == (width, height) && scale == 1.0 && x == 0.0 && y == 0.0 {
                self.response.clone()
            } else {
                let source = TypedImage::from_pixels(
                    width,
                    height,
                    self.response.iter().map(|&v| F32::new(v)).collect(),
                )
                .unwrap();
                let mut target = TypedImage::<F32>::new(size.0, size.1);
                // Bilinear convolution also antialiases downsampling. Crop in continuous
                // coordinates: stretching the rounded raster would lose its sampling phase.
                let options = ResizeOptions::new()
                    .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear))
                    .crop(
                        x,
                        y,
                        (size.0 as f64 * scale).min(width as f64 - x),
                        (size.1 as f64 * scale).min(height as f64 - y),
                    );
                Resizer::new()
                    .resize_typed(&source, &mut target, &options)
                    .unwrap();
                target.pixels().iter().map(|p| p.0).collect()
            };
            smooth_signature(&mut response, size.0 as usize);
            TextSignature { size, response }
        })
    }

    pub fn extract(image: RgbaImage, geometry: TextGeometry) -> Self {
        let width = image.width() as i32;
        let height = image.height() as i32;
        let radius = (3.0 * geometry.scale).round_ties_even().max(1.0) as i32;
        let luma = image
            .pixels()
            .map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
            .collect::<Vec<_>>();
        // Dark outlines separate glyph interiors from bright exterior background.
        // Four-connectivity avoids crossing diagonal gaps in thin outlines.
        let bright = luma
            .iter()
            .map(|&value| value >= WHITE_SEED_MIN as f32)
            .collect::<Vec<_>>();
        let mut background = vec![false; luma.len()];
        for pixels in connected(&bright, width as usize, false) {
            if pixels.iter().any(|&index| {
                let (x, y) = (index % width as usize, index / width as usize);
                x == 0 || x + 1 == width as usize || y == 0 || y + 1 == height as usize
            }) {
                for index in pixels {
                    background[index] = true;
                }
            }
        }
        let mut allowed = vec![false; luma.len()];
        let mut seeds = vec![false; luma.len()];
        let mut intensity = vec![0.0; luma.len()];
        for (index, pixel) in image.pixels().enumerate() {
            let low = pixel[0].min(pixel[1]).min(pixel[2]);
            let high = pixel[0].max(pixel[1]).max(pixel[2]);
            let chroma = high - low;
            if background[index] || low < 100 || chroma >= TEXT_CHROMA_MAX {
                continue;
            }
            let (x, y) = (index as i32 % width, index as i32 / width);
            let supported = [(1, 0), (0, 1), (1, 1), (1, -1)].iter().any(|&(dx, dy)| {
                [-1, 1].iter().all(|&sign| {
                    let darkest = (1..=radius)
                        .map(|distance| {
                            let nx = (x + sign * dx * distance).clamp(0, width - 1);
                            let ny = (y + sign * dy * distance).clamp(0, height - 1);
                            luma[(ny * width + nx) as usize]
                        })
                        .fold(f32::INFINITY, f32::min);
                    luma[index] - darkest >= 64.0
                })
            });
            allowed[index] = supported;
            seeds[index] =
                bright[index] && low >= WHITE_SEED_MIN && chroma <= WHITE_SEED_CHROMA_MAX;
            // Small channel changes attenuate a stroke rather than removing it abruptly.
            let neutrality = 1.0 - chroma as f32 / TEXT_CHROMA_MAX as f32;
            intensity[index] = (low as f32 - 100.0) / 155.0 * neutrality;
        }
        let mut response = vec![0.0; luma.len()];
        for pixels in connected(&allowed, width as usize, true) {
            if pixels.iter().any(|&index| seeds[index]) {
                for index in pixels {
                    response[index] = intensity[index];
                }
            }
        }
        Self {
            image,
            response,
            geometry,
            signature: OnceLock::new(),
        }
    }

    /// Item/category identity, including saved templates and caches from other resolutions.
    pub fn cosine(&self, other: &Self) -> f32 {
        self.signature().cosine(other.signature())
    }

    /// Frame-to-frame change at one resolution; no resampling needed.
    pub fn native_cosine(&self, other: &Self) -> f32 {
        if self.image.dimensions() != other.image.dimensions() {
            return 0.0;
        }
        response_cosine(&self.response, &other.response)
    }

    pub fn foreground(&self) -> GrayImage {
        GrayImage::from_raw(
            self.image.width(),
            self.image.height(),
            self.response
                .iter()
                .map(|value| (value * 255.0).round() as u8)
                .collect(),
        )
        .unwrap()
    }

    pub fn has_text(&self) -> bool {
        self.response.iter().any(|&value| value > 0.0)
    }
}

/// Separable three-tap Gaussian, sigma = 0.5 logical pixels, with replicated edges.
fn smooth_signature(response: &mut [f32], width: usize) {
    const SIDE: f32 = 0.106_506_98;
    const CENTER: f32 = 1.0 - 2.0 * SIDE;
    let height = response.len() / width;
    let mut horizontal = vec![0.0; response.len()];
    for (source, target) in response
        .chunks_exact(width)
        .zip(horizontal.chunks_exact_mut(width))
    {
        for x in 0..width {
            target[x] = SIDE * source[x.saturating_sub(1)]
                + CENTER * source[x]
                + SIDE * source[(x + 1).min(width - 1)];
        }
    }
    for y in 0..height {
        let above = y.saturating_sub(1) * width;
        let current = y * width;
        let below = (y + 1).min(height - 1) * width;
        for x in 0..width {
            response[current + x] = SIDE * horizontal[above + x]
                + CENTER * horizontal[current + x]
                + SIDE * horizontal[below + x];
        }
    }
}

/// Keep the left-aligned text group, stopping at a blank gap wider than one line.
fn retain_left_group(response: &mut [f32], width: usize, gap: usize) {
    let mut columns = vec![false; width];
    for row in response.chunks_exact(width) {
        for (occupied, &value) in columns.iter_mut().zip(row) {
            *occupied |= value > 0.0;
        }
    }
    // The first group must also start near the name's known left edge.
    let mut end = 0;
    for (x, &occupied) in columns.iter().enumerate() {
        if occupied {
            if x - end > gap {
                break;
            }
            end = x + 1;
        }
    }
    for row in response.chunks_exact_mut(width) {
        row[end..].fill(0.0);
    }
}

fn response_cosine(left: &[f32], right: &[f32]) -> f32 {
    let (mut dot, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for (&a, &b) in left.iter().zip(right) {
        dot += a as f64 * b as f64;
        aa += (a as f64).powi(2);
        bb += (b as f64).powi(2);
    }
    if aa == 0.0 || bb == 0.0 {
        return 0.0;
    }
    (dot / (aa * bb).sqrt()).clamp(0.0, 1.0) as f32
}

// Connected components in a row-major binary mask, optionally including diagonals.
fn connected(mask: &[bool], width: usize, diagonals: bool) -> Vec<Vec<usize>> {
    let height = mask.len() / width;
    let mut seen = vec![false; mask.len()];
    let mut components = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut pixels = vec![start];
        let mut head = 0;
        while head < pixels.len() {
            let index = pixels[head];
            head += 1;
            let (x, y) = (index % width, index / width);
            for ny in y.saturating_sub(1)..=(y + 1).min(height - 1) {
                for nx in x.saturating_sub(1)..=(x + 1).min(width - 1) {
                    if !diagonals && nx != x && ny != y {
                        continue;
                    }
                    let next = ny * width + nx;
                    if mask[next] && !seen[next] {
                        seen[next] = true;
                        pixels.push(next);
                    }
                }
            }
        }
        components.push(pixels);
    }
    components
}
