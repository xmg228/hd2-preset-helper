//! Interactive presentation and its event loop. No capture, injected input or game-window APIs.
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use slint::winit_030::{
    WinitWindowAccessor,
    winit::{event::WindowEvent, window::WindowLevel},
};
use slint::{ComponentHandle, Model, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::app_events::AppEvent;
use crate::config::{PanelPlacement, SettingsDraft, ShortcutBindings, UiLanguage};
use crate::item::EquipmentKind;
use crate::item_cache::CachedChoices;
use crate::preset::preview::{
    PresetPreview, PreviewContent, PreviewImage, PreviewRole as PreviewImageRole,
};
use crate::preset::{LibraryMetadata, PresetMetadata, PresetScope, PresetSection};
use crate::scan::{ScanKind, ScanProgress, ScanScope, ScanSummary};

mod editor;
mod library;
mod migration;
mod placement;
mod settings;
mod status;
#[cfg(target_os = "windows")]
mod windows;

slint::include_modules!();

#[derive(Clone, Copy)]
enum WindowRole {
    Panel,
    Status,
}

/// Native windows are created hidden when the event loop starts.
fn prepare_window(window: &slint::Window, role: WindowRole) -> Result<bool> {
    window
        .with_winit_window(|native| -> Result<()> {
            #[cfg(target_os = "windows")]
            windows::prepare_window(native, role)?;
            Ok(())
        })
        .transpose()
        .map(|native| native.is_some())
}

pub enum SettingsTarget {
    General,
    About,
    PresetShortcut(String),
}

pub enum UiRequest {
    OpenPanel,
    Exit,
    Action {
        preset: String,
        saving: bool,
        section: Option<PresetSection>,
    },
    Scan(ScanScope),
    InspectMigration(ScanScope),
    ScanMissingItems,
    RefreshItems(ScanKind),
    ConfirmScan,
    CancelScan,
    ResolveMigration {
        migrate_presets: Option<bool>,
    },
    ContinueMigration,
    LoadMigrationPreview,
    EditItems,
    SaveItemChoices {
        preset: String,
        choices: CachedChoices,
    },
    ToggleSection(PresetSection),
    ToggleEquipment(EquipmentKind),
    SetCompactList(bool),
    Rename {
        preset: String,
        label: String,
    },
    OpenSettings(SettingsTarget),
    SaveSettings {
        draft: SettingsDraft,
        startup_change: Option<bool>,
    },
    CheckUpdates,
    ViewUpdateNotes,
    ViewProject,
    ViewQuickStart,
    AcknowledgeQuickStart,
    DownloadUpdate,
    InstallUpdate,
    CreatePreset {
        source: Option<String>,
    },
    DeletePreset(String),
    CreateFolder,
    ToggleFolder(String),
    RenameFolder {
        folder: String,
        label: String,
    },
    UngroupFolder(String),
    MoveEntry {
        entry: String,
        parent: Option<String>,
        before: Option<String>,
    },
    HidePanel,
}

// Only in-process library entries carry this drag marker.
struct EntryDrag;

pub struct AppUi {
    window: PresetPanel,
    _tray: AppTray,
    requests: Receiver<UiRequest>,
    preset_info: Vec<PresetMetadata>,
    status: status::Status,
    status_monitor: String,
    language: UiLanguage,
    panel_placement: PanelPlacement,
    game_anchor: Option<(i32, i32)>,
    placement_dirty: Rc<Cell<bool>>,
    panel_hide_at: Option<Instant>,
    migration_view: migration::View,
}

impl AppUi {
    pub fn new(
        library: LibraryMetadata,
        status_monitor: String,
        language: UiLanguage,
        panel_placement: PanelPlacement,
        presets: PathBuf,
    ) -> Result<Self> {
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
        #[cfg(target_os = "windows")]
        windows::install_resize_sync(window.window())?;
        window.set_current_version(env!("CARGO_PKG_VERSION").into());
        #[cfg(target_os = "windows")]
        window.set_language_fonts(
            UiLanguage::ALL
                .map(|language| {
                    language
                        .locale()
                        .map(windows::font_family)
                        .unwrap_or("")
                        .into()
                })
                .into(),
        );
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
        library::connect(&window, send.clone());
        editor::connect(&window, send.clone());
        let confirm_send = send.clone();
        window.on_confirm_scan(move || {
            let _ = confirm_send.send(UiRequest::ConfirmScan);
        });
        let cancel_send = send.clone();
        window.on_cancel_scan(move || {
            let _ = cancel_send.send(UiRequest::CancelScan);
        });
        migration::connect(&window, send.clone());
        let quick_start_send = send.clone();
        window.on_acknowledge_quick_start(move || {
            let _ = quick_start_send.send(UiRequest::AcknowledgeQuickStart);
        });
        window.window().on_winit_window_event(move |_, event| {
            if matches!(event, WindowEvent::ScaleFactorChanged { .. }) {
                dirty.set(true);
            }
            handle_keys(event)
        });
        let add_send = send.clone();
        window.on_add_preset(move || {
            let _ = add_send.send(UiRequest::CreatePreset { source: None });
        });
        let compact_send = send.clone();
        window.on_request_compact_list(move |compact| {
            let _ = compact_send.send(UiRequest::SetCompactList(compact));
        });
        let duplicate_send = send.clone();
        window.on_duplicate_preset(move |preset| {
            let _ = duplicate_send.send(UiRequest::CreatePreset {
                source: Some(preset.to_string()),
            });
        });
        let delete_send = send.clone();
        window.on_request_delete(move |preset| {
            let _ = delete_send.send(UiRequest::DeletePreset(preset.to_string()));
        });
        let mut drag_data = slint::DataTransfer::default();
        drag_data.set_user_data(Rc::new(EntryDrag));
        window.set_entry_drag_data(drag_data);
        window.on_is_entry_drag(|data| data.user_data().is_some_and(|data| data.is::<EntryDrag>()));
        let move_send = send.clone();
        window.on_request_reorder(move |entry, parent, before| {
            let _ = move_send.send(UiRequest::MoveEntry {
                entry: entry.to_string(),
                parent: (!parent.is_empty()).then(|| parent.to_string()),
                before: (!before.is_empty()).then(|| before.to_string()),
            });
        });
        let weak = window.as_weak();
        let action_send = send.clone();
        window.on_request_action(move |saving, scope| {
            if let Some(window) = weak.upgrade()
                && window.invoke_action_available(saving, scope)
                && let Some(entry) = window
                    .get_presets()
                    .row_data(window.get_current_index() as usize)
            {
                let _ = action_send.send(UiRequest::Action {
                    preset: entry.name.to_string(),
                    saving,
                    section: preset_section(scope),
                });
            }
        });
        let scope_send = send.clone();
        window.on_request_scope(move |scope| {
            if let Some(section) = preset_section(scope) {
                let _ = scope_send.send(UiRequest::ToggleSection(section));
            }
        });
        let scope_send = send.clone();
        window.on_request_equipment_item(move |index| {
            if let Some(&kind) = EquipmentKind::ALL.get(index as usize) {
                let _ = scope_send.send(UiRequest::ToggleEquipment(kind));
            }
        });
        window.on_selected_equipment_state(|selected, items| {
            let mut state = PreviewState::Missing;
            for (selected, item) in selected.iter().zip(items.iter()) {
                if selected {
                    match item.state {
                        PreviewState::Invalid => return PreviewState::Invalid,
                        PreviewState::Ready => state = PreviewState::Ready,
                        PreviewState::Missing => {}
                    }
                }
            }
            state
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
        let mut ui = Self {
            status: status::Status::new(presets)?,
            status_monitor,
            language,
            panel_placement,
            game_anchor: None,
            placement_dirty,
            panel_hide_at: None,
            migration_view: migration::View::default(),
            window,
            _tray: tray,
            requests,
            preset_info: Vec::new(),
        };
        ui.set_language(language)?;
        ui.set_library(library, None);
        ui.set_previews(BTreeMap::new());
        Ok(ui)
    }

    pub fn prepare_windows(&self) -> Result<bool> {
        let panel_ready = prepare_window(self.window.window(), WindowRole::Panel)?;
        let status_ready = self.status.prepare_window()?;
        Ok(panel_ready && status_ready)
    }

    pub fn show_panel(&mut self, anchor: Option<(i32, i32)>) -> Result<()> {
        let installing = self.window.get_update_installing();
        self.panel_hide_at = None;
        if let Some(anchor) = anchor {
            self.set_game_anchor(anchor)?;
        }
        self.window.set_loading(!installing);
        self.window.set_status("".into());
        // Evaluate the initial hidden value before changing the lazy animation binding.
        let _ = self.window.get_panel_opacity();
        self.window.show()?;
        self.reposition(anchor)?;
        self.window.set_presented(true);
        self.window
            .window()
            .with_winit_window(|window| window.set_cursor_hittest(true))
            .transpose()?;
        self.activate_panel()?;
        if installing {
            self.window.set_settings_open(true);
            self.window.invoke_focus_settings(true);
        } else {
            self.window.invoke_focus_current_view();
        }
        Ok(())
    }

    pub fn activate_panel(&self) -> Result<()> {
        self.window.window().with_winit_window(|window| {
            window.set_minimized(false);
            window.focus_window();
        });
        self.status.raise()
    }

    pub fn set_scan_confirmation(&self, preset: &str, count: usize) {
        self.window.set_loading(false);
        if let Some(index) = self
            .window
            .get_presets()
            .iter()
            .position(|row| row.name == preset)
        {
            self.window.set_current_index(index as i32);
        }
        self.window.set_scan_confirm_count(count as i32);
        self.window.set_scan_confirm_open(true);
        self.window.invoke_focus_scan_confirm();
    }

    pub fn dismiss_scan_confirmation(&self) {
        self.window.set_scan_confirm_open(false);
        if self.is_panel_open() {
            self.window.invoke_focus_current_view();
        }
    }

    pub fn is_scan_confirm_open(&self) -> bool {
        self.window.get_scan_confirm_open()
    }

    /// Stop interaction now; hide the native window after the scene fades out.
    pub fn dismiss_panel(&mut self) -> Result<()> {
        if !self.is_panel_open() {
            return Ok(());
        }
        self.window.invoke_commit_names();
        self.window.invoke_prepare_hide();
        self.window
            .window()
            .with_winit_window(|window| window.set_cursor_hittest(false))
            .transpose()?;
        self.window.set_presented(false);
        self.panel_hide_at =
            Some(Instant::now() + Duration::from_millis(self.window.get_fade_duration() as u64));
        Ok(())
    }

    /// Hide immediately while retaining the current view.
    pub fn hide_panel(&mut self) -> Result<()> {
        self.panel_hide_at = None;
        self.window.invoke_commit_names();
        self.window.invoke_prepare_hide();
        self.window.set_presented(false);
        self.window.hide()?;
        Ok(())
    }

    pub fn can_scan_missing_items(&self) -> bool {
        self.can_refresh_items() && self.window.get_editor_missing()
    }

    pub fn show_quick_start(&self) {
        self.window.invoke_commit_names();
        self.window.set_quick_start_open(true);
        self.window.invoke_focus_quick_start();
    }

    pub fn dismiss_quick_start(&self) {
        self.window.set_quick_start_open(false);
        self.window.invoke_focus_current_view();
    }

    pub fn is_quick_start_open(&self) -> bool {
        self.window.get_quick_start_open()
    }

    pub fn can_refresh_items(&self) -> bool {
        self.window.get_editor_open()
            && !self.window.get_editor_busy()
            && !self.window.get_editor_loading()
    }

    pub fn resume_item_editor(&self, error: Option<&str>) {
        self.window.set_loading(false);
        self.window.set_editor_loading(true);
        self.window.set_editor_error(error.unwrap_or("").into());
        self.window.invoke_focus_item_editor();
    }

    pub fn is_panel_open(&self) -> bool {
        self.window.get_presented() && self.is_panel_visible()
    }

    fn is_panel_visible(&self) -> bool {
        self.window.window().is_visible()
    }

    pub fn is_panel_minimized(&self) -> bool {
        self.window
            .window()
            .with_winit_window(|window| window.is_minimized() == Some(true))
            .unwrap_or(false)
    }

    pub fn set_game_anchor(&mut self, anchor: (i32, i32)) -> Result<()> {
        if self.game_anchor != Some(anchor) {
            self.game_anchor = Some(anchor);
            self.status
                .reposition(&self.status_monitor, self.game_anchor)?;
        }
        Ok(())
    }

    pub fn set_always_on_top(&self, enabled: bool) -> Result<()> {
        if self.window.get_panel_always_on_top() == enabled || self.is_panel_minimized() {
            return Ok(());
        }
        self.window.set_panel_always_on_top(enabled);
        // Apply before automation starts; Slint retains the same level on later updates.
        self.window.window().with_winit_window(|window| {
            window.set_window_level(if enabled {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            });
        });
        self.status.raise()
    }

    pub fn set_status_monitor(&mut self, monitor: String) -> Result<()> {
        self.status_monitor = monitor;
        self.status
            .reposition(&self.status_monitor, self.game_anchor)
    }

    fn reposition(&self, anchor: Option<(i32, i32)>) -> Result<()> {
        if let Some(available) = placement::place(
            self.window.window(),
            self.panel_placement.monitor.as_deref(),
            anchor,
            24.0,
            Some(&self.panel_placement),
        )? {
            self.window.set_available_width(available.width);
            self.window.set_available_height(available.height);
        }
        Ok(())
    }

    pub fn status_monitor(&self) -> &str {
        &self.status_monitor
    }

    pub fn language(&self) -> UiLanguage {
        self.language
    }

    pub fn set_language(&mut self, language: UiLanguage) -> Result<()> {
        let locale = language.locale().unwrap_or_else(|| {
            sys_locale::get_locales()
                .find_map(|locale| UiLanguage::from_locale(&locale).and_then(UiLanguage::locale))
                .unwrap_or("en")
        });
        slint::select_bundled_translation(locale)?;
        #[cfg(target_os = "windows")]
        let font_family = windows::font_family(locale);
        #[cfg(not(target_os = "windows"))]
        let font_family = "";
        self.window.set_ui_font_family(font_family.into());
        self.status.set_font_family(font_family);
        self.language = language;
        Ok(())
    }

    pub fn open_settings(
        &self,
        draft: SettingsDraft,
        startup_state: Option<bool>,
        target: SettingsTarget,
    ) {
        self.window.invoke_commit_names();
        settings::open(&self.window, draft, startup_state, target);
    }

    pub fn event(&mut self, event: AppEvent) -> Result<()> {
        match &event {
            AppEvent::PresetStarted { .. } | AppEvent::HotkeyReleaseRequested { .. } => {
                self.set_status("");
                self.window.set_failure_details("".into());
            }
            AppEvent::PresetFailed { preset, error }
            | AppEvent::PresetCancelled {
                preset,
                reason: error,
            } => {
                self.set_status("");
                self.window.set_failure_scan(false);
                let label = self
                    .preset_info
                    .iter()
                    .find(|info| info.id == *preset)
                    .map_or("", |info| info.label.as_str());
                self.window.set_failure_label(label.into());
                self.window.set_failure_number(preset_number(preset).into());
                self.window.set_failure_details(error.as_str().into());
            }
            AppEvent::PresetDone { .. } => {
                self.window.set_failure_scan(false);
                self.window.set_failure_label("".into());
                self.window.set_failure_number("".into());
                self.window.set_failure_details("".into());
            }
            _ => {}
        }
        self.status.update(event, &self.preset_info);
        self.status.show(&self.status_monitor, self.game_anchor)
    }

    pub fn scan_waiting(&mut self) -> Result<()> {
        self.set_status("");
        self.window.set_failure_details("".into());
        if self.window.get_editor_open() {
            self.window.set_editor_loading(true);
        }
        self.status.scan_waiting();
        self.status.show(&self.status_monitor, self.game_anchor)
    }

    pub fn scan_progress(&mut self, progress: ScanProgress) -> Result<()> {
        // The first progress event follows confirmation of a supported game page.
        if self.window.get_editor_open()
            && self.window.get_editor_dirty()
            && !self.reset_item_editor_for_scan()
        {
            self.window.invoke_close_dialogs();
        }
        self.status.scan_progress(progress);
        self.status.show(&self.status_monitor, self.game_anchor)
    }

    pub fn migration_committing(&mut self, migrate_presets: bool) -> Result<()> {
        self.set_status("");
        self.window.set_failure_details("".into());
        self.status.migration_committing(migrate_presets);
        self.status.show(&self.status_monitor, self.game_anchor)
    }

    pub fn scan_finished(
        &mut self,
        result: std::result::Result<ScanSummary, String>,
        cancelled: bool,
    ) -> Result<()> {
        let failed = result.is_err() && !cancelled;
        if failed {
            self.set_status("");
        }
        self.window.set_failure_scan(true);
        self.window.set_failure_details(match &result {
            Err(error) if failed => error.as_str().into(),
            _ => "".into(),
        });
        self.status.scan_finished(&result, cancelled);
        self.status.show(&self.status_monitor, self.game_anchor)
    }

    pub fn tick(&mut self) -> Result<Option<PanelPlacement>> {
        #[cfg(target_os = "windows")]
        if windows::take_display_change() {
            self.placement_dirty.set(true);
            self.status
                .reposition(&self.status_monitor, self.game_anchor)?;
        }
        #[cfg(target_os = "windows")]
        let placement = self.remember_panel_placement();
        #[cfg(not(target_os = "windows"))]
        let placement = None;

        if self
            .panel_hide_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.hide_panel()?;
        }
        // Apply pending placement after DPI changes and native move/resize gestures.
        #[cfg(target_os = "windows")]
        let interacting = windows::panel_interacting();
        #[cfg(not(target_os = "windows"))]
        let interacting = false;
        if !interacting && self.placement_dirty.get() && !self.is_panel_minimized() {
            self.placement_dirty.set(false);
            if self.is_panel_visible() {
                self.reposition(None)?;
            }
        }
        self.status.tick()?;
        Ok(placement)
    }

    #[cfg(target_os = "windows")]
    fn remember_panel_placement(&mut self) -> Option<PanelPlacement> {
        let change = windows::take_panel_change()?;
        let mut size = self.panel_placement.size;
        let minimum = PanelPlacement::default().size;
        for (axis, changed) in change.size.into_iter().enumerate() {
            if let Some(changed) = changed {
                size[axis] = changed.max(minimum[axis]);
            }
        }
        let placement = PanelPlacement {
            size,
            position: Some(change.position),
            monitor: Some(change.monitor),
        };
        if placement == self.panel_placement {
            return None;
        }
        self.panel_placement = placement.clone();
        self.placement_dirty.set(true);
        Some(placement)
    }

    pub fn is_panel_active(&self) -> bool {
        self.is_panel_open()
            && self
                .window
                .window()
                .with_winit_window(|window| window.has_focus())
                .unwrap_or(false)
    }
    pub fn is_editing(&self) -> bool {
        self.is_quick_start_open()
            || self.is_editing_items()
            || self.is_scan_confirm_open()
            || self.window.get_migration_open()
            || self.window.get_editing_name()
            || self.window.get_editing_search()
            || self.window.get_editing_folder_name()
            || self.window.get_reordering_entry()
            || self.window.get_settings_open()
            || self.window.get_delete_open()
    }

    pub fn shortcuts_blocked(&self) -> bool {
        self.is_quick_start_open()
            || self.is_editing_items()
            || self.is_scan_confirm_open()
            || self.window.get_migration_open()
            || self.window.get_delete_open()
            || self.panel_shortcut_blocked()
            || (self.is_panel_open()
                && self.window.get_settings_open()
                && self.window.get_viewing_licenses())
    }

    pub fn panel_shortcut_blocked(&self) -> bool {
        self.is_panel_open()
            && (self.window.get_editing_name()
                || self.window.get_editing_search()
                || self.window.get_editing_folder_name()
                || self.window.get_reordering_entry()
                || self.window.get_recording_shortcut() >= 0)
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
            if let Some(info) = self
                .preset_info
                .iter_mut()
                .find(|info| info.id == row.name.as_str())
            {
                info.shortcut.clone_from(&shortcut);
            }
            row.shortcut = shortcut.into();
            rows.set_row_data(index, row);
        }
        self.window.set_settings_saving(false);
        self.window.invoke_close_dialogs();
    }

    pub fn settings_failed(&self, error: &str) {
        self.window.set_settings_saving(false);
        self.window.set_settings_error(error.into());
    }

    pub fn set_update_install_allowed(&self, allowed: bool) {
        self.window.set_update_install_allowed(allowed);
    }

    pub fn set_update_state(&self, phase: UpdatePhase, version: &str, error: &str) {
        if matches!(phase, UpdatePhase::Checking | UpdatePhase::Downloading) {
            self.window.set_update_download_fraction(-1.0);
            self.window.set_update_download_size("".into());
        }
        self.window.set_update_phase(phase);
        self.window.set_update_version(version.into());
        self.window.set_update_error(error.into());
    }

    pub fn set_update_download(&self, downloaded: u64, total: Option<u64>) {
        const MIB: f64 = 1024.0 * 1024.0;
        let total = total.filter(|&bytes| bytes > 0);
        self.window
            .set_update_download_fraction(total.map_or(-1.0, |total| {
                (downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32
            }));
        let downloaded = downloaded as f64 / MIB;
        self.window.set_update_download_size(
            total
                .map_or_else(
                    || format!("{downloaded:.1} MiB"),
                    |total| format!("{downloaded:.1} / {:.1} MiB", total as f64 / MIB),
                )
                .into(),
        );
    }

    pub fn set_shortcut_warning(&self, warning: &str) {
        if self.window.get_shortcut_warning() != warning {
            self.window.set_shortcut_warning(warning.into());
        }
    }

    pub fn set_library(&mut self, library: LibraryMetadata, selected: Option<&str>) {
        let previous_index = self.window.get_current_index().max(0) as usize;
        let mut previous: BTreeMap<_, _> = self
            .window
            .get_presets()
            .iter()
            .map(|row| (row.name.to_string(), row))
            .collect();
        let selected = selected.map(str::to_owned).or_else(|| {
            self.window
                .get_presets()
                .row_data(previous_index)
                .map(|row| row.name.to_string())
        });
        let rows: Vec<_> = library
            .presets
            .iter()
            .map(|info| {
                let mut row = previous.remove(&info.id).unwrap_or_else(|| PresetRow {
                    stratagems: stratagem_section(PreviewContent::Missing),
                    equipment: equipment_section(Default::default()),
                    ..Default::default()
                });
                row.name = info.id.as_str().into();
                row.number = preset_number(&info.id).into();
                row.label = info.label.as_str().into();
                row.shortcut = info.shortcut.as_str().into();
                row
            })
            .collect();
        let index = selected
            .as_deref()
            .and_then(|id| rows.iter().position(|row| row.name == id))
            .unwrap_or_else(|| previous_index.min(rows.len().saturating_sub(1)));
        self.window.invoke_finish_reorder();
        self.preset_info = library.presets;
        let entries = library::rows(library.entries);
        self.window
            .set_presets(Rc::new(VecModel::from(rows)).into());
        self.window.set_entries(entries);
        self.window.set_current_index(index as i32);
        if !self.is_editing() {
            self.window.invoke_focus_list();
        }
        self.set_status("");
    }

    pub fn insert_preset(&mut self, library: LibraryMetadata, id: &str, source: Option<&str>) {
        self.window.set_search_query("".into());
        let mut rows: Vec<_> = self.window.get_presets().iter().collect();
        if let Some(mut row) = source
            .and_then(|id| rows.iter().find(|row| row.name == id))
            .cloned()
        {
            row.name = id.into();
            rows.push(row);
            self.window
                .set_presets(Rc::new(VecModel::from(rows)).into());
        }
        self.set_library(library, Some(id));
        self.reveal_entry(id);
    }

    pub fn set_folder_collapsed(&self, id: &str, collapsed: bool) {
        let entries = self.window.get_entries();
        if let Some((index, mut row)) = entries
            .iter()
            .enumerate()
            .find(|(_, row)| row.is_folder && row.name == id)
        {
            row.expanded = !collapsed;
            entries.set_row_data(index, row);
            if !collapsed {
                // Set the target now so manual scrolling can cancel it before layout settles.
                self.window.invoke_prepare_reveal(id.into(), true);
                let weak = self.window.as_weak();
                slint::Timer::single_shot(Duration::ZERO, move || {
                    if let Some(window) = weak.upgrade() {
                        window.invoke_ensure_revealed();
                    }
                });
            }
        }
    }

    pub fn insert_folder(&mut self, library: LibraryMetadata, id: &str) {
        self.window.set_search_query("".into());
        self.set_library(library, None);
        self.reveal_entry(id);
    }

    pub fn reveal_entry(&self, id: &str) {
        let entries: Vec<_> = self.window.get_entries().iter().collect();
        let target = if self.window.get_search_query().trim().is_empty() {
            library::visible_entry(&entries, id)
        } else {
            entries.iter().find(|entry| entry.name == id)
        };
        let Some(target) = target else {
            return;
        };
        // Reveal after the queued search and model change handlers have settled.
        let weak = self.window.as_weak();
        let name = target.name.clone();
        slint::Timer::single_shot(Duration::ZERO, move || {
            if let Some(window) = weak.upgrade() {
                window.invoke_reveal_entry(name);
            }
        });
    }

    pub fn set_label(&mut self, preset: &str, label: String) {
        if let Some(info) = self.preset_info.iter_mut().find(|info| info.id == preset) {
            info.label.clone_from(&label);
        }
        let rows = self.window.get_presets();
        if let Some((index, mut row)) = rows.iter().enumerate().find(|(_, row)| row.name == preset)
        {
            row.label = label.into();
            rows.set_row_data(index, row);
        }
        self.set_status("");
    }

    pub fn remove_preset(&mut self, library: LibraryMetadata) {
        self.set_library(library, None);
        self.window.invoke_focus_search_results();
    }
    pub fn cancel_reorder(&self) {
        self.window.invoke_finish_reorder();
    }
    pub fn set_scope(&self, scope: PresetScope) {
        self.window.set_scope_stratagems(scope.stratagems);
        self.window.set_scope_equipment(scope.equipment);
        self.window.set_scope_equipment_items(
            Rc::new(VecModel::from(scope.equipment_items.flags().to_vec())).into(),
        );
        self.window
            .set_equipment_items_selected(!scope.equipment_items.is_empty());
        self.set_status("");
    }
    pub fn set_compact_list(&self, compact: bool) {
        self.window.set_compact_list(compact);
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
        if pending {
            self.window.invoke_focus_list();
        }
        self.window.set_pending(pending);
    }

    pub fn set_previews(&self, mut previews: BTreeMap<String, PresetPreview>) {
        self.window.set_loading(false);
        let current = self.window.get_presets();
        let rows: Vec<_> = self
            .preset_info
            .iter()
            .map(|info| {
                let name = &info.id;
                let incoming = previews.remove(name);
                // An in-flight snapshot can predate a newly inserted preset.
                if incoming.is_none()
                    && let Some(row) = current.iter().find(|row| row.name == name.as_str())
                {
                    return row;
                }
                let preview = incoming.unwrap_or(PresetPreview {
                    stratagems: PreviewContent::Missing,
                    equipment: Default::default(),
                });
                PresetRow {
                    number: preset_number(name).into(),
                    label: info.label.as_str().into(),
                    name: name.as_str().into(),
                    shortcut: info.shortcut.as_str().into(),
                    stratagems: stratagem_section(preview.stratagems),
                    equipment: equipment_section(preview.equipment),
                }
            })
            .collect();
        let selected = self
            .window
            .get_presets()
            .row_data(self.window.get_current_index() as usize);
        let index = selected
            .and_then(|selected| rows.iter().position(|row| row.name == selected.name))
            .unwrap_or(0) as i32;
        self.window
            .set_presets(Rc::new(VecModel::from(rows)).into());
        self.window.set_current_index(index);
    }
}

pub(super) fn preset_number(name: &str) -> &str {
    name.strip_prefix("preset_").unwrap_or_default()
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

fn stratagem_section(content: PreviewContent<Vec<PreviewImage>>) -> PreviewSection {
    match content {
        PreviewContent::Missing => PreviewSection::default(),
        PreviewContent::Invalid(error) => PreviewSection {
            saved: true,
            error: error.into(),
            ..Default::default()
        },
        PreviewContent::Ready(images) => preview_section(
            images
                .into_iter()
                .map(|item| {
                    let (role, number) = match item.role {
                        PreviewImageRole::Stratagem(number) => {
                            (PreviewRole::Stratagem, number as i32)
                        }
                        PreviewImageRole::Booster => (PreviewRole::Booster, 0),
                        PreviewImageRole::FallbackBooster => (PreviewRole::FallbackBooster, 0),
                    };
                    preview_item(role, number, PreviewContent::Ready(item.image))
                })
                .collect(),
        ),
    }
}

fn scan_role(kind: ScanKind) -> PreviewRole {
    match kind {
        ScanKind::Stratagems => PreviewRole::Stratagem,
        ScanKind::Booster => PreviewRole::Booster,
        ScanKind::Equipment(kind) => equipment_role(kind),
    }
}

fn equipment_role(kind: EquipmentKind) -> PreviewRole {
    match kind {
        EquipmentKind::Helmet => PreviewRole::Helmet,
        EquipmentKind::Armor => PreviewRole::Armor,
        EquipmentKind::Cape => PreviewRole::Cape,
        EquipmentKind::Primary => PreviewRole::Primary,
        EquipmentKind::Secondary => PreviewRole::Secondary,
        EquipmentKind::Throwable => PreviewRole::Throwable,
    }
}

fn equipment_section(items: [PreviewContent<image::RgbaImage>; 6]) -> PreviewSection {
    preview_section(
        EquipmentKind::ALL
            .into_iter()
            .zip(items)
            .map(|(kind, content)| preview_item(equipment_role(kind), 0, content))
            .collect(),
    )
}

fn preview_item(
    role: PreviewRole,
    number: i32,
    content: PreviewContent<image::RgbaImage>,
) -> PreviewRow {
    let (state, picture, error) = match content {
        PreviewContent::Missing => (
            PreviewState::Missing,
            Default::default(),
            Default::default(),
        ),
        PreviewContent::Ready(source) => (PreviewState::Ready, image(&source), Default::default()),
        PreviewContent::Invalid(error) => (PreviewState::Invalid, Default::default(), error.into()),
    };
    PreviewRow {
        role,
        number,
        state,
        picture,
        error,
    }
}

fn preview_section(items: Vec<PreviewRow>) -> PreviewSection {
    let mut section = PreviewSection::default();
    for item in &items {
        if item.state != PreviewState::Missing {
            section.saved = true;
        }
        match item.state {
            PreviewState::Ready => {
                if !section.ready || item.role == PreviewRole::Primary {
                    section.summary = item.picture.clone();
                }
                section.ready = true;
            }
            PreviewState::Invalid if section.error.is_empty() => section.error = item.error.clone(),
            _ => {}
        }
    }
    section.items = Rc::new(VecModel::from(items)).into();
    section
}

fn preset_section(scope: ActionScope) -> Option<PresetSection> {
    match scope {
        ActionScope::Selected => None,
        ActionScope::Stratagems => Some(PresetSection::Stratagems),
        ActionScope::Equipment => Some(PresetSection::Equipment),
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
pub fn run<T: 'static>(mut tick: impl FnMut() -> Result<ControlFlow<T>> + 'static) -> Result<T> {
    let result = Rc::new(RefCell::new(None));
    let tick_result = result.clone();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(10),
        move || {
            let outcome = match tick() {
                Ok(ControlFlow::Continue(())) => return,
                Ok(ControlFlow::Break(value)) => Ok(value),
                Err(error) => Err(error),
            };
            *tick_result.borrow_mut() = Some(outcome);
            let _ = slint::quit_event_loop();
        },
    );
    slint::run_event_loop_until_quit()?;
    result
        .borrow_mut()
        .take()
        .context("the UI event loop exited without a result")?
}
