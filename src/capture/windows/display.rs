use anyhow::{Result, bail};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LUID, WIN32_ERROR};
use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
};
use windows::core::{HRESULT, Interface};

#[derive(Debug, Clone, Copy)]
pub(super) struct DisplayColorInfo {
    pub hdr_active: bool,
    pub sdr_white_level: u32,
}

pub(super) fn query_color_info_for_window(hwnd: HWND) -> Result<DisplayColorInfo> {
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
        let hdr_active = query_hdr_active(monitor)?;
        if !hdr_active {
            return Ok(DisplayColorInfo {
                hdr_active: false,
                sdr_white_level: 1000,
            });
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
            return Ok(DisplayColorInfo {
                hdr_active: true,
                sdr_white_level: query_sdr_white_level(adapter_id, target_id)?.max(1),
            });
        }

        bail!("no active DisplayConfig path matches target window monitor {monitor_device:?}")
    }
}

unsafe fn query_hdr_active(monitor: HMONITOR) -> Result<bool> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut adapter_index = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        let mut output_index = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            if unsafe { output.GetDesc() }?.Monitor == monitor {
                let output: IDXGIOutput6 = output.cast()?;
                let color_space = unsafe { output.GetDesc1() }?.ColorSpace;
                return Ok(color_space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    bail!("no DXGI output matches the target window monitor")
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
