//! One-way migration from the published modifiers/keys + labels + overlay format.
use std::collections::BTreeMap;

use super::{DEFAULT_CONFIG_TOML, PresetSlot};
use crate::input::{HotkeyModifier, HotkeyModifiers, Key, Shortcut};
use anyhow::{Context, Result, bail};

#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyHotkeys {
    modifiers: Vec<HotkeyModifier>,
    keys: Vec<Key>,
}

impl Default for LegacyHotkeys {
    fn default() -> Self {
        Self {
            modifiers: vec![HotkeyModifier::Ctrl, HotkeyModifier::Shift],
            keys: vec![Key::F7, Key::F8, Key::F9, Key::F10, Key::F11, Key::F12],
        }
    }
}

pub(super) fn migrate(document: &mut toml_edit::DocumentMut) -> Result<bool> {
    let legacy = document.contains_key("hotkey")
        || document.contains_key("overlay")
        || document
            .get("presets")
            .is_some_and(|presets| presets.get("labels").is_some());
    if !legacy {
        return Ok(false);
    }
    if document.contains_key("ui")
        || document
            .get("presets")
            .is_some_and(|presets| presets.get("slots").is_some())
    {
        bail!(
            "cannot mix legacy configuration with ui or presets.slots; use the current config.toml format"
        );
    }
    let values: toml::Value = toml::from_str(&document.to_string())?;
    let hotkeys: LegacyHotkeys = values
        .get("hotkey")
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    let labels: BTreeMap<String, String> = values
        .get("presets")
        .and_then(|v| v.get("labels"))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    let modifiers = HotkeyModifiers::new(hotkeys.modifiers)?;
    let mut slots = BTreeMap::new();
    for (index, key) in hotkeys.keys.into_iter().enumerate() {
        slots.insert(
            format!("preset_{}", index + 1),
            PresetSlot {
                label: String::new(),
                shortcut: Shortcut { modifiers, key }.to_string(),
            },
        );
    }
    for (name, label) in labels {
        slots.entry(name).or_default().label = label;
    }
    let defaults = DEFAULT_CONFIG_TOML.parse::<toml_edit::DocumentMut>()?;
    let mut ui = defaults["ui"].clone();
    if let Some(overlay) = document.get("overlay") {
        for (key, value) in overlay
            .as_table_like()
            .context("overlay must be a table")?
            .iter()
        {
            match key {
                "enabled" => {} // Action feedback is always shown now.
                "monitor" => ui["monitor"] = value.clone(),
                _ => bail!("unknown overlay setting: {key}"),
            }
        }
    }
    let mut table = toml_edit::Table::new();
    for (name, slot) in slots {
        table.insert(&name, slot.to_item());
    }
    let presets = super::section_table(document, "presets")?;
    presets.insert("slots", toml_edit::Item::Table(table));
    presets.remove("labels");
    document["ui"] = ui;
    document.remove("hotkey");
    document.remove("overlay");
    Ok(true)
}
