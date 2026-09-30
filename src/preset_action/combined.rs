//! Compose the existing operations; intermediate sections never report the whole preset as done.
use anyhow::{Context, Result, ensure};

use super::{PresetActionContext, PresetActionOutcome, PresetCommand, equipment, stratagems};
use crate::{
    app_events::PresetCompletion,
    automation::AutomationSession,
    capture::CaptureSource,
    game_settings::GameColorSettings,
    loadout::{
        UiState, bind_loadout_region, collect_stratagem_preset, detect_ui_state,
        pages::{Page, page_visible, show_page},
        scan_loadout_home,
    },
    preset::equipment as storage,
    vision::RecognizerRuntime,
    window::WindowTarget,
};

pub(super) fn execute(
    runtime: &RecognizerRuntime,
    context: &mut PresetActionContext<'_>,
    preset: &str,
    command: PresetCommand,
    capture: &mut CaptureSource,
    target: WindowTarget,
    colors: GameColorSettings,
) -> Result<(PresetActionOutcome, PresetCompletion)> {
    if command == PresetCommand::SaveFull {
        let started_on_equipment = page_visible(capture, runtime, colors, Page::Equipment)?;
        show_page(capture, runtime, &target, colors, Page::Stratagems)?;
        let captured = {
            let bound = bind_loadout_region(capture, runtime.calibration(), colors)?;
            let recognizer = runtime.bind(bound.geometry);
            let mut automation = AutomationSession::new(bound.region, target.clone())?;
            let home = scan_loadout_home(&mut automation, recognizer)?;
            ensure!(
                detect_ui_state(&home) == UiState::Home,
                "loadout home not detected; equipment has not been changed"
            );
            collect_stratagem_preset(&home, recognizer.ui_scale())?
        };
        show_page(capture, runtime, &target, colors, Page::Equipment)?;
        let items = equipment::sample(capture, target.clone(), colors, context)?;
        if !started_on_equipment {
            show_page(capture, runtime, &target, colors, Page::Stratagems)?;
        }
        // Only write after all samples and navigation succeed. These reuse the
        // existing per-section writers; this is not a filesystem transaction.
        storage::save(context.presets, preset, &items)?;
        stratagems::save(context, preset, &captured)
            .context("equipment saved, but saving Stratagems failed")?;
        Ok((PresetActionOutcome::Saved, PresetCompletion::Saved))
    } else {
        // Validate both saved parts before switching pages or changing any equipment.
        stratagems::load(context, preset)?;
        let items = storage::load(context.presets, preset)?;
        show_page(capture, runtime, &target, colors, Page::Equipment)?;
        equipment::apply(capture, target.clone(), colors, context, preset, &items)?;
        // Stratagems/Booster go last, so fallback learning and auto-ready cannot
        // run before equipment has finished.
        (|| {
            show_page(capture, runtime, &target, colors, Page::Stratagems)?;
            stratagems::execute(
                runtime,
                context,
                preset,
                PresetCommand::ApplyStratagems,
                capture,
                target,
                colors,
            )
        })()
        .context("equipment applied; Stratagems/Booster stage failed")
    }
}
