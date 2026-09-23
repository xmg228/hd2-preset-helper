pub mod monitors;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::Threading::{CreateMutexExW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MESSAGEBOX_STYLE, MessageBoxW,
};
use windows::core::{PCWSTR, w};

use crate::app_paths::AppPaths;

pub struct SingleInstance(HANDLE);

impl SingleInstance {
    pub fn try_acquire() -> Result<Option<Self>> {
        // Scope to this login session, regardless of executable location.
        // Request only synchronization access, not full access to an elevated instance.
        let handle = unsafe {
            CreateMutexExW(
                None,
                w!("Local\\HD2PresetHelper"),
                0,
                SYNCHRONIZATION_SYNCHRONIZE.0,
            )
        }
        .context("failed to check whether HD2 Preset Helper is already running")?;
        let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let instance = Self(handle);
        if already_running {
            Ok(None)
        } else {
            Ok(Some(instance))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // Holding the handle keeps the marker alive; no mutex ownership is needed.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn initialize() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn notify_preset_saved() {
    unsafe {
        MessageBeep(MB_ICONINFORMATION).ok();
    }
}

pub fn show_error(title: &str, message: &str) {
    show_message(title, message, MB_ICONERROR);
}

pub fn show_notice(title: &str, message: &str) {
    show_message(title, message, MB_ICONINFORMATION);
}

fn show_message(title: &str, message: &str, icon: MESSAGEBOX_STYLE) {
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | icon | MB_SETFOREGROUND,
        );
    }
}

pub fn app_paths() -> Result<AppPaths> {
    let executable = std::env::current_exe().context("failed to locate the executable")?;
    let directory = executable.parent().with_context(|| {
        format!(
            "executable has no parent directory: {}",
            executable.display()
        )
    })?;
    let data = directory.join("data");
    Ok(AppPaths {
        config: data.join("config.toml"),
        presets: data.join("presets.json"),
        log: data.join("app.log"),
        #[cfg(feature = "diagnostics")]
        diagnostic_scores: data.join("diagnostics/matcher-scores.jsonl"),
        last_failure: data.join("debug/last_failure"),
    })
}
