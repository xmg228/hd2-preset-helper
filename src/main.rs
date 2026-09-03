#![cfg_attr(windows, windows_subsystem = "windows")]

mod app_events;
mod assets;
mod automation;
mod capture;
mod game_window;
mod image_rect;
mod input;
mod item;
mod loadout;
mod overlay;
mod preset;
mod preset_action;
mod tray;
mod vision;
mod window;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{Level, debug, error, info, warn};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::prelude::*;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW,
};
use windows::core::PCWSTR;

use crate::app_events::{AppEvent, AppEventSink, OverlayPreset, OverlayPresetStatus};
use crate::assets::IconCatalog;
use crate::capture::CaptureSessionManager;
use crate::loadout::bind_loadout_region;
use crate::preset::{Preset, invalid_preset_reason, load_presets, validate_preset};
use crate::preset_action::{
    PresetActionConfig, PresetActionOutcome, PresetHotkeyBinding, handle_preset_hotkey,
    preset_hotkeys,
};
use crate::vision::RecognizerRuntime;

const DEFAULT_CONFIG_TOML: &str = include_str!("../data/config.toml");
const CONFIG_RELATIVE_PATH: &str = "data/config.toml";
const LOG_RELATIVE_PATH: &str = "data/app.log";
const PRESETS_RELATIVE_PATH: &str = "data/presets.json";
const LAST_FAILURE_DEBUG_PATH: &str = "data/debug/last_failure";
const MAX_PRESET_HOTKEYS: usize = 12;
const HOTKEY_WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const HOTKEY_RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AppConfig {
    presets: PresetsConfig,
    hotkey: HotkeyConfig,
    overlay: OverlaySettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PresetsConfig {
    #[serde(rename = "path")]
    legacy_path: Option<PathBuf>,
    auto_ready_up: bool,
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HotkeyConfig {
    modifiers: Vec<input::HotkeyModifier>,
    keys: Vec<input::Key>,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            modifiers: vec![input::HotkeyModifier::Ctrl, input::HotkeyModifier::Shift],
            keys: vec![
                input::Key::F7,
                input::Key::F8,
                input::Key::F9,
                input::Key::F10,
                input::Key::F11,
                input::Key::F12,
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OverlaySettings {
    enabled: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn main() -> ExitCode {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            show_fatal_error(&error);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let log_path = app_path(LOG_RELATIVE_PATH)?;
    let _log_guard = init_tracing(&log_path)?;
    let config_path = app_path(CONFIG_RELATIVE_PATH)?;
    let presets_path = app_path(PRESETS_RELATIVE_PATH)?;
    let result = load_app_config(&config_path).and_then(|(config, notify_reset)| {
        if notify_reset {
            show_config_reset(&config_path, &presets_path);
        }
        run_preset_hotkey_mode(config, &config_path, &presets_path)
    });
    if let Err(error) = &result {
        error!(error = %format!("{error:#}"), "application terminated");
    }
    result
}

fn run_preset_hotkey_mode(
    config: AppConfig,
    config_path: &Path,
    presets_path: &Path,
) -> Result<()> {
    let runtime = RecognizerRuntime::load()?;

    let hotkey_modifiers = input::HotkeyModifiers::new(config.hotkey.modifiers.clone())?;
    let bindings = preset_hotkeys(hotkey_modifiers, &config.hotkey.keys);
    let hotkeys: Vec<input::HotkeySpec> = bindings.iter().map(|binding| binding.hotkey).collect();
    let tray = tray::spawn()?;

    for binding in &bindings {
        debug!(key = ?binding.hotkey.key, preset = %binding.preset, "preset hotkey binding");
    }

    let registered_hotkeys = input::RegisteredHotkeys::register(&hotkeys).with_context(|| {
        format!(
            "failed to register hotkeys configured in {}",
            config_path.display()
        )
    })?;
    let events = if config.overlay.enabled {
        let presets = overlay_presets_for_bindings(
            presets_path,
            &bindings,
            &config.presets.labels,
            runtime.icon_catalog().as_ref(),
        );
        let events = overlay::spawn_overlay(hotkey_modifiers, Arc::clone(runtime.icon_catalog()))?;
        events.emit(AppEvent::PresetListUpdated { presets });
        events
    } else {
        AppEventSink::default()
    };

    info!(
        config = %config_path.display(),
        hotkey_count = bindings.len(),
        overlay = config.overlay.enabled,
        "application ready"
    );
    let action_config = PresetActionConfig {
        presets: presets_path,
        auto_ready_up: config.presets.auto_ready_up,
        events: &events,
    };
    let mut capture_session = CaptureSessionManager::new();
    let mut modifiers_were_down = false;
    let mut prewarm_suppressed = false;

    loop {
        let hotkey_id = loop {
            let hotkey_poll = registered_hotkeys.wait_timeout(HOTKEY_WAIT_POLL_INTERVAL)?;
            if tray.exit_requested() {
                info!("tray exit requested");
                return Ok(());
            }

            match hotkey_poll {
                input::HotkeyPoll::Triggered(hotkey_id) => {
                    prewarm_capture(&mut capture_session);
                    break hotkey_id;
                }
                input::HotkeyPoll::Timeout => {
                    let modifiers_down = hotkey_modifiers.is_down();
                    if modifiers_down && !modifiers_were_down && !prewarm_suppressed {
                        prewarm_capture(&mut capture_session);
                    } else if !modifiers_down && modifiers_were_down {
                        capture_session.discard();
                        prewarm_suppressed = false;
                    }
                    modifiers_were_down = modifiers_down;
                }
            }
        };
        let binding = bindings
            .iter()
            .find(|binding| binding.hotkey.id == hotkey_id)
            .with_context(|| format!("unknown hotkey id: {hotkey_id}"))?;

        let preset_name = &binding.preset;
        events.emit(AppEvent::HotkeyReleaseRequested {
            preset: preset_name.to_string(),
        });
        if !registered_hotkeys.wait_released(hotkey_id, HOTKEY_RELEASE_TIMEOUT) {
            capture_session.discard();
            modifiers_were_down = hotkey_modifiers.is_down();
            prewarm_suppressed = modifiers_were_down;
            warn!(
                preset = %preset_name,
                timeout = ?HOTKEY_RELEASE_TIMEOUT,
                "preset action cancelled because the hotkey was not released"
            );
            events.emit(AppEvent::PresetCancelled {
                preset: preset_name.to_string(),
                reason: "hotkey was not released in time".to_string(),
            });
            sleep(Duration::from_millis(200));
            continue;
        }

        let action_start = Instant::now();
        let outcome =
            handle_preset_hotkey(&runtime, &action_config, preset_name, &mut capture_session);
        if let Err(error) = &outcome {
            save_last_failure(&runtime, &mut capture_session, error);
        }
        capture_session.discard();
        if matches!(&outcome, Ok(PresetActionOutcome::Saved)) {
            unsafe {
                MessageBeep(MB_ICONINFORMATION).ok();
            }
        }
        modifiers_were_down = hotkey_modifiers.is_down();
        prewarm_suppressed = modifiers_were_down;
        if let Err(error) = outcome {
            let error = format!("{error:#}");
            error!(
                preset = %preset_name,
                elapsed = ?action_start.elapsed(),
                %error,
                "preset action failed"
            );
            action_config.events.emit(AppEvent::PresetFailed {
                preset: preset_name.to_string(),
                error,
            });
        }
        sleep(Duration::from_millis(200));
    }
}

fn prewarm_capture(capture_session: &mut CaptureSessionManager) {
    let result = game_window::find_game_window_once().and_then(|target| {
        capture_session.get_or_create(&target)?;
        Ok(())
    });

    if let Err(error) = result {
        debug!(
            error = %format!("{error:#}"),
            "capture preparation skipped"
        );
    }
}

fn save_last_failure(
    runtime: &RecognizerRuntime,
    capture_session: &mut CaptureSessionManager,
    action_error: &anyhow::Error,
) {
    let result = (|| -> Result<Option<PathBuf>> {
        let Some(capture) = capture_session.active_capture() else {
            return Ok(None);
        };
        let mut region = bind_loadout_region(capture, runtime.calibration())?;
        let image = region.capture()?;
        let directory = app_path(LAST_FAILURE_DEBUG_PATH)?;
        fs::create_dir_all(&directory).with_context(|| {
            format!(
                "failed to create failure debug directory {}",
                directory.display()
            )
        })?;
        image
            .save(directory.join("frame.png"))
            .context("failed to save failure debug frame")?;
        fs::write(directory.join("error.txt"), format!("{action_error:#}"))
            .context("failed to save failure debug error")?;
        Ok(Some(directory))
    })();

    match result {
        Ok(Some(directory)) => warn!(
            path = %directory.display(),
            "saved last preset failure diagnostics"
        ),
        Ok(None) => debug!("preset failure has no active capture frame to save"),
        Err(error) => warn!(
            error = %format!("{error:#}"),
            "failed to save preset failure diagnostics"
        ),
    }
}

fn load_app_config(config_path: &Path) -> Result<(AppConfig, bool)> {
    if !config_path.exists() {
        if let Some(parent) = config_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }
        fs::write(config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("failed to create {}", config_path.display()))?;
        info!(path = %config_path.display(), "default configuration created");
    }

    let text = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let mut config: AppConfig = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    let reset_action = config_reset_action(&config);
    if reset_action != ConfigResetAction::None {
        fs::write(config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("failed to reset {}", config_path.display()))?;
        config = toml::from_str(DEFAULT_CONFIG_TOML)
            .context("failed to parse the embedded default configuration")?;
        info!(
            path = %config_path.display(),
            notify = reset_action == ConfigResetAction::Notify,
            "legacy configuration reset"
        );
    }
    validate_hotkey_keys(&config.hotkey.keys)?;
    validate_preset_labels(&config.presets.labels)?;
    Ok((config, reset_action == ConfigResetAction::Notify))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigResetAction {
    None,
    Silent,
    Notify,
}

fn config_reset_action(config: &AppConfig) -> ConfigResetAction {
    let Some(legacy_path) = config.presets.legacy_path.as_deref() else {
        return ConfigResetAction::None;
    };

    let mut current_without_legacy_path = config.clone();
    current_without_legacy_path.presets.legacy_path = None;
    if legacy_path == Path::new(PRESETS_RELATIVE_PATH)
        && current_without_legacy_path == AppConfig::default()
    {
        ConfigResetAction::Silent
    } else {
        ConfigResetAction::Notify
    }
}

fn validate_hotkey_keys(keys: &[input::Key]) -> Result<()> {
    if keys.is_empty() || keys.len() > MAX_PRESET_HOTKEYS {
        bail!(
            "hotkey.keys must contain 1 to {} keys, got {}",
            MAX_PRESET_HOTKEYS,
            keys.len(),
        );
    }
    if let Some(key) = keys.iter().find(|key| !key.is_function_key()) {
        bail!("hotkey.keys only supports f1 through f12, got {key:?}");
    }

    Ok(())
}

fn validate_preset_labels(labels: &BTreeMap<String, String>) -> Result<()> {
    for (preset, label) in labels {
        let valid_key = preset
            .strip_prefix("preset_")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| (1..=MAX_PRESET_HOTKEYS).contains(index))
            .is_some_and(|index| preset == &format!("preset_{index}"));
        if !valid_key {
            bail!(
                "presets.labels key must be preset_1 through preset_{MAX_PRESET_HOTKEYS}, got {preset}"
            );
        }
        if label.chars().any(char::is_control) {
            bail!("presets.labels.{preset} must not contain control characters");
        }
    }
    Ok(())
}

fn overlay_presets_for_bindings(
    presets_path: &Path,
    bindings: &[PresetHotkeyBinding],
    labels: &BTreeMap<String, String>,
    catalog: &IconCatalog,
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

            let status = invalid_preset_reason(preset, catalog).map_or(
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
    let (stratagems, booster) = preset.map_or_else(
        || (Vec::new(), None),
        |preset| (preset.stratagems.clone(), preset.booster.clone()),
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
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true)
        .with_writer(writer)
        .compact()
        .with_filter(
            Targets::new()
                .with_target(module_path!(), Level::INFO)
                .with_target("hd2_preset_helper", Level::INFO),
        );

    tracing_subscriber::registry().with(file_layer).init();
    Ok(guard)
}

fn show_fatal_error(error: &anyhow::Error) {
    let error = format!("{error:#}")
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    let log_hint = app_path(LOG_RELATIVE_PATH).map_or_else(
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
    let title = wide_null("HD2 Preset Helper");
    let message = wide_null(&message);

    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

fn show_config_reset(config_path: &Path, presets_path: &Path) {
    let message = format!(
        "The configuration file was reset for this version.\r\n\r\nSaved presets in\r\n{}\r\nwere not changed.\r\n\r\nIf you previously customized the settings, configure them again in\r\n{}\r\n\r\nIf you used a custom preset path, move that preset file to the location shown above.",
        presets_path.display(),
        config_path.display(),
    );
    let title = wide_null("HD2 Preset Helper - Configuration Updated");
    let message = wide_null(&message);

    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn app_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    let executable = std::env::current_exe().context("failed to locate the executable")?;
    let directory = executable.parent().with_context(|| {
        format!(
            "executable has no parent directory: {}",
            executable.display()
        )
    })?;
    Ok(directory.join(path))
}
