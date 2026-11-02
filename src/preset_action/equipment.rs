//! Connect equipment automation to persistent presets and application feedback.

use anyhow::{Context, Result, ensure};

use super::{PresetActionContext, PresetActionOutcome, PresetCommand};
use crate::{
    app_events::{AppEvent, PresetCompletion},
    automation::AutomationSession,
    capture::CaptureSource,
    game_settings::GameColorSettings,
    item::{EquipmentKind, EquipmentSelection},
    loadout::equipment::{ApplyProgress, EquipmentSet, Session},
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
    if let PresetCommand::SaveEquipment(selection) = command {
        let items = sample(capture, target, colors, context, selection)?;
        storage::save(context.presets, preset, &items)
            .context("failed to save equipment preset")?;
        Ok((PresetActionOutcome::Saved, PresetCompletion::Saved))
    } else {
        let PresetCommand::ApplyEquipment(selection) = command else {
            unreachable!("equipment command required")
        };
        let items = storage::load(context.presets, preset, selection)
            .context("failed to load equipment preset")?;
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
    selection: EquipmentSelection,
) -> Result<EquipmentSet> {
    context
        .events
        .emit(AppEvent::EquipmentActionStarted { saving: true });
    with_session(capture, target, colors, context.evidence, |session| {
        session.sample(selection)
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
    let total = items.iter().flatten().count();
    let mut completed = 0;
    with_session(capture, target, colors, context.evidence, |session| {
        session.apply(items, context.equipment_cache, |progress| {
            let event = match progress {
                ApplyProgress::Selecting(kind) => AppEvent::EquipmentProgress {
                    kind,
                    confirmed: false,
                    completed,
                    total,
                },
                ApplyProgress::Caching {
                    category,
                    repairing,
                } => AppEvent::EquipmentCaching {
                    category: crate::preset::preview::equipment_category(category),
                    repairing,
                },
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
                                tracing::info!("saved corrected equipment category")
                            }
                            Err(error) => tracing::warn!(path = %context.presets.display(), error = %format!("{error:#}"),
                                "equipment applied, but its corrected category could not be saved"),
                        }
                    }
                    completed += 1;
                    AppEvent::EquipmentProgress {
                        kind,
                        confirmed: true,
                        completed,
                        total,
                    }
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
    evidence: &crate::evidence::OperationEvidence,
    operation: impl FnOnce(&mut Session<'_>) -> Result<T>,
) -> Result<T> {
    let (width, height) = capture.output_size();
    let (observer, resolved) = EquipmentObserver::resolve(width, height, EquipmentKind::Helmet)?;
    let region = capture.region(resolved.rect, colors)?;
    let automation = AutomationSession::new(region, target, evidence)?;
    let mut session = Session::new(automation, observer, (width, height))?;
    ensure!(
        session.at_entry(),
        "return to the six-card equipment entry before saving or applying equipment"
    );
    operation(&mut session)
}
