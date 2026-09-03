use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetForegroundWindow, GetWindowTextW, IsWindow,
};

use super::ClientPoint;

#[derive(Debug, Clone)]
pub struct WindowTarget {
    hwnd: HWND,
    title: String,
    client_x: i32,
    client_y: i32,
    client_w: u32,
    client_h: u32,
    frame_x: i32,
    frame_y: i32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ClientCrop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl WindowTarget {
    pub(crate) fn foreground() -> Result<Self> {
        Self::from_hwnd(unsafe { GetForegroundWindow() })
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn ensure_input_target(&self) -> Result<()> {
        if !unsafe { IsWindow(Some(self.hwnd)).as_bool() } {
            bail!("target window is no longer available");
        }
        if unsafe { GetForegroundWindow() } != self.hwnd {
            bail!("target window lost focus; automation was cancelled");
        }

        let current = Self::from_hwnd(self.hwnd)?;
        if current.title != self.title {
            bail!("target window identity changed; automation was cancelled");
        }
        if current.client_geometry() != self.client_geometry() {
            bail!("target window moved or resized; automation was cancelled");
        }
        Ok(())
    }

    pub(crate) fn native_handle(&self) -> HWND {
        self.hwnd
    }

    pub(crate) fn client_origin(&self) -> (i32, i32) {
        (self.client_x, self.client_y)
    }

    pub(crate) fn client_size(&self) -> (u32, u32) {
        (self.client_w, self.client_h)
    }

    pub(crate) fn client_point_to_screen(&self, point: ClientPoint) -> (i32, i32) {
        (
            self.client_x + point.x as i32,
            self.client_y + point.y as i32,
        )
    }

    pub(crate) fn client_crop_in_frame(&self) -> Result<ClientCrop> {
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

    fn from_hwnd(hwnd: HWND) -> Result<Self> {
        let mut client_rect = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client_rect) }
            .map_err(|error| anyhow::anyhow!("failed to get window client rect: {error}"))?;

        let client_w = client_rect.right - client_rect.left;
        let client_h = client_rect.bottom - client_rect.top;
        if client_w <= 0 || client_h <= 0 {
            bail!("window client rect is empty: {client_w}x{client_h}");
        }

        let mut origin = POINT { x: 0, y: 0 };
        if !unsafe { ClientToScreen(hwnd, &mut origin).as_bool() } {
            bail!("failed to map window client origin to screen");
        }

        let (frame_x, frame_y) =
            dwm_frame_origin(hwnd).context("failed to get window DWM extended frame bounds")?;

        Ok(Self {
            hwnd,
            title: window_title(hwnd),
            client_x: origin.x,
            client_y: origin.y,
            client_w: client_w as u32,
            client_h: client_h as u32,
            frame_x,
            frame_y,
        })
    }

    fn client_geometry(&self) -> (i32, i32, u32, u32) {
        (self.client_x, self.client_y, self.client_w, self.client_h)
    }
}

fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}

fn dwm_frame_origin(hwnd: HWND) -> Result<(i32, i32)> {
    let mut rect = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast::<c_void>(),
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .context("DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS) failed")?;

    if rect.right <= rect.left || rect.bottom <= rect.top {
        bail!(
            "window bounds are empty: left={} top={} right={} bottom={}",
            rect.left,
            rect.top,
            rect.right,
            rect.bottom
        );
    }
    Ok((rect.left, rect.top))
}
