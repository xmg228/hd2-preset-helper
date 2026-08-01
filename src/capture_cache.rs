use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::capture::{
    CaptureRebuildSignature, CaptureSource, DisplayWhiteLevel, query_sdr_white_level_for_window,
};
use crate::game_window::{GameWindow, find_game_window_once};

const CAPTURE_PREWARM_CHECK_INTERVAL: Duration = Duration::from_millis(1000);
const CAPTURE_SIGNATURE_STABLE_TICKS: u32 = 2;

#[derive(Debug, Clone, Copy)]
struct PendingSignature {
    signature: CaptureRebuildSignature,
    seen_count: u32,
}

pub struct CaptureSessionCache {
    cached: Option<CaptureSource>,
    pending_signature: Option<PendingSignature>,
    last_check: Instant,
    window_not_ready_logged: bool,
    last_display_white_level: Option<DisplayWhiteLevel>,
    white_level_unavailable_logged: bool,
}

impl CaptureSessionCache {
    pub fn new() -> Self {
        Self {
            cached: None,
            pending_signature: None,
            last_check: Instant::now() - CAPTURE_PREWARM_CHECK_INTERVAL,
            window_not_ready_logged: false,
            last_display_white_level: None,
            white_level_unavailable_logged: false,
        }
    }

    pub fn tick_prewarm(&mut self) {
        if self.last_check.elapsed() < CAPTURE_PREWARM_CHECK_INTERVAL {
            return;
        }
        self.last_check = Instant::now();

        let game_window = match find_game_window_once() {
            Ok(game_window) => {
                if self.window_not_ready_logged {
                    debug!("WGC capture prewarm resumed: game window ready");
                    self.window_not_ready_logged = false;
                }
                game_window
            }
            Err(error) => {
                self.invalidate_dead_cached_window();
                if !self.window_not_ready_logged {
                    debug!(
                        error = %format!("{error:#}"),
                        "WGC capture prewarm paused: game window not ready"
                    );
                    self.window_not_ready_logged = true;
                }
                return;
            }
        };

        let sdr_white_level = self.resolve_sdr_white_level(game_window);
        if self
            .cached
            .as_mut()
            .is_some_and(|capture| capture.try_reuse_for_game_window(game_window, sdr_white_level))
        {
            self.pending_signature = None;
            return;
        }

        if self.cached.is_some() {
            let signature = match CaptureRebuildSignature::from_game_window(game_window) {
                Ok(signature) => signature,
                Err(error) => {
                    debug!(error = %format!("{error:#}"), "WGC capture prewarm skipped: failed to build window signature");
                    return;
                }
            };

            let seen_count = match self.pending_signature {
                Some(pending) if pending.signature == signature => pending.seen_count + 1,
                _ => 1,
            };
            self.pending_signature = Some(PendingSignature {
                signature,
                seen_count,
            });

            if seen_count < CAPTURE_SIGNATURE_STABLE_TICKS {
                debug!(
                    ?signature,
                    seen_count,
                    required = CAPTURE_SIGNATURE_STABLE_TICKS,
                    "WGC capture signature changed; waiting for stable window before rebuild"
                );
                return;
            }

            debug!(
                ?signature,
                seen_count, "invalidating prewarmed WGC capture session: window signature changed"
            );
            self.cached = None;
        }

        self.prewarm_now(game_window, sdr_white_level);
    }

    pub fn get_or_create(&mut self, game_window: GameWindow) -> Result<&mut CaptureSource> {
        let sdr_white_level = self.resolve_sdr_white_level(game_window);
        if self
            .cached
            .as_mut()
            .is_some_and(|capture| capture.try_reuse_for_game_window(game_window, sdr_white_level))
        {
            self.pending_signature = None;
            return Ok(self.cached.as_mut().expect("checked cached capture"));
        }

        if self.cached.is_some() {
            debug!("invalidating prewarmed capture session before action: incompatible window");
            self.cached = None;
            self.pending_signature = None;
        }

        let start = Instant::now();
        let capture =
            CaptureSource::new_for_game_window(game_window, sdr_white_level.unwrap_or(1000))
                .context("failed to create capture session on demand")?;
        self.cached = Some(capture);
        debug!(
            elapsed = ?start.elapsed(),
            "capture session created on demand"
        );

        Ok(self.cached.as_mut().expect("capture cache was just filled"))
    }

    fn resolve_sdr_white_level(&mut self, game_window: GameWindow) -> Option<u32> {
        match query_sdr_white_level_for_window(game_window.hwnd) {
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

    fn invalidate_dead_cached_window(&mut self) {
        let Some(capture) = self.cached.as_ref() else {
            return;
        };
        if capture.is_capture_window_alive() {
            return;
        }

        debug!("invalidating prewarmed WGC capture session: cached HWND is no longer alive");
        self.cached = None;
        self.pending_signature = None;
    }

    fn prewarm_now(&mut self, game_window: GameWindow, sdr_white_level: Option<u32>) {
        if self.cached.is_some() {
            return;
        }

        let start = Instant::now();
        match CaptureSource::new_for_game_window(game_window, sdr_white_level.unwrap_or(1000)) {
            Ok(capture) => {
                self.cached = Some(capture);
                self.pending_signature = None;
                debug!(
                    elapsed = ?start.elapsed(),
                    "WGC capture prewarm ready"
                );
            }
            Err(error) => {
                debug!(
                    error = %format!("{error:#}"),
                    "WGC capture prewarm failed"
                );
            }
        }
    }
}
