//! Slot lifecycle phases and the stable-slot / ephemeral-runner distinction.

use serde::{Deserialize, Serialize};

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
    }
}
