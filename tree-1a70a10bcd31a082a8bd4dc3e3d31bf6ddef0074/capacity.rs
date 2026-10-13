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
use std::sync::Mutex;

use velnor_control::permit_ledger::{DemandState as PermitDemandState, OwnedReleaseOutcome};

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
    pub pid: Option<u32>,
}

/// Outcome of [`CapacityLedger::acquire`]. Mirrors C2 `AcquireOutcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireOutcome {
    Acquired { attempt_token: String },
    AlreadyHeld,
    Full,
    StaleGeneration,
    NotConfigured,
}

/// Outcome of [`CapacityLedger::reconcile_attempts`]. Mirrors exact-token
/// control-ledger confirmation; reconciliation never adopts a holder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub marked_uncertain: Vec<String>,
    pub confirmed: Vec<String>,
}

/// Ledger failures. Mirrors C2 `LedgerError`.
#[derive(Debug)]
pub enum LedgerError {
    Storage(String),
    UnknownHolder(String),
    StaleGeneration { expected: u64, seen: u64 },
    StaleAttempt { holder: String },
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
            Self::StaleAttempt { holder } => {
                write!(f, "permit attempt token no longer owns {holder:?}")
            }
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
    /// Observationally verify a persisted attempt token. This returns only a
    /// boolean; callers must still pass the token to every mutation.
    fn is_current_attempt(&self, holder: &str, attempt_token: &str) -> Result<bool, Self::Error>;
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
    /// Close unheld upstream demand that the lane confirmed is no longer
    /// eligible. A held permit must use one of the cleanup release methods.
    fn cancel_demand(&mut self, _holder: &str) -> Result<bool, Self::Error> {
        Ok(false)
    }
    /// Acquire one permit for `holder`, returning an owner token only for a
    /// fresh acquisition. Existing holders are never ownership transfers.
    fn acquire(
        &mut self,
        holder: &str,
        lane: LedgerLane,
        state: LedgerPermitState,
        generation: u64,
    ) -> Result<AcquireOutcome, Self::Error>;
    /// Acquire with a token already persisted in the Scale Set acquisition
    /// journal. An exact existing token is an idempotent stage replay;
    /// another token is never adopted.
    fn acquire_with_attempt_token(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        generation: u64,
        attempt_token: &str,
    ) -> Result<AcquireOutcome, Self::Error>;
    /// Move one held permit only when `attempt_token` still owns it.
    fn transition(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        generation: u64,
        attempt_token: &str,
    ) -> Result<(), Self::Error>;
    /// Complete a durable Scale Set release stage. `AlreadyAbsent` is
    /// returned only when the same durable demand already has `target`;
    /// stale/newer attempts never count as completed releases.
    fn release_staged(
        &mut self,
        holder: &str,
        attempt_token: &str,
        target: PermitDemandState,
    ) -> Result<OwnedReleaseOutcome, Self::Error>;
    /// Keep exact-token occupancy after cleanup could not be confirmed.
    /// Implementations preserve an active Cleaning state and its demand; other
    /// states become Uncertain and close only still-open demand.
    fn retain_uncertain(
        &mut self,
        holder: &str,
        generation: u64,
        attempt_token: &str,
    ) -> Result<(), Self::Error>;

    /// Reconcile durable occupancy against exact-token observations and mark
    /// this epoch reconciled. Missing or mismatched owners fail closed;
    /// observations never create a permit or mint an owner token.
    fn reconcile_attempts(
        &mut self,
        alive: &[(&str, LedgerLane, &str)],
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
    holders: HashMap<String, MemPermit>,
    demand_states: HashMap<String, PermitDemandState>,
}

#[derive(Debug, Clone)]
struct MemPermit {
    lane: LedgerLane,
    state: LedgerPermitState,
    generation: u64,
    attempt_token: String,
    pid: Option<u32>,
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

    #[cfg(test)]
    pub(crate) fn set_test_holder_pid(&self, holder: &str, pid: Option<u32>) {
        if let Some(permit) = self.lock().holders.get_mut(holder) {
            permit.pid = pid;
        }
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
            .map(|(holder, permit)| LedgerHolder {
                holder: holder.clone(),
                lane: permit.lane,
                state: permit.state,
                generation: permit.generation,
                pid: permit.pid,
            })
            .collect();
        holders.sort_by(|left, right| left.holder.cmp(&right.holder));
        Ok(holders)
    }

    fn holder_state(&self, holder: &str) -> Result<Option<LedgerPermitState>, Self::Error> {
        Ok(self.lock().holders.get(holder).map(|permit| permit.state))
    }

    fn is_current_attempt(&self, holder: &str, attempt_token: &str) -> Result<bool, Self::Error> {
        Ok(self
            .lock()
            .holders
            .get(holder)
            .is_some_and(|permit| permit.attempt_token == attempt_token))
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
        let attempt_token = uuid::Uuid::new_v4().to_string();
        if lane == LedgerLane::ScaleSet {
            ledger
                .demand_states
                .entry(holder.to_owned())
                .or_insert(PermitDemandState::Granted);
        }
        ledger.holders.insert(
            holder.to_owned(),
            MemPermit {
                lane,
                state,
                generation,
                attempt_token: attempt_token.clone(),
                pid: Some(std::process::id()),
            },
        );
        Ok(AcquireOutcome::Acquired { attempt_token })
    }

    fn acquire_with_attempt_token(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        generation: u64,
        attempt_token: &str,
    ) -> Result<AcquireOutcome, Self::Error> {
        if attempt_token.is_empty() {
            return Err(LedgerError::StaleAttempt {
                holder: holder.to_owned(),
            });
        }
        let mut ledger = self.lock();
        if ledger.generation != generation {
            return Ok(AcquireOutcome::StaleGeneration);
        }
        let Some(max) = ledger.max_jobs else {
            return Ok(AcquireOutcome::NotConfigured);
        };
        if let Some(existing) = ledger.holders.get(holder) {
            if existing.lane == LedgerLane::ScaleSet && existing.attempt_token == attempt_token {
                return Ok(AcquireOutcome::Acquired {
                    attempt_token: attempt_token.to_owned(),
                });
            }
            return Ok(AcquireOutcome::AlreadyHeld);
        }
        if u64::try_from(ledger.holders.len()).unwrap_or(u64::MAX) >= u64::from(max) {
            return Ok(AcquireOutcome::Full);
        }
        ledger
            .demand_states
            .entry(holder.to_owned())
            .or_insert(PermitDemandState::Granted);
        ledger.holders.insert(
            holder.to_owned(),
            MemPermit {
                lane: LedgerLane::ScaleSet,
                state,
                generation,
                attempt_token: attempt_token.to_owned(),
                pid: Some(std::process::id()),
            },
        );
        Ok(AcquireOutcome::Acquired {
            attempt_token: attempt_token.to_owned(),
        })
    }

    fn transition(
        &mut self,
        holder: &str,
        state: LedgerPermitState,
        generation: u64,
        attempt_token: &str,
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
        if entry.attempt_token != attempt_token {
            return Err(LedgerError::StaleAttempt {
                holder: holder.to_owned(),
            });
        }
        entry.state = state;
        entry.generation = generation;
        entry.pid = Some(std::process::id());
        Ok(())
    }

    fn release_staged(
        &mut self,
        holder: &str,
        attempt_token: &str,
        target: PermitDemandState,
    ) -> Result<OwnedReleaseOutcome, Self::Error> {
        let mut ledger = self.lock();
        if !matches!(
            target,
            PermitDemandState::Eligible
                | PermitDemandState::Cancelled
                | PermitDemandState::Terminal
        ) {
            return Ok(OwnedReleaseOutcome::StaleAttempt);
        }
        if let Some(permit) = ledger.holders.get(holder) {
            if permit.lane != LedgerLane::ScaleSet || permit.attempt_token != attempt_token {
                return Ok(OwnedReleaseOutcome::StaleAttempt);
            }
            if target == PermitDemandState::Eligible
                && !matches!(
                    ledger.demand_states.get(holder),
                    Some(PermitDemandState::Eligible | PermitDemandState::Granted)
                )
            {
                return Ok(OwnedReleaseOutcome::StaleAttempt);
            }
            ledger.holders.remove(holder);
            ledger.demand_states.insert(holder.to_owned(), target);
            return Ok(OwnedReleaseOutcome::Released);
        }
        if ledger.demand_states.get(holder) == Some(&target) {
            Ok(OwnedReleaseOutcome::AlreadyAbsent)
        } else {
            Ok(OwnedReleaseOutcome::StaleAttempt)
        }
    }

    fn retain_uncertain(
        &mut self,
        holder: &str,
        generation: u64,
        attempt_token: &str,
    ) -> Result<(), Self::Error> {
        let mut ledger = self.lock();
        if ledger.generation != generation {
            return Err(LedgerError::StaleGeneration {
                expected: ledger.generation,
                seen: generation,
            });
        }
        let Some(permit) = ledger.holders.get(holder) else {
            return Err(LedgerError::UnknownHolder(holder.to_owned()));
        };
        if permit.attempt_token != attempt_token {
            return Err(LedgerError::StaleAttempt {
                holder: holder.to_owned(),
            });
        }
        if permit.state == LedgerPermitState::Cleaning {
            // Cleaning is an active cleanup claim. Preserve its state and
            // served demand until cleanup confirms or the lane retries.
            return Ok(());
        }

        if let Some(permit) = ledger.holders.get_mut(holder) {
            permit.state = LedgerPermitState::Uncertain;
            permit.generation = generation;
        }
        if matches!(
            ledger.demand_states.get(holder),
            Some(PermitDemandState::Eligible | PermitDemandState::Granted)
        ) {
            ledger
                .demand_states
                .insert(holder.to_owned(), PermitDemandState::Terminal);
        }
        Ok(())
    }

    fn reconcile_attempts(
        &mut self,
        alive: &[(&str, LedgerLane, &str)],
    ) -> Result<ReconcileReport, Self::Error> {
        let mut ledger = self.lock();
        let mut report = ReconcileReport::default();
        let generation = ledger.generation;
        for (holder, lane, attempt_token) in alive {
            let Some(permit) = ledger.holders.get_mut(*holder) else {
                return Err(LedgerError::StaleAttempt {
                    holder: (*holder).to_owned(),
                });
            };
            if permit.lane != *lane {
                return Err(LedgerError::StaleAttempt {
                    holder: (*holder).to_owned(),
                });
            }
            if attempt_token.is_empty() || permit.attempt_token != *attempt_token {
                return Err(LedgerError::StaleAttempt {
                    holder: (*holder).to_owned(),
                });
            }
            report.confirmed.push((*holder).to_owned());
        }
        let recorded: Vec<String> = ledger.holders.keys().cloned().collect();
        for holder in &recorded {
            if alive.iter().any(|(live, _, _)| live == holder) {
                continue;
            }
            if let Some(entry) = ledger.holders.get_mut(holder) {
                if entry.state != LedgerPermitState::Cleaning {
                    entry.state = LedgerPermitState::Uncertain;
                    entry.generation = generation;
                }
            }
            report.marked_uncertain.push(holder.clone());
        }
        ledger.reconciled_generation = Some(generation);
        report.marked_uncertain.sort();
        report.confirmed.sort();
        Ok(report)
    }

    fn is_stale_generation(error: &Self::Error) -> bool {
        matches!(error, LedgerError::StaleGeneration { .. })
    }
}

/// Outcome of reserving one permit for a granted offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveOutcome {
    /// Fresh `reserved` permit; only this result carries ownership.
    Reserved { attempt_token: String },
    /// A permit already exists. Caller has no ownership and must not proceed.
    AlreadyHeld,
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
/// `generation`. Existing permits are returned tokenless; callers must not
/// treat them as ownership or proceed as if adoption happened.
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
        AcquireOutcome::Acquired { attempt_token } => {
            Ok((ReserveOutcome::Reserved { attempt_token }, generation))
        }
        AcquireOutcome::AlreadyHeld => Ok((ReserveOutcome::AlreadyHeld, generation)),
        AcquireOutcome::Full => Ok((ReserveOutcome::CapacityExhausted, generation)),
        AcquireOutcome::NotConfigured => Ok((ReserveOutcome::NotConfigured, generation)),
        AcquireOutcome::StaleGeneration => {
            let fresh = ledger.generation().map_err(ReserveError::Ledger)?;
            match attempt(ledger, fresh)? {
                AcquireOutcome::Acquired { attempt_token } => {
                    Ok((ReserveOutcome::Reserved { attempt_token }, fresh))
                }
                AcquireOutcome::AlreadyHeld => Ok((ReserveOutcome::AlreadyHeld, fresh)),
                AcquireOutcome::Full => Ok((ReserveOutcome::CapacityExhausted, fresh)),
                AcquireOutcome::NotConfigured => Ok((ReserveOutcome::NotConfigured, fresh)),
                AcquireOutcome::StaleGeneration => Err(ReserveError::DoubleStale { seen: fresh }),
            }
        }
    }
}

/// Replay a caller-tokenized reservation from the durable Scale Set acquire
/// journal. The owner token exists in state storage before the permit row can
/// be committed, making both cross-database crash cuts recoverable.
pub fn reserve_for_offer_with_token<L: CapacityLedger>(
    ledger: &mut L,
    holder: &str,
    generation: u64,
    attempt_token: &str,
) -> Result<(ReserveOutcome, u64), ReserveError<L::Error>> {
    let attempt = |ledger: &mut L, generation| {
        ledger
            .acquire_with_attempt_token(
                holder,
                LedgerPermitState::Reserved,
                generation,
                attempt_token,
            )
            .map_err(ReserveError::Ledger)
    };
    match attempt(ledger, generation)? {
        AcquireOutcome::Acquired { attempt_token } => {
            Ok((ReserveOutcome::Reserved { attempt_token }, generation))
        }
        AcquireOutcome::AlreadyHeld => Ok((ReserveOutcome::AlreadyHeld, generation)),
        AcquireOutcome::Full => Ok((ReserveOutcome::CapacityExhausted, generation)),
        AcquireOutcome::NotConfigured => Ok((ReserveOutcome::NotConfigured, generation)),
        AcquireOutcome::StaleGeneration => {
            let fresh = ledger.generation().map_err(ReserveError::Ledger)?;
            match attempt(ledger, fresh)? {
                AcquireOutcome::Acquired { attempt_token } => {
                    Ok((ReserveOutcome::Reserved { attempt_token }, fresh))
                }
                AcquireOutcome::AlreadyHeld => Ok((ReserveOutcome::AlreadyHeld, fresh)),
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
        ledger.reconcile_attempts(&[]).unwrap();
        assert_eq!(advertise_free(&ledger), 2);
        let generation = ledger.generation().unwrap();
        let (outcome, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert!(matches!(outcome, ReserveOutcome::Reserved { .. }));
        assert_eq!(advertise_free(&ledger), 1);
    }

    #[test]
    fn reserve_is_idempotent_per_holder_and_full_when_spent() {
        let mut ledger = ledgers();
        ledger.reconcile_attempts(&[]).unwrap();
        let generation = ledger.generation().unwrap();
        let (first, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        let (again, _) = reserve_for_offer(&mut ledger, "scaleset/7/1", generation).unwrap();
        assert!(matches!(first, ReserveOutcome::Reserved { .. }));
        assert_eq!(again, ReserveOutcome::AlreadyHeld);
        assert_eq!(ledger.occupied().unwrap(), 1);
        let _ = reserve_for_offer(&mut ledger, "scaleset/7/2", generation).unwrap();
        let (full, _) = reserve_for_offer(&mut ledger, "scaleset/7/3", generation).unwrap();
        assert_eq!(full, ReserveOutcome::CapacityExhausted);
    }

    #[test]
    fn retain_uncertain_preserves_cleaning_but_terminalizes_other_exact_attempts() {
        let mut ledger = ledgers();
        ledger.reconcile_attempts(&[]).unwrap();
        let generation = ledger.generation().unwrap();
        let cleaning_holder = "scaleset/7/cleaning-retain";
        let (reserved, _) = reserve_for_offer(&mut ledger, cleaning_holder, generation).unwrap();
        let ReserveOutcome::Reserved {
            attempt_token: cleaning_token,
        } = reserved
        else {
            panic!("cleaning attempt was not reserved");
        };
        ledger
            .transition(
                cleaning_holder,
                LedgerPermitState::Cleaning,
                generation,
                &cleaning_token,
            )
            .unwrap();
        let cleaning_holders = ledger.holders().unwrap();
        let cleaning_demand = ledger.lock().demand_states.get(cleaning_holder).copied();
        assert_eq!(cleaning_demand, Some(PermitDemandState::Granted));

        assert!(matches!(
            ledger.retain_uncertain(
                cleaning_holder,
                generation.saturating_add(1),
                &cleaning_token
            ),
            Err(LedgerError::StaleGeneration { .. })
        ));
        assert!(matches!(
            ledger.retain_uncertain(cleaning_holder, generation, "stale-token"),
            Err(LedgerError::StaleAttempt { .. })
        ));
        ledger
            .retain_uncertain(cleaning_holder, generation, &cleaning_token)
            .unwrap();
        assert_eq!(ledger.holders().unwrap(), cleaning_holders);
        assert_eq!(
            ledger.lock().demand_states.get(cleaning_holder).copied(),
            cleaning_demand
        );

        let uncertain_holder = "scaleset/7/uncertain-retain";
        let (reserved, _) = reserve_for_offer(&mut ledger, uncertain_holder, generation).unwrap();
        let ReserveOutcome::Reserved {
            attempt_token: uncertain_token,
        } = reserved
        else {
            panic!("uncertain attempt was not reserved");
        };
        ledger
            .retain_uncertain(uncertain_holder, generation, &uncertain_token)
            .unwrap();
        assert_eq!(
            ledger.holder_state(uncertain_holder).unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
        assert_eq!(
            ledger.lock().demand_states.get(uncertain_holder).copied(),
            Some(PermitDemandState::Terminal)
        );
        assert_eq!(ledger.occupied().unwrap(), 2);
    }

    #[test]
    fn stale_attempt_cannot_release_or_transition_reacquired_holder() {
        let mut ledger = ledgers();
        ledger.reconcile_attempts(&[]).unwrap();
        let generation = ledger.generation().unwrap();
        let (first, _) = reserve_for_offer(&mut ledger, "scaleset/7/old", generation).unwrap();
        let old_token = match first {
            ReserveOutcome::Reserved { attempt_token } => attempt_token,
            outcome => panic!("unexpected first reserve outcome: {outcome:?}"),
        };
        assert_eq!(
            ledger
                .release_staged("scaleset/7/old", &old_token, PermitDemandState::Eligible,)
                .unwrap(),
            OwnedReleaseOutcome::Released
        );
        let (second, _) = reserve_for_offer(&mut ledger, "scaleset/7/old", generation).unwrap();
        let new_token = match second {
            ReserveOutcome::Reserved { attempt_token } => attempt_token,
            outcome => panic!("unexpected second reserve outcome: {outcome:?}"),
        };
        assert_ne!(old_token, new_token);

        assert_eq!(
            ledger
                .release_staged("scaleset/7/old", &old_token, PermitDemandState::Eligible,)
                .unwrap(),
            OwnedReleaseOutcome::StaleAttempt
        );
        assert!(matches!(
            ledger.transition(
                "scaleset/7/old",
                LedgerPermitState::Running,
                generation,
                &old_token,
            ),
            Err(LedgerError::StaleAttempt { .. })
        ));
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger
                .release_staged("scaleset/7/old", &new_token, PermitDemandState::Eligible,)
                .unwrap(),
            OwnedReleaseOutcome::Released
        );
    }

    #[test]
    fn stale_generation_retries_once_on_fresh_epoch() {
        let mut ledger = ledgers();
        ledger.reconcile_attempts(&[]).unwrap();
        let stale = ledger.generation().unwrap();
        ledger.begin_epoch();
        ledger.reconcile_attempts(&[]).unwrap();
        let (outcome, landed) = reserve_for_offer(&mut ledger, "scaleset/7/9", stale).unwrap();
        assert!(matches!(outcome, ReserveOutcome::Reserved { .. }));
        assert_eq!(landed, stale + 1);
    }

    #[test]
    fn reconcile_never_deletes_and_marks_unobserved_native_uncertain() {
        let mut ledger = ledgers();
        let generation = ledger.generation().unwrap();
        ledger
            .acquire(
                "native/1",
                LedgerLane::Native,
                LedgerPermitState::Running,
                generation,
            )
            .unwrap();
        let report = ledger.reconcile_attempts(&[]).unwrap();
        assert_eq!(report.marked_uncertain, vec!["native/1".to_owned()]);
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state("native/1").unwrap(),
            Some(LedgerPermitState::Uncertain)
        );
    }
}
