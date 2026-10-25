use std::ffi::c_void;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GetClientRect, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, SMTO_ABORTIFHUNG,
    SMTO_ERRORONEXIT, SW_RESTORE, SendMessageTimeoutW, SetForegroundWindow, ShowWindow, WM_NULL,
};
use windows::core::HSTRING;

use super::ClientPoint;

#[derive(Debug, Clone)]
pub struct WindowTarget {
    hwnd: HWND,
    process_id: u32,
    title: String,
    client_x: i32,
    client_y: i32,
    client_w: u32,
    client_h: u32,
    frame_x: i32,
    frame_y: i32,
}

/// Copyable identity only; geometry is refreshed before starting automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowIdentity {
    handle: isize,
    process_id: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ClientCrop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl WindowIdentity {
    /// Locate without reading geometry, which is refreshed after activation.
    pub(crate) fn find_by_title(title: &str) -> Result<Self> {
        // FindWindow does not set last-error when there is no matching window.
        let hwnd = unsafe { FindWindowW(None, &HSTRING::from(title)) }
            .map_err(|_| anyhow::anyhow!("window not found: {title:?}"))?;
        let mut process_id = 0;
        // Keep the same identity handling as foreground lookup. Failure to query
        // a thread ID does not itself mean that the matched window disappeared.
        if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process_id)) } == 0 {
            tracing::warn!(
                ?hwnd,
                process_id,
                valid = unsafe { IsWindow(Some(hwnd)).as_bool() },
                title = %window_title(hwnd),
                "matched window thread ID is unavailable"
            );
        }
        Ok(Self {
            handle: hwnd.0 as isize,
            process_id,
        })
    }

    pub(crate) fn activate(&self) -> Result<()> {
        anyhow::ensure!(
            self.request_activation()?,
            "Windows could not activate the target window; switch to the game manually and try again"
        );
        Ok(())
    }

    /// Request activation once. False means foreground is NULL and confirmation is pending.
    pub(crate) fn request_activation(&self) -> Result<bool> {
        let hwnd = self.checked_handle()?;
        let foreground_before = unsafe { GetForegroundWindow() };
        if foreground_before == hwnd {
            return Ok(true);
        }
        let started = Instant::now();
        let mut foreground_before_pid = 0;
        unsafe {
            GetWindowThreadProcessId(foreground_before, Some(&mut foreground_before_pid));
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
        }
        let set_foreground_succeeded = unsafe { SetForegroundWindow(hwnd).as_bool() };
        // Cross-process activation is asynchronous. Wait for the game's message
        // queue to process it before checking foreground ownership.
        let message_acknowledged = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_NULL,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT,
                1000,
                None,
            )
        }
        .0 != 0;
        let foreground_after = unsafe { GetForegroundWindow() };
        if !message_acknowledged || foreground_after != hwnd {
            let mut foreground_after_pid = 0;
            unsafe {
                GetWindowThreadProcessId(foreground_after, Some(&mut foreground_after_pid));
            }
            tracing::debug!(
                target = ?hwnd,
                target_pid = self.process_id,
                ?foreground_before,
                foreground_before_pid,
                ?foreground_after,
                foreground_after_pid,
                set_foreground_succeeded,
                message_acknowledged,
                elapsed_s = started.elapsed().as_secs_f64(),
                "target window activation result"
            );
        }
        anyhow::ensure!(
            message_acknowledged,
            "the target window did not acknowledge activation within 1 second, or could not be contacted"
        );
        anyhow::ensure!(
            foreground_after == hwnd || foreground_after.0.is_null(),
            "Windows could not activate the target window; switch to the game manually and try again"
        );
        Ok(foreground_after == hwnd)
    }

    /// Recheck without requesting activation again. False means foreground is still NULL.
    pub(crate) fn confirm_activation(&self) -> Result<bool> {
        let hwnd = self.checked_handle()?;
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.0.is_null() {
            return Ok(false);
        }
        if foreground == hwnd {
            return Ok(true);
        }
        let mut foreground_pid = 0;
        unsafe {
            GetWindowThreadProcessId(foreground, Some(&mut foreground_pid));
        }
        tracing::debug!(
            target = ?hwnd,
            target_pid = self.process_id,
            ?foreground,
            foreground_pid,
            "foreground changed while confirming window activation"
        );
        bail!(
            "another window became foreground while activating the target; automation was cancelled"
        )
    }

    fn checked_handle(&self) -> Result<HWND> {
        let hwnd = HWND(self.handle as *mut _);
        let mut process_id = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        }
        anyhow::ensure!(
            unsafe { IsWindow(Some(hwnd)).as_bool() } && process_id == self.process_id,
            "the target window is no longer available"
        );
        Ok(hwnd)
    }
}

impl WindowTarget {
    pub(crate) fn foreground() -> Result<Self> {
        Self::from_hwnd(unsafe { GetForegroundWindow() })
    }

    pub(crate) fn from_identity(identity: WindowIdentity) -> Result<Self> {
        let target = Self::from_hwnd(HWND(identity.handle as *mut _))?;
        anyhow::ensure!(
            target.identity() == identity,
            "the target window is no longer available"
        );
        Ok(target)
    }

    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn identity(&self) -> WindowIdentity {
        WindowIdentity {
            handle: self.hwnd.0 as isize,
            process_id: self.process_id,
        }
    }

    pub(crate) fn ensure_input_target(&self) -> Result<()> {
        if !unsafe { IsWindow(Some(self.hwnd)).as_bool() } {
            bail!("target window is no longer available");
        }
        let foreground = unsafe { GetForegroundWindow() };
        if foreground != self.hwnd {
            let foreground_null = foreground.0.is_null();
            let mut foreground_pid = 0;
            if !foreground_null {
                unsafe { GetWindowThreadProcessId(foreground, Some(&mut foreground_pid)) };
            }
            // Record only identity and style, not another application's title or path.
            tracing::debug!(
                target = ?self.hwnd,
                target_pid = self.process_id,
                ?foreground,
                foreground_pid,
                foreground_null,
                foreground_is_helper = foreground_pid == std::process::id(),
                foreground_ex_style = (!foreground_null)
                    .then(|| unsafe { GetWindowLongPtrW(foreground, GWL_EXSTYLE) }),
                "target window focus check failed"
            );
            bail!("target window lost focus; automation was cancelled");
        }

        let current = Self::from_hwnd(self.hwnd)?;
        if current.identity() != self.identity() || current.title != self.title {
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
            self.client_x + point.x.round() as i32,
            self.client_y + point.y.round() as i32,
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
        if hwnd.0.is_null() {
            bail!("no foreground window is available");
        }

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

        let mut process_id = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        }
        Ok(Self {
            hwnd,
            process_id,
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

pub(crate) fn foreground_title() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return String::new();
    }
    window_title(hwnd)
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
