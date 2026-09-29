//! Editable settings are saved together, without rewriting preset names or scope.
use std::path::Path;

use anyhow::Result;

use super::ShortcutBindings;

pub struct SettingsDraft {
    pub shortcuts: ShortcutBindings,
    pub monitor: String,
    pub apply_in_saved_order: bool,
    pub auto_ready_up: bool,
    pub save_fallback_when_taken: bool,
}

impl SettingsDraft {
    pub fn save(&self, path: &Path) -> Result<()> {
        super::edit_config(path, |document| {
            super::write_setting(
                document,
                "ui",
                "panel_shortcut",
                self.shortcuts.panel.as_str(),
            )?;
            super::write_setting(document, "ui", "monitor", self.monitor.as_str())?;
            for (key, value) in [
                ("apply_in_saved_order", self.apply_in_saved_order),
                ("auto_ready_up", self.auto_ready_up),
                ("save_fallback_when_taken", self.save_fallback_when_taken),
            ] {
                super::write_setting(document, "presets", key, value)?;
            }
            for (name, binding) in &self.shortcuts.presets {
                super::write_slot_field(document, name, "shortcut", binding)?;
            }
            Ok(())
        })
    }
}
