mod direct_select;
mod frame;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, debug_span, trace};

use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::preset::Preset;
use crate::vision::{
    RecognizerRuntime, RoiObservation, Slot, SlotKind, SlotLayout, icon_likeness, luma601_u8,
};

pub use direct_select::{apply_booster_from_home, apply_empty_loadout_preset};
pub use frame::bind_loadout_region;

use self::frame::fingerprint_distance;

const UI_TRANSITION_DELAY: Duration = Duration::from_millis(150);
const UI_STATE_STABLE_DISTANCE: f32 = 3.0;
const UI_HOME_Y_STABLE_DISTANCE: f32 = 4.0;
const SLOT_FINGERPRINT_GRID: u32 = 8;
const SLOT_FINGERPRINT_INSET_RATIO: f32 = 0.18;
const SLOT_FINGERPRINT_SAMPLES: f32 = (SLOT_FINGERPRINT_GRID * SLOT_FINGERPRINT_GRID) as f32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiState {
    HomeEmpty,
    HomeMixed,
    HomeFilled,
    List(ItemKind),
    Unknown,
}

enum UiStabilitySignature {
    Visual(Vec<u8>),
    HomeY(u32),
}

impl UiState {
    pub fn label(self) -> &'static str {
        match self {
            Self::HomeEmpty => "home_empty",
            Self::HomeMixed => "home_mixed",
            Self::HomeFilled => "home_filled",
            Self::List(kind) => SlotLayout::List(kind).label(),
            Self::Unknown => "unknown",
        }
    }
}

pub fn detect_ui_state(result: &RoiObservation) -> UiState {
    match result.layout {
        SlotLayout::Home => {
            let Some((stratagems, _booster)) = find_home_row(result) else {
                return UiState::Unknown;
            };
            match stratagems
                .iter()
                .filter(|slot| slot.kind == SlotKind::Stratagem)
                .count()
            {
                0 => UiState::HomeEmpty,
                4 => UiState::HomeFilled,
                _ => UiState::HomeMixed,
            }
        }
        SlotLayout::List(item_kind) if is_slot_list(result, item_kind) => UiState::List(item_kind),
        _ => UiState::Unknown,
    }
}

pub fn collect_current_preset(result: &RoiObservation) -> Result<Preset> {
    let stratagems = collect_home_stratagems(result)?;
    let booster = collect_home_booster(result)?;

    Ok(Preset {
        stratagems,
        booster,
    })
}

fn collect_home_stratagems(result: &RoiObservation) -> Result<Vec<String>> {
    let (stratagems, _) = find_home_row(result).context("missing home loadout row")?;
    let mut items = Vec::with_capacity(stratagems.len());

    for (col, slot) in stratagems.into_iter().enumerate() {
        if slot.kind != SlotKind::Stratagem {
            bail!("home stratagem slot {col} is empty");
        }
        let classification = slot
            .classification
            .as_ref()
            .with_context(|| format!("home stratagem slot {col} is not classified"))?;
        items.push(classification.item_id.clone());
    }

    Ok(items)
}

pub fn scan_loadout_home(
    automation: &mut AutomationSession<'_>,
    runtime: &RecognizerRuntime,
) -> Result<RoiObservation> {
    let image = automation.capture()?;
    runtime.recognize(image, SlotLayout::Home)
}

pub fn wait_for_ui_state(
    automation: &mut AutomationSession<'_>,
    runtime: &RecognizerRuntime,
    target_state: UiState,
    timeout: Duration,
) -> Result<RoiObservation> {
    let span = debug_span!(
        "wait_for_ui_state",
        target_state = %target_state.label(),
        timeout = ?timeout
    );
    let _guard = span.enter();
    let (expected_layout, stable_distance) = match target_state {
        UiState::HomeEmpty | UiState::HomeMixed | UiState::HomeFilled => {
            (SlotLayout::Home, UI_HOME_Y_STABLE_DISTANCE)
        }
        UiState::List(kind) => (SlotLayout::List(kind), UI_STATE_STABLE_DISTANCE),
        UiState::Unknown => bail!("cannot wait for an unknown UI state"),
    };

    let start = Instant::now();
    std::thread::sleep(UI_TRANSITION_DELAY);

    let mut stable_candidate: Option<UiStabilitySignature> = None;
    let mut attempt = 0usize;

    loop {
        attempt += 1;
        let image = automation.capture()?;
        let result = runtime.detect(image, expected_layout)?;
        let current_state = detect_ui_state(&result);

        if current_state == target_state {
            let signature = ui_stability_signature(&result)?;
            if let Some(distance) = stable_candidate
                .as_ref()
                .and_then(|previous| signature_distance(previous, &signature))
            {
                if distance <= stable_distance {
                    debug!(
                        target: "hd2_preset_helper::perf",
                        attempt,
                        slot_count = result.slots.len(),
                        distance,
                        threshold = stable_distance,
                        elapsed = ?start.elapsed(),
                        "target UI state stabilized"
                    );
                    return Ok(result);
                }

                trace!(
                    attempt,
                    slot_count = result.slots.len(),
                    distance,
                    threshold = stable_distance,
                    "target UI state not stable yet"
                );
            } else {
                trace!(
                    attempt,
                    slot_count = result.slots.len(),
                    "target UI state detected; waiting for stability"
                );
            }

            stable_candidate = Some(signature);
        } else {
            stable_candidate = None;
        }

        trace!(
            attempt,
            current_state = %current_state.label(),
            slot_count = result.slots.len(),
            "waiting for target UI state"
        );

        if start.elapsed() >= timeout {
            bail!(
                "timed out waiting for {} UI state; last_state={}, last_slot_count={}",
                target_state.label(),
                current_state.label(),
                result.slots.len()
            );
        }
    }
}

fn ui_stability_signature(result: &RoiObservation) -> Result<UiStabilitySignature> {
    match result.layout {
        SlotLayout::List(item_kind) => Ok(UiStabilitySignature::Visual(slot_region_fingerprint(
            result, item_kind,
        ))),
        SlotLayout::Home => {
            let (stratagems, _) = find_home_row(result).context("missing home loadout row")?;
            Ok(UiStabilitySignature::HomeY(stratagems[0].center().1))
        }
    }
}

fn signature_distance(left: &UiStabilitySignature, right: &UiStabilitySignature) -> Option<f32> {
    match (left, right) {
        (UiStabilitySignature::Visual(left), UiStabilitySignature::Visual(right)) => {
            Some(fingerprint_distance(left, right))
        }
        (UiStabilitySignature::HomeY(left), UiStabilitySignature::HomeY(right)) => {
            Some(left.abs_diff(*right) as f32)
        }
        _ => None,
    }
}

fn slot_region_fingerprint(result: &RoiObservation, item_kind: ItemKind) -> Vec<u8> {
    let rgba = &result.image;
    let mut fingerprint = Vec::with_capacity(result.slots.len() * 5);
    for slot in result
        .slots
        .iter()
        .filter(|slot| slot.kind.is_selectable_item_for(item_kind))
    {
        let right = slot.x.saturating_add(slot.w);
        let bottom = slot.y.saturating_add(slot.h);
        if right > rgba.width() || bottom > rgba.height() || slot.w == 0 || slot.h == 0 {
            continue;
        }

        let inset_x = (slot.w as f32 * SLOT_FINGERPRINT_INSET_RATIO) as u32;
        let inset_y = (slot.h as f32 * SLOT_FINGERPRINT_INSET_RATIO) as u32;
        let left = slot.x + inset_x;
        let top = slot.y + inset_y;
        let right = right - inset_x;
        let bottom = bottom - inset_y;

        let mut weighted_r = 0.0f32;
        let mut weighted_g = 0.0f32;
        let mut weighted_b = 0.0f32;
        let mut weighted_luma = 0.0f32;
        let mut weight_sum = 0.0f32;
        let mut plain_r = 0.0f32;
        let mut plain_g = 0.0f32;
        let mut plain_b = 0.0f32;
        let mut plain_luma = 0.0f32;

        for row in 0..SLOT_FINGERPRINT_GRID {
            for col in 0..SLOT_FINGERPRINT_GRID {
                let x = left
                    + ((col as f32 + 0.5) * (right - left) as f32 / SLOT_FINGERPRINT_GRID as f32)
                        as u32;
                let y = top
                    + ((row as f32 + 0.5) * (bottom - top) as f32 / SLOT_FINGERPRINT_GRID as f32)
                        as u32;
                let [r, g, b, _] = rgba.get_pixel(x, y).0;
                let luma = luma601_u8(r, g, b) as f32;
                let weight = icon_likeness(r, g, b);

                weighted_r += r as f32 * weight;
                weighted_g += g as f32 * weight;
                weighted_b += b as f32 * weight;
                weighted_luma += luma * weight;
                weight_sum += weight;

                plain_r += r as f32;
                plain_g += g as f32;
                plain_b += b as f32;
                plain_luma += luma;
            }
        }

        if weight_sum > f32::EPSILON {
            fingerprint.push(quantize_u8(weighted_luma / weight_sum));
            fingerprint.push(quantize_u8(weighted_r / weight_sum));
            fingerprint.push(quantize_u8(weighted_g / weight_sum));
            fingerprint.push(quantize_u8(weighted_b / weight_sum));
        } else {
            fingerprint.push(quantize_u8(plain_luma / SLOT_FINGERPRINT_SAMPLES));
            fingerprint.push(quantize_u8(plain_r / SLOT_FINGERPRINT_SAMPLES));
            fingerprint.push(quantize_u8(plain_g / SLOT_FINGERPRINT_SAMPLES));
            fingerprint.push(quantize_u8(plain_b / SLOT_FINGERPRINT_SAMPLES));
        }

        fingerprint.push(quantize_u8(weight_sum / SLOT_FINGERPRINT_SAMPLES * 255.0));
    }

    fingerprint
}

fn quantize_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

pub fn empty_loadout_entry_slot(result: &RoiObservation) -> Option<&Slot> {
    let (stratagems, _) = find_home_row(result)?;
    stratagems
        .iter()
        .all(|slot| slot.kind == SlotKind::StratagemEmpty)
        .then_some(stratagems[0])
}

pub fn home_booster_slot(result: &RoiObservation) -> Option<&Slot> {
    find_home_row(result).map(|(_stratagems, booster)| booster)
}

pub fn home_booster_needs_warning(result: &RoiObservation) -> bool {
    home_booster_slot(result)
        .is_some_and(|slot| slot.kind == SlotKind::HomeBooster && slot.classification.is_none())
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

fn collect_home_booster(result: &RoiObservation) -> Result<Option<String>> {
    let Some(slot) = home_booster_slot(result) else {
        return Ok(None);
    };

    debug_assert!(slot.kind.is_home_booster());
    Ok(slot
        .classification
        .as_ref()
        .map(|classification| classification.item_id.clone()))
}

fn find_home_row(result: &RoiObservation) -> Option<([&Slot; 4], &Slot)> {
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
