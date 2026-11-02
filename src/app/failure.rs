//! Publish the last failure ZIP. Never capture or re-run recognition here.
use std::{io::Write, path::Path};

use anyhow::Result;
use serde_json::json;
use tracing::{info, warn};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{evidence::OperationEvidence, preset_action::PresetCommand};

pub(super) fn save_failure_report(
    path: &Path,
    preset: &str,
    command: PresetCommand,
    error: &anyhow::Error,
    evidence: Option<&OperationEvidence>,
) {
    // Freeze execution timing before PNG encoding and report publication.
    let evidence_report = evidence.map(OperationEvidence::report);
    let result = (|| -> Result<()> {
        let mut pending = crate::persistence::temporary(path)?;
        let mut archive = ZipWriter::new(pending.as_file_mut());
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(1));
        archive.start_file("recent.log", options)?;
        archive.write_all(&crate::logging::recent_snapshot()?)?;
        let image_error = evidence.and_then(|evidence| {
            evidence
                .encode_images(|name, png| {
                    archive.start_file(name, options)?;
                    archive.write_all(png)?;
                    Ok(())
                })
                .err()
        });
        let report = json!({
            "version": env!("CARGO_PKG_VERSION"), "diagnostics": cfg!(feature = "diagnostics"),
            "preset": preset, "command": format!("{command:?}"),
            "errors": error.chain().map(ToString::to_string).collect::<Vec<_>>(),
            "evidence": evidence_report,
            "image_save_error": image_error.map(|error| format!("{error:#}")),
        });
        archive.start_file("report.json", options)?;
        archive.write_all(&serde_json::to_vec_pretty(&report)?)?;
        archive.finish()?;
        crate::persistence::publish(pending, path)
    })();
    match result {
        Ok(()) => info!(path = %path.display(), "failure report saved"),
        Err(error) => {
            warn!(path = %path.display(), error = %format!("{error:#}"), "failed to save failure report")
        }
    }
}
