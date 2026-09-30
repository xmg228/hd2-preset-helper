//! Interactive presentation and its event loop. No capture, injected input or game-window APIs.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use anyhow::Result;
use slint::winit_030::{WinitWindowAccessor, winit::event::WindowEvent};
use slint::{ComponentHandle, Model, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::app_events::AppEvent;
use crate::config::{SettingsDraft, ShortcutBindings};
use crate::preset::PresetScope;
use crate::preset::preview::{PresetPreview, PreviewContent};

mod settings;
mod status;
#[cfg(target_os = "windows")]
mod windows;

slint::include_modules!();

#[derive(Default)]
pub struct PresetInfo {
    pub name: String,
    pub label: String,
    pub shortcut: String,
}

pub enum UiRequest {
    OpenPanel,
    Exit,
    Action { preset: String, saving: bool },
    SetScope(PresetScope),
    Rename { preset: String, label: String },
    OpenSettings,
    SaveSettings(SettingsDraft),
    AddPreset,
    DeletePreset(String),
    HidePanel,
}

pub struct AppUi {
    window: PresetPanel,
    _tray: AppTray,
    requests: Receiver<UiRequest>,
    preset_info: BTreeMap<String, PresetInfo>,
    status: status::Status,
    monitor: String,
    anchor: Option<(i32, i32)>,
    placement_dirty: Rc<Cell<bool>>,
}

impl AppUi {
    pub fn new(preset_info: Vec<PresetInfo>, monitor: String, presets: PathBuf) -> Result<Self> {
        // Both surfaces are created without activation. Only the interactive
        // panel explicitly requests focus; status updates never do.
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_window_attributes_hook(|attributes| {
                let attributes = attributes.with_active(false);
                #[cfg(target_os = "windows")]
                let attributes = windows::window_attributes(attributes);
                attributes
            })
            .select()?;
        let window = PresetPanel::new()?;
        let (send, requests) = mpsc::channel();
        let tray = AppTray::new()?;
        let icon = image::load_from_memory(include_bytes!("../assets/app-icon.ico"))?.into_rgba8();
        tray.set_tray_icon(image(&icon));
        let open_send = send.clone();
        tray.on_open_panel(move || {
            let _ = open_send.send(UiRequest::OpenPanel);
        });
        let exit_send = send.clone();
        tray.on_exit(move || {
            let _ = exit_send.send(UiRequest::Exit);
        });
        let placement_dirty = Rc::new(Cell::new(false));
        let dirty = placement_dirty.clone();
        let mut handle_keys = settings::connect(&window, send.clone());
        window.window().on_winit_window_event(move |_, event| {
            if matches!(event, WindowEvent::ScaleFactorChanged { .. }) {
                dirty.set(true);
            }
            handle_keys(event)
        });
        let add_send = send.clone();
        window.on_add_preset(move || {
            let _ = add_send.send(UiRequest::AddPreset);
        });
        let delete_send = send.clone();
        window.on_request_delete(move |preset| {
            let _ = delete_send.send(UiRequest::DeletePreset(preset.to_string()));
        });
        let weak = window.as_weak();
        let action_send = send.clone();
        window.on_request_action(move |index, saving| {
            if let Some(window) = weak.upgrade()
                && let Some(entry) = window.get_presets().row_data(index as usize)
                && if saving {
                    window.get_save_available()
                } else {
                    window.get_available()
                }
            {
                let _ = action_send.send(UiRequest::Action {
                    preset: entry.name.to_string(),
                    saving,
                });
            }
        });
        let scope_send = send.clone();
        window.on_request_scope(move |stratagems, equipment| {
            let _ = scope_send.send(UiRequest::SetScope(PresetScope {
                stratagems,
                equipment,
            }));
        });
        let rename_send = send.clone();
        let weak = window.as_weak();
        window.on_request_rename(move |preset, label| {
            let label = label.trim();
            if let Some(window) = weak.upgrade()
                && window
                    .get_presets()
                    .iter()
                    .any(|row| row.name == preset && row.label != label)
            {
                let _ = rename_send.send(UiRequest::Rename {
                    preset: preset.to_string(),
                    label: label.to_owned(),
                });
            }
        });
        let close_send = send.clone();
        window.on_dismiss(move || {
            let _ = close_send.send(UiRequest::HidePanel);
        });
        window.window().on_close_requested(move || {
            let _ = send.send(UiRequest::HidePanel);
            slint::CloseRequestResponse::KeepWindowShown
        });
        let ui = Self {
            status: status::Status::new(presets)?,
            monitor,
            anchor: None,
            placement_dirty,
            window,
            _tray: tray,
            requests,
            preset_info: preset_info
                .into_iter()
                .map(|info| (info.name.clone(), info))
                .collect(),
        };
        ui.set_previews(BTreeMap::new());
        Ok(ui)
    }

    pub fn show_panel(&mut self, anchor: Option<(i32, i32)>) -> Result<()> {
        self.anchor = anchor;
        self.status.hide()?;
        self.window.set_pending(false);
        self.window.set_loading(true);
        self.window.set_status("Loading previews…".into());
        self.window.show()?;
        self.reposition()?;
        self.window.window().with_winit_window(|window| {
            window.set_minimized(false);
            window.focus_window();
        });
        self.window.invoke_focus_list();
        Ok(())
    }

    pub fn hide_panel(&self) -> Result<()> {
        self.window.invoke_finish_rename(true);
        self.window.set_settings_open(false);
        self.window.set_delete_open(false);
        self.window.set_recording_shortcut(-1);
        self.window.hide()?;
        Ok(())
    }

    pub fn is_panel_visible(&self) -> bool {
        self.window.window().is_visible()
    }

    pub fn set_anchor(&mut self, anchor: Option<(i32, i32)>) {
        self.anchor = anchor;
    }

    pub fn set_monitor(&mut self, monitor: String) -> Result<()> {
        self.monitor = monitor;
        if self.is_panel_visible() {
            self.reposition()?;
        }
        self.status.reposition(&self.monitor, self.anchor)
    }

    fn reposition(&self) -> Result<()> {
        let size = slint::LogicalSize::new(
            self.window.get_ideal_width(),
            self.window.get_ideal_height(),
        );
        if let Some(size) = place_top_right(
            self.window.window(),
            &self.monitor,
            self.anchor,
            24.0,
            Some(size),
        )? {
            self.window.set_available_width(size.width);
            self.window.set_available_height(size.height);
        }
        Ok(())
    }

    pub fn monitor(&self) -> &str {
        &self.monitor
    }

    pub fn open_settings(&self, draft: SettingsDraft) {
        self.window.invoke_finish_rename(true);
        settings::open(&self.window, draft);
    }

    pub fn event(&mut self, event: AppEvent) -> Result<()> {
        let failed = matches!(
            &event,
            AppEvent::PresetFailed { .. } | AppEvent::PresetCancelled { .. }
        );
        match &event {
            AppEvent::PresetFailed { preset, error }
            | AppEvent::PresetCancelled {
                preset,
                reason: error,
            } => {
                let label = self
                    .preset_info
                    .get(preset)
                    .map_or("", |info| info.label.as_str());
                self.window.set_failure_details(
                    format!("{} — {error}", preset_title(preset, label)).into(),
                );
            }
            AppEvent::PresetDone { .. } => self.window.set_failure_details("".into()),
            _ => {}
        }
        self.status.update(event, &self.preset_info);
        if self.is_panel_visible() {
            if failed {
                self.set_status("");
            } else {
                self.set_status(&self.status.message());
            }
        } else {
            self.status.show(&self.monitor, self.anchor)?;
        }
        Ok(())
    }

    pub fn tick(&mut self) -> Result<()> {
        // Wait until Slint and the OS have finished processing the DPI event.
        if self.placement_dirty.replace(false) {
            if self.is_panel_visible() {
                self.reposition()?;
            }
            self.status.reposition(&self.monitor, self.anchor)?;
        }
        self.status.tick()
    }

    pub fn is_panel_active(&self) -> bool {
        self.window
            .window()
            .with_winit_window(|window| window.has_focus())
            .unwrap_or(false)
    }
    pub fn is_editing(&self) -> bool {
        self.window.get_editing_name()
            || self.window.get_settings_open()
            || self.window.get_delete_open()
    }

    pub fn shortcuts_blocked(&self) -> bool {
        self.window.get_editing_name() || self.window.get_recording_shortcut() >= 0
    }

    pub fn settings_saved(&mut self, config: &ShortcutBindings) {
        self.set_panel_shortcut(&config.panel);
        let rows = self.window.get_presets();
        for (index, mut row) in rows.iter().enumerate() {
            let shortcut = config
                .presets
                .get(row.name.as_str())
                .cloned()
                .unwrap_or_default();
            self.preset_info
                .entry(row.name.to_string())
                .or_insert_with(|| PresetInfo {
                    name: row.name.to_string(),
                    label: row.label.to_string(),
                    ..Default::default()
                })
                .shortcut
                .clone_from(&shortcut);
            row.shortcut = shortcut.into();
            rows.set_row_data(index, row);
        }
        self.window.set_settings_saving(false);
        self.window.set_settings_open(false);
        self.window.invoke_focus_list();
    }

    pub fn settings_failed(&self, error: &str) {
        self.window.set_settings_saving(false);
        self.window.set_settings_error(error.into());
    }

    pub fn set_shortcut_warning(&self, warning: &str) {
        if self.window.get_shortcut_warning() != warning {
            self.window.set_shortcut_warning(warning.into());
        }
    }

    pub fn next_preset_name(&self) -> String {
        let next = self
            .window
            .get_presets()
            .iter()
            .filter_map(|row| {
                row.name
                    .strip_prefix("preset_")
                    .and_then(|n| n.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0)
            + 1;
        format!("preset_{next}")
    }

    pub fn add_preset(&mut self, name: String) {
        let mut rows: Vec<_> = self.window.get_presets().iter().collect();
        rows.push(PresetRow {
            title: preset_title(&name, "").into(),
            name: name.as_str().into(),
            stratagems: section(PreviewContent::Missing),
            equipment: section(PreviewContent::Missing),
            ..Default::default()
        });
        self.preset_info.insert(
            name.clone(),
            PresetInfo {
                name,
                ..Default::default()
            },
        );
        let index = rows.len() as i32 - 1;
        self.window
            .set_presets(Rc::new(VecModel::from(rows)).into());
        self.window.set_current_index(index);
        self.window.invoke_focus_list();
    }

    pub fn set_label(&mut self, preset: &str, label: String) {
        self.preset_info
            .entry(preset.to_owned())
            .or_insert_with(|| PresetInfo {
                name: preset.to_owned(),
                ..Default::default()
            })
            .label
            .clone_from(&label);
        let rows = self.window.get_presets();
        if let Some((index, mut row)) = rows.iter().enumerate().find(|(_, row)| row.name == preset)
        {
            row.title = preset_title(preset, &label).into();
            row.label = label.into();
            rows.set_row_data(index, row);
        }
        self.set_status("");
    }

    pub fn remove_preset(&mut self, preset: &str) {
        self.preset_info.remove(preset);
        let rows: Vec<_> = self
            .window
            .get_presets()
            .iter()
            .filter(|row| row.name != preset)
            .collect();
        let index = self
            .window
            .get_current_index()
            .min(rows.len().saturating_sub(1) as i32);
        self.window
            .set_presets(Rc::new(VecModel::from(rows)).into());
        self.window.set_current_index(index);
        self.window.invoke_focus_list();
        self.set_status("");
    }
    pub fn set_scope(&self, scope: PresetScope) {
        self.window.set_scope_stratagems(scope.stratagems);
        self.window.set_scope_equipment(scope.equipment);
        self.set_status("");
    }
    pub fn set_panel_shortcut(&self, shortcut: &str) {
        let keys = shortcut
            .split('+')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(slint::SharedString::from)
            .collect::<Vec<_>>();
        self.window
            .set_panel_shortcut_keys(Rc::new(VecModel::from(keys)).into());
    }
    pub fn take_request(&self) -> Option<UiRequest> {
        self.requests.try_recv().ok()
    }
    pub fn set_status(&self, message: &str) {
        self.window.set_status(message.into());
    }
    pub fn preview_failed(&self, message: &str) {
        self.window.set_loading(false);
        self.set_status(message);
    }
    pub fn set_pending(&self, pending: bool) {
        self.window.set_pending(pending);
    }

    pub fn set_previews(&self, mut previews: BTreeMap<String, PresetPreview>) {
        self.window.set_loading(false);
        let mut rows: Vec<_> = self
            .preset_info
            .iter()
            .map(|(name, info)| {
                let preview = previews.remove(name).unwrap_or(PresetPreview {
                    stratagems: PreviewContent::Missing,
                    equipment: PreviewContent::Missing,
                });
                PresetRow {
                    title: preset_title(name, &info.label).into(),
                    label: info.label.as_str().into(),
                    name: name.as_str().into(),
                    shortcut: info.shortcut.as_str().into(),
                    stratagems: section(preview.stratagems),
                    equipment: section(preview.equipment),
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            let order = |name: &str| {
                name.strip_prefix("preset_")
                    .and_then(|n| n.parse::<u32>().ok())
                    .unwrap_or(u32::MAX)
            };
            order(&a.name)
                .cmp(&order(&b.name))
                .then_with(|| a.name.as_str().cmp(b.name.as_str()))
        });
        let index = self
            .window
            .get_current_index()
            .min(rows.len().saturating_sub(1) as i32);
        self.window
            .set_presets(Rc::new(VecModel::from(rows)).into());
        self.window.set_current_index(index);
        if !self.window.get_pending() {
            self.set_status("");
        }
    }
}

fn preset_title(name: &str, label: &str) -> String {
    if !label.is_empty() {
        return label.to_owned();
    }
    name.strip_prefix("preset_")
        .map(|number| format!("Preset {number}"))
        .unwrap_or_else(|| name.to_owned())
}

/// Keep panel navigation available without forbidding these bindings in the game.
pub fn uses_shortcut(shortcut: &crate::input::HotkeySpec) -> bool {
    use crate::input::{HotkeyModifier, Key};
    let modifiers = shortcut.modifiers;
    (modifiers.iter().next().is_none()
        && matches!(
            shortcut.key,
            Key::Up
                | Key::Down
                | Key::W
                | Key::S
                | Key::Left
                | Key::Right
                | Key::Enter
                | Key::Space
                | Key::Tab
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
        ))
        || (shortcut.key == Key::Tab && modifiers.iter().eq([HotkeyModifier::Shift]))
        || (shortcut.key == Key::S && modifiers.iter().eq([HotkeyModifier::Ctrl]))
}

fn section(content: PreviewContent) -> PreviewSection {
    match content {
        PreviewContent::Missing => PreviewSection {
            message: "Not saved".into(),
            ..Default::default()
        },
        PreviewContent::Invalid(error) => PreviewSection {
            saved: true,
            message: error.into(),
            ..Default::default()
        },
        PreviewContent::Ready(images) => PreviewSection {
            ready: true,
            saved: true,
            message: Default::default(),
            items: Rc::new(VecModel::from(
                images
                    .into_iter()
                    .map(|item| PreviewRow {
                        label: item.label.into(),
                        picture: image(&item.image),
                    })
                    .collect::<Vec<_>>(),
            ))
            .into(),
        },
    }
}

fn image(image: &image::RgbaImage) -> slint::Image {
    slint::Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        image.as_raw(),
        image.width(),
        image.height(),
    ))
}

/// The UI owns the main event loop; the application supplies one non-blocking tick.
pub fn run(mut tick: impl FnMut() -> Result<bool> + 'static) -> Result<()> {
    let error = Rc::new(RefCell::new(None));
    let tick_error = error.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(10),
        move || {
            let done = match tick() {
                Ok(done) => done,
                Err(error) => {
                    *tick_error.borrow_mut() = Some(error);
                    true
                }
            };
            if done {
                let _ = slint::quit_event_loop();
            }
        },
    );
    slint::run_event_loop_until_quit()?;
    error.borrow_mut().take().map_or(Ok(()), Err)
}

/// Placement uses desktop coordinates only inside the presentation layer.
fn place_top_right(
    window: &slint::Window,
    monitor: &str,
    anchor: Option<(i32, i32)>,
    margin: f64,
    preferred_size: Option<slint::LogicalSize>,
) -> Result<Option<slint::LogicalSize>> {
    use slint::winit_030::winit::dpi::PhysicalPosition;
    window
        .with_winit_window(|native| -> Result<Option<slint::LogicalSize>> {
            let screens: Vec<_> = native.available_monitors().collect();
            let screen = anchor
                .and_then(|(x, y)| {
                    screens.iter().find(|screen| {
                        let p = screen.position();
                        let s = screen.size();
                        x >= p.x
                            && y >= p.y
                            && x < p.x + s.width as i32
                            && y < p.y + s.height as i32
                    })
                })
                .cloned()
                .or_else(|| native.current_monitor());
            let Some(screen) = screen else {
                return Ok(None);
            };
            let p = screen.position();
            let s = screen.size();
            let mut bounds = (p.x, p.y, s.width as i32, s.height as i32);
            #[cfg(target_os = "windows")]
            if monitor != "auto" {
                bounds = windows::monitor_bounds(monitor)?;
            }
            let scale = screens
                .iter()
                .find(|s| {
                    let p = s.position();
                    p.x == bounds.0 && p.y == bounds.1
                })
                .map_or(screen.scale_factor(), |s| s.scale_factor());
            #[cfg(target_os = "windows")]
            let bounds = windows::work_area(bounds)?;
            // Fit the layout in logical pixels; Slint owns DPI scaling and rendering.
            let margin = (margin * scale).round() as i32;
            let fitted = preferred_size.map(|size| {
                slint::LogicalSize::new(
                    size.width
                        .min(((bounds.2 - 2 * margin) as f64 / scale) as f32),
                    size.height
                        .min(((bounds.3 - 2 * margin) as f64 / scale) as f32),
                )
            });
            let size = fitted.map_or_else(
                || native.outer_size().to_logical::<f64>(native.scale_factor()),
                |size| {
                    slint::winit_030::winit::dpi::LogicalSize::new(
                        size.width as f64,
                        size.height as f64,
                    )
                },
            );
            let physical = slint::PhysicalSize::new(
                (size.width * scale).round() as u32,
                (size.height * scale).round() as u32,
            );
            if fitted.is_some() {
                // Slint 1.18 caches physical min/max sizes when logical constraints
                // stay unchanged. Refresh them as well as the actual window size.
                let bounds = slint::winit_030::winit::dpi::PhysicalSize::new(
                    physical.width,
                    physical.height,
                );
                native.set_min_inner_size(Some(bounds));
                native.set_max_inner_size(Some(bounds));
                window.set_size(physical);
            }
            let w = physical.width as i32;
            let (x, y) = (bounds.0 + bounds.2 - w - margin, bounds.1 + margin);
            native.set_outer_position(PhysicalPosition::new(x, y));
            Ok(fitted)
        })
        .transpose()
        .map(Option::flatten)
}
