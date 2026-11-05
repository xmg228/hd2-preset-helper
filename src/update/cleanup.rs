//! Background cleanup of self-replace's leftover Windows executables.
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

pub(super) fn start() {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let Some(stem) = executable.file_stem().and_then(|name| name.to_str()) else {
        return;
    };
    let canonical = executable.canonicalize().ok();
    let temporary = std::env::temp_dir();
    let mut files = Vec::new();
    for directory in [
        canonical.as_deref().and_then(Path::parent),
        Some(temporary.as_path()),
    ]
    .into_iter()
    .flatten()
    {
        files.extend(leftovers_in(directory, stem));
    }
    if files.is_empty() {
        return;
    }
    // Keep this snapshot: a later update may create new helpers in the same directories.
    if let Err(error) = thread::Builder::new()
        .name("update-cleanup".into())
        .spawn(move || remove_leftovers(files))
    {
        tracing::debug!(%error, "could not start update cleanup");
    }
}

fn leftovers_in(directory: &Path, stem: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| is_leftover(name, stem))
                && entry.file_type().is_ok_and(|kind| kind.is_file())
        })
        .map(|entry| entry.path())
        .collect()
}

fn is_leftover(name: &str, stem: &str) -> bool {
    let Some((prefix, random)) = name
        .strip_suffix(".__relocated__.exe")
        .or_else(|| name.strip_suffix(".__selfdelete__.exe"))
        .and_then(|name| name.rsplit_once('.'))
    else {
        return false;
    };
    prefix
        .strip_prefix('.')
        .is_some_and(|name| name.eq_ignore_ascii_case(stem))
        && random.len() == 32
        && random.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn remove_leftovers(mut files: Vec<PathBuf>) {
    let mut removed = 0;
    // The old process or its cleanup helper may still be exiting after a restart.
    for delay_ms in [0, 100, 200, 400, 800, 1600] {
        if files.is_empty() {
            break;
        }
        if delay_ms > 0 {
            thread::sleep(Duration::from_millis(delay_ms));
        }
        files.retain(|path| match fs::remove_file(path) {
            Ok(()) => {
                removed += 1;
                false
            }
            Err(error) => error.kind() != ErrorKind::NotFound,
        });
    }
    tracing::debug!(removed, remaining = files.len(), "update cleanup finished");
}
