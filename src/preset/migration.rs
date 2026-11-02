//! The JSON schema is the commit point; obsolete TOML slots never override it.
use super::{
    Entry, Preset, PresetLibrary, PresetMetadata, StratagemPreset,
    model::{self, SCHEMA_VERSION, SavedEquipment},
    storage,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};

#[derive(Deserialize)]
struct LegacyFile {
    presets: BTreeMap<String, StratagemPreset>,
    #[serde(default)]
    equipment: BTreeMap<String, SavedEquipment>,
}

/// Returns whether incompatible pre-v1 captures need to be saved again.
pub(crate) fn initialize(
    path: &Path,
    read_metadata: impl FnOnce() -> Result<Option<Vec<PresetMetadata>>>,
) -> Result<bool> {
    let value: Option<serde_json::Value> = path
        .try_exists()?
        .then(|| crate::assets::parse_json_file(path))
        .transpose()?;
    let version = match &value {
        Some(value) => {
            ensure!(value.is_object(), "preset data must be a JSON object");
            match value.get("schema_version") {
                Some(version) => version.as_u64().context("invalid preset schema version")?,
                None => 0,
            }
        }
        None => 0,
    };
    ensure!(
        version <= u64::from(SCHEMA_VERSION),
        "preset schema version {version} is newer than this application supports"
    );
    if version == u64::from(SCHEMA_VERSION) {
        let library: PresetLibrary = serde_json::from_value(value.unwrap())?;
        library.validate()?;
        return Ok(false);
    }

    let metadata = read_metadata()?.unwrap_or_else(|| {
        (1..=6)
            .map(|n| PresetMetadata {
                id: format!("preset_{n}"),
                label: String::new(),
                shortcut: format!("Ctrl + Shift + F{}", n + 6),
            })
            .collect()
    });
    // Merge legacy metadata and sections by ID before building the ordered tree.
    let mut presets: BTreeMap<_, _> = metadata
        .into_iter()
        .map(|info| {
            (
                info.id,
                Preset {
                    label: info.label,
                    shortcut: info.shortcut,
                    ..Default::default()
                },
            )
        })
        .collect();
    let needs_resave = value.is_some() && version == 0;
    if version == 1 {
        let legacy: LegacyFile =
            serde_json::from_value(value.unwrap()).context("invalid v1 preset data")?;
        for (id, section) in legacy.presets {
            presets.entry(id).or_default().stratagems = Some(section);
        }
        for (id, section) in legacy.equipment {
            presets.entry(id).or_default().equipment = Some(section);
        }
    } else if let Some(value) = value {
        ensure!(
            value
                .get("presets")
                .is_some_and(serde_json::Value::is_object),
            "unrecognized legacy preset data"
        );
    }
    let mut presets: Vec<_> = presets.into_iter().collect();
    presets.sort_by_key(|(id, _)| model::ordinal(id, "preset_").unwrap_or(usize::MAX));
    let mut library = PresetLibrary::default();
    for (id, mut preset) in presets {
        model::validate_label(&preset.label)?;
        preset.id = id;
        preset.label = preset.label.trim().into();
        library.entries.push(Entry::Preset(Box::new(preset)));
    }
    // Leave the original inputs untouched until the complete library is valid.
    storage::write(path, &library)?;
    tracing::info!(
        schema_version = SCHEMA_VERSION,
        preset_count = library.presets().count(),
        "preset library initialized"
    );
    Ok(needs_resave)
}
