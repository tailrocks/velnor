//! Per-job Docker invocation census, in the runner's own vocabulary.
//!
//! The runner classifies every host `docker` invocation into the closed
//! [`DockerOp`](velnor_runner::docker::DockerOp) vocabulary and counts
//! invocations and latency per class (`velnor-runner/src/docker/metrics.rs`).
//! This module is the benchmark side of that contract: it derives the same
//! census from the harness's recorded invocations using the runner's own
//! [`classify`](velnor_runner::docker::classify) function, so there is exactly
//! one classifier and one vocabulary. A census label is therefore always one
//! of the twelve [`DockerOp::label`](velnor_runner::docker::DockerOp::label)
//! strings, and [`BenchRecord`](crate::record::BenchRecord) validation rejects
//! anything else.
//!
//! Until the `velnor-job` driver exists, this census covers the invocations the
//! harness itself spawns, which is a strict subset of a real job's census.
//! When the driver lands, the same per-class shape is filled from the runner's
//! `velnor.docker` totals instead; the record shape does not change.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use velnor_runner::docker::{classify, DockerOp};

use crate::sys::Invocation;

/// Measured cost of one Docker operation class within one iteration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassObservation {
    pub count: u64,
    pub latency_ms: u64,
}

/// Census of one iteration: per-class counts and latencies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerCensus {
    /// Keyed by [`DockerOp::label`](velnor_runner::docker::DockerOp::label).
    pub by_class: BTreeMap<String, ClassObservation>,
}

impl DockerCensus {
    /// Classify every `docker` invocation in order. Non-docker programs are
    /// ignored: the census counts Docker invocations, not all processes.
    #[must_use]
    pub fn from_invocations(invocations: &[Invocation]) -> Self {
        let mut by_class: BTreeMap<String, ClassObservation> = BTreeMap::new();
        for invocation in invocations
            .iter()
            .filter(|invocation| invocation.program == "docker")
        {
            let label = classify(&invocation.args).label().to_owned();
            let entry = by_class.entry(label).or_default();
            entry.count += 1;
            let millis = u64::try_from(invocation.wall.as_millis()).unwrap_or(u64::MAX);
            entry.latency_ms = entry.latency_ms.saturating_add(millis);
        }
        Self { by_class }
    }

    /// Total invocations across all classes. Always equals the number of
    /// `docker` invocations the census was derived from.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.by_class.values().map(|class| class.count).sum()
    }

    /// True when every label is a member of the runner's closed vocabulary.
    #[must_use]
    pub fn labels_are_known(&self) -> bool {
        self.by_class
            .keys()
            .all(|label| DockerOp::ALL.iter().any(|op| op.label() == label))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn invocation(args: &[&str], wall_ms: u64) -> Invocation {
        Invocation {
            program: "docker".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            code: 0,
            stdout: String::new(),
            stderr: String::new(),
            wall: Duration::from_millis(wall_ms),
        }
    }

    #[test]
    fn the_census_uses_the_runner_vocabulary_and_sums_latency() {
        let invocations = vec![
            invocation(&["inspect", "abc"], 5),
            invocation(&["inspect", "def"], 7),
            invocation(&["rm", "-f", "abc"], 20),
            invocation(&["images"], 3),
        ];
        let census = DockerCensus::from_invocations(&invocations);
        assert_eq!(
            census.by_class.get("query"),
            Some(&ClassObservation {
                count: 3,
                latency_ms: 15,
            })
        );
        assert_eq!(
            census.by_class.get("remove"),
            Some(&ClassObservation {
                count: 1,
                latency_ms: 20,
            })
        );
        assert_eq!(census.total(), 4);
        assert!(census.labels_are_known());
    }

    #[test]
    fn non_docker_programs_are_not_counted() {
        let mut git = invocation(&["fetch"], 9);
        git.program = "git".to_owned();
        let mut cargo = invocation(&["build"], 11);
        cargo.program = "cargo".to_owned();
        let census = DockerCensus::from_invocations(&[git, cargo, invocation(&["ps"], 2)]);
        assert_eq!(census.total(), 1);
        assert_eq!(census.by_class.len(), 1);
    }

    #[test]
    fn an_empty_invocation_list_yields_an_empty_census() {
        let census = DockerCensus::from_invocations(&[]);
        assert_eq!(census.total(), 0);
        assert!(census.by_class.is_empty());
        assert!(census.labels_are_known());
    }

    #[test]
    fn an_unknown_subcommand_lands_in_unclassified_not_in_a_new_label() {
        let census = DockerCensus::from_invocations(&[invocation(&["frobnicate", "--qux"], 1)]);
        assert_eq!(census.by_class.len(), 1);
        assert!(census.by_class.contains_key("unclassified"));
        assert!(census.labels_are_known());
    }

    #[test]
    fn every_runner_class_label_is_accepted_and_nothing_else_is() {
        for op in DockerOp::ALL {
            let census = DockerCensus {
                by_class: BTreeMap::from([(
                    op.label().to_owned(),
                    ClassObservation {
                        count: 1,
                        latency_ms: 1,
                    },
                )]),
            };
            assert!(census.labels_are_known(), "{}", op.label());
        }
        let census = DockerCensus {
            by_class: BTreeMap::from([(
                "docker-exec".to_owned(),
                ClassObservation {
                    count: 1,
                    latency_ms: 1,
                },
            )]),
        };
        assert!(!census.labels_are_known());
    }

    #[test]
    fn the_census_round_trips_through_json() {
        let census = DockerCensus::from_invocations(&[invocation(&["ps"], 2)]);
        let json = serde_json::to_string(&census).expect("serialise");
        let parsed: DockerCensus = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, census);
    }
}
