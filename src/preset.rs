use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::assets::{IconCatalog, parse_json_file};
use crate::item::ItemKind;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Preset {
    pub stratagems: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booster: Option<String>,
}

pub fn invalid_preset_reason(preset: &Preset, catalog: &IconCatalog) -> Option<String> {
    for item_id in &preset.stratagems {
        let Some(item) = catalog.get(item_id) else {
            return Some(format!("missing {item_id}"));
        };
        if item.kind != ItemKind::Stratagem {
            return Some(format!("{item_id} is not a stratagem"));
        }
    }

    if let Some(item_id) = preset.booster.as_deref() {
        let Some(item) = catalog.get(item_id) else {
            return Some(format!("missing {item_id}"));
        };
        if item.kind != ItemKind::Booster {
            return Some(format!("{item_id} is not a booster"));
        }
    }

    None
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct PresetFile {
    presets: BTreeMap<String, Preset>,
}

pub fn load_preset(path: &Path, name: &str) -> Result<Preset> {
    let presets: PresetFile = parse_json_file(path)?;
    let preset = presets
        .presets
        .get(name)
        .with_context(|| format!("preset not found: {name}"))?;

    validate_preset(name, preset)?;
    Ok(preset.clone())
}

pub(crate) fn load_presets(path: &Path) -> Result<BTreeMap<String, Preset>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    Ok(parse_json_file::<PresetFile>(path)?.presets)
}

pub fn save_preset(path: &Path, name: &str, preset: &Preset) -> Result<()> {
    validate_preset(name, preset)?;

    let mut presets = if path.exists() {
        parse_json_file(path)?
    } else {
        PresetFile::default()
    };
    presets.presets.insert(name.to_string(), preset.clone());

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(&presets)?;
    std::fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;

    Ok(())
}

pub(crate) fn validate_preset(name: &str, preset: &Preset) -> Result<()> {
    if preset.stratagems.len() != 4 {
        bail!(
            "preset {name} must contain exactly 4 stratagems, got {}",
            preset.stratagems.len()
        );
    }

    for (index, item_id) in preset.stratagems.iter().enumerate() {
        if preset.stratagems[..index].contains(item_id) {
            bail!("preset {name} contains duplicate stratagem {item_id}");
        }
    }
    Ok(())
}
