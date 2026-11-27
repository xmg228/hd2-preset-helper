//! Settings request one change at a time; controls show the accepted values.
//! Key capture uses winit to distinguish keypad digits.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use slint::winit_030::{EventResult, winit};
use slint::{ComponentHandle, Model, VecModel};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use super::{
    MonitorChoice, MonitorRole, PresetPanel, SettingEdit, SettingToggleKey, SettingsPage,
    SettingsTarget, ShortcutRow, UiRequest,
};
use crate::config::{AUTO_MONITOR, PreferenceChange, ShortcutBindings, UiLanguage};
use crate::input::Shortcut;
use crate::scan::ScanScope;

struct RecordedKey {
    physical_key: PhysicalKey,
    row: i32,
    // None means Esc was pressed; wait for release without submitting a shortcut.
    value: Option<String>,
}

pub(super) fn connect(
    panel: &PresetPanel,
    send: Sender<UiRequest>,
) -> impl FnMut(&WindowEvent) -> EventResult + use<> {
    panel.on_license_text(|| crate::licenses::text().into());
    // ComboBox accepts strings; Slint supplies the translated parts reactively.
    panel.on_project_monitor_labels(|choices, automatic, unavailable, display, primary| {
        let labels = choices
            .iter()
            .map(|monitor| match monitor.role {
                MonitorRole::Automatic => automatic.clone(),
                MonitorRole::Unavailable => unavailable.clone(),
                MonitorRole::Available => {
                    let mut label = format!("{display} {}", monitor.number);
                    if !monitor.name.is_empty() {
                        label.push_str(&format!(" — {}", monitor.name));
                    }
                    if monitor.primary {
                        label.push_str(&format!(" ({primary})"));
                    }
                    label.into()
                }
            })
            .collect::<Vec<_>>();
        Rc::new(VecModel::from(labels)).into()
    });
    let open_send = send.clone();
    panel.on_edit_settings(move |page| {
        let _ = open_send.send(UiRequest::OpenSettings(SettingsTarget::Page(page)));
    });
    let open_send = send.clone();
    panel.on_edit_shortcut(move |preset| {
        let _ = open_send.send(UiRequest::OpenSettings(SettingsTarget::PresetShortcut(
            preset.to_string(),
        )));
    });
    let check_send = send.clone();
    panel.on_check_updates(move || {
        let _ = check_send.send(UiRequest::CheckUpdates);
    });
    let notes_send = send.clone();
    panel.on_view_update_notes(move || {
        let _ = notes_send.send(UiRequest::ViewUpdateNotes);
    });
    let project_send = send.clone();
    panel.on_view_project(move || {
        let _ = project_send.send(UiRequest::ViewProject);
    });
    let quick_start_send = send.clone();
    panel.on_view_quick_start(move || {
        let _ = quick_start_send.send(UiRequest::ViewQuickStart);
    });
    let download_send = send.clone();
    panel.on_download_update(move || {
        let _ = download_send.send(UiRequest::DownloadUpdate);
    });
    let install_send = send.clone();
    panel.on_install_update(move || {
        let _ = install_send.send(UiRequest::InstallUpdate);
    });
    let scan_send = send.clone();
    panel.on_scan_items(move |index| {
        if let Some(scope) = ScanScope::from_index(index) {
            let _ = scan_send.send(UiRequest::Scan(scope));
        }
    });
    let migration_send = send.clone();
    panel.on_inspect_migration(move |index| {
        if let Some(scope) = ScanScope::from_index(index) {
            let _ = migration_send.send(UiRequest::InspectMigration(scope));
        }
    });
    let recorded = Rc::new(RefCell::<Option<RecordedKey>>::new(None));
    let weak = panel.as_weak();
    let recording = recorded.clone();
    panel.on_record_shortcut(move |index| {
        if let Some(panel) = weak.upgrade() {
            recording.take();
            show_error(&panel, "", None);
            panel.set_recording_shortcut(index);
        }
    });
    let weak = panel.as_weak();
    let clear_send = send.clone();
    panel.on_clear_shortcut(move |index| {
        if let Some(panel) = weak.upgrade() {
            panel.set_recording_shortcut(-1);
            request_shortcut(&panel, &clear_send, index, String::new());
        }
    });
    let weak = panel.as_weak();
    let toggle_send = send.clone();
    panel.on_change_setting_toggle(move |key, value| {
        let Some(panel) = weak.upgrade() else { return };
        use PreferenceChange as P;
        use SettingToggleKey as K;
        let change = match key {
            K::ApplyInSavedOrder => P::ApplyInSavedOrder(value),
            K::AutoReadyUp => P::AutoReadyUp(value),
            K::SaveFallbackWhenTaken => P::SaveFallbackWhenTaken(value),
            K::KeepPanelOpen => P::KeepPanelOpen(value),
            K::AlwaysOnTop => P::AlwaysOnTop(value),
            K::CheckUpdatesAutomatically => P::CheckUpdatesAutomatically(value),
            K::StartWithWindows => {
                request(&panel, &toggle_send, SettingEdit::Startup(value));
                return;
            }
        };
        request(&panel, &toggle_send, SettingEdit::Preference(change));
    });
    let weak = panel.as_weak();
    let language_send = send.clone();
    panel.on_change_setting_language(move |index| {
        if let Some(panel) = weak.upgrade()
            && let Some(&language) = UiLanguage::ALL.get(index as usize)
        {
            request(
                &panel,
                &language_send,
                SettingEdit::Preference(PreferenceChange::Language(language)),
            );
        }
    });
    let weak = panel.as_weak();
    let monitor_send = send.clone();
    panel.on_change_setting_monitor(move |index| {
        if let Some(panel) = weak.upgrade()
            && let Some(monitor) = panel.get_monitor_choices().row_data(index as usize)
        {
            request(
                &panel,
                &monitor_send,
                SettingEdit::Preference(PreferenceChange::StatusMonitor(monitor.id.to_string())),
            );
        }
    });

    let weak = panel.as_weak();
    let mut modifiers = ModifiersState::empty();
    move |event: &WindowEvent| {
        let Some(panel) = weak.upgrade() else {
            return EventResult::Propagate;
        };
        match event {
            WindowEvent::ModifiersChanged(value) => modifiers = value.state(),
            WindowEvent::Focused(false) => {
                modifiers = ModifiersState::empty();
                recorded.take();
                panel.set_recording_shortcut(-1);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let index = panel.get_recording_shortcut();
                let recording = panel.get_settings_open() && index >= 0;
                let mut recorded = recorded.borrow_mut();
                if recording
                    && event.state == ElementState::Pressed
                    && !event.repeat
                    && event.logical_key == Key::Named(NamedKey::Escape)
                {
                    show_error(&panel, "", None);
                    *recorded = Some(RecordedKey {
                        physical_key: event.physical_key,
                        row: index,
                        value: None,
                    });
                    return EventResult::PreventDefault;
                }
                if recorded
                    .as_ref()
                    .is_some_and(|key| key.physical_key == event.physical_key)
                {
                    if event.state == ElementState::Released {
                        // Resume shortcuts only after the recorded key is released.
                        let key = recorded.take().unwrap();
                        if recording && key.row == index {
                            panel.set_recording_shortcut(-1);
                            if let Some(value) = key.value {
                                request_shortcut(&panel, &send, index, value);
                            }
                        }
                    }
                    return EventResult::PreventDefault;
                }
                if recording {
                    if event.state == ElementState::Pressed
                        && !event.repeat
                        && recorded.is_none()
                        && let Some(key) = key_name(event)
                    {
                        let mut parts = Vec::new();
                        if modifiers.control_key() {
                            parts.push("Ctrl");
                        }
                        if modifiers.shift_key() {
                            parts.push("Shift");
                        }
                        if modifiers.alt_key() {
                            parts.push("Alt");
                        }
                        if modifiers.super_key() {
                            parts.push("Win");
                        }
                        parts.push(&key);
                        match parts.join(" + ").parse::<Shortcut>() {
                            Ok(shortcut) => {
                                *recorded = Some(RecordedKey {
                                    physical_key: event.physical_key,
                                    row: index,
                                    value: Some(shortcut.to_string()),
                                });
                            }
                            Err(error) => show_error(&panel, &error.to_string(), Some(index)),
                        }
                    }
                    return EventResult::PreventDefault;
                }
            }
            _ => {}
        }
        EventResult::Propagate
    }
}

pub(super) fn open(
    panel: &PresetPanel,
    shortcuts: &ShortcutBindings,
    language: UiLanguage,
    status_monitor: &str,
    startup_state: Option<bool>,
    target: SettingsTarget,
) {
    let rows = std::iter::once(ShortcutRow {
        name: "".into(),
        binding: shortcuts.panel.as_str().into(),
        ..Default::default()
    })
    .chain(panel.get_presets().iter().map(|row| {
        ShortcutRow {
            binding: shortcuts
                .presets
                .get(row.name.as_str())
                .map(String::as_str)
                .unwrap_or("")
                .into(),
            name: row.name,
            label: row.label,
            number: row.number,
        }
    }))
    .collect::<Vec<_>>();
    let (page, shortcut_index) = match target {
        SettingsTarget::Page(page) => (page, -1),
        SettingsTarget::PresetShortcut(name) => {
            let Some(index) = rows.iter().position(|row| row.name == name) else {
                return;
            };
            (SettingsPage::General, index as i32)
        }
    };
    let mut monitors = vec![MonitorChoice {
        id: AUTO_MONITOR.into(),
        role: MonitorRole::Automatic,
        ..Default::default()
    }];
    #[cfg(target_os = "windows")]
    match super::windows::monitor_choices() {
        Ok(available) => monitors.extend(available),
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "could not list displays for settings")
        }
    }
    let selected = monitors
        .iter()
        .position(|monitor| monitor.id.eq_ignore_ascii_case(status_monitor))
        .unwrap_or_else(|| {
            monitors.push(MonitorChoice {
                id: status_monitor.into(),
                role: MonitorRole::Unavailable,
                ..Default::default()
            });
            monitors.len() - 1
        });
    panel.set_monitor_choices(Rc::new(VecModel::from(monitors)).into());
    panel.set_selected_monitor(selected as i32);
    show_preference(panel, &PreferenceChange::Language(language));
    panel.set_startup_available(startup_state.is_some());
    panel.set_setting_start_with_windows(startup_state.unwrap_or_default());
    panel.set_shortcut_rows(Rc::new(VecModel::from(rows)).into());
    show_error(panel, "", None);
    panel.set_settings_saving(false);
    panel.set_recording_shortcut(-1);
    panel.set_shortcut_settings_index(shortcut_index);
    panel.invoke_focus_list();
    panel.set_settings_open(true);
    panel.invoke_focus_settings(page);
}

pub(super) fn show_preference(panel: &PresetPanel, change: &PreferenceChange) {
    use PreferenceChange as P;
    match change {
        P::ApplyInSavedOrder(value) => panel.set_setting_apply_in_saved_order(*value),
        P::AutoReadyUp(value) => panel.set_setting_auto_ready_up(*value),
        P::SaveFallbackWhenTaken(value) => panel.set_setting_save_fallback_when_taken(*value),
        P::KeepPanelOpen(value) => panel.set_setting_keep_panel_open(*value),
        P::AlwaysOnTop(value) => panel.set_setting_always_on_top(*value),
        P::CheckUpdatesAutomatically(value) => {
            panel.set_setting_check_updates_automatically(*value)
        }
        P::Language(value) => panel.set_selected_language(
            UiLanguage::ALL
                .iter()
                .position(|language| language == value)
                .unwrap() as i32,
        ),
        P::StatusMonitor(value) => {
            if let Some(index) = panel
                .get_monitor_choices()
                .iter()
                .position(|monitor| monitor.id.eq_ignore_ascii_case(value))
            {
                panel.set_selected_monitor(index as i32);
            }
        }
    }
}

fn request_shortcut(panel: &PresetPanel, send: &Sender<UiRequest>, index: i32, value: String) {
    if let Some(row) = panel.get_shortcut_rows().row_data(index as usize)
        && row.binding != value
    {
        request(
            panel,
            send,
            SettingEdit::Shortcut {
                preset: (index != 0).then(|| row.name.to_string()),
                value,
                row: index,
            },
        );
    }
}

fn request(panel: &PresetPanel, send: &Sender<UiRequest>, edit: SettingEdit) {
    if panel.get_settings_open()
        && !panel.get_settings_saving()
        && send.send(UiRequest::EditSetting(edit)).is_ok()
    {
        show_error(panel, "", None);
        panel.set_settings_saving(true);
    }
}

pub(super) fn show_error(panel: &PresetPanel, error: &str, row: Option<i32>) {
    panel.set_shortcut_error_index(row.unwrap_or(-1));
    panel.set_settings_error(error.into());
}

fn key_name(event: &winit::event::KeyEvent) -> Option<String> {
    // Preserve keypad identity and physical punctuation even with Shift/dead keys.
    // The registration layer still follows Windows Num Lock rules.
    let physical = match event.physical_key {
        PhysicalKey::Code(KeyCode::Numpad0) => Some("Num0"),
        PhysicalKey::Code(KeyCode::Numpad1) => Some("Num1"),
        PhysicalKey::Code(KeyCode::Numpad2) => Some("Num2"),
        PhysicalKey::Code(KeyCode::Numpad3) => Some("Num3"),
        PhysicalKey::Code(KeyCode::Numpad4) => Some("Num4"),
        PhysicalKey::Code(KeyCode::Numpad5) => Some("Num5"),
        PhysicalKey::Code(KeyCode::Numpad6) => Some("Num6"),
        PhysicalKey::Code(KeyCode::Numpad7) => Some("Num7"),
        PhysicalKey::Code(KeyCode::Numpad8) => Some("Num8"),
        PhysicalKey::Code(KeyCode::Numpad9) => Some("Num9"),
        PhysicalKey::Code(KeyCode::NumpadAdd) => Some("NumAdd"),
        PhysicalKey::Code(KeyCode::NumpadSubtract) => Some("NumSubtract"),
        PhysicalKey::Code(KeyCode::NumpadMultiply) => Some("NumMultiply"),
        PhysicalKey::Code(KeyCode::NumpadDivide) => Some("NumDivide"),
        PhysicalKey::Code(KeyCode::NumpadDecimal) => Some("NumDecimal"),
        // RegisterHotKey does not distinguish the two Enter keys.
        PhysicalKey::Code(KeyCode::NumpadEnter) => Some("Enter"),
        PhysicalKey::Code(KeyCode::Minus) => Some("Minus"),
        PhysicalKey::Code(KeyCode::Equal) => Some("Equal"),
        PhysicalKey::Code(KeyCode::BracketLeft) => Some("BracketLeft"),
        PhysicalKey::Code(KeyCode::BracketRight) => Some("BracketRight"),
        PhysicalKey::Code(KeyCode::Backslash) => Some("Backslash"),
        PhysicalKey::Code(KeyCode::Semicolon) => Some("Semicolon"),
        PhysicalKey::Code(KeyCode::Quote) => Some("Quote"),
        PhysicalKey::Code(KeyCode::Backquote) => Some("Backquote"),
        PhysicalKey::Code(KeyCode::Comma) => Some("Comma"),
        PhysicalKey::Code(KeyCode::Period) => Some("Period"),
        PhysicalKey::Code(KeyCode::Slash) => Some("Slash"),
        PhysicalKey::Code(KeyCode::IntlBackslash) => Some("IntlBackslash"),
        _ => None,
    };
    if let Some(key) = physical {
        return Some(key.into());
    }
    match &event.key_without_modifiers() {
        Key::Character(text) => Some(text.to_string()),
        Key::Named(
            NamedKey::Control | NamedKey::Shift | NamedKey::Alt | NamedKey::Super | NamedKey::Meta,
        ) => None,
        Key::Named(key) => Some(match key {
            NamedKey::ArrowUp => "Up".into(),
            NamedKey::ArrowDown => "Down".into(),
            NamedKey::ArrowLeft => "Left".into(),
            NamedKey::ArrowRight => "Right".into(),
            _ => format!("{key:?}"),
        }),
        _ => None,
    }
}
