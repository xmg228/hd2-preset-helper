mod color;
mod display;
mod session;
mod wgc;

pub use session::CaptureSessionManager;

use anyhow::{Context, Result};
use image::RgbaImage;
use tracing::debug;

use crate::game_settings::GameColorSettings;
use crate::image_rect::ImageRect;
use crate::window::{ClientPoint, WindowTarget};
pub(super) use color::ColorNormalizer;
use display::DisplayColorInfo;

pub(super) struct WindowsCapture {
    backend: CaptureBackend,
    display_color_info: DisplayColorInfo,
}

enum CaptureBackend {
    Wgc(wgc::WgcCapture),
}

impl WindowsCapture {
    pub(super) fn new(target: &WindowTarget) -> Result<Self> {
        let display_color_info = display::query_color_info_for_window(target.native_handle())
            .context("failed to query target display color state")?;
        Ok(Self {
            backend: CaptureBackend::Wgc(wgc::WgcCapture::new(target)?),
            display_color_info,
        })
    }

    pub(super) fn try_reuse(&mut self, target: &WindowTarget) -> bool {
        let display_color_info = resolve_display_color_info(target);
        let reusable = match &mut self.backend {
            CaptureBackend::Wgc(capture) => capture.try_reuse(target),
        };
        if reusable && let Some(info) = display_color_info {
            self.display_color_info = info;
        }
        reusable
    }

    pub(super) fn output_size(&self) -> (u32, u32) {
        match &self.backend {
            CaptureBackend::Wgc(capture) => capture.output_size(),
        }
    }

    pub(super) fn map_to_client(&self, roi: ImageRect, local: (u32, u32)) -> ClientPoint {
        match &self.backend {
            CaptureBackend::Wgc(capture) => capture.map_to_client(roi, local),
        }
    }

    pub(super) fn colors(&self, settings: GameColorSettings) -> Result<ColorNormalizer> {
        ColorNormalizer::new(settings, self.display_color_info)
    }

    pub(super) fn capture_region(
        &mut self,
        client_roi: ImageRect,
        converter: &ColorNormalizer,
    ) -> Result<RgbaImage> {
        match &mut self.backend {
            CaptureBackend::Wgc(capture) => capture.capture_region(client_roi, converter),
        }
    }
}

fn resolve_display_color_info(target: &WindowTarget) -> Option<DisplayColorInfo> {
    match display::query_color_info_for_window(target.native_handle()) {
        Ok(info) => Some(info),
        Err(error) => {
            debug!(
                error = %format!("{error:#}"),
                "display color-state query unavailable; retaining the last verified state"
            );
            None
        }
    }
}
