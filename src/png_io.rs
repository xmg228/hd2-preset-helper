use std::io::Cursor;
use anyhow::{Context, Result, ensure};
use image::codecs::png::PngDecoder;
use image::{ColorType, ImageDecoder, RgbaImage};

pub fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage> {
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
