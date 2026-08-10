use anyhow::{Result, bail};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LUID, WIN32_ERROR};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
};
use windows::core::HRESULT;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayWhiteLevel {
    pub(crate) device: String,
    pub(crate) advanced_color: bool,
    pub(crate) value: u32,
}

pub(crate) fn query_sdr_white_level_for_window(hwnd: HWND) -> Result<DisplayWhiteLevel> {
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor.is_invalid() {
            bail!("failed to resolve the monitor containing the target window");
        }

        let mut monitor_info = MONITORINFOEXW::default();
        monitor_info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut monitor_info.monitorInfo).as_bool() {
            bail!("GetMonitorInfoW failed for the target window monitor");
        }
        let monitor_device = utf16_trim(&monitor_info.szDevice);

        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        let error =
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count);
        win32_error_to_result(error, "GetDisplayConfigBufferSizes failed")?;

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let error = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        );
        win32_error_to_result(error, "QueryDisplayConfig failed")?;
        paths.truncate(path_count as usize);

        for path in paths {
            let Ok(source_name) = query_source_name(path.sourceInfo.adapterId, path.sourceInfo.id)
            else {
                continue;
            };
            if !source_name.eq_ignore_ascii_case(&monitor_device) {
                continue;
            }

            let adapter_id = path.targetInfo.adapterId;
            let target_id = path.targetInfo.id;
            let advanced_color = query_advanced_color_enabled(adapter_id, target_id)?;
            let value = if advanced_color {
                query_sdr_white_level(adapter_id, target_id)?.max(1)
            } else {
                1000
            };

            return Ok(DisplayWhiteLevel {
                device: monitor_device,
                advanced_color,
                value,
            });
        }

        bail!("no active DisplayConfig path matches target window monitor {monitor_device:?}")
    }
}

unsafe fn query_source_name(adapter_id: LUID, source_id: u32) -> Result<String> {
    let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
    name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
    name.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
    name.header.adapterId = adapter_id;
    name.header.id = source_id;

    let error = unsafe { DisplayConfigGetDeviceInfo(&mut name.header) };
    win32_i32_to_result(error, "DisplayConfigGetDeviceInfo(GET_SOURCE_NAME) failed")?;
    Ok(utf16_trim(&name.viewGdiDeviceName))
}

unsafe fn query_sdr_white_level(adapter_id: LUID, target_id: u32) -> Result<u32> {
    let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();

    white.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
    white.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
    white.header.adapterId = adapter_id;
    white.header.id = target_id;

    let err_code = unsafe { DisplayConfigGetDeviceInfo(&mut white.header) };
    win32_i32_to_result(
        err_code,
        "DisplayConfigGetDeviceInfo(GET_SDR_WHITE_LEVEL) failed",
    )?;

    Ok(white.SDRWhiteLevel)
}

unsafe fn query_advanced_color_enabled(adapter_id: LUID, target_id: u32) -> Result<bool> {
    let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO::default();

    info.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
    info.header.size = std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32;
    info.header.adapterId = adapter_id;
    info.header.id = target_id;

    let err_code = unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
    win32_i32_to_result(
        err_code,
        "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO) failed",
    )?;

    let value = unsafe { info.Anonymous.value };
    Ok((value & 0x2) != 0)
}

fn win32_error_to_result(error: WIN32_ERROR, message: &'static str) -> Result<()> {
    if error == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{}: WIN32_ERROR({}) / HRESULT({:#010x})",
            message,
            error.0,
            HRESULT::from_win32(error.0).0 as u32,
        ))
    }
}

fn win32_i32_to_result(code: i32, message: &'static str) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        bail!(
            "{}: code={} / HRESULT({:#010x})",
            message,
            code,
            HRESULT::from_win32(code as u32).0 as u32,
        )
    }
}

fn utf16_trim(s: &[u16]) -> String {
    let len = s.iter().position(|c| *c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}
