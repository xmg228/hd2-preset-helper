//! Captured Stratagem and Booster templates and their persistence.
use std::path::Path;

use super::{
    LocalTemplate, Preset, StratagemPreset, images::NewImages, load_template_image,
    resolve_template_path, storage,
};
use crate::{
    item::ItemKind,
    item_cache::{IconChoice, ItemCache},
    vision::{ImageSample, SampleGeometry, TemplateClassifier},
};
use anyhow::{Context, Result, bail, ensure};

pub struct CapturedStratagemPreset {
    pub stratagems: Vec<ImageSample>,
    pub booster: Option<ImageSample>,
}

pub fn invalid_reason(path: &Path, preset: &StratagemPreset) -> Option<String> {
    for template in preset
        .stratagems
        .iter()
        .chain(preset.booster.iter())
        .chain(preset.fallback_booster.iter())
    {
        if !valid_sample_geometry(template.geometry) {
            return Some(format!("invalid sample geometry for {}", template.path));
        }
        let resolved = match resolve_template_path(path, &template.path) {
            Ok(path) => path,
            Err(error) => {
                return Some(format!(
                    "invalid template path {:?}: {error}",
                    template.path
                ));
            }
        };
        if !resolved.is_file() {
            return Some(format!("missing local template {}", resolved.display()));
        }
    }

    None
}

pub fn load(path: &Path, name: &str) -> Result<StratagemPreset> {
    let presets = storage::load(path)?;
    let preset = presets
        .get(name)?
        .stratagems
        .as_ref()
        .with_context(|| format!("preset not found: {name}"))?;

    validate(name, preset)?;
    Ok(preset.clone())
}

pub fn save(
    path: &Path,
    name: &str,
    captured: &CapturedStratagemPreset,
) -> Result<StratagemPreset> {
    super::validate_id(name)?;
    ensure!(
        captured.stratagems.len() == 4,
        "preset {name} must contain exactly 4 captured stratagems, got {}",
        captured.stratagems.len()
    );
    ensure!(
        captured
            .stratagems
            .iter()
            .all(|sample| valid_sample_geometry(sample.geometry)),
        "preset {name} has invalid local-template geometry"
    );

    storage::edit_images(path, name, "", |record, images| {
        let mut preset = StratagemPreset {
            stratagems: Vec::with_capacity(4),
            booster: None,
            fallback_booster: None,
        };
        for (index, sample) in captured.stratagems.iter().enumerate() {
            let relative = images.save(&format!("stratagem-{}", index + 1), &sample.image)?;
            preset.stratagems.push(LocalTemplate {
                path: relative,
                geometry: sample.geometry,
            });
        }
        if let Some(sample) = &captured.booster {
            ensure!(
                valid_sample_geometry(sample.geometry),
                "preset {name} has invalid booster-template geometry"
            );
            let relative = images.save("booster", &sample.image)?;
            preset.booster = Some(LocalTemplate {
                path: relative,
                geometry: sample.geometry,
            });
        }

        validate(name, &preset)?;
        record.stratagems = Some(preset.clone());
        Ok(preset)
    })
}

/// Validate newly selected items, then copy them into independent preset images.
pub(super) fn write_cached(
    path: &Path,
    name: &str,
    cache: &ItemCache,
    choices: &[IconChoice; 6],
    record: &mut Preset,
    images: &mut NewImages,
) -> Result<()> {
    let previous = record.stratagems.as_ref();
    if choices[..4].contains(&IconChoice::Keep)
        && choices[..4]
            .iter()
            .any(|choice| matches!(choice, IconChoice::Item(_)))
    {
        let saved = previous.map_or(&[][..], |preset| preset.stratagems.as_slice());
        let sources = choices[..4]
            .iter()
            .enumerate()
            .map(|(index, choice)| match *choice {
                IconChoice::Keep => {
                    let template = saved.get(index).with_context(|| {
                        format!("select a Stratagem for slot {} before saving", index + 1)
                    })?;
                    load_template_sample(path, template)
                }
                IconChoice::Clear => bail!("all four Stratagem slots must be filled"),
                IconChoice::Item(index) => {
                    catalog_sample(cache, ItemKind::Stratagem, index).cloned()
                }
            })
            .collect::<Result<Vec<_>>>()?;
        for index in 0..4 {
            for earlier in 0..index {
                if matches!(
                    (choices[earlier], choices[index]),
                    (IconChoice::Keep, IconChoice::Item(_))
                        | (IconChoice::Item(_), IconChoice::Keep)
                ) {
                    ensure!(
                        !TemplateClassifier::same_item(
                            ItemKind::Stratagem,
                            &sources[earlier],
                            &sources[index],
                        )?,
                        "could not distinguish the Stratagems in slots {} and {}; choose different items",
                        earlier + 1,
                        index + 1,
                    );
                }
            }
        }
    }
    let kept = previous.and_then(|preset| match (choices[4], choices[5]) {
        (IconChoice::Keep, IconChoice::Item(_)) => preset.booster.as_ref(),
        (IconChoice::Item(_), IconChoice::Keep) => preset.fallback_booster.as_ref(),
        _ => None,
    });
    if let Some(kept) = kept {
        let index = match (choices[4], choices[5]) {
            (IconChoice::Item(index), _) | (_, IconChoice::Item(index)) => index,
            _ => unreachable!("a mixed selection contains a library item"),
        };
        let kept = load_template_sample(path, kept)?;
        let selected = catalog_sample(cache, ItemKind::Booster, index)?;
        let (preferred, fallback) = if choices[4] == IconChoice::Keep {
            (&kept, selected)
        } else {
            (selected, &kept)
        };
        ensure!(
            !TemplateClassifier::same_item(ItemKind::Booster, preferred, fallback,)?,
            "could not distinguish the preferred and fallback Booster; choose different items"
        );
    }
    refresh_cached(name, cache, choices, record, images)
}

/// Replace already located items, preserving the loadout's existing selections.
pub(super) fn refresh_cached(
    name: &str,
    cache: &ItemCache,
    choices: &[IconChoice; 6],
    record: &mut Preset,
    images: &mut NewImages,
) -> Result<()> {
    let mut preset = record.stratagems.clone().unwrap_or(StratagemPreset {
        stratagems: Vec::new(),
        booster: None,
        fallback_booster: None,
    });
    if choices[..4]
        .iter()
        .any(|choice| *choice != IconChoice::Keep)
    {
        preset.stratagems =
            write_cached_stratagems(cache, &choices[..4], &preset.stratagems, images)?;
    }
    for (index, role, field) in [
        (4, "booster", &mut preset.booster),
        (5, "fallback-booster", &mut preset.fallback_booster),
    ] {
        match choices[index] {
            IconChoice::Keep => {}
            IconChoice::Clear => *field = None,
            IconChoice::Item(index) => {
                let sample = catalog_sample(cache, ItemKind::Booster, index)?;
                *field = Some(LocalTemplate {
                    path: images.save(role, &sample.image)?,
                    geometry: sample.geometry,
                });
            }
        }
    }
    if choices[4..]
        .iter()
        .any(|choice| *choice != IconChoice::Keep)
    {
        ensure!(
            preset.booster.is_some() || preset.fallback_booster.is_none(),
            "select a preferred Booster or clear the fallback before saving"
        );
        if let (Some(_), Some(_)) = (&preset.booster, &preset.fallback_booster)
            && let (IconChoice::Item(a), IconChoice::Item(b)) = (choices[4], choices[5])
        {
            ensure!(
                a != b,
                "the preferred and fallback Booster must be different"
            );
        }
    }
    validate(name, &preset)?;
    record.stratagems = Some(preset);
    Ok(())
}

fn write_cached_stratagems(
    cache: &ItemCache,
    choices: &[IconChoice],
    previous: &[LocalTemplate],
    images: &mut NewImages,
) -> Result<Vec<LocalTemplate>> {
    for index in 0..choices.len() {
        for earlier in 0..index {
            if let (IconChoice::Item(a), IconChoice::Item(b)) = (choices[earlier], choices[index]) {
                ensure!(
                    a != b,
                    "the same Stratagem cannot fill slots {} and {}",
                    earlier + 1,
                    index + 1,
                );
            }
        }
    }
    choices
        .iter()
        .enumerate()
        .map(|(index, choice)| match *choice {
            IconChoice::Keep => previous.get(index).cloned().with_context(|| {
                format!("select a Stratagem for slot {} before saving", index + 1)
            }),
            IconChoice::Item(item) => {
                let sample = catalog_sample(cache, ItemKind::Stratagem, item)?;
                Ok(LocalTemplate {
                    path: images.save(&format!("stratagem-{}", index + 1), &sample.image)?,
                    geometry: sample.geometry,
                })
            }
            IconChoice::Clear => bail!("all four Stratagem slots must be filled"),
        })
        .collect()
}

fn catalog_sample(cache: &ItemCache, kind: ItemKind, index: usize) -> Result<&ImageSample> {
    let (catalog, error) = match kind {
        ItemKind::Stratagem => (
            &cache.stratagems,
            "selected Stratagem is not available in the cache",
        ),
        ItemKind::Booster => (
            &cache.boosters,
            "selected Booster is not available in the cache",
        ),
    };
    catalog
        .as_ref()
        .and_then(|catalog| catalog.items.get(index))
        .map(|item| &item.icon.sample)
        .context(error)
}

pub fn save_fallback_booster(
    path: &Path,
    name: &str,
    sample: &ImageSample,
) -> Result<StratagemPreset> {
    super::validate_id(name)?;
    ensure!(
        valid_sample_geometry(sample.geometry),
        "preset {name} has invalid fallback booster-template geometry"
    );

    storage::edit_images(path, name, "", |record, images| {
        let preset = record
            .stratagems
            .as_mut()
            .context("Stratagem preset not saved")?;
        preset.fallback_booster = Some(LocalTemplate {
            path: images.save("fallback-booster", &sample.image)?,
            geometry: sample.geometry,
        });
        validate(name, preset)?;
        Ok(preset.clone())
    })
}

pub fn load_template_sample(presets_path: &Path, template: &LocalTemplate) -> Result<ImageSample> {
    let image = load_template_image(presets_path, &template.path)?;
    ensure!(
        valid_sample_geometry(template.geometry)
            && template.geometry.center_x <= image.width() as f32
            && template.geometry.center_y <= image.height() as f32,
        "local template {} has invalid sample geometry",
        template.path
    );
    Ok(ImageSample {
        image,
        geometry: template.geometry,
    })
}

pub(super) fn validate(name: &str, preset: &StratagemPreset) -> Result<()> {
    if preset.stratagems.len() != 4 {
        bail!(
            "preset {name} must contain exactly 4 stratagems, got {}",
            preset.stratagems.len()
        );
    }

    for (index, template) in preset.stratagems.iter().enumerate() {
        if preset.stratagems[..index]
            .iter()
            .any(|previous| previous.path == template.path)
        {
            bail!(
                "preset {name} contains duplicate template {}",
                template.path
            );
        }
    }
    Ok(())
}

fn valid_sample_geometry(geometry: SampleGeometry) -> bool {
    geometry.center_x.is_finite()
        && geometry.center_x >= 0.0
        && geometry.center_y.is_finite()
        && geometry.center_y >= 0.0
        && geometry.physical_size.is_finite()
        && geometry.physical_size > 0.0
}
