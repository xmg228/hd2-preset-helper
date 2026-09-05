#[cfg(target_os = "windows")]
mod windows;

use anyhow::Result;

pub fn ensure_input_access() -> Result<()> {
    windows::ensure_input_access()
}
