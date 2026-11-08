//! Coordinate preset actions; each section owns its save/apply implementation.
mod combined;
mod equipment;
mod stratagems;

use std::path::Path;
use std::time::Instant;

use anyhow::{Result, bail};
use tracing::{debug, info};

use crate::app_events::{AppEvent, AppEventSink};
use crate::capture::CaptureSessionManager;
use crate::item::EquipmentSelection;
use crate::loadout::pages::{Page, show_page};
use crate::preset::PresetScope;
use crate::vision::RecognizerRuntime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetActionOutcome {
    Saved,
    Applied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetCommand {
    ApplySaved(PresetScope),
    SaveStratagems,
    SaveEquipment(EquipmentSelection),
    ApplyStratagems,
    ApplyEquipment(EquipmentSelection),
    SaveFull(EquipmentSelection),
    ApplyFull(EquipmentSelection),
}

#[derive(Clone, Copy)]
pub struct PresetActionOptions {
    pub apply_in_saved_order: bool,
    pub auto_ready_up: bool,
    pub save_fallback_when_taken: bool,
}

pub(crate) struct PresetActionContext<'a> {
    pub presets: &'a Path,
    pub options: PresetActionOptions,
    pub events: &'a AppEventSink,
    pub item_cache: &'a mut crate::item_cache::ItemCache,
    pub evidence: &'a crate::evidence::OperationEvidence,
}

pub fn execute_preset(
    runtime: &RecognizerRuntime,
    context: &mut PresetActionContext<'_>,
    preset_name: &str,
    command: PresetCommand,
    capture_session: &mut CaptureSessionManager,
    expected_window: crate::window::WindowIdentity,
) -> Result<PresetActionOutcome> {
    let action_start = Instant::now();
    info!("preset action started");
    context.events.emit(AppEvent::PresetStarted {
        preset: preset_name.to_string(),
    });

    let command = if let PresetCommand::ApplySaved(scope) = command {
        let (stratagems, equipment) =
            crate::preset::saved_sections(context.presets, preset_name, scope.equipment_items)?;
        match (
            scope.stratagems && stratagems,
            scope.includes_equipment() && equipment,
        ) {
            (true, true) => PresetCommand::ApplyFull(scope.equipment_items),
            (true, false) => PresetCommand::ApplyStratagems,
            (false, true) => PresetCommand::ApplyEquipment(scope.equipment_items),
            (false, false) => {
                bail!("preset {preset_name} has no saved content in the selected sections")
            }
        }
    } else {
        command
    };
    debug!(resolved_command = ?command, "preset action scope resolved");

    let capture_start = Instant::now();
    let prepared = crate::automation::prepare_game(capture_session, expected_window)?;
    let game_window = prepared.target;
    let capture = prepared.source;
    let game_color_settings = prepared.colors;
    let (client_w, client_h) = game_window.client_size();
    debug!(
        elapsed_s = capture_start.elapsed().as_secs_f64(),
        "capture session ready"
    );
    context.evidence.record(
        "action-context",
        None,
        serde_json::json!({
            "client_size": [client_w, client_h], "game_colors": game_color_settings,
            "resolved_command": format!("{command:?}"),
        }),
    );
    let (outcome, completion) = match command {
        PresetCommand::SaveFull(_) | PresetCommand::ApplyFull(_) => combined::execute(
            runtime,
            context,
            preset_name,
            command,
            capture,
            game_window,
            game_color_settings,
        )?,
        PresetCommand::SaveStratagems | PresetCommand::ApplyStratagems => {
            show_page(
                capture,
                runtime,
                &game_window,
                game_color_settings,
                Page::Stratagems,
                context.evidence,
                None,
            )?;
            stratagems::execute(
                runtime,
                context,
                preset_name,
                command,
                capture,
                game_window,
                game_color_settings,
            )?
        }
        PresetCommand::SaveEquipment(_) | PresetCommand::ApplyEquipment(_) => {
            show_page(
                capture,
                runtime,
                &game_window,
                game_color_settings,
                Page::Equipment,
                context.evidence,
                None,
            )?;
            equipment::execute(
                capture,
                game_window,
                game_color_settings,
                context,
                preset_name,
                command,
            )?
        }
        PresetCommand::ApplySaved(_) => unreachable!("saved sections are resolved before dispatch"),
    };
    info!(
        elapsed_s = action_start.elapsed().as_secs_f64(),
        ?completion,
        "preset action completed"
    );
    context.events.emit(AppEvent::PresetDone {
        preset: preset_name.to_string(),
        completion,
    });
    Ok(outcome)
}
