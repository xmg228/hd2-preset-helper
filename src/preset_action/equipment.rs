//! Connect equipment automation to persistent presets and application feedback.

use anyhow::{Context, Result};

use super::{PresetActionContext, PresetActionOutcome, PresetCommand};
use crate::{
    app_events::{AppEvent, PresetCompletion},
    capture::CaptureSource,
    game_settings::GameColorSettings,
    item::EquipmentSelection,
    item_cache::EquipmentSet,
    loadout::equipment::{ApplyProgress, with_session},
    preset::equipment as storage,
    scan::ScanKind,
    vision::equipment::CapturedEquipmentSet,
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
) -> Result<CapturedEquipmentSet> {
    context
        .events
        .emit(AppEvent::EquipmentActionStarted { saving: true });
    with_session(
        capture,
        target,
        colors,
        context.evidence,
        context.cancel.clone(),
        |session| session.sample(selection),
    )
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
    with_session(
        capture,
        target,
        colors,
        context.evidence,
        context.cancel.clone(),
        |session| {
            session.apply(items, context.item_cache, |progress| {
                let event = match progress {
                    ApplyProgress::Selecting(kind) => AppEvent::EquipmentProgress {
                        kind,
                        confirmed: false,
                        completed,
                        total,
                    },
                    ApplyProgress::Caching {
                        kind,
                        category,
                        items,
                    } => AppEvent::ItemCaching {
                        kind: ScanKind::Equipment(kind),
                        category: Some(crate::preset::preview::equipment_category(category)),
                        items,
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
                                Err(error) => tracing::warn!(
                                    path = %context.presets.display(),
                                    error = %format!("{error:#}"),
                                    "equipment applied, but its corrected category could not be saved"
                                ),
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
        },
    )
}
