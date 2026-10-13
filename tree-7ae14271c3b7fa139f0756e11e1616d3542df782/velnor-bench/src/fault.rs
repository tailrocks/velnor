//! The fault catalogue: 27 failure classes across four injection seams.
//!
//! BC-27 recorded the starting point: fault-injection coverage was 6 HTTP
//! cases in one file, 21 of 27 fault classes untested, no soak suite. This
//! module names all 27 classes so every injection and every fault scenario
//! points at a catalogue entry instead of an ad-hoc failure.
//!
//! The four seams:
//!
//! * [`FaultSeam::Process`] — every host process spawn passes through the
//!   runner's `CommandRunner`, so one decorator injects Docker and git faults
//!   (`velnor-runner/src/fault_injection.rs`).
//! * [`FaultSeam::Http`] — broker, run-service and cache traffic. Injection
//!   needs a scripted transport, which does not exist yet; these classes are
//!   declared and reported as unrun.
//! * [`FaultSeam::Filesystem`] — scratch pressure and crash recovery. Soak and
//!   the disk monitors observe the first; the second needs a killable runner.
//! * [`FaultSeam::Signal`] — cancellation and timeouts delivered as signals.
//!
//! Four classes are measurable today through the `docker-direct` fallback: the
//! harness injects a real fault into a real container lifecycle and asserts
//! the real containment. The rest need the `velnor-job` driver and stay
//! declared-but-unrun until it exists.

use serde::{Deserialize, Serialize};

/// Where a fault is injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultSeam {
    /// Host process spawns through `CommandRunner` (docker CLI, git).
    Process,
    /// Broker, run-service and cache HTTP traffic.
    Http,
    /// Scratch disk pressure and daemon crash recovery.
    Filesystem,
    /// Cancellation and timeout signals.
    Signal,
}

impl FaultSeam {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Http => "http",
            Self::Filesystem => "filesystem",
            Self::Signal => "signal",
        }
    }
}

/// One named failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultClass {
    /// Stable identifier, referenced by `FaultOutcome` and the runner decorator.
    pub id: &'static str,
    pub seam: FaultSeam,
    /// How the fault is triggered.
    pub trigger: &'static str,
    /// The production behaviour a passing run proves.
    pub containment: &'static str,
    /// Matrix scenario that measures it, when one can run today.
    pub bench_scenario: Option<&'static str>,
}

/// All 27 classes. The count is asserted by
/// `the_catalogue_names_twenty_seven_classes`.
pub const FAULT_CATALOGUE: &[FaultClass] = &[
    // Process seam: Docker faults.
    FaultClass {
        id: "docker-daemon-unreachable",
        seam: FaultSeam::Process,
        trigger: "every docker spawn fails to connect",
        containment: "the job fails before the first container; no residue",
        bench_scenario: None,
    },
    FaultClass {
        id: "docker-command-timeout",
        seam: FaultSeam::Process,
        trigger: "the daemon never answers a control-plane call",
        containment: "the class deadline fires and the slot is freed",
        bench_scenario: None,
    },
    FaultClass {
        id: "docker-image-pull-failure",
        seam: FaultSeam::Process,
        trigger: "the registry answers 404 or denies access",
        containment: "the job fails at setup with the registry error",
        bench_scenario: None,
    },
    FaultClass {
        id: "docker-container-start-failure",
        seam: FaultSeam::Process,
        trigger: "the OCI runtime refuses the created container",
        containment: "the job fails and the dead container is removed",
        bench_scenario: None,
    },
    FaultClass {
        id: "docker-container-oomkilled",
        seam: FaultSeam::Process,
        trigger: "the step exceeds its memory limit (exit 137)",
        containment: "the step fails as OOM; teardown completes",
        bench_scenario: None,
    },
    FaultClass {
        id: "docker-network-conflict",
        seam: FaultSeam::Process,
        trigger: "the job network already exists",
        containment: "the conflict is contained and owned objects are removed",
        bench_scenario: Some("fault/network-conflict"),
    },
    FaultClass {
        id: "docker-volume-mount-failure",
        seam: FaultSeam::Process,
        trigger: "a host mount source is missing",
        containment: "the job fails at create with the mount error",
        bench_scenario: None,
    },
    FaultClass {
        id: "docker-exec-nonzero",
        seam: FaultSeam::Process,
        trigger: "the user command exits non-zero",
        containment: "the exit code is recorded and teardown completes",
        bench_scenario: Some("fault/step-command-fails"),
    },
    FaultClass {
        id: "docker-kill-mid-step",
        seam: FaultSeam::Process,
        trigger: "the container is SIGKILLed while the step runs",
        containment: "the step is marked failed and teardown completes",
        bench_scenario: Some("fault/container-killed-mid-step"),
    },
    FaultClass {
        id: "docker-disk-pressure",
        seam: FaultSeam::Process,
        trigger: "the Engine reports no space left on device",
        containment: "the job fails with a capacity note; GC reclaims",
        bench_scenario: None,
    },
    FaultClass {
        id: "docker-object-missing",
        seam: FaultSeam::Process,
        trigger: "an inspect or removal names an absent object",
        containment: "a clean not-found error; no residue",
        bench_scenario: Some("fault/object-missing"),
    },
    // Process seam: git faults.
    FaultClass {
        id: "git-remote-unreachable",
        seam: FaultSeam::Process,
        trigger: "DNS or connect fails for the git remote",
        containment: "checkout fails loudly; the job never runs unowned code",
        bench_scenario: None,
    },
    FaultClass {
        id: "git-auth-failure",
        seam: FaultSeam::Process,
        trigger: "the remote answers 401 or 403",
        containment: "checkout fails without logging the token",
        bench_scenario: None,
    },
    FaultClass {
        id: "git-fetch-failure",
        seam: FaultSeam::Process,
        trigger: "the remote hangs up mid-fetch",
        containment: "checkout aborts; no partial tree is used",
        bench_scenario: None,
    },
    FaultClass {
        id: "git-mirror-corruption",
        seam: FaultSeam::Process,
        trigger: "the shared mirror holds a corrupt object",
        containment: "the mirror is repaired or checkout fails closed",
        bench_scenario: None,
    },
    // HTTP seam: broker, run-service, cache.
    FaultClass {
        id: "broker-long-poll-timeout",
        seam: FaultSeam::Http,
        trigger: "the long poll expires with no message",
        containment: "the slot re-polls; no job is lost or duplicated",
        bench_scenario: None,
    },
    FaultClass {
        id: "broker-5xx",
        seam: FaultSeam::Http,
        trigger: "the broker answers 5xx",
        containment: "retried with backoff; no duplicate acquisition",
        bench_scenario: None,
    },
    FaultClass {
        id: "broker-auth-expired",
        seam: FaultSeam::Http,
        trigger: "the broker answers 401",
        containment: "acquisition stops; no unowned job runs",
        bench_scenario: None,
    },
    FaultClass {
        id: "run-service-acquire-conflict",
        seam: FaultSeam::Http,
        trigger: "the job was taken by another runner",
        containment: "the message is dropped; the next one is processed",
        bench_scenario: None,
    },
    FaultClass {
        id: "timeline-upload-failure",
        seam: FaultSeam::Http,
        trigger: "a timeline POST fails",
        containment: "retried; step output is held locally until acked",
        bench_scenario: None,
    },
    FaultClass {
        id: "completion-delivery-failure",
        seam: FaultSeam::Http,
        trigger: "the completion call fails",
        containment: "retried at-least-once; never double-completed",
        bench_scenario: None,
    },
    FaultClass {
        id: "cache-service-unavailable",
        seam: FaultSeam::Http,
        trigger: "the cache service is down",
        containment: "the job runs without cache; no failure",
        bench_scenario: None,
    },
    FaultClass {
        id: "cache-entry-corrupt",
        seam: FaultSeam::Http,
        trigger: "a cache entry fails its hash",
        containment: "the entry is discarded and rebuilt",
        bench_scenario: None,
    },
    // Filesystem seam.
    FaultClass {
        id: "host-scratch-disk-full",
        seam: FaultSeam::Filesystem,
        trigger: "the scratch filesystem returns ENOSPC",
        containment: "the job fails with a capacity note; GC reclaims",
        bench_scenario: None,
    },
    FaultClass {
        id: "runner-restart-mid-job",
        seam: FaultSeam::Filesystem,
        trigger: "the slot process dies mid-job",
        containment: "recovery reaps the job exactly once from durable state",
        bench_scenario: None,
    },
    // Signal seam.
    FaultClass {
        id: "cancel-during-step",
        seam: FaultSeam::Signal,
        trigger: "cancellation arrives while a step runs",
        containment: "the step stops, cancelled() holds, post steps run",
        bench_scenario: None,
    },
    FaultClass {
        id: "step-timeout",
        seam: FaultSeam::Signal,
        trigger: "timeout-minutes expires as a wall clock",
        containment: "the step is killed and the job fails",
        bench_scenario: None,
    },
];

/// Look one class up by id.
#[must_use]
pub fn find(id: &str) -> Option<&'static FaultClass> {
    FAULT_CATALOGUE.iter().find(|class| class.id == id)
}

/// What one fault-scenario iteration proved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultOutcome {
    /// Catalogue id of the injected fault.
    pub class: String,
    /// The fault was actually triggered. A run that never triggered its fault
    /// is not a measurement of anything.
    pub injected: bool,
    /// The catalogue containment held.
    pub contained: bool,
    /// Owned objects left behind. Non-empty while `contained` is a lie the
    /// record validation refuses.
    pub residue: Vec<String>,
    /// Human-readable evidence for the verdict.
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn the_catalogue_names_twenty_seven_classes() {
        assert_eq!(FAULT_CATALOGUE.len(), 27);
    }

    #[test]
    fn every_class_id_is_unique_and_every_field_is_set() {
        let mut ids = BTreeSet::new();
        for class in FAULT_CATALOGUE {
            assert!(ids.insert(class.id), "duplicate class {}", class.id);
            assert!(!class.trigger.is_empty(), "{}", class.id);
            assert!(!class.containment.is_empty(), "{}", class.id);
            assert_eq!(find(class.id).copied(), Some(*class));
        }
    }

    #[test]
    fn every_seam_is_represented() {
        for seam in [
            FaultSeam::Process,
            FaultSeam::Http,
            FaultSeam::Filesystem,
            FaultSeam::Signal,
        ] {
            assert!(
                FAULT_CATALOGUE.iter().any(|class| class.seam == seam),
                "{} has no class",
                seam.as_str()
            );
        }
    }

    #[test]
    fn every_runnable_class_names_its_matrix_scenario() {
        let runnable: Vec<&FaultClass> = FAULT_CATALOGUE
            .iter()
            .filter(|class| class.bench_scenario.is_some())
            .collect();
        assert_eq!(runnable.len(), 4);
        for class in runnable {
            let scenario = class.bench_scenario.expect("runnable");
            assert!(
                scenario.starts_with("fault/"),
                "{} names {scenario}",
                class.id
            );
            assert!(
                crate::scenario::find(scenario).is_some(),
                "{} names undeclared scenario {scenario}",
                class.id
            );
        }
    }

    #[test]
    fn fault_outcome_round_trips_through_json() {
        let outcome = FaultOutcome {
            class: "docker-kill-mid-step".to_owned(),
            injected: true,
            contained: true,
            residue: Vec::new(),
            detail: "kill delivered; teardown removed the container".to_owned(),
        };
        let json = serde_json::to_string(&outcome).expect("serialise");
        let parsed: FaultOutcome = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, outcome);
    }
}
