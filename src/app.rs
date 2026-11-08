mod failure;
mod panel;
mod settings;
mod update;
mod worker;

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{Level, error, info, warn};

use crate::app_paths::AppPaths;
use crate::config::{AppConfig, ShortcutBindings, load_app_config};
use crate::preset::PresetScope;
use crate::preset_action::{PresetActionOptions, PresetCommand};
use crate::scan::{ScanCancellation, ScanScope};
use crate::{game_window, input, platform, ui};
use worker::{ActionWorker, Work, WorkCompletion, WorkerEvent};

const HOTKEY_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
const ACTIVATION_CONFIRM_TIMEOUT: Duration = Duration::from_millis(200);
const ACTION_COOLDOWN: Duration = Duration::from_millis(200);

struct PresetHotkeyBinding {
    hotkey: input::HotkeySpec,
    preset: String,
}

fn preset_hotkeys(config: &ShortcutBindings) -> Result<Vec<PresetHotkeyBinding>> {
    let mut bindings = Vec::new();
    for (preset, value) in &config.presets {
        if let Some(shortcut) = ShortcutBindings::parse(value)? {
            bindings.push(PresetHotkeyBinding {
                hotkey: shortcut.spec(1001 + bindings.len() as i32),
                preset: preset.clone(),
            });
        }
    }
    Ok(bindings)
}

fn panel_hotkey(config: &ShortcutBindings) -> Result<input::Hotkeys> {
    let specs: Vec<_> = ShortcutBindings::parse(&config.panel)?
        .map(|key| key.spec(1))
        .into_iter()
        .collect();
    input::Hotkeys::new(&specs)
}

pub fn run() -> Result<Option<PathBuf>> {
    let background = std::env::args_os()
        .skip(1)
        .any(|arg| arg == platform::startup::BACKGROUND_ARGUMENT);
    let Some(_instance) = platform::SingleInstance::try_acquire()? else {
        if !background {
            platform::show_notice(
                "HD2 Preset Helper",
                "HD2 Preset Helper is already running. Check the system tray.",
            );
        }
        return Ok(None);
    };
    let paths = AppPaths::resolve()?;
    let _log_guard = crate::logging::init(
        &paths.log,
        if cfg!(feature = "diagnostics") {
            Level::DEBUG
        } else {
            Level::INFO
        },
    )?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        diagnostics = cfg!(feature = "diagnostics"),
        "application starting"
    );
    crate::update::cleanup_previous_updates();
    let config_path = &paths.config;
    let presets_path = &paths.presets;
    let result = load_app_config(config_path).and_then(|loaded| {
        let captures_need_resaving = crate::preset::initialize(presets_path, || loaded.legacy_preset_metadata())?;
        // Valid v2 JSON is authoritative, even if this cleanup was interrupted earlier.
        if let Err(error) = crate::config::cleanup_legacy(config_path) {
            warn!(error = %format!("{error:#}"), "preset library is valid; obsolete configuration cleanup will be retried");
        }
        let config = loaded.config;
        if loaded.legacy_hotkeys {
            platform::show_notice(
                "HD2 Preset Helper - Configuration Updated",
                &format!("Your preset shortcuts were preserved and now APPLY saved presets only.\r\n\r\nTo save, open the panel and use Capture from game. Modifier keys alone no longer open the panel.\r\n\r\nPanel shortcut: {}\r\nYou can change or clear each shortcut in Settings > Shortcuts. Saved presets were not changed.",
                    if config.ui.panel_shortcut.is_empty() { "Unassigned (use the tray)" } else { &config.ui.panel_shortcut }),
            );
        }
        if captures_need_resaving {
            show_preset_format_updated();
        }
        run_with_config(config, &paths, background)
    });
    if let Err(error) = &result {
        error!(error = %format!("{error:#}"), "application terminated");
    }
    result
}

fn run_with_config(
    config: AppConfig,
    paths: &AppPaths,
    background: bool,
) -> Result<Option<PathBuf>> {
    let library = crate::preset::library_metadata(&paths.presets)?;
    let shortcut_bindings = ShortcutBindings {
        panel: config.ui.panel_shortcut.clone(),
        presets: library
            .presets
            .iter()
            .map(|info| (info.id.clone(), info.shortcut.clone()))
            .collect(),
    };
    shortcut_bindings.validate()?;
    let bindings = preset_hotkeys(&shortcut_bindings)?;
    let specs: Vec<_> = bindings.iter().map(|binding| binding.hotkey).collect();
    let hotkeys = input::Hotkeys::new(&specs)?;
    let panel_hotkey = panel_hotkey(&shortcut_bindings)?;
    let settings = PresetActionOptions {
        apply_in_saved_order: config.presets.apply_in_saved_order,
        auto_ready_up: config.presets.auto_ready_up,
        save_fallback_when_taken: config.presets.save_fallback_when_taken,
    };
    let worker = ActionWorker::start(paths.clone())?;
    let ui = ui::AppUi::new(
        library,
        config.ui.monitor,
        config.ui.language,
        config.ui.panel,
        paths.presets.clone(),
    )?;
    ui.set_scope(config.presets.scope);
    ui.set_compact_list(config.ui.compact_preset_list);
    ui.set_panel_shortcut(&shortcut_bindings.panel);
    let mut app = AppController {
        paths: paths.clone(),
        bindings,
        hotkeys,
        panel_hotkey,
        shortcut_bindings,
        scope: config.presets.scope,
        worker,
        settings,
        state: ActionState::Idle,
        exiting: false,
        ready_after: Instant::now(),
        prewarm_requested: false,
        panel_return_target: None,
        updates: update::Updates::new(config.updates.check_automatically),
        ui,
    };
    info!(
        hotkey_count = specs.len(),
        apply_in_saved_order = settings.apply_in_saved_order,
        auto_ready_up = settings.auto_ready_up,
        save_fallback_when_taken = settings.save_fallback_when_taken,
        "application ready"
    );
    // Native window placement and focus require an active UI event loop.
    let mut opening = true;
    ui::run(move || {
        if opening {
            if !app.ui.prepare_windows()? {
                return Ok(ControlFlow::Continue(()));
            }
            opening = false;
            if !background {
                app.open_panel()?;
            }
            app.initialize_updates();
            return Ok(ControlFlow::Continue(()));
        }
        app.tick()
    })
}

#[derive(Clone)]
pub(super) enum GameAction {
    Preset {
        preset: String,
        command: PresetCommand,
        options: PresetActionOptions,
    },
    Scan {
        scope: ScanScope,
        cancel: ScanCancellation,
    },
}

impl std::fmt::Debug for GameAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preset {
                preset, command, ..
            } => f
                .debug_struct("Preset")
                .field("preset", preset)
                .field("command", command)
                .finish(),
            Self::Scan { scope, .. } => f.debug_tuple("Scan").field(scope).finish(),
        }
    }
}

impl GameAction {
    fn report(&self) -> serde_json::Value {
        match self {
            Self::Preset {
                preset, command, ..
            } => serde_json::json!({
                "kind": "preset", "preset": preset, "command": format!("{command:?}"),
            }),
            Self::Scan { scope, .. } => serde_json::json!({
                "kind": "scan", "scope": format!("{scope:?}"),
            }),
        }
    }
}

#[derive(Clone)]
struct PendingAction {
    action: GameAction,
    target: crate::window::WindowIdentity,
    hotkey_id: Option<i32>,
    from_panel: bool,
}

enum ActionState {
    Idle,
    Checking(Option<PendingAction>),
    Confirming(PendingAction),
    Waiting {
        request: PendingAction,
        deadline: Instant,
    },
    Activating {
        request: PendingAction,
        started: Instant,
    },
    Running {
        scan_cancel: Option<ScanCancellation>,
    },
}

struct AppController {
    paths: AppPaths,
    bindings: Vec<PresetHotkeyBinding>,
    hotkeys: input::Hotkeys,
    panel_hotkey: input::Hotkeys,
    shortcut_bindings: ShortcutBindings,
    scope: PresetScope,
    worker: ActionWorker,
    settings: PresetActionOptions,
    state: ActionState,
    exiting: bool,
    ready_after: Instant,
    prewarm_requested: bool,
    panel_return_target: Option<crate::window::WindowIdentity>,
    updates: update::Updates,
    ui: ui::AppUi,
}

impl AppController {
    fn tick(&mut self) -> Result<ControlFlow<Option<PathBuf>>> {
        // Handle UI requests before shortcuts that use the updated settings.
        self.poll_ui()?;
        if let Some(executable) = self.poll_updates()? {
            return Ok(ControlFlow::Break(Some(executable)));
        }
        let game_active = game_window::is_game_foreground();
        let panel_active = self.ui.is_panel_active();
        let input_enabled = !self.exiting
            && !self.updates.installing()
            && !matches!(
                self.state,
                ActionState::Activating { .. } | ActionState::Running { .. }
            );
        let shortcuts_enabled = input_enabled
            && !matches!(
                self.state,
                ActionState::Checking(_) | ActionState::Confirming(_)
            )
            && !self.ui.shortcuts_blocked();
        self.hotkeys
            .set_enabled_where(|key| {
                shortcuts_enabled
                    && (game_active || panel_active)
                    && (!panel_active || !ui::uses_shortcut(key))
            })
            .with_context(|| {
                format!(
                    "failed to update hotkeys configured in {}",
                    self.paths.config.display()
                )
            })?;
        self.panel_hotkey.set_enabled_where(|key| {
            !self.exiting
                && ((input_enabled && !self.ui.panel_shortcut_blocked())
                    || matches!(
                        self.state,
                        ActionState::Running {
                            scan_cancel: Some(_)
                        }
                    ))
                && (!panel_active || !ui::uses_shortcut(key))
        })?;
        self.ui.set_shortcut_warning(
            &self
                .panel_hotkey
                .unavailable()
                .chain(self.hotkeys.unavailable())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        // Consume triggers before completion: keys pressed during the action cannot
        // turn into a new action just because the worker finished in this iteration.
        while let Some(id) = self.hotkeys.next_trigger() {
            self.handle_preset_hotkey(id)?;
        }
        while self.panel_hotkey.next_trigger().is_some() {
            if let ActionState::Running {
                scan_cancel: Some(cancel),
            } = &self.state
            {
                cancel.cancel();
            }
            if matches!(
                self.state,
                ActionState::Checking(_) | ActionState::Confirming(_)
            ) {
                self.cancel_action_check()?;
                if self.ui.is_panel_open() {
                    self.close_panel()?;
                }
            } else if matches!(self.state, ActionState::Idle) {
                if self.ui.is_panel_open() {
                    self.close_panel()?;
                } else {
                    self.open_panel()?;
                }
            }
        }
        // Preparation starts before opening a window that may take foreground.
        let prewarm = shortcuts_enabled
            && game_active
            && self
                .bindings
                .iter()
                .any(|binding| binding.hotkey.modifiers.is_down());
        if prewarm != self.prewarm_requested {
            self.prewarm_requested = prewarm;
            if matches!(self.state, ActionState::Idle) {
                self.worker.send(if prewarm {
                    game_window::foreground_game_window()
                        .ok()
                        .map_or(Work::Discard, |target| Work::Prepare(target.identity()))
                } else {
                    Work::Discard
                })?;
            }
        }
        let check_lost_focus = match &self.state {
            ActionState::Checking(Some(request)) => !self.action_check_owns_focus(request),
            ActionState::Confirming(_) => !self.ui.is_panel_active(),
            _ => false,
        };
        if check_lost_focus {
            self.cancel_action_check()?;
            self.close_panel()?;
        }
        // Hide idle browsing whenever focus leaves the panel, including to the game.
        // Waiting actions keep their separate focus/release handling below.
        if self.ui.is_panel_open()
            && !self.ui.is_panel_active()
            && matches!(self.state, ActionState::Idle)
        {
            self.close_panel()?;
        }
        self.finish_pending_action()?;
        if let Some(placement) = self.ui.tick()?
            && let Err(error) = crate::config::save_panel_placement(&self.paths.config, placement)
        {
            tracing::warn!(error = %format!("{error:#}"), "could not save panel placement");
        }
        loop {
            match self.worker.events.try_recv() {
                Ok(WorkerEvent::Progress(event)) => {
                    self.ui.event(event)?;
                }
                Ok(WorkerEvent::PreviewsLoaded(result)) => match result {
                    Ok(previews) => self.ui.set_previews(previews),
                    Err(error) => {
                        warn!(path = %self.paths.presets.display(), %error, "could not load preset previews");
                        self.ui.preview_failed(&error);
                    }
                },
                Ok(WorkerEvent::ItemCandidatesLoaded(result)) => {
                    if let Err(error) = &result {
                        warn!(%error, "could not load cached item choices");
                    }
                    self.ui.item_candidates_loaded(result);
                }
                Ok(WorkerEvent::ItemChoicesSaved { preset, result }) => {
                    self.ui.item_choices_saved(&preset, &result);
                    if result.is_ok() {
                        self.worker.send(Work::LoadPreviews)?;
                    }
                }
                Ok(WorkerEvent::PresetChecked(result)) => self.finish_action_check(result)?,
                Ok(WorkerEvent::ScanProgress(progress)) => self.ui.scan_progress(progress)?,
                Ok(WorkerEvent::Finished(completion)) => {
                    self.hotkeys.discard_pending();
                    self.panel_hotkey.discard_pending();
                    self.state = ActionState::Idle;
                    self.ui.set_pending(false);
                    self.ready_after = Instant::now() + ACTION_COOLDOWN;
                    match completion {
                        WorkCompletion::Preset(Ok(
                            crate::preset_action::PresetActionOutcome::Saved,
                        )) => {
                            platform::notify_preset_saved();
                        }
                        WorkCompletion::Scan(result) => self.finish_scan(result, false)?,
                        WorkCompletion::ScanCancelled => {
                            self.finish_scan(Err("Scan stopped".into()), true)?
                        }
                        WorkCompletion::Preset(_) => {}
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    anyhow::bail!("action worker stopped unexpectedly")
                }
            }
        }
        if self.exiting
            && !matches!(self.state, ActionState::Running { .. })
            && !self.updates.installing()
        {
            return Ok(ControlFlow::Break(None));
        }
        Ok(ControlFlow::Continue(()))
    }

    fn handle_preset_hotkey(&mut self, id: i32) -> Result<()> {
        if self.exiting
            || self.updates.installing()
            || self.ui.shortcuts_blocked()
            || !matches!(self.state, ActionState::Idle)
            || Instant::now() < self.ready_after
        {
            return Ok(());
        }
        let binding = self
            .bindings
            .iter()
            .find(|binding| binding.hotkey.id == id)
            .with_context(|| format!("unknown hotkey id: {id}"))?;
        let preset = binding.preset.clone();
        self.begin_action(preset, false, self.scope, Some(id))
    }

    fn request_exit(&mut self) -> Result<()> {
        info!("exit requested");
        self.exiting = true;
        self.hotkeys.set_enabled(false)?;
        self.panel_hotkey.set_enabled(false)?;
        self.cancel_action_check()?;
        if let ActionState::Running {
            scan_cancel: Some(cancel),
        } = &self.state
        {
            cancel.cancel();
        }
        // Keep installation visible while replacement finishes; never exit
        // midway through replacing program files.
        if !self.updates.installing() {
            self.ui.hide_panel()?;
        }
        if matches!(
            self.state,
            ActionState::Waiting { .. } | ActionState::Activating { .. }
        ) {
            self.state = ActionState::Idle;
            self.worker.send(Work::Discard)?;
        }
        Ok(())
    }
}

impl Drop for AppController {
    fn drop(&mut self) {
        if let ActionState::Running {
            scan_cancel: Some(cancel),
        } = &self.state
        {
            cancel.cancel();
        }
        // Release shortcuts before waiting for an in-flight action to finish.
        let _ = self.hotkeys.set_enabled(false);
        let _ = self.panel_hotkey.set_enabled(false);
        self.worker.shutdown();
    }
}

pub fn show_fatal_error(error: &anyhow::Error) {
    let error = format!("{error:#}")
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    let log_hint = AppPaths::resolve().map(|paths| paths.log).map_or_else(
        |_| String::new(),
        |path| {
            format!(
                "\r\n\r\nSee {} for more information if the log was created.",
                path.display()
            )
        },
    );
    let message = format!(
        "HD2 Preset Helper could not start or encountered a fatal error.\r\n\r\n{error}{log_hint}"
    );
    platform::show_error("HD2 Preset Helper", &message);
}

fn show_preset_format_updated() {
    platform::show_notice(
        "HD2 Preset Helper - Presets Updated",
        "Captures from this older preset format must be saved again in game. Your preset names, shortcuts and other settings were preserved.",
    );
}
