//! Complete orders and native PNG text crops with their sampling geometry.
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, ErrorKind},
    path::Path,
};

use super::{Category, EquipmentCache};
use crate::vision::{
    equipment::EquipmentText,
    text::{TextGeometry, TextSample},
};

const VERSION: u32 = 4;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    scales: BTreeMap<String, [Vec<SavedCategory>; 6]>,
}

#[derive(Serialize, Deserialize)]
struct SavedCategory {
    title: SavedText,
    names: Vec<SavedText>,
    order: Vec<usize>,
    previous: Option<usize>,
    next: Option<usize>,
    circular: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedText {
    image: String,
    geometry: TextGeometry,
}

impl EquipmentCache {
    pub(crate) fn load(path: &Path) -> Self {
        let result = (|| -> Result<Self> {
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
                Err(error) => return Err(error).context("failed to read equipment cache"),
            };
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).context("failed to parse equipment cache")?;
            let saved_version = value["version"].as_u64();
            if saved_version != Some(VERSION as u64) {
                tracing::info!(
                    saved_version,
                    expected_version = VERSION,
                    "equipment cache format changed; categories will be learned again"
                );
                return Ok(Self::default());
            }
            let file: File = serde_json::from_value(value).context("invalid equipment cache")?;
            let mut cache = Self::default();
            for (scale, saved_kinds) in file.scales {
                let kinds = cache.scales.entry(scale).or_default();
                for (saved, categories) in saved_kinds.into_iter().zip(kinds) {
                    let count = saved.len();
                    for entry in saved {
                        ensure!(
                            entry.order.iter().all(|&p| p < entry.names.len())
                                && entry.previous.is_none_or(|p| p < count)
                                && entry.next.is_none_or(|p| p < count),
                            "invalid equipment cache references"
                        );
                        categories.push(Category {
                            title: decode(&entry.title, EquipmentText::Category)
                                .context("failed to decode equipment cache category heading")?,
                            names: entry
                                .names
                                .iter()
                                .map(|png| {
                                    decode(png, EquipmentText::Name)
                                        .context("failed to decode equipment cache item name")
                                })
                                .collect::<Result<_>>()?,
                            order: entry.order,
                            previous: entry.previous,
                            next: entry.next,
                            circular: entry.circular,
                        });
                    }
                }
            }
            tracing::info!(
                version = VERSION,
                scales = cache.scales.len(),
                categories = cache
                    .scales
                    .values()
                    .flatten()
                    .flatten()
                    .filter(|c| !c.order.is_empty())
                    .count(),
                "equipment cache loaded"
            );
            Ok(cache)
        })();
        result.unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), error = %format!("{error:#}"), "equipment cache ignored; categories will be learned again");
            Self::default()
        })
    }

    /// Called after an operation, never from the capture/input loop.
    pub(crate) fn save_if_changed(&mut self, path: &Path) {
        if !self.dirty {
            return;
        }
        let result = (|| -> Result<()> {
            let mut file = File {
                version: VERSION,
                scales: BTreeMap::new(),
            };
            for (scale, kinds) in &self.scales {
                let saved_kinds = file.scales.entry(scale.clone()).or_default();
                for (categories, saved) in kinds.iter().zip(saved_kinds) {
                    for category in categories {
                        saved.push(SavedCategory {
                            title: encode(&category.title)?,
                            names: category.names.iter().map(encode).collect::<Result<_>>()?,
                            order: category.order.clone(),
                            previous: category.previous,
                            next: category.next,
                            circular: category.circular,
                        });
                    }
                }
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).context("failed to create equipment cache directory")?;
            }
            let temporary = path.with_extension("tmp");
            fs::write(
                &temporary,
                serde_json::to_vec(&file).context("failed to serialize equipment cache")?,
            )
            .with_context(|| format!("failed to write {}", temporary.display()))?;
            fs::rename(temporary, path).context("failed to replace equipment cache")?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.dirty = false;
                tracing::debug!("equipment cache saved");
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), error = %format!("{error:#}"), "equipment cache could not be saved")
            }
        }
    }
}

fn encode(sample: &TextSample) -> Result<SavedText> {
    let mut bytes = Cursor::new(Vec::new());
    sample
        .image
        .write_to(&mut bytes, ImageFormat::Png)
        .context("failed to encode equipment cache image")?;
    Ok(SavedText {
        image: STANDARD.encode(bytes.into_inner()),
        geometry: sample.geometry,
    })
}

fn decode(saved: &SavedText, field: EquipmentText) -> Result<TextSample> {
    let bytes = STANDARD.decode(&saved.image)?;
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)?.into_rgba8();
    let sample = TextSample::from_image(image, saved.geometry)?;
    let sample = match field {
        EquipmentText::Name => sample.retain_left_group(),
        EquipmentText::Category => sample,
    };
    ensure!(sample.has_text(), "empty equipment cache foreground");
    Ok(sample)
}
