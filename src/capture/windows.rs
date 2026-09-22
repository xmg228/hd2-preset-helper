mod color;
mod display;
mod session;
mod wgc;

pub use session::CaptureSessionManager;

use anyhow::{Context, Result};
use image::RgbaImage;
use tracing::{debug, info};

use crate::game_settings::GameColorSettings;
use crate::image_rect::ImageRect;
use crate::window::{ClientPoint, WindowTarget};
pub(super) use color::ColorNormalizer;
use display::DisplayColorInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapturePixelFormat {
    Bgra8,
    Rgba16Float,
}

impl CapturePixelFormat {
    fn for_display(display: DisplayColorInfo) -> Self {
        if display.hdr_active {
            Self::Rgba16Float
        } else {
            Self::Bgra8
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Bgra8 => "bgra8",
            Self::Rgba16Float => "rgba16f",
        }
    }
}

pub(super) struct WindowsCapture {
    backend: CaptureBackend,
    display_color_info: DisplayColorInfo,
    pixel_format: CapturePixelFormat,
}

enum CaptureBackend {
    Wgc(wgc::WgcCapture),
}

impl WindowsCapture {
    pub(super) fn new(target: &WindowTarget) -> Result<Self> {
        let display_color_info = display::query_color_info_for_window(target.native_handle())
            .context("failed to query target display color state")?;
        let pixel_format = CapturePixelFormat::for_display(display_color_info);
        debug!(
            windows_hdr = display_color_info.hdr_active,
            sdr_white_level = display_color_info.sdr_white_level,
            wgc_format = pixel_format.label(),
            "configured Windows capture color path"
        );
        Ok(Self {
            backend: CaptureBackend::Wgc(wgc::WgcCapture::new(target, pixel_format)?),
            display_color_info,
            pixel_format,
        })
    }

    pub(super) fn try_reuse(&mut self, target: &WindowTarget) -> bool {
        let display_color_info = resolve_display_color_info(target);
        if let Some(info) = display_color_info {
            let required_format = CapturePixelFormat::for_display(info);
            if required_format != self.pixel_format {
                info!(
                    old_format = self.pixel_format.label(),
                    new_format = required_format.label(),
                    windows_hdr = info.hdr_active,
                    "display color mode changed; rebuilding WGC capture session"
                );
                return false;
            }
        }

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
        ColorNormalizer::new(settings, self.display_color_info, self.pixel_format)
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
