use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, info_span};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION;

use crate::app_events::{AppEvent, AppEventSink};
use crate::capture::CaptureSource;
use crate::capture_cache::CaptureSessionCache;
use crate::direct_select::{apply_booster_from_home, apply_empty_loadout_preset};
use crate::game_window::find_game_window;
use crate::input;
use crate::preset_flow::{
    UiState, collect_current_preset, detect_ui_state, scan_loadout_home, wait_for_ui_state,
};
use crate::presets::{Preset, invalid_preset_reason, load_preset, save_preset};
use crate::runtime::RecognizerRuntime;

const READY_UP_HOLD_MS: u64 = 45;

pub struct PresetActionConfig<'a> {
    pub presets: &'a Path,
    pub auto_ready_up: bool,
    pub events: &'a AppEventSink,
}

pub fn handle_preset_hotkey(
    runtime: &RecognizerRuntime,
    config: &PresetActionConfig<'_>,
    preset_name: &str,
    capture_cache: &mut CaptureSessionCache,
) -> Result<()> {
    let span = info_span!("preset_action");
    let _guard = span.enter();

    let action_start = Instant::now();
    info!(preset = %preset_name, "preset action started");
    config.events.emit(AppEvent::PresetStarted {
        preset: preset_name.to_string(),
    });

    let game_window = find_game_window().context("failed to locate Helldivers window")?;
    let _automation = input::AutomationScope::new(game_window)?;
    debug!(
        client_x = game_window.client_x,
        client_y = game_window.client_y,
        client_w = game_window.client_w,
        client_h = game_window.client_h,
        "game window ready"
    );

    let capture_start = Instant::now();
    let capture = capture_cache
        .get_or_create(game_window)
        .context("failed to get capture session")?;
    debug!(
        elapsed = ?capture_start.elapsed(),
        "capture session ready"
    );

    let (initial_result, ui_state) = {
        let initial_result =
            scan_loadout_home(capture, runtime).context("failed to scan loadout home")?;
        let ui_state = detect_ui_state(&initial_result);
        debug!(ui_state = %ui_state.label(), "detected loadout UI state");
        config.events.emit(AppEvent::UiStateDetected {
            state: ui_state.label(),
        });
        (initial_result, ui_state)
    };

    let ready_up_after_apply = match ui_state {
        UiState::HomeFilled => {
            let preset = collect_current_preset(&initial_result)
                .context("failed to collect current preset")?;
            save_current_preset(config, preset_name, &preset)?;
            false
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
            apply_empty_loadout_preset(runtime, capture, config.events, &preset.stratagems)
                .context("failed to apply stratagems from empty home")?;
            apply_booster_if_present(runtime, capture, config, &preset)?;
            preset.booster.is_some()
        }

        UiState::List(_) | UiState::Unknown => {
            bail!("loadout home not detected; return to the loadout home before using a preset");
        }
    };

    if config.auto_ready_up && ready_up_after_apply {
        wait_for_ui_state(
            capture,
            runtime,
            UiState::HomeFilled,
            Duration::from_millis(1500),
        )
        .context("booster was selected but the loadout home did not stabilize before starting")?;
        debug!("booster preset applied; sending READY UP key");
        input::tap(input::Vk::B, READY_UP_HOLD_MS)?;
    }

    config.events.emit(AppEvent::PresetDone {
        preset: preset_name.to_string(),
    });
    info!(
        preset = %preset_name,
        elapsed = ?action_start.elapsed(),
        "preset action completed"
    );
    Ok(())
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

    unsafe {
        MessageBeep(MB_ICONINFORMATION).ok();
    }

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
    capture: &mut CaptureSource,
    config: &PresetActionConfig<'_>,
    preset: &Preset,
) -> Result<()> {
    let Some(booster) = preset.booster.as_ref() else {
        return Ok(());
    };
    apply_booster_from_home(
        runtime,
        capture,
        config.events,
        std::slice::from_ref(booster),
    )
    .context("failed to apply booster from home")
}
