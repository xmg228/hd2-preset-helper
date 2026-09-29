use std::sync::Arc;

use crate::item::{EquipmentKind, ItemKind};

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
pub enum PresetCompletion {
    Complete,
    Saved,
    EquipmentApplied,
    BoosterUnavailable,
    FallbackBoosterSaved,
    FallbackBoosterNotSaved,
}

#[derive(Clone, Debug)]
pub enum AppEvent {
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
    EquipmentActionStarted {
        saving: bool,
    },
    ListSelectionStarted {
        item_kind: ItemKind,
        requested_items: usize,
    },
    FallbackBoosterRequested {
        preset: String,
    },
    ItemSelected,
    ItemSelectionStarted {
        item_id: String,
    },
    EquipmentSelectionStarted {
        kind: EquipmentKind,
    },
    PresetDone {
        preset: String,
        completion: PresetCompletion,
    },
    PresetFailed {
        preset: String,
        error: String,
    },
}
