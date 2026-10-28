#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod app_events;
mod app_paths;
mod assets;
mod automation;
mod capture;
mod config;
mod evidence;
mod game_settings;
mod game_window;
mod image_rect;
mod input;
mod item;
mod licenses;
mod loadout;
mod logging;
mod permissions;
mod platform;
mod preset;
mod preset_action;
mod ui;
mod update;
mod vision;
mod window;

use anyhow::Context;
use std::process::ExitCode;

fn main() -> ExitCode {
    platform::initialize();
    // app::run has dropped its worker, logging guard and single-instance marker
    // before the updated process is launched.
    let result = app::run().and_then(|restart| {
        if let Some(executable) = restart {
            std::process::Command::new(&executable)
                .current_dir(executable.parent().context("restart target has no parent")?)
                .spawn()
                .context("the update was installed, but could not restart; open HD2 Preset Helper manually")?;
        }
        Ok(())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            app::show_fatal_error(&error);
            ExitCode::FAILURE
        }
    }
}
