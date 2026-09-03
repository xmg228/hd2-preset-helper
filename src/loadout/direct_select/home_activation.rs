use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, debug_span};

use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::vision::{RecognizerRuntime, RoiObservation};

use super::super::{UiState, home_booster_slot, wait_for_ui_state};

use super::CLICK_HOLD_MS;

const LIST_OPEN_TIMEOUT: Duration = Duration::from_millis(1500);
const HOME_LAYOUT_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug)]
pub(super) struct HomeOpenTarget {
    pub(super) item_kind: ItemKind,
    pub(super) point: (u32, u32),
}

pub(super) fn open_slot_list(
    automation: &mut AutomationSession<'_>,
    runtime: &RecognizerRuntime,
    target: HomeOpenTarget,
) -> Result<RoiObservation> {
    let span = debug_span!("open_slot_list", item_kind = %target.item_kind.label());
    let _guard = span.enter();
    let target_state = UiState::List(target.item_kind);

    debug!(
        x = target.point.0,
        y = target.point.1,
        hold_ms = CLICK_HOLD_MS,
        "opening home list with a direct mouse click"
    );
    automation.click(target.point, CLICK_HOLD_MS)?;

    let observation = wait_for_ui_state(automation, runtime, target_state, LIST_OPEN_TIMEOUT)?;

    Ok(observation)
}

pub(super) fn wait_for_home_booster_target(
    automation: &mut AutomationSession<'_>,
    runtime: &RecognizerRuntime,
) -> Result<HomeOpenTarget> {
    let span = debug_span!("wait_home_booster_slot");
    let _guard = span.enter();

    let observation = wait_for_ui_state(
        automation,
        runtime,
        UiState::HomeFilled,
        HOME_LAYOUT_TIMEOUT,
    )?;
    let slot = home_booster_slot(&observation)
        .context("stable home layout did not contain a booster slot")?;
    let point = slot.center();

    debug!(
        click_x = point.0,
        click_y = point.1,
        booster_x = slot.x,
        booster_y = slot.y,
        booster_w = slot.w,
        booster_h = slot.h,
        "stable home booster slot ready for direct mouse click"
    );
    Ok(HomeOpenTarget {
        item_kind: ItemKind::Booster,
        point,
    })
}
