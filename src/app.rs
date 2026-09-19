mod worker;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::mpsc::TryRecvError;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{Level, error, info, warn};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::prelude::*;

use crate::app_events::{AppCommand, AppEvent, AppEventSink, OverlayPreset, OverlayPresetStatus};
use crate::app_paths::AppPaths;
use crate::config::{AppConfig, load_app_config, save_preset_setting};
use crate::preset::{
    Preset, archive_legacy_preset_file, invalid_preset_reason, load_presets, validate_preset,
};
use crate::preset_action::PresetActionOptions;
use crate::{game_window, input, overlay, platform, tray};
use worker::{ActionWorker, Work, WorkerEvent};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const HOTKEY_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_COOLDOWN: Duration = Duration::from_millis(200);

struct PresetHotkeyBinding {
    hotkey: input::HotkeySpec,
    preset: String,
}

fn preset_hotkeys(
    modifiers: input::HotkeyModifiers,
    keys: &[input::Key],
) -> Vec<PresetHotkeyBinding> {
    keys.iter()
        .enumerate()
        .map(|(index, key)| PresetHotkeyBinding {
            hotkey: input::HotkeySpec {
                id: 1001 + index as i32,
                modifiers,
                key: *key,
            },
            preset: format!("preset_{}", index + 1),
        })
        .collect()
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
    let _log_guard = init_tracing(&paths.log)?;
    #[cfg(feature = "diagnostics")]
    crate::vision::init_diagnostics(&paths.diagnostic_scores)?;
    let config_path = &paths.config;
    let presets_path = &paths.presets;
    let result = load_app_config(config_path).and_then(|(config, notify_reset)| {
        let legacy_presets = archive_legacy_preset_file(presets_path)?;
        if notify_reset {
            show_config_reset(config_path, presets_path);
        }
        if let Some(backup_path) = legacy_presets {
            info!(
                path = %presets_path.display(),
                backup = %backup_path.display(),
                "legacy preset data archived"
            );
            show_preset_format_updated(&backup_path);
        }
        run_preset_hotkey_mode(config, &paths)
    });
    if let Err(error) = &result {
        error!(error = %format!("{error:#}"), "application terminated");
    }
    result
}

fn run_preset_hotkey_mode(config: AppConfig, paths: &AppPaths) -> Result<()> {
    let modifiers = input::HotkeyModifiers::new(config.hotkey.modifiers.clone())?;
    let bindings = preset_hotkeys(modifiers, &config.hotkey.keys);
    let specs: Vec<_> = bindings.iter().map(|binding| binding.hotkey).collect();
    let hotkeys = input::Hotkeys::new(&specs);
    let settings = PresetActionOptions {
        apply_in_saved_order: config.presets.apply_in_saved_order,
        auto_ready_up: config.presets.auto_ready_up,
        save_fallback_when_taken: config.presets.save_fallback_when_taken,
    };
    let tray = tray::spawn(settings)?;
    let worker = ActionWorker::start(paths.clone())?;
    let overlay = if config.overlay.enabled {
        Some(overlay::start(&paths.presets)?)
    } else {
        None
    };
    let events = overlay
        .as_ref()
        .map(|overlay| overlay.events().clone())
        .unwrap_or_default();
    if overlay.is_some() {
        events.emit(AppEvent::PresetListUpdated {
            presets: overlay_presets_for_bindings(
                &paths.presets,
                &bindings,
                &config.presets.labels,
            ),
        });
    }
    let mut app = AppController {
        paths: paths.clone(),
        bindings,
        modifiers,
        hotkeys,
        tray,
        events,
        overlay,
        worker,
        settings,
        state: ActionState::Idle,
        modifier_down: false,
        exiting: false,
        ready_after: Instant::now(),
    };
    info!(config = %paths.config.display(), hotkey_count = specs.len(),
        overlay = config.overlay.enabled,
        apply_in_saved_order = settings.apply_in_saved_order,
        auto_ready_up = settings.auto_ready_up,
        save_fallback_when_taken = settings.save_fallback_when_taken,
        "application ready");
    app.run()
}

enum ActionState {
    Idle,
    Waiting {
        hotkey_id: i32,
        preset: String,
        deadline: Instant,
    },
    Running,
}

struct AppController {
    paths: AppPaths,
    bindings: Vec<PresetHotkeyBinding>,
    modifiers: input::HotkeyModifiers,
    hotkeys: input::Hotkeys,
    tray: tray::TrayHandle,
    events: AppEventSink,
    overlay: Option<overlay::OverlayHandle>,
    worker: ActionWorker,
    settings: PresetActionOptions,
    state: ActionState,
    modifier_down: bool,
    exiting: bool,
    ready_after: Instant,
}

impl AppController {
    fn run(&mut self) -> Result<()> {
        let mut prewarm_requested = false;
        loop {
            let enabled = !self.exiting && game_window::is_game_foreground();
            self.hotkeys.set_enabled(enabled).with_context(|| {
                format!(
                    "failed to update hotkeys configured in {}",
                    self.paths.config.display()
                )
            })?;
            // Consume triggers before completion: keys pressed during the action cannot
            // turn into a new action just because the worker finished in this iteration.
            while let Some(id) = self.hotkeys.next_trigger() {
                self.command(AppCommand::PresetTriggered(id))?;
            }
            let down = self.modifiers.is_down();
            if down != self.modifier_down {
                self.command(AppCommand::ModifiersChanged(down))?;
            }
            let prewarm = enabled && down;
            if prewarm != prewarm_requested {
                prewarm_requested = prewarm;
                // Once triggered, preparation belongs to the action until it finishes.
                if matches!(self.state, ActionState::Idle) {
                    self.worker.send(if prewarm {
                        Work::Prepare
                    } else {
                        Work::Discard
                    })?;
                }
            }
            while let Some(event) = self.tray.try_event() {
                self.command(match event {
                    tray::TrayEvent::ToggleApplyInSavedOrder => AppCommand::ToggleApplyInSavedOrder,
                    tray::TrayEvent::ToggleAutoReadyUp => AppCommand::ToggleAutoReadyUp,
                    tray::TrayEvent::ToggleSaveFallbackWhenTaken => {
                        AppCommand::ToggleSaveFallbackWhenTaken
                    }
                    tray::TrayEvent::ExitRequested => AppCommand::Exit,
                })?;
            }
            loop {
                match self.worker.events.try_recv() {
                    Ok(WorkerEvent::Progress(event)) => self.events.emit(event),
                    Ok(WorkerEvent::Finished { saved }) => {
                        self.hotkeys.discard_pending();
                        self.state = ActionState::Idle;
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
                return Ok(());
            }
            if let ActionState::Waiting {
                hotkey_id,
                preset,
                deadline,
            } = &self.state
            {
                if enabled && self.hotkeys.is_released(*hotkey_id) {
                    self.worker.send(Work::Run {
                        preset: preset.clone(),
                        settings: self.settings,
                    })?;
                    self.state = ActionState::Running;
                } else if !enabled || Instant::now() >= *deadline {
                    let reason = if enabled {
                        "hotkey was not released in time"
                    } else {
                        "game lost focus"
                    };
                    warn!(preset = %preset, reason,
                        "preset action cancelled while waiting for hotkey release");
                    self.events.emit(AppEvent::PresetCancelled {
                        preset: preset.clone(),
                        reason: reason.into(),
                    });
                    self.worker.send(Work::Discard)?;
                    self.state = ActionState::Idle;
                    self.ready_after = Instant::now() + ACTION_COOLDOWN;
                }
            }
            sleep(EVENT_POLL_INTERVAL);
        }
    }

    fn command(&mut self, command: AppCommand) -> Result<()> {
        match command {
            AppCommand::PresetTriggered(id) => {
                if self.exiting
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
                self.worker.send(Work::Prepare)?;
                self.events.emit(AppEvent::HotkeyReleaseRequested {
                    preset: preset.clone(),
                });
                self.state = ActionState::Waiting {
                    hotkey_id: id,
                    preset,
                    deadline: Instant::now() + HOTKEY_RELEASE_TIMEOUT,
                };
            }
            AppCommand::ModifiersChanged(down) => {
                self.modifier_down = down;
                self.events.emit(AppEvent::ModifiersChanged(down));
            }
            AppCommand::Exit => {
                info!("tray exit requested");
                self.exiting = true;
                self.hotkeys.set_enabled(false)?;
                if matches!(self.state, ActionState::Waiting { .. }) {
                    self.state = ActionState::Idle;
                    self.worker.send(Work::Discard)?;
                }
            }
            toggle => {
                let (key, value) = match toggle {
                    AppCommand::ToggleApplyInSavedOrder => {
                        self.settings.apply_in_saved_order = !self.settings.apply_in_saved_order;
                        ("apply_in_saved_order", self.settings.apply_in_saved_order)
                    }
                    AppCommand::ToggleAutoReadyUp => {
                        self.settings.auto_ready_up = !self.settings.auto_ready_up;
                        ("auto_ready_up", self.settings.auto_ready_up)
                    }
                    AppCommand::ToggleSaveFallbackWhenTaken => {
                        self.settings.save_fallback_when_taken =
                            !self.settings.save_fallback_when_taken;
                        (
                            "save_fallback_when_taken",
                            self.settings.save_fallback_when_taken,
                        )
                    }
                    _ => unreachable!(),
                };
                self.tray.update_settings(self.settings);
                if let Err(error) = save_preset_setting(&self.paths.config, key, value) {
                    warn!(setting = key, value, error = %format!("{error:#}"),
                        "failed to persist tray setting; it remains active for this session");
                }
                info!(setting = key, value, "preset setting changed from tray");
            }
        }
        Ok(())
    }
}

impl Drop for AppController {
    fn drop(&mut self) {
        // Release shortcuts before waiting for an in-flight action to finish.
        let _ = self.hotkeys.set_enabled(false);
        self.worker.shutdown();
        drop(self.overlay.take());
    }
}

fn overlay_presets_for_bindings(
    presets_path: &Path,
    bindings: &[PresetHotkeyBinding],
    labels: &BTreeMap<String, String>,
) -> Vec<OverlayPreset> {
    let presets = match load_presets(presets_path) {
        Ok(presets) => presets,
        Err(error) => {
            let error = format!("{error:#}");
            warn!(
                path = %presets_path.display(),
                %error,
                "failed to load overlay presets"
            );
            return bindings
                .iter()
                .map(|binding| {
                    overlay_preset(
                        binding,
                        None,
                        labels,
                        OverlayPresetStatus::Invalid(error.clone()),
                    )
                })
                .collect();
        }
    };

    bindings
        .iter()
        .map(|binding| {
            let Some(preset) = presets.get(&binding.preset) else {
                return overlay_preset(binding, None, labels, OverlayPresetStatus::NotSaved);
            };

            if let Err(error) = validate_preset(&binding.preset, preset) {
                let error = format!("{error:#}");
                warn!(
                    preset = %binding.preset,
                    %error,
                    "invalid overlay preset summary"
                );
                return overlay_preset(binding, None, labels, OverlayPresetStatus::Invalid(error));
            }

            let status = invalid_preset_reason(presets_path, preset).map_or(
                OverlayPresetStatus::Ready,
                |reason| {
                    warn!(
                        preset = %binding.preset,
                        %reason,
                        "preset references invalid icon items"
                    );
                    OverlayPresetStatus::Invalid(reason)
                },
            );

            overlay_preset(binding, Some(preset), labels, status)
        })
        .collect()
}

fn overlay_preset(
    binding: &PresetHotkeyBinding,
    preset: Option<&Preset>,
    labels: &BTreeMap<String, String>,
    status: OverlayPresetStatus,
) -> OverlayPreset {
    let (stratagems, booster, fallback_booster) = preset.map_or_else(
        || (Vec::new(), None, None),
        |preset| {
            (
                preset
                    .stratagems
                    .iter()
                    .map(|template| template.path.clone())
                    .collect(),
                preset
                    .booster
                    .as_ref()
                    .map(|template| template.path.clone()),
                preset
                    .fallback_booster
                    .as_ref()
                    .map(|template| template.path.clone()),
            )
        },
    );
    OverlayPreset {
        key_label: binding.hotkey.key.name(),
        name: binding.preset.clone(),
        label: labels
            .get(&binding.preset)
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
            .map(str::to_owned),
        stratagems,
        booster,
        fallback_booster,
        status,
    }
}

fn init_tracing(path: &Path) -> Result<WorkerGuard> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }
    let file = fs::File::create(path)
        .with_context(|| format!("failed to create log {}", path.display()))?;
    let (writer, guard) = NonBlockingBuilder::default().lossy(false).finish(file);
    let level = if cfg!(feature = "diagnostics") {
        Level::DEBUG
    } else {
        Level::INFO
    };
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true)
        .with_writer(writer)
        .compact()
        .with_filter(
            Targets::new()
                .with_target(env!("CARGO_BIN_NAME"), level)
                .with_target("hd2_preset_helper", level),
        );

    tracing_subscriber::registry().with(file_layer).init();
    Ok(guard)
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

fn show_config_reset(config_path: &Path, presets_path: &Path) {
    let message = format!(
        "The configuration file was reset for this version.\r\n\r\nSaved presets in\r\n{}\r\nwere not changed.\r\n\r\nIf you previously customized the settings, configure them again in\r\n{}\r\n\r\nIf you used a custom preset path, move that preset file to the location shown above.",
        presets_path.display(),
        config_path.display(),
    );
    platform::show_notice("HD2 Preset Helper - Configuration Updated", &message);
}

fn show_preset_format_updated(backup_path: &Path) {
    let message = format!(
        "The preset format changed in this version.\r\n\r\nPresets created by an earlier version cannot be used and must be recreated in game.\r\n\r\nThe old preset file was backed up to:\r\n{}\r\n\r\nYour configuration was not changed.",
        backup_path.display(),
    );
    platform::show_notice("HD2 Preset Helper - Presets Updated", &message);
}
