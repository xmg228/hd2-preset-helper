mod display;
mod wgc;

use anyhow::Result;
use image::RgbaImage;

use crate::image_rect::ImageRect;
use crate::window::WindowTarget;

pub(crate) use display::{DisplayWhiteLevel, query_sdr_white_level_for_window};

pub struct RoiFrame {
    pub image: RgbaImage,
    pub screen_x: i32,
    pub screen_y: i32,
}

pub struct CaptureSource {
    backend: CaptureBackend,
}

enum CaptureBackend {
    Wgc(wgc::WgcCapture),
}

impl CaptureSource {
    pub fn new_for_window_target(target: WindowTarget, sdr_white_level: u32) -> Result<Self> {
        Ok(Self {
            backend: CaptureBackend::Wgc(wgc::WgcCapture::new(target, sdr_white_level)?),
        })
    }

    pub fn try_reuse_for_window_target(
        &mut self,
        target: WindowTarget,
        sdr_white_level: Option<u32>,
    ) -> bool {
        match &mut self.backend {
            CaptureBackend::Wgc(capture) => capture.try_reuse(target, sdr_white_level),
        }
    }

    pub fn output_size(&self) -> (u32, u32) {
        match &self.backend {
            CaptureBackend::Wgc(capture) => capture.output_size(),
        }
    }

    pub fn sync_to_latest(&mut self) {
        match &mut self.backend {
            CaptureBackend::Wgc(capture) => capture.sync_to_latest(),
        }
    }

    pub fn capture_latest_region(&mut self, client_roi: ImageRect) -> Result<RoiFrame> {
        match &mut self.backend {
            CaptureBackend::Wgc(capture) => capture.capture_latest_region(client_roi),
        }
    }
}
