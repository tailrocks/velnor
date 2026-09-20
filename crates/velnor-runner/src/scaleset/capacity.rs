//! Shared-grant capacity authority (§5.1 steps 3 + 8).
//!
//! The one host-wide `max_jobs=N` ledger lives in `velnor-control`
//! (`PermitLedger`, C2). [`CapacityLedger`] is the Scale Set lane interface
//! for that authority; [`SharedLedger`](crate::scaleset::SharedLedger)
//! adapts its permit and demand-ordering operations, while [`MemLedger`]
//! keeps unit tests independent of SQLite.
//!
//! Advertisement rule (step 8): per poll, per session, `free = N −
//! occupied_global` across BOTH lanes; `None` (unreconciled or
//! unconfigured) maps to header `0` — take nothing — never to `N`.
//!
//! Upstream note: `listener.go` passes the configured `maxRunners` total on
//! every poll, not free headroom. The spec mandates shared-grant-derived
//! free headroom instead; the live canary must confirm the server honors
//! shrinking values before the estate proof.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// A local lane sharing the one host-wide `N`. Mirrors C2 `PermitLane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LedgerLane {
    Native,
    ScaleSet,
}

/// Lifecycle state of one held permit. Every state counts toward `N`.
/// Mirrors C2 `PermitState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LedgerPermitState {
    Reserved,
    Acquiring,
    Provisioning,
    Assignable,
    Running,
    Cleaning,
    Uncertain,
}

/// One counted occupant. Mirrors C2 `PermitHolder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerHolder {
    pub holder: String,
    pub lane: LedgerLane,
    pub state: LedgerPermitState,
    pub generation: u64,
    pub lease_generation: u64,
}

/// Closed queue state used when an upstream terminal callback arrives for a
/// request that never acquired a permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerDemandTerminalState {
    Terminal,
    Cancelled,
}

/// Outcome of [`CapacityLedger::acquire`]. Mirrors C2 `AcquireOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireOutcome {
    Acquired,
    AlreadyHeld,
    Full,
    Deferred,
    NotReady,
    Closed,
    StaleGeneration,
    NotConfigured,
}

/// Outcome of [`CapacityLedger::reconcile`]. Mirrors C2 `ReconcileReport`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub adopted: Vec<String>,
    pub marked_uncertain: Vec<String>,
    pub confirmed: Vec<String>,
}

/// A durable Scale Set demand to mirror before admission starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerDemandObservation {
    pub holder: String,
    pub lane: LedgerLane,
    pub scope: String,
    pub first_seen_unix: u64,
    pub first_seen_subsec_nanos: u32,
}

/// Ledger failures. Mirrors C2 `LedgerError`.
#[derive(Debug)]
pub enum LedgerError {
    Storage(String),
    UnknownHolder(String),
    StaleGeneration { expected: u64, seen: u64 },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "permit ledger storage: {error}"),
            Self::UnknownHolder(holder) => {
                write!(f, "permit ledger holds no permit for {holder:?}")
            }
            Self::StaleGeneration { expected, seen } => write!(
                f,
                "permit ledger generation moved from {seen} to {expected}; re-read and retry"
            ),
        }
    }
}

impl std::error::Error for LedgerError {}

/// Host-wide `max_jobs=N` capacity authority. Method-for-method mirror of
/// the C2 `PermitLedger` surface the adapter needs.
pub trait CapacityLedger {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Current generation. Callers fence mutations on this value.
    fn generation(&self) -> Result<u64, Self::Error>;
    /// Free capacity (`N − occupied`), only after this epoch reconciled.
    /// `None` means "do not advertise yet", never "infinite".
    fn advertised_free(&self) -> Result<Option<u32>, Self::Error>;
    /// Counted occupants across every lane and state.
    fn occupied(&self) -> Result<u32, Self::Error>;
    /// Every counted occupant, ordered by holder.
    fn holders(&self) -> Result<Vec<LedgerHolder>, Self::Error>;
    /// Current state of one holder's permit, if held.
    fn holder_state(&self, holder: &str) -> Result<Option<LedgerPermitState>, Self::Error>;
    /// Observe durable lane demand before any permit acquire attempt. The
    /// global implementation preserves `first_seen_unix` and its sequence
    /// across redelivery; lightweight ledgers may omit ordering.
    fn observe_demand(
        &mut self,
        _holder: &str,
        _lane: LedgerLane,
        _scope: &str,
        _first_seen_unix: u64,
        _observed_unix: u64,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
    /// Observe demand with exact fractional age. Implementations without a
    /// fractional queue may keep the whole-second behavior.
    fn observe_demand_with_subsecond(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        scope: &str,
        first_seen_unix: u64,
        _first_seen_subsec_nanos: u32,
        observed_unix: u64,
    ) -> Result<(), Self::Error> {
        self.observe_demand(holder, lane, scope, first_seen_unix, observed_unix)
    }
    /// Restore a durable queue snapshot before acquisitions can race it.
    /// Production's shared ledger overrides this with one atomic batch.
    fn observe_demands(
        &mut self,
        demands: &[LedgerDemandObservation],
        observed_unix: u64,
    ) -> Result<(), Self::Error> {
        for demand in demands {
            self.observe_demand_with_subsecond(
                &demand.holder,
                demand.lane,
                &demand.scope,
                demand.first_seen_unix,
                demand.first_seen_subsec_nanos,
                observed_unix,
            )?;
        }
        Ok(())
    }
    /// Publish a live Scale Set offer globally before the source database
    /// can commit it as eligible. Production leaves a durable pending marker
    /// until the source row is confirmed; lightweight ledgers preserve the
    /// same queue age without cross-database coordination.
    fn begin_scale_set_offer(
        &mut self,
        holder: &str,
        scope: &str,
        _source_db: &Path,
        generation: u64,
        first_seen: Option<(u64, u32)>,
        _publication: &velnor_control::permit_ledger::ScaleSetDemandPublication,
    ) -> Result<(u64, u32), Self::Error> {
        let (now, nanos) = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| (elapsed.as_secs(), elapsed.subsec_nanos()))
            .unwrap_or((0, 0));
        let (first_seen_unix, first_seen_subsec_nanos) = first_seen.unwrap_or((now, nanos));
        self.observe_demand_with_subsecond(
            holder,
            LedgerLane::ScaleSet,
            scope,
            first_seen_unix,
            first_seen_subsec_nanos,
            now,
        )?;
        let _ = generation;
        Ok((first_seen_unix, first_seen_subsec_nanos))
    }
    /// Remove the live-offer pending marker after its source row commits.
    fn complete_scale_set_offer(
        &mut self,
        _holder: &str,
        _source_db: &Path,
        _generation: u64,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
    /// Close unheld upstream demand that the lane confirmed is no longer
    /// eligible. A held permit must use one of the cleanup release methods.
    fn cancel_demand(&mut self, _holder: &str) -> Result<bool, Self::Error> {
        Ok(false)
    }
    /// Acquire one permit for `holder`, fenced on `generation`, and return
    /// the immutable lease identity for the successful/current hold.
    fn acquire_with_lease_generation(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<(AcquireOutcome, Option<u64>), Self::Error>;
    /// Transition only the exact immutable permit lease. This cannot mutate
    /// a replacement acquisition that reused the same holder.
    fn transition_if_lease_generation(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error>;
    /// Current immutable lease identity for one held permit.
    fn permit_lease_generation(&self, holder: &str) -> Result<Option<u64>, Self::Error>;
    /// Release the exact lease acquired by one attempt.
    fn release_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error>;
    /// Release the exact lease and return its demand to the queue.
    fn release_to_eligible_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error>;
    /// Release the exact lease and close the demand as cancelled.
    fn release_cancelled_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error>;
    /// Close demand only while no permit is held; it cannot authorize a
    /// permit deletion and is safe for unacquired terminal callbacks.
    fn close_demand_if_unheld(
        &mut self,
        holder: &str,
        state: LedgerDemandTerminalState,
    ) -> Result<bool, Self::Error>;
    /// Keep occupancy and close demand after cleanup could not be confirmed.
    fn retain_uncertain_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error>;

    /// Reconcile durable occupancy against the attested live set and mark
    /// this epoch reconciled. Never deletes: observed-but-unrecorded work
    /// is adopted, recorded-but-unobserved work is marked uncertain.
    fn reconcile(
        &mut self,
        alive: &[(&str, LedgerLane, LedgerPermitState)],
    ) -> Result<ReconcileReport, Self::Error>;
    /// Reconcile the complete host source roster before reopening admission.
    /// Lightweight ledgers keep the local behavior; the production adapter
    /// scans every configured source and fences this with the sampled epoch.
    fn reconcile_host_sources(
        &mut self,
        expected_generation: u64,
        _required_demand_db: &Path,
        alive: &[(&str, LedgerLane, LedgerPermitState)],
    ) -> Result<ReconcileReport, Self::Error> {
        let _ = expected_generation;
        self.reconcile(alive)
    }
}

/// In-memory [`CapacityLedger`] implementing the exact C2 state machine
/// (generation fencing, idempotent acquire, reconcile-before-advertise).
/// Unit-test double; production swaps in the SQLite `PermitLedger`.
#[derive(Debug, Default)]
pub struct MemLedger {
    inner: Mutex<MemLedgerState>,
}

#[derive(Debug, Default)]
struct MemLedgerState {
    max_jobs: Option<u32>,
    generation: u64,
    reconciled_generation: Option<u64>,
    next_lease_generation: u64,
    holders: HashMap<String, (LedgerLane, LedgerPermitState, u64, u64)>,
}

impl MemLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure `N` (mirrors C2 `set_max_jobs`; daemon startup owns it).
    pub fn set_max_jobs(&self, max_jobs: u32) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .max_jobs = Some(max_jobs);
    }

    /// Start a new epoch (mirrors C2 `begin_epoch`; daemon startup owns it).
    pub fn begin_epoch(&self) -> u64 {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.generation = state.generation.saturating_add(1);
        state.generation
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemLedgerState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl CapacityLedger for MemLedger {
    type Error = LedgerError;

    fn generation(&self) -> Result<u64, Self::Error> {
        Ok(self.lock().generation)
    }

    fn advertised_free(&self) -> Result<Option<u32>, Self::Error> {
        let state = self.lock();
        if state.reconciled_generation != Some(state.generation) {
            return Ok(None);
        }
        let Some(max) = state.max_jobs else {
            return Ok(None);
        };
        let occupied = u32::try_from(state.holders.len()).unwrap_or(u32::MAX);
        Ok(Some(max.saturating_sub(occupied)))
    }

    fn occupied(&self) -> Result<u32, Self::Error> {
        Ok(u32::try_from(self.lock().holders.len()).unwrap_or(u32::MAX))
    }

    fn holders(&self) -> Result<Vec<LedgerHolder>, Self::Error> {
        let state = self.lock();
        let mut holders: Vec<LedgerHolder> = state
            .holders
            .iter()
            .map(
                |(holder, (lane, permit, generation, lease_generation))| LedgerHolder {
                    holder: holder.clone(),
                    lane: *lane,
                    state: *permit,
                    generation: *generation,
                    lease_generation: *lease_generation,
                },
            )
            .collect();
        holders.sort_by(|left, right| left.holder.cmp(&right.holder));
        Ok(holders)
    }

    fn holder_state(&self, holder: &str) -> Result<Option<LedgerPermitState>, Self::Error> {
        Ok(self
            .lock()
            .holders
            .get(holder)
            .map(|(_, state, _, _)| *state))
    }

    fn acquire_with_lease_generation(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<(AcquireOutcome, Option<u64>), Self::Error> {
        let mut ledger = self.lock();
        if ledger.generation != generation {
            return Ok((AcquireOutcome::StaleGeneration, None));
        }
        let Some(max) = ledger.max_jobs else {
            return Ok((AcquireOutcome::NotConfigured, None));
        };
        if let Some((_, _, _, lease_generation)) = ledger.holders.get(holder) {
            return Ok((AcquireOutcome::AlreadyHeld, Some(*lease_generation)));
        }
        if u64::try_from(ledger.holders.len()).unwrap_or(u64::MAX) >= u64::from(max) {
            return Ok((AcquireOutcome::Full, None));
        }
        ledger.next_lease_generation = ledger.next_lease_generation.saturating_add(1);
        let lease_generation = ledger.next_lease_generation;
        ledger.holders.insert(
            holder.to_owned(),
            (lane, state, generation, lease_generation),
        );
        Ok((AcquireOutcome::Acquired, Some(lease_generation)))
    }

    fn transition_if_lease_generation(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error> {
        let mut ledger = self.lock();
        let Some((_, current_state, _, lease_generation)) = ledger.holders.get_mut(holder) else {
            return Ok(false);
        };
        if *lease_generation != expected_lease_generation {
            return Ok(false);
        }
        *current_state = state;
        Ok(true)
    }

    fn permit_lease_generation(&self, holder: &str) -> Result<Option<u64>, Self::Error> {
        Ok(self
            .lock()
            .holders
            .get(holder)
            .map(|(_, _, _, lease_generation)| *lease_generation))
    }

    fn release_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error> {
        let mut ledger = self.lock();
        if ledger
            .holders
            .get(holder)
            .is_some_and(|(_, _, _, generation)| *generation == expected_lease_generation)
        {
            ledger.holders.remove(holder);
            return Ok(true);
        }
        Ok(false)
    }

    fn release_to_eligible_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error> {
        self.release_if_generation(holder, expected_lease_generation)
    }

    fn release_cancelled_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error> {
        self.release_if_generation(holder, expected_lease_generation)
    }

    fn close_demand_if_unheld(
        &mut self,
        holder: &str,
        _state: LedgerDemandTerminalState,
    ) -> Result<bool, Self::Error> {
        Ok(!self.lock().holders.contains_key(holder))
    }

    fn retain_uncertain_if_generation(
        &mut self,
        holder: &str,
        expected_lease_generation: u64,
    ) -> Result<bool, Self::Error> {
        let mut ledger = self.lock();
        let current_generation = ledger.generation;
        let Some((_, state, epoch, lease_generation)) = ledger.holders.get_mut(holder) else {
            return Ok(false);
        };
        if *lease_generation != expected_lease_generation {
            return Ok(false);
        }
        *state = LedgerPermitState::Uncertain;
        *epoch = current_generation;
        Ok(true)
    }

    fn reconcile(
        &mut self,
        alive: &[(&str, LedgerLane, LedgerPermitState)],
    ) -> Result<ReconcileReport, Self::Error> {
        let mut ledger = self.lock();
        let mut report = ReconcileReport::default();
        let generation = ledger.generation;
        for (holder, lane, state) in alive {
            if ledger.holders.contains_key(*holder) {
                report.confirmed.push((*holder).to_owned());
            } else {
                let lease_generation = ledger.next_lease_generation.saturating_add(1);
                ledger.next_lease_generation = lease_generation;
                ledger.holders.insert(
                    (*holder).to_owned(),
                    (*lane, *state, generation, lease_generation),
                );
                report.adopted.push((*holder).to_owned());
            }
        }
        let recorded: Vec<String> = ledger.holders.keys().cloned().collect();
        for holder in &recorded {
            if alive.iter().any(|(live, _, _)| live == holder) {
                continue;
            }
            if let Some(entry) = ledger.holders.get_mut(holder) {
                entry.1 = LedgerPermitState::Uncertain;
                entry.2 = generation;
            }
            report.marked_uncertain.push(holder.clone());
        }
        ledger.reconciled_generation = Some(generation);
        report.adopted.sort();
        report.marked_uncertain.sort();
        report.confirmed.sort();
        Ok(report)
    }
}

/// Outcome of reserving one permit for a granted offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveOutcome {
    /// Fresh `reserved` permit (or an adopted redelivery of one).
    Reserved,
    /// Ledger full: the offer stays queued and keeps its age.
    CapacityExhausted,
    /// Ledger has no `N`: same treatment as full, louder in logs.
    NotConfigured,
}

/// Reserve failures: either the ledger failed or the epoch moved twice
/// inside one reserve (another process is bumping epochs concurrently).
#[derive(Debug)]
pub enum ReserveError<E> {
    Ledger(E),
    MissingLeaseGeneration,
    DoubleStale { seen: u64 },
}

impl<E: std::fmt::Display> std::fmt::Display for ReserveError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(error) => write!(f, "permit ledger reserve: {error}"),
            Self::MissingLeaseGeneration => {
                f.write_str("successful permit acquire returned no lease generation")
            }
            Self::DoubleStale { seen } => write!(
                f,
                "permit ledger epoch moved twice during one reserve (seen {seen}); retry the poll"
            ),
        }
    }
}

impl<E: std::error::Error + Send + Sync + 'static> std::error::Error for ReserveError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Ledger(error) => Some(error),
            Self::MissingLeaseGeneration | Self::DoubleStale { .. } => None,
        }
    }
}

/// Reserve one `reserved` permit for a granted offer (step 3), fenced on
/// `generation`. `AlreadyHeld` adopts the redelivered reservation instead
/// of double-spending; a stale generation is re-read and retried once.
/// Returns the generation the reservation landed under.
pub fn reserve_for_offer<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    generation: u64,
) -> Result<(ReserveOutcome, u64, Option<u64>), ReserveError<L::Error>> {
    let attempt = |ledger: &mut L, generation: u64| {
        ledger
            .acquire_with_lease_generation(
                holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .map_err(ReserveError::Ledger)
    };
    let result = attempt(ledger, generation)?;
    match result {
        (AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld, Some(lease_generation)) => {
            Ok((ReserveOutcome::Reserved, generation, Some(lease_generation)))
        }
        (AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld, None) => {
            Err(ReserveError::MissingLeaseGeneration)
        }
        (AcquireOutcome::Full, _) => Ok((ReserveOutcome::CapacityExhausted, generation, None)),
        (AcquireOutcome::NotConfigured, _) => Ok((ReserveOutcome::NotConfigured, generation, None)),
        (AcquireOutcome::StaleGeneration, _) => {
            let fresh = ledger.generation().map_err(ReserveError::Ledger)?;
            match attempt(ledger, fresh)? {
                (
                    AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld,
                    Some(lease_generation),
                ) => Ok((ReserveOutcome::Reserved, fresh, Some(lease_generation))),
                (AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld, None) => {
                    Err(ReserveError::MissingLeaseGeneration)
                }
                (AcquireOutcome::Full, _) => Ok((ReserveOutcome::CapacityExhausted, fresh, None)),
                (AcquireOutcome::NotConfigured, _) => {
                    Ok((ReserveOutcome::NotConfigured, fresh, None))
                }
                (AcquireOutcome::StaleGeneration, _) => {
                    Err(ReserveError::DoubleStale { seen: fresh })
                }
                (
                    AcquireOutcome::Deferred | AcquireOutcome::NotReady | AcquireOutcome::Closed,
                    _,
                ) => Ok((ReserveOutcome::CapacityExhausted, fresh, None)),
            }
        }
        (AcquireOutcome::Deferred | AcquireOutcome::NotReady | AcquireOutcome::Closed, _) => {
            Ok((ReserveOutcome::CapacityExhausted, generation, None))
        }
    }
}

/// Advertise free headroom for one poll (step 8): `N − occupied_global`
/// across both lanes, or `0` when the ledger is unreconciled,
/// unconfigured, or unreadable. Never `N`, never infinite.
pub fn advertise_free<L: CapacityLedger>(ledger: &L) -> u32 {
    match ledger.advertised_free() {
        Ok(Some(free)) => free,
        Ok(None) | Err(_) => 0,
    }
}

/// Release one holder after owned cleanup confirmed (the single release
/// path). Unfenced by design; returns whether a row was removed.
pub fn release_after_cleanup<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    expected_lease_generation: u64,
) -> Result<bool, L::Error> {
    ledger.release_if_generation(holder, expected_lease_generation)
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

    fn ledgers() -> MemLedger {
        let ledger = MemLedger::new();
        ledger.set_max_jobs(2);
        ledger
    }

    #[test]
    fn unreconciled_ledger_advertises_zero() {
        let ledger = ledgers();
        assert_eq!(advertise_free(&ledger), 0);
        assert_eq!(ledger.advertised_free().unwrap(), None);
    }

    #[test]
    fn reconcile_unlocks_free_headroom() {
        let mut ledger = ledgers();
        ledger.reconcile(&[]).unwrap();
        assert_eq!(advertise_free(&ledger), 2);
        let generation = ledger.generation().unwrap();
        let (outcome, _, lease_generation) =
            reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert_eq!(outcome, ReserveOutcome::Reserved);
        assert!(lease_generation.is_some());
        assert_eq!(advertise_free(&ledger), 1);
    }

    #[test]
    fn reserve_is_idempotent_per_holder_and_full_when_spent() {
        let mut ledger = ledgers();
        ledger.reconcile(&[]).unwrap();
        let generation = ledger.generation().unwrap();
        let (first, _, first_lease) =
            reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        let (again, _, again_lease) =
            reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert_eq!(first, ReserveOutcome::Reserved);
        assert_eq!(again, ReserveOutcome::Reserved);
        assert_eq!(first_lease, again_lease);
        assert_eq!(ledger.occupied().unwrap(), 1);
        let _ = reserve_for_offer(&mut ledger, "scaleset/7/2", generation).unwrap();
        let (full, _, lease) = reserve_for_offer(&mut ledger, "scaleset/7/3", generation).unwrap();
        assert_eq!(full, ReserveOutcome::CapacityExhausted);
        assert_eq!(lease, None);
    }

    #[test]
    fn stale_generation_retries_once_on_fresh_epoch() {
        let mut ledger = ledgers();
        ledger.reconcile(&[]).unwrap();
        let stale = ledger.generation().unwrap();
        ledger.begin_epoch();
        ledger.reconcile(&[]).unwrap();
        let (outcome, landed, lease) =
            reserve_for_offer(&mut ledger, "scaleset/7/9", stale).unwrap();
        assert_eq!(outcome, ReserveOutcome::Reserved);
        assert_eq!(landed, stale + 1);
        assert!(lease.is_some());
    }

    #[test]
    fn reconcile_never_deletes_and_marks_unobserved_uncertain() {
        let mut ledger = ledgers();
        let generation = ledger.generation().unwrap();
        ledger
            .acquire_with_lease_generation(
                "scaleset/7/1",
                LedgerLane::ScaleSet,
                LedgerPermitState::Running,
                generation,
            )
            .unwrap();
        let report = ledger.reconcile(&[]).unwrap();
        assert_eq!(report.marked_uncertain, vec!["scaleset/7/1".to_owned()]);
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state("scaleset/7/1").unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
    }
}
