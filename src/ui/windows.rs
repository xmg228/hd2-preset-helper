//! Windows-specific window, display and font choices stay at the UI platform boundary.
use std::cell::Cell;

use anyhow::{Context, Result, ensure};
use slint::winit_030::winit::{
    platform::windows::WindowAttributesExtWindows,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Window, WindowAttributes},
};
use windows::Win32::{
    Foundation::{
        GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, SetLastError, WIN32_ERROR, WPARAM,
    },
    Graphics::Gdi::{
        GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        MonitorFromWindow,
    },
    UI::{
        HiDpi::GetDpiForWindow,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GWL_EXSTYLE, GetClientRect, GetWindowLongPtrW, GetWindowRect, SPI_SETWORKAREA,
            STYLESTRUCT, SetWindowLongPtrW, WM_DISPLAYCHANGE, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE,
            WM_NCDESTROY, WM_SETTINGCHANGE, WM_SIZING, WM_STYLECHANGING, WMSZ_BOTTOM,
            WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT, WMSZ_LEFT, WMSZ_RIGHT, WMSZ_TOP, WMSZ_TOPLEFT,
            WMSZ_TOPRIGHT, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        },
    },
};

#[derive(Clone, Copy)]
pub(super) struct PanelChange {
    pub size: [Option<u32>; 2],
    pub position: [i32; 2],
}

#[derive(Clone, Copy)]
struct PanelInteraction {
    rect: RECT,
    size: [u32; 2],
    resize_edge: u32,
}

impl PanelInteraction {
    fn size_change(&self, current: [u32; 2]) -> [Option<u32>; 2] {
        let axes = match self.resize_edge {
            WMSZ_LEFT | WMSZ_RIGHT => [true, false],
            WMSZ_TOP | WMSZ_BOTTOM => [false, true],
            WMSZ_TOPLEFT | WMSZ_TOPRIGHT | WMSZ_BOTTOMLEFT | WMSZ_BOTTOMRIGHT => [true, true],
            _ => [false, false],
        };
        std::array::from_fn(|axis| {
            (axes[axis] && current[axis] != self.size[axis]).then_some(current[axis])
        })
    }
}

thread_local! {
    static PANEL_INTERACTION: Cell<Option<PanelInteraction>> = const { Cell::new(None) };
    static PANEL_CHANGE: Cell<Option<PanelChange>> = const { Cell::new(None) };
    static DISPLAY_CHANGED: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn panel_interacting() -> bool {
    PANEL_INTERACTION.with(|interaction| interaction.get().is_some())
}

// Synchronize OpenGL frames during live resize to reduce stutter.
// https://github.com/slint-ui/slint/issues/12730#issuecomment-5997314463
pub(super) fn install_resize_sync(window: &slint::Window) -> Result<()> {
    window.set_rendering_notifier(|state, api| {
        if let (
            slint::RenderingState::AfterRendering,
            slint::GraphicsAPI::NativeOpenGL { get_proc_address },
        ) = (state, api)
            && PANEL_INTERACTION.with(|interaction| {
                interaction
                    .get()
                    .is_some_and(|value| value.resize_edge != 0)
            })
        {
            let finish = get_proc_address(c"glFinish");
            if !finish.is_null() {
                // Slint keeps this context current until the frame is presented.
                let finish: unsafe extern "system" fn() = unsafe { std::mem::transmute(finish) };
                unsafe { finish() };
            }
        }
    })?;
    Ok(())
}

pub(super) fn take_panel_change() -> Option<PanelChange> {
    PANEL_CHANGE.with(Cell::take)
}

pub(super) fn take_display_change() -> bool {
    DISPLAY_CHANGED.with(Cell::take)
}

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
    if passive == 0
        && let Err(error) = track_panel_interaction(hwnd, message, wparam)
    {
        tracing::warn!(%error, "could not read adjusted panel placement");
    }
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

fn track_panel_interaction(hwnd: HWND, message: u32, wparam: WPARAM) -> Result<()> {
    match message {
        WM_DISPLAYCHANGE => DISPLAY_CHANGED.set(true),
        WM_SETTINGCHANGE if wparam.0 == SPI_SETWORKAREA.0 as usize => DISPLAY_CHANGED.set(true),
        WM_ENTERSIZEMOVE => {
            let mut rect = RECT::default();
            unsafe { GetWindowRect(hwnd, &mut rect)? };
            PANEL_INTERACTION.set(Some(PanelInteraction {
                rect,
                size: logical_client_size(hwnd)?.0,
                resize_edge: 0,
            }));
        }
        WM_SIZING => PANEL_INTERACTION.with(|interaction| {
            if let Some(mut value) = interaction.get() {
                value.resize_edge = wparam.0 as u32;
                interaction.set(Some(value));
            }
        }),
        WM_EXITSIZEMOVE => {
            if let Some(interaction) = PANEL_INTERACTION.with(Cell::take) {
                let mut rect = RECT::default();
                unsafe { GetWindowRect(hwnd, &mut rect)? };
                if rect != interaction.rect {
                    PANEL_CHANGE.set(Some(read_panel_change(hwnd, rect, interaction)?));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn logical_client_size(hwnd: HWND) -> Result<([u32; 2], f64)> {
    let mut client = RECT::default();
    let dpi = unsafe {
        GetClientRect(hwnd, &mut client)?;
        GetDpiForWindow(hwnd)
    };
    ensure!(dpi > 0, "could not read panel DPI");
    let scale = f64::from(dpi) / 96.0;
    Ok((
        [client.right - client.left, client.bottom - client.top]
            .map(|length| (f64::from(length) / scale).round() as u32),
        scale,
    ))
}

fn read_panel_change(hwnd: HWND, rect: RECT, initial: PanelInteraction) -> Result<PanelChange> {
    let (size, scale) = logical_client_size(hwnd)?;
    let (left, top, _, _) =
        monitor_work_area(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) })?;
    Ok(PanelChange {
        size: initial.size_change(size),
        position: [rect.left - left, rect.top - top]
            .map(|offset| (f64::from(offset) / scale).round() as i32),
    })
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
    monitor_work_area(unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) })
}

fn monitor_work_area(monitor: HMONITOR) -> Result<(i32, i32, i32, i32)> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }.ok()?;
    let rect = info.rcWork;
    Ok((
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ))
}
