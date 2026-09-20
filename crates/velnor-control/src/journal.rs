//! Durable node journal: WAL + `synchronous=FULL`, immutable events, reducer.
//!
//! Side-effect commands are returned only after the intent event is committed.
//! Completions are fail-closed around one durable local send claim: an outbox
//! row survives until a remote acknowledgement (or observed terminal) is
//! itself committed.

use std::collections::{HashMap, HashSet};
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
/// must not apply events (N-1 must not clobber an N writer's log).
///
/// Every terminal-affecting event rides a bump here. `Journal::open` stamps
/// the current version onto an older journal *before* any event may be
/// written, so a binary that predates the bump refuses the file outright
/// instead of decoding it with an incomplete event vocabulary.
pub const JOURNAL_SCHEMA_VERSION: u32 = 14;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_RETRIES: u32 = 5;
const SETUP_BACKOFF_STEP: Duration = Duration::from_millis(40);
const MAX_TERMINAL_ACK_SCAN_ROWS: i64 = 1_024;

/// Durable send attempts a completion may burn before it is unresolvable.
/// Each attempt is one full transport retry loop, not one HTTP request.
pub const MAX_COMPLETION_ATTEMPTS: u32 = 8;

/// Wall-clock budget for resolving one completion, from the moment its intent
/// became durable. GitHub's own default job timeout is six hours; a payload
/// older than that can no longer be delivered usefully, so holding its slot
/// hostage buys nothing.
pub const COMPLETION_RESOLUTION_SECONDS: u64 = 6 * 60 * 60;

/// Durable probes allowed for one provisional acquisition.
///
/// Each probe is one full `renewjob` call, made once per slot startup, so this
/// is a count of restarts and not of HTTP requests. Eight is the completion
/// budget: past that many restarts no further renewal is attempted. The row
/// and its exact native permit remain fenced until authoritative terminal
/// evidence arrives; exhausting a local retry budget does not prove the
/// remote assignment ended.
pub const MAX_ACQUISITION_PROBES: u32 = 8;

/// Wall-clock budget for resolving one provisional acquisition, from the
/// moment the intent became durable.
///
/// The same six hours the completion budget uses. This stops further renewal
/// attempts after the likely job lifetime; it is not terminal evidence and
/// cannot by itself release the permit or remove the provisional row.
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
    probe_deadline_unix INTEGER NOT NULL DEFAULT 0,
    runner_request_id TEXT NOT NULL DEFAULT '',
    permit_lease TEXT
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
";

const JOURNAL_WRITE_GATE_SCHEMA: &str = "
CREATE TABLE journal_write_gate (
    id INTEGER PRIMARY KEY CHECK (id = 1)
);
CREATE TRIGGER journal_write_gate_events_insert
BEFORE INSERT ON events
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_events_update
BEFORE UPDATE ON events
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_events_delete
BEFORE DELETE ON events
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_slots_insert
BEFORE INSERT ON slots
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_slots_update
BEFORE UPDATE ON slots
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_slots_delete
BEFORE DELETE ON slots
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_jobs_insert
BEFORE INSERT ON jobs
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_jobs_update
BEFORE UPDATE ON jobs
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_jobs_delete
BEFORE DELETE ON jobs
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_outbox_insert
BEFORE INSERT ON outbox
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_outbox_update
BEFORE UPDATE ON outbox
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_outbox_delete
BEFORE DELETE ON outbox
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_meta_insert
BEFORE INSERT ON meta
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_meta_update
BEFORE UPDATE ON meta
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
CREATE TRIGGER journal_write_gate_meta_delete
BEFORE DELETE ON meta
WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
";

/// Fleet materialization the reducer reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

fn job_has_acquired_identity(job: &JobRecord) -> bool {
    !job.job_id.0.is_empty()
        && !job.plan_id.is_empty()
        && !job.run_service_url.is_empty()
        && !job.runner_request_id.is_empty()
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
                && job_has_acquired_identity(job)
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

fn stamp_event(event: &mut Event, recorded_unix: u64) {
    if let Event::JobOwned { accepted_unix, .. } = event
        && *accepted_unix == 0
    {
        *accepted_unix = recorded_unix;
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
    /// Stable broker request identity that first occupied this slot. It is
    /// retained when the run-service job ID replaces the provisional row, so
    /// permit ownership remains correlated across restart and redelivery.
    #[serde(default)]
    pub runner_request_id: String,
    /// Exact host permit acquired before the run-service request. Provisional
    /// recovery must retain this immutable lease identity until remote
    /// ownership or terminal release is proven.
    #[serde(default)]
    pub permit_lease: Option<NativePermitLease>,
    /// A pre-v12 loss event carried no typed remote/lease proof. Replay keeps
    /// that provisional holder permanently fenced until later exact terminal
    /// proof arrives; later ownership or worker-loss events cannot erase the
    /// ambiguity.
    #[serde(default)]
    pub acquisition_loss_unproven: bool,
    /// Durable count of spent recovery probes. Only the reducer moves it.
    pub probe_attempts: u32,
    /// Wall-clock instant past which this provisional row is unresolvable.
    pub probe_deadline_unix: u64,
}

/// Exact ledger lease backing one native run-service acquisition.
///
/// `generation` is immutable lease identity, not the ledger's current host
/// epoch. Recovery may release only this exact holder from this exact ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePermitLease {
    pub holder: String,
    pub ledger_path: String,
    pub generation: u64,
}

/// Run-service terminal evidence that permits a provisional acquisition to
/// be forgotten after its exact host permit lease is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcquisitionLossSource {
    /// The typed acquire-job response proves the request is no longer live.
    AcquireJobNotFound,
    /// `renewjob` proves this runner does not own the acquired job.
    RenewJobNotOurs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionLossProof {
    pub source: AcquisitionLossSource,
    pub permit_lease: NativePermitLease,
}

impl JobRecord {
    /// Whether the recovery budget for a provisional row is spent.
    ///
    /// Mirrors `OutboxRecord::budget_exhausted`, and exists for the same
    /// reason: `renewjob` extends the lease as a side effect, so a job this
    /// node owns but can never execute must not be probed forever. An exhausted
    /// budget stops future renewals; only authoritative terminal evidence may
    /// remove the row and release its exact permit.
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

/// Intent events. The reducer never performs I/O.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
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
        /// Explicit broker request identity. Older events omit it; their
        /// provisional key is the only safe historical fallback.
        #[serde(default)]
        runner_request_id: Option<String>,
        /// Where the acquisition was addressed. Carried from the broker message
        /// so recovery knows which run service to probe without re-deriving it.
        run_service_url: String,
        /// When the intent became durable. The reducer is pure, so the probe
        /// deadline has to be stamped from the caller's clock, exactly as
        /// `JobOwned` stamps `accepted_unix`.
        intended_unix: u64,
    },
    /// Native acquisition intent with the exact permit lease already held by
    /// this request. Kept as a distinct event so older writers cannot silently
    /// deserialize and discard the lease fields.
    JobAcquisitionIntendedWithPermit {
        slot_id: SlotId,
        job_id: JobId,
        generation: Generation,
        message_id: String,
        runner_request_id: String,
        run_service_url: String,
        intended_unix: u64,
        permit_lease: NativePermitLease,
    },
    /// The acquire reply came back and named the job. Retargets the provisional
    /// row from the runner request identity onto the run-service identity, and
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
        /// Explicit broker request identity. Older resolve events do not
        /// carry this; distinct provisional-to-acquired transitions can
        /// recover it from the provisional row, while old self-to-self
        /// rebuilds remain unknown and must fail closed on redelivery.
        #[serde(default)]
        runner_request_id: Option<String>,
        /// When present, the resolver must still see this exact immutable
        /// permit identity on the provisional row before retargeting it.
        /// Older events omit it and can only preserve the association already
        /// present in their row.
        #[serde(default)]
        permit_lease: Option<NativePermitLease>,
    },
    /// Rebuild an acquisition that the run service already confirmed with a
    /// 200, when the pre-call intent is missing. One event carries all fields
    /// required for a durable recovery probe; an intent-then-resolve pair
    /// would leave a planless row that startup can mistake for an unowned job.
    /// This records existing ownership evidence, so admission fences do not
    /// reject it, but the recorded slot generation and all current claims must
    /// still match exactly.
    JobAcquisitionRebuilt {
        slot_id: SlotId,
        job_id: JobId,
        generation: Generation,
        runner_request_id: String,
        plan_id: String,
        run_service_url: String,
        probe_deadline_unix: u64,
    },
    /// v11 form of the confirmed-acquisition rebuild. It carries the exact
    /// permit lease from the active guard, so later provisional recovery never
    /// has to infer a holder, ledger, or lease generation.
    JobAcquisitionRebuiltWithPermit {
        slot_id: SlotId,
        job_id: JobId,
        generation: Generation,
        runner_request_id: String,
        plan_id: String,
        run_service_url: String,
        probe_deadline_unix: u64,
        permit_lease: NativePermitLease,
    },
    /// One recovery probe was spent without reaching a verdict. Charged to the
    /// row's durable budget so an unreachable run service cannot make this node
    /// renew the same lease on every restart forever.
    AcquisitionProbeFailed {
        job_id: JobId,
        generation: Generation,
    },
    /// The provisional row could not be resolved to ownership. The reducer
    /// drops it only when `proof` records authoritative run-service evidence
    /// and the exact native permit lease has already been durably released.
    JobAcquisitionLost {
        job_id: JobId,
        generation: Generation,
        reason: String,
        /// Missing on pre-v12 events. Those histories are ambiguous and keep
        /// their provisional row; a human-readable reason is not proof.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proof: Option<AcquisitionLossProof>,
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

/// Commands and rejection status for one committed journal event.
///
/// Unlike [`ReduceOutcome`], this does not copy the fleet snapshot into every
/// per-event result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplySummary {
    pub commands: Vec<SideEffect>,
    pub rejected: bool,
}

struct ObservationReduction {
    commands: Vec<SideEffect>,
    rejected: bool,
    changed: bool,
    created_slot: bool,
}

#[derive(Clone, Copy)]
enum SlotProof {
    Executor,
    Session,
}

fn reduce_slot_proof(
    state: &mut FleetState,
    slot_id: &SlotId,
    generation: Generation,
    proof: SlotProof,
) -> ObservationReduction {
    let created_slot = !state.slots.iter().any(|slot| slot.slot_id == *slot_id);
    let slot = state.slot_mut(slot_id);
    if generation != slot.generation || slot.phase == SlotPhase2::Fenced {
        return ObservationReduction {
            commands: Vec::new(),
            rejected: true,
            changed: false,
            created_slot,
        };
    }

    let changed = match proof {
        SlotProof::Executor => {
            let changed = !slot.executor_proven;
            slot.executor_proven = true;
            changed
        }
        SlotProof::Session => {
            let changed = !slot.session_live;
            slot.session_live = true;
            changed
        }
    };
    ObservationReduction {
        commands: Vec::new(),
        rejected: false,
        changed,
        created_slot,
    }
}

fn reduce_slot_heartbeat(
    state: &mut FleetState,
    slot_id: &SlotId,
    generation: Generation,
    pid: u32,
    recorded_unix: u64,
) -> ObservationReduction {
    let created_slot = !state.slots.iter().any(|slot| slot.slot_id == *slot_id);
    let slot = state.slot_mut(slot_id);
    if generation != slot.generation || slot.phase == SlotPhase2::Fenced {
        return ObservationReduction {
            commands: Vec::new(),
            rejected: true,
            changed: false,
            created_slot,
        };
    }

    let changed = slot.pid != Some(pid) || slot.heartbeat_unix != recorded_unix;
    slot.pid = Some(pid);
    slot.heartbeat_unix = recorded_unix;
    ObservationReduction {
        commands: Vec::new(),
        rejected: false,
        changed,
        created_slot,
    }
}

fn reduce_registration_intended(
    state: &mut FleetState,
    slot_id: &SlotId,
    generation: Generation,
) -> ObservationReduction {
    let slot_admission_blocked = slot_has_active_job(state, slot_id)
        || pending_outbox_blocks_admission(state, slot_id, generation);
    let fleet_blocked = fleet_admission_blocked(state);
    let created_slot = !state.slots.iter().any(|slot| slot.slot_id == *slot_id);
    let slot = state.slot_mut(slot_id);
    if generation != slot.generation
        || slot.phase == SlotPhase2::Fenced
        || slot_admission_blocked
        || fleet_blocked
        || slot.ready_proof().is_err()
    {
        return ObservationReduction {
            commands: Vec::new(),
            rejected: true,
            changed: false,
            created_slot,
        };
    }

    ObservationReduction {
        commands: vec![SideEffect::RegisterRunner {
            slot_id: slot_id.clone(),
            generation,
        }],
        rejected: false,
        changed: false,
        created_slot,
    }
}

fn reduce_job_acquisition_intended(
    state: &mut FleetState,
    slot_id: SlotId,
    job_id: JobId,
    generation: Generation,
    runner_request_id: Option<String>,
    run_service_url: String,
    intended_unix: u64,
    permit_lease: Option<NativePermitLease>,
) -> bool {
    // Occupy the slot before calling the run service, so a crash in the
    // acquire window leaves evidence. A permit-bound event records the exact
    // lease identity in the same durable transition.
    let runner_request_id = runner_request_id.unwrap_or_else(|| job_id.0.clone());
    let invalid_permit_lease = permit_lease
        .as_ref()
        .is_some_and(|permit| !native_permit_lease_is_valid(permit));
    let admission_blocked = fleet_admission_blocked(state);
    let slot_index = state.slots.iter().position(|slot| slot.slot_id == slot_id);
    let slot_ready = slot_index.is_some_and(|index| {
        state.slots[index].generation == generation && state.slots[index].phase == SlotPhase2::Ready
    });
    if job_id.0.is_empty()
        || runner_request_id.is_empty()
        || run_service_url.is_empty()
        || !slot_ready
        || admission_blocked
        || invalid_permit_lease
        || state_has_unknown_request_identity(state)
        || acquisition_identity_conflicts(
            &state.jobs,
            &job_id,
            &runner_request_id,
            permit_lease.as_ref(),
            None,
        )
    {
        return true;
    }

    let Some(slot_index) = slot_index else {
        return true;
    };
    state.slots[slot_index].phase = SlotPhase2::Assigned;
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
        plan_id: String::new(),
        run_service_url,
        runner_request_id,
        permit_lease,
        acquisition_loss_unproven: false,
        probe_attempts: 0,
        probe_deadline_unix: intended_unix.saturating_add(ACQUISITION_RESOLUTION_SECONDS),
    });
    false
}

fn reduce_job_acquisition_rebuilt(
    state: &mut FleetState,
    slot_id: SlotId,
    job_id: JobId,
    generation: Generation,
    runner_request_id: String,
    plan_id: String,
    run_service_url: String,
    probe_deadline_unix: u64,
    permit_lease: Option<NativePermitLease>,
) -> bool {
    let slot_index = state.slots.iter().position(|slot| slot.slot_id == slot_id);
    let outbox_blocks = pending_outbox_blocks_admission(state, &slot_id, generation);
    let slot_ready = slot_index.is_some_and(|index| {
        let slot = &state.slots[index];
        slot.generation == generation && slot.phase == SlotPhase2::Ready
    });
    let invalid_permit_lease = permit_lease
        .as_ref()
        .is_some_and(|permit| !native_permit_lease_is_valid(permit));
    if runner_request_id.is_empty()
        || job_id.0.is_empty()
        || plan_id.is_empty()
        || run_service_url.is_empty()
        || invalid_permit_lease
        || state_has_unknown_request_identity(state)
        || acquisition_identity_conflicts(
            &state.jobs,
            &job_id,
            &runner_request_id,
            permit_lease.as_ref(),
            None,
        )
        || !slot_ready
        || slot_has_active_job(state, &slot_id)
        || outbox_blocks
    {
        return true;
    }
    let Some(slot_index) = slot_index else {
        return true;
    };

    // This event records a run-service 200 already received. It must survive a
    // drain/cordon that arrived after that call, but cannot replace a stale
    // generation or conflicting claim.
    state.slots[slot_index].phase = SlotPhase2::Assigned;
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
        plan_id,
        run_service_url,
        runner_request_id,
        permit_lease,
        acquisition_loss_unproven: false,
        probe_attempts: 0,
        probe_deadline_unix,
    });
    false
}

fn native_permit_lease_is_valid(permit: &NativePermitLease) -> bool {
    !permit.holder.is_empty() && Path::new(&permit.ledger_path).is_absolute()
}

/// An empty request ID is retained only as unknown historical evidence. It
/// cannot establish that a new broker request is distinct from that holder,
/// so any such row closes acquisition admission until explicit recovery.
fn state_has_unknown_request_identity(state: &FleetState) -> bool {
    state
        .jobs
        .iter()
        .any(|job| job.runner_request_id.is_empty())
}

fn acquisition_identity_conflicts(
    jobs: &[JobRecord],
    job_id: &JobId,
    runner_request_id: &str,
    permit_lease: Option<&NativePermitLease>,
    except_index: Option<usize>,
) -> bool {
    jobs.iter().enumerate().any(|(index, job)| {
        if except_index == Some(index) {
            return false;
        }
        job.job_id == *job_id
            || (!runner_request_id.is_empty() && job.job_id.0 == runner_request_id)
            || (!job.runner_request_id.is_empty() && job.runner_request_id == runner_request_id)
            || (!job.runner_request_id.is_empty() && job.runner_request_id == job_id.0)
            || permit_lease.is_some_and(|lease| job.permit_lease.as_ref() == Some(lease))
    })
}

fn validate_job_identity_uniqueness(jobs: &[JobRecord]) -> StoreResult<()> {
    let mut job_ids = HashSet::with_capacity(jobs.len());
    let mut request_ids = HashSet::with_capacity(jobs.len());
    let mut permit_leases = HashSet::with_capacity(jobs.len());
    for job in jobs {
        if !job.runner_request_id.is_empty()
            && (request_ids.contains(job.job_id.0.as_str())
                || job_ids.contains(job.runner_request_id.as_str()))
        {
            return Err(invalid_materialized(
                "job identity",
                &format!("job ID {} aliases another runner request ID", job.job_id.0),
            ));
        }
        if job.job_id.0.is_empty() || !job_ids.insert(job.job_id.0.as_str()) {
            return Err(invalid_materialized(
                "job identity",
                &format!("empty or duplicate job ID {}", job.job_id.0),
            ));
        }
        if !job.runner_request_id.is_empty() && !request_ids.insert(job.runner_request_id.as_str())
        {
            return Err(invalid_materialized(
                "job runner request ID",
                &format!("duplicate runner request ID {}", job.runner_request_id),
            ));
        }
        if let Some(lease) = &job.permit_lease {
            let identity = (
                lease.holder.as_str(),
                lease.ledger_path.as_str(),
                lease.generation,
            );
            if !permit_leases.insert(identity) {
                return Err(invalid_materialized(
                    "job permit lease",
                    &format!("duplicate permit holder {}", lease.holder),
                ));
            }
        }
    }
    Ok(())
}

fn reduce_job_acquisition_resolved(
    state: &mut FleetState,
    provisional_job_id: JobId,
    acquired_job_id: JobId,
    plan_id: String,
    generation: Generation,
    runner_request_id: Option<String>,
    permit_lease: Option<NativePermitLease>,
) -> bool {
    let matches: Vec<_> = state
        .jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| {
            job.job_id == provisional_job_id && job.generation == generation && job.provisional
        })
        .map(|(index, _)| index)
        .collect();
    let [index] = matches.as_slice() else {
        return true;
    };
    let index = *index;
    let row = &state.jobs[index];

    let ambiguous_self_resolve =
        provisional_job_id == acquired_job_id && runner_request_id.is_none();
    let resolved_request_id = match runner_request_id.as_deref() {
        Some(request_id) if !request_id.is_empty() => Some(request_id.to_owned()),
        Some(_) => None,
        None if provisional_job_id != acquired_job_id && !row.runner_request_id.is_empty() => {
            Some(row.runner_request_id.clone())
        }
        None => None,
    };
    let invalid = row.acquisition_loss_unproven
        || state_has_unknown_request_identity(state)
        || acquired_job_id.0.is_empty()
        || plan_id.is_empty()
        || row.run_service_url.is_empty()
        || ambiguous_self_resolve
        || resolved_request_id.as_deref().is_none_or(str::is_empty)
        || resolved_request_id
            .as_deref()
            .is_some_and(|request_id| row.runner_request_id != request_id)
        || permit_lease
            .as_ref()
            .is_some_and(|expected| row.permit_lease.as_ref() != Some(expected))
        || row
            .permit_lease
            .as_ref()
            .is_some_and(|lease| !native_permit_lease_is_valid(lease))
        || acquisition_identity_conflicts(
            &state.jobs,
            &acquired_job_id,
            resolved_request_id.as_deref().unwrap_or_default(),
            row.permit_lease.as_ref(),
            Some(index),
        );

    if invalid {
        // Replayed old self-to-self rebuilds and malformed/conflicting resolves
        // leave an occupied, explicitly uncertain row. The new empty-plan
        // compatibility exception was unsafe: neither a plan nor an exact
        // request ID could be recovered from it.
        state.jobs[index].acquisition_loss_unproven = true;
        if ambiguous_self_resolve {
            state.jobs[index].runner_request_id.clear();
        }
        return true;
    }

    let row = &mut state.jobs[index];
    row.job_id = acquired_job_id;
    row.plan_id = plan_id;
    row.runner_request_id = resolved_request_id.unwrap_or_default();
    false
}

fn reduce_job_owned(
    state: &mut FleetState,
    job_id: JobId,
    slot_id: SlotId,
    attempt: u32,
    generation: Generation,
    worker: String,
    accepted_unix: u64,
) -> bool {
    let slot_matches = state
        .slots
        .iter()
        .find(|slot| slot.slot_id == slot_id)
        .is_some_and(|slot| slot.generation == generation && slot.phase == SlotPhase2::Assigned);
    let source_indices: Vec<_> = state
        .jobs
        .iter()
        .enumerate()
        .filter(|(_, job)| job.job_id == job_id && job.generation == generation)
        .map(|(index, _)| index)
        .collect();
    let other_generation = state
        .jobs
        .iter()
        .any(|job| job.job_id == job_id && job.generation != generation);
    let other_live = state
        .jobs
        .iter()
        .any(|job| job.slot_id == slot_id && job.job_id != job_id && job.phase.occupies_slot());
    if job_id.0.is_empty()
        || !slot_matches
        || other_generation
        || other_live
        || source_indices.len() > 1
    {
        return true;
    }

    let source_index = source_indices.first().copied();
    let (plan_id, run_service_url, runner_request_id, permit_lease, unknown_identity) =
        match source_index {
            Some(index) => {
                let source = &state.jobs[index];
                if !source.provisional
                    || source.acquisition_loss_unproven
                    || source.slot_id != slot_id
                {
                    if source.slot_id != slot_id {
                        state.jobs[index].acquisition_loss_unproven = true;
                    }
                    return true;
                }
                if source.plan_id.is_empty()
                    || source.run_service_url.is_empty()
                    || source.runner_request_id.is_empty()
                {
                    state.jobs[index].acquisition_loss_unproven = true;
                    return true;
                }
                (
                    source.plan_id.clone(),
                    source.run_service_url.clone(),
                    source.runner_request_id.clone(),
                    source.permit_lease.clone(),
                    source.runner_request_id.is_empty(),
                )
            }
            None => (String::new(), String::new(), String::new(), None, true),
        };
    let unknown_identity_elsewhere = state
        .jobs
        .iter()
        .enumerate()
        .any(|(index, job)| Some(index) != source_index && job.runner_request_id.is_empty());
    if unknown_identity_elsewhere {
        if let Some(index) = source_index {
            state.jobs[index].acquisition_loss_unproven = true;
        }
        return true;
    }
    let updated = JobRecord {
        job_id: job_id.clone(),
        slot_id,
        generation,
        attempt,
        worker,
        phase: JobPhase2::Assigned,
        accepted_unix,
        terminal_conclusion: None,
        provisional: false,
        plan_id,
        run_service_url,
        runner_request_id,
        permit_lease,
        // Old histories without an exact acquisition correlation are kept as
        // occupied forensic rows. They cannot be started, completed, or freed
        // by a worker-loss event under the current vocabulary.
        acquisition_loss_unproven: unknown_identity,
        probe_attempts: 0,
        probe_deadline_unix: 0,
    };
    if acquisition_identity_conflicts(
        &state.jobs,
        &updated.job_id,
        &updated.runner_request_id,
        updated.permit_lease.as_ref(),
        source_index,
    ) {
        if let Some(index) = source_index {
            state.jobs[index].acquisition_loss_unproven = true;
        }
        return true;
    }

    if let Some(index) = source_index {
        state.jobs[index] = updated;
    } else {
        state.jobs.push(updated);
    }
    false
}

/// Pure `State + Event -> New State + Commands`. No I/O.
#[must_use]
pub fn reduce(state: FleetState, event: Event) -> ReduceOutcome {
    reduce_at(state, event, unix_now())
}

/// Reduce an event using the timestamp captured when its durable row was
/// written. Event history must not re-read wall clock: doing so changes
/// heartbeat ages and completion deadlines every time the journal replays.
#[must_use]
fn reduce_at(mut state: FleetState, event: Event, recorded_unix: u64) -> ReduceOutcome {
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
            let outcome = reduce_slot_proof(&mut state, &slot_id, generation, SlotProof::Executor);
            commands = outcome.commands;
            rejected = outcome.rejected;
        }
        Event::SessionLive {
            slot_id,
            generation,
        } => {
            let outcome = reduce_slot_proof(&mut state, &slot_id, generation, SlotProof::Session);
            commands = outcome.commands;
            rejected = outcome.rejected;
        }
        Event::RegistrationIntended {
            slot_id,
            generation,
        } => {
            let outcome = reduce_registration_intended(&mut state, &slot_id, generation);
            commands = outcome.commands;
            rejected = outcome.rejected;
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
            runner_request_id,
            run_service_url,
            intended_unix,
        } => {
            rejected = reduce_job_acquisition_intended(
                &mut state,
                slot_id,
                job_id,
                generation,
                runner_request_id,
                run_service_url,
                intended_unix,
                None,
            );
        }
        Event::JobAcquisitionIntendedWithPermit {
            slot_id,
            job_id,
            generation,
            message_id: _,
            runner_request_id,
            run_service_url,
            intended_unix,
            permit_lease,
        } => {
            rejected = reduce_job_acquisition_intended(
                &mut state,
                slot_id,
                job_id,
                generation,
                Some(runner_request_id),
                run_service_url,
                intended_unix,
                Some(permit_lease),
            );
        }
        Event::JobAcquisitionResolved {
            provisional_job_id,
            acquired_job_id,
            plan_id,
            generation,
            runner_request_id,
            permit_lease,
        } => {
            rejected = reduce_job_acquisition_resolved(
                &mut state,
                provisional_job_id,
                acquired_job_id,
                plan_id,
                generation,
                runner_request_id,
                permit_lease,
            );
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
            proof,
        } => {
            // A diagnostic reason and exhausted local retry budget are not
            // terminal proof. Drop only after an authoritative run-service
            // verdict and a generation-fenced release of this exact lease.
            let matches: Vec<_> = state
                .jobs
                .iter()
                .enumerate()
                .filter(|(_, job)| job.job_id == job_id && job.generation == generation)
                .map(|(index, _)| index)
                .collect();
            if matches.len() != 1 {
                rejected = true;
            } else {
                let index = matches[0];
                let proof_matches = state.jobs[index].provisional
                    && proof.as_ref().is_some_and(|proof| {
                        state.jobs[index].permit_lease.as_ref() == Some(&proof.permit_lease)
                            && state
                                .jobs
                                .iter()
                                .filter(|job| {
                                    job.permit_lease.as_ref() == Some(&proof.permit_lease)
                                })
                                .count()
                                == 1
                    });
                if proof_matches {
                    restore_slot_after_job_removal(&mut state, &mut commands, &job_id);
                } else {
                    // This branch is reachable during replay of a v11 history:
                    // its reason-only event used to delete the row. Preserve
                    // the exact lease and sticky ambiguity so later
                    // same-ID intent/owned/worker-lost events cannot make that
                    // deletion appear valid under the v12 vocabulary.
                    state.jobs[index].acquisition_loss_unproven = true;
                    rejected = true;
                }
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
            rejected = reduce_job_owned(
                &mut state,
                job_id,
                slot_id,
                attempt,
                generation,
                worker,
                accepted_unix,
            );
        }
        Event::JobStarted { job_id, generation } => {
            if let Some(job) = state.jobs.iter_mut().find(|job| job.job_id == job_id) {
                if job.generation != generation
                    || job.provisional
                    || job.acquisition_loss_unproven
                    || !job_has_acquired_identity(job)
                {
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
                        && !job.provisional
                        && !job.acquisition_loss_unproven
                        && job_has_acquired_identity(job)
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
                    || job.acquisition_loss_unproven
                    || !job_has_acquired_identity(job)
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
                    let created = recorded_unix;
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
            if let Some(index) = state.outbox.iter().position(|row| row.job_id == job_id) {
                let valid = {
                    let row = &state.outbox[index];
                    row.generation == generation
                        && row.is_pending()
                        && row.send_started
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
                        && row.budget_exhausted(recorded_unix)
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
                        && !job.provisional
                        && !job.acquisition_loss_unproven
                        && job_has_acquired_identity(job) =>
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
            let outcome =
                reduce_slot_heartbeat(&mut state, &slot_id, generation, pid, recorded_unix);
            commands = outcome.commands;
            rejected = outcome.rejected;
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
        Event::JobAcquisitionRebuilt {
            slot_id,
            job_id,
            generation,
            runner_request_id,
            plan_id,
            run_service_url,
            probe_deadline_unix,
        } => {
            rejected = reduce_job_acquisition_rebuilt(
                &mut state,
                slot_id,
                job_id,
                generation,
                runner_request_id,
                plan_id,
                run_service_url,
                probe_deadline_unix,
                None,
            );
        }
        Event::JobAcquisitionRebuiltWithPermit {
            slot_id,
            job_id,
            generation,
            runner_request_id,
            plan_id,
            run_service_url,
            probe_deadline_unix,
            permit_lease,
        } => {
            rejected = reduce_job_acquisition_rebuilt(
                &mut state,
                slot_id,
                job_id,
                generation,
                runner_request_id,
                plan_id,
                run_service_url,
                probe_deadline_unix,
                Some(permit_lease),
            );
        }
    }
    ReduceOutcome {
        state,
        commands,
        rejected,
    }
}

/// Opened journal on local disk only.
#[derive(Debug)]
pub struct Journal {
    conn: Connection,
    path: PathBuf,
}

fn insert_event_row(
    tx: &rusqlite::Transaction<'_>,
    generation: Generation,
    kind: &str,
    payload: &str,
    recorded_unix: i64,
) -> StoreResult<()> {
    let generation_sql = generation_to_sql(generation)?;
    tx.execute(
        "INSERT INTO events (generation, kind, payload, checksum, recorded_unix)
         VALUES (?1, ?2, ?3, '', ?4)",
        params![generation_sql, kind, payload, recorded_unix],
    )?;
    let id = tx.last_insert_rowid();
    let checksum = event_row_checksum(id, generation_sql, kind, payload, recorded_unix)?;
    let changed = tx.execute(
        "UPDATE events SET checksum = ?1 WHERE id = ?2",
        params![checksum, id],
    )?;
    if changed != 1 {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.event.insert.incomplete",
        )
        .with_remediation(
            "event identity or checksum changed during the gated journal transaction",
        ));
    }
    Ok(())
}

impl Journal {
    /// Open (creating) a journal file. Parent directory must already exist.
    ///
    /// # Errors
    /// Missing parent, SQLite older than the WAL-reset fix, or schema setup.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref();
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
            match setup_journal(&mut conn) {
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
        };
        // Verify all existing event checksums once. The controller's steady
        // state must not replay an ever-growing log every two seconds.
        journal.load_state()?;
        Ok(journal)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
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

    /// Persist a controller observation batch without cloning the fleet state
    /// for every heartbeat, proof, and registration intent.
    ///
    /// Only `SlotHeartbeat`, `ExecutorProven`, `SessionLive`, and
    /// `RegistrationIntended` are accepted. Results stay in input order and
    /// contain only commands and rejection status. Commands are returned only
    /// after the event log and materialized state commit.
    ///
    /// # Errors
    /// SQLite or payload encode failures, invalid materialized state, or an
    /// event outside the controller-observation set.
    pub fn apply_observation_batch<I>(&mut self, events: I) -> StoreResult<Vec<ApplySummary>>
    where
        I: IntoIterator<Item = Event>,
    {
        let mut events = events.into_iter();
        let Some(first_event) = events.next() else {
            return Ok(Vec::new());
        };

        // Lock before loading state so another journal writer cannot commit
        // between this snapshot and our materialized-state rewrite.
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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

        let mut summaries = Vec::new();
        let mut pending = Vec::new();
        for event in std::iter::once(first_event).chain(events) {
            let recorded_unix = unix_now();
            let outcome = match &event {
                Event::SlotHeartbeat {
                    slot_id,
                    generation,
                    pid,
                } => reduce_slot_heartbeat(&mut state, slot_id, *generation, *pid, recorded_unix),
                Event::ExecutorProven {
                    slot_id,
                    generation,
                } => reduce_slot_proof(&mut state, slot_id, *generation, SlotProof::Executor),
                Event::SessionLive {
                    slot_id,
                    generation,
                } => reduce_slot_proof(&mut state, slot_id, *generation, SlotProof::Session),
                Event::RegistrationIntended {
                    slot_id,
                    generation,
                } => reduce_registration_intended(&mut state, slot_id, *generation),
                _ => {
                    return Err(StoreError::new(
                        velnor_model::ExitClass::Usage,
                        "journal.observation.event.invalid",
                    )
                    .with_remediation(
                        "controller observation batches accept only slot_heartbeat, executor_proven, session_live, and registration_intended events",
                    ));
                }
            };

            // The normal reducer materializes a default slot before rejecting
            // a mismatched generation, but `apply_many` discards that rejected
            // state. Undo that append here without copying the whole snapshot.
            if outcome.rejected && outcome.created_slot {
                let _ = state.slots.pop();
            }

            if !outcome.rejected && (outcome.changed || !outcome.commands.is_empty()) {
                generation_to_sql(event_generation(&event))?;
                let payload = serde_json::to_string(&event).map_err(|error| {
                    StoreError::new(velnor_model::ExitClass::Operation, "journal.encode.failed")
                        .with_remediation(error.to_string())
                })?;
                pending.push((
                    event_generation(&event),
                    event_kind(&event),
                    payload,
                    timestamp_to_sql(recorded_unix, "event recorded_unix")?,
                ));
            }
            summaries.push(ApplySummary {
                commands: outcome.commands,
                rejected: outcome.rejected,
            });
        }

        if pending.is_empty() {
            return Ok(summaries);
        }

        let tx = transaction;
        open_journal_write_gate(&tx)?;
        for (generation, kind, payload, recorded_unix) in pending {
            insert_event_row(&tx, generation, kind, &payload, recorded_unix)?;
        }
        persist_state(&tx, &state)?;
        close_journal_write_gate(&tx)?;
        tx.commit()?;
        Ok(summaries)
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
        let mut events = events.into_iter();
        let Some(first_event) = events.next() else {
            return Ok(Vec::new());
        };

        // Lock before reading materialized state. Controller, job, guardian,
        // and completion processes can overlap; a snapshot taken before the
        // write lock could otherwise clobber a concurrent committed event.
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
        for mut event in std::iter::once(first_event).chain(events) {
            let recorded_unix = unix_now();
            stamp_event(&mut event, recorded_unix);
            let outcome = reduce_at(state.clone(), event.clone(), recorded_unix);
            if !outcome.rejected {
                let unchanged_without_commands =
                    outcome.commands.is_empty() && outcome.state == state;
                state = outcome.state.clone();
                if !unchanged_without_commands {
                    generation_to_sql(event_generation(&event))?;
                    let payload = serde_json::to_string(&event).map_err(|error| {
                        StoreError::new(velnor_model::ExitClass::Operation, "journal.encode.failed")
                            .with_remediation(error.to_string())
                    })?;
                    pending.push((
                        event_generation(&event),
                        event_kind(&event),
                        payload,
                        timestamp_to_sql(recorded_unix, "event recorded_unix")?,
                    ));
                }
            }
            outcomes.push(outcome);
        }
        if pending.is_empty() {
            return Ok(outcomes);
        }

        let tx = transaction;
        open_journal_write_gate(&tx)?;
        for (generation, kind, payload, recorded_unix) in pending {
            insert_event_row(&tx, generation, kind, &payload, recorded_unix)?;
        }
        persist_state(&tx, &state)?;
        close_journal_write_gate(&tx)?;
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
        let generation_sql = generation_to_sql(generation)?;
        let mut statement = self.conn.prepare(
            "SELECT id, generation, kind, payload, checksum, recorded_unix
             FROM events
             WHERE generation = ?1
               AND kind IN ('remote_acked', 'remote_observed_terminal')
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![generation_sql, MAX_TERMINAL_ACK_SCAN_ROWS + 1],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
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
            let (id, event_generation_sql, kind, payload, checksum, recorded_unix) = row?;
            let expected =
                event_row_checksum(id, event_generation_sql, &kind, &payload, recorded_unix)?;
            if recorded_unix < 0 || expected != checksum {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.checksum.mismatch",
                )
                .with_remediation(
                    "the terminal acknowledgement event failed integrity verification",
                ));
            }
            let event: Event = serde_json::from_str(&payload).map_err(|_| {
                StoreError::new(velnor_model::ExitClass::Conflict, "journal.event.invalid")
                    .with_remediation("the terminal acknowledgement event could not be decoded")
            })?;
            if kind != event_kind(&event)
                || i64_u64(event_generation_sql, "event generation")? != event_generation(&event).0
            {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.generation.mismatch",
                ));
            }
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
            "SELECT id, generation, kind, payload, checksum, recorded_unix
             FROM events
             WHERE kind IN ('completion_unresolvable', 'completion_payload_lost')
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![MAX_TERMINAL_ACK_SCAN_ROWS], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut found = Vec::new();
        for row in rows {
            let (id, generation, kind, payload, checksum, recorded_unix) = row?;
            let expected = event_row_checksum(id, generation, &kind, &payload, recorded_unix)?;
            if recorded_unix < 0 || expected != checksum {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.checksum.mismatch",
                )
                .with_remediation("an abandoned completion event failed integrity verification"));
            }
            let event: Event = serde_json::from_str(&payload).map_err(|_| {
                StoreError::new(velnor_model::ExitClass::Conflict, "journal.event.invalid")
                    .with_remediation("an abandoned completion event could not be decoded")
            })?;
            if kind != event_kind(&event)
                || i64_u64(generation, "event generation")? != event_generation(&event).0
            {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.generation.mismatch",
                ));
            }
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
    /// new `Event` variant: older binaries must keep opening (and ignoring)
    /// this state, and the event vocabulary is frozen for drain purposes.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn set_drain(&mut self, version: u64) -> StoreResult<bool> {
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
            transaction.commit()?;
            return Ok(false);
        }
        open_journal_write_gate(&transaction)?;
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('drain', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![value],
        )?;
        close_journal_write_gate(&transaction)?;
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
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        open_journal_write_gate(&transaction)?;
        let removed = transaction.execute("DELETE FROM meta WHERE key = 'drain'", [])? > 0;
        close_journal_write_gate(&transaction)?;
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
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
            transaction.commit()?;
            return Ok(false);
        }
        open_journal_write_gate(&transaction)?;
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('admission', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![value],
        )?;
        close_journal_write_gate(&transaction)?;
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
        let Some(expected_version) = expected_version else {
            return Ok(false);
        };
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        open_journal_write_gate(&transaction)?;
        let expected = format!("blocked:{expected_version}");
        let removed = transaction.execute(
            "DELETE FROM meta WHERE key = 'admission' AND value = ?1",
            params![expected],
        )? > 0;
        close_journal_write_gate(&transaction)?;
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
fn setup_journal(conn: &mut Connection) -> StoreResult<()> {
    // Read version, physical shape, event history, and the materialized
    // snapshot from one read-only SQLite snapshot. This check preserves
    // future, legacy, and malformed journals without changing journal mode.
    let preflight = conn.unchecked_transaction()?;
    let (stored, _) = preflight_schema_snapshot(&preflight)?;
    verify_pending_migration_integrity_before_wal(&preflight, stored)?;
    assert_sqlite_version(&preflight)?;
    preflight.commit()?;
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    // One immediate transaction owns the complete setup sequence. The
    // physical DDL and every version stamp become visible together, so a
    // concurrent opener cannot combine an old user_version with a newer
    // table shape. The second preflight is inside that write transaction and
    // preserves the source's existing journal mode. A writer that races the
    // initial snapshot is therefore refused before this setup changes the WAL
    // header or creates sidecars.
    let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (stored, outbox_shape) = preflight_schema_snapshot(&transaction)?;
    verify_pending_migration_integrity_before_wal(&transaction, stored)?;
    let initially_fresh = stored == 0;
    let had_journal_tables = ["events", "slots", "jobs", "outbox", "meta"]
        .iter()
        .try_fold(false, |found, table| {
            Ok::<_, StoreError>(found || table_exists(&transaction, table)?)
        })?;
    repair_historic_jobs_shape(&transaction, stored)?;
    transaction.execute_batch(SCHEMA)?;
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
    migrate_v8_to_v9(&transaction)?;
    migrate_v9_to_v10(&transaction)?;
    migrate_v10_to_v11(&transaction, initially_fresh)?;
    migrate_v11_to_v12(&transaction, stored)?;
    migrate_v12_to_v13(&transaction, stored)?;
    migrate_v13_to_v14(&transaction, stored)?;
    if stored < JOURNAL_SCHEMA_VERSION && (had_journal_tables || initially_fresh) {
        install_replay_baseline(&transaction)?;
    }
    transaction.commit()?;

    // Only switch mode after the locked validation and all migrations commit.
    // A refusal from either preflight leaves a racing legacy database in its
    // original DELETE mode. If WAL is unavailable, the migrated journal is
    // still a valid current-schema DELETE database and the next open can retry.
    let wal: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !wal.eq_ignore_ascii_case("wal") {
        return Err(StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.wal.unavailable",
        )
        .with_remediation("the filesystem must support WAL journaling"));
    }
    conn.execute_batch("PRAGMA synchronous=FULL;")?;
    Ok(())
}

/// Verify source history before `PRAGMA journal_mode=WAL`, which mutates the
/// database header and can create sidecar files. The locked setup transaction
/// repeats this check to cover changes between the read-only preflight and the
/// schema lock.
fn verify_pending_migration_integrity_before_wal(
    conn: &Connection,
    stored: u32,
) -> StoreResult<()> {
    if stored == 0
        && !["events", "slots", "jobs", "outbox", "meta"]
            .iter()
            .try_fold(false, |found, table| {
                Ok::<_, StoreError>(found || table_exists(conn, table)?)
            })?
    {
        // Empty v0 is a genuinely fresh SQLite file. It has no phase rows or
        // event history to inspect; `SCHEMA` creates those tables only after
        // this read-only preflight and the WAL switch.
        return Ok(());
    }

    // v4 derives each completion deadline from the v2/v3 creation timestamp.
    // Check the full source range before enabling WAL; otherwise an overflowing
    // value would fail only after the database header and sidecars changed.
    verify_v3_deadline_backfill_before_wal(conn, stored)?;

    let events_exist = table_exists(conn, "events")?;
    let mut has_pre_v12_proofless_loss = false;
    if events_exist {
        let select = if stored >= 13 {
            "SELECT id, generation, kind, payload, checksum, recorded_unix FROM events ORDER BY id"
        } else {
            "SELECT id, generation, kind, payload, checksum, 0 FROM events ORDER BY id"
        };
        let mut statement = conn.prepare(select)?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        for row in rows {
            let (id, generation, kind, payload, checksum, recorded_unix) = row?;
            if recorded_unix < 0 {
                return Err(invalid_materialized(
                    "event recorded_unix",
                    &recorded_unix.to_string(),
                ));
            }
            let expected_checksum = if stored >= 14 {
                event_row_checksum(id, generation, &kind, &payload, recorded_unix)?
            } else {
                // v13 stored reducer timestamps but did not bind them into the
                // event checksum. Its migration rederives timestamps from the
                // event log and materialized evidence before stamping v14.
                sha256_hex(payload.as_bytes())
            };
            if expected_checksum != checksum {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.checksum.mismatch",
                )
                .with_remediation(format!(
                    "preserve the schema-v{stored} journal unchanged; event integrity verification failed before setup"
                )));
            }
            let event = decode_event_for_schema(&payload, stored)?;
            has_pre_v12_proofless_loss |=
                stored < 12 && matches!(&event, Event::JobAcquisitionLost { proof: None, .. });
            if kind != event_kind(&event) {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.unknown",
                )
                .with_remediation(format!(
                    "preserve the schema-v{stored} journal unchanged; event kind does not match its payload"
                )));
            }
            let recorded_generation = Generation(i64_u64(generation, "event generation")?);
            if recorded_generation != event_generation(&event) {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.generation.mismatch",
                )
                .with_remediation(format!(
                    "preserve the schema-v{stored} journal unchanged; stored event generation does not match its checksummed payload"
                )));
            }
        }
    }
    if stored < 8 {
        verify_v7_phase_split(conn, stored)?;
    }
    if stored < 11 && has_pre_v12_proofless_loss {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.migration.acquisition_loss.ambiguous",
        )
        .with_remediation(format!(
            "preserve the schema-v{stored} journal unchanged; its proofless acquisition-loss history predates durable native permit correlation and cannot be safely replayed before migration"
        )));
    }
    // Decode all present materialized values before WAL or sidecars are
    // created. Historical jobs/outbox columns absent from older supported
    // shapes are projected to their migration defaults by the version-aware
    // loader; malformed meta, negative timestamps/generations, invalid bools,
    // and bad lease JSON fail here without changing the file.
    let current_materialized = load_materialized_state(conn)?;
    if stored == JOURNAL_SCHEMA_VERSION {
        if load_replay_baseline(conn)?.is_none() {
            return Err(replay_baseline_error(
                "current-schema journal has no committed event-tail watermark",
            ));
        }
    } else if table_exists(conn, "meta")?
        && conn
            .query_row(
                "SELECT 1 FROM meta WHERE key = 'replay_baseline'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
    {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.schema.mismatch",
        )
        .with_remediation(format!(
            "preserve the schema-v{stored} journal unchanged; replay baseline is only valid in schema v{JOURNAL_SCHEMA_VERSION}"
        )));
    }
    if stored == 11 || stored == 12 {
        // The upcoming vocabulary migrations validate and rewrite materialized
        // state. Prove that deterministic replay retains existing owner,
        // heartbeat, and outbox identities before WAL.
        let replayed = load_state_from_conn(conn)?;
        ensure_v11_replay_preserves_materialized_evidence(&current_materialized, &replayed)?;
    }
    Ok(())
}

fn verify_v3_deadline_backfill_before_wal(conn: &Connection, stored: u32) -> StoreResult<()> {
    if stored >= 4
        || !table_exists(conn, "outbox")?
        || !table_has_column(conn, "outbox", "created_unix")?
    {
        return Ok(());
    }

    let mut statement = conn.prepare("SELECT job_id, created_unix FROM outbox ORDER BY job_id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (job_id, created_unix) = row?;
        completion_deadline_from_created(created_unix, &job_id)?;
    }
    Ok(())
}

fn completion_deadline_from_created(created_unix: i64, job_id: &str) -> StoreResult<i64> {
    if created_unix < 0 {
        return Err(invalid_materialized(
            "outbox created_unix",
            &created_unix.to_string(),
        ));
    }
    let resolution = i64::try_from(COMPLETION_RESOLUTION_SECONDS).map_err(|_| {
        StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.timestamp.range",
        )
        .with_remediation("the completion resolution window must fit SQLite's INTEGER range")
    })?;
    created_unix.checked_add(resolution).ok_or_else(|| {
        StoreError::new(velnor_model::ExitClass::Conflict, "journal.timestamp.range")
            .with_remediation(format!(
                "preserve the journal unchanged; outbox creation time for {job_id} cannot fit its v4 completion deadline in SQLite INTEGER"
            ))
    })
}

fn ensure_v11_replay_preserves_materialized_evidence(
    current_materialized: &FleetState,
    replayed: &FleetState,
) -> StoreResult<()> {
    // `persist_state` materializes only pending outbox rows. Terminal-acked
    // and abandoned rows remain in event history as proof but are deliberately
    // absent from the table, so compare only the projection a rewrite keeps.
    let current_pending_outbox: Vec<_> = current_materialized
        .outbox
        .iter()
        .filter(|row| row.is_pending())
        .collect();
    let replayed_pending_outbox: Vec<_> = replayed
        .outbox
        .iter()
        .filter(|row| row.is_pending())
        .collect();
    if replayed_pending_outbox.len() != current_pending_outbox.len() {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.migration.outbox.inconsistent",
        )
        .with_remediation(
            "preserve the schema-v11 journal unchanged; replay and materialized pending outbox identities differ",
        ));
    }
    for current in current_pending_outbox {
        let Some(replayed) = replayed_pending_outbox.iter().find(|replayed| {
            replayed.job_id == current.job_id && replayed.generation == current.generation
        }) else {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.outbox.inconsistent",
            )
            .with_remediation(format!(
                "preserve the schema-v11 journal unchanged; replay would drop outbox evidence for job {} generation {}",
                current.job_id.0, current.generation.0
            )));
        };
        // CompletionIntended does not record a creation instant, so replay
        // necessarily generates new timestamps and deadlines. Compare every
        // event-derived field exactly while treating those two materialized
        // timestamps as snapshot-owned data.
        let mut comparable = (*replayed).clone();
        comparable.created_unix = current.created_unix;
        comparable.deadline_unix = current.deadline_unix;
        if comparable != *current {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.outbox.inconsistent",
            )
            .with_remediation(format!(
                "preserve the schema-v11 journal unchanged; replay would rewrite outbox evidence for job {} generation {}",
                current.job_id.0, current.generation.0
            )));
        }
    }

    for current in &current_materialized.jobs {
        if !replayed.jobs.iter().any(|row| row == current) {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.job.inconsistent",
            )
            .with_remediation(format!(
                "preserve the schema-v11 journal unchanged; strict replay would drop or rewrite job evidence for {} generation {}",
                current.job_id.0, current.generation.0
            )));
        }
    }
    for replayed_job in &replayed.jobs {
        if !current_materialized
            .jobs
            .iter()
            .any(|row| row == replayed_job)
            && (!replayed_job.provisional || !replayed_job.acquisition_loss_unproven)
        {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.job.inconsistent",
            )
            .with_remediation(format!(
                "preserve the schema-v11 journal unchanged; strict replay adds non-quarantined job evidence for {} generation {}",
                replayed_job.job_id.0, replayed_job.generation.0
            )));
        }
    }

    // An ambiguous provisional row may conservatively keep a same-generation
    // slot Assigned after v11 had restored it to an earlier admission phase.
    // Every other slot field and every existing slot identity must survive
    // unchanged; in particular, a later generation, PID, or registration
    // proof cannot be silently replaced by replay that rejects a later job.
    if !(current_materialized.capacity_invalid || replayed.capacity_invalid) {
        if replayed.slots.len() != current_materialized.slots.len() {
            return Err(journal_migration_slot_evidence_inconsistent());
        }
        for current in &current_materialized.slots {
            let Some(replayed_slot) = replayed
                .slots
                .iter()
                .find(|slot| slot.slot_id == current.slot_id)
            else {
                return Err(journal_migration_slot_evidence_inconsistent());
            };
            let mut comparable = replayed_slot.clone();
            if comparable.phase != current.phase {
                let replay_is_more_restrictive = comparable.phase == SlotPhase2::Assigned
                    && matches!(
                        current.phase,
                        SlotPhase2::Provisioning | SlotPhase2::Registered | SlotPhase2::Ready
                    );
                if !replay_is_more_restrictive {
                    return Err(journal_migration_slot_evidence_inconsistent());
                }
                comparable.phase = current.phase;
            }
            if comparable != *current {
                return Err(journal_migration_slot_evidence_inconsistent());
            }
        }
    }

    if current_materialized.capacity_invalid || replayed.capacity_invalid {
        // The migration deliberately avoids a full `persist_state` rewrite
        // when either source says capacity is invalid, so its fallback is to
        // preserve the current materialized rows and latch the invalid marker.
        // That fallback is safe only when strict replay needs no additional
        // row changes. Otherwise the migration would either discard replayed
        // permit/owner evidence or overwrite forensic materialized rows.
        if current_materialized.jobs.len() != replayed.jobs.len()
            || current_materialized
                .jobs
                .iter()
                .any(|current| !replayed.jobs.contains(current))
        {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.job.inconsistent",
            )
            .with_remediation(
                "preserve the schema-v11 journal unchanged; capacity-invalid materialization cannot be rewritten without losing current or replayed job evidence",
            ));
        }
    }

    if current_materialized.control_live != replayed.control_live
        || current_materialized.journal_writable != replayed.journal_writable
        || current_materialized.github_reachable != replayed.github_reachable
        || current_materialized.routing_valid != replayed.routing_valid
        || current_materialized.runner_group_valid != replayed.runner_group_valid
        || current_materialized.desired_ready != replayed.desired_ready
        || current_materialized.canary != replayed.canary
        || current_materialized.package_generation != replayed.package_generation
        || current_materialized.package_apt_version != replayed.package_apt_version
        || current_materialized.execution_backend != replayed.execution_backend
        || current_materialized.capacity_declared != replayed.capacity_declared
    {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.migration.materialized.inconsistent",
        )
        .with_remediation(
            "preserve the schema-v11 journal unchanged; strict replay would rewrite materialized control evidence",
        ));
    }
    Ok(())
}

fn preserve_v11_outbox_timestamps(
    current: &FleetState,
    replayed: &mut FleetState,
) -> StoreResult<()> {
    for replayed_row in replayed.outbox.iter_mut().filter(|row| row.is_pending()) {
        let Some(current_row) = current.outbox.iter().find(|row| {
            row.is_pending()
                && row.job_id == replayed_row.job_id
                && row.generation == replayed_row.generation
        }) else {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.outbox.inconsistent",
            )
            .with_remediation(
                "preserve the schema-v11 journal unchanged; replay has no materialized timestamp source for a pending completion",
            ));
        };
        replayed_row.created_unix = current_row.created_unix;
        replayed_row.deadline_unix = current_row.deadline_unix;
    }
    Ok(())
}

fn journal_migration_slot_evidence_inconsistent() -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.migration.slot.inconsistent",
    )
    .with_remediation(
        "preserve the schema-v11 journal unchanged; strict replay would drop or rewrite slot evidence",
    )
}

fn verify_v7_phase_split(conn: &Connection, stored: u32) -> StoreResult<()> {
    if table_exists(conn, "slots")? {
        let mut statement = conn.prepare("SELECT phase FROM slots")?;
        for phase in statement.query_map([], |row| row.get::<_, String>(0))? {
            parse_slot_phase(&phase?)?;
        }
    }

    if table_exists(conn, "jobs")? {
        let mut statement = conn.prepare("SELECT phase FROM jobs")?;
        for phase in statement.query_map([], |row| row.get::<_, String>(0))? {
            parse_job_phase(&phase?)?;
        }
    }
    let _ = stored;
    Ok(())
}

/// Decode a migration event only after inspecting the raw object for fields
/// introduced by later schemas. Serde ignores unknown object members by
/// default, so typed decoding alone cannot protect older journals from a newer
/// writer that stamped a checksummed optional field onto an existing variant.
fn decode_event_for_schema(payload: &str, stored: u32) -> StoreResult<Event> {
    let raw: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
        StoreError::new(velnor_model::ExitClass::Conflict, "journal.event.unknown")
            .with_remediation(format!(
                "preserve the schema-v{stored} journal unchanged; event validation failed before migration: {error}"
            ))
    })?;
    let event_type = raw.get("type").and_then(serde_json::Value::as_str);
    let newer_variant = (stored < 7 && event_type == Some("completion_payload_lost"))
        || (stored < 10 && event_type == Some("job_acquisition_rebuilt"))
        || (stored < 11
            && matches!(
                event_type,
                Some("job_acquisition_intended_with_permit")
                    | Some("job_acquisition_rebuilt_with_permit")
            ));
    let has_v11_field = raw.as_object().is_some_and(|object| match event_type {
        Some("job_acquisition_intended") => object.contains_key("runner_request_id"),
        Some("job_acquisition_resolved") => {
            object.contains_key("runner_request_id") || object.contains_key("permit_lease")
        }
        _ => false,
    });
    let has_v12_field = raw.as_object().is_some_and(|object| {
        event_type == Some("job_acquisition_lost") && object.contains_key("proof")
    });
    if newer_variant || (stored < 11 && has_v11_field) || (stored < 12 && has_v12_field) {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.event.unknown",
        )
        .with_remediation(format!(
            "preserve the schema-v{stored} journal unchanged; event contains a variant or field from a newer journal vocabulary"
        )));
    }
    serde_json::from_value(raw).map_err(|error| {
        StoreError::new(velnor_model::ExitClass::Conflict, "journal.event.unknown")
            .with_remediation(format!(
                "preserve the schema-v{stored} journal unchanged; event validation failed before migration: {error}"
            ))
    })
}

/// Validate SQLite's schema namespace before switching journal mode or
/// running any shape migration. SQLite identifiers are case-insensitive, and
/// `CREATE ... IF NOT EXISTS` can silently accept a conflicting name or skip
/// the canonical index. Unknown views/triggers and foreign keys crossing the
/// journal boundary can also make later DDL or full-table rewrites fail or
/// cascade-delete evidence after WAL has already changed the file.
fn validate_schema_catalog_before_migration(conn: &Connection, stored: u32) -> StoreResult<()> {
    let mut statement = conn.prepare(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_master
         WHERE substr(name, 1, 7) COLLATE NOCASE <> 'sqlite_'
         ORDER BY type, name",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let core_tables = ["events", "slots", "jobs", "outbox", "meta"];
    let mut gate_triggers = Vec::with_capacity(15);
    for table in core_tables {
        for operation in ["insert", "update", "delete"] {
            gate_triggers.push((
                format!("journal_write_gate_{table}_{operation}"),
                table.to_owned(),
            ));
        }
    }

    for (kind, name, table, sql) in &objects {
        if kind.eq_ignore_ascii_case("view") {
            return Err(journal_schema_catalog_mismatch(
                stored,
                &format!("unsupported view {name}"),
            ));
        }
        if kind.eq_ignore_ascii_case("trigger") && stored < 12 {
            return Err(journal_schema_catalog_mismatch(
                stored,
                &format!("unexpected trigger {name}"),
            ));
        }

        if let Some(expected_table) = core_tables
            .iter()
            .find(|expected| name.eq_ignore_ascii_case(expected))
        {
            if !kind.eq_ignore_ascii_case("table") || name != expected_table {
                return Err(journal_schema_catalog_mismatch(
                    stored,
                    &format!("noncanonical object {name}"),
                ));
            }
        }

        if name.eq_ignore_ascii_case("journal_write_gate") {
            if stored < 12 || !kind.eq_ignore_ascii_case("table") || name != "journal_write_gate" {
                return Err(journal_schema_catalog_mismatch(
                    stored,
                    "reserved write-gate name is occupied before its migration",
                ));
            }
        }

        if let Some((expected_name, expected_table)) = gate_triggers
            .iter()
            .find(|(expected_name, _)| name.eq_ignore_ascii_case(expected_name))
        {
            if stored < 12
                || !kind.eq_ignore_ascii_case("trigger")
                || name != expected_name
                || table != expected_table
            {
                return Err(journal_schema_catalog_mismatch(
                    stored,
                    &format!("reserved write-gate trigger name {name} is occupied"),
                ));
            }
        }

        if name.eq_ignore_ascii_case("outbox_v3") {
            return Err(journal_schema_catalog_mismatch(
                stored,
                "reserved v2 outbox rebuild name is occupied",
            ));
        }

        if name.eq_ignore_ascii_case("events_generation_kind_id_idx") {
            let expected_sql = normalize_schema_sql(
                "CREATE INDEX events_generation_kind_id_idx
                 ON events (generation, kind, id DESC)",
            );
            if !kind.eq_ignore_ascii_case("index")
                || name != "events_generation_kind_id_idx"
                || table != "events"
                || !sql
                    .as_deref()
                    .is_some_and(|actual| normalize_schema_sql(actual) == expected_sql)
            {
                return Err(journal_schema_catalog_mismatch(
                    stored,
                    "reserved events index name has a noncanonical definition",
                ));
            }
        }
    }

    // No schema-side trigger may intercept a v2-v11 migration. For v12, the
    // exact fifteen gate triggers are validated by
    // `journal_write_gate_schema_matches`; checking the full catalog there
    // also rejects triggers attached to extension tables.
    if stored >= 12 {
        let trigger_count = objects
            .iter()
            .filter(|(kind, _, _, _)| kind.eq_ignore_ascii_case("trigger"))
            .count();
        if trigger_count != gate_triggers.len() {
            return Err(journal_schema_catalog_mismatch(
                stored,
                "unexpected trigger outside the canonical write gate",
            ));
        }
    }

    // Journal writes replace materialized tables and the v2 outbox migration
    // drops/rebuilds `outbox`. No external table may observe those operations
    // through SQLite foreign-key actions.
    let tables: Vec<_> = objects
        .iter()
        .filter(|(kind, _, _, _)| kind.eq_ignore_ascii_case("table"))
        .map(|(_, name, _, _)| name.clone())
        .collect();
    let is_journal_table = |name: &str| {
        core_tables
            .iter()
            .any(|table| name.eq_ignore_ascii_case(table))
            || name.eq_ignore_ascii_case("journal_write_gate")
            // The v2→v3 migration creates this staging table, inserts the
            // replacement outbox, then drops the old table before renaming
            // the stage. An extension FK to this reserved target could be
            // retargeted or cascade during that sequence even though the
            // table does not exist in the source schema yet.
            || name.eq_ignore_ascii_case("outbox_v3")
    };
    for child in &tables {
        let mut statement = conn.prepare("SELECT \"table\" FROM pragma_foreign_key_list(?1)")?;
        let targets = statement
            .query_map([child], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if (is_journal_table(child) && !targets.is_empty())
            || targets.iter().any(|target| is_journal_table(target))
        {
            return Err(journal_schema_catalog_mismatch(
                stored,
                &format!("foreign-key dependency crosses journal table {child}"),
            ));
        }
    }
    Ok(())
}

fn journal_schema_catalog_mismatch(version: u32, detail: &str) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch").with_remediation(
        format!("preserve the schema-v{version} journal unchanged; {detail}"),
    )
}

/// Validate the journal's recorded version and physical migration shape.
///
/// The caller must serialize this read against schema setup. Keeping all
/// observations in one helper prevents a future caller from accidentally
/// reintroducing a version/shape race between independent reads.
fn preflight_schema_snapshot(conn: &Connection) -> StoreResult<(u32, OutboxSchema)> {
    // A v1 database is still owned by the retired capacity model. Inspect it
    // before enabling WAL, creating missing tables, or starting a migration
    // transaction: contaminated evidence must remain byte stable and must
    // never reach `persist_state`.
    let stored_raw: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if stored_raw < 0 {
        return Err(journal_schema_version_invalid(stored_raw));
    }
    let stored =
        u32::try_from(stored_raw).map_err(|_| journal_schema_version_invalid(stored_raw))?;
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
    if stored > JOURNAL_SCHEMA_VERSION {
        return Err(journal_schema_newer());
    }
    validate_schema_catalog_before_migration(conn, stored)?;
    let has_write_gate = table_exists(conn, "journal_write_gate")?;
    if stored >= 12 {
        if !journal_write_gate_schema_matches(conn)? || journal_write_gate_has_open_row(conn)? {
            return Err(journal_write_gate_schema_mismatch(stored));
        }
    } else if has_write_gate || journal_data_table_triggers_exist(conn)? {
        return Err(journal_write_gate_schema_mismatch(stored));
    }
    // Physical shape ahead of the recorded version means a writer mutated
    // the tables without stamping `PRAGMA user_version`. Refuse rather than
    // guess which vocabulary wrote the events.
    if outbox_shape_rank(outbox_shape) > version_outbox_rank(stored) {
        return Err(outbox_schema_mismatch(stored, outbox_shape));
    }
    if outbox_shape == OutboxSchema::V2
        && table_exists(conn, "jobs")?
        && table_exists(conn, "slots")?
    {
        // Preserve the specific ownership failure before generic column-shape
        // refusal. This also proves the rebuild will not synthesize or drop a
        // v2 outbox owner before its migration transaction begins.
        validate_v2_outbox_owners(conn)?;
    }
    validate_recorded_migration_shape(conn, stored, outbox_shape)?;
    // `validate_recorded_migration_shape` owns every stamped jobs shape,
    // including the two narrowly repairable v5/v6 poison shapes. Keep all
    // version-to-shape rules there so preflight cannot accept a hybrid based
    // on a second, drifting set of column checks.
    // v9 added durable runner-request correlation. A positively stamped
    // schema below v9 cannot already have that column without skipping its
    // migration. Version 0 is the explicit unversioned shape-discovery path;
    // its columns are validated by the later migration shape checks before a
    // current stamp is committed.
    if stored > 0 && stored < 9 && table_has_column(conn, "jobs", "runner_request_id")? {
        return Err(jobs_column_schema_mismatch(stored, "runner_request_id"));
    }
    // v9 made request correlation mandatory in the physical jobs shape. The
    // migration intentionally returns early once the version stamp is 9, so
    // a missing column must be caught here before load/persist can query it.
    if stored >= 9 && !table_has_column(conn, "jobs", "runner_request_id")? {
        return Err(jobs_column_schema_mismatch(
            stored,
            "runner_request_id is missing",
        ));
    }
    if stored < 11 && table_has_column(conn, "jobs", "permit_lease")? {
        return Err(jobs_column_schema_mismatch(
            stored,
            "permit_lease is present before the v11 migration",
        ));
    }
    if stored < 12 && table_has_column(conn, "jobs", "acquisition_loss_unproven")? {
        return Err(jobs_column_schema_mismatch(
            stored,
            "acquisition_loss_unproven is present before the v12 migration",
        ));
    }
    if stored == 11 && !jobs_v11_shape_matches(conn)? {
        return Err(jobs_schema_mismatch(stored));
    }
    if stored >= 12 && !jobs_v12_shape_matches(conn)? {
        return Err(jobs_schema_mismatch(stored));
    }
    Ok((stored, outbox_shape))
}

fn validate_recorded_migration_shape(
    conn: &Connection,
    stored: u32,
    outbox_shape: OutboxSchema,
) -> StoreResult<()> {
    let mut has_any_journal_table = false;
    for table in ["events", "slots", "jobs", "outbox", "meta"] {
        has_any_journal_table |= table_exists(conn, table)?;
    }
    if stored == 0 && !has_any_journal_table {
        // Explicit fresh-file exception: a new empty SQLite database has no
        // historical shape to inspect and receives the current schema inside
        // the locked setup transaction.
        return Ok(());
    }

    if stored == 0 {
        // The only supported populated-unversioned shape is the historical
        // v2 physical layout, which setup can identify without guessing. A
        // current or partial schema with its version erased is ambiguous.
        if outbox_shape != OutboxSchema::V2 || !jobs_shape_matches_version(conn, 2)? {
            return Err(jobs_schema_mismatch(stored));
        }
    } else {
        let expected_outbox = match stored {
            2 => OutboxSchema::V2,
            3 => OutboxSchema::V3,
            _ => OutboxSchema::V4,
        };
        if outbox_shape != expected_outbox {
            return Err(outbox_schema_mismatch(stored, outbox_shape));
        }

        let jobs_match = match stored {
            2 | 3 => jobs_shape_matches_version(conn, 3)?,
            4 => jobs_shape_matches_version(conn, 4)?,
            5 => jobs_shape_matches_version(conn, 5)? || jobs_shape_matches_version(conn, 4)?,
            6 => jobs_shape_matches_version(conn, 6)? || jobs_v6_repair_shape_matches(conn)?,
            7 | 8 => jobs_shape_matches_version(conn, 8)?,
            9 | 10 => jobs_shape_matches_version(conn, 10)?,
            11 => jobs_v11_shape_matches(conn)?,
            12 | 13 | 14 => jobs_v12_shape_matches(conn)?,
            _ => false,
        };
        if !jobs_match {
            return Err(jobs_schema_mismatch(stored));
        }
    }

    if !event_schema_matches(conn, stored)?
        || !slots_schema_matches(conn)?
        || !meta_schema_matches(conn)?
    {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.schema.mismatch",
        )
        .with_remediation(format!(
            "preserve the schema-v{stored} journal unchanged; an event, slot, or meta table does not match its supported physical shape"
        )));
    }

    if outbox_shape == OutboxSchema::V2 {
        validate_v2_outbox_owners(conn)?;
    }
    Ok(())
}

fn load_current_state_checked(conn: &Connection) -> StoreResult<FleetState> {
    let replayed = load_state_from_conn_with_policy(conn, true)?;
    let stored_raw: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let stored =
        u32::try_from(stored_raw).map_err(|_| journal_schema_version_invalid(stored_raw))?;
    if stored != JOURNAL_SCHEMA_VERSION {
        return Ok(replayed);
    }

    let materialized = load_materialized_state(conn)?;
    let mut projected = replayed;
    // Drain and admission are deliberately meta-only controls. They are not
    // events and must survive replay from the event log.
    projected.drain_active = materialized.drain_active;
    projected.drain_version = materialized.drain_version;
    projected.admission_blocked = materialized.admission_blocked;
    projected.admission_version = materialized.admission_version;

    let mismatches = current_projection_mismatch_fields(&projected, &materialized);
    if !mismatches.is_empty() {
        if explicit_capacity_invalid_marker(conn)? {
            // Migration can preserve a forensic snapshot that the stricter
            // reducer cannot reconstruct. Keep that snapshot quarantined and
            // block writes; never replace it with a replay projection.
            return Ok(materialized);
        }
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.materialized.replay.mismatch",
        )
        .with_remediation(format!(
            "preserve the journal unchanged; materialized projection differs from accepted event history in {}",
            mismatches.join(", ")
        )));
    }
    Ok(projected)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayBaseline {
    /// Number of historical rows included in the projection snapshot.
    event_count: i64,
    /// Historical log boundary included in the projection snapshot.
    through_event_id: i64,
    /// Number of event rows observed by the last committed current writer.
    latest_event_count: i64,
    /// Event ID observed by the last committed current writer.
    latest_event_id: i64,
    /// Digest of every baseline field except this digest itself.
    integrity_checksum: String,
    state: FleetState,
}

fn replay_baseline_error(detail: impl Into<String>) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.replay.baseline.invalid",
    )
    .with_remediation(format!(
        "preserve the journal unchanged; committed replay baseline or event-tail watermark is invalid: {}",
        detail.into()
    ))
}

fn replay_baseline_integrity_checksum(baseline: &ReplayBaseline) -> StoreResult<String> {
    let mut unsigned = baseline.clone();
    unsigned.integrity_checksum.clear();
    let encoded = serde_json::to_vec(&unsigned).map_err(|error| {
        StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.replay.baseline.encode",
        )
        .with_remediation(error.to_string())
    })?;
    Ok(sha256_hex(&encoded))
}

fn decode_replay_baseline(conn: &Connection) -> StoreResult<Option<ReplayBaseline>> {
    if !table_exists(conn, "meta")? {
        return Ok(None);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'replay_baseline'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Ok(None);
    };
    let baseline: ReplayBaseline =
        serde_json::from_str(&value).map_err(|error| replay_baseline_error(error.to_string()))?;
    if replay_baseline_integrity_checksum(&baseline)? != baseline.integrity_checksum {
        return Err(replay_baseline_error(
            "baseline fields do not match their committed integrity checksum",
        ));
    }
    if baseline.event_count < 0
        || baseline.through_event_id < 0
        || (baseline.event_count == 0) != (baseline.through_event_id == 0)
        || baseline.latest_event_count < baseline.event_count
        || baseline.latest_event_id < baseline.through_event_id
        || (baseline.latest_event_count == 0) != (baseline.latest_event_id == 0)
        || baseline.latest_event_count > baseline.latest_event_id
        || !baseline.state.journal_writable
        || baseline.state.drain_active
        || baseline.state.drain_version != 0
        || baseline.state.admission_blocked
        || baseline.state.admission_version != 0
    {
        return Err(replay_baseline_error(
            "invalid boundary or projection fields",
        ));
    }
    validate_job_identity_uniqueness(&baseline.state.jobs)
        .map_err(|error| replay_baseline_error(error.to_string()))?;
    let (actual_count, actual_high_water): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(id), 0) FROM events WHERE id <= ?1",
        [baseline.through_event_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if actual_count != baseline.event_count || actual_high_water != baseline.through_event_id {
        return Err(replay_baseline_error(format!(
            "committed event boundary expected {} rows through {}, found {actual_count} through {actual_high_water}",
            baseline.event_count, baseline.through_event_id
        )));
    }
    Ok(Some(baseline))
}

fn load_replay_baseline(conn: &Connection) -> StoreResult<Option<ReplayBaseline>> {
    let Some(baseline) = decode_replay_baseline(conn)? else {
        return Ok(None);
    };
    let (actual_count, actual_high_water): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(id), 0) FROM events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if actual_count != baseline.latest_event_count || actual_high_water != baseline.latest_event_id
    {
        return Err(replay_baseline_error(format!(
            "committed log tail expected {} rows through {}, found {actual_count} through {actual_high_water}",
            baseline.latest_event_count, baseline.latest_event_id
        )));
    }
    Ok(Some(baseline))
}

fn refresh_replay_baseline_tail(
    conn: &Connection,
    baseline: &mut ReplayBaseline,
) -> StoreResult<()> {
    let (prior_count, prior_high_water): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(id), 0) FROM events WHERE id <= ?1",
        [baseline.latest_event_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if prior_count != baseline.latest_event_count || prior_high_water != baseline.latest_event_id {
        return Err(replay_baseline_error(format!(
            "previous writer boundary expected {} rows through {}, found {prior_count} through {prior_high_water}",
            baseline.latest_event_count, baseline.latest_event_id
        )));
    }
    let (latest_count, latest_id): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MAX(id), 0) FROM events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let inserted_count = latest_count - prior_count;
    let inserted_id_span = latest_id - prior_high_water;
    if inserted_count < 0 || inserted_id_span != inserted_count {
        return Err(replay_baseline_error(format!(
            "new event rows are not a contiguous append after {}, found {inserted_count} rows through {latest_id}",
            baseline.latest_event_id
        )));
    }
    baseline.latest_event_count = latest_count;
    baseline.latest_event_id = latest_id;
    baseline.integrity_checksum = replay_baseline_integrity_checksum(baseline)?;
    Ok(())
}

fn explicit_capacity_invalid_marker(conn: &Connection) -> StoreResult<bool> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'capacity_invalid'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let mut meta = HashMap::new();
    if let Some(value) = value {
        meta.insert("capacity_invalid".to_owned(), value);
    }
    meta_bool(&meta, "capacity_invalid")
}

fn install_replay_baseline(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let (event_count, through_event_id): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COALESCE(MAX(id), 0) FROM events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut state = load_materialized_state(tx)?;
    let event_projection = load_state_from_conn(tx)?;
    let materialized_pending = sorted_pending_outbox(&state);
    let event_pending = sorted_pending_outbox(&event_projection);
    if event_pending
        .iter()
        .any(|row| !materialized_pending.contains(row))
    {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.materialized.replay.mismatch",
        )
        .with_remediation(
            "preserve the journal unchanged; checksummed history has pending outbox evidence missing from the materialized snapshot",
        ));
    }
    // Terminal and abandoned outbox rows are intentionally absent from the
    // materialized table after their durable event. Keep those event-derived
    // records in the snapshot so current-schema replay preserves the API's
    // terminal evidence while the pending rows still come from the validated
    // materialized projection.
    let mut archived_outbox: Vec<_> = event_projection
        .outbox
        .into_iter()
        .filter(|row| !row.is_pending())
        .collect();
    archived_outbox.extend(state.outbox);
    archived_outbox.sort_by(|left, right| {
        left.job_id
            .0
            .cmp(&right.job_id.0)
            .then_with(|| left.generation.0.cmp(&right.generation.0))
    });
    state.outbox = archived_outbox;
    // These latches are direct meta writes, so they are copied from the live
    // snapshot on each comparison rather than frozen into the event baseline.
    state.drain_active = false;
    state.drain_version = 0;
    state.admission_blocked = false;
    state.admission_version = 0;
    state.journal_writable = true;
    let mut baseline = ReplayBaseline {
        event_count,
        through_event_id,
        latest_event_count: event_count,
        latest_event_id: through_event_id,
        integrity_checksum: String::new(),
        state,
    };
    baseline.integrity_checksum = replay_baseline_integrity_checksum(&baseline)?;
    let value = serde_json::to_string(&baseline).map_err(|error| {
        StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.replay.baseline.encode",
        )
        .with_remediation(error.to_string())
    })?;
    open_journal_write_gate(tx)?;
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('replay_baseline', ?1)",
        [value],
    )?;
    close_journal_write_gate(tx)?;
    Ok(())
}

fn current_projection_mismatch_fields(
    replayed: &FleetState,
    materialized: &FleetState,
) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if replayed.control_live != materialized.control_live {
        fields.push("control_live");
    }
    if replayed.journal_writable != materialized.journal_writable {
        fields.push("journal_writable");
    }
    if replayed.github_reachable != materialized.github_reachable {
        fields.push("github_reachable");
    }
    if replayed.routing_valid != materialized.routing_valid {
        fields.push("routing_valid");
    }
    if replayed.runner_group_valid != materialized.runner_group_valid {
        fields.push("runner_group_valid");
    }
    if replayed.drain_active != materialized.drain_active
        || replayed.drain_version != materialized.drain_version
    {
        fields.push("drain");
    }
    if replayed.admission_blocked != materialized.admission_blocked
        || replayed.admission_version != materialized.admission_version
    {
        fields.push("admission");
    }
    if replayed.desired_ready != materialized.desired_ready
        || replayed.capacity_declared != materialized.capacity_declared
    {
        fields.push("capacity");
    }
    if replayed.canary != materialized.canary {
        fields.push("canary");
    }
    if replayed.package_generation != materialized.package_generation
        || replayed.package_apt_version != materialized.package_apt_version
    {
        fields.push("package");
    }
    if replayed.execution_backend != materialized.execution_backend {
        fields.push("execution_backend");
    }
    if replayed.capacity_invalid != materialized.capacity_invalid {
        fields.push("capacity_invalid");
    }
    if replayed.slots != materialized.slots {
        fields.push("slots");
    }
    if replayed.jobs != materialized.jobs {
        fields.push("jobs");
    }
    if sorted_pending_outbox(replayed) != sorted_pending_outbox(materialized) {
        fields.push("outbox");
    }
    fields
}

fn sorted_pending_outbox(state: &FleetState) -> Vec<OutboxRecord> {
    let mut pending: Vec<_> = state
        .outbox
        .iter()
        .filter(|row| row.is_pending())
        .cloned()
        .collect();
    pending.sort_by(|left, right| {
        left.job_id
            .0
            .cmp(&right.job_id.0)
            .then_with(|| left.generation.0.cmp(&right.generation.0))
    });
    pending
}

fn load_state_from_conn(conn: &Connection) -> StoreResult<FleetState> {
    load_state_from_conn_with_policy(conn, false)
}

fn load_state_from_conn_with_policy(
    conn: &Connection,
    reject_current_replay_events: bool,
) -> StoreResult<FleetState> {
    // Journal open succeeded, so the file is writable unless a later apply
    // fails; recovery treats an opened journal as writable.
    let materialized = load_materialized_state(conn)?;
    let stored_raw: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let stored =
        u32::try_from(stored_raw).map_err(|_| journal_schema_version_invalid(stored_raw))?;
    let replay_baseline = if stored == JOURNAL_SCHEMA_VERSION {
        let baseline = load_replay_baseline(conn)?;
        if reject_current_replay_events && baseline.is_none() {
            return Err(replay_baseline_error(
                "current-schema journal has no committed event-tail watermark",
            ));
        }
        baseline
    } else {
        None
    };
    let has_recorded_unix = table_has_column(conn, "events", "recorded_unix")?;
    let mut state = if has_recorded_unix {
        let mut state = replay_baseline
            .as_ref()
            .map(|baseline| baseline.state.clone())
            .unwrap_or(FleetState {
                journal_writable: true,
                ..FleetState::default()
            });
        let mut expected_current_id = replay_baseline
            .as_ref()
            .map_or(1, |baseline| baseline.through_event_id.saturating_add(1));
        let mut stmt = conn.prepare(
            "SELECT id, generation, kind, payload, checksum, recorded_unix
             FROM events ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        for row in rows {
            let (id, generation, kind, payload, checksum, recorded_unix) = row?;
            if reject_current_replay_events && stored == JOURNAL_SCHEMA_VERSION && id <= 0 {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.id.invalid",
                )
                .with_remediation(
                    "preserve the journal unchanged; current event row IDs must be positive",
                ));
            }
            if recorded_unix < 0 {
                return Err(invalid_materialized(
                    "event recorded_unix",
                    &recorded_unix.to_string(),
                ));
            }
            let expected_checksum = if stored >= 14 {
                event_row_checksum(id, generation, &kind, &payload, recorded_unix)?
            } else {
                sha256_hex(payload.as_bytes())
            };
            if expected_checksum != checksum {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.checksum.mismatch",
                )
                .with_remediation("the event log failed integrity verification"));
            }
            let recorded_unix = i64_u64(recorded_unix, "event recorded_unix")?;
            let event = decode_current_event(&payload)?;
            if kind != event_kind(&event)
                || i64_u64(generation, "event generation")? != event_generation(&event).0
            {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.generation.mismatch",
                ));
            }
            if replay_baseline
                .as_ref()
                .is_some_and(|baseline| id <= baseline.through_event_id)
            {
                continue;
            }
            if reject_current_replay_events && stored == JOURNAL_SCHEMA_VERSION {
                if id != expected_current_id {
                    return Err(StoreError::new(
                        velnor_model::ExitClass::Conflict,
                        "journal.event.sequence.gap",
                    )
                    .with_remediation(format!(
                        "preserve the journal unchanged; expected current event row {expected_current_id}, found {id}"
                    )));
                }
                expected_current_id = expected_current_id.saturating_add(1);
            }
            let outcome = reduce_at(state, event, recorded_unix);
            if reject_current_replay_events && stored == JOURNAL_SCHEMA_VERSION && outcome.rejected
            {
                return Err(StoreError::new(
                    velnor_model::ExitClass::Conflict,
                    "journal.event.replay.rejected",
                )
                .with_remediation(format!(
                    "preserve the journal unchanged; current-schema event at row {id} was rejected by the reducer"
                )));
            }
            state = outcome.state;
        }
        state
    } else {
        // Only setup/migration can reach this path. Legacy event rows did not
        // persist reducer time; reconstruct the minimum deterministic times
        // from their materialized snapshot and committed timeout event. The
        // v13 migration writes these values durably before the journal opens
        // for normal use.
        replay_legacy_events(conn, &materialized)?.1
    };
    state.capacity_invalid |= materialized.capacity_invalid;
    state.capacity_invalid |= state_capacity_invalid(&state) || legacy_slots_schema(conn)?;
    Ok(state)
}

fn decode_current_event(payload: &str) -> StoreResult<Event> {
    serde_json::from_str(payload).map_err(|error| {
        StoreError::new(velnor_model::ExitClass::Conflict, "journal.event.unknown")
            .with_remediation(format!(
                "event could not be decoded by schema version {JOURNAL_SCHEMA_VERSION}; preserve the journal and reopen it with the binary that wrote it: {error}"
            ))
    })
}

/// Deterministically replay a pre-v13 history and produce the timestamp each
/// event must receive during v13 migration. The old schema omitted reducer
/// time. Pending completion and current heartbeat timestamps survive in the
/// materialized snapshot; a committed `CompletionUnresolvable` event itself
/// is the historical timeout proof, so its replay time is the recorded row's
/// deadline rather than today's wall clock.
fn replay_legacy_events(
    conn: &Connection,
    materialized: &FleetState,
) -> StoreResult<(HashMap<i64, u64>, FleetState)> {
    let mut statement = conn.prepare("SELECT id, payload, checksum FROM events ORDER BY id ASC")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut decoded = Vec::new();
    for row in rows {
        let (id, payload, checksum) = row?;
        if sha256_hex(payload.as_bytes()) != checksum {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.checksum.mismatch",
            )
            .with_remediation(
                "the legacy event log failed integrity verification before timestamp migration",
            ));
        }
        decoded.push((id, decode_current_event(&payload)?));
    }
    drop(statement);

    let mut latest_heartbeat = HashMap::<(String, u64), i64>::new();
    let mut accepted_by_job = HashMap::<(String, u64), u64>::new();
    for (id, event) in &decoded {
        match event {
            Event::SlotHeartbeat {
                slot_id,
                generation,
                ..
            } => {
                latest_heartbeat.insert((slot_id.0.clone(), generation.0), *id);
            }
            Event::JobOwned {
                job_id,
                generation,
                accepted_unix,
                ..
            } => {
                accepted_by_job.insert((job_id.0.clone(), generation.0), *accepted_unix);
            }
            _ => {}
        }
    }

    let mut recorded_by_id = HashMap::with_capacity(decoded.len());
    let mut state = FleetState {
        journal_writable: true,
        ..FleetState::default()
    };
    for (id, event) in decoded {
        let recorded_unix = match &event {
            Event::SlotHeartbeat {
                slot_id,
                generation,
                ..
            } if latest_heartbeat.get(&(slot_id.0.clone(), generation.0)) == Some(&id) => {
                materialized
                    .slots
                    .iter()
                    .find(|slot| slot.slot_id == *slot_id && slot.generation == *generation)
                    .map(|slot| slot.heartbeat_unix)
                    .unwrap_or(0)
            }
            Event::CompletionIntended {
                job_id, generation, ..
            } => materialized
                .outbox
                .iter()
                .find(|row| row.job_id == *job_id && row.generation == *generation)
                .map(|row| row.created_unix)
                // A terminal history no longer has its outbox row, and the
                // pre-v13 event did not record when the intent was written.
                // Use the durable accepted timestamp as a stable lower bound;
                // never substitute migration/replay wall clock. The paired
                // timeout event below then receives the derived deadline.
                .or_else(|| {
                    accepted_by_job
                        .get(&(job_id.0.clone(), generation.0))
                        .copied()
                })
                .unwrap_or(0),
            Event::CompletionUnresolvable {
                job_id, generation, ..
            } => state
                .outbox
                .iter()
                .find(|row| row.job_id == *job_id && row.generation == *generation)
                .map(|row| row.deadline_unix)
                .unwrap_or(0),
            _ => 0,
        };
        recorded_by_id.insert(id, recorded_unix);
        state = reduce_at(state, event, recorded_unix).state;
    }
    state.capacity_invalid |= materialized.capacity_invalid;
    state.capacity_invalid |= state_capacity_invalid(&state) || legacy_slots_schema(conn)?;
    Ok((recorded_by_id, state))
}

fn materialized_column_projection(
    conn: &Connection,
    table: &str,
    column: &str,
    historical_default: &str,
) -> StoreResult<String> {
    if table_has_column(conn, table, column)? {
        Ok(column.to_owned())
    } else {
        Ok(format!("{historical_default} AS {column}"))
    }
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

    // Materialized validation also runs before WAL setup on historical
    // schemas. Project fields absent from those supported physical shapes to
    // their migration defaults so every stored numeric, bool, phase, and JSON
    // value is validated before the file can be mutated.
    let terminal_conclusion =
        materialized_column_projection(conn, "jobs", "terminal_conclusion", "NULL")?;
    let provisional = materialized_column_projection(conn, "jobs", "provisional", "0")?;
    let plan_id = materialized_column_projection(conn, "jobs", "plan_id", "''")?;
    let run_service_url = materialized_column_projection(conn, "jobs", "run_service_url", "''")?;
    let probe_attempts = materialized_column_projection(conn, "jobs", "probe_attempts", "0")?;
    let probe_deadline_unix =
        materialized_column_projection(conn, "jobs", "probe_deadline_unix", "0")?;
    let runner_request_id =
        materialized_column_projection(conn, "jobs", "runner_request_id", "''")?;
    let permit_lease = materialized_column_projection(conn, "jobs", "permit_lease", "NULL")?;
    let loss_flag_projection =
        materialized_column_projection(conn, "jobs", "acquisition_loss_unproven", "0")?;
    let mut statement = conn.prepare(&format!(
        "SELECT job_id, slot_id, generation, attempt, worker, phase, accepted_unix,
                {terminal_conclusion}, {provisional}, {plan_id}, {run_service_url},
                {probe_attempts}, {probe_deadline_unix}, {runner_request_id}, {permit_lease},
                {loss_flag_projection}
         FROM jobs ORDER BY rowid"
    ))?;
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
            row.get::<_, i64>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, i64>(11)?,
            row.get::<_, i64>(12)?,
            row.get::<_, String>(13)?,
            row.get::<_, Option<String>>(14)?,
            row.get::<_, i64>(15)?,
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
            runner_request_id,
            permit_lease,
            acquisition_loss_unproven,
        ) = row?;
        let permit_lease = permit_lease
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| invalid_materialized("job permit lease", &error.to_string()))
            })
            .transpose()?;
        state.jobs.push(JobRecord {
            job_id: JobId(job_id),
            slot_id: SlotId(slot_id),
            generation: Generation(i64_u64(generation, "job generation")?),
            attempt: i64_u32(attempt, "job attempt")?,
            worker,
            phase: parse_job_phase(&phase)?,
            accepted_unix: i64_u64(accepted_unix, "job accepted_unix")?,
            terminal_conclusion,
            provisional: sqlite_bool(provisional, "job provisional")?,
            plan_id,
            run_service_url,
            runner_request_id,
            permit_lease,
            acquisition_loss_unproven: sqlite_bool(
                acquisition_loss_unproven,
                "job acquisition_loss_unproven",
            )?,
            probe_attempts: i64_u32(probe_attempts, "job probe_attempts")?,
            probe_deadline_unix: i64_u64(probe_deadline_unix, "job probe_deadline_unix")?,
        });
    }
    validate_job_identity_uniqueness(&state.jobs)?;

    let outbox_slot_id = if table_has_column(conn, "outbox", "slot_id")? {
        "slot_id".to_owned()
    } else {
        "(SELECT jobs.slot_id FROM jobs
          WHERE jobs.job_id = outbox.job_id AND jobs.generation = outbox.generation) AS slot_id"
            .to_owned()
    };
    let outbox_attempts = materialized_column_projection(conn, "outbox", "attempts", "0")?;
    let outbox_deadline_unix =
        materialized_column_projection(conn, "outbox", "deadline_unix", "0")?;
    let outbox_permanent = materialized_column_projection(conn, "outbox", "permanent", "0")?;
    let outbox_abandoned = materialized_column_projection(conn, "outbox", "abandoned", "0")?;
    let mut statement = conn.prepare(&format!(
        "SELECT job_id, {outbox_slot_id}, generation, payload_sha256, intended, send_started,
                remote_acked, created_unix, {outbox_attempts}, {outbox_deadline_unix},
                {outbox_permanent}, {outbox_abandoned}
         FROM outbox ORDER BY rowid"
    ))?;
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

fn generation_to_sql(generation: Generation) -> StoreResult<i64> {
    i64::try_from(generation.0).map_err(|_| {
        StoreError::new(velnor_model::ExitClass::Usage, "journal.generation.range")
            .with_remediation("journal generations must fit SQLite's signed 64-bit INTEGER range")
    })
}

fn timestamp_to_sql(timestamp: u64, field: &str) -> StoreResult<i64> {
    i64::try_from(timestamp).map_err(|_| {
        StoreError::new(velnor_model::ExitClass::Usage, "journal.timestamp.range").with_remediation(
            format!("journal timestamp {field} must fit SQLite's signed 64-bit INTEGER range"),
        )
    })
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

fn open_journal_write_gate(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    // A long-lived Journal handle may outlive another connection that changed
    // the schema. Validate the complete FK/trigger boundary again under this
    // IMMEDIATE write lock, before event, materialized, or meta writes begin.
    validate_schema_catalog_before_migration(tx, JOURNAL_SCHEMA_VERSION)?;
    if !journal_write_gate_schema_matches(tx)? || journal_write_gate_has_open_row(tx)? {
        return Err(journal_write_gate_schema_mismatch(JOURNAL_SCHEMA_VERSION));
    }
    let inserted = tx.execute("INSERT INTO journal_write_gate (id) VALUES (1)", [])?;
    if inserted != 1 || !journal_write_gate_has_open_row(tx)? {
        return Err(journal_write_gate_schema_mismatch(JOURNAL_SCHEMA_VERSION));
    }
    Ok(())
}

fn close_journal_write_gate(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let deleted = tx.execute("DELETE FROM journal_write_gate WHERE id = 1", [])?;
    if deleted != 1 || journal_write_gate_has_open_row(tx)? {
        return Err(journal_write_gate_schema_mismatch(JOURNAL_SCHEMA_VERSION));
    }
    Ok(())
}

fn persist_state(tx: &rusqlite::Transaction<'_>, state: &FleetState) -> StoreResult<()> {
    // Reject invalid identifiers and out-of-range integers before deleting
    // any current materialized evidence. The surrounding transaction would
    // roll back too, but prevalidation keeps the write path structurally safe
    // and prevents casts or partial row rewrites from ever beginning.
    validate_job_identity_uniqueness(&state.jobs)?;
    let stored_raw: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let stored =
        u32::try_from(stored_raw).map_err(|_| journal_schema_version_invalid(stored_raw))?;
    let replay_baseline = decode_replay_baseline(tx)?;
    if stored == JOURNAL_SCHEMA_VERSION && replay_baseline.is_none() {
        return Err(replay_baseline_error(
            "current-schema write has no committed event-tail watermark",
        ));
    }
    let slot_sql = state
        .slots
        .iter()
        .map(|slot| {
            Ok((
                generation_to_sql(slot.generation)?,
                timestamp_to_sql(slot.heartbeat_unix, "slot heartbeat_unix")?,
            ))
        })
        .collect::<StoreResult<Vec<_>>>()?;
    let job_sql = state
        .jobs
        .iter()
        .map(|job| {
            Ok((
                generation_to_sql(job.generation)?,
                timestamp_to_sql(job.accepted_unix, "job accepted_unix")?,
                timestamp_to_sql(job.probe_deadline_unix, "job probe_deadline_unix")?,
            ))
        })
        .collect::<StoreResult<Vec<_>>>()?;
    let outbox_sql = state
        .outbox
        .iter()
        .filter(|row| row.is_pending())
        .map(|row| {
            Ok((
                generation_to_sql(row.generation)?,
                timestamp_to_sql(row.created_unix, "outbox created_unix")?,
                timestamp_to_sql(row.deadline_unix, "outbox deadline_unix")?,
            ))
        })
        .collect::<StoreResult<Vec<_>>>()?;
    tx.execute("DELETE FROM slots", [])?;
    tx.execute("DELETE FROM jobs", [])?;
    tx.execute("DELETE FROM outbox", [])?;
    tx.execute("DELETE FROM meta", [])?;
    for (slot, (generation, heartbeat_unix)) in state.slots.iter().zip(slot_sql) {
        tx.execute(
            "INSERT INTO slots (
                slot_id, generation, phase, permit_held, routing_valid, session_live,
                executor_proven, registered, pid, heartbeat_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                slot.slot_id.0,
                generation,
                slot.phase.as_str(),
                slot.permit_held as i64,
                slot.routing_valid as i64,
                slot.session_live as i64,
                slot.executor_proven as i64,
                slot.registered as i64,
                slot.pid.map(i64::from),
                heartbeat_unix,
            ],
        )?;
    }
    for (job, (generation, accepted_unix, probe_deadline_unix)) in state.jobs.iter().zip(job_sql) {
        tx.execute(
            "INSERT INTO jobs (
                job_id, slot_id, generation, attempt, worker, phase, accepted_unix,
                terminal_conclusion, provisional, plan_id, run_service_url,
                probe_attempts, probe_deadline_unix, runner_request_id, permit_lease,
                acquisition_loss_unproven
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                job.job_id.0,
                job.slot_id.0,
                generation,
                job.attempt as i64,
                job.worker,
                job.phase.as_str(),
                accepted_unix,
                job.terminal_conclusion.as_deref(),
                job.provisional as i64,
                job.plan_id,
                job.run_service_url,
                job.probe_attempts as i64,
                probe_deadline_unix,
                job.runner_request_id,
                job.permit_lease
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|error| {
                        StoreError::new(
                            velnor_model::ExitClass::Operation,
                            "journal.materialized.serialize",
                        )
                        .with_remediation(format!("could not serialize permit lease: {error}"))
                    })?,
                job.acquisition_loss_unproven as i64,
            ],
        )?;
    }
    for (row, (generation, created_unix, deadline_unix)) in state
        .outbox
        .iter()
        .filter(|row| row.is_pending())
        .zip(outbox_sql)
    {
        tx.execute(
            "INSERT INTO outbox (
                job_id, slot_id, generation, payload_sha256, intended, send_started,
                remote_acked, created_unix, attempts, deadline_unix, permanent, abandoned
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                row.job_id.0,
                row.slot_id.0,
                generation,
                row.payload_sha256,
                row.intended as i64,
                row.send_started as i64,
                row.remote_acked as i64,
                created_unix,
                row.attempts as i64,
                deadline_unix,
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
    // re-emitting both markers from state keeps them sticky across unrelated
    // event writes with no new event and no schema bump. Absent when inactive,
    // so old journals keep their exact meta shape.
    //
    // Mixed-version warning: this rewrite drops every `meta` key it does
    // not know, and an older binary's `persist_state` does not know the
    // drain/admission keys — any write by an older binary clears a latched
    // marker. Forward tolerance is read-only: old binaries open fenced
    // journals fine but must not share one journal with a newer lifecycle
    // writer across an upgrade.
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
    if let Some(mut baseline) = replay_baseline {
        refresh_replay_baseline_tail(tx, &mut baseline)?;
        let value = serde_json::to_string(&baseline).map_err(|error| {
            StoreError::new(
                velnor_model::ExitClass::Operation,
                "journal.replay.baseline.encode",
            )
            .with_remediation(error.to_string())
        })?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('replay_baseline', ?1)",
            [value],
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
    if table_definition_has_explicit_collation(conn, "outbox")?
        || !table_indexes_use_binary_collation(conn, "outbox")?
    {
        return Err(outbox_schema_invalid("collation"));
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

fn outbox_schema_mismatch(version: u32, shape: OutboxSchema) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
        .with_remediation(format!(
            "preserve the journal unchanged: PRAGMA user_version={version} is incompatible with physical outbox shape {shape:?}"
        ))
}

fn jobs_column_schema_mismatch(version: u32, column: &str) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
        .with_remediation(format!(
            "preserve the journal unchanged: PRAGMA user_version={version} is incompatible with jobs column {column}"
        ))
}

/// Upgrade a v2 materialized outbox without inventing ownership. Every row is
/// backfilled from exactly one matching job and exactly one matching slot into
/// a rebuilt table whose `slot_id` is NOT NULL. Owner mismatches fail before
/// any schema mutation; the transaction is retryable if the process dies
/// mid-upgrade.
fn validate_v2_outbox_owners(conn: &Connection) -> StoreResult<()> {
    let inconsistent_owner: Option<(String, i64)> = conn
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
    Ok(())
}

fn migrate_v2_to_v3(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    validate_v2_outbox_owners(tx)?;
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
    let mut statement = tx.prepare(
        "SELECT job_id, created_unix FROM outbox WHERE deadline_unix = 0 ORDER BY job_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let deadlines = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    for (job_id, created_unix) in deadlines {
        let deadline_unix = completion_deadline_from_created(created_unix, &job_id)?;
        tx.execute(
            "UPDATE outbox SET deadline_unix = ?1 WHERE job_id = ?2 AND deadline_unix = 0",
            params![deadline_unix, job_id],
        )?;
    }
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

/// v9 preserves the original runner request ID after acquisition retargets a
/// provisional row to the run-service job ID. This is the durable permit
/// correlation key used by restart recovery and redelivery handling.
fn migrate_v8_to_v9(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 9 {
        return Ok(());
    }

    // Verify integrity before changing either the jobs shape or the version
    // stamp. Decoding JSON alone is not enough: valid but modified payloads
    // must not become the basis for durable request/permit correlation.
    let mut statement = tx.prepare("SELECT payload, checksum FROM events ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut verified_payloads = Vec::new();
    for row in rows {
        let (payload, checksum) = row?;
        if sha256_hex(payload.as_bytes()) != checksum {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.checksum.mismatch",
            )
            .with_remediation(
                "preserve the schema-v8 journal unchanged; event integrity verification failed before the v9 migration",
            ));
        }
        verified_payloads.push(payload);
    }
    drop(statement);

    if !table_has_column(tx, "jobs", "runner_request_id")? {
        tx.execute_batch(
            "ALTER TABLE jobs ADD COLUMN runner_request_id TEXT NOT NULL DEFAULT '';",
        )?;
    }

    // Reconstruct correlation from the immutable acquisition events. At the
    // intent boundary the provisional job ID is the runner request ID; the
    // resolve event records the acquired job ID that replaced it.
    let mut request_ids = HashMap::<(String, u64), String>::new();
    for payload in verified_payloads {
        let event = decode_event_for_schema(&payload, 8)?;
        match event {
            Event::JobAcquisitionIntended {
                job_id, generation, ..
            } => {
                request_ids.insert((job_id.0.clone(), generation.0), job_id.0);
            }
            Event::JobAcquisitionResolved {
                provisional_job_id,
                acquired_job_id,
                generation,
                ..
            } => {
                if provisional_job_id == acquired_job_id {
                    // v8's self-to-self rebuild event did not store the
                    // original runner request ID. Leave the materialized
                    // value empty instead of guessing from the acquired ID.
                    request_ids.remove(&(provisional_job_id.0, generation.0));
                    continue;
                }
                let request_id = request_ids
                    .remove(&(provisional_job_id.0.clone(), generation.0))
                    .unwrap_or_else(|| provisional_job_id.0.clone());
                request_ids.insert((acquired_job_id.0, generation.0), request_id);
            }
            Event::JobAcquisitionLost {
                job_id,
                generation,
                proof,
                ..
            } => {
                if proof.is_some() {
                    request_ids.remove(&(job_id.0, generation.0));
                }
            }
            _ => {}
        }
    }
    for ((job_id, generation), request_id) in request_ids {
        tx.execute(
            "UPDATE jobs SET runner_request_id = ?1
             WHERE job_id = ?2 AND generation = ?3 AND runner_request_id = ''",
            params![
                request_id,
                job_id,
                generation_to_sql(Generation(generation))?
            ],
        )?;
    }
    tx.pragma_update(None, "user_version", 9u32)?;
    Ok(())
}

/// v10 adds the atomic, probeable event used when repairing a confirmed
/// run-service acquisition whose pre-call intent is absent. Older writers do
/// not know this event vocabulary, so stamp before they can append to the log.
/// Verify existing checksums before changing the compatibility stamp.
fn migrate_v9_to_v10(tx: &rusqlite::Transaction<'_>) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 10 {
        return Ok(());
    }

    let mut statement = tx.prepare("SELECT payload, checksum FROM events ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (payload, checksum) = row?;
        if sha256_hex(payload.as_bytes()) != checksum {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.checksum.mismatch",
            )
            .with_remediation(
                "preserve the schema-v9 journal unchanged; event integrity verification failed before the v10 vocabulary stamp",
            ));
        }
        decode_event_for_schema(&payload, 9)?;
    }
    drop(statement);

    tx.pragma_update(None, "user_version", 10u32)?;
    Ok(())
}

/// v11 binds provisional acquisition rows to the exact native permit lease.
/// The event vocabulary is also bumped so older writers cannot ignore those
/// fields and rewrite the materialized rows without them.
fn migrate_v10_to_v11(tx: &rusqlite::Transaction<'_>, initially_fresh: bool) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 11 {
        return Ok(());
    }
    if stored != 10 {
        return Err(jobs_schema_mismatch(u32::try_from(stored).unwrap_or(0)));
    }

    let mut statement = tx.prepare("SELECT payload, checksum FROM events ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (payload, checksum) = row?;
        if sha256_hex(payload.as_bytes()) != checksum {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.checksum.mismatch",
            )
            .with_remediation(
                "preserve the schema-v10 journal unchanged; event integrity verification failed before the v11 migration",
            ));
        }
        decode_event_for_schema(&payload, 10)?;
    }
    drop(statement);

    if table_has_column(tx, "jobs", "permit_lease")? {
        if !initially_fresh || !jobs_v11_shape_matches(tx)? {
            return Err(jobs_schema_mismatch(10));
        }
    } else {
        if !jobs_v10_shape_matches(tx)? {
            return Err(jobs_schema_mismatch(10));
        }
        tx.execute_batch("ALTER TABLE jobs ADD COLUMN permit_lease TEXT;")?;
    }

    tx.pragma_update(None, "user_version", 11u32)?;
    Ok(())
}

/// v12 requires typed run-service loss proof bound to the exact released
/// native permit lease. A v11 writer could otherwise append a proofless event
/// that looks terminal by its reason string and frees occupied capacity.
fn migrate_v11_to_v12(tx: &rusqlite::Transaction<'_>, source_version: u32) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 12 {
        return Ok(());
    }
    if stored != 11 {
        return Err(jobs_schema_mismatch(u32::try_from(stored).unwrap_or(0)));
    }
    if !jobs_v11_shape_matches(tx)? {
        return Err(jobs_schema_mismatch(11));
    }

    let mut statement = tx.prepare("SELECT payload, checksum FROM events ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (payload, checksum) = row?;
        if sha256_hex(payload.as_bytes()) != checksum {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.checksum.mismatch",
            )
            .with_remediation(
                "preserve the schema-v11 journal unchanged; event integrity verification failed before the v12 migration",
            ));
        }
        decode_event_for_schema(&payload, 11)?;
    }
    drop(statement);

    let current_materialized = load_materialized_state(tx)?;
    // Direct v11 upgrades must prove the existing materialized owner identity
    // against replay before any rewrite. Older source schemas run their
    // version-specific migrations first; those historical transformations
    // preserve their own materialized projections and are not reinterpreted as
    // if they had been authored by the v11 reducer.
    let mut replayed = if source_version == 11 {
        let mut replayed = load_state_from_conn(tx)?;
        ensure_v11_replay_preserves_materialized_evidence(&current_materialized, &replayed)?;
        preserve_v11_outbox_timestamps(&current_materialized, &mut replayed)?;
        Some(replayed)
    } else {
        None
    };

    // Retain whether replay crosses a pre-v12 reason-only loss event. The
    // added bit prevents a later `JobOwned`/`JobWorkerLost` pair in the old
    // event history from laundering that ambiguity into a normal owner and
    // freeing its permit.
    tx.execute_batch(
        "ALTER TABLE jobs ADD COLUMN acquisition_loss_unproven INTEGER NOT NULL DEFAULT 0;",
    )?;
    // Earlier self-to-self rebuilds could lose the broker request identity,
    // and old reducers could materialize ownership without a plan. Empty
    // correlation/endpoint data is unknown, not a safe default; a non-
    // provisional row also needs a plan to prove acquired ownership. Keep
    // these rows and their exact permits as occupied evidence through the
    // vocabulary migration.
    tx.execute(
        "UPDATE jobs
         SET acquisition_loss_unproven = 1
         WHERE runner_request_id = ''
            OR run_service_url = ''
            OR (provisional = 0 AND plan_id = '')",
        [],
    )?;

    // v11's reason-only loss events previously removed provisional rows.
    // Under v12 they are ambiguous and must retain their permit correlation.
    // Rebuild materialization from the now-safe reducer before stamping v12,
    // preserving the meta-only drain/admission latches that do not appear in
    // the event log.
    if current_materialized.capacity_invalid
        || replayed
            .as_ref()
            .is_some_and(|state| state.capacity_invalid)
    {
        // Preserve the current rows as forensic evidence. Replay may expose
        // invalid capacity that the old snapshot did not record; latch the
        // fail-closed marker without rewriting any materialized rows.
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('capacity_invalid', '1')
                 ON CONFLICT(key) DO UPDATE SET value = '1'",
            [],
        )?;
    } else if let Some(replayed) = replayed.as_mut() {
        replayed.drain_active = current_materialized.drain_active;
        replayed.drain_version = current_materialized.drain_version;
        replayed.admission_blocked = current_materialized.admission_blocked;
        replayed.admission_version = current_materialized.admission_version;
        persist_state(tx, replayed)?;
    }

    // Install the write gate only after migration writes have completed.
    // From this commit onward every data-table mutation requires an explicit
    // transaction-local gate row, which older open connections cannot create.
    tx.execute_batch(JOURNAL_WRITE_GATE_SCHEMA)?;

    tx.pragma_update(None, "user_version", 12u32)?;
    Ok(())
}

/// v13 persists the wall-clock instant used by reducer decisions alongside
/// each event row. Older schemas omitted that instant, so replay could reset
/// heartbeat timestamps and completion deadlines on every load. The backfill
/// derives pending completion/heartbeat times from the materialized snapshot;
/// a committed timeout event uses its prior row deadline as its deterministic
/// proof time because legacy payloads did not record the observation instant.
fn migrate_v12_to_v13(tx: &rusqlite::Transaction<'_>, source_version: u32) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 13 {
        return Ok(());
    }
    if stored != 12 {
        return Err(journal_schema_version_invalid(stored));
    }
    if !event_schema_matches(tx, 12)? || table_has_column(tx, "events", "recorded_unix")? {
        return Err(journal_schema_catalog_mismatch(
            12,
            "event timestamp column is not in its v12 source shape",
        ));
    }

    let current_materialized = load_materialized_state(tx)?;
    let (recorded_by_id, replayed) = replay_legacy_events(tx, &current_materialized)?;
    if source_version >= 11 {
        ensure_v11_replay_preserves_materialized_evidence(&current_materialized, &replayed)?;
    }

    tx.execute_batch(
        "ALTER TABLE events
         ADD COLUMN recorded_unix INTEGER NOT NULL DEFAULT 0;",
    )?;
    open_journal_write_gate(tx)?;
    for (id, recorded_unix) in recorded_by_id {
        let changed = tx.execute(
            "UPDATE events SET recorded_unix = ?1 WHERE id = ?2",
            params![timestamp_to_sql(recorded_unix, "event recorded_unix")?, id],
        )?;
        if changed != 1 {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.event.timestamp.missing",
            )
            .with_remediation(
                "preserve the schema-v12 journal unchanged; an event row disappeared during timestamp backfill",
            ));
        }
    }
    close_journal_write_gate(tx)?;

    if source_version >= 11 {
        let replayed_after = load_state_from_conn(tx)?;
        ensure_v11_replay_preserves_materialized_evidence(&current_materialized, &replayed_after)?;
    }
    tx.pragma_update(None, "user_version", 13u32)?;
    Ok(())
}

/// v14 binds reducer time into each event checksum. v13 stored the timestamp
/// beside the payload but did not authenticate it, so its migration ignores
/// that column and deterministically rebuilds timestamps from the checksummed
/// event log and materialized heartbeat/outbox evidence before installing the
/// new checksum format.
fn migrate_v13_to_v14(tx: &rusqlite::Transaction<'_>, source_version: u32) -> StoreResult<()> {
    let stored: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if u32::try_from(stored).unwrap_or(0) >= 14 {
        return Ok(());
    }
    if stored != 13 {
        return Err(journal_schema_version_invalid(stored));
    }
    if !event_schema_matches(tx, 13)? {
        return Err(journal_schema_catalog_mismatch(
            13,
            "event timestamp column is not in its v13 source shape",
        ));
    }

    let current_materialized = load_materialized_state(tx)?;
    let (recorded_by_id, replayed) = replay_legacy_events(tx, &current_materialized)?;
    if source_version >= 11 {
        ensure_v11_replay_preserves_materialized_evidence(&current_materialized, &replayed)?;
    }

    let mut statement =
        tx.prepare("SELECT id, generation, kind, payload FROM events ORDER BY id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut payloads = Vec::new();
    for row in rows {
        payloads.push(row?);
    }
    drop(statement);
    let payload_count = payloads.len();

    open_journal_write_gate(tx)?;
    for (id, generation, kind, payload) in payloads {
        let recorded_unix = *recorded_by_id.get(&id).ok_or_else(|| {
            StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.event.timestamp.missing",
            )
            .with_remediation(
                "preserve the schema-v13 journal unchanged; deterministic timestamp replay omitted an event row",
            )
        })?;
        let recorded_unix_sql = timestamp_to_sql(recorded_unix, "event recorded_unix")?;
        let checksum = event_row_checksum(id, generation, &kind, &payload, recorded_unix_sql)?;
        let changed = tx.execute(
            "UPDATE events SET recorded_unix = ?1, checksum = ?2 WHERE id = ?3",
            params![recorded_unix_sql, checksum, id],
        )?;
        if changed != 1 {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.migration.event.timestamp.missing",
            )
            .with_remediation(
                "preserve the schema-v13 journal unchanged; an event row disappeared during integrity migration",
            ));
        }
    }
    if recorded_by_id.len() != payload_count {
        return Err(StoreError::new(
            velnor_model::ExitClass::Conflict,
            "journal.migration.event.timestamp.missing",
        )
        .with_remediation(
            "preserve the schema-v13 journal unchanged; event and timestamp replay row counts differ",
        ));
    }
    close_journal_write_gate(tx)?;

    // The v14 stamp is transaction-local until setup commits. It selects the
    // timestamp-bound checksum verifier used by the post-migration replay;
    // any later error rolls back both the stamp and rewritten rows.
    tx.pragma_update(None, "user_version", 14u32)?;
    let replayed_after = load_state_from_conn(tx)?;
    if source_version >= 11 {
        ensure_v11_replay_preserves_materialized_evidence(&current_materialized, &replayed_after)?;
    }
    Ok(())
}

fn jobs_v10_shape_matches(conn: &Connection) -> StoreResult<bool> {
    jobs_shape_matches_version(conn, 10)
}

fn jobs_v11_shape_matches(conn: &Connection) -> StoreResult<bool> {
    jobs_shape_matches_version(conn, 11)
}

fn jobs_v12_shape_matches(conn: &Connection) -> StoreResult<bool> {
    jobs_shape_matches_version(conn, 12)
}

fn jobs_shape_matches_version(conn: &Connection, version: u32) -> StoreResult<bool> {
    let mut expected = vec![
        ("job_id", "TEXT", 0, None, 1),
        ("slot_id", "TEXT", 1, None, 0),
        ("generation", "INTEGER", 1, None, 0),
        ("attempt", "INTEGER", 1, None, 0),
        ("worker", "TEXT", 1, None, 0),
        ("phase", "TEXT", 1, None, 0),
        ("accepted_unix", "INTEGER", 1, Some("0"), 0),
    ];
    if version >= 4 {
        expected.push(("terminal_conclusion", "TEXT", 0, None, 0));
    }
    if version >= 5 {
        expected.push(("provisional", "INTEGER", 1, Some("0"), 0));
    }
    if version >= 6 {
        expected.extend([
            ("plan_id", "TEXT", 1, Some("''"), 0),
            ("run_service_url", "TEXT", 1, Some("''"), 0),
            ("probe_attempts", "INTEGER", 1, Some("0"), 0),
            ("probe_deadline_unix", "INTEGER", 1, Some("0"), 0),
        ]);
    }
    if version >= 9 {
        expected.push(("runner_request_id", "TEXT", 1, Some("''"), 0));
    }
    if version >= 11 {
        expected.push(("permit_lease", "TEXT", 0, None, 0));
    }
    if version >= 12 {
        expected.push(("acquisition_loss_unproven", "INTEGER", 1, Some("0"), 0));
    }
    job_columns_match(conn, &expected)
}

fn jobs_v6_repair_shape_matches(conn: &Connection) -> StoreResult<bool> {
    let expected = vec![
        ("job_id", "TEXT", 0, None, 1),
        ("slot_id", "TEXT", 1, None, 0),
        ("generation", "INTEGER", 1, None, 0),
        ("attempt", "INTEGER", 1, None, 0),
        ("worker", "TEXT", 1, None, 0),
        ("phase", "TEXT", 1, None, 0),
        ("accepted_unix", "INTEGER", 1, Some("0"), 0),
        ("terminal_conclusion", "TEXT", 0, None, 0),
        ("plan_id", "TEXT", 1, Some("''"), 0),
        ("run_service_url", "TEXT", 1, Some("''"), 0),
        ("probe_attempts", "INTEGER", 1, Some("0"), 0),
        ("probe_deadline_unix", "INTEGER", 1, Some("0"), 0),
    ];
    // v6's known poisoned shape is the v6 fields without the v5 provisional
    // bit; no other partially migrated combination is repairable.
    job_columns_match(conn, &expected)
}

fn job_columns_match(
    conn: &Connection,
    expected: &[(&str, &str, i64, Option<&str>, i64)],
) -> StoreResult<bool> {
    if table_definition_has_explicit_collation(conn, "jobs")?
        || !table_indexes_use_binary_collation(conn, "jobs")?
    {
        return Ok(false);
    }
    let columns = table_schema_columns(conn, "jobs")?;
    // SQLite appends columns when repairing a known historical shape. Column
    // order has no meaning to the named queries and inserts in this module;
    // validate the complete definitions by name instead of rejecting a
    // semantically identical table solely because ALTER TABLE appended one.
    Ok(columns.len() == expected.len()
        && expected.iter().all(|expected| {
            columns.iter().any(|actual| {
                let (name, ty, not_null, default, pk) = actual;
                let (expected_name, expected_ty, expected_not_null, expected_default, expected_pk) =
                    expected;
                name.eq_ignore_ascii_case(expected_name)
                    && ty.eq_ignore_ascii_case(expected_ty)
                    && *not_null == *expected_not_null
                    && default.as_deref() == *expected_default
                    && *pk == *expected_pk
            })
        }))
}

fn event_schema_matches(conn: &Connection, version: u32) -> StoreResult<bool> {
    let mut expected = vec![
        ("id", "INTEGER", 0, None, 1),
        ("generation", "INTEGER", 1, None, 0),
        ("kind", "TEXT", 1, None, 0),
        ("payload", "TEXT", 1, None, 0),
        ("checksum", "TEXT", 1, None, 0),
    ];
    if version >= 13 {
        expected.push(("recorded_unix", "INTEGER", 1, Some("0"), 0));
    }
    table_columns_match(conn, "events", &expected)
}

fn slots_schema_matches(conn: &Connection) -> StoreResult<bool> {
    table_columns_match(
        conn,
        "slots",
        &[
            ("slot_id", "TEXT", 0, None, 1),
            ("generation", "INTEGER", 1, None, 0),
            ("phase", "TEXT", 1, None, 0),
            ("permit_held", "INTEGER", 1, Some("0"), 0),
            ("routing_valid", "INTEGER", 1, Some("0"), 0),
            ("session_live", "INTEGER", 1, Some("0"), 0),
            ("executor_proven", "INTEGER", 1, Some("0"), 0),
            ("registered", "INTEGER", 1, Some("0"), 0),
            ("pid", "INTEGER", 0, None, 0),
            ("heartbeat_unix", "INTEGER", 1, Some("0"), 0),
        ],
    )
}

fn meta_schema_matches(conn: &Connection) -> StoreResult<bool> {
    table_columns_match(
        conn,
        "meta",
        &[("key", "TEXT", 0, None, 1), ("value", "TEXT", 1, None, 0)],
    )
}

fn table_columns_match(
    conn: &Connection,
    table: &str,
    expected: &[(&str, &str, i64, Option<&str>, i64)],
) -> StoreResult<bool> {
    if table_definition_has_explicit_collation(conn, table)?
        || !table_indexes_use_binary_collation(conn, table)?
    {
        return Ok(false);
    }
    let columns = table_schema_columns(conn, table)?;
    Ok(columns.len() == expected.len()
        && expected.iter().all(|expected| {
            columns.iter().any(|actual| {
                let (name, ty, not_null, default, pk) = actual;
                let (expected_name, expected_ty, expected_not_null, expected_default, expected_pk) =
                    expected;
                name.eq_ignore_ascii_case(expected_name)
                    && ty.eq_ignore_ascii_case(expected_ty)
                    && *not_null == *expected_not_null
                    && default.as_deref() == *expected_default
                    && *pk == *expected_pk
            })
        }))
}

/// All journal table declarations use SQLite's default BINARY collation.
/// `PRAGMA table_info` omits collations, so a `COLLATE NOCASE` primary key can
/// otherwise pass the column-shape checks while changing job/slot identity
/// equality. Fail closed on any explicit table-level collation.
fn table_definition_has_explicit_collation(conn: &Connection, table: &str) -> StoreResult<bool> {
    let sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'table' AND name = ?1 COLLATE NOCASE",
            [table],
            |row| row.get(0),
        )
        .optional()?;
    Ok(sql.is_some_and(|sql| {
        sql.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| token.eq_ignore_ascii_case("collate"))
    }))
}

/// Reject non-BINARY collations on every indexed key column. This also
/// covers explicit index collations, which are not recorded in table_info.
fn table_indexes_use_binary_collation(conn: &Connection, table: &str) -> StoreResult<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA index_list({table})"))?;
    let indexes = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for index in indexes {
        let mut statement =
            conn.prepare("SELECT coll, \"key\" FROM pragma_index_xinfo(?1) ORDER BY seqno")?;
        let rows = statement.query_map([index], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (collation, key) = row?;
            if key != 0
                && !collation
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case("BINARY"))
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
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
    table_schema_columns(conn, "jobs")
}

fn table_schema_columns(conn: &Connection, table: &str) -> StoreResult<Vec<TableColumn>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
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

fn table_exists(conn: &Connection, table: &str) -> StoreResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = ?1 COLLATE NOCASE
         )",
        [table],
        |row| row.get(0),
    )?)
}

fn journal_write_gate_schema_matches(conn: &Connection) -> StoreResult<bool> {
    if !table_exists(conn, "journal_write_gate")? {
        return Ok(false);
    }
    let mut statement = conn.prepare("PRAGMA table_info(journal_write_gate)")?;
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
    if columns != vec![("id".to_owned(), "INTEGER".to_owned(), 0, None, 1)] {
        return Ok(false);
    }
    let gate_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'table' AND name = 'journal_write_gate' COLLATE NOCASE",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let expected_gate_sql = normalize_schema_sql(
        "CREATE TABLE journal_write_gate (
             id INTEGER PRIMARY KEY CHECK (id = 1)
         )",
    );
    if !gate_sql.is_some_and(|sql| normalize_schema_sql(&sql) == expected_gate_sql) {
        return Ok(false);
    }

    let mut expected = Vec::with_capacity(15);
    for table in ["events", "slots", "jobs", "outbox", "meta"] {
        for operation in ["insert", "update", "delete"] {
            expected.push((
                format!("journal_write_gate_{table}_{operation}"),
                table.to_owned(),
                operation.to_owned(),
            ));
        }
    }
    for (name, expected_table, operation) in &expected {
        let trigger: Option<(String, String)> = conn
            .query_row(
                "SELECT tbl_name, sql FROM sqlite_master
                 WHERE type = 'trigger' AND name = ?1 COLLATE NOCASE",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((table, sql)) = trigger else {
            return Ok(false);
        };
        let expected_sql = normalize_schema_sql(&format!(
            "CREATE TRIGGER {name}
             BEFORE {operation} ON {expected_table}
             WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
             BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END",
        ));
        if !table.eq_ignore_ascii_case(expected_table) || normalize_schema_sql(&sql) != expected_sql
        {
            return Ok(false);
        }
    }
    let trigger_count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM sqlite_master
         WHERE type = 'trigger'",
        [],
        |row| row.get(0),
    )?;
    Ok(trigger_count == i64::try_from(expected.len()).unwrap_or(i64::MAX))
}

fn journal_data_table_triggers_exist(conn: &Connection) -> StoreResult<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM sqlite_master
         WHERE type = 'trigger'",
        [],
        |row| row.get(0),
    )?;
    Ok(count != 0)
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(';')
        .to_ascii_lowercase()
}

fn journal_write_gate_has_open_row(conn: &Connection) -> StoreResult<bool> {
    if !table_exists(conn, "journal_write_gate")? {
        return Ok(false);
    }
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM journal_write_gate", [], |row| {
        row.get(0)
    })?;
    Ok(count != 0)
}

fn jobs_schema_mismatch(version: u32) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
        .with_remediation(format!(
            "preserve the journal unchanged: PRAGMA user_version={version} carries an unrecognized jobs shape"
        ))
}

fn journal_schema_version_invalid(version: i64) -> StoreError {
    StoreError::new(
        velnor_model::ExitClass::Conflict,
        "journal.schema.version.invalid",
    )
    .with_remediation(format!(
        "preserve the journal unchanged: PRAGMA user_version={version} is outside the supported nonnegative range"
    ))
}

fn journal_write_gate_schema_mismatch(version: u32) -> StoreError {
    StoreError::new(velnor_model::ExitClass::Conflict, "journal.schema.mismatch")
        .with_remediation(format!(
            "preserve the journal unchanged: PRAGMA user_version={version} has an incomplete or open write gate"
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
        | Event::JobAcquisitionIntendedWithPermit { generation, .. }
        | Event::JobAcquisitionResolved { generation, .. }
        | Event::AcquisitionProbeFailed { generation, .. }
        | Event::JobAcquisitionLost { generation, .. }
        | Event::JobAcquisitionRebuilt { generation, .. }
        | Event::JobAcquisitionRebuiltWithPermit { generation, .. }
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
        | Event::SlotStale { generation, .. } => *generation,
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
        Event::JobAcquisitionIntendedWithPermit { .. } => "job_acquisition_intended_with_permit",
        Event::JobAcquisitionResolved { .. } => "job_acquisition_resolved",
        Event::JobAcquisitionRebuilt { .. } => "job_acquisition_rebuilt",
        Event::JobAcquisitionRebuiltWithPermit { .. } => "job_acquisition_rebuilt_with_permit",
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

fn event_row_checksum(
    id: i64,
    generation: i64,
    kind: &str,
    payload: &str,
    recorded_unix: i64,
) -> StoreResult<String> {
    let domain = b"velnor-journal-event-v14\0";
    let kind_len = u64::try_from(kind.len()).map_err(|_| {
        StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.checksum.input.range",
        )
    })?;
    let payload_len = u64::try_from(payload.len()).map_err(|_| {
        StoreError::new(
            velnor_model::ExitClass::Operation,
            "journal.checksum.input.range",
        )
    })?;
    let mut bytes = Vec::with_capacity(domain.len() + 32 + kind.len() + payload.len());
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&id.to_be_bytes());
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(&kind_len.to_be_bytes());
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(&recorded_unix.to_be_bytes());
    bytes.extend_from_slice(&payload_len.to_be_bytes());
    bytes.extend_from_slice(payload.as_bytes());
    Ok(sha256_hex(&bytes))
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

    #[test]
    fn fresh_empty_database_initializes_before_historical_phase_checks() {
        let nanos = unix_now();
        let dir = std::env::temp_dir().join(format!(
            "velnor-journal-fresh-empty-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.db");

        let journal = Journal::open(&path).unwrap();
        let version: i64 = journal
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        assert!(journal.materialized_state().unwrap().slots.is_empty());
        let baseline: ReplayBaseline = serde_json::from_str(
            &journal
                .conn
                .query_row(
                    "SELECT value FROM meta WHERE key = 'replay_baseline'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(baseline.event_count, 0);
        assert_eq!(baseline.through_event_id, 0);
        assert_eq!(baseline.latest_event_count, 0);
        assert_eq!(baseline.latest_event_id, 0);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn drop_jobs_columns(conn: &Connection, columns: &[&str]) {
        for column in columns {
            if table_has_column(conn, "jobs", column).unwrap() {
                conn.execute_batch(&format!("ALTER TABLE jobs DROP COLUMN {column};"))
                    .unwrap();
            }
        }
    }

    fn open_fixture_write_gate(conn: &Connection) {
        if table_exists(conn, "journal_write_gate").unwrap() {
            conn.execute("INSERT INTO journal_write_gate (id) VALUES (1)", [])
                .unwrap();
        }
    }

    fn close_fixture_write_gate(conn: &Connection) {
        if table_exists(conn, "journal_write_gate").unwrap() {
            conn.execute("DELETE FROM journal_write_gate WHERE id = 1", [])
                .unwrap();
        }
    }

    /// Keep test-only raw event appends paired with the same tail watermark
    /// that a production transaction updates alongside `persist_state`.
    fn refresh_fixture_replay_baseline_tail(conn: &Connection) {
        if !table_exists(conn, "meta").unwrap() {
            return;
        }
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'replay_baseline'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        let Some(raw) = raw else {
            return;
        };
        let mut baseline: ReplayBaseline = serde_json::from_str(&raw).unwrap();
        let (event_count, event_id): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(id), 0) FROM events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        baseline.latest_event_count = event_count;
        baseline.latest_event_id = event_id;
        let checksum = replay_baseline_integrity_checksum(&baseline).unwrap();
        baseline.integrity_checksum = checksum;
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'replay_baseline'",
            [serde_json::to_string(&baseline).unwrap()],
        )
        .unwrap();
    }

    fn append_fixture_event(conn: &Connection, event: &Event) {
        append_fixture_event_omitting_fields(conn, event, &[]);
    }

    fn append_fixture_event_omitting_fields(
        conn: &Connection,
        event: &Event,
        omitted_fields: &[&str],
    ) {
        let mut value = serde_json::to_value(event).unwrap();
        if let Some(object) = value.as_object_mut() {
            for field in omitted_fields {
                object.remove(*field);
            }
        }
        let payload = serde_json::to_string(&value).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let checksum = if version >= 14 {
            String::new()
        } else {
            sha256_hex(payload.as_bytes())
        };
        open_fixture_write_gate(conn);
        conn.execute(
            "INSERT INTO events (generation, kind, payload, checksum)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                event_generation(event).0 as i64,
                event_kind(event),
                payload,
                checksum,
            ],
        )
        .unwrap();
        if version >= 14 {
            let id = conn.last_insert_rowid();
            let generation_sql = generation_to_sql(event_generation(event)).unwrap();
            let checksum =
                event_row_checksum(id, generation_sql, event_kind(event), &payload, 0).unwrap();
            conn.execute(
                "UPDATE events SET checksum = ?1 WHERE id = ?2",
                params![checksum, id],
            )
            .unwrap();
            refresh_fixture_replay_baseline_tail(conn);
        }
        close_fixture_write_gate(conn);
    }

    fn assert_bad_write_gate_fails_before_wal(label: &str, schema_edit: &str) {
        let (dir, journal) = open_tmp(label);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema_edit).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists(), "preflight starts without a WAL sidecar");
        assert!(!shm.exists(), "preflight starts without an SHM sidecar");
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );
        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn assert_v11_schema_edit_fails_before_wal(label: &str, schema_edit: &str) {
        let (dir, journal) = open_tmp(label);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.execute_batch(schema_edit).unwrap();
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );
        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn assert_v10_materialized_edit_fails_before_wal(label: &str, materialized_edit: &str) {
        let path = make_v10_acquisition_journal(label);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(materialized_edit).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );

        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 10);
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn assert_v11_materialized_edit_fails_before_wal(label: &str, materialized_edit: &str) {
        let (dir, mut journal) = open_tmp(label);
        prime_ready(&mut journal, "scope-1");
        let permit_lease = NativePermitLease {
            holder: "native/v1/scope/request-1".to_owned(),
            ledger_path: "/var/lib/velnor/permit-ledger.db".to_owned(),
            generation: 31,
        };
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("request-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                    permit_lease,
                })
                .unwrap()
                .rejected
        );
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        conn.execute_batch(materialized_edit).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );

        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn assert_v2_schema_edit_fails_before_wal(label: &str, schema_edit: &str) {
        let (dir, journal) = open_tmp(label);
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 2);

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema_edit).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );
        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn assert_bad_collation_fails_before_wal(label: &str, schema_edit: &str) {
        let (dir, journal) = open_tmp(label);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_journal_write_gate(&conn);
        conn.execute_batch(schema_edit).unwrap();
        conn.execute_batch(JOURNAL_WRITE_GATE_SCHEMA).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists(), "preflight starts without a WAL sidecar");
        assert!(!shm.exists(), "preflight starts without an SHM sidecar");
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn drop_journal_write_gate(conn: &Connection) {
        for table in ["events", "slots", "jobs", "outbox", "meta"] {
            for operation in ["insert", "update", "delete"] {
                let name = format!("journal_write_gate_{table}_{operation}");
                conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {name};"))
                    .unwrap();
            }
        }
        conn.execute_batch("DROP TABLE IF EXISTS journal_write_gate;")
            .unwrap();
    }

    /// Make a current-schema fixture carry the jobs shape that existed at the
    /// requested schema version. Migration tests must not pair an old stamp
    /// with columns introduced by later migrations: open() rejects that as a
    /// downgrade/partial-write shape before the migration under test runs.
    fn drop_jobs_columns_added_after_version(conn: &Connection, version: u32) {
        if version < 14 && table_exists(conn, "meta").unwrap() {
            let has_baseline = conn
                .query_row(
                    "SELECT 1 FROM meta WHERE key = 'replay_baseline'",
                    [],
                    |_| Ok(()),
                )
                .optional()
                .unwrap()
                .is_some();
            if has_baseline {
                open_fixture_write_gate(conn);
                conn.execute("DELETE FROM meta WHERE key = 'replay_baseline'", [])
                    .unwrap();
                close_fixture_write_gate(conn);
            }
        }
        if version < 12 {
            drop_journal_write_gate(conn);
        }
        if version < 13 && table_has_column(conn, "events", "recorded_unix").unwrap() {
            conn.execute_batch("ALTER TABLE events DROP COLUMN recorded_unix;")
                .unwrap();
        }
        for (introduced, column) in [
            (4, "terminal_conclusion"),
            (5, "provisional"),
            (6, "plan_id"),
            (6, "run_service_url"),
            (6, "probe_attempts"),
            (6, "probe_deadline_unix"),
            (9, "runner_request_id"),
            (11, "permit_lease"),
            (12, "acquisition_loss_unproven"),
        ] {
            if introduced > version && table_has_column(conn, "jobs", column).unwrap() {
                conn.execute_batch(&format!("ALTER TABLE jobs DROP COLUMN {column};"))
                    .unwrap();
            }
        }
        if version < 14 {
            let mut statement = conn
                .prepare("SELECT id, payload FROM events ORDER BY id")
                .unwrap();
            let rows: Vec<(i64, String)> = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            drop(statement);
            open_fixture_write_gate(conn);
            for (id, payload) in rows {
                conn.execute(
                    "UPDATE events SET checksum = ?1 WHERE id = ?2",
                    params![sha256_hex(payload.as_bytes()), id],
                )
                .unwrap();
            }
            close_fixture_write_gate(conn);
        }
    }

    fn make_v13_migrated_fixture(label: &str) -> (PathBuf, PathBuf) {
        let (dir, mut journal) = open_tmp(label);
        assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
        assert!(
            !journal
                .apply(Event::Dependency {
                    github_reachable: true,
                })
                .unwrap()
                .rejected
        );
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 13);
        conn.pragma_update(None, "user_version", 13u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let migrated = Journal::open(&path).expect("v13 fixture migrates to v14");
        assert!(migrated
            .conn
            .query_row(
                "SELECT 1 FROM meta WHERE key = 'replay_baseline'",
                [],
                |_| Ok(())
            )
            .optional()
            .unwrap()
            .is_some());
        drop(migrated);
        (dir, path)
    }

    fn strip_v11_event_fields(conn: &Connection) {
        let mut statement = conn
            .prepare("SELECT id, payload FROM events ORDER BY id")
            .unwrap();
        let rows: Vec<(i64, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(statement);

        for (event_id, original) in rows {
            let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
            let event_type = value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let Some(object) = value.as_object_mut() else {
                continue;
            };
            let fields = match event_type.as_deref() {
                Some("job_acquisition_intended") => &["runner_request_id"][..],
                Some("job_acquisition_resolved") => &["runner_request_id", "permit_lease"][..],
                _ => &[],
            };
            let changed = fields.iter().fold(false, |changed, field| {
                object.remove(*field).is_some() || changed
            });
            if changed {
                let payload = serde_json::to_string(&value).unwrap();
                let checksum = sha256_hex(payload.as_bytes());
                open_fixture_write_gate(conn);
                conn.execute(
                    "UPDATE events SET payload = ?1, checksum = ?2 WHERE id = ?3",
                    params![payload, checksum, event_id],
                )
                .unwrap();
                close_fixture_write_gate(conn);
            }
        }
    }

    fn add_v11_event_field(
        conn: &Connection,
        event_type: &str,
        field: &str,
        field_value: serde_json::Value,
    ) -> i64 {
        let mut statement = conn
            .prepare("SELECT id, payload FROM events ORDER BY id")
            .unwrap();
        let rows: Vec<(i64, String)> = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(statement);
        let (event_id, original) = rows
            .into_iter()
            .find(|(_, payload)| {
                serde_json::from_str::<serde_json::Value>(payload)
                    .unwrap()
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    == Some(event_type)
            })
            .unwrap_or_else(|| panic!("event {event_type} missing from fixture"));
        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), field_value);
        let payload = serde_json::to_string(&value).unwrap();
        let checksum = sha256_hex(payload.as_bytes());
        open_fixture_write_gate(conn);
        conn.execute(
            "UPDATE events SET payload = ?1, checksum = ?2 WHERE id = ?3",
            params![payload, checksum, event_id],
        )
        .unwrap();
        close_fixture_write_gate(conn);
        event_id
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

    #[test]
    fn empty_version_zero_file_migrates_from_no_tables() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-journal-empty-v0-{}-{}",
            std::process::id(),
            unix_now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("journal.db");
        drop(Connection::open(&path).unwrap());

        let journal = Journal::open(&path).expect("an empty version-0 database is a fresh journal");
        assert_eq!(
            journal.materialized_state().unwrap(),
            FleetState {
                journal_writable: true,
                ..FleetState::default()
            }
        );
        let version: i64 = journal
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        drop(journal);
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
            let event_label = format!("{event:?}");
            let outcome = journal.apply(event).unwrap();
            assert!(!outcome.rejected, "unexpectedly rejected {event_label}");
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

    fn prime_ready_in_existing_capacity(journal: &mut Journal, id: &str) {
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
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        for event in [
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
            let event_label = format!("{event:?}");
            let outcome = journal.apply(event).unwrap();
            assert!(!outcome.rejected, "unexpectedly rejected {event_label}");
        }
        let ready = journal
            .apply(Event::ReadyAttempt {
                slot_id: s,
                generation: g,
            })
            .unwrap();
        assert!(!ready.rejected, "ReadyAttempt rejected: {ready:?}");
    }

    fn resolve_test_acquisition(
        journal: &mut Journal,
        provisional_job_id: &str,
        acquired_job_id: &str,
        runner_request_id: &str,
        generation: Generation,
        permit_lease: Option<NativePermitLease>,
    ) {
        let outcome = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job(provisional_job_id),
                acquired_job_id: job(acquired_job_id),
                plan_id: "plan-1".to_owned(),
                generation,
                runner_request_id: Some(runner_request_id.to_owned()),
                permit_lease,
            })
            .unwrap();
        assert!(
            !outcome.rejected,
            "test acquisition resolution was rejected: {outcome:?}"
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: job(job_name),
                acquired_job_id: job(job_name),
                plan_id: "plan-1".into(),
                generation: g,
                runner_request_id: Some(job_name.to_owned()),
                permit_lease: None,
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

    fn make_v8_acquisition_journal(label: &str) -> PathBuf {
        let (dir, mut journal) = open_tmp(label);
        let slot_id = slot("scope-1");
        let request_id = job("request-1");
        let acquired_job_id = job("job-1");
        let generation = r#gen();
        prime_ready(&mut journal, &slot_id.0);
        for event in [
            Event::JobAcquisitionIntended {
                slot_id: slot_id.clone(),
                job_id: request_id.clone(),
                generation,
                message_id: "broker-message-1".to_owned(),
                runner_request_id: None,
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_000,
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: request_id,
                acquired_job_id: acquired_job_id.clone(),
                plan_id: "plan-1".to_owned(),
                generation,
                runner_request_id: None,
                permit_lease: None,
            },
            Event::JobOwned {
                job_id: acquired_job_id,
                slot_id,
                attempt: 1,
                generation,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_001,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 8);
        strip_v11_event_fields(&conn);
        conn.pragma_update(None, "user_version", 8u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);
        path
    }

    fn make_v9_acquisition_journal(label: &str) -> PathBuf {
        let path = make_v8_acquisition_journal(label);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE jobs ADD COLUMN runner_request_id TEXT NOT NULL DEFAULT '';",
        )
        .unwrap();
        conn.execute(
            "UPDATE jobs SET runner_request_id = 'request-1' WHERE job_id = 'job-1'",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 9u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);
        path
    }

    fn make_v8_self_resolved_journal(label: &str) -> PathBuf {
        let path = make_v8_acquisition_journal(label);
        let conn = Connection::open(&path).unwrap();
        let mut statement = conn
            .prepare("SELECT id, payload FROM events ORDER BY id")
            .unwrap();
        let mut rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap();
        let mut updates = Vec::new();
        while let Some(row) = rows.next() {
            let (id, payload) = row.unwrap();
            let mut event: Event = serde_json::from_str(&payload).unwrap();
            let changed = match &mut event {
                Event::JobAcquisitionIntended {
                    job_id, message_id, ..
                } => {
                    *job_id = job("job-1");
                    *message_id = "request-1".to_owned();
                    true
                }
                Event::JobAcquisitionResolved {
                    provisional_job_id,
                    acquired_job_id,
                    runner_request_id,
                    ..
                } => {
                    *provisional_job_id = job("job-1");
                    *acquired_job_id = job("job-1");
                    *runner_request_id = None;
                    true
                }
                _ => false,
            };
            if changed {
                updates.push((id, event));
            }
        }
        drop(rows);
        drop(statement);
        for (id, event) in updates {
            let payload = serde_json::to_string(&event).unwrap();
            let checksum = sha256_hex(payload.as_bytes());
            conn.execute(
                "UPDATE events SET payload = ?1, checksum = ?2 WHERE id = ?3",
                params![payload, checksum, id],
            )
            .unwrap();
        }
        strip_v11_event_fields(&conn);
        drop(conn);
        path
    }

    fn make_v10_acquisition_journal(label: &str) -> PathBuf {
        let (dir, mut journal) = open_tmp(label);
        let generation = r#gen();
        let slot_id = slot("scope-1");
        let request_id = job("request-1");
        let acquired_job_id = job("job-1");
        prime_ready(&mut journal, &slot_id.0);
        for event in [
            Event::JobAcquisitionIntended {
                slot_id: slot_id.clone(),
                job_id: request_id.clone(),
                generation,
                message_id: "broker-message-1".to_owned(),
                runner_request_id: None,
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_000,
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: request_id,
                acquired_job_id: acquired_job_id.clone(),
                plan_id: "plan-1".to_owned(),
                generation,
                runner_request_id: None,
                permit_lease: None,
            },
            Event::JobOwned {
                job_id: acquired_job_id,
                slot_id,
                attempt: 1,
                generation,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_001,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let rebuilt_slot = slot("scope-2");
        let rebuilt_generation = r#gen();
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        prime_ready_in_existing_capacity(&mut journal, &rebuilt_slot.0);
        assert!(
            !journal
                .apply(Event::JobAcquisitionRebuilt {
                    slot_id: rebuilt_slot,
                    job_id: job("job-rebuilt-1"),
                    generation: rebuilt_generation,
                    runner_request_id: "request-rebuilt-1".to_owned(),
                    plan_id: "plan-rebuilt-1".to_owned(),
                    run_service_url: "https://run.example/rebuilt-run".to_owned(),
                    probe_deadline_unix: 22_000,
                })
                .unwrap()
                .rejected
        );
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "delete");
        drop_jobs_columns_added_after_version(&conn, 10);
        strip_v11_event_fields(&conn);
        conn.pragma_update(None, "user_version", 10u32).unwrap();
        drop(conn);
        path
    }

    fn make_v10_proofless_loss_journal(label: &str) -> PathBuf {
        let (dir, mut journal) = open_tmp(label);
        let slot_id = slot("scope-1");
        let generation = r#gen();
        prime_ready(&mut journal, &slot_id.0);
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: job("request-1"),
                    generation,
                    message_id: "message-1".to_owned(),
                    runner_request_id: None,
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        append_fixture_event_omitting_fields(
            &conn,
            &Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation,
                reason: "legacy probe budget exhausted".to_owned(),
                proof: None,
            },
            &["proof"],
        );
        open_fixture_write_gate(&conn);
        conn.execute("DELETE FROM jobs", []).unwrap();
        conn.execute(
            "UPDATE slots SET phase = 'ready' WHERE slot_id = ?1",
            [slot_id.0],
        )
        .unwrap();
        close_fixture_write_gate(&conn);
        drop_jobs_columns_added_after_version(&conn, 10);
        strip_v11_event_fields(&conn);
        conn.pragma_update(None, "user_version", 10u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);
        path
    }

    fn make_v11_proofless_loss_journal(label: &str) -> (PathBuf, NativePermitLease) {
        let (dir, mut journal) = open_tmp(label);
        let slot_id = slot("scope-1");
        let generation = r#gen();
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-1"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 41,
        };
        prime_ready(&mut journal, &slot_id.0);
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot_id.clone(),
                    job_id: job("request-1"),
                    generation,
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                    permit_lease: permit_lease.clone(),
                })
                .unwrap()
                .rejected
        );
        assert!(journal.set_drain(7).unwrap());
        assert!(journal.set_admission_blocked(9).unwrap());
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        append_fixture_event(
            &conn,
            &Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation,
                reason: "probe budget spent".to_owned(),
                proof: None,
            },
        );
        append_fixture_event(
            &conn,
            &Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot_id.clone(),
                job_id: job("request-1"),
                generation,
                message_id: "message-1-replayed".to_owned(),
                runner_request_id: "request-1".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_001,
                permit_lease: permit_lease.clone(),
            },
        );
        append_fixture_event(
            &conn,
            &Event::JobOwned {
                job_id: job("request-1"),
                slot_id: slot_id.clone(),
                attempt: 1,
                generation,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_002,
            },
        );
        append_fixture_event(
            &conn,
            &Event::JobWorkerLost {
                job_id: job("request-1"),
                generation,
            },
        );

        // Model the v11 materialization after its reason-only loss and later
        // worker teardown have already removed the row. v12 must reconstruct
        // the retained permit holder from the checksummed history.
        conn.execute("DELETE FROM jobs", []).unwrap();
        conn.execute(
            "UPDATE slots SET phase = 'ready' WHERE slot_id = ?1",
            [slot_id.0],
        )
        .unwrap();
        drop(conn);
        (path, permit_lease)
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
                    runner_request_id: None,
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
                    runner_request_id: None,
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
        open_fixture_write_gate(&journal.conn);
        journal
            .conn
            .execute(
                "INSERT INTO meta (key, value) VALUES ('future_key', 'anything')",
                [],
            )
            .unwrap();
        close_fixture_write_gate(&journal.conn);
        let state = journal.materialized_state().unwrap();
        assert!(!state.drain_active);

        open_fixture_write_gate(&journal.conn);
        journal
            .conn
            .execute(
                "INSERT INTO meta (key, value) VALUES ('drain', 'bogus')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )
            .unwrap();
        close_fixture_write_gate(&journal.conn);
        let error = journal.materialized_state().unwrap_err();
        assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persist_state_drops_unknown_meta_keys_on_apply() {
        let (dir, mut journal) = open_tmp("drain-meta-apply-drops");
        open_fixture_write_gate(&journal.conn);
        journal
            .conn
            .execute(
                "INSERT INTO meta (key, value) VALUES ('future_key', 'anything')",
                [],
            )
            .unwrap();
        close_fixture_write_gate(&journal.conn);
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
    fn stale_pre_drain_connection_cannot_write_after_v12_migration() {
        let (dir, mut journal) = open_tmp("drain-old-writer");
        let generation = prime_running_job(&mut journal, "scope-1", "job-1");
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation,
                    payload_sha256: payload_checksum(b"done"),
                })
                .unwrap()
                .rejected
        );
        assert!(journal.set_drain(7).unwrap());
        drop(journal);

        let path = dir.join("journal.db");
        let downgrade = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&downgrade, 11);
        downgrade
            .pragma_update(None, "user_version", 11u32)
            .unwrap();
        downgrade
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(downgrade);

        // This handle was opened by the old writer before migration. Its
        // prepared table operations must encounter the persistent triggers
        // after v12 setup installs them.
        let stale = Connection::open(&path).unwrap();
        let mut migrated = Journal::open(&path).unwrap();
        let before_events = event_count(&migrated);
        let event = Event::JournalWritable;
        let payload = serde_json::to_string(&event).unwrap();
        let insert = stale.execute(
            "INSERT INTO events (generation, kind, payload, checksum)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                event_generation(&event).0 as i64,
                event_kind(&event),
                payload,
                sha256_hex(payload.as_bytes()),
            ],
        );
        assert!(insert.is_err(), "stale event append must be fenced");
        assert!(
            stale
                .execute("UPDATE events SET checksum = checksum WHERE id = 1", [])
                .is_err(),
            "stale event rewrite must be fenced"
        );
        assert!(
            stale
                .execute("UPDATE slots SET heartbeat_unix = 99", [])
                .is_err(),
            "stale slot rewrite must be fenced"
        );
        assert!(
            stale
                .execute("UPDATE jobs SET phase = 'running'", [])
                .is_err(),
            "stale job rewrite must be fenced"
        );
        assert!(
            stale
                .execute("UPDATE outbox SET attempts = attempts + 1", [])
                .is_err(),
            "stale outbox rewrite must be fenced"
        );
        let delete = stale.execute("DELETE FROM meta WHERE key = 'drain'", []);
        assert!(delete.is_err(), "stale meta rewrite must be fenced");

        let state = migrated.materialized_state().unwrap();
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 7);
        assert!(state.control_live);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.outbox.len(), 1);
        assert_eq!(state.outbox[0].attempts, 0);
        assert_eq!(event_count(&migrated), before_events);
        assert!(migrated.set_drain(8).unwrap());
        assert_eq!(
            read_drain_state(&path),
            Ok(Some(DrainState {
                active: true,
                version: 8,
            }))
        );
        drop(stale);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn inverted_write_gate_predicate_is_refused_before_wal_or_sidecars() {
        assert_bad_write_gate_fails_before_wal(
            "write-gate-inverted-predicate",
            "DROP TRIGGER journal_write_gate_events_insert;
             CREATE TRIGGER journal_write_gate_events_insert
             BEFORE INSERT ON events
             WHEN EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
             BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;",
        );
    }

    #[test]
    fn extra_triggers_on_guarded_tables_or_gate_are_refused_before_wal() {
        for (label, schema_edit) in [
            (
                "write-gate-extra-table-trigger",
                "CREATE TRIGGER rogue_after_event_delete
                 AFTER DELETE ON events BEGIN SELECT 1; END;",
            ),
            (
                "write-gate-extra-gate-trigger",
                "CREATE TRIGGER rogue_gate_reopen
                 AFTER DELETE ON journal_write_gate
                 BEGIN INSERT INTO journal_write_gate (id) VALUES (1); END;",
            ),
        ] {
            assert_bad_write_gate_fails_before_wal(label, schema_edit);
        }
    }

    #[test]
    fn foreign_key_to_write_gate_is_refused_before_wal() {
        assert_bad_write_gate_fails_before_wal(
            "write-gate-foreign-key",
            "CREATE TABLE extension_gate_rows (
                 gate_id INTEGER REFERENCES journal_write_gate(id) ON DELETE CASCADE
             );",
        );
    }

    #[test]
    fn explicit_nonbinary_table_and_index_collations_are_refused_before_wal() {
        assert_bad_collation_fails_before_wal(
            "journal-jobs-nocase-collation",
            "DROP TABLE jobs;
             CREATE TABLE jobs (
                 job_id TEXT PRIMARY KEY,
                 slot_id TEXT COLLATE NOCASE NOT NULL,
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
                 probe_deadline_unix INTEGER NOT NULL DEFAULT 0,
                 runner_request_id TEXT NOT NULL DEFAULT '',
                 permit_lease TEXT,
                 acquisition_loss_unproven INTEGER NOT NULL DEFAULT 0
             );",
        );
        assert_bad_collation_fails_before_wal(
            "journal-events-index-nocase-collation",
            "DROP INDEX events_generation_kind_id_idx;
             CREATE INDEX events_generation_kind_id_idx
                 ON events (generation, kind COLLATE NOCASE, id DESC);",
        );
    }

    #[test]
    fn a_write_that_reopens_the_gate_aborts_before_commit() {
        let (dir, mut journal) = open_tmp("write-gate-close-reopen");
        let path = dir.join("journal.db");
        let before_events = event_count(&journal);
        let schema = Connection::open(&path).unwrap();
        schema
            .execute_batch(
                "CREATE TRIGGER rogue_gate_reopen
                 AFTER DELETE ON journal_write_gate
                 BEGIN INSERT INTO journal_write_gate (id) VALUES (1); END;",
            )
            .unwrap();
        drop(schema);

        let error = journal.apply(Event::ControlLive).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(event_count(&journal), before_events);
        assert!(!journal.materialized_state().unwrap().control_live);
        assert!(!journal_write_gate_has_open_row(&journal.conn).unwrap());
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_generation_outside_sqlite_integer_range_is_rejected_without_writing() {
        let (dir, mut journal) = open_tmp("generation-sqlite-range");
        let before_events = event_count(&journal);
        let error = journal
            .apply(Event::PermitReserved {
                slot_id: slot("scope-1"),
                generation: Generation(i64::MAX as u64 + 1),
            })
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.generation.range");
        assert_eq!(event_count(&journal), before_events);
        assert!(journal.materialized_state().unwrap().slots.is_empty());
        assert!(!journal_write_gate_has_open_row(&journal.conn).unwrap());
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_overflowing_generation_in_a_later_batch_event_commits_nothing() {
        let (dir, mut journal) = open_tmp("generation-sqlite-range-batch");
        let before_events = event_count(&journal);
        let error = journal
            .apply_many([
                Event::ControlLive,
                Event::PackageActivated {
                    apt_version: "candidate".to_owned(),
                    generation: i64::MAX as u64 + 1,
                },
            ])
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.generation.range");
        assert_eq!(event_count(&journal), before_events);
        let state = journal.materialized_state().unwrap();
        assert!(!state.control_live);
        assert_eq!(state.package_generation, 0);
        assert!(!journal_write_gate_has_open_row(&journal.conn).unwrap());
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn materialized_generation_outside_sqlite_range_rolls_back_all_rows() {
        let (dir, mut journal) = open_tmp("materialized-generation-sqlite-range");
        prime_ready(&mut journal, "scope-1");
        let before = journal.materialized_state().unwrap();
        let before_events = event_count(&journal);
        let mut invalid = before.clone();
        invalid.slots[0].generation = Generation(i64::MAX as u64 + 1);

        let error = {
            let transaction = journal
                .conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .unwrap();
            open_journal_write_gate(&transaction).unwrap();
            persist_state(&transaction, &invalid).unwrap_err()
        };

        assert_eq!(error.envelope.reason, "journal.generation.range");
        assert_eq!(event_count(&journal), before_events);
        assert_eq!(journal.materialized_state().unwrap(), before);
        assert!(!journal_write_gate_has_open_row(&journal.conn).unwrap());
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn job_and_outbox_generation_overflow_is_rejected_before_materialized_delete() {
        let (dir, mut journal) = open_tmp("job-outbox-generation-sqlite-range");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-1"),
                    job_id: job("job-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: Some("request-1".to_owned()),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "request-1", r#gen(), None);
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job("job-1"),
                    slot_id: slot("scope-1"),
                    attempt: 1,
                    generation: r#gen(),
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1_001,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation: r#gen(),
                    payload_sha256: payload_checksum(b"ok"),
                })
                .unwrap()
                .rejected
        );
        let before = journal.materialized_state().unwrap();

        for mutate in [0_u8, 1_u8] {
            let mut invalid = before.clone();
            if mutate == 0 {
                invalid.jobs[0].generation = Generation(i64::MAX as u64 + 1);
            } else {
                invalid.outbox[0].generation = Generation(i64::MAX as u64 + 1);
            }
            let error = {
                let transaction = journal
                    .conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .unwrap();
                open_journal_write_gate(&transaction).unwrap();
                persist_state(&transaction, &invalid).unwrap_err()
            };
            assert_eq!(error.envelope.reason, "journal.generation.range");
            assert_eq!(journal.materialized_state().unwrap(), before);
            assert!(!journal_write_gate_has_open_row(&journal.conn).unwrap());
        }
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_persisted_timestamp_must_fit_sqlite_before_materialized_delete() {
        let (dir, mut journal) = open_tmp("materialized-timestamp-sqlite-range");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-1"),
                    job_id: job("job-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: Some("request-1".to_owned()),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "request-1", r#gen(), None);
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job("job-1"),
                    slot_id: slot("scope-1"),
                    attempt: 1,
                    generation: r#gen(),
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1_001,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation: r#gen(),
                    payload_sha256: payload_checksum(b"ok"),
                })
                .unwrap()
                .rejected
        );
        let before = journal.materialized_state().unwrap();
        let before_events = event_count(&journal);

        let overflow_timestamp: [fn(&mut FleetState); 5] = [
            |state| state.slots[0].heartbeat_unix = u64::MAX,
            |state| state.jobs[0].accepted_unix = u64::MAX,
            |state| state.jobs[0].probe_deadline_unix = u64::MAX,
            |state| state.outbox[0].created_unix = u64::MAX,
            |state| state.outbox[0].deadline_unix = u64::MAX,
        ];
        for mutate in overflow_timestamp {
            let mut invalid = before.clone();
            mutate(&mut invalid);
            let error = {
                let transaction = journal
                    .conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .unwrap();
                open_journal_write_gate(&transaction).unwrap();
                persist_state(&transaction, &invalid).unwrap_err()
            };
            assert_eq!(error.envelope.reason, "journal.timestamp.range");
            assert_eq!(event_count(&journal), before_events);
            assert_eq!(journal.materialized_state().unwrap(), before);
            assert!(!journal_write_gate_has_open_row(&journal.conn).unwrap());
        }

        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn overflowing_acquisition_deadline_rolls_back_event_and_state() {
        let (dir, mut journal) = open_tmp("acquisition-timestamp-sqlite-range");
        prime_ready(&mut journal, "scope-1");
        let before = journal.materialized_state().unwrap();
        let before_events = event_count(&journal);

        let error = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("request-1"),
                generation: r#gen(),
                message_id: "message-1".to_owned(),
                runner_request_id: None,
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: u64::MAX,
            })
            .unwrap_err();

        assert_eq!(error.envelope.reason, "journal.timestamp.range");
        assert_eq!(event_count(&journal), before_events);
        assert_eq!(journal.materialized_state().unwrap(), before);
        assert!(!journal_write_gate_has_open_row(&journal.conn).unwrap());
        drop(journal);
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
        open_fixture_write_gate(&malformed_journal.conn);
        malformed_journal
            .conn
            .execute(
                "INSERT INTO meta (key, value) VALUES ('drain', 'corrupt')",
                [],
            )
            .unwrap();
        close_fixture_write_gate(&malformed_journal.conn);
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
        open_fixture_write_gate(&journal.conn);
        journal
            .conn
            .execute(
                "UPDATE meta SET value = 'not-a-fence' WHERE key = 'admission'",
                [],
            )
            .unwrap();
        close_fixture_write_gate(&journal.conn);
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
                    runner_request_id: None,
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
                    runner_request_id: None,
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
                    runner_request_id: None,
                    permit_lease: None,
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
    fn recorded_event_time_drives_heartbeat_and_completion_replay() {
        let (dir, mut journal) = open_tmp("recorded-event-time-replay");
        let generation = prime_running_job(&mut journal, "scope-1", "job-1");
        assert!(
            !journal
                .apply(Event::SlotHeartbeat {
                    slot_id: slot("scope-1"),
                    generation,
                    pid: 1234,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("job-1"),
                    generation,
                    payload_sha256: payload_checksum(b"terminal"),
                })
                .unwrap()
                .rejected
        );

        let heartbeat_time: i64 = journal
            .conn
            .query_row(
                "SELECT recorded_unix FROM events WHERE kind = 'slot_heartbeat' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let completion_time: i64 = journal
            .conn
            .query_row(
                "SELECT recorded_unix FROM events WHERE kind = 'completion_intended' ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let materialized = journal.materialized_state().unwrap();
        let replayed = journal.load_state().unwrap();
        assert_eq!(replayed, materialized);
        assert_eq!(
            replayed.slots[0].heartbeat_unix,
            i64_u64(heartbeat_time, "test heartbeat time").unwrap()
        );
        let outbox = replayed
            .outbox
            .iter()
            .find(|row| row.job_id == job("job-1"))
            .unwrap();
        assert_eq!(
            outbox.created_unix,
            i64_u64(completion_time, "test completion time").unwrap()
        );
        assert_eq!(
            outbox.deadline_unix,
            outbox.created_unix + COMPLETION_RESOLUTION_SECONDS
        );

        drop(journal);
        let reopened = Journal::open(dir.join("journal.db")).unwrap();
        assert_eq!(reopened.load_state().unwrap(), materialized);
        drop(reopened);
        std::fs::remove_dir_all(dir).unwrap();
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
        drop_jobs_columns_added_after_version(&conn, 6);
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
                    runner_request_id: None,
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: job("job-1"),
                acquired_job_id: job("job-1"),
                plan_id: "plan-1".into(),
                generation: g,
                runner_request_id: Some("job-1".into()),
                permit_lease: None,
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
        drop_jobs_columns_added_after_version(&conn, 6);
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
        drop_journal_write_gate(&conn);
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
             ) VALUES ('job-1', 'scope-1', 1, 'sum', 1, 1, 0, 1000);",
        )
        .unwrap();
        drop_jobs_columns_added_after_version(&conn, 3);
        conn.pragma_update(None, "user_version", 3u32).unwrap();
        drop(conn);

        let migrated = Journal::open(&path).unwrap();
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
    fn v3_deadline_overflow_is_refused_before_wal() {
        let (dir, journal) = open_tmp("v3-deadline-overflow-prewal");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 3);
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
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox (
                 job_id, slot_id, generation, payload_sha256, intended,
                 send_started, remote_acked, created_unix
             ) VALUES ('job-1', 'scope-1', 1, 'sum', 1, 0, 0, ?1)",
            [i64::MAX],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 3u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.timestamp.range");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!wal.exists(), "overflow must fail before WAL");
        assert!(!shm.exists(), "overflow must fail before WAL");

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
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
    fn observation_batch_matches_full_reducer_state_and_ordered_summaries() {
        let (_expected_dir, mut expected_journal) = open_tmp("observation-reference");
        let (_batched_dir, mut batched_journal) = open_tmp("observation-batched");
        let generation = Generation(2);
        let seed = [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 2 },
            Event::PermitReserved {
                slot_id: slot("scope-a"),
                generation,
            },
            Event::PermitReserved {
                slot_id: slot("scope-b"),
                generation,
            },
        ];
        expected_journal.apply_many(seed.clone()).unwrap();
        batched_journal.apply_many(seed).unwrap();

        let events = vec![
            Event::SlotHeartbeat {
                slot_id: slot("scope-a"),
                generation,
                pid: 101,
            },
            Event::ExecutorProven {
                slot_id: slot("scope-a"),
                generation,
            },
            Event::SessionLive {
                slot_id: slot("scope-a"),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot("scope-a"),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot("scope-a"),
                generation,
            },
            Event::SessionLive {
                slot_id: slot("scope-a"),
                generation,
            },
            Event::SlotHeartbeat {
                slot_id: slot("scope-b"),
                generation,
                pid: 202,
            },
            Event::SessionLive {
                slot_id: slot("scope-b"),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot("scope-b"),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot("scope-b"),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot("scope-a"),
                generation: Generation(1),
            },
            Event::SlotHeartbeat {
                slot_id: slot("scope-b"),
                generation: Generation(1),
                pid: 303,
            },
            Event::RegistrationIntended {
                slot_id: slot("scope-b"),
                generation: Generation(1),
            },
        ];

        let expected_outcomes = expected_journal.apply_many(events.clone()).unwrap();
        let expected_summaries = expected_outcomes
            .into_iter()
            .map(|outcome| ApplySummary {
                commands: outcome.commands,
                rejected: outcome.rejected,
            })
            .collect::<Vec<_>>();
        let summaries = batched_journal.apply_observation_batch(events).unwrap();

        assert_eq!(summaries, expected_summaries);
        assert!(summaries[..10].iter().all(|summary| !summary.rejected));
        assert!(summaries[10..].iter().all(|summary| summary.rejected));
        assert_eq!(
            summaries[3].commands,
            vec![SideEffect::RegisterRunner {
                slot_id: slot("scope-a"),
                generation,
            }]
        );
        assert_eq!(
            summaries[9].commands,
            vec![SideEffect::RegisterRunner {
                slot_id: slot("scope-b"),
                generation,
            }]
        );
        assert!(summaries
            .iter()
            .enumerate()
            .all(|(index, summary)| matches!(index, 3 | 9) || summary.commands.is_empty()));

        let mut expected_state = expected_journal.materialized_state().unwrap();
        let mut batched_state = batched_journal.materialized_state().unwrap();
        for slot in expected_state
            .slots
            .iter_mut()
            .chain(&mut batched_state.slots)
        {
            // Clock reads occur in separate transactions; compare the durable
            // heartbeat value's other semantics without assuming one second.
            slot.heartbeat_unix = 0;
        }
        assert_eq!(batched_state, expected_state);
        assert_eq!(
            event_count(&batched_journal),
            event_count(&expected_journal)
        );
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
                    runner_request_id: None,
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
        let ownership = journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id,
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".to_owned(),
                accepted_unix: 1,
            })
            .unwrap();
        assert!(ownership.rejected);
        let after_rejected_promotion = journal.load_state().unwrap();
        assert_eq!(after_rejected_promotion.health().actual_ready_slots, 0);
        assert_eq!(after_rejected_promotion.jobs.len(), 1);
        assert!(after_rejected_promotion.jobs[0].provisional);
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
        open_fixture_write_gate(&seed);
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
                 ('capacity_declared', '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value;
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
        close_fixture_write_gate(&seed);
        drop_jobs_columns_added_after_version(&seed, 2);
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
        drop(Journal::open(&path).unwrap());
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
            let mut reopened = Journal::open(&path).unwrap();
            // The migration baseline retains the validated forensic
            // projection while its explicit capacity-invalid marker keeps
            // status and writes fenced.
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
        drop_journal_write_gate(&conn);
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
        if version == 0 || version == 2 {
            drop_jobs_columns_added_after_version(&conn, 2);
        }
        conn.pragma_update(None, "user_version", version).unwrap();
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
        drop_jobs_columns_added_after_version(&conn, 3);
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
             );",
        )
        .unwrap();
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
    fn version_zero_v2_outbox_migrates_from_physical_shape() {
        let (dir, journal) = open_tmp("version-zero-v2-outbox");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 0);

        let migrated = Journal::open(&path).unwrap();
        let state = migrated.materialized_state().unwrap();
        assert_eq!(state.outbox[0].slot_id, slot("scope-1"));
        let conn = Connection::open(&path).unwrap();
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
    fn malformed_outbox_shape_is_rejected_before_version_advance() {
        let (dir, journal) = open_tmp("malformed-outbox-shape");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_journal_write_gate(&conn);
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

        let seed = Connection::open(&path).unwrap();
        open_fixture_write_gate(&seed);
        seed.execute(
            "INSERT INTO slots (
                 slot_id, generation, phase, permit_held, routing_valid,
                 session_live, executor_proven, registered, pid, heartbeat_unix
             ) VALUES ('scope-extra', 1, 'provisioning', 0, 0, 0, 0, 0, NULL, 0)",
            [],
        )
        .unwrap();
        close_fixture_write_gate(&seed);
        seed.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(seed);

        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.materialized.replay.mismatch"
        );
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
        drop_journal_write_gate(&conn);
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
                    runner_request_id: None,
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(
            &mut journal,
            "worker-1",
            "worker-1",
            "worker-1",
            r#gen(),
            None,
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
        drop_journal_write_gate(&conn);
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
                    runner_request_id: String::new(),
                    permit_lease: None,
                    acquisition_loss_unproven: false,
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
                    runner_request_id: None,
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "job-1", generation, None);
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
                    runner_request_id: None,
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "job-1", r#gen(), None);
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: job_id.clone(),
                acquired_job_id: job_id.clone(),
                plan_id: "plan-1".into(),
                generation,
                runner_request_id: Some(job_id.0.clone()),
                permit_lease: None,
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "job-1", r#gen(), None);
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "job-1", r#gen(), None);
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "job-1", r#gen(), None);
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
    fn worker_lost_cannot_remove_a_provisional_acquisition() {
        let (dir, mut journal) = open_tmp("worker-lost-provisional");
        prime_ready(&mut journal, "scope-1");
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-1"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 41,
        };
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("request-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                    permit_lease: permit_lease.clone(),
                })
                .unwrap()
                .rejected
        );
        let before_events = event_count(&journal);

        let promoted = journal
            .apply(Event::JobOwned {
                job_id: job("request-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".to_owned(),
                accepted_unix: 1_001,
            })
            .unwrap();
        assert!(
            promoted.rejected,
            "a planless acquire intent is not ownership"
        );
        let started = journal
            .apply(Event::JobStarted {
                job_id: job("request-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(
            started.rejected,
            "an unresolved acquire intent cannot start"
        );
        assert_eq!(event_count(&journal), before_events);

        let lost = journal
            .apply(Event::JobWorkerLost {
                job_id: job("request-1"),
                generation: r#gen(),
            })
            .unwrap();

        assert!(lost.rejected);
        assert_eq!(event_count(&journal), before_events);
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs[0].provisional);
        assert_eq!(state.jobs[0].permit_lease, Some(permit_lease));
        assert!(!state.jobs[0].acquisition_loss_unproven);
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn acquisition_loss_proof_cannot_release_a_lease_claimed_by_two_rows() {
        let (dir, mut journal) = open_tmp("acquisition-loss-duplicate-lease-proof");
        prime_ready(&mut journal, "scope-1");
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-1"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 41,
        };
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("request-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                    permit_lease: permit_lease.clone(),
                })
                .unwrap()
                .rejected
        );

        let mut state = journal.materialized_state().unwrap();
        let mut duplicate = state.jobs[0].clone();
        duplicate.job_id = job("request-duplicate");
        duplicate.runner_request_id = "request-duplicate".to_owned();
        duplicate.slot_id = slot("scope-duplicate");
        state.jobs.push(duplicate);
        let outcome = reduce(
            state,
            Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation: r#gen(),
                reason: "renewjob says not ours".to_owned(),
                proof: Some(AcquisitionLossProof {
                    source: AcquisitionLossSource::RenewJobNotOurs,
                    permit_lease: permit_lease.clone(),
                }),
            },
        );
        assert!(outcome.rejected);
        assert_eq!(outcome.state.jobs.len(), 2);
        assert!(outcome.state.jobs[0].acquisition_loss_unproven);
        assert!(outcome
            .state
            .jobs
            .iter()
            .all(|job| job.permit_lease.as_ref() == Some(&permit_lease)));
        assert_eq!(outcome.state.slots[0].phase, SlotPhase2::Assigned);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        resolve_test_acquisition(&mut journal, "job-1", "job-1", "job-1", r#gen(), None);
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
    fn negative_user_version_is_not_treated_as_unversioned() {
        let (dir, journal) = open_tmp("negative-user-version");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 2);

        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", -1i64).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.version.invalid");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!wal.exists(), "negative version must fail before WAL setup");
        assert!(!shm.exists(), "negative version must fail before WAL setup");

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, -1);
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mixed_case_events_table_is_rejected_before_wal_setup() {
        let (dir, journal) = open_tmp("mixed-case-events-checksum");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 2);

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE events;
             CREATE TABLE EVENTS (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 generation INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 checksum TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO EVENTS (generation, kind, payload, checksum)
             VALUES (0, 'control_live', '{\"type\":\"control_live\"}', 'stale-checksum')",
            [],
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "case-insensitive event lookup must run pre-WAL"
        );
        assert!(
            !shm.exists(),
            "case-insensitive event lookup must run pre-WAL"
        );

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mixed_case_event_trigger_target_is_seen_before_wal_setup() {
        let (dir, mut journal) = open_tmp("mixed-case-events-trigger");
        assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.execute_batch(
            "DROP TABLE events;
             CREATE TABLE EVENTS (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 generation INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 checksum TEXT NOT NULL
             );
             CREATE TRIGGER rogue_uppercase_events_delete
             AFTER DELETE ON EVENTS BEGIN SELECT 1; END;
             PRAGMA user_version = 11;",
        )
        .unwrap();
        let trigger_table: String = conn
            .query_row(
                "SELECT tbl_name FROM sqlite_master
                 WHERE type = 'trigger' AND name = 'rogue_uppercase_events_delete'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trigger_table, "EVENTS");
        let payload = r#"{"type":"control_live"}"#;
        conn.execute(
            "INSERT INTO EVENTS (generation, kind, payload, checksum) VALUES (0, 'control_live', ?1, ?2)",
            params![payload, sha256_hex(payload.as_bytes())],
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "case-insensitive trigger target must fail pre-WAL"
        );
        assert!(
            !shm.exists(),
            "case-insensitive trigger target must fail pre-WAL"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_event_kind_is_a_hard_error_not_a_skip() {
        let (dir, mut journal) = open_tmp("unk");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);
        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        let payload = r#"{"type":"future_envelope","x":1}"#;
        open_fixture_write_gate(&conn);
        conn.execute(
            "INSERT INTO events (generation, kind, payload, checksum) VALUES (0, 'future_envelope', ?1, ?2)",
            params![payload, ""],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        let checksum = event_row_checksum(id, 0, "future_envelope", payload, 0).unwrap();
        conn.execute(
            "UPDATE events SET checksum = ?1 WHERE id = ?2",
            params![checksum, id],
        )
        .unwrap();
        refresh_fixture_replay_baseline_tail(&conn);
        close_fixture_write_gate(&conn);
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
                    runner_request_id: None,
                    run_service_url: "https://run.example/run".into(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(&mut journal, "guid-1", "guid-1", "guid-1", r#gen(), None);
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        resolve_test_acquisition(&mut journal, "guid-1", "guid-1", "guid-1", r#gen(), None);
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        resolve_test_acquisition(&mut journal, "guid-1", "guid-1", "guid-1", r#gen(), None);
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

            if stamped == 2 {
                seed_v2_outbox(&path, 2);
            } else {
                // Build a real historical table shape instead of rewinding a
                // current schema stamp, which should be refused as ambiguous.
                let conn = Connection::open(&path).unwrap();
                drop_jobs_columns_added_after_version(&conn, stamped);
                conn.execute("DELETE FROM meta", []).unwrap();
                if stamped == 3 {
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
                         );",
                    )
                    .unwrap();
                }
                conn.pragma_update(None, "user_version", stamped).unwrap();
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
                    .unwrap();
            }

            // Assert the fixture is physically historical before opening it.
            // A current jobs shape under an old stamp is a downgrade-shaped
            // hybrid, not migration coverage.
            let conn = Connection::open(&path).unwrap();
            let expected_jobs_version = match stamped {
                2 | 3 => 3,
                4 => 4,
                5 => 5,
                6 => 6,
                _ => unreachable!(),
            };
            assert!(
                jobs_shape_matches_version(&conn, expected_jobs_version).unwrap(),
                "v{stamped} fixture must have its historical jobs columns"
            );
            let expected_outbox = match stamped {
                2 => OutboxSchema::V2,
                3 => OutboxSchema::V3,
                _ => OutboxSchema::V4,
            };
            assert_eq!(
                outbox_schema_shape(&conn).unwrap(),
                expected_outbox,
                "v{stamped} fixture must have its historical outbox columns"
            );
            drop(conn);

            let journal = Journal::open(&path)
                .unwrap_or_else(|error| panic!("v{stamped} upgrade failed: {error:?}"));
            journal
                .load_state()
                .unwrap_or_else(|error| panic!("v{stamped} upgrade cannot materialize: {error:?}"));
            journal.materialized_state().unwrap_or_else(|error| {
                panic!("v{stamped} upgrade cannot read materialization: {error:?}")
            });
            let version: i64 = journal
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                u32::try_from(version).unwrap(),
                JOURNAL_SCHEMA_VERSION,
                "v{stamped} upgrade must land on the current version"
            );
            if stamped == 2 {
                assert_eq!(journal.materialized_state().unwrap().outbox.len(), 1);
            }
            drop(journal);
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
        drop_jobs_columns_added_after_version(&conn, 5);
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
            JOURNAL_SCHEMA_VERSION, 14,
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
        drop_jobs_columns(&conn, drop_columns);
        drop_jobs_columns_added_after_version(&conn, version);
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
                runner_request_id: None,
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
                runner_request_id: Some("request-1".into()),
                permit_lease: None,
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

    #[test]
    fn native_acquisition_retarget_requires_the_same_exact_permit_lease() {
        let (dir, mut journal) = open_tmp("acquisition-retarget-permit-identity");
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
        let lease = NativePermitLease {
            holder: "native/v1/scope/request-1".to_owned(),
            ledger_path: "/var/lib/velnor/permit-ledger.db".to_owned(),
            generation: 31,
        };
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("request-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                    permit_lease: lease.clone(),
                })
                .unwrap()
                .rejected
        );

        let mut wrong_lease = lease.clone();
        wrong_lease.generation += 1;
        let mismatched = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: "plan-1".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: Some(wrong_lease),
            })
            .unwrap();
        assert!(mismatched.rejected);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs[0].job_id, job("request-1"));
        assert!(state.jobs[0].plan_id.is_empty());
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&lease));

        let matched = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: "plan-1".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: Some(lease.clone()),
            })
            .unwrap();
        assert!(!matched.rejected);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs[0].job_id, job("run-service-job-1"));
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&lease));
        std::fs::remove_dir_all(dir).ok();
    }

    /// Retargeting is scoped to provisional rows and to free identities.
    /// Rewriting an owned row's identity would move a job that may already
    /// carry a terminal result or an outbox payload.
    #[test]
    fn only_a_provisional_row_may_be_retargeted_onto_a_free_identity() {
        let (dir, mut journal) = open_tmp("acquisition-retarget-refusals");
        prime_provisional(&mut journal, "scope-1", "request-1");

        // A planless row already owns this request identity. Resolving it
        // under a different request must not overwrite the permit mapping.
        let mismatched_request = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("request-1"),
                plan_id: "plan-1".into(),
                generation: r#gen(),
                runner_request_id: Some("request-2".into()),
                permit_lease: None,
            })
            .unwrap();
        assert!(mismatched_request.rejected);
        let untouched = journal.load_state().unwrap();
        assert_eq!(untouched.jobs[0].runner_request_id, "request-1");
        assert!(untouched.jobs[0].plan_id.is_empty());

        // Wrong generation.
        assert!(
            journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id: job("request-1"),
                    acquired_job_id: job("run-service-job-1"),
                    plan_id: "plan-1".into(),
                    generation: Generation(r#gen().0 + 1),
                    runner_request_id: Some("request-1".into()),
                    permit_lease: None,
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
                    runner_request_id: Some("request-absent".into()),
                    permit_lease: None,
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
                runner_request_id: Some("request-1".into()),
                permit_lease: None,
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
                    runner_request_id: Some("request-1".into()),
                    permit_lease: None,
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

    #[test]
    fn ambiguous_legacy_correlation_cannot_be_filled_by_a_later_resolve() {
        let (dir, mut journal) = open_tmp("ambiguous-legacy-correlation");
        prime_provisional(&mut journal, "scope-1", "request-1");

        // Historical self-to-self resolve events did not carry the request
        // identity. Replay must retain it as unknown instead of accepting the
        // old empty-plan compatibility behavior or guessing from the acquired
        // job ID.
        let legacy = reduce(
            journal.materialized_state().unwrap(),
            Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("request-1"),
                plan_id: String::new(),
                generation: r#gen(),
                runner_request_id: None,
                permit_lease: None,
            },
        );
        assert!(legacy.rejected);
        assert!(legacy.state.jobs[0].acquisition_loss_unproven);
        assert_eq!(legacy.state.jobs[0].runner_request_id, "");

        let guessed = reduce(
            legacy.state,
            Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("request-1"),
                plan_id: "plan-1".into(),
                generation: r#gen(),
                runner_request_id: Some("request-1".into()),
                permit_lease: None,
            },
        );
        assert!(guessed.rejected);
        let state = guessed.state;
        assert_eq!(state.jobs[0].runner_request_id, "");
        assert_eq!(state.jobs[0].plan_id, "");
        assert!(state.jobs[0].acquisition_loss_unproven);
        assert!(
            reduce(
                state,
                Event::JobStarted {
                    job_id: job("request-1"),
                    generation: r#gen(),
                },
            )
            .rejected
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

        // Exhausting a local probe budget stops future renewals; it is not
        // remote terminal evidence and cannot free this slot or its permit.
        let lost = journal
            .apply(Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation: r#gen(),
                reason: "probe budget spent".into(),
                proof: None,
            })
            .unwrap();
        assert!(lost.rejected);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].probe_attempts, MAX_ACQUISITION_PROBES);
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A probe charge is refused for a row that is not provisional: an owned
    /// job is never probed, and letting the counter move there would let a
    /// caller invent a budget for something the oracle already settled.
    #[test]
    fn a_probe_failure_is_refused_for_a_row_that_is_not_provisional() {
        let (dir, mut journal) = open_tmp("acquisition-probe-owned");
        prime_provisional(&mut journal, "scope-1", "request-1");
        resolve_test_acquisition(
            &mut journal,
            "request-1",
            "request-1",
            "request-1",
            r#gen(),
            None,
        );
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
                runner_request_id: None,
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

        // A run-service 200 records its exact plan/request identity before the
        // acquisition row may become an owned worker.
        let resolved = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("guid-1"),
                acquired_job_id: job("guid-1"),
                plan_id: "plan-1".into(),
                generation: r#gen(),
                runner_request_id: Some("guid-1".into()),
                permit_lease: None,
            })
            .unwrap();
        assert!(!resolved.rejected);
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

    /// Only typed run-service evidence plus a matching released permit lease
    /// can drop a provisional row. Neither arbitrary reasons nor a valid proof
    /// can forget an already-owned row.
    #[test]
    fn acquisition_loss_requires_typed_proof_for_the_exact_permit_lease() {
        let (dir, mut journal) = open_tmp("provisional-loss");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: r#gen(),
            })
            .unwrap();
        let permit_lease = NativePermitLease {
            holder:
                "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/guid-1"
                    .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 41,
        };
        journal
            .apply(Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: r#gen(),
                message_id: "msg-1".into(),
                runner_request_id: "guid-1".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
                permit_lease: permit_lease.clone(),
            })
            .unwrap();
        let lost = journal
            .apply(Event::JobAcquisitionLost {
                job_id: job("guid-1"),
                generation: r#gen(),
                reason: "another runner holds the lease".into(),
                proof: None,
            })
            .unwrap();
        assert!(lost.rejected, "a reason string alone is not terminal proof");
        assert_eq!(journal.load_state().unwrap().jobs.len(), 1);

        let wrong_lease = NativePermitLease {
            generation: permit_lease.generation + 1,
            ..permit_lease.clone()
        };
        let mismatched = journal
            .apply(Event::JobAcquisitionLost {
                job_id: job("guid-1"),
                generation: r#gen(),
                reason: "another runner holds the lease".into(),
                proof: Some(AcquisitionLossProof {
                    source: AcquisitionLossSource::RenewJobNotOurs,
                    permit_lease: wrong_lease,
                }),
            })
            .unwrap();
        assert!(
            mismatched.rejected,
            "a different lease cannot free this row"
        );

        let lost = journal
            .apply(Event::JobAcquisitionLost {
                job_id: job("guid-1"),
                generation: r#gen(),
                reason: "renewjob proved this runner does not own the job".into(),
                proof: Some(AcquisitionLossProof {
                    source: AcquisitionLossSource::RenewJobNotOurs,
                    permit_lease: permit_lease.clone(),
                }),
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
        let second_permit_lease = NativePermitLease {
            holder:
                "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/guid-2"
                    .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 42,
        };
        let reacquire = journal
            .apply(Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot("scope-1"),
                job_id: job("guid-2"),
                generation: r#gen(),
                message_id: "msg-2".into(),
                runner_request_id: "guid-2".into(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
                permit_lease: second_permit_lease.clone(),
            })
            .unwrap();
        assert!(!reacquire.rejected);
        resolve_test_acquisition(
            &mut journal,
            "guid-2",
            "guid-2",
            "guid-2",
            r#gen(),
            Some(second_permit_lease.clone()),
        );
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
                proof: Some(AcquisitionLossProof {
                    source: AcquisitionLossSource::RenewJobNotOurs,
                    permit_lease: second_permit_lease,
                }),
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        resolve_test_acquisition(&mut journal, "guid-1", "guid-1", "guid-1", r#gen(), None);
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
                runner_request_id: None,
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
                runner_request_id: None,
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
            })
            .unwrap();
        assert!(second.rejected);
    }

    #[test]
    fn acquisition_intents_reject_empty_urls_and_cross_slot_identity_collisions() {
        let (dir, mut journal) = open_tmp("acquisition-identity-collisions");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 3 })
                .unwrap()
                .rejected
        );
        prime_ready_in_existing_capacity(&mut journal, "scope-2");
        prime_ready_in_existing_capacity(&mut journal, "scope-3");

        let empty_url = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("bad-url-job"),
                generation: r#gen(),
                message_id: "message-bad-url".to_owned(),
                runner_request_id: Some("request-bad-url".to_owned()),
                run_service_url: String::new(),
                intended_unix: 1_000,
            })
            .unwrap();
        assert!(empty_url.rejected);

        let lease = NativePermitLease {
            holder: "native/v1/scope/request-1".to_owned(),
            ledger_path: "/var/lib/velnor/permit-ledger.db".to_owned(),
            generation: 31,
        };
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("job-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_001,
                    permit_lease: lease.clone(),
                })
                .unwrap()
                .rejected
        );

        let duplicate_request = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-2"),
                job_id: job("job-2"),
                generation: r#gen(),
                message_id: "message-2".to_owned(),
                runner_request_id: Some("request-1".to_owned()),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_002,
            })
            .unwrap();
        assert!(duplicate_request.rejected);

        let duplicate_job = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-2"),
                job_id: job("job-1"),
                generation: r#gen(),
                message_id: "message-duplicate-job".to_owned(),
                runner_request_id: Some("request-distinct".to_owned()),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_003,
            })
            .unwrap();
        assert!(duplicate_job.rejected);

        let duplicate_lease = journal
            .apply(Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot("scope-3"),
                job_id: job("job-3"),
                generation: r#gen(),
                message_id: "message-3".to_owned(),
                runner_request_id: "request-3".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_003,
                permit_lease: lease,
            })
            .unwrap();
        assert!(duplicate_lease.rejected);

        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("job-1"));
        assert!(state
            .slots
            .iter()
            .filter(|record| record.slot_id != slot("scope-1"))
            .all(|slot| slot.phase == SlotPhase2::Ready));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn job_owned_must_match_its_source_slot_and_generation() {
        let (dir, mut journal) = open_tmp("job-owned-source-identity");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        prime_ready_in_existing_capacity(&mut journal, "scope-2");
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-1"),
                    job_id: job("request-1"),
                    generation: r#gen(),
                    message_id: "message-request-1".to_owned(),
                    runner_request_id: Some("request-1".to_owned()),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(
            &mut journal,
            "request-1",
            "request-1",
            "request-1",
            r#gen(),
            None,
        );

        let wrong_slot = journal
            .apply(Event::JobOwned {
                job_id: job("request-1"),
                slot_id: slot("scope-2"),
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".to_owned(),
                accepted_unix: 1_001,
            })
            .unwrap();
        assert!(wrong_slot.rejected);

        let wrong_generation = journal
            .apply(Event::JobOwned {
                job_id: job("request-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: Generation(r#gen().0 + 1),
                worker: "worker-1".to_owned(),
                accepted_unix: 1_002,
            })
            .unwrap();
        assert!(wrong_generation.rejected);

        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs[0].provisional);
        assert_eq!(state.slots.len(), 2);
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        assert_eq!(state.slots[1].phase, SlotPhase2::Ready);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rebuilt_acquisition_rejects_a_duplicate_permit_lease() {
        let (dir, mut journal) = open_tmp("rebuilt-acquisition-lease-collision");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 4 })
                .unwrap()
                .rejected
        );
        for scope in ["scope-2", "scope-3", "scope-4"] {
            prime_ready_in_existing_capacity(&mut journal, scope);
        }
        let lease = NativePermitLease {
            holder: "native/v1/scope/request-1".to_owned(),
            ledger_path: "/var/lib/velnor/permit-ledger.db".to_owned(),
            generation: 31,
        };

        assert!(
            !journal
                .apply(Event::JobAcquisitionRebuiltWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("job-1"),
                    generation: r#gen(),
                    runner_request_id: "request-1".to_owned(),
                    plan_id: "plan-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    probe_deadline_unix: 2_000,
                    permit_lease: lease.clone(),
                })
                .unwrap()
                .rejected
        );

        // The runner correlates a job against either durable identity. A new
        // job ID may not alias another holder's request ID, and a new request
        // ID may not alias another holder's acquired job ID.
        for (job_id, runner_request_id) in [("request-1", "request-2"), ("job-2", "job-1")] {
            let alias = journal
                .apply(Event::JobAcquisitionRebuilt {
                    slot_id: slot("scope-2"),
                    job_id: job(job_id),
                    generation: r#gen(),
                    runner_request_id: runner_request_id.to_owned(),
                    plan_id: "plan-alias".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    probe_deadline_unix: 2_001,
                })
                .unwrap();
            assert!(
                alias.rejected,
                "identity alias {job_id}/{runner_request_id}"
            );
        }

        let duplicate_job_id = journal
            .apply(Event::JobAcquisitionRebuilt {
                slot_id: slot("scope-2"),
                job_id: job("job-1"),
                generation: r#gen(),
                runner_request_id: "request-2".to_owned(),
                plan_id: "plan-2".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                probe_deadline_unix: 2_001,
            })
            .unwrap();
        assert!(duplicate_job_id.rejected);

        let duplicate_request_id = journal
            .apply(Event::JobAcquisitionRebuilt {
                slot_id: slot("scope-3"),
                job_id: job("job-3"),
                generation: r#gen(),
                runner_request_id: "request-1".to_owned(),
                plan_id: "plan-3".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                probe_deadline_unix: 2_002,
            })
            .unwrap();
        assert!(duplicate_request_id.rejected);

        let duplicate = journal
            .apply(Event::JobAcquisitionRebuiltWithPermit {
                slot_id: slot("scope-4"),
                job_id: job("job-4"),
                generation: r#gen(),
                runner_request_id: "request-4".to_owned(),
                plan_id: "plan-4".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                probe_deadline_unix: 2_003,
                permit_lease: lease,
            })
            .unwrap();
        assert!(duplicate.rejected);
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);

        let mut cross_role_aliases = Vec::new();
        for (job_id, runner_request_id) in [("job-2", "job-1"), ("request-1", "request-2")] {
            let mut alias = state.jobs[0].clone();
            alias.job_id = job(job_id);
            alias.runner_request_id = runner_request_id.to_owned();
            alias.permit_lease = None;
            cross_role_aliases.push(alias);
        }
        for alias in cross_role_aliases {
            let mut jobs = state.jobs.clone();
            jobs.push(alias);
            assert_eq!(
                validate_job_identity_uniqueness(&jobs)
                    .unwrap_err()
                    .envelope
                    .reason,
                "journal.materialized.invalid",
                "materialized validation rejects cross-role identity aliases"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolved_acquisition_rejects_empty_plan_and_identity_aliases() {
        let (dir, mut journal) = open_tmp("resolved-acquisition-identity-aliases");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 3 })
                .unwrap()
                .rejected
        );
        prime_ready_in_existing_capacity(&mut journal, "scope-1");
        prime_ready_in_existing_capacity(&mut journal, "scope-2");
        prime_ready_in_existing_capacity(&mut journal, "scope-3");
        for (scope, request) in [("scope-1", "request-1"), ("scope-2", "request-2")] {
            assert!(
                !journal
                    .apply(Event::JobAcquisitionIntended {
                        slot_id: slot(scope),
                        job_id: job(request),
                        generation: r#gen(),
                        message_id: format!("message-{request}"),
                        runner_request_id: Some(request.to_owned()),
                        run_service_url: "https://run.example/run".to_owned(),
                        intended_unix: 1_000,
                    })
                    .unwrap()
                    .rejected
            );
        }

        let empty_plan = reduce(
            journal.materialized_state().unwrap(),
            Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: String::new(),
                generation: r#gen(),
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: None,
            },
        );
        assert!(empty_plan.rejected);
        assert!(empty_plan.state.jobs[0].acquisition_loss_unproven);
        assert_eq!(empty_plan.state.jobs[0].job_id, job("request-1"));

        let first_resolve = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: "plan-1".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: None,
            })
            .unwrap();
        assert!(!first_resolve.rejected);

        let request_id_alias = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-3"),
                job_id: job("new-request"),
                generation: r#gen(),
                message_id: "message-new-request".to_owned(),
                runner_request_id: Some("run-service-job-1".to_owned()),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_001,
            })
            .unwrap();
        assert!(
            request_id_alias.rejected,
            "a new request ID cannot retarget an acquired job ID"
        );

        let acquired_id_alias = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-2"),
                acquired_job_id: job("request-1"),
                plan_id: "plan-2".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-2".to_owned()),
                permit_lease: None,
            })
            .unwrap();
        assert!(
            acquired_id_alias.rejected,
            "an acquired job ID cannot retarget another request ID"
        );

        let duplicate_job_id = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-2"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: "plan-2".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-2".to_owned()),
                permit_lease: None,
            })
            .unwrap();
        assert!(duplicate_job_id.rejected);

        let duplicate_request_id = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: job("request-2"),
                acquired_job_id: job("run-service-job-2"),
                plan_id: "plan-2".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: None,
            })
            .unwrap();
        assert!(duplicate_request_id.rejected);
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 2);
        assert_eq!(state.jobs[0].job_id, job("run-service-job-1"));
        assert_eq!(state.jobs[1].job_id, job("request-2"));
        assert_eq!(state.jobs[1].runner_request_id, "request-2");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_request_identity_blocks_resolving_another_slot() {
        let (dir, mut journal) = open_tmp("unknown-request-blocks-resolve");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        prime_ready_in_existing_capacity(&mut journal, "scope-1");
        prime_ready_in_existing_capacity(&mut journal, "scope-2");
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-2"),
                    job_id: job("request-2"),
                    generation: r#gen(),
                    message_id: "message-2".to_owned(),
                    runner_request_id: Some("request-2".to_owned()),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );

        let mut state = journal.materialized_state().unwrap();
        state.slots[0].phase = SlotPhase2::Assigned;
        state.jobs.push(JobRecord {
            job_id: job("legacy-unknown-job"),
            slot_id: slot("scope-1"),
            generation: r#gen(),
            attempt: 1,
            worker: "legacy-worker".to_owned(),
            phase: JobPhase2::Assigned,
            accepted_unix: 0,
            terminal_conclusion: None,
            provisional: false,
            plan_id: String::new(),
            run_service_url: String::new(),
            runner_request_id: String::new(),
            permit_lease: None,
            acquisition_loss_unproven: true,
            probe_attempts: 0,
            probe_deadline_unix: 0,
        });

        let outcome = reduce(
            state,
            Event::JobAcquisitionResolved {
                provisional_job_id: job("request-2"),
                acquired_job_id: job("run-service-job-2"),
                plan_id: "plan-2".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-2".to_owned()),
                permit_lease: None,
            },
        );
        assert!(outcome.rejected);
        assert_eq!(outcome.state.jobs.len(), 2);
        let provisional = outcome
            .state
            .jobs
            .iter()
            .find(|row| row.job_id == job("request-2"))
            .unwrap();
        assert!(provisional.provisional);
        assert!(provisional.acquisition_loss_unproven);
        assert_eq!(provisional.runner_request_id, "request-2");
        assert!(outcome.state.jobs.iter().any(|row| {
            row.job_id == job("legacy-unknown-job") && row.runner_request_id.is_empty()
        }));
        assert_eq!(outcome.state.slots[1].phase, SlotPhase2::Assigned);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolved_acquisition_rejects_a_permit_lease_already_claimed_elsewhere() {
        let (dir, mut journal) = open_tmp("resolved-acquisition-lease-collision");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        prime_ready_in_existing_capacity(&mut journal, "scope-1");
        prime_ready_in_existing_capacity(&mut journal, "scope-2");
        for (scope, request_id) in [("scope-1", "request-1"), ("scope-2", "request-2")] {
            assert!(
                !journal
                    .apply(Event::JobAcquisitionIntended {
                        slot_id: slot(scope),
                        job_id: job(request_id),
                        generation: r#gen(),
                        message_id: format!("message-{request_id}"),
                        runner_request_id: Some(request_id.to_owned()),
                        run_service_url: "https://run.example/run".to_owned(),
                        intended_unix: 1_000,
                    })
                    .unwrap()
                    .rejected
            );
        }
        let lease = NativePermitLease {
            holder: "native/v1/scope/request-1".to_owned(),
            ledger_path: "/var/lib/velnor/permit-ledger.db".to_owned(),
            generation: 31,
        };
        let mut state = journal.materialized_state().unwrap();
        state.jobs[0].permit_lease = Some(lease.clone());
        state.jobs[1].permit_lease = Some(lease.clone());

        let outcome = reduce(
            state,
            Event::JobAcquisitionResolved {
                provisional_job_id: job("request-2"),
                acquired_job_id: job("run-service-job-2"),
                plan_id: "plan-2".to_owned(),
                generation: r#gen(),
                runner_request_id: Some("request-2".to_owned()),
                permit_lease: Some(lease.clone()),
            },
        );
        assert!(outcome.rejected);
        assert_eq!(outcome.state.jobs.len(), 2);
        assert!(outcome.state.jobs.iter().all(|row| row.provisional));
        assert!(outcome
            .state
            .jobs
            .iter()
            .all(|row| row.permit_lease.as_ref() == Some(&lease)));
        assert_eq!(outcome.state.slots[0].phase, SlotPhase2::Assigned);
        assert_eq!(outcome.state.slots[1].phase, SlotPhase2::Assigned);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn materialized_jobs_reject_duplicate_request_ids_and_permit_leases() {
        for duplicate_request in [true, false] {
            let label = if duplicate_request {
                "materialized-duplicate-request"
            } else {
                "materialized-duplicate-permit"
            };
            let (dir, mut journal) = open_tmp(label);
            prime_ready(&mut journal, "scope-1");
            let lease = NativePermitLease {
                holder: "native/v1/scope/request-1".to_owned(),
                ledger_path: "/var/lib/velnor/permit-ledger.db".to_owned(),
                generation: 31,
            };
            assert!(
                !journal
                    .apply(Event::JobAcquisitionIntendedWithPermit {
                        slot_id: slot("scope-1"),
                        job_id: job("job-1"),
                        generation: r#gen(),
                        message_id: "message-1".to_owned(),
                        runner_request_id: "request-1".to_owned(),
                        run_service_url: "https://run.example/run".to_owned(),
                        intended_unix: 1_000,
                        permit_lease: lease.clone(),
                    })
                    .unwrap()
                    .rejected
            );
            let duplicate_lease = if duplicate_request {
                None
            } else {
                Some(serde_json::to_string(&lease).unwrap())
            };
            open_fixture_write_gate(&journal.conn);
            journal
                .conn
                .execute(
                    "INSERT INTO jobs (
                         job_id, slot_id, generation, attempt, worker, phase,
                         accepted_unix, terminal_conclusion, provisional,
                         plan_id, run_service_url, probe_attempts,
                         probe_deadline_unix, runner_request_id, permit_lease,
                         acquisition_loss_unproven
                     ) VALUES (
                         'job-2', 'scope-1', 1, 0, '', 'assigned', 0,
                         NULL, 1, '', 'https://run.example/run', 0, 0,
                         ?1, ?2, 0
                     )",
                    params![
                        if duplicate_request {
                            "request-1"
                        } else {
                            "request-2"
                        },
                        duplicate_lease,
                    ],
                )
                .unwrap();
            close_fixture_write_gate(&journal.conn);

            let error = journal.materialized_state().unwrap_err();
            assert_eq!(error.envelope.reason, "journal.materialized.invalid");
            drop(journal);
            std::fs::remove_dir_all(dir).unwrap();
        }
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
                runner_request_id: None,
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
        drop_jobs_columns_added_after_version(&conn, 7);
        strip_v11_event_fields(&conn);
        conn.pragma_update(None, "user_version", 7u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let reopened = Journal::open(&path).unwrap();
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
    fn v7_stamp_with_v9_request_column_fails_without_mutation() {
        let (dir, journal) = open_tmp("v7-with-v9-request-column");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_journal_write_gate(&conn);
        drop_jobs_columns(&conn, &["permit_lease", "acquisition_loss_unproven"]);
        conn.pragma_update(None, "user_version", 7u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        drop(conn);

        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 7);
        assert!(table_has_column(&check, "jobs", "runner_request_id").unwrap());
        assert!(!table_has_column(&check, "jobs", "permit_lease").unwrap());
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v8_journal_migrates_active_acquisition_request_correlation() {
        let path = make_v8_acquisition_journal("v8-to-v9-correlation");
        let reopened = Journal::open(&path).unwrap();
        let version: i64 = reopened
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        let state = reopened.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("job-1"));
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert_eq!(state.jobs[0].plan_id, "plan-1");
        assert_eq!(state.jobs[0].phase, JobPhase2::Assigned);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v8_self_resolved_history_migrates_with_unknown_request_correlation() {
        let path = make_v8_self_resolved_journal("v8-self-resolved-unknown-correlation");
        let reopened = Journal::open(&path).unwrap();
        let state = reopened.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("job-1"));
        assert_eq!(state.jobs[0].runner_request_id, "");
        assert!(
            state.jobs[0].acquisition_loss_unproven,
            "old self-to-self history has no provable runner request identity"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v8_to_v9_refuses_valid_json_with_a_stale_checksum_before_schema_change() {
        let path = make_v8_acquisition_journal("v8-to-v9-checksum");
        let conn = Connection::open(&path).unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "delete");
        let (event_id, payload): (i64, String) = conn
            .query_row(
                "SELECT id, payload FROM events WHERE kind = 'job_acquisition_intended' LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let tampered = payload.replace("broker-message-1", "broker-message-2");
        assert_ne!(payload, tampered);
        conn.execute(
            "UPDATE events SET payload = ?1 WHERE id = ?2",
            params![tampered, event_id],
        )
        .unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.checksum.mismatch");
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 8);
        assert!(!table_has_column(&conn, "jobs", "runner_request_id").unwrap());
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            journal_mode, "delete",
            "failed migration must not mutate a non-WAL journal before checksum validation"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v9_to_v10_refuses_a_stale_checksum_before_vocabulary_stamp() {
        let (dir, mut journal) = open_tmp("v9-to-v10-checksum");
        journal.apply(Event::ControlLive).unwrap();
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 9);
        let (event_id, checksum): (i64, String) = conn
            .query_row(
                "SELECT id, checksum FROM events ORDER BY id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        conn.execute(
            "UPDATE events SET payload = '{\"ControlLive\":{}}' WHERE id = ?1",
            [event_id],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 9u32).unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.checksum.mismatch");
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 9, "failed v10 stamp must preserve v9");
        assert!(table_has_column(&conn, "jobs", "runner_request_id").unwrap());
        let preserved_checksum: String = conn
            .query_row(
                "SELECT checksum FROM events WHERE id = ?1",
                [event_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(preserved_checksum, checksum);
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v10_journal_migrates_historical_acquisition_events_to_v11() {
        let path = make_v10_acquisition_journal("v10-to-v11-acquisition");
        let old = Connection::open(&path).unwrap();
        let version: i64 = old
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 10);
        assert!(!table_has_column(&old, "jobs", "permit_lease").unwrap());
        drop(old);

        let migrated = Journal::open(&path).expect("valid v10 history must migrate");
        let version: i64 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        assert!(table_has_column(&migrated.conn, "jobs", "permit_lease").unwrap());
        let state = migrated.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 2);
        assert_eq!(state.jobs[0].job_id, job("job-1"));
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert_eq!(state.jobs[0].permit_lease, None);
        let rebuilt = state
            .jobs
            .iter()
            .find(|record| record.job_id == job("job-rebuilt-1"))
            .expect("the v10 rebuild event must survive migration");
        assert_eq!(rebuilt.runner_request_id, "request-rebuilt-1");
        assert_eq!(rebuilt.plan_id, "plan-rebuilt-1");
        assert_eq!(rebuilt.run_service_url, "https://run.example/rebuilt-run");
        assert_eq!(rebuilt.probe_deadline_unix, 22_000);
        drop(migrated);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v10_malformed_meta_is_refused_before_wal_or_schema_mutation() {
        assert_v10_materialized_edit_fails_before_wal(
            "v10-malformed-meta-prewal",
            "UPDATE meta SET value = 'not-a-bool' WHERE key = 'control_live';",
        );
    }

    #[test]
    fn v10_negative_materialized_generation_is_refused_before_wal_or_schema_mutation() {
        assert_v10_materialized_edit_fails_before_wal(
            "v10-negative-materialized-generation-prewal",
            "UPDATE jobs SET generation = -1 WHERE job_id = 'job-1';",
        );
    }

    #[test]
    fn v11_malformed_materialized_meta_is_refused_before_wal() {
        assert_v11_materialized_edit_fails_before_wal(
            "v11-malformed-meta-prewal",
            "UPDATE meta SET value = 'not-a-bool' WHERE key = 'control_live';",
        );
    }

    #[test]
    fn v11_malformed_permit_json_is_refused_before_wal() {
        assert_v11_materialized_edit_fails_before_wal(
            "v11-malformed-permit-json-prewal",
            "UPDATE jobs SET permit_lease = 'not-json' WHERE job_id = 'request-1';",
        );
    }

    #[test]
    fn v11_migration_replays_proofless_loss_and_retains_exact_permit() {
        let (path, permit_lease) = make_v11_proofless_loss_journal("v11-v12-proofless-loss-replay");
        let old = Connection::open(&path).unwrap();
        let old_version: i64 = old
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(old_version, 11);
        assert_eq!(
            old.query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "the v11 loss and worker teardown already erased its snapshot"
        );
        let proofless: String = old
            .query_row(
                "SELECT payload FROM events WHERE kind = 'job_acquisition_lost'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!serde_json::from_str::<serde_json::Value>(&proofless)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("proof"));
        drop(old);

        let mut migrated = Journal::open(&path).expect("valid v11 history must migrate");
        let version: i64 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);

        let state = migrated.materialized_state().unwrap();
        let mut replayed = migrated.load_state().unwrap();
        // Drain and admission fences are meta-only and intentionally survive
        // migration outside the event replay.
        replayed.drain_active = state.drain_active;
        replayed.drain_version = state.drain_version;
        replayed.admission_blocked = state.admission_blocked;
        replayed.admission_version = state.admission_version;
        assert_eq!(state, replayed);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("request-1"));
        assert!(state.jobs[0].provisional);
        assert!(state.jobs[0].acquisition_loss_unproven);
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert_eq!(state.jobs[0].permit_lease, Some(permit_lease.clone()));
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        assert!(state.drain_active);
        assert_eq!(state.drain_version, 7);
        assert!(state.admission_blocked);
        assert_eq!(state.admission_version, 9);

        let owned = migrated
            .apply(Event::JobOwned {
                job_id: job("request-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "stale-worker".to_owned(),
                accepted_unix: 1_100,
            })
            .unwrap();
        assert!(
            owned.rejected,
            "old ownership cannot launder loss ambiguity"
        );
        let worker_lost = migrated
            .apply(Event::JobWorkerLost {
                job_id: job("request-1"),
                generation: r#gen(),
            })
            .unwrap();
        assert!(
            worker_lost.rejected,
            "a provisional row is not a worker owner"
        );
        assert_eq!(
            migrated.materialized_state().unwrap().jobs[0].permit_lease,
            Some(permit_lease.clone())
        );

        let released = migrated
            .apply(Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation: r#gen(),
                reason: "renewjob returned typed not-owned evidence".to_owned(),
                proof: Some(AcquisitionLossProof {
                    source: AcquisitionLossSource::RenewJobNotOurs,
                    permit_lease,
                }),
            })
            .unwrap();
        assert!(!released.rejected);
        let final_state = migrated.materialized_state().unwrap();
        assert!(final_state.jobs.is_empty());
        assert_eq!(final_state.slots[0].phase, SlotPhase2::Ready);
        assert!(final_state.drain_active);
        assert!(final_state.admission_blocked);
        drop(migrated);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v11_pending_outbox_migration_preserves_snapshot_timestamps_and_events() {
        let (dir, mut journal) = open_tmp("v11-pending-outbox-timestamps");
        prime_ready(&mut journal, "scope-1");
        let generation = r#gen();
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-1"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 71,
        };
        for event in [
            Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation,
            },
            Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot("scope-1"),
                job_id: job("request-1"),
                generation,
                message_id: "message-1".to_owned(),
                runner_request_id: "request-1".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_000,
                permit_lease: permit_lease.clone(),
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        resolve_test_acquisition(
            &mut journal,
            "request-1",
            "job-1",
            "request-1",
            generation,
            Some(permit_lease),
        );
        for event in [
            Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_001,
            },
            Event::JobStarted {
                job_id: job("job-1"),
                generation,
            },
            Event::JobTerminalResult {
                job_id: job("job-1"),
                generation,
                conclusion: "success".to_owned(),
            },
            Event::CompletionIntended {
                job_id: job("job-1"),
                generation,
                payload_sha256: payload_checksum(b"terminal"),
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let before_outbox = outbox_row(&journal, "job-1").unwrap();
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        let before_events = {
            let mut statement = conn
                .prepare("SELECT id, generation, kind, payload FROM events ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let migrated = Journal::open(&path).expect("consistent v11 pending outbox migrates");
        let after_outbox = outbox_row(&migrated, "job-1").unwrap();
        assert_eq!(after_outbox, before_outbox);
        let after_events = {
            let mut statement = migrated
                .conn
                .prepare("SELECT id, generation, kind, payload FROM events ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(after_events, before_events);
        drop(migrated);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v11_timeout_abandonment_migrates_with_stable_recorded_deadline() {
        let (dir, mut journal) = open_tmp("v11-timeout-abandonment-timestamps");
        prime_ready(&mut journal, "scope-1");
        let generation = r#gen();
        for event in [
            Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation,
            },
            Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("request-1"),
                generation,
                message_id: "message-1".to_owned(),
                runner_request_id: Some("request-1".to_owned()),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_000,
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("job-1"),
                plan_id: "plan-1".to_owned(),
                generation,
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: None,
            },
            Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation,
                worker: "worker-1".to_owned(),
                accepted_unix: 1_001,
            },
            Event::JobStarted {
                job_id: job("job-1"),
                generation,
            },
            Event::JobTerminalResult {
                job_id: job("job-1"),
                generation,
                conclusion: "success".to_owned(),
            },
            Event::CompletionIntended {
                job_id: job("job-1"),
                generation,
                payload_sha256: payload_checksum(b"terminal"),
            },
            Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        open_fixture_write_gate(&journal.conn);
        journal
            .conn
            .execute(
                "UPDATE outbox SET deadline_unix = 1 WHERE job_id = 'job-1'",
                [],
            )
            .unwrap();
        close_fixture_write_gate(&journal.conn);
        let abandoned = journal
            .apply(Event::CompletionUnresolvable {
                job_id: job("job-1"),
                generation,
                reason: "completion deadline elapsed".to_owned(),
            })
            .unwrap();
        assert!(!abandoned.rejected);
        let before_state = journal.materialized_state().unwrap();
        assert!(before_state.jobs.is_empty());
        assert!(before_state.outbox.is_empty());
        assert_eq!(before_state.slots[0].phase, SlotPhase2::Ready);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 11);
        strip_v11_event_fields(&conn);
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        let before_events = {
            let mut statement = conn
                .prepare("SELECT id, generation, kind, payload FROM events ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let migrated = Journal::open(&path).expect("valid v11 timeout proof migrates");
        assert_eq!(migrated.materialized_state().unwrap(), before_state);
        let replayed = migrated.load_state().unwrap();
        assert!(replayed.jobs.is_empty());
        assert!(replayed.outbox[0].abandoned);
        assert!(!replayed.outbox[0].remote_acked);
        assert_eq!(replayed.outbox[0].created_unix, 1_001);
        assert_eq!(
            replayed.outbox[0].deadline_unix,
            1_001 + COMPLETION_RESOLUTION_SECONDS
        );
        let after_events = {
            let mut statement = migrated
                .conn
                .prepare("SELECT id, generation, kind, payload FROM events ORDER BY id")
                .unwrap();
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(
            after_events, before_events,
            "migration retains event identity"
        );

        let intended_time: i64 = migrated
            .conn
            .query_row(
                "SELECT recorded_unix FROM events WHERE kind = 'completion_intended'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let unresolvable_time: i64 = migrated
            .conn
            .query_row(
                "SELECT recorded_unix FROM events WHERE kind = 'completion_unresolvable'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(intended_time, 1_001);
        assert_eq!(
            unresolvable_time,
            i64::try_from(1_001 + COMPLETION_RESOLUTION_SECONDS).unwrap(),
            "legacy timeout replay uses the deterministic deadline derived from durable accepted time"
        );
        drop(migrated);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v13_migration_rederives_unchecked_time_and_v14_checksum_binds_it() {
        let (dir, mut journal) = open_tmp("v13-event-time-integrity");
        let generation = prime_running_job(&mut journal, "scope-1", "job-1");
        let completion = journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation,
                payload_sha256: payload_checksum(b"timestamp integrity"),
            })
            .unwrap();
        assert!(!completion.rejected);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        let (event_id, original_time): (i64, i64) = conn
            .query_row(
                "SELECT id, recorded_unix FROM events WHERE kind = 'completion_intended'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        drop_jobs_columns_added_after_version(&conn, 13);
        conn.pragma_update(None, "user_version", 13u32).unwrap();
        open_fixture_write_gate(&conn);
        conn.execute(
            "UPDATE events SET recorded_unix = ?1 WHERE id = ?2",
            params![original_time + 600, event_id],
        )
        .unwrap();
        close_fixture_write_gate(&conn);
        drop(conn);

        let migrated = Journal::open(&path).expect("v13 timestamps are rebuilt before v14");
        let migrated_time: i64 = migrated
            .conn
            .query_row(
                "SELECT recorded_unix FROM events WHERE id = ?1",
                [event_id],
                |row| row.get(0),
            )
            .unwrap();
        let (payload, checksum): (String, String) = migrated
            .conn
            .query_row(
                "SELECT payload, checksum FROM events WHERE id = ?1",
                [event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(migrated_time, original_time);
        let generation_sql: i64 = migrated
            .conn
            .query_row(
                "SELECT generation FROM events WHERE id = ?1",
                [event_id],
                |row| row.get(0),
            )
            .unwrap();
        let kind: String = migrated
            .conn
            .query_row("SELECT kind FROM events WHERE id = ?1", [event_id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            checksum,
            event_row_checksum(event_id, generation_sql, &kind, &payload, migrated_time).unwrap()
        );
        let version: i64 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 14);
        drop(migrated);

        let conn = Connection::open(&path).unwrap();
        open_fixture_write_gate(&conn);
        conn.execute(
            "UPDATE events SET recorded_unix = ?1 WHERE id = ?2",
            params![migrated_time + 1, event_id],
        )
        .unwrap();
        close_fixture_write_gate(&conn);
        drop(conn);
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.checksum.mismatch");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_event_checksum_binds_replay_order_and_row_metadata() {
        let edits = [
            (
                "id-reorder",
                "UPDATE events SET id = id + 100 WHERE id = (SELECT MIN(id) FROM events)",
            ),
            (
                "generation",
                "UPDATE events SET generation = generation + 1 WHERE id = (SELECT MIN(id) FROM events)",
            ),
            (
                "kind",
                "UPDATE events SET kind = 'tampered_kind' WHERE id = (SELECT MIN(id) FROM events)",
            ),
            (
                "recorded-time",
                "UPDATE events SET recorded_unix = recorded_unix + 1 WHERE id = (SELECT MIN(id) FROM events)",
            ),
            (
                "payload",
                "UPDATE events SET payload = payload || ' ' WHERE id = (SELECT MIN(id) FROM events)",
            ),
        ];

        for (label, edit) in edits {
            let (dir, mut journal) = open_tmp(&format!("event-checksum-{label}"));
            assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
            assert!(
                !journal
                    .apply(Event::Dependency {
                        github_reachable: true,
                    })
                    .unwrap()
                    .rejected
            );
            let path = dir.join("journal.db");
            drop(journal);

            let conn = Connection::open(&path).unwrap();
            open_fixture_write_gate(&conn);
            conn.execute_batch(edit).unwrap();
            close_fixture_write_gate(&conn);
            drop(conn);

            let error = Journal::open(&path).unwrap_err();
            assert_eq!(
                error.envelope.reason, "journal.checksum.mismatch",
                "{label} edits must invalidate journal integrity evidence"
            );
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn current_open_rejects_reducer_rejected_event_rows() {
        let (dir, journal) = open_tmp("current-replay-rejected-event");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        append_fixture_event(
            &conn,
            &Event::JobOwned {
                job_id: job("missing-job"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".to_owned(),
                accepted_unix: 1_000,
            },
        );
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.replay.rejected");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_open_rejects_materialized_slot_phase_drift_before_a_write() {
        let (dir, mut journal) = open_tmp("current-materialized-slot-drift");
        prime_ready(&mut journal, "scope-1");
        let acquired = journal
            .apply(Event::JobAcquisitionIntended {
                slot_id: slot("scope-1"),
                job_id: job("request-1"),
                generation: r#gen(),
                message_id: "message-1".to_owned(),
                runner_request_id: Some("request-1".to_owned()),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 1_000,
            })
            .unwrap();
        assert!(!acquired.rejected);
        assert_eq!(
            journal.materialized_state().unwrap().slots[0].phase,
            SlotPhase2::Assigned
        );
        let path = dir.join("journal.db");
        let original_event_count = event_count(&journal);
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        open_fixture_write_gate(&conn);
        conn.execute(
            "UPDATE slots SET phase = 'ready' WHERE slot_id = 'scope-1'",
            [],
        )
        .unwrap();
        close_fixture_write_gate(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.materialized.replay.mismatch"
        );
        let conn = Connection::open(&path).unwrap();
        let retained_event_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        let retained_phase: String = conn
            .query_row(
                "SELECT phase FROM slots WHERE slot_id = 'scope-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained_event_count, original_event_count);
        assert_eq!(retained_phase, "ready");
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_open_rejects_deleted_event_when_materialized_snapshot_keeps_its_effect() {
        let (dir, mut journal) = open_tmp("current-deleted-event-drift");
        assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
        assert!(
            !journal
                .apply(Event::Dependency {
                    github_reachable: true,
                })
                .unwrap()
                .rejected
        );
        assert!(journal.materialized_state().unwrap().github_reachable);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        open_fixture_write_gate(&conn);
        conn.execute("DELETE FROM events WHERE kind = 'dependency'", [])
            .unwrap();
        close_fixture_write_gate(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_v14_open_requires_a_committed_event_tail_watermark() {
        let (dir, journal) = open_tmp("current-v14-missing-tail-watermark");
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        open_fixture_write_gate(&conn);
        assert_eq!(
            conn.execute("DELETE FROM meta WHERE key = 'replay_baseline'", [],)
                .unwrap(),
            1
        );
        close_fixture_write_gate(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_writer_cannot_commit_after_its_tail_watermark_disappears() {
        let (dir, mut journal) = open_tmp("current-v14-writer-missing-tail-watermark");
        let path = dir.join("journal.db");
        let before_state = journal.materialized_state().unwrap();
        let before_events = event_count(&journal);

        // A handle can outlive another SQLite writer. Removing its anchor
        // after open must fail before materialized rows or events commit.
        let conn = Connection::open(&path).unwrap();
        open_fixture_write_gate(&conn);
        assert_eq!(
            conn.execute("DELETE FROM meta WHERE key = 'replay_baseline'", [],)
                .unwrap(),
            1
        );
        close_fixture_write_gate(&conn);
        drop(conn);

        let error = journal.apply(Event::ControlLive).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
        assert_eq!(event_count(&journal), before_events);
        assert_eq!(journal.materialized_state().unwrap(), before_state);
        assert_eq!(
            journal
                .conn
                .query_row("SELECT COUNT(*) FROM journal_write_gate", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "failed append rolls back its transaction-local gate"
        );
        drop(journal);

        let reopen_error = Journal::open(&path).unwrap_err();
        assert_eq!(
            reopen_error.envelope.reason,
            "journal.replay.baseline.invalid"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fresh_v14_archive_only_suffix_deletion_fails_the_transactional_tail_watermark() {
        let (dir, mut journal) = open_tmp("fresh-v14-archive-suffix-deleted");
        prime_ready(&mut journal, "scope-1");
        let before = journal.materialized_state().unwrap();
        let through_event_id: i64 = journal
            .conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |row| {
                row.get(0)
            })
            .unwrap();

        // This event emits capacity but leaves the Ready projection intact.
        // The suffix below then acquires and completes a job, returning the
        // same materialized slot/job/pending-outbox projection while adding an
        // archived RemoteAcked record that is not stored in the outbox table.
        assert!(
            !journal
                .apply(Event::ReadyAttempt {
                    slot_id: slot("scope-1"),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot("scope-1"),
                    job_id: job("guid-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: Some("request-1".to_owned()),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                })
                .unwrap()
                .rejected
        );
        resolve_test_acquisition(&mut journal, "guid-1", "guid-1", "request-1", r#gen(), None);
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job("guid-1"),
                    slot_id: slot("scope-1"),
                    attempt: 1,
                    generation: r#gen(),
                    worker: "worker-1".to_owned(),
                    accepted_unix: 1_001,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobStarted {
                    job_id: job("guid-1"),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::JobTerminalResult {
                    job_id: job("guid-1"),
                    generation: r#gen(),
                    conclusion: "success".to_owned(),
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job("guid-1"),
                    generation: r#gen(),
                    payload_sha256: payload_checksum(b"done"),
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionSendStarted {
                    job_id: job("guid-1"),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::RemoteAcked {
                    job_id: job("guid-1"),
                    generation: r#gen(),
                })
                .unwrap()
                .rejected
        );
        let replayed = journal.load_state().unwrap();
        assert!(replayed.outbox.iter().any(|row| {
            row.job_id == job("guid-1") && row.generation == r#gen() && row.remote_acked
        }));
        assert_eq!(
            current_projection_mismatch_fields(&replayed, &journal.materialized_state().unwrap()),
            Vec::<&str>::new()
        );
        assert_eq!(journal.materialized_state().unwrap(), before);
        assert!(journal.materialized_state().unwrap().outbox.is_empty());
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        let original_baseline: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'replay_baseline'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        open_fixture_write_gate(&conn);
        let removed = conn
            .execute("DELETE FROM events WHERE id > ?1", [through_event_id])
            .unwrap();
        assert!(removed > 0);
        // Temporarily remove the anchor to prove the deleted event suffix is
        // projection-neutral apart from its archived ack evidence. Restore it
        // before the actual open assertion below.
        conn.execute("DELETE FROM meta WHERE key = 'replay_baseline'", [])
            .unwrap();
        close_fixture_write_gate(&conn);
        let replay_without_suffix = load_state_from_conn(&conn).unwrap();
        let materialized = load_materialized_state(&conn).unwrap();
        assert_eq!(
            current_projection_mismatch_fields(&replay_without_suffix, &materialized),
            Vec::<&str>::new()
        );
        assert!(!replay_without_suffix
            .outbox
            .iter()
            .any(|row| row.job_id == job("guid-1") && row.remote_acked));
        open_fixture_write_gate(&conn);
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('replay_baseline', ?1)",
            [original_baseline],
        )
        .unwrap();
        close_fixture_write_gate(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrated_replay_baseline_tampering_is_rejected() {
        let (dir, path) = make_v13_migrated_fixture("replay-baseline-tampered");
        let conn = Connection::open(&path).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'replay_baseline'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut baseline: ReplayBaseline = serde_json::from_str(&raw).unwrap();
        assert!(baseline.state.control_live);
        baseline.state.control_live = false;
        let checksum = replay_baseline_integrity_checksum(&baseline).unwrap();
        baseline.integrity_checksum = checksum;
        open_fixture_write_gate(&conn);
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'replay_baseline'",
            [serde_json::to_string(&baseline).unwrap()],
        )
        .unwrap();
        close_fixture_write_gate(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.materialized.replay.mismatch"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn migrated_baseline_archive_checksum_rejects_outbox_row_tampering() {
        let (dir, mut journal) = open_tmp("replay-baseline-archive-tampered");
        let generation = prime_running_job(&mut journal, "scope-1", "job-1");
        for event in [
            Event::JobTerminalResult {
                job_id: job("job-1"),
                generation,
                conclusion: "success".to_owned(),
            },
            Event::CompletionIntended {
                job_id: job("job-1"),
                generation,
                payload_sha256: payload_checksum(b"archive payload"),
            },
            Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation,
            },
            Event::RemoteAcked {
                job_id: job("job-1"),
                generation,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let current_history = journal.load_state().unwrap();
        assert!(current_history
            .outbox
            .iter()
            .any(|row| { row.job_id == job("job-1") && row.remote_acked && !row.is_pending() }));
        assert!(journal.materialized_state().unwrap().outbox.is_empty());
        let path = dir.join("journal.db");
        drop(journal);

        // Convert this valid history to its pre-v14 representation. The v13
        // migration must place the terminal outbox record in its replay
        // checkpoint because materialized outbox only retains pending rows.
        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 13);
        conn.pragma_update(None, "user_version", 13u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        assert_eq!(
            conn.query_row("PRAGMA journal_mode=DELETE", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "delete"
        );
        drop(conn);

        let migrated = Journal::open(&path).unwrap();
        let migrated_state = migrated.load_state().unwrap();
        let archived = migrated_state
            .outbox
            .iter()
            .find(|row| row.job_id == job("job-1"))
            .expect("migration baseline retains acknowledged outbox history");
        assert!(archived.remote_acked && !archived.is_pending());
        drop(migrated);

        let conn = Connection::open(&path).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'replay_baseline'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut baseline: ReplayBaseline = serde_json::from_str(&raw).unwrap();
        let archived = baseline
            .state
            .outbox
            .iter_mut()
            .find(|row| row.job_id == job("job-1"))
            .expect("checkpoint includes the acknowledged row");
        assert!(archived.remote_acked && !archived.is_pending());
        archived.attempts += 1;
        open_fixture_write_gate(&conn);
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'replay_baseline'",
            [serde_json::to_string(&baseline).unwrap()],
        )
        .unwrap();
        close_fixture_write_gate(&conn);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_event_inside_migration_baseline_is_rejected() {
        let (dir, path) = make_v13_migrated_fixture("replay-baseline-missing-event");
        let conn = Connection::open(&path).unwrap();
        let raw: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'replay_baseline'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let baseline: ReplayBaseline = serde_json::from_str(&raw).unwrap();
        assert_eq!(baseline.latest_event_id, baseline.through_event_id);
        open_fixture_write_gate(&conn);
        let removed = conn
            .execute(
                "DELETE FROM events WHERE id = ?1",
                [baseline.through_event_id],
            )
            .unwrap();
        close_fixture_write_gate(&conn);
        assert_eq!(removed, 1);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_latest_post_baseline_event_is_rejected() {
        let (dir, path) = make_v13_migrated_fixture("replay-baseline-latest-event");
        let mut journal = Journal::open(&path).unwrap();
        assert!(
            !journal
                .apply(Event::Routing {
                    valid: true,
                    group_valid: true,
                })
                .unwrap()
                .rejected
        );
        let latest_id: i64 = journal
            .conn
            .query_row("SELECT MAX(id) FROM events", [], |row| row.get(0))
            .unwrap();
        let baseline_raw: String = journal
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'replay_baseline'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let baseline: ReplayBaseline = serde_json::from_str(&baseline_raw).unwrap();
        assert_eq!(latest_id, baseline.latest_event_id);
        assert_eq!(baseline.latest_event_count, event_count(&journal));
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        open_fixture_write_gate(&conn);
        let removed = conn
            .execute("DELETE FROM events WHERE id = ?1", [latest_id])
            .unwrap();
        close_fixture_write_gate(&conn);
        assert_eq!(removed, 1);
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.replay.baseline.invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v11_incomplete_owned_identity_divergence_is_refused_before_wal() {
        let (dir, mut journal) = open_tmp("v11-incomplete-owned-identity");
        prime_ready(&mut journal, "scope-1");
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-1"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 51,
        };
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("request-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                    permit_lease: permit_lease.clone(),
                })
                .unwrap()
                .rejected
        );
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        append_fixture_event(
            &conn,
            &Event::JobAcquisitionResolved {
                provisional_job_id: job("request-1"),
                acquired_job_id: job("run-service-job-1"),
                plan_id: String::new(),
                generation: r#gen(),
                runner_request_id: Some("request-1".to_owned()),
                permit_lease: Some(permit_lease.clone()),
            },
        );
        append_fixture_event(
            &conn,
            &Event::JobOwned {
                job_id: job("run-service-job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: r#gen(),
                worker: "worker-1".to_owned(),
                accepted_unix: 1_002,
            },
        );
        open_fixture_write_gate(&conn);
        assert_eq!(
            conn.execute(
                "UPDATE jobs
                 SET job_id = 'run-service-job-1', attempt = 1, worker = 'worker-1',
                     phase = 'assigned', accepted_unix = 1002, provisional = 0,
                     plan_id = ''
                 WHERE job_id = 'request-1'",
                [],
            )
            .unwrap(),
            1
        );
        close_fixture_write_gate(&conn);
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let old = Connection::open(&path).unwrap();
        let materialized = load_materialized_state(&old).unwrap();
        let replayed = load_state_from_conn(&old).unwrap();
        assert_eq!(materialized.jobs[0].job_id, job("run-service-job-1"));
        assert!(!materialized.jobs[0].provisional);
        assert_eq!(
            materialized.jobs[0].permit_lease,
            Some(permit_lease.clone())
        );
        assert_eq!(replayed.jobs[0].job_id, job("request-1"));
        assert!(replayed.jobs[0].provisional);
        drop(old);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.migration.job.inconsistent");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!wal.exists(), "identity mismatch must fail before WAL");
        assert!(!shm.exists(), "identity mismatch must fail before WAL");

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        assert!(!table_has_column(&check, "jobs", "acquisition_loss_unproven").unwrap());
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        let retained = load_materialized_state(&check).unwrap();
        assert_eq!(retained.jobs.len(), 1);
        assert_eq!(retained.jobs[0].job_id, job("run-service-job-1"));
        assert_eq!(retained.jobs[0].runner_request_id, "request-1");
        assert_eq!(retained.jobs[0].permit_lease, Some(permit_lease));
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v10_proofless_loss_is_refused_before_wal_or_migration() {
        let path = make_v10_proofless_loss_journal("v10-proofless-loss-prewal-refusal");
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.migration.acquisition_loss.ambiguous"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!wal.exists(), "ambiguous v10 history must fail before WAL");
        assert!(!shm.exists(), "ambiguous v10 history must not create SHM");

        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 10);
        assert_eq!(
            conn.query_row(
                "SELECT phase FROM slots WHERE slot_id = 'scope-1'",
                [],
                |row| { row.get::<_, String>(0) }
            )
            .unwrap(),
            "ready",
            "the v10 materialization remains available for recovery"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(conn);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v11_migration_ignores_terminal_outbox_rows_dropped_by_projection() {
        let (path, _) = make_v11_proofless_loss_journal("v11-v12-terminal-outbox-projection");
        let conn = Connection::open(&path).unwrap();
        let generation = r#gen();
        let completed_job = job("job-2");
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-2"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 42,
        };
        let payload_sha256 = payload_checksum(b"completed historical outbox");
        for event in [
            Event::JobAcquisitionRebuiltWithPermit {
                slot_id: slot("scope-1"),
                job_id: completed_job.clone(),
                generation,
                runner_request_id: "request-2".to_owned(),
                plan_id: "plan-2".to_owned(),
                run_service_url: "https://run.example/run".to_owned(),
                probe_deadline_unix: 20_000,
                permit_lease,
            },
            Event::JobOwned {
                job_id: completed_job.clone(),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation,
                worker: "worker-2".to_owned(),
                accepted_unix: 2_001,
            },
            Event::JobStarted {
                job_id: completed_job.clone(),
                generation,
            },
            Event::JobTerminalResult {
                job_id: completed_job.clone(),
                generation,
                conclusion: "success".to_owned(),
            },
            Event::CompletionIntended {
                job_id: completed_job.clone(),
                generation,
                payload_sha256,
            },
            Event::CompletionSendStarted {
                job_id: completed_job.clone(),
                generation,
            },
            Event::RemoteAcked {
                job_id: completed_job,
                generation,
            },
        ] {
            append_fixture_event(&conn, &event);
        }
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM outbox", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "the current materialized projection contains no terminal outbox row"
        );
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let migrated = Journal::open(&path)
            .expect("terminal historical outbox is omitted by both materialized projections");
        assert_eq!(
            migrated
                .materialized_state()
                .unwrap()
                .outbox
                .iter()
                .filter(|row| row.is_pending())
                .count(),
            0
        );
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'remote_acked'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1,
            "terminal acknowledgement remains in the immutable event history"
        );
        drop(conn);
        drop(migrated);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v11_capacity_invalid_marker_blocks_lossy_proofless_replay() {
        let (dir, mut journal) = open_tmp("v11-capacity-invalid-proofless-loss");
        prime_ready(&mut journal, "scope-1");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 3 })
                .unwrap()
                .rejected
        );
        prime_ready_in_existing_capacity(&mut journal, "scope-2");
        prime_ready_in_existing_capacity(&mut journal, "scope-3");
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-1"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 41,
        };
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntendedWithPermit {
                    slot_id: slot("scope-1"),
                    job_id: job("request-1"),
                    generation: r#gen(),
                    message_id: "message-1".to_owned(),
                    runner_request_id: "request-1".to_owned(),
                    run_service_url: "https://run.example/run".to_owned(),
                    intended_unix: 1_000,
                    permit_lease,
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        append_fixture_event(
            &conn,
            &Event::JobAcquisitionLost {
                job_id: job("request-1"),
                generation: r#gen(),
                reason: "probe budget spent".to_owned(),
                proof: None,
            },
        );
        // This is the v11 projection after the reason-only event removed the
        // job. The three durable slot rows still exceed the desired capacity,
        // so v12 must preserve the capacity-invalid marker and refuse a replay
        // that would recreate a holder row while skipping materialized writes.
        conn.execute("DELETE FROM jobs", []).unwrap();
        conn.execute(
            "UPDATE slots SET phase = 'ready' WHERE slot_id = 'scope-1'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('capacity_invalid', '1')
             ON CONFLICT(key) DO UPDATE SET value = '1'",
            [],
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.migration.job.inconsistent");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!wal.exists(), "unsafe replay must stop before WAL");
        assert!(!shm.exists(), "unsafe replay must not create SHM");

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        let invalid_marker: String = check
            .query_row(
                "SELECT value FROM meta WHERE key = 'capacity_invalid'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(invalid_marker, "1", "forensic fence must remain intact");
        assert_eq!(
            check
                .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0,
            "the original materialized job projection remains intact"
        );
        assert_eq!(
            check
                .query_row("SELECT COUNT(*) FROM slots", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3,
            "all overcapacity forensic slot rows remain intact"
        );
        assert_eq!(
            check
                .query_row(
                    "SELECT phase FROM slots WHERE slot_id = 'scope-1'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "ready",
            "the original slot projection remains intact"
        );
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v11_migration_refuses_to_drop_a_later_pending_outbox() {
        let (path, _) = make_v11_proofless_loss_journal("v11-v12-pending-outbox-preserve");
        let conn = Connection::open(&path).unwrap();
        conn.execute("DELETE FROM meta WHERE key IN ('drain', 'admission')", [])
            .unwrap();
        let request_id = job("request-2");
        let acquired_job_id = job("job-2");
        let generation = r#gen();
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-2"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 42,
        };
        let payload_sha256 = payload_checksum(b"job-2-completion");
        for event in [
            Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot("scope-1"),
                job_id: request_id.clone(),
                generation,
                message_id: "message-2".to_owned(),
                runner_request_id: request_id.0.clone(),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 2_000,
                permit_lease: permit_lease.clone(),
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: request_id,
                acquired_job_id: acquired_job_id.clone(),
                plan_id: "plan-2".to_owned(),
                generation,
                runner_request_id: Some("request-2".to_owned()),
                permit_lease: Some(permit_lease.clone()),
            },
            Event::JobOwned {
                job_id: acquired_job_id.clone(),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation,
                worker: "worker-2".to_owned(),
                accepted_unix: 2_001,
            },
            Event::JobStarted {
                job_id: acquired_job_id.clone(),
                generation,
            },
            Event::JobTerminalResult {
                job_id: acquired_job_id.clone(),
                generation,
                conclusion: "success".to_owned(),
            },
            Event::CompletionIntended {
                job_id: acquired_job_id.clone(),
                generation,
                payload_sha256: payload_sha256.clone(),
            },
        ] {
            append_fixture_event(&conn, &event);
        }

        // This is the valid v11 materialization after those later events.
        // Strict v12 replay keeps the earlier ambiguous acquisition and cannot
        // reconstruct this completed owner's outbox row, so migration must
        // stop before replacing the durable snapshot.
        let lease_json = serde_json::to_string(&permit_lease).unwrap();
        conn.execute(
            "UPDATE slots SET phase = 'assigned' WHERE slot_id = 'scope-1'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (
                 job_id, slot_id, generation, attempt, worker, phase, accepted_unix,
                 terminal_conclusion, provisional, plan_id, run_service_url,
                 probe_attempts, probe_deadline_unix, runner_request_id, permit_lease
             ) VALUES (?1, 'scope-1', 1, 1, 'worker-2', 'completing', 2001,
                       'success', 0, 'plan-2', 'https://run.example/run',
                       0, 0, 'request-2', ?2)",
            params![acquired_job_id.0, lease_json],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox (
                 job_id, slot_id, generation, payload_sha256, intended, send_started,
                 remote_acked, created_unix, attempts, deadline_unix, permanent, abandoned
             ) VALUES (?1, 'scope-1', 1, ?2, 1, 0, 0, 2002, 0, ?3, 0, 0)",
            params![
                acquired_job_id.0,
                payload_sha256,
                2_002 + COMPLETION_RESOLUTION_SECONDS as i64
            ],
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(
            error.envelope.reason,
            "journal.migration.outbox.inconsistent"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!wal.exists(), "ambiguous outbox must be refused before WAL");
        assert!(!shm.exists(), "ambiguous outbox must be refused before WAL");

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        assert_eq!(
            check
                .query_row(
                    "SELECT COUNT(*) FROM outbox WHERE job_id = 'job-2'",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .unwrap(),
            1,
            "the old pending outbox row remains available for recovery"
        );
        assert_eq!(
            check
                .query_row(
                    "SELECT COUNT(*) FROM jobs WHERE job_id = 'job-2'",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .unwrap(),
            1
        );
        drop(check);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v11_migration_refuses_to_drop_a_later_owned_job_without_outbox() {
        let (path, _) = make_v11_proofless_loss_journal("v11-v12-owned-job-preserve");
        let conn = Connection::open(&path).unwrap();
        conn.execute("DELETE FROM meta WHERE key IN ('drain', 'admission')", [])
            .unwrap();
        let request_id = job("request-2");
        let acquired_job_id = job("job-2");
        let generation = r#gen();
        let permit_lease = NativePermitLease {
            holder: "native/v1/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/request-2"
                .to_owned(),
            ledger_path: "/tmp/permit-ledger.db".to_owned(),
            generation: 42,
        };
        for event in [
            Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot("scope-1"),
                job_id: request_id.clone(),
                generation,
                message_id: "message-2".to_owned(),
                runner_request_id: request_id.0.clone(),
                run_service_url: "https://run.example/run".to_owned(),
                intended_unix: 2_000,
                permit_lease: permit_lease.clone(),
            },
            Event::JobAcquisitionResolved {
                provisional_job_id: request_id,
                acquired_job_id: acquired_job_id.clone(),
                plan_id: "plan-2".to_owned(),
                generation,
                runner_request_id: Some("request-2".to_owned()),
                permit_lease: Some(permit_lease.clone()),
            },
            Event::JobOwned {
                job_id: acquired_job_id.clone(),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation,
                worker: "worker-2".to_owned(),
                accepted_unix: 2_001,
            },
        ] {
            append_fixture_event(&conn, &event);
        }

        // v11 accepted and materialized this later owner after its reason-only
        // loss event freed the slot. Strict v12 replay cannot prove that the
        // later owner is safe, so it must refuse the migration before WAL
        // rather than erasing the current owner's only durable row.
        let lease_json = serde_json::to_string(&permit_lease).unwrap();
        conn.execute(
            "UPDATE slots SET phase = 'assigned' WHERE slot_id = 'scope-1'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (
                 job_id, slot_id, generation, attempt, worker, phase, accepted_unix,
                 terminal_conclusion, provisional, plan_id, run_service_url,
                 probe_attempts, probe_deadline_unix, runner_request_id, permit_lease
             ) VALUES (?1, 'scope-1', 1, 1, 'worker-2', 'assigned', 2001,
                       NULL, 0, 'plan-2', 'https://run.example/run',
                       0, 0, 'request-2', ?2)",
            params![acquired_job_id.0, lease_json],
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.migration.job.inconsistent");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "ambiguous owned job must be refused before WAL"
        );
        assert!(
            !shm.exists(),
            "ambiguous owned job must be refused before WAL"
        );

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        assert_eq!(
            check
                .query_row(
                    "SELECT COUNT(*) FROM jobs WHERE job_id = 'job-2'",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .unwrap(),
            1,
            "the later owned-job row remains available for recovery"
        );
        drop(check);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v11_migration_preserves_materialized_overcapacity_forensics() {
        let (dir, mut journal) = open_tmp("v11-v12-overcapacity-preserve");
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        for scope in ["scope-1", "scope-2"] {
            prime_ready_in_existing_capacity(&mut journal, scope);
        }
        drop(journal);

        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.execute(
            "INSERT INTO slots (
                 slot_id, generation, phase, permit_held, routing_valid,
                 session_live, executor_proven, registered, pid, heartbeat_unix
             ) VALUES ('scope-forensic', 1, 'provisioning', 0, 0, 0, 0, 0, NULL, 0)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        drop(conn);

        let mut migrated =
            Journal::open(&path).expect("migration must preserve and fence invalid rows");
        let version: i64 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        let state = migrated.materialized_state().unwrap();
        assert!(state.capacity_invalid);
        assert!(
            migrated.load_state().unwrap().capacity_invalid,
            "event replay must retain the durable forensic capacity fence"
        );
        assert_eq!(state.slots.len(), 3);
        assert_eq!(
            migrated
                .conn
                .query_row("SELECT COUNT(*) FROM slots", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            3,
            "v12 must preserve the materialized-only forensic slot"
        );
        let invalid_marker: String = migrated
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'capacity_invalid'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(invalid_marker, "1", "v12 must persist the forensic fence");
        let error = migrated
            .apply(Event::DesiredCapacity { ready: 2 })
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.capacity.invalid");
        drop(migrated);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_v11_meta_is_refused_before_wal_or_sidecars() {
        let (dir, mut journal) = open_tmp("v11-malformed-meta-pre-wal");
        assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.execute(
            "UPDATE meta SET value = 'not-a-bool' WHERE key = 'control_live'",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.materialized.invalid");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        assert!(!table_has_column(&check, "jobs", "acquisition_loss_unproven").unwrap());
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unrecognized_v11_trigger_is_refused_before_wal_or_sidecars() {
        let (dir, mut journal) = open_tmp("v11-extra-trigger-pre-wal");
        assert!(!journal.apply(Event::ControlLive).unwrap().rejected);
        let path = dir.join("journal.db");
        drop(journal);

        let conn = Connection::open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop_jobs_columns_added_after_version(&conn, 11);
        conn.execute_batch(
            "CREATE TRIGGER rogue_job_delete
             AFTER DELETE ON jobs BEGIN SELECT 1; END;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 11u32).unwrap();
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );

        let check = Connection::open(&path).unwrap();
        let version: i64 = check
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 11);
        assert!(table_has_column(&check, "jobs", "permit_lease").unwrap());
        assert!(!table_has_column(&check, "jobs", "acquisition_loss_unproven").unwrap());
        let mode: String = check
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v11_reserved_views_and_foreign_trigger_names_are_refused_before_wal() {
        for (label, edit) in [
            (
                "v11-write-gate-view-collision",
                "CREATE VIEW journal_write_gate AS SELECT 1 AS id;",
            ),
            (
                "v11-jobs-view-collision",
                "DROP TABLE jobs;
                 CREATE VIEW jobs AS SELECT 'job-1' AS job_id;",
            ),
            (
                "v11-core-name-non-table-collision",
                "DROP TABLE events;
                 CREATE INDEX events ON jobs (job_id);",
            ),
            (
                "v11-foreign-trigger-reserved-name",
                "CREATE TABLE extension_records (id INTEGER PRIMARY KEY);
                 CREATE TRIGGER journal_write_gate_events_insert
                 BEFORE INSERT ON extension_records BEGIN SELECT 1; END;",
            ),
        ] {
            assert_v11_schema_edit_fails_before_wal(label, edit);
        }
    }

    #[test]
    fn v2_rebuild_name_collision_is_refused_before_wal() {
        assert_v2_schema_edit_fails_before_wal(
            "v2-outbox-v3-name-collision",
            "CREATE VIEW outbox_v3 AS SELECT 1 AS job_id;",
        );
    }

    #[test]
    fn v2_rebuild_refuses_foreign_key_cascade_into_extension_rows() {
        let (dir, journal) = open_tmp("v2-outbox-foreign-key-cascade");
        let path = dir.join("journal.db");
        drop(journal);
        seed_v2_outbox(&path, 2);

        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sqliteX_extension_records (
                 job_id TEXT REFERENCES outbox(job_id) ON DELETE CASCADE
             );
             INSERT INTO sqliteX_extension_records (job_id) VALUES ('job-1');",
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_row| Ok(()))
            .unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(conn);

        let wal = PathBuf::from(format!("{}-wal", path.display()));
        let shm = PathBuf::from(format!("{}-shm", path.display()));
        assert!(!wal.exists());
        assert!(!shm.exists());
        let before = std::fs::read(&path).unwrap();
        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(
            !wal.exists(),
            "pre-WAL refusal must not create a WAL sidecar"
        );
        assert!(
            !shm.exists(),
            "pre-WAL refusal must not create an SHM sidecar"
        );

        let check = Connection::open(&path).unwrap();
        assert_eq!(
            check
                .query_row(
                    "SELECT COUNT(*) FROM sqliteX_extension_records",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1,
            "legacy extension evidence remains intact"
        );
        drop(check);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v2_rebuild_refuses_foreign_keys_to_staging_and_from_outbox() {
        assert_v2_schema_edit_fails_before_wal(
            "v2-fk-target-outbox-v3",
            "CREATE TABLE extension_rows (
                 job_id TEXT REFERENCES outbox_v3(job_id) ON DELETE CASCADE
             );",
        );
        assert_v2_schema_edit_fails_before_wal(
            "v2-outbox-outbound-fk",
            "CREATE TABLE extension_roots (id TEXT PRIMARY KEY);
             INSERT INTO extension_roots (id) VALUES ('job-1');
             DROP TABLE outbox;
             CREATE TABLE outbox (
                 job_id TEXT PRIMARY KEY REFERENCES extension_roots(id) ON DELETE CASCADE,
                 generation INTEGER NOT NULL,
                 payload_sha256 TEXT NOT NULL,
                 intended INTEGER NOT NULL DEFAULT 0,
                 send_started INTEGER NOT NULL DEFAULT 0,
                 remote_acked INTEGER NOT NULL DEFAULT 0,
                 created_unix INTEGER NOT NULL
             );
             INSERT INTO outbox (
                 job_id, generation, payload_sha256, intended,
                 send_started, remote_acked, created_unix
             ) VALUES ('job-1', 1, 'payload', 1, 0, 0, 1);",
        );
    }

    #[test]
    fn current_schema_rejects_external_cascade_before_rewrite() {
        let (dir, mut journal) = open_tmp("current-external-fk-cascade");
        prime_ready(&mut journal, "scope-1");
        let before_events = event_count(&journal);
        let path = dir.join("journal.db");
        let extension = Connection::open(&path).unwrap();
        extension
            .execute_batch(
                "CREATE TABLE extension_rows (
                     slot_id TEXT REFERENCES slots(slot_id) ON DELETE CASCADE
                 );
                 INSERT INTO extension_rows (slot_id) VALUES ('scope-1');",
            )
            .unwrap();
        drop(extension);

        let error = journal
            .apply(Event::Dependency {
                github_reachable: false,
            })
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(event_count(&journal), before_events);
        let state = journal.materialized_state().unwrap();
        assert!(state.github_reachable);
        assert_eq!(state.slots.len(), 1);
        assert_eq!(state.slots[0].slot_id, slot("scope-1"));

        let extension = Connection::open(&path).unwrap();
        assert_eq!(
            extension
                .query_row("SELECT COUNT(*) FROM extension_rows", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1,
            "a concurrent schema extension cannot be cascade-deleted by persist_state"
        );
        drop(extension);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn current_schema_rejects_outbox_outbound_fk_before_rewrite() {
        let (dir, mut journal) = open_tmp("current-outbox-outbound-fk");
        let before = journal.materialized_state().unwrap();
        let before_events = event_count(&journal);
        let path = dir.join("journal.db");

        let extension = Connection::open(&path).unwrap();
        extension
            .execute_batch(
                "CREATE TABLE extension_roots (
                     id TEXT PRIMARY KEY
                 );
                 INSERT INTO extension_roots (id) VALUES ('root-1');
                 DROP TRIGGER journal_write_gate_outbox_insert;
                 DROP TRIGGER journal_write_gate_outbox_update;
                 DROP TRIGGER journal_write_gate_outbox_delete;
                 DROP TABLE outbox;
                 CREATE TABLE outbox (
                     job_id TEXT PRIMARY KEY REFERENCES extension_roots(id) ON DELETE CASCADE,
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
                 CREATE TRIGGER journal_write_gate_outbox_insert
                 BEFORE INSERT ON outbox
                 WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
                 BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
                 CREATE TRIGGER journal_write_gate_outbox_update
                 BEFORE UPDATE ON outbox
                 WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
                 BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;
                 CREATE TRIGGER journal_write_gate_outbox_delete
                 BEFORE DELETE ON outbox
                 WHEN NOT EXISTS (SELECT 1 FROM journal_write_gate WHERE id = 1)
                 BEGIN SELECT RAISE(ABORT, 'journal write gate closed'); END;",
            )
            .unwrap();
        drop(extension);

        let error = journal
            .apply(Event::Dependency {
                github_reachable: true,
            })
            .unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        assert_eq!(journal.materialized_state().unwrap(), before);
        assert_eq!(event_count(&journal), before_events);

        let extension = Connection::open(&path).unwrap();
        assert_eq!(
            extension
                .query_row("SELECT COUNT(*) FROM extension_roots", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1,
            "the external parent sentinel remains intact"
        );
        assert_eq!(
            extension
                .query_row(
                    "SELECT \"table\" FROM pragma_foreign_key_list('outbox')",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "extension_roots",
            "the rejected schema edge remains intact"
        );
        drop(extension);
        drop(journal);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v10_stamp_refuses_checksummed_v11_fields_before_schema_change() {
        let fields = [
            (
                "job_acquisition_intended",
                "runner_request_id",
                serde_json::Value::Null,
            ),
            (
                "job_acquisition_resolved",
                "runner_request_id",
                serde_json::json!("request-1"),
            ),
            (
                "job_acquisition_resolved",
                "permit_lease",
                serde_json::json!({
                    "holder": "native/request-1",
                    "ledger_path": "/tmp/permit-ledger.db",
                    "generation": 1
                }),
            ),
        ];

        for (index, (event_type, field, field_value)) in fields.into_iter().enumerate() {
            let path = make_v10_acquisition_journal(&format!("v10-v11-field-{index}"));
            let conn = Connection::open(&path).unwrap();
            let event_id = add_v11_event_field(&conn, event_type, field, field_value);
            let (payload, checksum): (String, String) = conn
                .query_row(
                    "SELECT payload, checksum FROM events WHERE id = ?1",
                    [event_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(sha256_hex(payload.as_bytes()), checksum);
            drop(conn);

            let before = std::fs::read(&path).unwrap();
            let error = Journal::open(&path).unwrap_err();
            assert_eq!(error.envelope.reason, "journal.event.unknown");
            assert_eq!(std::fs::read(&path).unwrap(), before);

            let check = Connection::open(&path).unwrap();
            let version: i64 = check
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(version, 10);
            assert!(!table_has_column(&check, "jobs", "permit_lease").unwrap());
            let journal_mode: String = check
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal_mode, "delete");
            drop(check);
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn v8_to_v9_migration_rolls_back_and_retries_after_bad_event() {
        let path = make_v8_acquisition_journal("v8-to-v9-retry");
        let conn = Connection::open(&path).unwrap();
        let (event_id, payload, checksum): (i64, String, String) = conn
            .query_row(
                "SELECT id, payload, checksum FROM events ORDER BY id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let malformed_checksum = sha256_hex(b"{");
        conn.execute(
            "UPDATE events SET payload = '{', checksum = ?1 WHERE id = ?2",
            params![malformed_checksum, event_id],
        )
        .unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.event.unknown");
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 8);
        assert!(!table_has_column(&conn, "jobs", "runner_request_id").unwrap());
        conn.execute(
            "UPDATE events SET payload = ?1, checksum = ?2 WHERE id = ?3",
            params![payload, checksum, event_id],
        )
        .unwrap();
        drop(conn);

        let reopened = Journal::open(&path).unwrap();
        assert_eq!(
            reopened.materialized_state().unwrap().jobs[0].runner_request_id,
            "request-1"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v9_journal_migrates_valid_acquisition_history() {
        let path = make_v9_acquisition_journal("v9-to-v10-valid-acquisition");
        let old = Connection::open(&path).unwrap();
        let old_version: i64 = old
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(old_version, 9);
        assert!(table_has_column(&old, "jobs", "runner_request_id").unwrap());
        assert!(!table_has_column(&old, "jobs", "permit_lease").unwrap());
        let mode: String = old
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
        drop(old);

        let migrated = Journal::open(&path).expect("valid v9 history must migrate");
        let version: i64 = migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        assert!(table_has_column(&migrated.conn, "jobs", "permit_lease").unwrap());
        let state = migrated.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("job-1"));
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert_eq!(state.jobs[0].plan_id, "plan-1");
        assert_eq!(state.jobs[0].phase, JobPhase2::Assigned);
        drop(migrated);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn concurrent_v8_openers_converge_on_one_correlation_schema() {
        let path = make_v8_acquisition_journal("concurrent-v8-to-v9");
        let path = Arc::new(path);
        let start = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = Arc::clone(&path);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    Journal::open(path.as_path()).and_then(|journal| {
                        let state = journal.materialized_state()?;
                        if state.jobs.len() == 1 && state.jobs[0].runner_request_id == "request-1" {
                            Ok(())
                        } else {
                            Err(StoreError::new(
                                velnor_model::ExitClass::Conflict,
                                "journal.test.correlation.lost",
                            ))
                        }
                    })
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .expect("concurrent v8 opener panicked")
                .expect("concurrent v8 opener failed");
        }
        let conn = Connection::open(path.as_path()).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION as i64);
        assert!(table_has_column(&conn, "jobs", "runner_request_id").unwrap());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn v9_stamp_without_request_correlation_column_fails_before_load() {
        let (dir, journal) = open_tmp("v9-missing-correlation-column");
        let path = dir.join("journal.db");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_journal_write_gate(&conn);
        drop_jobs_columns(
            &conn,
            &[
                "runner_request_id",
                "permit_lease",
                "acquisition_loss_unproven",
            ],
        );
        conn.pragma_update(None, "user_version", 9u32).unwrap();
        drop(conn);

        let error = Journal::open(&path).unwrap_err();
        assert_eq!(error.envelope.reason, "journal.schema.mismatch");
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 9, "failed preflight must preserve the stamp");
        assert!(
            !table_has_column(&conn, "jobs", "runner_request_id").unwrap(),
            "failed preflight must not invent a replacement column"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn v7_migration_fails_closed_on_cross_type_phase_rows() {
        let (dir, mut journal) = open_tmp("v7-to-v8-poison");
        let path = dir.join("journal.db");
        prime_running_job(&mut journal, "scope-1", "job-1");
        drop(journal);
        let conn = Connection::open(&path).unwrap();
        drop_jobs_columns_added_after_version(&conn, 7);
        strip_v11_event_fields(&conn);
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
