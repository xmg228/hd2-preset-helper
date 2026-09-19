use std::{mem::size_of, thread::sleep, time::Duration};

use anyhow::{Context, Result, bail};
use tracing::{trace, warn};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    RegisterHotKey, SendInput, UnregisterHotKey, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MSG, PM_REMOVE, PeekMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WHEEL_DELTA, WM_HOTKEY,
};

use crate::window::{ClientPoint, WindowTarget};

use super::{HotkeyModifier, HotkeyModifiers, HotkeySpec, Key};

const CLICK_MOVE_SETTLE_DELAY: Duration = Duration::from_millis(30);

#[derive(Default)]
struct InjectedInputState {
    keys: Vec<Key>,
    left_mouse_down: bool,
}

pub struct InputSession {
    target: WindowTarget,
    injected: InjectedInputState,
}

impl Key {
    fn scan_code(self) -> u16 {
        match self {
            Key::B => 0x30,
            Key::Digit0 => 0x0B,
            Key::Digit1 => 0x02,
            Key::Digit2 => 0x03,
            Key::Digit3 => 0x04,
            Key::Digit4 => 0x05,
            Key::Digit5 => 0x06,
            Key::Digit6 => 0x07,
            Key::Digit7 => 0x08,
            Key::Digit8 => 0x09,
            Key::Digit9 => 0x0A,
            Key::Numpad0 => 0x52,
            Key::Numpad1 => 0x4F,
            Key::Numpad2 => 0x50,
            Key::Numpad3 => 0x51,
            Key::Numpad4 => 0x4B,
            Key::Numpad5 => 0x4C,
            Key::Numpad6 => 0x4D,
            Key::Numpad7 => 0x47,
            Key::Numpad8 => 0x48,
            Key::Numpad9 => 0x49,
            Key::F1 => 0x3B,
            Key::F2 => 0x3C,
            Key::F3 => 0x3D,
            Key::F4 => 0x3E,
            Key::F5 => 0x3F,
            Key::F6 => 0x40,
            Key::F7 => 0x41,
            Key::F8 => 0x42,
            Key::F9 => 0x43,
            Key::F10 => 0x44,
            Key::F11 => 0x57,
            Key::F12 => 0x58,
            Key::LCtrl | Key::RCtrl => 0x1D,
            Key::LShift => 0x2A,
            Key::RShift => 0x36,
            Key::LAlt | Key::RAlt => 0x38,
            Key::LWin => 0x5B,
            Key::RWin => 0x5C,
        }
    }

    fn virtual_key(self) -> u32 {
        match self {
            Key::B => 0x42,
            Key::Digit0 => 0x30,
            Key::Digit1 => 0x31,
            Key::Digit2 => 0x32,
            Key::Digit3 => 0x33,
            Key::Digit4 => 0x34,
            Key::Digit5 => 0x35,
            Key::Digit6 => 0x36,
            Key::Digit7 => 0x37,
            Key::Digit8 => 0x38,
            Key::Digit9 => 0x39,
            Key::Numpad0 => 0x60,
            Key::Numpad1 => 0x61,
            Key::Numpad2 => 0x62,
            Key::Numpad3 => 0x63,
            Key::Numpad4 => 0x64,
            Key::Numpad5 => 0x65,
            Key::Numpad6 => 0x66,
            Key::Numpad7 => 0x67,
            Key::Numpad8 => 0x68,
            Key::Numpad9 => 0x69,
            Key::F1 => 0x70,
            Key::F2 => 0x71,
            Key::F3 => 0x72,
            Key::F4 => 0x73,
            Key::F5 => 0x74,
            Key::F6 => 0x75,
            Key::F7 => 0x76,
            Key::F8 => 0x77,
            Key::F9 => 0x78,
            Key::F10 => 0x79,
            Key::F11 => 0x7A,
            Key::F12 => 0x7B,
            Key::LCtrl => 0xA2,
            Key::RCtrl => 0xA3,
            Key::LShift => 0xA0,
            Key::RShift => 0xA1,
            Key::LAlt => 0xA4,
            Key::RAlt => 0xA5,
            Key::LWin => 0x5B,
            Key::RWin => 0x5C,
        }
    }

    fn is_extended(self) -> bool {
        matches!(self, Key::RCtrl | Key::RAlt | Key::LWin | Key::RWin)
    }
}

impl HotkeyModifier {
    fn hotkey_modifiers(self) -> HOT_KEY_MODIFIERS {
        match self {
            Self::Shift => MOD_SHIFT,
            Self::Ctrl => MOD_CONTROL,
            Self::Alt => MOD_ALT,
            Self::Win => MOD_WIN,
        }
    }

    fn is_down(self) -> bool {
        match self {
            Self::Shift => is_pressed(Key::LShift) || is_pressed(Key::RShift),
            Self::Ctrl => is_pressed(Key::LCtrl) || is_pressed(Key::RCtrl),
            Self::Alt => is_pressed(Key::LAlt) || is_pressed(Key::RAlt),
            Self::Win => is_pressed(Key::LWin) || is_pressed(Key::RWin),
        }
    }
}

impl HotkeyModifiers {
    fn hotkey_modifiers(self) -> HOT_KEY_MODIFIERS {
        self.iter().fold(MOD_NOREPEAT, |modifiers, modifier| {
            modifiers | modifier.hotkey_modifiers()
        })
    }

    pub(crate) fn is_down(self) -> bool {
        // No modifiers means no hold-to-preview or capture preparation gesture.
        self.iter().next().is_some() && self.iter().all(HotkeyModifier::is_down)
    }
}

fn keyboard_input(key: Key, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if key.is_extended() {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: key.scan_code(),
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send_single_input(input: INPUT, description: &str) -> Result<()> {
    let sent = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    if sent != 1 {
        bail!(
            "Windows did not accept {description} (sent={sent}). If Helldivers is running as administrator, run HD2 Preset Helper as administrator too."
        );
    }
    Ok(())
}

impl InputSession {
    pub fn new(target: WindowTarget) -> Result<Self> {
        target.ensure_input_target()?;
        Ok(Self {
            target,
            injected: InjectedInputState::default(),
        })
    }

    fn ensure_target(&self) -> Result<()> {
        self.target.ensure_input_target()
    }

    fn key_down(&mut self, key: Key) -> Result<()> {
        send_single_input(keyboard_input(key, false), "key press input")?;
        if !self.injected.keys.contains(&key) {
            self.injected.keys.push(key);
        }
        Ok(())
    }

    fn key_up(&mut self, key: Key) -> Result<()> {
        send_single_input(keyboard_input(key, true), "key release input")?;
        self.injected.keys.retain(|pressed| *pressed != key);
        Ok(())
    }

    fn left_button_down(&mut self) -> Result<()> {
        send_single_input(mouse_input(MOUSEEVENTF_LEFTDOWN), "left mouse press input")?;
        self.injected.left_mouse_down = true;
        Ok(())
    }

    fn left_button_up(&mut self) -> Result<()> {
        send_single_input(mouse_input(MOUSEEVENTF_LEFTUP), "left mouse release input")?;
        self.injected.left_mouse_down = false;
        Ok(())
    }

    pub fn scroll(&mut self, notches: i32) -> Result<()> {
        self.ensure_target()?;
        let delta = notches * WHEEL_DELTA as i32;
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: delta as u32,
                    dwFlags: MOUSEEVENTF_WHEEL,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        send_single_input(input, "mouse wheel input")
            .with_context(|| format!("failed to send mouse wheel delta {delta}"))
    }

    pub fn click(&mut self, point: ClientPoint, hold_ms: u64) -> Result<()> {
        trace!(
            x = point.x,
            y = point.y,
            hold_ms,
            settle = ?CLICK_MOVE_SETTLE_DELAY,
            "mouse click input"
        );
        self.move_cursor(point)?;
        sleep(CLICK_MOVE_SETTLE_DELAY);
        self.click_current(hold_ms)
    }

    /// Click at the current cursor position without sending another mouse-move event.
    ///
    /// The button events do not carry cursor coordinates, so a caller can verify
    /// hover state between moving the cursor and issuing the click.
    pub fn click_current(&mut self, hold_ms: u64) -> Result<()> {
        self.ensure_target()?;
        self.left_button_down()?;
        sleep(Duration::from_millis(hold_ms));
        self.left_button_up()?;
        Ok(())
    }

    pub fn move_cursor(&mut self, point: ClientPoint) -> Result<()> {
        self.ensure_target()?;
        let (x, y) = self.target.client_point_to_screen(point);
        move_cursor_absolute(x, y)
    }

    pub fn tap_key(&mut self, key: Key, hold_ms: u64) -> Result<()> {
        self.ensure_target()?;
        trace!(key = key.name(), hold_ms, "key tap input");
        self.key_down(key)?;
        sleep(Duration::from_millis(hold_ms));
        self.key_up(key)?;
        Ok(())
    }

    fn release_tracked_inputs_best_effort(&mut self) {
        let state = std::mem::take(&mut self.injected);
        if state.keys.is_empty() && !state.left_mouse_down {
            return;
        }

        let key_names = state.keys.iter().map(|key| key.name()).collect::<Vec<_>>();
        let mut inputs = Vec::with_capacity(state.keys.len() + state.left_mouse_down as usize);
        if state.left_mouse_down {
            inputs.push(mouse_input(MOUSEEVENTF_LEFTUP));
        }
        inputs.extend(
            state
                .keys
                .iter()
                .rev()
                .map(|key| keyboard_input(*key, true)),
        );

        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
        if sent != inputs.len() {
            warn!(
                expected = inputs.len(),
                sent,
                keys = ?key_names,
                left_mouse_down = state.left_mouse_down,
                "failed to fully release tracked injected inputs"
            );
        } else {
            warn!(
                keys = ?key_names,
                left_mouse_down = state.left_mouse_down,
                "released tracked injected inputs after interrupted automation"
            );
        }
    }
}

impl Drop for InputSession {
    fn drop(&mut self) {
        self.release_tracked_inputs_best_effort();
    }
}

fn move_cursor_absolute(x: i32, y: i32) -> Result<()> {
    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1);
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1);
    let x = x.clamp(left, left + width - 1);
    let y = y.clamp(top, top + height - 1);

    let dx = normalize_absolute_mouse_coord(x - left, width);
    let dy = normalize_absolute_mouse_coord(y - top, height);

    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    send_single_input(input, "absolute mouse move input")
        .context("failed to move cursor with SendInput")
}

fn normalize_absolute_mouse_coord(value: i32, size: i32) -> i32 {
    if size <= 1 {
        return 0;
    }
    ((value as i64 * 65_535) / (size as i64 - 1)) as i32
}

pub struct Hotkeys {
    hotkeys: Vec<HotkeySpec>,
    enabled: bool,
}

impl Hotkeys {
    pub fn new(hotkeys: &[HotkeySpec]) -> Self {
        Self {
            hotkeys: hotkeys.to_vec(),
            enabled: false,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.enabled == enabled {
            return Ok(());
        }
        if !enabled {
            for hotkey in &self.hotkeys {
                unsafe { UnregisterHotKey(None, hotkey.id) }
                    .with_context(|| format!("failed to unregister {}", hotkey.label()))?;
            }
            self.enabled = false;
            self.discard_pending();
            return Ok(());
        }

        self.discard_pending();

        let mut registered = Vec::with_capacity(self.hotkeys.len());
        let mut failures = Vec::new();
        unsafe {
            for hotkey in &self.hotkeys {
                match RegisterHotKey(
                    None,
                    hotkey.id,
                    hotkey.modifiers.hotkey_modifiers(),
                    hotkey.key.virtual_key(),
                ) {
                    Ok(()) => registered.push(*hotkey),
                    Err(error) => failures.push(format!("{}: {error}", hotkey.label())),
                }
            }

            if !failures.is_empty() {
                for hotkey in &registered {
                    let _ = UnregisterHotKey(None, hotkey.id);
                }
            }
        }

        if !failures.is_empty() {
            bail!(
                "the following hotkeys could not be registered:\n- {}\n\nChange them in the configuration file",
                failures.join("\n- ")
            );
        }

        self.enabled = true;
        Ok(())
    }

    pub fn next_trigger(&self) -> Option<i32> {
        while let Some(id) = take_pending_hotkey() {
            if self.enabled && self.hotkeys.iter().any(|hotkey| hotkey.id == id) {
                return Some(id);
            }
        }
        None
    }

    pub fn discard_pending(&self) {
        while take_pending_hotkey().is_some() {}
    }

    pub fn is_released(&self, hotkey_id: i32) -> bool {
        let Some(hotkey) = self.hotkeys.iter().find(|hotkey| hotkey.id == hotkey_id) else {
            return false;
        };
        let mut release_keys = hotkey.modifiers.release_keys();
        release_keys.push(hotkey.key);
        release_keys.iter().all(|key| !is_pressed(*key))
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        unsafe {
            for hotkey in &self.hotkeys {
                let _ = UnregisterHotKey(None, hotkey.id);
            }
        }
    }
}

fn take_pending_hotkey() -> Option<i32> {
    let mut message = MSG::default();
    unsafe {
        PeekMessageW(&mut message, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE)
            .as_bool()
            .then_some(message.wParam.0 as i32)
    }
}

fn is_pressed(key: Key) -> bool {
    unsafe { GetAsyncKeyState(key.virtual_key() as i32) < 0 }
}
