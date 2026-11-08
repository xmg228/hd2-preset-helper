//! Captured Stratagem and Booster templates and their persistence.
use std::path::Path;

use super::{
    LocalTemplate, Preset, StratagemPreset, images::NewImages, load_template_image,
    resolve_template_path, storage,
};
use crate::{
    item::ItemKind,
    item_cache::{IconChoice, ItemCache},
    vision::{IconTemplate, ImageSample, SampleGeometry, TemplateClassifier},
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

/// Copy changed catalog items into independent preset images in one commit.
pub(super) fn write_cached(
    path: &Path,
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
            write_cached_stratagems(path, cache, &choices[..4], &preset.stratagems, images)?;
    }
    for (index, role, field) in [
        (4, "booster", &mut preset.booster),
        (5, "fallback-booster", &mut preset.fallback_booster),
    ] {
        match choices[index] {
            IconChoice::Keep => {}
            IconChoice::Clear => *field = None,
            IconChoice::Item(index) => {
                let item = cache
                    .boosters
                    .as_ref()
                    .and_then(|catalog| catalog.items.get(index))
                    .context("selected Booster is not available in the cache")?;
                *field = Some(LocalTemplate {
                    path: images.save(role, &item.icon.sample.image)?,
                    geometry: item.icon.sample.geometry,
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
        if let (Some(preferred), Some(fallback)) = (&preset.booster, &preset.fallback_booster) {
            match (choices[4], choices[5]) {
                (IconChoice::Item(a), IconChoice::Item(b)) => ensure!(
                    a != b,
                    "the preferred and fallback Booster must be different"
                ),
                (IconChoice::Keep, IconChoice::Keep) => {}
                _ => ensure!(
                    !TemplateClassifier::same_item(
                        ItemKind::Booster,
                        &load_template_sample(path, preferred)?,
                        &load_template_sample(path, fallback)?,
                    )?,
                    "could not distinguish the preferred and fallback Booster; choose different items"
                ),
            }
        }
    }
    validate(name, &preset)?;
    record.stratagems = Some(preset);
    Ok(())
}

fn write_cached_stratagems(
    path: &Path,
    cache: &ItemCache,
    choices: &[IconChoice],
    previous: &[LocalTemplate],
    images: &mut NewImages,
) -> Result<Vec<LocalTemplate>> {
    let sources = choices
        .iter()
        .enumerate()
        .map(|(index, choice)| match *choice {
            IconChoice::Keep => {
                let template = previous.get(index).with_context(|| {
                    format!("select a Stratagem for slot {} before saving", index + 1)
                })?;
                load_template_sample(path, template)
            }
            IconChoice::Clear => bail!("all four Stratagem slots must be filled"),
            IconChoice::Item(index) => cache
                .stratagems
                .as_ref()
                .and_then(|catalog| catalog.items.get(index))
                .map(|item| item.icon.sample.clone())
                .context("selected Stratagem is not available in the cache"),
        })
        .collect::<Result<Vec<_>>>()?;
    for index in 0..choices.len() {
        for earlier in 0..index {
            match (choices[earlier], choices[index]) {
                (IconChoice::Item(a), IconChoice::Item(b)) => ensure!(
                    a != b,
                    "the same Stratagem cannot fill slots {} and {}",
                    earlier + 1,
                    index + 1,
                ),
                (IconChoice::Keep, IconChoice::Keep) => {}
                _ => ensure!(
                    !TemplateClassifier::same_item(
                        ItemKind::Stratagem,
                        &sources[earlier],
                        &sources[index],
                    )?,
                    "could not distinguish the Stratagems in slots {} and {}; choose different items",
                    earlier + 1,
                    index + 1,
                ),
            }
        }
    }
    choices
        .iter()
        .zip(sources)
        .enumerate()
        .map(|(index, (choice, source))| match choice {
            IconChoice::Keep => Ok(previous[index].clone()),
            IconChoice::Item(_) => Ok(LocalTemplate {
                path: images.save(&format!("stratagem-{}", index + 1), &source.image)?,
                geometry: source.geometry,
            }),
            IconChoice::Clear => unreachable!("empty Stratagem slots were rejected above"),
        })
        .collect()
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

pub(crate) fn load_icon_template(
    presets_path: &Path,
    template: &LocalTemplate,
    kind: ItemKind,
) -> Result<IconTemplate> {
    IconTemplate::extract(kind, &load_template_sample(presets_path, template)?)
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
