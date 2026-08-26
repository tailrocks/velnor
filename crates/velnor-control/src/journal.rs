//! Durable node journal: WAL + `synchronous=FULL`, immutable events, reducer.
//!
//! Side-effect commands are returned only after the intent event is committed.
//! Completions are at-least-once: an outbox row survives until a remote
//! acknowledgement (or observed terminal) is itself committed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use velnor_model::{
    ActorPhase, CanaryStatus, ExecutionBackendKind, FleetHealthState, Generation, HealthDocument,
    JobId, ReadyProof, SlotId,
};

use crate::store::error::{StoreError, StoreResult};

/// Minimum bundled SQLite that includes the WAL-reset fix (3.51.3).
pub const MIN_SQLITE_VERSION: (u32, u32, u32) = (3, 51, 3);

/// Current journal schema. Older writers seeing a higher `PRAGMA user_version`
/// must not apply events (N-1 must not clobber an N writer's log).
pub const JOURNAL_SCHEMA_VERSION: u32 = 2;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    generation INTEGER NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    checksum TEXT NOT NULL
);
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
    heartbeat_unix INTEGER NOT NULL DEFAULT 0,
    surge INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS jobs (
    job_id TEXT PRIMARY KEY,
    slot_id TEXT NOT NULL,
    generation INTEGER NOT NULL,
    attempt INTEGER NOT NULL,
    worker TEXT NOT NULL,
    phase TEXT NOT NULL,
    accepted_unix INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS outbox (
    job_id TEXT PRIMARY KEY,
    generation INTEGER NOT NULL,
    payload_sha256 TEXT NOT NULL,
    intended INTEGER NOT NULL DEFAULT 0,
    send_started INTEGER NOT NULL DEFAULT 0,
    remote_acked INTEGER NOT NULL DEFAULT 0,
    created_unix INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Fleet materialization the reducer reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetState {
    pub control_live: bool,
    pub journal_writable: bool,
    pub github_reachable: bool,
    pub routing_valid: bool,
    pub runner_group_valid: bool,
    pub desired_ready: u32,
    pub surge: u32,
    pub canary: CanaryStatus,
    pub package_generation: u64,
    pub package_apt_version: String,
    pub execution_backend: ExecutionBackendKind,
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
            desired_ready: 0,
            surge: 0,
            canary: CanaryStatus::Unknown,
            package_generation: 0,
            package_apt_version: String::new(),
            // Packaged default until journal load; not a live fallback.
            execution_backend: ExecutionBackendKind::Docker,
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
        let permits = self.slots.iter().filter(|slot| slot.permit_held).count() as u32;
        let executor_ready = self
            .slots
            .iter()
            .filter(|slot| slot.executor_proven)
            .count() as u32;
        let surge_ready = self
            .slots
            .iter()
            .filter(|slot| slot.surge && slot.phase.counts_as_ready())
            .count() as u32;
        HealthDocument {
            control_live: self.control_live,
            journal_writable: self.journal_writable,
            github_reachable: self.github_reachable,
            routing_valid: self.routing_valid,
            runner_group_valid: self.runner_group_valid,
            desired_ready_slots: self.desired_ready,
            actual_ready_slots: actual_ready,
            surge_ready_slots: surge_ready,
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
        self.slots
            .iter()
            .filter(|slot| slot.permit_held && slot.phase.counts_as_ready())
            .count() as u32
    }
}

fn job_occupies_slot(phase: ActorPhase) -> bool {
    matches!(
        phase,
        ActorPhase::Assigned | ActorPhase::Starting | ActorPhase::Running | ActorPhase::Completing
    )
}

fn restore_slot_after_terminal_job(
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
    // Fencing is a recovery barrier. Terminal job reconciliation must clear
    // the job/outbox state, but only controller recovery may reopen the slot.
    if state.slots[index].phase == ActorPhase::Fenced {
        return;
    }
    if state.slots[index].ready_proof().is_ok() && state.slots[index].registered {
        state.slots[index].phase = ActorPhase::Ready;
        commands.push(SideEffect::AdvertiseCapacity {
            permits: state.advertised_capacity(),
        });
    } else if state.slots[index].registered {
        state.slots[index].phase = ActorPhase::Registered;
    } else {
        state.slots[index].phase = ActorPhase::Provisioning;
    }
}

fn oldest_queued_job_seconds(jobs: &[JobRecord]) -> u64 {
    let now = unix_now();
    jobs.iter()
        .filter(|job| job_occupies_slot(job.phase) && job.accepted_unix > 0)
        .map(|job| now.saturating_sub(job.accepted_unix))
        .max()
        .unwrap_or(0)
}

fn stamp_event(event: &mut Event) {
    if let Event::JobOwned { accepted_unix, .. } = event {
        if *accepted_unix == 0 {
            *accepted_unix = unix_now();
        }
    }
}

fn oldest_outbox_age_seconds(outbox: &[OutboxRecord]) -> u64 {
    let now = unix_now();
    outbox
        .iter()
        .filter(|row| row.intended && !row.remote_acked)
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
    pub phase: ActorPhase,
    pub permit_held: bool,
    pub routing_valid: bool,
    pub session_live: bool,
    pub executor_proven: bool,
    pub registered: bool,
    pub pid: Option<u32>,
    pub heartbeat_unix: u64,
    pub surge: bool,
}

impl SlotRecord {
    fn new(slot_id: SlotId) -> Self {
        Self {
            slot_id,
            generation: Generation::INITIAL,
            phase: ActorPhase::Absent,
            permit_held: false,
            routing_valid: false,
            session_live: false,
            executor_proven: false,
            registered: false,
            pid: None,
            heartbeat_unix: 0,
            surge: false,
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
    pub phase: ActorPhase,
    pub accepted_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRecord {
    pub job_id: JobId,
    pub generation: Generation,
    pub payload_sha256: String,
    pub intended: bool,
    pub send_started: bool,
    pub remote_acked: bool,
    pub created_unix: u64,
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
        surge: u32,
    },
    PermitReserved {
        slot_id: SlotId,
        generation: Generation,
        surge: bool,
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
    Assigned {
        slot_id: SlotId,
        job_id: JobId,
        generation: Generation,
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
    /// Installed apt generation is live. Additive: N-1 readers skip unknown kinds.
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
    StartJob {
        job_id: JobId,
        generation: Generation,
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
        Event::DesiredCapacity { ready, surge } => {
            state.desired_ready = ready;
            state.surge = surge;
        }
        Event::PermitReserved {
            slot_id,
            generation,
            surge,
        } => {
            let routing = state.routing_valid && state.runner_group_valid;
            let active_job = state
                .jobs
                .iter()
                .any(|job| job.slot_id == slot_id && job_occupies_slot(job.phase));
            let slot = state.slot_mut(&slot_id);
            if generation < slot.generation
                || (generation > slot.generation && active_job)
                || (generation == slot.generation && slot.phase == ActorPhase::Fenced)
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
                    slot.phase = ActorPhase::Provisioning;
                }
                slot.generation = generation;
                slot.permit_held = true;
                slot.surge = surge;
                slot.routing_valid = routing;
                if slot.phase == ActorPhase::Absent {
                    slot.phase = ActorPhase::Provisioning;
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
            if generation != slot.generation || slot.phase == ActorPhase::Fenced {
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
            if generation != slot.generation || slot.phase == ActorPhase::Fenced {
                rejected = true;
            } else {
                slot.session_live = true;
            }
        }
        Event::RegistrationIntended {
            slot_id,
            generation,
        } => {
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation
                || slot.phase == ActorPhase::Fenced
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
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || slot.phase == ActorPhase::Fenced {
                rejected = true;
            } else {
                slot.registered = true;
                slot.phase = ActorPhase::Registered;
            }
        }
        Event::RegistrationLost {
            slot_id,
            generation,
        } => {
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || slot.phase == ActorPhase::Fenced || !slot.registered
            {
                rejected = true;
            } else {
                slot.registered = false;
                slot.phase = ActorPhase::Provisioning;
            }
        }
        Event::ReadyAttempt {
            slot_id,
            generation,
        } => {
            {
                let slot = state.slot_mut(&slot_id);
                if generation != slot.generation || slot.phase == ActorPhase::Fenced {
                    rejected = true;
                } else if slot.ready_proof().is_ok() && slot.registered {
                    slot.phase = ActorPhase::Ready;
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
        Event::Assigned {
            slot_id,
            job_id: _,
            generation,
        } => {
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation || slot.phase != ActorPhase::Ready {
                rejected = true;
            } else {
                slot.phase = ActorPhase::Assigned;
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
                job.slot_id == slot_id && job.job_id != job_id && job_occupies_slot(job.phase)
            });
            if newer_job
                || slot_generation != Some(generation)
                || slot_phase != Some(ActorPhase::Assigned)
                || other_live
            {
                rejected = true;
            } else {
                state.jobs.retain(|job| job.job_id != job_id);
                state.jobs.push(JobRecord {
                    job_id: job_id.clone(),
                    slot_id,
                    generation,
                    attempt,
                    worker,
                    phase: ActorPhase::Assigned,
                    accepted_unix,
                });
                commands.push(SideEffect::StartJob { job_id, generation });
            }
        }
        Event::JobStarted { job_id, generation } => {
            if let Some(job) = state.jobs.iter_mut().find(|job| job.job_id == job_id) {
                if job.generation != generation {
                    rejected = true;
                } else {
                    job.phase = ActorPhase::Running;
                }
            } else {
                rejected = true;
            }
        }
        Event::CompletionIntended {
            job_id,
            generation,
            payload_sha256,
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
                            .map(|slot| (job.generation, slot.generation))
                    });
            if let Some((job_generation, slot_generation)) = slot_generation {
                if job_generation != generation || slot_generation != generation {
                    rejected = true;
                } else {
                    if let Some(job) = state.jobs.iter_mut().find(|job| job.job_id == job_id) {
                        job.phase = ActorPhase::Completing;
                    }
                    state.outbox.retain(|row| row.job_id != job_id);
                    state.outbox.push(OutboxRecord {
                        job_id: job_id.clone(),
                        generation,
                        payload_sha256: payload_sha256.clone(),
                        intended: true,
                        send_started: false,
                        remote_acked: false,
                        created_unix: unix_now(),
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
            if let Some(row) = state.outbox.iter_mut().find(|row| row.job_id == job_id) {
                if row.generation != generation {
                    rejected = true;
                } else {
                    row.send_started = true;
                }
            } else {
                rejected = true;
            }
        }
        Event::RemoteAcked { job_id, generation }
        | Event::RemoteObservedTerminal { job_id, generation } => {
            if let Some(row) = state.outbox.iter_mut().find(|row| row.job_id == job_id) {
                if row.generation != generation {
                    rejected = true;
                } else {
                    row.remote_acked = true;
                    commands.push(SideEffect::DeleteOutbox {
                        job_id: job_id.clone(),
                        generation,
                    });
                    restore_slot_after_terminal_job(&mut state, &mut commands, &job_id);
                }
            } else {
                rejected = true;
            }
        }
        Event::JobWorkerLost { job_id, generation } => {
            let job = state.jobs.iter().find(|job| job.job_id == job_id);
            match job {
                Some(job) if job.generation == generation => {
                    // A completing job may still have an outbox payload to
                    // send; the outbox row survives so the send path can
                    // retry — only the lost worker is dropped.
                    restore_slot_after_terminal_job(&mut state, &mut commands, &job_id);
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
            if generation != slot.generation || slot.phase == ActorPhase::Fenced {
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
            let slot = state.slot_mut(&slot_id);
            if generation != slot.generation {
                rejected = true;
            } else {
                slot.phase = ActorPhase::Fenced;
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
            let pending_outbox = state
                .outbox
                .iter()
                .any(|row| row.intended && !row.remote_acked);
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
#[derive(Debug)]
pub struct Journal {
    conn: Connection,
    path: PathBuf,
    /// True when on-disk `user_version` is newer than this binary (N-1 vs N).
    write_blocked: bool,
}

impl Journal {
    /// Open (creating) a journal file. Parent directory must already exist.
    ///
    /// # Errors
    /// Missing parent, SQLite older than the WAL-reset fix, or schema setup.
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
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
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let wal: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if !wal.eq_ignore_ascii_case("wal") {
            return Err(StoreError::new(
                velnor_model::ExitClass::Operation,
                "journal.wal.unavailable",
            )
            .with_remediation("the filesystem must support WAL journaling"));
        }
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        assert_sqlite_version(&conn)?;
        conn.execute_batch(SCHEMA)?;
        let stored: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let stored = u32::try_from(stored).unwrap_or(0);
        let mut write_blocked = stored > JOURNAL_SCHEMA_VERSION;
        let mut verified_during_migration = false;
        if stored == 0 {
            conn.pragma_update(None, "user_version", JOURNAL_SCHEMA_VERSION)?;
        } else if stored == 1 {
            let transaction =
                conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let current: i64 =
                transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            let current = u32::try_from(current).unwrap_or(0);
            if current == 1 {
                transaction.execute(
                    "ALTER TABLE jobs ADD COLUMN accepted_unix INTEGER NOT NULL DEFAULT 0",
                    [],
                )?;
                transaction.pragma_update(None, "user_version", JOURNAL_SCHEMA_VERSION)?;
                // Keep checksum verification and materialization under the
                // same write lock. Another process cannot commit an event
                // between the replay and the v1 rebuild.
                let verified_state = load_state_from_conn(&transaction)?;
                persist_state(&transaction, &verified_state)?;
                verified_during_migration = true;
            } else {
                write_blocked = current > JOURNAL_SCHEMA_VERSION;
            }
            transaction.commit()?;
        }
        let journal = Self {
            conn,
            path: path.to_path_buf(),
            write_blocked,
        };
        // Verify all existing event checksums once. The controller's steady
        // state must not replay an ever-growing log every two seconds.
        if !verified_during_migration {
            journal.load_state()?;
        }
        Ok(journal)
    }

    /// Whether apply is refused because a newer writer owns the schema.
    #[must_use]
    pub fn write_blocked(&self) -> bool {
        self.write_blocked
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

    /// Persist `event` then return the commands. Crash after this returns
    /// still has the intent; crash before it has neither intent nor command.
    ///
    /// # Errors
    /// SQLite write failures.
    pub fn apply(&mut self, event: Event) -> StoreResult<ReduceOutcome> {
        let mut outcomes = self.apply_many(std::iter::once(event))?;
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
        if self.write_blocked {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.schema.newer",
            )
            .with_remediation(
                "N-1 must not write a journal whose PRAGMA user_version is newer than this binary",
            ));
        }
        // Lock before reading materialized state. Controller, job, guardian,
        // and completion processes can overlap; a snapshot taken before the
        // write lock could otherwise clobber a concurrent committed event.
        let transaction = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut state = load_materialized_state(&transaction)?;
        let mut outcomes = Vec::new();
        let mut pending = Vec::new();
        for mut event in events {
            stamp_event(&mut event);
            let outcome = reduce(state.clone(), event.clone());
            if !outcome.rejected {
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
        if pending.is_empty() {
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
        tx.commit()?;
        Ok(outcomes)
    }

    /// Rebuild materialization from the event log (crash recovery).
    ///
    /// # Errors
    /// SQLite or payload decode failures.
    pub fn load_state(&self) -> StoreResult<FleetState> {
        load_state_from_conn(&self.conn)
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
            .filter(|row| row.intended && !row.remote_acked)
            .collect())
    }
}

fn load_state_from_conn(conn: &Connection) -> StoreResult<FleetState> {
    // Journal open succeeded, so the file is writable unless a later apply
    // fails; recovery treats an opened journal as writable.
    let mut state = FleetState {
        journal_writable: true,
        ..FleetState::default()
    };
    let mut stmt = conn.prepare("SELECT payload, checksum FROM events ORDER BY id ASC")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (payload, checksum) = row?;
        if sha256_hex(payload.as_bytes()) != checksum {
            return Err(StoreError::new(
                velnor_model::ExitClass::Conflict,
                "journal.checksum.mismatch",
            )
            .with_remediation("the event log failed integrity verification"));
        }
        let event: Event = match serde_json::from_str(&payload) {
            Ok(event) => event,
            Err(_) => {
                // Newer writer's unknown envelope: N-1 skips it. Checksum
                // already matched, so this is not corruption.
                continue;
            }
        };
        let outcome = reduce(state, event);
        state = outcome.state;
    }
    Ok(state)
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
    state.surge = meta_u32(&meta, "surge")?;
    state.canary = meta_canary(&meta)?;
    state.package_generation = meta_u64(&meta, "package_generation")?;
    state.package_apt_version = meta.get("package_apt_version").cloned().unwrap_or_default();

    let mut statement = conn.prepare(
        "SELECT slot_id, generation, phase, permit_held, routing_valid,
                session_live, executor_proven, registered, pid, heartbeat_unix, surge
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
            row.get::<_, i64>(10)?,
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
            surge,
        ) = row?;
        state.slots.push(SlotRecord {
            slot_id: SlotId(slot_id),
            generation: Generation(i64_u64(generation, "slot generation")?),
            phase: parse_actor_phase(&phase)?,
            permit_held: sqlite_bool(permit_held, "slot permit_held")?,
            routing_valid: sqlite_bool(routing_valid, "slot routing_valid")?,
            session_live: sqlite_bool(session_live, "slot session_live")?,
            executor_proven: sqlite_bool(executor_proven, "slot executor_proven")?,
            registered: sqlite_bool(registered, "slot registered")?,
            pid: pid.map(|value| i64_u32(value, "slot pid")).transpose()?,
            heartbeat_unix: i64_u64(heartbeat_unix, "slot heartbeat_unix")?,
            surge: sqlite_bool(surge, "slot surge")?,
        });
    }

    let mut statement = conn.prepare(
        "SELECT job_id, slot_id, generation, attempt, worker, phase, accepted_unix
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
        ))
    })?;
    for row in rows {
        let (job_id, slot_id, generation, attempt, worker, phase, accepted_unix) = row?;
        state.jobs.push(JobRecord {
            job_id: JobId(job_id),
            slot_id: SlotId(slot_id),
            generation: Generation(i64_u64(generation, "job generation")?),
            attempt: i64_u32(attempt, "job attempt")?,
            worker,
            phase: parse_actor_phase(&phase)?,
            accepted_unix: i64_u64(accepted_unix, "job accepted_unix")?,
        });
    }

    let mut statement = conn.prepare(
        "SELECT job_id, generation, payload_sha256, intended, send_started,
                remote_acked, created_unix
         FROM outbox ORDER BY rowid",
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
        ))
    })?;
    for row in rows {
        let (
            job_id,
            generation,
            payload_sha256,
            intended,
            send_started,
            remote_acked,
            created_unix,
        ) = row?;
        state.outbox.push(OutboxRecord {
            job_id: JobId(job_id),
            generation: Generation(i64_u64(generation, "outbox generation")?),
            payload_sha256,
            intended: sqlite_bool(intended, "outbox intended")?,
            send_started: sqlite_bool(send_started, "outbox send_started")?,
            remote_acked: sqlite_bool(remote_acked, "outbox remote_acked")?,
            created_unix: i64_u64(created_unix, "outbox created_unix")?,
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

fn parse_actor_phase(value: &str) -> StoreResult<ActorPhase> {
    match value {
        "absent" => Ok(ActorPhase::Absent),
        "provisioning" => Ok(ActorPhase::Provisioning),
        "registered" => Ok(ActorPhase::Registered),
        "ready" => Ok(ActorPhase::Ready),
        "assigned" => Ok(ActorPhase::Assigned),
        "starting" => Ok(ActorPhase::Starting),
        "running" => Ok(ActorPhase::Running),
        "completing" => Ok(ActorPhase::Completing),
        "retiring" => Ok(ActorPhase::Retiring),
        "degraded" => Ok(ActorPhase::Degraded),
        "fenced" => Ok(ActorPhase::Fenced),
        "quarantined" => Ok(ActorPhase::Quarantined),
        _ => Err(invalid_materialized("actor phase", value)),
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

fn persist_state(tx: &rusqlite::Transaction<'_>, state: &FleetState) -> StoreResult<()> {
    tx.execute("DELETE FROM slots", [])?;
    tx.execute("DELETE FROM jobs", [])?;
    tx.execute("DELETE FROM outbox", [])?;
    tx.execute("DELETE FROM meta", [])?;
    for slot in &state.slots {
        tx.execute(
            "INSERT INTO slots (
                slot_id, generation, phase, permit_held, routing_valid, session_live,
                executor_proven, registered, pid, heartbeat_unix, surge
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
                slot.surge as i64,
            ],
        )?;
    }
    for job in &state.jobs {
        tx.execute(
            "INSERT INTO jobs (
                job_id, slot_id, generation, attempt, worker, phase, accepted_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                job.job_id.0,
                job.slot_id.0,
                job.generation.0 as i64,
                job.attempt as i64,
                job.worker,
                job.phase.as_str(),
                job.accepted_unix as i64,
            ],
        )?;
    }
    for row in &state.outbox {
        if row.remote_acked {
            continue;
        }
        tx.execute(
            "INSERT INTO outbox (
                job_id, generation, payload_sha256, intended, send_started, remote_acked, created_unix
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                row.job_id.0,
                row.generation.0 as i64,
                row.payload_sha256,
                row.intended as i64,
                row.send_started as i64,
                row.remote_acked as i64,
                row.created_unix as i64,
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
        ("surge", state.surge.to_string()),
        ("canary", state.canary.as_str().to_owned()),
        ("package_generation", state.package_generation.to_string()),
        ("package_apt_version", state.package_apt_version.clone()),
    ];
    for (key, value) in meta {
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
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
        | Event::Assigned { generation, .. }
        | Event::JobOwned { generation, .. }
        | Event::JobStarted { generation, .. }
        | Event::CompletionIntended { generation, .. }
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
        Event::Assigned { .. } => "assigned",
        Event::JobOwned { .. } => "job_owned",
        Event::JobStarted { .. } => "job_started",
        Event::CompletionIntended { .. } => "completion_intended",
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
mod tests {
    use super::*;

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

    fn slot(id: &str) -> SlotId {
        SlotId(id.to_owned())
    }

    fn job(id: &str) -> JobId {
        JobId(id.to_owned())
    }

    fn gen() -> Generation {
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
        let g = gen();
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
            Event::DesiredCapacity { ready: 1, surge: 1 },
            Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
                surge: false,
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
                    generation: gen(),
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
    fn apply_many_elides_repeated_proof_and_routing_events() {
        let (_dir, mut journal) = open_tmp("batch-no-op");
        prime_ready(&mut journal, "scope-1");
        let before = event_count(&journal);
        let outcomes = journal
            .apply_many([
                Event::ExecutorProven {
                    slot_id: slot("scope-1"),
                    generation: gen(),
                },
                Event::SessionLive {
                    slot_id: slot("scope-1"),
                    generation: gen(),
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
    fn apply_many_persists_command_bearing_registration_intent() {
        let (dir, mut journal) = open_tmp("batch-command-bearing");
        let s = slot("scope-1");
        let g = gen();
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
                surge: false,
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
    fn newer_permit_generation_resets_fenced_actor_identity_and_proofs() {
        let (_dir, mut journal) = open_tmp("new-generation-reset");
        let slot_id = slot("scope-1");
        let generation = gen();
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
                surge: true,
            })
            .unwrap();
        assert!(same_generation.rejected);
        assert!(same_generation.commands.is_empty());
        assert_eq!(same_generation.state.slots[0].phase, ActorPhase::Fenced);
        let outcome = journal
            .apply(Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation: generation.next(),
                surge: true,
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
        assert_eq!(slot.phase, ActorPhase::Provisioning);
        assert!(slot.permit_held);
        assert!(slot.surge);
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
        let generation = gen();
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
            assert_eq!(outcome.state.slots[0].phase, ActorPhase::Fenced);
        }
    }

    #[test]
    fn permit_rotation_rejects_active_job_on_slot() {
        let (_dir, mut journal) = open_tmp("fenced-active-job");
        let slot_id = slot("scope-1");
        let generation = gen();
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
                .apply(Event::Assigned {
                    slot_id: slot_id.clone(),
                    job_id: job("job-1"),
                    generation,
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
                surge: true,
            })
            .unwrap();
        assert!(outcome.rejected);
        assert!(outcome.commands.is_empty());
        assert_eq!(outcome.state.slots[0].generation, generation);
        assert_eq!(outcome.state.slots[0].phase, ActorPhase::Assigned);
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
                    generation: gen(),
                })
                .unwrap()
                .rejected
        );
        let job_id = job("job-1");
        assert!(
            !journal
                .apply(Event::Assigned {
                    slot_id: slot_id.clone(),
                    job_id: job_id.clone(),
                    generation: gen(),
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
                    generation: gen(),
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
    fn v1_journal_migration_adds_materialized_queue_timestamp_column() {
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

        let journal = Journal::open(&path).unwrap();
        let version: u32 = journal
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, JOURNAL_SCHEMA_VERSION);
        let has_timestamp: bool = journal
            .conn
            .prepare("PRAGMA table_info(jobs)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(|name| name.unwrap())
            .any(|name| name == "accepted_unix");
        assert!(has_timestamp);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn registration_lost_clears_stale_local_identity() {
        let (_dir, mut journal) = open_tmp("registration-lost");
        prime_ready(&mut journal, "scope-1");
        let outcome = journal
            .apply(Event::RegistrationLost {
                slot_id: slot("scope-1"),
                generation: gen(),
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
        assert_eq!(slot.phase, ActorPhase::Provisioning);
    }

    #[test]
    fn ready_requires_permit_routing_session_and_executor() {
        let (_dir, mut journal) = open_tmp("not-ready");
        let s = slot("scope-1");
        let g = gen();
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        journal
            .apply(Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
                surge: false,
            })
            .unwrap();
        let outcome = journal
            .apply(Event::ReadyAttempt {
                slot_id: s,
                generation: g,
            })
            .unwrap();
        assert!(outcome.rejected);
        assert_eq!(outcome.state.slots[0].phase, ActorPhase::Provisioning);
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
        let g = gen();
        journal.apply(Event::ControlLive).unwrap();
        journal.apply(Event::JournalWritable).unwrap();
        journal
            .apply(Event::PermitReserved {
                slot_id: s.clone(),
                generation: g,
                surge: false,
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
        let g = gen();
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
                surge: false,
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
    fn job_ownership_survives_reopen_before_start_command() {
        let (dir, mut journal) = open_tmp("job-own");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: gen(),
            })
            .unwrap();
        let outcome = journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: gen(),
                worker: "velnor-job@job-1".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        assert!(matches!(
            outcome.commands.as_slice(),
            [SideEffect::StartJob { .. }]
        ));
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
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: gen(),
                worker: "w".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        let checksum = payload_checksum(b"conclusion=success");
        // Kill before send: intent is durable, outbox pending.
        journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: gen(),
                payload_sha256: checksum.clone(),
            })
            .unwrap();
        assert_eq!(journal.pending_outbox().unwrap().len(), 1);
        // Kill during send.
        journal
            .apply(Event::CompletionSendStarted {
                job_id: job("job-1"),
                generation: gen(),
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
                generation: gen(),
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
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: gen(),
                worker: "w".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        // Worker killed mid-run: no completion intent, no outbox — the only
        // terminal path is JobWorkerLost.
        let outcome = journal
            .apply(Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: gen(),
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
                |record| record.slot_id == slot("scope-1") && record.phase == ActorPhase::Ready
            ),
            "{state:?}"
        );
        // Losing the same worker twice is rejected, not duplicated.
        let repeat = journal
            .apply(Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: gen(),
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
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: gen(),
                worker: "w".to_owned(),
                accepted_unix: 0,
            })
            .unwrap();
        journal
            .apply(Event::JobWorkerLost {
                job_id: job("job-1"),
                generation: gen(),
            })
            .unwrap();
        let newer = Generation(2);
        journal
            .apply(Event::PermitReserved {
                slot_id: slot("scope-1"),
                generation: newer,
                surge: false,
            })
            .unwrap();
        let complete = journal
            .apply(Event::CompletionIntended {
                job_id: job("job-1"),
                generation: gen(),
                payload_sha256: payload_checksum(b"nope"),
            })
            .unwrap();
        assert!(complete.rejected);
        let cleanup = journal
            .apply(Event::CleanupIntended {
                slot_id: slot("scope-1"),
                isolation_id: "job-1".to_owned(),
                generation: gen(),
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
                generation: gen(),
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
        drop(conn);
        let mut older = Journal::open(&path).unwrap();
        assert!(older.write_blocked());
        assert!(older.apply(Event::ControlLive).is_err());
        assert!(older.load_state().unwrap().journal_writable);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_event_kind_does_not_drop_known_state() {
        let (dir, mut journal) = open_tmp("unk");
        journal.apply(Event::ControlLive).unwrap();
        drop(journal);
        let path = dir.join("journal.db");
        let conn = Connection::open(&path).unwrap();
        let payload = r#"{"type":"future_envelope","x":1}"#;
        let checksum = payload_checksum(payload.as_bytes());
        conn.execute(
            "INSERT INTO events (generation, kind, payload, checksum) VALUES (0, 'future_envelope', ?1, ?2)",
            params![payload, checksum],
        )
        .unwrap();
        drop(conn);
        let state = Journal::open(&path).unwrap().load_state().unwrap();
        assert!(state.control_live);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn second_live_job_on_assigned_slot_is_rejected() {
        let (_dir, mut journal) = open_tmp("two-jobs");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: gen(),
            })
            .unwrap();
        assert!(
            !journal
                .apply(Event::Assigned {
                    slot_id: slot("scope-1"),
                    job_id: job("guid-1"),
                    generation: gen(),
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
                    generation: gen(),
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
                generation: gen(),
                worker: "w2".into(),
                accepted_unix: 0,
            })
            .unwrap();
        assert!(second.rejected);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job("guid-1"));
        assert_eq!(state.slots[0].phase, ActorPhase::Assigned);
    }

    #[test]
    fn completion_intended_marks_completing_and_counts_queued_age() {
        let (_dir, mut journal) = open_tmp("complete-phase");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("guid-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        let owned = journal.load_state().unwrap();
        assert!(owned.health().oldest_queued_job_seconds < 60, "{owned:?}");
        journal
            .apply(Event::CompletionIntended {
                job_id: job("guid-1"),
                generation: gen(),
                payload_sha256: payload_checksum(b"ok"),
            })
            .unwrap();
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs[0].phase, ActorPhase::Completing);
        assert!(state.jobs[0].accepted_unix > 0);
    }

    #[test]
    fn remote_ack_restores_ready() {
        let (_dir, mut journal) = open_tmp("ack-ready");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("guid-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("guid-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: gen(),
                worker: "w".into(),
                accepted_unix: 0,
            })
            .unwrap();
        journal
            .apply(Event::CompletionIntended {
                job_id: job("guid-1"),
                generation: gen(),
                payload_sha256: payload_checksum(b"ok"),
            })
            .unwrap();
        let acked = journal
            .apply(Event::RemoteAcked {
                job_id: job("guid-1"),
                generation: gen(),
            })
            .unwrap();
        assert!(!acked.rejected);
        let state = journal.load_state().unwrap();
        assert!(state.jobs.is_empty(), "{:?}", state.jobs);
        assert_eq!(state.slots[0].phase, ActorPhase::Ready);
        assert!(state.slots[0].phase.counts_as_ready());
    }

    #[test]
    fn duplicate_assignment_is_rejected() {
        let (_dir, mut journal) = open_tmp("dup-assign");
        prime_ready(&mut journal, "scope-1");
        journal
            .apply(Event::ReadyAttempt {
                slot_id: slot("scope-1"),
                generation: gen(),
            })
            .unwrap();
        let first = journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: gen(),
            })
            .unwrap();
        assert!(!first.rejected);
        let second = journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("job-2"),
                generation: gen(),
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
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::Assigned {
                slot_id: slot("scope-1"),
                job_id: job("job-1"),
                generation: gen(),
            })
            .unwrap();
        journal
            .apply(Event::JobOwned {
                job_id: job("job-1"),
                slot_id: slot("scope-1"),
                attempt: 1,
                generation: gen(),
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
