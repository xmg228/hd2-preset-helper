//! Captured Stratagem and Booster templates and their persistence.
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use super::{
    LOCAL_TEMPLATES_DIR, PRESET_SCHEMA_VERSION, PresetFile, load_preset_file, load_template_image,
    resolve_template_path, save_template_image, validate_preset_name, write_preset_file,
};
use crate::vision::{ImageSample, SampleGeometry};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocalTemplate {
    /// Local template path relative to the directory containing presets.json.
    pub path: String,
    #[serde(flatten)]
    pub geometry: SampleGeometry,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StratagemPreset {
    pub stratagems: Vec<LocalTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booster: Option<LocalTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_booster: Option<LocalTemplate>,
}

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
    let presets = load_preset_file(path)?;
    let preset = presets
        .presets
        .get(name)
        .with_context(|| format!("preset not found: {name}"))?;

    validate(name, preset)?;
    Ok(preset.clone())
}

pub fn save(
    path: &Path,
    name: &str,
    captured: &CapturedStratagemPreset,
) -> Result<StratagemPreset> {
    validate_preset_name(name)?;
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

    let mut presets = if path.exists() {
        load_preset_file(path)?
    } else {
        PresetFile::default()
    };

    let mut preset = StratagemPreset {
        stratagems: Vec::with_capacity(4),
        booster: None,
        fallback_booster: None,
    };
    for (index, sample) in captured.stratagems.iter().enumerate() {
        let relative = format!("{LOCAL_TEMPLATES_DIR}/{name}/stratagem-{}.png", index + 1);
        save_template_image(path, &relative, &sample.image)?;
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
        let relative = format!("{LOCAL_TEMPLATES_DIR}/{name}/booster.png");
        save_template_image(path, &relative, &sample.image)?;
        preset.booster = Some(LocalTemplate {
            path: relative,
            geometry: sample.geometry,
        });
    }

    validate(name, &preset)?;
    presets.schema_version = PRESET_SCHEMA_VERSION;
    presets.presets.insert(name.to_string(), preset.clone());

    write_preset_file(path, &presets)?;
    remove_template_image_if_exists(
        path,
        &format!("{LOCAL_TEMPLATES_DIR}/{name}/fallback-booster.png"),
    )?;
    if captured.booster.is_none() {
        remove_template_image_if_exists(
            path,
            &format!("{LOCAL_TEMPLATES_DIR}/{name}/booster.png"),
        )?;
    }
    Ok(preset)
}

pub fn save_fallback_booster(
    path: &Path,
    name: &str,
    sample: &ImageSample,
) -> Result<StratagemPreset> {
    validate_preset_name(name)?;
    ensure!(
        valid_sample_geometry(sample.geometry),
        "preset {name} has invalid fallback booster-template geometry"
    );

    let mut presets = load_preset_file(path)?;
    let preset = presets
        .presets
        .get_mut(name)
        .with_context(|| format!("preset not found: {name}"))?;
    let relative = format!("{LOCAL_TEMPLATES_DIR}/{name}/fallback-booster.png");
    save_template_image(path, &relative, &sample.image)?;
    preset.fallback_booster = Some(LocalTemplate {
        path: relative,
        geometry: sample.geometry,
    });
    validate(name, preset)?;
    let saved = preset.clone();

    write_preset_file(path, &presets)?;
    Ok(saved)
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

fn remove_template_image_if_exists(presets_path: &Path, relative: &str) -> Result<()> {
    let destination = resolve_template_path(presets_path, relative)?;
    match fs::remove_file(&destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove local template {}", destination.display())),
    }
}
