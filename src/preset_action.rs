use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, info_span, warn};

use crate::app_events::{AppEvent, AppEventSink};
use crate::automation::AutomationSession;
use crate::capture::CaptureSessionManager;
use crate::game_window::find_game_window;
use crate::input;
use crate::loadout::{
    UiState, apply_booster_from_home, apply_empty_loadout_preset, bind_loadout_region,
    collect_current_preset, detect_ui_state, home_booster_needs_warning, scan_loadout_home,
    wait_for_ui_state,
};
use crate::permissions;
use crate::preset::{Preset, invalid_preset_reason, load_preset, save_preset};
use crate::vision::RecognizerRuntime;

const READY_UP_HOLD_MS: u64 = 45;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetActionOutcome {
    Saved,
    Applied,
}

pub struct PresetHotkeyBinding {
    pub hotkey: input::HotkeySpec,
    pub preset: String,
}

pub fn preset_hotkeys(
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

pub struct PresetActionConfig<'a> {
    pub presets: &'a Path,
    pub apply_in_saved_order: bool,
    pub auto_ready_up: bool,
    pub events: &'a AppEventSink,
}

pub fn handle_preset_hotkey(
    runtime: &RecognizerRuntime,
    config: &PresetActionConfig<'_>,
    preset_name: &str,
    capture_session: &mut CaptureSessionManager,
) -> Result<PresetActionOutcome> {
    let span = info_span!("preset_action");
    let _guard = span.enter();

    let action_start = Instant::now();
    info!(preset = %preset_name, "preset action started");
    config.events.emit(AppEvent::PresetStarted {
        preset: preset_name.to_string(),
    });

    let game_window = find_game_window().context("failed to locate Helldivers window")?;
    permissions::ensure_input_access()?;
    let (client_w, client_h) = game_window.client_size();
    debug!(client_w, client_h, "game window ready");

    let capture_start = Instant::now();
    let capture = capture_session
        .get_or_create(&game_window)
        .context("failed to get capture session")?;
    debug!(
        elapsed = ?capture_start.elapsed(),
        "capture session ready"
    );
    let region = bind_loadout_region(capture, runtime.calibration())
        .context("failed to bind loadout capture region")?;
    let mut automation = AutomationSession::new(region, game_window)
        .context("failed to start automation session")?;

    let (initial_result, ui_state) = {
        let initial_result =
            scan_loadout_home(&mut automation, runtime).context("failed to scan loadout home")?;
        let ui_state = detect_ui_state(&initial_result);
        debug!(ui_state = %ui_state.label(), "detected loadout UI state");
        config.events.emit(AppEvent::UiStateDetected {
            state: ui_state.label(),
        });
        (initial_result, ui_state)
    };

    let (outcome, ready_up_after_apply, completion_warning) = match ui_state {
        UiState::HomeFilled => {
            let booster_needs_warning = home_booster_needs_warning(&initial_result);
            let preset = collect_current_preset(&initial_result)
                .context("failed to collect current preset")?;
            save_current_preset(config, preset_name, &preset)?;
            let warning = booster_needs_warning.then(|| {
                warn!(
                    preset = %preset_name,
                    "home booster appears filled but was not recognized; preset saved without a booster"
                );
                "Booster not recognized; saved without it".to_string()
            });
            (PresetActionOutcome::Saved, false, warning)
        }

        UiState::HomeMixed => {
            bail!(
                "loadout home is partially filled; clear or complete the loadout before saving or applying a preset"
            );
        }

        UiState::HomeEmpty => {
            let preset = load_named_preset(runtime, config, preset_name)?;
            debug!(
                stratagem_count = preset.stratagems.len(),
                booster_present = preset.booster.is_some(),
                "applying preset from empty home"
            );
            log_preset_contents(&preset);
            apply_empty_loadout_preset(
                runtime,
                &mut automation,
                config.events,
                &preset.stratagems,
                config.apply_in_saved_order,
            )
            .context("failed to apply stratagems from empty home")?;
            apply_booster_if_present(runtime, &mut automation, config, &preset)?;
            (PresetActionOutcome::Applied, preset.booster.is_some(), None)
        }

        UiState::List(_) | UiState::Unknown => {
            bail!("loadout home not detected; return to the loadout home before using a preset");
        }
    };

    if config.auto_ready_up && ready_up_after_apply {
        wait_for_ui_state(
            &mut automation,
            runtime,
            UiState::HomeFilled,
            Duration::from_millis(1500),
        )
        .context("booster was selected but the loadout home did not stabilize before starting")?;
        debug!("booster preset applied; sending READY UP key");
        automation.tap_key(input::Key::B, READY_UP_HOLD_MS)?;
    }

    config.events.emit(AppEvent::PresetDone {
        preset: preset_name.to_string(),
        warning: completion_warning,
    });
    info!(
        preset = %preset_name,
        elapsed = ?action_start.elapsed(),
        "preset action completed"
    );
    Ok(outcome)
}

fn load_named_preset(
    runtime: &RecognizerRuntime,
    config: &PresetActionConfig<'_>,
    preset_name: &str,
) -> Result<Preset> {
    let preset = load_preset(config.presets, preset_name)
        .with_context(|| format!("failed to load preset \"{preset_name}\""))?;

    if let Some(reason) = invalid_preset_reason(&preset, runtime.icon_catalog().as_ref()) {
        bail!("preset \"{preset_name}\" is invalid: {reason}");
    }

    Ok(preset)
}

fn save_current_preset(
    config: &PresetActionConfig<'_>,
    preset_name: &str,
    preset: &Preset,
) -> Result<()> {
    debug!(
        stratagem_count = preset.stratagems.len(),
        booster_present = preset.booster.is_some(),
        "saving current preset"
    );
    log_preset_contents(preset);

    save_preset(config.presets, preset_name, preset)
        .with_context(|| format!("failed to save preset \"{preset_name}\""))?;
    config.events.emit(AppEvent::PresetSaved {
        preset: preset_name.to_string(),
        stratagems: preset.stratagems.clone(),
        booster: preset.booster.clone(),
    });
    info!(
        preset = %preset_name,
        stratagem_count = preset.stratagems.len(),
        booster_present = preset.booster.is_some(),
        presets_path = %config.presets.display(),
        "preset saved"
    );

    Ok(())
}

fn log_preset_contents(preset: &Preset) {
    debug!(
        stratagems = ?preset.stratagems,
        booster_present = preset.booster.is_some(),
        booster_item = preset.booster.as_deref().unwrap_or(""),
        "preset contents"
    );
}

fn apply_booster_if_present(
    runtime: &RecognizerRuntime,
    automation: &mut AutomationSession<'_>,
    config: &PresetActionConfig<'_>,
    preset: &Preset,
) -> Result<()> {
    let Some(booster) = preset.booster.as_ref() else {
        return Ok(());
    };
    apply_booster_from_home(
        runtime,
        automation,
        config.events,
        std::slice::from_ref(booster),
    )
    .context("failed to apply booster from home")
}
