use std::str::FromStr;

use anyhow::{Context, Result};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows::settings_path as platform_settings_path;

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct GameColorSettings {
    pub hdr_enabled: bool,
    pub screen_brightness: f32,
    pub ui_brightness: f32,
}

pub fn read_color_settings() -> Result<GameColorSettings> {
    let path = platform_settings_path()?;
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(GameColorSettings {
        hdr_enabled: parse_setting(&contents, "hdr_enabled")?,
        screen_brightness: parse_setting(&contents, "screen_brightness")?,
        ui_brightness: parse_setting(&contents, "ui_brightness")?,
    })
}

fn parse_setting<T>(contents: &str, name: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let value = contents
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == name).then(|| value.trim())
        })
        .with_context(|| format!("game setting {name} is missing"))?;
    value
        .parse()
        .with_context(|| format!("invalid game setting {name}: {value}"))
}
