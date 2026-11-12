//! Passive action feedback, driven by the same events as the automation.
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, VecModel};

use super::{ActionStatus, StatusItem, StatusTone, placement};
use crate::app_events::{AppEvent, PresetCompletion};
use crate::config::AUTO_MONITOR;
use crate::preset::PresetMetadata;
use crate::scan::{ScanKind, ScanProgress, ScanSummary};

fn scan_progress_content(
    kind: ScanKind,
    category: Option<&image::RgbaImage>,
    items: usize,
) -> (String, slint::Image) {
    (
        format!("Scanning {} · {items} items", kind.label()),
        category.map(super::image).unwrap_or_default(),
    )
}

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

    pub fn set_font_family(&self, family: &str) {
        self.window.set_ui_font_family(family.into());
    }

    pub fn update(&mut self, event: AppEvent, preset_info: &[PresetMetadata]) {
        self.window.set_item_scan(false);
        use StatusTone::{Error, Success, Warning, Working};
        if let AppEvent::PresetStarted { preset }
        | AppEvent::HotkeyReleaseRequested { preset }
        | AppEvent::PresetCancelled { preset, .. }
        | AppEvent::FallbackBoosterRequested { preset }
        | AppEvent::PresetDone { preset, .. }
        | AppEvent::PresetFailed { preset, .. } = &event
        {
            self.preset.clone_from(preset);
            let label = preset_info
                .iter()
                .find(|info| info.id == *preset)
                .map(|info| info.label.as_str())
                .unwrap_or("");
            self.window.set_preset_label(label.into());
            self.window
                .set_preset_number(super::preset_number(preset).into());
        }
        if !matches!(
            &event,
            AppEvent::StratagemsProgress { .. }
                | AppEvent::ItemCaching { .. }
                | AppEvent::BoosterProgress { .. }
                | AppEvent::EquipmentProgress { .. }
                | AppEvent::PresetDone { .. }
        ) {
            self.window.set_stratagems(Default::default());
            self.window.set_booster(StatusItem::default());
            self.window.set_picture(slint::Image::default());
        }
        let (message, tone, seconds) = match event {
            AppEvent::PresetStarted { .. } => ("Starting".into(), Working, None),
            AppEvent::HotkeyReleaseRequested { .. } => (
                "Release the shortcut keys and mouse button to continue".into(), Working, None),
            AppEvent::PresetCancelled { reason, .. } => (format!("Cancelled: {reason}"), Warning, Some(5)),
            AppEvent::EquipmentActionStarted { saving } => {
                (if saving { "Saving equipment" } else { "Applying equipment" }.into(), Working, None)
            }
            AppEvent::ItemCaching { kind, category, items } => {
                let (message, picture) = scan_progress_content(kind, category.as_ref(), items);
                self.window.set_picture(picture);
                (message, Working, None)
            }
            AppEvent::StratagemsApplyStarted { stratagems, booster, booster_confirmed } => {
                let icons = stratagems.iter().map(|id| self.icon(id)).collect::<Vec<_>>();
                self.window.set_stratagems(Rc::new(VecModel::from(icons)).into());
                let mut booster = booster.map(|id| self.icon(&id)).unwrap_or_default();
                booster.confirmed = booster_confirmed;
                self.window.set_booster(booster);
                (format!("Applying Stratagems · 0/{}", stratagems.len()), Working, None)
            }
            AppEvent::StratagemsProgress { remaining } => {
                let icons = self.window.get_stratagems();
                for (index, mut icon) in icons.iter().enumerate() {
                    icon.confirmed = !remaining.iter().any(|id| id == icon.item_id.as_str());
                    icons.set_row_data(index, icon);
                }
                let completed = icons.iter().filter(|icon| icon.confirmed).count();
                (format!("Applying Stratagems · {completed}/{}", icons.row_count()), Working, None)
            }
            AppEvent::BoosterProgress { item_id, confirmed } => {
                let mut icon = self.window.get_booster();
                if icon.item_id.as_str() != item_id {
                    icon = self.icon(&item_id);
                }
                icon.confirmed = confirmed;
                self.window.set_booster(icon);
                ("Applying Booster".into(), Working, None)
            }
            AppEvent::EquipmentProgress { kind, confirmed, completed, total } => {
                if !confirmed {
                    self.window.set_picture(match crate::preset::preview::equipment_name(
                        &self.presets, &self.preset, kind)
                    {
                        Ok(image) => super::image(&image),
                        Err(error) => {
                            tracing::warn!(preset = %self.preset, ?kind,
                                error = %format!("{error:#}"), "could not load status preview");
                            slint::Image::default()
                        }
                    });
                }
                (format!("Applying equipment · {completed}/{total}"), Working, None)
            }
            AppEvent::FallbackBoosterRequested { .. } => (
                "Saved Booster is unavailable. Select another to save as your fallback, or return to cancel.".into(),
                Warning, None),
            AppEvent::PresetDone { completion, .. } => match completion {
                PresetCompletion::Complete => ("Done".into(), Success, Some(2)),
                PresetCompletion::Saved => ("Saved".into(), Success, Some(2)),
                PresetCompletion::EquipmentApplied => ("Equipment applied".into(), Success, Some(2)),
                PresetCompletion::BoosterUnavailable => ("Booster unavailable".into(), Warning, Some(5)),
                PresetCompletion::FallbackBoosterSaved => ("Fallback Booster saved".into(), Success, Some(2)),
                PresetCompletion::FallbackBoosterNotSaved => ("Fallback Booster not saved".into(), Warning, Some(5)),
            },
            AppEvent::PresetFailed { error, .. } => (
                format!("Failed: {}", error.lines().next().unwrap_or(&error).trim()), Error, Some(5)),
        };
        self.window.set_message(message.into());
        self.window.set_tone(tone);
        self.hide_at = seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));
    }

    fn scan_feedback(&mut self, message: String, tone: StatusTone, seconds: Option<u64>) {
        self.preset.clear();
        self.window.set_item_scan(true);
        self.window.set_stratagems(Default::default());
        self.window.set_booster(StatusItem::default());
        self.window.set_picture(slint::Image::default());
        self.window.set_message(message.into());
        self.window.set_tone(tone);
        self.hide_at = seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));
    }

    pub fn scan_waiting(&mut self) {
        self.scan_feedback(
            "Release the keys and mouse button to continue".into(),
            StatusTone::Working,
            None,
        );
    }

    pub fn scan_progress(&mut self, progress: ScanProgress) {
        let (message, picture) =
            scan_progress_content(progress.kind, progress.category.as_ref(), progress.items);
        self.scan_feedback(message, StatusTone::Working, None);
        self.window.set_picture(picture);
    }

    pub fn scan_finished(
        &mut self,
        result: &std::result::Result<ScanSummary, String>,
        cancelled: bool,
    ) {
        let (message, tone, seconds) = match result {
            Ok(summary) => (
                format!(
                    "Collected {} items · {} types",
                    summary.items,
                    summary.completed.len()
                ),
                StatusTone::Success,
                3,
            ),
            Err(_) if cancelled => ("Scan stopped".into(), StatusTone::Warning, 3),
            Err(error) => (
                format!("Failed: {}", error.lines().next().unwrap_or(error).trim()),
                StatusTone::Error,
                5,
            ),
        };
        self.scan_feedback(message, tone, Some(seconds));
    }

    /// Load only at the start or when switching to the fallback Booster.
    fn icon(&self, item_id: &str) -> StatusItem {
        let picture =
            match crate::preset::preview::stratagem_icon(&self.presets, &self.preset, item_id) {
                Ok(image) => super::image(&image),
                Err(error) => {
                    tracing::warn!(preset = %self.preset, item_id,
                    error = %format!("{error:#}"), "could not load status preview");
                    slint::Image::default()
                }
            };
        StatusItem {
            item_id: item_id.into(),
            picture,
            confirmed: false,
        }
    }

    pub fn prepare_window(&self) -> Result<bool> {
        super::prepare_window(self.window.window(), super::WindowRole::Status)
    }

    pub fn show(&self, monitor: &str, anchor: Option<(i32, i32)>) -> Result<()> {
        if !self.window.window().is_visible() {
            self.window.show()?;
            // Let Slint present its first frame before hit-testing adds WS_EX_LAYERED.
            self.window
                .window()
                .with_winit_window(|window| window.set_cursor_hittest(false))
                .transpose()?;
            self.reposition(monitor, anchor)?;
        }
        self.raise()
    }

    pub fn raise(&self) -> Result<()> {
        #[cfg(target_os = "windows")]
        if self.window.window().is_visible() {
            self.window
                .window()
                .with_winit_window(super::windows::raise_window)
                .transpose()?;
        }
        Ok(())
    }

    pub fn reposition(&self, monitor: &str, anchor: Option<(i32, i32)>) -> Result<()> {
        if self.window.window().is_visible() {
            placement::place(
                self.window.window(),
                (monitor != AUTO_MONITOR).then_some(monitor),
                anchor,
                16.0,
                None,
            )?;
        }
        Ok(())
    }

    fn hide(&mut self) -> Result<()> {
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
