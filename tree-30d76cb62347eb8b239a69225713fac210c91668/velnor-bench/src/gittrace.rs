//! GIT_TRACE2 event consumption.
//!
//! Checkout cost is dominated by bytes and refs moved, not by wall time alone.
//! Git already reports both through its `trace2` event stream, so the harness
//! sets `GIT_TRACE2_EVENT` to a file and reads the counters back rather than
//! asking the runner to compute them. Nothing here parses human-readable git
//! output: only the documented event JSON is read, and an unrecognised event is
//! ignored rather than guessed at.

use std::path::Path;

use serde::{Deserialize, Serialize};

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
    pub fn from_event_file(path: &Path) -> std::io::Result<Self> {
        Ok(Self::from_events(&std::fs::read_to_string(path)?))
    }

    /// Parse a trace2 event stream (one JSON object per line).
    #[must_use]
    pub fn from_events(stream: &str) -> Self {
        let mut counters = Self::default();
        for line in stream.lines() {
            let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            match event.get("event").and_then(serde_json::Value::as_str) {
                Some("version") => counters.processes = counters.processes.saturating_add(1),
                Some("data") => counters.absorb_data(&event),
                _ => {}
            }
        }
        counters
    }

    fn absorb_data(&mut self, event: &serde_json::Value) {
        let Some(key) = event.get("key").and_then(serde_json::Value::as_str) else {
            return;
        };
        let Some(value) = event.get("value").and_then(numeric) else {
            return;
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
    }
}

/// trace2 writes `value` as a JSON number for some keys and a decimal string
/// for others; both are accepted, anything else is ignored.
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
not json at all
{"event":"version","evt":"3","exe":"2.51.0"}
{"event":"data","key":"bytes-received","value":1000}
{"event":"exit","code":0}
"#;

    #[test]
    fn counters_are_summed_across_processes() {
        let counters = GitCounters::from_events(STREAM);
        assert_eq!(counters.received_bytes, 124_456);
        assert_eq!(counters.refs, 42);
        assert_eq!(counters.objects, 9001);
        assert_eq!(counters.processes, 2);
        assert_eq!(counters.sent_bytes, 0);
    }

    #[test]
    fn a_malformed_stream_yields_zeroes_not_a_panic() {
        assert_eq!(GitCounters::from_events(""), GitCounters::default());
        assert_eq!(
            GitCounters::from_events("garbage\n{\"event\":\"data\"}\n"),
            GitCounters::default()
        );
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
        let counters = GitCounters::from_event_file(&path).expect("read");
        assert_eq!(counters.received_bytes, 124_456);
        let _ = std::fs::remove_file(&path);
    }
}
