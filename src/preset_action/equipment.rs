//! Connect equipment automation to persistent presets and application feedback.
use std::path::Path;

use anyhow::{Result, ensure};

use super::{PresetActionContext, PresetActionOutcome, PresetCommand};
use crate::{
    app_events::{AppEvent, PresetCompletion},
    automation::AutomationSession,
    capture::CaptureSource,
    game_settings::GameColorSettings,
    item::EquipmentKind,
    loadout::equipment::{ApplyProgress, Diagnostics, EquipmentSet, Session},
    preset::equipment as storage,
    vision::equipment::EquipmentObserver,
    window::WindowTarget,
};

pub(super) fn execute(
    capture: &mut CaptureSource,
    target: WindowTarget,
    colors: GameColorSettings,
    context: &mut PresetActionContext<'_>,
    preset: &str,
    command: PresetCommand,
) -> Result<(PresetActionOutcome, PresetCompletion)> {
    if command == PresetCommand::SaveEquipment {
        let items = sample(capture, target, colors, context)?;
        storage::save(context.presets, preset, &items)?;
        Ok((PresetActionOutcome::Saved, PresetCompletion::Saved))
    } else {
        let items = storage::load(context.presets, preset)?;
        apply(capture, target, colors, context, preset, &items)?;
        Ok((
            PresetActionOutcome::Applied,
            PresetCompletion::EquipmentApplied,
        ))
    }
}

pub(super) fn sample(
    capture: &mut CaptureSource,
    target: WindowTarget,
    colors: GameColorSettings,
    context: &mut PresetActionContext<'_>,
) -> Result<EquipmentSet> {
    context
        .events
        .emit(AppEvent::EquipmentActionStarted { saving: true });
    with_session(capture, target, colors, context.presets, |session| {
        session.sample_all()
    })
}

pub(super) fn apply(
    capture: &mut CaptureSource,
    target: WindowTarget,
    colors: GameColorSettings,
    context: &mut PresetActionContext<'_>,
    preset: &str,
    items: &EquipmentSet,
) -> Result<()> {
    context
        .events
        .emit(AppEvent::EquipmentActionStarted { saving: false });
    let client_size = capture.output_size();
    with_session(capture, target, colors, context.presets, |session| {
        session.apply_all(items, context.equipment_cache, |progress| {
            let event = match progress {
                ApplyProgress::Selecting(kind) => AppEvent::EquipmentSelectionStarted { kind },
                ApplyProgress::Selected {
                    kind,
                    corrected_category,
                } => {
                    if let Some(category) = corrected_category {
                        match storage::update_category(
                            context.presets,
                            preset,
                            kind,
                            &category,
                            client_size,
                        ) {
                            Ok(()) => {
                                tracing::info!(preset, ?kind, "saved corrected equipment category")
                            }
                            Err(error) => tracing::warn!(preset, ?kind, %error,
                                "equipment applied, but its corrected category could not be saved"),
                        }
                    }
                    AppEvent::ItemSelected
                }
            };
            context.events.emit(event);
        })
    })
}

fn with_session<T>(
    capture: &mut CaptureSource,
    target: WindowTarget,
    colors: GameColorSettings,
    presets: &Path,
    operation: impl FnOnce(&mut Session<'_, '_>) -> Result<T>,
) -> Result<T> {
    let (width, height) = capture.output_size();
    let (observer, resolved) = EquipmentObserver::resolve(width, height, EquipmentKind::Helmet)?;
    let region = capture.region(resolved.rect, colors)?;
    let automation = AutomationSession::new(region, target)?;
    #[cfg(not(feature = "diagnostics"))]
    let mut diagnostics = Diagnostics::default();
    #[cfg(feature = "diagnostics")]
    let mut diagnostics = {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        let directory = presets
            .with_file_name("diagnostics")
            .join("equipment")
            .join(stamp.to_string());
        Diagnostics::new(&directory).unwrap_or_else(|error| {
            tracing::warn!(%error, "equipment diagnostics unavailable");
            Diagnostics::default()
        })
    };
    diagnostics.set_failure_directory(presets.with_file_name("debug").join("equipment-search"));
    let mut session = Session::new(automation, observer, (width, height), &mut diagnostics)?;
    ensure!(
        session.at_entry(),
        "return to the six-card equipment entry before saving or applying equipment"
    );
    operation(&mut session)
}
