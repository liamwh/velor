//! Tracing subscriber setup with a runtime-switchable writer.
//!
//! The default writer is **stderr**. While the streaming TUI is active it owns
//! the alternate screen on stdout, so any tracing output landing on stderr
//! corrupts the display (e.g. the `WARN vel: retryable error…` lines that bleed
//! over the box border). [`redirect_to_file`] switches the writer to a dedicated
//! log file once a run starts, keeping the logs available for debugging without
//! leaking them onto the screen.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;

/// A [`MakeWriter`] whose destination is chosen at runtime: stderr until a file
/// is set via [`TracingSink::redirect_to_file`], then that file for the rest of
/// the process.
#[derive(Clone)]
pub struct TracingSink {
    file: Arc<Mutex<Option<File>>>,
}

impl TracingSink {
    fn new() -> Self {
        Self {
            file: Arc::new(Mutex::new(None)),
        }
    }

    /// Redirects tracing output to `path` (appended, created if missing).
    /// Subsequent events are written there until the process exits.
    fn redirect_to_file(&self, path: &Path) -> io::Result<()> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        if let Ok(mut guard) = self.file.lock() {
            *guard = Some(file);
        }
        Ok(())
    }
}

/// The per-event writer: stderr while no file is set, else the redirected file.
/// The `File` variant holds the sink's lock for the duration of the write, which
/// also serialises concurrent events to the shared handle.
pub enum TracingWriter<'a> {
    Stderr,
    File(std::sync::MutexGuard<'a, Option<File>>),
}

impl Write for TracingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Stderr => io::stderr().write(buf),
            // `File` is only constructed when the guard is `Some`, but guard for
            // a race regardless by falling back to stderr.
            Self::File(guard) => match guard.as_mut() {
                Some(f) => f.write(buf),
                None => io::stderr().write(buf),
            },
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Stderr => io::stderr().flush(),
            Self::File(guard) => match guard.as_mut() {
                Some(f) => f.flush(),
                None => io::stderr().flush(),
            },
        }
    }
}

impl<'a> MakeWriter<'a> for TracingSink {
    type Writer = TracingWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        match self.file.lock() {
            // Hold the guard only when a file is actually set.
            Ok(guard) if guard.is_some() => TracingWriter::File(guard),
            _ => TracingWriter::Stderr,
        }
    }
}

static SINK: OnceLock<TracingSink> = OnceLock::new();

/// Returns the process-wide [`TracingSink`], initialising it on first call.
fn sink() -> TracingSink {
    SINK.get_or_init(TracingSink::new).clone()
}

/// Installs the tracing subscriber with the runtime-switchable sink.
///
/// Output goes to stderr until [`redirect_to_file`] is called. The env filter
/// defaults to `INFO` and silences `tui_markdown` HTML-not-supported spam.
pub fn install() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into())
                .add_directive("tui_markdown=error".parse().unwrap_or_default()),
        )
        .with_writer(sink())
        .init();
}

/// Switches tracing output from stderr to a dedicated file that is a sibling of
/// `jsonl_log_path` (i.e. `<run>.tracing.log` next to `<run>.jsonl`). Returns
/// the tracing log path on success; on failure stderr remains the target and the
/// error is returned so the caller can warn.
pub fn redirect_to_file(jsonl_log_path: &Path) -> io::Result<PathBuf> {
    let tracing_path = sibling_tracing_path(jsonl_log_path);
    sink().redirect_to_file(&tracing_path)?;
    Ok(tracing_path)
}

/// Derives a sibling `.tracing.log` path from a `.jsonl` run-log path.
fn sibling_tracing_path(jsonl_log_path: &Path) -> PathBuf {
    let mut p = jsonl_log_path.to_path_buf();
    p.set_extension("tracing.log");
    p
}
