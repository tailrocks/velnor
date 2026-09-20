//! Reconciliation (§5.1 step 9 + startup gate).
//!
//! * [`startup`]: reconcile-before-advertise after (re)start. Stale-epoch
//!   grants reset, the ledger reconciles against the attested live set
//!   (native rows pass through untouched — this adapter never judges the
//!   other lane — plus scale-set demand in permit states), and crash
//!   orphaned `intended` batches move to `uncertain`. Only then may the
//!   listener advertise capacity.
//! * [`idle_poll`]: bounded uncertain resolution on 202/None polls. Members
//!   observed `acquired`/`terminal` resolve their batch; the oldest batch
//!   still uncertain past [`UNCERTAIN_REACQUIRE_AFTER`] is re-acquired once
//!   (same intent, authoritative answer), one batch per idle poll.
//! * [`unknown_event`]: future server message types are counted and logged,
//!   never fatal.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::scaleset::capacity::{
    AcquireOutcome, CapacityLedger, LedgerDemandObservation, LedgerLane, LedgerPermitState,
};
use crate::scaleset::demand::{DemandState, DemandStore};
use crate::scaleset::intents::{permit_holder, reconcile_returned_ids, AcquireBatchStore};
use crate::scaleset::metrics::Metrics;
use crate::scaleset::scale::QueueSession;
use crate::scaleset::shared_ledger::to_control_state;

/// How long an uncertain batch waits for `JobAssigned`/`JobCompleted`
/// observations before idle reconcile re-acquires it.
pub const UNCERTAIN_REACQUIRE_AFTER: Duration = Duration::from_secs(60);

/// Demand states that attest a live scale-set holder at startup. This is
/// the attestation set, NOT the convergence population: `granted` rows
/// attest (a crash between reserve and `acquire_intent` leaves a granted
/// row holding a permit, and unattested-but-granted rows adopt their
/// about-to-reserve permit early — retention direction), while step-7
/// convergence excludes `granted` (it is the acquire pass's input).
pub(crate) const PERMIT_STATES: [DemandState; 7] = [
    DemandState::Granted,
    DemandState::AcquireIntent,
    DemandState::Acquired,
    DemandState::Uncertain,
    DemandState::CanceledPending,
    DemandState::CanceledAcquired,
    DemandState::ProvisionIntent,
];

pub(crate) fn permit_state_for_demand(state: DemandState) -> LedgerPermitState {
    match state {
        DemandState::Granted => LedgerPermitState::Reserved,
        DemandState::AcquireIntent | DemandState::Acquired | DemandState::CanceledAcquired => {
            LedgerPermitState::Acquiring
        }
        DemandState::Uncertain | DemandState::CanceledPending => LedgerPermitState::Uncertain,
        DemandState::ProvisionIntent => LedgerPermitState::Provisioning,
        DemandState::Observed
        | DemandState::Eligible
        | DemandState::Declined
        | DemandState::CanceledDone
        | DemandState::Terminal => LedgerPermitState::Uncertain,
    }
}

/// What [`startup`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupReport {
    pub generation: u64,
    pub stale_grants_reset: u64,
    /// Acquire intents with no batch are pre-network crash orphans: the
    /// request could not have been sent, so their reservation was released.
    pub unbatched_acquire_intents_recovered: u64,
    pub adopted: Vec<String>,
    pub marked_uncertain: Vec<String>,
    pub confirmed: Vec<String>,
    pub batches_orphaned: u64,
}

/// Daemon-startup attestation input: every scale-set demand row in a
/// permit state, across all sets, as `(holder, control permit state)`.
/// The daemon feeds this plus its native markers into the ONE startup
/// reconcile so neither lane's live rows go uncertain spuriously.
pub(crate) fn attest_demand_holders(
    state_db: &Path,
) -> Result<Vec<(String, velnor_control::permit_ledger::PermitState)>> {
    let (_lock, attested) = lock_and_attest_demand_holders(state_db)?;
    Ok(attested)
}

/// SQLite writer reservation held through the host ledger commit. Without
/// it, cleanup can release a permit after attestation and before stale
/// snapshot reconciliation adopts the same holder again.
pub(crate) struct DemandSourceReconcileLock {
    _connection: Connection,
    _lifecycle_lock: DemandSourceLifecycleLock,
}

/// Cross-database lifecycle fence shared by host reconciliation and worker
/// terminal cleanup. SQLite cannot atomically commit the worker row and host
/// permit, so both operations hold this stable lock through their commit
/// sequence and order the durable state to make retries safe.
pub(crate) struct DemandSourceLifecycleLock {
    _file: File,
}

/// Exclusive fence for Docker operations on one durable worker identity.
///
/// The descriptor is intentionally inheritable: a synchronous Docker CLI
/// child keeps the flock alive if its lane process exits while an Engine
/// request is outstanding. An ambiguous create also remains marked pending
/// in the worker registry, so losing the last descriptor can never authorize
/// RunnerAbsent cleanup by itself.
pub(crate) struct WorkerDockerLifecycleLock {
    _file: File,
    ownership_id: String,
    stripe: u64,
}

impl WorkerDockerLifecycleLock {
    /// Check that this held descriptor was acquired for this exact worker.
    pub(crate) fn authorizes(&self, ownership_id: &str) -> bool {
        self.ownership_id == ownership_id
            && self.stripe == worker_docker_lifecycle_stripe(ownership_id)
    }
}

fn worker_docker_lifecycle_stripe(ownership_id: &str) -> u64 {
    const STRIPES: u64 = 128;
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    ownership_id
        .as_bytes()
        .iter()
        .fold(FNV_OFFSET, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        })
        % STRIPES
}

/// Lock one stable stripe derived from the host ledger and worker identity.
/// The finite stripe set avoids an unbounded pile of lock files while keeping
/// every daemon using the same canonical host ledger on the same lock.
pub(crate) fn lock_worker_docker_lifecycle(
    ledger_path: &Path,
    ownership_id: &str,
) -> Result<WorkerDockerLifecycleLock> {
    let ledger_path = ledger_path
        .canonicalize()
        .with_context(|| format!("canonicalize host permit ledger {}", ledger_path.display()))?;
    let parent = ledger_path
        .parent()
        .context("host permit ledger has no parent directory")?;
    let stripe = worker_docker_lifecycle_stripe(ownership_id);
    let lock_path = parent.join(format!(".velnor-worker-lifecycle-{stripe:03}.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("open worker Docker lifecycle lock {}", lock_path.display()))?;
    let metadata = file.metadata().with_context(|| {
        format!(
            "inspect worker Docker lifecycle lock {}",
            lock_path.display()
        )
    })?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        anyhow::bail!(
            "worker Docker lifecycle lock {} is not a private regular file owned by this daemon",
            lock_path.display()
        );
    }
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .with_context(|| format!("lock worker Docker lifecycle for {ownership_id:?}"))?;

    // Keep the lock attached to synchronous Docker children. If a lane dies
    // after sending a create request, terminal cleanup waits while the CLI
    // is still alive; if the CLI itself dies, the durable create-pending bit
    // remains the fail-closed proof and prevents a no-runner release.
    let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .context("read worker lifecycle lock descriptor flags");
    }
    if unsafe {
        libc::fcntl(
            file.as_raw_fd(),
            libc::F_SETFD,
            descriptor & !libc::FD_CLOEXEC,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error())
            .context("make worker lifecycle lock inheritable by Docker CLI");
    }
    Ok(WorkerDockerLifecycleLock {
        _file: file,
        ownership_id: ownership_id.to_owned(),
        stripe,
    })
}

pub(crate) fn lock_demand_source_lifecycle(state_db: &Path) -> Result<DemandSourceLifecycleLock> {
    let mut lock_name = state_db.as_os_str().to_os_string();
    lock_name.push(".permit-source.lock");
    let lock_path = std::path::PathBuf::from(lock_name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("open demand source lifecycle lock {}", lock_path.display()))?;
    if !file
        .metadata()
        .with_context(|| {
            format!(
                "inspect demand source lifecycle lock {}",
                lock_path.display()
            )
        })?
        .is_file()
    {
        anyhow::bail!(
            "demand source lifecycle lock {} is not a regular file",
            lock_path.display()
        );
    }
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .with_context(|| format!("lock demand source lifecycle {}", state_db.display()))?;
    Ok(DemandSourceLifecycleLock { _file: file })
}

pub(crate) fn lock_and_attest_demand_holders(
    state_db: &Path,
) -> Result<(
    DemandSourceReconcileLock,
    Vec<(String, velnor_control::permit_ledger::PermitState)>,
)> {
    let lifecycle_lock = lock_demand_source_lifecycle(state_db)?;
    // Apply one-time schema migrations before holding the writer reservation.
    let demand = DemandStore::open(state_db)?;
    drop(demand);
    let registry = crate::scaleset::WorkerRegistry::open(state_db)?;
    drop(registry);
    let connection = Connection::open(state_db)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch("BEGIN IMMEDIATE")?;

    let mut scale_sets = std::collections::HashMap::new();
    {
        let mut statement =
            connection.prepare("SELECT request_id, scale_set_id FROM scaleset_demand")?;
        for row in
            statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i32>(1)?)))?
        {
            let (request_id, scale_set_id) = row?;
            scale_sets.insert(request_id, scale_set_id);
        }
    }

    let mut attested = std::collections::BTreeMap::new();
    {
        let mut statement = connection.prepare(
            "SELECT request_id, state FROM scaleset_demand
             WHERE state IN ('granted', 'acquire_intent', 'acquired', 'uncertain', 'provision_intent')",
        )?;
        for row in statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })? {
            let (request_id, raw_state) = row?;
            let scale_set_id = scale_sets.get(&request_id).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "permit-state demand request {request_id} is absent in {}",
                    state_db.display()
                )
            })?;
            let state = match raw_state.as_str() {
                "granted" => velnor_control::permit_ledger::PermitState::Reserved,
                "acquire_intent" | "acquired" => {
                    velnor_control::permit_ledger::PermitState::Acquiring
                }
                "uncertain" => velnor_control::permit_ledger::PermitState::Uncertain,
                "provision_intent" => velnor_control::permit_ledger::PermitState::Provisioning,
                _ => anyhow::bail!("unknown permit-state demand {raw_state:?}"),
            };
            attested.insert(permit_holder(scale_set_id, request_id), state);
        }
    }

    // Worker rows remain occupancy evidence after demand becomes terminal,
    // through diagnostics export, owned cleanup, and permit release.
    {
        let mut statement = connection.prepare(
            "SELECT ownership_id, request_id, worker_state FROM scaleset_workers
             WHERE worker_state != 'permit_released' ORDER BY created_at, ownership_id",
        )?;
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (ownership_id, request_id, worker_state) = row?;
            let request_id = request_id.with_context(|| {
                format!(
                    "live scale-set worker {ownership_id} has no request id in {}",
                    state_db.display()
                )
            })?;
            let scale_set_id = scale_sets.get(&request_id).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "live scale-set worker {ownership_id} references missing demand request {request_id} in {}",
                    state_db.display()
                )
            })?;
            let state = match worker_state.as_str() {
                "provision_intent" => velnor_control::permit_ledger::PermitState::Provisioning,
                "dind_ready" | "runner_connected" | "running" => {
                    velnor_control::permit_ledger::PermitState::Running
                }
                "terminal" | "diagnostic_export" | "owned_cleanup" => {
                    velnor_control::permit_ledger::PermitState::Cleaning
                }
                "observed" | "eligible" | "reserved" | "acquire_intent" | "acquired"
                | "uncertain" => velnor_control::permit_ledger::PermitState::Uncertain,
                "permit_released" => continue,
                _ => anyhow::bail!("unknown live Scale Set worker state {worker_state:?}"),
            };
            attested.insert(permit_holder(scale_set_id, request_id), state);
        }
    }

    Ok((
        DemandSourceReconcileLock {
            _connection: connection,
            _lifecycle_lock: lifecycle_lock,
        },
        attested.into_iter().collect(),
    ))
}

/// Restore every persisted eligible offer to the host-wide queue. Called
/// from lane startup before native slots are supervised; one ledger batch
/// prevents a peer acquisition from seeing only part of the restored queue.
pub(crate) fn backfill_eligible_demands<L: CapacityLedger>(
    demand: &DemandStore,
    ledger: &mut L,
) -> Result<()> {
    let mut observations = Vec::new();
    for row in demand.list_eligible_all()? {
        let first_seen = velnor_model::Timestamp::parse(&row.first_seen_at).with_context(|| {
            format!(
                "parse durable Scale Set first-seen time for request {}",
                row.request_id
            )
        })?;
        let instant = first_seen.as_offset_datetime();
        let first_seen_unix = u64::try_from(instant.unix_timestamp()).with_context(|| {
            format!(
                "Scale Set first-seen time predates Unix epoch for request {}",
                row.request_id
            )
        })?;
        let holder = permit_holder(row.scale_set_id, row.request_id);
        let scope = format!("scaleset/{}", row.scale_set_id);
        observations.push((
            row.sequence,
            LedgerDemandObservation {
                holder,
                lane: LedgerLane::ScaleSet,
                scope,
                first_seen_unix,
                first_seen_subsec_nanos: instant.nanosecond(),
            },
        ));
    }
    observations.sort_by_key(|(sequence, demand)| {
        (
            demand.first_seen_unix,
            demand.first_seen_subsec_nanos,
            *sequence,
        )
    });
    if observations.is_empty() {
        return Ok(());
    }
    let observations = observations
        .into_iter()
        .map(|(_, demand)| demand)
        .collect::<Vec<_>>();
    ledger
        .observe_demands(&observations, velnor_control::permit_ledger::unix_now())
        .map_err(|error| anyhow::anyhow!("backfill persisted Scale Set demand batch: {error}"))?;
    Ok(())
}

/// Reconcile-before-advertise: run once at adapter start (and after every
/// epoch bump) before the first poll advertises capacity.
pub fn startup<L: CapacityLedger>(
    ledger: &mut L,
    demand: &mut DemandStore,
    batches: &mut AcquireBatchStore,
    scale_set_id: i32,
    metrics: &Metrics,
) -> Result<StartupReport> {
    let generation = ledger
        .generation()
        .map_err(|error| anyhow::anyhow!("read ledger generation: {error}"))?;

    // An unbatched acquire intent cannot have crossed the network boundary:
    // both the previous row-first writer and the current batch-first writer
    // persist the acquire batch before calling `acquirejobs`. Return these
    // crash orphans to eligibility before ledger attestation.
    let mut unbatched_acquire_intents_recovered = 0;
    for (request_id, state) in demand.list_in_states(
        scale_set_id,
        &[DemandState::AcquireIntent, DemandState::CanceledPending],
    )? {
        if !batches.contains_request(scale_set_id, request_id)? {
            let holder = permit_holder(scale_set_id, request_id);
            let lease_generation = exact_demand_lease_generation(ledger, demand, request_id)?;
            if state == DemandState::CanceledPending {
                demand.set_state(request_id, DemandState::CanceledDone, None, generation)?;
                if !ledger.release_cancelled_if_generation(&holder, lease_generation)? {
                    anyhow::bail!(
                        "could not release exact canceled Scale Set lease {lease_generation} for request {request_id}"
                    );
                }
            } else {
                demand.set_state(request_id, DemandState::Eligible, None, generation)?;
                if !ledger.release_to_eligible_if_generation(&holder, lease_generation)? {
                    anyhow::bail!(
                        "could not requeue exact Scale Set lease {lease_generation} for request {request_id}"
                    );
                }
            }
            unbatched_acquire_intents_recovered += 1;
        }
    }

    // Open batches are the durable network boundary. Mark members uncertain
    // before stale-generation reset so a crash between batch and demand-row
    // writes cannot be mistaken for an unsent grant.
    let open_batches = batches.open_batches(scale_set_id, usize::MAX)?;
    let mut batches_orphaned = 0u64;
    for batch in &open_batches {
        if batch.state == crate::scaleset::intents::BatchState::Intended {
            batches.resolve(&batch.batch_id, true)?;
            for request_id in &batch.request_ids {
                if let Some(row) = demand.get(*request_id)?
                    && matches!(row.state, DemandState::Granted | DemandState::AcquireIntent)
                {
                    demand.set_state(*request_id, DemandState::Uncertain, None, generation)?;
                }
            }
            batches_orphaned += 1;
        }
    }

    let stale_grants_reset = demand.reset_stale_grants(scale_set_id, generation)?;
    metrics.add_stale_grants_reset(stale_grants_reset);

    // Attested live set: every recorded native holder passes through as-is
    // (the other lane's truth is not ours to revise) plus scale-set demand
    // in permit states. Recorded-but-unattested holders are marked
    // uncertain (still counted) — reconcile never deletes.
    let mut alive: Vec<(String, LedgerLane, LedgerPermitState)> = Vec::new();
    let holders = ledger
        .holders()
        .map_err(|error| anyhow::anyhow!("list ledger holders: {error}"))?;
    for holder in &holders {
        if holder.lane == LedgerLane::Native {
            alive.push((holder.holder.clone(), holder.lane, holder.state));
        }
    }
    for (request_id, state) in demand.list_in_states(scale_set_id, &PERMIT_STATES)? {
        alive.push((
            permit_holder(scale_set_id, request_id),
            LedgerLane::ScaleSet,
            permit_state_for_demand(state),
        ));
    }
    let alive_refs: Vec<(&str, LedgerLane, LedgerPermitState)> = alive
        .iter()
        .map(|(holder, lane, state)| (holder.as_str(), *lane, *state))
        .collect();
    let report = ledger
        .reconcile_host_sources(generation, demand.path(), &alive_refs)
        .map_err(|error| anyhow::anyhow!("reconcile full host roster: {error}"))?;

    // Pre-lease-schema rows may lack the immutable acquisition pointer.
    // Backfill it only after full roster reconciliation has proved a held
    // permit for each live local request. A mismatch stays fail-closed.
    for (request_id, _) in demand.list_in_states(scale_set_id, &PERMIT_STATES)? {
        let _ = exact_demand_lease_generation(ledger, demand, request_id)?;
    }

    metrics.inc_reconcile_runs();
    Ok(StartupReport {
        generation,
        stale_grants_reset,
        unbatched_acquire_intents_recovered,
        adopted: report.adopted,
        marked_uncertain: report.marked_uncertain,
        confirmed: report.confirmed,
        batches_orphaned,
    })
}

/// What [`idle_poll`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdleReport {
    pub batches_resolved: u64,
    pub reacquired_batches: u64,
    pub reacquire_failed: bool,
}

fn batch_age(created_at: &str) -> Option<Duration> {
    let created = velnor_model::Timestamp::parse(created_at).ok()?;
    let age_secs = velnor_model::Timestamp::now()
        .as_offset_datetime()
        .unix_timestamp()
        - created.as_offset_datetime().unix_timestamp();
    u64::try_from(age_secs.max(0)).ok().map(Duration::from_secs)
}

/// Bounded idle-poll reconcile: resolve uncertain batches from observations
/// first, then re-acquire at most one overdue batch. Never fails the poll:
/// a failed re-acquire stays uncertain for the next idle round.
pub async fn idle_poll<Q: QueueSession, L: CapacityLedger>(
    queue: &Q,
    ledger: &mut L,
    demand: &mut DemandStore,
    batches: &mut AcquireBatchStore,
    scale_set_id: i32,
    generation: u64,
    metrics: &Metrics,
) -> Result<IdleReport> {
    metrics.inc_reconcile_runs();
    let mut report = IdleReport::default();
    let open = batches.open_batches(scale_set_id, 16)?;
    for batch in &open {
        if resolve_from_observations(ledger, demand, batches, batch, generation).await? {
            report.batches_resolved += 1;
        }
    }
    let overdue = open.iter().find(|batch| {
        batch_age(&batch.created_at).is_none_or(|age| age >= UNCERTAIN_REACQUIRE_AFTER)
    });
    if let Some(batch) = overdue {
        // Re-check: observations above may have resolved it already.
        let fresh = batches.get(&batch.batch_id)?;
        let still_open =
            fresh.is_some_and(|row| row.state == crate::scaleset::intents::BatchState::Uncertain);
        if still_open
            && reacquire_batch(queue, ledger, demand, batches, batch, generation, metrics).await?
        {
            report.reacquired_batches += 1;
        } else if still_open {
            report.reacquire_failed = true;
        }
    }
    Ok(report)
}

/// Resolve one batch when no member remains uncertain: members observed
/// `acquired` confirm the grant, members observed `terminal` ran elsewhere
/// (permits released — nothing was ever provisioned for an uncertain
/// member). Returns whether the batch resolved.
#[allow(clippy::too_many_arguments, reason = "single idle-resolve call site")]
async fn resolve_from_observations<L: CapacityLedger>(
    ledger: &mut L,
    demand: &mut DemandStore,
    batches: &mut AcquireBatchStore,
    batch: &crate::scaleset::intents::AcquireBatch,
    generation: u64,
) -> Result<bool> {
    let mut states = Vec::with_capacity(batch.request_ids.len());
    for request_id in &batch.request_ids {
        states.push(demand.get(*request_id)?);
    }
    if states.iter().any(|row| {
        row.as_ref().is_some_and(|row| {
            matches!(
                row.state,
                DemandState::Uncertain | DemandState::CanceledPending
            )
        })
    }) {
        return Ok(false);
    }
    for (request_id, row) in batch.request_ids.iter().zip(states.iter()) {
        let Some(row) = row else { continue };
        let holder = permit_holder(batch.scale_set_id, *request_id);
        match row.state {
            DemandState::Acquired => {
                transition_demand_lease(
                    ledger,
                    demand,
                    *request_id,
                    &holder,
                    LedgerPermitState::Acquiring,
                )?;
            }
            DemandState::CanceledAcquired => {
                transition_demand_lease(
                    ledger,
                    demand,
                    *request_id,
                    &holder,
                    LedgerPermitState::Acquiring,
                )?;
            }
            // The terminal handler owns worker cleanup and permit release.
            // A completion message alone is not cleanup confirmation.
            DemandState::Terminal => {}
            _ => {}
        }
    }
    batches.resolve(&batch.batch_id, false)?;
    Ok(true)
}

fn transition_demand_lease<L: CapacityLedger>(
    ledger: &mut L,
    demand: &mut DemandStore,
    request_id: i64,
    holder: &str,
    state: LedgerPermitState,
) -> Result<()> {
    let lease_generation = exact_demand_lease_generation(ledger, demand, request_id)?;
    if ledger.transition_if_lease_generation(holder, state, lease_generation)? {
        Ok(())
    } else {
        anyhow::bail!(
            "Scale Set request {request_id} no longer owns exact permit lease {lease_generation}"
        )
    }
}

/// Resolve one immutable permit lease. A missing local pointer is repaired
/// only from a currently held ledger row; a pointer with no held row remains
/// useful only as an idempotent release receipt.
fn exact_demand_lease_generation<L: CapacityLedger>(
    ledger: &L,
    demand: &mut DemandStore,
    request_id: i64,
) -> Result<u64> {
    let row = demand
        .get(request_id)?
        .with_context(|| format!("missing Scale Set demand {request_id}"))?;
    let holder = permit_holder(row.scale_set_id, request_id);
    let current = ledger
        .permit_lease_generation(&holder)
        .map_err(|error| anyhow::anyhow!("read exact Scale Set lease for {holder}: {error}"))?;
    match (row.permit_lease_generation, current) {
        (Some(saved), Some(current)) if saved == current => Ok(saved),
        (Some(saved), Some(current)) => anyhow::bail!(
            "Scale Set request {request_id} records lease {saved}, ledger holds replacement lease {current}"
        ),
        (Some(saved), None) => Ok(saved),
        (None, Some(current)) => {
            demand.record_permit_lease_generation(request_id, current)?;
            Ok(current)
        }
        (None, None) => anyhow::bail!(
            "Scale Set request {request_id} has no recorded or held permit lease"
        ),
    }
}

/// Re-acquire one overdue uncertain batch: same intent, authoritative
/// answer. Acquired members move to `acquired`, missing members release
/// their permits and return to `eligible` with age kept. Transport failure
/// keeps the batch uncertain (returns `Ok(false)`).
async fn reacquire_batch<Q: QueueSession, L: CapacityLedger>(
    queue: &Q,
    ledger: &mut L,
    demand: &mut DemandStore,
    batches: &mut AcquireBatchStore,
    batch: &crate::scaleset::intents::AcquireBatch,
    generation: u64,
    metrics: &Metrics,
) -> Result<bool> {
    let mut canceled_pending = std::collections::HashSet::new();
    let uncertain: Vec<i64> = {
        let mut members = Vec::new();
        for request_id in &batch.request_ids {
            if let Some(row) = demand.get(*request_id)? {
                match row.state {
                    DemandState::Uncertain => members.push(*request_id),
                    DemandState::CanceledPending => {
                        members.push(*request_id);
                        canceled_pending.insert(*request_id);
                    }
                    _ => {}
                }
            }
        }
        members
    };
    if uncertain.is_empty() {
        batches.resolve(&batch.batch_id, false)?;
        return Ok(true);
    }
    let returned = match queue.acquire_jobs(&uncertain).await {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(
                batch = batch.batch_id.as_str(),
                error = error.to_string(),
                "uncertain batch re-acquire failed; staying uncertain"
            );
            return Ok(false);
        }
    };
    let (acquired, missing) = reconcile_returned_ids(&uncertain, &returned);
    for request_id in &acquired {
        let state = if canceled_pending.contains(request_id) {
            DemandState::CanceledAcquired
        } else {
            DemandState::Acquired
        };
        demand.set_state(*request_id, state, None, generation)?;
        transition_demand_lease(
            ledger,
            demand,
            *request_id,
            &permit_holder(batch.scale_set_id, *request_id),
            LedgerPermitState::Acquiring,
        )?;
    }
    for request_id in &missing {
        let holder = permit_holder(batch.scale_set_id, *request_id);
        if canceled_pending.contains(request_id) {
            // The cancellation message was already ACKed. Once the batch
            // proves this request was not acquired, close this old attempt;
            // upstream sends a new JobAvailable for the requeued job.
            demand.set_state(*request_id, DemandState::CanceledDone, None, generation)?;
            let lease_generation = exact_demand_lease_generation(ledger, demand, *request_id)?;
            if !ledger.release_cancelled_if_generation(&holder, lease_generation)? {
                anyhow::bail!(
                    "could not release exact canceled Scale Set lease {lease_generation} for request {request_id}"
                );
            }
        } else {
            let lease_generation = exact_demand_lease_generation(ledger, demand, *request_id)?;
            if !ledger.release_to_eligible_if_generation(&holder, lease_generation)? {
                anyhow::bail!(
                    "could not requeue exact Scale Set lease {lease_generation} for request {request_id}"
                );
            }
            demand.set_state(*request_id, DemandState::Eligible, None, generation)?;
        }
    }
    metrics.add_acquired_ids(acquired.len() as u64);
    metrics.add_missing_ids(missing.len() as u64);
    batches.resolve(&batch.batch_id, false)?;
    Ok(true)
}

/// Record unknown server message types. Counting + logging only: future
/// types must never fail the loop.
pub fn unknown_event(metrics: &Metrics, kinds: &[String]) {
    if kinds.is_empty() {
        return;
    }
    metrics.add_unknown_events(kinds.len() as u64);
    tracing::warn!(
        count = kinds.len(),
        kinds = format!("{kinds:?}"),
        "scale-set message carried unknown batched types; ignored per upstream dispatch"
    );
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
    use crate::scaleset::capacity::{AcquireOutcome, MemLedger};
    use crate::scaleset::scale::QueueSession;

    #[derive(Debug, Default)]
    struct ScriptedQueue {
        answer: std::sync::Mutex<Option<Result<Vec<i64>, String>>>,
        seen: std::sync::Mutex<Vec<Vec<i64>>>,
    }

    #[derive(Debug)]
    struct QueueError(String);

    impl std::fmt::Display for QueueError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "queue: {}", self.0)
        }
    }

    impl std::error::Error for QueueError {}

    impl QueueSession for ScriptedQueue {
        type Error = QueueError;

        async fn acquire_jobs(&self, request_ids: &[i64]) -> Result<Vec<i64>, Self::Error> {
            self.seen.lock().unwrap().push(request_ids.to_vec());
            self.answer
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Ok(request_ids.to_vec()))
                .map_err(QueueError)
        }
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-reconcile-{name}-{}",
            std::process::id()
        ));
        // Drop stale state from pid-reusing earlier runs: every test starts
        // from an empty database, deterministically.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    #[test]
    fn worker_lifecycle_lock_authorizes_only_its_exact_owner() {
        let path = temp_path("worker-lock-owner");
        std::fs::write(&path, []).unwrap();
        let lock = lock_worker_docker_lifecycle(&path, "7/worker-a").unwrap();

        assert!(lock.authorizes("7/worker-a"));
        assert!(!lock.authorizes("7/worker-b"));
        drop(lock);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn push_offer(id: i64) -> velnor_model::ScaleSetJobAvailable {
        velnor_model::ScaleSetJobAvailable {
            acquire_job_url: String::new(),
            base: velnor_model::ScaleSetJobMessage {
                message_type: velnor_model::ScaleSetJobMessageType::JobAvailable,
                runner_request_id: id,
                repository_name: "velnor".to_owned(),
                owner_name: "tailrocks".to_owned(),
                job_id: format!("job-{id}"),
                job_workflow_ref: String::new(),
                job_display_name: String::new(),
                workflow_run_id: 0,
                event_name: "push".to_owned(),
                request_labels: Vec::new(),
                queue_time: String::new(),
                scale_set_assign_time: String::new(),
                runner_assign_time: String::new(),
                finish_time: String::new(),
            },
        }
    }

    #[test]
    fn startup_attests_worker_cleanup_after_demand_is_terminal() {
        let path = temp_path("terminal-worker-still-owns-permit");
        let mut demand = DemandStore::open(&path).unwrap();
        demand.submit_offer(7, &push_offer(77), 0).unwrap();
        demand
            .set_state(77, DemandState::Terminal, None, 0)
            .unwrap();

        let ownership = crate::scaleset::worker::OwnershipId::bind(7, "7-77").as_str();
        let mut workers = crate::scaleset::WorkerRegistry::open(&path).unwrap();
        workers
            .upsert(
                &ownership,
                "operation-77",
                77,
                "7-77",
                "network-77",
                "/workers/s77/workspace",
                "/workers/s77/dind-data",
                "runner@sha256:test",
                "dind@sha256:test",
            )
            .unwrap();
        workers
            .set_state(&ownership, velnor_model::ScaleSetWorkerState::OwnedCleanup)
            .unwrap();

        assert_eq!(
            attest_demand_holders(&path).unwrap(),
            vec![(
                permit_holder(7, 77),
                velnor_control::permit_ledger::PermitState::Cleaning,
            )]
        );
    }

    #[test]
    fn startup_reconciles_before_advertising() {
        let path = temp_path("startup");
        let mut demand = DemandStore::open(&path).unwrap();
        let mut batches = AcquireBatchStore::open(&path).unwrap();
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(4);
        let metrics = Metrics::new();

        // Unreconciled ledger advertises nothing.
        assert_eq!(ledger.advertised_free().unwrap(), None);

        demand.submit_offer(7, &push_offer(11), 0).unwrap();
        demand
            .set_state(11, DemandState::Acquired, None, 0)
            .unwrap();
        let report = startup(&mut ledger, &mut demand, &mut batches, 7, &metrics).unwrap();
        // No row existed for the attested holder: adopted as counted occupancy.
        assert_eq!(report.adopted, vec!["scaleset/7/11".to_owned()]);
        assert_eq!(ledger.advertised_free().unwrap(), Some(3));
        assert_eq!(metrics.snapshot().reconcile_runs, 1);
    }

    #[test]
    fn startup_requeues_unbatched_acquire_intent_without_claiming_network_success() {
        let path = temp_path("unbatched-acquire-intent");
        let mut demand = DemandStore::open(&path).unwrap();
        let mut batches = AcquireBatchStore::open(&path).unwrap();
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(4);
        let metrics = Metrics::new();

        demand.submit_offer(7, &push_offer(12), 0).unwrap();
        demand
            .set_state(12, DemandState::AcquireIntent, None, 0)
            .unwrap();
        let holder = permit_holder(7, 12);
        let (outcome, _) = ledger
            .acquire_with_lease_generation(
                &holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Acquiring,
                0,
            )
            .unwrap();
        assert_eq!(outcome, AcquireOutcome::Acquired);

        let report = startup(&mut ledger, &mut demand, &mut batches, 7, &metrics).unwrap();
        assert_eq!(report.unbatched_acquire_intents_recovered, 1);
        assert_eq!(report.batches_orphaned, 0);
        assert_eq!(
            demand.get(12).unwrap().unwrap().state,
            DemandState::Eligible
        );
        assert_eq!(ledger.holder_state(&holder).unwrap(), None);
        assert_eq!(ledger.occupied().unwrap(), 0);

        // Restart replay is idempotent after the reservation was freed.
        let replay = startup(&mut ledger, &mut demand, &mut batches, 7, &metrics).unwrap();
        assert_eq!(replay.unbatched_acquire_intents_recovered, 0);
        assert_eq!(
            demand.get(12).unwrap().unwrap().state,
            DemandState::Eligible
        );
        assert_eq!(ledger.occupied().unwrap(), 0);
    }

    #[test]
    fn startup_adopts_crash_orphaned_batches_as_uncertain() {
        let path = temp_path("orphan");
        let mut demand = DemandStore::open(&path).unwrap();
        let mut batches = AcquireBatchStore::open(&path).unwrap();
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(4);
        let metrics = Metrics::new();

        demand.submit_offer(7, &push_offer(21), 0).unwrap();
        demand
            .set_state(21, DemandState::AcquireIntent, None, 0)
            .unwrap();
        let holders = vec![permit_holder(7, 21)];
        batches
            .record_intended("acq-orphan", 7, &[21], &holders, 0)
            .unwrap();
        let generation = ledger.generation().unwrap();
        let (outcome, _) = ledger
            .acquire_with_lease_generation(
                &holders[0],
                LedgerLane::ScaleSet,
                LedgerPermitState::Acquiring,
                generation,
            )
            .unwrap();
        assert_eq!(outcome, AcquireOutcome::Acquired);

        let report = startup(&mut ledger, &mut demand, &mut batches, 7, &metrics).unwrap();
        assert_eq!(report.batches_orphaned, 1);
        assert_eq!(
            batches.get("acq-orphan").unwrap().unwrap().state,
            crate::scaleset::intents::BatchState::Uncertain
        );
        assert_eq!(
            demand.get(21).unwrap().unwrap().state,
            DemandState::Uncertain
        );
        // Still counted: occupancy survives the crash.
        assert_eq!(ledger.occupied().unwrap(), 1);
    }

    #[tokio::test]
    async fn idle_poll_reacquires_overdue_uncertain_batches() {
        let path = temp_path("idle");
        let mut demand = DemandStore::open(&path).unwrap();
        let mut batches = AcquireBatchStore::open(&path).unwrap();
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(4);
        ledger.reconcile(&[]).unwrap();
        let metrics = Metrics::new();
        let generation = ledger.generation().unwrap();

        for id in [31, 32] {
            demand.submit_offer(7, &push_offer(id), generation).unwrap();
            demand
                .set_state(id, DemandState::Uncertain, None, generation)
                .unwrap();
            let holder = permit_holder(7, id);
            ledger
                .acquire_with_lease_generation(
                    &holder,
                    LedgerLane::ScaleSet,
                    LedgerPermitState::Acquiring,
                    generation,
                )
                .unwrap();
        }
        let holders = vec![permit_holder(7, 31), permit_holder(7, 32)];
        batches
            .record_intended("acq-idle", 7, &[31, 32], &holders, generation)
            .unwrap();
        batches.resolve("acq-idle", true).unwrap();
        // Age the batch past the re-acquire horizon.
        let old = velnor_model::Timestamp::now()
            .minus(std::time::Duration::from_secs(3600))
            .to_rfc3339()
            .unwrap();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute(
                "UPDATE scaleset_acquire_batches SET created_at = ?1 WHERE batch_id = 'acq-idle'",
                [old],
            )
            .unwrap();

        let queue = ScriptedQueue::default();
        *queue.answer.lock().unwrap() = Some(Ok(vec![31]));
        let report = idle_poll(
            &queue,
            &mut ledger,
            &mut demand,
            &mut batches,
            7,
            generation,
            &metrics,
        )
        .await
        .unwrap();
        assert_eq!(report.reacquired_batches, 1);
        assert_eq!(
            demand.get(31).unwrap().unwrap().state,
            DemandState::Acquired
        );
        // Missing member releases its permit and re-queues with age kept.
        assert_eq!(
            demand.get(32).unwrap().unwrap().state,
            DemandState::Eligible
        );
        assert_eq!(ledger.holder_state(&holders[1]).unwrap(), None);
        assert_eq!(queue.seen.lock().unwrap().as_slice(), &[vec![31, 32]]);
    }

    #[test]
    fn unknown_events_count_without_failing() {
        let metrics = Metrics::new();
        unknown_event(
            &metrics,
            &["JobMigrated".to_owned(), "JobMigrated".to_owned()],
        );
        assert_eq!(metrics.snapshot().unknown_events, 2);
        unknown_event(&metrics, &[]);
        assert_eq!(metrics.snapshot().unknown_events, 2);
    }
}
