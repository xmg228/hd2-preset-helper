//! Read-only presentation data. No window, UI framework or current game state required.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};

use super::{PresetFile, load_preset_file, load_template_image, stratagems};
use crate::{
    item::EquipmentKind,
    vision::{equipment::EquipmentText, text::TextSample},
};

pub struct PreviewImage {
    pub label: String,
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
                    .map(|(index, item)| (format!("Stratagem {}", index + 1), item))
                    .chain(preset.booster.iter().map(|item| ("Booster".into(), item)))
                    .chain(
                        preset
                            .fallback_booster
                            .iter()
                            .map(|item| ("Fallback Booster".into(), item)),
                    )
                    .map(|(label, item)| {
                        Ok(PreviewImage {
                            label,
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
                            label: format!("{kind:?}"),
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
