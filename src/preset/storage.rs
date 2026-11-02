//! A single read-modify-write boundary shared by UI edits and captured sections.
use std::{path::Path, sync::Mutex};

use anyhow::{Context, Result};

use super::{
    Preset, PresetLibrary,
    images::{self, NewImages},
};

static WRITER: Mutex<()> = Mutex::new(());

pub(super) fn load(path: &Path) -> Result<PresetLibrary> {
    let library: PresetLibrary = crate::assets::parse_json_file(path)?;
    library.validate()?;
    Ok(library)
}

pub(super) fn write(path: &Path, library: &PresetLibrary) -> Result<()> {
    crate::persistence::write(path, &encode(library)?)
}

pub(super) fn encode(library: &PresetLibrary) -> Result<Vec<u8>> {
    library.validate()?;
    Ok(serde_json::to_vec_pretty(library)?)
}

pub(super) fn edit<T>(
    path: &Path,
    update: impl FnOnce(&mut PresetLibrary) -> Result<T>,
) -> Result<T> {
    locked(path, |library| {
        let result = update(library)?;
        write(path, library)?;
        Ok(result)
    })
}

/// Publish references only after all new images exist. Reclaim old files after commit.
pub(super) fn edit_images<T>(
    path: &Path,
    id: &str,
    subdirectory: &str,
    update: impl FnOnce(&mut Preset, &mut NewImages) -> Result<T>,
) -> Result<T> {
    locked(path, |library| {
        let preset = library.get_mut(id)?;
        let previous = preset.image_paths().map(str::to_owned).collect();
        let mut images = NewImages::new(path, id, subdirectory)?;
        let result = update(preset, &mut images)?;
        write(path, library)?;
        images.commit();
        // Keep the writer lock while checking references and removing old images.
        images::cleanup(path, id, previous, library);
        Ok(result)
    })
}

pub(super) fn locked<T>(
    path: &Path,
    update: impl FnOnce(&mut PresetLibrary) -> Result<T>,
) -> Result<T> {
    let _writer = WRITER
        .lock()
        .map_err(|_| anyhow::anyhow!("preset storage lock was poisoned"))?;
    let mut library = load(path).context("failed to read preset library")?;
    update(&mut library)
}
