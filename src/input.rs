use anyhow::{Result, bail};

#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Key {
    B,

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

    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,

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
    pub fn is_preset_key(self) -> bool {
        matches!(
            self,
            Self::F1
                | Self::F2
                | Self::F3
                | Self::F4
                | Self::F5
                | Self::F6
                | Self::F7
                | Self::F8
                | Self::F9
                | Self::F10
                | Self::F11
                | Self::F12
                | Self::Digit0
                | Self::Digit1
                | Self::Digit2
                | Self::Digit3
                | Self::Digit4
                | Self::Digit5
                | Self::Digit6
                | Self::Digit7
                | Self::Digit8
                | Self::Digit9
                | Self::Numpad0
                | Self::Numpad1
                | Self::Numpad2
                | Self::Numpad3
                | Self::Numpad4
                | Self::Numpad5
                | Self::Numpad6
                | Self::Numpad7
                | Self::Numpad8
                | Self::Numpad9
        )
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Key::B => "B",
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
pub use windows::{InputSession, RegisteredHotkeys};

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

#[derive(Debug, Clone, Copy)]
pub struct HotkeyModifiers {
    values: [Option<HotkeyModifier>; 4],
}

impl HotkeyModifiers {
    pub fn new(values: Vec<HotkeyModifier>) -> Result<Self> {
        if values.len() > 4 {
            bail!(
                "hotkey modifiers support at most 4 keys, got {}",
                values.len()
            );
        }
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

    fn iter(self) -> impl Iterator<Item = HotkeyModifier> {
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
