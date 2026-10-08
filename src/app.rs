mod panel;
mod settings;
mod worker;

use std::path::Path;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{Level, error, info};

use crate::app_paths::AppPaths;
use crate::config::{AppConfig, ShortcutBindings, load_app_config};
use crate::preset::{PresetScope, archive_legacy_preset_file};
use crate::preset_action::{PresetActionOptions, PresetCommand};
use crate::{game_window, input, platform, ui};
use worker::{ActionWorker, Work, WorkerEvent};

const HOTKEY_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
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

pub fn run() -> Result<()> {
    let Some(_instance) = platform::SingleInstance::try_acquire()? else {
        platform::show_notice(
            "HD2 Preset Helper",
            "HD2 Preset Helper is already running. Check the system tray.",
        );
        return Ok(());
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
    #[cfg(feature = "diagnostics")]
    crate::vision::init_diagnostics(&paths.diagnostic_scores)?;
    let config_path = &paths.config;
    let presets_path = &paths.presets;
    let result = load_app_config(config_path).and_then(|(config, migrated)| {
        let legacy_presets = archive_legacy_preset_file(presets_path)?;
        if migrated {
            platform::show_notice(
                "HD2 Preset Helper - Configuration Updated",
                &format!("Your preset shortcuts were preserved and now APPLY saved presets only.\r\n\r\nTo save, open the panel and use Save preset or Ctrl+S. Modifier keys alone no longer open the panel.\r\n\r\nPanel shortcut: {}\r\nYou can change or clear each shortcut in Settings > Shortcuts. Saved presets were not changed.",
                    if config.ui.panel_shortcut.is_empty() { "Unassigned (use the tray)" } else { &config.ui.panel_shortcut }),
            );
        }
        if let Some(backup_path) = legacy_presets {
            info!(
                path = %presets_path.display(),
                backup = %backup_path.display(),
                "legacy preset data archived"
            );
            show_preset_format_updated(&backup_path);
        }
        run_with_config(config, &paths)
    });
    if let Err(error) = &result {
        error!(error = %format!("{error:#}"), "application terminated");
    }
    result
}

fn run_with_config(config: AppConfig, paths: &AppPaths) -> Result<()> {
    let shortcut_bindings = config.shortcut_bindings();
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
    let preset_info = config
        .presets
        .slots
        .iter()
        .map(|(name, slot)| ui::PresetInfo {
            name: name.clone(),
            label: slot.label.trim().into(),
            shortcut: slot.shortcut.clone(),
        })
        .collect();
    let ui = ui::AppUi::new(
        preset_info,
        config.ui.monitor,
        config.ui.language,
        paths.presets.clone(),
    )?;
    ui.set_scope(config.presets.scope);
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
        ui,
    };
    info!(config = %paths.config.display(), hotkey_count = specs.len(),
        apply_in_saved_order = settings.apply_in_saved_order,
        auto_ready_up = settings.auto_ready_up,
        save_fallback_when_taken = settings.save_fallback_when_taken,
        "application ready");
    // Native window placement and focus require an active UI event loop.
    let mut opening = true;
    ui::run(move || {
        if std::mem::take(&mut opening) {
            app.open_panel()?;
            return Ok(false);
        }
        app.tick()
    })
}

enum ActionState {
    Idle,
    Waiting {
        target: crate::window::WindowIdentity,
        hotkey_id: Option<i32>,
        preset: String,
        command: PresetCommand,
        deadline: Instant,
    },
    Running,
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
    ui: ui::AppUi,
}

impl AppController {
    fn tick(&mut self) -> Result<bool> {
        // Handle UI requests before shortcuts that use the updated settings.
        self.poll_ui()?;
        let game_active = game_window::is_game_foreground();
        let panel_active = self.ui.is_panel_active();
        let shortcuts_enabled = !self.exiting
            && !matches!(self.state, ActionState::Running)
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
            shortcuts_enabled && (!panel_active || !ui::uses_shortcut(key))
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
            if matches!(self.state, ActionState::Idle) {
                if self.ui.is_panel_visible() {
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
        // Hide idle browsing whenever focus leaves the panel, including to the game.
        // Waiting actions keep their separate focus/release handling below.
        if self.ui.is_panel_visible()
            && !self.ui.is_panel_active()
            && matches!(self.state, ActionState::Idle)
        {
            self.close_panel()?;
        }
        self.finish_pending_action()?;
        self.ui.tick()?;
        loop {
            match self.worker.events.try_recv() {
                Ok(WorkerEvent::Progress(event)) => {
                    self.ui.event(event)?;
                }
                Ok(WorkerEvent::PreviewsLoaded(result)) => match result {
                    Ok(previews) => self.ui.set_previews(previews),
                    Err(error) => self.ui.preview_failed(&error),
                },
                Ok(WorkerEvent::Finished { saved }) => {
                    self.hotkeys.discard_pending();
                    self.panel_hotkey.discard_pending();
                    self.state = ActionState::Idle;
                    self.ui.set_pending(false);
                    self.ready_after = Instant::now() + ACTION_COOLDOWN;
                    if saved {
                        platform::notify_preset_saved();
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    anyhow::bail!("action worker stopped unexpectedly")
                }
            }
        }
        if self.exiting && !matches!(self.state, ActionState::Running) {
            return Ok(true);
        }
        Ok(false)
    }

    fn handle_preset_hotkey(&mut self, id: i32) -> Result<()> {
        if self.exiting
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
        self.begin_action(preset, false, Some(id))
    }

    fn request_exit(&mut self) -> Result<()> {
        info!("exit requested");
        self.exiting = true;
        self.hotkeys.set_enabled(false)?;
        self.panel_hotkey.set_enabled(false)?;
        self.ui.hide_panel()?;
        if matches!(self.state, ActionState::Waiting { .. }) {
            self.state = ActionState::Idle;
            self.worker.send(Work::Discard)?;
        }
        Ok(())
    }
}

impl Drop for AppController {
    fn drop(&mut self) {
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

fn show_preset_format_updated(backup_path: &Path) {
    let message = format!(
        "The preset format changed in this version.\r\n\r\nPresets created by an earlier version cannot be used and must be recreated in game.\r\n\r\nThe old preset file was backed up to:\r\n{}\r\n\r\nYour configuration was not changed.",
        backup_path.display(),
    );
    platform::show_notice("HD2 Preset Helper - Presets Updated", &message);
}
