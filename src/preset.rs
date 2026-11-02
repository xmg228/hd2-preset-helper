//! Preset content and organization, independent of global application settings.
pub(crate) mod equipment;
mod images;
mod migration;
mod model;
pub(crate) mod preview;
mod storage;
pub(crate) mod stratagems;

use anyhow::{Context, Result};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::Path};

pub(crate) use images::{load_template_image, resolve_template_path};
pub(crate) use migration::initialize;
use model::{Entry, Preset, PresetLibrary};
pub(crate) use model::{LocalTemplate, PresetMetadata, StratagemPreset};
const LOCAL_TEMPLATES_DIR: &str = "local_templates";

/// Sections to save or apply. Unchecked sections are left unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetScope {
    pub stratagems: bool,
    pub equipment: bool,
}

impl Default for PresetScope {
    fn default() -> Self {
        Self {
            stratagems: true,
            equipment: true,
        }
    }
}

pub(crate) fn metadata(path: &Path) -> Result<Vec<PresetMetadata>> {
    Ok(storage::load(path)?.metadata())
}

pub(crate) fn validate_id(id: &str) -> Result<()> {
    model::validate_id(id, "preset_")
}

pub fn saved_sections(path: &Path, id: &str) -> Result<(bool, bool)> {
    let library = storage::load(path)?;
    let preset = library.get(id)?;
    Ok((preset.stratagems.is_some(), preset.equipment.is_some()))
}

pub(crate) fn add(path: &Path) -> Result<PresetMetadata> {
    storage::edit(path, |library| {
        let mut number = library
            .presets()
            .filter_map(|preset| model::ordinal(&preset.id, "preset_"))
            .max()
            .unwrap_or(0);
        let id = loop {
            number = number.checked_add(1).context("preset ID space exhausted")?;
            let id = format!("preset_{number}");
            let directory = resolve_template_path(path, &format!("{LOCAL_TEMPLATES_DIR}/{id}"))?;
            match fs::symlink_metadata(directory) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break id,
                Err(error) => {
                    return Err(error).context("failed to check preset template directory");
                }
            }
        };
        library.entries.push(Entry::Preset(Box::new(Preset {
            id: id.clone(),
            ..Default::default()
        })));
        Ok(PresetMetadata {
            id,
            ..Default::default()
        })
    })
}

pub(crate) fn rename(path: &Path, id: &str, label: &str) -> Result<()> {
    model::validate_label(label)?;
    storage::edit(path, |library| {
        library.get_mut(id)?.label = label.trim().into();
        Ok(())
    })
}

pub(crate) fn move_before(path: &Path, id: &str, before: Option<&str>) -> Result<()> {
    storage::edit(path, |library| library.move_before(id, before))
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
pub fn remove(path: &Path, id: &str) -> Result<()> {
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
            return Ok(());
        }
        if let Err(error) = fs::remove_dir_all(directory)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%id, %error, "preset removed; template cleanup failed");
        }
        Ok(())
    })
}
