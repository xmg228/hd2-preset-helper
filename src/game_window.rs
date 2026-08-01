use std::ffi::c_void;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetForegroundWindow, GetWindowTextW, IsWindow,
};

const GAME_TITLE_MATCH: &str = "helldivers";
const WINDOW_LOOKUP_ATTEMPTS: usize = 10;
const WINDOW_LOOKUP_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy)]
pub struct GameWindow {
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

impl GameWindow {
    pub fn client_crop_in_frame(&self) -> Result<ClientCrop> {
        let x = self.client_x - self.frame_x;
        let y = self.client_y - self.frame_y;
        if x < 0 || y < 0 {
            bail!(
                "game client origin is outside DWM frame: client=({},{}), frame=({},{})",
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

    pub fn ensure_automation_target(self) -> Result<()> {
        if !unsafe { IsWindow(Some(self.hwnd)).as_bool() } {
            bail!("Helldivers window is no longer available");
        }
        if unsafe { GetForegroundWindow() } != self.hwnd {
            bail!("Helldivers window lost focus; automation was cancelled");
        }
        if !is_game_window(self.hwnd) {
            bail!("foreground window is no longer Helldivers; automation was cancelled");
        }

        let current = game_window_from_hwnd(self.hwnd)?;
        if (
            current.client_x,
            current.client_y,
            current.client_w,
            current.client_h,
        ) != (self.client_x, self.client_y, self.client_w, self.client_h)
        {
            bail!("Helldivers window moved or resized; automation was cancelled");
        }
        Ok(())
    }
}

pub fn find_game_window() -> Result<GameWindow> {
    let mut last_error = None;
    for attempt in 0..WINDOW_LOOKUP_ATTEMPTS {
        match find_game_window_once() {
            Ok(window) => return Ok(window),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < WINDOW_LOOKUP_ATTEMPTS {
            sleep(WINDOW_LOOKUP_RETRY_DELAY);
        }
    }
    Err(last_error.expect("window lookup loop always runs at least once"))
}

pub fn find_game_window_once() -> Result<GameWindow> {
    let hwnd = unsafe { GetForegroundWindow() };
    let title = window_title(hwnd);
    if !title_matches(&title) {
        bail!("Helldivers is not the foreground window; foreground={hwnd:?}, title={title:?}");
    }
    game_window_from_hwnd(hwnd)
}

fn game_window_from_hwnd(hwnd: HWND) -> Result<GameWindow> {
    let mut client_rect = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client_rect) }
        .map_err(|error| anyhow::anyhow!("failed to get game client rect: {error}"))?;

    let client_w = client_rect.right - client_rect.left;
    let client_h = client_rect.bottom - client_rect.top;
    if client_w <= 0 || client_h <= 0 {
        bail!("game client rect is empty: {}x{}", client_w, client_h);
    }

    let mut origin = POINT { x: 0, y: 0 };
    if !unsafe { ClientToScreen(hwnd, &mut origin).as_bool() } {
        bail!("failed to map game client origin to screen");
    }

    let (frame_x, frame_y) =
        dwm_frame_origin(hwnd).context("failed to get game DWM extended frame bounds")?;

    Ok(GameWindow {
        hwnd,
        client_x: origin.x,
        client_y: origin.y,
        client_w: client_w as u32,
        client_h: client_h as u32,
        frame_x,
        frame_y,
    })
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

fn is_game_window(hwnd: HWND) -> bool {
    title_matches(&window_title(hwnd))
}

fn title_matches(title: &str) -> bool {
    title.to_lowercase().contains(GAME_TITLE_MATCH)
}

fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}
