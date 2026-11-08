//! Panel-to-automation handoff. Only the controller activates the game.
use anyhow::{Result, ensure};
use std::time::Instant;
use tracing::{debug, error, warn};

use super::failure::save_failure_report;
use super::{
    ACTION_COOLDOWN, ACTIVATION_CONFIRM_TIMEOUT, ActionState, AppController, GameAction,
    HOTKEY_RELEASE_TIMEOUT, PendingAction, Work, preset_hotkeys,
};
use crate::{
    app_events::AppEvent,
    game_window, input,
    preset::{PresetScope, PresetSection},
    preset_action::PresetCommand,
    scan::{ScanCancellation, ScanScope, ScanSummary},
    ui::UiRequest,
    window::WindowTarget,
};

impl AppController {
    pub(super) fn open_panel(&mut self) -> Result<()> {
        if let ActionState::Running {
            scan_cancel: Some(cancel),
        } = &self.state
        {
            cancel.cancel();
            return Ok(());
        }
        if self.exiting || !matches!(self.state, ActionState::Idle) {
            return Ok(());
        }
        // Closing returns to the game only if opening the panel took its focus.
        // Applying or saving resolves its own target, even when opened elsewhere.
        let game = game_window::foreground_game_window().ok();
        self.panel_return_target = game.as_ref().map(|target| target.identity());
        let anchor = game.as_ref().map(|target| target.client_origin());
        self.ui.show_panel(anchor)?;
        if !self.updates.installing() {
            self.worker.send(Work::LoadPreviews)?;
        }
        Ok(())
    }

    pub(super) fn close_panel(&mut self) -> Result<()> {
        self.cancel_action_check()?;
        // Never reclaim focus after the user has switched to another application.
        if self.ui.is_panel_active()
            && let Some(target) = &self.panel_return_target
            && let Err(error) = target.activate()
        {
            warn!(error = %format!("{error:#}"), "could not return focus when closing the preset panel");
        }
        self.ui.dismiss_panel()?;
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
            || self.updates.installing()
            || !matches!(self.state, ActionState::Idle)
            || Instant::now() < self.ready_after
        {
            return Ok(());
        }
        let command = match (saving, scope.stratagems, scope.includes_equipment()) {
            (_, false, false) => {
                debug!(%preset, saving, reason = "no sections selected", "preset action cancelled");
                self.ui.event(AppEvent::PresetCancelled {
                    preset,
                    reason: "no sections selected".into(),
                })?;
                return Ok(());
            }
            (true, true, true) => PresetCommand::SaveFull(scope.equipment_items),
            (true, true, false) => PresetCommand::SaveStratagems,
            (true, false, true) => PresetCommand::SaveEquipment(scope.equipment_items),
            (false, _, _) => PresetCommand::ApplySaved(scope),
        };
        self.begin_game_action(
            GameAction::Preset {
                preset,
                command,
                options: self.settings,
            },
            hotkey_id,
        )
    }

    fn begin_scan(&mut self, scope: ScanScope) -> Result<()> {
        self.begin_game_action(
            GameAction::Scan {
                scope,
                cancel: ScanCancellation::default(),
            },
            None,
        )
    }

    fn begin_game_action(&mut self, action: GameAction, hotkey_id: Option<i32>) -> Result<()> {
        let editor_scan = matches!(
            action,
            GameAction::Scan {
                scope: ScanScope::Missing,
                ..
            }
        ) && self.ui.can_scan_missing_items();
        if self.exiting
            || self.updates.installing()
            || (self.ui.is_editing_items() && !editor_scan)
            || !matches!(self.state, ActionState::Idle)
            || Instant::now() < self.ready_after
        {
            return Ok(());
        }
        // Include window lookup and activation, not just action-worker execution.
        crate::logging::clear_recent();
        debug!(?action, "game action requested");
        let from_panel = self.ui.is_panel_active();
        let target = if hotkey_id.is_some() && !from_panel {
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
                error!(?action, error = %format!("{error:#}"), "could not locate game window");
                save_failure_report(&self.paths.last_failure, &action, &error, None);
                self.present_action_problem(&action, format!("{error:#}"), false)?;
                return Ok(());
            }
        };
        let request = PendingAction {
            action,
            target,
            hotkey_id,
            from_panel,
        };
        if let GameAction::Preset {
            preset,
            command: PresetCommand::ApplySaved(scope),
            ..
        } = &request.action
        {
            self.worker.send(Work::CheckPreset {
                preset: preset.clone(),
                scope: *scope,
            })?;
            self.ui.set_pending(true);
            self.state = ActionState::Checking(Some(request));
            Ok(())
        } else {
            self.wait_for_action(request)
        }
    }

    fn wait_for_action(&mut self, request: PendingAction) -> Result<()> {
        if request.hotkey_id.is_some() {
            self.worker.send(Work::Prepare(request.target))?;
        }
        self.ui.set_pending(true);
        match &request.action {
            GameAction::Preset { preset, .. } => {
                self.ui.event(AppEvent::HotkeyReleaseRequested {
                    preset: preset.clone(),
                })?
            }
            GameAction::Scan { .. } => self.ui.scan_waiting()?,
        }
        self.state = ActionState::Waiting {
            request,
            deadline: Instant::now() + HOTKEY_RELEASE_TIMEOUT,
        };
        Ok(())
    }

    pub(super) fn action_check_owns_focus(&self, request: &PendingAction) -> bool {
        if request.from_panel {
            self.ui.is_panel_active()
        } else {
            game_window::foreground_game_window()
                .is_ok_and(|window| window.identity() == request.target)
        }
    }

    pub(super) fn finish_action_check(
        &mut self,
        result: std::result::Result<usize, String>,
    ) -> Result<()> {
        let ActionState::Checking(request) = &mut self.state else {
            return Ok(());
        };
        let request = request.take();
        self.state = ActionState::Idle;
        self.ui.set_pending(false);
        let Some(request) =
            request.filter(|request| !self.exiting && self.action_check_owns_focus(request))
        else {
            return Ok(());
        };
        let count = match result {
            Ok(count) => count,
            Err(error) => {
                error!(action = ?request.action, %error, "could not prepare preset action");
                save_failure_report(
                    &self.paths.last_failure,
                    &request.action,
                    &anyhow::Error::msg(error.clone()),
                    None,
                );
                return self.present_action_problem(&request.action, error, false);
            }
        };
        if count == 0 {
            return self.wait_for_action(request);
        }
        let target = match WindowTarget::from_identity(request.target) {
            Ok(target) => target,
            Err(error) => {
                save_failure_report(&self.paths.last_failure, &request.action, &error, None);
                return self.present_action_problem(&request.action, format!("{error:#}"), false);
            }
        };
        let GameAction::Preset { preset, .. } = &request.action else {
            unreachable!("only preset application requests a cache check");
        };
        if !request.from_panel {
            self.panel_return_target = Some(request.target);
        }
        self.ui
            .show_scan_confirmation(preset, count, Some(target.client_origin()))?;
        self.state = ActionState::Confirming(request);
        Ok(())
    }

    pub(super) fn cancel_action_check(&mut self) -> Result<bool> {
        let hide_panel = match &mut self.state {
            ActionState::Checking(request) => {
                if request.take().is_none() {
                    return Ok(false);
                }
                false
            }
            ActionState::Confirming(request) => !request.from_panel,
            _ => return Ok(false),
        };
        if matches!(self.state, ActionState::Confirming(_)) {
            self.state = ActionState::Idle;
        }
        self.ui.dismiss_scan_confirmation();
        self.ui.set_pending(false);
        self.hotkeys.discard_pending();
        self.panel_hotkey.discard_pending();
        self.ready_after = Instant::now() + ACTION_COOLDOWN;
        self.worker.send(Work::Discard)?;
        Ok(hide_panel)
    }

    fn confirm_scan(&mut self) -> Result<()> {
        if !self.ui.is_panel_active() || !self.ui.is_scan_confirm_open() {
            return Ok(());
        }
        if !matches!(self.state, ActionState::Confirming(_)) {
            return Ok(());
        }
        let ActionState::Confirming(mut request) =
            std::mem::replace(&mut self.state, ActionState::Idle)
        else {
            unreachable!();
        };
        self.ui.dismiss_scan_confirmation();
        request.hotkey_id = None;
        self.wait_for_action(request)
    }

    fn present_action_problem(
        &mut self,
        action: &GameAction,
        reason: String,
        cancelled: bool,
    ) -> Result<()> {
        match action {
            GameAction::Preset { preset, .. } => self.ui.event(AppEvent::PresetCancelled {
                preset: preset.clone(),
                reason,
            }),
            GameAction::Scan { .. } => self.finish_scan(Err(reason), cancelled),
        }
    }

    pub(super) fn finish_scan(
        &mut self,
        result: std::result::Result<ScanSummary, String>,
        cancelled: bool,
    ) -> Result<()> {
        if self.exiting {
            return Ok(());
        }
        if self.ui.is_editing_items() {
            self.panel_return_target = game_window::foreground_game_window()
                .ok()
                .map(|target| target.identity());
            self.ui
                .resume_item_editor(result.as_ref().err().map(String::as_str))?;
            self.worker.send(Work::LoadItemCandidates)?;
        } else {
            self.ui.scan_finished(result, cancelled)?;
        }
        self.worker.send(Work::LoadPreviews)
    }

    fn save_scope(&mut self, scope: PresetScope) {
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
                        request: PendingAction { action, .. },
                        ..
                    }
                    | ActionState::Activating {
                        request: PendingAction { action, .. },
                        ..
                    } = &self.state
                    {
                        debug!(?action, reason = "panel closed", "game action cancelled");
                        self.state = ActionState::Idle;
                        self.worker.send(Work::Discard)?;
                        self.ready_after = Instant::now() + ACTION_COOLDOWN;
                        self.ui.cancel_pending_action();
                    }
                    self.close_panel()?;
                }
                UiRequest::ConfirmScan => self.confirm_scan()?,
                UiRequest::CancelScan => {
                    if self.cancel_action_check()? {
                        self.close_panel()?;
                    }
                }
                UiRequest::Action {
                    preset,
                    saving,
                    section,
                } if self.ui.is_panel_active() => {
                    let mut scope = self.scope;
                    if let Some(section) = section {
                        (scope.stratagems, scope.equipment) = match section {
                            PresetSection::Stratagems => (true, false),
                            PresetSection::Equipment => (false, true),
                        };
                    }
                    self.begin_action(preset, saving, scope, None)?;
                }
                UiRequest::Action { .. } => {}
                UiRequest::Scan(scope) if self.ui.is_panel_active() => self.begin_scan(scope)?,
                UiRequest::Scan(_) => {}
                UiRequest::ScanMissingItems
                    if self.ui.is_panel_active() && self.ui.can_scan_missing_items() =>
                {
                    self.begin_scan(ScanScope::Missing)?;
                }
                UiRequest::ScanMissingItems => {}
                UiRequest::EditItems
                    if !self.exiting
                        && !self.updates.installing()
                        && matches!(self.state, ActionState::Idle)
                        && self.ui.is_panel_active()
                        && !self.ui.is_editing() =>
                {
                    if self.ui.open_item_editor() {
                        self.worker.send(Work::LoadItemCandidates)?;
                    }
                }
                UiRequest::EditItems => {}
                UiRequest::SaveItemChoices { preset, choices } => {
                    self.worker.send(Work::SaveItemChoices {
                        preset,
                        choices: Box::new(choices),
                    })?;
                }
                UiRequest::ToggleSection(section) => {
                    let mut scope = self.scope;
                    match section {
                        PresetSection::Stratagems => scope.stratagems = !scope.stratagems,
                        PresetSection::Equipment => scope.equipment = !scope.equipment,
                    }
                    self.save_scope(scope);
                }
                UiRequest::ToggleEquipment(kind) => {
                    let mut scope = self.scope;
                    scope.equipment_items.toggle(kind);
                    self.save_scope(scope);
                }
                UiRequest::SetCompactList(compact)
                    if !self.exiting
                        && matches!(self.state, ActionState::Idle)
                        && !self.updates.installing()
                        && !self.ui.is_editing() =>
                {
                    match crate::config::save_compact_preset_list(&self.paths.config, compact) {
                        Ok(()) => self.ui.set_compact_list(compact),
                        Err(error) => {
                            warn!(error = %format!("{error:#}"), "failed to save compact list preference");
                            self.ui
                                .set_status(&format!("Could not save list view: {error:#}"));
                        }
                    }
                }
                UiRequest::SetCompactList(_) => {}
                UiRequest::Rename { preset, label } => {
                    match crate::preset::rename(&self.paths.presets, &preset, &label) {
                        Ok(()) => self.ui.set_label(&preset, label.trim().into()),
                        Err(error) => {
                            warn!(%preset, error = %format!("{error:#}"), "failed to rename preset");
                            self.ui
                                .set_status(&format!("Could not save preset name: {error:#}"));
                        }
                    }
                }
                UiRequest::OpenSettings(target) => self.open_settings(target),
                UiRequest::CheckUpdates => self.check_updates(),
                UiRequest::ViewUpdateNotes => self.view_update_notes(),
                UiRequest::ViewProject => self.open_link(env!("CARGO_PKG_REPOSITORY")),
                UiRequest::DownloadUpdate => {
                    if let Err(error) = self.download_update() {
                        self.update_failed(&error);
                    }
                }
                UiRequest::InstallUpdate => {
                    if let Err(error) = self.install_update() {
                        self.update_failed(&error);
                    }
                }
                UiRequest::SaveSettings {
                    draft,
                    startup_change,
                } => match self.save_settings(draft, startup_change) {
                    Ok(()) => {}
                    Err(error) if error.is::<crate::persistence::RollbackFailed>() => {
                        return Err(error.context(
                            "settings could not be restored; restart after checking the saved files",
                        ));
                    }
                    Err(error) => {
                        warn!(error = %format!("{error:#}"), "settings were not saved");
                        self.ui.settings_failed(&format!("{error:#}"));
                    }
                },
                UiRequest::CreatePreset { source }
                    if !self.exiting
                        && matches!(self.state, ActionState::Idle)
                        && !self.updates.installing()
                        && !self.ui.is_editing() =>
                {
                    let result = match source.as_deref() {
                        Some(id) => crate::preset::duplicate(&self.paths.presets, id),
                        None => crate::preset::add(&self.paths.presets),
                    };
                    match result {
                        Ok((id, library)) => {
                            self.shortcut_bindings
                                .presets
                                .insert(id.clone(), String::new());
                            self.ui.insert_preset(library, &id, source.as_deref());
                        }
                        Err(error) => {
                            warn!(source = source.as_deref(), error = %format!("{error:#}"), "failed to create preset");
                            self.ui
                                .set_status(&format!("Could not create preset: {error:#}"));
                        }
                    }
                }
                UiRequest::CreatePreset { .. } => {}
                UiRequest::CreateFolder
                    if !self.exiting
                        && matches!(self.state, ActionState::Idle)
                        && !self.updates.installing()
                        && !self.ui.is_editing() =>
                {
                    match crate::preset::add_folder(&self.paths.presets) {
                        Ok((folder, library)) => {
                            self.ui.insert_folder(library, &folder);
                        }
                        Err(error) => self.organization_updated(Err(error), "create folder"),
                    }
                }
                UiRequest::CreateFolder => {}
                UiRequest::ToggleFolder(folder)
                    if matches!(self.state, ActionState::Idle) && !self.updates.installing() =>
                {
                    match crate::preset::toggle_folder(&self.paths.presets, &folder) {
                        Ok(collapsed) => self.ui.set_folder_collapsed(&folder, collapsed),
                        Err(error) => {
                            warn!(%folder, error = %format!("{error:#}"), "could not save folder state");
                            self.ui
                                .set_status(&format!("Could not save folder state: {error:#}"));
                        }
                    }
                }
                UiRequest::ToggleFolder(_) => {}
                UiRequest::RenameFolder { folder, label }
                    if matches!(self.state, ActionState::Idle) && !self.updates.installing() =>
                {
                    let result = crate::preset::rename_folder(&self.paths.presets, &folder, &label);
                    self.organization_updated(result, "rename folder");
                }
                UiRequest::RenameFolder { .. } => {}
                UiRequest::UngroupFolder(folder)
                    if matches!(self.state, ActionState::Idle) && !self.updates.installing() =>
                {
                    let result = crate::preset::ungroup_folder(&self.paths.presets, &folder);
                    self.organization_updated(result, "ungroup folder");
                }
                UiRequest::UngroupFolder(_) => {}
                UiRequest::DeletePreset(preset) => {
                    if let Err(error) = self.delete_preset(&preset) {
                        warn!(%preset, error = %format!("{error:#}"), "failed to delete preset");
                        self.ui
                            .set_status(&format!("Could not finish deleting preset: {error:#}"));
                    }
                }
                UiRequest::MoveEntry {
                    entry,
                    parent,
                    before,
                } if matches!(self.state, ActionState::Idle) && !self.updates.installing() => {
                    let result = crate::preset::move_entry(
                        &self.paths.presets,
                        &entry,
                        parent.as_deref(),
                        before.as_deref(),
                    );
                    match result {
                        Ok(library) => {
                            self.ui.set_library(library, None);
                            self.ui.reveal_entry(&entry);
                        }
                        Err(error) => self.organization_updated(Err(error), "move entry"),
                    }
                }
                UiRequest::MoveEntry { .. } => self.ui.cancel_reorder(),
            }
        }
        Ok(())
    }

    fn organization_updated(
        &mut self,
        result: Result<crate::preset::LibraryMetadata>,
        operation: &str,
    ) {
        match result {
            Ok(library) => self.ui.set_library(library, None),
            Err(error) => {
                warn!(operation, error = %format!("{error:#}"), "could not update preset organization");
                self.ui.cancel_reorder();
                self.ui
                    .set_status(&format!("Could not {operation}: {error:#}"));
            }
        }
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

        let library = crate::preset::remove(&self.paths.presets, preset)?;
        self.shortcut_bindings = shortcuts;
        self.bindings = bindings;
        self.hotkeys = hotkeys;
        self.ui.remove_preset(library);
        Ok(())
    }

    pub(super) fn finish_pending_action(&mut self) -> Result<()> {
        let (action, target) = match &self.state {
            ActionState::Waiting { request, deadline } => {
                let PendingAction {
                    action,
                    target,
                    hotkey_id,
                    ..
                } = request;
                let owned_focus = self.ui.is_panel_active() || game_window::is_game_foreground();
                if !owned_focus || Instant::now() >= *deadline {
                    let action = action.clone();
                    let reason = if owned_focus {
                        "input was not released in time"
                    } else {
                        "focus changed"
                    };
                    debug!(?action, reason, "game action cancelled");
                    if self.ui.is_editing_items() {
                        self.ui.hide_panel()?;
                    } else {
                        self.close_panel()?;
                    }
                    self.ui.set_pending(false);
                    self.present_action_problem(&action, reason.into(), true)?;
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
                        request: request.clone(),
                        started: Instant::now(),
                    };
                    return Ok(());
                }
                // Restore first, then refresh placement; minimized geometry is not useful.
                (
                    action.clone(),
                    result.and_then(|_| WindowTarget::from_identity(*target)),
                )
            }
            ActionState::Activating { request, started } => {
                let PendingAction { action, target, .. } = request;
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
                debug!(
                    ?action,
                    confirmed = target.is_ok(),
                    confirmation_wait_s = started.elapsed().as_secs_f64(),
                    "game window activation confirmation finished"
                );
                (action.clone(), target)
            }
            _ => return Ok(()),
        };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                error!(?action, error = %format!("{error:#}"), "could not activate game window");
                save_failure_report(&self.paths.last_failure, &action, &error, None);
                self.ui.hide_panel()?;
                self.panel_return_target = None;
                self.present_action_problem(&action, format!("{error:#}"), false)?;
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
        let scan_cancel = match &action {
            GameAction::Scan { cancel, .. } => Some(cancel.clone()),
            GameAction::Preset { .. } => None,
        };
        self.panel_hotkey.set_enabled(scan_cancel.is_some())?;
        self.worker.send(Work::Run {
            action,
            target: target.identity(),
        })?;
        self.state = ActionState::Running { scan_cancel };
        Ok(())
    }
}
