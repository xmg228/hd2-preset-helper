//! Shared file logging; callers choose the destination and verbosity.
use anyhow::{Context, Result};
use std::{fmt, fs, path::Path};
use tracing::{
    Level,
    field::{Field, Visit},
};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::{
    field::{RecordFields, VisitOutput},
    filter::Targets,
    fmt::{
        FormatFields,
        format::{DefaultVisitor, Writer},
    },
    prelude::*,
};

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
        .fmt_fields(FloatFields)
        .with_filter(
            Targets::new()
                .with_target(env!("CARGO_BIN_NAME"), level)
                .with_target("hd2_preset_helper", level),
        );

    tracing_subscriber::registry().with(file_layer).init();
    Ok(guard)
}

// Round raw floating-point fields; keep the default formatting for text and errors.
struct FloatFields;

impl<'writer> FormatFields<'writer> for FloatFields {
    fn format_fields<R: RecordFields>(&self, writer: Writer<'writer>, fields: R) -> fmt::Result {
        let mut visitor = FloatVisitor(DefaultVisitor::new(writer, true));
        fields.record(&mut visitor);
        visitor.0.finish()
    }
}

struct FloatVisitor<'writer>(DefaultVisitor<'writer>);

impl Visit for FloatVisitor<'_> {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.record_debug(field, &format_args!("{value:.3}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.record_str(field, value);
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.0.record_error(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.record_debug(field, value);
    }
}
