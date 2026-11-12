//! Panel-to-automation handoff. Only the controller activates the game.
use anyhow::{Result, ensure};
use std::time::Instant;
use tracing::{debug, error, warn};

use super::failure::save_failure_report;
use super::{
    ACTION_COOLDOWN, ACTIVATION_CONFIRM_TIMEOUT, ActionState, AppController, GameAction,
    HOTKEY_RELEASE_TIMEOUT, NOTICE_REVISION, PendingAction, Work, preset_hotkeys,
};
use crate::{
    app_events::AppEvent,
    automation::Cancellation,
    game_window, input,
    preset::{PresetScope, PresetSection},
    preset_action::PresetCommand,
    scan::{ScanScope, ScanSummary},
    ui::UiRequest,
    window::WindowTarget,
};

impl AppController {
    pub(super) fn open_panel(&mut self) -> Result<()> {
        if self.exiting
            || !matches!(
                self.state,
                ActionState::Idle | ActionState::Checking(None) | ActionState::Confirming(_)
            )
        {
            return Ok(());
        }
        if self.ui.is_panel_active() {
            return Ok(());
        }
        // Closing returns to the game only if opening the panel took its focus.
        // Applying or saving resolves its own target, even when opened elsewhere.
        let game = game_window::foreground_game_window().ok();
        self.panel_return_target = game.as_ref().map(|target| target.identity());
        if self.ui.is_panel_open() {
            if let Some(game) = &game {
                self.ui.set_game_anchor(game.client_origin())?;
            }
            return self.ui.activate_panel();
        }
        let anchor = game.as_ref().map(|target| target.client_origin());
        self.ui.show_panel(anchor)?;
        if !self.updates.installing() {
            if self.notice_pending {
                self.ui.show_quick_start();
            }
            self.worker.send(Work::LoadPreviews)?;
        }
        Ok(())
    }

    pub(super) fn close_panel(&mut self) -> Result<()> {
        if matches!(self.state, ActionState::Confirming(_)) {
            self.cancel_current_action("confirmation closed")?;
        }
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
                cancel: Cancellation::default(),
            },
            hotkey_id,
        )
    }

    fn begin_scan(&mut self, scope: ScanScope) -> Result<()> {
        self.begin_game_action(
            GameAction::Scan {
                scope,
                cancel: Cancellation::default(),
            },
            None,
        )
    }

    fn begin_game_action(&mut self, action: GameAction, hotkey_id: Option<i32>) -> Result<()> {
        let editor_scan = matches!(
            action,
            GameAction::Scan {
                scope: ScanScope::Missing | ScanScope::One(_),
                ..
            }
        ) && self.ui.can_refresh_items();
        if self.exiting
            || self.updates.installing()
            || self.ui.is_quick_start_open()
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
            game_window::foreground_game_window()
                .map(|window| (window.identity(), Some(window.client_origin())))
        } else {
            game_window::find_game_window().map(|target| (target, None))
        };
        let (target, anchor) = match target {
            Ok(target) => target,
            Err(error) => {
                error!(?action, error = %format!("{error:#}"), "could not locate game window");
                save_failure_report(&self.paths.last_failure, &action, &error, None);
                self.present_action_problem(&action, format!("{error:#}"), false)?;
                return Ok(());
            }
        };
        if let Some(anchor) = anchor {
            self.ui.set_game_anchor(anchor)?;
        }
        let request = PendingAction {
            action,
            target,
            hotkey_id,
            panel_was_open: self.ui.is_panel_open(),
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

    pub(super) fn action_owns_focus(&self, request: &PendingAction) -> bool {
        self.ui.is_panel_active()
            || game_window::foreground_game_window()
                .is_ok_and(|window| window.identity() == request.target)
    }

    pub(super) fn finish_action_check(
        &mut self,
        result: std::result::Result<usize, String>,
    ) -> Result<()> {
        let cancel_reason = match &self.state {
            ActionState::Checking(Some(request)) if !self.action_owns_focus(request) => {
                Some("focus changed")
            }
            ActionState::Checking(Some(request))
                if matches!(result, Ok(1..))
                    && request.panel_was_open
                    && (!self.ui.is_panel_open() || self.ui.is_panel_minimized()) =>
            {
                Some("panel closed")
            }
            _ => None,
        };
        if let Some(reason) = cancel_reason {
            self.cancel_current_action(reason)?;
        }
        let ActionState::Checking(request) = &mut self.state else {
            return Ok(());
        };
        let request = request.take();
        self.state = ActionState::Idle;
        self.ui.set_pending(false);
        let Some(request) = request.filter(|_| !self.exiting) else {
            return Ok(());
        };
        if matches!(result, Ok(0)) {
            return self.wait_for_action(request);
        }
        self.worker.send(Work::Discard)?;
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
        if !self.ui.is_panel_active() {
            self.panel_return_target = Some(request.target);
        }
        self.ui.show_panel(Some(target.client_origin()))?;
        self.ui.set_scan_confirmation(preset, count);
        self.state = ActionState::Confirming(request);
        Ok(())
    }

    pub(super) fn cancel_current_action(&mut self, reason: &str) -> Result<bool> {
        let action = match &mut self.state {
            ActionState::Idle | ActionState::Checking(None) => return Ok(false),
            ActionState::Running { cancel } => {
                cancel.cancel();
                return Ok(true);
            }
            ActionState::Checking(request) => request.take().unwrap().action,
            ActionState::Confirming(request)
            | ActionState::Waiting { request, .. }
            | ActionState::Activating { request, .. } => {
                let action = request.action.clone();
                self.state = ActionState::Idle;
                action
            }
        };
        debug!(?action, reason, "game action cancelled");
        self.ui.dismiss_scan_confirmation();
        self.ui
            .set_pending(matches!(self.state, ActionState::Checking(_)));
        self.hotkeys.discard_pending();
        self.panel_hotkey.discard_pending();
        self.ready_after = Instant::now() + ACTION_COOLDOWN;
        self.worker.send(Work::Discard)?;
        if !self.exiting {
            self.present_action_problem(&action, reason.into(), true)?;
        }
        Ok(true)
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
        if !cancelled && let Ok(game) = game_window::foreground_game_window() {
            self.panel_return_target = Some(game.identity());
            self.ui.show_panel(Some(game.client_origin()))?;
        }
        if self.ui.is_editing_items() {
            self.ui
                .resume_item_editor(result.as_ref().err().map(String::as_str));
            self.worker.send(Work::LoadItemCandidates)?;
        }
        self.ui.scan_finished(result, cancelled)?;
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
                UiRequest::HidePanel => self.close_panel()?,
                UiRequest::ConfirmScan => self.confirm_scan()?,
                UiRequest::CancelScan => {
                    if let ActionState::Confirming(request) = &self.state {
                        let hide_panel = !request.panel_was_open;
                        self.cancel_current_action("scan cancelled")?;
                        if hide_panel {
                            self.close_panel()?;
                        }
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
                UiRequest::RefreshItems(kind)
                    if self.ui.is_panel_active() && self.ui.can_refresh_items() =>
                {
                    self.begin_scan(ScanScope::One(kind))?;
                }
                UiRequest::RefreshItems(_) => {}
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
                UiRequest::ViewQuickStart
                    if !self.exiting
                        && !self.updates.installing()
                        && matches!(self.state, ActionState::Idle)
                        && self.ui.is_panel_active()
                        && !self.ui.is_editing_items()
                        && !self.ui.is_scan_confirm_open() =>
                {
                    self.ui.show_quick_start();
                }
                UiRequest::ViewQuickStart => {}
                UiRequest::AcknowledgeQuickStart => {
                    if self.notice_pending {
                        self.notice_pending = false;
                        if let Err(error) =
                            crate::config::save_notice_revision(&self.paths.config, NOTICE_REVISION)
                        {
                            warn!(error = %format!("{error:#}"), "could not save notice acknowledgement");
                        }
                    }
                    self.ui.dismiss_quick_start();
                }
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
        let (action, target, keep_panel_open) = match &self.state {
            ActionState::Waiting { request, deadline } => {
                let PendingAction {
                    action,
                    target,
                    hotkey_id,
                    panel_was_open,
                    ..
                } = request;
                let owned_focus = self.action_owns_focus(request);
                if !owned_focus || Instant::now() >= *deadline {
                    let hide_panel = !self.keep_panel_open || !panel_was_open;
                    let reason = if owned_focus {
                        "input was not released in time"
                    } else {
                        "focus changed"
                    };
                    self.cancel_current_action(reason)?;
                    if hide_panel {
                        if self.ui.is_editing_items() {
                            self.ui.hide_panel()?;
                        } else {
                            self.close_panel()?;
                        }
                    }
                    return Ok(());
                }
                if !input::activation_released()
                    || hotkey_id.is_some_and(|id| !self.hotkeys.is_released(id))
                {
                    return Ok(());
                }
                self.ui.set_always_on_top(false)?;
                self.panel_return_target = None;
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
                    self.keep_panel_open && *panel_was_open,
                )
            }
            ActionState::Activating { request, started } => {
                let PendingAction {
                    action,
                    target,
                    panel_was_open,
                    ..
                } = request;
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
                (
                    action.clone(),
                    target,
                    self.keep_panel_open && *panel_was_open,
                )
            }
            _ => return Ok(()),
        };
        let target = match target {
            Ok(target) => target,
            Err(error) => {
                error!(?action, error = %format!("{error:#}"), "could not activate game window");
                save_failure_report(&self.paths.last_failure, &action, &error, None);
                if !keep_panel_open {
                    self.ui.hide_panel()?;
                }
                self.present_action_problem(&action, format!("{error:#}"), false)?;
                self.ui.set_pending(false);
                self.worker.send(Work::Discard)?;
                self.state = ActionState::Idle;
                self.ready_after = Instant::now() + ACTION_COOLDOWN;
                return Ok(());
            }
        };
        self.ui.set_game_anchor(target.client_origin())?;
        if !keep_panel_open {
            self.ui.hide_panel()?;
        }
        // Unregister before the worker can inject its first key, not on the next tick.
        self.hotkeys.set_enabled(false)?;
        let cancel = match &action {
            GameAction::Scan { cancel, .. } | GameAction::Preset { cancel, .. } => cancel.clone(),
        };
        self.panel_hotkey.set_enabled(true)?;
        self.worker.send(Work::Run {
            action,
            target: target.identity(),
        })?;
        self.state = ActionState::Running { cancel };
        Ok(())
    }
}
