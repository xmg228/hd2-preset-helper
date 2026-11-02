//! Captured equipment names and their persistence, independent of automation.
use std::path::Path;

use super::{
    load_template_image,
    model::{SavedEquipment, SavedItem},
    storage,
};
use crate::{
    item::EquipmentKind,
    vision::{
        equipment::{EquipmentObserver, EquipmentText},
        text::TextSample,
    },
};
use anyhow::{Context, Result, ensure};

pub(crate) struct EquipmentItem {
    pub name: TextSample,
    pub category: Option<TextSample>,
    pub client_size: (u32, u32),
}

pub(crate) type EquipmentSet = [Option<EquipmentItem>; 6];

pub(crate) fn save(path: &Path, name: &str, items: &EquipmentSet) -> Result<()> {
    super::validate_id(name)?;
    ensure!(
        items.iter().all(Option::is_some),
        "all six equipment items must be sampled before saving"
    );
    let client_size = items[0].as_ref().unwrap().client_size;
    storage::edit_images(path, name, "equipment", |record, images| {
        let mut saved = Vec::with_capacity(6);
        for (index, item) in items.iter().enumerate() {
            let item = item.as_ref().unwrap();
            saved.push(SavedItem {
                name: images.save(&format!("{}-name", index + 1), &item.name.image)?,
                category: item
                    .category
                    .as_ref()
                    .map(|category| {
                        images.save(&format!("{}-category", index + 1), &category.image)
                    })
                    .transpose()?,
                category_client_size: None,
            });
        }
        record.equipment = Some(SavedEquipment {
            client_size,
            items: saved.try_into().expect("six captured items"),
        });
        Ok(())
    })
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
        let category_client_size = (client_size != saved.client_size).then_some(client_size);
        let item = &mut saved.items[kind.index()];
        item.category =
            Some(images.save(&format!("{}-category", kind.index() + 1), &category.image)?);
        item.category_client_size = category_client_size;
        Ok(())
    })
}

pub(crate) fn load(path: &Path, name: &str) -> Result<EquipmentSet> {
    let file = storage::load(path)?;
    let saved = file.get(name)?.equipment.as_ref().with_context(|| {
        format!("equipment preset {name} has not been saved; use Save preset in the panel first")
    })?;
    let mut items = std::array::from_fn(|_| None);
    for (item, source) in items.iter_mut().zip(&saved.items) {
        *item = Some(EquipmentItem {
            name: load_text(path, &source.name, saved.client_size, EquipmentText::Name)?,
            category: source
                .category
                .as_deref()
                .map(|relative| {
                    load_text(
                        path,
                        relative,
                        source.category_client_size.unwrap_or(saved.client_size),
                        EquipmentText::Category,
                    )
                })
                .transpose()?,
            client_size: saved.client_size,
        });
    }
    Ok(items)
}

pub(super) fn load_text(
    path: &Path,
    relative: &str,
    client_size: (u32, u32),
    field: EquipmentText,
) -> Result<TextSample> {
    let (observer, _) =
        EquipmentObserver::resolve(client_size.0, client_size.1, EquipmentKind::Helmet)?;
    let (rect, geometry) = observer.text_region(field);
    let image = load_template_image(path, relative)?;
    ensure!(
        image.dimensions() == (rect.w, rect.h),
        "equipment template {relative} does not match its saved capture geometry; save it again"
    );
    Ok(TextSample::extract(image, geometry))
}
