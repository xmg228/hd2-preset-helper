use std::time::Instant;

use anyhow::{Context, Result};
use tracing::debug;

use crate::window::WindowTarget;

use super::CaptureSource;

pub struct CaptureSessionManager {
    cached: Option<CaptureSource>,
}

impl CaptureSessionManager {
    pub fn new() -> Self {
        Self { cached: None }
    }

    pub fn discard(&mut self) {
        if self.cached.take().is_some() {
            debug!("capture session discarded");
        }
    }

    pub fn active_capture(&mut self) -> Option<&mut CaptureSource> {
        self.cached.as_mut()
    }

    pub fn get_or_create(&mut self, target: &WindowTarget) -> Result<&mut CaptureSource> {
        if let Some(mut capture) = self.cached.take() {
            if capture.try_reuse_for_window_target(target) {
                return Ok(self.cached.insert(capture));
            }
            debug!("discarding incompatible cached capture session");
        }

        let start = Instant::now();
        let capture = CaptureSource::new_for_window_target(target)
            .context("failed to create capture session")?;
        debug!(
            elapsed = ?start.elapsed(),
            "capture session created"
        );

        Ok(self.cached.insert(capture))
    }
}
