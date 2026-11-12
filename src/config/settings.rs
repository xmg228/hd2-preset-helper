//! Prepare global settings without changing preset contents or shared scope.
use std::path::Path;

use anyhow::Result;

use super::{ShortcutBindings, UiLanguage};

pub struct SettingsDraft {
    pub shortcuts: ShortcutBindings,
    pub keep_panel_open: bool,
    pub always_on_top: bool,
    pub status_monitor: String,
    pub language: UiLanguage,
    pub apply_in_saved_order: bool,
    pub auto_ready_up: bool,
    pub save_fallback_when_taken: bool,
    pub check_updates_automatically: bool,
}

impl SettingsDraft {
    pub fn render(&self, config_path: &Path) -> Result<String> {
        let text = std::fs::read_to_string(config_path)?;
        let mut document = text.parse::<toml_edit::DocumentMut>()?;
        super::migration::prepare(&mut document)?;
        super::write_setting(
            &mut document,
            "ui",
            "panel_shortcut",
            self.shortcuts.panel.as_str(),
        )?;
        super::section_table(&mut document, "ui")?.remove("keep_panel_open_on_focus_loss");
        super::write_setting(&mut document, "ui", "keep_panel_open", self.keep_panel_open)?;
        super::write_setting(&mut document, "ui", "always_on_top", self.always_on_top)?;
        super::section_table(&mut document, "ui")?.remove("monitor");
        super::write_setting(
            &mut document,
            "ui",
            "status_monitor",
            self.status_monitor.as_str(),
        )?;
        super::write_setting(
            &mut document,
            "ui",
            "language",
            self.language.locale().unwrap_or("auto"),
        )?;
        for (key, value) in [
            ("apply_in_saved_order", self.apply_in_saved_order),
            ("auto_ready_up", self.auto_ready_up),
            ("save_fallback_when_taken", self.save_fallback_when_taken),
        ] {
            super::write_setting(&mut document, "presets", key, value)?;
        }
        super::write_setting(
            &mut document,
            "updates",
            "check_automatically",
            self.check_updates_automatically,
        )?;
        Ok(document.to_string())
    }
}
