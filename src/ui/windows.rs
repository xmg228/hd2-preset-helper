//! Windows-specific window, display and font choices stay at the UI platform boundary.
use anyhow::Result;
use slint::winit_030::winit::{
    platform::windows::WindowAttributesExtWindows,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Window, WindowAttributes},
};
use windows::Win32::{
    Foundation::{HWND, POINT},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_NOACTIVATE,
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

pub(super) fn make_passive(window: &Window) -> Result<()> {
    let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
        unreachable!("Windows UI has a Win32 handle");
    };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE.0 as isize);
    }
    Ok(())
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
