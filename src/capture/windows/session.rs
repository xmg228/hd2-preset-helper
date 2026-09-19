use std::time::Instant;

use anyhow::{Context, Result};
use tracing::debug;

use crate::window::WindowTarget;

use crate::capture::CaptureSource;

pub struct CaptureSessionManager {
    cached: Option<CaptureSource>,
}

impl CaptureSessionManager {
    pub fn new() -> Self {
        Self { cached: None }
    }

    /// WGC starts producing frames as soon as the session is created.
    pub fn prepare(&mut self, target: &WindowTarget) -> Result<()> {
        self.acquire(target)?;
        Ok(())
    }

    pub fn cancel_prepare(&mut self) {
        self.close();
    }

    pub fn finish_action(&mut self) {
        self.close();
    }

    fn close(&mut self) {
        if self.cached.take().is_some() {
            debug!("capture session discarded");
        }
    }

    pub fn active_capture(&mut self) -> Option<&mut CaptureSource> {
        self.cached.as_mut()
    }

    pub fn acquire(&mut self, target: &WindowTarget) -> Result<&mut CaptureSource> {
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
