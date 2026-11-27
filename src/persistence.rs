//! Publish complete files; never truncate the live copy while preparing an update.
use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};
use tempfile::NamedTempFile;

pub(crate) fn temporary(path: &Path) -> Result<NamedTempFile> {
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(directory)?;
    tempfile::Builder::new()
        .prefix(".hd2-")
        .tempfile_in(directory)
        .with_context(|| format!("failed to prepare {}", path.display()))
}

pub(crate) fn publish(file: NamedTempFile, path: &Path) -> Result<()> {
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = temporary(path)?;
    file.write_all(bytes)?;
    publish(file, path)
}
