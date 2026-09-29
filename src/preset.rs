pub(crate) mod equipment;
pub(crate) mod preview;
pub(crate) mod stratagems;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::assets::{decode_rgba8, parse_json_file};
use stratagems::StratagemPreset;

const PRESET_SCHEMA_VERSION: u32 = 1;
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

#[derive(Debug, Deserialize, Serialize)]
struct PresetFile {
    #[serde(default)]
    schema_version: u32,
    presets: BTreeMap<String, StratagemPreset>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    equipment: BTreeMap<String, equipment::SavedEquipment>,
}

impl Default for PresetFile {
    fn default() -> Self {
        Self {
            schema_version: PRESET_SCHEMA_VERSION,
            presets: BTreeMap::new(),
            equipment: BTreeMap::new(),
        }
    }
}

pub fn saved_sections(path: &Path, name: &str) -> Result<(bool, bool)> {
    let file = load_preset_file(path)?;
    Ok((
        file.presets.contains_key(name),
        file.equipment.contains_key(name),
    ))
}

/// Remove both saved sections and only this preset's own template directory.
pub fn remove(path: &Path, name: &str) -> Result<()> {
    validate_preset_name(name)?;
    let directory = resolve_template_path(path, &format!("{LOCAL_TEMPLATES_DIR}/{name}"))?;
    if directory.exists() {
        let root = directory
            .parent()
            .expect("template directory has a parent")
            .canonicalize()?;
        ensure!(
            directory.canonicalize()?.parent() == Some(root.as_path()),
            "preset template directory resolves outside its template root"
        );
    }
    if path.exists() {
        let mut file = load_preset_file(path)?;
        file.presets.remove(name);
        file.equipment.remove(name);
        write_preset_file(path, &file)?;
    }
    match fs::remove_dir_all(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove preset templates {}", directory.display())),
    }
}

pub(crate) fn archive_legacy_preset_file(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }

    let document: serde_json::Value = parse_json_file(path)?;
    let schema_version = document
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);

    match schema_version {
        Some(version) if version == u64::from(PRESET_SCHEMA_VERSION) => return Ok(None),
        Some(version) if version > u64::from(PRESET_SCHEMA_VERSION) => {
            bail!(
                "preset data uses schema version {version}, but this version only supports schema version {PRESET_SCHEMA_VERSION}"
            );
        }
        _ => {}
    }

    let backup = path.with_file_name("presets.backup.json");
    if backup.exists() {
        fs::remove_file(&backup)
            .with_context(|| format!("failed to replace {}", backup.display()))?;
    }

    fs::rename(path, &backup).with_context(|| {
        format!(
            "failed to archive legacy preset data from {} to {}",
            path.display(),
            backup.display()
        )
    })?;
    Ok(Some(backup))
}

pub fn resolve_template_path(presets_path: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    ensure!(
        !relative_path.is_absolute(),
        "local template path must be relative"
    );
    ensure!(
        relative_path
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "local template path contains an invalid component"
    );
    ensure!(
        relative_path.starts_with(LOCAL_TEMPLATES_DIR),
        "local template path must be inside {LOCAL_TEMPLATES_DIR}"
    );
    let base = presets_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(base.join(relative_path))
}

pub fn load_template_image(presets_path: &Path, relative: &str) -> Result<RgbaImage> {
    let path = resolve_template_path(presets_path, relative)?;
    let bytes = std::fs::read(&path)
        .with_context(|| format!("failed to read local template {}", path.display()))?;
    decode_rgba8(&bytes)
        .with_context(|| format!("failed to decode local template {}", path.display()))
}

fn load_preset_file(path: &Path) -> Result<PresetFile> {
    let presets: PresetFile = parse_json_file(path)?;
    ensure!(
        presets.schema_version == PRESET_SCHEMA_VERSION,
        "unsupported preset schema version {}; expected {}. Delete the old preset data before continuing",
        presets.schema_version,
        PRESET_SCHEMA_VERSION
    );
    Ok(presets)
}

fn write_preset_file(path: &Path, presets: &PresetFile) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(presets)?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

fn validate_preset_name(name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "preset name must not be empty");
    ensure!(
        name.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
        "preset name contains characters that cannot be used in a template directory: {name:?}"
    );
    Ok(())
}

fn save_template_image(presets_path: &Path, relative: &str, image: &RgbaImage) -> Result<()> {
    ensure!(
        image.width() > 0 && image.height() > 0,
        "cannot save an empty local template"
    );
    let destination = resolve_template_path(presets_path, relative)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    image
        .save(&destination)
        .with_context(|| format!("failed to save local template {}", destination.display()))
}
