//! Read-only candidates for editing presets from the local catalog.
use anyhow::{Context, Result};
use image::RgbaImage;

use super::{EquipmentChoice, ItemCache};
use crate::{
    item::StratagemCategory,
    vision::{
        render_icon,
        text::{render_category, render_text},
    },
};

pub(crate) struct EquipmentCandidate {
    pub choice: EquipmentChoice,
    pub picture: RgbaImage,
}

pub(crate) struct EquipmentCategoryPreview {
    pub title: RgbaImage,
    pub items: Vec<EquipmentCandidate>,
}

pub(crate) type EquipmentCandidates = [Vec<EquipmentCategoryPreview>; 6];

pub(crate) struct IconCandidate {
    pub index: usize,
    pub picture: RgbaImage,
}

pub(crate) struct IconCandidates {
    pub stratagems: [Vec<IconCandidate>; 3],
    pub boosters: Vec<IconCandidate>,
}

pub(crate) struct ItemCandidates {
    pub icons: IconCandidates,
    pub equipment: EquipmentCandidates,
    pub missing: bool,
}

pub(crate) fn item_candidates(cache: &ItemCache) -> Result<ItemCandidates> {
    Ok(ItemCandidates {
        icons: icon_candidates(cache)?,
        equipment: equipment_candidates(cache)?,
        missing: cache.stratagems.is_none()
            || cache.boosters.is_none()
            || cache.equipment.iter().any(Option::is_none),
    })
}

fn equipment_candidates(cache: &ItemCache) -> Result<EquipmentCandidates> {
    let mut kinds: EquipmentCandidates = std::array::from_fn(|_| Vec::new());
    for (index, catalog) in cache.equipment.iter().enumerate() {
        let Some(catalog) = catalog else {
            continue;
        };
        kinds[index] = catalog
            .categories
            .iter()
            .enumerate()
            .map(|(category_index, category)| {
                let mut seen = vec![false; category.names.len()];
                let mut items = Vec::new();
                for &name in &category.order {
                    let seen = seen
                        .get_mut(name)
                        .context("cached equipment order refers to a missing name")?;
                    if *seen {
                        continue;
                    }
                    *seen = true;
                    items.push(EquipmentCandidate {
                        choice: EquipmentChoice {
                            category: category_index,
                            name,
                        },
                        picture: render_text(&category.names[name]),
                    });
                }
                Ok(EquipmentCategoryPreview {
                    title: render_category(&category.title),
                    items,
                })
            })
            .collect::<Result<_>>()?;
    }
    Ok(kinds)
}

fn icon_candidates(cache: &ItemCache) -> Result<IconCandidates> {
    let mut stratagems: [Vec<IconCandidate>; 3] = std::array::from_fn(|_| Vec::new());
    if let Some(catalog) = &cache.stratagems {
        for (index, item) in catalog.items.iter().enumerate() {
            let icon = &item.icon;
            let group = match icon.category.context("cached Stratagem has no category")? {
                StratagemCategory::Offensive => 0,
                StratagemCategory::Supply => 1,
                StratagemCategory::Defensive => 2,
            };
            stratagems[group].push(IconCandidate {
                index,
                picture: render_icon(icon)?,
            });
        }
    }
    let boosters = cache
        .boosters
        .iter()
        .flat_map(|catalog| &catalog.items)
        .enumerate()
        .map(|(index, item)| {
            Ok(IconCandidate {
                index,
                picture: render_icon(&item.icon)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(IconCandidates {
        stratagems,
        boosters,
    })
}
