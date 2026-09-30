//! Game loadout operations. Equipment and icon-list selection share capture/input, not navigation state.
mod direct_select;
pub(crate) mod equipment;
mod frame;
mod home;
pub(crate) mod pages;

pub use direct_select::{BoosterApplyOutcome, apply_booster_from_home, apply_stratagems_from_home};
pub use frame::bind_loadout_region;
pub use home::{
    UiState, collect_home_booster, collect_stratagem_preset, detect_ui_state, scan_loadout_home,
    wait_for_stable_ui_state,
};
