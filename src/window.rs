/// Target-local input coordinates, independent of capture pixels and desktop
/// coordinates. Preserve fractional scaling until native input conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientPoint {
    pub x: f64,
    pub y: f64,
}

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::WindowTarget;

#[cfg(target_os = "windows")]
pub(crate) use windows::ClientCrop;
