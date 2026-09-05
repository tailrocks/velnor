//! Job lifecycle stages, each attributed to an execution lane.
//!
//! The point of the lane split is that a Velnor number must never be inflated
//! by GitHub's broker or by a git remote. [`TelemetryLane`] already encodes
//! exactly this distinction inside the runner
//! (`crates/velnor-model/src/telemetry.rs`), so it is reused here rather than
//! reinvented:
//!
//! * [`TelemetryLane::Velnor`] — latency produced inside the Velnor process
//!   boundary and therefore attributable to this project.
//! * [`TelemetryLane::Github`] — latency produced by a service Velnor does not
//!   control: the GitHub broker and run-service, the git remote, and the image
//!   registry. Each such stage names its external endpoint so a reader can
//!   disaggregate "GitHub" from "registry" without guessing.

use serde::{Deserialize, Serialize};
use velnor_model::telemetry::TelemetryLane;

/// One measured stage of the acquisition, startup, execution and teardown path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Slot has announced readiness and is waiting for work.
    Ready,
    /// Broker held the long poll before delivering a message.
    BrokerDelivery,
    /// `acquirejob` round trip that returns the job payload.
    AcquiredPayload,
    /// Local admission decision for the acquired job.
    Admission,
    /// Local capacity reservation (disk, slots, Docker lifecycle lock).
    Capacity,
    /// Everything between admission and the first git process.
    CheckoutStart,
    /// The checkout itself; see [`CheckoutPhase`] for its breakdown.
    Checkout,
    /// Image resolution, network creation, volume preparation.
    DockerSetup,
    /// `docker create`.
    ContainerCreate,
    /// `docker start`.
    ContainerStart,
    /// From container start to the first byte of the first user command.
    FirstUserCommand,
    /// Post-step completion work: upload, timeline flush, job completion call.
    CompletionOverhead,
    /// Container, network and workspace teardown.
    Teardown,
}

impl Stage {
    /// Every stage, in lifecycle order.
    pub const ALL: [Self; 13] = [
        Self::Ready,
        Self::BrokerDelivery,
        Self::AcquiredPayload,
        Self::Admission,
        Self::Capacity,
        Self::CheckoutStart,
        Self::Checkout,
        Self::DockerSetup,
        Self::ContainerCreate,
        Self::ContainerStart,
        Self::FirstUserCommand,
        Self::CompletionOverhead,
        Self::Teardown,
    ];

    /// Which lane owns this stage's latency.
    #[must_use]
    pub const fn lane(self) -> TelemetryLane {
        match self {
            Self::BrokerDelivery | Self::AcquiredPayload | Self::Checkout => TelemetryLane::Github,
            Self::Ready
            | Self::Admission
            | Self::Capacity
            | Self::CheckoutStart
            | Self::DockerSetup
            | Self::ContainerCreate
            | Self::ContainerStart
            | Self::FirstUserCommand
            | Self::CompletionOverhead
            | Self::Teardown => TelemetryLane::Velnor,
        }
    }

    /// The external dependency whose latency lands in this stage, if any.
    #[must_use]
    pub const fn external_endpoint(self) -> Option<&'static str> {
        match self {
            Self::BrokerDelivery => Some("github-broker"),
            Self::AcquiredPayload => Some("github-run-service"),
            Self::Checkout => Some("git-remote"),
            Self::DockerSetup => Some("image-registry"),
            _ => None,
        }
    }

    /// Stable identifier used in the wire record.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::BrokerDelivery => "broker-delivery",
            Self::AcquiredPayload => "acquired-payload",
            Self::Admission => "admission",
            Self::Capacity => "capacity",
            Self::CheckoutStart => "checkout-start",
            Self::Checkout => "checkout",
            Self::DockerSetup => "docker-setup",
            Self::ContainerCreate => "container-create",
            Self::ContainerStart => "container-start",
            Self::FirstUserCommand => "first-user-command",
            Self::CompletionOverhead => "completion-overhead",
            Self::Teardown => "teardown",
        }
    }
}

/// Per-phase breakdown inside [`Stage::Checkout`].
///
/// These phases require spans the runner does not emit yet; see
/// `crates/velnor-bench/README.md` for the exact hook the harness needs from
/// `crates/velnor-runner/src/checkout.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckoutPhase {
    /// Blocked waiting for the shared bare-mirror lock.
    MirrorLockWait,
    /// Fetching new objects into the shared mirror from the remote.
    MirrorFetch,
    /// Fetching from the local mirror into the job workspace.
    WorkspaceFetch,
    /// Materialising the tree in the workspace.
    WorkspaceCheckout,
    /// Rewriting file mtimes for build-tool fingerprint stability.
    MtimeNormalization,
}

impl CheckoutPhase {
    pub const ALL: [Self; 5] = [
        Self::MirrorLockWait,
        Self::MirrorFetch,
        Self::WorkspaceFetch,
        Self::WorkspaceCheckout,
        Self::MtimeNormalization,
    ];

    /// Only the remote fetch is external; every other phase is Velnor's own.
    #[must_use]
    pub const fn lane(self) -> TelemetryLane {
        match self {
            Self::MirrorFetch => TelemetryLane::Github,
            Self::MirrorLockWait
            | Self::WorkspaceFetch
            | Self::WorkspaceCheckout
            | Self::MtimeNormalization => TelemetryLane::Velnor,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MirrorLockWait => "mirror-lock-wait",
            Self::MirrorFetch => "mirror-fetch",
            Self::WorkspaceFetch => "workspace-fetch",
            Self::WorkspaceCheckout => "workspace-checkout",
            Self::MtimeNormalization => "mtime-normalization",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_is_listed_once_and_has_a_unique_name() {
        let mut names: Vec<&str> = Stage::ALL.iter().map(|stage| stage.as_str()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        assert_eq!(total, 13);
    }

    #[test]
    fn external_stages_are_github_lane_and_name_their_endpoint() {
        for stage in Stage::ALL {
            match stage.lane() {
                TelemetryLane::Github => assert!(
                    stage.external_endpoint().is_some(),
                    "{} is external but names no endpoint",
                    stage.as_str()
                ),
                TelemetryLane::Velnor => {
                    // docker-setup is Velnor-owned work that may touch a
                    // registry; it is the only Velnor stage allowed an endpoint.
                    if stage.external_endpoint().is_some() {
                        assert_eq!(stage, Stage::DockerSetup);
                    }
                }
            }
        }
    }

    #[test]
    fn container_lifecycle_is_never_charged_to_github() {
        for stage in [
            Stage::ContainerCreate,
            Stage::ContainerStart,
            Stage::FirstUserCommand,
            Stage::Teardown,
            Stage::Admission,
            Stage::Capacity,
        ] {
            assert_eq!(stage.lane(), TelemetryLane::Velnor, "{}", stage.as_str());
        }
    }

    #[test]
    fn only_the_remote_fetch_phase_is_external() {
        for phase in CheckoutPhase::ALL {
            let expected = if phase == CheckoutPhase::MirrorFetch {
                TelemetryLane::Github
            } else {
                TelemetryLane::Velnor
            };
            assert_eq!(phase.lane(), expected, "{}", phase.as_str());
        }
    }

    #[test]
    fn stages_round_trip_through_json() {
        for stage in Stage::ALL {
            let json = serde_json::to_string(&stage).expect("serialise");
            assert_eq!(json, format!("\"{}\"", stage.as_str()));
            assert_eq!(serde_json::from_str::<Stage>(&json).unwrap(), stage);
        }
    }
}
