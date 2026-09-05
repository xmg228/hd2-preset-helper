use std::sync::Arc;

use crate::item::ItemKind;

type AppEventHandler = dyn Fn(AppEvent) + Send + Sync;

#[derive(Clone, Default)]
pub struct AppEventSink(Option<Arc<AppEventHandler>>);

impl AppEventSink {
    pub fn new(handler: impl Fn(AppEvent) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(handler)))
    }

    pub fn emit(&self, event: AppEvent) {
        if let Some(handler) = &self.0 {
            handler(event);
        }
    }
}

#[derive(Clone, Debug)]
pub struct OverlayPreset {
    pub key_label: &'static str,
    pub name: String,
    pub label: Option<String>,
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
