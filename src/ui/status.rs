//! Passive action feedback, driven by the same events as the automation.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, VecModel};

use super::{ActionStatus, PresetInfo, StatusItem, place_top_right};
use crate::app_events::{AppEvent, PresetCompletion};

pub(super) struct Status {
    window: ActionStatus,
    hide_at: Option<Instant>,
    presets: PathBuf,
    preset: String,
}

impl Status {
    pub fn new(presets: PathBuf) -> Result<Self> {
        Ok(Self {
            window: ActionStatus::new()?,
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
        if !matches!(
            &event,
            AppEvent::StratagemsProgress { .. }
                | AppEvent::BoosterProgress { .. }
                | AppEvent::EquipmentProgress { .. }
                | AppEvent::EquipmentCaching { .. }
                | AppEvent::PresetDone { .. }
        ) {
            self.window.set_stratagems(Default::default());
            self.window.set_booster(StatusItem::default());
            self.window.set_picture(slint::Image::default());
        }
        let (message, color, seconds) = match event {
            AppEvent::PresetStarted { .. } => ("Starting".into(), WORKING, None),
            AppEvent::HotkeyReleaseRequested { .. } => (
                "Release the shortcut keys and mouse button to continue".into(), WORKING, None),
            AppEvent::PresetCancelled { reason, .. } => (format!("Cancelled: {reason}"), WARNING, Some(5)),
            AppEvent::EquipmentActionStarted { saving } => {
                (if saving { "Saving equipment" } else { "Applying equipment" }.into(), WORKING, None)
            }
            AppEvent::EquipmentCaching { category, repairing } => {
                self.window.set_picture(super::image(&category));
                (if repairing { "Updating equipment cache" } else { "Building equipment cache" }.into(),
                    WORKING, None)
            }
            AppEvent::StratagemsApplyStarted { stratagems, booster, booster_confirmed } => {
                let icons = stratagems.iter().map(|id| self.icon(id)).collect::<Vec<_>>();
                self.window.set_stratagems(Rc::new(VecModel::from(icons)).into());
                let mut booster = booster.map(|id| self.icon(&id)).unwrap_or_default();
                booster.confirmed = booster_confirmed;
                self.window.set_booster(booster);
                (format!("Applying Stratagems · 0/{}", stratagems.len()), WORKING, None)
            }
            AppEvent::StratagemsProgress { remaining } => {
                let icons = self.window.get_stratagems();
                for (index, mut icon) in icons.iter().enumerate() {
                    icon.confirmed = !remaining.iter().any(|id| id == icon.item_id.as_str());
                    icons.set_row_data(index, icon);
                }
                let completed = icons.iter().filter(|icon| icon.confirmed).count();
                (format!("Applying Stratagems · {completed}/{}", icons.row_count()), WORKING, None)
            }
            AppEvent::BoosterProgress { item_id, confirmed } => {
                let mut icon = self.window.get_booster();
                if icon.item_id.as_str() != item_id {
                    icon = self.icon(&item_id);
                }
                icon.confirmed = confirmed;
                self.window.set_booster(icon);
                ("Applying Booster".into(), WORKING, None)
            }
            AppEvent::EquipmentProgress { kind, confirmed } => {
                if !confirmed {
                    self.window.set_picture(match crate::preset::preview::equipment_name(
                        &self.presets, &self.preset, kind)
                    {
                        Ok(image) => super::image(&image),
                        Err(error) => {
                            tracing::warn!(%error, "could not load status preview");
                            slint::Image::default()
                        }
                    });
                }
                (format!("Applying equipment · {}/{}", kind.index() + usize::from(confirmed),
                    crate::item::EquipmentKind::ALL.len()), WORKING, None)
            }
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

    /// Load only at the start or when switching to the fallback Booster.
    fn icon(&self, item_id: &str) -> StatusItem {
        let picture = match crate::preset::load_template_image(&self.presets, item_id) {
            Ok(image) => super::image(&image),
            Err(error) => {
                tracing::warn!(%error, "could not load status preview");
                slint::Image::default()
            }
        };
        StatusItem {
            item_id: item_id.into(),
            picture,
            confirmed: false,
        }
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
