//! Persist each preference without rewriting unrelated settings.
use std::path::Path;

use anyhow::Result;
use toml_edit::Value;

use super::{UiLanguage, edit_config, section_table, write_setting};

#[derive(PartialEq, Eq)]
pub(crate) enum PreferenceChange {
    ApplyInSavedOrder(bool),
    AutoReadyUp(bool),
    SaveFallbackWhenTaken(bool),
    KeepPanelOpen(bool),
    AlwaysOnTop(bool),
    CheckUpdatesAutomatically(bool),
    Language(UiLanguage),
    StatusMonitor(String),
}

impl PreferenceChange {
    pub(crate) fn save(&self, config_path: &Path) -> Result<()> {
        edit_config(config_path, |document| {
            let (section, key, value): (_, _, Value) = match self {
                Self::ApplyInSavedOrder(value) => {
                    ("presets", "apply_in_saved_order", (*value).into())
                }
                Self::AutoReadyUp(value) => ("presets", "auto_ready_up", (*value).into()),
                Self::SaveFallbackWhenTaken(value) => {
                    ("presets", "save_fallback_when_taken", (*value).into())
                }
                Self::KeepPanelOpen(value) => {
                    section_table(document, "ui")?.remove("keep_panel_open_on_focus_loss");
                    ("ui", "keep_panel_open", (*value).into())
                }
                Self::AlwaysOnTop(value) => ("ui", "always_on_top", (*value).into()),
                Self::CheckUpdatesAutomatically(value) => {
                    ("updates", "check_automatically", (*value).into())
                }
                Self::Language(value) => {
                    ("ui", "language", value.locale().unwrap_or("auto").into())
                }
                Self::StatusMonitor(value) => {
                    section_table(document, "ui")?.remove("monitor");
                    ("ui", "status_monitor", value.as_str().into())
                }
            };
            write_setting(document, section, key, value)
        })
    }
}

pub(crate) fn save_panel_shortcut(config_path: &Path, value: &str) -> Result<()> {
    edit_config(config_path, |document| {
        write_setting(document, "ui", "panel_shortcut", value)
    })
}
