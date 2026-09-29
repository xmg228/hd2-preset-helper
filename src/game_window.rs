use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::window::{self, WindowIdentity, WindowTarget};

const GAME_WINDOW_TITLE: &str = "HELLDIVERS™ 2";
const WINDOW_LOOKUP_ATTEMPTS: usize = 10;
const WINDOW_LOOKUP_RETRY_DELAY: Duration = Duration::from_millis(50);

pub fn is_game_foreground() -> bool {
    window::foreground_title() == GAME_WINDOW_TITLE
}

pub fn find_game_window() -> Result<WindowIdentity> {
    WindowIdentity::find_by_title(GAME_WINDOW_TITLE)
        .context("could not find Helldivers; make sure the game is running")
}

pub fn wait_for_foreground_game_window() -> Result<WindowTarget> {
    for _ in 1..WINDOW_LOOKUP_ATTEMPTS {
        if let Ok(window) = foreground_game_window() {
            return Ok(window);
        }
        sleep(WINDOW_LOOKUP_RETRY_DELAY);
    }
    foreground_game_window()
}

pub fn foreground_game_window() -> Result<WindowTarget> {
    let target = WindowTarget::foreground().context("failed to inspect the foreground window")?;
    let title = target.title();
    if title != GAME_WINDOW_TITLE {
        bail!("Helldivers is not the foreground window; title={title:?}");
    }
    Ok(target)
}
