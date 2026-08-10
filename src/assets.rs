use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};
use fast_image_resize as fir;
use fir::{ResizeAlg, ResizeOptions, Resizer};
use image::codecs::png::PngDecoder;
use image::{ColorType, ImageDecoder, RgbaImage};
use rust_embed::Embed;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::item::{ItemKind, StratagemCategory};

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

#[derive(Embed)]
#[folder = "assets/icons/"]
struct IconAssets;

pub fn default_icon_manifest() -> JsonAsset {
    JsonAsset {
        name: "assets/icons/manifest.json",
        bytes: include_bytes!("../assets/icons/manifest.json"),
    }
}

pub fn icon_image(path: &str) -> Result<RgbaImage> {
    let bytes = IconAssets::get(path)
        .map(|file| file.data)
        .with_context(|| format!("embedded icon {path} was not found"))?;
    decode_rgba8(&bytes).with_context(|| format!("failed to decode embedded icon {path}"))
}

fn decode_rgba8(bytes: &[u8]) -> Result<RgbaImage> {
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

pub fn resize_rgba_box(source: &RgbaImage, width: u32, height: u32) -> Result<RgbaImage> {
    if source.width() == width && source.height() == height {
        return Ok(source.clone());
    }

    let mut destination = RgbaImage::new(width, height);
    let options = ResizeOptions::new()
        .resize_alg(ResizeAlg::Convolution(fir::FilterType::Box))
        .use_alpha(true);
    Resizer::new()
        .resize(source, &mut destination, &options)
        .map_err(|error| anyhow!("failed to resize icon: {error}"))?;
    Ok(destination)
}

#[derive(Debug)]
pub struct IconEntry {
    pub display_name: Box<str>,
    pub kind: ItemKind,
    pub category: Option<StratagemCategory>,
    pub path: Box<str>,
}

#[derive(Debug)]
pub struct IconCatalog {
    by_id: HashMap<Box<str>, IconEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IconManifest {
    items: Vec<IconManifestItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IconManifestItem {
    item_id: String,
    display_name: String,
    kind: ItemKind,
    #[serde(default)]
    category: Option<StratagemCategory>,
    path: String,
}

impl IconCatalog {
    pub fn load(manifest: JsonAsset) -> Result<Self> {
        let manifest: IconManifest = parse_json_asset(manifest)?;
        let mut by_id = HashMap::with_capacity(manifest.items.len());
        let mut paths = HashSet::with_capacity(manifest.items.len());

        for item in manifest.items {
            validate_item_id(&item.item_id)?;
            ensure!(
                !item.display_name.trim().is_empty(),
                "icon {} has an empty display name",
                item.item_id
            );
            ensure!(
                !item.path.trim().is_empty(),
                "icon {} has an empty PNG path",
                item.item_id
            );

            match (item.kind, item.category) {
                (ItemKind::Stratagem, Some(_)) | (ItemKind::Booster, None) => {}
                (ItemKind::Stratagem, None) => {
                    bail!("stratagem {} has no category", item.item_id)
                }
                (ItemKind::Booster, Some(category)) => {
                    bail!(
                        "booster {} unexpectedly has category {}",
                        item.item_id,
                        category.label()
                    )
                }
            }

            ensure!(
                IconAssets::get(&item.path).is_some(),
                "embedded icon {} was not found",
                item.path
            );
            ensure!(
                paths.insert(item.path.clone()),
                "duplicate icon PNG path: {}",
                item.path
            );

            let item_id = item.item_id.into_boxed_str();
            let entry = IconEntry {
                display_name: item.display_name.into_boxed_str(),
                kind: item.kind,
                category: item.category,
                path: item.path.into_boxed_str(),
            };
            ensure!(
                by_id.insert(item_id.clone(), entry).is_none(),
                "duplicate icon item ID: {item_id}"
            );
        }

        ensure!(!by_id.is_empty(), "icon manifest contains no items");
        Ok(Self { by_id })
    }

    pub fn get(&self, item_id: &str) -> Option<&IconEntry> {
        self.by_id.get(item_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &IconEntry)> {
        self.by_id
            .iter()
            .map(|(item_id, entry)| (item_id.as_ref(), entry))
    }
}

fn validate_item_id(item_id: &str) -> Result<()> {
    ensure!(!item_id.is_empty(), "icon item ID must not be empty");
    ensure!(
        item_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
        "invalid icon item ID {item_id:?}"
    );
    ensure!(
        !item_id.starts_with('-') && !item_id.ends_with('-') && !item_id.contains("--"),
        "invalid icon item ID {item_id:?}"
    );
    Ok(())
}
