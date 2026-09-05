use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::window::WindowTarget;

const GAME_WINDOW_TITLE: &str = "HELLDIVERS™ 2";
const WINDOW_LOOKUP_ATTEMPTS: usize = 10;
const WINDOW_LOOKUP_RETRY_DELAY: Duration = Duration::from_millis(50);

pub fn find_game_window() -> Result<WindowTarget> {
    for _ in 1..WINDOW_LOOKUP_ATTEMPTS {
        if let Ok(window) = find_game_window_once() {
            return Ok(window);
        }
        sleep(WINDOW_LOOKUP_RETRY_DELAY);
    }
    find_game_window_once()
}

pub fn find_game_window_once() -> Result<WindowTarget> {
    let target = WindowTarget::foreground().context("failed to inspect the foreground window")?;
    let title = target.title();
    if title != GAME_WINDOW_TITLE {
        bail!("Helldivers is not the foreground window; title={title:?}");
    }
    Ok(target)
}
