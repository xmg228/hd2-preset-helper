mod session;

#[cfg(target_os = "windows")]
mod windows;

use anyhow::Result;
use image::RgbaImage;

use crate::image_rect::ImageRect;
use crate::window::{ClientPoint, WindowTarget};

pub use session::CaptureSessionManager;

#[cfg(target_os = "windows")]
use windows::WindowsCapture as PlatformCapture;

pub struct CaptureSource {
    platform: PlatformCapture,
}

pub struct CaptureRegion<'a> {
    source: &'a mut CaptureSource,
    rect: ImageRect,
}

impl CaptureSource {
    pub fn new_for_window_target(target: &WindowTarget) -> Result<Self> {
        Ok(Self {
            platform: PlatformCapture::new(target)?,
        })
    }

    pub fn try_reuse_for_window_target(&mut self, target: &WindowTarget) -> bool {
        self.platform.try_reuse(target)
    }

    pub fn output_size(&self) -> (u32, u32) {
        self.platform.output_size()
    }

    fn capture_region(&mut self, client_roi: ImageRect) -> Result<RgbaImage> {
        self.platform.capture_region(client_roi)
    }

    pub fn region(&mut self, rect: ImageRect) -> CaptureRegion<'_> {
        CaptureRegion { source: self, rect }
    }
}

impl CaptureRegion<'_> {
    pub fn capture(&mut self) -> Result<RgbaImage> {
        self.source.capture_region(self.rect)
    }

    pub fn map_to_client(&self, local: (u32, u32)) -> ClientPoint {
        ClientPoint {
            x: self.rect.x + local.0,
            y: self.rect.y + local.1,
        }
    }
}
