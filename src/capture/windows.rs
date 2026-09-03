mod display;
mod wgc;

use anyhow::Result;
use image::RgbaImage;
use tracing::debug;

use crate::image_rect::ImageRect;
use crate::window::WindowTarget;

pub(super) struct WindowsCapture {
    backend: CaptureBackend,
}

enum CaptureBackend {
    Wgc(wgc::WgcCapture),
}

impl WindowsCapture {
    pub(super) fn new(target: &WindowTarget) -> Result<Self> {
        let sdr_white_level = resolve_sdr_white_level(target).unwrap_or(1000);
        Ok(Self {
            backend: CaptureBackend::Wgc(wgc::WgcCapture::new(target, sdr_white_level)?),
        })
    }

    pub(super) fn try_reuse(&mut self, target: &WindowTarget) -> bool {
        let sdr_white_level = resolve_sdr_white_level(target);
        match &mut self.backend {
            CaptureBackend::Wgc(capture) => capture.try_reuse(target, sdr_white_level),
        }
    }

    pub(super) fn output_size(&self) -> (u32, u32) {
        match &self.backend {
            CaptureBackend::Wgc(capture) => capture.output_size(),
        }
    }

    pub(super) fn capture_region(&mut self, client_roi: ImageRect) -> Result<RgbaImage> {
        match &mut self.backend {
            CaptureBackend::Wgc(capture) => capture.capture_region(client_roi),
        }
    }
}

fn resolve_sdr_white_level(target: &WindowTarget) -> Option<u32> {
    match display::query_sdr_white_level_for_window(target.native_handle()) {
        Ok(value) => Some(value),
        Err(error) => {
            debug!(
                error = %format!("{error:#}"),
                "display white-level query unavailable"
            );
            None
        }
    }
}
