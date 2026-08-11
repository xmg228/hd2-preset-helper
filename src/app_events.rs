use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;

use crate::item::ItemKind;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_APP};

pub const APP_EVENT_MESSAGE: u32 = WM_APP + 1;

#[derive(Clone, Debug)]
pub struct OverlayPreset {
    pub key_label: &'static str,
    pub name: String,
    pub stratagems: Vec<String>,
    pub booster: Option<String>,
    pub status: OverlayPresetStatus,
}

#[derive(Clone, Debug)]
pub enum OverlayPresetStatus {
    Ready,
    NotSaved,
    Invalid(String),
}

#[derive(Clone, Debug)]
pub enum AppEvent {
    PresetListUpdated {
        presets: Vec<OverlayPreset>,
    },
    PresetStarted {
        preset: String,
    },
    HotkeyReleaseRequested {
        preset: String,
    },
    PresetCancelled {
        preset: String,
        reason: String,
    },
    PresetSaved {
        preset: String,
        stratagems: Vec<String>,
        booster: Option<String>,
    },
    UiStateDetected {
        state: &'static str,
    },
    ListSelectionStarted {
        item_kind: ItemKind,
        requested_items: usize,
    },
    ItemSelected {
        item_id: String,
    },
    PresetDone {
        preset: String,
        warning: Option<String>,
    },
    PresetFailed {
        preset: String,
        error: String,
    },
}

#[derive(Clone, Default)]
pub struct AppEventSink {
    channel: Option<(Sender<AppEvent>, Arc<AtomicU32>)>,
}

impl AppEventSink {
    pub fn channel(sender: Sender<AppEvent>, wake_thread: Arc<AtomicU32>) -> Self {
        Self {
            channel: Some((sender, wake_thread)),
        }
    }

    pub fn emit(&self, event: AppEvent) {
        let Some((sender, wake_thread)) = &self.channel else {
            return;
        };
        if sender.send(event).is_err() {
            return;
        }
        let thread_id = wake_thread.load(Ordering::Acquire);
        if thread_id != 0 {
            unsafe {
                let _ = PostThreadMessageW(thread_id, APP_EVENT_MESSAGE, WPARAM(0), LPARAM(0));
            }
        }
    }
}
