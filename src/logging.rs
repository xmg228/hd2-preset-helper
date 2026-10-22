//! File logging with caller-selected verbosity and a bounded recent DEBUG tail.
use anyhow::{Context, Result};
use std::{
    collections::VecDeque,
    fmt, fs,
    io::{self, Write},
    path::Path,
    sync::Mutex,
};
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
        writer::MakeWriterExt,
    },
    prelude::*,
};

const RECENT_LOG_BYTES: usize = 1024 * 1024;

static RECENT_LOG: Mutex<RecentLog> = Mutex::new(RecentLog {
    events: VecDeque::new(),
    bytes: 0,
    omitted: 0,
});

pub fn init(path: &Path, level: Level) -> Result<WorkerGuard> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create log directory {}", parent.display()))?;
    }
    let file = fs::File::create(path)
        .with_context(|| format!("failed to create log {}", path.display()))?;
    let (writer, guard) = NonBlockingBuilder::default().lossy(false).finish(file);
    let log_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_thread_names(true)
        .with_writer(writer.with_max_level(level).and(|| RecentWriter))
        .compact()
        .fmt_fields(FloatFields)
        .with_filter(
            Targets::new()
                .with_target(env!("CARGO_BIN_NAME"), level.max(Level::DEBUG))
                .with_target("hd2_preset_helper", level.max(Level::DEBUG)),
        );

    tracing_subscriber::registry().with(log_layer).init();
    Ok(guard)
}

pub fn clear_recent() {
    if let Ok(mut recent) = RECENT_LOG.lock() {
        recent.events.clear();
        recent.bytes = 0;
        recent.omitted = 0;
    }
}

pub fn save_recent(path: &Path) -> Result<()> {
    let snapshot = {
        let recent = RECENT_LOG
            .lock()
            .map_err(|_| anyhow::anyhow!("recent log lock was poisoned"))?;
        let mut bytes = Vec::with_capacity(recent.bytes + 256);
        writeln!(
            bytes,
            "# HD2 Preset Helper v{} diagnostics={}",
            env!("CARGO_PKG_VERSION"),
            cfg!(feature = "diagnostics")
        )?;
        writeln!(
            bytes,
            "# Logs since request start; up to 1 MiB, may be truncated."
        )?;
        if recent.omitted > 0 {
            writeln!(
                bytes,
                "# {} earlier or oversized events omitted.",
                recent.omitted
            )?;
        }
        for event in &recent.events {
            bytes.extend_from_slice(event);
        }
        bytes
    };
    fs::write(path, snapshot)
        .with_context(|| format!("failed to write recent log {}", path.display()))
}

struct RecentLog {
    events: VecDeque<Vec<u8>>,
    bytes: usize,
    omitted: usize,
}

impl RecentLog {
    fn push(&mut self, event: &[u8]) {
        if event.is_empty() {
            return;
        }
        if event.len() > RECENT_LOG_BYTES {
            self.omitted = self.omitted.saturating_add(1);
            return;
        }
        while self.bytes + event.len() > RECENT_LOG_BYTES {
            let oldest = self.events.pop_front().unwrap();
            self.bytes -= oldest.len();
            self.omitted = self.omitted.saturating_add(1);
        }
        self.bytes += event.len();
        self.events.push_back(event.to_vec());
    }
}

struct RecentWriter;

impl Write for RecentWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // The fmt layer writes each formatted UTF-8 event as one complete block.
        RECENT_LOG
            .lock()
            .map_err(|_| io::Error::other("recent log lock was poisoned"))?
            .push(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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
