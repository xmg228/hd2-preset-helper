use std::sync::OnceLock;

use fast_image_resize::{
    FilterType, ResizeAlg, ResizeOptions, Resizer, images::TypedImage, pixels::F32,
};
use image::{GrayImage, RgbaImage};
use serde::Serialize;

const WHITE_SEED_MIN: u8 = 200;
const TEXT_CHROMA_MAX: u8 = 5;

/// Continuous logical text region inside its integer native crop.
#[derive(Clone, Copy, Debug, Serialize)]
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

/// Foreground on the fixed logical grid, independent of capture resolution.
#[derive(Clone)]
pub(crate) struct TextSignature {
    size: (u32, u32),
    response: Vec<f32>,
}

impl TextSignature {
    pub fn from_foreground(image: GrayImage) -> Self {
        Self {
            size: image.dimensions(),
            response: image
                .into_raw()
                .into_iter()
                .map(|p| p as f32 / 255.0)
                .collect(),
        }
    }

    pub fn foreground(&self) -> GrayImage {
        GrayImage::from_raw(
            self.size.0,
            self.size.1,
            self.response
                .iter()
                .map(|p| (p * 255.0).round() as u8)
                .collect(),
        )
        .unwrap()
    }

    pub fn cosine(&self, other: &Self) -> f32 {
        if self.size != other.size {
            return 0.0;
        }
        response_cosine(&self.response, &other.response)
    }
}

impl TextSample {
    pub fn signature(&self) -> &TextSignature {
        self.signature.get_or_init(|| {
            let TextGeometry {
                logical_size: size,
                crop_offset: [x, y],
                scale,
            } = self.geometry;
            let (width, height) = self.image.dimensions();
            if size == (width, height) && scale == 1.0 && x == 0.0 && y == 0.0 {
                return TextSignature {
                    size,
                    response: self.response.clone(),
                };
            }
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
            TextSignature {
                size,
                response: target.pixels().iter().map(|p| p.0).collect(),
            }
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
            if background[index] || low < 100 || high - low > TEXT_CHROMA_MAX {
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
            seeds[index] = bright[index] && low >= WHITE_SEED_MIN;
            intensity[index] = (low as f32 - 100.0) / 155.0;
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
