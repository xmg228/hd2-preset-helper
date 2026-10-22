//! Replace the last failure report; reporting must never replace the action error.
use std::{fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::{capture::CaptureSource, game_settings::read_color_settings, image_rect::ImageRect};

pub(super) fn save_failure_report(directory: &Path, capture: Option<&mut CaptureSource>) {
    let result = (|| -> Result<()> {
        // This dedicated directory contains only the previous failure report.
        match fs::remove_dir_all(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("failed to remove the previous failure report");
            }
        }
        fs::create_dir_all(directory).context("failed to create the failure report directory")?;
        crate::logging::save_recent(&directory.join("recent.log"))
    })();
    if let Err(error) = result {
        warn!(path = %directory.display(), error = %format!("{error:#}"), "failed to save failure report");
        return;
    }

    if let Some(capture) = capture {
        let result = (|| -> Result<()> {
            // A fresh post-failure frame, not necessarily the failed observation.
            let (w, h) = capture.output_size();
            let mut region =
                capture.region(ImageRect { x: 0, y: 0, w, h }, read_color_settings()?)?;
            region.capture()?.save(directory.join("frame.png"))?;
            Ok(())
        })();
        if let Err(error) = result {
            warn!(path = %directory.display(), error = %format!("{error:#}"), "failed to add a frame to the failure report");
        }
    }
    info!(path = %directory.display(), "failure report saved");
}
