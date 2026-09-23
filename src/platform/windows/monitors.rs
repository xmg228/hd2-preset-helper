use anyhow::{Context, Result};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WIN32_ERROR};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFOEXW, MonitorFromPoint, MonitorFromWindow,
};
use windows::core::BOOL;

#[derive(PartialEq)]
pub struct Monitor {
    pub id: String,
    pub label: String,
    pub bounds: RECT,
}

pub fn available() -> Result<Vec<Monitor>> {
    let mut desktops = Vec::<MONITORINFOEXW>::new();
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut desktops as *mut _ as isize),
        )
        .ok()?;
    }

    let mut path_count = 0;
    let mut mode_count = 0;
    unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) }
        .ok()?;
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
    unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
    }
    .ok()?;

    let mut monitors = Vec::new();
    for path in &paths[..path_count as usize] {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = std::mem::size_of_val(&source) as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        WIN32_ERROR(unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } as u32).ok()?;
        let source_name = wide_string(&source.viewGdiDeviceName);
        let Some(desktop) = desktops
            .iter()
            .find(|info| wide_string(&info.szDevice).eq_ignore_ascii_case(&source_name))
        else {
            continue;
        };

        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        target.header.size = std::mem::size_of_val(&target) as u32;
        target.header.adapterId = path.targetInfo.adapterId;
        target.header.id = path.targetInfo.id;
        WIN32_ERROR(unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } as u32).ok()?;
        let id = wide_string(&target.monitorDevicePath);
        if id.is_empty() {
            continue;
        }
        let name = wide_string(&target.monitorFriendlyDeviceName);
        let mut label = source_name.replace(r"\\.\DISPLAY", "Display ");
        if !name.is_empty() {
            label.push_str(&format!(" — {name}"));
        }
        if desktop.monitorInfo.dwFlags & 1 != 0 {
            label.push_str(" (Primary)");
        }
        monitors.push(Monitor {
            id,
            label,
            bounds: desktop.monitorInfo.rcMonitor,
        });
    }
    monitors.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(monitors)
}

/// Resolve a saved device interface path afresh; never replace the saved choice on fallback.
pub fn resolve_bounds(id: Option<&str>, fallback_window: Option<HWND>) -> Result<RECT> {
    if let Some(id) = id {
        match available() {
            Ok(monitors) => {
                if let Some(monitor) = monitors
                    .iter()
                    .find(|monitor| monitor.id.eq_ignore_ascii_case(id))
                {
                    return Ok(monitor.bounds);
                }
            }
            Err(error) => tracing::debug!(%error, "monitor lookup unavailable; using fallback"),
        }
    }
    let monitor = unsafe {
        fallback_window.map_or_else(
            || MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY),
            |hwnd| MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
        )
    };
    Ok(monitor_info(monitor)?.monitorInfo.rcMonitor)
}

fn monitor_info(monitor: HMONITOR) -> Result<MONITORINFOEXW> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of_val(&info) as u32;
    unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }
        .ok()
        .context("failed to query monitor bounds")?;
    Ok(info)
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    if let Ok(info) = monitor_info(monitor) {
        let monitors = unsafe { &mut *(data.0 as *mut Vec<MONITORINFOEXW>) };
        monitors.push(info);
    }
    BOOL(1)
}

fn wide_string(value: &[u16]) -> String {
    let end = value.iter().position(|c| *c == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}
