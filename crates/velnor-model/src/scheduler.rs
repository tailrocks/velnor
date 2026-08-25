//! GitHub scheduler backends. Production remains Legacy JIT V2 until
//! ScaleSetV2 proves exact group/repo/label/YAML equivalence.

use serde::{Deserialize, Serialize};

/// Pinned `actions/scaleset` revision used for protocol fixtures.
/// <https://github.com/actions/scaleset/commit/cb0405b2d874500e75ae34eff8d582ab75956b45>
pub const SCALESET_UPSTREAM_COMMIT: &str = "cb0405b2d874500e75ae34eff8d582ab75956b45";

/// Actions Service scale-set path from that revision (`client.go`).
pub const SCALESET_ENDPOINT: &str = "_apis/runtime/runnerscalesets";
/// Max-capacity header from that revision (`HeaderScaleSetMaxCapacity`).
pub const SCALESET_MAX_CAPACITY_HEADER: &str = "X-ScaleSetMaxCapacity";
/// Actions Service API version appended on scale-set requests.
pub const SCALESET_API_VERSION: &str = "6.0-preview";

/// Which GitHub scheduler a fleet may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerKind {
    /// Current production: per-slot `generate-jitconfig` + V2 broker.
    LegacyJitV2,
    /// Public-preview scale-set APIs. Not production until estate proof.
    ScaleSetV2,
}

impl SchedulerKind {
    /// The only backend allowed to register or advertise capacity.
    pub const PRODUCTION: Self = Self::LegacyJitV2;

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LegacyJitV2 => "legacy_jit_v2",
            Self::ScaleSetV2 => "scale_set_v2",
        }
    }

    /// ScaleSetV2 is not an allowed production activate.
    ///
    /// # Errors
    /// [`ScaleSetNotProven`] when `self` is not [`Self::PRODUCTION`].
    pub fn activate_production(self) -> Result<(), ScaleSetNotProven> {
        if self == Self::PRODUCTION {
            Ok(())
        } else {
            Err(ScaleSetNotProven { requested: self })
        }
    }
}

/// Why ScaleSetV2 cannot take production traffic yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaleSetNotProven {
    pub requested: SchedulerKind,
}

impl std::fmt::Display for ScaleSetNotProven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scheduler {} is not production; estate group/repo/label/YAML equivalence is unproven (upstream {})",
            self.requested.as_str(),
            SCALESET_UPSTREAM_COMMIT
        )
    }
}

impl std::error::Error for ScaleSetNotProven {}

/// `RunnerScaleSetStatistic` from `types.go` at [`SCALESET_UPSTREAM_COMMIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunnerScaleSetStatistic {
    pub total_available_jobs: i32,
    pub total_acquired_jobs: i32,
    pub total_assigned_jobs: i32,
    pub total_running_jobs: i32,
    pub total_registered_runners: i32,
    pub total_busy_runners: i32,
    pub total_idle_runners: i32,
}

impl RunnerScaleSetStatistic {
    /// Desired online runners. Message bodies cap at 50; statistics are authoritative.
    #[must_use]
    pub fn desired_runners(self) -> u32 {
        u32::try_from(self.total_assigned_jobs.max(0)).unwrap_or(0)
    }
}

/// Batched scale-set message wrapper (`RunnerScaleSetJobMessages`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerScaleSetMessageResponse {
    pub message_id: i32,
    pub message_type: String,
    #[serde(default)]
    pub body: String,
    pub statistics: Option<RunnerScaleSetStatistic>,
}

/// Job lifecycle message types from `types.go`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScaleSetJobMessageType {
    JobAvailable,
    JobAssigned,
    JobStarted,
    JobCompleted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_is_legacy_jit_v2() {
        assert_eq!(SchedulerKind::PRODUCTION, SchedulerKind::LegacyJitV2);
        assert!(SchedulerKind::LegacyJitV2.activate_production().is_ok());
        assert!(SchedulerKind::ScaleSetV2.activate_production().is_err());
    }

    #[test]
    fn job_message_types_match_upstream_names() {
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobAvailable).unwrap(),
            "\"JobAvailable\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobAssigned).unwrap(),
            "\"JobAssigned\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobStarted).unwrap(),
            "\"JobStarted\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobCompleted).unwrap(),
            "\"JobCompleted\""
        );
    }

    #[test]
    fn desired_runners_uses_statistics_not_message_count() {
        let stats = RunnerScaleSetStatistic {
            total_assigned_jobs: 3,
            ..RunnerScaleSetStatistic::default()
        };
        assert_eq!(stats.desired_runners(), 3);
    }
}
