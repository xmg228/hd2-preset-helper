//! Connect Stratagem and Booster automation to persistent presets and application feedback.
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use tracing::{debug, info};

use super::{PresetActionContext, PresetActionOutcome, PresetCommand};
use crate::app_events::{AppEvent, PresetCompletion};
use crate::automation::AutomationSession;
use crate::input;
use crate::loadout::{
    BoosterApplyOutcome, UiState, apply_booster_from_home, apply_stratagems_from_home,
    bind_loadout_region, collect_home_booster, collect_stratagem_preset, detect_ui_state,
    scan_loadout_home, wait_for_filled_home,
};
use crate::preset::stratagems::{self as storage, CapturedStratagemPreset, StratagemPreset};
#[cfg(feature = "diagnostics")]
use crate::vision::log_home_tone;
use crate::vision::{RecognizerRuntime, RecognizerSession, RoiObservation};

const READY_UP_HOLD_MS: u64 = 45;
const FALLBACK_BOOSTER_SELECTION_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn execute(
    runtime: &RecognizerRuntime,
    context: &mut PresetActionContext<'_>,
    preset_name: &str,
    command: PresetCommand,
    capture: &mut crate::capture::CaptureSource,
    game_window: crate::window::WindowTarget,
    game_color_settings: crate::game_settings::GameColorSettings,
) -> Result<(PresetActionOutcome, PresetCompletion)> {
    let bound_region = bind_loadout_region(capture, runtime.calibration(), game_color_settings)
        .context("failed to bind loadout capture region")?;
    let recognizer = runtime.bind(bound_region.geometry);
    let mut automation = AutomationSession::new(bound_region.region, game_window.clone())
        .context("failed to start automation session")?;

    let (initial_result, ui_state) = {
        let initial_result = scan_loadout_home(&mut automation, recognizer)
            .context("failed to scan loadout home")?;
        let ui_state = detect_ui_state(&initial_result);
        debug!(ui_state = %ui_state.label(), "detected loadout UI state");
        (initial_result, ui_state)
    };

    ensure!(
        matches!(
            ui_state,
            UiState::HomeEmpty | UiState::HomeMixed | UiState::HomeFilled
        ),
        "loadout home not detected; return to the loadout home before using a preset"
    );
    ensure!(
        command != PresetCommand::SaveStratagems || ui_state == UiState::HomeFilled,
        "fill all four Stratagem slots before saving a preset"
    );
    #[cfg(feature = "diagnostics")]
    log_home_tone(&initial_result);

    let (outcome, ready_up_after_apply, completion) = match command {
        PresetCommand::SaveStratagems => {
            let captured = collect_stratagem_preset(&initial_result, recognizer.ui_scale())
                .context("failed to collect current preset")?;
            save(context, preset_name, &captured)?;
            (PresetActionOutcome::Saved, false, PresetCompletion::Saved)
        }

        PresetCommand::ApplyStratagems => {
            let preset = load(context, preset_name)?;
            debug!(
                stratagem_count = preset.stratagems.len(),
                booster_present = preset.booster.is_some(),
                fallback_booster_present = preset.fallback_booster.is_some(),
                "applying preset from home"
            );
            log_preset_contents(&preset);
            let home = apply_stratagems_from_home(
                recognizer,
                &mut automation,
                context.events,
                initial_result,
                context.presets,
                &preset.stratagems,
                context.options.apply_in_saved_order,
            )
            .context("failed to apply stratagems from home")?;
            let booster =
                apply_booster_if_present(recognizer, &mut automation, context, &preset, home)?;
            let (ready_up_after_apply, completion) = match booster {
                Some(BoosterApplyOutcome::Applied) => (true, PresetCompletion::Complete),
                Some(BoosterApplyOutcome::Unavailable) => {
                    if context.options.save_fallback_when_taken {
                        if learn_fallback_booster(
                            recognizer,
                            &mut automation,
                            context,
                            preset_name,
                        )? {
                            (true, PresetCompletion::FallbackBoosterSaved)
                        } else {
                            (false, PresetCompletion::FallbackBoosterNotSaved)
                        }
                    } else {
                        (false, PresetCompletion::BoosterUnavailable)
                    }
                }
                None => (false, PresetCompletion::Complete),
            };
            (
                PresetActionOutcome::Applied,
                ready_up_after_apply,
                completion,
            )
        }

        _ => unreachable!("other preset commands are dispatched before the Stratagem section"),
    };

    if context.options.auto_ready_up && ready_up_after_apply {
        debug!("booster preset applied; sending READY UP key");
        automation.tap_key(input::Key::B, READY_UP_HOLD_MS)?;
    }

    Ok((outcome, completion))
}

pub(super) fn load(
    context: &PresetActionContext<'_>,
    preset_name: &str,
) -> Result<StratagemPreset> {
    let preset = storage::load(context.presets, preset_name)
        .with_context(|| format!("failed to load preset \"{preset_name}\""))?;

    if let Some(reason) = storage::invalid_reason(context.presets, &preset) {
        bail!("preset \"{preset_name}\" is invalid: {reason}");
    }

    Ok(preset)
}

pub(super) fn save(
    context: &PresetActionContext<'_>,
    preset_name: &str,
    captured: &CapturedStratagemPreset,
) -> Result<()> {
    debug!(
        stratagem_count = captured.stratagems.len(),
        booster_present = captured.booster.is_some(),
        "saving current preset"
    );

    let preset = storage::save(context.presets, preset_name, captured)
        .with_context(|| format!("failed to save preset \"{preset_name}\""))?;
    log_preset_contents(&preset);
    info!(
        preset = %preset_name,
        stratagem_count = preset.stratagems.len(),
        booster_present = preset.booster.is_some(),
        presets_path = %context.presets.display(),
        "preset saved"
    );

    Ok(())
}

fn log_preset_contents(preset: &StratagemPreset) {
    let stratagems = preset
        .stratagems
        .iter()
        .map(|template| template.path.as_str())
        .collect::<Vec<_>>();
    debug!(
        ?stratagems,
        booster_present = preset.booster.is_some(),
        booster_template = preset
            .booster
            .as_ref()
            .map_or("", |template| template.path.as_str()),
        fallback_booster_present = preset.fallback_booster.is_some(),
        fallback_booster_template = preset
            .fallback_booster
            .as_ref()
            .map_or("", |template| template.path.as_str()),
        "preset contents"
    );
}

fn learn_fallback_booster(
    recognizer: RecognizerSession,
    automation: &mut AutomationSession<'_>,
    context: &PresetActionContext<'_>,
    preset_name: &str,
) -> Result<bool> {
    context.events.emit(AppEvent::FallbackBoosterRequested {
        preset: preset_name.to_string(),
    });
    let Some(home) =
        wait_for_filled_home(automation, recognizer, FALLBACK_BOOSTER_SELECTION_TIMEOUT)?
    else {
        info!(
            preset = %preset_name,
            timeout = ?FALLBACK_BOOSTER_SELECTION_TIMEOUT,
            "fallback booster selection timed out"
        );
        return Ok(false);
    };
    let Some(sample) = collect_home_booster(&home, recognizer.ui_scale())? else {
        info!(
            preset = %preset_name,
            "fallback booster selection cancelled without choosing a booster"
        );
        return Ok(false);
    };

    let preset = storage::save_fallback_booster(context.presets, preset_name, &sample)
        .with_context(|| format!("failed to save fallback booster for \"{preset_name}\""))?;
    log_preset_contents(&preset);
    info!(
        preset = %preset_name,
        presets_path = %context.presets.display(),
        "fallback booster saved"
    );
    Ok(true)
}

fn apply_booster_if_present(
    recognizer: RecognizerSession,
    automation: &mut AutomationSession<'_>,
    context: &PresetActionContext<'_>,
    preset: &StratagemPreset,
    home: RoiObservation,
) -> Result<Option<BoosterApplyOutcome>> {
    let Some(booster) = preset.booster.as_ref() else {
        return Ok(None);
    };
    apply_booster_from_home(
        recognizer,
        automation,
        context.events,
        home,
        context.presets,
        booster,
        preset.fallback_booster.as_ref(),
    )
    .map(Some)
    .context("failed to apply booster from home")
}
