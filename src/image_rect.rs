use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct ImageRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}
