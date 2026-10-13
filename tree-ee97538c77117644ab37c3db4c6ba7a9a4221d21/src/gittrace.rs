//! GIT_TRACE2 event consumption.
//!
//! Checkout cost is dominated by bytes and refs moved, not by wall time alone.
//! Git already reports both through its `trace2` event stream, so the harness
//! sets `GIT_TRACE2_EVENT` to a file and reads the counters back rather than
//! asking the runner to compute them. Nothing here parses human-readable git
//! output: only event JSON is read. An unrecognised but well-formed event is
//! ignored rather than guessed at; incomplete evidence is rejected.

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Failure while reading or validating requested Git trace evidence.
#[derive(Debug)]
pub enum TraceError {
    /// The trace file could not be read.
    Read { path: PathBuf, source: io::Error },
    /// The trace stream contains no events.
    Empty,
    /// A line is not a JSON value.
    MalformedJson {
        line: usize,
        source: serde_json::Error,
    },
    /// A JSON event has the wrong shape for trace2.
    InvalidEvent { line: usize, reason: String },
    /// The stream contains no process-start marker, so it cannot prove that
    /// the requested command emitted complete trace evidence.
    NoProcessEvents,
}

impl fmt::Display for TraceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "read Git trace {}: {source}", path.display())
            }
            Self::Empty => formatter.write_str("Git trace is empty"),
            Self::MalformedJson { line, source } => {
                write!(
                    formatter,
                    "Git trace line {line} is not valid JSON: {source}"
                )
            }
            Self::InvalidEvent { line, reason } => {
                write!(formatter, "Git trace line {line} is invalid: {reason}")
            }
            Self::NoProcessEvents => {
                formatter.write_str("Git trace contains no process version event")
            }
        }
    }
}

impl std::error::Error for TraceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::MalformedJson { source, .. } => Some(source),
            Self::Empty | Self::InvalidEvent { .. } | Self::NoProcessEvents => None,
        }
    }
}

/// Counters extracted from one git process tree's trace2 event stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCounters {
    /// Bytes received from the remote across all fetch/clone processes.
    pub received_bytes: u64,
    /// Bytes sent to the remote.
    pub sent_bytes: u64,
    /// Refs advertised or updated, summed across processes.
    pub refs: u64,
    /// Objects reported by the transfer.
    pub objects: u64,
    /// Number of git processes that appeared in the stream.
    pub processes: u64,
}

impl GitCounters {
    /// Read a trace2 event file written by `GIT_TRACE2_EVENT`.
    ///
    /// # Errors
    /// The file could not be read.
    pub fn from_event_file(path: &Path) -> Result<Self, TraceError> {
        let stream = std::fs::read_to_string(path).map_err(|source| TraceError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_events(&stream)
    }

    /// Parse a trace2 event stream (one JSON object per line).
    ///
    /// Every line must be valid trace2 JSON and the stream must contain at
    /// least one process version event. This prevents an absent, truncated,
    /// or malformed file from becoming fabricated zero counters.
    pub fn from_events(stream: &str) -> Result<Self, TraceError> {
        if stream.trim().is_empty() {
            return Err(TraceError::Empty);
        }

        let mut counters = Self::default();
        for (line_index, line) in stream.lines().enumerate() {
            let line_number = line_index + 1;
            if line.trim().is_empty() {
                return Err(TraceError::InvalidEvent {
                    line: line_number,
                    reason: "blank line".to_owned(),
                });
            }
            let event = serde_json::from_str::<serde_json::Value>(line).map_err(|source| {
                TraceError::MalformedJson {
                    line: line_number,
                    source,
                }
            })?;
            let Some(event) = event.as_object() else {
                return Err(TraceError::InvalidEvent {
                    line: line_number,
                    reason: "event must be a JSON object".to_owned(),
                });
            };
            let Some(name) = event.get("event").and_then(serde_json::Value::as_str) else {
                return Err(TraceError::InvalidEvent {
                    line: line_number,
                    reason: "event name is missing or not a string".to_owned(),
                });
            };
            if name.is_empty() {
                return Err(TraceError::InvalidEvent {
                    line: line_number,
                    reason: "event name is empty".to_owned(),
                });
            }
            match name {
                "version" => counters.processes = counters.processes.saturating_add(1),
                "data" => counters.absorb_data(event, line_number)?,
                _ => {}
            }
        }

        if counters.processes == 0 {
            return Err(TraceError::NoProcessEvents);
        }
        Ok(counters)
    }

    fn absorb_data(
        &mut self,
        event: &serde_json::Map<String, serde_json::Value>,
        line: usize,
    ) -> Result<(), TraceError> {
        let Some(key) = event.get("key").and_then(serde_json::Value::as_str) else {
            return Err(TraceError::InvalidEvent {
                line,
                reason: "data key is missing or not a string".to_owned(),
            });
        };
        if key.is_empty() {
            return Err(TraceError::InvalidEvent {
                line,
                reason: "data key is empty".to_owned(),
            });
        }
        let Some(raw_value) = event.get("value") else {
            return Err(TraceError::InvalidEvent {
                line,
                reason: format!("data value is missing for key {key}"),
            });
        };
        let value = numeric(raw_value);
        if is_counter_key(key) && value.is_none() {
            return Err(TraceError::InvalidEvent {
                line,
                reason: format!("counter value for key {key} is not an unsigned integer"),
            });
        }
        let Some(value) = value else {
            return Ok(());
        };
        match key {
            // transfer counters, emitted by fetch-pack / push
            "bytes-received" | "transfer/bytes-received" => {
                self.received_bytes = self.received_bytes.saturating_add(value);
            }
            "bytes-sent" | "transfer/bytes-sent" => {
                self.sent_bytes = self.sent_bytes.saturating_add(value);
            }
            "fetch/ref-count" | "ref-count" | "negotiated-refs" => {
                self.refs = self.refs.saturating_add(value);
            }
            "transfer/object-count" | "object-count" | "fetch/object-count" => {
                self.objects = self.objects.saturating_add(value);
            }
            _ => {}
        }
        Ok(())
    }

    /// Add counters from one isolated worker trace to this sample.
    pub(crate) fn merge(&mut self, other: Self) {
        self.received_bytes = self.received_bytes.saturating_add(other.received_bytes);
        self.sent_bytes = self.sent_bytes.saturating_add(other.sent_bytes);
        self.refs = self.refs.saturating_add(other.refs);
        self.objects = self.objects.saturating_add(other.objects);
        self.processes = self.processes.saturating_add(other.processes);
    }
}

fn is_counter_key(key: &str) -> bool {
    matches!(
        key,
        "bytes-received"
            | "transfer/bytes-received"
            | "bytes-sent"
            | "transfer/bytes-sent"
            | "fetch/ref-count"
            | "ref-count"
            | "negotiated-refs"
            | "transfer/object-count"
            | "object-count"
            | "fetch/object-count"
    )
}

/// trace2 writes `value` as a JSON number for some keys and a decimal string
/// for others; both are accepted for counters.
fn numeric(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

/// Environment pair that points git at an event file.
#[must_use]
pub fn trace_env(event_file: &Path) -> [(String, String); 2] {
    [
        (
            "GIT_TRACE2_EVENT".to_owned(),
            event_file.display().to_string(),
        ),
        ("GIT_TRACE2_EVENT_NESTING".to_owned(), "5".to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAM: &str = r#"{"event":"version","evt":"3","exe":"2.51.0"}
{"event":"data","key":"bytes-received","value":123456}
{"event":"data","key":"fetch/ref-count","value":"42"}
{"event":"data","key":"transfer/object-count","value":9001}
{"event":"data","key":"unrelated","value":7}
{"event":"version","evt":"3","exe":"2.51.0"}
{"event":"data","key":"bytes-received","value":1000}
{"event":"exit","code":0}
"#;

    #[test]
    fn counters_are_summed_across_processes() {
        let counters = GitCounters::from_events(STREAM).expect("valid trace");
        assert_eq!(counters.received_bytes, 124_456);
        assert_eq!(counters.refs, 42);
        assert_eq!(counters.objects, 9001);
        assert_eq!(counters.processes, 2);
        assert_eq!(counters.sent_bytes, 0);
    }

    #[test]
    fn an_empty_or_malformed_stream_is_rejected() {
        assert!(matches!(
            GitCounters::from_events(""),
            Err(TraceError::Empty)
        ));
        assert!(matches!(
            GitCounters::from_events("garbage\n{\"event\":\"data\"}\n"),
            Err(TraceError::MalformedJson { line: 1, .. })
        ));
    }

    #[test]
    fn a_stream_without_process_evidence_is_rejected() {
        assert!(matches!(
            GitCounters::from_events("{\"event\":\"exit\",\"code\":0}\n"),
            Err(TraceError::NoProcessEvents)
        ));
    }

    #[test]
    fn malformed_counter_data_is_rejected() {
        let error = GitCounters::from_events(
            "{\"event\":\"version\"}\n{\"event\":\"data\",\"key\":\"bytes-received\",\"value\":\"unknown\"}\n",
        )
        .expect_err("invalid counter must fail");
        assert!(matches!(error, TraceError::InvalidEvent { line: 2, .. }));
    }

    #[test]
    fn a_missing_trace_file_is_rejected() {
        let path =
            std::env::temp_dir().join(format!("velnor-bench-missing-trace-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            GitCounters::from_event_file(&path),
            Err(TraceError::Read { .. })
        ));
    }

    #[test]
    fn trace_env_points_at_the_event_file() {
        let path = std::path::Path::new("/tmp/velnor-trace.jsonl");
        let env = trace_env(path);
        assert_eq!(env[0].0, "GIT_TRACE2_EVENT");
        assert_eq!(env[0].1, "/tmp/velnor-trace.jsonl");
    }

    #[test]
    fn event_file_round_trips_from_disk() {
        let path = std::env::temp_dir().join(format!("velnor-bench-trace-{}", std::process::id()));
        std::fs::write(&path, STREAM).expect("write");
        let counters = GitCounters::from_event_file(&path).expect("read valid trace");
        assert_eq!(counters.received_bytes, 124_456);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn merge_sums_isolated_worker_counters() {
        let mut counters = GitCounters {
            received_bytes: 11,
            processes: 1,
            ..GitCounters::default()
        };
        counters.merge(GitCounters {
            received_bytes: 22,
            processes: 1,
            ..GitCounters::default()
        });
        assert_eq!(counters.received_bytes, 33);
        assert_eq!(counters.processes, 2);
    }
}
