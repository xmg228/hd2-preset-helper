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

use crate::window::WindowTarget;

const GAME_WINDOW_TITLE: &str = "HELLDIVERS™ 2";
const WINDOW_LOOKUP_ATTEMPTS: usize = 10;
const WINDOW_LOOKUP_RETRY_DELAY: Duration = Duration::from_millis(50);

pub fn ensure_automation_target(target: WindowTarget) -> Result<()> {
    if !unsafe { IsWindow(Some(target.hwnd)).as_bool() } {
        bail!("Helldivers window is no longer available");
    }
    if unsafe { GetForegroundWindow() } != target.hwnd {
        bail!("Helldivers window lost focus; automation was cancelled");
    }
    if !is_game_window(target.hwnd) {
        bail!("foreground window is no longer Helldivers; automation was cancelled");
    }

    let current = game_window_from_hwnd(target.hwnd)?;
    if (
        current.client_x,
        current.client_y,
        current.client_w,
        current.client_h,
    ) != (
        target.client_x,
        target.client_y,
        target.client_w,
        target.client_h,
    ) {
        bail!("Helldivers window moved or resized; automation was cancelled");
    }
    Ok(())
}

pub fn find_game_window() -> Result<WindowTarget> {
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

pub fn find_game_window_once() -> Result<WindowTarget> {
    let hwnd = unsafe { GetForegroundWindow() };
    let title = window_title(hwnd);
    if !title_matches(&title) {
        bail!("Helldivers is not the foreground window; foreground={hwnd:?}, title={title:?}");
    }
    game_window_from_hwnd(hwnd)
}

fn game_window_from_hwnd(hwnd: HWND) -> Result<WindowTarget> {
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

    Ok(WindowTarget {
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
    title == GAME_WINDOW_TITLE
}

fn window_title(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    String::from_utf16_lossy(&buffer[..copied])
}
