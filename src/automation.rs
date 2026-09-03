use anyhow::Result;
use image::RgbaImage;

use crate::capture::CaptureRegion;
use crate::input::{InputSession, Key};
use crate::window::WindowTarget;

pub struct AutomationSession<'a> {
    region: CaptureRegion<'a>,
    input: InputSession,
}

impl<'a> AutomationSession<'a> {
    pub fn new(region: CaptureRegion<'a>, target: WindowTarget) -> Result<Self> {
        Ok(Self {
            region,
            input: InputSession::new(target)?,
        })
    }

    pub fn capture(&mut self) -> Result<RgbaImage> {
        self.region.capture()
    }

    pub fn move_cursor(&mut self, point: (u32, u32)) -> Result<()> {
        self.input.move_cursor(self.region.map_to_client(point))
    }

    pub fn click(&mut self, point: (u32, u32), hold_ms: u64) -> Result<()> {
        self.input.click(self.region.map_to_client(point), hold_ms)
    }

    pub fn click_current(&mut self, hold_ms: u64) -> Result<()> {
        self.input.click_current(hold_ms)
    }

    pub fn scroll(&mut self, delta: i32) -> Result<()> {
        self.input.scroll(delta)
    }

    pub fn tap_key(&mut self, key: Key, hold_ms: u64) -> Result<()> {
        self.input.tap_key(key, hold_ms)
    }
}
