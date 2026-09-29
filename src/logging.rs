//! Shared file logging; callers choose the destination and verbosity.
use anyhow::{Context, Result};
use std::{fs, path::Path};
use tracing::Level;
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::{filter::Targets, prelude::*};

pub fn init(path: &Path, level: Level) -> Result<WorkerGuard> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }
    let file = fs::File::create(path)
        .with_context(|| format!("failed to create log {}", path.display()))?;
    let (writer, guard) = NonBlockingBuilder::default().lossy(false).finish(file);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true)
        .with_writer(writer)
        .compact()
        .with_filter(
            Targets::new()
                .with_target(env!("CARGO_BIN_NAME"), level)
                .with_target("hd2_preset_helper", level),
        );

    tracing_subscriber::registry().with(file_layer).init();
    Ok(guard)
}
