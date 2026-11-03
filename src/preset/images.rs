//! Template paths and staged PNGs. Failed saves never overwrite active images.
use super::{LOCAL_TEMPLATES_DIR, PresetLibrary};
use anyhow::{Context, Result, ensure};
use image::{ImageFormat, RgbaImage};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub(crate) fn resolve_template_path(presets: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "invalid relative template path"
    );
    ensure!(
        relative.starts_with(LOCAL_TEMPLATES_DIR),
        "local template path must be inside {LOCAL_TEMPLATES_DIR}"
    );
    Ok(presets
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .join(relative))
}

pub(crate) fn load_template_image(presets: &Path, relative: &str) -> Result<RgbaImage> {
    let bytes = fs::read(resolve_template_path(presets, relative)?)
        .with_context(|| format!("failed to read local template {relative}"))?;
    crate::assets::decode_rgba8(&bytes)
        .with_context(|| format!("failed to decode local template {relative}"))
}

pub(super) fn owned_directory(presets: &Path, id: &str) -> Result<PathBuf> {
    super::model::validate_id(id, "preset_")?;
    let directory = resolve_template_path(presets, &format!("{LOCAL_TEMPLATES_DIR}/{id}"))?;
    if directory.try_exists()? {
        let metadata = fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "preset template directory must be a real directory"
        );
        let root = directory
            .parent()
            .context("template directory has no parent")?
            .canonicalize()?;
        ensure!(
            directory.canonicalize()?.parent() == Some(root.as_path()),
            "preset template directory resolves outside its template root"
        );
    }
    Ok(directory)
}

pub(super) struct NewImages {
    base: PathBuf,
    directory: PathBuf,
    created: Vec<PathBuf>,
}

impl NewImages {
    pub(super) fn new(presets: &Path, id: &str, subdirectory: &str) -> Result<Self> {
        let owner = owned_directory(presets, id)?;
        let directory = owner.join(subdirectory);
        fs::create_dir_all(&directory)?;
        ensure!(
            directory.canonicalize()?.starts_with(owner.canonicalize()?),
            "template section resolves outside its preset directory"
        );
        Ok(Self {
            base: presets
                .parent()
                .context("preset file has no parent")?
                .into(),
            directory,
            created: Vec::new(),
        })
    }

    pub fn save(&mut self, role: &str, image: &RgbaImage) -> Result<String> {
        ensure!(
            image.width() > 0 && image.height() > 0,
            "cannot save an empty local template"
        );
        self.stage(role, |file| {
            image.write_to(file, ImageFormat::Png)?;
            Ok(())
        })
    }

    pub(super) fn copy(&mut self, role: &str, presets: &Path, relative: &str) -> Result<String> {
        let mut source = fs::File::open(resolve_template_path(presets, relative)?)
            .with_context(|| format!("failed to read local template {relative}"))?;
        self.stage(role, |file| {
            std::io::copy(&mut source, file)
                .with_context(|| format!("failed to copy local template {relative}"))?;
            Ok(())
        })
    }

    fn stage(
        &mut self,
        role: &str,
        write: impl FnOnce(&mut fs::File) -> Result<()>,
    ) -> Result<String> {
        let mut file = tempfile::Builder::new()
            .prefix(&format!("{role}-"))
            .suffix(".png")
            .tempfile_in(&self.directory)?;
        write(file.as_file_mut())?;
        file.as_file().sync_all()?;
        let (handle, path) = file.keep().map_err(|error| error.error)?;
        drop(handle);
        self.created.push(path.clone());
        let relative = path
            .strip_prefix(&self.base)?
            .to_str()
            .context("template path is not UTF-8")?
            .replace('\\', "/");
        Ok(relative)
    }

    pub(super) fn commit(mut self) {
        self.created.clear();
    }
}

impl Drop for NewImages {
    fn drop(&mut self) {
        for path in &self.created {
            let _ = fs::remove_file(path);
        }
    }
}

pub(super) fn cleanup(presets: &Path, id: &str, previous: Vec<String>, library: &PresetLibrary) {
    let result = (|| -> Result<()> {
        let referenced = library.image_paths();
        let owner = Path::new(LOCAL_TEMPLATES_DIR).join(id);
        let directory = owned_directory(presets, id)?.canonicalize()?;
        for relative in previous {
            if referenced.contains(relative.as_str()) || !Path::new(&relative).starts_with(&owner) {
                continue;
            }
            let path = resolve_template_path(presets, &relative)?;
            if !path.try_exists()? {
                continue;
            }
            ensure!(
                path.canonicalize()?.starts_with(&directory),
                "replaced template resolves outside its preset directory"
            );
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("failed to remove replaced template"),
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(%id, error = %format!("{error:#}"), "saved preset is valid; old template cleanup failed");
    }
}
