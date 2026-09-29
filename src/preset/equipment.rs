//! Captured equipment names and their persistence, independent of automation.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{
    LOCAL_TEMPLATES_DIR, PresetFile, load_preset_file, load_template_image, save_template_image,
    validate_preset_name, write_preset_file,
};
use crate::{
    item::EquipmentKind,
    vision::{
        equipment::{EquipmentObserver, EquipmentText},
        text::TextSample,
    },
};

pub(crate) struct EquipmentItem {
    pub name: TextSample,
    pub category: Option<TextSample>,
    pub client_size: (u32, u32),
}

pub(crate) type EquipmentSet = [Option<EquipmentItem>; 6];

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct SavedEquipment {
    pub(super) client_size: (u32, u32),
    // Same order as the six entry cards: helmet, armor, cape, primary, secondary, throwable.
    pub(super) items: [SavedItem; 6],
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct SavedItem {
    pub(super) name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    /// A repaired category can be captured at a different resolution from the names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category_client_size: Option<(u32, u32)>,
}

pub(crate) fn save(path: &Path, name: &str, items: &EquipmentSet) -> Result<()> {
    validate_preset_name(name)?;
    ensure!(
        items.iter().all(Option::is_some),
        "all six equipment items must be sampled before saving"
    );
    let client_size = items[0].as_ref().unwrap().client_size;
    let mut file = if path.exists() {
        load_preset_file(path)?
    } else {
        PresetFile::default()
    };
    let saved = std::array::from_fn(|index| SavedItem {
        name: format!(
            "{LOCAL_TEMPLATES_DIR}/{name}/equipment/{}-name.png",
            index + 1
        ),
        category: items[index].as_ref().unwrap().category.as_ref().map(|_| {
            format!(
                "{LOCAL_TEMPLATES_DIR}/{name}/equipment/{}-category.png",
                index + 1
            )
        }),
        category_client_size: None,
    });
    for (item, target) in items.iter().zip(&saved) {
        let item = item.as_ref().unwrap();
        save_template_image(path, &target.name, &item.name.image)?;
        if let Some((category, relative)) = item.category.as_ref().zip(target.category.as_ref()) {
            save_template_image(path, relative, &category.image)?;
        }
    }
    file.equipment.insert(
        name.to_owned(),
        SavedEquipment {
            client_size,
            items: saved,
        },
    );
    write_preset_file(path, &file)
}

/// Repair only the category hint of a successfully applied item; keep its name template.
pub(crate) fn update_category(
    path: &Path,
    name: &str,
    kind: EquipmentKind,
    category: &TextSample,
    client_size: (u32, u32),
) -> Result<()> {
    validate_preset_name(name)?;
    let mut file = load_preset_file(path)?;
    let saved = file
        .equipment
        .get_mut(name)
        .with_context(|| format!("equipment preset not found: {name}"))?;
    let category_client_size = (client_size != saved.client_size).then_some(client_size);
    let item = &mut saved.items[kind.index()];
    let relative = item.category.clone().unwrap_or_else(|| {
        format!(
            "{LOCAL_TEMPLATES_DIR}/{name}/equipment/{}-category.png",
            kind.index() + 1
        )
    });
    save_template_image(path, &relative, &category.image)?;
    if item.category.is_none() || item.category_client_size != category_client_size {
        item.category = Some(relative);
        item.category_client_size = category_client_size;
        write_preset_file(path, &file)?;
    }
    Ok(())
}

pub(crate) fn load(path: &Path, name: &str) -> Result<EquipmentSet> {
    let file = if path.exists() {
        load_preset_file(path)?
    } else {
        PresetFile::default()
    };
    let saved = file.equipment.get(name).with_context(|| {
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
