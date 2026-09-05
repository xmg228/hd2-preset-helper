use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "windows")]
use anyhow::Result;

#[cfg(target_os = "windows")]
use crate::app_events::AppEventSink;
use crate::app_events::{AppEvent, OverlayPreset, OverlayPresetStatus};
use crate::assets::IconCatalog;
#[cfg(target_os = "windows")]
use crate::input::HotkeyModifiers;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub fn start(modifiers: HotkeyModifiers, catalog: Arc<IconCatalog>) -> Result<AppEventSink> {
    windows::start(modifiers, catalog)
}

const DONE_HIDE_DELAY: Duration = Duration::from_secs(2);
const FAILED_HIDE_DELAY: Duration = Duration::from_secs(5);
const READY_HIDE_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
enum OverlayTone {
    Info,
    Working,
    Success,
    Warning,
    Error,
}

struct OverlayModel {
    catalog: Arc<IconCatalog>,
    presets: Vec<OverlayPreset>,
    active_preset: Option<String>,
    status: String,
    tone: OverlayTone,
    selected_count: usize,
    requested_count: usize,
}

#[derive(Clone, Copy)]
enum OverlayEventPolicy {
    Hold,
    HideAfter(Duration),
}

struct OverlayModelUpdate {
    policy: OverlayEventPolicy,
    presets_changed: bool,
}

impl OverlayModel {
    fn new(catalog: Arc<IconCatalog>) -> Self {
        Self {
            catalog,
            presets: Vec::new(),
            active_preset: None,
            status: "Waiting for preset hotkey".to_string(),
            tone: OverlayTone::Info,
            selected_count: 0,
            requested_count: 0,
        }
    }

    fn set_ready(&mut self) {
        self.active_preset = None;
        self.status = "Ready".to_string();
        self.tone = OverlayTone::Info;
        self.selected_count = 0;
        self.requested_count = 0;
    }

    fn apply(&mut self, event: AppEvent) -> OverlayModelUpdate {
        let (policy, presets_changed) = match event {
            AppEvent::PresetListUpdated { presets } => {
                self.presets = presets;
                self.set_ready();
                (OverlayEventPolicy::HideAfter(READY_HIDE_DELAY), true)
            }
            AppEvent::PresetStarted { preset } => {
                self.active_preset = Some(preset);
                self.status = "Starting".to_string();
                self.selected_count = 0;
                self.requested_count = 0;
                self.tone = OverlayTone::Working;
                (OverlayEventPolicy::Hold, false)
            }
            AppEvent::HotkeyReleaseRequested { preset } => {
                self.active_preset = Some(preset);
                self.status = "Release all shortcut keys to continue".to_string();
                self.selected_count = 0;
                self.requested_count = 0;
                self.tone = OverlayTone::Working;
                (OverlayEventPolicy::Hold, false)
            }
            AppEvent::PresetCancelled { preset, reason } => {
                self.active_preset = Some(preset);
                self.status = format!("Cancelled: {reason}");
                self.selected_count = 0;
                self.requested_count = 0;
                self.tone = OverlayTone::Warning;
                (OverlayEventPolicy::HideAfter(FAILED_HIDE_DELAY), false)
            }
            AppEvent::PresetSaved {
                preset,
                stratagems,
                booster,
            } => {
                if let Some(row) = self.presets.iter_mut().find(|row| row.name == preset) {
                    row.stratagems = stratagems;
                    row.booster = booster;
                    row.status = OverlayPresetStatus::Ready;
                }
                self.active_preset = Some(preset);
                self.status = "Saved".to_string();
                self.selected_count = 0;
                self.requested_count = 0;
                self.tone = OverlayTone::Success;
                (OverlayEventPolicy::HideAfter(DONE_HIDE_DELAY), true)
            }
            AppEvent::UiStateDetected { state } => {
                self.status = format!("Detected UI: {state}");
                self.tone = OverlayTone::Working;
                (OverlayEventPolicy::Hold, false)
            }
            AppEvent::ListSelectionStarted {
                item_kind,
                requested_items,
            } => {
                self.selected_count = 0;
                self.requested_count = requested_items;
                self.status = format!("Selecting {}: 0/{requested_items}", item_kind.label());
                self.tone = OverlayTone::Working;
                (OverlayEventPolicy::Hold, false)
            }
            AppEvent::ItemSelected { item_id } => {
                self.selected_count += 1;
                let progress = if self.requested_count > 0 {
                    format!("{}/{}", self.selected_count, self.requested_count)
                } else {
                    self.selected_count.to_string()
                };
                self.status = format!(
                    "Selected {progress}: {}",
                    self.catalog
                        .get(&item_id)
                        .map(|entry| entry.display_name.as_ref())
                        .unwrap_or(&item_id)
                );
                self.tone = OverlayTone::Working;
                (OverlayEventPolicy::Hold, false)
            }
            AppEvent::PresetDone { preset, warning } => {
                self.active_preset = Some(preset);
                if let Some(warning) = warning {
                    self.status = format!("Warning: {warning}");
                    self.tone = OverlayTone::Working;
                    (OverlayEventPolicy::HideAfter(FAILED_HIDE_DELAY), false)
                } else {
                    self.status = "Done".to_string();
                    self.tone = OverlayTone::Success;
                    (OverlayEventPolicy::HideAfter(DONE_HIDE_DELAY), false)
                }
            }
            AppEvent::PresetFailed { preset, error } => {
                self.active_preset = Some(preset);
                self.status = format!("Failed: {}", first_error_line(&error));
                self.tone = OverlayTone::Error;
                (OverlayEventPolicy::HideAfter(FAILED_HIDE_DELAY), false)
            }
        };

        OverlayModelUpdate {
            policy,
            presets_changed,
        }
    }
}

fn compact_error(error: &str) -> String {
    const MAX_CHARS: usize = 70;
    let first_line = first_error_line(error);
    if first_line.chars().count() <= MAX_CHARS {
        first_line.to_string()
    } else {
        let mut value = first_line.chars().take(MAX_CHARS - 3).collect::<String>();
        value.push_str("...");
        value
    }
}

fn first_error_line(error: &str) -> &str {
    error.lines().next().unwrap_or(error).trim()
}
