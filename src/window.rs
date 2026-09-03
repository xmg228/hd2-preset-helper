#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientPoint {
    pub x: u32,
    pub y: u32,
}

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::WindowTarget;

#[cfg(target_os = "windows")]
pub(crate) use windows::ClientCrop;
