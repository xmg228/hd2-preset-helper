use std::time::Duration;

#[cfg(target_os = "windows")]
use anyhow::Result;
#[cfg(target_os = "windows")]
use std::path::Path;

use crate::app_events::{AppEvent, OverlayPreset, OverlayPresetStatus, PresetCompletion};

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::OverlayHandle;

#[cfg(target_os = "windows")]
pub fn start(presets_path: &Path) -> Result<OverlayHandle> {
    windows::start(presets_path)
}

const DONE_HIDE_DELAY: Duration = Duration::from_secs(2);
const ATTENTION_HIDE_DELAY: Duration = Duration::from_secs(5);
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
    fn new() -> Self {
        Self {
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

    fn shows_fallback_booster(&self) -> bool {
        self.presets
            .iter()
            .any(|preset| preset.fallback_booster.is_some())
    }

    fn apply(&mut self, event: AppEvent) -> Option<OverlayModelUpdate> {
        let (policy, presets_changed) = match event {
            AppEvent::ModifiersChanged(_) | AppEvent::Shutdown => return None,
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
                (OverlayEventPolicy::HideAfter(ATTENTION_HIDE_DELAY), false)
            }
            AppEvent::PresetSaved {
                preset,
                stratagems,
                booster,
            } => {
                if let Some(row) = self.presets.iter_mut().find(|row| row.name == preset) {
                    row.stratagems = stratagems;
                    row.booster = booster;
                    row.fallback_booster = None;
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
            AppEvent::FallbackBoosterRequested { preset } => {
                self.active_preset = Some(preset);
                self.status = "Saved Booster is already in use. Select another to save as your \
                               fallback, or return to cancel."
                    .to_string();
                self.selected_count = 0;
                self.requested_count = 0;
                self.tone = OverlayTone::Warning;
                (OverlayEventPolicy::Hold, false)
            }
            AppEvent::ItemSelected => {
                self.selected_count += 1;
                let progress = if self.requested_count > 0 {
                    format!("{}/{}", self.selected_count, self.requested_count)
                } else {
                    self.selected_count.to_string()
                };
                self.status = format!("Selected {progress}");
                self.tone = OverlayTone::Working;
                (OverlayEventPolicy::Hold, false)
            }
            AppEvent::PresetDone { preset, completion } => {
                self.active_preset = Some(preset.clone());
                match completion {
                    PresetCompletion::Complete => {
                        self.status = "Done".to_string();
                        self.tone = OverlayTone::Success;
                        (OverlayEventPolicy::HideAfter(DONE_HIDE_DELAY), false)
                    }
                    PresetCompletion::BoosterUnavailable => {
                        self.status = "Booster already in use".to_string();
                        self.tone = OverlayTone::Warning;
                        (OverlayEventPolicy::HideAfter(ATTENTION_HIDE_DELAY), false)
                    }
                    PresetCompletion::FallbackBoosterSaved { path } => {
                        if let Some(row) = self.presets.iter_mut().find(|row| row.name == preset) {
                            row.fallback_booster = Some(path);
                        }
                        self.status = "Fallback Booster saved".to_string();
                        self.tone = OverlayTone::Success;
                        (OverlayEventPolicy::HideAfter(DONE_HIDE_DELAY), true)
                    }
                    PresetCompletion::FallbackBoosterNotSaved => {
                        self.status = "Fallback Booster not saved".to_string();
                        self.tone = OverlayTone::Warning;
                        (OverlayEventPolicy::HideAfter(ATTENTION_HIDE_DELAY), false)
                    }
                }
            }
            AppEvent::PresetFailed { preset, error } => {
                self.active_preset = Some(preset);
                self.status = format!("Failed: {}", first_error_line(&error));
                self.tone = OverlayTone::Error;
                (OverlayEventPolicy::HideAfter(ATTENTION_HIDE_DELAY), false)
            }
        };

        Some(OverlayModelUpdate {
            policy,
            presets_changed,
        })
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
