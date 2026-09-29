//! Passive action feedback, driven by the same events as the automation.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use slint::ComponentHandle;
use slint::winit_030::WinitWindowAccessor;

use super::{ActionStatus, PresetInfo, place_top_right};
use crate::app_events::{AppEvent, PresetCompletion};

pub(super) struct Status {
    window: ActionStatus,
    selected: usize,
    requested: usize,
    hide_at: Option<Instant>,
    presets: PathBuf,
    preset: String,
}

impl Status {
    pub fn new(presets: PathBuf) -> Result<Self> {
        Ok(Self {
            window: ActionStatus::new()?,
            selected: 0,
            requested: 0,
            hide_at: None,
            presets,
            preset: String::new(),
        })
    }

    pub fn message(&self) -> String {
        self.window.get_message().into()
    }

    pub fn update(&mut self, event: AppEvent, preset_info: &BTreeMap<String, PresetInfo>) {
        const WORKING: u32 = 0xffe9d778;
        const SUCCESS: u32 = 0xff9fd39c;
        const WARNING: u32 = 0xffe6b975;
        const ERROR: u32 = 0xffef9b94;
        if let AppEvent::PresetStarted { preset }
        | AppEvent::HotkeyReleaseRequested { preset }
        | AppEvent::PresetCancelled { preset, .. }
        | AppEvent::FallbackBoosterRequested { preset }
        | AppEvent::PresetDone { preset, .. }
        | AppEvent::PresetFailed { preset, .. } = &event
        {
            self.preset.clone_from(preset);
            let label = preset_info
                .get(preset)
                .map(|info| info.label.as_str())
                .unwrap_or("");
            self.window
                .set_heading(super::preset_title(preset, label).into());
        }
        // Load only when the target changes, not on captured frames. A missing
        // preview must never interrupt the actual automation.
        let picture = match &event {
            AppEvent::ItemSelectionStarted { item_id } => {
                Some(crate::preset::load_template_image(&self.presets, item_id))
            }
            AppEvent::EquipmentSelectionStarted { kind } => Some(
                crate::preset::preview::equipment_name(&self.presets, &self.preset, *kind),
            ),
            _ => None,
        };
        if !matches!(&event, AppEvent::ItemSelected) {
            self.window
                .set_name_picture(matches!(&event, AppEvent::EquipmentSelectionStarted { .. }));
            self.window.set_picture(match picture {
                Some(Ok(image)) => super::image(&image),
                Some(Err(error)) => {
                    tracing::warn!(%error, "could not load status preview");
                    slint::Image::default()
                }
                None => slint::Image::default(),
            });
        }
        let (message, color, seconds) = match event {
            AppEvent::PresetStarted { .. } => {
                self.selected = 0;
                self.requested = 0;
                ("Starting".into(), WORKING, None)
            }
            AppEvent::HotkeyReleaseRequested { .. } => (
                "Release the shortcut keys and mouse button to continue".into(), WORKING, None),
            AppEvent::PresetCancelled { reason, .. } => (format!("Cancelled: {reason}"), WARNING, Some(5)),
            AppEvent::EquipmentActionStarted { saving } => {
                self.selected = 0;
                self.requested = crate::item::EquipmentKind::ALL.len();
                (if saving { "Saving equipment" } else { "Applying equipment" }.into(), WORKING, None)
            }
            AppEvent::ListSelectionStarted { item_kind, requested_items } => {
                self.selected = 0;
                self.requested = requested_items;
                (format!("Selecting {}: 0/{requested_items}", item_kind.label()), WORKING, None)
            }
            AppEvent::ItemSelected => {
                self.selected += 1;
                (format!("Selected · {}/{}", self.selected, self.requested), WORKING, None)
            }
            AppEvent::ItemSelectionStarted { .. } | AppEvent::EquipmentSelectionStarted { .. } => (
                format!("Selecting · {}/{}", self.selected + 1, self.requested), WORKING, None),
            AppEvent::FallbackBoosterRequested { .. } => (
                "Saved Booster is already in use. Select another to save as your fallback, or return to cancel.".into(),
                WARNING, None),
            AppEvent::PresetDone { completion, .. } => match completion {
                PresetCompletion::Complete => ("Done".into(), SUCCESS, Some(2)),
                PresetCompletion::Saved => ("Saved".into(), SUCCESS, Some(2)),
                PresetCompletion::EquipmentApplied => ("Equipment applied".into(), SUCCESS, Some(2)),
                PresetCompletion::BoosterUnavailable => ("Booster already in use".into(), WARNING, Some(5)),
                PresetCompletion::FallbackBoosterSaved => ("Fallback Booster saved".into(), SUCCESS, Some(2)),
                PresetCompletion::FallbackBoosterNotSaved => ("Fallback Booster not saved".into(), WARNING, Some(5)),
            },
            AppEvent::PresetFailed { error, .. } => (
                format!("Failed: {}", error.lines().next().unwrap_or(&error).trim()), ERROR, Some(5)),
        };
        self.window.set_message(message.into());
        self.window
            .set_accent(slint::Color::from_argb_encoded(color));
        self.hide_at = seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));
    }

    pub fn show(&self, monitor: &str, anchor: Option<(i32, i32)>) -> Result<()> {
        if !self.window.window().is_visible() {
            self.window.show()?;
            // Creation uses with_active(false). Set hit-testing before returning
            // to the event loop, so this surface can never intercept a click.
            self.window
                .window()
                .with_winit_window(|window| -> Result<()> {
                    window.set_cursor_hittest(false)?;
                    #[cfg(target_os = "windows")]
                    super::windows::make_passive(window)?;
                    Ok(())
                })
                .transpose()?;
            self.reposition(monitor, anchor)?;
        }
        Ok(())
    }

    pub fn reposition(&self, monitor: &str, anchor: Option<(i32, i32)>) -> Result<()> {
        if self.window.window().is_visible() {
            place_top_right(self.window.window(), monitor, anchor, 16.0, None)?;
        }
        Ok(())
    }

    pub fn hide(&mut self) -> Result<()> {
        self.hide_at = None;
        self.window.hide()?;
        Ok(())
    }

    pub fn tick(&mut self) -> Result<()> {
        if self
            .hide_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.hide()?;
        }
        Ok(())
    }
}
