//! Tracing subscriber wiring (master-plan P1.9).
//!
//! Performance and incident analysis must be possible from on-disk data:
//! every `tracing` span and event is appended as JSON lines (with span
//! busy/idle timings on close) to `<config-base>/logs/trace.jsonl`. A stderr
//! layer surfaces warnings/errors only, so the runner's existing stdout
//! protocol output stays clean. With the `otel` cargo feature, spans are also
//! exported over OTLP when `VELNOR_OTLP_ENDPOINT` is set.
//!
//! Filtering: `VELNOR_LOG` (fallback `RUST_LOG`) using standard
//! `tracing_subscriber::EnvFilter` syntax; defaults to `info` for the file
//! layer.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Rotate trace.jsonl past this size (one `.1` generation kept) — the daemon
/// runs for days and every poll/forensic event lands here; an unbounded
/// writer would be the only unbounded disk consumer on the host.
const TRACE_ROTATE_BYTES: u64 = 32 * 1024 * 1024;

struct RotatingFile {
    file: BufWriter<File>,
    path: PathBuf,
    written: u64,
}

impl RotatingFile {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            file: BufWriter::new(file),
            path,
            written,
        })
    }

    fn rotate_if_needed(&mut self) {
        if self.written < TRACE_ROTATE_BYTES {
            return;
        }
        let _ = self.file.flush();
        let mut rotated = self.path.file_name().unwrap_or_default().to_os_string();
        rotated.push(".1");
        let rotated = self.path.with_file_name(rotated);
        if std::fs::rename(&self.path, rotated).is_ok() {
            if let Ok(fresh) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                self.file = BufWriter::new(fresh);
                self.written = 0;
            }
        }
    }
}

/// `MakeWriter` over a shared, size-capped append handle. Line-at-a-time
/// JSONL writes through one descriptor stay intact; rotation happens between
/// writes.
#[derive(Clone)]
struct SharedFileWriter(Arc<Mutex<RotatingFile>>);

impl Write for SharedFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let Ok(mut inner) = self.0.lock() else {
            return Ok(buf.len());
        };
        inner.rotate_if_needed();
        inner.file.write_all(buf)?;
        inner.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let Ok(mut inner) = self.0.lock() else {
            return Ok(());
        };
        inner.file.flush()
    }
}

fn env_filter(default_directive: &str) -> EnvFilter {
    let spec = std::env::var("VELNOR_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| default_directive.to_string());
    EnvFilter::try_new(spec).unwrap_or_else(|_| EnvFilter::new(default_directive))
}

/// Install the global subscriber. `log_dir` enables the JSON file layer
/// (`trace.jsonl`); without it only warnings/errors go to stderr. Never
/// fails: telemetry must not take the runner down, and a second call (e.g.
/// in tests) is a no-op.
pub fn init(log_dir: Option<&Path>) {
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_filter(EnvFilter::new("warn"));

    let file_layer = log_dir.and_then(|dir| {
        std::fs::create_dir_all(dir).ok()?;
        let file = RotatingFile::open(dir.join("trace.jsonl")).ok()?;
        let writer = SharedFileWriter(Arc::new(Mutex::new(file)));
        Some(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(move || writer.clone())
                .with_span_events(FmtSpan::CLOSE)
                .with_current_span(true)
                .with_filter(env_filter("info")),
        )
    });

    let registry = tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .with(otel_layer());

    // try_init: keep the first subscriber when something (a test harness)
    // already installed one.
    let _ = registry.try_init();
}

#[cfg(feature = "otel")]
fn otel_layer<S>() -> Option<impl Layer<S>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use opentelemetry_otlp::WithExportConfig as _;
    let endpoint = std::env::var("VELNOR_OTLP_ENDPOINT").ok()?;
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .ok()?;
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();
    use opentelemetry::trace::TracerProvider as _;
    let tracer = provider.tracer("velnor-runner");
    Some(tracing_opentelemetry::layer().with_tracer(tracer))
}

#[cfg(not(feature = "otel"))]
fn otel_layer() -> Option<tracing_subscriber::layer::Identity> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent_and_never_panics() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-telemetry-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        init(Some(&dir));
        init(Some(&dir));
        init(None);
        tracing::info!(check = true, "telemetry smoke event");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_filter_falls_back_to_default() {
        let filter = env_filter("info");
        assert!(!format!("{filter}").is_empty());
    }

    #[test]
    fn shared_file_writer_flushes_buffered_line() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-telemetry-flush-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("trace.jsonl");
        let file = RotatingFile::open(path.clone()).expect("open trace");
        let mut writer = SharedFileWriter(Arc::new(Mutex::new(file)));

        writer.write_all(b"{\"message\":\"hello\"}\n").unwrap();
        writer.flush().unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).expect("read trace"),
            "{\"message\":\"hello\"}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotating_file_flushes_before_renaming() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-telemetry-rotate-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("trace.jsonl");
        let mut file = RotatingFile::open(path.clone()).expect("open trace");
        file.file.write_all(b"before\n").unwrap();
        file.written = TRACE_ROTATE_BYTES;
        let mut writer = SharedFileWriter(Arc::new(Mutex::new(file)));

        writer.write_all(b"after\n").unwrap();
        writer.flush().unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join("trace.jsonl.1")).expect("read rotated"),
            "before\n"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read current"),
            "after\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
