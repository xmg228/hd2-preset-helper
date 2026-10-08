//! Read-only presentation data. No window, UI framework or current game state required.
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};

use super::{PresetFile, load_preset_file, load_template_image, stratagems};
use crate::{
    item::EquipmentKind,
    vision::{equipment::EquipmentText, text::TextSample},
};

#[derive(Clone, Copy, Debug)]
pub enum PreviewRole {
    Stratagem(usize),
    Booster,
    FallbackBooster,
    Equipment(EquipmentKind),
}

pub struct PreviewImage {
    pub role: PreviewRole,
    pub image: RgbaImage,
}

pub enum PreviewContent {
    Missing,
    Ready(Vec<PreviewImage>),
    Invalid(String),
}

pub struct PresetPreview {
    pub stratagems: PreviewContent,
    pub equipment: PreviewContent,
}

pub fn load(path: &Path) -> Result<BTreeMap<String, PresetPreview>> {
    let file = if path.exists() {
        load_preset_file(path)?
    } else {
        PresetFile::default()
    };
    let names: BTreeSet<_> = file.presets.keys().chain(file.equipment.keys()).collect();
    Ok(names
        .into_iter()
        .map(|name| {
            let stratagems = content(file.presets.get(name).map(|preset| {
                stratagems::validate(name, preset)?;
                preset
                    .stratagems
                    .iter()
                    .enumerate()
                    .map(|(index, item)| (PreviewRole::Stratagem(index + 1), item))
                    .chain(
                        preset
                            .booster
                            .iter()
                            .map(|item| (PreviewRole::Booster, item)),
                    )
                    .chain(
                        preset
                            .fallback_booster
                            .iter()
                            .map(|item| (PreviewRole::FallbackBooster, item)),
                    )
                    .map(|(role, item)| {
                        Ok(PreviewImage {
                            role,
                            image: load_template_image(path, &item.path)?,
                        })
                    })
                    .collect()
            }));
            let equipment = content(file.equipment.get(name).map(|equipment| {
                EquipmentKind::ALL
                    .iter()
                    .zip(&equipment.items)
                    .map(|(kind, item)| {
                        Ok(PreviewImage {
                            role: PreviewRole::Equipment(*kind),
                            image: load_name(path, &item.name, equipment.client_size)?,
                        })
                    })
                    .collect()
            }));
            (
                name.clone(),
                PresetPreview {
                    stratagems,
                    equipment,
                },
            )
        })
        .collect())
}

pub fn equipment_name(path: &Path, preset: &str, kind: EquipmentKind) -> Result<RgbaImage> {
    let file = load_preset_file(path)?;
    let equipment = file
        .equipment
        .get(preset)
        .context("equipment preset not saved")?;
    load_name(
        path,
        &equipment.items[kind.index()].name,
        equipment.client_size,
    )
}

fn load_name(path: &Path, name: &str, client_size: (u32, u32)) -> Result<RgbaImage> {
    Ok(name_preview(&super::equipment::load_text(
        path,
        name,
        client_size,
        EquipmentText::Name,
    )?))
}

fn content(result: Option<Result<Vec<PreviewImage>>>) -> PreviewContent {
    match result {
        None => PreviewContent::Missing,
        Some(Ok(images)) => PreviewContent::Ready(images),
        Some(Err(error)) => PreviewContent::Invalid(format!("{error:#}")),
    }
}

fn name_preview(sample: &TextSample) -> RgbaImage {
    let foreground = sample.foreground();
    let (width, height) = foreground.dimensions();
    // Keep the captured extent; presentation scales by height and clips overflow.
    // Use response as alpha only, so strokes are not dimmed twice.
    RgbaImage::from_fn(width, height, |x, y| {
        Rgba([255, 255, 255, foreground.get_pixel(x, y)[0]])
    })
}

/// Display-only crop of a centered heading. Identity keeps the complete text region.
pub(crate) fn equipment_category(sample: &TextSample) -> RgbaImage {
    let image = name_preview(sample);
    let (width, height) = image.dimensions();
    let mut projection = vec![0u32; width as usize];
    for (x, _, pixel) in image.enumerate_pixels() {
        projection[x as usize] += pixel[3] as u32;
    }
    let threshold = projection.iter().copied().max().unwrap_or(0) as f32 * 0.03;
    let columns = projection
        .iter()
        .enumerate()
        .filter_map(|(x, &value)| (value as f32 > threshold).then_some(x))
        .collect::<Vec<_>>();
    let gap = (height as f32 * 0.5).round_ties_even() as usize;
    let center = (width / 2) as usize;
    let group = columns
        .chunk_by(|a, b| b - a - 1 <= gap)
        .min_by_key(|group| {
            let distance = group[0]
                .saturating_sub(center)
                .max(center.saturating_sub(group[group.len() - 1]));
            (
                distance,
                Reverse(group.iter().map(|&x| projection[x]).sum::<u32>()),
            )
        });
    let Some(group) = group else {
        return RgbaImage::new(0, 0);
    };
    let padding = (height as f32 * 0.1).round_ties_even().max(2.0) as u32;
    let left = (group[0] as u32).saturating_sub(padding);
    let right = (group[group.len() - 1] as u32 + 1 + padding).min(width);
    image::imageops::crop_imm(&image, left, 0, right - left, height).to_image()
}
