use anyhow::{Result, bail};
use windows::Win32::Foundation::HWND;

#[derive(Debug, Clone, Copy)]
pub struct WindowTarget {
    pub hwnd: HWND,
    pub client_x: i32,
    pub client_y: i32,
    pub client_w: u32,
    pub client_h: u32,
    pub frame_x: i32,
    pub frame_y: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct ClientCrop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl WindowTarget {
    pub fn client_crop_in_frame(&self) -> Result<ClientCrop> {
        let x = self.client_x - self.frame_x;
        let y = self.client_y - self.frame_y;
        if x < 0 || y < 0 {
            bail!(
                "window client origin is outside DWM frame: client=({},{}), frame=({},{})",
                self.client_x,
                self.client_y,
                self.frame_x,
                self.frame_y
            );
        }

        Ok(ClientCrop {
            x: x as u32,
            y: y as u32,
            w: self.client_w,
            h: self.client_h,
        })
    }
}
