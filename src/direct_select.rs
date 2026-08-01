mod click_plan;
mod page_relation;
mod page_navigation;
mod home_activation;

use std::time::Duration;

use anyhow::{Result, bail};
use tracing::{debug, debug_span, info_span};

use crate::app_events::{AppEvent, AppEventSink};
use crate::capture::CaptureSource;
use crate::input;
use crate::page_sync::capture_latest_roi_frame;
use crate::item::ItemKind;
use crate::preset_flow::empty_loadout_entry_slot;
use crate::runtime::RecognizerRuntime;
use crate::slot::SlotLayout;
use crate::vision::RoiObservation;

use self::click_plan::{DirectClickTarget, build as build_click_plan};
use self::home_activation::{HomeOpenTarget, open_slot_list, wait_for_home_booster_target};
use self::page_navigation::{PageNavigator, PageTurnResult};

const MAX_PAGES: u32 = 10;
const CLICK_HOLD_MS: u64 = 30;
const AFTER_CLICK_MS: u64 = 10;

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
    let mut remaining = items.to_vec();
    let mut page_index = 0u32;
    let mut list_end_confirmed = false;

    let mut current_page = {
        let span = debug_span!("scan_page", page_index);
        let _guard = span.enter();
        navigator.prepare_direct_page(initial_observation)?
    };

    loop {
        let page_span = debug_span!("selection_page", page_index);
        let _page_guard = page_span.enter();
        let clicked = {
            let span = debug_span!("select_visible_items");
            let _guard = span.enter();

            click_visible_preset_targets(
                capture,
                &current_page.roi,
                &mut remaining,
                item_kind,
                list_end_confirmed,
                events,
            )?
        };

        debug!(
            selected_items = clicked,
            remaining_items = remaining.len(),
            "page selection summary"
        );

        if remaining.is_empty() {
            return Ok(());
        }

        // A clean no-movement result or a calibrated short final page turn can
        // confirm the physical list end, where automatic downward snapping can
        // no longer invalidate the remaining coordinates.
        if list_end_confirmed || page_index + 1 >= MAX_PAGES {
            break;
        }

        let span = debug_span!("turn_page");
        let _guard = span.enter();

        match navigator.perform_confirmed_semantic_page_turn(
            capture,
            current_page,
            page_index + 1,
        )? {
            PageTurnResult::Moved { page, reached_end } => {
                current_page = page;
                page_index += 1;
                list_end_confirmed = reached_end;
                if reached_end {
                    debug!(
                        remaining_items = remaining.len(),
                        "list end reached after short page turn"
                    );
                }
            }
            PageTurnResult::NoMovement(last_page) => {
                current_page = last_page;
                list_end_confirmed = true;
                debug!(
                    remaining_items = remaining.len(),
                    "list end confirmed by no movement"
                );
                continue;
            }
        }
    }

    if list_end_confirmed {
        bail!(
            "{} list end reached with {} preset items still missing: {}",
            item_kind.label(),
            remaining.len(),
            remaining.join(", ")
        );
    }
    bail!(
        "{} preset items not found after {} confirmed pages: {}",
        item_kind.label(),
        MAX_PAGES,
        remaining.join(", ")
    )
}

fn click_visible_preset_targets(
    capture: &mut CaptureSource,
    result: &RoiObservation,
    remaining: &mut Vec<String>,
    item_kind: ItemKind,
    list_end_confirmed: bool,
    events: &AppEventSink,
) -> Result<usize> {
    let plan = build_click_plan(result, remaining, item_kind, list_end_confirmed);
    let mut clicked = 0;

    for target in plan.immediate {
        click_preset_target(capture, &target, false, events)?;
        remove_remaining(remaining, &target.item_id);
        clicked += 1;
    }

    if let Some(target) = plan.terminal_bottom {
        if remaining.first().is_some_and(|item| {
            remaining.len() == 1 && item == &target.item_id
        }) {
            click_preset_target(capture, &target, true, events)?;
            remove_remaining(remaining, &target.item_id);
            clicked += 1;
        } else {
            debug!(
                item_id = %target.item_id,
                remaining_items = remaining.len(),
                center_y_roi = target.center_y_roi,
                "bottom target deferred until a later page"
            );
        }
    }

    Ok(clicked)
}

fn click_preset_target(
    capture: &mut CaptureSource,
    target: &DirectClickTarget,
    terminal_bottom: bool,
    events: &AppEventSink,
) -> Result<()> {
    debug!(
        item_id = %target.item_id,
        x = target.x,
        y = target.y,
        match_score = target.match_score,
        match_margin = target.match_margin,
        gate_quality = target.gate_quality,
        terminal_bottom,
        "selecting preset item"
    );
    input::click_with_boundary(target.x, target.y, CLICK_HOLD_MS, || {
        capture.sync_to_latest()
    })?;
    std::thread::sleep(Duration::from_millis(AFTER_CLICK_MS));
    events.emit(AppEvent::ItemSelected {
        item_id: target.item_id.clone(),
    });
    Ok(())
}

fn remove_remaining(remaining: &mut Vec<String>, item_id: &str) {
    if let Some(index) = remaining.iter().position(|item| item == item_id) {
        remaining.remove(index);
    }
}
