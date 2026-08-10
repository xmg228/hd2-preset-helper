mod click_plan;
mod home_activation;
mod hover;
mod page_navigation;
mod page_relation;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, debug_span, info_span};

use crate::app_events::{AppEvent, AppEventSink};
use crate::capture::CaptureSource;
use crate::input;
use crate::item::ItemKind;
use crate::page_sync::capture_latest_roi_frame;
use crate::preset_flow::{UiState, detect_ui_state, empty_loadout_entry_slot};
use crate::runtime::RecognizerRuntime;
use crate::slot::SlotLayout;
use crate::vision::RoiObservation;

use self::click_plan::{DirectClickTarget, find_visible_target, next_visible_target};
use self::home_activation::{HomeOpenTarget, open_slot_list, wait_for_home_booster_target};
use self::hover::{HoverSample, HoverVerifier};
use self::page_navigation::{PageNavigator, PageSnapshot, PageTurnInput, PageTurnResult};

const MAX_WHEEL_INPUTS: u32 = 20;
const CLICK_HOLD_MS: u64 = 45;
const MAX_TARGET_CLICK_ATTEMPTS: usize = 3;
const MAX_HOVER_RELOCATIONS: usize = 4;
const TARGET_POSITION_TOLERANCE: u32 = 2;
const POST_CLICK_CONFIRM_TIMEOUT: Duration = Duration::from_millis(400);
const POST_CLICK_MIN_OBSERVATIONS: u32 = 2;
const TERMINAL_SETTLE_TIMEOUT: Duration = Duration::from_millis(600);

pub fn apply_empty_loadout_preset(
    runtime: &RecognizerRuntime,
    capture: &mut CaptureSource,
    events: &AppEventSink,
    items: &[String],
) -> Result<()> {
    if items.len() != 4 {
        bail!(
            "empty loadout preset application requires exactly 4 stratagems, got {}",
            items.len()
        );
    }

    let opened_list = {
        let frame = capture_latest_roi_frame(capture, runtime.calibration())?;
        let result = runtime.recognize(frame, SlotLayout::Home)?;
        let Some(entry_slot) = empty_loadout_entry_slot(&result).cloned() else {
            bail!("current screen is not an empty loadout home layout");
        };

        let (click_x, click_y) = result.screen_center(&entry_slot);
        let target = HomeOpenTarget {
            item_kind: ItemKind::Stratagem,
            click_x,
            click_y,
        };
        open_slot_list(capture, runtime, target)?
    };
    select_items_from_open_list(
        runtime,
        capture,
        events,
        items,
        ItemKind::Stratagem,
        opened_list,
    )
}

pub fn apply_booster_from_home(
    runtime: &RecognizerRuntime,
    capture: &mut CaptureSource,
    events: &AppEventSink,
    items: &[String],
) -> Result<()> {
    if items.len() != 1 {
        bail!(
            "booster preset application requires exactly 1 booster, got {}",
            items.len()
        );
    }

    let target = wait_for_home_booster_target(capture, runtime)?;
    let opened_list = open_slot_list(capture, runtime, target)?;
    select_items_from_open_list(
        runtime,
        capture,
        events,
        items,
        ItemKind::Booster,
        opened_list,
    )
}

fn select_items_from_open_list(
    runtime: &RecognizerRuntime,
    capture: &mut CaptureSource,
    events: &AppEventSink,
    items: &[String],
    item_kind: ItemKind,
    initial_observation: RoiObservation,
) -> Result<()> {
    let span = info_span!(
        "preset_list_selection",
        item_kind = %item_kind.label(),
        requested_items = items.len()
    );
    let _guard = span.enter();
    events.emit(AppEvent::ListSelectionStarted {
        item_kind,
        requested_items: items.len(),
    });

    let navigator = PageNavigator::new(runtime, item_kind);
    let mut hover_verifier = HoverVerifier::default();
    let mut remaining = items.to_vec();
    let mut wheel_attempts = 0u32;
    let mut end_candidate = false;

    let mut current_page = {
        let span = debug_span!("scan_page", wheel_attempts);
        let _guard = span.enter();
        navigator.prepare_direct_page(initial_observation)?
    };

    loop {
        let page_span = debug_span!("selection_page", wheel_attempts, end_candidate);
        let _page_guard = page_span.enter();
        if let Some(target) = next_visible_target(&current_page.roi, &remaining, item_kind) {
            let span = debug_span!("select_visible_item", item_id = %target.item_id);
            let _guard = span.enter();
            let selected_item_id = target.item_id.clone();
            let final_requested_item = remaining.len() == 1;
            let outcome = select_preset_target(
                capture,
                &navigator,
                target,
                item_kind,
                &mut hover_verifier,
                final_requested_item,
            )?;
            events.emit(AppEvent::ItemSelected {
                item_id: selected_item_id.clone(),
            });
            remove_remaining(&mut remaining, &selected_item_id);
            match outcome {
                TargetSelectionOutcome::List {
                    page,
                    viewport_repositioned,
                } => {
                    current_page = page;
                    if viewport_repositioned {
                        end_candidate = false;
                    }
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

        if remaining.is_empty() {
            return Ok(());
        }

        if wheel_attempts >= MAX_WHEEL_INPUTS {
            break;
        }

        let span = debug_span!("turn_page");
        let _guard = span.enter();
        let input = if end_candidate {
            PageTurnInput::EndProbe
        } else {
            PageTurnInput::Full
        };
        let wheel_attempt = wheel_attempts + 1;

        match navigator.perform_confirmed_semantic_page_turn(
            capture,
            current_page,
            input,
            wheel_attempt,
        )? {
            PageTurnResult::Moved { page, short } => {
                current_page = page;
                wheel_attempts = wheel_attempt;
                end_candidate = matches!(input, PageTurnInput::Full) && short;
                if end_candidate {
                    debug!(
                        remaining_items = remaining.len(),
                        "short page turn accepted; list end will be probed after processing this page"
                    );
                } else if matches!(input, PageTurnInput::EndProbe) {
                    debug!(
                        remaining_items = remaining.len(),
                        "end probe moved the viewport; normal page turns will resume"
                    );
                }
            }
            PageTurnResult::NoMovement(last_page) => {
                if matches!(input, PageTurnInput::EndProbe) {
                    debug!(
                        remaining_items = remaining.len(),
                        "list end confirmed by a conservative end probe"
                    );
                    bail!(
                        "{} list end reached with {} preset items still missing: {}",
                        item_kind.label(),
                        remaining.len(),
                        remaining.join(", ")
                    );
                }
                wheel_attempts = wheel_attempt;
                current_page = last_page;
                end_candidate = true;
                debug!(
                    remaining_items = remaining.len(),
                    "full page turn produced no movement; scheduling a small end probe"
                );
            }
        }
    }

    bail!(
        "{} preset items not found after {} wheel inputs: {}",
        item_kind.label(),
        MAX_WHEEL_INPUTS,
        remaining.join(", ")
    )
}

enum TargetSelectionOutcome {
    List {
        page: PageSnapshot,
        viewport_repositioned: bool,
    },
    Home,
}

fn select_preset_target(
    capture: &mut CaptureSource,
    navigator: &PageNavigator<'_>,
    initial_target: DirectClickTarget,
    item_kind: ItemKind,
    hover_verifier: &mut HoverVerifier,
    final_requested_item: bool,
) -> Result<TargetSelectionOutcome> {
    let item_id = initial_target.item_id.clone();
    let prepared = relocate_and_wait_hover(
        capture,
        navigator,
        &item_id,
        item_kind,
        initial_target,
        hover_verifier,
    )?;
    let mut target = prepared.target;
    let before = prepared.sample;
    let mut viewport_repositioned = prepared.viewport_repositioned;

    for attempt in 1..=MAX_TARGET_CLICK_ATTEMPTS {
        debug!(
            item_id = %target.item_id,
            attempt,
            x = target.x,
            y = target.y,
            match_score = target.match_score,
            match_margin = target.match_margin,
            gate_quality = target.gate_quality,
            hover_score = before.target_score(),
            "clicking preset item after hover confirmation"
        );

        input::click_current_with_boundary(CLICK_HOLD_MS, || capture.sync_to_latest())?;
        capture.sync_to_latest();
        if final_requested_item {
            if wait_for_terminal_home(capture, navigator, item_kind, POST_CLICK_CONFIRM_TIMEOUT)? {
                return Ok(TargetSelectionOutcome::Home);
            }

            if let Some(retry_target) = unchanged_terminal_target(
                capture,
                navigator,
                &item_id,
                item_kind,
                &target,
                &before,
                hover_verifier,
            )? {
                debug!(
                    item_id = %item_id,
                    attempt,
                    "final click left the target unchanged; retrying in place"
                );
                target = retry_target;
                continue;
            }

            if wait_for_terminal_home(capture, navigator, item_kind, TERMINAL_SETTLE_TIMEOUT)? {
                return Ok(TargetSelectionOutcome::Home);
            }
            bail!(
                "final {} click changed the list state but did not return to a confirmed loadout home",
                item_kind.label()
            );
        }

        match observe_post_click_state(
            capture,
            navigator,
            &item_id,
            item_kind,
            &target,
            &before,
            hover_verifier,
        )? {
            PostClickObservation::Selected {
                page,
                after_score,
                moved,
            } => {
                viewport_repositioned |= moved;
                debug!(
                    item_id = %item_id,
                    attempt,
                    viewport_repositioned,
                    before_score = before.target_score(),
                    after_score,
                    "preset item selected state confirmed"
                );
                return Ok(TargetSelectionOutcome::List {
                    page,
                    viewport_repositioned,
                });
            }
            PostClickObservation::Unchanged {
                target: retry_target,
                after_score,
            } => {
                debug!(
                    item_id = %item_id,
                    attempt,
                    before_score = before.target_score(),
                    after_score,
                    "click left the target at the same position and brightness; retrying in place"
                );
                target = retry_target;
            }
        }
    }

    if final_requested_item {
        bail!(
            "final target item {item_id} remained unchanged after {MAX_TARGET_CLICK_ATTEMPTS} confirmed click attempts"
        );
    }
    bail!(
        "target item {item_id} neither moved nor dimmed after {MAX_TARGET_CLICK_ATTEMPTS} click attempts"
    )
}

enum PostClickObservation {
    Selected {
        page: PageSnapshot,
        after_score: f32,
        moved: bool,
    },
    Unchanged {
        target: DirectClickTarget,
        after_score: f32,
    },
}

fn observe_post_click_state(
    capture: &mut CaptureSource,
    navigator: &PageNavigator<'_>,
    item_id: &str,
    item_kind: ItemKind,
    clicked_target: &DirectClickTarget,
    before: &HoverSample,
    hover_verifier: &HoverVerifier,
) -> Result<PostClickObservation> {
    let started = Instant::now();
    let mut observation = 0u32;

    loop {
        observation += 1;
        let frame = capture_latest_roi_frame(capture, navigator.calibration())?;
        let page = navigator.scan_direct_page(frame)?;
        let Some(target) = find_visible_target(&page.roi, item_id, item_kind) else {
            debug!(
                item_id,
                observation,
                "post-click target is absent from the current viewport; observing another frame"
            );
            if started.elapsed() >= POST_CLICK_CONFIRM_TIMEOUT
                && observation >= POST_CLICK_MIN_OBSERVATIONS
            {
                bail!(
                    "target item {item_id} left the visible viewport before its selected state could be confirmed"
                );
            }
            continue;
        };

        let moved = clicked_target.slot.x.abs_diff(target.slot.x) > TARGET_POSITION_TOLERANCE
            || clicked_target.slot.y.abs_diff(target.slot.y) > TARGET_POSITION_TOLERANCE;

        let sample = hover_verifier.sample_current_frame(&page.roi.image, &target.slot)?;
        let brightness_dropped = !moved && sample.is_dimmer_than(before);

        if moved || brightness_dropped {
            debug!(
                item_id,
                observation,
                moved,
                brightness_dropped,
                before_score = before.target_score(),
                after_score = sample.target_score(),
                "post-click success confirmed by movement or brightness drop"
            );
            return Ok(PostClickObservation::Selected {
                page,
                after_score: sample.target_score(),
                moved,
            });
        }

        if started.elapsed() >= POST_CLICK_CONFIRM_TIMEOUT
            && observation >= POST_CLICK_MIN_OBSERVATIONS
        {
            return Ok(PostClickObservation::Unchanged {
                target,
                after_score: sample.target_score(),
            });
        }
    }
}

struct PreparedHover {
    target: DirectClickTarget,
    sample: HoverSample,
    viewport_repositioned: bool,
}

fn relocate_and_wait_hover(
    capture: &mut CaptureSource,
    navigator: &PageNavigator<'_>,
    item_id: &str,
    item_kind: ItemKind,
    mut target: DirectClickTarget,
    hover_verifier: &mut HoverVerifier,
) -> Result<PreparedHover> {
    let mut viewport_repositioned = false;
    let mut last_hover_error = None;

    for relocation in 1..=MAX_HOVER_RELOCATIONS {
        capture.sync_to_latest();
        input::move_cursor(target.x, target.y)?;

        let frame = capture_latest_roi_frame(capture, navigator.calibration())?;
        let page = navigator.scan_direct_page(frame)?;
        let relocated_target = find_visible_target(&page.roi, item_id, item_kind).with_context(|| {
            format!(
                "target item {item_id} left the visible viewport while relocating its hover position"
            )
        })?;
        let moved = target.slot.x.abs_diff(relocated_target.slot.x) > TARGET_POSITION_TOLERANCE
            || target.slot.y.abs_diff(relocated_target.slot.y) > TARGET_POSITION_TOLERANCE;
        viewport_repositioned |= moved;

        if moved {
            debug!(
                item_id,
                relocation,
                old_x = target.x,
                old_y = target.y,
                new_x = relocated_target.x,
                new_y = relocated_target.y,
                "target moved after cursor placement; relocating again"
            );
            target = relocated_target;
            continue;
        }

        match hover_verifier.wait_at_current_position(
            capture,
            navigator.calibration(),
            &page.roi.slots,
            &relocated_target.slot,
        ) {
            Ok(sample) => {
                return Ok(PreparedHover {
                    target: relocated_target,
                    sample,
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
    capture: &mut CaptureSource,
    navigator: &PageNavigator<'_>,
    item_id: &str,
    item_kind: ItemKind,
    clicked_target: &DirectClickTarget,
    before: &HoverSample,
    hover_verifier: &HoverVerifier,
) -> Result<Option<DirectClickTarget>> {
    let frame = capture_latest_roi_frame(capture, navigator.calibration())?;
    let Ok(page) = navigator.scan_direct_page(frame) else {
        return Ok(None);
    };
    let Some(target) = find_visible_target(&page.roi, item_id, item_kind) else {
        return Ok(None);
    };
    let moved = clicked_target.slot.x.abs_diff(target.slot.x) > TARGET_POSITION_TOLERANCE
        || clicked_target.slot.y.abs_diff(target.slot.y) > TARGET_POSITION_TOLERANCE;
    let sample = hover_verifier.sample_current_frame(&page.roi.image, &target.slot)?;
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
    capture: &mut CaptureSource,
    navigator: &PageNavigator<'_>,
    item_kind: ItemKind,
    timeout: Duration,
) -> Result<bool> {
    let started = Instant::now();
    loop {
        let frame = capture_latest_roi_frame(capture, navigator.calibration())?;
        if let Ok(home) = navigator.detect_home(frame)
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

fn remove_remaining(remaining: &mut Vec<String>, item_id: &str) {
    if let Some(index) = remaining.iter().position(|item| item == item_id) {
        remaining.remove(index);
    }
}
