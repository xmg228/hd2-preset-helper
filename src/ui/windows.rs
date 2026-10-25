//! Windows-specific window, display and font choices stay at the UI platform boundary.
use anyhow::{Context, Result, ensure};
use slint::winit_030::winit::{
    platform::windows::WindowAttributesExtWindows,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Window, WindowAttributes},
};
use windows::Win32::{
    Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, SetLastError, WIN32_ERROR, WPARAM},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GWL_EXSTYLE, GetWindowLongPtrW, STYLESTRUCT, SetWindowLongPtrW, WM_NCDESTROY,
            WM_STYLECHANGING, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        },
    },
};

pub(super) fn window_attributes(attributes: WindowAttributes) -> WindowAttributes {
    attributes.with_skip_taskbar(true)
}

pub(super) fn font_family(locale: &str) -> &'static str {
    match locale {
        "zh-Hans" => "Microsoft YaHei UI",
        "zh-Hant" => "Microsoft JhengHei UI",
        "ja" => "Yu Gothic UI",
        "ko" => "Malgun Gothic",
        _ => "Segoe UI",
    }
}

/// Install while hidden, before window managers observe the first show event.
pub(super) fn prepare_window(window: &Window, role: super::WindowRole) -> Result<()> {
    let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
        unreachable!("Windows UI has a Win32 handle");
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    let passive = matches!(role, super::WindowRole::Status);
    unsafe {
        // Reinstalling the same callback/ID only updates its reference data.
        ensure!(
            SetWindowSubclass(hwnd, Some(window_role_proc), 1, usize::from(passive)).as_bool(),
            "could not install the UI window role"
        );
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let updated = role_ex_style(style, passive);
        if updated != style {
            SetLastError(WIN32_ERROR(0));
            if SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated as isize) == 0 {
                GetLastError()
                    .ok()
                    .context("could not set the UI window role")?;
            }
        }
    }
    Ok(())
}

fn role_ex_style(style: u32, passive: bool) -> u32 {
    let style = (style | WS_EX_TOOLWINDOW.0) & !(WS_EX_APPWINDOW.0 | WS_EX_NOACTIVATE.0);
    style | if passive { WS_EX_NOACTIVATE.0 } else { 0 }
}

unsafe extern "system" fn window_role_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass_id: usize,
    passive: usize,
) -> LRESULT {
    if message == WM_STYLECHANGING && wparam.0 as i32 == GWL_EXSTYLE.0 {
        // Winit rewrites the whole extended style on visibility/hit-test changes.
        // WM_STYLECHANGING supplies a writable STYLESTRUCT for this message.
        let styles = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        styles.styleNew = role_ex_style(styles.styleNew, passive != 0);
    } else if message == WM_NCDESTROY {
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(window_role_proc), subclass_id) };
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub(super) fn monitor_bounds(id: &str) -> Result<(i32, i32, i32, i32)> {
    let bounds = crate::platform::monitors::resolve_bounds(Some(id), None)?;
    Ok((
        bounds.left,
        bounds.top,
        bounds.right - bounds.left,
        bounds.bottom - bounds.top,
    ))
}

pub(super) fn monitor_choices() -> Result<Vec<super::MonitorChoice>> {
    Ok(crate::platform::monitors::available()?
        .into_iter()
        .map(|monitor| super::MonitorChoice {
            id: monitor.id.into(),
            role: super::MonitorRole::Available,
            number: monitor.source.trim_start_matches(r"\\.\DISPLAY").into(),
            name: monitor.name.into(),
            primary: monitor.primary,
        })
        .collect())
}

pub(super) fn work_area(bounds: (i32, i32, i32, i32)) -> Result<(i32, i32, i32, i32)> {
    let point = POINT {
        x: bounds.0 + bounds.2 / 2,
        y: bounds.1 + bounds.3 / 2,
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST), &mut info) }
        .ok()?;
    let rect = info.rcWork;
    Ok((
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ))
}
