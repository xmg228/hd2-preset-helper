//! Panel-to-automation handoff. Only the controller activates the game.
use anyhow::{Result, ensure};
use std::time::Instant;
use tracing::{debug, error, warn};

use super::failure::save_failure_report;
use super::{
    ACTION_COOLDOWN, ACTIVATION_CONFIRM_TIMEOUT, ActionState, AppController,
    HOTKEY_RELEASE_TIMEOUT, Work, preset_hotkeys,
};
use crate::{
    app_events::AppEvent, game_window, input, preset::PresetScope, preset_action::PresetCommand,
    ui::UiRequest, window::WindowTarget,
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
            warn!(error = %format!("{error:#}"), "could not return focus when closing the preset panel");
        }
        self.ui.hide_panel()?;
        self.panel_return_target = None;
        Ok(())
    }

    pub(super) fn begin_action(
        &mut self,
        preset: String,
        saving: bool,
        scope: PresetScope,
        hotkey_id: Option<i32>,
    ) -> Result<()> {
        if self.exiting
            || !matches!(self.state, ActionState::Idle)
            || Instant::now() < self.ready_after
        {
            return Ok(());
        }
        let command = match (saving, scope.stratagems, scope.equipment) {
            (_, false, false) => {
                debug!(%preset, saving, reason = "no sections selected", "preset action cancelled");
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
        // Include window lookup and activation, not just action-worker execution.
        crate::logging::clear_recent();
        debug!(%preset, ?command, "preset action requested");
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
                error!(%preset, ?command, error = %format!("{error:#}"), "could not locate game window for preset action");
                save_failure_report(&self.paths.last_failure, None);
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
                    if matches!(self.state, ActionState::Activating { .. }) {
                        // Cancelling activation must not request foreground again.
                        self.panel_return_target = None;
                    }
                    if let ActionState::Waiting {
                        preset, command, ..
                    }
                    | ActionState::Activating {
                        preset, command, ..
                    } = &self.state
                    {
                        debug!(%preset, ?command, reason = "panel closed", "preset action cancelled");
                        self.state = ActionState::Idle;
                        self.worker.send(Work::Discard)?;
                        self.ready_after = Instant::now() + ACTION_COOLDOWN;
                    }
                    self.ui.set_pending(false);
                    self.close_panel()?;
                }
                UiRequest::Action {
                    preset,
                    saving,
                    scope,
                } if self.ui.is_panel_active() => {
                    self.begin_action(preset, saving, scope.unwrap_or(self.scope), None)?;
                }
                UiRequest::Action { .. } => {}
                UiRequest::SetScope(scope) => {
                    match crate::config::save_scope(&self.paths.config, scope) {
                        Ok(()) => {
                            self.scope = scope;
                            self.ui.set_scope(scope);
                        }
                        Err(error) => {
                            error!(error = %format!("{error:#}"), "failed to save preset scope");
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
                        Err(error) => {
                            error!(preset = %name, error = %format!("{error:#}"), "failed to add preset");
                            self.ui
                                .set_status(&format!("Could not add preset: {error:#}"));
                        }
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
        let (preset, command, target) = match &self.state {
            ActionState::Waiting {
                target,
                preset,
                command,
                hotkey_id,
                deadline,
            } => {
                let owned_focus = self.ui.is_panel_active() || game_window::is_game_foreground();
                if !owned_focus || Instant::now() >= *deadline {
                    let preset = preset.clone();
                    let reason = if owned_focus {
                        "input was not released in time"
                    } else {
                        "focus changed"
                    };
                    debug!(%preset, ?command, reason, "preset action cancelled");
                    self.close_panel()?;
                    self.ui.set_pending(false);
                    self.ui.event(AppEvent::PresetCancelled {
                        preset,
                        reason: reason.into(),
                    })?;
                    self.worker.send(Work::Discard)?;
                    self.state = ActionState::Idle;
                    self.ready_after = Instant::now() + ACTION_COOLDOWN;
                    return Ok(());
                }
                if !input::activation_released()
                    || hotkey_id.is_some_and(|id| !self.hotkeys.is_released(id))
                {
                    return Ok(());
                }
                let result = target.request_activation();
                if matches!(result, Ok(false)) {
                    self.state = ActionState::Activating {
                        target: *target,
                        preset: preset.clone(),
                        command: *command,
                        started: Instant::now(),
                    };
                    return Ok(());
                }
                // Restore first, then refresh placement; minimized geometry is not useful.
                (
                    preset.clone(),
                    *command,
                    result.and_then(|_| WindowTarget::from_identity(*target)),
                )
            }
            ActionState::Activating {
                target,
                preset,
                command,
                started,
            } => {
                let result = if started.elapsed() < ACTIVATION_CONFIRM_TIMEOUT {
                    target.confirm_activation()
                } else {
                    Ok(false)
                };
                if matches!(result, Ok(false)) && started.elapsed() < ACTIVATION_CONFIRM_TIMEOUT {
                    return Ok(());
                }
                let target = result.and_then(|confirmed| {
                    ensure!(
                        confirmed,
                        "the target window did not become foreground within {:.3} seconds",
                        ACTIVATION_CONFIRM_TIMEOUT.as_secs_f64()
                    );
                    WindowTarget::from_identity(*target)
                });
                debug!(%preset, ?command, confirmed = target.is_ok(), confirmation_wait_s = started.elapsed().as_secs_f64(), "game window activation confirmation finished");
                (preset.clone(), *command, target)
            }
            _ => return Ok(()),
        };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                error!(%preset, ?command, error = %format!("{error:#}"), "could not activate game window for preset action");
                save_failure_report(&self.paths.last_failure, None);
                self.ui.hide_panel()?;
                self.panel_return_target = None;
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
        Ok(())
    }
}
