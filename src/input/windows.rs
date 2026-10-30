use std::{mem::size_of, thread::sleep, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use tracing::{trace, warn};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyboardLayout, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD,
    INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MAPVK_VK_TO_VSC, MAPVK_VSC_TO_VK_EX, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyExW,
    MapVirtualKeyW, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowThreadProcessId, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WHEEL_DELTA,
};

use crate::window::{ClientPoint, WindowTarget};

use super::{HotkeyModifier, HotkeyModifiers, Key};

mod hotkeys;
pub use hotkeys::Hotkeys;

const CLICK_MOVE_SETTLE_DELAY: Duration = Duration::from_millis(30);

#[derive(Clone, Copy)]
struct KeyPress {
    key: Key,
    // Reuse the press's scan code for release, even if the keyboard layout changes.
    scan_code: u16,
}

#[derive(Default)]
struct InjectedInputState {
    keys: Vec<KeyPress>,
    left_mouse_down: bool,
}

pub struct InputSession {
    target: WindowTarget,
    injected: InjectedInputState,
}

impl Key {
    fn scan_code(self) -> u16 {
        match self {
            Key::Up => 0x48,
            Key::Down => 0x50,
            Key::Left => 0x4B,
            Key::Right => 0x4D,
            Key::Space => 0x39,
            Key::Escape => 0x01,
            Key::Minus => 0x0C,
            Key::Equal => 0x0D,
            Key::BracketLeft => 0x1A,
            Key::BracketRight => 0x1B,
            Key::Backslash => 0x2B,
            Key::Semicolon => 0x27,
            Key::Quote => 0x28,
            Key::Backquote => 0x29,
            Key::Comma => 0x33,
            Key::Period => 0x34,
            Key::Slash => 0x35,
            Key::IntlBackslash => 0x56,
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
            _ => unsafe { MapVirtualKeyW(self.virtual_key(), MAPVK_VK_TO_VSC) as u16 },
        }
    }

    fn virtual_key(self) -> u32 {
        match self {
            Key::A => 0x41,
            Key::B => 0x42,
            Key::D => 0x44,
            Key::E => 0x45,
            Key::F => 0x46,
            Key::G => 0x47,
            Key::H => 0x48,
            Key::I => 0x49,
            Key::J => 0x4A,
            Key::K => 0x4B,
            Key::L => 0x4C,
            Key::M => 0x4D,
            Key::N => 0x4E,
            Key::O => 0x4F,
            Key::P => 0x50,
            Key::Q => 0x51,
            Key::R => 0x52,
            Key::S => 0x53,
            Key::T => 0x54,
            Key::U => 0x55,
            Key::V => 0x56,
            Key::W => 0x57,
            Key::X => 0x58,
            Key::Y => 0x59,
            Key::Up => 0x26,
            Key::Down => 0x28,
            Key::Left => 0x25,
            Key::Right => 0x27,
            Key::Z => 0x5A,
            Key::C => 0x43,
            Key::Space => 0x20,
            Key::Escape => 0x1B,
            Key::Tab => 0x09,
            Key::Enter => 0x0D,
            Key::Backspace => 0x08,
            Key::Insert => 0x2D,
            Key::Delete => 0x2E,
            Key::Home => 0x24,
            Key::End => 0x23,
            Key::PageUp => 0x21,
            Key::PageDown => 0x22,
            Key::CapsLock => 0x14,
            Key::NumLock => 0x90,
            Key::ScrollLock => 0x91,
            Key::PrintScreen => 0x2C,
            Key::Pause => 0x13,
            Key::Menu => 0x5D,
            // Punctuation names denote physical keys; OEM virtual keys vary by layout.
            Key::Minus
            | Key::Equal
            | Key::BracketLeft
            | Key::BracketRight
            | Key::Backslash
            | Key::Semicolon
            | Key::Quote
            | Key::Backquote
            | Key::Comma
            | Key::Period
            | Key::Slash
            | Key::IntlBackslash => unsafe {
                MapVirtualKeyW(self.scan_code() as u32, MAPVK_VSC_TO_VK_EX)
            },
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
            Key::NumpadAdd => 0x6B,
            Key::NumpadSubtract => 0x6D,
            Key::NumpadMultiply => 0x6A,
            Key::NumpadDivide => 0x6F,
            Key::NumpadDecimal => 0x6E,
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
            Key::F13 => 0x7C,
            Key::F14 => 0x7D,
            Key::F15 => 0x7E,
            Key::F16 => 0x7F,
            Key::F17 => 0x80,
            Key::F18 => 0x81,
            Key::F19 => 0x82,
            Key::F20 => 0x83,
            Key::F21 => 0x84,
            Key::F22 => 0x85,
            Key::F23 => 0x86,
            Key::F24 => 0x87,
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
        matches!(
            self,
            Key::RCtrl
                | Key::RAlt
                | Key::LWin
                | Key::RWin
                | Key::Up
                | Key::Down
                | Key::Left
                | Key::Right
                | Key::Insert
                | Key::Delete
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::NumpadDivide
                | Key::NumLock
                | Key::PrintScreen
                | Key::Menu
        )
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
        // Bare keys cannot start capture preparation before the trigger is pressed.
        self.iter().next().is_some() && self.iter().all(HotkeyModifier::is_down)
    }
}

impl KeyPress {
    fn resolve(key: Key, target: &WindowTarget) -> Result<Self> {
        let virtual_key = key.virtual_key();
        let scan_code = if (0x41..=0x5A).contains(&virtual_key) {
            // Letters name logical keys; resolve them using the game, not our UI thread.
            let thread = unsafe { GetWindowThreadProcessId(target.native_handle(), None) };
            ensure!(
                thread != 0,
                "could not query the game window's keyboard layout; no key was sent"
            );
            let layout = unsafe { GetKeyboardLayout(thread) };
            ensure!(
                !layout.0.is_null(),
                "the game window's keyboard layout is unavailable; no key was sent"
            );
            let scan_code = unsafe { MapVirtualKeyExW(virtual_key, MAPVK_VK_TO_VSC, Some(layout)) };
            ensure!(
                scan_code != 0,
                "key {} has no scan code in the game's keyboard layout; no key was sent",
                key.name()
            );
            scan_code as u16
        } else {
            key.scan_code()
        };
        Ok(Self { key, scan_code })
    }

    fn input(self, up: bool) -> INPUT {
        // Pause has a special multi-byte scan sequence; let Windows generate it.
        let mut flags = if self.key == Key::Pause {
            Default::default()
        } else {
            KEYEVENTF_SCANCODE
        };
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        if self.key.is_extended() {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }

        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(if self.key == Key::Pause {
                        self.key.virtual_key() as u16
                    } else {
                        0
                    }),
                    wScan: if self.key == Key::Pause {
                        0
                    } else {
                        self.scan_code
                    },
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
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
            hold_s = hold_ms as f64 / 1000.0,
            settle_s = CLICK_MOVE_SETTLE_DELAY.as_secs_f64(),
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
        let press = KeyPress::resolve(key, &self.target)?;
        trace!(
            key = key.name(),
            scan_code = press.scan_code,
            hold_s = hold_ms as f64 / 1000.0,
            "key tap input"
        );
        send_single_input(press.input(false), "key press input")?;
        self.injected.keys.push(press);
        sleep(Duration::from_millis(hold_ms));
        send_single_input(press.input(true), "key release input")?;
        self.injected.keys.pop();
        Ok(())
    }

    fn release_tracked_inputs_best_effort(&mut self) {
        let state = std::mem::take(&mut self.injected);
        if state.keys.is_empty() && !state.left_mouse_down {
            return;
        }

        let key_names = state
            .keys
            .iter()
            .map(|press| press.key.name())
            .collect::<Vec<_>>();
        let mut inputs = Vec::with_capacity(state.keys.len() + state.left_mouse_down as usize);
        if state.left_mouse_down {
            inputs.push(mouse_input(MOUSEEVENTF_LEFTUP));
        }
        inputs.extend(state.keys.iter().rev().map(|press| press.input(true)));

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

fn is_pressed(key: Key) -> bool {
    unsafe { GetAsyncKeyState(key.virtual_key() as i32) < 0 }
}

/// Observe physical input only; never synthesize releases of the user's keys.
pub(crate) fn activation_released() -> bool {
    // Panel activation keys (including Ctrl+S), left button and modifiers.
    [0x01, 0x0D, 0x20, 0x53, 0x10, 0x11, 0x12, 0x5B, 0x5C]
        .iter()
        .all(|&key| unsafe { GetAsyncKeyState(key) >= 0 })
}
