use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::capture::{CaptureSource, DisplayWhiteLevel, query_sdr_white_level_for_window};
use crate::game_window::find_game_window_once;
use crate::window::WindowTarget;

pub struct CaptureSessionManager {
    cached: Option<CaptureSource>,
    last_display_white_level: Option<DisplayWhiteLevel>,
    white_level_unavailable_logged: bool,
}

impl CaptureSessionManager {
    pub fn new() -> Self {
        Self {
            cached: None,
            last_display_white_level: None,
            white_level_unavailable_logged: false,
        }
    }

    pub fn prewarm(&mut self) {
        if self.cached.is_some() {
            return;
        }

        let target = match find_game_window_once() {
            Ok(target) => target,
            Err(error) => {
                debug!(
                    error = %format!("{error:#}"),
                    "capture prewarm skipped: game window not ready"
                );
                return;
            }
        };

        let sdr_white_level = self.resolve_sdr_white_level(target);
        self.prewarm_now(target, sdr_white_level);
    }

    pub fn discard(&mut self) {
        if self.cached.take().is_some() {
            debug!("capture session discarded");
        }
    }

    pub fn get_or_create(&mut self, target: WindowTarget) -> Result<&mut CaptureSource> {
        let sdr_white_level = self.resolve_sdr_white_level(target);
        if self
            .cached
            .as_mut()
            .is_some_and(|capture| capture.try_reuse_for_window_target(target, sdr_white_level))
        {
            return Ok(self.cached.as_mut().expect("checked cached capture"));
        }

        if self.cached.is_some() {
            debug!("invalidating prewarmed capture session before action: incompatible window");
            self.cached = None;
        }

        let start = Instant::now();
        let capture = CaptureSource::new_for_window_target(target, sdr_white_level.unwrap_or(1000))
            .context("failed to create capture session on demand")?;
        self.cached = Some(capture);
        debug!(
            elapsed = ?start.elapsed(),
            "capture session created on demand"
        );

        Ok(self.cached.as_mut().expect("capture cache was just filled"))
    }

    fn resolve_sdr_white_level(&mut self, target: WindowTarget) -> Option<u32> {
        match query_sdr_white_level_for_window(target.hwnd) {
            Ok(state) => {
                if self.white_level_unavailable_logged
                    || self.last_display_white_level.as_ref() != Some(&state)
                {
                    info!(
                        display = %state.device,
                        advanced_color = state.advanced_color,
                        sdr_white_level = state.value,
                        scale = 1000.0 / state.value as f32,
                        "display white level resolved"
                    );
                }
                let value = state.value;
                self.last_display_white_level = Some(state);
                self.white_level_unavailable_logged = false;
                Some(value)
            }
            Err(error) => {
                if !self.white_level_unavailable_logged {
                    warn!(
                        error = %format!("{error:#}"),
                        "display white-level query unavailable; retaining the current LUT or using the default for a new capture"
                    );
                    self.white_level_unavailable_logged = true;
                }
                None
            }
        }
    }

    fn prewarm_now(&mut self, target: WindowTarget, sdr_white_level: Option<u32>) {
        if self.cached.is_some() {
            return;
        }

        let start = Instant::now();
        match CaptureSource::new_for_window_target(target, sdr_white_level.unwrap_or(1000)) {
            Ok(capture) => {
                self.cached = Some(capture);
                debug!(
                    elapsed = ?start.elapsed(),
                    "capture prewarm ready"
                );
            }
            Err(error) => {
                debug!(
                    error = %format!("{error:#}"),
                    "capture prewarm failed"
                );
            }
        }
    }
}
