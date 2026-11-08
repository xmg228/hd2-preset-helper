//! Native icon semantics shared by captured presets and list scans.
use std::ops::Deref;

use anyhow::{Result, ensure};
use image::RgbaImage;
use rayon::prelude::*;

use crate::{
    image_rect::ImageRect,
    item::{ItemKind, StratagemCategory},
};

use super::{
    ImageSample, SampleGeometry, Slot, booster,
    matcher::{SemanticImage, render_to_raster},
    semantic_extractor::{
        FIXED_BACKGROUND, FIXED_WHITE, SemanticSource, crop_slot_sample, fixed_class_endpoint,
        stratagem_foreground_response,
    },
};

#[derive(Clone)]
pub(crate) struct IconSample {
    pub sample: ImageSample,
    template: IconTemplate,
}

impl IconSample {
    pub(crate) fn extract(kind: ItemKind, sample: ImageSample) -> Result<Self> {
        let template = IconTemplate::extract(kind, &sample)?;
        Ok(Self { sample, template })
    }
}

impl Deref for IconSample {
    type Target = IconTemplate;

    fn deref(&self) -> &Self::Target {
        &self.template
    }
}

#[derive(Clone)]
pub(crate) struct IconTemplate {
    pub semantic: SemanticImage,
    pub geometry: SampleGeometry,
    pub category: Option<StratagemCategory>,
}

impl IconTemplate {
    pub(crate) fn extract(kind: ItemKind, sample: &ImageSample) -> Result<Self> {
        let (semantic, category) = match kind {
            ItemKind::Stratagem => {
                let source = SemanticSource::prepare(sample)?;
                let category = source.infer_category()?;
                (source.extract(category)?.image, Some(category))
            }
            ItemKind::Booster => (booster::extract(sample)?.image, None),
        };
        Ok(Self {
            semantic,
            geometry: sample.geometry,
            category,
        })
    }
}

/// Native two-channel cosine errors, indexed by candidate then template.
pub(crate) fn icon_cosine_errors(
    kind: ItemKind,
    templates: &[&IconTemplate],
    candidates: &[&ImageSample],
) -> Result<Vec<Vec<f32>>> {
    ensure!(
        templates.iter().all(|template| match kind {
            ItemKind::Stratagem => template.category.is_some(),
            ItemKind::Booster => template.category.is_none(),
        }),
        "icon template semantics do not match the requested item kind"
    );
    if templates.is_empty() {
        return Ok(vec![Vec::new(); candidates.len()]);
    }
    let categories = templates
        .iter()
        .fold(Vec::new(), |mut categories, template| {
            if let Some(category) = template.category
                && !categories.contains(&category)
            {
                categories.push(category);
            }
            categories
        });
    candidates
        .par_iter()
        .map(|sample| {
            let rect = texture_rect(sample)?;
            let responses = match kind {
                ItemKind::Stratagem => {
                    let source = SemanticSource::prepare(sample)?;
                    categories
                        .iter()
                        .map(|&category| {
                            source.extract(category).map(|extraction| {
                                normalized_response(&extraction.image, rect, sample.image.width())
                            })
                        })
                        .collect::<Result<Vec<_>>>()?
                }
                ItemKind::Booster => vec![normalized_response(
                    &booster::extract(sample)?.image,
                    rect,
                    sample.image.width(),
                )],
            };
            let center = (
                sample.geometry.center_x - rect.x as f32,
                sample.geometry.center_y - rect.y as f32,
            );
            Ok(templates
                .iter()
                .map(|template| {
                    let index = template.category.map_or(0, |category| {
                        categories
                            .iter()
                            .position(|&value| value == category)
                            .expect("every template category has a candidate response")
                    });
                    let reference = render_to_raster(
                        &template.semantic,
                        template.geometry.physical_size,
                        (rect.w as usize, rect.h as usize),
                        center,
                        sample.geometry.physical_size,
                        (0.0, 0.0),
                    );
                    let norm = reference
                        .pixels()
                        .iter()
                        .flatten()
                        .map(|value| value * value)
                        .sum::<f32>()
                        .sqrt()
                        .max(f32::EPSILON);
                    let cosine = reference
                        .pixels()
                        .iter()
                        .flatten()
                        .zip(&responses[index])
                        .map(|(left, right)| left / norm * right)
                        .sum::<f32>();
                    1.0 - cosine.clamp(0.0, 1.0)
                })
                .collect())
        })
        .collect()
}

fn normalized_response(image: &SemanticImage, rect: ImageRect, width: u32) -> Vec<f32> {
    let mut response = (rect.y..rect.y + rect.h)
        .flat_map(|y| {
            let start = (y * width + rect.x) as usize;
            image.pixels()[start..start + rect.w as usize]
                .iter()
                .flatten()
        })
        .copied()
        .collect::<Vec<_>>();
    let norm = response
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt()
        .max(f32::EPSILON);
    response.iter_mut().for_each(|value| *value /= norm);
    response
}

pub(crate) fn crop_icon_sample(
    kind: ItemKind,
    image: &RgbaImage,
    slot: &Slot,
    physical_size: f32,
) -> Result<ImageSample> {
    match kind {
        ItemKind::Stratagem => crop_slot_sample(image, slot, physical_size),
        ItemKind::Booster => booster::crop_sample(image, slot, physical_size),
    }
}

pub(crate) fn icon_alignment_response(kind: ItemKind, sample: &ImageSample) -> Result<Vec<u8>> {
    match kind {
        ItemKind::Stratagem => Ok(sample
            .image
            .pixels()
            .map(|pixel| stratagem_foreground_response(pixel[0], pixel[1], pixel[2]))
            .collect()),
        ItemKind::Booster => booster::glyph_response(sample),
    }
}

/// Rebuild Stratagem display colors over their fixed gray base; Boosters retain native colors.
pub(crate) fn render_icon(icon: &IconSample) -> Result<RgbaImage> {
    let sample = &icon.sample;
    let rect = texture_rect(sample)?;
    let width = sample.image.width();
    let mut image =
        image::imageops::crop_imm(&sample.image, rect.x, rect.y, rect.w, rect.h).to_image();
    if let Some(category) = icon.category {
        let color = fixed_class_endpoint(category);
        let semantic = icon.semantic.pixels();
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let [class, white] = semantic[((rect.y + y) * width + rect.x + x) as usize];
            let background = 1.0 - class - white;
            let rgb: [u8; 3] = std::array::from_fn(|channel| {
                let value = background * FIXED_BACKGROUND[channel]
                    + class * color[channel]
                    + white * FIXED_WHITE[channel];
                (value * 255.0).round() as u8
            });
            *pixel = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    Ok(image)
}

fn texture_rect(sample: &ImageSample) -> Result<ImageRect> {
    let (width, height) = sample.image.dimensions();
    let geometry = sample.geometry;
    ensure!(
        width > 0
            && height > 0
            && geometry.physical_size.is_finite()
            && geometry.physical_size > 0.0
            && geometry.center_x.is_finite()
            && geometry.center_y.is_finite()
            && geometry.center_x >= 0.0
            && geometry.center_x <= width as f32
            && geometry.center_y >= 0.0
            && geometry.center_y <= height as f32,
        "icon sample has invalid geometry"
    );
    let side = (geometry.physical_size.round_ties_even().max(1.0) as u32)
        .min(width)
        .min(height);
    let x = (geometry.center_x - side as f32 * 0.5)
        .round_ties_even()
        .clamp(0.0, (width - side) as f32) as u32;
    let y = (geometry.center_y - side as f32 * 0.5)
        .round_ties_even()
        .clamp(0.0, (height - side) as f32) as u32;
    Ok(ImageRect {
        x,
        y,
        w: side,
        h: side,
    })
}
