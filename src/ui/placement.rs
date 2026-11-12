//! Place UI windows within the selected display's work area.
use anyhow::Result;
use slint::winit_030::{
    WinitWindowAccessor,
    winit::dpi::{LogicalSize, PhysicalPosition},
};

#[cfg(target_os = "windows")]
use super::windows;
use crate::config::PanelPlacement;

pub(super) fn place(
    window: &slint::Window,
    monitor: Option<&str>,
    anchor: Option<(i32, i32)>,
    margin: f64,
    panel: Option<&PanelPlacement>,
) -> Result<Option<slint::LogicalSize>> {
    window
        .with_winit_window(|native| -> Result<Option<slint::LogicalSize>> {
            let screens: Vec<_> = native.available_monitors().collect();
            let screen = anchor
                .and_then(|(x, y)| {
                    screens.iter().find(|screen| {
                        let p = screen.position();
                        let s = screen.size();
                        x >= p.x
                            && y >= p.y
                            && x < p.x + s.width as i32
                            && y < p.y + s.height as i32
                    })
                })
                .cloned()
                .or_else(|| panel.and_then(|_| native.current_monitor()))
                .or_else(|| native.primary_monitor())
                .or_else(|| native.current_monitor());
            let Some(screen) = screen else {
                return Ok(None);
            };
            let p = screen.position();
            let s = screen.size();
            let mut bounds = (p.x, p.y, s.width as i32, s.height as i32);
            #[cfg(target_os = "windows")]
            if let Some(monitor) = monitor {
                bounds = windows::monitor_bounds(monitor)?;
            }
            let scale = screens
                .iter()
                .find(|s| {
                    let p = s.position();
                    p.x == bounds.0 && p.y == bounds.1
                })
                .map_or(screen.scale_factor(), |s| s.scale_factor());
            #[cfg(target_os = "windows")]
            let bounds = windows::work_area(bounds)?;
            let available = slint::LogicalSize::new(
                (bounds.2 as f64 / scale).max(1.0) as f32,
                (bounds.3 as f64 / scale).max(1.0) as f32,
            );
            let size = if let Some(panel) = panel {
                let default = PanelPlacement::default().size;
                let minimum = LogicalSize::new(
                    (default[0] as f64).min(available.width as f64),
                    (default[1] as f64).min(available.height as f64),
                );
                let maximum = LogicalSize::new(available.width as f64, available.height as f64);
                native.set_resizable(minimum != maximum);
                native.set_min_inner_size(Some(minimum));
                native.set_max_inner_size(Some(maximum));
                LogicalSize::new(
                    (panel.size[0].max(default[0]) as f64).min(maximum.width),
                    (panel.size[1].max(default[1]) as f64).min(maximum.height),
                )
            } else {
                native.outer_size().to_logical::<f64>(native.scale_factor())
            };
            let physical = slint::PhysicalSize::new(
                (size.width * scale).round() as u32,
                (size.height * scale).round() as u32,
            );
            if panel.is_some() {
                window.set_size(physical);
            }
            let margin = (margin * scale).round() as i32;
            let (x, y) = if let Some([x, y]) = panel.and_then(|panel| panel.position) {
                (
                    bounds.0
                        + (x as f64 * scale)
                            .round()
                            .clamp(0.0, (bounds.2 - physical.width as i32).max(0) as f64)
                            as i32,
                    bounds.1
                        + (y as f64 * scale)
                            .round()
                            .clamp(0.0, (bounds.3 - physical.height as i32).max(0) as f64)
                            as i32,
                )
            } else {
                (
                    bounds.0 + (bounds.2 - physical.width as i32 - margin).max(0),
                    bounds.1 + margin.min((bounds.3 - physical.height as i32).max(0)),
                )
            };
            native.set_outer_position(PhysicalPosition::new(x, y));
            Ok(panel.map(|_| available))
        })
        .transpose()
        .map(Option::flatten)
}
