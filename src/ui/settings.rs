//! Settings stay in a UI draft until the controller validates and saves them.
//! Key capture uses winit to distinguish keypad digits.
use std::rc::Rc;
use std::sync::mpsc::Sender;

use slint::winit_030::{EventResult, winit};
use slint::{ComponentHandle, Model, VecModel};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use super::{MonitorChoice, PresetPanel, ShortcutRow, UiRequest};
use crate::config::{AUTO_MONITOR, SettingsDraft, ShortcutBindings};
use crate::input::Shortcut;

pub(super) fn connect(
    panel: &PresetPanel,
    send: Sender<UiRequest>,
) -> impl FnMut(&WindowEvent) -> EventResult + use<> {
    let open_send = send.clone();
    panel.on_edit_settings(move || {
        let _ = open_send.send(UiRequest::OpenSettings);
    });
    let weak = panel.as_weak();
    panel.on_clear_shortcut(move |index| {
        if let Some(panel) = weak.upgrade() {
            set_binding(&panel, index, String::new());
            panel.set_recording_shortcut(-1);
        }
    });
    let weak = panel.as_weak();
    panel.on_save_settings(move || {
        let Some(panel) = weak.upgrade() else { return };
        let monitor = panel
            .get_monitor_choices()
            .row_data(panel.get_selected_monitor() as usize)
            .expect("settings always include a selected display");
        panel.set_settings_saving(true);
        let _ = send.send(UiRequest::SaveSettings(SettingsDraft {
            shortcuts: bindings(&panel),
            monitor: monitor.id.to_string(),
            apply_in_saved_order: panel.get_draft_apply_in_saved_order(),
            auto_ready_up: panel.get_draft_auto_ready_up(),
            save_fallback_when_taken: panel.get_draft_save_fallback_when_taken(),
        }));
    });

    let weak = panel.as_weak();
    let mut modifiers = ModifiersState::empty();
    let mut captured = None;
    move |event: &WindowEvent| {
        let Some(panel) = weak.upgrade() else {
            return EventResult::Propagate;
        };
        match event {
            WindowEvent::ModifiersChanged(value) => modifiers = value.state(),
            WindowEvent::Focused(false) => {
                modifiers = ModifiersState::empty();
                captured = None;
                panel.set_recording_shortcut(-1);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if captured == Some(event.physical_key) {
                    if event.state == ElementState::Released {
                        captured = None;
                        // Resume shortcuts only after the recorded key is released.
                        panel.set_recording_shortcut(-1);
                    }
                    return EventResult::PreventDefault;
                }
                if panel.get_settings_open() && panel.get_recording_shortcut() >= 0 {
                    if event.state == ElementState::Pressed && !event.repeat {
                        if event.logical_key == Key::Named(NamedKey::Escape) {
                            captured = Some(event.physical_key);
                        } else if let Some(key) = key_name(event) {
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
                                    set_binding(
                                        &panel,
                                        panel.get_recording_shortcut(),
                                        shortcut.to_string(),
                                    );
                                    captured = Some(event.physical_key);
                                }
                                Err(error) => panel.set_settings_error(error.to_string().into()),
                            }
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

pub(super) fn open(panel: &PresetPanel, draft: SettingsDraft) {
    let rows = std::iter::once(ShortcutRow {
        name: "".into(),
        title: "Open panel".into(),
        binding: draft.shortcuts.panel.as_str().into(),
    })
    .chain(panel.get_presets().iter().map(|row| {
        ShortcutRow {
            binding: draft
                .shortcuts
                .presets
                .get(row.name.as_str())
                .map(String::as_str)
                .unwrap_or("")
                .into(),
            name: row.name,
            title: row.title,
        }
    }))
    .collect::<Vec<_>>();
    let mut monitors = vec![MonitorChoice {
        id: AUTO_MONITOR.into(),
        label: "Follow game (automatic)".into(),
    }];
    #[cfg(target_os = "windows")]
    match super::windows::monitor_choices() {
        Ok(available) => monitors.extend(available),
        Err(error) => tracing::warn!(%error, "could not list displays for settings"),
    }
    let selected = monitors
        .iter()
        .position(|monitor| monitor.id.eq_ignore_ascii_case(&draft.monitor))
        .unwrap_or_else(|| {
            monitors.push(MonitorChoice {
                id: draft.monitor.as_str().into(),
                label: "Saved display (unavailable)".into(),
            });
            monitors.len() - 1
        });
    panel.set_monitor_labels(
        Rc::new(VecModel::from(
            monitors
                .iter()
                .map(|monitor| monitor.label.clone())
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    panel.set_monitor_choices(Rc::new(VecModel::from(monitors)).into());
    panel.set_selected_monitor(selected as i32);
    panel.set_draft_apply_in_saved_order(draft.apply_in_saved_order);
    panel.set_draft_auto_ready_up(draft.auto_ready_up);
    panel.set_draft_save_fallback_when_taken(draft.save_fallback_when_taken);
    panel.set_shortcut_rows(Rc::new(VecModel::from(rows)).into());
    panel.set_settings_error("".into());
    panel.set_settings_saving(false);
    panel.set_recording_shortcut(-1);
    panel.set_settings_open(true);
    panel.invoke_focus_settings();
}

fn bindings(panel: &PresetPanel) -> ShortcutBindings {
    let rows = panel.get_shortcut_rows();
    ShortcutBindings {
        panel: rows
            .row_data(0)
            .map(|row| row.binding.to_string())
            .unwrap_or_default(),
        presets: rows
            .iter()
            .skip(1)
            .map(|row| (row.name.to_string(), row.binding.to_string()))
            .collect(),
    }
}

fn set_binding(panel: &PresetPanel, index: i32, binding: String) {
    let rows = panel.get_shortcut_rows();
    if let Some(mut row) = rows.row_data(index as usize) {
        row.binding = binding.into();
        rows.set_row_data(index as usize, row);
    }
    panel.set_settings_error(
        bindings(panel)
            .validate()
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
            .into(),
    );
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
