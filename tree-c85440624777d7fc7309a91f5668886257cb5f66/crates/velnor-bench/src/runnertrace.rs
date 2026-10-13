//! Consumption of the runner's per-job host `docker` counters.
//!
//! `crates/velnor-runner/src/docker/metrics.rs` counts every host `docker`
//! process a job spawns, classified by
//! `crates/velnor-runner/src/docker/deadline.rs::DockerOp`, and reports the
//! totals from a guard that fires however the job ends. That counter lives in
//! process globals inside the runner, so no other process can read it directly:
//! the seam it is published through is the `tracing` event the guard emits, and
//! the runner's file layer writes those events as JSON to
//! `<config-base>/logs/trace.jsonl` (`crates/velnor-runner/src/telemetry.rs`).
//!
//! This module is that seam's reader. It parses only fields the runner emits by
//! name; it never derives a class from an argument vector, because the runner
//! deliberately does not put one in the log. An unrecognised line is skipped
//! rather than guessed at, and a trace with no job scope in it yields an empty
//! census rather than a zero.
//!
//! Two levels are available and the harness reports which it found:
//!
//! * the per-job summary (`INFO`), always present in a runner log;
//! * the per-invocation events (`DEBUG`), present only when the runner was run
//!   with a filter that admits `velnor.docker` at debug level. Without them the
//!   per-class latency is a sum over the job, not a sample, so no percentile
//!   over individual calls can be computed and none is reported.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// `tracing` target the runner's docker accounting writes under.
pub const DOCKER_TARGET: &str = "velnor.docker";
/// Message of the per-job summary event.
pub const JOB_SUMMARY_MESSAGE: &str = "host docker invocations for job";
/// Message of the per-invocation event.
pub const INVOCATION_MESSAGE: &str = "host docker invocation";

/// One host `docker` invocation, as the runner reported it.
///
/// Only present when the runner logged `velnor.docker` at debug level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerCall {
    /// Operation class label from `DockerOp::label`.
    pub class: String,
    pub latency_ms: u64,
    pub exit_code: i64,
    pub timed_out: bool,
}

/// One job's host `docker` process census.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobDockerCensus {
    pub job_id: String,
    /// Host `docker` processes the job spawned.
    pub invocations: u64,
    /// `class -> invocations`.
    pub calls_by_class: BTreeMap<String, u64>,
    /// `class -> summed wall milliseconds`. This is a total, never a mean.
    pub latency_ms_by_class: BTreeMap<String, u64>,
    /// Summed wall milliseconds across every class.
    pub wall_ms: u64,
    pub timeouts: u64,
    pub failures: u64,
    /// Individual calls, when the runner emitted them at debug level.
    pub calls: Vec<DockerCall>,
}

impl JobDockerCensus {
    /// Mean wall milliseconds per call of one class, or `None` when the class
    /// did not occur.
    #[must_use]
    pub fn mean_latency_ms(&self, class: &str) -> Option<f64> {
        let count = *self.calls_by_class.get(class)?;
        if count == 0 {
            return None;
        }
        let total = *self.latency_ms_by_class.get(class).unwrap_or(&0);
        // Millisecond counts are far below 2^53, so the conversion is exact.
        Some(total as f64 / count as f64)
    }

    /// Read every job census in a runner trace file.
    ///
    /// # Errors
    /// The file could not be read.
    pub fn from_trace_file(path: &Path) -> std::io::Result<Vec<Self>> {
        Ok(Self::from_trace(&std::fs::read_to_string(path)?))
    }

    /// Parse a runner `trace.jsonl` stream.
    ///
    /// Per-invocation debug events are attributed to the job summary that
    /// follows them, which is the order the runner writes them in: the guard
    /// reports on drop, after every call it counted.
    #[must_use]
    pub fn from_trace(stream: &str) -> Vec<Self> {
        let mut pending: Vec<DockerCall> = Vec::new();
        let mut census = Vec::new();
        for line in stream.lines() {
            let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if event.get("target").and_then(serde_json::Value::as_str) != Some(DOCKER_TARGET) {
                continue;
            }
            let fields = fields_of(&event);
            match text(&fields, "message") {
                Some(INVOCATION_MESSAGE) => {
                    if let Some(call) = invocation(&fields) {
                        pending.push(call);
                    }
                }
                Some(JOB_SUMMARY_MESSAGE) => {
                    census.push(summary(&fields, std::mem::take(&mut pending)));
                }
                _ => {}
            }
        }
        census
    }
}

/// The runner's JSON layer nests event fields under `fields`; a layer
/// configured to flatten them puts them at the top level. Accept both, because
/// which one a trace uses is a subscriber setting, not a contract.
fn fields_of(event: &serde_json::Value) -> serde_json::Value {
    event.get("fields").map_or_else(
        || event.clone(),
        |fields| {
            let mut merged = fields.clone();
            if let (Some(object), Some(message)) = (
                merged.as_object_mut(),
                event.get("message").and_then(serde_json::Value::as_str),
            ) {
                object
                    .entry("message")
                    .or_insert_with(|| serde_json::Value::String(message.to_owned()));
            }
            merged
        },
    )
}

fn text<'a>(fields: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    fields.get(key).and_then(serde_json::Value::as_str)
}

fn count(fields: &serde_json::Value, key: &str) -> u64 {
    fields
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

fn invocation(fields: &serde_json::Value) -> Option<DockerCall> {
    Some(DockerCall {
        class: text(fields, "docker_op")?.to_owned(),
        latency_ms: count(fields, "docker_latency_ms"),
        exit_code: fields
            .get("docker_exit_code")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0),
        timed_out: fields
            .get("docker_timed_out")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn summary(fields: &serde_json::Value, calls: Vec<DockerCall>) -> JobDockerCensus {
    JobDockerCensus {
        job_id: text(fields, "job_id").unwrap_or_default().to_owned(),
        invocations: count(fields, "docker_invocations"),
        calls_by_class: class_map(text(fields, "docker_invocations_by_class").unwrap_or("")),
        latency_ms_by_class: class_map(text(fields, "docker_latency_ms_by_class").unwrap_or("")),
        wall_ms: count(fields, "docker_wall_ms"),
        timeouts: count(fields, "docker_timeouts"),
        failures: count(fields, "docker_failures"),
        calls,
    }
}

/// Parse the runner's `class=value,class=value` encoding. An entry whose value
/// is not a number is dropped, never defaulted to zero: a zero would read as a
/// measured absence.
fn class_map(encoded: &str) -> BTreeMap<String, u64> {
    encoded
        .split(',')
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let (class, value) = entry.split_once('=')?;
            Some((class.to_owned(), value.parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = r#"{"timestamp":"t","level":"INFO","fields":{"message":"unrelated"},"target":"velnor.runner"}
{"timestamp":"t","level":"DEBUG","fields":{"message":"host docker invocation","docker_op":"query","docker_latency_ms":12,"docker_exit_code":0,"docker_timed_out":false,"docker_invocation":1},"target":"velnor.docker"}
{"timestamp":"t","level":"DEBUG","fields":{"message":"host docker invocation","docker_op":"remove","docker_latency_ms":20000,"docker_exit_code":124,"docker_timed_out":true,"docker_invocation":2},"target":"velnor.docker"}
not json
{"timestamp":"t","level":"INFO","fields":{"message":"host docker invocations for job","job_id":"job-1","docker_invocations":14,"docker_invocations_by_class":"daemon-query=1,query=4,create=1,start=1,remove=7","docker_latency_ms_by_class":"daemon-query=310,query=160,create=90,start=240,remove=20140","docker_wall_ms":20940,"docker_timeouts":1,"docker_failures":1},"target":"velnor.docker"}
{"timestamp":"t","level":"INFO","fields":{"message":"host docker invocations for job","job_id":"job-2","docker_invocations":0,"docker_invocations_by_class":"","docker_latency_ms_by_class":"","docker_wall_ms":0,"docker_timeouts":0,"docker_failures":0},"target":"velnor.docker"}
"#;

    #[test]
    fn a_job_summary_is_read_back_class_by_class() {
        let census = JobDockerCensus::from_trace(TRACE);
        assert_eq!(census.len(), 2);
        let first = &census[0];
        assert_eq!(first.job_id, "job-1");
        assert_eq!(first.invocations, 14);
        assert_eq!(first.calls_by_class["remove"], 7);
        assert_eq!(first.latency_ms_by_class["remove"], 20_140);
        assert_eq!(first.wall_ms, 20_940);
        assert_eq!(first.timeouts, 1);
        assert_eq!(first.failures, 1);
        // 14 is the sum of the per-class counts; the two must agree or the
        // runner dropped a class.
        assert_eq!(first.calls_by_class.values().sum::<u64>(), 14);
    }

    #[test]
    fn per_call_debug_events_attach_to_the_job_that_follows_them() {
        let census = JobDockerCensus::from_trace(TRACE);
        assert_eq!(census[0].calls.len(), 2);
        assert_eq!(census[0].calls[1].class, "remove");
        assert!(census[0].calls[1].timed_out);
        assert_eq!(census[0].calls[1].exit_code, 124);
        // The second job saw none, and must not inherit the first job's.
        assert!(census[1].calls.is_empty());
    }

    #[test]
    fn mean_latency_is_only_defined_for_classes_that_occurred() {
        let census = JobDockerCensus::from_trace(TRACE);
        assert!((census[0].mean_latency_ms("query").expect("query") - 40.0).abs() < f64::EPSILON);
        assert_eq!(census[0].mean_latency_ms("prune"), None);
        assert_eq!(census[1].mean_latency_ms("query"), None);
    }

    #[test]
    fn a_trace_with_no_docker_scope_yields_no_census_rather_than_a_zero() {
        assert!(JobDockerCensus::from_trace("").is_empty());
        assert!(JobDockerCensus::from_trace(
            "{\"target\":\"velnor.runner\",\"fields\":{\"message\":\"hi\"}}\n"
        )
        .is_empty());
    }

    #[test]
    fn flattened_events_parse_identically() {
        let flat = "{\"target\":\"velnor.docker\",\"message\":\"host docker invocations for job\",\
                    \"job_id\":\"job-9\",\"docker_invocations\":3,\
                    \"docker_invocations_by_class\":\"query=3\",\
                    \"docker_latency_ms_by_class\":\"query=30\",\"docker_wall_ms\":30,\
                    \"docker_timeouts\":0,\"docker_failures\":0}\n";
        let census = JobDockerCensus::from_trace(flat);
        assert_eq!(census.len(), 1);
        assert_eq!(census[0].job_id, "job-9");
        assert_eq!(census[0].calls_by_class["query"], 3);
    }

    #[test]
    fn a_malformed_class_entry_is_dropped_not_defaulted() {
        let map = class_map("query=3,broken,create=x,start=1");
        assert_eq!(map.len(), 2);
        assert_eq!(map["query"], 3);
        assert_eq!(map["start"], 1);
        assert!(!map.contains_key("create"));
    }

    #[test]
    fn a_trace_file_round_trips_from_disk() {
        let path = std::env::temp_dir().join(format!(
            "velnor-bench-runnertrace-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, TRACE).expect("write");
        let census = JobDockerCensus::from_trace_file(&path).expect("read");
        assert_eq!(census.len(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
