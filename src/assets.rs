use std::fs;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use image::codecs::png::PngDecoder;
use image::{ColorType, ImageDecoder, RgbaImage};
use serde::de::DeserializeOwned;

#[derive(Clone, Copy)]
pub struct JsonAsset {
    name: &'static str,
    bytes: &'static [u8],
}

pub fn parse_json_asset<T: DeserializeOwned>(asset: JsonAsset) -> Result<T> {
    let bytes = asset
        .bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or(asset.bytes);
    serde_json::from_slice(bytes)
        .with_context(|| format!("failed to parse <embedded:{}>", asset.name))
}

pub fn parse_json_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn default_calibration() -> JsonAsset {
    JsonAsset {
        name: "data/calibration.json",
        bytes: include_bytes!("../data/calibration.json"),
    }
}

pub(crate) fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage> {
    let decoder = PngDecoder::new(Cursor::new(bytes)).context("failed to read PNG header")?;
    ensure!(
        decoder.color_type() == ColorType::Rgba8,
        "expected an RGBA8 PNG, got {:?}",
        decoder.color_type(),
    );

    let (width, height) = decoder.dimensions();
    let mut image = RgbaImage::new(width, height);
    decoder
        .read_image(image.as_mut())
        .context("failed to decode RGBA8 PNG")?;
    Ok(image)
}
