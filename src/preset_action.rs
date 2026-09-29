//! Coordinate preset actions; each section owns its save/apply implementation.
mod combined;
mod equipment;
mod stratagems;

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use tracing::{debug, info, info_span};

use crate::app_events::{AppEvent, AppEventSink};
use crate::capture::CaptureSessionManager;
use crate::game_settings::read_color_settings;
use crate::game_window::wait_for_foreground_game_window;
use crate::loadout::pages::{Page, show_page};
use crate::permissions;
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
    SaveEquipment,
    ApplyStratagems,
    ApplyEquipment,
    SaveFull,
    ApplyFull,
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
    pub equipment_cache: &'a mut crate::loadout::equipment::EquipmentCache,
}

pub fn execute_preset(
    runtime: &RecognizerRuntime,
    context: &mut PresetActionContext<'_>,
    preset_name: &str,
    command: PresetCommand,
    capture_session: &mut CaptureSessionManager,
    expected_window: crate::window::WindowIdentity,
) -> Result<PresetActionOutcome> {
    let span = info_span!("preset_action");
    let _guard = span.enter();

    let action_start = Instant::now();
    info!(preset = %preset_name, ?command, "preset action started");
    context.events.emit(AppEvent::PresetStarted {
        preset: preset_name.to_string(),
    });

    let command = if let PresetCommand::ApplySaved(scope) = command {
        let (stratagems, equipment) = crate::preset::saved_sections(context.presets, preset_name)?;
        match (scope.stratagems && stratagems, scope.equipment && equipment) {
            (true, true) => PresetCommand::ApplyFull,
            (true, false) => PresetCommand::ApplyStratagems,
            (false, true) => PresetCommand::ApplyEquipment,
            (false, false) => {
                bail!("preset {preset_name} has no saved content in the selected sections")
            }
        }
    } else {
        command
    };

    let game_window =
        wait_for_foreground_game_window().context("failed to locate Helldivers window")?;
    ensure!(
        expected_window == game_window.identity(),
        "the game window changed before the action started; try again"
    );
    permissions::ensure_input_access()?;
    let (client_w, client_h) = game_window.client_size();
    debug!(client_w, client_h, "game window ready");

    let capture_start = Instant::now();
    let capture = capture_session
        .acquire(&game_window)
        .context("failed to get capture session")?;
    debug!(
        elapsed = ?capture_start.elapsed(),
        "capture session ready"
    );
    let game_color_settings =
        read_color_settings().context("failed to read Helldivers color settings")?;
    let (outcome, completion) = match command {
        PresetCommand::SaveFull | PresetCommand::ApplyFull => combined::execute(
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
        PresetCommand::SaveEquipment | PresetCommand::ApplyEquipment => {
            show_page(
                capture,
                runtime,
                &game_window,
                game_color_settings,
                Page::Equipment,
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
    context.events.emit(AppEvent::PresetDone {
        preset: preset_name.to_string(),
        completion,
    });
    info!(preset = %preset_name, elapsed = ?action_start.elapsed(), "preset action completed");
    Ok(outcome)
}
