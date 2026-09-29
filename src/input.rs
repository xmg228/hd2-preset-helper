use anyhow::{Context, Result, bail, ensure};

#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Key {
    A,
    B,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Up,
    Down,
    Left,
    Right,
    Z,
    C,
    Space,
    Escape,
    Tab,
    #[serde(alias = "return", alias = "numpadenter", alias = "numenter")]
    Enter,
    Backspace,
    #[serde(alias = "ins")]
    Insert,
    #[serde(alias = "del")]
    Delete,
    Home,
    End,
    #[serde(alias = "pgup")]
    PageUp,
    #[serde(alias = "pgdn")]
    PageDown,
    CapsLock,
    NumLock,
    ScrollLock,
    PrintScreen,
    Pause,
    #[serde(alias = "contextmenu")]
    Menu,

    #[serde(alias = "-")]
    Minus,
    #[serde(alias = "=")]
    Equal,
    #[serde(alias = "[")]
    BracketLeft,
    #[serde(alias = "]")]
    BracketRight,
    #[serde(alias = "\\")]
    Backslash,
    #[serde(alias = ";")]
    Semicolon,
    #[serde(alias = "'")]
    Quote,
    #[serde(alias = "`")]
    Backquote,
    #[serde(alias = ",")]
    Comma,
    #[serde(alias = ".")]
    Period,
    #[serde(alias = "/")]
    Slash,
    IntlBackslash,

    #[serde(rename = "0")]
    Digit0,
    #[serde(rename = "1")]
    Digit1,
    #[serde(rename = "2")]
    Digit2,
    #[serde(rename = "3")]
    Digit3,
    #[serde(rename = "4")]
    Digit4,
    #[serde(rename = "5")]
    Digit5,
    #[serde(rename = "6")]
    Digit6,
    #[serde(rename = "7")]
    Digit7,
    #[serde(rename = "8")]
    Digit8,
    #[serde(rename = "9")]
    Digit9,

    #[serde(alias = "num0")]
    Numpad0,
    #[serde(alias = "num1")]
    Numpad1,
    #[serde(alias = "num2")]
    Numpad2,
    #[serde(alias = "num3")]
    Numpad3,
    #[serde(alias = "num4")]
    Numpad4,
    #[serde(alias = "num5")]
    Numpad5,
    #[serde(alias = "num6")]
    Numpad6,
    #[serde(alias = "num7")]
    Numpad7,
    #[serde(alias = "num8")]
    Numpad8,
    #[serde(alias = "num9")]
    Numpad9,
    #[serde(alias = "numadd")]
    NumpadAdd,
    #[serde(alias = "numsubtract")]
    NumpadSubtract,
    #[serde(alias = "nummultiply")]
    NumpadMultiply,
    #[serde(alias = "numdivide")]
    NumpadDivide,
    #[serde(alias = "numdecimal")]
    NumpadDecimal,

    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,

    LCtrl,
    RCtrl,
    LShift,
    RShift,
    LAlt,
    RAlt,
    LWin,
    RWin,
}

impl Key {
    pub fn is_shortcut_key(self) -> bool {
        !matches!(
            self,
            Self::Escape
                | Self::LCtrl
                | Self::RCtrl
                | Self::LShift
                | Self::RShift
                | Self::LAlt
                | Self::RAlt
                | Self::LWin
                | Self::RWin
        )
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Key::A => "A",
            Key::B => "B",
            Key::D => "D",
            Key::E => "E",
            Key::F => "F",
            Key::G => "G",
            Key::H => "H",
            Key::I => "I",
            Key::J => "J",
            Key::K => "K",
            Key::L => "L",
            Key::M => "M",
            Key::N => "N",
            Key::O => "O",
            Key::P => "P",
            Key::Q => "Q",
            Key::R => "R",
            Key::S => "S",
            Key::T => "T",
            Key::U => "U",
            Key::V => "V",
            Key::W => "W",
            Key::X => "X",
            Key::Y => "Y",
            Key::Up => "Up",
            Key::Down => "Down",
            Key::Left => "Left",
            Key::Right => "Right",
            Key::Z => "Z",
            Key::C => "C",
            Key::Space => "Space",
            Key::Escape => "Escape",
            Key::Tab => "Tab",
            Key::Enter => "Enter",
            Key::Backspace => "Backspace",
            Key::Insert => "Insert",
            Key::Delete => "Delete",
            Key::Home => "Home",
            Key::End => "End",
            Key::PageUp => "PageUp",
            Key::PageDown => "PageDown",
            Key::CapsLock => "CapsLock",
            Key::NumLock => "NumLock",
            Key::ScrollLock => "ScrollLock",
            Key::PrintScreen => "PrintScreen",
            Key::Pause => "Pause",
            Key::Menu => "Menu",
            Key::Minus => "Minus",
            Key::Equal => "Equal",
            Key::BracketLeft => "BracketLeft",
            Key::BracketRight => "BracketRight",
            Key::Backslash => "Backslash",
            Key::Semicolon => "Semicolon",
            Key::Quote => "Quote",
            Key::Backquote => "Backquote",
            Key::Comma => "Comma",
            Key::Period => "Period",
            Key::Slash => "Slash",
            Key::IntlBackslash => "IntlBackslash",
            Key::Digit0 => "0",
            Key::Digit1 => "1",
            Key::Digit2 => "2",
            Key::Digit3 => "3",
            Key::Digit4 => "4",
            Key::Digit5 => "5",
            Key::Digit6 => "6",
            Key::Digit7 => "7",
            Key::Digit8 => "8",
            Key::Digit9 => "9",
            Key::Numpad0 => "Num0",
            Key::Numpad1 => "Num1",
            Key::Numpad2 => "Num2",
            Key::Numpad3 => "Num3",
            Key::Numpad4 => "Num4",
            Key::Numpad5 => "Num5",
            Key::Numpad6 => "Num6",
            Key::Numpad7 => "Num7",
            Key::Numpad8 => "Num8",
            Key::Numpad9 => "Num9",
            Key::NumpadAdd => "NumAdd",
            Key::NumpadSubtract => "NumSubtract",
            Key::NumpadMultiply => "NumMultiply",
            Key::NumpadDivide => "NumDivide",
            Key::NumpadDecimal => "NumDecimal",
            Key::F1 => "F1",
            Key::F2 => "F2",
            Key::F3 => "F3",
            Key::F4 => "F4",
            Key::F5 => "F5",
            Key::F6 => "F6",
            Key::F7 => "F7",
            Key::F8 => "F8",
            Key::F9 => "F9",
            Key::F10 => "F10",
            Key::F11 => "F11",
            Key::F12 => "F12",
            Key::F13 => "F13",
            Key::F14 => "F14",
            Key::F15 => "F15",
            Key::F16 => "F16",
            Key::F17 => "F17",
            Key::F18 => "F18",
            Key::F19 => "F19",
            Key::F20 => "F20",
            Key::F21 => "F21",
            Key::F22 => "F22",
            Key::F23 => "F23",
            Key::F24 => "F24",
            Key::LCtrl => "LCtrl",
            Key::RCtrl => "RCtrl",
            Key::LShift => "LShift",
            Key::RShift => "RShift",
            Key::LAlt => "LAlt",
            Key::RAlt => "RAlt",
            Key::LWin => "LWin",
            Key::RWin => "RWin",
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows::{Hotkeys, InputSession};

#[cfg(target_os = "windows")]
pub(crate) use windows::activation_released;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyModifier {
    Shift,
    Ctrl,
    Alt,
    Win,
}

impl HotkeyModifier {
    fn name(self) -> &'static str {
        match self {
            Self::Shift => "shift",
            Self::Ctrl => "ctrl",
            Self::Alt => "alt",
            Self::Win => "win",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Shift => "Shift",
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Win => "Win",
        }
    }

    fn release_keys(self) -> &'static [Key] {
        match self {
            Self::Shift => &[Key::LShift, Key::RShift],
            Self::Ctrl => &[Key::LCtrl, Key::RCtrl],
            Self::Alt => &[Key::LAlt, Key::RAlt],
            Self::Win => &[Key::LWin, Key::RWin],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyModifiers {
    values: [Option<HotkeyModifier>; 4],
}

impl HotkeyModifiers {
    pub fn new(mut values: Vec<HotkeyModifier>) -> Result<Self> {
        if values.len() > 4 {
            bail!(
                "hotkey modifiers support at most 4 keys, got {}",
                values.len()
            );
        }
        values.sort_by_key(|modifier| match modifier {
            HotkeyModifier::Ctrl => 0,
            HotkeyModifier::Shift => 1,
            HotkeyModifier::Alt => 2,
            HotkeyModifier::Win => 3,
        });
        let mut modifiers = Self { values: [None; 4] };
        for (index, modifier) in values.into_iter().enumerate() {
            if modifiers.iter().any(|existing| existing == modifier) {
                bail!(
                    "hotkey modifiers cannot contain duplicate {}",
                    modifier.name()
                );
            }
            modifiers.values[index] = Some(modifier);
        }
        Ok(modifiers)
    }

    fn release_keys(self) -> Vec<Key> {
        let mut keys = Vec::new();
        for modifier in self.iter() {
            keys.extend_from_slice(modifier.release_keys());
        }
        keys
    }

    pub(crate) fn iter(self) -> impl Iterator<Item = HotkeyModifier> {
        self.values.into_iter().flatten()
    }

    fn label_with_key(self, key: Key) -> String {
        let mut parts = self
            .iter()
            .map(HotkeyModifier::display_name)
            .collect::<Vec<_>>();
        parts.push(key.name());
        parts.join(" + ")
    }
}

/// A platform-independent key combination, without registration or preset identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub modifiers: HotkeyModifiers,
    pub key: Key,
}

impl std::str::FromStr for Shortcut {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut parts = value.split('+').map(str::trim).collect::<Vec<_>>();
        let key = parts.pop().context("shortcut is empty")?.to_lowercase();
        let key: Key = serde_json::from_value(serde_json::Value::String(key))
            .with_context(|| format!("unsupported shortcut: {value}"))?;
        ensure!(
            key.is_shortcut_key(),
            "choose a non-modifier key other than Esc"
        );
        let modifiers = parts
            .into_iter()
            .map(|part| {
                serde_json::from_value(serde_json::Value::String(part.to_lowercase()))
                    .with_context(|| format!("unknown modifier: {part}"))
            })
            .collect::<Result<Vec<HotkeyModifier>>>()?;
        Ok(Self {
            modifiers: HotkeyModifiers::new(modifiers)?,
            key,
        })
    }
}

impl std::fmt::Display for Shortcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.modifiers.label_with_key(self.key))
    }
}

impl Shortcut {
    pub fn spec(self, id: i32) -> HotkeySpec {
        HotkeySpec {
            id,
            modifiers: self.modifiers,
            key: self.key,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HotkeySpec {
    pub id: i32,
    pub modifiers: HotkeyModifiers,
    pub key: Key,
}

impl HotkeySpec {
    fn label(self) -> String {
        self.modifiers.label_with_key(self.key)
    }
}
