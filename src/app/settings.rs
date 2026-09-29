//! Commit settings only after shortcut registration and the single file write succeed.
use anyhow::Result;

use super::{ActionState, AppController, panel_hotkey, preset_hotkeys};
use crate::{config::SettingsDraft, input::Hotkeys, preset_action::PresetActionOptions};

impl AppController {
    pub(super) fn open_settings(&self) {
        if self.exiting || !matches!(self.state, ActionState::Idle) || self.ui.is_editing() {
            return;
        }
        self.ui.open_settings(SettingsDraft {
            shortcuts: self.shortcut_bindings.clone(),
            monitor: self.ui.monitor().to_owned(),
            apply_in_saved_order: self.settings.apply_in_saved_order,
            auto_ready_up: self.settings.auto_ready_up,
            save_fallback_when_taken: self.settings.save_fallback_when_taken,
        });
    }

    pub(super) fn save_settings(&mut self, draft: SettingsDraft) -> Result<()> {
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
        self.ui.settings_saved(&self.shortcut_bindings);
        if self.ui.monitor() != draft.monitor
            && let Err(error) = self.ui.set_monitor(draft.monitor)
        {
            tracing::warn!(%error, "settings saved but window placement failed");
            self.ui.set_status(&format!(
                "Settings saved, but window placement failed: {error:#}"
            ));
        }
        tracing::info!("settings saved");
        Ok(())
    }
}
