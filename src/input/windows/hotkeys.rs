//! A message-only recipient keeps hotkeys independent of the GUI's message loop.
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetWindowLongPtrW, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
    SetWindowLongPtrW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_HOTKEY, WM_NCCREATE, WNDCLASSW,
};
use windows::core::w;

use crate::input::HotkeySpec;

pub struct Hotkeys {
    hotkeys: Vec<HotkeySpec>,
    enabled: Vec<i32>,
    window: HWND,
    // The window borrows this stable address until DestroyWindow in Drop.
    _sender: Box<Sender<i32>>,
    pending: Receiver<i32>,
}

impl Hotkeys {
    pub fn new(hotkeys: &[HotkeySpec]) -> Result<Self> {
        let (sender, pending) = mpsc::channel();
        let sender = Box::new(sender);
        let instance = unsafe { GetModuleHandleW(None) }?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("HD2PresetHelperHotkeys"),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(windows::core::Error::from_thread())
                .context("failed to register hotkey window class");
        }
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class.lpszClassName,
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                Some((&*sender as *const Sender<i32>).cast()),
            )
        }
        .context("failed to create hotkey message window")?;
        Ok(Self {
            hotkeys: hotkeys.to_vec(),
            enabled: Vec::new(),
            window,
            _sender: sender,
            pending,
        })
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<()> {
        self.set_enabled_where(|_| enabled)
    }

    /// Registration policy belongs to the caller, not the input backend.
    pub fn set_enabled_where(&mut self, enabled: impl Fn(&HotkeySpec) -> bool) -> Result<()> {
        let requested: Vec<_> = self
            .hotkeys
            .iter()
            .filter(|key| enabled(key))
            .map(|key| key.id)
            .collect();
        if self.enabled == requested {
            return Ok(());
        }
        for hotkey in self
            .hotkeys
            .iter()
            .filter(|key| self.enabled.contains(&key.id))
        {
            unsafe { UnregisterHotKey(Some(self.window), hotkey.id) }
                .with_context(|| format!("failed to unregister {}", hotkey.label()))?;
        }
        self.enabled.clear();
        self.discard_pending();
        let mut failures = Vec::new();
        for hotkey in self
            .hotkeys
            .iter()
            .filter(|key| requested.contains(&key.id))
        {
            match unsafe {
                RegisterHotKey(
                    Some(self.window),
                    hotkey.id,
                    hotkey.modifiers.hotkey_modifiers(),
                    hotkey.key.virtual_key(),
                )
            } {
                Ok(()) => self.enabled.push(hotkey.id),
                Err(error) => failures.push(format!("{}: {error}", hotkey.label())),
            }
        }
        if !failures.is_empty() {
            for id in self.enabled.drain(..) {
                let _ = unsafe { UnregisterHotKey(Some(self.window), id) };
            }
            bail!(
                "the following hotkeys could not be registered:\n- {}\n\nChange them in the configuration file",
                failures.join("\n- ")
            );
        }
        Ok(())
    }

    fn pump_pending(&self) {
        // Drain queued hotkey messages before consuming or discarding triggers.
        let mut message = MSG::default();
        while unsafe {
            PeekMessageW(
                &mut message,
                Some(self.window),
                WM_HOTKEY,
                WM_HOTKEY,
                PM_REMOVE,
            )
        }
        .as_bool()
        {
            unsafe {
                DispatchMessageW(&message);
            }
        }
    }

    pub fn next_trigger(&self) -> Option<i32> {
        self.pump_pending();
        self.pending.try_iter().find(|id| self.enabled.contains(id))
    }

    pub fn discard_pending(&self) {
        self.pump_pending();
        for _ in self.pending.try_iter() {}
    }

    pub fn is_released(&self, id: i32) -> bool {
        self.hotkeys
            .iter()
            .find(|key| key.id == id)
            .is_some_and(|hotkey| {
                hotkey
                    .modifiers
                    .release_keys()
                    .into_iter()
                    .chain([hotkey.key])
                    .all(|key| !super::is_pressed(key))
            })
    }
}

impl Drop for Hotkeys {
    fn drop(&mut self) {
        for &id in &self.enabled {
            let _ = unsafe { UnregisterHotKey(Some(self.window), id) };
        }
        let _ = unsafe { DestroyWindow(self.window) };
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        } else if message == WM_HOTKEY {
            let sender = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Sender<i32>;
            if let Some(sender) = sender.as_ref() {
                let _ = sender.send(wparam.0 as i32);
            }
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}
