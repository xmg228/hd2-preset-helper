use std::{
    cell::{Cell, RefCell},
    mem::size_of,
    thread::sleep,
    time::{Duration, Instant},
};

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
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WM_HOTKEY,
};

use crate::game_window::GameWindow;

const CLICK_MOVE_SETTLE_DELAY: Duration = Duration::from_millis(30);

#[derive(Default)]
struct InjectedInputState {
    keys: Vec<Vk>,
    left_mouse_down: bool,
}

thread_local! {
    static AUTOMATION_TARGET: Cell<Option<GameWindow>> = const { Cell::new(None) };
    static INJECTED_INPUT_STATE: RefCell<InjectedInputState> =
        RefCell::new(InjectedInputState::default());
}

pub struct AutomationScope {
    previous: Option<GameWindow>,
}

impl AutomationScope {
    pub fn new(target: GameWindow) -> Result<Self> {
        target.ensure_automation_target()?;
        let previous = AUTOMATION_TARGET.replace(Some(target));
        Ok(Self { previous })
    }
}

impl Drop for AutomationScope {
    fn drop(&mut self) {
        release_tracked_inputs_best_effort();
        AUTOMATION_TARGET.set(self.previous);
    }
}

fn ensure_automation_target() -> Result<()> {
    AUTOMATION_TARGET.with(|target| match target.get() {
        Some(target) => target.ensure_automation_target(),
        None => Ok(()),
    })
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vk {
    B = 0x42,

    F1 = 0x70,
    F2 = 0x71,
    F3 = 0x72,
    F4 = 0x73,
    F5 = 0x74,
    F6 = 0x75,
    F7 = 0x76,
    F8 = 0x77,
    F9 = 0x78,
    F10 = 0x79,
    F11 = 0x7A,
    F12 = 0x7B,

    LCtrl = 0xA2,
    RCtrl = 0xA3,
    LShift = 0xA0,
    RShift = 0xA1,
    LAlt = 0xA4,
    RAlt = 0xA5,
    LWin = 0x5B,
    RWin = 0x5C,
}
impl Vk {
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

    fn scan_code(self) -> u16 {
        match self {
            Vk::B => 0x30,
            Vk::F1 => 0x3B,
            Vk::F2 => 0x3C,
            Vk::F3 => 0x3D,
            Vk::F4 => 0x3E,
            Vk::F5 => 0x3F,
            Vk::F6 => 0x40,
            Vk::F7 => 0x41,
            Vk::F8 => 0x42,
            Vk::F9 => 0x43,
            Vk::F10 => 0x44,
            Vk::F11 => 0x57,
            Vk::F12 => 0x58,
            Vk::LCtrl | Vk::RCtrl => 0x1D,
            Vk::LShift => 0x2A,
            Vk::RShift => 0x36,
            Vk::LAlt | Vk::RAlt => 0x38,
            Vk::LWin => 0x5B,
            Vk::RWin => 0x5C,
        }
    }

    fn is_extended(self) -> bool {
        matches!(self, Vk::RCtrl | Vk::RAlt | Vk::LWin | Vk::RWin)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Vk::B => "B",
            Vk::F1 => "F1",
            Vk::F2 => "F2",
            Vk::F3 => "F3",
            Vk::F4 => "F4",
            Vk::F5 => "F5",
            Vk::F6 => "F6",
            Vk::F7 => "F7",
            Vk::F8 => "F8",
            Vk::F9 => "F9",
            Vk::F10 => "F10",
            Vk::F11 => "F11",
            Vk::F12 => "F12",
            Vk::LCtrl => "LCtrl",
            Vk::RCtrl => "RCtrl",
            Vk::LShift => "LShift",
            Vk::RShift => "RShift",
            Vk::LAlt => "LAlt",
            Vk::RAlt => "RAlt",
            Vk::LWin => "LWin",
            Vk::RWin => "RWin",
        }
    }
}

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

    fn hotkey_modifiers(self) -> HOT_KEY_MODIFIERS {
        match self {
            Self::Shift => MOD_SHIFT,
            Self::Ctrl => MOD_CONTROL,
            Self::Alt => MOD_ALT,
            Self::Win => MOD_WIN,
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

    fn release_keys(self) -> &'static [Vk] {
        match self {
            Self::Shift => &[Vk::LShift, Vk::RShift],
            Self::Ctrl => &[Vk::LCtrl, Vk::RCtrl],
            Self::Alt => &[Vk::LAlt, Vk::RAlt],
            Self::Win => &[Vk::LWin, Vk::RWin],
        }
    }

    fn is_down(self) -> bool {
        match self {
            Self::Shift => is_pressed(Vk::LShift) || is_pressed(Vk::RShift),
            Self::Ctrl => is_pressed(Vk::LCtrl) || is_pressed(Vk::RCtrl),
            Self::Alt => is_pressed(Vk::LAlt) || is_pressed(Vk::RAlt),
            Self::Win => is_pressed(Vk::LWin) || is_pressed(Vk::RWin),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HotkeyModifiers {
    values: [Option<HotkeyModifier>; 4],
}

impl HotkeyModifiers {
    pub fn new(values: Vec<HotkeyModifier>) -> Result<Self> {
        if values.is_empty() {
            bail!("hotkey modifiers must not be empty");
        }
        if values.len() > 4 {
            bail!(
                "hotkey modifiers support at most 4 keys, got {}",
                values.len()
            );
        }

        let mut registration_bits = 0u32;
        let mut modifiers = Self {
            values: [None, None, None, None],
        };

        for (index, modifier) in values.into_iter().enumerate() {
            let bit = modifier.hotkey_modifiers().0;
            if registration_bits & bit != 0 {
                bail!(
                    "hotkey modifiers cannot contain duplicate {}",
                    modifier.name()
                );
            }
            registration_bits |= bit;
            modifiers.values[index] = Some(modifier);
        }

        Ok(modifiers)
    }

    fn hotkey_modifiers(self) -> HOT_KEY_MODIFIERS {
        self.iter().fold(MOD_NOREPEAT, |modifiers, modifier| {
            modifiers | modifier.hotkey_modifiers()
        })
    }

    fn release_keys(self) -> Vec<Vk> {
        let mut keys = Vec::new();
        for modifier in self.iter() {
            keys.extend_from_slice(modifier.release_keys());
        }
        keys
    }

    pub(crate) fn is_down(self) -> bool {
        self.iter().all(HotkeyModifier::is_down)
    }

    fn iter(self) -> impl Iterator<Item = HotkeyModifier> {
        self.values.into_iter().flatten()
    }

    fn label_with_key(self, key: Vk) -> String {
        let mut parts = self
            .iter()
            .map(HotkeyModifier::display_name)
            .collect::<Vec<_>>();
        parts.push(key.name());
        parts.join(" + ")
    }
}

fn keyboard_input(key: Vk, up: bool) -> INPUT {
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
        bail!("{description} was not fully sent, sent={sent}");
    }
    Ok(())
}

fn track_key_down(key: Vk) {
    INJECTED_INPUT_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if !state.keys.contains(&key) {
            state.keys.push(key);
        }
    });
}

fn track_key_up(key: Vk) {
    INJECTED_INPUT_STATE.with(|state| {
        state.borrow_mut().keys.retain(|pressed| *pressed != key);
    });
}

fn key_down(key: Vk) -> Result<()> {
    send_single_input(keyboard_input(key, false), "key press input")?;
    track_key_down(key);
    Ok(())
}

fn key_up(key: Vk) -> Result<()> {
    send_single_input(keyboard_input(key, true), "key release input")?;
    track_key_up(key);
    Ok(())
}

fn left_button_down() -> Result<()> {
    send_single_input(mouse_input(MOUSEEVENTF_LEFTDOWN), "left mouse press input")?;
    INJECTED_INPUT_STATE.with(|state| state.borrow_mut().left_mouse_down = true);
    Ok(())
}

fn left_button_up() -> Result<()> {
    send_single_input(mouse_input(MOUSEEVENTF_LEFTUP), "left mouse release input")?;
    INJECTED_INPUT_STATE.with(|state| state.borrow_mut().left_mouse_down = false);
    Ok(())
}

fn release_tracked_inputs_best_effort() {
    let state = INJECTED_INPUT_STATE.with(|state| std::mem::take(&mut *state.borrow_mut()));
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

pub fn wheel_with_boundary<F>(delta: i32, before_send: F) -> Result<()>
where
    F: FnOnce(),
{
    ensure_automation_target()?;
    before_send();
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

pub fn click_with_boundary<F>(x: i32, y: i32, hold_ms: u64, before_press: F) -> Result<()>
where
    F: FnOnce(),
{
    trace!(
        x,
        y,
        hold_ms,
        settle = ?CLICK_MOVE_SETTLE_DELAY,
        "mouse click input"
    );
    move_cursor(x, y)?;
    sleep(CLICK_MOVE_SETTLE_DELAY);
    click_current_with_boundary(hold_ms, before_press)
}

/// Click at the current cursor position without sending another mouse-move event.
///
/// This is useful for game UI automation where the cursor should be moved and
/// allowed to settle/hover before taking a screenshot or before issuing the
/// actual button press.  The button down/up events are pure button events; they
/// do not carry MOUSEEVENTF_MOVE / ABSOLUTE coordinates.
pub(crate) fn click_current_with_boundary<F>(hold_ms: u64, before_press: F) -> Result<()>
where
    F: FnOnce(),
{
    ensure_automation_target()?;
    before_press();
    left_button_down()?;
    sleep(Duration::from_millis(hold_ms));
    left_button_up()?;
    Ok(())
}

pub fn move_cursor(x: i32, y: i32) -> Result<()> {
    ensure_automation_target()?;
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

pub fn tap(key: Vk, hold_ms: u64) -> Result<()> {
    ensure_automation_target()?;
    trace!(key = key.name(), hold_ms, "key tap input");
    key_down(key)?;
    sleep(Duration::from_millis(hold_ms));
    key_up(key)?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct HotkeySpec {
    pub id: i32,
    pub modifiers: HotkeyModifiers,
    pub key: Vk,
}

impl HotkeySpec {
    fn label(self) -> String {
        self.modifiers.label_with_key(self.key)
    }
}

pub enum HotkeyPoll {
    Triggered(i32),
    ExitRequested,
    Timeout,
}

pub struct RegisteredHotkeys {
    hotkeys: Vec<HotkeySpec>,
}

impl RegisteredHotkeys {
    pub fn register(hotkeys: &[HotkeySpec]) -> Result<Self> {
        if hotkeys.is_empty() {
            bail!("no hotkeys to register");
        }

        let mut registered = Vec::with_capacity(hotkeys.len());
        let mut failures = Vec::new();
        let mut seen = Vec::with_capacity(hotkeys.len());
        unsafe {
            for hotkey in hotkeys {
                let registration_key = (hotkey.modifiers.hotkey_modifiers().0, hotkey.key as u32);
                if seen.contains(&registration_key) {
                    failures.push(format!("{}: configured more than once", hotkey.label()));
                    continue;
                }
                seen.push(registration_key);

                match RegisterHotKey(
                    None,
                    hotkey.id,
                    hotkey.modifiers.hotkey_modifiers(),
                    hotkey.key as u32,
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

        Ok(Self {
            hotkeys: registered,
        })
    }

    pub fn wait_timeout(&self, timeout: Duration) -> Result<HotkeyPoll> {
        let result = wait_for_any_hotkey_message_timeout(&self.hotkeys, timeout)?;
        if let HotkeyPoll::Triggered(hotkey_id) = result
            && let Some(hotkey) = self.hotkeys.iter().find(|hotkey| hotkey.id == hotkey_id)
        {
            let mut release_keys = hotkey.modifiers.release_keys();
            release_keys.push(hotkey.key);
            wait_released(&release_keys, 1500);
        }
        Ok(result)
    }
}

impl Drop for RegisteredHotkeys {
    fn drop(&mut self) {
        unsafe {
            for hotkey in &self.hotkeys {
                let _ = UnregisterHotKey(None, hotkey.id);
            }
        }
    }
}

fn wait_for_any_hotkey_message_timeout(
    hotkeys: &[HotkeySpec],
    timeout: Duration,
) -> Result<HotkeyPoll> {
    let start = Instant::now();
    let mut msg = MSG::default();

    loop {
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == crate::tray::EXIT_MESSAGE {
                return Ok(HotkeyPoll::ExitRequested);
            }
            if msg.message != WM_HOTKEY {
                continue;
            }

            let hotkey_id = msg.wParam.0 as i32;
            if hotkeys.iter().any(|hotkey| hotkey.id == hotkey_id) {
                return Ok(HotkeyPoll::Triggered(hotkey_id));
            }
        }

        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return Ok(HotkeyPoll::Timeout);
        }

        let remaining = timeout.saturating_sub(elapsed);
        sleep(remaining.min(Duration::from_millis(20)));
    }
}

fn wait_released(keys: &[Vk], timeout_ms: u64) {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(timeout_ms) {
        if keys.iter().all(|key| !is_pressed(*key)) {
            return;
        }
        sleep(Duration::from_millis(20));
    }
}

fn is_pressed(key: Vk) -> bool {
    unsafe { GetAsyncKeyState(key as i32) < 0 }
}
