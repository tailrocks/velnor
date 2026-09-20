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

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use crate::scaleset::capacity::{AcquireOutcome, CapacityLedger, LedgerLane, LedgerPermitState};
use crate::scaleset::converge::WorkerLane;
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
    let demand = DemandStore::open(state_db)?;
    let mut attested = Vec::new();
    for (scale_set_id, request_id, state) in demand.list_in_states_all(&PERMIT_STATES)? {
        attested.push((
            permit_holder(scale_set_id, request_id),
            to_control_state(permit_state_for_demand(state)),
        ));
    }
    Ok(attested)
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
            if state == DemandState::CanceledPending {
                demand.set_state(request_id, DemandState::CanceledDone, None, generation)?;
                ledger.release_cancelled(&holder)?;
            } else {
                demand.set_state(request_id, DemandState::Eligible, None, generation)?;
                ledger.release_to_eligible(&holder)?;
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
        .reconcile(&alive_refs)
        .map_err(|error| anyhow::anyhow!("reconcile ledger: {error}"))?;

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
#[allow(clippy::too_many_arguments, reason = "single idle-reconcile call path")]
pub async fn idle_poll<Q: QueueSession, L: CapacityLedger, W: WorkerLane>(
    queue: &Q,
    ledger: &mut L,
    demand: &mut DemandStore,
    batches: &mut AcquireBatchStore,
    lane: &mut W,
    scale_set_id: i32,
    generation: u64,
    metrics: &Metrics,
) -> Result<IdleReport> {
    metrics.inc_reconcile_runs();
    let mut report = IdleReport::default();
    let open = batches.open_batches(scale_set_id, 16)?;
    for batch in &open {
        if resolve_from_observations(ledger, demand, batches, lane, batch, generation).await? {
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
/// `acquired` confirm the grant; members observed `terminal` stay owned by
/// the terminal handler unless the lane disowns them (rowless orphans free
/// their permits under the epoch fence — nothing was ever provisioned for
/// them and no further messages will arrive). Returns whether the batch
/// resolved.
#[allow(clippy::too_many_arguments, reason = "single idle-resolve call site")]
async fn resolve_from_observations<L: CapacityLedger, W: WorkerLane>(
    ledger: &mut L,
    demand: &mut DemandStore,
    batches: &mut AcquireBatchStore,
    lane: &mut W,
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
                transition_or_adopt(ledger, &holder, LedgerPermitState::Acquiring, generation)?;
            }
            DemandState::CanceledAcquired => {
                transition_or_adopt(ledger, &holder, LedgerPermitState::Acquiring, generation)?;
            }
            // The terminal handler owns worker cleanup and permit release.
            // A completion message alone is not cleanup confirmation. The
            // one exception is a rowless orphan: no worker row exists, so
            // nobody will ever visit this holder again (a completed job
            // emits no further messages) and the held permit would leak
            // occupancy forever. Free it under the epoch fence.
            DemandState::Terminal => {
                let owned = lane
                    .owns_terminal_cleanup(*request_id)
                    .map_err(|error| anyhow::anyhow!("check terminal ownership: {error}"))?;
                if !owned {
                    fenced_release_orphan(ledger, &holder, generation)?;
                }
            }
            _ => {}
        }
    }
    batches.resolve(&batch.batch_id, false)?;
    Ok(true)
}

/// Release a rowless orphan's permit under the epoch fence. The demand
/// rows were read under `generation`, so a moved epoch means the Terminal
/// observation may be stale — bail and let the next poll retry rather
/// than free another epoch's permit. Idempotent: an already-released
/// holder is a no-op.
fn fenced_release_orphan<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    generation: u64,
) -> Result<bool> {
    let fresh = ledger
        .generation()
        .map_err(|error| anyhow::anyhow!("re-read ledger generation: {error}"))?;
    if fresh != generation {
        anyhow::bail!(
            "ledger generation moved during idle resolve (saw {generation}, now {fresh}); retry under the fresh epoch"
        );
    }
    ledger
        .release(holder)
        .map_err(|error| anyhow::anyhow!("release orphaned holder: {error}"))
}

pub(crate) fn transition_or_adopt<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    state: LedgerPermitState,
    generation: u64,
) -> Result<()> {
    match ledger.transition(holder, state, generation) {
        Ok(()) => Ok(()),
        Err(error) if L::is_stale_generation(&error) => {
            let fresh = ledger
                .generation()
                .map_err(|error| anyhow::anyhow!("re-read ledger generation: {error}"))?;
            match ledger.transition(holder, state, fresh) {
                Ok(()) => Ok(()),
                Err(_) => adopt_holder(ledger, holder, state, fresh),
            }
        }
        Err(_) => adopt_holder(ledger, holder, state, generation),
    }
}

/// Adopt a lost holder row back as counted occupancy rather than run
/// rowless. A full or unconfigured ledger fails the step (the message is
/// redelivered and the adoption retries); only a stale generation retries
/// in place, once.
fn adopt_holder<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    state: LedgerPermitState,
    generation: u64,
) -> Result<()> {
    match ledger.acquire(holder, LedgerLane::ScaleSet, state, generation) {
        Ok(AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld) => Ok(()),
        Ok(AcquireOutcome::StaleGeneration) => {
            let fresh = ledger
                .generation()
                .map_err(|error| anyhow::anyhow!("re-read ledger generation: {error}"))?;
            match ledger.acquire(holder, LedgerLane::ScaleSet, state, fresh) {
                Ok(AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld) => Ok(()),
                Ok(outcome) => anyhow::bail!("adopt lost holder {holder}: ledger says {outcome:?}"),
                Err(error) => anyhow::bail!("adopt lost holder {holder}: {error}"),
            }
        }
        Ok(outcome) => anyhow::bail!("adopt lost holder {holder}: ledger says {outcome:?}"),
        Err(error) => anyhow::bail!("adopt lost holder {holder}: {error}"),
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
        transition_or_adopt(
            ledger,
            &permit_holder(batch.scale_set_id, *request_id),
            LedgerPermitState::Acquiring,
            generation,
        )?;
    }
    for request_id in &missing {
        let holder = permit_holder(batch.scale_set_id, *request_id);
        if canceled_pending.contains(request_id) {
            // The cancellation message was already ACKed. Once the batch
            // proves this request was not acquired, close this old attempt;
            // upstream sends a new JobAvailable for the requeued job.
            demand.set_state(*request_id, DemandState::CanceledDone, None, generation)?;
            ledger
                .release_cancelled(&holder)
                .map_err(|error| anyhow::anyhow!("release canceled holder: {error}"))?;
        } else {
            ledger
                .release_to_eligible(&holder)
                .map_err(|error| anyhow::anyhow!("release missing holder: {error}"))?;
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

    struct OwnershipLane {
        owned: bool,
    }

    impl WorkerLane for OwnershipLane {
        type Error = std::convert::Infallible;

        async fn provision(
            &mut self,
            _intent: &crate::scaleset::intents::ProvisionIntent,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn note_assigned(
            &mut self,
            _assigned: &velnor_model::ScaleSetJobAssigned,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn note_started(
            &mut self,
            _started: &velnor_model::ScaleSetJobStarted,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn note_terminal(
            &mut self,
            _completed: &velnor_model::ScaleSetJobCompleted,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn owns_terminal_cleanup(&self, _request_id: i64) -> Result<bool, Self::Error> {
            Ok(self.owned)
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
        assert_eq!(
            ledger
                .acquire(
                    &holder,
                    LedgerLane::ScaleSet,
                    LedgerPermitState::Acquiring,
                    0,
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );

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
        assert_eq!(
            ledger
                .acquire(
                    &holders[0],
                    LedgerLane::ScaleSet,
                    LedgerPermitState::Acquiring,
                    generation
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );

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
                .acquire(
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
        let mut lane = OwnershipLane { owned: true };
        let report = idle_poll(
            &queue,
            &mut ledger,
            &mut demand,
            &mut batches,
            &mut lane,
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

    #[tokio::test]
    async fn idle_poll_releases_rowless_terminal_orphans() {
        let path = temp_path("terminal-orphan");
        let mut demand = DemandStore::open(&path).unwrap();
        let mut batches = AcquireBatchStore::open(&path).unwrap();
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(4);
        ledger.reconcile(&[]).unwrap();
        let metrics = Metrics::new();
        let generation = ledger.generation().unwrap();

        // Crash orphan: the demand row went `terminal` but the permit
        // release never landed (crash between the row write and the
        // terminal path). No worker row exists and a completed job emits
        // no further messages, so the lane disowns cleanup.
        for id in [41, 42] {
            demand.submit_offer(7, &push_offer(id), generation).unwrap();
            demand
                .set_state(id, DemandState::Terminal, None, generation)
                .unwrap();
            ledger
                .acquire(
                    &permit_holder(7, id),
                    LedgerLane::ScaleSet,
                    LedgerPermitState::Uncertain,
                    generation,
                )
                .unwrap();
        }
        let holders = vec![permit_holder(7, 41), permit_holder(7, 42)];
        batches
            .record_intended("acq-orphan", 7, &[41, 42], &holders, generation)
            .unwrap();
        batches.resolve("acq-orphan", true).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 2);

        let queue = ScriptedQueue::default();
        let mut lane = OwnershipLane { owned: false };
        let report = idle_poll(
            &queue,
            &mut ledger,
            &mut demand,
            &mut batches,
            &mut lane,
            7,
            generation,
            &metrics,
        )
        .await
        .unwrap();
        assert_eq!(report.batches_resolved, 1);
        assert_eq!(ledger.occupied().unwrap(), 0);
        assert_eq!(ledger.holder_state(&holders[0]).unwrap(), None);
        assert_eq!(ledger.holder_state(&holders[1]).unwrap(), None);
        // Observation-only resolve: no re-acquire traffic for members that
        // already observed terminal.
        assert!(queue.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn idle_poll_keeps_lane_owned_terminal_permits() {
        let path = temp_path("terminal-owned");
        let mut demand = DemandStore::open(&path).unwrap();
        let mut batches = AcquireBatchStore::open(&path).unwrap();
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(4);
        ledger.reconcile(&[]).unwrap();
        let metrics = Metrics::new();
        let generation = ledger.generation().unwrap();

        demand.submit_offer(7, &push_offer(43), generation).unwrap();
        demand
            .set_state(43, DemandState::Terminal, None, generation)
            .unwrap();
        let holder = permit_holder(7, 43);
        ledger
            .acquire(
                &holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Cleaning,
                generation,
            )
            .unwrap();
        batches
            .record_intended("acq-owned", 7, &[43], &[holder.clone()], generation)
            .unwrap();
        batches.resolve("acq-owned", true).unwrap();

        // The lane owns cleanup (a worker row exists): the terminal handler
        // — not the idle path — releases this permit.
        let queue = ScriptedQueue::default();
        let mut lane = OwnershipLane { owned: true };
        let report = idle_poll(
            &queue,
            &mut ledger,
            &mut demand,
            &mut batches,
            &mut lane,
            7,
            generation,
            &metrics,
        )
        .await
        .unwrap();
        assert_eq!(report.batches_resolved, 1);
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(LedgerPermitState::Cleaning)
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
    }

    #[test]
    fn fenced_orphan_release_bails_on_moved_epoch() {
        let mut ledger = MemLedger::new();
        ledger.set_max_jobs(4);
        ledger.reconcile(&[]).unwrap();
        let generation = ledger.generation().unwrap();
        let holder = permit_holder(7, 44);
        ledger
            .acquire(
                &holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Uncertain,
                generation,
            )
            .unwrap();
        ledger.begin_epoch();
        let error = fenced_release_orphan(&mut ledger, &holder, generation).unwrap_err();
        assert!(
            error.to_string().contains("retry under the fresh epoch"),
            "unexpected fence error: {error:#}"
        );
        // No release happened: the next poll retries under the fresh epoch.
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
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
