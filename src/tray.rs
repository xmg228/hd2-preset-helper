use std::sync::mpsc::{Receiver, channel};

use crate::preset_action::PresetActionOptions;
use anyhow::Result;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
use windows::WindowsTray as PlatformTray;

pub struct TrayHandle {
    platform: PlatformTray,
    events: Receiver<TrayEvent>,
}

impl TrayHandle {
    pub fn update_settings(&self, settings: PresetActionOptions) {
        self.platform.update_settings(settings);
    }

    pub fn try_event(&self) -> Option<TrayEvent> {
        self.events.try_recv().ok()
    }
}

pub enum TrayEvent {
    ToggleApplyInSavedOrder,
    ToggleAutoReadyUp,
    ToggleSaveFallbackWhenTaken,
    ExitRequested,
}

pub fn spawn(settings: PresetActionOptions) -> Result<TrayHandle> {
    let (event_tx, events) = channel();
    let platform = PlatformTray::spawn(settings, event_tx)?;
    Ok(TrayHandle { platform, events })
}
