//! GIT_TRACE2 event consumption.
//!
//! Checkout cost is dominated by bytes and refs moved, not by wall time alone.
//! Git already reports both through its `trace2` event stream, so the harness
//! sets `GIT_TRACE2_EVENT` to a file and reads the counters back rather than
//! asking the runner to compute them. Nothing here parses human-readable git
//! output: only event JSON is read. An unrecognised but well-formed event is
//! ignored rather than guessed at; incomplete evidence is rejected. A complete
//! process with a non-zero exit is retained as observed-but-unsuccessful
//! evidence instead of being mistaken for a parser failure.

use std::{
    collections::BTreeMap,
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
    /// A process did not emit both lifecycle completion events.
    IncompleteProcess { sid: String, missing: &'static str },
    /// A process's exit and atexit codes do not match.
    MismatchedCompletion {
        sid: String,
        exit_code: i64,
        atexit_code: i64,
    },
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
            Self::IncompleteProcess { sid, missing } => {
                write!(formatter, "Git trace process {sid} is missing {missing}")
            }
            Self::MismatchedCompletion {
                sid,
                exit_code,
                atexit_code,
            } => write!(
                formatter,
                "Git trace process {sid} has exit code {exit_code} but atexit code {atexit_code}"
            ),
        }
    }
}

impl std::error::Error for TraceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::MalformedJson { source, .. } => Some(source),
            Self::Empty
            | Self::InvalidEvent { .. }
            | Self::IncompleteProcess { .. }
            | Self::MismatchedCompletion { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
struct ProcessTrace {
    exit_code: Option<i64>,
    atexit_code: Option<i64>,
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

/// A complete trace stream and the outcome reported by its Git processes.
///
/// `successful` is separate from parse validity: a failed Git command still
/// produced useful, attributable evidence when its lifecycle is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitTrace {
    /// Counters extracted from the complete event stream.
    pub counters: GitCounters,
    /// Whether every traced process completed with code zero.
    pub successful: bool,
}

impl GitTrace {
    /// Read a trace2 event file written by `GIT_TRACE2_EVENT`.
    ///
    /// # Errors
    /// The file could not be read or its event stream is incomplete/invalid.
    pub fn from_event_file(path: &Path) -> Result<Self, TraceError> {
        let stream = std::fs::read_to_string(path).map_err(|source| TraceError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_events(&stream)
    }

    /// Parse a trace2 event stream (one JSON object per line).
    ///
    /// Every line must be valid trace2 JSON, carry a process SID, and belong
    /// to a process with a complete `version` → `exit` → `atexit` lifecycle.
    /// A non-zero but matching completion code is retained in `successful`.
    ///
    /// # Errors
    /// The stream contains malformed events, an incomplete process lifecycle,
    /// or mismatched completion codes.
    pub fn from_events(stream: &str) -> Result<Self, TraceError> {
        GitCounters::parse_events(stream)
    }
}

/// Evidence state stored on a benchmark observation.
///
/// `NoGitTraceObserved` is only valid for an armed trace slot whose marker
/// survived the measured command without any Trace2 event. It is not a
/// synonym for zero counters or a parser fallback. `Mixed` preserves the
/// distinction when concurrent workers do not all emit Trace2 evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GitEvidence {
    /// This driver did not measure Git trace evidence.
    NotMeasured,
    /// The measured command produced no Git process trace after trace setup
    /// had been validated.
    NoGitTraceObserved,
    /// One or more Git processes emitted complete trace evidence.
    Observed {
        counters: GitCounters,
        /// False means at least one traced Git process completed non-zero.
        successful: bool,
    },
    /// Concurrent workers included both traced and Git-free workers.
    Mixed {
        counters: GitCounters,
        /// False means at least one observed Git process completed non-zero.
        successful: bool,
        observed_workers: u64,
        no_git_workers: u64,
    },
}

impl GitEvidence {
    /// Whether this value satisfies the structural invariants guaranteed by
    /// the trace reader.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        match self {
            Self::NotMeasured | Self::NoGitTraceObserved => true,
            Self::Observed { counters, .. } => counters.processes > 0,
            Self::Mixed {
                counters,
                observed_workers,
                no_git_workers,
                ..
            } => counters.processes > 0 && *observed_workers > 0 && *no_git_workers > 0,
        }
    }

    /// Bytes received by observed Git processes, or zero when none were
    /// observed. The evidence variant remains available to distinguish zero
    /// bytes from no measurement.
    #[must_use]
    pub const fn received_bytes(&self) -> u64 {
        match self {
            Self::NotMeasured | Self::NoGitTraceObserved => 0,
            Self::Observed { counters, .. } | Self::Mixed { counters, .. } => {
                counters.received_bytes
            }
        }
    }
}

impl GitCounters {
    /// Read a trace2 event file written by `GIT_TRACE2_EVENT`.
    ///
    /// # Errors
    /// The file could not be read or its event stream is incomplete/invalid.
    pub fn from_event_file(path: &Path) -> Result<Self, TraceError> {
        GitTrace::from_event_file(path).map(|trace| trace.counters)
    }

    /// Parse a trace2 event stream and retain its completion status.
    ///
    /// # Errors
    /// The file could not be read or its event stream is incomplete/invalid.
    pub fn from_events(stream: &str) -> Result<Self, TraceError> {
        GitTrace::from_events(stream).map(|trace| trace.counters)
    }

    fn parse_events(stream: &str) -> Result<GitTrace, TraceError> {
        if stream.trim().is_empty() {
            return Err(TraceError::Empty);
        }

        let mut counters = Self::default();
        let mut processes = BTreeMap::<String, ProcessTrace>::new();
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
            let Some(sid) = event.get("sid").and_then(serde_json::Value::as_str) else {
                return Err(TraceError::InvalidEvent {
                    line: line_number,
                    reason: "sid is missing or not a string".to_owned(),
                });
            };
            if sid.trim().is_empty() {
                return Err(TraceError::InvalidEvent {
                    line: line_number,
                    reason: "sid is empty".to_owned(),
                });
            }
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
            if let Some(process) = processes.get(sid)
                && process.exit_code.is_some()
                && process.atexit_code.is_none()
                && name != "atexit"
            {
                return Err(TraceError::InvalidEvent {
                    line: line_number,
                    reason: format!("event {name} appears after exit before atexit for sid {sid}"),
                });
            }
            match name {
                "version" => {
                    if processes
                        .insert(sid.to_owned(), ProcessTrace::default())
                        .is_some()
                    {
                        return Err(TraceError::InvalidEvent {
                            line: line_number,
                            reason: format!("duplicate version event for sid {sid}"),
                        });
                    }
                    counters.processes = counters.processes.saturating_add(1);
                }
                _ => {
                    let Some(process) = processes.get_mut(sid) else {
                        return Err(TraceError::InvalidEvent {
                            line: line_number,
                            reason: format!("event {name} appears before version for sid {sid}"),
                        });
                    };
                    if process.atexit_code.is_some() {
                        return Err(TraceError::InvalidEvent {
                            line: line_number,
                            reason: format!("event {name} appears after atexit for sid {sid}"),
                        });
                    }
                    match name {
                        "data" => counters.absorb_data(event, line_number)?,
                        "exit" => {
                            if process.exit_code.is_some() {
                                return Err(TraceError::InvalidEvent {
                                    line: line_number,
                                    reason: format!("duplicate exit event for sid {sid}"),
                                });
                            }
                            process.exit_code = Some(completion_code(event, line_number, name)?);
                        }
                        "atexit" => {
                            if process.exit_code.is_none() {
                                return Err(TraceError::InvalidEvent {
                                    line: line_number,
                                    reason: format!("atexit appears before exit for sid {sid}"),
                                });
                            }
                            if process.atexit_code.is_some() {
                                return Err(TraceError::InvalidEvent {
                                    line: line_number,
                                    reason: format!("duplicate atexit event for sid {sid}"),
                                });
                            }
                            let code = completion_code(event, line_number, name)?;
                            let Some(exit_code) = process.exit_code else {
                                return Err(TraceError::InvalidEvent {
                                    line: line_number,
                                    reason: format!("atexit appears before exit for sid {sid}"),
                                });
                            };
                            if exit_code != code {
                                return Err(TraceError::MismatchedCompletion {
                                    sid: sid.to_owned(),
                                    exit_code,
                                    atexit_code: code,
                                });
                            }
                            process.atexit_code = Some(code);
                        }
                        _ => {}
                    }
                }
            }
        }

        let mut successful = true;
        for (sid, process) in processes {
            let Some(exit_code) = process.exit_code else {
                return Err(TraceError::IncompleteProcess {
                    sid,
                    missing: "exit and atexit",
                });
            };
            let Some(atexit_code) = process.atexit_code else {
                return Err(TraceError::IncompleteProcess {
                    sid,
                    missing: "atexit",
                });
            };
            if exit_code != atexit_code {
                return Err(TraceError::MismatchedCompletion {
                    sid,
                    exit_code,
                    atexit_code,
                });
            }
            successful &= exit_code == 0;
        }
        Ok(GitTrace {
            counters,
            successful,
        })
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

fn completion_code(
    event: &serde_json::Map<String, serde_json::Value>,
    line: usize,
    name: &str,
) -> Result<i64, TraceError> {
    event
        .get("code")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| TraceError::InvalidEvent {
            line,
            reason: format!("{name} completion code is missing or not an integer"),
        })
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

    const STREAM: &str = r#"{"event":"version","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000001Z","evt":"4","exe":"2.50.1"}
{"event":"start","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000002Z","argv":["git","fetch"]}
{"event":"data","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000003Z","key":"bytes-received","value":123456}
{"event":"data","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000004Z","key":"fetch/ref-count","value":"42"}
{"event":"data","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000005Z","key":"transfer/object-count","value":9001}
{"event":"data","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000006Z","key":"unrelated","value":7}
{"event":"exit","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000007Z","code":0}
{"event":"atexit","sid":"20260905T000000.000000Z-H00000001-P00000001","thread":"main","time":"2026-09-05T00:00:00.000008Z","code":0}
{"event":"version","sid":"20260905T000000.000000Z-H00000002-P00000002","thread":"main","time":"2026-09-05T00:00:01.000001Z","evt":"4","exe":"2.50.1"}
{"event":"data","sid":"20260905T000000.000000Z-H00000002-P00000002","thread":"main","time":"2026-09-05T00:00:01.000002Z","key":"bytes-received","value":1000}
{"event":"exit","sid":"20260905T000000.000000Z-H00000002-P00000002","thread":"main","time":"2026-09-05T00:00:01.000003Z","code":0}
{"event":"atexit","sid":"20260905T000000.000000Z-H00000002-P00000002","thread":"main","time":"2026-09-05T00:00:01.000004Z","code":0}
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
    fn a_version_without_sid_is_rejected() {
        let error = GitCounters::from_events(r#"{"event":"version","evt":"4"}"#)
            .expect_err("missing sid must fail");
        assert!(matches!(error, TraceError::InvalidEvent { line: 1, .. }));
        assert!(error.to_string().contains("sid"));
    }

    #[test]
    fn a_whitespace_only_sid_is_rejected() {
        let error = GitCounters::from_events(r#"{"event":"version","sid":"   ","evt":"4"}"#)
            .expect_err("blank sid must fail");
        assert!(matches!(error, TraceError::InvalidEvent { line: 1, .. }));
        assert!(error.to_string().contains("sid"));
    }

    #[test]
    fn a_truncated_process_is_rejected_without_fabricated_counters() {
        let error = GitCounters::from_events(
            r#"{"event":"version","sid":"sid-1","evt":"4","exe":"2.50.1"}"#,
        )
        .expect_err("missing lifecycle must fail");
        assert!(matches!(
            error,
            TraceError::IncompleteProcess {
                missing: "exit and atexit",
                ..
            }
        ));
    }

    #[test]
    fn a_missing_atexit_is_rejected() {
        let error = GitCounters::from_events(
            r#"{"event":"version","sid":"sid-1","evt":"4"}
{"event":"exit","sid":"sid-1","code":0}"#,
        )
        .expect_err("missing atexit must fail");
        assert!(matches!(
            error,
            TraceError::IncompleteProcess {
                missing: "atexit",
                ..
            }
        ));
    }

    #[test]
    fn malformed_completion_is_rejected() {
        let error = GitCounters::from_events(
            r#"{"event":"version","sid":"sid-1","evt":"4"}
{"event":"exit","sid":"sid-1"}
{"event":"atexit","sid":"sid-1","code":0}"#,
        )
        .expect_err("missing completion code must fail");
        assert!(matches!(error, TraceError::InvalidEvent { line: 2, .. }));
    }

    #[test]
    fn mismatched_completion_is_rejected() {
        let error = GitCounters::from_events(
            r#"{"event":"version","sid":"sid-1","evt":"4"}
{"event":"exit","sid":"sid-1","code":0}
{"event":"atexit","sid":"sid-1","code":1}"#,
        )
        .expect_err("mismatched completion must fail");
        assert!(matches!(error, TraceError::MismatchedCompletion { .. }));
    }

    #[test]
    fn data_after_exit_before_atexit_is_rejected() {
        let error = GitCounters::from_events(
            r#"{"event":"version","sid":"sid-1","evt":"4"}
{"event":"exit","sid":"sid-1","code":0}
{"event":"data","sid":"sid-1","key":"bytes-received","value":7}
{"event":"atexit","sid":"sid-1","code":0}"#,
        )
        .expect_err("data after exit must fail");
        assert!(matches!(error, TraceError::InvalidEvent { line: 3, .. }));
        assert!(error.to_string().contains("after exit"));
    }

    #[test]
    fn event_after_exit_before_atexit_is_rejected() {
        let error = GitCounters::from_events(
            r#"{"event":"version","sid":"sid-1","evt":"4"}
{"event":"start","sid":"sid-1","argv":["git","fetch"]}
{"event":"exit","sid":"sid-1","code":0}
{"event":"child","sid":"sid-1","child_id":"child-1"}
{"event":"atexit","sid":"sid-1","code":0}"#,
        )
        .expect_err("event after exit must fail");
        assert!(matches!(error, TraceError::InvalidEvent { line: 4, .. }));
        assert!(error.to_string().contains("after exit"));
    }

    #[test]
    fn nonzero_completion_is_retained_as_unsuccessful_evidence() {
        let trace = GitTrace::from_events(
            r#"{"event":"version","sid":"sid-1","evt":"4"}
{"event":"exit","sid":"sid-1","code":7}
{"event":"atexit","sid":"sid-1","code":7}"#,
        )
        .expect("complete nonzero lifecycle is parseable");
        assert_eq!(trace.counters.processes, 1);
        assert!(!trace.successful);
    }

    #[test]
    fn an_event_without_a_process_version_is_rejected() {
        assert!(matches!(
            GitCounters::from_events("{\"event\":\"exit\",\"sid\":\"sid-1\",\"code\":0}\n"),
            Err(TraceError::InvalidEvent { line: 1, .. })
        ));
    }

    #[test]
    fn malformed_counter_data_is_rejected() {
        let error = GitCounters::from_events(
            "{\"event\":\"version\",\"sid\":\"sid-1\"}\n{\"event\":\"data\",\"sid\":\"sid-1\",\"key\":\"bytes-received\",\"value\":\"unknown\"}\n",
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
