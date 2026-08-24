//! The approved resource nouns and the untagged-at-rest [`AnyResource`]
//! envelope that carries any of them on the wire.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::condition::ResourceMeta;
use crate::phase::{SlotKind, SlotPhase};
use crate::sanitized::RepositoryRef;
use crate::sanitized::SanitizedUrl;
use crate::time::{DurationMs, Timestamp};

/// One host running Velnor daemons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Hostname of the machine.
    pub hostname: String,
    /// Velnor agent version installed, when known.
    #[serde(default)]
    pub agent_version: Option<String>,
    /// Scheduling labels advertised by the host.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// One daemon instance process running on a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Hostname the instance runs on.
    pub host: String,
    /// Version of the daemon binary.
    pub version: String,
    /// Uptime in whole milliseconds; `null` when unknown.
    #[serde(default)]
    pub uptime_ms: Option<DurationMs>,
    /// Number of slots configured for this instance.
    pub slots_configured: u32,
    /// Number of slots currently busy.
    pub slots_busy: u32,
}

/// One execution slot with its operator-visible phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Hostname owning the slot.
    pub host: String,
    /// Zero-based slot index within its instance.
    pub index: u32,
    /// Stable-slot versus ephemeral-runner class.
    ///
    /// Serialized `slotKind`: the wire tag `kind` names the resource noun
    /// itself and can never be reused by a payload field.
    pub slot_kind: SlotKind,
    /// Current lifecycle phase.
    pub phase: SlotPhase,
    /// Canonical name of the job currently occupying the slot.
    #[serde(default)]
    pub job: Option<String>,
}

/// One runner registration as GitHub sees it; never a credential field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerRegistration {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Labels advertised at registration.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Whether the registration is created for one job.
    pub ephemeral: bool,
    /// Whether GitHub reports the runner connected.
    pub online: bool,
}

/// One workflow job with machine-safe durations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Repository the job belongs to.
    pub repository: RepositoryRef,
    /// Canonical name of the owning run, when known.
    #[serde(default)]
    pub run: Option<String>,
    /// Workflow file path.
    pub workflow: String,
    /// Head branch, when known.
    #[serde(default)]
    pub head_branch: Option<String>,
    /// Queue wait in whole milliseconds; `null` when unknown.
    #[serde(default)]
    pub queued_ms: Option<DurationMs>,
    /// Execution duration in whole milliseconds; `null` when unknown.
    #[serde(default)]
    pub duration_ms: Option<DurationMs>,
    /// Workflow conclusion; remains data unless an exit-status contract says
    /// otherwise.
    #[serde(default)]
    pub conclusion: Option<String>,
}

/// One workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Repository the run belongs to.
    pub repository: RepositoryRef,
    /// Human run number within the repository.
    pub number: u64,
    /// Head commit SHA.
    pub head_sha: String,
    /// Head branch.
    pub head_branch: String,
    /// Triggering event name.
    pub event: String,
    /// Upstream status token (`queued`, `in_progress`, `completed`).
    pub status: String,
    /// Workflow conclusion; remains data unless an exit-status contract says
    /// otherwise.
    #[serde(default)]
    pub conclusion: Option<String>,
    /// Sanitized run URL; credentials are dropped at construction.
    #[serde(default)]
    pub url: Option<SanitizedUrl>,
}

/// One entry observed waiting in a queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Zero-based position in the observed queue.
    pub position: u64,
    /// Canonical job name.
    pub job: String,
    /// Observed wait in whole milliseconds; `null` when unknown, never zero
    /// by construction.
    #[serde(default)]
    pub wait_ms: Option<DurationMs>,
}

/// One observed control-plane event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Monotonic sequence within its stream.
    pub sequence: u64,
    /// RFC 3339 instant the event occurred.
    pub occurred_at: Timestamp,
    /// Stable event kind (dot-separated).
    ///
    /// Serialized `eventKind`: the envelope wire tag `kind` names the
    /// resource noun and can never be reused by a payload field.
    pub event_kind: String,
    /// Canonical subject name.
    pub subject: String,
    /// Human-facing detail, when present.
    #[serde(default)]
    pub detail: Option<String>,
}

/// One durable space reservation held by the filesystem coordinator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reservation {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Canonical slot name the reservation bounds.
    pub slot: String,
    /// Machine-stable purpose token.
    pub purpose: String,
    /// RFC 3339 expiry deadline.
    pub expires_at: Timestamp,
}

/// One lease tracked by the coordinator or daemons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Holder canonical name.
    pub holder: String,
    /// Time-to-live in whole milliseconds; `null` when unbounded.
    #[serde(default)]
    pub ttl_ms: Option<DurationMs>,
    /// RFC 3339 expiry deadline.
    pub expires_at: Timestamp,
}

/// One declared capability from the strict capability manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Capability key exactly as declared.
    pub key: String,
    /// Whether the capability is supported by this build.
    pub supported: bool,
    /// Human-facing details, when present.
    #[serde(default)]
    pub details: Option<String>,
}

/// One native action adapter and its pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Adapter {
    /// Shared identity, provenance, and condition header.
    #[serde(flatten)]
    pub meta: ResourceMeta,
    /// Adapter name (`owner/action` served).
    pub adapter: String,
    /// Upstream action version pinned.
    pub version: String,
    /// Exact action refs handled by the adapter.
    #[serde(default)]
    pub actions: Vec<String>,
}

/// Any approved resource noun, discriminated on the wire by `resourceKind`.
///
/// The tag is internal so every noun serializes flat with its metadata at
/// the top level; the tag spelling deliberately avoids colliding with the
/// `Event.kind` payload field. Unknown kinds fail closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resourceKind", rename_all = "PascalCase")]
pub enum AnyResource {
    Host(Host),
    Instance(Instance),
    Slot(Slot),
    RunnerRegistration(RunnerRegistration),
    Job(Job),
    Run(Run),
    QueueEntry(QueueEntry),
    Event(Event),
    Reservation(Reservation),
    Lease(Lease),
    Capability(Capability),
    Adapter(Adapter),
}

impl AnyResource {
    /// Wire discriminator for this variant.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            AnyResource::Host(_) => "Host",
            AnyResource::Instance(_) => "Instance",
            AnyResource::Slot(_) => "Slot",
            AnyResource::RunnerRegistration(_) => "RunnerRegistration",
            AnyResource::Job(_) => "Job",
            AnyResource::Run(_) => "Run",
            AnyResource::QueueEntry(_) => "QueueEntry",
            AnyResource::Event(_) => "Event",
            AnyResource::Reservation(_) => "Reservation",
            AnyResource::Lease(_) => "Lease",
            AnyResource::Capability(_) => "Capability",
            AnyResource::Adapter(_) => "Adapter",
        }
    }

    /// Shared metadata header of the wrapped resource.
    #[must_use]
    pub const fn meta(&self) -> &ResourceMeta {
        match self {
            AnyResource::Host(r) => &r.meta,
            AnyResource::Instance(r) => &r.meta,
            AnyResource::Slot(r) => &r.meta,
            AnyResource::RunnerRegistration(r) => &r.meta,
            AnyResource::Job(r) => &r.meta,
            AnyResource::Run(r) => &r.meta,
            AnyResource::QueueEntry(r) => &r.meta,
            AnyResource::Event(r) => &r.meta,
            AnyResource::Reservation(r) => &r.meta,
            AnyResource::Lease(r) => &r.meta,
            AnyResource::Capability(r) => &r.meta,
            AnyResource::Adapter(r) => &r.meta,
        }
    }

    /// `<kind>:<name>` identity line used by renderers and diagnostics.
    #[must_use]
    pub fn identity(&self) -> String {
        format!("{}:{}", self.kind(), self.meta().name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Source;

    fn stamp() -> Timestamp {
        Timestamp::parse("2026-08-24T00:00:00Z").expect("fixed stamp")
    }

    #[test]
    fn every_approved_noun_is_modeled_once() {
        let fixtures = vec![
            AnyResource::Host(Host {
                meta: ResourceMeta::new("h", Source::Local, stamp()),
                hostname: "h".to_owned(),
                agent_version: None,
                labels: BTreeMap::new(),
            }),
            AnyResource::Instance(Instance {
                meta: ResourceMeta::new("i", Source::Local, stamp()),
                host: "h".to_owned(),
                version: "0.1.0".to_owned(),
                uptime_ms: None,
                slots_configured: 1,
                slots_busy: 0,
            }),
            AnyResource::Slot(Slot {
                meta: ResourceMeta::new("s", Source::Local, stamp()),
                host: "h".to_owned(),
                index: 0,
                slot_kind: SlotKind::Stable,
                phase: SlotPhase::Idle,
                job: None,
            }),
            AnyResource::RunnerRegistration(RunnerRegistration {
                meta: ResourceMeta::new("r", Source::Github, stamp()),
                labels: BTreeMap::new(),
                ephemeral: false,
                online: true,
            }),
            AnyResource::Job(Job {
                meta: ResourceMeta::new("j", Source::Merged, stamp()),
                repository: RepositoryRef::new("o", "n"),
                run: None,
                workflow: "w.yml".to_owned(),
                head_branch: None,
                queued_ms: None,
                duration_ms: None,
                conclusion: None,
            }),
            AnyResource::Run(Run {
                meta: ResourceMeta::new("run", Source::Github, stamp()),
                repository: RepositoryRef::new("o", "n"),
                number: 1,
                head_sha: "a".to_owned(),
                head_branch: "main".to_owned(),
                event: "push".to_owned(),
                status: "completed".to_owned(),
                conclusion: None,
                url: None,
            }),
            AnyResource::QueueEntry(QueueEntry {
                meta: ResourceMeta::new("q", Source::Local, stamp()),
                position: 0,
                job: "j".to_owned(),
                wait_ms: None,
            }),
            AnyResource::Event(Event {
                meta: ResourceMeta::new("e", Source::Local, stamp()),
                sequence: 1,
                occurred_at: stamp(),
                event_kind: "slot.phase-changed".to_owned(),
                subject: "slot/s".to_owned(),
                detail: None,
            }),
            AnyResource::Reservation(Reservation {
                meta: ResourceMeta::new("res", Source::Local, stamp()),
                slot: "slot/s".to_owned(),
                purpose: "job-acquire".to_owned(),
                expires_at: stamp(),
            }),
            AnyResource::Lease(Lease {
                meta: ResourceMeta::new("lease", Source::Local, stamp()),
                holder: "instance/i".to_owned(),
                ttl_ms: None,
                expires_at: stamp(),
            }),
            AnyResource::Capability(Capability {
                meta: ResourceMeta::new("cap", Source::Local, stamp()),
                key: "actions.checkout".to_owned(),
                supported: true,
                details: None,
            }),
            AnyResource::Adapter(Adapter {
                meta: ResourceMeta::new("ad", Source::Local, stamp()),
                adapter: "actions/checkout".to_owned(),
                version: "v6".to_owned(),
                actions: Vec::new(),
            }),
        ];
        assert_eq!(fixtures.len(), 12);
        let mut seen_kinds: Vec<&str> = fixtures.iter().map(|r| r.kind()).collect();
        seen_kinds.sort_unstable();
        seen_kinds.dedup();
        assert_eq!(seen_kinds.len(), 12);
    }

    #[test]
    fn wire_field_names_are_stable_camel_case_with_flat_meta() {
        let slot = Slot {
            meta: ResourceMeta::new("sentry-slot-0", Source::Local, stamp()),
            host: "sentry".to_owned(),
            index: 0,
            slot_kind: SlotKind::Stable,
            phase: SlotPhase::WaitingForCapacity,
            job: None,
        };
        let json = serde_json::to_string(&slot).expect("serialize");
        for key in [
            "\"schemaVersion\":1",
            "\"name\":\"sentry-slot-0\"",
            "\"source\":\"LOCAL\"",
            "\"lastTransitionTime\":\"2026-08-24T00:00:00Z\"",
            "\"host\":\"sentry\"",
            "\"index\":0",
            "\"slotKind\":\"stable\"",
            "\"phase\":\"waiting-for-capacity\"",
            "\"job\":null",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
    }

    #[test]
    fn any_resource_round_trips_through_json() {
        let run = Run {
            meta: ResourceMeta::new("run-1", Source::Github, stamp()),
            repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
            number: 84,
            head_sha: "abc".to_owned(),
            head_branch: "main".to_owned(),
            event: "workflow_dispatch".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            url: Some(SanitizedUrl::project(
                "https://github.com/tailrocks/velnor-actions-fixture/actions/runs/84",
            )),
        };
        let wrapped = AnyResource::Run(run.clone());
        let text = serde_json::to_string(&wrapped).expect("serialize");
        assert!(
            text.contains("\"resourceKind\":\"Run\""),
            "missing resourceKind tag in {text}"
        );
        let back: AnyResource = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, wrapped);
        assert_eq!(back.identity(), "Run:run-1");
        assert_eq!(back.meta().name, "run-1");
    }
}
