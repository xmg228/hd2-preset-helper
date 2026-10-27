//! Stratagem/Booster overview, sampling and transitions to their icon lists.
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use tracing::{debug, debug_span, trace};

use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::preset::stratagems::CapturedStratagemPreset;
use crate::vision::{
    HOME_ICON_SIZE_LOGICAL, RecognizerSession, RoiObservation, Slot, SlotKind, SlotLayout,
    crop_booster_sample, crop_slot_sample,
};

const UI_HOME_Y_STABLE_DISTANCE: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiState {
    Home,
    List(ItemKind),
    Unknown,
}

impl UiState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::List(kind) => SlotLayout::List(kind).label(),
            Self::Unknown => "unknown",
        }
    }
}

pub fn detect_ui_state(result: &RoiObservation) -> UiState {
    match result.layout {
        SlotLayout::Home if find_home_row(result).is_some() => UiState::Home,
        SlotLayout::List(item_kind) if is_slot_list(result, item_kind) => UiState::List(item_kind),
        _ => UiState::Unknown,
    }
}

pub fn collect_stratagem_preset(
    result: &RoiObservation,
    template_scale: f32,
) -> Result<CapturedStratagemPreset> {
    let stratagems = collect_home_stratagems(result, template_scale)?;
    let booster = collect_home_booster(result, template_scale)?;

    Ok(CapturedStratagemPreset {
        stratagems,
        booster,
    })
}

fn collect_home_stratagems(
    result: &RoiObservation,
    stratagem_template_scale: f32,
) -> Result<Vec<crate::vision::ImageSample>> {
    let (stratagems, _) = find_home_row(result).context("missing home loadout row")?;
    ensure!(
        stratagems
            .iter()
            .all(|slot| slot.kind == SlotKind::Stratagem),
        "fill all four Stratagem slots before saving a preset"
    );
    let mut items = Vec::with_capacity(stratagems.len());

    for slot in stratagems {
        let physical_size = HOME_ICON_SIZE_LOGICAL * stratagem_template_scale;
        items.push(crop_slot_sample(&result.image, slot, physical_size)?);
    }

    Ok(items)
}

pub fn scan_loadout_home(
    automation: &mut AutomationSession<'_>,
    recognizer: RecognizerSession,
) -> Result<RoiObservation> {
    let image = automation.capture()?;
    let result = recognizer.detect(image, SlotLayout::Home)?;
    automation
        .evidence()
        .record("loadout-home", Some(&result.image), &result.slots);
    Ok(result)
}

pub(super) fn wait_for_filled_home(
    automation: &mut AutomationSession<'_>,
    recognizer: RecognizerSession,
    timeout: Duration,
) -> Result<Option<RoiObservation>> {
    wait_for_ui_state(automation, recognizer, UiState::Home, timeout, |home| {
        find_home_row(home)
            .is_some_and(|(slots, _)| slots.iter().all(|slot| slot.kind == SlotKind::Stratagem))
    })
}

/// Wait for the requested layout at its resting position. Home additionally
/// retains its two-observation Y check; list entry uses the fixed upper row.
pub fn wait_for_ui_state(
    automation: &mut AutomationSession<'_>,
    recognizer: RecognizerSession,
    target_state: UiState,
    timeout: Duration,
    accept: impl Fn(&RoiObservation) -> bool,
) -> Result<Option<RoiObservation>> {
    let span = debug_span!(
        "wait_for_ui_state",
        target_state = %target_state.label(),
        timeout_s = timeout.as_secs_f64()
    );
    let _guard = span.enter();
    let expected_layout = match target_state {
        UiState::Home => SlotLayout::Home,
        UiState::List(kind) => SlotLayout::List(kind),
        UiState::Unknown => bail!("cannot wait for an unknown UI state"),
    };

    let start = Instant::now();
    let mut previous_home: Option<(u32, crate::capture::CapturedFrame)> = None;
    let mut attempt = 0usize;

    loop {
        attempt += 1;
        let image = automation.capture()?;
        let entry_ready =
            !matches!(target_state, UiState::List(_)) || recognizer.list_entry_ready(&image);
        // List geometry alone can also find rows in the home screen's background.
        let mut result = recognizer.detect(image, SlotLayout::Home)?;
        if matches!(expected_layout, SlotLayout::List(_))
            && entry_ready
            && detect_ui_state(&result) == UiState::Unknown
        {
            result = recognizer.detect(result.image, expected_layout)?;
        }
        let current_state = detect_ui_state(&result);

        let content_accepted = current_state == target_state && accept(&result);
        let home_y = if content_accepted && target_state == UiState::Home {
            find_home_row(&result).map(|(slots, _)| slots[0].center().1)
        } else {
            None
        };
        let reference = previous_home.as_ref().map(|(_, frame)| frame);
        let distance = previous_home
            .as_ref()
            .zip(home_y)
            .map(|((previous, _), current)| previous.abs_diff(current) as f32);
        let ready = content_accepted
            && match target_state {
                UiState::Home => distance.is_some_and(|d| d <= UI_HOME_Y_STABLE_DISTANCE),
                UiState::List(_) => entry_ready,
                UiState::Unknown => unreachable!(),
            };

        if ready {
            if reference.is_some() {
                automation.evidence().record(
                    "ui-state-reference",
                    reference,
                    serde_json::json!({"target": target_state.label()}),
                );
            }
            automation.evidence().record(
                "ui-state-ready",
                Some(&result.image),
                serde_json::json!({
                    "target": target_state.label(), "slots": result.slots,
                    "entry_ready": entry_ready, "home_y_distance": distance, "observations": attempt,
                }),
            );
            debug!(
                target: "hd2_preset_helper::perf",
                attempt,
                slot_count = result.slots.len(),
                home_y_distance = distance,
                elapsed_s = start.elapsed().as_secs_f64(),
                "target UI state ready"
            );
            return Ok(Some(result));
        }

        trace!(
            attempt,
            current_state = %current_state.label(),
            slot_count = result.slots.len(),
            entry_ready,
            "waiting for target UI state"
        );

        if start.elapsed() >= timeout {
            if reference.is_some() {
                automation.evidence().record(
                    "ui-state-reference",
                    reference,
                    serde_json::json!({"target": target_state.label()}),
                );
            }
            automation.evidence().record(
                "ui-state-timeout",
                Some(&result.image),
                serde_json::json!({
                    "target": target_state.label(), "observed": current_state.label(),
                    "content_accepted": content_accepted, "slots": result.slots,
                    "entry_ready": entry_ready, "home_y_distance": distance, "observations": attempt,
                }),
            );
            debug!(
                target_state = %target_state.label(),
                last_state = %current_state.label(),
                last_slot_count = result.slots.len(),
                content_accepted,
                entry_ready,
                home_y_distance = distance,
                reason = if !entry_ready {
                    "entry_frame_not_ready"
                } else if current_state != target_state {
                    "state_not_detected"
                } else if !content_accepted {
                    "content_not_accepted"
                } else if distance.is_none() {
                    "awaiting_second_observation"
                } else {
                    "unstable"
                },
                elapsed_s = start.elapsed().as_secs_f64(),
                "timed out waiting for target UI state"
            );
            return Ok(None);
        }
        // Home content requirements must hold on both observations.
        previous_home = home_y.map(|y| (y, result.image.clone()));
    }
}

pub fn home_booster_slot(result: &RoiObservation) -> Option<&Slot> {
    find_home_row(result).map(|(_stratagems, booster)| booster)
}

fn is_slot_list(result: &RoiObservation, item_kind: ItemKind) -> bool {
    let mut rows = result
        .slots
        .iter()
        .filter(|slot| slot.kind.is_selectable_item_for(item_kind))
        .map(|slot| slot.row);

    match item_kind {
        ItemKind::Stratagem => rows
            .next()
            .is_some_and(|first| rows.any(|row| row != first)),
        ItemKind::Booster => rows.next().is_some(),
    }
}

pub fn collect_home_booster(
    result: &RoiObservation,
    template_scale: f32,
) -> Result<Option<crate::vision::ImageSample>> {
    let Some(slot) = home_booster_slot(result) else {
        return Ok(None);
    };

    debug_assert!(slot.kind.is_home_booster());
    if slot.kind == SlotKind::HomeBoosterEmpty {
        return Ok(None);
    }
    let physical_size = HOME_ICON_SIZE_LOGICAL * template_scale;
    crop_booster_sample(&result.image, slot, physical_size).map(Some)
}

pub(super) fn find_home_row(result: &RoiObservation) -> Option<([&Slot; 4], &Slot)> {
    let mut stratagems = [None; 4];
    let mut booster = None;

    for slot in result.slots.iter().filter(|slot| slot.row == 0) {
        match slot.kind {
            SlotKind::Stratagem | SlotKind::StratagemEmpty => {
                if let Some(target) = stratagems.get_mut(slot.col as usize) {
                    *target = Some(slot);
                }
            }
            SlotKind::HomeBooster | SlotKind::HomeBoosterEmpty if slot.col == 4 => {
                booster = Some(slot);
            }
            _ => {}
        }
    }

    let [Some(a), Some(b), Some(c), Some(d)] = stratagems else {
        return None;
    };
    Some(([a, b, c, d], booster?))
}
