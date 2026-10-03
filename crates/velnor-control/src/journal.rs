//! Durable node journal: WAL + `synchronous=FULL`, immutable events, reducer.
//!
//! Side-effect commands are returned only after the intent event is committed.
//! Completions are fail-closed around one durable local send claim: an outbox
//! row survives until a remote acknowledgement (or observed terminal) is
//! itself committed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use velnor_model::{
    CanaryStatus, ExecutionBackendKind, FleetHealthState, Generation, HealthDocument, JobId,
    JobPhase2, ReadyProof, SlotId, SlotPhase2,
};

use crate::store::error::{StoreError, StoreResult};

/// Minimum bundled SQLite that includes the WAL-reset fix (3.51.3).
pub const MIN_SQLITE_VERSION: (u32, u32, u32) = (3, 51, 3);

/// Current journal schema. Older writers seeing a higher `PRAGMA user_version`
/// must not apply events (N-1 must not clobber an N writer's log). Version 10
/// fences pre-baseline schema-8 writers that would otherwise delete the
/// replay anchor from `meta` during their next state persist, and adds durable
/// disk-pressure episodes, launch fences, and both bounded pressure deadlines.
/// Version 11 binds each fleet journal to one service instance and fences old
/// writers from changing or ignoring that identity.
///
/// Every terminal-affecting event rides a bump here. `Journal::open` stamps
/// the current version onto an older journal *before* any event may be
/// written, so a binary that predates the bump refuses the file outright
/// instead of decoding it with an incomplete event vocabulary.
///
/// This migration is forward-only. To recover with a v8 binary, stop every
/// journal writer and restore a consistent pre-v9 SQLite backup as one set:
/// the main database plus its `-wal` and `-shm` sidecars when present. Never
/// lower `user_version`, drop the replay-baseline keys, or delete the fence on
/// a live v11 database; those actions destroy the migration boundary.
pub const JOURNAL_SCHEMA_VERSION: u32 = 11;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_RETRIES: u32 = 5;
const SETUP_BACKOFF_STEP: Duration = Duration::from_millis(40);
const MAX_TERMINAL_ACK_SCAN_ROWS: i64 = 1_024;
const REPLAY_BASELINE_KEY: &str = "replay_baseline_v1";
const REPLAY_BASELINE_CHECKSUM_KEY: &str = "replay_baseline_sha256_v1";
const JOURNAL_WRITE_GATE_TABLE: &str = "journal_write_gate";
const JOURNAL_WRITE_FENCE_REASON: &str = "journal.write.fenced";
const PRESSURE_TERMINAL_RECOVERY_WORKER_PREFIX: &str = "velnor-pressure-terminal-recovery:";
const LEGACY_REPLAY_BASELINE_DELETE_FENCE_TRIGGER: &str = "replay_baseline_delete_fence";
const LEGACY_REPLAY_BASELINE_RENAME_FENCE_TRIGGER: &str = "replay_baseline_rename_fence";

const JOURNAL_WRITE_FENCE_TRIGGERS: [(&str, &str, &str); 27] = [
    ("journal_write_fence_events_insert", "events", "INSERT"),
    ("journal_write_fence_events_update", "events", "UPDATE"),
    ("journal_write_fence_events_delete", "events", "DELETE"),
    ("journal_write_fence_slots_insert", "slots", "INSERT"),
    ("journal_write_fence_slots_update", "slots", "UPDATE"),
    ("journal_write_fence_slots_delete", "slots", "DELETE"),
    ("journal_write_fence_jobs_insert", "jobs", "INSERT"),
    ("journal_write_fence_jobs_update", "jobs", "UPDATE"),
    ("journal_write_fence_jobs_delete", "jobs", "DELETE"),
    ("journal_write_fence_outbox_insert", "outbox", "INSERT"),
    ("journal_write_fence_outbox_update", "outbox", "UPDATE"),
    ("journal_write_fence_outbox_delete", "outbox", "DELETE"),
    ("journal_write_fence_meta_insert", "meta", "INSERT"),
    ("journal_write_fence_meta_update", "meta", "UPDATE"),
    ("journal_write_fence_meta_delete", "meta", "DELETE"),
    (
        "journal_write_fence_disk_pressure_episodes_insert",
        "disk_pressure_episodes",
        "INSERT",
    ),
    (
        "journal_write_fence_disk_pressure_episodes_update",
        "disk_pressure_episodes",
        "UPDATE",
    ),
    (
        "journal_write_fence_disk_pressure_episodes_delete",
        "disk_pressure_episodes",
        "DELETE",
    ),
    (
        "journal_write_fence_disk_pressure_launches_insert",
        "disk_pressure_launches",
        "INSERT",
    ),
    (
        "journal_write_fence_disk_pressure_launches_update",
        "disk_pressure_launches",
        "UPDATE",
    ),
    (
        "journal_write_fence_disk_pressure_launches_delete",
        "disk_pressure_launches",
        "DELETE",
    ),
    (
        "journal_write_fence_disk_pressure_observations_insert",
        "disk_pressure_observations",
        "INSERT",
    ),
    (
        "journal_write_fence_disk_pressure_observations_update",
        "disk_pressure_observations",
        "UPDATE",
    ),
    (
        "journal_write_fence_disk_pressure_observations_delete",
        "disk_pressure_observations",
        "DELETE",
    ),
    (
        "journal_write_fence_identity_insert",
        "journal_identity",
        "INSERT",
    ),
    (
        "journal_write_fence_identity_update",
        "journal_identity",
        "UPDATE",
    ),
    (
        "journal_write_fence_identity_delete",
        "journal_identity",
        "DELETE",
    ),
];

/// Durable send attempts a completion may burn before it is unresolvable.
/// Each attempt is one full transport retry loop, not one HTTP request.
pub const MAX_COMPLETION_ATTEMPTS: u32 = 8;

/// Wall-clock budget for resolving one completion, from the moment its intent
/// became durable. GitHub's own default job timeout is six hours; a payload
/// older than that can no longer be delivered usefully, so holding its slot
/// hostage buys nothing.
pub const COMPLETION_RESOLUTION_SECONDS: u64 = 6 * 60 * 60;

/// Durable probes a provisional acquisition may burn before it is abandoned.
///
/// Each probe is one full `renewjob` call, made once per slot startup, so this
/// is a count of restarts and not of HTTP requests. Eight is the completion
/// budget: past that many restarts the node, not the run service, is what is
/// broken, and every further probe renews a lease for a job this node has
/// repeatedly failed to make progress on.
pub const MAX_ACQUISITION_PROBES: u32 = 8;

/// Wall-clock budget for resolving one provisional acquisition, from the
/// moment the intent became durable.
///
/// The same six hours the completion budget uses, for the same reason: GitHub's
/// default job timeout is six hours, so a lease renewed past it buys nothing
/// that the run service will still honour, and holding the slot costs capacity.
pub const ACQUISITION_RESOLUTION_SECONDS: u64 = 6 * 60 * 60;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    generation INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    checksum TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_generation_kind_id_idx
    ON events (generation, kind, id DESC);
CREATE TABLE IF NOT EXISTS slots (
    slot_id TEXT PRIMARY KEY,
    generation INTEGER NOT NULL,
    phase TEXT NOT NULL,
    permit_held INTEGER NOT NULL DEFAULT 0,
    routing_valid INTEGER NOT NULL DEFAULT 0,
    session_live INTEGER NOT NULL DEFAULT 0,
    executor_proven INTEGER NOT NULL DEFAULT 0,
    registered INTEGER NOT NULL DEFAULT 0,
    pid INTEGER,
    heartbeat_unix INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS jobs (
    job_id TEXT PRIMARY KEY,
    slot_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    attempt INTEGER NOT NULL,
    worker TEXT NOT NULL,
    phase TEXT NOT NULL,
    accepted_unix INTEGER NOT NULL DEFAULT 0,
    terminal_conclusion TEXT,
    provisional INTEGER NOT NULL DEFAULT 0,
    plan_id TEXT NOT NULL DEFAULT '',
    run_service_url TEXT NOT NULL DEFAULT '',
    probe_attempts INTEGER NOT NULL DEFAULT 0,
    probe_deadline_unix INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS outbox (
    job_id TEXT PRIMARY KEY,
    slot_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    payload_sha256 TEXT NOT NULL,
    intended INTEGER NOT NULL DEFAULT 0,
    send_started INTEGER NOT NULL DEFAULT 0,
    remote_acked INTEGER NOT NULL DEFAULT 0,
    created_unix INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    deadline_unix INTEGER NOT NULL DEFAULT 0,
    permanent INTEGER NOT NULL DEFAULT 0,
    abandoned INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS journal_write_gate (
    id INTEGER PRIMARY KEY CHECK (id = 1)
);
CREATE TABLE IF NOT EXISTS disk_pressure_episodes (
    service_instance TEXT NOT NULL,
    filesystem_id TEXT NOT NULL,
    volume_fingerprint TEXT,
    episode_id TEXT NOT NULL,
    started_unix INTEGER NOT NULL,
    deadline_unix INTEGER NOT NULL,
    drain_deadline_unix INTEGER NOT NULL,
    last_observed_unix INTEGER NOT NULL,
    reclaim_attempted INTEGER NOT NULL DEFAULT 0,
    revision INTEGER NOT NULL,
    draining INTEGER NOT NULL DEFAULT 0,
    terminal INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (service_instance, filesystem_id)
);
CREATE TABLE IF NOT EXISTS disk_pressure_launches (
    service_instance TEXT NOT NULL,
    slot_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    launch_nonce TEXT NOT NULL,
    issued_unix INTEGER NOT NULL,
    active INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (service_instance, slot_id)
);
CREATE TABLE IF NOT EXISTS disk_pressure_observations (
    service_instance TEXT NOT NULL,
    filesystem_id TEXT NOT NULL,
    volume_fingerprint TEXT,
    identity_confirmed INTEGER NOT NULL DEFAULT 0,
    last_observed_unix INTEGER NOT NULL,
    PRIMARY KEY (service_instance, filesystem_id)
);
CREATE TABLE IF NOT EXISTS journal_identity (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    service_instance TEXT NOT NULL
);
";

/// Fleet materialization the reducer reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskPressureEpisode {
    pub episode_id: String,
    pub started_unix: u64,
    /// Degraded admission cutoff (D). It never moves during this episode.
    pub deadline_unix: u64,
    /// Fixed end of the drain window. It never moves during this episode.
    pub drain_deadline_unix: u64,
    /// Latched once an observation reaches the degraded deadline.
    pub draining: bool,
    pub last_observed_unix: u64,
    pub reclaim_attempted: bool,
    pub revision: u64,
    pub terminal: bool,
    /// Stable volume UUID captured from the pinned descriptor. `None` means
    /// the configured root has not yet yielded a trustworthy volume identity.
    pub volume_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskPressureObservation {
    /// `None` means the host is healthy and no episode remains active.
    pub episode: Option<DiskPressureEpisode>,
    pub cleared: bool,
    /// Exactly the first low observation claims the episode's one reclaim.
    /// The claim commits before cleanup; a crash after it may skip reclaim and
    /// still proceeds through the persisted D/E fail-closed timeline.
    pub reclaim_needed: bool,
}

/// One root measurement folded into the current pressure episode batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskPressureFilesystemSample {
    /// Physical `unix-device:<st_dev>` episode key, or a stable `root:<hash>`
    /// key while the configured root cannot be pinned to a device.
    pub filesystem_id: String,
    /// Configured-root keys that the current pinned batch proves belong to
    /// this physical device. Any unpinnable-root episode is merged into the
    /// device episode without moving the earliest deadline.
    pub alias_ids: Vec<String>,
    /// `None` is unknown capacity; it never authorizes reclaim or clearing.
    pub available_bytes: Option<u64>,
    pub min_free_bytes: u64,
    pub volume_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetState {
    pub control_live: bool,
    pub journal_writable: bool,
    pub github_reachable: bool,
    pub routing_valid: bool,
    pub runner_group_valid: bool,
    /// Latched fleet drain request (`meta[drain]`, never an `Event`: older
    /// binaries ignore the unknown meta key, so drain state is forward
    /// compatible without a schema bump or vocabulary change).
    pub drain_active: bool,
    /// Lifecycle resource version that requested the drain. Observability
    /// only; staleness is settled by the lifecycle ledger, not this value.
    pub drain_version: u64,
    /// Durable soft admission fence (`meta[admission]`). A cordon stops new
    /// registration and job acquisition while preserving the daemon and any
    /// in-flight work. It is separate from `drain_active`: drain exits the
    /// process, cordon does not.
    pub admission_blocked: bool,
    /// Lifecycle resource version that requested the admission fence.
    pub admission_version: u64,
    pub desired_ready: u32,
    pub canary: CanaryStatus,
    pub package_generation: u64,
    pub package_apt_version: String,
    pub execution_backend: ExecutionBackendKind,
    /// Whether the journal has received and materialized an explicit
    /// capacity declaration. A zero value before that declaration is the
    /// reducer's initialization state, not an observed zero-capacity fleet.
    capacity_declared: bool,
    /// Existing state was written by the retired surge-capacity model or is
    /// otherwise larger than the declared capacity.  It is forensic-only:
    /// no capacity-affecting event may be applied while this is set.
    pub capacity_invalid: bool,
    pub slots: Vec<SlotRecord>,
    pub jobs: Vec<JobRecord>,
    pub outbox: Vec<OutboxRecord>,
}

impl Default for FleetState {
    fn default() -> Self {
        Self {
            control_live: false,
            journal_writable: false,
            github_reachable: false,
            routing_valid: false,
            runner_group_valid: false,
            drain_active: false,
            drain_version: 0,
            admission_blocked: false,
            admission_version: 0,
            desired_ready: 0,
            canary: CanaryStatus::Unknown,
            package_generation: 0,
            package_apt_version: String::new(),
            // Packaged default until journal load; not a live fallback.
            execution_backend: ExecutionBackendKind::Docker,
            capacity_declared: false,
            capacity_invalid: false,
            slots: Vec::new(),
            jobs: Vec::new(),
            outbox: Vec::new(),
        }
    }
}

impl FleetState {
    #[must_use]
    pub fn health(&self) -> HealthDocument {
        let actual_ready = self
            .slots
            .iter()
            .filter(|slot| slot.phase.counts_as_ready())
            .count() as u32;
        let registered = self.slots.iter().filter(|slot| slot.registered).count() as u32;
        let permits = if self.admission_blocked || self.drain_active {
            0
        } else {
            self.slots.iter().filter(|slot| slot.permit_held).count() as u32
        };
        let executor_ready = self
            .slots
            .iter()
            .filter(|slot| slot.executor_proven)
            .count() as u32;
        HealthDocument {
            control_live: self.control_live && !self.capacity_invalid,
            journal_writable: self.journal_writable,
            github_reachable: self.github_reachable,
            routing_valid: self.routing_valid,
            runner_group_valid: self.runner_group_valid,
            desired_ready_slots: self.desired_ready,
            actual_ready_slots: actual_ready,
            registered_slots: registered,
            capacity_permits: permits,
            executor_ready_slots: executor_ready,
            oldest_queued_job_seconds: oldest_queued_job_seconds(&self.jobs),
            oldest_outbox_entry_seconds: oldest_outbox_age_seconds(&self.outbox),
            external_canary: self.canary,
            execution_backend: self.execution_backend,
            state: FleetHealthState::NotReady,
        }
        .with_derived_state()
    }

    fn slot_mut(&mut self, id: &SlotId) -> &mut SlotRecord {
        if let Some(index) = self.slots.iter().position(|slot| slot.slot_id == *id) {
            return &mut self.slots[index];
        }
        self.slots.push(SlotRecord::new(id.clone()));
        let index = self.slots.len() - 1;
        &mut self.slots[index]
    }

    #[must_use]
    pub fn advertised_capacity(&self) -> u32 {
        if self.capacity_invalid || self.admission_blocked || self.drain_active {
            return 0;
        }
        self.slots
            .iter()
            .filter(|slot| slot.permit_held && slot.phase.counts_as_ready())
            .count() as u32
    }
}

/// A pending outbox row is an admission barrier for its exact slot identity.
/// An owner that cannot be proven from durable state is a global barrier: it
/// must never be guessed or silently reassigned to another slot.
fn pending_outbox_blocks_admission(
    state: &FleetState,
    slot_id: &SlotId,
    generation: Generation,
) -> bool {
    state.outbox.iter().any(|row| {
        if !row.is_pending() {
            return false;
        }
        if row.slot_id == *slot_id && row.generation == generation {
            return true;
        }
        !outbox_owner_is_proven(state, row)
    })
}

fn fleet_admission_blocked(state: &FleetState) -> bool {
    state.admission_blocked || state.drain_active
}

fn outbox_owner_is_proven(state: &FleetState, row: &OutboxRecord) -> bool {
    state
        .slots
        .iter()
        .any(|slot| slot.slot_id == row.slot_id && slot.generation == row.generation)
        && state.jobs.iter().any(|job| {
            job.job_id == row.job_id
                && job.slot_id == row.slot_id
                && job.generation == row.generation
                // A provisional row records an *intent* to acquire. Treating it
                // as ownership would let a runner that never received a 200
                // publish a terminal completion for a job another runner owns.
                && !job.provisional
        })
}

fn slot_has_active_job(state: &FleetState, slot_id: &SlotId) -> bool {
    state
        .jobs
        .iter()
        .any(|job| job.slot_id == *slot_id && job.phase.occupies_slot())
}

fn restore_slot_after_job_removal(
    state: &mut FleetState,
    commands: &mut Vec<SideEffect>,
    job_id: &JobId,
) {
    let Some(job) = state.jobs.iter().find(|job| job.job_id == *job_id).cloned() else {
        return;
    };
    state.jobs.retain(|item| item.job_id != *job_id);
    let Some(index) = state
        .slots
        .iter()
        .position(|slot| slot.slot_id == job.slot_id)
    else {
        return;
    };
    if state.slots[index].generation != job.generation {
        return;
    }
    // Fencing is a recovery barrier. Job reconciliation must clear the row,
    // but only a healthy same-generation actor may reopen the slot.
    if state.slots[index].phase == SlotPhase2::Fenced {
        return;
    }
    if state.slots[index].ready_proof().is_ok() && state.slots[index].registered {
        state.slots[index].phase = SlotPhase2::Ready;
        commands.push(SideEffect::AdvertiseCapacity {
            permits: state.advertised_capacity(),
        });
    } else if state.slots[index].registered {
        state.slots[index].phase = SlotPhase2::Registered;
    } else {
        state.slots[index].phase = SlotPhase2::Provisioning;
    }
}

fn oldest_queued_job_seconds(jobs: &[JobRecord]) -> u64 {
    let now = unix_now();
    jobs.iter()
        .filter(|job| job.phase.occupies_slot() && job.accepted_unix > 0)
        .map(|job| now.saturating_sub(job.accepted_unix))
        .max()
        .unwrap_or(0)
}

fn stamp_event(event: &mut Event) {
    if let Event::JobOwned { accepted_unix, .. } = event
        && *accepted_unix == 0
    {
        *accepted_unix = unix_now();
    }
}

fn oldest_outbox_age_seconds(outbox: &[OutboxRecord]) -> u64 {
    let now = unix_now();
    outbox
        .iter()
        .filter(|row| row.is_pending())
        .map(|row| now.saturating_sub(row.created_unix))
        .max()
        .unwrap_or(0)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRecord {
    pub slot_id: SlotId,
    pub generation: Generation,
    pub phase: SlotPhase2,
    pub permit_held: bool,
    pub routing_valid: bool,
    pub session_live: bool,
    pub executor_proven: bool,
    pub registered: bool,
    pub pid: Option<u32>,
    pub heartbeat_unix: u64,
}

impl SlotRecord {
    fn new(slot_id: SlotId) -> Self {
        Self {
            slot_id,
            generation: Generation::INITIAL,
            phase: SlotPhase2::Absent,
            permit_held: false,
            routing_valid: false,
            session_live: false,
            executor_proven: false,
            registered: false,
            pid: None,
            heartbeat_unix: 0,
        }
    }

    pub fn ready_proof(&self) -> Result<ReadyProof, velnor_model::NotReady> {
        ReadyProof::try_new(
            self.permit_held,
            self.routing_valid,
            self.session_live,
            self.executor_proven,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: JobId,
    pub slot_id: SlotId,
    pub generation: Generation,
    pub attempt: u32,
    pub worker: String,
    pub phase: JobPhase2,
    pub accepted_unix: u64,
    /// Terminal conclusion recorded by `JobTerminalResult` before the
    /// completion payload was serialised. Recovery must reuse this instead of
    /// synthesising a failure for a job that had already finished green.
    pub terminal_conclusion: Option<String>,
    /// True while the runner has *told GitHub it intends to acquire* this job
    /// but has not seen a 200 back.
    ///
    /// The row exists so the slot is occupied and a crash leaves evidence, but
    /// it is deliberately not proof of ownership: `outbox_owner_is_proven`
    /// refuses a provisional row, so no completion can ever be sent against
    /// one. `JobOwned` clears it.
    pub provisional: bool,
    /// Run-service plan holding this job. Empty until `JobAcquisitionResolved`
    /// retargets the row onto the identity the acquire reply carried: the
    /// broker message that opens the acquisition names no plan, and `renewjob`
    /// needs one, so a row without this cannot be probed.
    pub plan_id: String,
    /// Run-service base URL this acquisition was addressed to. Known from the
    /// broker message, so it is durable from the intent onward and recovery
    /// never has to guess where to probe.
    pub run_service_url: String,
    /// Durable count of spent recovery probes. Only the reducer moves it.
    pub probe_attempts: u32,
    /// Wall-clock instant past which this provisional row is unresolvable.
    pub probe_deadline_unix: u64,
}

impl JobRecord {
    /// Whether the recovery budget for a provisional row is spent.
    ///
    /// Mirrors `OutboxRecord::budget_exhausted`, and exists for the same
    /// reason: `renewjob` extends the lease as a side effect, so a job this
    /// node owns but can never execute must not be probed forever. The
    /// reducer, not the caller, decides when the row may be abandoned.
    #[must_use]
    pub fn probe_budget_exhausted(&self, now: u64) -> bool {
        self.probe_attempts >= MAX_ACQUISITION_PROBES
            || (self.probe_deadline_unix > 0 && now >= self.probe_deadline_unix)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRecord {
    pub job_id: JobId,
    pub slot_id: SlotId,
    pub generation: Generation,
    pub payload_sha256: String,
    pub intended: bool,
    pub send_started: bool,
    pub remote_acked: bool,
    pub created_unix: u64,
    /// Durable count of exhausted send attempts. Only the reducer moves it.
    pub attempts: u32,
    /// Wall-clock instant past which this completion is unresolvable.
    pub deadline_unix: u64,
    /// The remote rejected the payload in a way retrying cannot change.
    pub permanent: bool,
    /// Bounded terminal state: the completion could not be resolved inside its
    /// attempt and time budget. The send claim is never released, so this can
    /// never become a second terminal send; it only stops the row from
    /// blocking admission forever.
    pub abandoned: bool,
}

impl OutboxRecord {
    /// A row still owed to the remote service.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.intended && !self.remote_acked && !self.abandoned
    }

    /// Whether the recovery budget for this row is spent. The reducer refuses
    /// `CompletionUnresolvable` for a row that still has budget, so the
    /// terminal state can never be asserted by a caller's say-so alone.
    #[must_use]
    pub fn budget_exhausted(&self, now: u64) -> bool {
        self.permanent
            || self.attempts >= MAX_COMPLETION_ATTEMPTS
            || (self.deadline_unix > 0 && now >= self.deadline_unix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReplayBaselineSource {
    Empty,
    LegacyMaterialized,
}

/// Versioned, closed baseline envelope. Keep this shape strict: a baseline is
/// replay input, so silently accepting a missing or future semantic field
/// would produce a state that only looks valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayBaseline {
    format_version: u32,
    source: ReplayBaselineSource,
    state: ReplayBaselineState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayBaselineState {
    control_live: bool,
    journal_writable: bool,
    github_reachable: bool,
    routing_valid: bool,
    runner_group_valid: bool,
    desired_ready: u32,
    canary: CanaryStatus,
    package_generation: u64,
    package_apt_version: String,
    execution_backend: ExecutionBackendKind,
    capacity_declared: bool,
    capacity_invalid: bool,
    slots: Vec<ReplayBaselineSlot>,
    jobs: Vec<ReplayBaselineJob>,
    outbox: Vec<ReplayBaselineOutbox>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayBaselineSlot {
    slot_id: SlotId,
    generation: Generation,
    phase: SlotPhase2,
    permit_held: bool,
    routing_valid: bool,
    session_live: bool,
    executor_proven: bool,
    registered: bool,
    pid: Option<u32>,
    heartbeat_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayBaselineJob {
    job_id: JobId,
    slot_id: SlotId,
    generation: Generation,
    attempt: u32,
    worker: String,
    phase: JobPhase2,
    accepted_unix: u64,
    terminal_conclusion: Option<String>,
    provisional: bool,
    plan_id: String,
    run_service_url: String,
    probe_attempts: u32,
    probe_deadline_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayBaselineOutbox {
    job_id: JobId,
    slot_id: SlotId,
    generation: Generation,
    payload_sha256: String,
    intended: bool,
    send_started: bool,
    remote_acked: bool,
    created_unix: u64,
    attempts: u32,
    deadline_unix: u64,
    permanent: bool,
    abandoned: bool,
}

impl ReplayBaselineState {
    fn from_fleet(state: &FleetState) -> Self {
        Self {
            control_live: state.control_live,
            journal_writable: state.journal_writable,
            github_reachable: state.github_reachable,
            routing_valid: state.routing_valid,
            runner_group_valid: state.runner_group_valid,
            desired_ready: state.desired_ready,
            canary: state.canary,
            package_generation: state.package_generation,
            package_apt_version: state.package_apt_version.clone(),
            execution_backend: state.execution_backend,
            capacity_declared: state.capacity_declared,
            capacity_invalid: state.capacity_invalid,
            slots: state
                .slots
                .iter()
                .cloned()
                .map(ReplayBaselineSlot::from)
                .collect(),
            jobs: state
                .jobs
                .iter()
                .cloned()
                .map(ReplayBaselineJob::from)
                .collect(),
            outbox: state
                .outbox
                .iter()
                .cloned()
                .map(ReplayBaselineOutbox::from)
                .collect(),
        }
    }

    fn into_fleet(self) -> FleetState {
        FleetState {
            control_live: self.control_live,
            journal_writable: self.journal_writable,
            github_reachable: self.github_reachable,
            routing_valid: self.routing_valid,
            runner_group_valid: self.runner_group_valid,
            // Drain and admission are lifecycle meta overlays, not replay
            // events. They are read from the current materialized snapshot.
            drain_active: false,
            drain_version: 0,
            admission_blocked: false,
            admission_version: 0,
            desired_ready: self.desired_ready,
            canary: self.canary,
            package_generation: self.package_generation,
            package_apt_version: self.package_apt_version,
            execution_backend: self.execution_backend,
            capacity_declared: self.capacity_declared,
            capacity_invalid: self.capacity_invalid,
            slots: self.slots.into_iter().map(Into::into).collect(),
            jobs: self.jobs.into_iter().map(Into::into).collect(),
            outbox: self.outbox.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<SlotRecord> for ReplayBaselineSlot {
    fn from(slot: SlotRecord) -> Self {
        Self {
            slot_id: slot.slot_id,
            generation: slot.generation,
            phase: slot.phase,
            permit_held: slot.permit_held,
            routing_valid: slot.routing_valid,
            session_live: slot.session_live,
            executor_proven: slot.executor_proven,
            registered: slot.registered,
            pid: slot.pid,
            heartbeat_unix: slot.heartbeat_unix,
        }
    }
}

impl From<ReplayBaselineSlot> for SlotRecord {
    fn from(slot: ReplayBaselineSlot) -> Self {
        Self {
            slot_id: slot.slot_id,
            generation: slot.generation,
            phase: slot.phase,
            permit_held: slot.permit_held,
            routing_valid: slot.routing_valid,
            session_live: slot.session_live,
            executor_proven: slot.executor_proven,
            registered: slot.registered,
            pid: slot.pid,
            heartbeat_unix: slot.heartbeat_unix,
        }
    }
}

impl From<JobRecord> for ReplayBaselineJob {
    fn from(job: JobRecord) -> Self {
        Self {
            job_id: job.job_id,
            slot_id: job.slot_id,
            generation: job.generation,
            attempt: job.attempt,
            worker: job.worker,
            phase: job.phase,
            accepted_unix: job.accepted_unix,
            terminal_conclusion: job.terminal_conclusion,
            provisional: job.provisional,
            plan_id: job.plan_id,
            run_service_url: job.run_service_url,
            probe_attempts: job.probe_attempts,
            probe_deadline_unix: job.probe_deadline_unix,
        }
    }
}

impl From<ReplayBaselineJob> for JobRecord {
    fn from(job: ReplayBaselineJob) -> Self {
        Self {
            job_id: job.job_id,
            slot_id: job.slot_id,
            generation: job.generation,
            attempt: job.attempt,
            worker: job.worker,
            phase: job.phase,
            accepted_unix: job.accepted_unix,
            terminal_conclusion: job.terminal_conclusion,
            provisional: job.provisional,
            plan_id: job.plan_id,
            run_service_url: job.run_service_url,
            probe_attempts: job.probe_attempts,
            probe_deadline_unix: job.probe_deadline_unix,
        }
    }
}

impl From<OutboxRecord> for ReplayBaselineOutbox {
    fn from(row: OutboxRecord) -> Self {
        Self {
            job_id: row.job_id,
            slot_id: row.slot_id,
            generation: row.generation,
            payload_sha256: row.payload_sha256,
            intended: row.intended,
            send_started: row.send_started,
            remote_acked: row.remote_acked,
            created_unix: row.created_unix,
            attempts: row.attempts,
            deadline_unix: row.deadline_unix,
            permanent: row.permanent,
            abandoned: row.abandoned,
        }
    }
}

impl From<ReplayBaselineOutbox> for OutboxRecord {
    fn from(row: ReplayBaselineOutbox) -> Self {
        Self {
            job_id: row.job_id,
            slot_id: row.slot_id,
            generation: row.generation,
            payload_sha256: row.payload_sha256,
            intended: row.intended,
            send_started: row.send_started,
            remote_acked: row.remote_acked,
            created_unix: row.created_unix,
            attempts: row.attempts,
            deadline_unix: row.deadline_unix,
            permanent: row.permanent,
            abandoned: row.abandoned,
        }
    }
}

/// Intent events. The reducer never performs I/O.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ControlLive,
    JournalWritable,
    Dependency {
        github_reachable: bool,
    },
    Routing {
        valid: bool,
        group_valid: bool,
    },
    DesiredCapacity {
        ready: u32,
    },
    PermitReserved {
        slot_id: SlotId,
        generation: Generation,
    },
    ExecutorProven {
        slot_id: SlotId,
        generation: Generation,
    },
    SessionLive {
        slot_id: SlotId,
        generation: Generation,
    },
    RegistrationIntended {
        slot_id: SlotId,
        generation: Generation,
    },
    Registered {
        slot_id: SlotId,
        generation: Generation,
    },
    /// GitHub no longer has the runner identity recorded for this slot.
    /// Clear the local registration claim so reconciliation can issue a fresh
    /// JIT request instead of trusting split-brain state forever.
    RegistrationLost {
        slot_id: SlotId,
        generation: Generation,
    },
    ReadyAttempt {
        slot_id: SlotId,
        generation: Generation,
    },
    /// Written *before* `acquirejob` is called, so a crash between the call and
    /// its reply leaves durable evidence that this runner may already own the
    /// job. Recovery resolves it with `renewjob`, which only the lease holder
    /// can call successfully — the 409 from `acquirejob` cannot be used, since
    /// upstream's `RunServiceError` carries no runner identity.
    JobAcquisitionIntended {
        slot_id: SlotId,
        job_id: JobId,
        generation: Generation,
        message_id: String,
        /// Where the acquisition was addressed. Carried from the broker message
        /// so recovery knows which run service to probe without re-deriving it.
        run_service_url: String,
        /// When the intent became durable. The reducer is pure, so the probe
        /// deadline has to be stamped from the caller's clock, exactly as
        /// `JobOwned` stamps `accepted_unix`.
        intended_unix: u64,
    },
    /// The acquire reply came back and named the job. Retargets the provisional
    /// row from the broker message identity onto the run-service identity, and
    /// records the plan so `renewjob` becomes possible.
    ///
    /// One event, because the alternative is two: drop the message-keyed row
    /// and create the job-keyed one. That pair frees the slot in between, which
    /// destroys exactly the evidence this whole mechanism exists to keep, and
    /// reintroduces the lost-acquisition window a few microseconds wide.
    ///
    /// The row stays provisional. It is promoted by `JobOwned` once the runner
    /// has committed to running the job, and the window between the two is the
    /// one a `renewjob` probe can settle for certain.
    JobAcquisitionResolved {
        provisional_job_id: JobId,
        acquired_job_id: JobId,
        plan_id: String,
        generation: Generation,
    },
    /// One recovery probe was spent without reaching a verdict. Charged to the
    /// row's durable budget so an unreachable run service cannot make this node
    /// renew the same lease on every restart forever.
    AcquisitionProbeFailed {
        job_id: JobId,
        generation: Generation,
    },
    /// The provisional row could not be resolved to ownership: `acquirejob`
    /// reported the message gone, or the probe proved another runner holds it.
    JobAcquisitionLost {
        job_id: JobId,
        generation: Generation,
        reason: String,
    },
    JobOwned {
        job_id: JobId,
        slot_id: SlotId,
        attempt: u32,
        generation: Generation,
        worker: String,
        #[serde(default)]
        accepted_unix: u64,
    },
    JobStarted {
        job_id: JobId,
        generation: Generation,
    },
    /// The job produced a terminal result. Written *before* the completion
    /// payload is serialised, so a crash in that window leaves durable proof
    /// of what the job actually concluded. Without it, recovery can only guess,
    /// and guessing turns a green job into a synthetic failure.
    JobTerminalResult {
        job_id: JobId,
        generation: Generation,
        /// Wire conclusion string as the run service will be told it.
        conclusion: String,
    },
    CompletionIntended {
        job_id: JobId,
        generation: Generation,
        payload_sha256: String,
    },
    CompletionSendStarted {
        job_id: JobId,
        generation: Generation,
    },
    RemoteAcked {
        job_id: JobId,
        generation: Generation,
    },
    RemoteObservedTerminal {
        job_id: JobId,
        generation: Generation,
    },
    /// One completion send attempt was spent without reaching a terminal
    /// acknowledgement. This is the durable attempt counter: recovery is
    /// bounded because every failed attempt costs budget that survives a
    /// crash, rather than restarting from zero on each controller cycle.
    CompletionAttemptFailed {
        job_id: JobId,
        generation: Generation,
        /// The remote refused the payload in a way retrying cannot change.
        permanent: bool,
    },
    /// Bounded terminal state for a completion that can never be acknowledged.
    ///
    /// The reducer refuses this unless the row's durable attempt or time
    /// budget is actually spent, so it cannot be asserted by a caller's
    /// say-so. It marks the row abandoned and frees the slot; it never sets
    /// `remote_acked` and never releases the send claim, so an abandoned
    /// completion can never become a second terminal send on any generation.
    CompletionUnresolvable {
        job_id: JobId,
        generation: Generation,
        /// Operator-facing explanation, recorded immutably in the log.
        reason: String,
    },
    /// The local completion payload is missing or no longer matches the
    /// checksum committed by `CompletionIntended`. This is terminal local
    /// evidence: the remote service is never told that the job completed.
    CompletionPayloadLost {
        job_id: JobId,
        generation: Generation,
        payload_sha256: String,
        /// Operator-facing explanation, recorded immutably in the log.
        reason: String,
    },
    /// A live job worker disappeared without a terminal completion (for
    /// example killed by a daemon drain or an OS reboot). The job cannot
    /// finish; the slot must return to Ready so capacity is not lost forever.
    JobWorkerLost {
        job_id: JobId,
        generation: Generation,
    },
    CleanupIntended {
        slot_id: SlotId,
        isolation_id: String,
        generation: Generation,
    },
    SlotHeartbeat {
        slot_id: SlotId,
        generation: Generation,
        pid: u32,
    },
    SlotStale {
        slot_id: SlotId,
        generation: Generation,
    },
    /// Terminal host-pressure deadline fence. Unlike `SlotStale`, this
    /// revokes the slot generation even while its job row remains occupied;
    /// the controller stops the actor and resolves job/outbox ownership next.
    DiskPressureTerminalFence {
        slot_id: SlotId,
        generation: Generation,
    },
    CanaryObserved {
        status: CanaryStatus,
    },
    /// Installed apt generation is live.
    PackageActivated {
        apt_version: String,
        generation: u64,
    },
    /// Retire an old apt generation only when no job or outbox still names it.
    PackageRetireIntended {
        generation: u64,
    },
}

/// Effects the I/O layer may run only after the matching intent is durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SideEffect {
    RegisterRunner {
        slot_id: SlotId,
        generation: Generation,
    },
    AdvertiseCapacity {
        permits: u32,
    },
    SendCompletion {
        job_id: JobId,
        generation: Generation,
        payload_sha256: String,
    },
    Cleanup {
        isolation_id: String,
        generation: Generation,
    },
    DeleteOutbox {
        job_id: JobId,
        generation: Generation,
    },
    SpawnSlot {
        slot_id: SlotId,
        generation: Generation,
    },
    FenceSlot {
        slot_id: SlotId,
        generation: Generation,
    },
}

/// One completion the node gave up on, read back from the immutable log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvableCompletion {
    pub job_id: JobId,
    pub generation: Generation,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceOutcome {
    pub state: FleetState,
    pub commands: Vec<SideEffect>,
    pub rejected: bool,
}

/// Pure `State + Event -> New State + Commands`. No I/O.
#[must_use]
pub fn reduce(mut state: FleetState, event: Event) -> ReduceOutcome {
    let mut commands = Vec::new();
    let mut rejected = false;
    match event {
        Event::ControlLive => state.control_live = true,
        Event::JournalWritable => state.journal_writable = true,
        Event::Dependency { github_reachable } => {
            state.github_reachable = github_reachable;
            // GitHub down is degraded, never a restart storm.
        }
        Event::Routing { valid, group_valid } => {
            state.routing_valid = valid;
            state.runner_group_valid = group_valid;
            for slot in &mut state.slots {
                slot.routing_valid = valid && group_valid;
            }
        }
        Event::DesiredCapacity { ready } => {
            state.desired_ready = ready;
            state.capacity_declared = true;
        }
        Event::PermitReserved {
            slot_id,
            generation,
        } => {
            let routing = state.routing_valid && state.runner_group_valid;
            let slot_admission_blocked = slot_has_active_job(&state, &slot_id)
                || pending_outbox_blocks_admission(&state, &slot_id, generation);
            // A drain or cordon stops new capacity, never in-flight work: no
            // fresh permit may spawn a slot while admission is fenced.
            // Deliberately unconditional: a durable marker in state always
            // gates new work (fail-closed).
            let admission_blocked = fleet_admission_blocked(&state);
            let slot = state.slot_mut(&slot_id);
            if generation < slot.generation
                || slot_admission_blocked
                || admission_blocked
                || (generation == slot.generation && slot.phase == SlotPhase2::Fenced)
            {
                rejected = true;
            } else {
                if generation > slot.generation {
                    // A newer generation is a new actor identity. Never let
                    // proofs or process metadata from the fenced predecessor
                    // satisfy this generation's Ready contract.
                    slot.executor_proven = false;
                    slot.session_live = false;
                    slot.registered = false;
                    slot.pid = None;
                    slot.heartbeat_unix = 0;
                    slot.phase = SlotPhase2::Provisioning;
                }
                slot.generation = generation;
                slot.permit_held = true;
                slot.routing_valid = routing;
                if slot.phase == SlotPhase2::Absent {
                    slot.phase = SlotPhase2::Provisioning;
                }
                commands.push(SideEffect::SpawnSlot {
                    slot_id,
                    generation,
                });
            }
        }
        Event::ExecutorProven {
            slot_id,
            generation,
        } => {
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || slot.phase == SlotPhase2::Fenced {
                rejected = true;
            } else {
                slot.executor_proven = true;
            }
        }
        Event::SessionLive {
            slot_id,
            generation,
        } => {
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || slot.phase == SlotPhase2::Fenced {
                rejected = true;
            } else {
                slot.session_live = true;
            }
        }
        Event::RegistrationIntended {
            slot_id,
            generation,
        } => {
            let slot_admission_blocked = slot_has_active_job(&state, &slot_id)
                || pending_outbox_blocks_admission(&state, &slot_id, generation);
            let fleet_blocked = fleet_admission_blocked(&state);
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation
                || slot.phase == SlotPhase2::Fenced
                || slot_admission_blocked
                || fleet_blocked
                || slot.ready_proof().is_err()
            {
                rejected = true;
            } else {
                commands.push(SideEffect::RegisterRunner {
                    slot_id,
                    generation,
                });
            }
        }
        Event::Registered {
            slot_id,
            generation,
        } => {
            let slot_admission_blocked = slot_has_active_job(&state, &slot_id)
                || pending_outbox_blocks_admission(&state, &slot_id, generation);
            let fleet_blocked = fleet_admission_blocked(&state);
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation
                || slot.phase == SlotPhase2::Fenced
                || slot_admission_blocked
                || fleet_blocked
            {
                rejected = true;
            } else {
                slot.registered = true;
                slot.phase = SlotPhase2::Registered;
            }
        }
        Event::RegistrationLost {
            slot_id,
            generation,
        } => {
            let draining = slot_has_active_job(&state, &slot_id)
                || pending_outbox_blocks_admission(&state, &slot_id, generation);
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || slot.phase == SlotPhase2::Fenced || !slot.registered
            {
                rejected = true;
            } else {
                slot.registered = false;
                // The remote runner identity and broker session are gone.
                // Release admission atomically, but leave any active job and
                // outbox rows untouched until their normal teardown path.
                slot.permit_held = false;
                slot.session_live = false;
                if draining {
                    slot.phase = SlotPhase2::Fenced;
                    commands.push(SideEffect::FenceSlot {
                        slot_id,
                        generation,
                    });
                } else {
                    slot.phase = SlotPhase2::Provisioning;
                }
            }
        }
        Event::ReadyAttempt {
            slot_id,
            generation,
        } => {
            let slot_admission_blocked = slot_has_active_job(&state, &slot_id)
                || pending_outbox_blocks_admission(&state, &slot_id, generation);
            let fleet_blocked = fleet_admission_blocked(&state);
            {
                let slot = state.slot_mut(&slot_id);
                if generation != slot.generation
                    || slot.phase == SlotPhase2::Fenced
                    || slot_admission_blocked
                    || fleet_blocked
                {
                    rejected = true;
                } else if slot.ready_proof().is_ok() && slot.registered {
                    slot.phase = SlotPhase2::Ready;
                } else {
                    rejected = true;
                }
            }
            if !rejected {
                commands.push(SideEffect::AdvertiseCapacity {
                    permits: state.advertised_capacity(),
                });
            }
        }
        Event::JobAcquisitionIntended {
            slot_id,
            job_id,
            generation,
            message_id: _,
            run_service_url,
            intended_unix,
        } => {
            // Occupy the slot before calling GitHub, so a crash in the acquire
            // window leaves evidence. This is the only transition that moves a
            // slot to `Assigned` — the slot must be `Assigned` for `JobOwned`
            // to be accepted — while the job row it creates is deliberately
            // provisional: it is not proof of ownership and cannot back a
            // completion.
            // A drain or cordon stops new acquisitions, never in-flight work:
            // an already-intended job still resolves, owns, and completes.
            // Deliberately unconditional: fail-closed like the permit arm.
            let admission_blocked = fleet_admission_blocked(&state);
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation
                || slot.phase != SlotPhase2::Ready
                || admission_blocked
                || state.jobs.iter().any(|job| job.job_id == job_id)
            {
                rejected = true;
            } else {
                state.slot_mut(&slot_id).phase = SlotPhase2::Assigned;
                state.jobs.push(JobRecord {
                    job_id,
                    slot_id,
                    generation,
                    attempt: 0,
                    worker: String::new(),
                    phase: JobPhase2::Assigned,
                    accepted_unix: 0,
                    terminal_conclusion: None,
                    provisional: true,
                    // No plan id exists yet: the broker message that opens an
                    // acquisition names none, and only the acquire reply does.
                    plan_id: String::new(),
                    run_service_url,
                    probe_attempts: 0,
                    probe_deadline_unix: intended_unix
                        .saturating_add(ACQUISITION_RESOLUTION_SECONDS),
                });
            }
        }
        Event::JobAcquisitionResolved {
            provisional_job_id,
            acquired_job_id,
            plan_id,
            generation,
        } => {
            // Only a provisional row may be retargeted, and only onto an
            // identity nothing else already holds. Rewriting an owned row's
            // identity would silently move a job that may already have a
            // terminal result or an outbox payload attached to it.
            let target_taken = acquired_job_id != provisional_job_id
                && state.jobs.iter().any(|job| job.job_id == acquired_job_id);
            let row = state.jobs.iter_mut().find(|job| {
                job.job_id == provisional_job_id && job.generation == generation && job.provisional
            });
            match row {
                Some(row) if !target_taken => {
                    row.job_id = acquired_job_id;
                    row.plan_id = plan_id;
                }
                _ => rejected = true,
            }
        }
        Event::AcquisitionProbeFailed { job_id, generation } => {
            let row = state.jobs.iter_mut().find(|job| {
                job.job_id == job_id && job.generation == generation && job.provisional
            });
            match row {
                Some(row) => row.probe_attempts = row.probe_attempts.saturating_add(1),
                None => rejected = true,
            }
        }
        Event::JobAcquisitionLost {
            job_id,
            generation,
            reason: _,
        } => {
            // Only a provisional row may be dropped this way. Once ownership is
            // proven the job has to reach a terminal state through completion,
            // never by being forgotten.
            let droppable = state
                .jobs
                .iter()
                .any(|job| job.job_id == job_id && job.generation == generation && job.provisional);
            if droppable {
                restore_slot_after_job_removal(&mut state, &mut commands, &job_id);
            } else {
                rejected = true;
            }
        }
        Event::JobOwned {
            job_id,
            slot_id,
            attempt,
            generation,
            worker,
            accepted_unix,
        } => {
            let slot = state.slots.iter().find(|slot| slot.slot_id == slot_id);
            let slot_generation = slot.map(|slot| slot.generation);
            let slot_phase = slot.map(|slot| slot.phase);
            let newer_job = state
                .jobs
                .iter()
                .any(|job| job.job_id == job_id && job.generation > generation);
            let other_live = state.jobs.iter().any(|job| {
                job.slot_id == slot_id && job.job_id != job_id && job.phase.occupies_slot()
            });
            let terminal_handoff_row = worker.starts_with(PRESSURE_TERMINAL_RECOVERY_WORKER_PREFIX)
                && state.jobs.iter().any(|job| {
                    job.job_id == job_id
                        && job.slot_id == slot_id
                        && job.generation == generation
                        && job.provisional
                        && !job.plan_id.is_empty()
                });
            let slot_phase_allowed = slot_phase == Some(SlotPhase2::Assigned)
                || (slot_phase == Some(SlotPhase2::Fenced) && terminal_handoff_row);
            if newer_job || slot_generation != Some(generation) || !slot_phase_allowed || other_live
            {
                rejected = true;
            } else {
                // Carry the acquisition's addressing across the promotion.
                // Losing it here would leave an owned row that recovery can no
                // longer probe if the process dies before the job starts.
                let acquisition = state
                    .jobs
                    .iter()
                    .find(|job| job.job_id == job_id)
                    .map(|job| (job.plan_id.clone(), job.run_service_url.clone()));
                let (plan_id, run_service_url) = acquisition.unwrap_or_default();
                state.jobs.retain(|job| job.job_id != job_id);
                state.jobs.push(JobRecord {
                    job_id: job_id.clone(),
                    slot_id,
                    generation,
                    attempt,
                    worker,
                    phase: JobPhase2::Assigned,
                    accepted_unix,
                    terminal_conclusion: None,
                    // A 200 came back: this row is now proof of ownership.
                    provisional: false,
                    plan_id,
                    run_service_url,
                    // The row is no longer provisional, so nothing probes it.
                    probe_attempts: 0,
                    probe_deadline_unix: 0,
                });
            }
        }
        Event::JobStarted { job_id, generation } => {
            let job_slot_id = state
                .jobs
                .iter()
                .find(|job| job.job_id == job_id)
                .map(|job| job.slot_id.clone());
            let slot_generation = job_slot_id.and_then(|slot_id| {
                state
                    .slots
                    .iter()
                    .find(|slot| slot.slot_id == slot_id)
                    .map(|slot| slot.generation)
            });
            if let Some(job) = state.jobs.iter_mut().find(|job| job.job_id == job_id) {
                if job.generation != generation || slot_generation != Some(generation) {
                    rejected = true;
                } else {
                    job.phase = JobPhase2::Running;
                }
            } else {
                rejected = true;
            }
        }
        Event::JobTerminalResult {
            job_id,
            generation,
            conclusion,
        } => {
            let slot_generation =
                state
                    .jobs
                    .iter()
                    .find(|job| job.job_id == job_id)
                    .and_then(|job| {
                        state
                            .slots
                            .iter()
                            .find(|slot| slot.slot_id == job.slot_id)
                            .map(|slot| slot.generation)
                    });
            match state.jobs.iter_mut().find(|job| job.job_id == job_id) {
                Some(job)
                    if job.generation == generation
                        && slot_generation == Some(generation)
                        && job.phase.occupies_slot()
                        // A terminal result is written once. A second, different
                        // conclusion for the same job generation is a caller bug,
                        // never a correction.
                        && job
                            .terminal_conclusion
                            .as_ref()
                            .is_none_or(|recorded| *recorded == conclusion) =>
                {
                    job.terminal_conclusion = Some(conclusion);
                    job.phase = JobPhase2::Completing;
                }
                _ => rejected = true,
            }
        }
        Event::CompletionIntended {
            job_id,
            generation,
            payload_sha256,
        } => {
            if let Some(job_index) = state.jobs.iter().position(|job| job.job_id == job_id) {
                let job = &state.jobs[job_index];
                let slot_generation = state
                    .slots
                    .iter()
                    .find(|slot| slot.slot_id == job.slot_id)
                    .map(|slot| slot.generation);
                // A provisional row records an intent to acquire, not ownership.
                // This check has to be here as well as in
                // `outbox_owner_is_proven`, because that one is only consulted
                // when a row already exists — the *first* intent would otherwise
                // create one against a job this runner may not own.
                if job.provisional
                    || job.generation != generation
                    || slot_generation != Some(generation)
                {
                    rejected = true;
                } else if let Some(outbox_index) =
                    state.outbox.iter().position(|row| row.job_id == job_id)
                {
                    // Completion intent is a durable prepare record. Replaying the
                    // same prepare must not replace the row: replacement used to
                    // clear `send_started`, allowing concurrent/replayed callers to
                    // issue more than one terminal send.
                    let row = &state.outbox[outbox_index];
                    if row.generation != generation
                        || !row.is_pending()
                        || row.payload_sha256 != payload_sha256
                        || !outbox_owner_is_proven(&state, row)
                    {
                        rejected = true;
                    } else if state.jobs[job_index].phase != JobPhase2::Completing {
                        state.jobs[job_index].phase = JobPhase2::Completing;
                    }
                } else {
                    let created = unix_now();
                    state.jobs[job_index].phase = JobPhase2::Completing;
                    state.outbox.push(OutboxRecord {
                        job_id: job_id.clone(),
                        slot_id: state.jobs[job_index].slot_id.clone(),
                        generation,
                        payload_sha256: payload_sha256.clone(),
                        intended: true,
                        send_started: false,
                        remote_acked: false,
                        created_unix: created,
                        attempts: 0,
                        deadline_unix: created.saturating_add(COMPLETION_RESOLUTION_SECONDS),
                        permanent: false,
                        abandoned: false,
                    });
                    commands.push(SideEffect::SendCompletion {
                        job_id,
                        generation,
                        payload_sha256,
                    });
                }
            } else {
                rejected = true;
            }
        }
        Event::CompletionSendStarted { job_id, generation } => {
            if let Some(index) = state.outbox.iter().position(|row| row.job_id == job_id) {
                let valid = {
                    let row = &state.outbox[index];
                    row.generation == generation
                        && row.is_pending()
                        && !row.send_started
                        && outbox_owner_is_proven(&state, row)
                };
                if valid {
                    state.outbox[index].send_started = true;
                } else {
                    rejected = true;
                }
            } else {
                rejected = true;
            }
        }
        Event::RemoteAcked { job_id, generation }
        | Event::RemoteObservedTerminal { job_id, generation } => {
            let job_slot_generation =
                state
                    .jobs
                    .iter()
                    .find(|job| job.job_id == job_id)
                    .and_then(|job| {
                        state
                            .slots
                            .iter()
                            .find(|slot| slot.slot_id == job.slot_id)
                            .map(|slot| (job.generation, slot.generation))
                    });
            if let Some(index) = state.outbox.iter().position(|row| row.job_id == job_id) {
                let valid = {
                    let row = &state.outbox[index];
                    row.generation == generation
                        && row.is_pending()
                        && row.send_started
                        && job_slot_generation == Some((generation, generation))
                        && outbox_owner_is_proven(&state, row)
                };
                if valid {
                    state.outbox[index].remote_acked = true;
                    commands.push(SideEffect::DeleteOutbox {
                        job_id: job_id.clone(),
                        generation,
                    });
                    restore_slot_after_job_removal(&mut state, &mut commands, &job_id);
                } else {
                    rejected = true;
                }
            } else {
                rejected = true;
            }
        }
        Event::CompletionAttemptFailed {
            job_id,
            generation,
            permanent,
        } => {
            if let Some(index) = state.outbox.iter().position(|row| row.job_id == job_id) {
                let valid = {
                    let row = &state.outbox[index];
                    row.generation == generation
                        && row.is_pending()
                        && row.send_started
                        && outbox_owner_is_proven(&state, row)
                };
                if valid {
                    state.outbox[index].attempts = state.outbox[index].attempts.saturating_add(1);
                    state.outbox[index].permanent |= permanent;
                } else {
                    rejected = true;
                }
            } else {
                rejected = true;
            }
        }
        Event::CompletionUnresolvable {
            job_id,
            generation,
            reason: _,
        } => {
            if let Some(index) = state.outbox.iter().position(|row| row.job_id == job_id) {
                let valid = {
                    let row = &state.outbox[index];
                    row.generation == generation
                        && row.is_pending()
                        && outbox_owner_is_proven(&state, row)
                        // The budget must already be spent in durable state.
                        // Recovery is bounded because the budget only ever
                        // shrinks, never because a caller says it is done.
                        && row.budget_exhausted(unix_now())
                };
                if valid {
                    // `remote_acked` deliberately stays false and the send
                    // claim is never released: this is a local abandonment,
                    // not a delivery, and it must never authorize a second
                    // terminal send on this or any later generation.
                    state.outbox[index].abandoned = true;
                    commands.push(SideEffect::DeleteOutbox {
                        job_id: job_id.clone(),
                        generation,
                    });
                    restore_slot_after_job_removal(&mut state, &mut commands, &job_id);
                } else {
                    rejected = true;
                }
            } else {
                rejected = true;
            }
        }
        Event::CompletionPayloadLost {
            job_id,
            generation,
            payload_sha256,
            reason: _,
        } => {
            let valid = state
                .outbox
                .iter()
                .find(|row| row.job_id == job_id)
                .is_some_and(|row| {
                    row.generation == generation
                        && row.payload_sha256 == payload_sha256
                        && row.is_pending()
                        && outbox_owner_is_proven(&state, row)
                        && state.jobs.iter().any(|job| {
                            job.job_id == job_id
                                && job.slot_id == row.slot_id
                                && job.generation == generation
                                && !job.provisional
                                && job.phase == JobPhase2::Completing
                        })
                });
            if valid {
                // Keep `remote_acked` false and never release a send claim:
                // local payload loss is not remote delivery and must not
                // authorize a fabricated or second terminal send.
                if let Some(row) = state.outbox.iter_mut().find(|row| row.job_id == job_id) {
                    row.abandoned = true;
                }
                commands.push(SideEffect::DeleteOutbox {
                    job_id: job_id.clone(),
                    generation,
                });
                restore_slot_after_job_removal(&mut state, &mut commands, &job_id);
            } else {
                rejected = true;
            }
        }
        Event::JobWorkerLost { job_id, generation } => {
            let job = state.jobs.iter().find(|job| job.job_id == job_id);
            match job {
                Some(job)
                    if job.generation == generation
                        && state
                            .slots
                            .iter()
                            .find(|slot| slot.slot_id == job.slot_id)
                            .is_some_and(|slot| slot.generation == generation) =>
                {
                    // A completing job may still have an outbox payload to
                    // send; preserve its slot ownership until remote
                    // terminal acknowledgement supplies the second proof.
                    let pending_outbox = state.outbox.iter().any(|row| {
                        row.job_id == job_id && row.generation == generation && row.is_pending()
                    });
                    if !pending_outbox {
                        restore_slot_after_job_removal(&mut state, &mut commands, &job_id);
                    }
                }
                _ => rejected = true,
            }
        }
        Event::CleanupIntended {
            slot_id,
            isolation_id,
            generation,
        } => {
            let slot_generation = state
                .slots
                .iter()
                .find(|slot| slot.slot_id == slot_id)
                .map(|slot| slot.generation);
            if slot_generation != Some(generation) {
                rejected = true;
            } else {
                commands.push(SideEffect::Cleanup {
                    isolation_id,
                    generation,
                });
            }
        }
        Event::SlotHeartbeat {
            slot_id,
            generation,
            pid,
        } => {
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || slot.phase == SlotPhase2::Fenced {
                rejected = true;
            } else {
                slot.pid = Some(pid);
                slot.heartbeat_unix = unix_now();
            }
        }
        Event::SlotStale {
            slot_id,
            generation,
        } => {
            let occupied = slot_has_active_job(&state, &slot_id);
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || occupied {
                rejected = true;
            } else {
                slot.phase = SlotPhase2::Fenced;
                commands.push(SideEffect::FenceSlot {
                    slot_id,
                    generation,
                });
            }
        }
        Event::DiskPressureTerminalFence {
            slot_id,
            generation,
        } => {
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation {
                rejected = true;
            } else {
                slot.phase = SlotPhase2::Fenced;
                commands.push(SideEffect::FenceSlot {
                    slot_id,
                    generation,
                });
            }
        }
        Event::CanaryObserved { status } => state.canary = status,
        Event::PackageActivated {
            apt_version,
            generation,
        } => {
            state.package_generation = generation;
            state.package_apt_version = apt_version;
        }
        Event::PackageRetireIntended { generation } => {
            let pending_outbox = state.outbox.iter().any(OutboxRecord::is_pending);
            if generation != state.package_generation || !state.jobs.is_empty() || pending_outbox {
                rejected = true;
            } else {
                state.package_generation = 0;
                state.package_apt_version.clear();
            }
        }
    }
    ReduceOutcome {
        state,
        commands,
        rejected,
    }
}

/// Opened journal on local disk only.
#[derive(Debug, Clone)]
struct WorkerLaunchFence {
    service_instance: String,
    slot_id: SlotId,
    generation: i64,
    launch_nonce: String,
}

#[derive(Debug, Clone)]
struct AcquisitionIntentFence {
    service_instance: String,
    slot_id: SlotId,
    generation: i64,
    launch_nonce: String,
    provisional_job_id: JobId,
    message_id: String,
    run_service_url: String,
}

#[derive(Debug, Clone)]
struct AcquisitionResponseFence {
    service_instance: String,
    slot_id: SlotId,
    generation: i64,
    launch_nonce: String,
    provisional_job_id: JobId,
    message_id: String,
    run_service_url: String,
}

#[derive(Debug, Clone)]
struct AcquisitionAbandonFence {
    service_instance: String,
    slot_id: SlotId,
    generation: i64,
    launch_nonce: String,
    provisional_job_id: JobId,
    message_id: String,
    run_service_url: String,
}

#[derive(Debug, Clone)]
struct AcquisitionRecoveryFence {
    service_instance: String,
    slot_id: SlotId,
    generation: i64,
    acquired_job_id: JobId,
    plan_id: String,
    run_service_url: String,
}

#[derive(Debug)]
pub struct Journal {
    conn: Connection,
    path: PathBuf,
    worker_launch: Option<WorkerLaunchFence>,
    service_instance: Option<String>,
}

#[derive(Debug, Clone)]
struct JournalWriterContext {
    service_instance: Option<String>,
}

impl JournalWriterContext {
    fn validate(&self, conn: &Connection) -> StoreResult<()> {
        let stored: Option<String> = conn
            .query_row(
                "SELECT service_instance FROM journal_identity WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match (stored.as_deref(), self.service_instance.as_deref()) {
            (None, None) => Ok(()),
            (Some(stored), Some(requested)) if stored == requested => Ok(()),
            (Some(_), None) => Err(journal_service_instance_mismatch(
                "an unbound journal handle cannot mutate a service-owned database",
            )),
            (None, Some(_)) => Err(journal_service_instance_mismatch(
                "the journal has no owner row for this service context",
            )),
            (Some(_), Some(_)) => Err(journal_service_instance_mismatch(
                "the journal belongs to a different service context",
            )),
        }
    }

    fn begin_write(&self, tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
        begin_journal_write_gate(tx)?;
        self.validate(tx)
    }
}

impl Journal {
    /// Open (creating) a journal file. Parent directory must already exist.
    ///
    /// # Errors
    /// Missing parent, SQLite older than the WAL-reset fix, or schema setup.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        Self::open_inner(path.as_ref(), None, None)
    }

    /// Open a controller journal bound to the sole service instance that owns
    /// this fleet database. Fleet events and materialized rows are global, so a
    /// second service identity cannot safely share this database. Supported
    /// mutations enforce the binding inside their write transaction; direct SQL
    /// writes are outside the journal API contract.
    pub fn open_for_service_instance(
        path: impl AsRef<Path>,
        service_instance: &str,
    ) -> StoreResult<Self> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        let mut journal = Self::open_inner(path.as_ref(), None, Some(service_instance))?;
        let transaction = journal
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_replay_baseline_before_write(&transaction)?;
        begin_journal_write_gate(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        journal.service_instance = Some(service_instance.to_owned());
        Ok(journal)
    }

    /// Open a journal handle whose every mutation is atomically fenced by one
    /// slot generation and launch nonce.
    pub fn open_for_launch(
        path: impl AsRef<Path>,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
        launch_nonce: &str,
    ) -> StoreResult<Self> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        validate_disk_pressure_key(launch_nonce, "launch nonce")?;
        let fence = WorkerLaunchFence {
            service_instance: service_instance.to_owned(),
            slot_id: slot_id.clone(),
            generation: disk_pressure_sql_integer(generation.0, "launch generation")?,
            launch_nonce: launch_nonce.to_owned(),
        };
        Self::open_worker_inner(path.as_ref(), fence)
    }

    /// Open a bound worker handle from controller-supplied launch metadata.
    /// No metadata means the caller is a controller or an administrative
    /// process; a partial set is an error and cannot become an unbound writer.
    pub fn open_for_launch_from_environment(path: impl AsRef<Path>) -> StoreResult<Self> {
        const SERVICE: &str = "VELNOR_DISK_PRESSURE_INSTANCE";
        const SLOT: &str = "VELNOR_DISK_PRESSURE_SLOT_ID";
        const GENERATION: &str = "VELNOR_DISK_PRESSURE_GENERATION";
        const NONCE: &str = "VELNOR_DISK_PRESSURE_LAUNCH_NONCE";
        let service = std::env::var(SERVICE).ok();
        let slot = std::env::var(SLOT).ok();
        let generation = std::env::var(GENERATION).ok();
        let nonce = std::env::var(NONCE).ok();
        let count = [
            service.is_some(),
            slot.is_some(),
            generation.is_some(),
            nonce.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if count == 0 {
            return Self::open(path);
        }
        if count != 4 {
            return Err(disk_pressure_state_invalid(
                "worker launch environment is incomplete".to_owned(),
            ));
        }
        let generation = generation
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| disk_pressure_state_invalid("invalid launch generation".to_owned()))?;
        Self::open_for_launch(
            path,
            service.as_deref().unwrap_or_default(),
            &SlotId(slot.unwrap_or_default()),
            Generation(generation),
            nonce.as_deref().unwrap_or_default(),
        )
    }

    fn open_inner(
        path: &Path,
        worker_launch: Option<WorkerLaunchFence>,
        requested_service_instance: Option<&str>,
    ) -> StoreResult<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.is_dir()
        {
            return Err(StoreError::new(
                velnor_model::ExitClass::Unavailable,
                "journal.parent.missing",
            )
            .with_remediation(format!(
                "create directory {} before opening {}",
                parent.display(),
                path.display()
            )));
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let mut attempt = 0;
        loop {
            match setup_journal(&mut conn, requested_service_instance) {
                Ok(()) => break,
                Err(error) if is_transient_contention(&error) && attempt < SETUP_RETRIES => {
                    attempt += 1;
                    std::thread::sleep(SETUP_BACKOFF_STEP * attempt);
                }
                Err(error) => return Err(error),
            }
        }
        let journal = Self {
            conn,
            path: path.to_path_buf(),
            worker_launch,
            service_instance: None,
        };
        // Verify all existing event checksums once. The controller's steady
        // state must not replay an ever-growing log every two seconds.
        journal.load_state()?;
        Ok(journal)
    }

    fn open_worker_inner(path: &Path, worker_launch: WorkerLaunchFence) -> StoreResult<Self> {
        if !path.is_file() {
            return Err(StoreError::new(
                velnor_model::ExitClass::Unavailable,
                "journal.file.missing",
            )
            .with_remediation("controller must initialize the journal before launching a worker"));
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        configure_journal_connection(&conn)?;
        let transaction = conn.unchecked_transaction().map_err(StoreError::from)?;
        validate_replay_integrity_before_read(&transaction)?;
        validate_journal_service_instance(&transaction, &worker_launch.service_instance)?;
        let bound_service: Option<String> = transaction
            .query_row(
                "SELECT service_instance FROM journal_identity WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if bound_service.as_deref() != Some(&worker_launch.service_instance) {
            return Err(journal_service_instance_mismatch(
                "controller must bind the fleet journal before opening a worker launch",
            ));
        }
        if !disk_pressure_launch_is_current(
            &transaction,
            &worker_launch.service_instance,
            &worker_launch.slot_id,
            worker_launch.generation,
            &worker_launch.launch_nonce,
        )? {
            return Err(disk_pressure_launch_fenced());
        }
        transaction.commit()?;
        let journal = Self {
            conn,
            path: path.to_path_buf(),
            worker_launch: Some(worker_launch),
            service_instance: None,
        };
        journal.load_state()?;
        Ok(journal)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn validate_worker_context(
        &self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
        launch_nonce: &str,
    ) -> StoreResult<()> {
        if let Some(fence) = &self.worker_launch {
            if fence.service_instance != service_instance
                || fence.slot_id != *slot_id
                || fence.generation != disk_pressure_sql_integer(generation.0, "launch generation")?
                || fence.launch_nonce != launch_nonce
            {
                return Err(disk_pressure_launch_fenced());
            }
        } else if self.service_instance.as_deref() != Some(service_instance) {
            return Err(journal_service_instance_mismatch(
                "launch-scoped mutation requires this service's bound controller or worker handle",
            ));
        }
        Ok(())
    }

    fn require_controller_authority(&self) -> StoreResult<()> {
        if self.worker_launch.is_some() {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.disk_pressure.worker_authority",
            )
            .with_remediation(
                "worker-bound journal handles cannot perform controller operations",
            ));
        }
        self.writer_context().validate(&self.conn)
    }

    fn writer_context(&self) -> JournalWriterContext {
        JournalWriterContext {
            service_instance: self
                .worker_launch
                .as_ref()
                .map(|fence| fence.service_instance.as_str())
                .or(self.service_instance.as_deref())
                .map(str::to_owned),
        }
    }

    fn validate_acquisition_handle(
        &self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
        launch_nonce: &str,
    ) -> StoreResult<()> {
        if self.worker_launch.is_some() {
            self.validate_worker_context(service_instance, slot_id, generation, launch_nonce)
        } else if self.service_instance.as_deref() == Some(service_instance) {
            Ok(())
        } else {
            Err(journal_service_instance_mismatch(
                "acquisition operations require a journal bound to this service instance and launch",
            ))
        }
    }

    fn validate_pressure_service_instance(&self, service_instance: &str) -> StoreResult<()> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        let writer_context = self.writer_context();
        if writer_context.service_instance.as_deref() != Some(service_instance) {
            return Err(journal_service_instance_mismatch(
                "pressure operations require this service's bound controller or worker handle",
            ));
        }
        validate_journal_service_instance(&self.conn, service_instance)
    }

    /// Issue the single current writer lease for a slot launch. A new nonce
    /// fences every worker from the previous launch, including one that wakes
    /// after generation reuse.
    pub fn issue_disk_pressure_launch(
        &self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
        issued_unix: u64,
    ) -> StoreResult<String> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "generation")?;
        let issued_sql = disk_pressure_sql_integer(issued_unix, "launch time")?;
        let nonce = uuid::Uuid::new_v4().to_string();
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        let pressure_active: i64 = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM disk_pressure_episodes
                 WHERE service_instance = ?1
             )",
            [service_instance],
            |row| row.get(0),
        )?;
        if disk_pressure_bool(pressure_active, "active pressure episode")? {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.disk_pressure.launch.pressure",
            )
            .with_remediation(
                "refuse a worker launch while any configured filesystem pressure episode remains active",
            ));
        }
        let state = load_materialized_state(&transaction)?;
        if state.capacity_invalid || state.drain_active || state.admission_blocked {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.disk_pressure.launch.stale",
            )
            .with_remediation(
                "refuse a pressure writer lease while slot materialization or fleet admission is fenced",
            ));
        }
        let current = state.slots.iter().find(|slot| slot.slot_id == *slot_id);
        if !current.is_some_and(|slot| {
            slot.generation == generation
                && matches!(slot.phase, SlotPhase2::Ready | SlotPhase2::Assigned)
        }) {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.disk_pressure.launch.stale",
            )
            .with_remediation(
                "re-read the current ready or assigned slot generation before issuing a pressure launch lease",
            ));
        }
        let existing_launch: Option<(i64, String, i64)> = transaction
            .query_row(
                "SELECT generation, launch_nonce, active FROM disk_pressure_launches
                 WHERE service_instance = ?1 AND slot_id = ?2",
                params![service_instance, slot_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let has_provisional_row = state
            .jobs
            .iter()
            .any(|job| job.slot_id == *slot_id && job.generation == generation && job.provisional);
        if let Some((existing_generation, existing_nonce, active)) = &existing_launch
            && *existing_generation == generation_sql
            && disk_pressure_bool(*active, "launch active")?
            && has_provisional_row
        {
            // A durable provisional row pins its intent to this lease. Reuse
            // the nonce only while that row exists, so terminal response
            // recovery can prove the handoff without a second nonce copy in
            // the event schema.
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(existing_nonce.clone());
        }
        if has_provisional_row {
            return Err(disk_pressure_launch_fenced());
        }
        transaction.execute(
            "INSERT INTO disk_pressure_launches (
                 service_instance, slot_id, generation, launch_nonce, issued_unix, active
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1)
             ON CONFLICT (service_instance, slot_id) DO UPDATE SET
                 generation = excluded.generation,
                 launch_nonce = excluded.launch_nonce,
                 issued_unix = excluded.issued_unix,
                 active = 1",
            params![
                service_instance,
                slot_id.0,
                generation_sql,
                nonce,
                issued_sql
            ],
        )?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(nonce)
    }

    /// Revoke a slot writer lease at the terminal pressure deadline before
    /// signaling its worker. Every bound Journal mutation rechecks `active`
    /// under its immediate transaction, so revocation is the durable fence
    /// that orders before process termination.
    pub fn revoke_disk_pressure_launch(
        &self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
    ) -> StoreResult<bool> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "generation")?;
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        let revoked = transaction.execute(
            "UPDATE disk_pressure_launches SET active = 0
             WHERE service_instance = ?1 AND slot_id = ?2
               AND generation = ?3 AND active = 1",
            params![service_instance, slot_id.0, generation_sql],
        )?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(revoked == 1)
    }

    /// Reject a worker that does not own the current pressure-writer lease.
    /// Call at process entry before the worker can publish job or slot state.
    pub fn validate_disk_pressure_launch(
        &mut self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
        launch_nonce: &str,
    ) -> StoreResult<()> {
        self.validate_worker_context(service_instance, slot_id, generation, launch_nonce)?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "generation")?;
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_replay_integrity_before_read(&transaction)?;
        let is_current = disk_pressure_launch_is_current(
            &transaction,
            service_instance,
            slot_id,
            generation_sql,
            launch_nonce,
        )?;
        if !is_current {
            return Err(disk_pressure_launch_fenced());
        }
        transaction.commit()?;
        Ok(())
    }

    /// Read the current launch nonce for a non-fenced slot generation. A
    /// revoked or stale lease is absent to process adoption; recovery must
    /// prove any recorded process identity dead before issuing a replacement.
    pub fn disk_pressure_launch_nonce(
        &self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
    ) -> StoreResult<Option<String>> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "generation")?;
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_integrity_before_read(&transaction)?;
        let launch: Option<(i64, String, i64)> = transaction
            .query_row(
                "SELECT generation, launch_nonce, active FROM disk_pressure_launches
                 WHERE service_instance = ?1 AND slot_id = ?2",
                params![service_instance, slot_id.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let nonce = match launch {
            Some((stored_generation, nonce, active))
                if disk_pressure_bool(active, "launch active")?
                    && stored_generation == generation_sql =>
            {
                validate_disk_pressure_key(&nonce, "launch nonce")?;
                disk_pressure_launch_is_current(
                    &transaction,
                    service_instance,
                    slot_id,
                    generation_sql,
                    &nonce,
                )?
                .then_some(nonce)
            }
            Some((_, _, active)) => {
                let _ = disk_pressure_bool(active, "launch active")?;
                None
            }
            None => None,
        };
        transaction.commit()?;
        Ok(nonce)
    }

    /// Persist a launcher's first job mutation only if its nonce is still
    /// current. Validation and event reduction share the same immediate write
    /// transaction, so a replacement launch cannot race between the check and
    /// `JobStarted`.
    pub fn apply_with_disk_pressure_launch(
        &mut self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
        launch_nonce: &str,
        event: Event,
    ) -> StoreResult<ReduceOutcome> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "generation")?;
        let mut outcomes = self.apply_many_inner(
            std::iter::once(event),
            Some((service_instance, slot_id, generation_sql, launch_nonce)),
            None,
            None,
            None,
            None,
        )?;
        #[allow(clippy::expect_used, reason = "one event always yields one outcome")]
        Ok(outcomes
            .pop()
            .expect("one event must produce one reduction outcome"))
    }

    /// Durably record the exact acquisition intent only while this service's
    /// launch lease is current and no service pressure episode is active.
    /// The lease, pressure admission, and reducer write share one immediate
    /// transaction, ordering this intent against pressure observation and
    /// terminal revocation.
    #[allow(clippy::too_many_arguments)]
    pub fn intend_acquisition_with_disk_pressure_launch(
        &mut self,
        service_instance: &str,
        slot_id: SlotId,
        generation: Generation,
        launch_nonce: &str,
        provisional_job_id: JobId,
        message_id: String,
        run_service_url: String,
        intended_unix: u64,
    ) -> StoreResult<ReduceOutcome> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        validate_disk_pressure_key(launch_nonce, "launch nonce")?;
        validate_disk_pressure_key(&provisional_job_id.0, "provisional job id")?;
        validate_disk_pressure_key(&message_id, "acquisition message id")?;
        validate_disk_pressure_key(&run_service_url, "run service URL")?;
        self.validate_acquisition_handle(service_instance, &slot_id, generation, launch_nonce)?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "launch generation")?;
        let intent_fence = AcquisitionIntentFence {
            service_instance: service_instance.to_owned(),
            slot_id: slot_id.clone(),
            generation: generation_sql,
            launch_nonce: launch_nonce.to_owned(),
            provisional_job_id: provisional_job_id.clone(),
            message_id: message_id.clone(),
            run_service_url: run_service_url.clone(),
        };
        let mut outcomes = self.apply_many_inner(
            std::iter::once(Event::JobAcquisitionIntended {
                slot_id: slot_id.clone(),
                job_id: provisional_job_id,
                generation,
                message_id,
                run_service_url,
                intended_unix,
            }),
            Some((service_instance, &slot_id, generation_sql, launch_nonce)),
            Some(intent_fence),
            None,
            None,
            None,
        )?;
        #[allow(clippy::expect_used, reason = "one event always yields one outcome")]
        Ok(outcomes
            .pop()
            .expect("one event must produce one reduction outcome"))
    }

    /// Retarget the exact durable provisional row named by an acquirejob 200.
    /// This accepts either its still-current launch lease or that same nonce
    /// after a terminal pressure fence. The latter is a response handoff only:
    /// it cannot create or retarget any other acquisition.
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_acquisition_response(
        &mut self,
        service_instance: &str,
        slot_id: SlotId,
        generation: Generation,
        launch_nonce: &str,
        provisional_job_id: JobId,
        message_id: &str,
        acquired_job_id: JobId,
        run_service_url: &str,
        plan_id: &str,
    ) -> StoreResult<ReduceOutcome> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        validate_disk_pressure_key(launch_nonce, "launch nonce")?;
        validate_disk_pressure_key(&provisional_job_id.0, "provisional job id")?;
        validate_disk_pressure_key(message_id, "acquisition message id")?;
        validate_disk_pressure_key(&acquired_job_id.0, "acquired job id")?;
        validate_disk_pressure_key(run_service_url, "run service URL")?;
        validate_disk_pressure_key(plan_id, "run service plan id")?;
        self.validate_acquisition_handle(service_instance, &slot_id, generation, launch_nonce)?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "launch generation")?;
        let response_fence = AcquisitionResponseFence {
            service_instance: service_instance.to_owned(),
            slot_id,
            generation: generation_sql,
            launch_nonce: launch_nonce.to_owned(),
            provisional_job_id: provisional_job_id.clone(),
            message_id: message_id.to_owned(),
            run_service_url: run_service_url.to_owned(),
        };
        let mut outcomes = self.apply_many_inner(
            std::iter::once(Event::JobAcquisitionResolved {
                provisional_job_id,
                acquired_job_id,
                plan_id: plan_id.to_owned(),
                generation,
            }),
            None,
            None,
            Some(response_fence),
            None,
            None,
        )?;
        #[allow(clippy::expect_used, reason = "one event always yields one outcome")]
        Ok(outcomes
            .pop()
            .expect("one event must produce one reduction outcome"))
    }

    /// Abandon the exact provisional acquisition only after this worker's
    /// launch was revoked by a terminal disk-pressure fence. This narrow
    /// handoff exists for a typed run-service-gone response that arrives after
    /// the controller has fenced the slot; ordinary stale worker writes remain
    /// rejected by the launch lease.
    #[allow(clippy::too_many_arguments)]
    pub fn abandon_acquisition_after_disk_pressure_terminal(
        &mut self,
        service_instance: &str,
        slot_id: SlotId,
        generation: Generation,
        launch_nonce: &str,
        provisional_job_id: JobId,
        message_id: &str,
        run_service_url: &str,
        reason: String,
    ) -> StoreResult<ReduceOutcome> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        validate_disk_pressure_key(launch_nonce, "launch nonce")?;
        validate_disk_pressure_key(&provisional_job_id.0, "provisional job id")?;
        validate_disk_pressure_key(message_id, "acquisition message id")?;
        validate_disk_pressure_key(run_service_url, "run service URL")?;
        let Some(bound) = &self.worker_launch else {
            return Err(acquisition_abandon_fenced());
        };
        let generation_sql = disk_pressure_sql_integer(generation.0, "launch generation")?;
        if bound.service_instance != service_instance
            || bound.slot_id != slot_id
            || bound.generation != generation_sql
            || bound.launch_nonce != launch_nonce
        {
            return Err(acquisition_abandon_fenced());
        }
        let abandon_fence = AcquisitionAbandonFence {
            service_instance: service_instance.to_owned(),
            slot_id: slot_id.clone(),
            generation: generation_sql,
            launch_nonce: launch_nonce.to_owned(),
            provisional_job_id: provisional_job_id.clone(),
            message_id: message_id.to_owned(),
            run_service_url: run_service_url.to_owned(),
        };
        let mut outcomes = self.apply_many_inner(
            std::iter::once(Event::JobAcquisitionLost {
                job_id: provisional_job_id,
                generation,
                reason,
            }),
            None,
            None,
            None,
            Some(abandon_fence),
            None,
        )?;
        #[allow(clippy::expect_used, reason = "one event always yields one outcome")]
        Ok(outcomes
            .pop()
            .expect("one event must produce one reduction outcome"))
    }

    /// Promote a recovered, plan-bearing acquisition after `renewjob` proved
    /// this runner still owns it, even when terminal pressure fenced its slot.
    /// The journal derives the original intent and nonce association from the
    /// checked event chain and the non-rotating generation lease invariant.
    pub fn confirm_acquisition_after_disk_pressure_terminal(
        &mut self,
        service_instance: &str,
        slot_id: SlotId,
        generation: Generation,
        acquired_job_id: JobId,
        plan_id: &str,
        run_service_url: &str,
    ) -> StoreResult<ReduceOutcome> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        validate_disk_pressure_key(&acquired_job_id.0, "acquired job id")?;
        validate_disk_pressure_key(plan_id, "run service plan id")?;
        validate_disk_pressure_key(run_service_url, "run service URL")?;
        self.validate_pressure_service_instance(service_instance)?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "launch generation")?;
        let recovery_fence = AcquisitionRecoveryFence {
            service_instance: service_instance.to_owned(),
            slot_id: slot_id.clone(),
            generation: generation_sql,
            acquired_job_id: acquired_job_id.clone(),
            plan_id: plan_id.to_owned(),
            run_service_url: run_service_url.to_owned(),
        };
        let mut outcomes = self.apply_many_inner(
            std::iter::once(Event::JobOwned {
                job_id: acquired_job_id.clone(),
                slot_id,
                attempt: 1,
                generation,
                worker: pressure_terminal_recovery_worker(&acquired_job_id),
                accepted_unix: 0,
            }),
            None,
            None,
            None,
            None,
            Some(recovery_fence),
        )?;
        #[allow(clippy::expect_used, reason = "one event always yields one outcome")]
        Ok(outcomes
            .pop()
            .expect("one event must produce one reduction outcome"))
    }

    /// Observe all admission filesystems under one writer fence and one SQLite
    /// transaction. Reclaim claims therefore cannot be committed for only a
    /// prefix of roots when a later root has corrupt or unwritable state.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_disk_pressure_roots(
        &self,
        service_instance: &str,
        slot_id: &SlotId,
        generation: Generation,
        launch_nonce: &str,
        samples: &[DiskPressureFilesystemSample],
        degraded_seconds: u64,
        drain_seconds: u64,
        now_unix: u64,
    ) -> StoreResult<Vec<(String, DiskPressureObservation)>> {
        self.validate_worker_context(service_instance, slot_id, generation, launch_nonce)?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(&slot_id.0, "slot id")?;
        validate_disk_pressure_key(launch_nonce, "launch nonce")?;
        if samples.is_empty() {
            return Err(disk_pressure_state_invalid(
                "empty filesystem observation batch".to_owned(),
            ));
        }
        let mut filesystem_ids = Vec::<String>::new();
        for sample in samples {
            if let Some(fingerprint) = &sample.volume_fingerprint {
                validate_disk_pressure_key(fingerprint, "volume fingerprint")?;
            }
            for filesystem_id in std::iter::once(&sample.filesystem_id).chain(&sample.alias_ids) {
                validate_disk_pressure_key(filesystem_id, "filesystem identity")?;
                if filesystem_ids.contains(filesystem_id) {
                    return Err(disk_pressure_state_invalid(format!(
                        "duplicate filesystem observation {filesystem_id}"
                    )));
                }
                filesystem_ids.push(filesystem_id.clone());
            }
        }
        let _ = disk_pressure_sql_integer(now_unix, "observation time")?;
        let generation_sql = disk_pressure_sql_integer(generation.0, "generation")?;
        let degraded_sql = disk_pressure_sql_integer(degraded_seconds, "degraded deadline")?;
        let drain_sql = disk_pressure_sql_integer(drain_seconds, "drain deadline")?;
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        if !disk_pressure_launch_is_current(
            &transaction,
            service_instance,
            slot_id,
            generation_sql,
            launch_nonce,
        )? {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.disk_pressure.launch.fenced",
            )
            .with_remediation(
                "discard this worker's observation; a newer slot launch owns the pressure writer lease",
            ));
        }
        let mut observations = Vec::with_capacity(samples.len());
        for sample in samples {
            let observation = observe_disk_pressure_in_transaction(
                &transaction,
                service_instance,
                sample,
                degraded_sql,
                drain_sql,
                now_unix,
            )?;
            observations.push((sample.filesystem_id.clone(), observation));
        }
        if observations.iter().any(|(_, observation)| {
            observation
                .episode
                .as_ref()
                .is_some_and(|episode| episode.terminal)
        }) {
            // Latch, revoke leases, and fence all slots in this transaction.
            // This closes the gap where another slot could issue a nonce or
            // reserve a permit after the terminal observation commits.
            persist_pressure_terminal_slot_fences(&transaction, service_instance)?;
        }
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(observations)
    }

    /// Advance persisted low episodes through D (draining) and E (terminal)
    /// from controller observations. This does not need a live worker nonce,
    /// so silent or crashed workers cannot stretch either deadline.
    pub fn advance_disk_pressure_roots(
        &self,
        service_instance: &str,
        samples: &[DiskPressureFilesystemSample],
        degraded_seconds: u64,
        drain_seconds: u64,
        now_unix: u64,
    ) -> StoreResult<()> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        if samples.is_empty() {
            return Err(disk_pressure_state_invalid(
                "empty controller filesystem observation batch".to_owned(),
            ));
        }
        let _ = disk_pressure_sql_integer(now_unix, "controller observation time")?;
        let degraded_sql = disk_pressure_sql_integer(degraded_seconds, "degraded deadline")?;
        let drain_sql = disk_pressure_sql_integer(drain_seconds, "drain deadline")?;
        let mut filesystem_ids = Vec::<String>::new();
        for sample in samples {
            if let Some(fingerprint) = &sample.volume_fingerprint {
                validate_disk_pressure_key(fingerprint, "volume fingerprint")?;
            }
            for filesystem_id in std::iter::once(&sample.filesystem_id).chain(&sample.alias_ids) {
                validate_disk_pressure_key(filesystem_id, "filesystem identity")?;
                if filesystem_ids.contains(filesystem_id) {
                    return Err(disk_pressure_state_invalid(format!(
                        "duplicate controller filesystem observation {filesystem_id}"
                    )));
                }
                filesystem_ids.push(filesystem_id.clone());
            }
        }
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        for sample in samples {
            advance_controller_pressure_sample(
                &transaction,
                service_instance,
                sample,
                degraded_sql,
                drain_sql,
                now_unix,
            )?;
        }
        persist_pressure_terminal_slot_fences(&transaction, service_instance)?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Advance every stored episode conservatively when the controller cannot
    /// measure configured roots. Unknown capacity never clears an episode.
    pub fn advance_unmeasurable_disk_pressure(
        &self,
        service_instance: &str,
        degraded_seconds: u64,
        drain_seconds: u64,
        now_unix: u64,
    ) -> StoreResult<(bool, bool)> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        let _ = disk_pressure_sql_integer(now_unix, "controller observation time")?;
        let degraded_sql = disk_pressure_sql_integer(degraded_seconds, "degraded deadline")?;
        let drain_sql = disk_pressure_sql_integer(drain_seconds, "drain deadline")?;
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        let filesystem_ids = {
            let mut statement = transaction.prepare(
                "SELECT filesystem_id FROM disk_pressure_episodes
                 WHERE service_instance = ?1
                 ORDER BY filesystem_id",
            )?;
            let rows = statement.query_map([service_instance], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for filesystem_id in filesystem_ids {
            advance_controller_pressure_sample(
                &transaction,
                service_instance,
                &DiskPressureFilesystemSample {
                    filesystem_id,
                    alias_ids: Vec::new(),
                    available_bytes: None,
                    min_free_bytes: 0,
                    volume_fingerprint: None,
                },
                degraded_sql,
                drain_sql,
                now_unix,
            )?;
        }
        persist_pressure_terminal_slot_fences(&transaction, service_instance)?;
        let (draining, terminal) = pressure_episode_stages(&transaction, service_instance)?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok((draining, terminal))
    }

    /// Retire episodes for physical/root identities absent from a complete,
    /// measurable current-root batch. Absence is not evidence while any root
    /// is unknown. Retiring an old identity first fences every slot and waits
    /// for all occupied jobs to recover.
    pub fn retire_unobserved_disk_pressure_episodes(
        &self,
        service_instance: &str,
        observed_filesystem_ids: &[String],
        complete_identity_batch: bool,
    ) -> StoreResult<()> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        if !complete_identity_batch {
            return Ok(());
        }
        if observed_filesystem_ids.is_empty() {
            return Err(disk_pressure_state_invalid(
                "complete filesystem observation batch is empty".to_owned(),
            ));
        }
        for id in observed_filesystem_ids {
            validate_disk_pressure_key(id, "observed filesystem identity")?;
        }
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        let mut statement = transaction.prepare(
            "SELECT filesystem_id, revision FROM disk_pressure_episodes
             WHERE service_instance = ?1 ORDER BY filesystem_id",
        )?;
        let rows = statement.query_map([service_instance], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut unobserved = Vec::new();
        for row in rows {
            let (filesystem_id, revision) = row?;
            if !observed_filesystem_ids.contains(&filesystem_id) {
                unobserved.push((filesystem_id, revision));
            }
        }
        drop(statement);
        if unobserved.is_empty() {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(());
        }
        for (filesystem_id, revision) in &unobserved {
            let revision = disk_pressure_u64(*revision, "episode revision")?;
            let next_revision = revision.checked_add(1).ok_or_else(|| {
                disk_pressure_state_invalid("episode revision overflow".to_owned())
            })?;
            transaction.execute(
                "UPDATE disk_pressure_episodes
                 SET draining = 1, terminal = 1, revision = ?1
                 WHERE service_instance = ?2 AND filesystem_id = ?3 AND revision = ?4",
                params![
                    disk_pressure_sql_integer(next_revision, "episode revision")?,
                    service_instance,
                    filesystem_id,
                    disk_pressure_sql_integer(revision, "episode revision")?,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE disk_pressure_launches SET active = 0 WHERE service_instance = ?1",
            [service_instance],
        )?;
        persist_pressure_terminal_slot_fences(&transaction, service_instance)?;
        let state = load_materialized_state(&transaction)?;
        if pressure_state_slots_are_safe(&state) {
            for (filesystem_id, _) in &unobserved {
                transaction.execute(
                    "DELETE FROM disk_pressure_episodes
                     WHERE service_instance = ?1 AND filesystem_id = ?2 AND terminal = 1",
                    params![service_instance, filesystem_id],
                )?;
            }
        }
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(())
    }

    /// Aggregate active episode stages, including identities no longer
    /// present in the controller's current filesystem batch.
    pub fn disk_pressure_state(&self, service_instance: &str) -> StoreResult<(bool, bool, bool)> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_integrity_before_read(&transaction)?;
        let mut statement = transaction.prepare(
            "SELECT draining, terminal FROM disk_pressure_episodes
             WHERE service_instance = ?1",
        )?;
        let rows = statement.query_map([service_instance], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut any = false;
        let mut draining = false;
        let mut terminal = false;
        for row in rows {
            let (row_draining, row_terminal) = row?;
            any = true;
            draining |= disk_pressure_bool(row_draining, "draining")?;
            terminal |= disk_pressure_bool(row_terminal, "terminal")?;
        }
        drop(statement);
        transaction.commit()?;
        Ok((any, draining, terminal))
    }

    /// Read the current host episode through the checked v11 journal boundary.
    pub fn disk_pressure_episode(
        &self,
        service_instance: &str,
        filesystem_id: &str,
    ) -> StoreResult<Option<DiskPressureEpisode>> {
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(filesystem_id, "filesystem identity")?;
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_integrity_before_read(&transaction)?;
        let episode = load_disk_pressure_episode(&transaction, service_instance, filesystem_id)?;
        transaction.commit()?;
        Ok(episode)
    }

    /// Claim a controller-side maintenance reclaim for a measured-low root.
    /// The durable CAS lets the controller recover cleanup even when every
    /// worker is correctly blocked from admission by this episode.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_disk_pressure_reclaim(
        &self,
        service_instance: &str,
        filesystem_id: &str,
        alias_ids: &[String],
        expected: &DiskPressureEpisode,
        available_bytes: u64,
        min_free_bytes: u64,
        volume_fingerprint: &str,
        now_unix: u64,
    ) -> StoreResult<bool> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(filesystem_id, "filesystem identity")?;
        for alias in alias_ids {
            validate_disk_pressure_key(alias, "filesystem alias")?;
        }
        validate_disk_pressure_key(volume_fingerprint, "volume fingerprint")?;
        let _ = disk_pressure_sql_integer(now_unix, "reclaim claim time")?;
        if available_bytes >= min_free_bytes {
            return Ok(false);
        }
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        let Some(clock_high_water) = confirmed_pressure_observation_group_high_water(
            &transaction,
            service_instance,
            filesystem_id,
            alias_ids,
            volume_fingerprint,
        )?
        else {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        };
        if now_unix < clock_high_water {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        let Some(mut current) =
            load_disk_pressure_episode(&transaction, service_instance, filesystem_id)?
        else {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        };
        if current.episode_id != expected.episode_id
            || current.revision != expected.revision
            || current.volume_fingerprint.as_deref() != Some(volume_fingerprint)
            || current.reclaim_attempted
            || current.draining
            || current.terminal
            || now_unix < current.started_unix
            || now_unix >= current.deadline_unix
        {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        let old_revision = current.revision;
        current.reclaim_attempted = true;
        current.last_observed_unix = current.last_observed_unix.max(now_unix);
        current.revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| disk_pressure_state_invalid("episode revision overflow".to_owned()))?;
        let updated = transaction.execute(
            "UPDATE disk_pressure_episodes
             SET reclaim_attempted = 1, revision = ?1, last_observed_unix = ?2
             WHERE service_instance = ?3 AND filesystem_id = ?4
               AND episode_id = ?5 AND revision = ?6
               AND reclaim_attempted = 0 AND draining = 0 AND terminal = 0",
            params![
                disk_pressure_sql_integer(current.revision, "episode revision")?,
                disk_pressure_sql_integer(current.last_observed_unix, "last observation")?,
                service_instance,
                filesystem_id,
                current.episode_id,
                disk_pressure_sql_integer(old_revision, "episode revision")?,
            ],
        )?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(updated == 1)
    }

    /// Clear only the exact durable episode after this caller measured healthy
    /// free space. A rollback observation instead makes the episode terminal.
    #[allow(clippy::too_many_arguments)]
    pub fn clear_disk_pressure_episode_if_healthy(
        &self,
        service_instance: &str,
        filesystem_id: &str,
        alias_ids: &[String],
        expected: &DiskPressureEpisode,
        available_bytes: u64,
        min_free_bytes: u64,
        volume_fingerprint: &str,
        now_unix: u64,
    ) -> StoreResult<bool> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(filesystem_id, "filesystem identity")?;
        for alias in alias_ids {
            validate_disk_pressure_key(alias, "filesystem alias")?;
        }
        if available_bytes < min_free_bytes {
            return Ok(false);
        }
        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        let Some(clock_high_water) = confirmed_pressure_observation_group_high_water(
            &transaction,
            service_instance,
            filesystem_id,
            alias_ids,
            volume_fingerprint,
        )?
        else {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        };
        let current = load_disk_pressure_episode(&transaction, service_instance, filesystem_id)?;
        let Some(mut current) = current else {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        };
        if current.episode_id != expected.episode_id || current.revision != expected.revision {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        current.last_observed_unix = current.last_observed_unix.max(clock_high_water);
        if current.volume_fingerprint.as_deref() != Some(volume_fingerprint)
            || expected.volume_fingerprint.as_deref() != Some(volume_fingerprint)
        {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        if current.terminal {
            let state = load_materialized_state(&transaction)?;
            if !pressure_state_slots_are_safe(&state) {
                end_journal_write_gate(&transaction)?;
                transaction.commit()?;
                return Ok(false);
            }
        }
        if now_unix < current.last_observed_unix || now_unix < current.started_unix {
            let old_revision = current.revision;
            current.revision = current.revision.checked_add(1).ok_or_else(|| {
                disk_pressure_state_invalid("episode revision overflow".to_owned())
            })?;
            transaction.execute(
                "UPDATE disk_pressure_episodes
                 SET revision = ?1, draining = 1, terminal = 1,
                     last_observed_unix = ?2
                 WHERE service_instance = ?3 AND filesystem_id = ?4
                   AND episode_id = ?5 AND revision = ?6",
                params![
                    disk_pressure_sql_integer(current.revision, "episode revision")?,
                    disk_pressure_sql_integer(current.last_observed_unix, "last observation")?,
                    service_instance,
                    filesystem_id,
                    current.episode_id,
                    disk_pressure_sql_integer(old_revision, "episode revision")?,
                ],
            )?;
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        let revision_sql = disk_pressure_sql_integer(current.revision, "episode revision")?;
        let mut deleted_primary = 0;
        for id in std::iter::once(filesystem_id).chain(alias_ids.iter().map(String::as_str)) {
            let deleted = transaction.execute(
                "DELETE FROM disk_pressure_episodes
                 WHERE service_instance = ?1 AND filesystem_id = ?2
                   AND episode_id = ?3 AND revision = ?4",
                params![service_instance, id, current.episode_id, revision_sql],
            )?;
            if id == filesystem_id {
                deleted_primary = deleted;
            }
        }
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(deleted_primary == 1)
    }

    /// Retire an episode whose configured path now resolves to another volume
    /// incarnation. The old generation must already be fenced and drained;
    /// a low new volume starts a fresh immutable D/E timeline at `now_unix`.
    #[allow(clippy::too_many_arguments)]
    pub fn rebind_disk_pressure_episode_if_safe(
        &self,
        service_instance: &str,
        filesystem_id: &str,
        alias_ids: &[String],
        expected: &DiskPressureEpisode,
        available_bytes: Option<u64>,
        min_free_bytes: u64,
        volume_fingerprint: &str,
        degraded_seconds: u64,
        drain_seconds: u64,
        now_unix: u64,
    ) -> StoreResult<bool> {
        self.require_controller_authority()?;
        validate_disk_pressure_key(service_instance, "service instance")?;
        self.validate_pressure_service_instance(service_instance)?;
        validate_disk_pressure_key(filesystem_id, "filesystem identity")?;
        validate_disk_pressure_key(volume_fingerprint, "volume fingerprint")?;
        let now_sql = disk_pressure_sql_integer(now_unix, "volume rebind time")?;
        if expected.volume_fingerprint.as_deref() == Some(volume_fingerprint) {
            return Ok(false);
        }
        for alias in alias_ids {
            validate_disk_pressure_key(alias, "filesystem alias")?;
        }

        let writer_context = self.writer_context();
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        bind_journal_service_instance(&transaction, service_instance)?;
        let current = load_disk_pressure_episode(&transaction, service_instance, filesystem_id)?;
        let Some(current) = current else {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        };
        if current.episode_id != expected.episode_id
            || current.revision != expected.revision
            || !current.terminal
            || current.volume_fingerprint != expected.volume_fingerprint
        {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        let root_identity: Option<(Option<String>, i64, i64)> = transaction
            .query_row(
                "SELECT volume_fingerprint, identity_confirmed, last_observed_unix
                 FROM disk_pressure_observations
                 WHERE service_instance = ?1 AND filesystem_id = ?2",
                params![service_instance, filesystem_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((persisted_fingerprint, identity_confirmed, last_observed)) = root_identity else {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        };
        if persisted_fingerprint != expected.volume_fingerprint
            || disk_pressure_bool(identity_confirmed, "pressure identity confirmed")?
        {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        let state = load_materialized_state(&transaction)?;
        if !pressure_state_slots_are_safe(&state) {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        let high_water = disk_pressure_u64(last_observed, "pressure clock high-water")?;
        if now_unix < high_water || now_unix < current.started_unix {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }

        let revision_sql = disk_pressure_sql_integer(current.revision, "episode revision")?;
        let deleted = transaction.execute(
            "DELETE FROM disk_pressure_episodes
             WHERE service_instance = ?1 AND filesystem_id = ?2
               AND episode_id = ?3 AND revision = ?4",
            params![
                service_instance,
                filesystem_id,
                current.episode_id,
                revision_sql
            ],
        )?;
        if deleted != 1 {
            return Err(disk_pressure_state_invalid(
                "volume rebind episode compare-and-swap missed".to_owned(),
            ));
        }
        let observed_roots =
            std::iter::once(filesystem_id).chain(alias_ids.iter().map(String::as_str));
        for root_id in observed_roots {
            let updated = transaction.execute(
                "UPDATE disk_pressure_observations
                 SET volume_fingerprint = ?1, identity_confirmed = 1,
                     last_observed_unix = MAX(last_observed_unix, ?2)
                 WHERE service_instance = ?3 AND filesystem_id = ?4",
                params![volume_fingerprint, now_sql, service_instance, root_id],
            )?;
            if root_id == filesystem_id && updated != 1 {
                return Err(disk_pressure_state_invalid(
                    "volume rebind observation compare-and-swap missed".to_owned(),
                ));
            }
        }
        transaction.execute(
            "UPDATE disk_pressure_launches SET active = 0 WHERE service_instance = ?1",
            [service_instance],
        )?;

        if available_bytes.is_none_or(|available| available < min_free_bytes) {
            let deadline = now_unix.checked_add(degraded_seconds).ok_or_else(|| {
                disk_pressure_state_invalid("cleanup deadline overflow".to_owned())
            })?;
            let drain_deadline = deadline
                .checked_add(drain_seconds)
                .ok_or_else(|| disk_pressure_state_invalid("drain deadline overflow".to_owned()))?;
            transaction.execute(
                "INSERT INTO disk_pressure_episodes (
                     service_instance, filesystem_id, volume_fingerprint, episode_id,
                     started_unix, deadline_unix, drain_deadline_unix, last_observed_unix,
                     reclaim_attempted, revision, draining, terminal
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 1, 0, 0)",
                params![
                    service_instance,
                    filesystem_id,
                    volume_fingerprint,
                    uuid::Uuid::new_v4().to_string(),
                    disk_pressure_sql_integer(now_unix, "episode start")?,
                    disk_pressure_sql_integer(deadline, "cleanup deadline")?,
                    disk_pressure_sql_integer(drain_deadline, "drain deadline")?,
                    disk_pressure_sql_integer(now_unix, "last observation")?,
                ],
            )?;
        }
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Read the current materialized state without replaying the event log.
    ///
    /// The materialized tables are committed in the same SQLite transaction
    /// as their corresponding events. This is the bounded hot path used by
    /// controllers and other processes that need fresh cross-process state.
    ///
    /// # Errors
    /// SQLite reads or invalid materialized values.
    pub fn materialized_state(&self) -> StoreResult<FleetState> {
        let transaction = self.conn.unchecked_transaction()?;
        validate_replay_integrity_before_read(&transaction)?;
        let state = load_materialized_state(&transaction)?;
        transaction.commit()?;
        Ok(state)
    }

    /// Record terminal local loss of a completion payload.
    ///
    /// The reducer validates the exact job owner, generation, and checksum
    /// before releasing the slot. It never records remote acknowledgement.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn record_completion_payload_loss(
        &mut self,
        job_id: &JobId,
        generation: Generation,
        payload_sha256: &str,
        reason: &str,
    ) -> StoreResult<ReduceOutcome> {
        self.apply(Event::CompletionPayloadLost {
            job_id: job_id.clone(),
            generation,
            payload_sha256: payload_sha256.to_owned(),
            reason: reason.to_owned(),
        })
    }

    /// Persist `event` then return the commands. Crash after this returns
    /// still has the intent; crash before it has neither intent nor command.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn apply(&mut self, event: Event) -> StoreResult<ReduceOutcome> {
        let mut outcomes = self.apply_many(std::iter::once(event))?;
        // Proof: `apply_many` pushes exactly one outcome per input event and
        // `apply` passes exactly one, so `pop` is always `Some`.
        #[allow(clippy::expect_used, reason = "one event always yields one outcome")]
        Ok(outcomes
            .pop()
            .expect("one event must produce one reduction outcome"))
    }

    /// Persist several events atomically after reducing them in order. This
    /// keeps controller-owned heartbeat ingestion to one replay and one
    /// materialization transaction per reconciliation cycle.
    ///
    /// # Errors
    /// SQLite or payload encode failures.
    pub fn apply_many<I>(&mut self, events: I) -> StoreResult<Vec<ReduceOutcome>>
    where
        I: IntoIterator<Item = Event>,
    {
        self.apply_many_inner(events, None, None, None, None, None)
    }

    fn apply_many_inner<'a, I>(
        &mut self,
        events: I,
        launch_fence: Option<(&'a str, &'a SlotId, i64, &'a str)>,
        acquisition_intent_fence: Option<AcquisitionIntentFence>,
        acquisition_response_fence: Option<AcquisitionResponseFence>,
        acquisition_abandon_fence: Option<AcquisitionAbandonFence>,
        acquisition_recovery_fence: Option<AcquisitionRecoveryFence>,
    ) -> StoreResult<Vec<ReduceOutcome>>
    where
        I: IntoIterator<Item = Event>,
    {
        if (acquisition_response_fence.is_some() && acquisition_abandon_fence.is_some())
            || (acquisition_recovery_fence.is_some()
                && (acquisition_intent_fence.is_some()
                    || acquisition_response_fence.is_some()
                    || acquisition_abandon_fence.is_some()
                    || self.worker_launch.is_some()))
        {
            return Err(acquisition_response_fenced());
        }
        let launch_fence = if let Some(bound) = &self.worker_launch {
            if let Some((service_instance, slot_id, generation, launch_nonce)) = launch_fence
                && (bound.service_instance != service_instance
                    || bound.slot_id != *slot_id
                    || bound.generation != generation
                    || bound.launch_nonce != launch_nonce)
            {
                return Err(disk_pressure_launch_fenced());
            }
            if let Some(response) = &acquisition_response_fence {
                if bound.service_instance != response.service_instance
                    || bound.slot_id != response.slot_id
                    || bound.generation != response.generation
                    || bound.launch_nonce != response.launch_nonce
                {
                    return Err(disk_pressure_launch_fenced());
                }
                // The exact-response validator below decides whether this
                // existing intent may be handed off after terminal revocation.
                None
            } else if let Some(abandon) = &acquisition_abandon_fence {
                if bound.service_instance != abandon.service_instance
                    || bound.slot_id != abandon.slot_id
                    || bound.generation != abandon.generation
                    || bound.launch_nonce != abandon.launch_nonce
                {
                    return Err(acquisition_abandon_fenced());
                }
                // Only the exact terminal-fenced provisional row validator
                // below may authorize this stale-launch cleanup.
                None
            } else {
                Some((
                    bound.service_instance.as_str(),
                    &bound.slot_id,
                    bound.generation,
                    bound.launch_nonce.as_str(),
                ))
            }
        } else {
            if acquisition_abandon_fence.is_some() {
                return Err(acquisition_abandon_fenced());
            }
            launch_fence
        };
        if let Some(fence) = &acquisition_intent_fence
            && !launch_fence.is_some_and(|(service_instance, slot_id, generation, launch_nonce)| {
                service_instance == fence.service_instance
                    && slot_id == &fence.slot_id
                    && generation == fence.generation
                    && launch_nonce == fence.launch_nonce
            })
        {
            return Err(disk_pressure_launch_fenced());
        }
        let mut events = events.into_iter();
        let Some(first_event) = events.next() else {
            return Ok(Vec::new());
        };
        if (acquisition_intent_fence.is_some()
            || acquisition_response_fence.is_some()
            || acquisition_abandon_fence.is_some()
            || acquisition_recovery_fence.is_some())
            && events.next().is_some()
        {
            return Err(acquisition_response_fenced());
        }
        if let Some(fence) = &acquisition_intent_fence
            && !matches!(
                &first_event,
                Event::JobAcquisitionIntended {
                    slot_id,
                    job_id,
                    generation,
                    message_id,
                    run_service_url,
                    ..
                } if slot_id == &fence.slot_id
                    && job_id == &fence.provisional_job_id
                    && generation.0 as i64 == fence.generation
                    && message_id == &fence.message_id
                    && run_service_url == &fence.run_service_url
            )
        {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.acquisition.intent.fenced",
            ));
        }
        if let Some(fence) = &acquisition_abandon_fence
            && !matches!(
                &first_event,
                Event::JobAcquisitionLost {
                    job_id,
                    generation,
                    ..
                } if job_id == &fence.provisional_job_id
                    && generation.0 as i64 == fence.generation
            )
        {
            return Err(acquisition_abandon_fenced());
        }
        if let Some(fence) = &acquisition_recovery_fence
            && !matches!(
                &first_event,
                Event::JobOwned {
                    job_id,
                    slot_id,
                    generation,
                    worker,
                    ..
                } if job_id == &fence.acquired_job_id
                    && slot_id == &fence.slot_id
                    && generation.0 as i64 == fence.generation
                    && worker == &pressure_terminal_recovery_worker(&fence.acquired_job_id)
            )
        {
            return Err(acquisition_recovery_fenced());
        }

        // Lock before reading materialized state. Controller, job, guardian,
        // and completion processes can overlap; a snapshot taken before the
        // write lock could otherwise clobber a concurrent committed event.
        let writer_context = self.writer_context();
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        if let Some(fence) = &acquisition_intent_fence {
            validate_journal_service_instance(&transaction, &fence.service_instance)?;
        }
        if let Some(fence) = &acquisition_response_fence {
            validate_journal_service_instance(&transaction, &fence.service_instance)?;
        }
        if let Some(fence) = &acquisition_abandon_fence {
            validate_journal_service_instance(&transaction, &fence.service_instance)?;
        }
        if let Some(fence) = &acquisition_recovery_fence {
            validate_journal_service_instance(&transaction, &fence.service_instance)?;
        }
        if let Some((service_instance, slot_id, generation_sql, launch_nonce)) = launch_fence
            && !disk_pressure_launch_is_current(
                &transaction,
                service_instance,
                slot_id,
                generation_sql,
                launch_nonce,
            )?
        {
            return Err(disk_pressure_launch_fenced());
        }
        let mut state = load_materialized_state(&transaction)?;
        if state.capacity_invalid {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.capacity.invalid",
            )
            .with_remediation(
                "preserve the legacy journal for forensics and perform an explicit verified migration before writing capacity state",
            ));
        }
        let mut outcomes = Vec::new();
        let mut pending = Vec::new();
        let mut launch_deactivations = Vec::new();
        let pressure_service_instance = launch_fence
            .map(|(service_instance, _, _, _)| service_instance)
            .or_else(|| {
                self.worker_launch
                    .as_ref()
                    .map(|fence| fence.service_instance.as_str())
            })
            .or(self.service_instance.as_deref());
        let active_pressure_episode: i64 = if let Some(service_instance) = pressure_service_instance
        {
            transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM disk_pressure_episodes
                     WHERE service_instance = ?1
                 )",
                [service_instance],
                |row| row.get(0),
            )?
        } else {
            // An unbound administrative handle has no safe service identity.
            // Preserve its conservative global admission fence.
            transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM disk_pressure_episodes)",
                [],
                |row| row.get(0),
            )?
        };
        let active_pressure_episode =
            disk_pressure_bool(active_pressure_episode, "active pressure episode")?;
        if let Some(fence) = &acquisition_response_fence {
            validate_acquisition_response_fence(&transaction, &state, fence)?;
        }
        if let Some(fence) = &acquisition_abandon_fence {
            validate_acquisition_abandon_fence(&transaction, &state, fence)?;
        }
        if let Some(fence) = &acquisition_recovery_fence {
            validate_acquisition_recovery_fence(&transaction, &state, fence)?;
        }
        if let Some(fence) = &acquisition_intent_fence
            && exact_provisional_acquisition_row(&state, fence)
            && acquisition_intent_event_is_exact(&transaction, fence)?
        {
            // The active nonce check above proves this durable row belongs to
            // the current slot launch. Exact retries are a no-op; they do not
            // create another event or bypass a changed message/URL/row.
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(vec![ReduceOutcome {
                state,
                commands: Vec::new(),
                rejected: false,
            }]);
        }
        for mut event in std::iter::once(first_event).chain(events) {
            if let Some(fence) = &acquisition_intent_fence
                && !matches!(
                    &event,
                    Event::JobAcquisitionIntended {
                        slot_id,
                        generation,
                        ..
                    } if slot_id == &fence.slot_id
                        && generation.0 as i64 == fence.generation
                )
            {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.acquisition.intent.fenced",
                ));
            }
            if self.worker_launch.is_some()
                && matches!(&event, Event::DiskPressureTerminalFence { .. })
            {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.disk_pressure.worker_authority",
                )
                .with_remediation(
                    "only the controller may fence a slot at the terminal pressure deadline",
                ));
            }
            if matches!(
                &event,
                Event::JobOwned { worker, .. }
                    if worker.starts_with(PRESSURE_TERMINAL_RECOVERY_WORKER_PREFIX)
            ) && acquisition_recovery_fence.is_none()
            {
                return Err(acquisition_recovery_fenced());
            }
            stamp_event(&mut event);
            let pressure_blocks_admission = active_pressure_episode
                && (matches!(&event, Event::PermitReserved { .. })
                    || matches!(&event, Event::JobAcquisitionIntended { .. }));
            let outcome = if pressure_blocks_admission {
                ReduceOutcome {
                    state: state.clone(),
                    commands: Vec::new(),
                    rejected: true,
                }
            } else {
                reduce(state.clone(), event.clone())
            };
            if !outcome.rejected {
                if let Event::SlotStale {
                    slot_id,
                    generation,
                }
                | Event::DiskPressureTerminalFence {
                    slot_id,
                    generation,
                } = &event
                {
                    launch_deactivations.push((
                        pressure_service_instance.map(str::to_owned),
                        slot_id.clone(),
                        disk_pressure_sql_integer(generation.0, "stale launch generation")?,
                    ));
                }
                let unchanged_without_commands =
                    outcome.commands.is_empty() && outcome.state == state;
                state = outcome.state.clone();
                if !unchanged_without_commands {
                    let payload = serde_json::to_string(&event).map_err(|error| {
                        StoreError::new(velnor_model::ExitClass::Operation, "journal.encode.failed")
                            .with_remediation(error.to_string())
                    })?;
                    pending.push((event_generation(&event), event_kind(&event), payload));
                }
            }
            outcomes.push(outcome);
        }
        for (service_instance, slot_id, generation) in launch_deactivations {
            if let Some(service_instance) = service_instance {
                transaction.execute(
                    "UPDATE disk_pressure_launches SET active = 0
                     WHERE service_instance = ?1 AND slot_id = ?2 AND generation = ?3",
                    params![service_instance, slot_id.0, generation],
                )?;
            } else {
                let matching_launch: bool = transaction.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM disk_pressure_launches
                         WHERE slot_id = ?1 AND generation = ?2 AND active = 1
                     )",
                    params![slot_id.0, generation],
                    |row| row.get(0),
                )?;
                if matching_launch {
                    return Err(StoreError::new(
                        velnor_model::ExitClass::Conflict,
                        "journal.disk_pressure.service_instance.required",
                    )
                    .with_remediation(
                        "bind this controller journal to a service instance before revoking pressure launch leases",
                    ));
                }
            }
        }
        if pending.is_empty() {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(outcomes);
        }

        let tx = transaction;
        for (generation, kind, payload) in pending {
            let checksum = sha256_hex(payload.as_bytes());
            tx.execute(
                "INSERT INTO events (generation, kind, payload, checksum) VALUES (?1, ?2, ?3, ?4)",
                params![generation.0 as i64, kind, payload, checksum],
            )?;
        }
        persist_state(&tx, &state)?;
        end_journal_write_gate(&tx)?;
        tx.commit()?;
        Ok(outcomes)
    }

    /// Rebuild materialization from the event log (crash recovery).
    ///
    /// # Errors
    /// SQLite or payload decode failures.
    pub fn load_state(&self) -> StoreResult<FleetState> {
        load_current_state_checked(&self.conn)
    }

    /// Check durable terminal acknowledgement evidence without replaying the
    /// full event log. The controller uses this bounded, indexed query during
    /// every reconciliation cycle after local cleanup may have failed.
    pub fn has_remote_terminal_ack(
        &self,
        job_id: &JobId,
        generation: Generation,
    ) -> StoreResult<bool> {
        let mut statement = self.conn.prepare(
            "SELECT generation, kind, payload, checksum
             FROM events
             WHERE generation = ?1
               AND kind IN ('remote_acked', 'remote_observed_terminal')
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![generation.0 as i64, MAX_TERMINAL_ACK_SCAN_ROWS + 1],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        let mut scanned = 0;
        for row in rows {
            scanned += 1;
            if scanned > MAX_TERMINAL_ACK_SCAN_ROWS {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.terminal_ack.scan.bound",
                )
                .with_remediation(
                    "the terminal acknowledgement history exceeded the bounded recovery scan; preserve the journal and compact it through the retention path",
                ));
            }
            let (generation_sql, kind, payload, checksum) = row?;
            let event = decode_checked_event(generation_sql, &kind, &payload, &checksum)?;
            if matches!(
                event,
                Event::RemoteAcked {
                    job_id: ref event_job_id,
                    generation: event_generation,
                }
                | Event::RemoteObservedTerminal {
                    job_id: ref event_job_id,
                    generation: event_generation,
                } if event_job_id == job_id && event_generation == generation
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Terminal conclusion durably recorded before the completion payload was
    /// serialised, if any. Recovery uses it instead of inventing a failure for
    /// a job whose real result is already known.
    ///
    /// # Errors
    /// SQLite read failures.
    pub fn recorded_terminal_conclusion(
        &self,
        job_id: &JobId,
        generation: Generation,
    ) -> StoreResult<Option<String>> {
        Ok(self
            .materialized_state()?
            .jobs
            .into_iter()
            .find(|job| job.job_id == *job_id && job.generation == generation)
            .and_then(|job| job.terminal_conclusion))
    }

    /// Completions abandoned in a bounded terminal state. This is the operator
    /// surface: the materialized outbox drops an abandoned row so a later job
    /// attempt is not blocked, and the immutable log keeps the evidence.
    ///
    /// # Errors
    /// SQLite reads, checksum mismatch, or an undecodable event.
    pub fn unresolvable_completions(&self) -> StoreResult<Vec<UnresolvableCompletion>> {
        let mut statement = self.conn.prepare(
            "SELECT generation, kind, payload, checksum
             FROM events
             WHERE kind IN ('completion_unresolvable', 'completion_payload_lost')
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![MAX_TERMINAL_ACK_SCAN_ROWS], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut found = Vec::new();
        for row in rows {
            let (generation_sql, kind, payload, checksum) = row?;
            let event = decode_checked_event(generation_sql, &kind, &payload, &checksum)?;
            match event {
                Event::CompletionUnresolvable {
                    job_id,
                    generation,
                    reason,
                }
                | Event::CompletionPayloadLost {
                    job_id,
                    generation,
                    reason,
                    ..
                } => found.push(UnresolvableCompletion {
                    job_id,
                    generation,
                    reason,
                }),
                _ => {}
            }
        }
        Ok(found)
    }

    /// Pending completion outbox rows that still need remote reconciliation.
    ///
    /// # Errors
    /// SQLite read failures.
    pub fn pending_outbox(&self) -> StoreResult<Vec<OutboxRecord>> {
        Ok(self
            .materialized_state()?
            .outbox
            .into_iter()
            .filter(OutboxRecord::is_pending)
            .collect())
    }

    /// Latch the fleet drain request at `version`. Idempotent: re-setting the
    /// effective value is a no-op, and the recorded version never regresses.
    ///
    /// This is a direct `meta` write in its own immediate transaction, never a
    /// new `Event` variant: v9-capable binaries that do not use this marker
    /// may ignore it, while pre-v9 writers are fenced from state rewrites.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn set_drain(&mut self, version: u64) -> StoreResult<bool> {
        self.require_controller_authority()?;
        let writer_context = self.writer_context();
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        let existing: Option<String> = transaction
            .query_row("SELECT value FROM meta WHERE key = 'drain'", [], |row| {
                row.get(0)
            })
            .optional()?;
        let effective = existing
            .as_deref()
            .and_then(parse_drain_value)
            .map(|state| state.version.max(version))
            .unwrap_or(version);
        let value = format!("requested:{effective}");
        if existing.as_deref() == Some(value.as_str()) {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('drain', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![value],
        )?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Remove a durable drain request when an explicit restart/resume has
    /// taken ownership of this journal. Idempotent: a missing marker is
    /// already clear. The caller must establish its process ownership before
    /// invoking this method; this API never guesses which daemon to resume.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn clear_drain(&mut self) -> StoreResult<bool> {
        self.require_controller_authority()?;
        let writer_context = self.writer_context();
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        let removed = transaction.execute("DELETE FROM meta WHERE key = 'drain'", [])? > 0;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(removed)
    }

    /// Latch a soft admission fence at `version`. Unlike drain, this keeps
    /// the daemon alive and lets in-flight jobs finish; all new registration,
    /// permits, and acquisitions are rejected until
    /// `clear_admission_blocked_if` observes and clears this version.
    /// The version never regresses across retries.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn set_admission_blocked(&mut self, version: u64) -> StoreResult<bool> {
        self.require_controller_authority()?;
        let writer_context = self.writer_context();
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT value FROM meta WHERE key = 'admission'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let effective = match existing.as_deref() {
            None => version,
            Some(value) => parse_admission_value(value)
                .ok_or_else(|| invalid_materialized("admission", value))?
                .version
                .max(version),
        };
        let value = format!("blocked:{effective}");
        if existing.as_deref() == Some(value.as_str()) {
            end_journal_write_gate(&transaction)?;
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('admission', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![value],
        )?;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(true)
    }

    /// Clear a soft admission fence only if the marker still has the version
    /// observed by the caller. The compare-and-delete closes the race where
    /// a concurrent cordon could otherwise be erased after the caller's
    /// read and before an unconditional delete.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn clear_admission_blocked_if(
        &mut self,
        expected_version: Option<u64>,
    ) -> StoreResult<bool> {
        self.require_controller_authority()?;
        let Some(expected_version) = expected_version else {
            return Ok(false);
        };
        let writer_context = self.writer_context();
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_replay_baseline_before_write(&transaction)?;
        writer_context.begin_write(&transaction)?;
        let expected = format!("blocked:{expected_version}");
        let removed = transaction.execute(
            "DELETE FROM meta WHERE key = 'admission' AND value = ?1",
            params![expected],
        )? > 0;
        end_journal_write_gate(&transaction)?;
        transaction.commit()?;
        Ok(removed)
    }
}

/// Durable fleet drain request read without a journal handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainState {
    /// Always true: absence of the marker is `None`, not inactive.
    pub active: bool,
    /// Lifecycle resource version that requested the drain.
    pub version: u64,
}

/// Why a durable drain marker could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainStateReadError {
    /// The journal could not be opened or queried.
    Unavailable,
    /// The marker exists but is not a valid `requested:{version}` value.
    Malformed,
}

/// Read the latched drain marker with a throwaway read-only connection.
///
/// One `SELECT` on `meta` with a zero busy timeout: slot and daemon poll
/// boundaries call this and must never block on a writer's lock. An unreadable
/// existing journal is an error, not an absent marker: admission callers must
/// fail closed instead of treating corruption or lock contention as permission
/// to run.
pub fn read_drain_state(path: &Path) -> Result<Option<DrainState>, DrainStateReadError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| DrainStateReadError::Unavailable)?;
    conn.busy_timeout(Duration::ZERO)
        .map_err(|_| DrainStateReadError::Unavailable)?;
    let value: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'drain'", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|_| DrainStateReadError::Unavailable)?;
    value
        .map(|value| {
            parse_drain_value(&value)
                .ok_or(DrainStateReadError::Malformed)
                .map(Some)
        })
        .unwrap_or(Ok(None))
}

/// Parse one `meta[drain]` value (`requested:{version}`).
fn parse_drain_value(value: &str) -> Option<DrainState> {
    let version = value.strip_prefix("requested:")?.parse::<u64>().ok()?;
    Some(DrainState {
        active: true,
        version,
    })
}

/// Durable soft admission fence read without a journal handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionState {
    /// Lifecycle resource version that requested the fence.
    pub version: u64,
}

/// Why a soft admission marker could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionStateReadError {
    /// The journal could not be opened or queried.
    Unavailable,
    /// The marker exists but is not a valid `blocked:<version>` value.
    Malformed,
}

/// Read the soft admission marker with a zero-timeout read-only connection.
/// An error is distinct from an absent marker so runner admission can fail
/// closed on a malformed or inaccessible journal instead of accepting work
/// without a durable cordon decision.
pub fn read_admission_state(
    path: &Path,
) -> Result<Option<AdmissionState>, AdmissionStateReadError> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| AdmissionStateReadError::Unavailable)?;
    conn.busy_timeout(Duration::ZERO)
        .map_err(|_| AdmissionStateReadError::Unavailable)?;
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'admission'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| AdmissionStateReadError::Unavailable)?;
    match value {
        None => Ok(None),
        Some(value) => parse_admission_value(&value)
            .map(Some)
            .ok_or(AdmissionStateReadError::Malformed),
    }
}

fn parse_admission_value(value: &str) -> Option<AdmissionState> {
    let version = value.strip_prefix("blocked:")?.parse::<u64>().ok()?;
    Some(AdmissionState { version })
}

fn is_transient_contention(error: &StoreError) -> bool {
    error.envelope.reason == "store.locked"
}

/// Run the complete cold-start setup once. The caller retries only SQLite
/// contention; schema, WAL, and integrity errors remain fail-closed.
fn setup_journal(
    conn: &mut Connection,
    requested_service_instance: Option<&str>,
) -> StoreResult<()> {
    // Inspect before enabling WAL or mutating schema. This transaction is
    // read-only and preserves future, legacy, and malformed journals.
    preflight_schema(conn)?;
    let wal: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !wal.eq_ignore_ascii_case("wal") {
        return Err(StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.wal.unavailable",
        )
        .with_remediation("the filesystem must support WAL journaling"));
    }
    configure_journal_connection(conn)?;
    // One immediate transaction owns the complete setup sequence. The
    // physical DDL and every version stamp become visible together, so a
    // concurrent opener cannot combine an old user_version with a newer
    // table shape. The second preflight is inside that write transaction:
    // another opener may have completed setup after the first read-only
    // preflight, and its current state is the only state we may migrate.
    let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (stored, outbox_shape) = preflight_schema_snapshot(&transaction)?;
    repair_historic_jobs_shape(&transaction, stored)?;
    transaction.execute_batch(SCHEMA)?;
    if !journal_identity_table_is_exact(&transaction)? {
        return Err(
            StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
                .with_remediation(
                "preserve the journal unchanged; service identity table does not match schema 11",
            ),
        );
    }
    // Older schemas may carry an earlier fence implementation. Remove it
    // before migrations touch guarded tables. A v11 journal already has the
    // complete fence; leave an exact installation byte-stable on reopen.
    if stored < JOURNAL_SCHEMA_VERSION {
        remove_journal_write_fence_triggers(&transaction)?;
    }
    let legacy_eventless = legacy_eventless_source(&transaction, stored, outbox_shape)?;
    if matches!(outbox_shape, OutboxSchema::V2) {
        migrate_v2_to_v3(&transaction)?;
    }
    // Upgrade the physical shape and stamp the current version *before* any
    // event may be written. An older binary that reopens this file then hits
    // `journal.schema.newer` and refuses it, instead of decoding it with an
    // incomplete event vocabulary.
    migrate_v3_to_v4(&transaction)?;
    migrate_v4_to_v5(&transaction)?;
    migrate_v5_to_v6(&transaction)?;
    migrate_v6_to_v7(&transaction)?;
    migrate_v7_to_v8(&transaction)?;
    migrate_v8_to_v9(&transaction, legacy_eventless)?;
    migrate_v9_to_v10(&transaction)?;
    migrate_v10_to_v11(&transaction, requested_service_instance)?;
    ensure_journal_write_fence(&transaction)?;
    transaction.commit()?;
    Ok(())
}

/// Apply and verify connection-local journal guarantees on every connection.
/// WAL mode is persistent in the database; FULL sync and foreign keys are not.
fn configure_journal_connection(conn: &Connection) -> StoreResult<()> {
    let wal: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !wal.eq_ignore_ascii_case("wal") {
        return Err(StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.wal.unavailable",
        )
        .with_remediation("the filesystem must support WAL journaling"));
    }
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    let synchronous: i64 = conn.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    let foreign_keys: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if synchronous != 2 || foreign_keys != 1 {
        return Err(StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.connection.settings",
        )
        .with_remediation(format!(
            "journal connection requires synchronous=FULL and foreign_keys=ON; found synchronous={synchronous}, foreign_keys={foreign_keys}"
        )));
    }
    assert_sqlite_version(conn)
}

/// The pre-event schema could contain live materialized rows without any
/// corresponding event history. Record that fact before migrations alter the
/// physical shape; setup installs the actual state snapshot after migrations.
fn legacy_eventless_source(
    tx: &rusqlite::Transaction<'_>,
    stored: u32,
    outbox_shape: OutboxSchema,
) -> StoreResult<bool> {
    if stored >= 9 {
        return Ok(false);
    }
    let event_count: i64 = tx.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
    if event_count != 0 {
        return Ok(false);
    }
    let materialized_exists: i64 = tx.query_row(
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM slots)
                          OR EXISTS (SELECT 1 FROM jobs)
                          OR EXISTS (SELECT 1 FROM outbox)
                          OR EXISTS (SELECT 1 FROM meta)
                     THEN 1 ELSE 0 END",
        [],
        |row| row.get(0),
    )?;
    if materialized_exists == 0 {
        return Ok(false);
    }
    // The pre-v8 version is the provenance boundary: those versions are the
    // documented materialized-state migrations, including the v2 fixture.
    // A nonempty eventless v8 (or a newer physical shape stamped as v8) could
    // be a writer that lost its events, so guessing a replay origin would
    // make deletion invisible. Preserve it unchanged and fail closed.
    if stored < 8
        && matches!(
            outbox_shape,
            OutboxSchema::V2 | OutboxSchema::V3 | OutboxSchema::V4
        )
    {
        return Ok(true);
    }
    Err(replay_baseline_provenance(stored, outbox_shape))
}

/// Install a strict replay anchor. Legacy eventless files use their current
/// materialized state; all other upgrades use the reducer's empty origin and
/// replay their complete event log.
fn install_replay_baseline(
    tx: &rusqlite::Transaction<'_>,
    legacy_eventless: bool,
) -> StoreResult<()> {
    let mut state = if legacy_eventless {
        load_materialized_state(tx)?
    } else {
        FleetState {
            journal_writable: true,
            ..FleetState::default()
        }
    };
    // These latches are lifecycle metadata, not event projections. The
    // materialized read overlays the live values after replay validation.
    state.drain_active = false;
    state.drain_version = 0;
    state.admission_blocked = false;
    state.admission_version = 0;
    let baseline = ReplayBaseline {
        format_version: 1,
        source: if legacy_eventless {
            ReplayBaselineSource::LegacyMaterialized
        } else {
            ReplayBaselineSource::Empty
        },
        state: ReplayBaselineState::from_fleet(&state),
    };
    let serialized = serde_json::to_string(&baseline).map_err(|error| {
        StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.replay.baseline.encode",
        )
        .with_remediation(error.to_string())
    })?;
    let checksum = sha256_hex(serialized.as_bytes());
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2), (?3, ?4)",
        params![
            REPLAY_BASELINE_KEY,
            serialized,
            REPLAY_BASELINE_CHECKSUM_KEY,
            checksum
        ],
    )?;
    Ok(())
}

/// Validate the journal's recorded version and physical migration shape.
///
/// The caller must serialize this read against schema setup. Keeping all
/// observations in one helper prevents a future caller from accidentally
/// reintroducing a version/shape race between independent reads.
fn preflight_schema(conn: &Connection) -> StoreResult<(u32, OutboxSchema)> {
    let transaction = conn.unchecked_transaction()?;
    let result = preflight_schema_snapshot(&transaction);
    if result.is_ok() {
        transaction.commit()?;
    }
    result
}

fn preflight_schema_snapshot(conn: &Connection) -> StoreResult<(u32, OutboxSchema)> {
    // A v1 database is still owned by the retired capacity model. Inspect it
    // before enabling WAL, creating missing tables, or starting a migration
    // transaction: contaminated evidence must remain byte stable and must
    // never reach `persist_state`.
    let stored: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if stored < 0 {
        return Err(journal_schema_shape_mismatch(
            "PRAGMA user_version is negative",
        ));
    }
    let stored = u32::try_from(stored).map_err(|_| journal_schema_newer())?;
    let outbox_shape = outbox_schema_shape(conn)?;
    if stored == 1 {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.legacy.unsafe",
        )
        .with_remediation(
            "preserve the schema-v1 journal unchanged for forensics and perform an explicit verified migration",
        ));
    }
    ensure_supported_schema(stored, JOURNAL_SCHEMA_VERSION)?;
    if stored >= 10 {
        if !journal_pressure_tables_are_exact(conn)? {
            return Err(journal_schema_shape_mismatch(
                "schema-10 disk-pressure tables are missing or invalid",
            ));
        }
    } else if journal_pressure_table_exists(conn)? {
        return Err(journal_schema_shape_mismatch(
            "disk-pressure tables exist before their schema-10 introduction",
        ));
    }
    if stored >= 11 {
        if !journal_identity_table_is_exact(conn)? {
            return Err(journal_schema_shape_mismatch(
                "schema-11 service identity table is missing or invalid",
            ));
        }
    } else if journal_identity_table_exists(conn)? {
        return Err(journal_schema_shape_mismatch(
            "service identity table exists before its schema-11 introduction",
        ));
    }
    if stored == JOURNAL_SCHEMA_VERSION {
        require_replay_baseline_keys(conn)?;
        if load_replay_baseline(conn)?.is_none() {
            return Err(replay_baseline_missing());
        }
        validate_current_schema_before_ddl(conn)?;
    }
    // Physical shape ahead of the recorded version means a writer mutated
    // the tables without stamping `PRAGMA user_version`. Refuse rather than
    // guess which vocabulary wrote the events.
    if outbox_shape_rank(outbox_shape) > version_outbox_rank(stored) {
        return Err(outbox_schema_mismatch(stored, outbox_shape));
    }
    // The same rule for v5, whose shape change is in `jobs` rather than
    // `outbox`, so the outbox-only check above cannot see it.
    //
    // Scoped to exactly a v4 stamp, written as the literal 4. Older stamps
    // are not evidence of a mutated shape: a v2 or v3 file opened here may
    // legitimately carry the current `jobs` shape, because `SCHEMA` creates
    // that table at the current definition and their own migrations stamp
    // explicitly. Only the v4-to-v5 step can be silently re-stamped, so only
    // it needs guarding.
    //
    // This was `JOURNAL_SCHEMA_VERSION - 1`, which is the same defect that
    // bricked every journal on the v5 bump: raising the constant silently
    // re-aimed the check at a different transition. Version comparisons name
    // their own version.
    if stored == 4 && table_has_column(conn, "jobs", "provisional")? {
        return Err(outbox_schema_mismatch(stored, outbox_shape));
    }
    // And again for v6, whose shape change is also in `jobs`.
    if stored == 5 && table_has_column(conn, "jobs", "plan_id")? {
        return Err(outbox_schema_mismatch(stored, outbox_shape));
    }
    Ok((stored, outbox_shape))
}

fn ensure_supported_schema(stored: u32, supported: u32) -> StoreResult<()> {
    if stored > supported {
        return Err(journal_schema_newer());
    }
    Ok(())
}

fn require_replay_baseline_keys(conn: &Connection) -> StoreResult<()> {
    let meta_exists: i64 = conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta'
         )",
        [],
        |row| row.get(0),
    )?;
    if meta_exists == 0 {
        return Err(replay_baseline_missing());
    }
    let baseline_exists: i64 = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM meta WHERE key = ?1)",
        [REPLAY_BASELINE_KEY],
        |row| row.get(0),
    )?;
    let checksum_exists: i64 = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM meta WHERE key = ?1)",
        [REPLAY_BASELINE_CHECKSUM_KEY],
        |row| row.get(0),
    )?;
    if baseline_exists == 0 || checksum_exists == 0 {
        return Err(replay_baseline_missing());
    }
    Ok(())
}

/// Remove all write-fence triggers while setup owns the immediate migration
/// transaction. This also upgrades databases produced by the earlier
/// baseline-delete-only fence.
fn remove_journal_write_fence_triggers(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    for (name, _, _) in JOURNAL_WRITE_FENCE_TRIGGERS {
        tx.execute_batch(&format!("DROP TRIGGER IF EXISTS {name};"))?;
    }
    tx.execute_batch(&format!(
        "DROP TRIGGER IF EXISTS {delete_trigger};
         DROP TRIGGER IF EXISTS {rename_trigger};",
        delete_trigger = LEGACY_REPLAY_BASELINE_DELETE_FENCE_TRIGGER,
        rename_trigger = LEGACY_REPLAY_BASELINE_RENAME_FENCE_TRIGGER,
    ))?;
    Ok(())
}

/// Install and verify the durable mixed-version fence after the replay anchor
/// exists. `PRAGMA user_version` is only an open-time convention: an already
/// open old connection can otherwise issue every DML write path after a
/// different connection completes migration. The persistent gate row exists
/// only inside a current writer's transaction; every guarded table-operation
/// triggers reject writes made without that row. The complete replacement is
/// one atomic setup transaction.
fn ensure_journal_write_fence(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    if journal_write_fence_is_exact(tx)? {
        // An older build used these names for a narrower baseline-only
        // fence. They are harmless when absent and must not survive beside
        // the complete table-operation fence.
        tx.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS {delete_trigger};
             DROP TRIGGER IF EXISTS {rename_trigger};",
            delete_trigger = LEGACY_REPLAY_BASELINE_DELETE_FENCE_TRIGGER,
            rename_trigger = LEGACY_REPLAY_BASELINE_RENAME_FENCE_TRIGGER,
        ))?;
        return Ok(());
    }

    // Replace an incomplete or malformed installation while the setup
    // transaction owns the write lock. There is no observable interval in
    // which the migrated tables are writable without the fence.
    remove_journal_write_fence_triggers(tx)?;
    tx.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {gate} (
             id INTEGER PRIMARY KEY CHECK (id = 1)
         );",
        gate = JOURNAL_WRITE_GATE_TABLE,
    ))?;
    for (name, table, operation) in JOURNAL_WRITE_FENCE_TRIGGERS {
        let sql = journal_write_fence_trigger_sql(name, table, operation);
        tx.execute_batch(&sql)?;
    }

    if !journal_write_fence_is_exact(tx)? {
        return Err(journal_write_fence_invalid(
            "installed trigger set or definition is not exact".to_owned(),
        ));
    }
    Ok(())
}

fn journal_write_fence_is_exact(tx: &Connection) -> StoreResult<bool> {
    let gate_schema: Option<String> = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [JOURNAL_WRITE_GATE_TABLE],
            |row| row.get(0),
        )
        .optional()?;
    let Some(gate_schema) = gate_schema else {
        return Ok(false);
    };
    if normalize_sql(&gate_schema) != normalize_sql(journal_write_gate_table_sql()) {
        return Ok(false);
    }
    let gate_rows: i64 = tx.query_row("SELECT COUNT(*) FROM journal_write_gate", [], |row| {
        row.get(0)
    })?;
    if gate_rows != 0 {
        return Ok(false);
    }

    // The expected triggers are the complete schema boundary. Count every
    // trigger attached to a guarded table, including the gate itself, so a
    // differently named BEFORE/AFTER trigger cannot open the gate indirectly.
    // Temporary triggers are connection-local and are never part of this
    // journal schema; reject them on the same tables as well.
    for catalog in ["sqlite_master", "sqlite_temp_master"] {
        let trigger_count: i64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM {catalog}
                 WHERE type = 'trigger'
                   AND lower(tbl_name) IN (
                       'events', 'slots', 'jobs', 'outbox', 'meta',
                       'disk_pressure_episodes', 'disk_pressure_launches',
                       'disk_pressure_observations',
                       'journal_identity',
                       'journal_write_gate'
                   )"
            ),
            [],
            |row| row.get(0),
        )?;
        let expected = if catalog == "sqlite_master" {
            JOURNAL_WRITE_FENCE_TRIGGERS.len() as i64
        } else {
            0
        };
        if trigger_count != expected {
            return Ok(false);
        }
    }

    for (name, table, operation) in JOURNAL_WRITE_FENCE_TRIGGERS {
        let actual: Option<(String, String)> = tx
            .query_row(
                "SELECT tbl_name, sql
                 FROM sqlite_master
                 WHERE type = 'trigger' AND name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((actual_table, actual_sql)) = actual else {
            return Ok(false);
        };
        let expected = journal_write_fence_trigger_sql(name, table, operation);
        if !actual_table.eq_ignore_ascii_case(table)
            || normalize_sql(&actual_sql) != normalize_sql(&expected)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn journal_identity_table_is_exact(conn: &Connection) -> StoreResult<bool> {
    let schema: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'journal_identity'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(schema) = schema else {
        return Ok(false);
    };
    if normalize_sql(&schema) != normalize_sql(journal_identity_table_sql()) {
        return Ok(false);
    }
    let mut statement = conn.prepare("PRAGMA table_info(journal_identity)")?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(columns
        == [
            ("id".to_owned(), "INTEGER".to_owned(), 0, 1),
            ("service_instance".to_owned(), "TEXT".to_owned(), 1, 0),
        ])
}

fn validate_current_schema_before_ddl(conn: &Connection) -> StoreResult<()> {
    if !journal_identity_table_is_exact(conn)? || !journal_write_fence_is_exact(conn)? {
        return Err(journal_schema_shape_mismatch(
            "schema-11 identity table or required write-fence schema is missing or invalid",
        ));
    }
    Ok(())
}

fn journal_schema_shape_mismatch(detail: &str) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
        .with_remediation(format!("preserve the journal unchanged; {detail}"))
}

fn journal_pressure_table_exists(conn: &Connection) -> StoreResult<bool> {
    conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name IN (
                 'disk_pressure_episodes',
                 'disk_pressure_launches',
                 'disk_pressure_observations'
             )
         )",
        [],
        |row| row.get(0),
    )
    .map_err(StoreError::from)
}

fn journal_pressure_tables_are_exact(conn: &Connection) -> StoreResult<bool> {
    for (name, expected) in [
        (
            "disk_pressure_episodes",
            "CREATE TABLE disk_pressure_episodes (
                 service_instance TEXT NOT NULL,
                 filesystem_id TEXT NOT NULL,
                 volume_fingerprint TEXT,
                 episode_id TEXT NOT NULL,
                 started_unix INTEGER NOT NULL,
                 deadline_unix INTEGER NOT NULL,
                 drain_deadline_unix INTEGER NOT NULL,
                 last_observed_unix INTEGER NOT NULL,
                 reclaim_attempted INTEGER NOT NULL DEFAULT 0,
                 revision INTEGER NOT NULL,
                 draining INTEGER NOT NULL DEFAULT 0,
                 terminal INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (service_instance, filesystem_id)
             )",
        ),
        (
            "disk_pressure_launches",
            "CREATE TABLE disk_pressure_launches (
                 service_instance TEXT NOT NULL,
                 slot_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 launch_nonce TEXT NOT NULL,
                 issued_unix INTEGER NOT NULL,
                 active INTEGER NOT NULL DEFAULT 1,
                 PRIMARY KEY (service_instance, slot_id)
             )",
        ),
        (
            "disk_pressure_observations",
            "CREATE TABLE disk_pressure_observations (
                 service_instance TEXT NOT NULL,
                 filesystem_id TEXT NOT NULL,
                 volume_fingerprint TEXT,
                 identity_confirmed INTEGER NOT NULL DEFAULT 0,
                 last_observed_unix INTEGER NOT NULL,
                 PRIMARY KEY (service_instance, filesystem_id)
             )",
        ),
    ] {
        let schema: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if schema
            .as_deref()
            .is_none_or(|schema| normalize_sql(schema) != normalize_sql(expected))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn journal_identity_table_exists(conn: &Connection) -> StoreResult<bool> {
    conn.query_row(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'journal_identity'
         )",
        [],
        |row| row.get(0),
    )
    .map_err(StoreError::from)
}

fn journal_identity_table_sql() -> &'static str {
    "CREATE TABLE journal_identity (
         id INTEGER PRIMARY KEY CHECK (id = 1),
         service_instance TEXT NOT NULL
     )"
}

fn journal_write_gate_table_sql() -> &'static str {
    "CREATE TABLE journal_write_gate (
         id INTEGER PRIMARY KEY CHECK (id = 1)
     );"
}

fn journal_write_fence_trigger_sql(name: &str, table: &str, operation: &str) -> String {
    format!(
        "CREATE TRIGGER {name}
             BEFORE {operation} ON {table}
             FOR EACH ROW
             WHEN NOT EXISTS (
                 SELECT 1 FROM {gate} WHERE id = 1
             )
             BEGIN
                 SELECT RAISE(ROLLBACK, '{reason}');
             END;",
        gate = JOURNAL_WRITE_GATE_TABLE,
        reason = JOURNAL_WRITE_FENCE_REASON,
    )
}

fn normalize_sql(sql: &str) -> String {
    sql.trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn begin_journal_write_gate(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    if !journal_write_fence_is_exact(tx)? {
        return Err(journal_write_fence_invalid(
            "gate schema, trigger set, or empty gate row is not exact".to_owned(),
        ));
    }
    let inserted = tx.execute("INSERT INTO journal_write_gate (id) VALUES (1)", [])?;
    if inserted != 1 {
        return Err(journal_write_fence_invalid(format!(
            "gate insert affected {inserted} rows"
        )));
    }
    let gate_rows: i64 = tx.query_row("SELECT COUNT(*) FROM journal_write_gate", [], |row| {
        row.get(0)
    })?;
    if gate_rows != 1 {
        return Err(journal_write_fence_invalid(format!(
            "gate row count after insert is {gate_rows}"
        )));
    }
    Ok(())
}

fn end_journal_write_gate(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let deleted = tx.execute("DELETE FROM journal_write_gate WHERE id = 1", [])?;
    if deleted != 1 {
        return Err(journal_write_fence_invalid(format!(
            "gate delete affected {deleted} rows"
        )));
    }
    let gate_rows: i64 = tx.query_row("SELECT COUNT(*) FROM journal_write_gate", [], |row| {
        row.get(0)
    })?;
    if gate_rows != 0 {
        return Err(journal_write_fence_invalid(format!(
            "gate row count after delete is {gate_rows}"
        )));
    }
    Ok(())
}

fn validate_disk_pressure_key(value: &str, field: &str) -> StoreResult<()> {
    if value.trim().is_empty() || value.len() > 512 {
        return Err(disk_pressure_state_invalid(format!(
            "{field} is empty or exceeds 512 bytes"
        )));
    }
    Ok(())
}

fn journal_service_instance_mismatch(detail: &str) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.service_instance.mismatch",
    )
    .with_remediation(format!(
        "open and mutate this fleet journal only as its bound service instance: {detail}"
    ))
}

fn validate_journal_service_instance(conn: &Connection, service_instance: &str) -> StoreResult<()> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT service_instance FROM journal_identity WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if stored
        .as_deref()
        .is_some_and(|stored| stored != service_instance)
    {
        return Err(journal_service_instance_mismatch(
            "another service instance already owns this database",
        ));
    }
    let has_foreign_pressure_rows: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM disk_pressure_episodes WHERE service_instance <> ?1
         ) OR EXISTS(
             SELECT 1 FROM disk_pressure_launches WHERE service_instance <> ?1
         ) OR EXISTS(
             SELECT 1 FROM disk_pressure_observations WHERE service_instance <> ?1
         )",
        [service_instance],
        |row| row.get(0),
    )?;
    if has_foreign_pressure_rows {
        return Err(journal_service_instance_mismatch(
            "pressure tables contain rows owned by another service instance",
        ));
    }
    Ok(())
}

fn journal_is_provably_new_and_empty(conn: &Connection) -> StoreResult<bool> {
    let serialized: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [REPLAY_BASELINE_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let Some(serialized) = serialized else {
        return Ok(false);
    };
    let baseline: ReplayBaseline = serde_json::from_str(&serialized).map_err(|error| {
        StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.replay.baseline.invalid",
        )
        .with_remediation(format!("preserve the journal unchanged; {error}"))
    })?;
    let empty_state = FleetState {
        journal_writable: true,
        ..FleetState::default()
    };
    // Older empty journals may have gained a LegacyMaterialized anchor during
    // upgrade. Its source label is safe here only because the checked state
    // and the physical row counts below still prove the database is empty.
    if !matches!(
        baseline.source,
        ReplayBaselineSource::Empty | ReplayBaselineSource::LegacyMaterialized
    ) || baseline.state.into_fleet() != empty_state
    {
        return Ok(false);
    }

    for table in [
        "events",
        "slots",
        "jobs",
        "outbox",
        "disk_pressure_episodes",
        "disk_pressure_launches",
        "disk_pressure_observations",
    ] {
        let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
        if count != 0 {
            return Ok(false);
        }
    }
    let ordinary_meta_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM meta WHERE key NOT IN (?1, ?2)",
        params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
        |row| row.get(0),
    )?;
    Ok(ordinary_meta_rows == 0)
}

fn bind_journal_service_instance(
    tx: &rusqlite::Transaction<'_>,
    service_instance: &str,
) -> StoreResult<()> {
    validate_journal_service_instance(tx, service_instance)?;
    let already_bound: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM journal_identity WHERE id = 1)",
        [],
        |row| row.get(0),
    )?;
    if !already_bound {
        if !journal_is_provably_new_and_empty(tx)? {
            return Err(journal_service_instance_mismatch(
                "an unowned journal may be bound only before it contains events, materialized state, or pressure history",
            ));
        }
        tx.execute(
            "INSERT INTO journal_identity (id, service_instance) VALUES (1, ?1)",
            [service_instance],
        )?;
    }
    Ok(())
}

fn disk_pressure_sql_integer(value: u64, field: &str) -> StoreResult<i64> {
    i64::try_from(value).map_err(|_| {
        disk_pressure_state_invalid(format!("{field} exceeds SQLite's signed integer range"))
    })
}

fn disk_pressure_u64(value: i64, field: &str) -> StoreResult<u64> {
    u64::try_from(value)
        .map_err(|_| disk_pressure_state_invalid(format!("{field} is negative in the journal")))
}

fn disk_pressure_bool(value: i64, field: &str) -> StoreResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(disk_pressure_state_invalid(format!(
            "{field} is not a SQLite boolean"
        ))),
    }
}

fn disk_pressure_state_invalid(detail: String) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.disk_pressure.state.invalid",
    )
    .with_remediation(format!(
        "preserve the journal unchanged and refuse admission while disk pressure is low: {detail}"
    ))
}

fn disk_pressure_launch_fenced() -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.disk_pressure.launch.fenced",
    )
    .with_remediation("discard this worker; a newer slot launch owns the pressure writer lease")
}

fn pressure_state_slots_are_safe(state: &FleetState) -> bool {
    !state.capacity_invalid
        && state
            .slots
            .iter()
            .all(|slot| slot.phase == SlotPhase2::Fenced)
        && state.jobs.iter().all(|job| !job.phase.occupies_slot())
}

fn disk_pressure_launch_is_current(
    conn: &Connection,
    service_instance: &str,
    slot_id: &SlotId,
    generation: i64,
    launch_nonce: &str,
) -> StoreResult<bool> {
    let launch: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT generation, launch_nonce, active FROM disk_pressure_launches
             WHERE service_instance = ?1 AND slot_id = ?2",
            params![service_instance, slot_id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let lease_matches = launch.is_some_and(|(stored_generation, stored_nonce, active)| {
        stored_generation == generation && stored_nonce == launch_nonce && active == 1
    });
    if !lease_matches {
        return Ok(false);
    }
    let generation_u64 = disk_pressure_u64(generation, "launch generation")?;
    let state = load_materialized_state(conn)?;
    Ok(!state.capacity_invalid
        && state.slots.iter().any(|slot| {
            slot.slot_id == *slot_id
                && slot.generation.0 == generation_u64
                && slot.phase != SlotPhase2::Fenced
        }))
}

fn validate_acquisition_response_fence(
    conn: &Connection,
    state: &FleetState,
    fence: &AcquisitionResponseFence,
) -> StoreResult<()> {
    let launch: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT generation, launch_nonce, active FROM disk_pressure_launches
             WHERE service_instance = ?1 AND slot_id = ?2",
            params![fence.service_instance, fence.slot_id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stored_generation, stored_nonce, active)) = launch else {
        return Err(acquisition_response_fenced());
    };
    if stored_generation != fence.generation || stored_nonce != fence.launch_nonce {
        return Err(acquisition_response_fenced());
    }
    let active = disk_pressure_bool(active, "launch active")?;
    let intent_fence = AcquisitionIntentFence {
        service_instance: fence.service_instance.clone(),
        slot_id: fence.slot_id.clone(),
        generation: fence.generation,
        launch_nonce: fence.launch_nonce.clone(),
        provisional_job_id: fence.provisional_job_id.clone(),
        message_id: fence.message_id.clone(),
        run_service_url: fence.run_service_url.clone(),
    };
    if !exact_provisional_acquisition_row(state, &intent_fence)
        || !acquisition_intent_event_is_exact(conn, &intent_fence)?
    {
        return Err(acquisition_response_fenced());
    }

    if active {
        if disk_pressure_launch_is_current(
            conn,
            &fence.service_instance,
            &fence.slot_id,
            fence.generation,
            &fence.launch_nonce,
        )? {
            return Ok(());
        }
        return Err(acquisition_response_fenced());
    }

    let terminally_fenced = state.slots.iter().any(|slot| {
        slot.slot_id == fence.slot_id
            && slot.generation.0 == fence.generation as u64
            && slot.phase == SlotPhase2::Fenced
    });
    if !terminally_fenced
        || !has_disk_pressure_terminal_fence(conn, &fence.slot_id, fence.generation)?
    {
        return Err(acquisition_response_fenced());
    }
    Ok(())
}

fn exact_provisional_acquisition_row(state: &FleetState, fence: &AcquisitionIntentFence) -> bool {
    state.jobs.iter().any(|job| {
        job.job_id == fence.provisional_job_id
            && job.slot_id == fence.slot_id
            && job.generation.0 == fence.generation as u64
            && job.provisional
            && job.phase == JobPhase2::Assigned
            && job.plan_id.is_empty()
            && job.run_service_url == fence.run_service_url
    })
}

fn acquisition_intent_event_is_exact(
    conn: &Connection,
    fence: &AcquisitionIntentFence,
) -> StoreResult<bool> {
    let mut statement = conn.prepare(
        "SELECT generation, kind, payload, checksum FROM events
         WHERE generation = ?1 AND kind = 'job_acquisition_intended'
         ORDER BY id DESC",
    )?;
    let rows = statement.query_map([fence.generation], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (generation, kind, payload, checksum) = row?;
        if matches!(
            decode_checked_event(generation, &kind, &payload, &checksum)?,
            Event::JobAcquisitionIntended {
                slot_id,
                job_id,
                generation: event_generation,
                message_id,
                run_service_url,
                ..
            } if slot_id == fence.slot_id
                && job_id == fence.provisional_job_id
                && event_generation.0 as i64 == fence.generation
                && message_id == fence.message_id
                && run_service_url == fence.run_service_url
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn validate_acquisition_abandon_fence(
    conn: &Connection,
    state: &FleetState,
    fence: &AcquisitionAbandonFence,
) -> StoreResult<()> {
    let launch: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT generation, launch_nonce, active FROM disk_pressure_launches
             WHERE service_instance = ?1 AND slot_id = ?2",
            params![fence.service_instance, fence.slot_id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stored_generation, stored_nonce, active)) = launch else {
        return Err(acquisition_abandon_fenced());
    };
    if stored_generation != fence.generation || stored_nonce != fence.launch_nonce {
        return Err(acquisition_abandon_fenced());
    }
    if disk_pressure_bool(active, "launch active")? {
        if !disk_pressure_launch_is_current(
            conn,
            &fence.service_instance,
            &fence.slot_id,
            fence.generation,
            &fence.launch_nonce,
        )? {
            return Err(acquisition_abandon_fenced());
        }
    } else {
        let slot_is_terminally_fenced = state.slots.iter().any(|slot| {
            slot.slot_id == fence.slot_id
                && slot.generation.0 == fence.generation as u64
                && slot.phase == SlotPhase2::Fenced
        });
        let active_terminal_episode: i64 = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM disk_pressure_episodes
                 WHERE service_instance = ?1 AND terminal = 1
             )",
            [&fence.service_instance],
            |row| row.get(0),
        )?;
        if !slot_is_terminally_fenced
            || !disk_pressure_bool(active_terminal_episode, "active terminal episode")?
            || !has_disk_pressure_terminal_fence(conn, &fence.slot_id, fence.generation)?
        {
            return Err(acquisition_abandon_fenced());
        }
    }
    let intent_fence = AcquisitionIntentFence {
        service_instance: fence.service_instance.clone(),
        slot_id: fence.slot_id.clone(),
        generation: fence.generation,
        launch_nonce: fence.launch_nonce.clone(),
        provisional_job_id: fence.provisional_job_id.clone(),
        message_id: fence.message_id.clone(),
        run_service_url: fence.run_service_url.clone(),
    };
    if !exact_provisional_acquisition_row(state, &intent_fence)
        || !acquisition_intent_event_is_exact(conn, &intent_fence)?
    {
        return Err(acquisition_abandon_fenced());
    }
    Ok(())
}

fn validate_acquisition_recovery_fence(
    conn: &Connection,
    state: &FleetState,
    fence: &AcquisitionRecoveryFence,
) -> StoreResult<()> {
    let launch: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT generation, launch_nonce, active FROM disk_pressure_launches
             WHERE service_instance = ?1 AND slot_id = ?2",
            params![fence.service_instance, fence.slot_id.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((stored_generation, launch_nonce, active)) = launch else {
        return Err(acquisition_recovery_fenced());
    };
    validate_disk_pressure_key(&launch_nonce, "launch nonce")?;
    if stored_generation != fence.generation || disk_pressure_bool(active, "launch active")? {
        return Err(acquisition_recovery_fenced());
    }

    let terminally_fenced = state.slots.iter().any(|slot| {
        slot.slot_id == fence.slot_id
            && slot.generation.0 == fence.generation as u64
            && slot.phase == SlotPhase2::Fenced
    });
    let exact_row = state.jobs.iter().any(|job| {
        job.job_id == fence.acquired_job_id
            && job.slot_id == fence.slot_id
            && job.generation.0 == fence.generation as u64
            && job.phase == JobPhase2::Assigned
            && job.provisional
            && job.plan_id == fence.plan_id
            && job.run_service_url == fence.run_service_url
    });
    if !terminally_fenced
        || !has_disk_pressure_terminal_fence(conn, &fence.slot_id, fence.generation)?
        || !exact_row
        || !resolved_acquisition_intent_is_exact(conn, fence)?
    {
        return Err(acquisition_recovery_fenced());
    }
    Ok(())
}

fn resolved_acquisition_intent_is_exact(
    conn: &Connection,
    fence: &AcquisitionRecoveryFence,
) -> StoreResult<bool> {
    let mut statement = conn.prepare(
        "SELECT generation, kind, payload, checksum FROM events
         WHERE generation = ?1
           AND kind IN (
               'job_acquisition_intended', 'job_acquisition_resolved',
               'job_acquisition_lost'
           )
         ORDER BY id ASC",
    )?;
    let rows = statement.query_map([fence.generation], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut pending_intents = HashMap::<JobId, (SlotId, String)>::new();
    for row in rows {
        let (generation, kind, payload, checksum) = row?;
        match decode_checked_event(generation, &kind, &payload, &checksum)? {
            Event::JobAcquisitionIntended {
                slot_id,
                job_id,
                generation: event_generation,
                run_service_url,
                ..
            } if event_generation.0 as i64 == fence.generation => {
                pending_intents.insert(job_id, (slot_id, run_service_url));
            }
            Event::JobAcquisitionLost {
                job_id,
                generation: event_generation,
                ..
            } if event_generation.0 as i64 == fence.generation => {
                pending_intents.remove(&job_id);
            }
            Event::JobAcquisitionResolved {
                provisional_job_id,
                acquired_job_id,
                plan_id,
                generation: event_generation,
            } if event_generation.0 as i64 == fence.generation => {
                let intent = pending_intents.remove(&provisional_job_id);
                if acquired_job_id == fence.acquired_job_id
                    && plan_id == fence.plan_id
                    && intent.is_some_and(|(slot_id, url)| {
                        slot_id == fence.slot_id && url == fence.run_service_url
                    })
                {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

fn has_disk_pressure_terminal_fence(
    conn: &Connection,
    slot_id: &SlotId,
    generation: i64,
) -> StoreResult<bool> {
    let mut statement = conn.prepare(
        "SELECT generation, kind, payload, checksum FROM events
         WHERE generation = ?1 AND kind = 'disk_pressure_terminal_fence'
         ORDER BY id DESC",
    )?;
    let rows = statement.query_map([generation], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (event_generation, kind, payload, checksum) = row?;
        if matches!(
            decode_checked_event(event_generation, &kind, &payload, &checksum)?,
            Event::DiskPressureTerminalFence {
                slot_id: ref fenced_slot,
                generation: fenced_generation,
            } if fenced_slot == slot_id && fenced_generation.0 == generation as u64
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn acquisition_response_fenced() -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.acquisition.response.fenced",
    )
    .with_remediation(
        "preserve the acquirejob response and provisional row; only the exact recorded launch may resolve it",
    )
}

fn acquisition_abandon_fenced() -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.acquisition.abandon.fenced",
    )
    .with_remediation(
        "preserve the provisional row unless this worker's exact intent is terminal-fenced and its run-service response is typed gone",
    )
}

fn acquisition_recovery_fenced() -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.acquisition.recovery.fenced",
    )
    .with_remediation(
        "only a service-bound controller may confirm the exact terminal-fenced acquisition after renewjob proves ownership",
    )
}

fn pressure_terminal_recovery_worker(job_id: &JobId) -> String {
    format!("{PRESSURE_TERMINAL_RECOVERY_WORKER_PREFIX}{}", job_id.0)
}

#[derive(Debug, Clone)]
struct PressureClockSample {
    /// Highest persisted timestamp seen across this service's configured roots
    /// before this observation.
    high_water_before: Option<u64>,
    rollback: bool,
    fingerprint_changed: bool,
    identity_confirmed: bool,
    late_binding: bool,
    /// The last trusted UUID. A changed or missing UUID never overwrites it.
    trusted_fingerprint: Option<String>,
}

/// Return the highest clock observation only when the whole configured-root
/// alias group has been confirmed on the same volume. Controller CAS paths
/// run independently of the worker observation reducer, so they must enforce
/// the same aggregate identity gate under their own transaction.
fn confirmed_pressure_observation_group_high_water(
    conn: &Connection,
    service_instance: &str,
    filesystem_id: &str,
    alias_ids: &[String],
    volume_fingerprint: &str,
) -> StoreResult<Option<u64>> {
    let mut roots = Vec::with_capacity(alias_ids.len() + 1);
    roots.push(filesystem_id);
    for alias in alias_ids {
        if !roots.contains(&alias.as_str()) {
            roots.push(alias);
        }
    }

    let mut high_water = None;
    for root_id in roots {
        let identity: Option<(Option<String>, i64, i64)> = conn
            .query_row(
                "SELECT volume_fingerprint, identity_confirmed, last_observed_unix
                 FROM disk_pressure_observations
                 WHERE service_instance = ?1 AND filesystem_id = ?2",
                params![service_instance, root_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((persisted_fingerprint, confirmed, observed)) = identity else {
            return Ok(None);
        };
        if persisted_fingerprint.as_deref() != Some(volume_fingerprint)
            || !disk_pressure_bool(confirmed, "pressure identity confirmed")?
        {
            return Ok(None);
        }
        let observed = disk_pressure_u64(observed, "pressure clock high-water")?;
        high_water = Some(high_water.map_or(observed, |current: u64| current.max(observed)));
    }
    Ok(high_water)
}

fn persist_pressure_clock_sample(
    transaction: &rusqlite::Transaction<'_>,
    service_instance: &str,
    sample: &DiskPressureFilesystemSample,
    now_unix: u64,
) -> StoreResult<PressureClockSample> {
    let existing: Option<(Option<String>, i64, i64)> = transaction
        .query_row(
            "SELECT volume_fingerprint, identity_confirmed, last_observed_unix
             FROM disk_pressure_observations
             WHERE service_instance = ?1 AND filesystem_id = ?2",
            params![service_instance, sample.filesystem_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let service_high_water: Option<i64> = transaction.query_row(
        "SELECT MAX(last_observed_unix) FROM disk_pressure_observations
         WHERE service_instance = ?1",
        [service_instance],
        |row| row.get(0),
    )?;
    let local_high_water = existing.as_ref().map(|(_, _, observed)| *observed);
    let high_water_before_sql = match (service_high_water, local_high_water) {
        (Some(service), Some(local)) => Some(service.max(local)),
        (Some(service), None) => Some(service),
        (None, Some(local)) => Some(local),
        (None, None) => None,
    };
    let high_water_before = high_water_before_sql
        .map(|value| disk_pressure_u64(value, "pressure clock high-water"))
        .transpose()?;
    let rollback = high_water_before.is_some_and(|high_water| now_unix < high_water);
    let episode_exists: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM disk_pressure_episodes
             WHERE service_instance = ?1 AND filesystem_id = ?2
         )",
        params![service_instance, sample.filesystem_id],
        |row| row.get(0),
    )?;

    let (trusted_fingerprint, identity_confirmed, fingerprint_changed, late_binding) =
        match existing {
            None => (
                sample.volume_fingerprint.clone(),
                sample.volume_fingerprint.is_some(),
                false,
                false,
            ),
            Some((previous, confirmed, _)) => {
                let _ = disk_pressure_bool(confirmed, "pressure identity confirmed")?;
                match (previous, sample.volume_fingerprint.as_ref()) {
                    (Some(previous), Some(observed)) if previous == *observed => {
                        (Some(previous), true, false, false)
                    }
                    (Some(_), Some(observed)) if !episode_exists => {
                        (Some(observed.clone()), true, false, false)
                    }
                    (Some(previous), Some(_)) => (Some(previous), false, true, false),
                    (Some(previous), None) => (Some(previous), false, false, false),
                    (None, Some(observed)) if episode_exists => {
                        (Some(observed.clone()), false, false, true)
                    }
                    (None, Some(observed)) => (Some(observed.clone()), true, false, false),
                    (None, None) => (None, false, false, false),
                }
            }
        };
    let persisted_high_water = high_water_before.unwrap_or(now_unix).max(now_unix);
    let persisted_high_water_sql =
        disk_pressure_sql_integer(persisted_high_water, "pressure clock high-water")?;
    transaction.execute(
        "INSERT INTO disk_pressure_observations (
             service_instance, filesystem_id, volume_fingerprint,
             identity_confirmed, last_observed_unix
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (service_instance, filesystem_id) DO UPDATE SET
             volume_fingerprint = excluded.volume_fingerprint,
             identity_confirmed = excluded.identity_confirmed,
             last_observed_unix = MAX(
                 disk_pressure_observations.last_observed_unix,
                 excluded.last_observed_unix
             )",
        params![
            service_instance,
            sample.filesystem_id,
            trusted_fingerprint,
            if identity_confirmed { 1_i64 } else { 0_i64 },
            persisted_high_water_sql,
        ],
    )?;
    Ok(PressureClockSample {
        high_water_before,
        rollback,
        fingerprint_changed,
        identity_confirmed,
        late_binding,
        trusted_fingerprint,
    })
}

fn persist_pressure_clock_samples(
    transaction: &rusqlite::Transaction<'_>,
    service_instance: &str,
    sample: &DiskPressureFilesystemSample,
    now_unix: u64,
) -> StoreResult<PressureClockSample> {
    let mut root_ids = vec![sample.filesystem_id.as_str()];
    root_ids.extend(sample.alias_ids.iter().map(String::as_str));
    let mut combined: Option<PressureClockSample> = None;
    for root_id in root_ids {
        let mut root_sample = sample.clone();
        root_sample.filesystem_id = root_id.to_owned();
        root_sample.alias_ids.clear();
        let observed =
            persist_pressure_clock_sample(transaction, service_instance, &root_sample, now_unix)?;
        if let Some(combined) = combined.as_mut() {
            combined.high_water_before =
                match (combined.high_water_before, observed.high_water_before) {
                    (Some(first), Some(second)) => Some(first.max(second)),
                    (Some(value), None) | (None, Some(value)) => Some(value),
                    (None, None) => None,
                };
            let fingerprint_conflicts = match (
                combined.trusted_fingerprint.as_deref(),
                observed.trusted_fingerprint.as_deref(),
            ) {
                (Some(first), Some(second)) => first != second,
                _ => false,
            };
            if combined.trusted_fingerprint.is_none() {
                combined.trusted_fingerprint = observed.trusted_fingerprint;
            }
            combined.rollback |= observed.rollback;
            combined.fingerprint_changed |= observed.fingerprint_changed || fingerprint_conflicts;
            combined.identity_confirmed &= observed.identity_confirmed && !fingerprint_conflicts;
            combined.late_binding |= observed.late_binding;
        } else {
            combined = Some(observed);
        }
    }
    combined.ok_or_else(|| disk_pressure_state_invalid("empty pressure root key set".to_owned()))
}

fn merge_pressure_alias_episodes(
    transaction: &rusqlite::Transaction<'_>,
    service_instance: &str,
    sample: &DiskPressureFilesystemSample,
    clocks: &PressureClockSample,
) -> StoreResult<()> {
    if sample.alias_ids.is_empty() {
        return Ok(());
    }
    let mut roots = Vec::with_capacity(sample.alias_ids.len() + 1);
    roots.push(sample.filesystem_id.clone());
    for alias in &sample.alias_ids {
        if !roots.contains(alias) {
            roots.push(alias.clone());
        }
    }
    let mut episodes = Vec::new();
    for root in &roots {
        if let Some(episode) = load_disk_pressure_episode(transaction, service_instance, root)? {
            episodes.push((root.clone(), episode));
        }
    }
    if episodes.is_empty() {
        return Ok(());
    }

    episodes.sort_by_key(|(_, episode)| {
        (
            episode.started_unix,
            episode.deadline_unix,
            episode.drain_deadline_unix,
        )
    });
    let (_, oldest) = &episodes[0];
    let mut merged = oldest.clone();
    let mut fingerprints = Vec::<String>::new();
    for (_, episode) in &episodes {
        if let Some(fingerprint) = &episode.volume_fingerprint
            && !fingerprints.contains(fingerprint)
        {
            fingerprints.push(fingerprint.clone());
        }
        merged.deadline_unix = merged.deadline_unix.min(episode.deadline_unix);
        merged.drain_deadline_unix = merged.drain_deadline_unix.min(episode.drain_deadline_unix);
        merged.last_observed_unix = merged.last_observed_unix.max(episode.last_observed_unix);
        merged.reclaim_attempted |= episode.reclaim_attempted;
        merged.draining |= episode.draining;
        merged.terminal |= episode.terminal;
        merged.revision = merged.revision.max(episode.revision);
    }
    let identity_conflict = fingerprints.len() > 1
        || fingerprints
            .first()
            .is_some_and(|stored| sample.volume_fingerprint.as_ref() != Some(stored));
    merged.volume_fingerprint = fingerprints.first().cloned();
    merged.draining |= identity_conflict || clocks.rollback || clocks.fingerprint_changed;
    merged.terminal |= identity_conflict || clocks.rollback || clocks.fingerprint_changed;
    merged.revision = merged
        .revision
        .checked_add(1)
        .ok_or_else(|| disk_pressure_state_invalid("episode revision overflow".to_owned()))?;

    transaction.execute(
        "INSERT INTO disk_pressure_episodes (
             service_instance, filesystem_id, volume_fingerprint, episode_id, started_unix,
             deadline_unix, drain_deadline_unix, last_observed_unix, reclaim_attempted,
             revision, draining, terminal
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (service_instance, filesystem_id) DO UPDATE SET
             volume_fingerprint = excluded.volume_fingerprint,
             episode_id = excluded.episode_id,
             started_unix = excluded.started_unix,
             deadline_unix = excluded.deadline_unix,
             drain_deadline_unix = excluded.drain_deadline_unix,
             last_observed_unix = excluded.last_observed_unix,
             reclaim_attempted = excluded.reclaim_attempted,
             revision = excluded.revision,
             draining = excluded.draining,
             terminal = excluded.terminal",
        params![
            service_instance,
            sample.filesystem_id,
            merged.volume_fingerprint,
            merged.episode_id,
            disk_pressure_sql_integer(merged.started_unix, "episode start")?,
            disk_pressure_sql_integer(merged.deadline_unix, "cleanup deadline")?,
            disk_pressure_sql_integer(merged.drain_deadline_unix, "drain deadline")?,
            disk_pressure_sql_integer(merged.last_observed_unix, "last observation")?,
            if merged.reclaim_attempted {
                1_i64
            } else {
                0_i64
            },
            disk_pressure_sql_integer(merged.revision, "episode revision")?,
            if merged.draining { 1_i64 } else { 0_i64 },
            if merged.terminal { 1_i64 } else { 0_i64 },
        ],
    )?;
    for alias in roots.iter().filter(|root| **root != sample.filesystem_id) {
        transaction.execute(
            "DELETE FROM disk_pressure_episodes
             WHERE service_instance = ?1 AND filesystem_id = ?2",
            params![service_instance, alias],
        )?;
    }
    Ok(())
}

fn delete_pressure_episode_alias_group(
    transaction: &rusqlite::Transaction<'_>,
    service_instance: &str,
    sample: &DiskPressureFilesystemSample,
    episode: &DiskPressureEpisode,
) -> StoreResult<bool> {
    let revision = disk_pressure_sql_integer(episode.revision, "episode revision")?;
    let mut deleted_primary = false;
    for filesystem_id in std::iter::once(&sample.filesystem_id).chain(&sample.alias_ids) {
        let deleted = transaction.execute(
            "DELETE FROM disk_pressure_episodes
             WHERE service_instance = ?1 AND filesystem_id = ?2
               AND episode_id = ?3 AND revision = ?4",
            params![
                service_instance,
                filesystem_id,
                episode.episode_id,
                revision
            ],
        )?;
        if filesystem_id == &sample.filesystem_id {
            deleted_primary = deleted == 1;
        }
    }
    Ok(deleted_primary)
}

fn observe_disk_pressure_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    service_instance: &str,
    sample: &DiskPressureFilesystemSample,
    degraded_seconds: i64,
    drain_seconds: i64,
    now_unix: u64,
) -> StoreResult<DiskPressureObservation> {
    let filesystem_id = &sample.filesystem_id;
    let clock = persist_pressure_clock_samples(transaction, service_instance, sample, now_unix)?;
    merge_pressure_alias_episodes(transaction, service_instance, sample, &clock)?;
    let measurable = sample.available_bytes.is_some() && sample.volume_fingerprint.is_some();
    let low = !measurable
        || sample
            .available_bytes
            .is_some_and(|available| available < sample.min_free_bytes);
    let mut episode = load_disk_pressure_episode(transaction, service_instance, filesystem_id)?;
    if let Some(current) = episode.as_mut() {
        let rollback = clock.rollback
            || now_unix < current.last_observed_unix
            || now_unix < current.started_unix;
        let previously_unbound = current.volume_fingerprint.is_none();
        let fingerprint_changed = match (
            current.volume_fingerprint.as_deref(),
            sample.volume_fingerprint.as_deref(),
        ) {
            (Some(previous), Some(observed)) => previous != observed,
            (Some(_), None) => false,
            (None, Some(observed)) => {
                current.volume_fingerprint = Some(observed.to_owned());
                false
            }
            (None, None) => false,
        } || clock.fingerprint_changed;
        if rollback || fingerprint_changed {
            current.draining = true;
            current.terminal = true;
        } else if !low
            && !current.terminal
            && clock.identity_confirmed
            && !clock.late_binding
            && current.volume_fingerprint.as_deref() == sample.volume_fingerprint.as_deref()
        {
            if !delete_pressure_episode_alias_group(transaction, service_instance, sample, current)?
            {
                return Err(disk_pressure_state_invalid(
                    "healthy episode compare-and-swap missed".to_owned(),
                ));
            }
            return Ok(DiskPressureObservation {
                episode: None,
                cleared: true,
                reclaim_needed: false,
            });
        }
        let old_revision = current.revision;
        let mut reclaim_needed = false;
        let episode_fingerprint_matches = current.volume_fingerprint.as_deref()
            == sample.volume_fingerprint.as_deref()
            && clock.identity_confirmed
            && !clock.late_binding;
        if low
            && measurable
            && episode_fingerprint_matches
            && !previously_unbound
            && !current.reclaim_attempted
            && !current.terminal
        {
            current.reclaim_attempted = true;
            reclaim_needed = true;
        }
        current.draining |=
            rollback || fingerprint_changed || (low && now_unix >= current.deadline_unix);
        current.terminal |=
            rollback || fingerprint_changed || (low && now_unix >= current.drain_deadline_unix);
        current.last_observed_unix = current
            .last_observed_unix
            .max(now_unix)
            .max(clock.high_water_before.unwrap_or(0));
        current.revision = current
            .revision
            .checked_add(1)
            .ok_or_else(|| disk_pressure_state_invalid("episode revision overflow".to_owned()))?;
        let updated = transaction.execute(
            "UPDATE disk_pressure_episodes
             SET volume_fingerprint = ?1, last_observed_unix = ?2, reclaim_attempted = ?3,
                 revision = ?4, draining = ?5, terminal = ?6
             WHERE service_instance = ?7 AND filesystem_id = ?8
               AND episode_id = ?9 AND revision = ?10",
            params![
                current.volume_fingerprint,
                disk_pressure_sql_integer(current.last_observed_unix, "last observation")?,
                if current.reclaim_attempted {
                    1_i64
                } else {
                    0_i64
                },
                disk_pressure_sql_integer(current.revision, "episode revision")?,
                if current.draining { 1_i64 } else { 0_i64 },
                if current.terminal { 1_i64 } else { 0_i64 },
                service_instance,
                filesystem_id,
                current.episode_id,
                disk_pressure_sql_integer(old_revision, "episode revision")?,
            ],
        )?;
        if updated != 1 {
            return Err(disk_pressure_state_invalid(
                "low-space episode compare-and-swap missed".to_owned(),
            ));
        }
        return Ok(DiskPressureObservation {
            episode: Some(current.clone()),
            cleared: false,
            reclaim_needed,
        });
    } else if low || clock.rollback || clock.fingerprint_changed {
        let degraded_seconds = u64::try_from(degraded_seconds)
            .map_err(|_| disk_pressure_state_invalid("degraded deadline is negative".to_owned()))?;
        let drain_seconds = u64::try_from(drain_seconds)
            .map_err(|_| disk_pressure_state_invalid("drain deadline is negative".to_owned()))?;
        let started_unix = if clock.rollback {
            clock.high_water_before.unwrap_or(now_unix)
        } else {
            now_unix
        };
        let deadline_unix = started_unix
            .checked_add(degraded_seconds)
            .ok_or_else(|| disk_pressure_state_invalid("cleanup deadline overflow".to_owned()))?;
        let drain_deadline_unix = deadline_unix
            .checked_add(drain_seconds)
            .ok_or_else(|| disk_pressure_state_invalid("drain deadline overflow".to_owned()))?;
        let claim_reclaim = low
            && measurable
            && clock.identity_confirmed
            && !clock.late_binding
            && !clock.rollback
            && !clock.fingerprint_changed;
        let created = DiskPressureEpisode {
            episode_id: uuid::Uuid::new_v4().to_string(),
            started_unix,
            deadline_unix,
            drain_deadline_unix,
            draining: clock.rollback
                || clock.fingerprint_changed
                || (low && now_unix >= deadline_unix),
            last_observed_unix: now_unix.max(clock.high_water_before.unwrap_or(0)),
            reclaim_attempted: claim_reclaim,
            revision: 1,
            terminal: clock.rollback
                || clock.fingerprint_changed
                || (low && now_unix >= drain_deadline_unix),
            volume_fingerprint: clock.trusted_fingerprint.clone(),
        };
        transaction.execute(
            "INSERT INTO disk_pressure_episodes (
                 service_instance, filesystem_id, volume_fingerprint, episode_id, started_unix,
                 deadline_unix, drain_deadline_unix, last_observed_unix, reclaim_attempted,
                 revision, draining, terminal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11)",
            params![
                service_instance,
                filesystem_id,
                created.volume_fingerprint,
                created.episode_id,
                disk_pressure_sql_integer(created.started_unix, "episode start")?,
                disk_pressure_sql_integer(created.deadline_unix, "cleanup deadline")?,
                disk_pressure_sql_integer(created.drain_deadline_unix, "drain deadline")?,
                disk_pressure_sql_integer(created.last_observed_unix, "last observation")?,
                if created.reclaim_attempted {
                    1_i64
                } else {
                    0_i64
                },
                if created.draining { 1_i64 } else { 0_i64 },
                if created.terminal { 1_i64 } else { 0_i64 },
            ],
        )?;
        return Ok(DiskPressureObservation {
            episode: Some(created),
            cleared: false,
            reclaim_needed: claim_reclaim,
        });
    }
    Ok(DiskPressureObservation {
        episode,
        cleared: false,
        reclaim_needed: false,
    })
}

fn advance_controller_pressure_sample(
    transaction: &rusqlite::Transaction<'_>,
    service_instance: &str,
    sample: &DiskPressureFilesystemSample,
    degraded_seconds: i64,
    drain_seconds: i64,
    now_unix: u64,
) -> StoreResult<()> {
    let clock = persist_pressure_clock_samples(transaction, service_instance, sample, now_unix)?;
    merge_pressure_alias_episodes(transaction, service_instance, sample, &clock)?;
    let current = load_disk_pressure_episode(transaction, service_instance, &sample.filesystem_id)?;
    let low = sample.volume_fingerprint.is_none()
        || sample
            .available_bytes
            .is_none_or(|available| available < sample.min_free_bytes);
    let Some(mut current) = current else {
        if !low && !clock.rollback && !clock.fingerprint_changed {
            return Ok(());
        }
        let degraded = disk_pressure_u64(degraded_seconds, "degraded deadline")?;
        let drain = disk_pressure_u64(drain_seconds, "drain deadline")?;
        let started = if clock.rollback {
            clock.high_water_before.unwrap_or(now_unix)
        } else {
            now_unix
        };
        let deadline = started
            .checked_add(degraded)
            .ok_or_else(|| disk_pressure_state_invalid("cleanup deadline overflow".to_owned()))?;
        let end = deadline
            .checked_add(drain)
            .ok_or_else(|| disk_pressure_state_invalid("drain deadline overflow".to_owned()))?;
        let episode_id = uuid::Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO disk_pressure_episodes (
                 service_instance, filesystem_id, volume_fingerprint, episode_id, started_unix,
                 deadline_unix, drain_deadline_unix, last_observed_unix, reclaim_attempted,
                 revision, draining, terminal
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 1, ?9, ?10)",
            params![
                service_instance,
                sample.filesystem_id,
                clock.trusted_fingerprint,
                episode_id,
                disk_pressure_sql_integer(started, "episode start")?,
                disk_pressure_sql_integer(deadline, "cleanup deadline")?,
                disk_pressure_sql_integer(end, "drain deadline")?,
                disk_pressure_sql_integer(
                    now_unix.max(clock.high_water_before.unwrap_or(0)),
                    "last observation",
                )?,
                if clock.rollback || clock.fingerprint_changed || now_unix >= deadline {
                    1_i64
                } else {
                    0_i64
                },
                if clock.rollback || clock.fingerprint_changed || now_unix >= end {
                    1_i64
                } else {
                    0_i64
                },
            ],
        )?;
        return Ok(());
    };
    let rollback =
        clock.rollback || now_unix < current.last_observed_unix || now_unix < current.started_unix;
    let fingerprint_changed = match (
        current.volume_fingerprint.as_deref(),
        sample.volume_fingerprint.as_deref(),
    ) {
        (Some(previous), Some(observed)) => previous != observed,
        (Some(_), None) => false,
        (None, Some(observed)) => {
            current.volume_fingerprint = Some(observed.to_owned());
            false
        }
        (None, None) => false,
    } || clock.fingerprint_changed;
    let old_revision = current.revision;
    current.draining |=
        rollback || fingerprint_changed || (low && now_unix >= current.deadline_unix);
    current.terminal |=
        rollback || fingerprint_changed || (low && now_unix >= current.drain_deadline_unix);
    current.last_observed_unix = current
        .last_observed_unix
        .max(now_unix)
        .max(clock.high_water_before.unwrap_or(0));
    current.revision = current
        .revision
        .checked_add(1)
        .ok_or_else(|| disk_pressure_state_invalid("episode revision overflow".to_owned()))?;
    let updated = transaction.execute(
        "UPDATE disk_pressure_episodes
         SET volume_fingerprint = ?1, revision = ?2, draining = ?3, terminal = ?4,
             last_observed_unix = ?5
         WHERE service_instance = ?6 AND filesystem_id = ?7
           AND episode_id = ?8 AND revision = ?9",
        params![
            current.volume_fingerprint,
            disk_pressure_sql_integer(current.revision, "episode revision")?,
            if current.draining { 1_i64 } else { 0_i64 },
            if current.terminal { 1_i64 } else { 0_i64 },
            disk_pressure_sql_integer(current.last_observed_unix, "last observation")?,
            service_instance,
            sample.filesystem_id,
            current.episode_id,
            disk_pressure_sql_integer(old_revision, "episode revision")?,
        ],
    )?;
    if updated != 1 {
        return Err(disk_pressure_state_invalid(
            "controller pressure episode compare-and-swap missed".to_owned(),
        ));
    }
    Ok(())
}

/// Atomically turn a terminal pressure latch into a generation fence and
/// revoke every launch lease for that slot. Job/outbox ownership remains in
/// materialized state for ordinary recovery after the controller stops the
/// actor. This removes the gap where a concurrent permit or nonce issue could
/// revive the generation between pressure expiry and `SlotStale`.
fn persist_pressure_terminal_slot_fences(
    transaction: &rusqlite::Transaction<'_>,
    service_instance: &str,
) -> StoreResult<()> {
    let terminal: i64 = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM disk_pressure_episodes
             WHERE service_instance = ?1 AND terminal = 1
         )",
        [service_instance],
        |row| row.get(0),
    )?;
    if !disk_pressure_bool(terminal, "terminal pressure episode")? {
        return Ok(());
    }

    let mut state = load_materialized_state(transaction)?;
    if state.capacity_invalid {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.capacity.invalid",
        )
        .with_remediation(
            "preserve the legacy journal and refuse terminal pressure recovery until capacity state is repaired",
        ));
    }
    let slots = state.slots.clone();
    for slot in slots {
        transaction.execute(
            "UPDATE disk_pressure_launches SET active = 0
             WHERE service_instance = ?1 AND slot_id = ?2",
            params![service_instance, slot.slot_id.0],
        )?;
        if slot.phase == SlotPhase2::Fenced {
            continue;
        }
        let mut event = Event::DiskPressureTerminalFence {
            slot_id: slot.slot_id,
            generation: slot.generation,
        };
        stamp_event(&mut event);
        let outcome = reduce(state.clone(), event.clone());
        if outcome.rejected {
            return Err(disk_pressure_state_invalid(format!(
                "terminal pressure fence rejected slot generation {}",
                slot.generation.0
            )));
        }
        let payload = serde_json::to_string(&event).map_err(|error| {
            StoreError::new(velnor_model::ExitClass::Operation, "journal.encode.failed")
                .with_remediation(error.to_string())
        })?;
        transaction.execute(
            "INSERT INTO events (generation, kind, payload, checksum)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                disk_pressure_sql_integer(slot.generation.0, "terminal fence generation")?,
                event_kind(&event),
                payload,
                sha256_hex(payload.as_bytes()),
            ],
        )?;
        state = outcome.state;
    }
    persist_state(transaction, &state)?;
    Ok(())
}

type DiskPressureEpisodeSqlRow = (
    String,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
);

fn load_disk_pressure_episode(
    conn: &Connection,
    service_instance: &str,
    filesystem_id: &str,
) -> StoreResult<Option<DiskPressureEpisode>> {
    let row: Option<DiskPressureEpisodeSqlRow> = conn
        .query_row(
            "SELECT episode_id, volume_fingerprint, started_unix, deadline_unix, drain_deadline_unix,
                    last_observed_unix, reclaim_attempted, revision, draining, terminal
             FROM disk_pressure_episodes
             WHERE service_instance = ?1 AND filesystem_id = ?2",
            params![service_instance, filesystem_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(
            episode_id,
            volume_fingerprint,
            started_unix,
            deadline_unix,
            drain_deadline_unix,
            last_observed_unix,
            reclaim_attempted,
            revision,
            draining,
            terminal,
        )| {
            Ok(DiskPressureEpisode {
                episode_id,
                volume_fingerprint,
                started_unix: disk_pressure_u64(started_unix, "episode start")?,
                deadline_unix: disk_pressure_u64(deadline_unix, "cleanup deadline")?,
                drain_deadline_unix: disk_pressure_u64(drain_deadline_unix, "drain deadline")?,
                draining: disk_pressure_bool(draining, "draining")?,
                last_observed_unix: disk_pressure_u64(last_observed_unix, "last observation")?,
                reclaim_attempted: disk_pressure_bool(reclaim_attempted, "reclaim_attempted")?,
                revision: disk_pressure_u64(revision, "episode revision")?,
                terminal: disk_pressure_bool(terminal, "terminal")?,
            })
        },
    )
    .transpose()
}

fn pressure_episode_stages(conn: &Connection, service_instance: &str) -> StoreResult<(bool, bool)> {
    let mut statement = conn.prepare(
        "SELECT draining, terminal FROM disk_pressure_episodes
         WHERE service_instance = ?1",
    )?;
    let rows = statement.query_map([service_instance], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut draining = false;
    let mut terminal = false;
    for row in rows {
        let (row_draining, row_terminal) = row?;
        draining |= disk_pressure_bool(row_draining, "draining")?;
        terminal |= disk_pressure_bool(row_terminal, "terminal")?;
    }
    Ok((draining, terminal))
}

/// Every public state/overlay write must validate the current replay anchor before it can
/// delete or replace materialized rows. This closes the already-open-handle
/// case where the file is tampered with after `Journal::open` completed.
fn validate_replay_baseline_before_write(conn: &Connection) -> StoreResult<()> {
    let stored: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).ok() != Some(JOURNAL_SCHEMA_VERSION) {
        return Err(replay_baseline_write_fenced(
            u32::try_from(stored).unwrap_or(0),
        ));
    }
    require_replay_baseline_keys(conn)?;
    if load_replay_baseline(conn)?.is_none() {
        return Err(replay_baseline_missing());
    }
    Ok(())
}

fn validate_replay_integrity_before_read(conn: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).ok() != Some(JOURNAL_SCHEMA_VERSION) {
        return Err(replay_baseline_read_fenced(
            u32::try_from(stored).unwrap_or(0),
        ));
    }
    require_replay_baseline_keys(conn)?;
    if load_replay_baseline(conn)?.is_none() {
        return Err(replay_baseline_missing());
    }
    if !journal_write_fence_is_exact(conn)? {
        return Err(journal_write_fence_invalid(
            "gate schema, trigger set, or empty gate row is not exact".to_owned(),
        ));
    }
    Ok(())
}

fn load_state_from_conn_seed(conn: &Connection, mut state: FleetState) -> StoreResult<FleetState> {
    state.journal_writable = true;
    let mut stmt =
        conn.prepare("SELECT generation, kind, payload, checksum FROM events ORDER BY id ASC")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (generation, kind, payload, checksum) = row?;
        // The version gate in `open` already refused a journal newer than this
        // binary; a decoded event must still agree with its stored metadata.
        let event = decode_checked_event(generation, &kind, &payload, &checksum)?;
        let outcome = reduce(state, event);
        if outcome.rejected {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.event.rejected",
            )
            .with_remediation(
                "preserve the journal unchanged; replay rejected an event that was previously materialized",
            ));
        }
        state = outcome.state;
    }
    state.capacity_invalid =
        state.capacity_invalid || state_capacity_invalid(&state) || legacy_slots_schema(conn)?;
    Ok(state)
}

fn load_replay_baseline(conn: &Connection) -> StoreResult<Option<FleetState>> {
    let serialized: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [REPLAY_BASELINE_KEY],
            |row| row.get(0),
        )
        .optional()?;
    let checksum: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [REPLAY_BASELINE_CHECKSUM_KEY],
            |row| row.get(0),
        )
        .optional()?;
    match (serialized, checksum) {
        (None, None) => Ok(None),
        (Some(serialized), Some(checksum)) => {
            if sha256_hex(serialized.as_bytes()) != checksum {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.replay.baseline.checksum",
                )
                .with_remediation(
                    "preserve the journal unchanged; the legacy replay baseline failed integrity verification",
                ));
            }
            let baseline: ReplayBaseline = serde_json::from_str(&serialized).map_err(|error| {
                StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.replay.baseline.invalid",
                )
                .with_remediation(format!(
                    "preserve the journal unchanged; the legacy replay baseline could not be decoded: {error}"
                ))
            })?;
            if baseline.format_version != 1 {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.replay.baseline.version",
                )
                .with_remediation(
                    "preserve the journal unchanged; the replay baseline format is unsupported",
                ));
            }
            let ReplayBaseline {
                format_version: _,
                source,
                state: baseline_state,
            } = baseline;
            let state = baseline_state.into_fleet();
            if matches!(source, ReplayBaselineSource::Empty)
                && state
                    != (FleetState {
                        journal_writable: true,
                        ..FleetState::default()
                    })
            {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.replay.baseline.invalid",
                )
                .with_remediation(
                    "preserve the journal unchanged; an empty replay baseline contains semantic state",
                ));
            }
            Ok(Some(state))
        }
        _ => Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.replay.baseline.invalid",
        )
        .with_remediation(
            "preserve the journal unchanged; the legacy replay baseline is incomplete",
        )),
    }
}

fn load_state_from_conn_legacy_aware(conn: &Connection) -> StoreResult<FleetState> {
    // A pre-event migration supplies the materialized snapshot as the replay
    // origin. A v9 journal always carries an explicit baseline; the checked
    // read caller validates its presence before reaching this helper.
    let baseline = load_replay_baseline(conn)?;
    load_state_from_conn_seed(
        conn,
        baseline.unwrap_or(FleetState {
            journal_writable: true,
            ..FleetState::default()
        }),
    )
}

fn load_current_state_checked(conn: &Connection) -> StoreResult<FleetState> {
    // Replay and materialized reads must observe one SQLite snapshot. Reading
    // them through separate connections would turn a legitimate concurrent
    // commit into a false corruption report.
    let transaction = conn.unchecked_transaction()?;
    validate_replay_integrity_before_read(&transaction)?;
    let replayed = load_state_from_conn_legacy_aware(&transaction)?;
    let materialized = load_materialized_state(&transaction)?;
    // Compare even when the event table is empty. A legacy baseline is part
    // of the replay input; it is not a bypass around this check.
    let mismatch =
        canonical_projection(replayed.clone()) != canonical_projection(materialized.clone());
    if mismatch && !materialized.capacity_invalid {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.materialized.replay.mismatch",
        )
        .with_remediation(
            "preserve the journal unchanged; replayed event history differs from materialized state",
        ));
    }
    transaction.commit()?;

    // Drain and admission are meta overlays, not event projections. Return
    // replayed state for a normal journal, with those durable latches overlaid
    // from the same snapshot used for validation. A capacity-invalid
    // materialization is forensic evidence; preserve that exact state instead
    // of hiding an unlogged row behind a clean replay result.
    let materialized_capacity_invalid = materialized.capacity_invalid;
    let materialized_drain_active = materialized.drain_active;
    let materialized_drain_version = materialized.drain_version;
    let materialized_admission_blocked = materialized.admission_blocked;
    let materialized_admission_version = materialized.admission_version;
    let mut state = if materialized_capacity_invalid {
        materialized
    } else {
        replayed
    };
    state.drain_active = materialized_drain_active;
    state.drain_version = materialized_drain_version;
    state.admission_blocked = materialized_admission_blocked;
    state.admission_version = materialized_admission_version;
    Ok(state)
}

/// Stable projection used for replay validation.
///
/// The v8 reducer stamps heartbeat and outbox timing from wall clock during
/// replay, so those fields are intentionally excluded until durable event
/// timestamps can be introduced in a separate migration. Terminal outbox
/// rows are immutable evidence and are absent from materialized state; only
/// pending rows participate in the comparison. Drain/admission are meta-only
/// overlays and are copied after validation.
fn canonical_projection(mut state: FleetState) -> FleetState {
    state.drain_active = false;
    state.drain_version = 0;
    state.admission_blocked = false;
    state.admission_version = 0;
    for slot in &mut state.slots {
        slot.heartbeat_unix = 0;
    }
    for job in &mut state.jobs {
        job.accepted_unix = 0;
        job.probe_deadline_unix = 0;
    }
    for row in &mut state.outbox {
        row.created_unix = 0;
        row.deadline_unix = 0;
    }
    state.outbox.retain(OutboxRecord::is_pending);
    state.slots.sort_by(|left, right| {
        left.slot_id
            .0
            .cmp(&right.slot_id.0)
            .then_with(|| left.generation.0.cmp(&right.generation.0))
    });
    state.jobs.sort_by(|left, right| {
        left.job_id
            .0
            .cmp(&right.job_id.0)
            .then_with(|| left.generation.0.cmp(&right.generation.0))
    });
    state.outbox.sort_by(|left, right| {
        left.job_id
            .0
            .cmp(&right.job_id.0)
            .then_with(|| left.generation.0.cmp(&right.generation.0))
    });
    state
}

fn load_materialized_state(conn: &Connection) -> StoreResult<FleetState> {
    let mut state = FleetState {
        // An opened journal has passed SQLite/schema checks. Keep this field
        // consistent with the replay path; write blocking is exposed by the
        // Journal itself, not by the reducer state.
        journal_writable: true,
        ..FleetState::default()
    };

    let mut meta = HashMap::new();
    let mut statement = conn.prepare("SELECT key, value FROM meta")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (key, value) = row?;
        meta.insert(key, value);
    }
    state.control_live = meta_bool(&meta, "control_live")?;
    state.github_reachable = meta_bool(&meta, "github_reachable")?;
    state.routing_valid = meta_bool(&meta, "routing_valid")?;
    state.runner_group_valid = meta_bool(&meta, "runner_group_valid")?;
    state.desired_ready = meta_u32(&meta, "desired_ready")?;
    // `desired_ready` is serialized for every materialized snapshot, including
    // the reducer's initial zero. Only the explicit marker written after a
    // DesiredCapacity event is authoritative.
    state.capacity_declared = meta_bool(&meta, "capacity_declared")?;
    state.canary = meta_canary(&meta)?;
    state.package_generation = meta_u64(&meta, "package_generation")?;
    state.package_apt_version = meta.get("package_apt_version").cloned().unwrap_or_default();
    state.capacity_invalid = meta_bool(&meta, "capacity_invalid")?;
    // Absent on every journal written before drain unification: not draining.
    // Unknown sibling keys are ignored by construction (each key is read by
    // name), but a malformed `drain` value fails closed like any other
    // materialized field: it is evidence from a writer this binary cannot
    // interpret, not an instruction to run.
    match meta.get("drain") {
        None => {
            state.drain_active = false;
            state.drain_version = 0;
        }
        Some(value) => match parse_drain_value(value) {
            Some(drain) => {
                state.drain_active = true;
                state.drain_version = drain.version;
            }
            None => return Err(invalid_materialized("drain", value)),
        },
    }
    // Absent on journals written before soft admission fencing. A malformed
    // marker is a materialized-state error: controller callers must not
    // accept work when the durable admission decision cannot be decoded.
    match meta.get("admission") {
        None => {
            state.admission_blocked = false;
            state.admission_version = 0;
        }
        Some(value) => match parse_admission_value(value) {
            Some(admission) => {
                state.admission_blocked = true;
                state.admission_version = admission.version;
            }
            None => return Err(invalid_materialized("admission", value)),
        },
    }

    let mut statement = conn.prepare(
        "SELECT slot_id, generation, phase, permit_held, routing_valid,
                session_live, executor_proven, registered, pid, heartbeat_unix
         FROM slots ORDER BY rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, Option<i64>>(8)?,
            row.get::<_, i64>(9)?,
        ))
    })?;
    for row in rows {
        let (
            slot_id,
            generation,
            phase,
            permit_held,
            routing_valid,
            session_live,
            executor_proven,
            registered,
            pid,
            heartbeat_unix,
        ) = row?;
        state.slots.push(SlotRecord {
            slot_id: SlotId(slot_id),
            generation: Generation(i64_u64(generation, "slot generation")?),
            phase: parse_slot_phase(&phase)?,
            permit_held: sqlite_bool(permit_held, "slot permit_held")?,
            routing_valid: sqlite_bool(routing_valid, "slot routing_valid")?,
            session_live: sqlite_bool(session_live, "slot session_live")?,
            executor_proven: sqlite_bool(executor_proven, "slot executor_proven")?,
            registered: sqlite_bool(registered, "slot registered")?,
            pid: pid.map(|value| i64_u32(value, "slot pid")).transpose()?,
            heartbeat_unix: i64_u64(heartbeat_unix, "slot heartbeat_unix")?,
        });
    }

    state.capacity_invalid =
        state.capacity_invalid || state_capacity_invalid(&state) || legacy_slots_schema(conn)?;

    let mut statement = conn.prepare(
        "SELECT job_id, slot_id, generation, attempt, worker, phase, accepted_unix,
                terminal_conclusion, provisional, plan_id, run_service_url,
                probe_attempts, probe_deadline_unix
         FROM jobs ORDER BY rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, i64>(8)? != 0,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, i64>(11)?,
            row.get::<_, i64>(12)?,
        ))
    })?;
    for row in rows {
        let (
            job_id,
            slot_id,
            generation,
            attempt,
            worker,
            phase,
            accepted_unix,
            terminal_conclusion,
            provisional,
            plan_id,
            run_service_url,
            probe_attempts,
            probe_deadline_unix,
        ) = row?;
        state.jobs.push(JobRecord {
            job_id: JobId(job_id),
            slot_id: SlotId(slot_id),
            generation: Generation(i64_u64(generation, "job generation")?),
            attempt: i64_u32(attempt, "job attempt")?,
            worker,
            phase: parse_job_phase(&phase)?,
            accepted_unix: i64_u64(accepted_unix, "job accepted_unix")?,
            terminal_conclusion,
            provisional,
            plan_id,
            run_service_url,
            probe_attempts: i64_u32(probe_attempts, "job probe_attempts")?,
            probe_deadline_unix: i64_u64(probe_deadline_unix, "job probe_deadline_unix")?,
        });
    }

    let mut statement = conn.prepare(
        "SELECT job_id, slot_id, generation, payload_sha256, intended, send_started,
                remote_acked, created_unix, attempts, deadline_unix, permanent, abandoned
         FROM outbox ORDER BY rowid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, i64>(9)?,
            row.get::<_, i64>(10)?,
            row.get::<_, i64>(11)?,
        ))
    })?;
    for row in rows {
        let (
            job_id,
            slot_id,
            generation,
            payload_sha256,
            intended,
            send_started,
            remote_acked,
            created_unix,
            attempts,
            deadline_unix,
            permanent,
            abandoned,
        ) = row?;
        let slot_id = slot_id.ok_or_else(|| outbox_owner_unknown(&job_id, generation))?;
        state.outbox.push(OutboxRecord {
            job_id: JobId(job_id),
            slot_id: SlotId(slot_id),
            generation: Generation(i64_u64(generation, "outbox generation")?),
            payload_sha256,
            intended: sqlite_bool(intended, "outbox intended")?,
            send_started: sqlite_bool(send_started, "outbox send_started")?,
            remote_acked: sqlite_bool(remote_acked, "outbox remote_acked")?,
            created_unix: i64_u64(created_unix, "outbox created_unix")?,
            attempts: i64_u32(attempts, "outbox attempts")?,
            deadline_unix: i64_u64(deadline_unix, "outbox deadline_unix")?,
            permanent: sqlite_bool(permanent, "outbox permanent")?,
            abandoned: sqlite_bool(abandoned, "outbox abandoned")?,
        });
    }
    Ok(state)
}

fn meta_bool(meta: &HashMap<String, String>, key: &str) -> StoreResult<bool> {
    meta.get(key)
        .map(|value| match value.as_str() {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(invalid_materialized(key, value)),
        })
        .unwrap_or(Ok(false))
}

fn meta_u32(meta: &HashMap<String, String>, key: &str) -> StoreResult<u32> {
    meta.get(key)
        .map(|value| value.parse().map_err(|_| invalid_materialized(key, value)))
        .unwrap_or(Ok(0))
}

fn meta_u64(meta: &HashMap<String, String>, key: &str) -> StoreResult<u64> {
    meta.get(key)
        .map(|value| value.parse().map_err(|_| invalid_materialized(key, value)))
        .unwrap_or(Ok(0))
}

fn meta_canary(meta: &HashMap<String, String>) -> StoreResult<CanaryStatus> {
    match meta.get("canary").map(String::as_str) {
        None | Some("unknown") => Ok(CanaryStatus::Unknown),
        Some("passing") => Ok(CanaryStatus::Passing),
        Some("failing") => Ok(CanaryStatus::Failing),
        Some("timeout") => Ok(CanaryStatus::Timeout),
        Some(value) => Err(invalid_materialized("canary", value)),
    }
}

fn parse_slot_phase(value: &str) -> StoreResult<SlotPhase2> {
    match value {
        "absent" => Ok(SlotPhase2::Absent),
        "provisioning" => Ok(SlotPhase2::Provisioning),
        "registered" => Ok(SlotPhase2::Registered),
        "ready" => Ok(SlotPhase2::Ready),
        "assigned" => Ok(SlotPhase2::Assigned),
        "fenced" => Ok(SlotPhase2::Fenced),
        // The retired phases (`starting`, `retiring`, `degraded`,
        // `quarantined`) and the job-only phases (`running`, `completing`)
        // fail closed here: a slot row naming one is evidence from a
        // vocabulary this binary no longer speaks for slots, and guessing
        // which live phase it meant would invent capacity state.
        _ => Err(invalid_materialized("slot phase", value)),
    }
}

fn parse_job_phase(value: &str) -> StoreResult<JobPhase2> {
    match value {
        "assigned" => Ok(JobPhase2::Assigned),
        "running" => Ok(JobPhase2::Running),
        "completing" => Ok(JobPhase2::Completing),
        // Slot-only phases on a job row are the mirror image of the slot
        // case above: fail closed rather than invent execution state.
        _ => Err(invalid_materialized("job phase", value)),
    }
}

fn sqlite_bool(value: i64, field: &str) -> StoreResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid_materialized(field, &value.to_string())),
    }
}

fn i64_u32(value: i64, field: &str) -> StoreResult<u32> {
    u32::try_from(value).map_err(|_| invalid_materialized(field, &value.to_string()))
}

fn i64_u64(value: i64, field: &str) -> StoreResult<u64> {
    u64::try_from(value).map_err(|_| invalid_materialized(field, &value.to_string()))
}

fn invalid_materialized(field: &str, value: &str) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.materialized.invalid",
    )
    .with_remediation(format!(
        "materialized field {field} has invalid value {value}"
    ))
}

fn outbox_owner_unknown(job_id: &str, generation: i64) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.outbox.owner.unknown",
    )
    .with_remediation(format!(
        "outbox row for job {job_id} generation {generation} has no exact slot owner; preserve it and recover the matching job before retrying"
    ))
}

fn persist_state(tx: &rusqlite::Transaction<'_>, state: &FleetState) -> StoreResult<()> {
    // Validate the anchor before deleting any materialized row. The caller
    // performs the same check before reducing, and this defense keeps the
    // persistence primitive safe if another writer invokes it later.
    if load_replay_baseline(tx)?.is_none() {
        return Err(replay_baseline_missing());
    }
    tx.execute("DELETE FROM slots", [])?;
    tx.execute("DELETE FROM jobs", [])?;
    tx.execute("DELETE FROM outbox", [])?;
    // Keep the anchor rows in place. Besides avoiding a replace-trigger on the
    // v9 fence, this makes an old v8 `DELETE FROM meta` observably fail while
    // allowing current writers to refresh the ordinary materialized keys.
    tx.execute(
        "DELETE FROM meta WHERE key NOT IN (?1, ?2)",
        params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
    )?;
    for slot in &state.slots {
        tx.execute(
            "INSERT INTO slots (
                slot_id, generation, phase, permit_held, routing_valid, session_live,
                executor_proven, registered, pid, heartbeat_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                slot.slot_id.0,
                slot.generation.0 as i64,
                slot.phase.as_str(),
                slot.permit_held as i64,
                slot.routing_valid as i64,
                slot.session_live as i64,
                slot.executor_proven as i64,
                slot.registered as i64,
                slot.pid.map(i64::from),
                slot.heartbeat_unix as i64,
            ],
        )?;
    }
    for job in &state.jobs {
        tx.execute(
            "INSERT INTO jobs (
                job_id, slot_id, generation, attempt, worker, phase, accepted_unix,
                terminal_conclusion, provisional, plan_id, run_service_url,
                probe_attempts, probe_deadline_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                job.job_id.0,
                job.slot_id.0,
                job.generation.0 as i64,
                job.attempt as i64,
                job.worker,
                job.phase.as_str(),
                job.accepted_unix as i64,
                job.terminal_conclusion.as_deref(),
                job.provisional as i64,
                job.plan_id,
                job.run_service_url,
                job.probe_attempts as i64,
                job.probe_deadline_unix as i64,
            ],
        )?;
    }
    for row in &state.outbox {
        // Acknowledged and abandoned rows are both terminal: the immutable
        // event log keeps their evidence, and dropping them here is what lets
        // GitHub redeliver the same job id on a later attempt.
        if row.remote_acked || row.abandoned {
            continue;
        }
        tx.execute(
            "INSERT INTO outbox (
                job_id, slot_id, generation, payload_sha256, intended, send_started,
                remote_acked, created_unix, attempts, deadline_unix, permanent, abandoned
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                row.job_id.0,
                row.slot_id.0,
                row.generation.0 as i64,
                row.payload_sha256,
                row.intended as i64,
                row.send_started as i64,
                row.remote_acked as i64,
                row.created_unix as i64,
                row.attempts as i64,
                row.deadline_unix as i64,
                row.permanent as i64,
                row.abandoned as i64,
            ],
        )?;
    }
    let meta = [
        ("control_live", (state.control_live as u8).to_string()),
        (
            "github_reachable",
            (state.github_reachable as u8).to_string(),
        ),
        ("routing_valid", (state.routing_valid as u8).to_string()),
        (
            "runner_group_valid",
            (state.runner_group_valid as u8).to_string(),
        ),
        ("desired_ready", state.desired_ready.to_string()),
        ("canary", state.canary.as_str().to_owned()),
        ("package_generation", state.package_generation.to_string()),
        ("package_apt_version", state.package_apt_version.clone()),
        (
            "capacity_invalid",
            (state.capacity_invalid as u8).to_string(),
        ),
        (
            "capacity_declared",
            (state.capacity_declared as u8).to_string(),
        ),
    ];
    // Every `apply` loads the materialized state (drain and admission
    // included) under the same immediate transaction it persists under, so
    // Re-emitting both lifecycle markers from state keeps them sticky across
    // unrelated event writes with no new event and no schema bump. Absent when
    // inactive, so old journals keep their exact meta shape.
    //
    // Mixed-version warning: this rewrite drops every `meta` key it does
    // not know. The v9 baseline-delete trigger prevents a pre-v9 writer from
    // completing this rewrite; binaries that understand v9 but do not know
    // newer lifecycle overlays remain read-only with respect to those keys.
    let drain = state
        .drain_active
        .then(|| ("drain", format!("requested:{}", state.drain_version)));
    let admission = state
        .admission_blocked
        .then(|| ("admission", format!("blocked:{}", state.admission_version)));
    for (key, value) in meta.into_iter().chain(drain).chain(admission) {
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboxSchema {
    Missing,
    V2,
    V3,
    V4,
}

/// Ordering of physical outbox shapes, oldest first.
fn outbox_shape_rank(shape: OutboxSchema) -> u32 {
    match shape {
        // A missing table is created by `SCHEMA` at the current shape, so it
        // never counts as ahead of any recorded version.
        OutboxSchema::Missing => 0,
        OutboxSchema::V2 => 2,
        OutboxSchema::V3 => 3,
        OutboxSchema::V4 => 4,
    }
}

/// Highest physical outbox shape a recorded `PRAGMA user_version` may carry.
fn version_outbox_rank(version: u32) -> u32 {
    match version {
        // A brand new file has no recorded version and no rows to misread.
        0 => JOURNAL_SCHEMA_VERSION,
        other => other,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct OutboxIndexShape {
    unique: bool,
    columns: Vec<String>,
}

fn outbox_schema_shape(conn: &Connection) -> StoreResult<OutboxSchema> {
    let mut statement = conn.prepare("PRAGMA table_info(outbox)")?;
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if columns.is_empty() {
        return Ok(OutboxSchema::Missing);
    }

    let v4_columns = [
        ("job_id", "TEXT", 0, None, 1),
        ("slot_id", "TEXT", 1, None, 0),
        ("generation", "INTEGER", 1, None, 0),
        ("payload_sha256", "TEXT", 1, None, 0),
        ("intended", "INTEGER", 1, Some("0"), 0),
        ("send_started", "INTEGER", 1, Some("0"), 0),
        ("remote_acked", "INTEGER", 1, Some("0"), 0),
        ("created_unix", "INTEGER", 1, None, 0),
        ("attempts", "INTEGER", 1, Some("0"), 0),
        ("deadline_unix", "INTEGER", 1, Some("0"), 0),
        ("permanent", "INTEGER", 1, Some("0"), 0),
        ("abandoned", "INTEGER", 1, Some("0"), 0),
    ];
    let v3_columns = [
        ("job_id", "TEXT", 0, None, 1),
        ("slot_id", "TEXT", 1, None, 0),
        ("generation", "INTEGER", 1, None, 0),
        ("payload_sha256", "TEXT", 1, None, 0),
        ("intended", "INTEGER", 1, Some("0"), 0),
        ("send_started", "INTEGER", 1, Some("0"), 0),
        ("remote_acked", "INTEGER", 1, Some("0"), 0),
        ("created_unix", "INTEGER", 1, None, 0),
    ];
    let v2_columns = [
        ("job_id", "TEXT", 0, None, 1),
        ("generation", "INTEGER", 1, None, 0),
        ("payload_sha256", "TEXT", 1, None, 0),
        ("intended", "INTEGER", 1, Some("0"), 0),
        ("send_started", "INTEGER", 1, Some("0"), 0),
        ("remote_acked", "INTEGER", 1, Some("0"), 0),
        ("created_unix", "INTEGER", 1, None, 0),
    ];
    let matches_columns = |expected: &[(&str, &str, i64, Option<&str>, i64)]| {
        columns.len() == expected.len()
            && columns.iter().zip(expected).all(
                |(
                    (name, ty, not_null, default, pk),
                    (expected_name, expected_ty, expected_not_null, expected_default, expected_pk),
                )| {
                    name.eq_ignore_ascii_case(expected_name)
                        && ty.eq_ignore_ascii_case(expected_ty)
                        && *not_null == *expected_not_null
                        && default.as_deref() == *expected_default
                        && *pk == *expected_pk
                },
            )
    };
    let mut statement = conn.prepare("PRAGMA index_list(outbox)")?;
    let indexes = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)? == 1))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut index_shapes = Vec::with_capacity(indexes.len());
    for (name, unique) in indexes {
        let mut info = conn.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
        let columns = info
            .query_map([name], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        index_shapes.push(OutboxIndexShape { unique, columns });
    }
    let canonical_indexes = [OutboxIndexShape {
        unique: true,
        columns: vec!["job_id".to_owned()],
    }];
    if index_shapes != canonical_indexes {
        return Err(outbox_schema_invalid("index set"));
    }
    if matches_columns(&v4_columns) {
        Ok(OutboxSchema::V4)
    } else if matches_columns(&v3_columns) {
        Ok(OutboxSchema::V3)
    } else if matches_columns(&v2_columns) {
        Ok(OutboxSchema::V2)
    } else {
        Err(outbox_schema_invalid("column set"))
    }
}

fn outbox_schema_invalid(part: &str) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.outbox.schema.invalid",
    )
    .with_remediation(format!(
        "preserve the outbox and repair its canonical v3 {part} before reopening the journal"
    ))
}

fn journal_schema_newer() -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.newer")
        .with_remediation(
            "preserve the journal unchanged and reopen it with a binary that supports its PRAGMA user_version",
        )
}

fn replay_baseline_missing() -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.replay.baseline.missing",
    )
    .with_remediation(
        "preserve the journal unchanged and reopen it with the migration that writes both replay baseline keys",
    )
}

fn replay_baseline_provenance(version: u32, shape: OutboxSchema) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.replay.baseline.provenance",
    )
    .with_remediation(format!(
        "preserve the eventless journal unchanged: PRAGMA user_version={version} with physical outbox shape {shape:?} has no trusted replay origin"
    ))
}

fn replay_baseline_write_fenced(version: u32) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.replay.baseline.fenced",
    )
    .with_remediation(format!(
        "preserve the journal unchanged: state writes require schema v{JOURNAL_SCHEMA_VERSION} with a valid replay baseline, found PRAGMA user_version={version}"
    ))
}

fn replay_baseline_read_fenced(version: u32) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.replay.baseline.fenced",
    )
    .with_remediation(format!(
        "preserve the journal unchanged: checked reads require schema v{JOURNAL_SCHEMA_VERSION} with a valid replay baseline and write fence, found PRAGMA user_version={version}"
    ))
}

fn journal_write_fence_invalid(detail: String) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.write.fence.invalid",
    )
    .with_remediation(format!(
        "preserve the journal unchanged; the v11 write-fence schema is invalid: {detail}"
    ))
}

fn outbox_schema_mismatch(version: u32, shape: OutboxSchema) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
        .with_remediation(format!(
            "preserve the journal unchanged: PRAGMA user_version={version} is incompatible with physical outbox shape {shape:?}"
        ))
}

/// Upgrade a v2 materialized outbox without inventing ownership. Every row is
/// backfilled from exactly one matching job and exactly one matching slot into
/// a rebuilt table whose `slot_id` is NOT NULL. Owner mismatches fail before
/// any schema mutation; the transaction is retryable if the process dies
/// mid-upgrade.
fn migrate_v2_to_v3(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let inconsistent_owner: Option<(String, i64)> = tx
        .query_row(
            "SELECT outbox.job_id, outbox.generation
             FROM outbox
             WHERE (SELECT COUNT(*)
                    FROM jobs
                    WHERE jobs.job_id = outbox.job_id
                      AND jobs.generation = outbox.generation) != 1
                OR (SELECT COUNT(*)
                    FROM jobs
                    JOIN slots
                      ON slots.slot_id = jobs.slot_id
                     AND slots.generation = jobs.generation
                    WHERE jobs.job_id = outbox.job_id
                      AND jobs.generation = outbox.generation) != 1
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((job_id, generation)) = inconsistent_owner {
        return Err(outbox_owner_inconsistent(&job_id, generation));
    }
    tx.execute_batch(
        "CREATE TABLE outbox_v3 (
             job_id TEXT PRIMARY KEY,
             slot_id TEXT NOT NULL,
             generation INTEGER NOT NULL,
             payload_sha256 TEXT NOT NULL,
             intended INTEGER NOT NULL DEFAULT 0,
             send_started INTEGER NOT NULL DEFAULT 0,
             remote_acked INTEGER NOT NULL DEFAULT 0,
             created_unix INTEGER NOT NULL
         );
         INSERT INTO outbox_v3 (
             job_id, slot_id, generation, payload_sha256, intended,
             send_started, remote_acked, created_unix
         )
         SELECT outbox.job_id, jobs.slot_id, outbox.generation,
                outbox.payload_sha256, outbox.intended, outbox.send_started,
                outbox.remote_acked, outbox.created_unix
         FROM outbox
         JOIN jobs
           ON jobs.job_id = outbox.job_id
          AND jobs.generation = outbox.generation
         JOIN slots
           ON slots.slot_id = jobs.slot_id
          AND slots.generation = jobs.generation;
         DROP TABLE outbox;
         ALTER TABLE outbox_v3 RENAME TO outbox;",
    )?;
    // Stamp exactly v3: `migrate_v3_to_v4` runs next and owns the final
    // version. Stamping the current version here would make that step
    // early-return and leave a v3 shape claiming to be v4.
    tx.pragma_update(None, "user_version", 3u32)?;
    Ok(())
}

/// Add the bounded-terminal-state columns and stamp schema v4.
///
/// Idempotent and retryable: a crash mid-upgrade leaves either the old shape
/// or the new one, never a partially stamped version. Existing pending rows
/// inherit a deadline measured from when their intent became durable, so an
/// upgrade cannot silently extend a completion's budget to infinity.
fn migrate_v3_to_v4(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    // Gate and stamp on *this* step's own target, never the symbolic current
    // version. When v5 was added, this comparison against
    // `JOURNAL_SCHEMA_VERSION` silently became "skip if already 5" while the
    // stamp below became "claim 5" — so a v3 file was upgraded to the v4 shape,
    // stamped 5, and the v5 step then early-returned without adding its column.
    // Every existing journal was left claiming a shape it did not have, and no
    // older binary could open it either.
    if u32::try_from(stored).unwrap_or(0) >= 4 {
        return Ok(());
    }
    if !table_has_column(tx, "outbox", "attempts")? {
        tx.execute_batch(
            "ALTER TABLE outbox ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE outbox ADD COLUMN deadline_unix INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE outbox ADD COLUMN permanent INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE outbox ADD COLUMN abandoned INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    tx.execute(
        "UPDATE outbox SET deadline_unix = created_unix + ?1 WHERE deadline_unix = 0",
        params![COMPLETION_RESOLUTION_SECONDS as i64],
    )?;
    if !table_has_column(tx, "jobs", "terminal_conclusion")? {
        tx.execute_batch("ALTER TABLE jobs ADD COLUMN terminal_conclusion TEXT;")?;
    }
    // Stamp exactly v4: `migrate_v4_to_v5` runs next and owns the final version.
    tx.pragma_update(None, "user_version", 4u32)?;
    Ok(())
}

/// v5 adds the provisional bit to `jobs`.
///
/// Existing rows migrate to `provisional = 0`: every row written before this
/// version came from a `JobOwned` event, which only follows a 200 from
/// `acquirejob`, so they are all genuine ownership and must keep proving it.
fn migrate_v4_to_v5(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 5 {
        return Ok(());
    }
    if !table_has_column(tx, "jobs", "provisional")? {
        tx.execute_batch("ALTER TABLE jobs ADD COLUMN provisional INTEGER NOT NULL DEFAULT 0;")?;
    }
    // Always stamp, even when the column was already present. Detecting a shape
    // that ran ahead of its stamp is `Journal::open`'s job and is scoped there
    // to the one transition where it is evidence of tampering; refusing to
    // stamp *here* instead left legitimate upgrades from v2 and v3 stuck at 4
    // with the v5 column in place — the file then materialized fine but claimed
    // the wrong version forever.
    tx.pragma_update(None, "user_version", 5u32)?;
    Ok(())
}

/// v6 gives a provisional acquisition the addressing and the bounded budget its
/// recovery needs: the plan and run-service URL `renewjob` is called with, and
/// the durable probe counter and deadline that stop this node renewing a lease
/// it can never make progress on.
///
/// Existing rows migrate to empty addressing and a zero budget. Every row
/// written before this bump is either owned or absent, never provisional, so
/// nothing probes them and the zeros are never read.
fn migrate_v5_to_v6(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    // Gate on this step's own target, never `JOURNAL_SCHEMA_VERSION`.
    if u32::try_from(stored).unwrap_or(0) >= 6 {
        return Ok(());
    }
    if !table_has_column(tx, "jobs", "plan_id")? {
        tx.execute_batch("ALTER TABLE jobs ADD COLUMN plan_id TEXT NOT NULL DEFAULT '';")?;
    }
    if !table_has_column(tx, "jobs", "run_service_url")? {
        tx.execute_batch("ALTER TABLE jobs ADD COLUMN run_service_url TEXT NOT NULL DEFAULT '';")?;
    }
    if !table_has_column(tx, "jobs", "probe_attempts")? {
        tx.execute_batch("ALTER TABLE jobs ADD COLUMN probe_attempts INTEGER NOT NULL DEFAULT 0;")?;
    }
    if !table_has_column(tx, "jobs", "probe_deadline_unix")? {
        tx.execute_batch(
            "ALTER TABLE jobs ADD COLUMN probe_deadline_unix INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    // Stamp exactly 6, and stamp even when the columns were already present:
    // refusing to stamp there is what left legitimate upgrades claiming an
    // older version than the shape they carry.
    tx.pragma_update(None, "user_version", 6u32)?;
    Ok(())
}

/// v7 adds the terminal local-payload-loss event vocabulary. No materialized
/// table changed, but older writers must refuse journals that may contain it.
fn migrate_v6_to_v7(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 7 {
        return Ok(());
    }
    tx.pragma_update(None, "user_version", 7u32)?;
    Ok(())
}

/// v8 splits the shared actor-phase vocabulary into slot and job phases.
/// The reducer never wrote `running`/`completing` to a slot row or a
/// slot-only phase to a job row, so conforming rows migrate untouched; a row
/// outside its new vocabulary is foreign evidence and fails closed here,
/// inside the setup transaction, before the version stamp can move.
fn migrate_v7_to_v8(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 8 {
        return Ok(());
    }
    let mut statement = tx.prepare("SELECT phase FROM slots")?;
    let phases = statement.query_map([], |row| row.get::<_, String>(0))?;
    for phase in phases {
        parse_slot_phase(&phase?)?;
    }
    let mut statement = tx.prepare("SELECT phase FROM jobs")?;
    let phases = statement.query_map([], |row| row.get::<_, String>(0))?;
    for phase in phases {
        parse_job_phase(&phase?)?;
    }
    tx.pragma_update(None, "user_version", 8u32)?;
    Ok(())
}

/// v9 installs a replay anchor and fences every older writer. A journal that
/// already has event history starts from the reducer's empty state; an
/// eventless legacy materialization starts from its explicit snapshot. Older
/// v8 writers did not persist generation metadata for acquisition events, so
/// that narrow, derivable backfill happens in this same transaction before
/// the v9 stamp becomes visible.
fn migrate_v8_to_v9(tx: &rusqlite::Transaction<'_>, legacy_eventless: bool) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 9 {
        return Ok(());
    }
    backfill_v8_acquisition_generations(tx)?;
    install_replay_baseline(tx, legacy_eventless)?;
    validate_replay_against_materialized(tx)?;
    tx.pragma_update(None, "user_version", 9u32)?;
    Ok(())
}

/// v10 adds durable pressure episodes, launch fences, and both pressure
/// deadlines. Their DDL is installed by `SCHEMA` before this stamp.
fn migrate_v9_to_v10(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 10 {
        return Ok(());
    }
    if stored != 9 {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.schema.mismatch",
        )
        .with_remediation(format!(
            "preserve the journal unchanged; expected schema 9 before pressure migration, found {stored}"
        )));
    }
    tx.pragma_update(None, "user_version", 10u32)?;
    Ok(())
}

/// v11 binds the fleet journal to one stable service identity. The table is
/// installed by `SCHEMA`; this stamp makes every v10 writer refuse the file
/// before it can ignore the identity or alter globally keyed fleet state.
fn migrate_v10_to_v11(
    tx: &rusqlite::Transaction<'_>,
    requested_service_instance: Option<&str>,
) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 11 {
        return Ok(());
    }
    if stored != 10 {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.schema.mismatch",
        )
        .with_remediation(format!(
            "preserve the journal unchanged; expected schema 10 before service identity migration, found {stored}"
        )));
    }
    if !journal_identity_table_is_exact(tx)? {
        return Err(journal_schema_shape_mismatch(
            "schema-10 migration did not create the canonical service identity table",
        ));
    }
    let identity_rows: i64 = tx.query_row("SELECT COUNT(*) FROM journal_identity", [], |row| {
        row.get(0)
    })?;
    if identity_rows != 0 {
        return Err(journal_schema_shape_mismatch(
            "schema-10 migration found a pre-existing service identity row",
        ));
    }
    // SCHEMA and every earlier migration in this setup transaction roll back
    // with this error, leaving nonempty legacy history available to the
    // explicit service-bound migration path.
    if requested_service_instance.is_none() && !journal_is_provably_new_and_empty(tx)? {
        return Err(journal_service_instance_mismatch(
            "a nonempty legacy journal needs an explicit service instance during schema-11 migration; reopen it with open_for_service_instance",
        ));
    }
    if let Some(service_instance) = requested_service_instance {
        validate_disk_pressure_key(service_instance, "service instance")?;
        validate_journal_service_instance(tx, service_instance)?;
        tx.execute(
            "INSERT INTO journal_identity (id, service_instance) VALUES (1, ?1)",
            [service_instance],
        )?;
    }
    tx.pragma_update(None, "user_version", 11u32)?;
    Ok(())
}

/// Validate the complete v8 event log against its materialized projection
/// before the migration stamps v9. Keeping this inside the setup transaction
/// means checksum, decode, reducer, and projection failures roll back the
/// baseline, generation backfill, and version stamp together.
fn validate_replay_against_materialized(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let replayed = load_state_from_conn_legacy_aware(tx)?;
    let materialized = load_materialized_state(tx)?;
    if canonical_projection(replayed) != canonical_projection(materialized.clone())
        && !materialized.capacity_invalid
    {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.materialized.replay.mismatch",
        )
        .with_remediation(
            "preserve the journal unchanged; v8 replay differs from materialized state during v9 migration",
        ));
    }
    Ok(())
}

/// Backfill only the metadata v8 omitted for acquisition events. The payload
/// remains the source of truth: a zero generation is repaired, a nonzero
/// disagreement is corruption, and every failure rolls back with the schema
/// migration. No other event kind is rewritten here.
fn backfill_v8_acquisition_generations(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let mut statement = tx.prepare(
        "SELECT id, generation, kind, payload, checksum
         FROM events
         WHERE kind IN (
             'job_acquisition_intended',
             'job_acquisition_resolved',
             'acquisition_probe_failed',
             'job_acquisition_lost'
         )
         ORDER BY id ASC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (id, generation, kind, payload, checksum) in rows {
        if sha256_hex(payload.as_bytes()) != checksum {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.checksum.mismatch",
            )
            .with_remediation(
                "preserve the journal unchanged; an acquisition event failed checksum verification during v9 migration",
            ));
        }
        let event: Event = serde_json::from_str(&payload).map_err(|error| {
            StoreError::new(velnor_model::ExitClass::Conflict, "journal.event.unknown")
                .with_remediation(format!(
                    "preserve the journal unchanged; an acquisition event could not be decoded during v9 migration: {error}"
                ))
        })?;
        if kind != event_kind(&event) {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.event.metadata.mismatch",
            )
            .with_remediation(
                "preserve the journal unchanged; acquisition event kind does not match its payload during v9 migration",
            ));
        }
        let expected = event_generation(&event).0;
        if expected == 0 {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.event.metadata.mismatch",
            )
            .with_remediation(
                "preserve the journal unchanged; acquisition event payload has no derivable generation during v9 migration",
            ));
        }
        let stored_generation = u64::try_from(generation).map_err(|_| {
            StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.event.metadata.mismatch",
            )
            .with_remediation(
                "preserve the journal unchanged; acquisition event generation is negative during v9 migration",
            )
        })?;
        if stored_generation == 0 {
            let expected_sql = i64::try_from(expected).map_err(|_| {
                StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.metadata.mismatch",
                )
                .with_remediation(
                    "preserve the journal unchanged; acquisition event generation exceeds SQLite's integer range",
                )
            })?;
            tx.execute(
                "UPDATE events SET generation = ?1 WHERE id = ?2 AND generation = 0",
                params![expected_sql, id],
            )?;
        } else if stored_generation != expected {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.event.metadata.mismatch",
            )
            .with_remediation(
                "preserve the journal unchanged; acquisition event generation disagrees with its payload during v9 migration",
            ));
        }
    }
    Ok(())
}

/// Repair the one historical migration poison that can pass the version gate.
///
/// The v5 bump once stamped a v4 `jobs` table as version 5. A later opener
/// therefore skipped the v4-to-v5 migration and, when v6 was introduced,
/// advanced the same table to version 6 without ever adding `provisional`.
/// `Journal::open` used to accept both states and only fail later when
/// materialization selected that missing column.
///
/// This is deliberately narrower than making migrations infer arbitrary
/// partial schemas. Only the exact v4 shape at stamp 5, or that same shape
/// plus all v6 columns at stamp 6, is repairable. Anything else remains
/// forensic evidence and fails in the transaction before its version stamp can
/// change.
fn repair_historic_jobs_shape(tx: &rusqlite::Transaction<'_>, stored: u32) -> StoreResult<()> {
    if !matches!(stored, 5 | 6) || table_has_column(tx, "jobs", "provisional")? {
        return Ok(());
    }

    let mut expected = vec![
        ("job_id", "TEXT", 0, None, 1),
        ("slot_id", "TEXT", 1, None, 0),
        ("generation", "INTEGER", 1, None, 0),
        ("attempt", "INTEGER", 1, None, 0),
        ("worker", "TEXT", 1, None, 0),
        ("phase", "TEXT", 1, None, 0),
        ("accepted_unix", "INTEGER", 1, Some("0"), 0),
        ("terminal_conclusion", "TEXT", 0, None, 0),
    ];
    if stored == 6 {
        expected.extend([
            ("plan_id", "TEXT", 1, Some("''"), 0),
            ("run_service_url", "TEXT", 1, Some("''"), 0),
            ("probe_attempts", "INTEGER", 1, Some("0"), 0),
            ("probe_deadline_unix", "INTEGER", 1, Some("0"), 0),
        ]);
    }

    let columns = job_schema_columns(tx)?;
    let matches = columns.len() == expected.len()
        && columns.iter().zip(expected).all(
            |(
                (name, ty, not_null, default, pk),
                (expected_name, expected_ty, expected_not_null, expected_default, expected_pk),
            )| {
                name.eq_ignore_ascii_case(expected_name)
                    && ty.eq_ignore_ascii_case(expected_ty)
                    && *not_null == expected_not_null
                    && default.as_deref() == expected_default
                    && *pk == expected_pk
            },
        );
    if !matches {
        return Err(jobs_schema_mismatch(stored));
    }

    tx.execute_batch("ALTER TABLE jobs ADD COLUMN provisional INTEGER NOT NULL DEFAULT 0;")?;
    Ok(())
}

/// One `PRAGMA table_info` row: name, type, not-null, default, primary-key flag.
type TableColumn = (String, String, i64, Option<String>, i64);

fn job_schema_columns(conn: &Connection) -> StoreResult<Vec<TableColumn>> {
    let mut statement = conn.prepare("PRAGMA table_info(jobs)")?;
    Ok(statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> StoreResult<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns.any(|name| {
        name.map(|name| name.eq_ignore_ascii_case(column))
            .unwrap_or(false)
    }))
}

fn jobs_schema_mismatch(version: u32) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
        .with_remediation(format!(
            "preserve the journal unchanged: PRAGMA user_version={version} carries an unrecognized jobs shape"
        ))
}

fn outbox_owner_inconsistent(job_id: &str, generation: i64) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.outbox.owner.inconsistent",
    )
    .with_remediation(format!(
        "outbox row for job {job_id} generation {generation} must match exactly one job and one slot; preserve it and repair durable ownership before retrying"
    ))
}

/// Detect schema and materialized-state shapes owned by the retired implicit
/// surge model.  The rows are deliberately not rewritten or deleted: opening
/// such a journal must preserve evidence and make the instance not-ready.
fn legacy_slots_schema(conn: &Connection) -> StoreResult<bool> {
    let mut statement = conn.prepare("PRAGMA table_info(slots)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column?.eq_ignore_ascii_case("surge") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn state_capacity_invalid(state: &FleetState) -> bool {
    // Materialized slot rows are the authoritative capacity-bearing set.
    // Count every row: stale state must not evade detection through an
    // unheld permit, non-ready phase, or malformed/non-numeric slot ID.
    state.capacity_declared && state.slots.len() > state.desired_ready as usize
}

fn event_generation(event: &Event) -> Generation {
    match event {
        Event::PermitReserved { generation, .. }
        | Event::ExecutorProven { generation, .. }
        | Event::SessionLive { generation, .. }
        | Event::RegistrationIntended { generation, .. }
        | Event::Registered { generation, .. }
        | Event::RegistrationLost { generation, .. }
        | Event::ReadyAttempt { generation, .. }
        | Event::JobAcquisitionIntended { generation, .. }
        | Event::JobAcquisitionResolved { generation, .. }
        | Event::AcquisitionProbeFailed { generation, .. }
        | Event::JobAcquisitionLost { generation, .. }
        | Event::JobOwned { generation, .. }
        | Event::JobStarted { generation, .. }
        | Event::JobTerminalResult { generation, .. }
        | Event::CompletionIntended { generation, .. }
        | Event::CompletionAttemptFailed { generation, .. }
        | Event::CompletionUnresolvable { generation, .. }
        | Event::CompletionPayloadLost { generation, .. }
        | Event::CompletionSendStarted { generation, .. }
        | Event::RemoteAcked { generation, .. }
        | Event::JobWorkerLost { generation, .. }
        | Event::RemoteObservedTerminal { generation, .. }
        | Event::CleanupIntended { generation, .. }
        | Event::SlotHeartbeat { generation, .. }
        | Event::SlotStale { generation, .. }
        | Event::DiskPressureTerminalFence { generation, .. } => *generation,
        Event::PackageActivated { generation, .. }
        | Event::PackageRetireIntended { generation } => Generation(*generation),
        _ => Generation(0),
    }
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::ControlLive => "control_live",
        Event::JournalWritable => "journal_writable",
        Event::JobAcquisitionIntended { .. } => "job_acquisition_intended",
        Event::JobAcquisitionResolved { .. } => "job_acquisition_resolved",
        Event::AcquisitionProbeFailed { .. } => "acquisition_probe_failed",
        Event::JobAcquisitionLost { .. } => "job_acquisition_lost",
        Event::Dependency { .. } => "dependency",
        Event::Routing { .. } => "routing",
        Event::DesiredCapacity { .. } => "desired_capacity",
        Event::PermitReserved { .. } => "permit_reserved",
        Event::ExecutorProven { .. } => "executor_proven",
        Event::SessionLive { .. } => "session_live",
        Event::RegistrationIntended { .. } => "registration_intended",
        Event::Registered { .. } => "registered",
        Event::RegistrationLost { .. } => "registration_lost",
        Event::ReadyAttempt { .. } => "ready_attempt",
        Event::JobOwned { .. } => "job_owned",
        Event::JobStarted { .. } => "job_started",
        Event::JobTerminalResult { .. } => "job_terminal_result",
        Event::CompletionIntended { .. } => "completion_intended",
        Event::CompletionAttemptFailed { .. } => "completion_attempt_failed",
        Event::CompletionUnresolvable { .. } => "completion_unresolvable",
        Event::CompletionPayloadLost { .. } => "completion_payload_lost",
        Event::CompletionSendStarted { .. } => "completion_send_started",
        Event::RemoteAcked { .. } => "remote_acked",
        Event::RemoteObservedTerminal { .. } => "remote_observed_terminal",
        Event::JobWorkerLost { .. } => "job_worker_lost",
        Event::CleanupIntended { .. } => "cleanup_intended",
        Event::SlotHeartbeat { .. } => "slot_heartbeat",
        Event::SlotStale { .. } => "slot_stale",
        Event::DiskPressureTerminalFence { .. } => "disk_pressure_terminal_fence",
        Event::CanaryObserved { .. } => "canary_observed",
        Event::PackageActivated { .. } => "package_activated",
        Event::PackageRetireIntended { .. } => "package_retire_intended",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn assert_sqlite_version(conn: &Connection) -> StoreResult<()> {
    let raw: String = conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let parsed = parse_sqlite_version(&raw).ok_or_else(|| {
        StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.sqlite.version.unparsed",
        )
        .with_remediation(format!("sqlite_version() returned {raw}"))
    })?;
    if parsed < MIN_SQLITE_VERSION {
        return Err(
            StoreError::new(velnor_model::ExitClass::Operation, "journal.sqlite.too_old")
                .with_remediation(format!(
                    "bundled SQLite {raw} is older than {}.{}.{} (WAL-reset fix)",
                    MIN_SQLITE_VERSION.0, MIN_SQLITE_VERSION.1, MIN_SQLITE_VERSION.2
                )),
        );
    }
    Ok(())
}

fn parse_sqlite_version(raw: &str) -> Option<(u32, u32, u32)> {
    let mut parts = raw.split('.');
    let major = u32::from_str(parts.next()?).ok()?;
    let minor = u32::from_str(parts.next()?).ok()?;
    let patch = u32::from_str(parts.next()?).ok()?;
    Some((major, minor, patch))
}

/// Checksum of an outbox payload, recorded before send.
#[must_use]
pub fn payload_checksum(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

fn decode_checked_event(
    generation: i64,
    kind: &str,
    payload: &str,
    checksum: &str,
) -> StoreResult<Event> {
    if sha256_hex(payload.as_bytes()) != checksum {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.checksum.mismatch",
        )
        .with_remediation("the event log failed integrity verification"));
    }
    let event: Event = serde_json::from_str(payload).map_err(|error| {
        StoreError::new(velnor_model::ExitClass::Conflict, "journal.event.unknown")
            .with_remediation(format!(
                "event could not be decoded by schema version {JOURNAL_SCHEMA_VERSION}; preserve the journal and reopen it with the binary that wrote it: {error}"
            ))
    })?;
    let stored_generation = u64::try_from(generation).map_err(|_| {
        StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.event.metadata.mismatch",
        )
        .with_remediation("event generation is negative")
    })?;
    let expected_generation = event_generation(&event).0;
    if kind != event_kind(&event) || stored_generation != expected_generation {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.event.metadata.mismatch",
        )
        .with_remediation(
            "preserve the journal unchanged; event kind or generation does not match its payload",
        ));
    }
    Ok(event)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;
    use rusqlite::OptionalExtension;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn open_tmp(label: &str) -> (PathBuf, Journal) {
        let nanos = unix_now();
        let dir = std::env::temp_dir().join(format!(
            "velnor-journal-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.db");
        let journal = Journal::open(&path).unwrap();
        (dir, journal)
    }

    fn open_pressure_tmp(label: &str) -> (PathBuf, Journal) {
        let nanos = unix_now();
        let dir = std::env::temp_dir().join(format!(
            "velnor-journal-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.db");
        let journal = Journal::open_for_service_instance(&path, "service-one").unwrap();
        (dir, journal)
    }

    #[test]
    fn concurrent_fresh_openers_converge_on_one_schema() {
        let nanos = unix_now();
        let dir = std::env::temp_dir().join(format!(
            "velnor-journal-concurrent-open-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Arc::new(dir.join("journal.db"));
        let start = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    Journal::open(path.as_path())
                        .and_then(|journal| journal.load_state().map(|_| ()))
                })
            })
            .collect();

        for handle in handles {
            handle
                .join()
                .expect("concurrent journal opener panicked")
                .expect("concurrent journal opener failed");
        }

        let conn = Connection::open(path.as_path()).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(u32::try_from(version).unwrap(), JOURNAL_SCHEMA_VERSION);
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn slot(id: &str) -> SlotId {
        SlotId(id.to_owned())
    }

    fn job(id: &str) -> JobId {
        JobId(id.to_owned())
    }

    fn r#gen() -> Generation {
        Generation::INITIAL
    }

    #[allow(clippy::too_many_arguments)]
    fn observe_pressure_one(
        journal: &Journal,
        service_instance: &str,
        filesystem_id: &str,
        volume_fingerprint: &str,
        slot_id: &SlotId,
        generation: Generation,
        launch_nonce: &str,
        available_bytes: u64,
        min_free_bytes: u64,
        degraded_seconds: u64,
        drain_seconds: u64,
        now_unix: u64,
    ) -> StoreResult<DiskPressureObservation> {
        let samples = [DiskPressureFilesystemSample {
            filesystem_id: filesystem_id.to_owned(),
            alias_ids: Vec::new(),
            available_bytes: Some(available_bytes),
            min_free_bytes,
            volume_fingerprint: Some(volume_fingerprint.to_owned()),
        }];
        journal
            .observe_disk_pressure_roots(
                service_instance,
                slot_id,
                generation,
                launch_nonce,
                &samples,
                degraded_seconds,
                drain_seconds,
                now_unix,
            )?
            .into_iter()
            .next()
            .map(|(_, observation)| observation)
            .ok_or_else(|| disk_pressure_state_invalid("empty filesystem observation".to_owned()))
    }

    fn event_count(journal: &Journal) -> i64 {
        journal
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap()
    }

    fn prime_ready(journal: &mut Journal, id: &str) {
        let s = slot(id);
        let g = r#gen();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
            },
            Event::ExecutorProven {
                slot_id: s.clone(),
                generation: g,
            },
            Event::SessionLive {
                slot_id: s.clone(),
                generation: g,
            },
            Event::RegistrationIntended {
                slot_id: s.clone(),
                generation: g,
            },
            Event::Registered {
                slot_id: s.clone(),
                generation: g,
            },
        ] {
            let outcome = journal.apply(event).unwrap();
            assert!(!outcome.rejected);
        }
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: s,
                    generation: g,
                })
                .unwrap()
                .rejected
        );
    }

    /// Drive one slot to a running job so completion tests start from the
    /// exact state the runner reaches before it produces a terminal result.
    fn prime_running_job(journal: &mut Journal, slot_name: &str, job_name: &str) -> Generation {
        let g = r#gen();
        prime_ready(journal, slot_name);
        for event in [
            Event::ReadyAttempt {
                slot_id: slot(slot_name),
                generation: g,
            },
            Event::JobAcquisitionIntended {
                slot_id: slot(slot_name),
                job_id: job(job_name),
                generation: g,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            },
            Event::JobOwned {
                job_id: job(job_name),
                slot_id: slot(slot_name),
                attempt: 1,
                generation: g,
                worker: "worker-1".into(),
                accepted_unix: 0,
            },
            Event::JobStarted {
                job_id: job(job_name),
                generation: g,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        g
    }

    fn outbox_row(journal: &Journal, job_name: &str) -> Option<OutboxRecord> {
        journal
            .materialized_state()
            .unwrap()
            .outbox
            .into_iter()
            .find(|row| row.job_id == job(job_name))
    }

    #[test]
    fn set_drain_round_trips_through_materialized_state() {
        let (dir, mut journal) = open_tmp("drain-round-trip");
        let before = journal.materialized_state().unwrap();
        assert!(!before.drain_active);
        assert_eq!(before.drain_version, 0);

        assert!(journal.set_drain(3).unwrap());
        let raw: Option<String> = journal
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'drain'", [], |row| {
                row.get(0)
            })
            .optional()
            .unwrap();
        assert_eq!(raw.as_deref(), Some("requested:3"));

        let after = journal.materialized_state().unwrap();
        assert!(after.drain_active);
        assert_eq!(after.drain_version, 3);
        // No event was written: the drain marker is meta-only by design.
        assert_eq!(event_count(&journal), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn set_drain_is_idempotent_and_monotonic() {
        let (dir, mut journal) = open_tmp("drain-idempotent");
        assert!(journal.set_drain(3).unwrap());
        assert!(!journal.set_drain(3).unwrap());
        assert!(journal.set_drain(9).unwrap());
        // A stale writer never regresses the latched version.
        assert!(!journal.set_drain(4).unwrap());
        let state = journal.materialized_state().unwrap();
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 9);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn admission_fence_round_trips_persists_and_clears() {
        let (dir, mut journal) = open_tmp("admission-fence");
        prime_ready(&mut journal, "s-1");
        assert!(journal.set_admission_blocked(3).unwrap());
        assert!(!journal.set_admission_blocked(2).unwrap());
        let fenced = journal.materialized_state().unwrap();
        assert!(fenced.admission_blocked);
        assert_eq!(fenced.admission_version, 3);
        assert_eq!(fenced.advertised_capacity(), 0);
        assert_eq!(fenced.health().capacity_permits, 0);
        assert_eq!(
            read_admission_state(&dir.join("journal.db")).unwrap(),
            Some(AdmissionState { version: 3 })
        );

        assert!(
            journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("s-1"),
                    job_id: job("blocked-job"),
                    generation: r#gen(),
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(journal.clear_admission_blocked_if(Some(3)).unwrap());
        assert!(!journal.clear_admission_blocked_if(Some(3)).unwrap());
        assert!(!journal.materialized_state().unwrap().admission_blocked);
        assert_eq!(read_admission_state(&dir.join("journal.db")).unwrap(), None);
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("s-1"),
                    job_id: job("released-job"),
                    generation: r#gen(),
                    message_id: "msg-2".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn admission_fence_compare_and_delete_preserves_a_newer_concurrent_fence() {
        let (dir, mut journal) = open_tmp("admission-fence-cas");
        assert!(journal.set_admission_blocked(3).unwrap());
        assert!(!journal.clear_admission_blocked_if(Some(2)).unwrap());
        assert_eq!(
            read_admission_state(&dir.join("journal.db")).unwrap(),
            Some(AdmissionState { version: 3 })
        );
        assert!(journal.set_admission_blocked(4).unwrap());
        assert!(!journal.clear_admission_blocked_if(Some(3)).unwrap());
        assert_eq!(
            read_admission_state(&dir.join("journal.db")).unwrap(),
            Some(AdmissionState { version: 4 })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn materialized_state_tolerates_unknown_meta_keys_and_rejects_malformed_drain() {
        let (dir, journal) = open_tmp("drain-meta-tolerance");
        {
            let transaction = journal.conn.unchecked_transaction().unwrap();
            begin_journal_write_gate(&transaction).unwrap();
            transaction
                .execute(
                    "INSERT INTO meta (key, value) VALUES ('future_key', 'anything')",
                    [],
                )
                .unwrap();
            end_journal_write_gate(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        let state = journal.materialized_state().unwrap();
        assert!(!state.drain_active);

        {
            let transaction = journal.conn.unchecked_transaction().unwrap();
            begin_journal_write_gate(&transaction).unwrap();
            transaction
                .execute(
                    "INSERT INTO meta (key, value) VALUES ('drain', 'bogus')
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [],
                )
                .unwrap();
            end_journal_write_gate(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        let error = journal.materialized_state().unwrap_err();
        assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persist_state_drops_unknown_meta_keys_on_apply() {
        let (dir, mut journal) = open_tmp("drain-meta-apply-drops");
        drop_replay_baseline_fence(&journal.conn);
        journal
            .conn
            .execute(
                "INSERT INTO meta (key, value) VALUES ('future_key', 'anything')",
                [],
            )
            .unwrap();
        restore_journal_write_fence(&journal.conn);
        journal.apply(Event::ControlLive).unwrap();
        let raw: Option<String> = journal
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'future_key'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(raw, None);
        // Known keys survive the same rewrite.
        let live: String = journal
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'control_live'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(live, "1");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pre_drain_binary_write_is_fenced_without_clearing_marker() {
        let (dir, mut journal) = open_tmp("drain-old-writer");
        assert!(journal.set_drain(7).unwrap());
        // A pre-v9 binary's persist_state would rewrite every meta row it
        // knows and drop the drain key. The durable v9 fence rejects that
        // stale write before it can unlatch the fleet.
        let old_writer = Connection::open(dir.join("journal.db")).unwrap();
        let error = old_writer.execute("DELETE FROM meta", []).unwrap_err();
        assert!(error.to_string().contains(JOURNAL_WRITE_FENCE_REASON));
        let state = journal.materialized_state().unwrap();
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 7);
        assert_eq!(
            read_drain_state(&dir.join("journal.db")),
            Ok(Some(DrainState {
                active: true,
                version: 7
            }))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_drain_state_distinguishes_absent_from_unreadable_or_corrupt() {
        let missing = std::env::temp_dir().join(format!(
            "velnor-journal-drain-missing-{}-{}",
            std::process::id(),
            unix_now()
        ));
        let _ = std::fs::remove_file(&missing);
        assert!(matches!(
            read_drain_state(&missing),
            Err(DrainStateReadError::Unavailable)
        ));

        let (dir, mut journal) = open_tmp("drain-read-none");
        assert!(journal.set_drain(5).unwrap());
        let path = dir.join("journal.db");
        assert_eq!(
            read_drain_state(&path),
            Ok(Some(DrainState {
                active: true,
                version: 5
            }))
        );

        // A writer holding an exclusive lock makes the zero-timeout read
        // report no marker instead of blocking. Exercised on a rollback-mode
        // fixture: WAL snapshot readers never block on a writer, which is
        // exactly why the production read needs no wait there either.
        let locked_path = dir.join("locked.db");
        {
            let setup = Connection::open(&locked_path).unwrap();
            setup
                .execute_batch(
                    "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                     INSERT INTO meta (key, value) VALUES ('drain', 'requested:11');",
                )
                .unwrap();
        }
        let blocker = Connection::open(&locked_path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(matches!(
            read_drain_state(&locked_path),
            Err(DrainStateReadError::Unavailable)
        ));
        blocker.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            read_drain_state(&locked_path),
            Ok(Some(DrainState {
                active: true,
                version: 11
            }))
        );

        let garbage = dir.join("garbage.db");
        std::fs::write(&garbage, b"not a sqlite database").unwrap();
        assert!(matches!(
            read_drain_state(&garbage),
            Err(DrainStateReadError::Unavailable)
        ));
        let malformed = dir.join("malformed.db");
        let malformed_journal = Journal::open(&malformed).unwrap();
        drop_replay_baseline_fence(&malformed_journal.conn);
        malformed_journal
            .conn
            .execute(
                "INSERT INTO meta (key, value) VALUES ('drain', 'corrupt')",
                [],
            )
            .unwrap();
        assert!(matches!(
            read_drain_state(&malformed),
            Err(DrainStateReadError::Malformed)
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_admission_state_distinguishes_absent_from_corrupt() {
        let (dir, mut journal) = open_tmp("admission-read");
        let path = dir.join("journal.db");
        assert_eq!(read_admission_state(&path).unwrap(), None);
        journal.set_admission_blocked(5).unwrap();
        assert_eq!(
            read_admission_state(&path).unwrap(),
            Some(AdmissionState { version: 5 })
        );
        drop_replay_baseline_fence(&journal.conn);
        journal
            .conn
            .execute(
                "UPDATE meta SET value = 'not-a-fence' WHERE key = 'admission'",
                [],
            )
            .unwrap();
        assert_eq!(
            read_admission_state(&path),
            Err(AdmissionStateReadError::Malformed)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reducer_rejects_new_permits_and_acquisitions_when_draining() {
        let (dir, mut journal) = open_tmp("drain-reducer-gate");
        prime_ready(&mut journal, "s-1");
        let g = r#gen();
        // Second slot without re-declaring capacity: `prime_ready` pins
        // `DesiredCapacity { ready: 1 }`, and two slots past a declared one
        // is the contaminated-capacity shape that must fail closed.
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        let s2 = slot("s-2");
        for event in [
            Event::PermitReserved {
                slot_id: s2.clone(),
                generation: g,
            },
            Event::ExecutorProven {
                slot_id: s2.clone(),
                generation: g,
            },
            Event::SessionLive {
                slot_id: s2.clone(),
                generation: g,
            },
            Event::RegistrationIntended {
                slot_id: s2.clone(),
                generation: g,
            },
            Event::Registered {
                slot_id: s2.clone(),
                generation: g,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        for id in ["s-1", "s-2"] {
            assert!(
                !journal
                    .apply(Event::ReadyAttempt {
                        slot_id: slot(id),
                        generation: g,
                    })
                    .unwrap()
                    .rejected
            );
        }
        // Intended before the drain: in-flight work must still resolve, own,
        // and complete after the marker latches.
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("s-1"),
                    job_id: job("job-1"),
                    generation: g,
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(journal.set_drain(2).unwrap());

        // s-3 never had a permit and s-2 sits Ready with no job: both
        // rejections isolate the drain gate from occupancy or fencing.
        assert!(
            journal
                .apply(Event::PermitReserved {
                    slot_id: slot("s-3"),
                    generation: g,
                })
                .unwrap()
                .rejected
        );
        assert!(
            journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("s-2"),
                    job_id: job("job-2"),
                    generation: g,
                    message_id: "msg-2".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id: job("job-1"),
                    acquired_job_id: job("job-1-real"),
                    plan_id: "plan-1".into(),
                    generation: g,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job("job-1-real"),
                    slot_id: slot("s-1"),
                    attempt: 1,
                    generation: g,
                    worker: "worker-1".into(),
                    accepted_unix: 1,
                })
                .unwrap()
                .rejected
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn drain_marker_survives_unrelated_event_applies() {
        let (dir, mut journal) = open_tmp("drain-sticky");
        assert!(journal.set_drain(7).unwrap());
        assert!(
            !journal
                .apply(Event::Dependency {
                    github_reachable: true,
                })
                .unwrap()
                .rejected
        );
        let state = journal.materialized_state().unwrap();
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 7);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Burn the durable attempt budget the way the controller does.
    fn exhaust_attempts(journal: &mut Journal, job_name: &str, generation: Generation) {
        for _ in 0..MAX_COMPLETION_ATTEMPTS {
            assert!(
                !journal
                    .apply(Event::CompletionAttemptFailed {
                        job_id: job(job_name),
                        generation,
                        permanent: false,
                    })
                    .unwrap()
                    .rejected
            );
        }
    }

    #[test]
    fn terminal_result_is_durable_before_any_payload_exists() {
        let (_dir, mut journal) = open_tmp("terminal-result");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        assert!(
            !journal
                .apply(Event::JobTerminalResult {
                    job_id: job("job-1"),
                    generation: g,
                    conclusion: "succeeded".into(),
                })
                .unwrap()
                .rejected
        );
        // Crash point C5: the terminal result is durable, the outbox is not.
        let state = journal.materialized_state().unwrap();
        let row = state.jobs.iter().find(|row| row.job_id == job("job-1"));
        let row = row.expect("job survives");
        assert_eq!(row.phase, JobPhase2::Completing);
        assert_eq!(row.terminal_conclusion.as_deref(), Some("succeeded"));
        assert!(state.outbox.is_empty());
        assert_eq!(
            journal
                .recorded_terminal_conclusion(&job("job-1"), g)
                .unwrap()
                .as_deref(),
            Some("succeeded"),
            "recovery must read the real conclusion instead of inventing a failure"
        );
    }

    #[test]
    fn terminal_result_replay_is_idempotent_and_never_rewritten() {
        let (_dir, mut journal) = open_tmp("terminal-result-replay");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        let result = Event::JobTerminalResult {
            job_id: job("job-1"),
            generation: g,
            conclusion: "succeeded".into(),
        };
        assert!(!journal.apply(result.clone()).unwrap().rejected);
        let before = event_count(&journal);
        assert!(!journal.apply(result).unwrap().rejected);
        assert_eq!(event_count(&journal), before, "replay must not append");
        assert!(
            journal
                .apply(Event::JobTerminalResult {
                    job_id: job("job-1"),
                    generation: g,
                    conclusion: "failed".into(),
                })
                .unwrap()
                .rejected,
            "a recorded conclusion is never corrected in place"
        );
    }

    #[test]
    fn terminal_result_on_a_stale_generation_is_rejected() {
        let (_dir, mut journal) = open_tmp("terminal-result-stale");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        assert!(
            journal
                .apply(Event::JobTerminalResult {
                    job_id: job("job-1"),
                    generation: g.next(),
                    conclusion: "succeeded".into(),
                })
                .unwrap()
                .rejected
        );
    }

    #[test]
    fn pending_completion_carries_a_durable_attempt_budget_and_deadline() {
        let (_dir, mut journal) = open_tmp("completion-budget");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation: g,
                    payload_sha256: "sum".into(),
                })
                .unwrap()
                .rejected
        );
        let row = outbox_row(&journal, "job-1").expect("row");
        assert_eq!(row.attempts, 0);
        assert!(!row.permanent);
        assert!(!row.abandoned);
        assert_eq!(
            row.deadline_unix,
            row.created_unix + COMPLETION_RESOLUTION_SECONDS
        );
        assert!(row.is_pending());
        assert!(!row.budget_exhausted(row.created_unix));
        assert!(row.budget_exhausted(row.deadline_unix));
    }

    #[test]
    fn attempt_counter_survives_reopen_so_recovery_cannot_restart_from_zero() {
        let (dir, mut journal) = open_tmp("attempt-counter");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "sum".into(),
            })
            .unwrap();
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: g,
            })
            .unwrap();
        for _ in 0..3 {
            assert!(
                !journal
                    .apply(Event::CompletionAttemptFailed {
                        job_id: job("job-1"),
                        generation: g,
                        permanent: false,
                    })
                    .unwrap()
                    .rejected
            );
        }
        drop(journal);
        let reopened = Journal::open(dir.join("journal.db")).unwrap();
        assert_eq!(outbox_row(&reopened, "job-1").unwrap().attempts, 3);
        assert_eq!(reopened.load_state().unwrap().outbox[0].attempts, 3);
    }

    #[test]
    fn attempt_failure_before_the_send_claim_is_rejected() {
        let (_dir, mut journal) = open_tmp("attempt-before-claim");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "sum".into(),
            })
            .unwrap();
        assert!(
            journal
                .apply(Event::CompletionAttemptFailed {
                    job_id: job("job-1"),
                    generation: g,
                    permanent: false,
                })
                .unwrap()
                .rejected,
            "an attempt cannot fail before it was claimed"
        );
    }

    #[test]
    fn unresolvable_is_refused_while_the_completion_still_has_budget() {
        let (_dir, mut journal) = open_tmp("unresolvable-early");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "sum".into(),
            })
            .unwrap();
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: g,
            })
            .unwrap();
        assert!(
            journal
                .apply(Event::CompletionUnresolvable {
                    job_id: job("job-1"),
                    generation: g,
                    reason: "impatient caller".into(),
                })
                .unwrap()
                .rejected,
            "the terminal state must be provable from durable state, not asserted"
        );
        assert!(outbox_row(&journal, "job-1").unwrap().is_pending());
    }

    #[test]
    fn exhausted_completion_reaches_a_bounded_terminal_state_and_frees_the_slot() {
        let (_dir, mut journal) = open_tmp("unresolvable-bounded");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "sum".into(),
            })
            .unwrap();
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: g,
            })
            .unwrap();
        // Before: the pending row is a hard admission barrier.
        let blocked = journal.materialized_state().unwrap();
        assert!(pending_outbox_blocks_admission(
            &blocked,
            &slot("scope-1"),
            g
        ));
        exhaust_attempts(&mut journal, "job-1", g);

        let outcome = journal
            .apply(Event::CompletionUnresolvable {
                job_id: job("job-1"),
                generation: g,
                reason: "send budget exhausted".into(),
            })
            .unwrap();
        assert!(!outcome.rejected);
        assert!(outcome.commands.contains(&SideEffect::DeleteOutbox {
            job_id: job("job-1"),
            generation: g,
        }));

        let state = journal.materialized_state().unwrap();
        assert!(state.outbox.is_empty(), "terminal rows leave the outbox");
        assert!(state.jobs.is_empty(), "the slot's job is released");
        assert!(!pending_outbox_blocks_admission(
            &state,
            &slot("scope-1"),
            g
        ));
        assert!(journal.pending_outbox().unwrap().is_empty());
        assert_eq!(state.health().oldest_outbox_entry_seconds, 0);

        // The operator surface is the immutable log, not the dropped row.
        let abandoned = journal.unresolvable_completions().unwrap();
        assert_eq!(abandoned.len(), 1);
        assert_eq!(abandoned[0].job_id, job("job-1"));
        assert_eq!(abandoned[0].reason, "send budget exhausted");

        // A permanently unacknowledgeable completion never becomes a second
        // terminal send: the claim stands and no ack was forged.
        assert!(
            journal
                .apply(Event::CompletionSendStarted {
                    job_id: job("job-1"),
                    generation: g,
                })
                .unwrap()
                .rejected
        );
        assert!(
            journal
                .apply(Event::RemoteAcked {
                    job_id: job("job-1"),
                    generation: g,
                })
                .unwrap()
                .rejected
        );
    }

    #[test]
    fn payload_loss_requires_exact_owner_generation_and_checksum() {
        let (dir, mut journal) = open_tmp("payload-loss");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        let checksum = payload_checksum(b"ok");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: checksum.clone(),
            })
            .unwrap();

        for event in [
            Event::CompletionPayloadLost {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "wrong".into(),
                reason: "corrupt payload".into(),
            },
            Event::CompletionPayloadLost {
                job_id: job("job-1"),
                generation: g.next(),
                payload_sha256: checksum.clone(),
                reason: "stale generation".into(),
            },
            Event::CompletionPayloadLost {
                job_id: job("job-2"),
                generation: g,
                payload_sha256: checksum.clone(),
                reason: "wrong owner".into(),
            },
        ] {
            assert!(journal.apply(event).unwrap().rejected);
        }
        assert!(outbox_row(&journal, "job-1").unwrap().is_pending());

        let outcome = journal
            .record_completion_payload_loss(
                &job("job-1"),
                g,
                &checksum,
                "completion payload is missing",
            )
            .unwrap();
        assert!(!outcome.rejected);
        assert!(outcome.commands.contains(&SideEffect::DeleteOutbox {
            job_id: job("job-1"),
            generation: g,
        }));
        assert!(outcome
            .commands
            .contains(&SideEffect::AdvertiseCapacity { permits: 1 }));

        let state = journal.materialized_state().unwrap();
        assert!(state.jobs.is_empty());
        assert!(state.outbox.is_empty());
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        assert!(!journal.has_remote_terminal_ack(&job("job-1"), g).unwrap());
        let losses = journal.unresolvable_completions().unwrap();
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].reason, "completion payload is missing");

        drop(journal);
        let reopened = Journal::open(dir.join("journal.db")).unwrap();
        assert!(reopened.pending_outbox().unwrap().is_empty());
        assert!(reopened.materialized_state().unwrap().jobs.is_empty());
        assert_eq!(
            reopened.unresolvable_completions().unwrap()[0].reason,
            "completion payload is missing"
        );
    }

    #[test]
    fn v6_journal_is_upgraded_for_payload_loss_event_vocabulary() {
        let (dir, journal) = open_tmp("v6-to-v7");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 6u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let reopened = Journal::open(&path).unwrap();
        let version: i64 = reopened
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
    }

    #[test]
    fn a_permanent_remote_refusal_spends_the_whole_budget_at_once() {
        let (_dir, mut journal) = open_tmp("unresolvable-permanent");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "sum".into(),
            })
            .unwrap();
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: g,
            })
            .unwrap();
        assert!(
            !journal
                .apply(Event::CompletionAttemptFailed {
                    job_id: job("job-1"),
                    generation: g,
                    permanent: true,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionUnresolvable {
                    job_id: job("job-1"),
                    generation: g,
                    reason: "run service refused the payload".into(),
                })
                .unwrap()
                .rejected
        );
        assert!(journal.pending_outbox().unwrap().is_empty());
    }

    #[test]
    fn a_freed_slot_admits_the_next_job_after_a_completion_is_abandoned() {
        let (_dir, mut journal) = open_tmp("unresolvable-readmit");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "sum".into(),
            })
            .unwrap();
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: g,
            })
            .unwrap();
        exhaust_attempts(&mut journal, "job-1", g);
        journal
            .apply(Event::CompletionUnresolvable {
                job_id: job("job-1"),
                generation: g,
                reason: "send budget exhausted".into(),
            })
            .unwrap();
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: slot("scope-1"),
                    generation: g,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-1"),
                    job_id: job("job-2"),
                    generation: g,
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected,
            "one unacknowledgeable completion must not wedge the slot forever"
        );
    }

    #[test]
    fn an_abandoned_job_id_can_be_redelivered_on_a_later_attempt() {
        let (_dir, mut journal) = open_tmp("unresolvable-redeliver");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: g,
                payload_sha256: "sum".into(),
            })
            .unwrap();
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: g,
            })
            .unwrap();
        exhaust_attempts(&mut journal, "job-1", g);
        journal
            .apply(Event::CompletionUnresolvable {
                job_id: job("job-1"),
                generation: g,
                reason: "send budget exhausted".into(),
            })
            .unwrap();
        for event in [
            Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: g,
            },
            Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: g,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            },
            Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 2,
                generation: g,
                worker: "worker-2".into(),
                accepted_unix: 0,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation: g,
                    payload_sha256: "second-sum".into(),
                })
                .unwrap()
                .rejected,
            "the second attempt gets a fresh outbox row and a fresh claim"
        );
        let row = outbox_row(&journal, "job-1").unwrap();
        assert_eq!(row.payload_sha256, "second-sum");
        assert!(!row.send_started);
        assert_eq!(row.attempts, 0);
    }

    #[test]
    fn a_version_behind_its_physical_shape_is_refused_without_mutation() {
        let (dir, journal) = open_tmp("stamp-behind-shape");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        // This specifically guards the v5-to-v6 physical-shape transition;
        // v6-to-v7 changes only the event vocabulary and is intentionally
        // compatible with the current materialized tables.
        conn.pragma_update(None, "user_version", 5u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        // The tables carry the v6 vocabulary but the stamp does not. Some
        // writer mutated the shape without stamping; guessing which
        // vocabulary wrote the events is exactly what must never happen.
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_journal_written_by_a_newer_binary_is_refused_not_reinterpreted() {
        let (dir, journal) = open_tmp("refuse-newer");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", JOURNAL_SCHEMA_VERSION + 1)
            .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.newer");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_v3_shape_claiming_the_current_version_is_repaired_not_misread() {
        let (dir, journal) = open_tmp("v3-shape-upgrade");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE outbox;
             CREATE TABLE outbox (
                 job_id TEXT PRIMARY KEY,
                 slot_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 payload_sha256 TEXT NOT NULL,
                 intended INTEGER NOT NULL DEFAULT 0,
                 send_started INTEGER NOT NULL DEFAULT 0,
                 remote_acked INTEGER NOT NULL DEFAULT 0,
                 created_unix INTEGER NOT NULL
             );
             INSERT INTO outbox (
                 job_id, slot_id, generation, payload_sha256, intended,
                 send_started, remote_acked, created_unix
             ) VALUES ('job-1', 'scope-1', 1, 'sum', 1, 1, 0, 1000);
             ALTER TABLE jobs DROP COLUMN terminal_conclusion;",
        )
        .unwrap();
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 3u32).unwrap();
        drop(conn);

        let migrated = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        let row = outbox_row(&migrated, "job-1").expect("pending row survives");
        assert!(row.is_pending());
        assert_eq!(row.attempts, 0);
        assert_eq!(
            row.deadline_unix,
            1000 + COMPLETION_RESOLUTION_SECONDS,
            "an upgrade must not extend an existing completion's budget to infinity"
        );
        drop(migrated);
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
    }

    #[test]
    fn apply_many_commits_accepted_events_without_poisoning_after_rejection() {
        let (_dir, mut journal) = open_tmp("batch-heartbeat");
        prime_ready(&mut journal, "scope-1");
        let outcomes = journal
            .apply_many([
                Event::SlotHeartbeat {
                    slot_id: slot("scope-1"),
                    generation: Generation(2),
                    pid: 123,
                },
                Event::SlotHeartbeat {
                    slot_id: slot("scope-1"),
                    generation: r#gen(),
                    pid: 456,
                },
            ])
            .unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes[0].rejected);
        assert!(!outcomes[1].rejected);
        assert_eq!(
            journal
                .load_state()
                .unwrap()
                .slots
                .iter()
                .find(|row| row.slot_id == slot("scope-1"))
                .and_then(|slot| slot.pid),
            Some(456)
        );
    }

    #[test]
    fn apply_many_empty_does_not_start_an_immediate_transaction() {
        let (dir, mut journal) = open_tmp("batch-empty");
        journal.conn.busy_timeout(Duration::ZERO).unwrap();
        let mut blocker = Connection::open(dir.join("journal.db")).unwrap();
        blocker.busy_timeout(Duration::ZERO).unwrap();
        let _blocking_transaction = blocker
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();

        let outcomes = journal.apply_many(std::iter::empty()).unwrap();

        assert!(outcomes.is_empty());
    }

    #[test]
    fn apply_many_elides_repeated_proof_and_routing_events() {
        let (_dir, mut journal) = open_tmp("batch-no-op");
        prime_ready(&mut journal, "scope-1");
        let before = event_count(&journal);
        let outcomes = journal
            .apply_many([
                Event::ExecutorProven {
                    slot_id: slot("scope-1"),
                    generation: r#gen(),
                },
                Event::SessionLive {
                    slot_id: slot("scope-1"),
                    generation: r#gen(),
                },
                Event::Routing {
                    valid: true,
                    group_valid: true,
                },
            ])
            .unwrap();

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|outcome| !outcome.rejected));
        assert!(outcomes.iter().all(|outcome| outcome.commands.is_empty()));
        assert_eq!(event_count(&journal), before);
    }

    #[test]
    fn health_distinguishes_active_jobs_from_ready_slots() {
        let (_dir, mut journal) = open_tmp("health-capacity");
        prime_ready(&mut journal, "scope-1");
        let slot_id = slot("scope-1");
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: slot_id.clone(),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        assert_eq!(journal.load_state().unwrap().health().actual_ready_slots, 1);
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: job("job-1"),
                    generation: r#gen(),
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        let intended = journal.load_state().unwrap();
        assert_eq!(intended.health().actual_ready_slots, 0);
        assert_eq!(intended.jobs.len(), 1);
        assert!(intended.jobs[0].provisional);
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job("job-1"),
                    slot_id,
                    attempt: 1,
                    generation: r#gen(),
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1,
                })
                .unwrap()
                .rejected
        );
        let owned = journal.load_state().unwrap();
        assert_eq!(owned.health().actual_ready_slots, 0);
        assert_eq!(owned.jobs.len(), 1);
    }

    #[test]
    fn apply_many_persists_command_bearing_registration_intent() {
        let (dir, mut journal) = open_tmp("batch-command-bearing");
        let s = slot("scope-1");
        let g = r#gen();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
            },
            Event::ExecutorProven {
                slot_id: s.clone(),
                generation: g,
            },
            Event::SessionLive {
                slot_id: s.clone(),
                generation: g,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let before = event_count(&journal);

        let outcomes = journal
            .apply_many([Event::RegistrationIntended {
                slot_id: s.clone(),
                generation: g,
            }])
            .unwrap();

        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            outcomes[0].commands,
            vec![SideEffect::RegisterRunner {
                slot_id: s,
                generation: g,
            }]
        );
        assert_eq!(event_count(&journal), before + 1);
        drop(journal);

        let recovered = Journal::open(dir.join("journal.db")).unwrap();
        assert_eq!(event_count(&recovered), before + 1);
        let registration_events: i64 = recovered
            .conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'registration_intended'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(registration_events, 1);
    }

    #[test]
    fn legacy_extra_capacity_is_preserved_but_blocks_restart_reconcile() {
        let (dir, mut journal) = open_tmp("legacy-extra-capacity");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        for slot_id in ["scope-1", "scope-2", "scope-3"] {
            let outcome = journal
                .apply(Event::PermitReserved {
                    slot_id: slot(slot_id),
                    generation: r#gen(),
                })
                .unwrap();
            assert!(!outcome.rejected);
        }
        drop(journal);

        let mut reopened = Journal::open(dir.join("journal.db")).unwrap();
        let state = reopened.load_state().unwrap();
        assert!(state.capacity_invalid);
        assert_eq!(state.advertised_capacity(), 0);
        assert_eq!(state.health().state, FleetHealthState::NotReady);
        assert_eq!(state.slots.len(), 3, "forensic rows must survive restart");
        assert_eq!(
            reopened
                .conn
                .query_row("SELECT COUNT(*) FROM slots", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        let error = reopened
            .apply(Event::DesiredCapacity { ready: 2 })
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.capacity.invalid");
    }

    #[test]
    fn current_schema_zero_capacity_rejects_without_mutating_forensics() {
        let (dir, journal) = open_tmp("current-schema-stale-capacity");
        let path = dir.join("journal.db");
        drop(journal);

        // Journal::apply must reject this state. Seed the intentionally stale
        // N+1 materialization directly so the test exercises forensic safety.
        let seed = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&seed);
        seed.execute_batch(
            "DROP TABLE outbox;
             CREATE TABLE outbox (
                 job_id TEXT PRIMARY KEY,
                 generation INTEGER NOT NULL,
                 payload_sha256 TEXT NOT NULL,
                 intended INTEGER NOT NULL DEFAULT 0,
                 send_started INTEGER NOT NULL DEFAULT 0,
                 remote_acked INTEGER NOT NULL DEFAULT 0,
                 created_unix INTEGER NOT NULL
             );
             INSERT INTO meta (key, value) VALUES
                 ('control_live', '1'),
                 ('github_reachable', '1'),
                 ('routing_valid', '1'),
                 ('runner_group_valid', '1'),
                 ('desired_ready', '0'),
                 ('canary', 'passing'),
                 ('package_generation', '1'),
                 ('package_apt_version', '0.1.215'),
                 ('capacity_invalid', '0'),
                 ('capacity_declared', '1');
             INSERT INTO slots (
                 slot_id, generation, phase, permit_held, routing_valid,
                 session_live, executor_proven, registered, pid, heartbeat_unix
             ) VALUES
                 ('scope-1', 1, 'ready', 1, 1, 1, 1, 1, NULL, 101),
                 ('scope-2', 1, 'ready', 1, 1, 1, 1, 1, NULL, 102),
                 ('scope-3', 1, 'ready', 1, 1, 1, 1, 1, NULL, 103);
             INSERT INTO jobs (
                 job_id, slot_id, generation, attempt, worker, phase, accepted_unix
             ) VALUES ('job-3', 'scope-3', 1, 1, 'worker-3', 'assigned', 123);
             INSERT INTO outbox (
                 job_id, generation, payload_sha256, intended, send_started,
                 remote_acked, created_unix
             ) VALUES ('job-3', 1, 'payload-checksum', 1, 0, 0, 123);
             ",
        )
        .unwrap();
        let fixture_payload = r#"{"type":"control_live"}"#;
        seed.execute(
            "INSERT INTO events (generation, kind, payload, checksum)
             VALUES (0, 'control_live', ?1, ?2)",
            params![fixture_payload, sha256_hex(fixture_payload.as_bytes())],
        )
        .unwrap();
        drop_schema10_pressure_and_schema11_identity(&seed);
        seed.execute("PRAGMA user_version = 2", []).unwrap();
        drop(seed);

        let checkpoint = Connection::open(&path).unwrap();
        checkpoint
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(checkpoint);

        let snapshot = || {
            let bytes = std::fs::read(&path).unwrap();
            let checksum = sha256_hex(&bytes);
            let conn = Connection::open(&path).unwrap();
            let schema: Vec<String> = conn
                .prepare(
                    "SELECT name || ':' || COALESCE(sql, '')
                     FROM sqlite_master WHERE type IN ('table', 'index')
                     ORDER BY name",
                )
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let mut columns = Vec::new();
            for table in ["events", "slots", "jobs", "outbox", "meta"] {
                let mut statement = conn
                    .prepare("SELECT cid, name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)")
                    .unwrap();
                let table_columns: Vec<String> = statement
                    .query_map([table], |row| {
                        Ok(format!(
                            "{table}:{}:{}:{}:{}:{}:{}",
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                            row.get::<_, i64>(5)?,
                        ))
                    })
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap();
                columns.extend(table_columns);
            }
            let forensic_rows: Vec<String> = conn
                .prepare(
                    "SELECT 'slot:' || slot_id || ':' || generation || ':' || phase || ':' ||
                            permit_held || ':' || routing_valid || ':' || session_live || ':' ||
                            executor_proven || ':' || registered || ':' || COALESCE(pid, '') || ':' ||
                            heartbeat_unix FROM slots ORDER BY slot_id",
                )
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let events: Vec<String> = conn
                .prepare("SELECT kind || ':' || payload || ':' || checksum FROM events ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let event_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
                .unwrap();
            (
                bytes,
                checksum,
                schema,
                columns,
                forensic_rows,
                events,
                event_count,
            )
        };
        // Migrate the explicit v2 fixture before taking the forensic baseline;
        // the failed-reconcile assertion covers v3 state, not migration.
        drop(Journal::open_for_service_instance(&path, "legacy-migration").unwrap());
        let migrated = Connection::open(&path).unwrap();
        let slot_id_not_null: Option<i64> = migrated
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('outbox') WHERE name = 'slot_id'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(slot_id_not_null, Some(1));
        let before = snapshot();

        for _ in 0..2 {
            let mut reopened =
                Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
            // This fixture intentionally corrupts only the materialized
            // tables. Both read APIs preserve the forensic capacity-invalid
            // materialized state instead of hiding the stale N+1 slot behind
            // a clean replay result.
            let replayed = reopened.load_state().unwrap();
            assert!(replayed.capacity_invalid);
            assert_eq!(replayed.slots.len(), 3);

            let state = reopened.materialized_state().unwrap();
            assert!(state.capacity_invalid);
            assert_eq!(state.advertised_capacity(), 0);
            assert_eq!(state.slots.len(), 3);
            assert_eq!(state.health().actual_ready_slots, 3);
            let error = reopened
                .apply_many([
                    Event::ControlLive,
                    Event::DesiredCapacity { ready: 2 },
                    Event::SlotHeartbeat {
                        slot_id: slot("scope-3"),
                        generation: r#gen(),
                        pid: 123,
                    },
                ])
                .unwrap_err();
            assert_eq!(error.envelope.reason, "journal.capacity.invalid");
            assert!(reopened.materialized_state().unwrap().capacity_invalid);
            drop(reopened);

            let after = snapshot();
            assert_eq!(
                after, before,
                "failed reconcile must preserve all forensic state"
            );
        }
    }

    fn seed_v2_outbox(path: &Path, version: i64) {
        let conn = Connection::open(path).unwrap();
        drop_replay_baseline_fence(&conn);
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.execute_batch(
            "DROP TABLE outbox;
             CREATE TABLE outbox (
                 job_id TEXT PRIMARY KEY,
                 generation INTEGER NOT NULL,
                 payload_sha256 TEXT NOT NULL,
                 intended INTEGER NOT NULL DEFAULT 0,
                 send_started INTEGER NOT NULL DEFAULT 0,
                 remote_acked INTEGER NOT NULL DEFAULT 0,
                 created_unix INTEGER NOT NULL
             );
             INSERT INTO slots (
                 slot_id, generation, phase, permit_held, routing_valid,
                 session_live, executor_proven, registered, pid, heartbeat_unix
             ) VALUES ('scope-1', 1, 'ready', 1, 1, 1, 1, 1, NULL, 1);
             INSERT INTO jobs (
                 job_id, slot_id, generation, attempt, worker, phase, accepted_unix
             ) VALUES ('job-1', 'scope-1', 1, 1, 'worker-1', 'assigned', 1);
             INSERT INTO outbox (
                 job_id, generation, payload_sha256, intended, send_started,
                 remote_acked, created_unix
             ) VALUES ('job-1', 1, 'payload', 1, 0, 0, 1);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
    }

    fn demote_eventful_journal_to_v8(path: &Path) {
        let conn = Connection::open(path).unwrap();
        drop_replay_baseline_fence(&conn);
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 8u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);
    }

    fn assert_schema8_migration_rejected(path: &Path, reason: &str) {
        let error = Journal::open(path).unwrap_err();
        assert_eq!(error.envelope.reason, reason);
        let conn = Connection::open(path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            8
        );
        assert!(load_replay_baseline(&conn).unwrap().is_none());
        let trigger_count: i64 = conn
            .query_row(
                "SELECT COUNT(*)
                 FROM sqlite_master
                 WHERE type = 'trigger'
                   AND lower(tbl_name) IN (
                       'events', 'slots', 'jobs', 'outbox', 'meta',
                       'disk_pressure_episodes', 'disk_pressure_launches',
                       'journal_write_gate'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trigger_count, 0);
    }

    fn rewrite_replay_baseline(path: &Path, mutate: impl FnOnce(&mut serde_json::Value)) {
        let conn = Connection::open(path).unwrap();
        drop_replay_baseline_fence(&conn);
        let serialized: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [REPLAY_BASELINE_KEY],
                |row| row.get(0),
            )
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        mutate(&mut value);
        let serialized = serde_json::to_string(&value).unwrap();
        let checksum = sha256_hex(serialized.as_bytes());
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = ?2",
            params![serialized, REPLAY_BASELINE_KEY],
        )
        .unwrap();
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = ?2",
            params![checksum, REPLAY_BASELINE_CHECKSUM_KEY],
        )
        .unwrap();
        restore_journal_write_fence(&conn);
    }

    /// Model a pre-v9 database or an explicit forensic tamper fixture. Live
    /// v9 writers must never remove the durable fence.
    fn drop_replay_baseline_fence(conn: &Connection) {
        for (name, _, _) in JOURNAL_WRITE_FENCE_TRIGGERS {
            conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {name};"))
                .unwrap();
        }
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS replay_baseline_delete_fence;
             DROP TRIGGER IF EXISTS replay_baseline_rename_fence;",
        )
        .unwrap();
    }

    fn drop_schema10_pressure_and_schema11_identity(conn: &Connection) {
        conn.execute_batch(
            "DROP TABLE IF EXISTS disk_pressure_episodes;
             DROP TABLE IF EXISTS disk_pressure_launches;
             DROP TABLE IF EXISTS disk_pressure_observations;
             DROP TABLE IF EXISTS journal_identity;",
        )
        .unwrap();
    }

    fn restore_journal_write_fence(conn: &Connection) {
        let transaction = conn.unchecked_transaction().unwrap();
        ensure_journal_write_fence(&transaction).unwrap();
        transaction.commit().unwrap();
    }

    #[test]
    fn future_version_v2_outbox_is_rejected_without_mutation() {
        let (dir, journal) = open_tmp("future-version-v2-outbox");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 99);
        let conn = Connection::open(&path).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.newer");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let check = Connection::open(&path).unwrap();
        assert_eq!(
            check
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            99
        );
    }

    #[test]
    fn v2_marker_with_v3_outbox_shape_is_rejected_without_mutation() {
        let (dir, journal) = open_tmp("v2-marker-v3-outbox");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 2u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let check = Connection::open(&path).unwrap();
        assert_eq!(
            check
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn negative_version_supported_v2_outbox_is_rejected_without_mutation() {
        let (dir, journal) = open_tmp("negative-version-v2-outbox");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, -1);
        let conn = Connection::open(&path).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let check = Connection::open(&path).unwrap();
        assert_eq!(
            check
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            -1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_zero_v2_outbox_migrates_from_physical_shape() {
        let (dir, journal) = open_tmp("version-zero-v2-outbox");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 0);

        let migrated = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        let replayed = migrated.load_state().unwrap();
        let state = migrated.materialized_state().unwrap();
        assert_eq!(
            canonical_projection(replayed),
            canonical_projection(state.clone())
        );
        assert_eq!(state.outbox[0].slot_id, slot("scope-1"));
        let conn = Connection::open(&path).unwrap();
        let baseline: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [REPLAY_BASELINE_KEY],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        let baseline_checksum: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [REPLAY_BASELINE_CHECKSUM_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sha256_hex(baseline.as_bytes()), baseline_checksum);
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        let slot_id_not_null: i64 = conn
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('outbox') WHERE name = 'slot_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(slot_id_not_null, 1);
    }

    #[test]
    fn schema8_upgrade_seeds_anchor_and_fences_schema8_writer() {
        let (dir, journal) = open_tmp("schema8-to-schema11-anchor");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 8u32).unwrap();
        drop(conn);

        let upgraded = Journal::open(&path).unwrap();
        let version: u32 = upgraded
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .map(|value| u32::try_from(value).unwrap())
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION);
        assert!(load_replay_baseline(&upgraded.conn).unwrap().is_some());
        let before = upgraded
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [REPLAY_BASELINE_KEY],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        let error = ensure_supported_schema(version, 8).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.newer");
        let after = upgraded
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [REPLAY_BASELINE_KEY],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(before, after, "a schema-8 writer must not touch v11 state");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema11_reopen_rejects_missing_owner_table_without_recreating_it() {
        let (dir, journal) = open_tmp("schema11-missing-owner");
        let path = dir.join("journal.db");
        drop(journal);
        drop(Journal::open_for_service_instance(&path, "service-one").unwrap());

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE journal_identity;").unwrap();
        drop(conn);

        let error = Journal::open_for_service_instance(&path, "service-two").unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        let conn = Connection::open(&path).unwrap();
        let owner_table_exists: i64 = conn
            .query_row(
                "SELECT EXISTS (
                     SELECT 1 FROM sqlite_master
                     WHERE type = 'table' AND name = 'journal_identity'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner_table_exists, 0, "failed open must preserve evidence");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema11_reopen_rejects_missing_owner_row_on_nonempty_journal() {
        let (dir, mut journal) = open_tmp("schema11-missing-owner-row");
        let path = dir.join("journal.db");
        assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
        drop(journal);

        let error = Journal::open_for_service_instance(&path, "service-two").unwrap_err();
        assert_eq!(error.envelope.reason, "journal.service_instance.mismatch");
        let conn = Connection::open(&path).unwrap();
        let owner_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM journal_identity", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            owner_rows, 0,
            "failed binding must preserve the missing owner"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema11_lowered_to_v10_stamp_is_rejected_without_mutation() {
        let (dir, journal) = open_tmp("schema11-lowered-to-v10");
        let path = dir.join("journal.db");
        drop(journal);
        drop(Journal::open_for_service_instance(&path, "service-one").unwrap());

        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 10u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();

        let error = Journal::open_for_service_instance(&path, "service-one").unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema11_reopen_rejects_missing_pressure_tables_without_recreating_them() {
        for table in [
            "disk_pressure_episodes",
            "disk_pressure_launches",
            "disk_pressure_observations",
        ] {
            let (dir, journal) = open_tmp(&format!("schema11-missing-{table}"));
            let path = dir.join("journal.db");
            drop(journal);
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("DROP TABLE {table};")).unwrap();
            drop(conn);

            let error = Journal::open(&path).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.schema.mismatch");
            let conn = Connection::open(&path).unwrap();
            let table_exists: i64 = conn
                .query_row(
                    "SELECT EXISTS (
                         SELECT 1 FROM sqlite_master
                         WHERE type = 'table' AND name = ?1
                     )",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(table_exists, 0, "failed open recreated {table}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn schema10_upgrade_creates_owner_table_before_binding() {
        let (dir, journal) = open_tmp("schema10-to-schema11-owner");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE journal_identity;").unwrap();
        conn.pragma_update(None, "user_version", 10u32).unwrap();
        drop(conn);

        let upgraded = Journal::open_for_service_instance(&path, "service-one").unwrap();
        let version: i64 = upgraded
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        let owner: String = upgraded
            .conn
            .query_row(
                "SELECT service_instance FROM journal_identity WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner, "service-one");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nonempty_pre_v11_journals_require_explicit_binding_for_migration() {
        for version in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
            let (dir, journal) = open_tmp(&format!("unbound-legacy-v{version}"));
            let path = dir.join("journal.db");
            drop(journal);

            if version <= 2 {
                seed_v2_outbox(&path, i64::from(version));
            } else {
                let mut journal = Journal::open(&path).unwrap();
                assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
                drop(journal);

                let conn = Connection::open(&path).unwrap();
                drop_replay_baseline_fence(&conn);
                if version < 10 {
                    drop_schema10_pressure_and_schema11_identity(&conn);
                } else {
                    conn.execute_batch("DROP TABLE journal_identity;").unwrap();
                }
                if version < 9 {
                    conn.execute(
                        "DELETE FROM meta WHERE key IN (?1, ?2)",
                        params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
                    )
                    .unwrap();
                }
                match version {
                    3 => {
                        for column in ["attempts", "deadline_unix", "permanent", "abandoned"] {
                            conn.execute_batch(&format!(
                                "ALTER TABLE outbox DROP COLUMN {column};"
                            ))
                            .unwrap();
                        }
                        for column in [
                            "terminal_conclusion",
                            "provisional",
                            "plan_id",
                            "run_service_url",
                            "probe_attempts",
                            "probe_deadline_unix",
                        ] {
                            conn.execute_batch(&format!("ALTER TABLE jobs DROP COLUMN {column};"))
                                .unwrap();
                        }
                    }
                    4 => {
                        for column in [
                            "provisional",
                            "plan_id",
                            "run_service_url",
                            "probe_attempts",
                            "probe_deadline_unix",
                        ] {
                            conn.execute_batch(&format!("ALTER TABLE jobs DROP COLUMN {column};"))
                                .unwrap();
                        }
                    }
                    5 => {
                        for column in [
                            "plan_id",
                            "run_service_url",
                            "probe_attempts",
                            "probe_deadline_unix",
                        ] {
                            conn.execute_batch(&format!("ALTER TABLE jobs DROP COLUMN {column};"))
                                .unwrap();
                        }
                    }
                    6..=10 => {}
                    _ => unreachable!(),
                }
                conn.pragma_update(None, "user_version", version).unwrap();
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
                    .unwrap();
            }

            let expected_events = if version <= 2 { 0 } else { 1 };
            let error = Journal::open(&path).unwrap_err();
            assert_eq!(
                error.envelope.reason, "journal.service_instance.mismatch",
                "v{version} unbound migration must request an explicit owner"
            );
            let conn = Connection::open(&path).unwrap();
            assert_eq!(
                conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                i64::from(version),
                "v{version} unbound migration must roll back its schema stamp"
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'journal_identity'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
                0,
                "v{version} unbound migration must not leave an empty owner table"
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                expected_events,
                "v{version} legacy history must survive the rejected open"
            );
            drop(conn);

            let bound = Journal::open_for_service_instance(&path, "service-one").unwrap();
            let state = bound.materialized_state().unwrap();
            if version <= 2 {
                assert!(!state.control_live);
                assert_eq!(state.jobs.len(), 1);
                assert_eq!(state.outbox.len(), 1);
            } else {
                assert!(state.control_live);
            }
            assert_eq!(
                bound
                    .conn
                    .query_row(
                        "SELECT service_instance FROM journal_identity WHERE id = 1",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .unwrap(),
                "service-one"
            );
            assert_eq!(
                bound
                    .conn
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                i64::from(JOURNAL_SCHEMA_VERSION)
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn worker_connections_enable_and_verify_connection_local_pragmas() {
        let (dir, journal) = open_tmp("worker-connection-pragmas");
        let path = dir.join("journal.db");
        drop(journal);
        let mut controller = Journal::open_for_service_instance(&path, "service-one").unwrap();
        prime_ready(&mut controller, "scope-1");
        let slot_id = slot("scope-1");
        let generation = Generation::INITIAL;
        let nonce = controller
            .issue_disk_pressure_launch("service-one", &slot_id, generation, 100)
            .unwrap();

        let worker =
            Journal::open_for_launch(&path, "service-one", &slot_id, generation, &nonce).unwrap();
        let journal_mode: String = worker
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = worker
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = worker
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        assert_eq!(synchronous, 2);
        assert_eq!(foreign_keys, 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_unbound_handle_cannot_mutate_after_service_binding() {
        let (dir, mut stale_unbound) = open_tmp("stale-unbound-after-binding");
        let path = dir.join("journal.db");
        let stale_context = stale_unbound.writer_context();
        let mut controller = Journal::open_for_service_instance(&path, "service-one").unwrap();
        let generation = prime_running_job(&mut controller, "scope-1", "job-1");
        let before_events = event_count(&controller);
        let before_state = controller.materialized_state().unwrap();
        assert_eq!(stale_unbound.materialized_state().unwrap(), before_state);
        assert_eq!(
            canonical_projection(stale_unbound.load_state().unwrap()),
            canonical_projection(before_state.clone()),
            "unbound current-schema handles may inspect but not mutate the journal"
        );

        // Model the open-then-bind race directly: this handle captured no
        // owner context before another connection bound the still-empty DB.
        let transaction = stale_unbound
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let gate_error = stale_context.begin_write(&transaction).unwrap_err();
        assert_eq!(
            gate_error.envelope.reason,
            "journal.service_instance.mismatch"
        );
        drop(transaction);

        for result in [
            stale_unbound.apply(Event::JobTerminalResult {
                job_id: job("job-1"),
                generation,
                conclusion: "success".to_owned(),
            }),
            stale_unbound.apply(Event::ControlLive),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.envelope.reason, "journal.service_instance.mismatch");
        }
        assert_eq!(
            stale_unbound.set_drain(1).unwrap_err().envelope.reason,
            "journal.service_instance.mismatch"
        );
        assert_eq!(
            stale_unbound
                .set_admission_blocked(1)
                .unwrap_err()
                .envelope
                .reason,
            "journal.service_instance.mismatch"
        );
        assert_eq!(
            stale_unbound
                .advance_unmeasurable_disk_pressure("service-one", 60, 30, 100)
                .unwrap_err()
                .envelope
                .reason,
            "journal.service_instance.mismatch"
        );
        assert_eq!(event_count(&controller), before_events);
        assert_eq!(controller.materialized_state().unwrap(), before_state);
        assert_eq!(read_drain_state(&path), Ok(None));
        assert_eq!(read_admission_state(&path), Ok(None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pressure_deadlines_survive_reopen_and_block_worker_relaunch() {
        let (dir, mut setup) = open_pressure_tmp("pressure-episode-relaunch");
        prime_ready(&mut setup, "scope-1");
        let path = dir.join("journal.db");
        drop(setup);
        let service_instance = "service-one";
        let filesystem_id = "dev:42";
        let slot_id = slot("scope-1");
        let journal = Journal::open_for_service_instance(&path, service_instance).unwrap();
        let first_nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, r#gen(), 100)
            .unwrap();
        let first_observation = observe_pressure_one(
            &journal,
            service_instance,
            filesystem_id,
            "volume-dev-42",
            &slot_id,
            r#gen(),
            &first_nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap();
        assert!(first_observation.reclaim_needed);
        let first = first_observation.episode.unwrap();
        assert_eq!(first.started_unix, 100);
        assert_eq!(first.deadline_unix, 160);
        assert_eq!(first.drain_deadline_unix, 190);
        assert!(!first.draining);
        assert!(!first.terminal);
        assert!(first.reclaim_attempted);

        drop(journal);
        let reopened = Journal::open_for_service_instance(&path, service_instance).unwrap();
        let relaunch_error = reopened
            .issue_disk_pressure_launch(service_instance, &slot_id, r#gen(), 120)
            .unwrap_err();
        assert_eq!(
            relaunch_error.envelope.reason,
            "journal.disk_pressure.launch.pressure"
        );
        let sample = [DiskPressureFilesystemSample {
            filesystem_id: filesystem_id.to_owned(),
            alias_ids: Vec::new(),
            available_bytes: Some(0),
            min_free_bytes: 10,
            volume_fingerprint: Some("volume-dev-42".to_owned()),
        }];
        reopened
            .advance_disk_pressure_roots(service_instance, &sample, 60, 30, 120)
            .unwrap();
        let after_restart = reopened
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .unwrap();
        assert_eq!(after_restart.episode_id, first.episode_id);
        assert_eq!(after_restart.started_unix, 100);
        assert_eq!(after_restart.deadline_unix, 160);
        assert_eq!(after_restart.drain_deadline_unix, 190);
        assert!(after_restart.reclaim_attempted);
        reopened
            .advance_disk_pressure_roots(service_instance, &sample, 60, 30, 160)
            .unwrap();
        let draining = reopened
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(draining.draining);
        assert!(!draining.terminal);
        assert_eq!(draining.deadline_unix, 160);
        assert_eq!(draining.drain_deadline_unix, 190);

        drop(reopened);
        let reopened =
            Journal::open_for_service_instance(dir.join("journal.db"), service_instance).unwrap();
        reopened
            .advance_disk_pressure_roots(service_instance, &sample, 60, 30, 189)
            .unwrap();
        let still_draining = reopened
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(still_draining.draining);
        assert!(!still_draining.terminal);
        drop(reopened);

        let reopened =
            Journal::open_for_service_instance(dir.join("journal.db"), service_instance).unwrap();
        reopened
            .advance_disk_pressure_roots(service_instance, &sample, 60, 30, 190)
            .unwrap();
        let terminal = reopened
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(terminal.draining);
        assert!(terminal.terminal);
        assert_eq!(terminal.deadline_unix, 160);
        assert_eq!(terminal.drain_deadline_unix, 190);
        let terminal_state = reopened.materialized_state().unwrap();
        assert_eq!(
            terminal_state
                .slots
                .iter()
                .find(|slot| slot.slot_id == slot_id)
                .unwrap()
                .phase,
            SlotPhase2::Fenced
        );
        assert!(!reopened
            .clear_disk_pressure_episode_if_healthy(
                service_instance,
                filesystem_id,
                &[],
                &terminal,
                10,
                10,
                "volume-dev-42",
                189,
            )
            .unwrap());
        let terminal_after_rollback = reopened
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(terminal_after_rollback.draining);
        assert!(terminal_after_rollback.terminal);
        assert!(reopened
            .clear_disk_pressure_episode_if_healthy(
                service_instance,
                filesystem_id,
                &[],
                &terminal_after_rollback,
                10,
                10,
                "volume-dev-42",
                191,
            )
            .unwrap());
        assert!(reopened
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn controller_creates_root_episodes_and_persists_high_water_without_workers() {
        let (_dir, journal) = open_pressure_tmp("pressure-controller-first-observation");
        let service_instance = "service-one";
        let config_root = "root:config";
        let work_root = "root:work";
        let unknown_root = "root:unknown";
        let samples = [
            DiskPressureFilesystemSample {
                filesystem_id: config_root.to_owned(),
                alias_ids: Vec::new(),
                available_bytes: Some(0),
                min_free_bytes: 10,
                volume_fingerprint: Some("volume-config".to_owned()),
            },
            DiskPressureFilesystemSample {
                filesystem_id: work_root.to_owned(),
                alias_ids: Vec::new(),
                available_bytes: Some(100),
                min_free_bytes: 10,
                volume_fingerprint: Some("volume-work".to_owned()),
            },
            DiskPressureFilesystemSample {
                filesystem_id: unknown_root.to_owned(),
                alias_ids: Vec::new(),
                available_bytes: None,
                min_free_bytes: 10,
                volume_fingerprint: None,
            },
        ];

        journal
            .advance_disk_pressure_roots(service_instance, &samples, 60, 30, 100)
            .unwrap();
        let config_episode = journal
            .disk_pressure_episode(service_instance, config_root)
            .unwrap()
            .unwrap();
        let unknown_episode = journal
            .disk_pressure_episode(service_instance, unknown_root)
            .unwrap()
            .unwrap();
        assert!(!config_episode.reclaim_attempted);
        assert_eq!(config_episode.started_unix, 100);
        assert_eq!(config_episode.deadline_unix, 160);
        assert_eq!(config_episode.drain_deadline_unix, 190);
        assert!(config_episode.volume_fingerprint.is_some());
        assert!(!unknown_episode.reclaim_attempted);
        assert_eq!(unknown_episode.volume_fingerprint, None);
        assert!(journal
            .disk_pressure_episode(service_instance, work_root)
            .unwrap()
            .is_none());

        journal
            .advance_disk_pressure_roots(service_instance, &samples, 600, 300, 120)
            .unwrap();
        let high_water = journal
            .disk_pressure_episode(service_instance, config_root)
            .unwrap()
            .unwrap();
        assert_eq!(high_water.started_unix, 100);
        assert_eq!(high_water.deadline_unix, 160);
        assert_eq!(high_water.drain_deadline_unix, 190);
        assert_eq!(high_water.last_observed_unix, 120);
        assert!(high_water.revision > config_episode.revision);

        journal
            .advance_disk_pressure_roots(service_instance, &samples, 60, 30, 110)
            .unwrap();
        let rolled_back = journal
            .disk_pressure_episode(service_instance, config_root)
            .unwrap()
            .unwrap();
        assert!(rolled_back.draining);
        assert!(rolled_back.terminal);
        assert_eq!(rolled_back.started_unix, 100);
        assert_eq!(rolled_back.deadline_unix, 160);
        assert_eq!(rolled_back.drain_deadline_unix, 190);
        assert_eq!(rolled_back.last_observed_unix, 120);
        assert!(!rolled_back.reclaim_attempted);

        let unknown_rolled_back = journal
            .disk_pressure_episode(service_instance, unknown_root)
            .unwrap()
            .unwrap();
        assert!(unknown_rolled_back.terminal);
        assert_eq!(unknown_rolled_back.deadline_unix, 160);
        assert_eq!(unknown_rolled_back.drain_deadline_unix, 190);
        assert!(!unknown_rolled_back.reclaim_attempted);
    }

    #[test]
    fn healthy_observation_at_deadline_clears_in_either_cross_connection_order() {
        for boundary in [160, 190] {
            for controller_first in [false, true] {
                let label = format!("pressure-healthy-{boundary}-{controller_first}");
                let (dir, mut controller) = open_pressure_tmp(&label);
                prime_ready(&mut controller, "scope-1");
                let service = "service-one";
                let filesystem_id = "unix-device:2a";
                let slot_id = slot("scope-1");
                let nonce = controller
                    .issue_disk_pressure_launch(service, &slot_id, r#gen(), 100)
                    .unwrap();
                let low = [DiskPressureFilesystemSample {
                    filesystem_id: filesystem_id.to_owned(),
                    alias_ids: Vec::new(),
                    available_bytes: Some(0),
                    min_free_bytes: 10,
                    volume_fingerprint: Some("volume-a".to_owned()),
                }];
                controller
                    .advance_disk_pressure_roots(service, &low, 60, 30, 100)
                    .unwrap();
                let worker = Journal::open_for_launch(
                    dir.join("journal.db"),
                    service,
                    &slot_id,
                    r#gen(),
                    &nonce,
                )
                .unwrap();
                let healthy = [DiskPressureFilesystemSample {
                    filesystem_id: filesystem_id.to_owned(),
                    alias_ids: Vec::new(),
                    available_bytes: Some(10),
                    min_free_bytes: 10,
                    volume_fingerprint: Some("volume-a".to_owned()),
                }];

                if controller_first {
                    controller
                        .advance_disk_pressure_roots(service, &healthy, 60, 30, boundary)
                        .unwrap();
                    let episode = controller
                        .disk_pressure_episode(service, filesystem_id)
                        .unwrap()
                        .unwrap();
                    assert!(!episode.draining);
                    assert!(!episode.terminal);
                    assert!(controller
                        .clear_disk_pressure_episode_if_healthy(
                            service,
                            filesystem_id,
                            &[],
                            &episode,
                            10,
                            10,
                            "volume-a",
                            boundary,
                        )
                        .unwrap());
                    worker
                        .observe_disk_pressure_roots(
                            service,
                            &slot_id,
                            r#gen(),
                            &nonce,
                            &healthy,
                            60,
                            30,
                            boundary,
                        )
                        .unwrap();
                } else {
                    let observation = worker
                        .observe_disk_pressure_roots(
                            service,
                            &slot_id,
                            r#gen(),
                            &nonce,
                            &healthy,
                            60,
                            30,
                            boundary,
                        )
                        .unwrap();
                    assert!(observation[0].1.episode.is_none());
                    controller
                        .advance_disk_pressure_roots(service, &healthy, 60, 30, boundary)
                        .unwrap();
                }
                assert!(controller
                    .disk_pressure_episode(service, filesystem_id)
                    .unwrap()
                    .is_none());
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }

    #[test]
    fn pinned_identity_merges_unknown_root_deadline_and_delays_first_reclaim() {
        let (dir, controller) = open_pressure_tmp("pressure-unpinnable-to-device");
        let service = "service-one";
        let unknown_root = "root:config";
        let filesystem_id = "unix-device:2a";
        let pinned = [DiskPressureFilesystemSample {
            filesystem_id: filesystem_id.to_owned(),
            alias_ids: vec![unknown_root.to_owned(), "root:work".to_owned()],
            available_bytes: Some(0),
            min_free_bytes: 10,
            volume_fingerprint: Some("volume-a".to_owned()),
        }];
        controller
            .advance_disk_pressure_roots(
                service,
                &[DiskPressureFilesystemSample {
                    filesystem_id: unknown_root.to_owned(),
                    alias_ids: Vec::new(),
                    available_bytes: None,
                    min_free_bytes: 10,
                    volume_fingerprint: None,
                }],
                60,
                30,
                100,
            )
            .unwrap();
        let before = controller
            .disk_pressure_episode(service, unknown_root)
            .unwrap()
            .unwrap();
        let second_connection =
            Journal::open_for_service_instance(dir.join("journal.db"), service).unwrap();
        second_connection
            .advance_disk_pressure_roots(service, &pinned, 60, 30, 120)
            .unwrap();
        let bound = controller
            .disk_pressure_episode(service, filesystem_id)
            .unwrap()
            .unwrap();
        assert_eq!(bound.started_unix, before.started_unix);
        assert_eq!(bound.deadline_unix, before.deadline_unix);
        assert_eq!(bound.drain_deadline_unix, before.drain_deadline_unix);
        assert!(!bound.reclaim_attempted);
        assert_eq!(bound.volume_fingerprint.as_deref(), Some("volume-a"));
        assert!(controller
            .disk_pressure_episode(service, unknown_root)
            .unwrap()
            .is_none());

        assert!(
            !controller
                .claim_disk_pressure_reclaim(
                    service,
                    filesystem_id,
                    &pinned[0].alias_ids,
                    &bound,
                    0,
                    10,
                    "volume-a",
                    120,
                )
                .unwrap(),
            "a late-bound alias must block controller reclaim"
        );
        assert!(
            !controller
                .clear_disk_pressure_episode_if_healthy(
                    service,
                    filesystem_id,
                    &pinned[0].alias_ids,
                    &bound,
                    10,
                    10,
                    "volume-a",
                    120,
                )
                .unwrap(),
            "a late-bound alias must block controller clearing"
        );
        assert!(controller
            .disk_pressure_episode(service, filesystem_id)
            .unwrap()
            .is_some());

        controller
            .advance_disk_pressure_roots(service, &pinned, 60, 30, 121)
            .unwrap();
        let confirmed = controller
            .disk_pressure_episode(service, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(controller
            .claim_disk_pressure_reclaim(
                service,
                filesystem_id,
                &pinned[0].alias_ids,
                &confirmed,
                0,
                10,
                "volume-a",
                121,
            )
            .unwrap());

        let mut healthy = pinned[0].clone();
        healthy.available_bytes = Some(10);
        controller
            .advance_disk_pressure_roots(service, &[healthy.clone()], 60, 30, 122)
            .unwrap();
        let clearable = controller
            .disk_pressure_episode(service, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(controller
            .clear_disk_pressure_episode_if_healthy(
                service,
                filesystem_id,
                &healthy.alias_ids,
                &clearable,
                10,
                10,
                "volume-a",
                122,
            )
            .unwrap());
        assert!(controller
            .disk_pressure_episode(service, filesystem_id)
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn changed_volume_rebinds_only_after_terminal_generation_is_safe() {
        let (dir, mut controller) = open_pressure_tmp("pressure-volume-rebind");
        prime_ready(&mut controller, "scope-1");
        let service = "service-one";
        let filesystem_id = "unix-device:2a";
        let slot_id = slot("scope-1");
        controller
            .issue_disk_pressure_launch(service, &slot_id, r#gen(), 100)
            .unwrap();
        let old_sample = [DiskPressureFilesystemSample {
            filesystem_id: filesystem_id.to_owned(),
            alias_ids: vec!["root:config".to_owned()],
            available_bytes: Some(0),
            min_free_bytes: 10,
            volume_fingerprint: Some("volume-old".to_owned()),
        }];
        controller
            .advance_disk_pressure_roots(service, &old_sample, 60, 30, 100)
            .unwrap();
        let new_sample = [DiskPressureFilesystemSample {
            filesystem_id: filesystem_id.to_owned(),
            alias_ids: vec!["root:config".to_owned()],
            available_bytes: Some(0),
            min_free_bytes: 10,
            volume_fingerprint: Some("volume-new".to_owned()),
        }];
        let second_connection =
            Journal::open_for_service_instance(dir.join("journal.db"), service).unwrap();
        second_connection
            .advance_disk_pressure_roots(service, &new_sample, 60, 30, 110)
            .unwrap();
        let terminal = controller
            .disk_pressure_episode(service, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(terminal.terminal);
        assert_eq!(terminal.volume_fingerprint.as_deref(), Some("volume-old"));
        assert_eq!(
            controller
                .materialized_state()
                .unwrap()
                .slots
                .iter()
                .find(|slot| slot.slot_id == slot_id)
                .unwrap()
                .phase,
            SlotPhase2::Fenced
        );
        assert!(controller
            .rebind_disk_pressure_episode_if_safe(
                service,
                filesystem_id,
                &["root:config".to_owned()],
                &terminal,
                Some(0),
                10,
                "volume-new",
                60,
                30,
                111,
            )
            .unwrap());
        let rebound = controller
            .disk_pressure_episode(service, filesystem_id)
            .unwrap()
            .unwrap();
        assert_eq!(rebound.started_unix, 111);
        assert_eq!(rebound.deadline_unix, 171);
        assert_eq!(rebound.drain_deadline_unix, 201);
        assert_eq!(rebound.volume_fingerprint.as_deref(), Some("volume-new"));
        assert!(!rebound.terminal);
        assert!(!rebound.reclaim_attempted);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn absent_device_episode_retires_after_complete_root_switch() {
        let (dir, mut controller) = open_pressure_tmp("pressure-root-switch");
        prime_ready(&mut controller, "scope-1");
        let service = "service-one";
        let old_id = "unix-device:old";
        let new_id = "unix-device:new";
        controller
            .advance_disk_pressure_roots(
                service,
                &[DiskPressureFilesystemSample {
                    filesystem_id: old_id.to_owned(),
                    alias_ids: vec!["root:old-config".to_owned()],
                    available_bytes: Some(0),
                    min_free_bytes: 10,
                    volume_fingerprint: Some("volume-old".to_owned()),
                }],
                60,
                30,
                100,
            )
            .unwrap();
        let second_connection =
            Journal::open_for_service_instance(dir.join("journal.db"), service).unwrap();
        second_connection
            .advance_disk_pressure_roots(
                service,
                &[DiskPressureFilesystemSample {
                    filesystem_id: new_id.to_owned(),
                    alias_ids: vec!["root:new-config".to_owned()],
                    available_bytes: Some(100),
                    min_free_bytes: 10,
                    volume_fingerprint: Some("volume-new".to_owned()),
                }],
                60,
                30,
                120,
            )
            .unwrap();
        second_connection
            .retire_unobserved_disk_pressure_episodes(
                service,
                &[new_id.to_owned(), "root:new-config".to_owned()],
                true,
            )
            .unwrap();
        assert!(controller
            .disk_pressure_episode(service, old_id)
            .unwrap()
            .is_none());
        assert!(controller
            .disk_pressure_episode(service, new_id)
            .unwrap()
            .is_none());
        assert_eq!(
            second_connection.disk_pressure_state(service).unwrap(),
            (false, false, false)
        );
        second_connection
            .advance_disk_pressure_roots(
                service,
                &[DiskPressureFilesystemSample {
                    filesystem_id: new_id.to_owned(),
                    alias_ids: vec!["root:new-config".to_owned()],
                    available_bytes: Some(0),
                    min_free_bytes: 10,
                    volume_fingerprint: Some("volume-new".to_owned()),
                }],
                60,
                30,
                110,
            )
            .unwrap();
        let rolled_back = second_connection
            .disk_pressure_episode(service, new_id)
            .unwrap()
            .unwrap();
        assert_eq!(rolled_back.started_unix, 120);
        assert!(rolled_back.terminal);
        assert_eq!(
            controller
                .materialized_state()
                .unwrap()
                .slots
                .iter()
                .find(|record| record.slot_id == slot("scope-1"))
                .unwrap()
                .phase,
            SlotPhase2::Fenced
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn occupied_slot_cannot_be_staled_without_pressure_terminal_fence() {
        let (dir, mut journal) = open_pressure_tmp("pressure-launch-assigned-slot");
        let generation = prime_running_job(&mut journal, "scope-1", "job-1");
        let slot_id = slot("scope-1");
        let slot = journal
            .materialized_state()
            .unwrap()
            .slots
            .into_iter()
            .find(|slot| slot.slot_id == slot_id)
            .unwrap();
        assert_eq!(slot.phase, SlotPhase2::Assigned);

        let nonce = journal
            .issue_disk_pressure_launch("service-one", &slot_id, generation, 100)
            .unwrap();
        assert_eq!(
            journal
                .disk_pressure_launch_nonce("service-one", &slot_id, generation)
                .unwrap()
                .as_deref(),
            Some(nonce.as_str())
        );
        let stale = journal
            .apply(Event::SlotStale {
                slot_id: slot_id.clone(),
                generation,
            })
            .unwrap();
        assert!(stale.rejected);
        assert_eq!(
            journal
                .disk_pressure_launch_nonce("service-one", &slot_id, generation)
                .unwrap()
                .as_deref(),
            Some(nonce.as_str())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pressure_episode_blocks_permit_and_launch_issue_across_connections() {
        let (dir, mut controller) = open_pressure_tmp("pressure-admission-cross-connection");
        prime_ready(&mut controller, "scope-1");
        let service = "service-one";
        let slot_id = slot("scope-1");
        let generation = Generation::INITIAL;
        let nonce = controller
            .issue_disk_pressure_launch(service, &slot_id, generation, 100)
            .unwrap();
        let worker = Journal::open_for_launch(
            dir.join("journal.db"),
            service,
            &slot_id,
            generation,
            &nonce,
        )
        .unwrap();
        observe_pressure_one(
            &worker,
            service,
            "pressure-volume",
            "volume-pressure-volume",
            &slot_id,
            generation,
            &nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap();

        let mut second_connection = Journal::open(dir.join("journal.db")).unwrap();
        let next_generation = generation.next();
        let unbound_error = second_connection
            .apply(Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: next_generation,
            })
            .unwrap_err();
        assert_eq!(
            unbound_error.envelope.reason,
            "journal.service_instance.mismatch"
        );
        assert_eq!(
            second_connection
                .materialized_state()
                .unwrap()
                .slots
                .iter()
                .find(|slot| slot.slot_id == slot_id)
                .unwrap()
                .generation,
            generation
        );
        assert!(second_connection
            .issue_disk_pressure_launch(service, &slot_id, generation, 101)
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pressure_orders_before_or_after_durable_acquisition_intent() {
        for intent_first in [true, false] {
            let label = format!("pressure-acquisition-order-{intent_first}");
            let (dir, mut setup) = open_pressure_tmp(&label);
            prime_ready(&mut setup, "scope-1");
            let path = dir.join("journal.db");
            drop(setup);

            let service = "service-one";
            let slot_id = slot("scope-1");
            let generation = r#gen();
            let mut controller = Journal::open_for_service_instance(&path, service).unwrap();
            let nonce = controller
                .issue_disk_pressure_launch(service, &slot_id, generation, 100)
                .unwrap();
            let worker =
                Journal::open_for_launch(&path, service, &slot_id, generation, &nonce).unwrap();

            if intent_first {
                let intent = controller
                    .intend_acquisition_with_disk_pressure_launch(
                        service,
                        slot_id.clone(),
                        generation,
                        &nonce,
                        job("request-before-pressure"),
                        "message-before-pressure".to_owned(),
                        "https://run.example/run".to_owned(),
                        100,
                    )
                    .unwrap();
                assert!(!intent.rejected);
            }

            observe_pressure_one(
                &worker,
                service,
                "device:pressure-order",
                "volume-pressure-order",
                &slot_id,
                generation,
                &nonce,
                0,
                10,
                60,
                30,
                100,
            )
            .unwrap();

            if !intent_first {
                let intent = controller
                    .intend_acquisition_with_disk_pressure_launch(
                        service,
                        slot_id.clone(),
                        generation,
                        &nonce,
                        job("request-after-pressure"),
                        "message-after-pressure".to_owned(),
                        "https://run.example/run".to_owned(),
                        101,
                    )
                    .unwrap();
                assert!(intent.rejected);
                let generic_intent = controller
                    .apply(Event::JobAcquisitionIntended {
                        slot_id: slot_id.clone(),
                        job_id: job("generic-after-pressure"),
                        generation,
                        message_id: "generic-after-pressure".to_owned(),
                        run_service_url: "https://run.example/run".to_owned(),
                        intended_unix: 101,
                    })
                    .unwrap();
                assert!(generic_intent.rejected);
            }

            let state = controller.materialized_state().unwrap();
            if intent_first {
                assert_eq!(state.jobs.len(), 1);
                assert_eq!(state.jobs[0].job_id, job("request-before-pressure"));
                assert!(state.jobs[0].provisional);
                assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
            } else {
                assert!(state.jobs.is_empty());
                assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn exact_pressure_acquisition_retry_is_idempotent_and_keeps_launch_nonce() {
        let (dir, mut controller) = open_pressure_tmp("pressure-acquisition-exact-retry");
        prime_ready(&mut controller, "scope-1");
        let service = "service-one";
        let slot_id = slot("scope-1");
        let generation = r#gen();
        let nonce = controller
            .issue_disk_pressure_launch(service, &slot_id, generation, 100)
            .unwrap();
        let mut worker = Journal::open_for_launch(
            dir.join("journal.db"),
            service,
            &slot_id,
            generation,
            &nonce,
        )
        .unwrap();
        let provisional = job("request-exact-retry");
        let first = worker
            .intend_acquisition_with_disk_pressure_launch(
                service,
                slot_id.clone(),
                generation,
                &nonce,
                provisional.clone(),
                "message-exact-retry".to_owned(),
                "https://run.example/run".to_owned(),
                100,
            )
            .unwrap();
        assert!(!first.rejected);
        let after_first = event_count(&worker);

        let exact_retry = worker
            .intend_acquisition_with_disk_pressure_launch(
                service,
                slot_id.clone(),
                generation,
                &nonce,
                provisional.clone(),
                "message-exact-retry".to_owned(),
                "https://run.example/run".to_owned(),
                101,
            )
            .unwrap();
        assert!(!exact_retry.rejected);
        assert_eq!(exact_retry.state.jobs.len(), 1);
        assert_eq!(event_count(&worker), after_first);
        assert_eq!(
            controller
                .issue_disk_pressure_launch(service, &slot_id, generation, 102)
                .unwrap(),
            nonce,
            "reopening the same generation must reuse its active nonce"
        );

        let wrong_message = worker
            .intend_acquisition_with_disk_pressure_launch(
                service,
                slot_id.clone(),
                generation,
                &nonce,
                provisional.clone(),
                "different-message".to_owned(),
                "https://run.example/run".to_owned(),
                103,
            )
            .unwrap();
        assert!(wrong_message.rejected);
        let wrong_url = worker
            .intend_acquisition_with_disk_pressure_launch(
                service,
                slot_id.clone(),
                generation,
                &nonce,
                provisional,
                "message-exact-retry".to_owned(),
                "https://other.example/run".to_owned(),
                104,
            )
            .unwrap();
        assert!(wrong_url.rejected);
        assert_eq!(event_count(&worker), after_first);

        assert!(controller
            .revoke_disk_pressure_launch(service, &slot_id, generation)
            .unwrap());
        assert!(
            controller
                .issue_disk_pressure_launch(service, &slot_id, generation, 105)
                .is_err(),
            "a provisional row pins its nonce until resolved or abandoned"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn typed_gone_abandon_is_exact_and_orders_with_terminal_revocation() {
        for terminal_first in [false, true] {
            let label = format!("pressure-acquisition-gone-{terminal_first}");
            let (dir, mut controller) = open_pressure_tmp(&label);
            prime_ready(&mut controller, "scope-1");
            let service = "service-one";
            let slot_id = slot("scope-1");
            let generation = r#gen();
            let nonce = controller
                .issue_disk_pressure_launch(service, &slot_id, generation, 100)
                .unwrap();
            let mut worker = Journal::open_for_launch(
                dir.join("journal.db"),
                service,
                &slot_id,
                generation,
                &nonce,
            )
            .unwrap();
            let provisional = job("request-typed-gone");
            let run_service_url = "https://run.example/run";
            let message_id = "message-typed-gone";
            assert!(
                !worker
                    .intend_acquisition_with_disk_pressure_launch(
                        service,
                        slot_id.clone(),
                        generation,
                        &nonce,
                        provisional.clone(),
                        message_id.to_owned(),
                        run_service_url.to_owned(),
                        100,
                    )
                    .unwrap()
                    .rejected
            );

            if terminal_first {
                observe_pressure_one(
                    &worker,
                    service,
                    "device:typed-gone",
                    "volume-typed-gone",
                    &slot_id,
                    generation,
                    &nonce,
                    0,
                    10,
                    60,
                    30,
                    100,
                )
                .unwrap();
                controller
                    .advance_disk_pressure_roots(
                        service,
                        &[DiskPressureFilesystemSample {
                            filesystem_id: "device:typed-gone".to_owned(),
                            alias_ids: Vec::new(),
                            available_bytes: Some(0),
                            min_free_bytes: 10,
                            volume_fingerprint: Some("volume-typed-gone".to_owned()),
                        }],
                        60,
                        30,
                        190,
                    )
                    .unwrap();
            }

            if terminal_first {
                assert!(
                    worker
                        .apply(Event::JobAcquisitionLost {
                            job_id: provisional.clone(),
                            generation,
                            reason: "typed gone".to_owned(),
                        })
                        .is_err(),
                    "the generic stale-worker mutation path stays fenced after terminal revoke"
                );
            }
            let wrong_message = worker
                .abandon_acquisition_after_disk_pressure_terminal(
                    service,
                    slot_id.clone(),
                    generation,
                    &nonce,
                    provisional.clone(),
                    "wrong-message",
                    run_service_url,
                    "typed gone".to_owned(),
                )
                .unwrap_err();
            assert_eq!(
                wrong_message.envelope.reason,
                "journal.acquisition.abandon.fenced"
            );
            let wrong_url = worker
                .abandon_acquisition_after_disk_pressure_terminal(
                    service,
                    slot_id.clone(),
                    generation,
                    &nonce,
                    provisional.clone(),
                    message_id,
                    "https://wrong.example/run",
                    "typed gone".to_owned(),
                )
                .unwrap_err();
            assert_eq!(
                wrong_url.envelope.reason,
                "journal.acquisition.abandon.fenced"
            );
            let abandoned = worker
                .abandon_acquisition_after_disk_pressure_terminal(
                    service,
                    slot_id.clone(),
                    generation,
                    &nonce,
                    provisional,
                    message_id,
                    run_service_url,
                    "typed gone".to_owned(),
                )
                .unwrap();
            assert!(!abandoned.rejected);
            let state = controller.materialized_state().unwrap();
            assert!(state.jobs.is_empty());
            if terminal_first {
                assert_eq!(state.slots[0].phase, SlotPhase2::Fenced);
                assert_eq!(state.advertised_capacity(), 0);
            } else {
                assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
                assert_eq!(state.advertised_capacity(), 1);
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn terminal_pressure_revocation_allows_only_exact_acquire_response_handoff() {
        let (dir, mut setup) = open_pressure_tmp("pressure-acquisition-response-handoff");
        prime_ready(&mut setup, "scope-1");
        let path = dir.join("journal.db");
        drop(setup);

        let service = "service-one";
        let slot_id = slot("scope-1");
        let generation = r#gen();
        let filesystem_id = "device:response-handoff";
        let run_service_url = "https://run.example/run";
        let mut controller = Journal::open_for_service_instance(&path, service).unwrap();
        let nonce = controller
            .issue_disk_pressure_launch(service, &slot_id, generation, 100)
            .unwrap();
        let mut worker =
            Journal::open_for_launch(&path, service, &slot_id, generation, &nonce).unwrap();
        let intent = worker
            .intend_acquisition_with_disk_pressure_launch(
                service,
                slot_id.clone(),
                generation,
                &nonce,
                job("request-handoff"),
                "message-handoff".to_owned(),
                run_service_url.to_owned(),
                100,
            )
            .unwrap();
        assert!(!intent.rejected);
        observe_pressure_one(
            &worker,
            service,
            filesystem_id,
            "volume-response-handoff",
            &slot_id,
            generation,
            &nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap();

        let terminal = [DiskPressureFilesystemSample {
            filesystem_id: filesystem_id.to_owned(),
            alias_ids: Vec::new(),
            available_bytes: Some(0),
            min_free_bytes: 10,
            volume_fingerprint: Some("volume-response-handoff".to_owned()),
        }];
        controller
            .advance_disk_pressure_roots(service, &terminal, 60, 30, 190)
            .unwrap();
        assert_eq!(
            controller
                .disk_pressure_launch_nonce(service, &slot_id, generation)
                .unwrap(),
            None
        );
        assert_eq!(
            controller
                .materialized_state()
                .unwrap()
                .slots
                .iter()
                .find(|slot| slot.slot_id == slot_id)
                .unwrap()
                .phase,
            SlotPhase2::Fenced
        );

        let wrong_url = worker
            .resolve_acquisition_response(
                service,
                slot_id.clone(),
                generation,
                &nonce,
                job("request-handoff"),
                "message-handoff",
                job("acquired-handoff"),
                "https://other.example/run",
                "plan-handoff",
            )
            .unwrap_err();
        assert_eq!(
            wrong_url.envelope.reason,
            "journal.acquisition.response.fenced"
        );
        let resolved = worker
            .resolve_acquisition_response(
                service,
                slot_id.clone(),
                generation,
                &nonce,
                job("request-handoff"),
                "message-handoff",
                job("acquired-handoff"),
                run_service_url,
                "plan-handoff",
            )
            .unwrap();
        assert!(!resolved.rejected);
        assert!(resolved.commands.is_empty());

        let unauthorized_confirmation = controller
            .apply(Event::JobOwned {
                job_id: job("acquired-handoff"),
                slot_id: slot_id.clone(),
                attempt: 1,
                generation,
                worker: "velnor-job@acquired-handoff".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        assert!(unauthorized_confirmation.rejected);
        let confirmed = controller
            .confirm_acquisition_after_disk_pressure_terminal(
                service,
                slot_id.clone(),
                generation,
                job("acquired-handoff"),
                "plan-handoff",
                run_service_url,
            )
            .unwrap();
        assert!(!confirmed.rejected);

        let state = controller.materialized_state().unwrap();
        assert_eq!(state.slots[0].phase, SlotPhase2::Fenced);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("acquired-handoff"));
        assert_eq!(state.jobs[0].plan_id, "plan-handoff");
        assert_eq!(state.jobs[0].run_service_url, run_service_url);
        assert!(!state.jobs[0].provisional);
        assert_eq!(state.advertised_capacity(), 0);
        assert_eq!(
            canonical_projection(controller.load_state().unwrap()),
            canonical_projection(state),
            "terminal handoff confirmation must replay with the slot still fenced"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn journal_identity_rejects_a_second_service_on_the_same_fleet_database() {
        let (dir, mut setup) = open_pressure_tmp("pressure-single-service-database");
        prime_ready(&mut setup, "scope-1");
        let path = dir.join("journal.db");
        drop(setup);

        let service_a = "service-one";
        let service_b = "service-two";
        let slot_id = slot("scope-1");
        let mut journal = Journal::open_for_service_instance(&path, service_a).unwrap();
        let nonce = journal
            .issue_disk_pressure_launch(service_a, &slot_id, r#gen(), 100)
            .unwrap();
        observe_pressure_one(
            &journal,
            service_a,
            "device:one",
            "volume-one-uuid",
            &slot_id,
            r#gen(),
            &nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap();

        assert!(
            !journal
                .apply(Event::Dependency {
                    github_reachable: false,
                })
                .unwrap()
                .rejected
        );
        let owner: String = journal
            .conn
            .query_row(
                "SELECT service_instance FROM journal_identity WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(owner, service_a, "materialization keeps the durable owner");

        let second_open = Journal::open_for_service_instance(&path, service_b).unwrap_err();
        assert_eq!(
            second_open.envelope.reason,
            "journal.service_instance.mismatch"
        );
        let second_launch = journal
            .issue_disk_pressure_launch(service_b, &slot_id, r#gen(), 101)
            .unwrap_err();
        assert_eq!(
            second_launch.envelope.reason,
            "journal.service_instance.mismatch"
        );

        let mut unbound_admin = Journal::open(&path).unwrap();
        let unbound_write = unbound_admin
            .apply(Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: r#gen().next(),
            })
            .unwrap_err();
        assert_eq!(
            unbound_write.envelope.reason,
            "journal.service_instance.mismatch"
        );
        let foreign_read = unbound_admin
            .disk_pressure_episode(service_b, "device:one")
            .unwrap_err();
        assert_eq!(
            foreign_read.envelope.reason,
            "journal.service_instance.mismatch"
        );
        let foreign_launches: i64 = journal
            .conn
            .query_row(
                "SELECT COUNT(*) FROM disk_pressure_launches WHERE service_instance = ?1",
                [service_b],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(foreign_launches, 0);
        assert!(journal
            .disk_pressure_episode(service_a, "device:one")
            .unwrap()
            .is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminal_pressure_fences_occupied_generation_atomically() {
        let (dir, mut controller) = open_pressure_tmp("pressure-terminal-occupied");
        let generation = prime_running_job(&mut controller, "scope-1", "job-1");
        let service = "service-one";
        let slot_id = slot("scope-1");
        let nonce = controller
            .issue_disk_pressure_launch(service, &slot_id, generation, 100)
            .unwrap();
        let mut stale_worker = Journal::open_for_launch(
            dir.join("journal.db"),
            service,
            &slot_id,
            generation,
            &nonce,
        )
        .unwrap();

        let sample = [DiskPressureFilesystemSample {
            filesystem_id: "pressure-volume".to_owned(),
            alias_ids: Vec::new(),
            available_bytes: Some(0),
            min_free_bytes: 10,
            volume_fingerprint: Some("volume-uuid".to_owned()),
        }];
        controller
            .advance_disk_pressure_roots(service, &sample, 0, 5, 100)
            .unwrap();
        controller
            .advance_disk_pressure_roots(service, &sample, 0, 5, 105)
            .unwrap();

        let state = controller.materialized_state().unwrap();
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.slot_id == slot_id)
            .unwrap();
        assert_eq!(slot.generation, generation);
        assert_eq!(slot.phase, SlotPhase2::Fenced);
        assert!(state
            .jobs
            .iter()
            .any(|job| job.slot_id == slot_id && job.phase.occupies_slot()));
        assert!(controller
            .disk_pressure_launch_nonce(service, &slot_id, generation)
            .unwrap()
            .is_none());
        let stale_write = stale_worker.apply(Event::JobStarted {
            job_id: JobId("job-1".to_owned()),
            generation,
        });
        assert!(stale_write.is_err());
        let next_permit = controller
            .apply(Event::PermitReserved {
                slot_id,
                generation: generation.next(),
            })
            .unwrap();
        assert!(next_permit.rejected);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminal_pressure_revocation_and_slot_stale_fence_bound_writers() {
        let (dir, mut setup) = open_pressure_tmp("pressure-launch-terminal-revoke");
        prime_ready(&mut setup, "scope-1");
        let path = dir.join("journal.db");
        drop(setup);
        let service = "service-one";
        let mut journal = Journal::open_for_service_instance(&path, service).unwrap();
        let generation = Generation::INITIAL;
        let slot_id = slot("scope-1");
        let nonce = journal
            .issue_disk_pressure_launch(service, &slot_id, generation, 100)
            .unwrap();
        let mut stale_worker =
            Journal::open_for_launch(&path, service, &slot_id, generation, &nonce).unwrap();

        assert!(journal
            .revoke_disk_pressure_launch(service, &slot_id, generation)
            .unwrap());
        assert!(journal
            .disk_pressure_launch_nonce(service, &slot_id, generation)
            .unwrap()
            .is_none());
        assert!(stale_worker
            .apply(Event::SlotHeartbeat {
                slot_id: slot_id.clone(),
                generation,
                pid: 17,
            })
            .is_err());

        let replacement = journal
            .issue_disk_pressure_launch(service, &slot_id, generation, 101)
            .unwrap();
        let mut fenced_worker =
            Journal::open_for_launch(&path, service, &slot_id, generation, &replacement).unwrap();
        assert!(
            !journal
                .apply(Event::SlotStale {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(journal
            .disk_pressure_launch_nonce(service, &slot_id, generation)
            .unwrap()
            .is_none());
        assert!(fenced_worker
            .apply(Event::SlotHeartbeat {
                slot_id,
                generation,
                pid: 18,
            })
            .is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stale_bound_journal_cannot_mutate_after_same_generation_nonce_replacement() {
        let (dir, mut controller) = open_pressure_tmp("pressure-bound-journal-stale-nonce");
        prime_ready(&mut controller, "scope-1");
        let service_instance = "service-one";
        let slot_id = slot("scope-1");
        let generation = r#gen();
        let first_nonce = controller
            .issue_disk_pressure_launch(service_instance, &slot_id, generation, 100)
            .unwrap();
        let mut stale_worker = Journal::open_for_launch(
            dir.join("journal.db"),
            service_instance,
            &slot_id,
            generation,
            &first_nonce,
        )
        .unwrap();
        let replacement_nonce = controller
            .issue_disk_pressure_launch(service_instance, &slot_id, generation, 101)
            .unwrap();
        assert_ne!(first_nonce, replacement_nonce);

        let error = stale_worker
            .apply(Event::Dependency {
                github_reachable: false,
            })
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.disk_pressure.launch.fenced");
        assert!(controller.materialized_state().unwrap().github_reachable);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unmeasurable_controller_advances_existing_deadline_stages() {
        let (_dir, mut journal) = open_pressure_tmp("pressure-unmeasurable-controller-advance");
        prime_ready(&mut journal, "scope-1");
        let service_instance = "service-one";
        let slot_id = slot("scope-1");
        let nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, r#gen(), 100)
            .unwrap();
        observe_pressure_one(
            &journal,
            service_instance,
            "config-device",
            "config-volume",
            &slot_id,
            r#gen(),
            &nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap();

        assert_eq!(
            journal
                .advance_unmeasurable_disk_pressure(service_instance, 60, 30, 160)
                .unwrap(),
            (true, false),
            "unknown capacity advances to drain but does not clear or skip it"
        );
        let draining = journal
            .disk_pressure_episode(service_instance, "config-device")
            .unwrap()
            .unwrap();
        assert!(draining.draining);
        assert!(!draining.terminal);

        assert_eq!(
            journal
                .advance_unmeasurable_disk_pressure(service_instance, 60, 30, 190)
                .unwrap(),
            (true, true),
            "unknown capacity reaches terminal at the persisted drain cutoff"
        );
        let terminal = journal
            .disk_pressure_episode(service_instance, "config-device")
            .unwrap()
            .unwrap();
        assert!(terminal.draining);
        assert!(terminal.terminal);
    }

    #[test]
    fn unmeasurable_advance_ignores_retired_observation_history() {
        let (_dir, mut journal) = open_pressure_tmp("pressure-unmeasurable-retired-history");
        prime_ready(&mut journal, "scope-1");
        let service_instance = "service-one";
        let filesystem_id = "unix-device:retired";
        let new_filesystem_id = "unix-device:current";
        let volume_fingerprint = "volume-retired-uuid";
        let slot_id = slot("scope-1");
        let old_generation = r#gen();
        let nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, old_generation, 100)
            .unwrap();
        let low = observe_pressure_one(
            &journal,
            service_instance,
            filesystem_id,
            volume_fingerprint,
            &slot_id,
            old_generation,
            &nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap()
        .episode
        .unwrap();

        let current_root = [DiskPressureFilesystemSample {
            filesystem_id: new_filesystem_id.to_owned(),
            alias_ids: Vec::new(),
            available_bytes: Some(20),
            min_free_bytes: 10,
            volume_fingerprint: Some("volume-current-uuid".to_owned()),
        }];
        journal
            .advance_disk_pressure_roots(service_instance, &current_root, 60, 30, 110)
            .unwrap();
        assert_eq!(
            journal
                .disk_pressure_episode(service_instance, filesystem_id)
                .unwrap()
                .as_ref()
                .map(|episode| episode.episode_id.as_str()),
            Some(low.episode_id.as_str())
        );
        journal
            .retire_unobserved_disk_pressure_episodes(
                service_instance,
                &[new_filesystem_id.to_owned()],
                true,
            )
            .unwrap();
        assert!(journal
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .is_none());

        let old_observation_before: (String, i64, i64) = journal
            .conn
            .query_row(
                "SELECT volume_fingerprint, identity_confirmed, last_observed_unix
                 FROM disk_pressure_observations
                 WHERE service_instance = ?1 AND filesystem_id = ?2",
                params![service_instance, filesystem_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            old_observation_before,
            (volume_fingerprint.to_owned(), 1, 100)
        );
        let current_observation_before: (String, i64, i64) = journal
            .conn
            .query_row(
                "SELECT volume_fingerprint, identity_confirmed, last_observed_unix
                 FROM disk_pressure_observations
                 WHERE service_instance = ?1 AND filesystem_id = ?2",
                params![service_instance, new_filesystem_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            current_observation_before,
            ("volume-current-uuid".to_owned(), 1, 110)
        );

        let new_generation = old_generation.next();
        for event in [
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: new_generation,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation: new_generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation: new_generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation: new_generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation: new_generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation: new_generation,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let current_nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, new_generation, 120)
            .unwrap();

        assert_eq!(
            journal
                .advance_unmeasurable_disk_pressure(service_instance, 60, 30, 700)
                .unwrap(),
            (false, false)
        );
        assert_eq!(
            journal
                .disk_pressure_launch_nonce(service_instance, &slot_id, new_generation)
                .unwrap()
                .as_deref(),
            Some(current_nonce.as_str()),
            "history-only identities cannot revoke the current launch"
        );
        assert!(journal
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .is_none());
        let old_observation_after: (String, i64, i64) = journal
            .conn
            .query_row(
                "SELECT volume_fingerprint, identity_confirmed, last_observed_unix
                 FROM disk_pressure_observations
                 WHERE service_instance = ?1 AND filesystem_id = ?2",
                params![service_instance, filesystem_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(old_observation_after, old_observation_before);
        let current_observation_after: (String, i64, i64) = journal
            .conn
            .query_row(
                "SELECT volume_fingerprint, identity_confirmed, last_observed_unix
                 FROM disk_pressure_observations
                 WHERE service_instance = ?1 AND filesystem_id = ?2",
                params![service_instance, new_filesystem_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(current_observation_after, current_observation_before);
        assert_eq!(
            journal
                .materialized_state()
                .unwrap()
                .slots
                .iter()
                .find(|slot| slot.slot_id == slot_id)
                .unwrap()
                .phase,
            SlotPhase2::Ready,
            "history-only identities cannot recreate pressure and fence a slot"
        );
    }

    #[test]
    fn pressure_clock_rollback_latches_terminal_until_healthy_cas() {
        let (dir, mut journal) = open_pressure_tmp("pressure-clock-rollback");
        prime_ready(&mut journal, "scope-1");
        let service_instance = "service-one";
        let filesystem_id = "dev:42";
        let slot_id = slot("scope-1");
        let nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, r#gen(), 100)
            .unwrap();
        observe_pressure_one(
            &journal,
            service_instance,
            filesystem_id,
            "volume-dev-42",
            &slot_id,
            r#gen(),
            &nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap();
        let rolled_back = observe_pressure_one(
            &journal,
            service_instance,
            filesystem_id,
            "volume-dev-42",
            &slot_id,
            r#gen(),
            &nonce,
            0,
            10,
            600,
            30,
            90,
        )
        .unwrap()
        .episode
        .unwrap();
        assert!(rolled_back.terminal);
        assert_eq!(rolled_back.deadline_unix, 160);
        assert_eq!(rolled_back.drain_deadline_unix, 190);
        assert!(rolled_back.draining);
        assert!(!journal
            .clear_disk_pressure_episode_if_healthy(
                service_instance,
                filesystem_id,
                &[],
                &rolled_back,
                10,
                10,
                "volume-dev-42",
                90,
            )
            .unwrap());
        let terminal = journal
            .disk_pressure_episode(service_instance, filesystem_id)
            .unwrap()
            .unwrap();
        assert!(terminal.terminal);
        assert!(journal
            .clear_disk_pressure_episode_if_healthy(
                service_instance,
                filesystem_id,
                &[],
                &terminal,
                10,
                10,
                "volume-dev-42",
                101,
            )
            .unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pressure_root_batch_rolls_back_all_reclaim_claims_on_corrupt_root() {
        let (_dir, mut journal) = open_pressure_tmp("pressure-root-batch-atomic");
        prime_ready(&mut journal, "scope-1");
        let service_instance = "service-one";
        let slot_id = slot("scope-1");
        let generation = r#gen();
        let nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, generation, 100)
            .unwrap();

        let transaction = journal.conn.unchecked_transaction().unwrap();
        begin_journal_write_gate(&transaction).unwrap();
        transaction
            .execute(
                "INSERT INTO disk_pressure_episodes (
                     service_instance, filesystem_id, episode_id, started_unix, deadline_unix,
                     drain_deadline_unix, last_observed_unix, reclaim_attempted, revision,
                     draining, terminal
                 ) VALUES (?1, ?2, ?3, -1, 160, 190, 100, 1, 1, 0, 0)",
                params![service_instance, "work-device", "corrupt-episode"],
            )
            .unwrap();
        end_journal_write_gate(&transaction).unwrap();
        transaction.commit().unwrap();

        let error = journal
            .observe_disk_pressure_roots(
                service_instance,
                &slot_id,
                generation,
                &nonce,
                &[
                    DiskPressureFilesystemSample {
                        filesystem_id: "config-device".to_owned(),
                        alias_ids: Vec::new(),
                        available_bytes: Some(0),
                        min_free_bytes: 10,
                        volume_fingerprint: Some("config-volume".to_owned()),
                    },
                    DiskPressureFilesystemSample {
                        filesystem_id: "work-device".to_owned(),
                        alias_ids: Vec::new(),
                        available_bytes: Some(0),
                        min_free_bytes: 10,
                        volume_fingerprint: Some("work-volume".to_owned()),
                    },
                ],
                60,
                30,
                100,
            )
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.disk_pressure.state.invalid");
        assert!(journal
            .disk_pressure_episode(service_instance, "config-device")
            .unwrap()
            .is_none());
    }

    #[test]
    fn pressure_episodes_are_independent_per_filesystem() {
        let (_dir, mut journal) = open_pressure_tmp("pressure-multiple-filesystems");
        prime_ready(&mut journal, "scope-1");
        let service_instance = "service-one";
        let slot_id = slot("scope-1");
        let generation = r#gen();
        let nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, generation, 100)
            .unwrap();
        let config_episode = observe_pressure_one(
            &journal,
            service_instance,
            "config-device",
            "config-volume",
            &slot_id,
            generation,
            &nonce,
            0,
            10,
            60,
            30,
            100,
        )
        .unwrap()
        .episode
        .unwrap();
        let work_episode = observe_pressure_one(
            &journal,
            service_instance,
            "work-device",
            "work-volume",
            &slot_id,
            generation,
            &nonce,
            0,
            10,
            60,
            30,
            110,
        )
        .unwrap()
        .episode
        .unwrap();
        assert_ne!(config_episode.episode_id, work_episode.episode_id);
        assert_eq!(config_episode.deadline_unix, 160);
        assert_eq!(work_episode.deadline_unix, 170);

        let cleared_config = observe_pressure_one(
            &journal,
            service_instance,
            "config-device",
            "config-volume",
            &slot_id,
            generation,
            &nonce,
            10,
            10,
            60,
            30,
            120,
        )
        .unwrap();
        assert!(cleared_config.cleared);
        assert!(journal
            .disk_pressure_episode(service_instance, "config-device")
            .unwrap()
            .is_none());
        let still_low_work = journal
            .disk_pressure_episode(service_instance, "work-device")
            .unwrap()
            .unwrap();
        assert_eq!(still_low_work.episode_id, work_episode.episode_id);
        assert_eq!(still_low_work.deadline_unix, work_episode.deadline_unix);
    }

    #[test]
    fn config_root_episode_survives_relaunch_while_work_root_is_healthy() {
        let (dir, mut setup) = open_pressure_tmp("pressure-config-root-relaunch");
        prime_ready(&mut setup, "scope-1");
        let path = dir.join("journal.db");
        drop(setup);
        let service_instance = "service-one";
        let slot_id = slot("scope-1");
        let generation = r#gen();
        let journal = Journal::open_for_service_instance(&path, service_instance).unwrap();
        let first_nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, generation, 100)
            .unwrap();
        let first_observations = journal
            .observe_disk_pressure_roots(
                service_instance,
                &slot_id,
                generation,
                &first_nonce,
                &[
                    DiskPressureFilesystemSample {
                        filesystem_id: "config-device".to_owned(),
                        alias_ids: Vec::new(),
                        available_bytes: Some(0),
                        min_free_bytes: 10,
                        volume_fingerprint: Some("config-volume".to_owned()),
                    },
                    DiskPressureFilesystemSample {
                        filesystem_id: "work-device".to_owned(),
                        alias_ids: Vec::new(),
                        available_bytes: Some(10),
                        min_free_bytes: 10,
                        volume_fingerprint: Some("work-volume".to_owned()),
                    },
                ],
                60,
                30,
                100,
            )
            .unwrap();
        assert!(first_observations[0].1.reclaim_needed);
        let first_config = first_observations[0].1.episode.as_ref().unwrap();
        assert!(first_observations[1].1.episode.is_none());

        drop(journal);
        let reopened = Journal::open_for_service_instance(&path, service_instance).unwrap();
        let relaunch_error = reopened
            .issue_disk_pressure_launch(service_instance, &slot_id, generation, 120)
            .unwrap_err();
        assert_eq!(
            relaunch_error.envelope.reason,
            "journal.disk_pressure.launch.pressure"
        );
        reopened
            .advance_disk_pressure_roots(
                service_instance,
                &[
                    DiskPressureFilesystemSample {
                        filesystem_id: "config-device".to_owned(),
                        alias_ids: Vec::new(),
                        available_bytes: Some(0),
                        min_free_bytes: 10,
                        volume_fingerprint: Some("config-volume".to_owned()),
                    },
                    DiskPressureFilesystemSample {
                        filesystem_id: "work-device".to_owned(),
                        alias_ids: Vec::new(),
                        available_bytes: Some(10),
                        min_free_bytes: 10,
                        volume_fingerprint: Some("work-volume".to_owned()),
                    },
                ],
                600,
                30,
                120,
            )
            .unwrap();
        let config_episode = reopened
            .disk_pressure_episode(service_instance, "config-device")
            .unwrap()
            .unwrap();
        assert_eq!(config_episode.episode_id, first_config.episode_id);
        assert_eq!(config_episode.started_unix, 100);
        assert_eq!(config_episode.deadline_unix, 160);
        assert!(reopened
            .disk_pressure_episode(service_instance, "work-device")
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pressure_launch_fence_survives_slot_generation_reuse() {
        let (dir, mut setup) = open_pressure_tmp("pressure-generation-reuse");
        prime_ready(&mut setup, "scope-1");
        let service_instance = "service-one";
        let path = dir.join("journal.db");
        drop(setup);
        let mut journal = Journal::open_for_service_instance(&path, service_instance).unwrap();
        let filesystem_id = "dev:42";
        let slot_id = slot("scope-1");
        let old_generation = r#gen();
        let old_nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, old_generation, 100)
            .unwrap();

        assert!(
            !journal
                .apply(Event::SlotStale {
                    slot_id: slot_id.clone(),
                    generation: old_generation,
                })
                .unwrap()
                .rejected
        );
        let generation = old_generation.next();
        for event in [
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let new_nonce = journal
            .issue_disk_pressure_launch(service_instance, &slot_id, generation, 120)
            .unwrap();
        assert_ne!(old_nonce, new_nonce);

        let stale = observe_pressure_one(
            &journal,
            service_instance,
            filesystem_id,
            "volume-dev-42",
            &slot_id,
            old_generation,
            &old_nonce,
            0,
            10,
            60,
            30,
            130,
        )
        .unwrap_err();
        assert_eq!(stale.envelope.reason, "journal.disk_pressure.launch.fenced");
        assert!(
            observe_pressure_one(
                &journal,
                service_instance,
                filesystem_id,
                "volume-dev-42",
                &slot_id,
                generation,
                &new_nonce,
                0,
                10,
                60,
                30,
                130,
            )
            .unwrap()
            .reclaim_needed
        );
    }

    #[test]
    fn schema8_eventful_upgrade_backfills_acquisition_generation_atomically() {
        let (dir, mut journal) = open_tmp("schema8-eventful-acquisition-backfill");
        prime_provisional(&mut journal, "scope-1", "request-1");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
        )
        .unwrap();
        conn.execute(
            "UPDATE events SET generation = 0 WHERE kind = 'job_acquisition_intended'",
            [],
        )
        .unwrap();
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 8u32).unwrap();
        drop(conn);

        let upgraded = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        let generation: i64 = upgraded
            .conn
            .query_row(
                "SELECT generation FROM events WHERE kind = 'job_acquisition_intended'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(generation, 1);
        assert_eq!(
            upgraded.load_state().unwrap().jobs[0].job_id,
            job("request-1")
        );
        assert_eq!(
            upgraded
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            i64::from(JOURNAL_SCHEMA_VERSION)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema8_acquisition_generation_mismatch_rolls_back_without_stamping() {
        let (dir, mut journal) = open_tmp("schema8-acquisition-backfill-rollback");
        prime_provisional(&mut journal, "scope-1", "request-1");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
        )
        .unwrap();
        conn.execute(
            "UPDATE events SET generation = 2 WHERE kind = 'job_acquisition_intended'",
            [],
        )
        .unwrap();
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 8u32).unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.metadata.mismatch");
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            8
        );
        assert_eq!(
            conn.query_row(
                "SELECT generation FROM events WHERE kind = 'job_acquisition_intended'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            2
        );
        assert!(load_replay_baseline(&conn).unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema8_eventless_nonempty_state_fails_closed_without_stamping() {
        let (dir, mut journal) = open_tmp("schema8-eventless-nonempty");
        assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        conn.pragma_update(None, "user_version", 8u32).unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.provenance");
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            8
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema8_migration_rejects_nonacquisition_checksum_before_stamping() {
        let (dir, mut journal) = open_tmp("schema8-migration-checksum");
        journal.apply(Event::ControlLive).unwrap();
        let path = dir.join("journal.db");
        drop(journal);
        demote_eventful_journal_to_v8(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE events SET checksum = 'tampered' WHERE kind = 'control_live'",
            [],
        )
        .unwrap();
        drop(conn);

        assert_schema8_migration_rejected(&path, "journal.checksum.mismatch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema8_migration_rejects_unknown_event_before_stamping() {
        let (dir, mut journal) = open_tmp("schema8-migration-unknown-event");
        journal.apply(Event::ControlLive).unwrap();
        let path = dir.join("journal.db");
        drop(journal);
        demote_eventful_journal_to_v8(&path);
        let payload = r#"{"type":"future_envelope","x":1}"#;
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE events SET kind = 'future_envelope', payload = ?1, checksum = ?2 WHERE id = 1",
            params![payload, payload_checksum(payload.as_bytes())],
        )
        .unwrap();
        drop(conn);

        assert_schema8_migration_rejected(&path, "journal.event.unknown");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema8_migration_rejects_reducer_event_before_stamping() {
        let (dir, mut journal) = open_tmp("schema8-migration-reducer-rejection");
        journal.apply(Event::ControlLive).unwrap();
        let path = dir.join("journal.db");
        drop(journal);
        demote_eventful_journal_to_v8(&path);
        let event = Event::SlotStale {
            slot_id: slot("scope-1"),
            generation: r#gen().next(),
        };
        let payload = serde_json::to_string(&event).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE events
             SET generation = ?1, kind = ?2, payload = ?3, checksum = ?4
             WHERE id = 1",
            params![
                event_generation(&event).0 as i64,
                event_kind(&event),
                payload,
                payload_checksum(payload.as_bytes()),
            ],
        )
        .unwrap();
        drop(conn);

        assert_schema8_migration_rejected(&path, "journal.event.rejected");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema8_migration_rejects_replay_materialized_drift_before_stamping() {
        let (dir, mut journal) = open_tmp("schema8-migration-projection-drift");
        journal.apply(Event::ControlLive).unwrap();
        let path = dir.join("journal.db");
        drop(journal);
        demote_eventful_journal_to_v8(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute("UPDATE meta SET value = '0' WHERE key = 'control_live'", [])
            .unwrap();
        drop(conn);

        assert_schema8_migration_rejected(&path, "journal.materialized.replay.mismatch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema11_sql_fence_blocks_an_already_open_v10_writer() {
        let (dir, mut journal) = open_tmp("schema11-already-open-v10-writer");
        // Seed one row in every guarded table through the current writer. The stale connection must then be unable to exercise any
        // INSERT, UPDATE, or DELETE path, including rows an empty fixture
        // would not visit for UPDATE/DELETE triggers.
        let generation = prime_running_job(&mut journal, "scope-1", "job-1");
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation,
                    payload_sha256: "payload".into(),
                })
                .unwrap()
                .rejected
        );
        let path = dir.join("journal.db");
        drop(journal);

        let setup = Connection::open(&path).unwrap();
        for (name, _, _) in JOURNAL_WRITE_FENCE_TRIGGERS {
            setup
                .execute_batch(&format!("DROP TRIGGER IF EXISTS {name};"))
                .unwrap();
        }
        setup.execute_batch("DROP TABLE journal_identity;").unwrap();
        setup
            .execute(
                "INSERT INTO disk_pressure_episodes (
                     service_instance, filesystem_id, episode_id, started_unix,
                     deadline_unix, drain_deadline_unix, last_observed_unix,
                     reclaim_attempted, revision, draining, terminal
                 ) VALUES ('fence-test', 'dev:42', 'episode-1', 100, 160, 460, 100, 1, 1, 0, 0)",
                [],
            )
            .unwrap();
        setup
            .execute(
                "INSERT INTO disk_pressure_launches (
                     service_instance, slot_id, generation, launch_nonce, issued_unix, active
                 ) VALUES ('fence-test', 'scope-2', 1, 'launch-1', 100, 1)",
                [],
            )
            .unwrap();
        setup
            .execute(
                "INSERT INTO disk_pressure_observations (
                     service_instance, filesystem_id, volume_fingerprint,
                     identity_confirmed, last_observed_unix
                 ) VALUES ('fence-test', 'dev:42', 'volume-42', 1, 100)",
                [],
            )
            .unwrap();
        // Model the v10 writer's 24 triggers: all pressure/event/materialized
        // rows are fenced, but it predates the identity table.
        for (name, table, operation) in JOURNAL_WRITE_FENCE_TRIGGERS[..24].iter().copied() {
            setup
                .execute_batch(&journal_write_fence_trigger_sql(name, table, operation))
                .unwrap();
        }
        setup.pragma_update(None, "user_version", 10u32).unwrap();
        drop(setup);

        // This connection represents a v10 process that opened before the
        // migration and therefore cannot be protected by a fresh-read
        // `PRAGMA user_version` check.
        let old_writer = Connection::open(&path).unwrap();
        let mut prepared_delete = old_writer.prepare("DELETE FROM meta").unwrap();
        let mut upgraded = Journal::open_for_service_instance(&path, "fence-test").unwrap();
        let before_events = event_count(&upgraded);
        let before_state = upgraded.materialized_state().unwrap();
        let before_baseline = load_replay_baseline(&upgraded.conn).unwrap();

        // SQLite may invalidate and transparently recompile a statement when
        // migration installs triggers. Either outcome must fail closed; the
        // prepared pre-migration statement must never delete post-migration
        // metadata.
        let prepared_error = prepared_delete.execute([]).unwrap_err();
        assert!(
            prepared_error
                .to_string()
                .contains(JOURNAL_WRITE_FENCE_REASON)
                || prepared_error
                    .to_string()
                    .to_ascii_lowercase()
                    .contains("schema has changed"),
            "prepared stale write was rejected for the wrong reason: {prepared_error}"
        );
        drop(prepared_delete);

        let blocked_writes = [
            (
                "events insert",
                "INSERT INTO events (generation, kind, payload, checksum) VALUES (0, 'tampered', '{}', 'bad')",
            ),
            (
                "events update",
                "UPDATE events SET payload = 'tampered' WHERE id = (SELECT MIN(id) FROM events)",
            ),
            (
                "events delete",
                "DELETE FROM events WHERE id = (SELECT MIN(id) FROM events)",
            ),
            (
                "slots insert",
                "INSERT INTO slots (slot_id, generation, phase, permit_held, routing_valid, session_live, executor_proven, registered, pid, heartbeat_unix) VALUES ('stale-slot', 1, 'ready', 0, 0, 0, 0, 0, NULL, 0)",
            ),
            (
                "slots update",
                "UPDATE slots SET phase = 'tampered' WHERE slot_id = 'scope-1'",
            ),
            (
                "slots delete",
                "DELETE FROM slots WHERE slot_id = 'scope-1'",
            ),
            (
                "jobs insert",
                "INSERT INTO jobs (job_id, slot_id, generation, attempt, worker, phase, accepted_unix) VALUES ('stale-job', 'scope-1', 1, 1, 'stale', 'assigned', 0)",
            ),
            (
                "jobs update",
                "UPDATE jobs SET phase = 'tampered' WHERE job_id = 'job-1'",
            ),
            (
                "jobs delete",
                "DELETE FROM jobs WHERE job_id = 'job-1'",
            ),
            (
                "outbox insert",
                "INSERT INTO outbox (job_id, slot_id, generation, payload_sha256, created_unix) VALUES ('stale-job', 'scope-1', 1, 'stale', 0)",
            ),
            (
                "outbox update",
                "UPDATE outbox SET payload_sha256 = 'tampered' WHERE job_id = 'job-1'",
            ),
            (
                "outbox delete",
                "DELETE FROM outbox WHERE job_id = 'job-1'",
            ),
            (
                "meta insert",
                "INSERT INTO meta (key, value) VALUES ('stale', 'tampered')",
            ),
            (
                "meta update",
                "UPDATE meta SET value = 'tampered' WHERE key = 'replay_baseline_sha256_v1'",
            ),
            (
                "meta delete",
                "DELETE FROM meta WHERE key = 'replay_baseline_v1'",
            ),
            (
                "pressure episode insert",
                "INSERT INTO disk_pressure_episodes (service_instance, filesystem_id, episode_id, started_unix, deadline_unix, drain_deadline_unix, last_observed_unix, reclaim_attempted, revision, draining, terminal) VALUES ('fence-test', 'other-device', 'stale-episode', 100, 160, 460, 100, 0, 1, 0, 0)",
            ),
            (
                "pressure episode update",
                "UPDATE disk_pressure_episodes SET terminal = 1 WHERE service_instance = 'fence-test' AND filesystem_id = 'dev:42'",
            ),
            (
                "pressure episode delete",
                "DELETE FROM disk_pressure_episodes WHERE service_instance = 'fence-test' AND filesystem_id = 'dev:42'",
            ),
            (
                "pressure launch insert",
                "INSERT INTO disk_pressure_launches (service_instance, slot_id, generation, launch_nonce, issued_unix, active) VALUES ('fence-test', 'other-slot', 1, 'nonce', 100, 1)",
            ),
            (
                "pressure launch update",
                "UPDATE disk_pressure_launches SET active = 0 WHERE service_instance = 'fence-test' AND slot_id = 'scope-2'",
            ),
            (
                "pressure launch delete",
                "DELETE FROM disk_pressure_launches WHERE service_instance = 'fence-test' AND slot_id = 'scope-2'",
            ),
            (
                "pressure observation insert",
                "INSERT INTO disk_pressure_observations (service_instance, filesystem_id, volume_fingerprint, identity_confirmed, last_observed_unix) VALUES ('fence-test', 'other-device', 'other-volume', 1, 100)",
            ),
            (
                "pressure observation update",
                "UPDATE disk_pressure_observations SET last_observed_unix = 101 WHERE service_instance = 'fence-test' AND filesystem_id = 'dev:42'",
            ),
            (
                "pressure observation delete",
                "DELETE FROM disk_pressure_observations WHERE service_instance = 'fence-test' AND filesystem_id = 'dev:42'",
            ),
            (
                "journal identity insert",
                "INSERT INTO journal_identity (id, service_instance) VALUES (1, 'stale-service')",
            ),
            (
                "journal identity update",
                "UPDATE journal_identity SET service_instance = 'stale-service' WHERE id = 1",
            ),
            (
                "journal identity delete",
                "DELETE FROM journal_identity WHERE id = 1",
            ),
        ];
        assert_eq!(blocked_writes.len(), JOURNAL_WRITE_FENCE_TRIGGERS.len());
        for (label, sql) in blocked_writes {
            let error = match old_writer.execute(sql, []) {
                Ok(changed) => panic!("{label} unexpectedly changed {changed} rows"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains(JOURNAL_WRITE_FENCE_REASON),
                "{label} was rejected for the wrong reason: {error}"
            );
        }

        assert_eq!(event_count(&upgraded), before_events);
        assert_eq!(upgraded.materialized_state().unwrap(), before_state);
        assert_eq!(
            load_replay_baseline(&upgraded.conn).unwrap(),
            before_baseline
        );

        // The same database remains writable through the current API. These
        // calls exercise both ordinary event materialization and direct meta
        // overlays while the stale connection remains open.
        assert!(
            !upgraded
                .apply(Event::SlotHeartbeat {
                    slot_id: slot("scope-1"),
                    generation,
                    pid: 42,
                })
                .unwrap()
                .rejected
        );
        assert!(upgraded.set_drain(17).unwrap());
        assert!(upgraded.clear_drain().unwrap());
        assert!(upgraded.set_admission_blocked(18).unwrap());
        assert!(upgraded.clear_admission_blocked_if(Some(18)).unwrap());
        assert!(event_count(&upgraded) > before_events);
        assert!(load_replay_baseline(&upgraded.conn).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema9_trigger_census_rejects_case_variant_persistent_and_temp_triggers() {
        let (dir, mut journal) = open_tmp("schema9-case-variant-trigger-census");
        let guarded_tables = [
            ("events", "EVENTS", "INSERT"),
            ("slots", "SLOTS", "INSERT"),
            ("jobs", "JOBS", "INSERT"),
            ("outbox", "OUTBOX", "INSERT"),
            ("meta", "META", "INSERT"),
            ("journal_write_gate", "JOURNAL_WRITE_GATE", "DELETE"),
        ];
        for (index, (_table, case_variant, operation)) in guarded_tables.iter().enumerate() {
            let persistent_name = format!("rogue_case_persistent_{index}");
            journal
                .conn
                .execute_batch(&format!(
                    "CREATE TRIGGER {persistent_name}
                     AFTER {operation} ON \"{case_variant}\"
                     FOR EACH ROW BEGIN SELECT 1; END;"
                ))
                .unwrap();
            let error = journal.set_drain(index as u64 + 1).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.write.fence.invalid");
            journal
                .conn
                .execute_batch(&format!("DROP TRIGGER {persistent_name};"))
                .unwrap();

            let temp_name = format!("rogue_case_temp_{index}");
            journal
                .conn
                .execute_batch(&format!(
                    "CREATE TEMP TRIGGER {temp_name}
                     AFTER {operation} ON \"{case_variant}\"
                     FOR EACH ROW BEGIN SELECT 1; END;"
                ))
                .unwrap();
            let error = journal.set_drain(index as u64 + 100).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.write.fence.invalid");
            journal
                .conn
                .execute_batch(&format!("DROP TRIGGER {temp_name};"))
                .unwrap();
        }
        assert_eq!(read_drain_state(&dir.join("journal.db")), Ok(None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn live_v9_writer_rejects_gate_after_delete_reentrancy() {
        let (dir, mut journal) = open_tmp("schema9-rogue-gate-after-delete");
        let path = dir.join("journal.db");
        let external = Connection::open(&path).unwrap();
        external
            .execute_batch(
                "CREATE TRIGGER rogue_gate_after_delete
                 AFTER DELETE ON journal_write_gate
                 FOR EACH ROW
                 BEGIN
                     INSERT INTO journal_write_gate (id) VALUES (1);
                 END;",
            )
            .unwrap();

        let error = journal.set_drain(17).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.write.fence.invalid");
        assert_eq!(read_drain_state(&path), Ok(None));
        assert_eq!(event_count(&journal), 0);
        drop(external);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn live_v9_writer_rejects_before_dml_gate_injection() {
        let (dir, mut journal) = open_tmp("schema9-rogue-before-dml");
        let path = dir.join("journal.db");
        let external = Connection::open(&path).unwrap();
        external
            .execute_batch(
                "CREATE TRIGGER rogue_events_before_insert
                 BEFORE INSERT ON events
                 FOR EACH ROW
                 BEGIN
                     INSERT OR IGNORE INTO journal_write_gate (id) VALUES (1);
                 END;",
            )
            .unwrap();

        let error = journal.apply(Event::ControlLive).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.write.fence.invalid");
        assert_eq!(event_count(&journal), 0);
        assert!(load_replay_baseline(&journal.conn).unwrap().is_some());
        drop(external);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn live_v9_handle_rejects_fence_removal_before_writing() {
        let (dir, mut journal) = open_tmp("schema9-fence-removed-live-handle");
        let path = dir.join("journal.db");
        let external = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&external);

        let error = journal.set_drain(17).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.write.fence.invalid");
        assert_eq!(read_drain_state(&path), Ok(None));

        restore_journal_write_fence(&external);
        assert!(journal.set_drain(17).unwrap());
        drop(external);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checked_reads_reject_baseline_or_fence_tamper_on_open_handle() {
        let (dir, journal) = open_tmp("schema9-checked-read-tamper");
        let path = dir.join("journal.db");
        let external = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&external);
        let error = journal.materialized_state().unwrap_err();
        assert_eq!(error.envelope.reason, "journal.write.fence.invalid");
        let error = journal.load_state().unwrap_err();
        assert_eq!(error.envelope.reason, "journal.write.fence.invalid");

        restore_journal_write_fence(&external);
        drop_replay_baseline_fence(&external);
        external
            .execute(
                "DELETE FROM meta WHERE key IN (?1, ?2)",
                params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
            )
            .unwrap();
        restore_journal_write_fence(&external);
        let error = journal.materialized_state().unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.missing");
        let error = journal.load_state().unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.missing");
        drop(external);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema9_rejects_malformed_write_gate_schema_without_installing_fence() {
        let (dir, journal) = open_tmp("schema9-malformed-write-gate");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute_batch(
            "DROP TABLE journal_write_gate;
             CREATE TABLE journal_write_gate (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");

        let check = Connection::open(&path).unwrap();
        let gate_schema: String = check
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'journal_write_gate'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            normalize_sql(&gate_schema),
            normalize_sql("CREATE TABLE journal_write_gate (id INTEGER PRIMARY KEY);")
        );
        let trigger_count: i64 = check
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'journal_write_fence_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trigger_count, 0);
        assert_eq!(
            check
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            JOURNAL_SCHEMA_VERSION as i64
        );
        assert!(load_replay_baseline(&check).unwrap().is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_handle_rejects_baseline_deletion_before_state_or_overlay_writes() {
        let (dir, mut journal) = open_tmp("schema9-write-baseline-missing");
        let path = dir.join("journal.db");
        let external = Connection::open(&path).unwrap();
        let direct_error = external
            .execute("DELETE FROM meta WHERE key = ?1", [REPLAY_BASELINE_KEY])
            .unwrap_err();
        assert!(direct_error
            .to_string()
            .contains(JOURNAL_WRITE_FENCE_REASON));
        drop_replay_baseline_fence(&external);
        external
            .execute(
                "DELETE FROM meta WHERE key IN (?1, ?2)",
                params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
            )
            .unwrap();
        drop(external);

        let before_events = event_count(&journal);
        let error = journal.apply(Event::ControlLive).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.missing");
        assert_eq!(event_count(&journal), before_events);
        let error = journal.set_drain(7).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.missing");
        assert_eq!(read_drain_state(&path), Ok(None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_handle_rejects_baseline_checksum_tamper_before_overlay_write() {
        let (dir, mut journal) = open_tmp("schema9-write-baseline-checksum");
        let path = dir.join("journal.db");
        let external = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&external);
        external
            .execute(
                "UPDATE meta SET value = 'tampered' WHERE key = ?1",
                [REPLAY_BASELINE_CHECKSUM_KEY],
            )
            .unwrap();
        drop(external);
        let error = journal.set_admission_blocked(11).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.checksum");
        assert_eq!(read_admission_state(&path), Ok(None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema9_missing_both_replay_baseline_keys_fails_closed() {
        let (dir, journal) = open_tmp("schema9-missing-baseline");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
        )
        .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.missing");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_upgrade_baseline_excludes_drain_and_admission_overlays() {
        let (dir, journal) = open_tmp("legacy-baseline-overlays");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 0);
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES
                ('drain', 'requested:7'),
                ('admission', 'blocked:11')",
            [],
        )
        .unwrap();
        drop(conn);

        let reopened = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        let baseline: serde_json::Value = serde_json::from_str(
            &reopened
                .conn
                .query_row(
                    "SELECT value FROM meta WHERE key = ?1",
                    [REPLAY_BASELINE_KEY],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        let baseline_state = baseline.get("state").unwrap();
        assert!(baseline_state.get("drain_active").is_none());
        assert!(baseline_state.get("admission_blocked").is_none());
        let state = reopened.load_state().unwrap();
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 7);
        assert!(state.admission_blocked);
        assert_eq!(state.admission_version, 11);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn schema9_rejects_missing_and_unknown_baseline_fields() {
        fn assert_malformed_baseline(label: &str, mutate: fn(&mut serde_json::Value)) {
            let (dir, journal) = open_tmp(label);
            let path = dir.join("journal.db");
            drop(journal);
            rewrite_replay_baseline(&path, mutate);
            let error = Journal::open(&path).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
            std::fs::remove_dir_all(dir).unwrap();
        }
        assert_malformed_baseline("missing-state", |value| {
            value.as_object_mut().unwrap().remove("state");
        });
        assert_malformed_baseline("unknown-field", |value| {
            value
                .as_object_mut()
                .unwrap()
                .insert("future_semantic_field".into(), true.into());
        });
        assert_malformed_baseline("missing-state-field", |value| {
            value
                .get_mut("state")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("control_live");
        });
        assert_malformed_baseline("unknown-state-field", |value| {
            value
                .get_mut("state")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("future_state_field".into(), true.into());
        });
    }

    #[test]
    fn legacy_v2_migration_writes_and_reloads_from_replay_baseline() {
        let (dir, journal) = open_tmp("legacy-v2-baseline-write");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 0);

        let mut migrated = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        assert!(!migrated.apply(Event::ControlLive).unwrap().rejected);
        drop(migrated);

        let reopened = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        let state = reopened.load_state().unwrap();
        assert!(state.control_live);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.outbox.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_v2_delete_all_post_migration_events_is_rejected() {
        let (dir, journal) = open_tmp("legacy-v2-delete-all-events");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 0);

        let mut migrated = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        migrated.apply(Event::ControlLive).unwrap();
        drop(migrated);

        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.materialized.replay.mismatch"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_outbox_shape_is_rejected_before_version_advance() {
        let (dir, journal) = open_tmp("malformed-outbox-shape");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE outbox;
             CREATE TABLE outbox (
                 job_id TEXT PRIMARY KEY,
                 slot_id TEXT,
                 generation TEXT NOT NULL,
                 payload_sha256 TEXT NOT NULL,
                 intended INTEGER NOT NULL DEFAULT 0,
                 send_started INTEGER NOT NULL DEFAULT 0,
                 remote_acked INTEGER NOT NULL DEFAULT 0,
                 created_unix INTEGER NOT NULL
             );
             PRAGMA user_version = 2;",
        )
        .unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.outbox.schema.invalid");
        let check = Connection::open(&path).unwrap();
        assert_eq!(
            check
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert!(check
            .query_row(
                "SELECT type FROM sqlite_master WHERE name = 'outbox'",
                [],
                |row| row.get::<_, String>(0),
            )
            .is_ok());
    }

    #[test]
    fn v2_outbox_duplicate_slot_owner_is_rejected_before_ddl() {
        let (dir, journal) = open_tmp("duplicate-v2-outbox-owner");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 0);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE slots RENAME TO slots_valid;
             CREATE TABLE slots (
                 slot_id TEXT,
                 generation INTEGER,
                 phase TEXT,
                 permit_held INTEGER,
                 routing_valid INTEGER,
                 session_live INTEGER,
                 executor_proven INTEGER,
                 registered INTEGER,
                 pid INTEGER,
                 heartbeat_unix INTEGER
             );
             INSERT INTO slots SELECT * FROM slots_valid;
             INSERT INTO slots SELECT * FROM slots_valid;
             DROP TABLE slots_valid;",
        )
        .unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.outbox.owner.inconsistent");
        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
        assert!(check
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'outbox_v3'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_none());
    }

    #[test]
    fn v2_outbox_inconsistent_owner_rolls_back_without_version_advance() {
        let (dir, journal) = open_tmp("inconsistent-v2-outbox-owner");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 2);
        let conn = Connection::open(&path).unwrap();
        conn.execute("DELETE FROM slots", []).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.outbox.owner.inconsistent");
        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        assert!(check
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'outbox_v3'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_none());
    }

    #[test]
    fn current_schema_rejects_unpermitted_nonnumeric_extra_slot() {
        let (dir, mut journal) = open_tmp("current-schema-extra-slot-shape");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        for slot_id in ["scope-1", "scope-2"] {
            assert!(
                !journal
                    .apply(Event::PermitReserved {
                        slot_id: slot(slot_id),
                        generation: r#gen(),
                    })
                    .unwrap()
                    .rejected
            );
        }
        let path = dir.join("journal.db");
        drop(journal);

        let mut seed = Connection::open(&path).unwrap();
        let transaction = seed
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        begin_journal_write_gate(&transaction).unwrap();
        transaction
            .execute(
                "INSERT INTO slots (
                     slot_id, generation, phase, permit_held, routing_valid,
                     session_live, executor_proven, registered, pid, heartbeat_unix
                 ) VALUES ('scope-extra', 1, 'provisioning', 0, 0, 0, 0, 0, NULL, 0)",
                [],
            )
            .unwrap();
        end_journal_write_gate(&transaction).unwrap();
        transaction.commit().unwrap();
        seed.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(seed);

        let before = std::fs::read(&path).unwrap();
        let mut reopened = Journal::open(&path).unwrap();
        let replayed = reopened.load_state().unwrap();
        assert!(replayed.capacity_invalid);
        assert_eq!(replayed.slots.len(), 3);
        let state = reopened.materialized_state().unwrap();
        assert!(state.capacity_invalid);
        assert_eq!(state.slots.len(), 3);
        assert_eq!(state.advertised_capacity(), 0);
        assert_eq!(state.health().state, FleetHealthState::NotReady);

        let error = reopened
            .apply(Event::SlotHeartbeat {
                slot_id: slot("scope-extra"),
                generation: r#gen(),
                pid: 123,
            })
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.capacity.invalid");
        drop(reopened);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let forensic = Connection::open(&path).unwrap();
        assert_eq!(
            forensic
                .query_row("SELECT COUNT(*) FROM slots", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            forensic
                .query_row("SELECT COUNT(*) FROM events", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            3
        );
    }

    #[test]
    fn schema_v1_contaminated_open_preserves_forensic_database() {
        let (dir, mut journal) = open_tmp("legacy-surge-schema");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::PermitReserved {
                    slot_id: slot("scope-1"),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "ALTER TABLE slots ADD COLUMN surge INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .unwrap();
        conn.execute("UPDATE slots SET surge = 1", []).unwrap();
        conn.execute("PRAGMA user_version = 1", []).unwrap();
        drop(conn);

        let before = std::fs::read(&path).unwrap();
        for _ in 0..2 {
            let error = Journal::open(&path).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.legacy.unsafe");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }

        let forensic = Connection::open(&path).unwrap();
        let version: i64 = forensic
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let surge: i64 = forensic
            .query_row(
                "SELECT surge FROM slots WHERE slot_id = 'scope-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(surge, 1);
        let slot_count: i64 = forensic
            .query_row("SELECT COUNT(*) FROM slots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(slot_count, 1);
        let event_count: i64 = forensic
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_count, 2);
    }

    #[test]
    fn realistic_schema_v1_fails_closed_before_migration() {
        let (dir, mut journal) = open_tmp("clean-schema-v1-migration");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: slot("scope-1"),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        let job_id = job("worker-1");
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-1"),
                    job_id: job_id.clone(),
                    generation: r#gen(),
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id,
                    slot_id: slot("scope-1"),
                    attempt: 1,
                    generation: r#gen(),
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1_234,
                })
                .unwrap()
                .rejected
        );
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute_batch(
            "ALTER TABLE jobs RENAME TO jobs_v2;
             CREATE TABLE jobs (
                 job_id TEXT PRIMARY KEY,
                 slot_id TEXT NOT NULL,
                 generation INTEGER NOT NULL,
                 attempt INTEGER NOT NULL,
                 worker TEXT NOT NULL,
                 phase TEXT NOT NULL
             );
             INSERT INTO jobs (job_id, slot_id, generation, attempt, worker, phase)
             SELECT job_id, slot_id, generation, attempt, worker, phase FROM jobs_v2;
             DROP TABLE jobs_v2;
             ALTER TABLE slots ADD COLUMN surge INTEGER NOT NULL DEFAULT 0;
             UPDATE slots SET surge = 1;
             PRAGMA user_version = 1;",
        )
        .unwrap();
        // The fixture above is a committed direct mutation while the journal
        // normally runs in WAL mode. Checkpoint it before taking the exact
        // database-file snapshot; otherwise the snapshot omits committed WAL
        // pages and cannot prove whether Journal::open wrote anything.
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let expected_events: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        drop(conn);

        for _ in 0..2 {
            let error = Journal::open(&path).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.legacy.unsafe");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }

        let forensic = Connection::open(&path).unwrap();
        let version: i64 = forensic
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let surge: i64 = forensic
            .query_row(
                "SELECT surge FROM slots WHERE slot_id = 'scope-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(surge, 1);
        let accepted_column: Option<String> = forensic
            .query_row(
                "SELECT name FROM pragma_table_info('jobs') WHERE name = 'accepted_unix'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(accepted_column, None);
        let actual_events: i64 = forensic
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(actual_events, expected_events);
    }

    #[test]
    fn newer_permit_generation_resets_fenced_actor_identity_and_proofs() {
        let (_dir, mut journal) = open_tmp("new-generation-reset");
        let slot_id = slot("scope-1");
        let generation = r#gen();
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::SlotHeartbeat {
                    slot_id: slot_id.clone(),
                    generation,
                    pid: 123,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::SlotStale {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );

        let same_generation = journal
            .apply(Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation,
            })
            .unwrap();
        assert!(same_generation.rejected);
        assert!(same_generation.commands.is_empty());
        assert_eq!(same_generation.state.slots[0].phase, SlotPhase2::Fenced);
        let outcome = journal
            .apply(Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: generation.next(),
            })
            .unwrap();
        assert!(!outcome.rejected);
        assert_eq!(
            outcome.commands,
            vec![SideEffect::SpawnSlot {
                slot_id: slot_id.clone(),
                generation: generation.next(),
            }]
        );

        let slot = journal
            .load_state()
            .unwrap()
            .slots
            .into_iter()
            .find(|slot| slot.slot_id == slot_id)
            .unwrap();
        assert_eq!(slot.generation, generation.next());
        assert_eq!(slot.phase, SlotPhase2::Provisioning);
        assert!(slot.permit_held);
        assert!(slot.routing_valid);
        assert!(!slot.executor_proven);
        assert!(!slot.session_live);
        assert!(!slot.registered);
        assert_eq!(slot.pid, None);
        assert_eq!(slot.heartbeat_unix, 0);
    }

    #[test]
    fn fenced_slot_rejects_readiness_resurrection_events() {
        let (_dir, mut journal) = open_tmp("fenced-resurrection");
        let slot_id = slot("scope-1");
        let generation = r#gen();
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::SlotStale {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );

        for event in [
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationLost {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
        ] {
            let outcome = journal.apply(event).unwrap();
            assert!(outcome.rejected);
            assert!(outcome.commands.is_empty());
            assert_eq!(outcome.state.slots[0].phase, SlotPhase2::Fenced);
        }
    }

    #[test]
    fn slot_stale_rejects_occupied_slot() {
        let slot_id = slot("scope-1");
        let generation = r#gen();

        for phase in [
            JobPhase2::Assigned,
            JobPhase2::Running,
            JobPhase2::Completing,
        ] {
            let state = FleetState {
                slots: vec![SlotRecord {
                    slot_id: slot_id.clone(),
                    generation,
                    phase: SlotPhase2::Assigned,
                    permit_held: true,
                    ..SlotRecord::new(slot_id.clone())
                }],
                jobs: vec![JobRecord {
                    job_id: job("job-1"),
                    slot_id: slot_id.clone(),
                    generation,
                    attempt: 1,
                    worker: "worker-1".to_owned(),
                    phase,
                    accepted_unix: 1,
                    terminal_conclusion: None,
                    provisional: false,
                    plan_id: String::new(),
                    run_service_url: String::new(),
                    probe_attempts: 0,
                    probe_deadline_unix: 0,
                }],
                ..FleetState::default()
            };
            let outcome = reduce(
                state.clone(),
                Event::SlotStale {
                    slot_id: slot_id.clone(),
                    generation,
                },
            );
            assert!(outcome.rejected, "phase {phase:?}");
            assert!(outcome.commands.is_empty());
            assert_eq!(outcome.state, state);
        }
    }

    fn migrated_generation_mismatch_state(phase: JobPhase2) -> FleetState {
        let slot_id = slot("scope-1");
        FleetState {
            slots: vec![SlotRecord {
                slot_id: slot_id.clone(),
                generation: Generation(2),
                phase: SlotPhase2::Assigned,
                permit_held: true,
                ..SlotRecord::new(slot_id.clone())
            }],
            jobs: vec![JobRecord {
                job_id: job("job-1"),
                slot_id,
                generation: r#gen(),
                attempt: 1,
                worker: "worker-1".to_owned(),
                phase,
                accepted_unix: 1,
                terminal_conclusion: None,
                provisional: false,
                plan_id: String::new(),
                run_service_url: String::new(),
                probe_attempts: 0,
                probe_deadline_unix: 0,
            }],
            ..FleetState::default()
        }
    }

    fn migrated_completion_generation_mismatch_state() -> FleetState {
        let mut state = migrated_generation_mismatch_state(JobPhase2::Completing);
        state.outbox.push(OutboxRecord {
            job_id: job("job-1"),
            slot_id: slot("scope-1"),
            generation: r#gen(),
            payload_sha256: "payload".to_owned(),
            intended: true,
            send_started: true,
            remote_acked: false,
            created_unix: 1,
            attempts: 0,
            deadline_unix: 2,
            permanent: false,
            abandoned: false,
        });
        state
    }

    #[test]
    fn job_started_rejects_migrated_slot_generation_mismatch_without_mutation() {
        let state = migrated_generation_mismatch_state(JobPhase2::Assigned);
        let outcome = reduce(
            state.clone(),
            Event::JobStarted {
                job_id: job("job-1"),
                generation: r#gen(),
            },
        );

        assert!(outcome.rejected);
        assert!(outcome.commands.is_empty());
        assert_eq!(outcome.state, state);
    }

    #[test]
    fn job_worker_lost_rejects_migrated_slot_generation_mismatch_without_mutation() {
        let state = migrated_generation_mismatch_state(JobPhase2::Running);
        let outcome = reduce(
            state.clone(),
            Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: r#gen(),
            },
        );

        assert!(outcome.rejected);
        assert!(outcome.commands.is_empty());
        assert_eq!(outcome.state, state);
    }

    #[test]
    fn schema7_eventless_migration_preserves_generation_mismatch_evidence() {
        let (dir, journal) = open_tmp("schema7-generation-mismatch");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "DELETE FROM meta WHERE key IN (?1, ?2)",
            params![REPLAY_BASELINE_KEY, REPLAY_BASELINE_CHECKSUM_KEY],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO slots (
                 slot_id, generation, phase, permit_held, routing_valid,
                 session_live, executor_proven, registered, pid, heartbeat_unix
             ) VALUES ('scope-1', 2, 'assigned', 1, 1, 1, 1, 1, NULL, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (
                 job_id, slot_id, generation, attempt, worker, phase, accepted_unix,
                 terminal_conclusion, provisional, plan_id, run_service_url,
                 probe_attempts, probe_deadline_unix
             ) VALUES ('job-1', 'scope-1', 1, 1, 'worker-1', 'assigned', 1,
                       NULL, 0, '', '', 0, 0)",
            [],
        )
        .unwrap();
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 7u32).unwrap();
        drop(conn);

        let mut migrated = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        let before = migrated.load_state().unwrap();
        assert_eq!(before.slots[0].generation, Generation(2));
        assert_eq!(before.jobs[0].generation, r#gen());
        let events_before = event_count(&migrated);

        for event in [
            Event::JobStarted {
                job_id: job("job-1"),
                generation: r#gen(),
            },
            Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: r#gen(),
            },
        ] {
            let outcome = migrated.apply(event).unwrap();
            assert!(outcome.rejected);
            assert!(outcome.commands.is_empty());
            assert_eq!(outcome.state, before);
        }

        assert_eq!(migrated.load_state().unwrap(), before);
        assert_eq!(event_count(&migrated), events_before);
    }

    #[test]
    fn terminal_observation_rejects_migrated_slot_generation_mismatch_without_mutation() {
        let state = migrated_completion_generation_mismatch_state();

        for event in [
            Event::RemoteAcked {
                job_id: job("job-1"),
                generation: r#gen(),
            },
            Event::RemoteObservedTerminal {
                job_id: job("job-1"),
                generation: r#gen(),
            },
        ] {
            let outcome = reduce(state.clone(), event);
            assert!(outcome.rejected);
            assert!(outcome.commands.is_empty());
            assert_eq!(outcome.state, state);
        }
    }

    #[test]
    fn terminal_observation_accepts_matching_job_and_slot_generation() {
        for (suffix, event) in [
            (
                "acked",
                Event::RemoteAcked {
                    job_id: job("job-1"),
                    generation: r#gen(),
                },
            ),
            (
                "observed",
                Event::RemoteObservedTerminal {
                    job_id: job("job-1"),
                    generation: r#gen(),
                },
            ),
        ] {
            let (_dir, mut journal) = open_tmp(&format!("terminal-observation-{suffix}"));
            let generation = prime_running_job(&mut journal, "scope-1", "job-1");
            journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation,
                    payload_sha256: "payload".to_owned(),
                })
                .unwrap();
            journal
                .apply(Event::CompletionSendStarted {
                    job_id: job("job-1"),
                    generation,
                })
                .unwrap();

            let outcome = journal.apply(event).unwrap();
            assert!(!outcome.rejected, "{suffix}");
            assert!(outcome.commands.iter().any(|command| matches!(
                command,
                SideEffect::DeleteOutbox {
                    job_id,
                    generation: command_generation
                } if *job_id == job("job-1") && *command_generation == generation
            )));
            assert!(outcome.state.jobs.is_empty());
            assert!(outcome
                .state
                .outbox
                .iter()
                .any(|row| row.job_id == job("job-1") && row.remote_acked));
        }
    }

    #[test]
    fn journal_slot_stale_rejection_preserves_running_job() {
        let (_dir, mut journal) = open_tmp("occupied-slot-stale");
        let slot_id = slot("scope-1");
        let generation = prime_running_job(&mut journal, "scope-1", "job-1");
        let before = journal.load_state().unwrap();
        let events_before = event_count(&journal);

        let outcome = journal
            .apply(Event::SlotStale {
                slot_id,
                generation,
            })
            .unwrap();
        assert!(outcome.rejected);
        assert!(outcome.commands.is_empty());
        assert_eq!(outcome.state, before);
        assert_eq!(journal.load_state().unwrap(), before);
        assert_eq!(event_count(&journal), events_before);
    }

    #[test]
    fn permit_rotation_rejects_active_job_on_slot() {
        let (_dir, mut journal) = open_tmp("fenced-active-job");
        let slot_id = slot("scope-1");
        let generation = r#gen();
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: job("job-1"),
                    generation,
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job("job-1"),
                    slot_id: slot_id.clone(),
                    attempt: 1,
                    generation,
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobStarted {
                    job_id: job("job-1"),
                    generation,
                })
                .unwrap()
                .rejected
        );

        let outcome = journal
            .apply(Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: generation.next(),
            })
            .unwrap();
        assert!(outcome.rejected);
        assert!(outcome.commands.is_empty());
        assert_eq!(outcome.state.slots[0].generation, generation);
        assert_eq!(outcome.state.slots[0].phase, SlotPhase2::Assigned);
        assert_eq!(outcome.state.jobs[0].generation, generation);
    }

    #[test]
    fn materialized_state_matches_replayed_state_including_queue_age() {
        let (_dir, mut journal) = open_tmp("materialized-state");
        let slot_id = slot("scope-1");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: slot_id.clone(),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        let job_id = job("job-1");
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: job_id.clone(),
                    generation: r#gen(),
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id,
                    slot_id,
                    attempt: 1,
                    generation: r#gen(),
                    worker: "worker-1".to_owned(),
                    accepted_unix: 123,
                })
                .unwrap()
                .rejected
        );

        assert_eq!(
            journal.materialized_state().unwrap(),
            journal.load_state().unwrap()
        );
        assert_eq!(
            journal.materialized_state().unwrap().jobs[0].accepted_unix,
            123
        );
    }

    #[test]
    fn schema9_reopen_replay_validates_current_event_projection() {
        let (dir, mut journal) = open_tmp("schema9-replay-valid");
        journal.apply(Event::ControlLive).unwrap();
        journal
            .apply(Event::Dependency {
                github_reachable: true,
            })
            .unwrap();
        drop(journal);

        let reopened = Journal::open(dir.join("journal.db")).unwrap();
        let state = reopened.load_state().unwrap();
        assert!(state.control_live);
        assert!(state.github_reachable);
        let conn = Connection::open(dir.join("journal.db")).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_materialized_projection_drift() {
        let (dir, mut journal) = open_tmp("replay-projection-drift");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);

        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute("UPDATE meta SET value = '0' WHERE key = 'control_live'", [])
            .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.materialized.replay.mismatch"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_event_deletion_that_breaks_projection() {
        let (dir, mut journal) = open_tmp("replay-event-deletion");
        journal.apply(Event::ControlLive).unwrap();
        journal
            .apply(Event::Dependency {
                github_reachable: true,
            })
            .unwrap();
        drop(journal);

        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute("DELETE FROM events WHERE kind = 'control_live'", [])
            .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.materialized.replay.mismatch"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_deleting_all_events_with_materialized_state() {
        let (dir, mut journal) = open_tmp("replay-delete-all-events");
        journal.apply(Event::ControlLive).unwrap();
        journal
            .apply(Event::Dependency {
                github_reachable: true,
            })
            .unwrap();
        drop(journal);

        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute("DELETE FROM events", []).unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.materialized.replay.mismatch"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_event_payload_checksum_tamper() {
        let (dir, mut journal) = open_tmp("replay-event-checksum");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);

        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "UPDATE events SET payload = '{\"type\":\"dependency\",\"github_reachable\":false}' WHERE id = 1",
            [],
        )
        .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.checksum.mismatch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_decoded_event_metadata_mismatch() {
        let (dir, mut journal) = open_tmp("replay-event-metadata");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);

        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute("UPDATE events SET kind = 'dependency' WHERE id = 1", [])
            .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.metadata.mismatch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_zero_generation_metadata_tamper() {
        let (dir, mut journal) = open_tmp("replay-zero-generation-metadata");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);

        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute("UPDATE events SET generation = 1 WHERE id = 1", [])
            .unwrap();
        // The payload and its checksum are unchanged. Only the denormalized
        // database generation was tampered with.
        let (payload, checksum): (String, String) = conn
            .query_row(
                "SELECT payload, checksum FROM events WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(sha256_hex(payload.as_bytes()), checksum);
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.metadata.mismatch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn replay_rejects_acquisition_generation_metadata_tamper() {
        let (dir, mut journal) = open_tmp("replay-acquisition-generation");
        prime_provisional(&mut journal, "scope-1", "request-1");
        drop(journal);

        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "UPDATE events SET generation = 2 WHERE kind = 'job_acquisition_intended'",
            [],
        )
        .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.metadata.mismatch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn acquisition_events_bind_generation_metadata() {
        let generation = Generation(7);
        let events = [
            Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("request-1"),
                generation,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("job-1"),
                plan_id: "plan-1".into(),
                generation,
            },
            Event::AcquisitionProbeFailed {
                job_id: job("request-1"),
                generation,
            },
            Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation,
                reason: "lost".into(),
            },
        ];
        for event in events {
            assert_eq!(event_generation(&event), generation);
        }
    }

    #[test]
    fn replay_rejects_event_that_reducer_now_rejects() {
        let (dir, mut journal) = open_tmp("replay-event-rejection");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);

        let event = Event::SlotStale {
            slot_id: slot("scope-1"),
            generation: r#gen().next(),
        };
        let payload = serde_json::to_string(&event).unwrap();
        let conn = Connection::open(dir.join("journal.db")).unwrap();
        drop_replay_baseline_fence(&conn);
        conn.execute(
            "UPDATE events
             SET generation = ?1, kind = ?2, payload = ?3, checksum = ?4
             WHERE id = 1",
            params![
                event_generation(&event).0 as i64,
                event_kind(&event),
                payload,
                payload_checksum(payload.as_bytes()),
            ],
        )
        .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);

        let error = Journal::open(dir.join("journal.db")).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.rejected");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn materialized_state_is_fresh_across_journal_connections() {
        let (dir, mut writer) = open_tmp("materialized-cross-process");
        writer.apply(Event::ControlLive).unwrap();
        let reader = Journal::open(dir.join("journal.db")).unwrap();
        writer
            .apply(Event::Dependency {
                github_reachable: true,
            })
            .unwrap();
        assert!(reader.materialized_state().unwrap().github_reachable);
    }

    #[test]
    fn v1_journal_is_preserved_and_rejected_without_migration() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-journal-v1-migration-{}-{}",
            std::process::id(),
            unix_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE jobs (
                    job_id TEXT PRIMARY KEY,
                    slot_id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    attempt INTEGER NOT NULL,
                    worker TEXT NOT NULL,
                    phase TEXT NOT NULL
                );
                PRAGMA user_version = 1;",
            )
            .unwrap();
        }

        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.legacy.unsafe");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
        let has_timestamp: bool = conn
            .prepare("PRAGMA table_info(jobs)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|name| name.unwrap())
            .any(|name| name == "accepted_unix");
        assert!(!has_timestamp);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn registration_lost_clears_stale_local_identity() {
        let (_dir, mut journal) = open_tmp("registration-lost");
        prime_ready(&mut journal, "scope-1");
        let outcome = journal
            .apply(Event::RegistrationLost {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(!outcome.rejected);
        let slot = journal
            .load_state()
            .unwrap()
            .slots
            .into_iter()
            .find(|row| row.slot_id == slot("scope-1"))
            .unwrap();
        assert!(!slot.registered);
        assert!(!slot.permit_held);
        assert!(!slot.session_live);
        assert_eq!(slot.phase, SlotPhase2::Provisioning);
    }

    #[test]
    fn active_registration_loss_preserves_teardown_state_and_re_admits() {
        let (dir, mut journal) = open_tmp("active-registration-lost");
        let slot_id = slot("scope-1");
        let job_id = job("job-1");
        let generation = r#gen();
        prime_ready(&mut journal, "scope-1");
        for event in [
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::JobAcquisitionIntended {
                slot_id: slot_id.clone(),
                job_id: job_id.clone(),
                generation,
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            },
            Event::JobOwned {
                job_id: job_id.clone(),
                slot_id: slot_id.clone(),
                attempt: 1,
                generation,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_234,
            },
            Event::JobStarted {
                job_id: job_id.clone(),
                generation,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }

        let lost = journal
            .apply(Event::RegistrationLost {
                slot_id: slot_id.clone(),
                generation,
            })
            .unwrap();
        assert!(!lost.rejected);
        let state = journal.load_state().unwrap();
        let slot_state = state
            .slots
            .iter()
            .find(|row| row.slot_id == slot_id)
            .unwrap();
        assert!(!slot_state.registered);
        assert!(!slot_state.permit_held);
        assert!(!slot_state.session_live);
        assert_eq!(slot_state.phase, SlotPhase2::Fenced);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job_id);

        // A completion intent remains durable even after registration loss;
        // teardown owns the job and outbox until remote acknowledgement.
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job_id.clone(),
                    generation,
                    payload_sha256: payload_checksum(b"result"),
                })
                .unwrap()
                .rejected
        );
        drop(journal);
        let mut recovered = Journal::open(dir.join("journal.db")).unwrap();
        let recovered_state = recovered.load_state().unwrap();
        assert_eq!(recovered_state.jobs.len(), 1);
        assert_eq!(recovered_state.outbox.len(), 1);
        assert!(!recovered_state.slots[0].registered);
        assert!(!recovered_state.slots[0].permit_held);
        assert!(!recovered_state.slots[0].session_live);

        assert!(
            !recovered
                .apply(Event::CompletionSendStarted {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );

        assert!(
            !recovered
                .apply(Event::RemoteObservedTerminal {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        let torn_down = recovered.load_state().unwrap();
        assert!(torn_down.jobs.is_empty());
        assert!(torn_down.outbox[0].remote_acked);
        assert!(!torn_down.slots[0].permit_held);
        assert_eq!(torn_down.slots[0].phase, SlotPhase2::Fenced);

        // The old generation remains fenced; recovery must rotate only after
        // the durable job and outbox teardown proof.
        assert!(
            recovered
                .apply(Event::PermitReserved {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        let generation = generation.next();
        assert!(
            !recovered
                .apply(Event::PermitReserved {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !recovered
                .apply(Event::ExecutorProven {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !recovered
                .apply(Event::SessionLive {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !recovered
                .apply(Event::RegistrationIntended {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !recovered
                .apply(Event::Registered {
                    slot_id: slot_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !recovered
                .apply(Event::ReadyAttempt {
                    slot_id,
                    generation,
                })
                .unwrap()
                .rejected
        );
        assert_eq!(
            recovered.load_state().unwrap().slots[0].phase,
            SlotPhase2::Ready
        );
    }

    #[test]
    fn ready_requires_permit_routing_session_and_executor() {
        let (_dir, mut journal) = open_tmp("not-ready");
        let s = slot("scope-1");
        let g = r#gen();
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        journal
            .apply(Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
            })
            .unwrap();
        let outcome = journal
            .apply(Event::ReadyAttempt {
                slot_id: s,
                generation: g,
            })
            .unwrap();
        assert!(outcome.rejected);
        assert_eq!(outcome.state.slots[0].phase, SlotPhase2::Provisioning);
    }

    #[test]
    fn github_unreachable_stays_control_live_and_not_ready_state() {
        let (_dir, mut journal) = open_tmp("gh-down");
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        journal
            .apply(Event::Dependency {
                github_reachable: false,
            })
            .unwrap();
        let health = journal.load_state().unwrap().health();
        assert!(health.control_live);
        assert!(!health.github_reachable);
        assert_eq!(health.state, FleetHealthState::Degraded);
        assert_ne!(health.state.as_str(), "ready");
    }

    #[test]
    fn registration_without_ready_proof_is_rejected() {
        let (_dir, mut journal) = open_tmp("reg-no-proof");
        let s = slot("scope-1");
        let g = r#gen();
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        journal
            .apply(Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
            })
            .unwrap();
        let outcome = journal
            .apply(Event::RegistrationIntended {
                slot_id: s,
                generation: g,
            })
            .unwrap();
        assert!(outcome.rejected);
        assert!(outcome.commands.is_empty());
    }

    #[test]
    fn registration_intent_is_durable_before_register_command() {
        let (dir, mut journal) = open_tmp("reg-intent");
        let s = slot("scope-1");
        let g = r#gen();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
            },
            Event::ExecutorProven {
                slot_id: s.clone(),
                generation: g,
            },
            Event::SessionLive {
                slot_id: s.clone(),
                generation: g,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let outcome = journal
            .apply(Event::RegistrationIntended {
                slot_id: s.clone(),
                generation: g,
            })
            .unwrap();
        assert!(!outcome.rejected);
        assert_eq!(
            outcome.commands,
            vec![SideEffect::RegisterRunner {
                slot_id: s.clone(),
                generation: g,
            }]
        );
        drop(journal);
        let recovered = Journal::open(dir.join("journal.db")).unwrap();
        let state = recovered.load_state().unwrap();
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.slot_id == s)
            .expect("slot");
        assert!(slot.permit_held);
        assert!(slot.executor_proven);
        assert!(slot.session_live);
        assert!(!slot.registered);
    }

    #[test]
    fn job_ownership_survives_reopen() {
        let (dir, mut journal) = open_tmp("job-own");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        let outcome = journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "velnor-job@job-1".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        assert!(
            outcome.commands.is_empty(),
            "ownership is durable state, not a spawn command: {:?}",
            outcome.commands
        );
        drop(journal);
        let recovered = Journal::open(dir.join("journal.db"))
            .unwrap()
            .load_state()
            .unwrap();
        assert_eq!(recovered.jobs[0].job_id, job("job-1"));
        assert_eq!(recovered.jobs[0].worker, "velnor-job@job-1");
    }

    #[test]
    fn completion_kill_points_leave_outbox_recoverable() {
        let (dir, mut journal) = open_tmp("complete");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        let checksum = payload_checksum(b"conclusion=success");
        // Kill before send: intent is durable, outbox pending.
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: r#gen(),
                payload_sha256: checksum.clone(),
            })
            .unwrap();
        assert_eq!(journal.pending_outbox().unwrap().len(), 1);
        // Kill during send.
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: r#gen(),
            })
            .unwrap();
        drop(journal);
        let mut recovered = Journal::open(dir.join("journal.db")).unwrap();
        let pending = recovered.pending_outbox().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].send_started);
        assert!(!pending[0].remote_acked);
        // Kill after remote accept before local ack: observe terminal, then
        // delete command is issued only after that event commits.
        let outcome = recovered
            .apply(Event::RemoteObservedTerminal {
                job_id: job("job-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(
            outcome
                .commands
                .iter()
                .any(|command| matches!(command, SideEffect::DeleteOutbox { .. })),
            "{:?}",
            outcome.commands
        );
        assert!(
            outcome
                .commands
                .iter()
                .any(|command| matches!(command, SideEffect::AdvertiseCapacity { .. })),
            "terminal job must restore advertised Ready capacity: {:?}",
            outcome.commands
        );
        drop(recovered);
        let again = Journal::open(dir.join("journal.db")).unwrap();
        assert!(again.pending_outbox().unwrap().is_empty());
    }

    #[test]
    fn lost_worker_restores_the_slot_to_ready() {
        let (_dir, mut journal) = open_tmp("worker-lost");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        // Worker killed mid-run: no completion intent, no outbox — the only
        // terminal path is JobWorkerLost.
        let outcome = journal
            .apply(Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(!outcome.rejected);
        assert!(
            outcome
                .commands
                .iter()
                .any(|command| matches!(command, SideEffect::AdvertiseCapacity { .. })),
            "lost worker must restore advertised Ready capacity: {:?}",
            outcome.commands
        );
        let state = journal.load_state().unwrap();
        assert!(state.jobs.is_empty());
        assert!(
            state.slots.iter().any(
                |record| record.slot_id == slot("scope-1") && record.phase == SlotPhase2::Ready
            ),
            "{state:?}"
        );
        // Losing the same worker twice is rejected, not duplicated.
        let repeat = journal
            .apply(Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(repeat.rejected);
    }

    #[test]
    fn stale_generation_cannot_complete_or_cleanup() {
        let (_dir, mut journal) = open_tmp("stale");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        journal
            .apply(Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: r#gen(),
            })
            .unwrap();
        let newer = Generation(2);
        journal
            .apply(Event::PermitReserved {
                slot_id: slot("scope-1"),
                generation: newer,
            })
            .unwrap();
        let complete = journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: r#gen(),
                payload_sha256: payload_checksum(b"nope"),
            })
            .unwrap();
        assert!(complete.rejected);
        let cleanup = journal
            .apply(Event::CleanupIntended {
                slot_id: slot("scope-1"),
                isolation_id: "job-1".to_owned(),
                generation: r#gen(),
            })
            .unwrap();
        assert!(cleanup.rejected);
        assert!(cleanup.commands.is_empty());
    }

    #[test]
    fn registration_command_is_not_emitted_without_permit() {
        let state = FleetState::default();
        let outcome = reduce(
            state,
            Event::RegistrationIntended {
                slot_id: slot("x"),
                generation: r#gen(),
            },
        );
        assert!(outcome.rejected);
        assert!(outcome.commands.is_empty());
    }

    #[test]
    fn n_minus_one_cannot_write_a_newer_schema() {
        let (dir, journal) = open_tmp("nn1");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.newer");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let check = Connection::open(&path).unwrap();
        assert_eq!(
            check
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            99
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_event_kind_is_a_hard_error_not_a_skip() {
        let (dir, mut journal) = open_tmp("unk");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);
        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        let payload = r#"{"type":"future_envelope","x":1}"#;
        let checksum = payload_checksum(payload.as_bytes());
        conn.execute(
            "INSERT INTO events (generation, kind, payload, checksum) VALUES (0, 'future_envelope', ?1, ?2)",
            params![payload, checksum],
        )
        .unwrap();
        restore_journal_write_fence(&conn);
        drop(conn);
        // Skipping it would drop terminal state and re-drive a completion the
        // writer had already resolved. The version gate is what keeps an
        // older binary from ever reaching this log in the first place.
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.unknown");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn second_live_job_on_assigned_slot_is_rejected() {
        let (_dir, mut journal) = open_tmp("two-jobs");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-1"),
                    job_id: job("guid-1"),
                    generation: r#gen(),
                    message_id: "msg-1".into(),
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job("guid-1"),
                    slot_id: slot("scope-1"),
                    attempt: 1,
                    generation: r#gen(),
                    worker: "w".into(),
                    accepted_unix: 0,
                })
                .unwrap()
                .rejected
        );
        let second = journal
            .apply(Event::JobOwned {
                job_id: job("424242"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w2".into(),
                accepted_unix: 0,
            })
            .unwrap();
        assert!(second.rejected);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("guid-1"));
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
    }

    #[test]
    fn completion_intended_marks_completing_and_counts_queued_age() {
        let (_dir, mut journal) = open_tmp("complete-phase");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("guid-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        let owned = journal.load_state().unwrap();
        assert!(owned.health().oldest_queued_job_seconds < 60, "{owned:?}");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("guid-1"),
                generation: r#gen(),
                payload_sha256: payload_checksum(b"ok"),
            })
            .unwrap();
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs[0].phase, JobPhase2::Completing);
        assert!(state.jobs[0].accepted_unix > 0);
    }

    #[test]
    fn terminal_ack_requires_a_durable_send_claim() {
        let (_dir, mut journal) = open_tmp("ack-requires-send-claim");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("guid-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        journal
            .apply(Event::CompletionIntended {
                job_id: job("guid-1"),
                generation: r#gen(),
                payload_sha256: payload_checksum(b"ok"),
            })
            .unwrap();

        let ack = journal
            .apply(Event::RemoteAcked {
                job_id: job("guid-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(ack.rejected);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.outbox.len(), 1);
        assert!(!state.outbox[0].remote_acked);
    }

    /// The whole point of the provisional row: it occupies the slot and records
    /// that this runner may already own the job, but it must never be able to
    /// back a terminal completion. If it could, a runner that crashed before
    /// seeing a 200 could publish a result for a job another runner owns.
    /// Every migration step must stamp its **own** target version, not the
    /// symbolic current one.
    ///
    /// This is the regression test for a one-way data-loss bug: when v5 was
    /// added, `migrate_v3_to_v4` still stamped `JOURNAL_SCHEMA_VERSION`, so an
    /// upgraded file claimed 5 while carrying the v4 shape, the v5 step
    /// early-returned without adding its column, and every later
    /// `materialized_state()` failed with `no such column: provisional`. The
    /// stamp being 5 meant no older binary could open it either — no run, no
    /// rollback. `Journal::open` alone does not catch it, because it only
    /// replays events, so this asserts on materialization.
    #[test]
    fn an_upgrade_from_every_older_version_can_still_materialize() {
        for stamped in [2u32, 3, 4, 5, 6] {
            let (dir, journal) = open_tmp(&format!("upgrade-from-v{stamped}"));
            let path = dir.join("journal.db");
            drop(journal);

            // Rewind the stamp to simulate a file written by an older binary.
            // The shape is current, which is exactly the case a real upgrade
            // hits after `CREATE TABLE IF NOT EXISTS` has run.
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", stamped).unwrap();
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
                .unwrap();
            drop(conn);

            match Journal::open(&path) {
                Ok(journal) => {
                    // Opening is not proof: materialization is where a missing
                    // column actually surfaces.
                    journal.load_state().unwrap_or_else(|error| {
                        panic!("v{stamped} upgrade cannot materialize: {error:?}")
                    });
                    let conn = Connection::open(&path).unwrap();
                    let version: i64 = conn
                        .query_row("PRAGMA user_version", [], |row| row.get(0))
                        .unwrap();
                    assert_eq!(
                        u32::try_from(version).unwrap(),
                        JOURNAL_SCHEMA_VERSION,
                        "v{stamped} upgrade must land on the current version"
                    );
                }
                Err(error) => {
                    // Refusing is acceptable only for the shape-ahead-of-stamp
                    // guard, which must leave the file untouched. Silently
                    // stamping a shape it does not have is not.
                    assert_eq!(
                        error.envelope.reason, "journal.schema.mismatch",
                        "v{stamped} upgrade failed for an unexpected reason"
                    );
                }
            }
            std::fs::remove_dir_all(dir).ok();
        }
    }

    /// The genuine v5-to-v6 shape upgrade: an *older shape* under an older stamp, not
    /// merely a rewound stamp on a current file.
    ///
    /// This is the case the v5 bump got wrong. The columns really are absent,
    /// so a migration that early-returns, or that stamps without altering the
    /// table, produces a file that opens fine and then fails forever on the
    /// first `materialized_state()`. Assert the shape is repaired, the stamp
    /// lands on exactly the current version, and the state materializes.
    #[test]
    fn a_v5_shaped_journal_upgrades_to_current_and_materializes() {
        let (dir, journal) = open_tmp("upgrade-v5-shape");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        for column in [
            "plan_id",
            "run_service_url",
            "probe_attempts",
            "probe_deadline_unix",
        ] {
            conn.execute_batch(&format!("ALTER TABLE jobs DROP COLUMN {column};"))
                .unwrap();
        }
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 5u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let journal = Journal::open(&path).expect("a real v5 journal must upgrade");
        journal
            .load_state()
            .expect("the upgraded journal must materialize");
        journal
            .materialized_state()
            .expect("the upgraded journal must read its materialized tables");

        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(u32::try_from(version).unwrap(), JOURNAL_SCHEMA_VERSION);
        assert_eq!(
            JOURNAL_SCHEMA_VERSION, 11,
            "this test pins the current upgrade"
        );
        for column in [
            "plan_id",
            "run_service_url",
            "probe_attempts",
            "probe_deadline_unix",
        ] {
            assert!(
                table_has_column(&conn, "jobs", column).unwrap(),
                "{column} must exist after the upgrade"
            );
        }
        drop(conn);
        std::fs::remove_dir_all(dir).ok();
    }

    fn stamp_historic_jobs_shape(path: &Path, version: u32, drop_columns: &[&str]) {
        let conn = Connection::open(path).unwrap();
        for column in drop_columns {
            conn.execute_batch(&format!("ALTER TABLE jobs DROP COLUMN {column};"))
                .unwrap();
        }
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", version).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
    }

    #[test]
    fn a_historically_poisoned_v5_jobs_shape_is_repaired_before_materialization() {
        let (dir, journal) = open_tmp("repair-poisoned-v5");
        let path = dir.join("journal.db");
        drop(journal);
        stamp_historic_jobs_shape(
            &path,
            5,
            &[
                "provisional",
                "plan_id",
                "run_service_url",
                "probe_attempts",
                "probe_deadline_unix",
            ],
        );

        let journal = Journal::open(&path).expect("known v5 poison is repairable");
        journal
            .load_state()
            .expect("repaired v5 journal must replay");
        journal
            .materialized_state()
            .expect("repaired v5 journal must materialize");

        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(u32::try_from(version).unwrap(), JOURNAL_SCHEMA_VERSION);
        assert!(table_has_column(&conn, "jobs", "provisional").unwrap());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_v6_jobs_shape_missing_provisional_is_repaired_without_reversion() {
        let (dir, journal) = open_tmp("repair-poisoned-v6");
        let path = dir.join("journal.db");
        drop(journal);
        stamp_historic_jobs_shape(&path, 6, &["provisional"]);

        let journal = Journal::open(&path).expect("known v6 poison is repairable");
        journal
            .materialized_state()
            .expect("repaired v6 journal must materialize");

        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(u32::try_from(version).unwrap(), JOURNAL_SCHEMA_VERSION);
        assert!(table_has_column(&conn, "jobs", "provisional").unwrap());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_unrecognized_poisoned_jobs_shape_is_preserved_and_rejected() {
        let (dir, journal) = open_tmp("reject-poisoned-jobs");
        let path = dir.join("journal.db");
        drop(journal);
        stamp_historic_jobs_shape(
            &path,
            5,
            &[
                "provisional",
                "plan_id",
                "run_service_url",
                "probe_attempts",
                "probe_deadline_unix",
                "worker",
            ],
        );

        let error = Journal::open(&path).expect_err("partial job shape must fail closed");
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5);
        assert!(!table_has_column(&conn, "jobs", "provisional").unwrap());
        std::fs::remove_dir_all(dir).ok();
    }

    /// Helper: a Ready slot carrying one provisional acquisition.
    fn prime_provisional(journal: &mut Journal, scope: &str, message_job: &str) {
        prime_ready(journal, scope);
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot(scope),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot(scope),
                job_id: job(message_job),
                generation: r#gen(),
                message_id: message_job.into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
    }

    /// The retarget is one event because the two-event alternative frees the
    /// slot in between. Assert the slot never becomes free and no capacity is
    /// advertised: a permit released here is a second runner picking up work
    /// this node is already committed to.
    #[test]
    fn resolving_an_acquisition_retargets_the_row_without_freeing_the_slot() {
        let (dir, mut journal) = open_tmp("acquisition-retarget");
        prime_provisional(&mut journal, "scope-1", "request-1");

        let resolved = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: "plan-1".into(),
                generation: r#gen(),
            })
            .unwrap();
        assert!(!resolved.rejected);
        assert!(
            resolved.commands.is_empty(),
            "retargeting must not advertise a permit"
        );

        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        let row = &state.jobs[0];
        assert_eq!(row.job_id, job("run-service-job-1"));
        assert_eq!(row.plan_id, "plan-1");
        assert_eq!(row.run_service_url, "https://run.example/run");
        assert!(
            row.provisional,
            "the retarget records identity, not ownership"
        );
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        assert_eq!(state.advertised_capacity(), 0);

        // The addressing survives promotion: a crash after JobOwned but before
        // the job starts still leaves something recovery can renew.
        journal
            .apply(Event::JobOwned {
                job_id: job("run-service-job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".into(),
                accepted_unix: 2_000,
            })
            .unwrap();
        let state = journal.load_state().unwrap();
        assert!(!state.jobs[0].provisional);
        assert_eq!(state.jobs[0].plan_id, "plan-1");
        assert_eq!(state.jobs[0].run_service_url, "https://run.example/run");
        std::fs::remove_dir_all(dir).ok();
    }

    /// Retargeting is scoped to provisional rows and to free identities.
    /// Rewriting an owned row's identity would move a job that may already
    /// carry a terminal result or an outbox payload.
    #[test]
    fn only_a_provisional_row_may_be_retargeted_onto_a_free_identity() {
        let (dir, mut journal) = open_tmp("acquisition-retarget-refusals");
        prime_provisional(&mut journal, "scope-1", "request-1");

        // Wrong generation.
        assert!(
            journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id: job("request-1"),
                    acquired_job_id: job("run-service-job-1"),
                    plan_id: "plan-1".into(),
                    generation: Generation(r#gen().0 + 1),
                })
                .unwrap()
                .rejected
        );

        // Unknown provisional row.
        assert!(
            journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id: job("request-absent"),
                    acquired_job_id: job("run-service-job-1"),
                    plan_id: "plan-1".into(),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );

        journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: "plan-1".into(),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("run-service-job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".into(),
                accepted_unix: 2_000,
            })
            .unwrap();

        // An owned row is no longer retargetable.
        assert!(
            journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id: job("run-service-job-1"),
                    acquired_job_id: job("run-service-job-2"),
                    plan_id: "plan-2".into(),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        assert_eq!(
            journal.load_state().unwrap().jobs[0].job_id,
            job("run-service-job-1")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// `renewjob` extends the lease as a side effect, so an indeterminate probe
    /// that repeats on every restart would renew a job this node can never make
    /// progress on, forever. The budget is durable and the reducer owns it.
    #[test]
    fn a_provisional_row_carries_a_durable_probe_budget() {
        let (dir, mut journal) = open_tmp("acquisition-probe-budget");
        prime_provisional(&mut journal, "scope-1", "request-1");

        let row = journal.load_state().unwrap().jobs[0].clone();
        assert_eq!(row.probe_attempts, 0);
        assert_eq!(
            row.probe_deadline_unix,
            1_000 + ACQUISITION_RESOLUTION_SECONDS,
            "the deadline is stamped from the intent's own clock"
        );
        assert!(!row.probe_budget_exhausted(1_000));
        // Time alone spends it, even with attempts left.
        assert!(row.probe_budget_exhausted(row.probe_deadline_unix));

        for spent in 1..=MAX_ACQUISITION_PROBES {
            let outcome = journal
                .apply(Event::AcquisitionProbeFailed {
                    job_id: job("request-1"),
                    generation: r#gen(),
                })
                .unwrap();
            assert!(!outcome.rejected);
            assert_eq!(journal.load_state().unwrap().jobs[0].probe_attempts, spent);
        }
        assert!(journal.load_state().unwrap().jobs[0].probe_budget_exhausted(1_000));

        // The bound is what lets the row terminate and the slot come back.
        let lost = journal
            .apply(Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation: r#gen(),
                reason: "probe budget spent".into(),
            })
            .unwrap();
        assert!(!lost.rejected);
        assert!(journal.load_state().unwrap().jobs.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    /// A probe charge is refused for a row that is not provisional: an owned
    /// job is never probed, and letting the counter move there would let a
    /// caller invent a budget for something the oracle already settled.
    #[test]
    fn a_probe_failure_is_refused_for_a_row_that_is_not_provisional() {
        let (dir, mut journal) = open_tmp("acquisition-probe-owned");
        prime_provisional(&mut journal, "scope-1", "request-1");
        journal
            .apply(Event::JobOwned {
                job_id: job("request-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".into(),
                accepted_unix: 2_000,
            })
            .unwrap();
        assert!(
            journal
                .apply(Event::AcquisitionProbeFailed {
                    job_id: job("request-1"),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_provisional_row_cannot_back_a_completion() {
        let (dir, mut journal) = open_tmp("provisional-not-owner");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();

        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1, "the slot is occupied");
        assert!(state.jobs[0].provisional);

        // Ownership is not proven, so a completion intent is refused.
        let intent = journal
            .apply(Event::CompletionIntended {
                job_id: job("guid-1"),
                generation: r#gen(),
                payload_sha256: payload_checksum(b"ok"),
            })
            .unwrap();
        assert!(
            intent.rejected,
            "a provisional row must not prove ownership"
        );
        assert!(journal.load_state().unwrap().outbox.is_empty());

        // A 200 promotes it, and the same intent is then accepted.
        journal
            .apply(Event::JobOwned {
                job_id: job("guid-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        let state = journal.load_state().unwrap();
        assert!(!state.jobs[0].provisional, "a 200 proves ownership");
        let intent = journal
            .apply(Event::CompletionIntended {
                job_id: job("guid-1"),
                generation: r#gen(),
                payload_sha256: payload_checksum(b"ok"),
            })
            .unwrap();
        assert!(!intent.rejected);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A provisional row can be dropped when the probe proves the job is not
    /// ours. An owned row cannot: once ownership is proven the job has to reach
    /// a terminal state through completion, never by being forgotten.
    #[test]
    fn only_a_provisional_row_may_be_abandoned_by_acquisition_loss() {
        let (dir, mut journal) = open_tmp("provisional-loss");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        let lost = journal
            .apply(Event::JobAcquisitionLost {
                job_id: job("guid-1"),
                generation: r#gen(),
                reason: "another runner holds the lease".into(),
            })
            .unwrap();
        assert!(!lost.rejected);
        assert_eq!(
            lost.commands,
            vec![SideEffect::AdvertiseCapacity { permits: 1 }]
        );
        let state = journal.load_state().unwrap();
        assert!(state.jobs.is_empty(), "the slot is freed");
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        assert_eq!(state.advertised_capacity(), 1);

        // Now prove ownership and try again: it must be refused.
        let reacquire = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("guid-2"),
                generation: r#gen(),
                message_id: "msg-2".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        assert!(!reacquire.rejected);
        let owned = journal
            .apply(Event::JobOwned {
                job_id: job("guid-2"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        assert!(!owned.rejected);
        let lost = journal
            .apply(Event::JobAcquisitionLost {
                job_id: job("guid-2"),
                generation: r#gen(),
                reason: "should not be allowed".into(),
            })
            .unwrap();
        assert!(lost.rejected, "an owned job cannot be forgotten");
        assert_eq!(journal.load_state().unwrap().jobs.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remote_ack_restores_ready() {
        let (dir, mut journal) = open_tmp("ack-ready");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("guid-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        journal
            .apply(Event::CompletionIntended {
                job_id: job("guid-1"),
                generation: r#gen(),
                payload_sha256: payload_checksum(b"ok"),
            })
            .unwrap();
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("guid-1"),
                generation: r#gen(),
            })
            .unwrap();
        let acked = journal
            .apply(Event::RemoteAcked {
                job_id: job("guid-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(!acked.rejected);
        let state = journal.load_state().unwrap();
        assert!(state.jobs.is_empty(), "{:?}", state.jobs);
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        assert!(state.slots[0].phase.counts_as_ready());
        drop(journal);

        let reopened = Journal::open(dir.join("journal.db")).unwrap();
        assert!(reopened
            .has_remote_terminal_ack(&job("guid-1"), r#gen())
            .unwrap());
        let replayed = reopened.load_state().unwrap();
        assert!(replayed.outbox.iter().any(|row| {
            row.job_id == job("guid-1") && row.generation == r#gen() && row.remote_acked
        }));
        assert!(reopened.materialized_state().unwrap().outbox.is_empty());
    }

    #[test]
    fn duplicate_acquisition_intent_is_rejected() {
        let (_dir, mut journal) = open_tmp("dup-assign");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        let first = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        assert!(!first.rejected);
        let second = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-2"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        assert!(second.rejected);
    }

    #[test]
    fn package_retire_blocked_while_outbox_pending() {
        let (_dir, mut journal) = open_tmp("pkg");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        journal
            .apply(Event::PackageActivated {
                apt_version: "0.1.209".into(),
                generation: 3,
            })
            .unwrap();
        let retire = journal
            .apply(Event::PackageRetireIntended { generation: 3 })
            .unwrap();
        assert!(retire.rejected);
        assert_eq!(journal.load_state().unwrap().package_generation, 3);
    }

    #[test]
    fn slot_and_job_phase_vocabularies_are_separated() {
        for retired in ["starting", "retiring", "degraded", "quarantined"] {
            let slot_error = parse_slot_phase(retired).unwrap_err();
            assert_eq!(slot_error.envelope.reason, "journal.materialized.invalid");
            let job_error = parse_job_phase(retired).unwrap_err();
            assert_eq!(job_error.envelope.reason, "journal.materialized.invalid");
        }
        // Cross-type phases fail closed: a slot can never be Running or
        // Completing, and a job can never carry a slot-only phase.
        for job_only in ["running", "completing"] {
            let error = parse_slot_phase(job_only).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        }
        for slot_only in ["absent", "provisioning", "registered", "ready", "fenced"] {
            let error = parse_job_phase(slot_only).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        }
        for live in [
            "absent",
            "provisioning",
            "registered",
            "ready",
            "assigned",
            "fenced",
        ] {
            assert!(parse_slot_phase(live).is_ok(), "{live}");
        }
        for live in ["assigned", "running", "completing"] {
            assert!(parse_job_phase(live).is_ok(), "{live}");
        }
        // Occupancy is a property of the job type: every job phase occupies.
        for phase in JobPhase2::ALL {
            assert!(phase.occupies_slot(), "{phase:?}");
        }
        for phase in SlotPhase2::ALL {
            assert_eq!(phase.counts_as_ready(), phase == SlotPhase2::Ready);
        }
    }

    #[test]
    fn v7_journal_migrates_phase_vocabulary_to_v8() {
        let (dir, mut journal) = open_tmp("v7-to-v8");
        let path = dir.join("journal.db");
        let g = prime_running_job(&mut journal, "scope-1", "job-1");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.pragma_update(None, "user_version", 7u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let reopened = Journal::open_for_service_instance(&path, "legacy-migration").unwrap();
        let version: i64 = reopened
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        let state = reopened.materialized_state().unwrap();
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        assert_eq!(state.jobs[0].phase, JobPhase2::Running);
        assert_eq!(state.jobs[0].generation, g);
    }

    #[test]
    fn v7_migration_fails_closed_on_cross_type_phase_rows() {
        let (dir, mut journal) = open_tmp("v7-to-v8-poison");
        let path = dir.join("journal.db");
        prime_running_job(&mut journal, "scope-1", "job-1");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_replay_baseline_fence(&conn);
        drop_schema10_pressure_and_schema11_identity(&conn);
        conn.execute("UPDATE slots SET phase = 'running'", [])
            .unwrap();
        conn.pragma_update(None, "user_version", 7u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
    }

    #[test]
    fn package_retire_succeeds_without_jobs_or_outbox() {
        let (_dir, mut journal) = open_tmp("pkg-ok");
        journal
            .apply(Event::PackageActivated {
                apt_version: "0.1.209".into(),
                generation: 3,
            })
            .unwrap();
        let retire = journal
            .apply(Event::PackageRetireIntended { generation: 3 })
            .unwrap();
        assert!(!retire.rejected);
        let state = journal.load_state().unwrap();
        assert_eq!(state.package_generation, 0);
        assert!(state.package_apt_version.is_empty());
    }
}
