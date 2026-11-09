//! Captured equipment names and their persistence, independent of automation.
use std::path::Path;

use super::{
    Preset,
    images::NewImages,
    load_template_image,
    model::{SavedEquipment, SavedItem},
    storage,
};
use crate::{
    item::{EquipmentKind, EquipmentSelection},
    item_cache::{EquipmentChoice, EquipmentItem, EquipmentSet, ItemCache},
    vision::{
        equipment::{
            CapturedEquipmentItem, CapturedEquipmentSet, EquipmentObserver, EquipmentText,
        },
        text::{TextSample, TextTemplate},
    },
};
use anyhow::{Context, Result, ensure};

pub(crate) fn save(path: &Path, name: &str, items: &CapturedEquipmentSet) -> Result<()> {
    super::validate_id(name)?;
    storage::edit_images(path, name, "equipment", |record, images| {
        write_items(record, images, items)
    })
}

fn write_items(
    record: &mut Preset,
    images: &mut NewImages,
    items: &CapturedEquipmentSet,
) -> Result<()> {
    let client_size = items
        .iter()
        .flatten()
        .next()
        .context("no equipment items were sampled")?
        .client_size;
    let saved = record.equipment.get_or_insert_with(|| SavedEquipment {
        client_size,
        items: std::array::from_fn(|_| None),
    });
    if items.iter().all(Option::is_some) {
        saved.client_size = client_size;
    }
    for (index, item) in items.iter().enumerate() {
        let Some(item) = item else {
            continue;
        };
        saved.items[index] = Some(SavedItem {
            name: images.save(&format!("{}-name", index + 1), &item.name.image)?,
            name_client_size: (item.client_size != saved.client_size).then_some(item.client_size),
            category: item
                .category
                .as_ref()
                .map(|category| images.save(&format!("{}-category", index + 1), &category.image))
                .transpose()?,
            category_client_size: None,
        });
    }
    Ok(())
}

/// Copy selected catalog samples into a preset, keeping all other items unchanged.
pub(super) fn write_cached(
    record: &mut Preset,
    images: &mut NewImages,
    cache: &ItemCache,
    choices: &[Option<EquipmentChoice>; 6],
) -> Result<()> {
    let mut items: CapturedEquipmentSet = std::array::from_fn(|_| None);
    for kind in EquipmentKind::ALL {
        let Some(choice) = choices[kind.index()] else {
            continue;
        };
        let catalog = cache.equipment[kind.index()]
            .as_ref()
            .with_context(|| format!("{} catalog is not available", kind.label()))?;
        let category = catalog
            .categories
            .get(choice.category)
            .context("selected equipment category is not available")?;
        let sample = category
            .names
            .get(choice.name)
            .filter(|_| category.order.contains(&choice.name))
            .context("selected equipment item is not available")?;
        items[kind.index()] = Some(CapturedEquipmentItem {
            name: sample.clone(),
            category: Some(category.title.clone()),
            client_size: catalog.client_size,
        });
    }
    if items.iter().all(Option::is_none) {
        return Ok(());
    }
    write_items(record, images, &items)
}

/// Repair only the category hint of a successfully applied item; keep its name template.
pub(crate) fn update_category(
    path: &Path,
    name: &str,
    kind: EquipmentKind,
    category: &TextSample,
    client_size: (u32, u32),
) -> Result<()> {
    storage::edit_images(path, name, "equipment", |record, images| {
        let saved = record
            .equipment
            .as_mut()
            .with_context(|| format!("equipment preset not found: {name}"))?;
        let item = saved.items[kind.index()]
            .as_mut()
            .context("equipment item has not been saved")?;
        let category_client_size = (client_size
            != item.name_client_size.unwrap_or(saved.client_size))
        .then_some(client_size);
        item.category =
            Some(images.save(&format!("{}-category", kind.index() + 1), &category.image)?);
        item.category_client_size = category_client_size;
        Ok(())
    })
}

pub(crate) fn load(path: &Path, name: &str, selection: EquipmentSelection) -> Result<EquipmentSet> {
    let file = storage::load(path)?;
    let saved = file
        .get(name)?
        .equipment
        .as_ref()
        .with_context(|| format!("equipment preset {name} has not been saved"))?;
    let mut items = std::array::from_fn(|_| None);
    for kind in selection.iter() {
        let Some(source) = &saved.items[kind.index()] else {
            continue;
        };
        let client_size = source.name_client_size.unwrap_or(saved.client_size);
        items[kind.index()] = Some(EquipmentItem {
            name: load_text(path, &source.name, client_size, EquipmentText::Name)?,
            category: source
                .category
                .as_deref()
                .map(|relative| {
                    load_text(
                        path,
                        relative,
                        source.category_client_size.unwrap_or(client_size),
                        EquipmentText::Category,
                    )
                })
                .transpose()?,
            client_size,
        });
    }
    Ok(items)
}

pub(super) fn load_text(
    path: &Path,
    relative: &str,
    client_size: (u32, u32),
    field: EquipmentText,
) -> Result<TextTemplate> {
    let (observer, _) =
        EquipmentObserver::resolve(client_size.0, client_size.1, EquipmentKind::Helmet)?;
    let (rect, geometry) = observer.text_region(field);
    let image = load_template_image(path, relative)?;
    ensure!(
        image.dimensions() == (rect.w, rect.h),
        "equipment template {relative} does not match its saved capture geometry; save it again"
    );
    let sample = TextSample::extract(image, geometry);
    Ok(match field {
        EquipmentText::Name => sample.retain_left_group(),
        EquipmentText::Category => sample,
    }
    .into_template())
}
