#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::{app_paths, initialize, notify_preset_saved, show_error, show_notice};
