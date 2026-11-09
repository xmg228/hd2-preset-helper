//! Native PNG samples and logical list orders in one atomically published file.
use std::{fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageEncoder, ImageFormat, RgbaImage, codecs::png::PngEncoder};
use serde::{Deserialize, Serialize};

use super::{
    CachedIcon, EquipmentCatalog, EquipmentCategory, IconCatalog, IconPosition, ItemCache,
};
use crate::{
    item::ItemKind,
    vision::{
        IconSample, ImageSample, SampleGeometry,
        equipment::EquipmentText,
        text::{TextGeometry, TextSample},
    },
};

const VERSION: u32 = 2;
const ICON_ROW_TOLERANCE: f32 = 0.1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    equipment: [Option<Catalog>; 6],
    stratagems: Option<Vec<Icon>>,
    boosters: Option<Vec<Icon>>,
}

#[derive(Serialize, Deserialize)]
struct Catalog {
    client_size: (u32, u32),
    categories: Vec<Category>,
}

#[derive(Serialize, Deserialize)]
struct Text {
    image: String,
    geometry: TextGeometry,
}

#[derive(Serialize, Deserialize)]
struct Category {
    title: Text,
    names: Vec<Text>,
    order: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
struct Icon {
    image: String,
    geometry: SampleGeometry,
    #[serde(default)]
    position: Option<IconPosition>,
}

impl ItemCache {
    pub fn load(path: &Path) -> Self {
        let result = (|| -> Result<Self> {
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
                Err(error) => return Err(error).context("failed to read item cache"),
            };
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).context("failed to parse item cache")?;
            if value["version"].as_u64() != Some(VERSION as u64) {
                tracing::debug!("item cache format changed; samples will be learned again");
                return Ok(Self::default());
            }
            let file: File = serde_json::from_value(value).context("invalid item cache")?;
            let mut cache = Self::default();
            for (saved, catalog) in file.equipment.into_iter().zip(&mut cache.equipment) {
                *catalog = saved.map(decode_catalog).transpose()?;
            }
            cache.stratagems = decode_catalog_icons(ItemKind::Stratagem, file.stratagems)?;
            cache.boosters = decode_catalog_icons(ItemKind::Booster, file.boosters)?;
            tracing::debug!(
                categories = cache
                    .equipment
                    .iter()
                    .flatten()
                    .map(|catalog| catalog.categories.len())
                    .sum::<usize>(),
                "item cache loaded"
            );
            Ok(cache)
        })();
        result.unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "item cache could not be loaded");
            Self::default()
        })
    }

    pub fn save_if_changed(&mut self, path: &Path) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut equipment: [Option<Catalog>; 6] = std::array::from_fn(|_| None);
        for (catalog, saved) in self.equipment.iter().zip(&mut equipment) {
            *saved = catalog.as_ref().map(encode_catalog).transpose()?;
        }
        let file = File {
            version: VERSION,
            equipment,
            stratagems: self
                .stratagems
                .as_ref()
                .map(encode_catalog_icons)
                .transpose()?,
            boosters: self
                .boosters
                .as_ref()
                .map(encode_catalog_icons)
                .transpose()?,
        };
        crate::persistence::write(path, &serde_json::to_vec(&file)?)?;
        self.dirty = false;
        tracing::debug!("item cache saved");
        Ok(())
    }
}

fn encode_catalog(catalog: &EquipmentCatalog) -> Result<Catalog> {
    Ok(Catalog {
        client_size: catalog.client_size,
        categories: catalog
            .categories
            .iter()
            .map(|category| {
                Ok(Category {
                    title: encode_text(&category.title)?,
                    names: category
                        .names
                        .iter()
                        .map(encode_text)
                        .collect::<Result<_>>()?,
                    order: category.order.clone(),
                })
            })
            .collect::<Result<_>>()?,
    })
}

fn decode_catalog(catalog: Catalog) -> Result<EquipmentCatalog> {
    ensure!(
        catalog.client_size.0 > 0 && catalog.client_size.1 > 0,
        "invalid item-cache client dimensions"
    );
    ensure!(
        !catalog.categories.is_empty(),
        "empty item-cache equipment catalog"
    );
    let categories = catalog
        .categories
        .into_iter()
        .map(|category| {
            ensure!(
                !category.order.is_empty()
                    && category
                        .order
                        .iter()
                        .all(|&index| index < category.names.len()),
                "invalid item-cache equipment order"
            );
            Ok(EquipmentCategory {
                title: decode_text(category.title, EquipmentText::Category)?,
                names: category
                    .names
                    .into_iter()
                    .map(|sample| decode_text(sample, EquipmentText::Name))
                    .collect::<Result<_>>()?,
                order: category.order,
            })
        })
        .collect::<Result<_>>()?;
    Ok(EquipmentCatalog {
        client_size: catalog.client_size,
        categories,
    })
}

fn encode_image(image: &RgbaImage) -> Result<String> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes).write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(STANDARD.encode(bytes))
}

fn decode_image(png: &str) -> Result<RgbaImage> {
    let image = image::load_from_memory_with_format(&STANDARD.decode(png)?, ImageFormat::Png)?;
    match image {
        DynamicImage::ImageRgba8(image) => Ok(image),
        _ => anyhow::bail!("cached image is not native RGBA8"),
    }
}

fn encode_text(sample: &TextSample) -> Result<Text> {
    Ok(Text {
        image: encode_image(&sample.image)?,
        geometry: sample.geometry,
    })
}

fn decode_text(sample: Text, field: EquipmentText) -> Result<TextSample> {
    let sample = TextSample::from_image(decode_image(&sample.image)?, sample.geometry)?;
    let sample = match field {
        EquipmentText::Name => sample.retain_left_group(),
        EquipmentText::Category => sample,
    };
    ensure!(sample.has_text(), "empty cached text foreground");
    Ok(sample)
}

fn encode_catalog_icons(catalog: &IconCatalog) -> Result<Vec<Icon>> {
    catalog.items.iter().map(encode_icon).collect()
}

fn decode_catalog_icons(kind: ItemKind, items: Option<Vec<Icon>>) -> Result<Option<IconCatalog>> {
    let Some(items) = items else {
        return Ok(None);
    };
    let Some(positions) = items
        .iter()
        .map(|item| item.position)
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(None);
    };
    ensure!(
        positions
            .iter()
            .all(|position| position.col < 4 && position.y.is_finite() && position.y >= 0.0),
        "invalid cached icon position"
    );
    ensure!(
        positions.windows(2).all(|pair| {
            let dy = pair[1].y - pair[0].y;
            dy >= -ICON_ROW_TOLERANCE
                && (dy.abs() > ICON_ROW_TOLERANCE || pair[1].col > pair[0].col)
        }),
        "invalid cached icon order"
    );
    let items = items
        .into_iter()
        .zip(positions)
        .map(|(item, position)| {
            Ok(CachedIcon {
                icon: decode_icon(kind, item)?,
                position,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Some(IconCatalog { items }))
}

fn encode_icon(sample: &CachedIcon) -> Result<Icon> {
    Ok(Icon {
        image: encode_image(&sample.icon.sample.image)?,
        geometry: sample.icon.sample.geometry,
        position: Some(sample.position),
    })
}

fn decode_icon(kind: ItemKind, sample: Icon) -> Result<IconSample> {
    let image = decode_image(&sample.image)?;
    let (width, height) = image.dimensions();
    ensure!(
        width > 0
            && height > 0
            && sample.geometry.physical_size.is_finite()
            && sample.geometry.physical_size > 0.0
            && sample.geometry.center_x.is_finite()
            && sample.geometry.center_y.is_finite()
            && sample.geometry.center_x >= 0.0
            && sample.geometry.center_x <= width as f32
            && sample.geometry.center_y >= 0.0
            && sample.geometry.center_y <= height as f32,
        "invalid cached icon geometry"
    );
    IconSample::extract(
        kind,
        ImageSample {
            image,
            geometry: sample.geometry,
        },
    )
}
