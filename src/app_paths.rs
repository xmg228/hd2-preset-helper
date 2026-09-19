use std::path::PathBuf;

use anyhow::Result;

#[derive(Clone)]
pub struct AppPaths {
    pub config: PathBuf,
    pub presets: PathBuf,
    pub log: PathBuf,
    #[cfg(feature = "diagnostics")]
    pub diagnostic_scores: PathBuf,
    pub last_failure: PathBuf,
}

impl AppPaths {
    pub fn resolve() -> Result<Self> {
        crate::platform::app_paths()
    }
}
