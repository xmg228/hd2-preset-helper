#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Key {
    B,

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
    pub fn is_function_key(self) -> bool {
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
        )
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Key::B => "B",
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
pub use windows::{
    HotkeyModifier, HotkeyModifiers, HotkeyPoll, HotkeySpec, InputSession, RegisteredHotkeys,
};
