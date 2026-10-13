//! Shared-grant capacity authority (§5.1 steps 3 + 8).
//!
//! The one host-wide `max_jobs=N` ledger lives in `velnor-control`
//! (`PermitLedger`, C2). This branch predates that merge, so the adapter
//! programs against [`CapacityLedger`]: a method-for-method mirror of the
//! C2 API (`acquire`/`transition`/`release`/`reconcile`/
//! `advertised_free`/`generation`/`occupied`/`holder_state`, same outcome
//! enums, same generation-fencing and reconcile-before-advertise rules).
//! Integration swaps [`MemLedger`] for a thin `PermitLedger` adapter with
//! no call-site changes.
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
}

/// Outcome of [`CapacityLedger::acquire`]. Mirrors C2 `AcquireOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireOutcome {
    Acquired,
    AlreadyHeld,
    Full,
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
    /// Acquire one permit for `holder`, fenced on `generation`. Idempotent
    /// per holder: duplicates report [`AcquireOutcome::AlreadyHeld`].
    fn acquire(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<AcquireOutcome, Self::Error>;
    /// Move one held permit to a new state, fenced on `generation`.
    fn transition(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<(), Self::Error>;
    /// Release one holder's permit. Unfenced by design; freeing capacity is
    /// always safe. Returns whether a row was removed.
    fn release(&mut self, holder: &str) -> Result<bool, Self::Error>;
    /// Reconcile durable occupancy against the attested live set and mark
    /// this epoch reconciled. Never deletes: observed-but-unrecorded work
    /// is adopted, recorded-but-unobserved work is marked uncertain.
    fn reconcile(
        &mut self,
        alive: &[(&str, LedgerLane, LedgerPermitState)],
    ) -> Result<ReconcileReport, Self::Error>;

    /// Whether a mutation error is generation fencing (re-read + retry)
    /// rather than a hard failure.
    fn is_stale_generation(error: &Self::Error) -> bool;
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
    holders: HashMap<String, (LedgerLane, LedgerPermitState, u64)>,
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
            .map(|(holder, (lane, permit, generation))| LedgerHolder {
                holder: holder.clone(),
                lane: *lane,
                state: *permit,
                generation: *generation,
            })
            .collect();
        holders.sort_by(|left, right| left.holder.cmp(&right.holder));
        Ok(holders)
    }

    fn holder_state(&self, holder: &str) -> Result<Option<LedgerPermitState>, Self::Error> {
        Ok(self.lock().holders.get(holder).map(|(_, state, _)| *state))
    }

    fn acquire(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<AcquireOutcome, Self::Error> {
        let mut ledger = self.lock();
        if ledger.generation != generation {
            return Ok(AcquireOutcome::StaleGeneration);
        }
        let Some(max) = ledger.max_jobs else {
            return Ok(AcquireOutcome::NotConfigured);
        };
        if ledger.holders.contains_key(holder) {
            return Ok(AcquireOutcome::AlreadyHeld);
        }
        if u64::try_from(ledger.holders.len()).unwrap_or(u64::MAX) >= u64::from(max) {
            return Ok(AcquireOutcome::Full);
        }
        ledger
            .holders
            .insert(holder.to_owned(), (lane, state, generation));
        Ok(AcquireOutcome::Acquired)
    }

    fn transition(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<(), Self::Error> {
        let mut ledger = self.lock();
        if ledger.generation != generation {
            return Err(LedgerError::StaleGeneration {
                expected: ledger.generation,
                seen: generation,
            });
        }
        let Some(entry) = ledger.holders.get_mut(holder) else {
            return Err(LedgerError::UnknownHolder(holder.to_owned()));
        };
        entry.1 = state;
        entry.2 = generation;
        Ok(())
    }

    fn release(&mut self, holder: &str) -> Result<bool, Self::Error> {
        Ok(self.lock().holders.remove(holder).is_some())
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
                ledger
                    .holders
                    .insert((*holder).to_owned(), (*lane, *state, generation));
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

    fn is_stale_generation(error: &Self::Error) -> bool {
        matches!(error, LedgerError::StaleGeneration { .. })
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
    DoubleStale { seen: u64 },
}

impl<E: std::fmt::Display> std::fmt::Display for ReserveError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(error) => write!(f, "permit ledger reserve: {error}"),
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
            Self::DoubleStale { .. } => None,
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
) -> Result<(ReserveOutcome, u64), ReserveError<L::Error>> {
    let attempt = |ledger: &mut L, generation: u64| {
        ledger
            .acquire(
                holder,
                LedgerLane::ScaleSet,
                LedgerPermitState::Reserved,
                generation,
            )
            .map_err(ReserveError::Ledger)
    };
    match attempt(ledger, generation)? {
        AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld => {
            Ok((ReserveOutcome::Reserved, generation))
        }
        AcquireOutcome::Full => Ok((ReserveOutcome::CapacityExhausted, generation)),
        AcquireOutcome::NotConfigured => Ok((ReserveOutcome::NotConfigured, generation)),
        AcquireOutcome::StaleGeneration => {
            let fresh = ledger.generation().map_err(ReserveError::Ledger)?;
            match attempt(ledger, fresh)? {
                AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld => {
                    Ok((ReserveOutcome::Reserved, fresh))
                }
                AcquireOutcome::Full => Ok((ReserveOutcome::CapacityExhausted, fresh)),
                AcquireOutcome::NotConfigured => Ok((ReserveOutcome::NotConfigured, fresh)),
                AcquireOutcome::StaleGeneration => Err(ReserveError::DoubleStale { seen: fresh }),
            }
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
) -> Result<bool, L::Error> {
    ledger.release(holder)
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
        let (outcome, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert_eq!(outcome, ReserveOutcome::Reserved);
        assert_eq!(advertise_free(&ledger), 1);
    }

    #[test]
    fn reserve_is_idempotent_per_holder_and_full_when_spent() {
        let mut ledger = ledgers();
        ledger.reconcile(&[]).unwrap();
        let generation = ledger.generation().unwrap();
        let (first, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        let (again, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert_eq!(first, ReserveOutcome::Reserved);
        assert_eq!(again, ReserveOutcome::Reserved);
        assert_eq!(ledger.occupied().unwrap(), 1);
        let _ = reserve_for_offer(&mut ledger, "scaleset/7/2", generation).unwrap();
        let (full, _) = reserve_for_offer(&mut ledger, "scaleset/7/3", generation).unwrap();
        assert_eq!(full, ReserveOutcome::CapacityExhausted);
    }

    #[test]
    fn stale_generation_retries_once_on_fresh_epoch() {
        let mut ledger = ledgers();
        ledger.reconcile(&[]).unwrap();
        let stale = ledger.generation().unwrap();
        ledger.begin_epoch();
        ledger.reconcile(&[]).unwrap();
        let (outcome, landed) = reserve_for_offer(&mut ledger, "scaleset/7/9", stale).unwrap();
        assert_eq!(outcome, ReserveOutcome::Reserved);
        assert_eq!(landed, stale + 1);
    }

    #[test]
    fn reconcile_never_deletes_and_marks_unobserved_uncertain() {
        let mut ledger = ledgers();
        let generation = ledger.generation().unwrap();
        ledger
            .acquire(
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
