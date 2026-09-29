//! Panel-to-automation handoff. Only the controller activates the game.
use anyhow::{Result, ensure};
use std::time::Instant;
use tracing::warn;

use super::{
    ACTION_COOLDOWN, ActionState, AppController, HOTKEY_RELEASE_TIMEOUT, Work, preset_hotkeys,
};
use crate::{
    app_events::AppEvent, game_window, input, preset_action::PresetCommand, ui::UiRequest,
    window::WindowTarget,
};

impl AppController {
    pub(super) fn open_panel(&mut self) -> Result<()> {
        if self.exiting || !matches!(self.state, ActionState::Idle) {
            return Ok(());
        }
        // Closing returns to the game only if opening the panel took its focus.
        // Applying or saving resolves its own target, even when opened elsewhere.
        let game = game_window::foreground_game_window().ok();
        self.panel_return_target = game.as_ref().map(|target| target.identity());
        let anchor = game.as_ref().map(|target| target.client_origin());
        self.ui.show_panel(anchor)?;
        self.worker.send(Work::LoadPreviews)
    }

    pub(super) fn close_panel(&mut self) -> Result<()> {
        // Never reclaim focus after the user has switched to another application.
        if self.ui.is_panel_active()
            && let Some(target) = &self.panel_return_target
            && let Err(error) = target.activate()
        {
            warn!(%error, "could not return focus when closing the preset panel");
        }
        self.ui.hide_panel()?;
        self.panel_return_target = None;
        Ok(())
    }

    pub(super) fn begin_action(
        &mut self,
        preset: String,
        saving: bool,
        hotkey_id: Option<i32>,
    ) -> Result<()> {
        if self.exiting
            || !matches!(self.state, ActionState::Idle)
            || Instant::now() < self.ready_after
        {
            return Ok(());
        }
        let scope = self.scope;
        let command = match (saving, scope.stratagems, scope.equipment) {
            (_, false, false) => {
                self.ui.event(AppEvent::PresetCancelled {
                    preset,
                    reason: "no sections selected".into(),
                })?;
                return Ok(());
            }
            (true, true, true) => PresetCommand::SaveFull,
            (true, true, false) => PresetCommand::SaveStratagems,
            (true, false, true) => PresetCommand::SaveEquipment,
            (false, _, _) => PresetCommand::ApplySaved(scope),
        };
        let target = if hotkey_id.is_some() && !self.ui.is_panel_active() {
            game_window::foreground_game_window().map(|window| {
                self.ui.set_anchor(Some(window.client_origin()));
                window.identity()
            })
        } else {
            game_window::find_game_window()
        };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                warn!(%preset, error = %format!("{error:#}"), "could not locate game window for preset action");
                self.ui.event(AppEvent::PresetCancelled {
                    preset,
                    reason: format!("{error:#}"),
                })?;
                return Ok(());
            }
        };
        if hotkey_id.is_some() {
            self.worker.send(Work::Prepare(target))?;
        }
        self.ui.set_pending(true);
        self.ui.event(AppEvent::HotkeyReleaseRequested {
            preset: preset.clone(),
        })?;
        self.state = ActionState::Waiting {
            target,
            preset,
            command,
            hotkey_id,
            deadline: Instant::now() + HOTKEY_RELEASE_TIMEOUT,
        };
        Ok(())
    }

    pub(super) fn poll_ui(&mut self) -> Result<()> {
        while let Some(request) = self.ui.take_request() {
            match request {
                UiRequest::OpenPanel => self.open_panel()?,
                UiRequest::Exit => self.request_exit()?,
                UiRequest::HidePanel => {
                    if matches!(self.state, ActionState::Waiting { .. }) {
                        self.state = ActionState::Idle;
                        self.worker.send(Work::Discard)?;
                        self.ready_after = Instant::now() + ACTION_COOLDOWN;
                    }
                    self.ui.set_pending(false);
                    self.close_panel()?;
                }
                UiRequest::Action { preset, saving } if self.ui.is_panel_active() => {
                    self.begin_action(preset, saving, None)?;
                }
                UiRequest::Action { .. } => {}
                UiRequest::SetScope(scope) => {
                    match crate::config::save_scope(&self.paths.config, scope) {
                        Ok(()) => {
                            self.scope = scope;
                            self.ui.set_scope(scope);
                        }
                        Err(error) => {
                            warn!(error = %format!("{error:#}"), "failed to save preset scope");
                            self.ui
                                .set_status(&format!("Could not save preset scope: {error:#}"));
                        }
                    }
                }
                UiRequest::Rename { preset, label } => {
                    match crate::config::save_preset_label(&self.paths.config, &preset, &label) {
                        Ok(()) => self.ui.set_label(&preset, label),
                        Err(error) => {
                            warn!(%preset, error = %format!("{error:#}"), "failed to rename preset");
                            self.ui
                                .set_status(&format!("Could not save preset name: {error:#}"));
                        }
                    }
                }
                UiRequest::OpenSettings => self.open_settings(),
                UiRequest::SaveSettings(draft) => match self.save_settings(draft) {
                    Ok(()) => {}
                    Err(error) => {
                        warn!(error = %format!("{error:#}"), "settings were not saved");
                        self.ui.settings_failed(&format!("{error:#}"));
                    }
                },
                UiRequest::AddPreset => {
                    let name = self.ui.next_preset_name();
                    match crate::config::add_preset(&self.paths.config, &name) {
                        Ok(()) => {
                            self.shortcut_bindings
                                .presets
                                .insert(name.clone(), String::new());
                            self.ui.add_preset(name);
                        }
                        Err(error) => self
                            .ui
                            .set_status(&format!("Could not add preset: {error:#}")),
                    }
                }
                UiRequest::DeletePreset(preset) => {
                    if let Err(error) = self.delete_preset(&preset) {
                        warn!(%preset, error = %format!("{error:#}"), "failed to delete preset");
                        self.ui
                            .set_status(&format!("Could not finish deleting preset: {error:#}"));
                    }
                }
            }
        }
        Ok(())
    }

    fn delete_preset(&mut self, preset: &str) -> Result<()> {
        ensure!(
            matches!(self.state, ActionState::Idle),
            "wait for the current action to finish"
        );
        let mut shortcuts = self.shortcut_bindings.clone();
        shortcuts.presets.remove(preset);
        let bindings = preset_hotkeys(&shortcuts)?;
        let specs: Vec<_> = bindings.iter().map(|binding| binding.hotkey).collect();
        let hotkeys = input::Hotkeys::new(&specs)?;

        crate::preset::remove(&self.paths.presets, preset)?;
        crate::config::remove_preset(&self.paths.config, preset)?;
        self.shortcut_bindings = shortcuts;
        self.bindings = bindings;
        self.hotkeys = hotkeys;
        self.ui.remove_preset(preset);
        Ok(())
    }

    pub(super) fn finish_pending_action(&mut self) -> Result<()> {
        let ActionState::Waiting {
            target,
            preset,
            command,
            hotkey_id,
            deadline,
        } = &self.state
        else {
            return Ok(());
        };
        let owned_focus = self.ui.is_panel_active() || crate::game_window::is_game_foreground();
        if !owned_focus || Instant::now() >= *deadline {
            let preset = preset.clone();
            let reason = if owned_focus {
                "input was not released in time"
            } else {
                "focus changed"
            };
            self.close_panel()?;
            self.ui.set_pending(false);
            self.ui.event(AppEvent::PresetCancelled {
                preset,
                reason: reason.into(),
            })?;
            self.worker.send(Work::Discard)?;
            self.state = ActionState::Idle;
            self.ready_after = Instant::now() + ACTION_COOLDOWN;
        } else if input::activation_released()
            && hotkey_id.is_none_or(|id| self.hotkeys.is_released(id))
        {
            let preset = preset.clone();
            let command = *command;
            let activation = if self.ui.is_panel_active() {
                target.activate()
            } else {
                Ok(())
            };
            // Restore first, then refresh placement; minimized geometry is not useful.
            let target = match activation.and_then(|()| WindowTarget::from_identity(*target)) {
                Ok(target) => target,
                Err(error) => {
                    warn!(%preset, error = %format!("{error:#}"), "could not activate game window for preset action");
                    self.ui.event(AppEvent::PresetCancelled {
                        preset,
                        reason: format!("{error:#}"),
                    })?;
                    self.ui.set_pending(false);
                    self.worker.send(Work::Discard)?;
                    self.state = ActionState::Idle;
                    self.ready_after = Instant::now() + ACTION_COOLDOWN;
                    return Ok(());
                }
            };
            self.ui.set_anchor(Some(target.client_origin()));
            self.ui.hide_panel()?;
            self.panel_return_target = None;
            // Unregister before the worker can inject its first key, not on the next tick.
            self.hotkeys.set_enabled(false)?;
            self.panel_hotkey.set_enabled(false)?;
            self.worker.send(Work::Run {
                preset,
                command,
                settings: self.settings,
                target: target.identity(),
            })?;
            self.state = ActionState::Running;
        }
        Ok(())
    }
}
