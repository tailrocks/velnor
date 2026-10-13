//! Normalized operational event reasons and the enforced job state machine.
//!
//! Plan 066 step 3 fixes the retained event-reason taxonomy and the legal
//! job phase-transition table. Every reason the leaf retains is a variant
//! of [`EventReason`]; every persisted job walks the closed [`JobState`]
//! graph through [`transition_target`]. Both serialize as stable dotted or
//! single tokens and both deserialize fail-closed: an unknown token is an
//! error, never silently mapped.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A retained event-reason or job-state token was not recognized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLifecycleToken {
    /// Which closed set rejected the token (`event_reason`/`job_state`).
    pub field: &'static str,
}

impl fmt::Display for InvalidLifecycleToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid lifecycle token for '{}': not part of the closed taxonomy",
            self.field
        )
    }
}

impl std::error::Error for InvalidLifecycleToken {}

/// Every retained operational event reason (Plan 066 step 3).
///
/// Canonical spellings are dot-separated machine tokens; serialization uses
/// exactly these spellings and deserialization is fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventReason {
    #[serde(rename = "readiness.ready")]
    ReadinessReady,
    #[serde(rename = "readiness.degraded")]
    ReadinessDegraded,
    #[serde(rename = "drain.started")]
    DrainStarted,
    #[serde(rename = "drain.completed")]
    DrainCompleted,
    #[serde(rename = "slot.state_changed")]
    SlotStateChanged,
    #[serde(rename = "registration.missing")]
    RegistrationMissing,
    #[serde(rename = "registration.offline")]
    RegistrationOffline,
    #[serde(rename = "registration.stale_busy")]
    RegistrationStaleBusy,
    #[serde(rename = "job.acquired")]
    JobAcquired,
    #[serde(rename = "job.waiting")]
    JobWaiting,
    #[serde(rename = "job.started")]
    JobStarted,
    #[serde(rename = "job.completed")]
    JobCompleted,
    #[serde(rename = "job.canceled")]
    JobCanceled,
    #[serde(rename = "job.rejected")]
    JobRejected,
    #[serde(rename = "capacity.pressure")]
    CapacityPressure,
    #[serde(rename = "gc.started")]
    GcStarted,
    #[serde(rename = "gc.completed")]
    GcCompleted,
}

impl EventReason {
    /// Every variant in taxonomy order.
    pub const ALL: [EventReason; 17] = [
        EventReason::ReadinessReady,
        EventReason::ReadinessDegraded,
        EventReason::DrainStarted,
        EventReason::DrainCompleted,
        EventReason::SlotStateChanged,
        EventReason::RegistrationMissing,
        EventReason::RegistrationOffline,
        EventReason::RegistrationStaleBusy,
        EventReason::JobAcquired,
        EventReason::JobWaiting,
        EventReason::JobStarted,
        EventReason::JobCompleted,
        EventReason::JobCanceled,
        EventReason::JobRejected,
        EventReason::CapacityPressure,
        EventReason::GcStarted,
        EventReason::GcCompleted,
    ];

    /// Canonical serialized spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            EventReason::ReadinessReady => "readiness.ready",
            EventReason::ReadinessDegraded => "readiness.degraded",
            EventReason::DrainStarted => "drain.started",
            EventReason::DrainCompleted => "drain.completed",
            EventReason::SlotStateChanged => "slot.state_changed",
            EventReason::RegistrationMissing => "registration.missing",
            EventReason::RegistrationOffline => "registration.offline",
            EventReason::RegistrationStaleBusy => "registration.stale_busy",
            EventReason::JobAcquired => "job.acquired",
            EventReason::JobWaiting => "job.waiting",
            EventReason::JobStarted => "job.started",
            EventReason::JobCompleted => "job.completed",
            EventReason::JobCanceled => "job.canceled",
            EventReason::JobRejected => "job.rejected",
            EventReason::CapacityPressure => "capacity.pressure",
            EventReason::GcStarted => "gc.started",
            EventReason::GcCompleted => "gc.completed",
        }
    }

    /// Whether this reason drives the persisted job state machine.
    #[must_use]
    pub const fn is_job_transition(self) -> bool {
        matches!(
            self,
            EventReason::JobAcquired
                | EventReason::JobWaiting
                | EventReason::JobStarted
                | EventReason::JobCompleted
                | EventReason::JobCanceled
                | EventReason::JobRejected
        )
    }

    /// The job state this reason produces, when it is a job transition.
    #[must_use]
    pub const fn job_target(self) -> Option<JobState> {
        match self {
            EventReason::JobAcquired => Some(JobState::Acquired),
            EventReason::JobWaiting => Some(JobState::Waiting),
            EventReason::JobStarted => Some(JobState::Started),
            EventReason::JobCompleted => Some(JobState::Completed),
            EventReason::JobCanceled => Some(JobState::Canceled),
            EventReason::JobRejected => Some(JobState::Rejected),
            _ => None,
        }
    }
}

impl TryFrom<&str> for EventReason {
    type Error = InvalidLifecycleToken;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == raw)
            .ok_or(InvalidLifecycleToken {
                field: "event_reason",
            })
    }
}

/// Lifecycle state of one persisted job under the transition table.
///
/// Distinct from the GitHub-facing summary phase ([`crate::JobPhase`]):
/// this is the store-side machine vocabulary. `Queued` is the genesis row
/// state before any transition; terminal states have no outgoing edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Acquired,
    Waiting,
    Started,
    Completed,
    Canceled,
    Rejected,
}

impl JobState {
    /// Every state in lifecycle order.
    pub const ALL: [JobState; 7] = [
        JobState::Queued,
        JobState::Acquired,
        JobState::Waiting,
        JobState::Started,
        JobState::Completed,
        JobState::Canceled,
        JobState::Rejected,
    ];

    /// Canonical serialized spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Acquired => "acquired",
            JobState::Waiting => "waiting",
            JobState::Started => "started",
            JobState::Completed => "completed",
            JobState::Canceled => "canceled",
            JobState::Rejected => "rejected",
        }
    }

    /// Terminal states accept no further transitions; replays of an
    /// already-applied terminal token stay idempotent no-ops at the store.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Completed | JobState::Canceled | JobState::Rejected
        )
    }
}

impl TryFrom<&str> for JobState {
    type Error = InvalidLifecycleToken;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == raw)
            .ok_or(InvalidLifecycleToken { field: "job_state" })
    }
}

/// The legal edge `(from, reason) -> target`, or `None` when illegal.
///
/// The happy path is exactly `queued --acquired--> acquired --waiting-->
/// waiting --started--> started --> {completed | canceled | rejected}`.
/// Plan 066 grants no infrastructure retry edges: an infra-failed job ends
/// terminal via `job.rejected`; it never re-enters the graph. Because the
/// real daemon also rejects or loses jobs before workflow execution begins,
/// the pre-start states keep their explicit fail-close exits
/// (`acquired|waiting --canceled/rejected--> terminal`); without them a
/// pre-execution rejection could never reach a terminal row and the job
/// would be stuck nonterminal forever. Every other `(from, reason)` pair —
/// including any edge out of a terminal state and any non-job reason — is
/// illegal.
#[must_use]
pub fn transition_target(from: JobState, reason: EventReason) -> Option<JobState> {
    match (from, reason) {
        (JobState::Queued, EventReason::JobAcquired) => Some(JobState::Acquired),
        (JobState::Acquired, EventReason::JobWaiting) => Some(JobState::Waiting),
        (JobState::Waiting, EventReason::JobStarted) => Some(JobState::Started),
        (JobState::Started, EventReason::JobCompleted) => Some(JobState::Completed),
        (JobState::Started, EventReason::JobCanceled) => Some(JobState::Canceled),
        (JobState::Started, EventReason::JobRejected) => Some(JobState::Rejected),
        // Pre-execution fail-close exits: the daemon can lose the
        // registration, hit host-capacity backpressure, or reject the job on
        // trust/capability/store grounds before any step runs. Those jobs
        // must still reach a terminal row.
        (JobState::Acquired, EventReason::JobCanceled) => Some(JobState::Canceled),
        (JobState::Acquired, EventReason::JobRejected) => Some(JobState::Rejected),
        (JobState::Waiting, EventReason::JobCanceled) => Some(JobState::Canceled),
        (JobState::Waiting, EventReason::JobRejected) => Some(JobState::Rejected),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_retained_reason_is_present_in_taxonomy_order() {
        let spellings: Vec<&str> = EventReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            spellings,
            [
                "readiness.ready",
                "readiness.degraded",
                "drain.started",
                "drain.completed",
                "slot.state_changed",
                "registration.missing",
                "registration.offline",
                "registration.stale_busy",
                "job.acquired",
                "job.waiting",
                "job.started",
                "job.completed",
                "job.canceled",
                "job.rejected",
                "capacity.pressure",
                "gc.started",
                "gc.completed",
            ]
        );
        assert_eq!(spellings.len(), 17);
    }

    #[test]
    fn reason_serialization_matches_as_str_and_fails_closed() {
        for reason in EventReason::ALL {
            assert_eq!(
                serde_json::to_string(&reason).unwrap(),
                format!("\"{}\"", reason.as_str())
            );
            assert_eq!(EventReason::try_from(reason.as_str()).unwrap(), reason);
        }
        for unknown in ["", "job.acuire", "JOB.ACQUIRED", "slot.ready", "job."] {
            assert!(EventReason::try_from(unknown).is_err(), "{unknown:?}");
            let parsed: Result<EventReason, _> = serde_json::from_str(&format!("\"{unknown}\""));
            assert!(parsed.is_err(), "{unknown:?}");
        }
    }

    #[test]
    fn only_the_six_job_reasons_drive_the_job_machine() {
        let job_reasons: Vec<_> = EventReason::ALL
            .iter()
            .copied()
            .filter(|reason| reason.is_job_transition())
            .collect();
        assert_eq!(
            job_reasons,
            [
                EventReason::JobAcquired,
                EventReason::JobWaiting,
                EventReason::JobStarted,
                EventReason::JobCompleted,
                EventReason::JobCanceled,
                EventReason::JobRejected,
            ]
        );
        for reason in job_reasons {
            assert_eq!(reason.job_target().map(JobState::as_str).unwrap(), {
                // target spelling mirrors the reason's object
                reason.as_str().trim_start_matches("job.")
            });
        }
        assert_eq!(EventReason::SlotStateChanged.job_target(), None);
        assert!(!EventReason::CapacityPressure.is_job_transition());
    }

    #[test]
    fn state_serialization_is_snake_case_and_fail_closed() {
        for state in JobState::ALL {
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{}\"", state.as_str())
            );
            assert_eq!(JobState::try_from(state.as_str()).unwrap(), state);
        }
        assert!(JobState::try_from("running").is_err());
        assert!(JobState::try_from("").is_err());
        assert!(serde_json::from_str::<JobState>("\"Running\"").is_err());
    }

    #[test]
    fn exactly_three_states_are_terminal() {
        let terminals: Vec<_> = JobState::ALL
            .iter()
            .copied()
            .filter(|state| state.is_terminal())
            .collect();
        assert_eq!(
            terminals,
            [JobState::Completed, JobState::Canceled, JobState::Rejected]
        );
    }

    #[test]
    fn happy_path_walks_each_required_reason_exactly_once() {
        let path = [
            (
                JobState::Queued,
                EventReason::JobAcquired,
                JobState::Acquired,
            ),
            (
                JobState::Acquired,
                EventReason::JobWaiting,
                JobState::Waiting,
            ),
            (
                JobState::Waiting,
                EventReason::JobStarted,
                JobState::Started,
            ),
            (
                JobState::Started,
                EventReason::JobCompleted,
                JobState::Completed,
            ),
        ];
        let mut seen: Vec<EventReason> = Vec::new();
        for (from, reason, expected) in path {
            assert_eq!(transition_target(from, reason), Some(expected));
            seen.push(reason);
        }
        for required in [
            EventReason::JobAcquired,
            EventReason::JobWaiting,
            EventReason::JobStarted,
            EventReason::JobCompleted,
        ] {
            assert_eq!(
                seen.iter().filter(|r| **r == required).count(),
                1,
                "{} must be emitted exactly once on the happy path",
                required.as_str()
            );
        }
    }

    #[test]
    fn replaying_a_step_from_its_post_state_is_illegal_not_a_duplicate() {
        // After each happy-path step, re-emitting the same reason from the
        // reached state has no edge: replay can never duplicate a step.
        for (_, reason, reached) in [
            (
                JobState::Queued,
                EventReason::JobAcquired,
                JobState::Acquired,
            ),
            (
                JobState::Acquired,
                EventReason::JobWaiting,
                JobState::Waiting,
            ),
            (
                JobState::Waiting,
                EventReason::JobStarted,
                JobState::Started,
            ),
            (
                JobState::Started,
                EventReason::JobCompleted,
                JobState::Completed,
            ),
        ] {
            assert_eq!(transition_target(reached, reason), None);
        }
    }

    #[test]
    fn impossible_transition_matrix_rejects_spot_checks() {
        let impossible = [
            (JobState::Completed, EventReason::JobStarted),
            (JobState::Completed, EventReason::JobAcquired),
            (JobState::Completed, EventReason::JobCompleted),
            (JobState::Canceled, EventReason::JobCompleted),
            (JobState::Canceled, EventReason::JobStarted),
            (JobState::Rejected, EventReason::JobCompleted),
            (JobState::Rejected, EventReason::JobStarted),
            (JobState::Queued, EventReason::JobStarted),
            (JobState::Queued, EventReason::JobCompleted),
            (JobState::Queued, EventReason::JobCanceled),
            (JobState::Queued, EventReason::JobRejected),
            (JobState::Acquired, EventReason::JobStarted),
            (JobState::Acquired, EventReason::JobCompleted),
            (JobState::Waiting, EventReason::JobAcquired),
            (JobState::Waiting, EventReason::JobCompleted),
            (JobState::Started, EventReason::JobStarted),
            (JobState::Started, EventReason::JobAcquired),
        ];
        for (from, reason) in impossible {
            assert_eq!(
                transition_target(from, reason),
                None,
                "{} must reject {}",
                from.as_str(),
                reason.as_str()
            );
        }
    }

    #[test]
    fn pre_execution_fail_close_edges_reach_terminal_rows() {
        // A job lost or rejected before workflow execution must still reach
        // a terminal row; without these edges the row would stay
        // nonterminal forever.
        for from in [JobState::Acquired, JobState::Waiting] {
            assert_eq!(
                transition_target(from, EventReason::JobRejected),
                Some(JobState::Rejected),
                "{} must reject to terminal",
                from.as_str()
            );
            assert_eq!(
                transition_target(from, EventReason::JobCanceled),
                Some(JobState::Canceled),
                "{} must cancel to terminal",
                from.as_str()
            );
        }
        assert_eq!(
            transition_target(JobState::Queued, EventReason::JobRejected),
            None,
            "the genesis row has no direct rejection edge"
        );
    }

    #[test]
    fn terminal_states_accept_no_edge_at_all() {
        for terminal in [JobState::Completed, JobState::Canceled, JobState::Rejected] {
            for reason in EventReason::ALL {
                assert_eq!(
                    transition_target(terminal, reason),
                    None,
                    "terminal {} must reject {}",
                    terminal.as_str(),
                    reason.as_str()
                );
            }
        }
    }

    #[test]
    fn non_job_reasons_never_move_job_state() {
        for reason in EventReason::ALL
            .iter()
            .copied()
            .filter(|reason| !reason.is_job_transition())
        {
            for state in JobState::ALL {
                assert_eq!(transition_target(state, reason), None);
            }
        }
    }
}
