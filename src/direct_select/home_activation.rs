use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, debug_span};

use crate::capture::CaptureSource;
use crate::input;
use crate::item::ItemKind;
use crate::preset_flow::{UiState, home_booster_slot, wait_for_ui_state};
use crate::runtime::RecognizerRuntime;
use crate::vision::RoiObservation;

use super::CLICK_HOLD_MS;

const LIST_OPEN_TIMEOUT: Duration = Duration::from_millis(1500);
const HOME_LAYOUT_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug)]
pub(super) struct HomeOpenTarget {
    pub(super) item_kind: ItemKind,
    pub(super) click_x: i32,
    pub(super) click_y: i32,
}

pub(super) fn open_slot_list(
    capture: &mut CaptureSource,
    runtime: &RecognizerRuntime,
    target: HomeOpenTarget,
) -> Result<RoiObservation> {
    let span = debug_span!("open_slot_list", item_kind = %target.item_kind.label());
    let _guard = span.enter();
    let target_state = UiState::List(target.item_kind);

    debug!(
        x = target.click_x,
        y = target.click_y,
        hold_ms = CLICK_HOLD_MS,
        "opening home list with a direct mouse click"
    );
    input::click_with_boundary(
        target.click_x,
        target.click_y,
        CLICK_HOLD_MS,
        || capture.sync_to_latest(),
    )?;

    let observation = wait_for_ui_state(capture, runtime, target_state, LIST_OPEN_TIMEOUT)?;

    Ok(observation)
}

pub(super) fn wait_for_home_booster_target(
    capture: &mut CaptureSource,
    runtime: &RecognizerRuntime,
) -> Result<HomeOpenTarget> {
    let span = debug_span!("wait_home_booster_slot");
    let _guard = span.enter();

    let observation = wait_for_ui_state(
        capture,
        runtime,
        UiState::HomeFilled,
        HOME_LAYOUT_TIMEOUT,
    )?;
    let slot = home_booster_slot(&observation)
        .context("stable home layout did not contain a booster slot")?;
    let (click_x, click_y) = observation.screen_center(slot);

    debug!(
        click_x,
        click_y,
        booster_x = slot.x,
        booster_y = slot.y,
        booster_w = slot.w,
        booster_h = slot.h,
        "stable home booster slot ready for direct mouse click"
    );
    Ok(HomeOpenTarget {
        item_kind: ItemKind::Booster,
        click_x,
        click_y,
    })
}
