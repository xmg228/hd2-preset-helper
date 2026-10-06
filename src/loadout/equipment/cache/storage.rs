//! Disposable JSON cache: complete orders plus Base64-encoded PNG foreground signatures.
use anyhow::{Result, ensure};
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
use crate::vision::text::TextSignature;

// Also bump when text extraction/geometry changes incompatibly.
// Rebuild unscoped maps that may contain mixed category orders.
const VERSION: u32 = 3;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    scales: BTreeMap<String, [Vec<SavedCategory>; 6]>,
}

#[derive(Serialize, Deserialize)]
struct SavedCategory {
    title: String,
    names: Vec<String>,
    order: Vec<usize>,
    previous: Option<usize>,
    next: Option<usize>,
    circular: bool,
}

impl EquipmentCache {
    pub(crate) fn load(path: &Path) -> Self {
        let result = (|| -> Result<Self> {
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
                Err(error) => return Err(error.into()),
            };
            let file: File = serde_json::from_slice(&bytes)?;
            ensure!(file.version == VERSION, "equipment cache format changed");
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
                            title: decode(&entry.title)?,
                            names: entry
                                .names
                                .iter()
                                .map(|png| decode(png))
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
            tracing::warn!(%error, "equipment cache ignored; categories will be learned again");
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
                fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, serde_json::to_vec(&file)?)?;
            fs::rename(temporary, path)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.dirty = false;
                tracing::debug!("equipment cache saved");
            }
            Err(error) => tracing::warn!(%error, "equipment cache could not be saved"),
        }
    }
}

fn encode(signature: &TextSignature) -> Result<String> {
    let mut bytes = Cursor::new(Vec::new());
    signature
        .foreground()
        .write_to(&mut bytes, ImageFormat::Png)?;
    Ok(STANDARD.encode(bytes.into_inner()))
}

fn decode(png: &str) -> Result<TextSignature> {
    let bytes = STANDARD.decode(png)?;
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)?.into_luma8();
    ensure!(
        image.pixels().any(|p| p[0] > 0),
        "empty equipment cache signature"
    );
    Ok(TextSignature::from_foreground(image))
}
