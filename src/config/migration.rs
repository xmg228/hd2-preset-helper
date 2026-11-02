//! Read published preset metadata directly; normalize only global TOML settings.
use std::collections::BTreeMap;

use super::DEFAULT_CONFIG_TOML;
use crate::{
    input::{HotkeyModifier, HotkeyModifiers, Key, Shortcut},
    preset::PresetMetadata,
};
use anyhow::{Context, Result, ensure};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Format {
    Current,
    Slots,
    Hotkeys,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacySlot {
    label: String,
    shortcut: String,
}

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

/// The original metadata is retained by the caller until the JSON library commits.
pub(super) fn prepare(document: &mut toml_edit::DocumentMut) -> Result<Format> {
    let hotkeys = document.contains_key("hotkey")
        || document.contains_key("overlay")
        || document
            .get("presets")
            .is_some_and(|presets| presets.get("labels").is_some());
    let slots = document
        .get("presets")
        .is_some_and(|presets| presets.get("slots").is_some());
    if hotkeys {
        ensure!(
            !document.contains_key("ui") && !slots,
            "cannot mix legacy configuration with ui or presets.slots; use the current config.toml format"
        );
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
                    _ => anyhow::bail!("unknown overlay setting: {key}"),
                }
            }
        }
        document["ui"] = ui;
        document.remove("hotkey");
        document.remove("overlay");
    }
    if let Some(presets) = document
        .get_mut("presets")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        presets.remove("labels");
        presets.remove("slots");
    }
    Ok(if hotkeys {
        Format::Hotkeys
    } else if slots {
        Format::Slots
    } else {
        Format::Current
    })
}

/// Parse only when no authoritative v2 library exists. Stale slots must not block it.
pub(super) fn metadata(values: &toml::Value, hotkeys: bool) -> Result<Option<Vec<PresetMetadata>>> {
    let slots: Option<BTreeMap<String, LegacySlot>> = if hotkeys {
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
        let mut slots: BTreeMap<_, _> = hotkeys
            .keys
            .into_iter()
            .enumerate()
            .map(|(index, key)| {
                (
                    format!("preset_{}", index + 1),
                    LegacySlot {
                        label: String::new(),
                        shortcut: Shortcut { modifiers, key }.to_string(),
                    },
                )
            })
            .collect();
        for (id, label) in labels {
            slots.entry(id).or_default().label = label;
        }
        Some(slots)
    } else {
        values
            .get("presets")
            .and_then(|v| v.get("slots"))
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .context("invalid legacy preset slots")?
    };
    Ok(slots.map(|slots| {
        slots
            .into_iter()
            .map(|(id, slot)| PresetMetadata {
                id,
                label: slot.label,
                shortcut: slot.shortcut,
            })
            .collect()
    }))
}
