use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::Result;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
use windows::WindowsTray as PlatformTray;

pub struct TrayHandle {
    _platform: PlatformTray,
    exit_requested: Arc<AtomicBool>,
}

impl TrayHandle {
    pub fn exit_requested(&self) -> bool {
        self.exit_requested.load(Ordering::Relaxed)
    }
}

pub fn spawn() -> Result<TrayHandle> {
    let exit_requested = Arc::new(AtomicBool::new(false));
    let platform = PlatformTray::spawn(Arc::clone(&exit_requested))?;
    Ok(TrayHandle {
        _platform: platform,
        exit_requested,
    })
}
