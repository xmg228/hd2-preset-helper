use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tracing::{debug, debug_span, warn};

use crate::automation::AutomationSession;
use crate::item::ItemKind;
use crate::vision::{RecognizerSession, RoiObservation};

use super::super::home::{
    UiState, detect_ui_state, home_booster_slot, scan_loadout_home, wait_for_stable_ui_state,
};

use super::CLICK_HOLD_MS;

const LIST_OPEN_TIMEOUT: Duration = Duration::from_millis(1500);
const LIST_OPEN_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const MAX_OPEN_CLICK_ATTEMPTS: u32 = 3;

#[derive(Clone, Copy, Debug)]
pub(super) struct HomeOpenTarget {
    pub(super) item_kind: ItemKind,
    pub(super) point: (u32, u32),
}

pub(super) fn open_slot_list(
    automation: &mut AutomationSession<'_>,
    recognizer: RecognizerSession,
    target: HomeOpenTarget,
) -> Result<RoiObservation> {
    let span = debug_span!("open_slot_list", item_kind = %target.item_kind.label());
    let _guard = span.enter();
    let target_state = UiState::List(target.item_kind);
    let started = Instant::now();
    let mut click_attempts = 1;

    debug!(
        x = target.point.0,
        y = target.point.1,
        hold_s = CLICK_HOLD_MS as f64 / 1000.0,
        "opening home list with a direct mouse click"
    );
    automation.click(target.point, CLICK_HOLD_MS)?;

    loop {
        let remaining = LIST_OPEN_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            bail!(
                "timed out waiting for {} after {click_attempts} home slot click attempts",
                target_state.label()
            );
        }
        let wait = if click_attempts < MAX_OPEN_CLICK_ATTEMPTS {
            remaining.min(LIST_OPEN_RETRY_INTERVAL)
        } else {
            remaining
        };
        if let Some(observation) =
            wait_for_stable_ui_state(automation, recognizer, target_state, wait, |_| true)?
        {
            debug!(
                item_kind = %target.item_kind.label(),
                click_attempts,
                elapsed_s = started.elapsed().as_secs_f64(),
                "home list opened"
            );
            return Ok(observation);
        }

        // Never retry on an unconfirmed transition or an already-open list.
        if click_attempts < MAX_OPEN_CLICK_ATTEMPTS {
            let home = scan_loadout_home(automation, recognizer)?;
            if detect_ui_state(&home) == UiState::Home && started.elapsed() < LIST_OPEN_TIMEOUT {
                click_attempts += 1;
                warn!(
                    item_kind = %target.item_kind.label(),
                    click_attempts,
                    elapsed_s = started.elapsed().as_secs_f64(),
                    "still on loadout home; retrying list entry click"
                );
                automation.click(target.point, CLICK_HOLD_MS)?;
            }
        }
    }
}

pub(super) fn home_booster_target(observation: &RoiObservation) -> Result<HomeOpenTarget> {
    let span = debug_span!("locate_home_booster_slot");
    let _guard = span.enter();

    let slot =
        home_booster_slot(observation).context("home layout did not contain a booster slot")?;
    let point = slot.center();

    debug!(
        click_x = point.0,
        click_y = point.1,
        booster_x = slot.x,
        booster_y = slot.y,
        booster_w = slot.w,
        booster_h = slot.h,
        "home booster slot ready for direct mouse click"
    );
    Ok(HomeOpenTarget {
        item_kind: ItemKind::Booster,
        point,
    })
}
