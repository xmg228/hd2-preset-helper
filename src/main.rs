#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod app_events;
mod app_paths;
mod assets;
mod automation;
mod capture;
mod config;
mod game_settings;
mod game_window;
mod image_rect;
mod input;
mod item;
mod loadout;
mod overlay;
mod permissions;
mod platform;
mod preset;
mod preset_action;
mod tray;
mod vision;
mod window;

use std::process::ExitCode;

fn main() -> ExitCode {
    platform::initialize();
    match app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            app::show_fatal_error(&error);
            ExitCode::FAILURE
        }
    }
}
