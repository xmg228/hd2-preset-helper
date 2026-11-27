//! Apply each settings change independently.
use anyhow::{Context, Result, bail, ensure};

use super::{ActionState, AppController, panel_hotkey, preset_hotkeys};
use crate::{
    config::{PreferenceChange, ShortcutBindings, save_panel_shortcut},
    input::Hotkeys,
    ui::{SettingEdit, SettingsTarget},
};

use PreferenceChange as P;

impl AppController {
    pub(super) fn open_settings(&self, target: SettingsTarget) {
        if self.exiting
            || self.updates.installing()
            || !matches!(self.state, ActionState::Idle)
            || self.ui.is_editing()
        {
            return;
        }
        let startup =
            matches!(target, SettingsTarget::Page(_)).then(crate::platform::startup::is_enabled);
        for change in [
            P::KeepPanelOpen(self.keep_panel_open),
            P::AlwaysOnTop(self.always_on_top),
            P::ApplyInSavedOrder(self.settings.apply_in_saved_order),
            P::AutoReadyUp(self.settings.auto_ready_up),
            P::SaveFallbackWhenTaken(self.settings.save_fallback_when_taken),
            P::CheckUpdatesAutomatically(self.updates.automatic),
        ] {
            self.ui.show_preference(&change);
        }
        self.ui.open_settings(
            &self.shortcut_bindings,
            startup
                .as_ref()
                .and_then(|result| result.as_ref().ok().copied()),
            target,
        );
        if let Some(Err(error)) = startup {
            tracing::warn!(error = %format!("{error:#}"), "could not read Windows startup setting");
            self.ui.settings_error(&format!("{error:#}"), None);
        }
    }

    pub(super) fn open_link(&self, url: &str) {
        if self.exiting || self.updates.installing() || !matches!(self.state, ActionState::Idle) {
            return;
        }
        if let Err(error) = crate::platform::open_url(url) {
            let error = format!("{error:#}");
            tracing::warn!(error, "could not open the page in the browser");
            self.ui.settings_error(&error, None);
        }
    }

    fn check_settings_change(&self) -> Result<()> {
        ensure!(
            !self.exiting && matches!(self.state, ActionState::Idle) && !self.updates.installing(),
            "settings cannot be changed during an action or shutdown"
        );
        Ok(())
    }

    pub(super) fn edit_setting(&mut self, edit: SettingEdit) {
        let row = match &edit {
            SettingEdit::Shortcut { row, .. } => Some(*row),
            _ => None,
        };
        let result = self.check_settings_change().and_then(|()| match edit {
            SettingEdit::Preference(change) => self.change_preference(&change),
            SettingEdit::Startup(enabled) => self.change_startup(enabled),
            SettingEdit::Shortcut { preset, value, row } => {
                self.change_shortcut(preset.as_deref(), &value, row)
            }
        });
        self.ui.settings_ready();
        if let Err(error) = result {
            let message = format!("{error:#}");
            tracing::warn!(%message, "could not change setting");
            self.ui.settings_error(&message, row);
        }
    }

    fn change_preference(&mut self, change: &PreferenceChange) -> Result<()> {
        let previous = match change {
            P::KeepPanelOpen(_) => P::KeepPanelOpen(self.keep_panel_open),
            P::AlwaysOnTop(_) => P::AlwaysOnTop(self.always_on_top),
            P::Language(_) => P::Language(self.ui.language()),
            P::StatusMonitor(_) => P::StatusMonitor(self.ui.status_monitor().into()),
            P::ApplyInSavedOrder(_) => P::ApplyInSavedOrder(self.settings.apply_in_saved_order),
            P::AutoReadyUp(_) => P::AutoReadyUp(self.settings.auto_ready_up),
            P::SaveFallbackWhenTaken(_) => {
                P::SaveFallbackWhenTaken(self.settings.save_fallback_when_taken)
            }
            P::CheckUpdatesAutomatically(_) => P::CheckUpdatesAutomatically(self.updates.automatic),
        };
        if change != &previous {
            self.apply_preference(change)?;
            if let Err(error) = change.save(&self.paths.config) {
                if let Err(restore) = self.apply_preference(&previous) {
                    return Err(error.context(format!(
                        "could not restore the previous setting: {restore:#}"
                    )));
                }
                return Err(error);
            }
        }
        self.ui.show_preference(change);
        Ok(())
    }

    fn apply_preference(&mut self, change: &PreferenceChange) -> Result<()> {
        match change {
            P::KeepPanelOpen(value) => self.keep_panel_open = *value,
            P::AlwaysOnTop(value) => self.always_on_top = *value,
            P::Language(value) => self.ui.set_language(*value)?,
            P::StatusMonitor(value) => self.ui.set_status_monitor(value.clone())?,
            P::ApplyInSavedOrder(value) => self.settings.apply_in_saved_order = *value,
            P::AutoReadyUp(value) => self.settings.auto_ready_up = *value,
            P::SaveFallbackWhenTaken(value) => self.settings.save_fallback_when_taken = *value,
            P::CheckUpdatesAutomatically(value) => self.updates.automatic = *value,
        }
        Ok(())
    }

    fn change_startup(&self, enabled: bool) -> Result<()> {
        crate::platform::startup::set_enabled(enabled)?;
        self.ui.startup_changed(enabled);
        Ok(())
    }

    fn change_shortcut(&mut self, preset: Option<&str>, value: &str, row: i32) -> Result<()> {
        let mut shortcuts = self.shortcut_bindings.clone();
        let binding = if let Some(preset) = preset {
            shortcuts
                .presets
                .get_mut(preset)
                .context("preset no longer exists")?
        } else {
            &mut shortcuts.panel
        };
        let previous = ShortcutBindings::parse(binding)?;
        let next = ShortcutBindings::parse(value)?;
        *binding = value.into();
        shortcuts.validate()?;

        // Probe only the new key, leaving all existing bindings intact on failure.
        let _probe = if let Some(next) = next.filter(|key| Some(*key) != previous) {
            let mut probe = Hotkeys::new(&[next.spec(1)])?;
            probe.set_enabled(true)?;
            if let Some(error) = probe.unavailable().next() {
                bail!("{error}");
            }
            Some(probe)
        } else {
            None
        };
        if let Some(preset) = preset {
            let bindings = preset_hotkeys(&shortcuts)?;
            let specs = bindings
                .iter()
                .map(|binding| binding.hotkey)
                .collect::<Vec<_>>();
            let hotkeys = Hotkeys::new(&specs)?;
            crate::preset::set_shortcut(&self.paths.presets, preset, value)?;
            self.bindings = bindings;
            self.hotkeys = hotkeys;
        } else {
            let hotkey = panel_hotkey(&shortcuts)?;
            save_panel_shortcut(&self.paths.config, value)?;
            self.panel_hotkey = hotkey;
        }
        self.shortcut_bindings = shortcuts;
        self.ui.shortcut_changed(preset, value, row);
        Ok(())
    }
}
