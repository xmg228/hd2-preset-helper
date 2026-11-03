use anyhow::{Context, Result};
use auto_launch::{AutoLaunch, AutoLaunchBuilder};

pub const BACKGROUND_ARGUMENT: &str = "--background";

const STARTUP_NAME: &str = "HD2PresetHelper";

pub fn is_enabled() -> Result<bool> {
    registration()?
        .is_enabled()
        .context("failed to read the startup setting")
}

/// Apply an explicit user choice; enabling also refreshes a moved executable's path.
/// Do not call this automatically at startup: the system may have disabled the entry.
pub fn set_enabled(enabled: bool) -> Result<()> {
    let registration = registration()?;
    if enabled {
        registration
            .enable()
            .context("failed to enable startup at login")
    } else {
        registration
            .disable()
            .context("failed to disable startup at login")
    }
}

fn registration() -> Result<AutoLaunch> {
    let executable = std::env::current_exe().context("failed to locate the executable")?;
    let executable = executable
        .to_str()
        .context("the executable path is not valid Unicode")?;
    let mut builder = AutoLaunchBuilder::new();
    builder
        .set_app_name(STARTUP_NAME)
        .set_app_path(executable)
        .set_args(&[BACKGROUND_ARGUMENT]);
    #[cfg(target_os = "windows")]
    {
        // auto-launch 0.6.0 does not quote Windows executable paths.
        // Revisit this workaround when upgrading the dependency.
        builder
            .set_app_path(&format!("\"{executable}\""))
            .set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);
    }
    builder
        .build()
        .context("failed to configure startup at login")
}
