use anyhow::{Result, ensure};
use image::RgbaImage;

use crate::item::StratagemCategory;

use super::matcher::SemanticImage;
use super::{ImageSample, SampleGeometry, Slot};

const FIXED_BACKGROUND: [f32; 3] = [44.0 / 255.0; 3];
const FIXED_WHITE: [f32; 3] = [1.0, 1.0, 238.0 / 255.0];
const CATEGORIES: [StratagemCategory; 3] = [
    StratagemCategory::Offensive,
    StratagemCategory::Defensive,
    StratagemCategory::Supply,
];

pub(super) fn stratagem_foreground_response(r: u8, g: u8, b: u8) -> u8 {
    let pixel = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0];
    let response = CATEGORIES
        .into_iter()
        .map(|category| {
            let [class, white] = project_triangle(
                pixel,
                FIXED_BACKGROUND,
                FIXED_WHITE,
                fixed_class_endpoint(category),
            );
            class + white
        })
        .fold(0.0, f32::max);
    (response.clamp(0.0, 1.0) * 255.0).round() as u8
}

pub struct SemanticExtraction {
    pub image: SemanticImage,
    pub mode: &'static str,
    pub primary_endpoint: [f32; 3],
    pub secondary_endpoint: [f32; 3],
    pub primary_mass: f32,
    pub secondary_mass: f32,
}

pub struct SemanticSource {
    width: usize,
    height: usize,
    center_x: f32,
    center_y: f32,
    pixels: Vec<[f32; 3]>,
}

impl SemanticSource {
    pub fn prepare(sample: &ImageSample) -> Result<Self> {
        let image = &sample.image;
        ensure!(
            image.width() >= 3 && image.height() >= 3,
            "sample image is too small for semantic extraction"
        );

        let width = image.width() as usize;
        let height = image.height() as usize;
        let pixels = image
            .pixels()
            .map(|pixel| {
                [
                    pixel[0] as f32 / 255.0,
                    pixel[1] as f32 / 255.0,
                    pixel[2] as f32 / 255.0,
                ]
            })
            .collect::<Vec<_>>();
        Ok(Self {
            width,
            height,
            center_x: sample.geometry.center_x,
            center_y: sample.geometry.center_y,
            pixels,
        })
    }

    /// Chooses only the nominal color family; it does not redefine the endpoint.
    pub fn infer_category(&self) -> Result<StratagemCategory> {
        let mut hue_x = 0.0f64;
        let mut hue_y = 0.0f64;
        for &pixel in &self.pixels {
            let [l, a, b] = rgb_to_lab(pixel);
            let chroma = a.hypot(b);
            let delta = squared_distance(pixel, FIXED_BACKGROUND).sqrt();
            let seed = smoothstep(chroma, 8.0, 28.0)
                * smoothstep(delta, 0.025, 0.10)
                * smoothstep(l, 12.0, 45.0);
            if chroma > 1e-6 {
                hue_x += (seed * a / chroma) as f64;
                hue_y += (seed * b / chroma) as f64;
            }
        }
        let hue_mass = hue_x.hypot(hue_y);
        ensure!(
            hue_mass > 1e-6,
            "local template has insufficient category-color evidence"
        );
        let hue = [hue_x / hue_mass, hue_y / hue_mass];
        Ok(CATEGORIES
            .into_iter()
            .max_by(|left, right| {
                category_hue_score(*left, hue).total_cmp(&category_hue_score(*right, hue))
            })
            .expect("the fixed category table is not empty"))
    }

    pub fn extract(&self, category: StratagemCategory) -> Result<SemanticExtraction> {
        let class_endpoint = fixed_class_endpoint(category);
        let semantic_pixels = self
            .pixels
            .iter()
            .map(|&pixel| project_triangle(pixel, FIXED_BACKGROUND, FIXED_WHITE, class_endpoint))
            .collect::<Vec<_>>();
        let class_mass = semantic_pixels
            .iter()
            .map(|pixel| pixel[0] as f64)
            .sum::<f64>() as f32;
        let white_mass = semantic_pixels
            .iter()
            .map(|pixel| pixel[1] as f64)
            .sum::<f64>() as f32;
        let image = SemanticImage::new(self.width, self.height, semantic_pixels)?
            .with_center(self.center_x, self.center_y);

        Ok(SemanticExtraction {
            image,
            mode: "stratagem_cw",
            primary_endpoint: FIXED_WHITE,
            secondary_endpoint: class_endpoint,
            primary_mass: white_mass,
            secondary_mass: class_mass,
        })
    }
}

fn fixed_class_endpoint(category: StratagemCategory) -> [f32; 3] {
    let rgb = match category {
        StratagemCategory::Offensive => [213.0, 97.0, 82.0],
        StratagemCategory::Defensive => [103.0, 149.0, 82.0],
        StratagemCategory::Supply => [79.0, 179.0, 208.0],
    };
    rgb.map(|value| value / 255.0)
}

fn category_hue_score(category: StratagemCategory, hue: [f64; 2]) -> f64 {
    let [_, a, b] = rgb_to_lab(fixed_class_endpoint(category));
    let chroma = a.hypot(b).max(1e-6);
    hue[0] * (a / chroma) as f64 + hue[1] * (b / chroma) as f64
}

pub fn crop_slot_sample(image: &RgbaImage, slot: &Slot, physical_size: f32) -> Result<ImageSample> {
    ensure!(
        physical_size.is_finite() && physical_size > 0.0,
        "sample physical size must be positive"
    );

    let core = slot.core_rect();
    ensure!(
        core.x + core.w <= image.width() && core.y + core.h <= image.height(),
        "sample crop ({},{},{},{}) is outside image {}x{}",
        core.x,
        core.y,
        core.w,
        core.h,
        image.width(),
        image.height()
    );

    Ok(ImageSample {
        image: image::imageops::crop_imm(image, core.x, core.y, core.w, core.h).to_image(),
        geometry: SampleGeometry {
            center_x: slot.center_f32().0 - core.x as f32,
            center_y: slot.center_f32().1 - core.y as f32,
            physical_size,
        },
    })
}

fn project_triangle(pixel: [f32; 3], base: [f32; 3], white: [f32; 3], class: [f32; 3]) -> [f32; 2] {
    let white_direction = subtract(white, base);
    let class_direction = subtract(class, base);
    let delta = subtract(pixel, base);
    let ww = dot(white_direction, white_direction);
    let cc = dot(class_direction, class_direction);
    let wc = dot(white_direction, class_direction);
    let dw = dot(delta, white_direction);
    let dc = dot(delta, class_direction);
    let determinant = ww * cc - wc * wc;
    let white_alpha = (dw * cc - dc * wc) / (determinant + 1e-12);
    let class_alpha = (dc * ww - dw * wc) / (determinant + 1e-12);

    let mut best = Projection::default();
    if white_alpha >= 0.0
        && class_alpha >= 0.0
        && white_alpha + class_alpha <= 1.0
        && determinant > 1e-12
    {
        best = Projection {
            white: white_alpha,
            class: class_alpha,
            error: reconstruction_error(
                pixel,
                base,
                white_direction,
                class_direction,
                white_alpha,
                class_alpha,
            ),
        };
    }

    let bw = (dw / (ww + 1e-12)).clamp(0.0, 1.0);
    best.replace_if_better(Projection {
        white: bw,
        class: 0.0,
        error: reconstruction_error(pixel, base, white_direction, class_direction, bw, 0.0),
    });

    let bc = (dc / (cc + 1e-12)).clamp(0.0, 1.0);
    best.replace_if_better(Projection {
        white: 0.0,
        class: bc,
        error: reconstruction_error(pixel, base, white_direction, class_direction, 0.0, bc),
    });

    let edge = subtract(class_direction, white_direction);
    let from_white = subtract(pixel, add(base, white_direction));
    let wc = (dot(from_white, edge) / (dot(edge, edge) + 1e-12)).clamp(0.0, 1.0);
    best.replace_if_better(Projection {
        white: 1.0 - wc,
        class: wc,
        error: reconstruction_error(pixel, base, white_direction, class_direction, 1.0 - wc, wc),
    });

    [best.class, best.white]
}

struct Projection {
    white: f32,
    class: f32,
    error: f32,
}

impl Default for Projection {
    fn default() -> Self {
        Self {
            white: 0.0,
            class: 0.0,
            error: f32::INFINITY,
        }
    }
}

impl Projection {
    fn replace_if_better(&mut self, candidate: Self) {
        if candidate.error < self.error {
            *self = candidate;
        }
    }
}

fn reconstruction_error(
    pixel: [f32; 3],
    base: [f32; 3],
    white_direction: [f32; 3],
    class_direction: [f32; 3],
    white_alpha: f32,
    class_alpha: f32,
) -> f32 {
    let reconstructed = std::array::from_fn(|channel| {
        base[channel]
            + white_alpha * white_direction[channel]
            + class_alpha * class_direction[channel]
    });
    squared_distance(pixel, reconstructed)
}

fn rgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    let linear = rgb.map(|value| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    let x = (0.412_453 * linear[0] + 0.357_58 * linear[1] + 0.180_423 * linear[2]) / 0.950_456;
    let y = 0.212_671 * linear[0] + 0.715_16 * linear[1] + 0.072_169 * linear[2];
    let z = (0.019_334 * linear[0] + 0.119_193 * linear[1] + 0.950_227 * linear[2]) / 1.088_754;
    let transform = |value: f32| {
        if value > 0.008_856 {
            value.cbrt()
        } else {
            7.787 * value + 16.0 / 116.0
        }
    };
    let fx = transform(x);
    let fy = transform(y);
    let fz = transform(z);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn smoothstep(value: f32, low: f32, high: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn add(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|channel| left[channel] + right[channel])
}

fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|channel| left[channel] - right[channel])
}

fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn squared_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    dot(subtract(left, right), subtract(left, right))
}
