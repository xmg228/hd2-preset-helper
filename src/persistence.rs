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

fn prepare(path: &Path, bytes: &[u8]) -> Result<NamedTempFile> {
    let mut file = temporary(path)?;
    file.write_all(bytes)?;
    Ok(file)
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    publish(prepare(path, bytes)?, path)
}

/// Both publication and restoration of the original contents failed.
#[derive(Debug)]
pub(crate) struct RollbackFailed(pub anyhow::Error);

impl std::fmt::Display for RollbackFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("file update and rollback both failed")
    }
}
impl std::error::Error for RollbackFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Roll back the first file if the second fails; this is not a crash-atomic transaction.
pub(crate) fn write_pair(first: (&Path, &[u8]), second: (&Path, &[u8])) -> Result<()> {
    let previous = fs::read(first.0)?;
    let first_file = prepare(first.0, first.1)?;
    let second_file = prepare(second.0, second.1)?;
    publish(first_file, first.0)?;
    if let Err(error) = publish(second_file, second.0) {
        if let Err(rollback) = write(first.0, &previous) {
            return Err(RollbackFailed(
                error.context(format!("rollback also failed: {rollback:#}")),
            )
            .into());
        }
        return Err(error);
    }
    Ok(())
}
