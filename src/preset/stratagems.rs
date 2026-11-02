//! Captured Stratagem and Booster templates and their persistence.
use std::path::Path;

use super::{LocalTemplate, StratagemPreset, load_template_image, resolve_template_path, storage};
use crate::vision::{ImageSample, SampleGeometry};
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
