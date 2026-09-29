//! A shortcut-editing snapshot, derived from UI settings and preset slots.
//! It is not a separate configuration section. System registration belongs to input.
use std::collections::BTreeMap;

use crate::input::Shortcut;
use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutBindings {
    pub panel: String,
    pub presets: BTreeMap<String, String>,
}

impl ShortcutBindings {
    pub fn parse(value: &str) -> Result<Option<Shortcut>> {
        if value.trim().is_empty() {
            Ok(None)
        } else {
            value.parse().map(Some)
        }
    }

    pub fn validate(&self) -> Result<()> {
        let mut used = Vec::new();
        for (name, value) in std::iter::once(("Open panel", &self.panel)).chain(
            self.presets
                .iter()
                .map(|(name, value)| (name.as_str(), value)),
        ) {
            if name != "Open panel" {
                super::validate_preset_label(name, "")?;
            }
            if let Some(shortcut) = Self::parse(value).with_context(|| name.to_string())? {
                if let Some((_, previous)) = used.iter().find(|(key, _)| *key == shortcut) {
                    bail!("{shortcut} is assigned to both {previous} and {name}");
                }
                used.push((shortcut, name));
            }
        }
        Ok(())
    }
}
