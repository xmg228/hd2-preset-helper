//! Commit settings only after shortcut registration and the single file write succeed.
use anyhow::{Result, ensure};

use super::{ActionState, AppController, panel_hotkey, preset_hotkeys};
use crate::{config::SettingsDraft, input::Hotkeys, preset_action::PresetActionOptions};

impl AppController {
    pub(super) fn open_settings(&self, show_about: bool) {
        if self.exiting
            || self.updates.installing()
            || !matches!(self.state, ActionState::Idle)
            || self.ui.is_editing()
        {
            return;
        }
        self.ui.open_settings(
            SettingsDraft {
                shortcuts: self.shortcut_bindings.clone(),
                monitor: self.ui.monitor().to_owned(),
                language: self.ui.language(),
                apply_in_saved_order: self.settings.apply_in_saved_order,
                auto_ready_up: self.settings.auto_ready_up,
                save_fallback_when_taken: self.settings.save_fallback_when_taken,
                check_updates_automatically: self.updates.automatic,
            },
            show_about,
        );
    }

    pub(super) fn open_link(&self, url: &str) {
        if self.exiting || self.updates.installing() || !matches!(self.state, ActionState::Idle) {
            return;
        }
        if let Err(error) = crate::platform::open_url(url) {
            let error = format!("{error:#}");
            tracing::warn!(error, "could not open the page in the browser");
            self.ui.settings_failed(&error);
        }
    }

    pub(super) fn save_settings(&mut self, draft: SettingsDraft) -> Result<()> {
        ensure!(
            !self.updates.installing(),
            "settings cannot be saved while installing an update"
        );
        draft.shortcuts.validate()?;
        self.hotkeys.set_enabled(false)?;
        self.panel_hotkey.set_enabled(false)?;

        let bindings = preset_hotkeys(&draft.shortcuts)?;
        let specs: Vec<_> = bindings.iter().map(|binding| binding.hotkey).collect();
        let mut hotkeys = Hotkeys::new(&specs)?;
        let mut panel = panel_hotkey(&draft.shortcuts)?;
        // Validate the entire candidate set before replacing the saved bindings.
        panel.set_enabled(true)?;
        hotkeys.set_enabled(true)?;
        let unavailable = panel
            .unavailable()
            .chain(hotkeys.unavailable())
            .collect::<Vec<_>>()
            .join("\n");
        ensure!(
            unavailable.is_empty(),
            "Shortcuts unavailable:\n{unavailable}"
        );
        panel.set_enabled(false)?;
        hotkeys.set_enabled(false)?;
        draft.save(&self.paths.config)?;

        self.bindings = bindings;
        self.hotkeys = hotkeys;
        self.panel_hotkey = panel;
        self.shortcut_bindings = draft.shortcuts;
        self.settings = PresetActionOptions {
            apply_in_saved_order: draft.apply_in_saved_order,
            auto_ready_up: draft.auto_ready_up,
            save_fallback_when_taken: draft.save_fallback_when_taken,
        };
        self.updates.automatic = draft.check_updates_automatically;
        let language_result = self.ui.set_language(draft.language);
        self.ui.settings_saved(&self.shortcut_bindings);
        if let Err(error) = language_result {
            tracing::warn!(error = %format!("{error:#}"), "settings saved but language change failed");
            self.ui.set_status(&format!(
                "Settings saved, but language change failed: {error:#}"
            ));
        }
        if self.ui.monitor() != draft.monitor
            && let Err(error) = self.ui.set_monitor(draft.monitor)
        {
            tracing::warn!(error = %format!("{error:#}"), "settings saved but window placement failed");
            self.ui.set_status(&format!(
                "Settings saved, but window placement failed: {error:#}"
            ));
        }
        tracing::info!("settings saved");
        Ok(())
    }
}
