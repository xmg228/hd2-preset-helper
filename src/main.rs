#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod app_events;
mod app_paths;
mod assets;
mod automation;
mod capture;
mod config;
#[cfg(feature = "diagnostics")]
mod equipment_probe;
mod game_settings;
mod game_window;
mod image_rect;
mod input;
mod item;
mod loadout;
mod logging;
mod permissions;
mod platform;
mod preset;
mod preset_action;
mod ui;
mod vision;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    platform::initialize();
    #[cfg(feature = "diagnostics")]
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--equipment-probe")
    {
        return match equipment_probe::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error:#}");
                ExitCode::FAILURE
            }
        };
    }
    match app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            app::show_fatal_error(&error);
            ExitCode::FAILURE
        }
    }
}
