//! Slot lifecycle phases and the stable-slot / ephemeral-runner distinction.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A durable slot phase or kind token was not recognized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSlotToken {
    /// Closed vocabulary that rejected the token (`slot_phase`/`slot_kind`).
    pub field: &'static str,
}

impl fmt::Display for InvalidSlotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid slot token for '{}': not part of the closed taxonomy",
            self.field
        )
    }
}

impl std::error::Error for InvalidSlotToken {}

/// Operator-visible phase of one slot.
///
/// Serialized in kebab-case (for example `waiting-for-capacity`);
/// deserialization is fail-closed: an unrecognized phase string is an error,
/// never silently mapped to another phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlotPhase {
    Configuring,
    Idle,
    Acquiring,
    WaitingForCapacity,
    Running,
    Finalizing,
    Teardown,
    Recycling,
    Parked,
    Draining,
    Error,
}

impl SlotPhase {
    /// Every phase in lifecycle order, exactly as the plan fixes them.
    pub const ALL: [SlotPhase; 11] = [
        SlotPhase::Configuring,
        SlotPhase::Idle,
        SlotPhase::Acquiring,
        SlotPhase::WaitingForCapacity,
        SlotPhase::Running,
        SlotPhase::Finalizing,
        SlotPhase::Teardown,
        SlotPhase::Recycling,
        SlotPhase::Parked,
        SlotPhase::Draining,
        SlotPhase::Error,
    ];

    /// Canonical serialized spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SlotPhase::Configuring => "configuring",
            SlotPhase::Idle => "idle",
            SlotPhase::Acquiring => "acquiring",
            SlotPhase::WaitingForCapacity => "waiting-for-capacity",
            SlotPhase::Running => "running",
            SlotPhase::Finalizing => "finalizing",
            SlotPhase::Teardown => "teardown",
            SlotPhase::Recycling => "recycling",
            SlotPhase::Parked => "parked",
            SlotPhase::Draining => "draining",
            SlotPhase::Error => "error",
        }
    }

    /// Phases that represent a slot actively doing or holding work.
    #[must_use]
    pub fn is_busy(self) -> bool {
        matches!(
            self,
            SlotPhase::Acquiring | SlotPhase::Running | SlotPhase::Finalizing
        )
    }

    /// True when the phase warrants a stderr warning line from renderers.
    #[must_use]
    pub fn is_warning(self) -> bool {
        matches!(self, SlotPhase::Error | SlotPhase::Draining)
    }
}

impl TryFrom<&str> for SlotPhase {
    type Error = InvalidSlotToken;

    fn try_from(raw: &str) -> Result<Self, InvalidSlotToken> {
        Self::ALL
            .into_iter()
            .find(|phase| phase.as_str() == raw)
            .ok_or(InvalidSlotToken {
                field: "slot_phase",
            })
    }
}

/// Whether one observed phase may follow another in the same slot generation.
///
/// The durable projection may skip internal implementation detail, but it may
/// never skip teardown between active work and recycling. A newer actor
/// generation establishes a fresh ownership root and therefore has no `from`
/// edge to validate with this function.
#[must_use]
pub const fn slot_transition_allowed(from: SlotPhase, to: SlotPhase) -> bool {
    match from {
        SlotPhase::Configuring => matches!(
            to,
            SlotPhase::Idle | SlotPhase::Parked | SlotPhase::Draining | SlotPhase::Error
        ),
        SlotPhase::Idle => matches!(
            to,
            SlotPhase::Acquiring
                | SlotPhase::Running
                | SlotPhase::Teardown
                | SlotPhase::Parked
                | SlotPhase::Draining
                | SlotPhase::Error
        ),
        SlotPhase::Acquiring => matches!(
            to,
            SlotPhase::WaitingForCapacity
                | SlotPhase::Running
                | SlotPhase::Teardown
                | SlotPhase::Parked
                | SlotPhase::Draining
                | SlotPhase::Error
        ),
        SlotPhase::WaitingForCapacity => matches!(
            to,
            SlotPhase::Running
                | SlotPhase::Teardown
                | SlotPhase::Parked
                | SlotPhase::Draining
                | SlotPhase::Error
        ),
        SlotPhase::Running => matches!(
            to,
            SlotPhase::Finalizing | SlotPhase::Teardown | SlotPhase::Draining | SlotPhase::Error
        ),
        SlotPhase::Finalizing => matches!(
            to,
            SlotPhase::Teardown | SlotPhase::Draining | SlotPhase::Error
        ),
        SlotPhase::Teardown => matches!(
            to,
            SlotPhase::Recycling | SlotPhase::Parked | SlotPhase::Draining | SlotPhase::Error
        ),
        SlotPhase::Recycling => matches!(
            to,
            SlotPhase::Configuring
                | SlotPhase::Idle
                | SlotPhase::Parked
                | SlotPhase::Draining
                | SlotPhase::Error
        ),
        SlotPhase::Parked => matches!(
            to,
            SlotPhase::Configuring | SlotPhase::Idle | SlotPhase::Draining | SlotPhase::Error
        ),
        SlotPhase::Draining => matches!(to, SlotPhase::Teardown | SlotPhase::Error),
        SlotPhase::Error => matches!(to, SlotPhase::Teardown | SlotPhase::Draining),
    }
}

/// Whether a slot is a long-lived stable slot or an ephemeral runner slot.
///
/// The stable-slot / ephemeral-runner distinction is preserved in types so
/// downstream consumers never infer it from labels or names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SlotKind {
    /// Persistent named slot reused across jobs.
    Stable,
    /// Single-job runner created for one job and discarded afterwards.
    Ephemeral,
}

impl SlotKind {
    /// Every variant, canonical order.
    pub const ALL: [SlotKind; 2] = [SlotKind::Stable, SlotKind::Ephemeral];

    /// Canonical serialized spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SlotKind::Stable => "stable",
            SlotKind::Ephemeral => "ephemeral",
        }
    }
}

impl TryFrom<&str> for SlotKind {
    type Error = InvalidSlotToken;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == raw)
            .ok_or(InvalidSlotToken { field: "slot_kind" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_planned_phase_is_present_in_order() {
        let spellings: Vec<&str> = SlotPhase::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            spellings,
            [
                "configuring",
                "idle",
                "acquiring",
                "waiting-for-capacity",
                "running",
                "finalizing",
                "teardown",
                "recycling",
                "parked",
                "draining",
                "error"
            ]
        );
        assert_eq!(spellings.len(), 11);
    }

    #[test]
    fn phase_serialization_matches_as_str() {
        for phase in SlotPhase::ALL {
            assert_eq!(
                serde_json::to_string(&phase).unwrap(),
                format!("\"{}\"", phase.as_str())
            );
        }
    }

    #[test]
    fn unknown_phase_is_fail_closed() {
        assert!(serde_json::from_str::<SlotPhase>("\"queued\"").is_err());
        assert!(serde_json::from_str::<SlotPhase>("\"WaitingForCapacity\"").is_err());
        assert!(serde_json::from_str::<SlotPhase>("\"\"").is_err());
        assert_eq!(
            SlotPhase::try_from("teardown").unwrap(),
            SlotPhase::Teardown
        );
        assert!(SlotPhase::try_from("queued").is_err());
    }

    #[test]
    fn durable_transition_graph_requires_teardown_before_recycling() {
        assert!(slot_transition_allowed(
            SlotPhase::Running,
            SlotPhase::Teardown
        ));
        assert!(slot_transition_allowed(
            SlotPhase::Teardown,
            SlotPhase::Recycling
        ));
        assert!(slot_transition_allowed(
            SlotPhase::Recycling,
            SlotPhase::Idle
        ));
        assert!(!slot_transition_allowed(
            SlotPhase::Running,
            SlotPhase::Recycling
        ));
        assert!(!slot_transition_allowed(
            SlotPhase::Idle,
            SlotPhase::Finalizing
        ));
        for phase in SlotPhase::ALL {
            if phase != SlotPhase::Teardown {
                assert!(
                    !slot_transition_allowed(phase, SlotPhase::Recycling),
                    "{phase:?} bypasses teardown"
                );
            }
        }
    }

    #[test]
    fn slot_kind_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&SlotKind::Stable).unwrap(),
            "\"stable\""
        );
        assert_eq!(
            serde_json::to_string(&SlotKind::Ephemeral).unwrap(),
            "\"ephemeral\""
        );
        assert!(serde_json::from_str::<SlotKind>("\"Stable\"").is_err());
        assert_eq!(SlotKind::try_from("stable").unwrap(), SlotKind::Stable);
        assert!(SlotKind::try_from("persistent").is_err());
    }
}
