mod click_plan;
mod home_activation;
mod hover;
mod list_map;
mod page_navigation;
mod page_relation;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, debug_span, info_span};

use crate::app_events::{AppEvent, AppEventSink};
use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::vision::{RecognizerRuntime, RoiObservation, Slot, SlotLayout};

use super::{UiState, detect_ui_state, empty_loadout_entry_slot};

use self::click_plan::{DirectClickTarget, find_visible_target, next_visible_target};
use self::home_activation::{HomeOpenTarget, open_slot_list, wait_for_home_booster_target};
use self::hover::{HoverSample, HoverVerifier};
use self::list_map::{ListMap, NavigationHint};
use self::page_navigation::{PageNavigator, PageSnapshot, PageTurnInput, PageTurnResult};
use self::page_relation::common_identity_vertical_shift;

const MAX_WHEEL_INPUTS: u32 = 20;
const CLICK_HOLD_MS: u64 = 45;
const MAX_TARGET_CLICK_ATTEMPTS: usize = 3;
const MAX_HOVER_RELOCATIONS: usize = 4;
const TARGET_POSITION_TOLERANCE: u32 = 2;
const POST_CLICK_ROW_SNAP_TOLERANCE: f32 = 4.0;
const POST_CLICK_CONFIRM_TIMEOUT: Duration = Duration::from_millis(400);
const POST_CLICK_MIN_OBSERVATIONS: u32 = 2;
const TERMINAL_SETTLE_TIMEOUT: Duration = Duration::from_millis(600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollDirection {
    Up,
    Down,
}

impl ScrollDirection {
    const fn boundary_label(self) -> &'static str {
        match self {
            Self::Up => "top",
            Self::Down => "bottom",
        }
    }
}

pub fn apply_empty_loadout_preset(
    runtime: &RecognizerRuntime,
    automation: &mut AutomationSession<'_>,
    events: &AppEventSink,
    items: &[String],
    apply_in_saved_order: bool,
) -> Result<()> {
    if items.len() != 4 {
        bail!(
            "empty loadout preset application requires exactly 4 stratagems, got {}",
            items.len()
        );
    }

    let opened_list = {
        let image = automation.capture()?;
        let result = runtime.recognize(image, SlotLayout::Home)?;
        let Some(entry_slot) = empty_loadout_entry_slot(&result).cloned() else {
            bail!("current screen is not an empty loadout home layout");
        };

        let target = HomeOpenTarget {
            item_kind: ItemKind::Stratagem,
            point: entry_slot.center(),
        };
        open_slot_list(automation, runtime, target)?
    };
    select_items_from_open_list(
        runtime,
        automation,
        events,
        items,
        ItemKind::Stratagem,
        opened_list,
        apply_in_saved_order,
    )
}

pub fn apply_booster_from_home(
    runtime: &RecognizerRuntime,
    automation: &mut AutomationSession<'_>,
    events: &AppEventSink,
    items: &[String],
) -> Result<()> {
    if items.len() != 1 {
        bail!(
            "booster preset application requires exactly 1 booster, got {}",
            items.len()
        );
    }

    let target = wait_for_home_booster_target(automation, runtime)?;
    let opened_list = open_slot_list(automation, runtime, target)?;
    select_items_from_open_list(
        runtime,
        automation,
        events,
        items,
        ItemKind::Booster,
        opened_list,
        true,
    )
}

fn select_items_from_open_list(
    runtime: &RecognizerRuntime,
    automation: &mut AutomationSession<'_>,
    events: &AppEventSink,
    items: &[String],
    item_kind: ItemKind,
    initial_observation: RoiObservation,
    apply_in_saved_order: bool,
) -> Result<()> {
    let span = info_span!(
        "preset_list_selection",
        item_kind = %item_kind.label(),
        requested_items = items.len(),
        apply_in_saved_order
    );
    let _guard = span.enter();
    events.emit(AppEvent::ListSelectionStarted {
        item_kind,
        requested_items: items.len(),
    });

    let navigator = PageNavigator::new(runtime, item_kind);
    let mut hover_verifier = HoverVerifier;
    let mut remaining = items.to_vec();
    let mut wheel_attempts = 0u32;
    let mut boundary_candidate = None;

    let mut current_page = {
        let span = debug_span!("scan_page", wheel_attempts);
        let _guard = span.enter();
        navigator.prepare_direct_page(initial_observation)?
    };
    let mut list_map = ListMap::new(&current_page.roi, item_kind);

    loop {
        let page_span = debug_span!(
            "selection_page",
            wheel_attempts,
            remaining_items = remaining.len(),
            ?boundary_candidate
        );
        let _page_guard = page_span.enter();
        let target = if apply_in_saved_order {
            find_visible_target(&current_page.roi, &remaining[0], item_kind)
        } else {
            next_visible_target(&current_page.roi, &remaining, item_kind)
        };
        if let Some(target) = target {
            let span = debug_span!("select_visible_item", item_id = %target.item_id);
            let _guard = span.enter();
            let selected_item_id = target.item_id.clone();
            let final_requested_item = remaining.len() == 1;
            let outcome = select_preset_target(
                automation,
                &navigator,
                target,
                item_kind,
                &mut hover_verifier,
                final_requested_item,
            )?;
            events.emit(AppEvent::ItemSelected {
                item_id: selected_item_id.clone(),
            });
            remaining.retain(|item_id| item_id != &selected_item_id);
            match outcome {
                TargetSelectionOutcome::List {
                    page,
                    viewport_repositioned,
                } => {
                    if !list_map.relocalize(&page.roi, item_kind) {
                        if viewport_repositioned {
                            bail!(
                                "target item {selected_item_id} changed the viewport without a shared landmark for the temporary list map"
                            );
                        }
                        list_map.record_current(&page.roi, item_kind);
                    }
                    current_page = page;
                    wheel_attempts = 0;
                    boundary_candidate = None;
                    debug!(
                        item_id = %selected_item_id,
                        remaining_items = remaining.len(),
                        viewport_repositioned,
                        "single-item selection confirmed"
                    );
                    continue;
                }
                TargetSelectionOutcome::Home => {
                    debug!(
                        item_id = %selected_item_id,
                        remaining_items = remaining.len(),
                        "final item selection confirmed after returning home"
                    );
                    debug_assert!(remaining.is_empty());
                    return Ok(());
                }
            }
        }

        let item_id = &remaining[0];
        if wheel_attempts >= MAX_WHEEL_INPUTS {
            bail!(
                "{} preset item {item_id} not found after {} wheel inputs",
                item_kind.label(),
                MAX_WHEEL_INPUTS,
            );
        }

        let hint = list_map.navigation_hint(item_id, &current_page.roi, item_kind);
        let (direction, recenter) = match hint {
            NavigationHint::Scroll(direction) => (direction, false),
            NavigationHint::Recenter(direction) => (direction, true),
            NavigationHint::ExpectedVisible => {
                bail!(
                    "mapped target item {item_id} was not recognized in its expected visible row"
                );
            }
        };
        let span = debug_span!("turn_page");
        let _guard = span.enter();
        let input = if recenter || boundary_candidate == Some(direction) {
            PageTurnInput::Probe(direction)
        } else {
            PageTurnInput::Full(direction)
        };
        let wheel_attempt = wheel_attempts + 1;

        match navigator.perform_confirmed_semantic_page_turn(
            automation,
            &current_page,
            input,
            wheel_attempt,
        )? {
            PageTurnResult::Moved { page, short } => {
                list_map.advance(&current_page.roi, &page.roi, item_kind, direction);
                current_page = page;
                wheel_attempts = wheel_attempt;
                boundary_candidate = (input.is_full() && short).then_some(direction);
                if boundary_candidate.is_some() {
                    debug!(
                        remaining_items = remaining.len(),
                        ?direction,
                        "short page turn accepted; list boundary will be probed after processing this page"
                    );
                } else if input.is_probe() {
                    debug!(
                        remaining_items = remaining.len(),
                        ?direction,
                        "probe moved the viewport; normal page turns will resume"
                    );
                }
            }
            PageTurnResult::NoMovement(last_page) => {
                list_map.record_current(&last_page.roi, item_kind);
                current_page = last_page;
                wheel_attempts = wheel_attempt;
                if input.is_probe() {
                    let reason = if recenter {
                        "could not be recognized after recentering"
                    } else {
                        "was not found"
                    };
                    debug!(
                        remaining_items = remaining.len(),
                        ?direction,
                        "list boundary confirmed by a conservative probe"
                    );
                    bail!(
                        "target item {item_id} {reason} before the {} of the {} list",
                        direction.boundary_label(),
                        item_kind.label(),
                    );
                }
                boundary_candidate = Some(direction);
                debug!(
                    remaining_items = remaining.len(),
                    ?direction,
                    "full page turn produced no movement; scheduling a small boundary probe"
                );
            }
        }
    }
}

enum TargetSelectionOutcome {
    List {
        page: PageSnapshot,
        viewport_repositioned: bool,
    },
    Home,
}

fn select_preset_target(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator<'_>,
    initial_target: DirectClickTarget,
    item_kind: ItemKind,
    hover_verifier: &mut HoverVerifier,
    final_requested_item: bool,
) -> Result<TargetSelectionOutcome> {
    let item_id = initial_target.item_id.clone();
    let prepared = relocate_and_wait_hover(
        automation,
        navigator,
        &item_id,
        item_kind,
        initial_target,
        hover_verifier,
    )?;
    let mut target = prepared.target;
    let before = prepared.sample;
    let mut before_slots = prepared.page_slots;
    let mut viewport_repositioned = prepared.viewport_repositioned;
    let mut last_after_score = None;

    for attempt in 1..=MAX_TARGET_CLICK_ATTEMPTS {
        let (x, y) = target.slot.center();
        debug!(
            item_id = %target.item_id,
            attempt,
            x,
            y,
            match_score = target.match_score,
            match_margin = target.match_margin,
            gate_quality = target.gate_quality,
            hover_score = before.target_score(),
            "clicking preset item after hover confirmation"
        );

        automation.click_current(CLICK_HOLD_MS)?;
        if final_requested_item {
            if wait_for_terminal_home(automation, navigator, item_kind, POST_CLICK_CONFIRM_TIMEOUT)?
            {
                return Ok(TargetSelectionOutcome::Home);
            }

            if let Some(retry_target) = unchanged_terminal_target(
                automation, navigator, &item_id, item_kind, &target, &before,
            )? {
                debug!(
                    item_id = %item_id,
                    attempt,
                    "final click left the target unchanged; retrying in place"
                );
                target = retry_target;
                continue;
            }

            if wait_for_terminal_home(automation, navigator, item_kind, TERMINAL_SETTLE_TIMEOUT)? {
                return Ok(TargetSelectionOutcome::Home);
            }
            bail!(
                "final {} click changed the list state but did not return to a confirmed loadout home",
                item_kind.label()
            );
        }

        match observe_post_click_state(automation, navigator, &target, &before, &before_slots)? {
            PostClickObservation::Selected { page, moved } => {
                viewport_repositioned |= moved;
                debug!(
                    item_id = %item_id,
                    attempt,
                    viewport_repositioned,
                    "preset item selected state confirmed"
                );
                return Ok(TargetSelectionOutcome::List {
                    page,
                    viewport_repositioned,
                });
            }
            PostClickObservation::Unchanged {
                slot,
                page,
                after_score,
            } => {
                debug!(
                    item_id = %item_id,
                    attempt,
                    "click left the target at the same position and brightness; retrying in place"
                );
                target.slot = slot;
                before_slots = page.roi.slots;
                last_after_score = Some(after_score);
            }
        }
    }

    if final_requested_item {
        bail!(
            "final target item {item_id} remained unchanged after {MAX_TARGET_CLICK_ATTEMPTS} confirmed click attempts"
        );
    }
    let after_score = last_after_score.unwrap_or(before.target_score());
    bail!(
        "target item {item_id} neither moved nor dimmed after {MAX_TARGET_CLICK_ATTEMPTS} click attempts (before={:.1}, after={after_score:.1}, required drop={:.1})",
        before.target_score(),
        before.required_score_drop(),
    )
}

enum PostClickObservation {
    Selected {
        page: PageSnapshot,
        moved: bool,
    },
    Unchanged {
        slot: Slot,
        page: PageSnapshot,
        after_score: f32,
    },
}

fn observe_post_click_state(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator<'_>,
    clicked_target: &DirectClickTarget,
    before: &HoverSample,
    before_slots: &[Slot],
) -> Result<PostClickObservation> {
    let started = Instant::now();
    let mut observation = 0u32;
    let item_id = clicked_target.item_id.as_str();
    let item_kind = clicked_target
        .slot
        .kind
        .classification_kind()
        .context("clicked target slot has no classifiable item kind")?;

    loop {
        observation += 1;
        let image = automation.capture()?;
        let page = navigator.scan_direct_page(image)?;
        let vertical_shift = common_identity_vertical_shift(before_slots, &page.roi, item_kind);
        let moved =
            vertical_shift.is_some_and(|shift| shift.abs() > TARGET_POSITION_TOLERANCE as f32);
        if moved {
            debug!(
                item_id,
                observation,
                vertical_shift,
                "post-click success confirmed by vertical viewport movement"
            );
            return Ok(PostClickObservation::Selected { page, moved: true });
        }

        let Some(slot) = track_post_click_slot(
            &page,
            item_kind,
            clicked_target,
            vertical_shift.unwrap_or(0.0),
        ) else {
            debug!(
                item_id,
                observation, "post-click target row cannot be tracked; observing another frame"
            );
            if started.elapsed() >= POST_CLICK_CONFIRM_TIMEOUT
                && observation >= POST_CLICK_MIN_OBSERVATIONS
            {
                bail!("target item {item_id} could not be tracked after clicking");
            }
            continue;
        };

        let sample = HoverVerifier::sample_current_frame(&page.roi.image, &slot)?;
        let brightness_dropped = sample.is_dimmer_than(before);

        if brightness_dropped {
            debug!(
                item_id,
                observation,
                before_score = before.target_score(),
                after_score = sample.target_score(),
                "post-click success confirmed by brightness drop"
            );
            return Ok(PostClickObservation::Selected { page, moved: false });
        }

        if started.elapsed() >= POST_CLICK_CONFIRM_TIMEOUT
            && observation >= POST_CLICK_MIN_OBSERVATIONS
        {
            debug!(
                item_id,
                observation,
                before_score = before.target_score(),
                after_score = sample.target_score(),
                "post-click target remained bright"
            );
            return Ok(PostClickObservation::Unchanged {
                slot,
                page,
                after_score: sample.target_score(),
            });
        }
    }
}

fn track_post_click_slot(
    current_page: &PageSnapshot,
    item_kind: ItemKind,
    clicked_target: &DirectClickTarget,
    vertical_shift: f32,
) -> Option<Slot> {
    if let Some(target) = find_visible_target(&current_page.roi, &clicked_target.item_id, item_kind)
    {
        return Some(target.slot);
    }

    let predicted_y = clicked_target.slot.center_f32().1 + vertical_shift;
    let slot = current_page
        .roi
        .slots
        .iter()
        .filter(|slot| {
            slot.kind.is_selectable_item_for(item_kind) && slot.col == clicked_target.slot.col
        })
        .map(|slot| (slot, (slot.center_f32().1 - predicted_y).abs()))
        .min_by(|left, right| left.1.total_cmp(&right.1))?;
    (slot.1 <= POST_CLICK_ROW_SNAP_TOLERANCE).then(|| slot.0.clone())
}

struct PreparedHover {
    target: DirectClickTarget,
    sample: HoverSample,
    page_slots: Vec<Slot>,
    viewport_repositioned: bool,
}

fn relocate_and_wait_hover(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator<'_>,
    item_id: &str,
    item_kind: ItemKind,
    mut target: DirectClickTarget,
    hover_verifier: &mut HoverVerifier,
) -> Result<PreparedHover> {
    let mut viewport_repositioned = false;
    let mut last_hover_error = None;

    for relocation in 1..=MAX_HOVER_RELOCATIONS {
        automation.move_cursor(target.slot.center())?;

        let image = automation.capture()?;
        let page = navigator.scan_direct_page(image)?;
        let relocated_target = find_visible_target(&page.roi, item_id, item_kind).with_context(|| {
            format!(
                "target item {item_id} left the visible viewport while relocating its hover position"
            )
        })?;
        let moved = target.slot.x.abs_diff(relocated_target.slot.x) > TARGET_POSITION_TOLERANCE
            || target.slot.y.abs_diff(relocated_target.slot.y) > TARGET_POSITION_TOLERANCE;
        viewport_repositioned |= moved;

        if moved {
            let (old_x, old_y) = target.slot.center();
            let (new_x, new_y) = relocated_target.slot.center();
            debug!(
                item_id,
                relocation,
                old_x,
                old_y,
                new_x,
                new_y,
                "target moved after cursor placement; relocating again"
            );
            target = relocated_target;
            continue;
        }

        match hover_verifier.wait_at_current_position(
            automation,
            &page.roi.slots,
            &relocated_target.slot,
        ) {
            Ok(sample) => {
                return Ok(PreparedHover {
                    target: relocated_target,
                    sample,
                    page_slots: page.roi.slots,
                    viewport_repositioned,
                });
            }
            Err(error) => {
                debug!(
                    item_id,
                    relocation,
                    error = %error,
                    "hover confirmation failed; rescanning and relocating target"
                );
                last_hover_error = Some(error);
                target = relocated_target;
            }
        }
    }

    let suffix = last_hover_error
        .map(|error| format!(": {error:#}"))
        .unwrap_or_default();
    bail!(
        "target item {item_id} did not stabilize under the cursor after {MAX_HOVER_RELOCATIONS} relocations{suffix}"
    )
}

fn unchanged_terminal_target(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator<'_>,
    item_id: &str,
    item_kind: ItemKind,
    clicked_target: &DirectClickTarget,
    before: &HoverSample,
) -> Result<Option<DirectClickTarget>> {
    let image = automation.capture()?;
    let Ok(page) = navigator.scan_direct_page(image) else {
        return Ok(None);
    };
    let Some(target) = find_visible_target(&page.roi, item_id, item_kind) else {
        return Ok(None);
    };
    let moved = clicked_target.slot.x.abs_diff(target.slot.x) > TARGET_POSITION_TOLERANCE
        || clicked_target.slot.y.abs_diff(target.slot.y) > TARGET_POSITION_TOLERANCE;
    let sample = HoverVerifier::sample_current_frame(&page.roi.image, &target.slot)?;
    let brightness_dropped = !moved && sample.is_dimmer_than(before);

    if moved || brightness_dropped {
        debug!(
            item_id,
            moved,
            brightness_dropped,
            before_score = before.target_score(),
            after_score = sample.target_score(),
            "final click changed the target state; continuing to wait for home"
        );
        return Ok(None);
    }

    Ok(Some(target))
}

fn wait_for_terminal_home(
    automation: &mut AutomationSession<'_>,
    navigator: &PageNavigator<'_>,
    item_kind: ItemKind,
    timeout: Duration,
) -> Result<bool> {
    let started = Instant::now();
    loop {
        let image = automation.capture()?;
        if let Ok(home) = navigator.detect_home(image)
            && detect_ui_state(&home) == UiState::HomeFilled
        {
            debug!(
                item_kind = %item_kind.label(),
                elapsed = ?started.elapsed(),
                "terminal item selection returned to the loadout home"
            );
            return Ok(true);
        }
        if started.elapsed() >= timeout {
            return Ok(false);
        }
    }
}
