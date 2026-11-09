//! Read-only presentation data. No window, UI framework or current game state required.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use image::RgbaImage;

use super::{LocalTemplate, storage, stratagems};
use crate::{
    item::{EquipmentKind, ItemKind},
    vision::{IconSample, equipment::EquipmentText, render_icon},
};

#[derive(Clone, Copy, Debug)]
pub enum PreviewRole {
    Stratagem(usize),
    Booster,
    FallbackBooster,
}

pub struct PreviewImage {
    pub role: PreviewRole,
    pub image: RgbaImage,
}

#[derive(Default)]
pub enum PreviewContent<T> {
    #[default]
    Missing,
    Ready(T),
    Invalid(String),
}

pub struct PresetPreview {
    pub stratagems: PreviewContent<Vec<PreviewImage>>,
    pub equipment: [PreviewContent<RgbaImage>; 6],
}

pub(crate) fn load(path: &Path) -> Result<BTreeMap<String, PresetPreview>> {
    let file = storage::load(path)?;
    Ok(file
        .into_presets()
        .map(|record| {
            let id = record.id;
            let stratagems = content(record.stratagems.as_ref().map(|preset| {
                stratagems::validate(&id, preset)?;
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
                        let kind = match role {
                            PreviewRole::Stratagem(_) => ItemKind::Stratagem,
                            PreviewRole::Booster | PreviewRole::FallbackBooster => {
                                ItemKind::Booster
                            }
                        };
                        Ok(PreviewImage {
                            role,
                            image: icon_preview(path, item, kind)?,
                        })
                    })
                    .collect()
            }));
            let equipment = std::array::from_fn(|index| {
                let Some(equipment) = record.equipment.as_ref() else {
                    return PreviewContent::Missing;
                };
                let Some(item) = equipment.items[index].as_ref() else {
                    return PreviewContent::Missing;
                };
                match super::equipment::load_text(
                    path,
                    &item.name,
                    item.name_client_size.unwrap_or(equipment.client_size),
                    EquipmentText::Name,
                ) {
                    Ok(sample) => PreviewContent::Ready(name_preview(&sample)),
                    Err(error) => PreviewContent::Invalid(format!("{error:#}")),
                }
            });
            (
                id,
                PresetPreview {
                    stratagems,
                    equipment,
                },
            )
        })
        .collect())
}

pub fn equipment_name(path: &Path, preset: &str, kind: EquipmentKind) -> Result<RgbaImage> {
    let file = storage::load(path)?;
    let equipment = file
        .get(preset)?
        .equipment
        .as_ref()
        .context("equipment preset not saved")?;
    let item = equipment.items[kind.index()]
        .as_ref()
        .context("equipment item has not been saved")?;
    load_name(
        path,
        &item.name,
        item.name_client_size.unwrap_or(equipment.client_size),
    )
}

pub fn stratagem_icon(path: &Path, preset: &str, item: &str) -> Result<RgbaImage> {
    let preset = stratagems::load(path, preset)?;
    let (kind, template) = preset
        .stratagems
        .iter()
        .map(|template| (ItemKind::Stratagem, template))
        .chain(
            preset
                .booster
                .iter()
                .chain(preset.fallback_booster.iter())
                .map(|template| (ItemKind::Booster, template)),
        )
        .find(|(_, template)| template.path == item)
        .context("icon template was not found in this preset")?;
    icon_preview(path, template, kind)
}

fn icon_preview(path: &Path, template: &LocalTemplate, kind: ItemKind) -> Result<RgbaImage> {
    let sample = IconSample::extract(kind, stratagems::load_template_sample(path, template)?)?;
    render_icon(&sample)
}

fn load_name(path: &Path, name: &str, client_size: (u32, u32)) -> Result<RgbaImage> {
    Ok(name_preview(&super::equipment::load_text(
        path,
        name,
        client_size,
        EquipmentText::Name,
    )?))
}

fn content(result: Option<Result<Vec<PreviewImage>>>) -> PreviewContent<Vec<PreviewImage>> {
    match result {
        None => PreviewContent::Missing,
        Some(Ok(images)) => PreviewContent::Ready(images),
        Some(Err(error)) => PreviewContent::Invalid(format!("{error:#}")),
    }
}

pub(crate) use crate::vision::text::{
    render_category as equipment_category, render_text as name_preview,
};
