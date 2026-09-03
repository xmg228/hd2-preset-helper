use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::window::WindowTarget;

const GAME_WINDOW_TITLE: &str = "HELLDIVERS™ 2";
const WINDOW_LOOKUP_ATTEMPTS: usize = 10;
const WINDOW_LOOKUP_RETRY_DELAY: Duration = Duration::from_millis(50);

pub fn find_game_window() -> Result<WindowTarget> {
    let mut last_error = None;
    for attempt in 0..WINDOW_LOOKUP_ATTEMPTS {
        match find_game_window_once() {
            Ok(window) => return Ok(window),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < WINDOW_LOOKUP_ATTEMPTS {
            sleep(WINDOW_LOOKUP_RETRY_DELAY);
        }
    }
    Err(last_error.expect("window lookup loop always runs at least once"))
}

pub fn find_game_window_once() -> Result<WindowTarget> {
    let target = WindowTarget::foreground().context("failed to inspect the foreground window")?;
    let title = target.title();
    if title != GAME_WINDOW_TITLE {
        bail!("Helldivers is not the foreground window; title={title:?}");
    }
    Ok(target)
}
