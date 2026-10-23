#[cfg(target_os = "windows")]
mod windows;

use anyhow::Result;
use image::RgbaImage;
use std::{ops::Deref, sync::Arc, time::Instant};

use crate::game_settings::GameColorSettings;
use crate::image_rect::ImageRect;
use crate::window::{ClientPoint, WindowTarget};

// Preparation is optional for a backend. Cancellation and action completion
// are separate hints: the backend decides which resources to retain.
#[cfg(target_os = "windows")]
pub use windows::CaptureSessionManager;

#[cfg(target_os = "windows")]
use windows::{ColorNormalizer as PlatformColors, WindowsCapture as PlatformCapture};

pub struct CaptureSource {
    platform: PlatformCapture,
    next_frame: u64,
}

/// The exact normalized ROI delivered to recognition. Cloning shares pixels.
#[derive(Clone, Debug)]
pub struct CapturedFrame {
    pub id: u64,
    pub rect: ImageRect,
    pub output_size: (u32, u32),
    pub captured_at: Instant,
    image: Arc<RgbaImage>,
}

impl Deref for CapturedFrame {
    type Target = RgbaImage;

    fn deref(&self) -> &Self::Target {
        &self.image
    }
}

pub struct CaptureRegion<'a> {
    source: &'a mut CaptureSource,
    rect: ImageRect,
    colors: PlatformColors,
}

impl CaptureSource {
    fn new_for_window_target(target: &WindowTarget) -> Result<Self> {
        Ok(Self {
            platform: PlatformCapture::new(target)?,
            next_frame: 0,
        })
    }

    fn try_reuse_for_window_target(&mut self, target: &WindowTarget) -> bool {
        self.platform.try_reuse(target)
    }

    pub fn output_size(&self) -> (u32, u32) {
        self.platform.output_size()
    }

    pub fn region(
        &mut self,
        rect: ImageRect,
        settings: GameColorSettings,
    ) -> Result<CaptureRegion<'_>> {
        let colors = self.platform.colors(settings)?;
        Ok(CaptureRegion {
            source: self,
            rect,
            colors,
        })
    }
}

impl CaptureRegion<'_> {
    /// Return a fresh observation, not another read of the previously delivered
    /// frame. Pacing and a bounded wait for new content belong to the backend;
    /// unchanged pixels in a genuinely new frame are still a valid observation.
    /// Pixels are top-down RGBA8 with opaque alpha, in the recognizer's encoded
    /// UI color space after game brightness/HDR correction. Native pixel layout,
    /// transfer functions and compositor white levels stay in the backend.
    pub fn capture(&mut self) -> Result<CapturedFrame> {
        let image = self
            .source
            .platform
            .capture_region(self.rect, &self.colors)?;
        self.source.next_frame += 1;
        Ok(CapturedFrame {
            id: self.source.next_frame,
            rect: self.rect,
            output_size: self.source.output_size(),
            captured_at: Instant::now(),
            image: Arc::new(image),
        })
    }

    pub fn map_to_client(&self, local: (u32, u32)) -> ClientPoint {
        self.source.platform.map_to_client(self.rect, local)
    }
}
