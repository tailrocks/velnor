//! Node Architecture v2: health vector, slot/job lifecycle, fencing, permits.
//!
//! Ready is unrepresentable without a held capacity permit, valid routing, a
//! live session, and a proven executor. Generation tokens make stale actors
//! unable to complete or clean up a newer generation's work.

use serde::{Deserialize, Serialize};

use crate::ExecutionBackendKind;

/// Overall fleet schedulability. Distinct from systemd `READY=1`, which only
/// means a control process completed a local cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FleetHealthState {
    Ready,
    Degraded,
    NotReady,
}

impl FleetHealthState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::NotReady => "not_ready",
        }
    }
}

/// External black-box canary observation. Never inferred from local liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanaryStatus {
    Passing,
    Failing,
    Timeout,
    Unknown,
}

impl CanaryStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passing => "passing",
            Self::Failing => "failing",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }
}

/// Structured health document for the Unix socket and `velnorctl status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthDocument {
    pub control_live: bool,
    pub journal_writable: bool,
    pub github_reachable: bool,
    pub routing_valid: bool,
    pub runner_group_valid: bool,
    pub desired_ready_slots: u32,
    pub actual_ready_slots: u32,
    pub surge_ready_slots: u32,
    pub registered_slots: u32,
    pub capacity_permits: u32,
    pub executor_ready_slots: u32,
    pub oldest_queued_job_seconds: u64,
    pub oldest_outbox_entry_seconds: u64,
    pub external_canary: CanaryStatus,
    pub execution_backend: ExecutionBackendKind,
    pub state: FleetHealthState,
}

impl HealthDocument {
    /// Every JSON object key the health contract requires, in document order.
    pub const REQUIRED_KEYS: [&'static str; 16] = [
        "control_live",
        "journal_writable",
        "github_reachable",
        "routing_valid",
        "runner_group_valid",
        "desired_ready_slots",
        "actual_ready_slots",
        "surge_ready_slots",
        "registered_slots",
        "capacity_permits",
        "executor_ready_slots",
        "oldest_queued_job_seconds",
        "oldest_outbox_entry_seconds",
        "external_canary",
        "execution_backend",
        "state",
    ];

    /// Empty vector before journal/config load. `execution_backend` is the
    /// packaged default (`docker`), not a live selection or a fallback.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            control_live: false,
            journal_writable: false,
            github_reachable: false,
            routing_valid: false,
            runner_group_valid: false,
            desired_ready_slots: 0,
            actual_ready_slots: 0,
            surge_ready_slots: 0,
            registered_slots: 0,
            capacity_permits: 0,
            executor_ready_slots: 0,
            oldest_queued_job_seconds: 0,
            oldest_outbox_entry_seconds: 0,
            external_canary: CanaryStatus::Unknown,
            execution_backend: ExecutionBackendKind::Docker,
            state: FleetHealthState::NotReady,
        }
    }

    /// Derive overall `state` from the vector. Control liveness can stay true
    /// while GitHub or routing is down (`degraded`); that must not look ready.
    #[must_use]
    pub fn with_derived_state(mut self) -> Self {
        self.state = self.derive_state();
        self
    }

    #[must_use]
    pub fn derive_state(&self) -> FleetHealthState {
        if !self.control_live || !self.journal_writable {
            return FleetHealthState::NotReady;
        }
        if !self.github_reachable
            || !self.routing_valid
            || !self.runner_group_valid
            || self.actual_ready_slots < self.desired_ready_slots
        {
            return FleetHealthState::Degraded;
        }
        if self.actual_ready_slots == 0 || self.capacity_permits == 0 {
            return FleetHealthState::NotReady;
        }
        FleetHealthState::Ready
    }
}

/// Compare-and-swap generation. External mutations must carry the current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Generation(pub u64);

impl Generation {
    pub const INITIAL: Self = Self(1);

    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Identity of one slot actor (`<scope>-<index>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SlotId(pub String);

/// Identity of one accepted job attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub String);

/// Formal actor lifecycle. Exceptional states are first-class, not flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorPhase {
    Absent,
    Provisioning,
    Registered,
    Ready,
    Assigned,
    Starting,
    Running,
    Completing,
    Retiring,
    Degraded,
    Fenced,
    Quarantined,
}

impl ActorPhase {
    pub const ALL: [Self; 12] = [
        Self::Absent,
        Self::Provisioning,
        Self::Registered,
        Self::Ready,
        Self::Assigned,
        Self::Starting,
        Self::Running,
        Self::Completing,
        Self::Retiring,
        Self::Degraded,
        Self::Fenced,
        Self::Quarantined,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Provisioning => "provisioning",
            Self::Registered => "registered",
            Self::Ready => "ready",
            Self::Assigned => "assigned",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Completing => "completing",
            Self::Retiring => "retiring",
            Self::Degraded => "degraded",
            Self::Fenced => "fenced",
            Self::Quarantined => "quarantined",
        }
    }

    #[must_use]
    pub fn counts_as_ready(self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// Why a slot cannot enter [`ActorPhase::Ready`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotReady {
    pub missing_permit: bool,
    pub routing_invalid: bool,
    pub session_not_live: bool,
    pub executor_unproven: bool,
}

impl NotReady {
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.missing_permit
            || self.routing_invalid
            || self.session_not_live
            || self.executor_unproven
    }
}

/// Proof that a slot may be advertised. Constructed only when every
/// precondition holds; there is no `Ready` without this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyProof {
    pub permit_held: bool,
    pub routing_valid: bool,
    pub session_live: bool,
    pub executor_proven: bool,
}

impl ReadyProof {
    /// # Errors
    /// Returns [`NotReady`] naming every failed precondition.
    pub fn try_new(
        permit_held: bool,
        routing_valid: bool,
        session_live: bool,
        executor_proven: bool,
    ) -> Result<Self, NotReady> {
        let missing = NotReady {
            missing_permit: !permit_held,
            routing_invalid: !routing_valid,
            session_not_live: !session_live,
            executor_unproven: !executor_proven,
        };
        if missing.is_blocked() {
            return Err(missing);
        }
        Ok(Self {
            permit_held: true,
            routing_valid: true,
            session_live: true,
            executor_proven: true,
        })
    }
}

/// Durable capacity permit for the largest job a slot may legally accept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityPermit {
    pub slot_id: SlotId,
    pub generation: Generation,
    pub held: bool,
    pub surge: bool,
}

/// End-to-end vs component SLI dimensions. Numerical SLAs are not published
/// from these until the Stage-8 soak; the types exist so later work cannot
/// invent a second mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SliDimension {
    EndToEnd,
    VelnorComponent,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_proof_rejects_any_missing_precondition() {
        assert!(ReadyProof::try_new(false, true, true, true).is_err());
        assert!(ReadyProof::try_new(true, false, true, true).is_err());
        assert!(ReadyProof::try_new(true, true, false, true).is_err());
        assert!(ReadyProof::try_new(true, true, true, false).is_err());
        assert!(ReadyProof::try_new(true, true, true, true).is_ok());
    }

    #[test]
    fn github_down_is_degraded_not_ready_while_control_stays_live() {
        let doc = HealthDocument {
            control_live: true,
            journal_writable: true,
            github_reachable: false,
            routing_valid: true,
            runner_group_valid: true,
            desired_ready_slots: 4,
            actual_ready_slots: 4,
            surge_ready_slots: 1,
            registered_slots: 4,
            capacity_permits: 5,
            executor_ready_slots: 4,
            oldest_queued_job_seconds: 0,
            oldest_outbox_entry_seconds: 0,
            external_canary: CanaryStatus::Unknown,
            execution_backend: ExecutionBackendKind::Docker,
            state: FleetHealthState::Ready,
        }
        .with_derived_state();
        assert!(doc.control_live);
        assert_eq!(doc.state, FleetHealthState::Degraded);
        assert_ne!(doc.state.as_str(), "ready");
    }

    #[test]
    fn health_document_serializes_every_required_key() {
        let json = serde_json::to_value(HealthDocument::empty().with_derived_state()).unwrap();
        let obj = json.as_object().unwrap();
        for key in HealthDocument::REQUIRED_KEYS {
            assert!(obj.contains_key(key), "missing {key}");
        }
        assert_eq!(obj["state"], "not_ready");
        assert_eq!(obj["external_canary"], "unknown");
    }
}
