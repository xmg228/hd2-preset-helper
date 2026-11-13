//! Preset content and organization, independent of global application settings.
pub(crate) mod equipment;
mod images;
mod migration;
mod model;
pub(crate) mod preview;
mod refresh;
mod storage;
pub(crate) mod stratagems;

use crate::item_cache::{CachedChoices, IconChoice, ItemCache};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub(crate) use images::{load_template_image, resolve_template_path};
pub(crate) use migration::initialize;
use model::{Entry, Preset, PresetLibrary};
pub(crate) use model::{
    EntryMetadata, LibraryMetadata, LocalTemplate, PresetMetadata, StratagemPreset,
};
pub(crate) use refresh::{
    MigrationGroupKey, MigrationGroupSummary, MigrationPair, MigrationReport, MigrationSelection,
    MigrationStatus, PresetMigration,
};
const LOCAL_TEMPLATES_DIR: &str = "local_templates";

#[derive(Clone, Copy)]
pub enum PresetSection {
    Stratagems,
    Equipment,
}

/// Sections to save or apply. Unchecked sections are left unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetScope {
    pub stratagems: bool,
    pub equipment: bool,
    pub equipment_items: crate::item::EquipmentSelection,
}

impl Default for PresetScope {
    fn default() -> Self {
        Self {
            stratagems: true,
            equipment: true,
            equipment_items: Default::default(),
        }
    }
}

impl PresetScope {
    pub fn includes_equipment(self) -> bool {
        self.equipment && !self.equipment_items.is_empty()
    }
}

pub(crate) fn library_metadata(path: &Path) -> Result<LibraryMetadata> {
    Ok(storage::load(path)?.library_metadata())
}

pub(crate) fn add_folder(path: &Path) -> Result<(String, LibraryMetadata)> {
    storage::edit(path, |library| {
        let id = library.add_folder()?;
        Ok((id, library.library_metadata()))
    })
}

pub(crate) fn rename_folder(path: &Path, id: &str, label: &str) -> Result<LibraryMetadata> {
    storage::edit(path, |library| {
        library.rename_folder(id, label)?;
        Ok(library.library_metadata())
    })
}

pub(crate) fn toggle_folder(path: &Path, id: &str) -> Result<bool> {
    storage::edit(path, |library| library.toggle_folder(id))
}

pub(crate) fn ungroup_folder(path: &Path, id: &str) -> Result<LibraryMetadata> {
    storage::edit(path, |library| {
        library.ungroup_folder(id)?;
        Ok(library.library_metadata())
    })
}

pub(crate) fn move_entry(
    path: &Path,
    id: &str,
    parent: Option<&str>,
    before: Option<&str>,
) -> Result<LibraryMetadata> {
    storage::edit(path, |library| {
        library.move_entry(id, parent, before)?;
        Ok(library.library_metadata())
    })
}

pub(crate) fn validate_id(id: &str) -> Result<()> {
    model::validate_id(id, "preset_")
}

/// Commit an edited loadout and its images together, keeping untouched slots intact.
pub(crate) fn save_cached(
    path: &Path,
    id: &str,
    cache: &ItemCache,
    choices: &CachedChoices,
) -> Result<()> {
    validate_id(id)?;
    storage::edit_images(path, id, "", |record, images| {
        if choices
            .stratagems
            .iter()
            .any(|choice| *choice != IconChoice::Keep)
        {
            stratagems::write_cached(path, id, cache, &choices.stratagems, record, images)?;
        }
        equipment::write_cached(record, images, cache, &choices.equipment)
    })
}

pub fn saved_sections(
    path: &Path,
    id: &str,
    selection: crate::item::EquipmentSelection,
) -> Result<(bool, bool)> {
    let library = storage::load(path)?;
    let preset = library.get(id)?;
    Ok((
        preset.stratagems.is_some(),
        preset.equipment.as_ref().is_some_and(|equipment| {
            selection
                .iter()
                .any(|kind| equipment.items[kind.index()].is_some())
        }),
    ))
}

/// Count selected, saved item types that do not yet have a complete list.
pub(crate) fn uncached_type_count(
    path: &Path,
    id: &str,
    scope: PresetScope,
    cache: &ItemCache,
) -> Result<usize> {
    let library = storage::load(path)?;
    let preset = library.get(id)?;
    let mut count = 0;
    if scope.stratagems
        && let Some(stratagems) = &preset.stratagems
    {
        count += usize::from(cache.stratagems.is_none());
        count += usize::from(stratagems.booster.is_some() && cache.boosters.is_none());
    }
    if scope.equipment
        && let Some(equipment) = &preset.equipment
    {
        count += scope
            .equipment_items
            .iter()
            .filter(|kind| {
                equipment.items[kind.index()].is_some() && cache.equipment[kind.index()].is_none()
            })
            .count();
    }
    Ok(count)
}

pub(crate) fn add(path: &Path) -> Result<(String, LibraryMetadata)> {
    storage::edit(path, |library| {
        let id = next_id(path, library)?;
        library.entries.push(Entry::Preset(Box::new(Preset {
            id: id.clone(),
            ..Default::default()
        })));
        Ok((id, library.library_metadata()))
    })
}

/// Insert an independent copy beside its source, publishing only after all images exist.
pub(crate) fn duplicate(path: &Path, source_id: &str) -> Result<(String, LibraryMetadata)> {
    storage::locked(path, |library| {
        let mut preset = library.get(source_id)?.clone();
        preset.id = next_id(path, library)?;
        preset.label = duplicate_label(&preset.label, library);
        preset.shortcut.clear();
        let mut created = Vec::new();
        if let Some(stratagems) = &mut preset.stratagems {
            let mut images = images::NewImages::new(path, &preset.id, "stratagems")?;
            for item in stratagems
                .stratagems
                .iter_mut()
                .chain(stratagems.booster.iter_mut())
                .chain(stratagems.fallback_booster.iter_mut())
            {
                item.path = images.copy("stratagem", path, &item.path)?;
            }
            created.push(images);
        }
        if let Some(equipment) = &mut preset.equipment {
            let mut images = images::NewImages::new(path, &preset.id, "equipment")?;
            for item in equipment.items.iter_mut().flatten() {
                item.name = images.copy("name", path, &item.name)?;
                if let Some(category) = &mut item.category {
                    *category = images.copy("category", path, category)?;
                }
            }
            created.push(images);
        }
        let id = preset.id.clone();
        library.insert_after(source_id, preset)?;
        storage::write(path, library)?;
        for images in created {
            images.commit();
        }
        Ok((id, library.library_metadata()))
    })
}

fn next_id(path: &Path, library: &PresetLibrary) -> Result<String> {
    let mut number = library
        .presets()
        .filter_map(|preset| model::ordinal(&preset.id, "preset_"))
        .max()
        .unwrap_or(0);
    loop {
        number = number.checked_add(1).context("preset ID space exhausted")?;
        let id = format!("preset_{number}");
        let directory = resolve_template_path(path, &format!("{LOCAL_TEMPLATES_DIR}/{id}"))?;
        match fs::symlink_metadata(directory) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(id),
            Err(error) => return Err(error).context("failed to check preset template directory"),
        }
    }
}

fn duplicate_label(label: &str, library: &PresetLibrary) -> String {
    let label = label.trim();
    if label.is_empty() {
        return String::new();
    }
    let labels: BTreeSet<_> = library
        .presets()
        .map(|preset| preset.label.trim())
        .collect();
    let base = label
        .rsplit_once(" (")
        .and_then(|(base, suffix)| {
            let suffix = suffix.strip_suffix(')')?;
            let number = suffix.parse::<usize>().ok()?;
            (number >= 2
                && suffix == number.to_string()
                && !base.is_empty()
                && labels.contains(base))
            .then_some(base)
        })
        .unwrap_or(label);
    for number in 2usize.. {
        let candidate = format!("{base} ({number})");
        if !labels.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!("a finite library cannot occupy every numeric suffix")
}

pub(crate) fn rename(path: &Path, id: &str, label: &str) -> Result<()> {
    model::validate_label(label)?;
    storage::edit(path, |library| {
        library.get_mut(id)?.label = label.trim().into();
        Ok(())
    })
}

/// Keep the writer lock until the caller has published the updated document.
pub(crate) fn edit_shortcuts(
    path: &Path,
    bindings: &BTreeMap<String, String>,
    publish: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<()> {
    storage::locked(path, |library| {
        library.set_shortcuts(bindings)?;
        publish(&storage::encode(library)?)
    })
}

/// Commit removal before reclaiming resources. Cleanup failure must not resurrect a preset.
pub(crate) fn remove(path: &Path, id: &str) -> Result<LibraryMetadata> {
    storage::locked(path, |library| {
        let directory = images::owned_directory(path, id)?;
        library.remove(id)?;
        storage::write(path, library)?;
        let relative = Path::new(LOCAL_TEMPLATES_DIR).join(id);
        if library
            .image_paths()
            .iter()
            .any(|p| Path::new(p).starts_with(&relative))
        {
            tracing::warn!(%id, "removed preset still has shared template references; files retained");
        } else if let Err(error) = fs::remove_dir_all(directory)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, %error, "preset removed; template cleanup failed");
        }
        Ok(library.library_metadata())
    })
}
