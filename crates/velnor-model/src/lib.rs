//! Shared versioned model types for the Velnor control plane, transport
//! contract, and operator CLI.
//!
//! Dependency law (Plan 064): every other new crate may depend on this one;
//! this crate depends on none of them and never on Clap or Axum.
//!
//! Plan 065 owns the versioned resource nouns: every object carries stable
//! identity, provenance ([`Source`]), conditions, reason, message, and an
//! RFC 3339 [`Timestamp`] under a numeric [`SCHEMA_VERSION`]. Tables are
//! views of these types, never the source of truth.

pub mod cli_meta;
pub mod condition;
pub mod error_envelope;
pub mod job_summary;
pub mod lifecycle;
pub mod microvm;
pub mod node;
pub mod phase;
pub mod resources;
pub mod sanitized;
pub mod scheduler;
pub mod since;
pub mod source;
pub mod time;

pub use cli_meta::{CommandMetadata, FlagMetadata, SchemaDocument};
pub use condition::{Condition, ConditionStatus, Labels, ResourceMeta};
pub use error_envelope::{exit_code_for_class, ExitClass, MachineErrorEnvelope};
pub use job_summary::{
    InfrastructureCategory, InvalidJobSummaryField, JobConclusion, JobPhase, JobSummary,
    NormalizedJob, Slug, TriggerEvent, MAX_SLUG_LEN,
};
pub use lifecycle::{transition_target, EventReason, InvalidLifecycleToken, JobState};
pub use microvm::{
    GuestIsolation, IsolationRejected, JobExecutorKind, MicroVmControl, MicroVmControlRejected,
    MicroVmKind, MicroVmNotLive, MicroVmNotProven, FIRECRACKER_DEVICES, FIRECRACKER_REPO_URL,
    FIRECRACKER_SPEC_URL, JAILER_CONTROLS,
};
pub use node::{
    ActorPhase, CanaryStatus, CapacityPermit, FleetHealthState, Generation, HealthDocument, JobId,
    NotReady, ReadyProof, SliDimension, SlotId,
};
pub use phase::{SlotKind, SlotPhase};
pub use resources::{
    Adapter, AnyResource, Capability, Event, Host, Instance, Job, Lease, QueueEntry, Reservation,
    Run, RunnerRegistration, Slot,
};
pub use sanitized::{IdentityRef, RepositoryRef, SanitizedUrl, SecretRef};
pub use scheduler::{
    RunnerScaleSetMessageResponse, RunnerScaleSetStatistic, ScaleSetJobMessageType,
    ScaleSetNotProven, SchedulerKind, SCALESET_API_VERSION, SCALESET_ENDPOINT,
    SCALESET_MAX_CAPACITY_HEADER, SCALESET_UPSTREAM_COMMIT,
};
pub use since::{InvalidSince, Since};
pub use source::Source;
pub use time::{DurationMs, DurationOverflowError, InvalidTimestamp, Timestamp};

/// Crate version reported by `velnorctl --version`.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Schema version stamped in every [`ResourceMeta`] header. Bumped only on
/// breaking field changes; later additions must be optional or versioned.
pub const SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_version_is_reported() {
        assert!(!super::CRATE_VERSION.is_empty());
    }

    #[test]
    fn schema_version_is_the_first_generation() {
        assert_eq!(super::SCHEMA_VERSION, 1);
    }
}
