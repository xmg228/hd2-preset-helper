#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::{
    SingleInstance, app_paths, initialize, monitors, notify_preset_saved, open_url, show_error,
    show_notice,
};
