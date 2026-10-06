//! Scale-set lane binding for the ONE host-wide `max_jobs=N` ledger.
//!
//! There is exactly one capacity authority on the host:
//! [`velnor_control::permit_ledger::PermitLedger`]. This module is the
//! scale-set lane's binding to it — the same ledger the native lane
//! acquires from, the same `N`, the same generation fencing, the same
//! reconcile-before-advertise rule. There is NO second ledger here, NO
//! per-scale-set `N`, and NO per-lane reservation: a permit held by set 7
//! denies set 9 exactly as it denies a native acquisition.
//!
//! Holder namespace: `scaleset/<scale-set-id>/<runner-request-id>`.
//! One holder = one acquired GitHub request; redelivery of the same
//! request maps to the same holder and holds once.
//!
//! Lifecycle of one grant:
//! * `acquire` beside the durable acquire intent (`acquiring`); full →
//!   `None` (the offer keeps its age; GitHub redelivers).
//! * `transition_provisioning` when Docker provisioning starts,
//!   `transition_running` when the job executes.
//! * `release` after [`crate::scaleset::worker`] confirms owned cleanup
//!   (or the guard's drop on any early return).
//! * `mark_uncertain_and_disarm` when cleanup itself fails: the visible
//!   reservation stays occupied until recovery converges it.
//!
//! Fresh Scale Set acquisitions record the daemon pid as recovery evidence;
//! the pid alone never authorizes adoption. Recovery must also prove exact
//! worker/container ownership before rotating the attempt token.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use velnor_control::permit_ledger::{
    deferred_wait, AcquireAttemptOutcome, DemandState as PermitDemandState, LedgerError,
    OwnedReleaseOutcome, PermitLane, PermitLedger, PermitState, ReconcileReport,
    DEFERRED_WAIT_BUDGET,
};

/// Scale-set lane allocator over the shared host-wide ledger.
///
/// Cheap to clone; every method opens the ledger file fresh (the ledger
/// is the shared mutable state, guarded by immediate transactions — the
/// allocator itself holds no capacity view that could go stale).
#[derive(Debug, Clone)]
pub struct ScaleSetAllocator {
    ledger_path: PathBuf,
}

impl ScaleSetAllocator {
    /// Bind to the host-wide ledger file. The path MUST be the same file
    /// the native lane uses (production: `permit-ledger.db` next to the
    /// state db, `VELNOR_PERMIT_LEDGER` override — resolved once by the
    /// daemon startup, never per lane).
    #[must_use]
    pub fn open(ledger_path: &Path) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
        }
    }

    #[must_use]
    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    /// Acquire one permit for `holder`.
    ///
    /// * `Ok(Some(guard))` — freshly granted with an attempt token owned by
    ///   the guard.
    /// * `Ok(None)` — ledger full, or older demand never yielded within
    ///   the wait budget; the offer stays queued with its age.
    /// * `Err` — storage/configuration failure, or a duplicate delivery
    ///   that requires proof-bearing recovery before it may proceed.
    pub fn acquire(&self, holder: &str) -> Result<Option<ScaleSetPermitGuard>, AllocatorError> {
        let mut ledger = PermitLedger::open(&self.ledger_path).map_err(AllocatorError::Storage)?;
        let observed = velnor_control::permit_ledger::unix_now();
        ledger
            .observe_demand(holder, PermitLane::ScaleSet, "", observed, observed)
            .map_err(AllocatorError::Storage)?;
        // Younger waiters must outlive the older attempts ahead of them
        // (see DEFERRED_WAIT_BUDGET); every departure below parks its
        // demand so it cannot head-block younger work.
        let deadline = std::time::Instant::now() + DEFERRED_WAIT_BUDGET;
        let mut deferred_attempts = 0u32;
        loop {
            let generation = ledger.generation().map_err(AllocatorError::Storage)?;
            match ledger
                .acquire_attempt(
                    holder,
                    PermitLane::ScaleSet,
                    PermitState::Acquiring,
                    generation,
                    Some(std::process::id()),
                )
                .map_err(AllocatorError::Storage)?
            {
                AcquireAttemptOutcome::Acquired { attempt_token } => {
                    return Ok(Some(ScaleSetPermitGuard::owned(
                        &self.ledger_path,
                        holder,
                        attempt_token,
                    )));
                }
                // Existing rows do not prove that this delivery owns the
                // attempt. Redelivery recovery must first prove the old
                // attempt is gone, then rotate ownership through the
                // explicit adoption path.
                AcquireAttemptOutcome::AlreadyHeld => {
                    return Err(AllocatorError::AlreadyHeld {
                        holder: holder.to_owned(),
                    });
                }
                AcquireAttemptOutcome::Full | AcquireAttemptOutcome::Closed => {
                    park_departed(&mut ledger, holder);
                    return Ok(None);
                }
                AcquireAttemptOutcome::Deferred => {
                    if std::time::Instant::now() >= deadline {
                        park_departed(&mut ledger, holder);
                        return Ok(None);
                    }
                    std::thread::sleep(deferred_wait(deferred_attempts));
                    deferred_attempts = deferred_attempts.saturating_add(1);
                }
                AcquireAttemptOutcome::StaleGeneration => {
                    if std::time::Instant::now() >= deadline {
                        park_departed(&mut ledger, holder);
                        return Ok(None);
                    }
                }
                AcquireAttemptOutcome::NotConfigured => return Err(AllocatorError::NotConfigured),
            }
        }
    }

    /// Free capacity (`N − occupied`) across BOTH lanes — but only after
    /// this epoch reconciled. `None` means "do not advertise yet", never
    /// "infinite". The poll loop carries this value (or skips the poll's
    /// capacity claim) on `X-ScaleSetMaxCapacity`.
    pub fn advertised_free(&self) -> Result<Option<u32>, LedgerError> {
        let ledger = PermitLedger::open(&self.ledger_path)?;
        ledger.advertised_free()
    }

    /// Counted occupants across every lane and state.
    pub fn occupied(&self) -> Result<u32, LedgerError> {
        let ledger = PermitLedger::open(&self.ledger_path)?;
        ledger.occupied()
    }

    /// Counted occupants in the scale-set lane (observability only —
    /// admission never consults a per-lane count).
    pub fn occupied_scaleset(&self) -> Result<u32, LedgerError> {
        let ledger = PermitLedger::open(&self.ledger_path)?;
        ledger.occupied_by_lane(PermitLane::ScaleSet)
    }
}

/// Daemon-startup reconcile over BOTH lanes' attested live sets.
///
/// * `scaleset_alive`: `(holder, attempt_token)` attested from the
///   durable demand/worker records.
/// * `native_alive`: `(holder, attempt_token)` attested from native markers.
///
/// This is the one startup call both lanes share: one exact-token
/// [`PermitLedger::reconcile_attempts`] attests both lanes atomically.
pub fn startup_reconcile(
    ledger_path: &Path,
    scaleset_alive: &[(&str, &str)],
    native_alive: &[(&str, &str)],
) -> Result<ReconcileReport, LedgerError> {
    startup_reconcile_with_staged(ledger_path, scaleset_alive, native_alive, &[], &[], &[])
}

/// Startup reconcile with durable Scale Set recovery stages. Rotation tuples
/// are `(holder, previous_token, target_token)`, release tuples are
/// `(holder, token, demand_state)`, and acquire tuples are `(holder, token)`.
/// Staged holders are omitted from the ordinary exact-live set and validated
/// atomically by the matching recovery record.
pub fn startup_reconcile_with_staged(
    ledger_path: &Path,
    scaleset_alive: &[(&str, &str)],
    native_alive: &[(&str, &str)],
    scaleset_staged: &[(&str, Option<&str>, &str)],
    scaleset_releases: &[(&str, &str, PermitDemandState)],
    scaleset_acquires: &[(&str, &str)],
) -> Result<ReconcileReport, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    let staged_holders: BTreeSet<&str> = scaleset_staged
        .iter()
        .map(|(holder, _, _)| *holder)
        .chain(scaleset_releases.iter().map(|(holder, _, _)| *holder))
        .chain(scaleset_acquires.iter().map(|(holder, _)| *holder))
        .collect();
    let mut attested: Vec<(&str, PermitLane, &str)> = scaleset_alive
        .iter()
        .filter(|(holder, _)| !staged_holders.contains(holder))
        .map(|(holder, attempt_token)| (*holder, PermitLane::ScaleSet, *attempt_token))
        .collect();
    attested.extend(
        native_alive
            .iter()
            .map(|(holder, attempt_token)| (*holder, PermitLane::Native, *attempt_token)),
    );
    let staged: Vec<(&str, PermitLane, Option<&str>, &str)> = scaleset_staged
        .iter()
        .map(|(holder, previous, target)| (*holder, PermitLane::ScaleSet, *previous, *target))
        .collect();
    let staged_releases: Vec<(&str, PermitLane, &str, PermitDemandState)> = scaleset_releases
        .iter()
        .map(|(holder, token, state)| (*holder, PermitLane::ScaleSet, *token, *state))
        .collect();
    let staged_acquisitions: Vec<(&str, PermitLane, &str)> = scaleset_acquires
        .iter()
        .map(|(holder, token)| (*holder, PermitLane::ScaleSet, *token))
        .collect();
    let report = ledger.reconcile_attempts_with_staged_acquisitions(
        &attested,
        &staged,
        &staged_releases,
        &staged_acquisitions,
    )?;
    Ok(report)
}

/// One scale-set acquisition's held permit. Releases on drop unless
/// disarmed by an explicit terminal call.
#[derive(Debug)]
pub struct ScaleSetPermitGuard {
    ledger_path: PathBuf,
    holder: String,
    attempt_token: String,
    drop_target: PermitDemandState,
    disarmed: bool,
}

impl ScaleSetPermitGuard {
    #[must_use]
    pub fn holder(&self) -> &str {
        &self.holder
    }

    #[must_use]
    pub fn attempt_token(&self) -> &str {
        &self.attempt_token
    }

    fn owned(ledger_path: &Path, holder: &str, attempt_token: String) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder: holder.to_string(),
            attempt_token,
            drop_target: PermitDemandState::Eligible,
            disarmed: false,
        }
    }

    /// Best-effort state transition; occupancy never depended on the
    /// state spelling, so failures warn loudly and continue.
    fn transition_best_effort(&self, state: PermitState) {
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) = ledger.transition_owned(
                        &self.holder,
                        state,
                        generation,
                        &self.attempt_token,
                    ) {
                        eprintln!(
                            "Warning: scale-set permit transition to {state:?} failed for {}: {error}",
                            self.holder
                        );
                    }
                }
                Err(error) => eprintln!(
                    "Warning: permit ledger generation read failed for {}: {error}",
                    self.holder
                ),
            },
            Err(error) => eprintln!(
                "Warning: permit ledger open failed for {}: {error}",
                self.holder
            ),
        }
    }

    /// Docker provisioning started for this acquisition.
    pub fn transition_provisioning(&self) {
        self.transition_best_effort(PermitState::Provisioning);
    }

    /// The job started executing.
    pub fn transition_running(&self) {
        self.transition_best_effort(PermitState::Running);
    }

    /// Terminal success: owned cleanup is confirmed, free the permit.
    /// Best-effort with a loud warning: a failed release converges via
    /// the next reconcile (uncertain) and proof-bearing recovery.
    pub fn release(mut self) -> Result<(), LedgerError> {
        self.drop_target = PermitDemandState::Terminal;
        match release_permit_target(
            &self.ledger_path,
            &self.holder,
            &self.attempt_token,
            PermitDemandState::Terminal,
        )? {
            OwnedReleaseOutcome::Released | OwnedReleaseOutcome::AlreadyAbsent => {
                self.disarmed = true;
                Ok(())
            }
            OwnedReleaseOutcome::StaleAttempt => {
                self.disarmed = true;
                Err(LedgerError::StaleAttempt(self.holder.clone()))
            }
        }
    }

    /// Terminal cleanup failure: retain a visible uncertain reservation
    /// instead of releasing fictitious capacity. Recovery converges it
    /// once the residue is actually gone.
    pub fn mark_uncertain_and_disarm(mut self) {
        self.disarmed = true;
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) =
                        ledger.retain_uncertain_owned(&self.holder, generation, &self.attempt_token)
                    {
                        eprintln!(
                            "Warning: uncertain scale-set permit retention failed for {}: {error}",
                            self.holder
                        );
                    }
                }
                Err(error) => eprintln!(
                    "Warning: permit ledger generation read failed for {}: {error}",
                    self.holder
                ),
            },
            Err(error) => eprintln!(
                "Warning: permit ledger open failed for {}: {error}",
                self.holder
            ),
        }
    }
}

impl Drop for ScaleSetPermitGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        // Every non-terminal return makes this demand regrantable with its
        // original age. The permit and demand transition share one
        // transaction. Callers retain uncertain holds after cleanup fails.
        match release_permit_target(
            &self.ledger_path,
            &self.holder,
            &self.attempt_token,
            self.drop_target,
        ) {
            Ok(OwnedReleaseOutcome::Released | OwnedReleaseOutcome::AlreadyAbsent) => {}
            Ok(OwnedReleaseOutcome::StaleAttempt) => eprintln!(
                "Warning: permit release refused a stale attempt for {}",
                self.holder
            ),
            Err(error) => eprintln!(
                "Warning: permit ledger release failed for {}: {error}",
                self.holder
            ),
        }
    }
}

/// Yield our queue place without losing our age: a departed waiter must
/// not head-block younger demand. Best-effort: on storage failure the
/// demand stays eligible (younger waiters defer to it) rather than
/// failing the departure.
fn park_departed(ledger: &mut PermitLedger, holder: &str) {
    if let Err(error) = ledger.park_demand(holder) {
        eprintln!("Warning: permit demand park failed for {holder}: {error}");
    }
}

fn release_permit_target(
    ledger_path: &Path,
    holder: &str,
    attempt_token: &str,
    target: PermitDemandState,
) -> Result<OwnedReleaseOutcome, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_scaleset_staged_owned(holder, attempt_token, target)
}

/// Release one holder's permit from outside the acquiring attempt
/// (worker recovery converging a crashed attempt after it completes and
/// cleans the job). Best-effort: whatever is missed converges via the
/// next reconcile.
pub fn release_permit_best_effort(ledger_path: &Path, holder: &str, attempt_token: &str) {
    match release_permit_target(
        ledger_path,
        holder,
        attempt_token,
        PermitDemandState::Terminal,
    ) {
        Ok(OwnedReleaseOutcome::Released | OwnedReleaseOutcome::AlreadyAbsent) => {}
        Ok(OwnedReleaseOutcome::StaleAttempt) => {
            eprintln!("permit ledger: holder {holder} belongs to a different attempt");
        }
        Err(error) => {
            eprintln!("Warning: permit ledger release failed for {holder}: {error}");
        }
    }
}

/// Why a scale-set acquisition failed. Every variant fails the
/// acquisition closed: the offer keeps its age and GitHub redelivers.
#[derive(Debug)]
pub enum AllocatorError {
    Storage(LedgerError),
    /// A duplicate delivery found a permit owned by another attempt. The
    /// caller must prove recovery before adopting it; this path never
    /// fabricates ownership from the holder name alone.
    AlreadyHeld {
        holder: String,
    },
    /// No `max_jobs` was ever configured: the daemon never opened the
    /// ledger. Acquiring without an authority would spend uncapped.
    NotConfigured,
}

impl std::fmt::Display for AllocatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::AlreadyHeld { holder } => write!(
                f,
                "scale-set permit for {holder:?} is already held; recovery adoption proof required"
            ),
            Self::NotConfigured => write!(
                f,
                "no max_jobs configured; start the daemon before scale-set acquisition"
            ),
        }
    }
}

impl std::error::Error for AllocatorError {}

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
    use crate::scaleset::intents::permit_holder;
    use velnor_control::permit_ledger::AcquireAttemptOutcome;

    fn temp_ledger_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-scaleset-alloc-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("permit-ledger.db")
    }

    fn configure(path: &Path, max_jobs: u32) {
        let mut ledger = PermitLedger::open(path).unwrap();
        ledger.set_max_jobs(max_jobs).unwrap();
        ledger.begin_epoch().unwrap();
        ledger.reconcile_attempts(&[]).unwrap();
    }

    #[test]
    fn holder_namespaces_set_and_request() {
        assert_eq!(permit_holder(7, 4242), "scaleset/7/4242");
        assert_ne!(permit_holder(7, 4242), permit_holder(9, 4242));
    }

    #[test]
    fn acquire_grants_release_frees() {
        let path = temp_ledger_path("cycle");
        configure(&path, 1);
        let allocator = ScaleSetAllocator::open(&path);

        let guard = allocator
            .acquire(&permit_holder(7, 4242))
            .unwrap()
            .expect("first acquire grants");
        assert_eq!(allocator.occupied().unwrap(), 1);
        guard.transition_provisioning();
        guard.transition_running();
        guard.release().unwrap();
        assert_eq!(allocator.occupied().unwrap(), 0);
    }

    #[test]
    fn failed_release_retains_exact_attempt_for_redelivery() {
        let path = temp_ledger_path("release-error");
        configure(&path, 1);
        let allocator = ScaleSetAllocator::open(&path);
        let holder = permit_holder(7, 4243);
        let guard = allocator.acquire(&holder).unwrap().expect("grants");
        let attempt_token = guard.attempt_token().to_owned();
        let directory = path.parent().unwrap().to_path_buf();
        let moved_directory = directory.with_extension("offline");

        // Simulate the ledger becoming unavailable after acquisition. The
        // explicit release and Drop retry both fail; neither may clear the
        // only durable attempt evidence.
        std::fs::rename(&directory, &moved_directory).unwrap();
        assert!(guard.release().is_err());
        std::fs::rename(&moved_directory, &directory).unwrap();

        let ledger = PermitLedger::open(&path).unwrap();
        assert!(ledger.is_current_attempt(&holder, &attempt_token).unwrap());
        assert!(matches!(
            allocator.acquire(&holder),
            Err(AllocatorError::AlreadyHeld { .. })
        ));
        assert_eq!(allocator.occupied().unwrap(), 1);
    }

    #[test]
    fn full_refuses_without_spending() {
        let path = temp_ledger_path("full");
        configure(&path, 1);
        let allocator = ScaleSetAllocator::open(&path);

        let _guard = allocator
            .acquire(&permit_holder(7, 1))
            .unwrap()
            .expect("grants");
        assert!(allocator.acquire(&permit_holder(7, 2)).unwrap().is_none());
        assert_eq!(allocator.occupied().unwrap(), 1);
    }

    #[test]
    fn duplicate_delivery_fails_closed_without_releasing_owner() {
        let path = temp_ledger_path("dup");
        configure(&path, 2);
        let allocator = ScaleSetAllocator::open(&path);

        let guard = allocator
            .acquire(&permit_holder(7, 4242))
            .unwrap()
            .expect("grants");
        assert!(matches!(
            allocator.acquire(&permit_holder(7, 4242)),
            Err(AllocatorError::AlreadyHeld { .. })
        ));
        assert_eq!(allocator.occupied().unwrap(), 1);
        drop(guard);
        assert_eq!(allocator.occupied().unwrap(), 0);
    }

    #[test]
    fn unconfigured_ledger_refuses() {
        let path = temp_ledger_path("unconfigured");
        // Opened but never configured: no N, no epoch, no reconcile.
        PermitLedger::open(&path).unwrap();
        let allocator = ScaleSetAllocator::open(&path);
        assert!(matches!(
            allocator.acquire(&permit_holder(7, 1)).unwrap_err(),
            AllocatorError::NotConfigured
        ));
        assert_eq!(allocator.advertised_free().unwrap(), None);
    }

    #[test]
    fn capacity_advertises_only_after_reconcile() {
        let path = temp_ledger_path("advertise");
        let mut ledger = PermitLedger::open(&path).unwrap();
        ledger.set_max_jobs(4).unwrap();
        ledger.begin_epoch().unwrap();
        let allocator = ScaleSetAllocator::open(&path);
        // Epoch began but reconcile has not run: do not advertise.
        assert_eq!(allocator.advertised_free().unwrap(), None);

        let _report = startup_reconcile(&path, &[], &[]).unwrap();
        assert_eq!(allocator.advertised_free().unwrap(), Some(4));

        let _guard = allocator
            .acquire(&permit_holder(7, 1))
            .unwrap()
            .expect("grants");
        assert_eq!(allocator.advertised_free().unwrap(), Some(3));
    }

    #[test]
    fn cleanup_failure_retains_uncertain() {
        let path = temp_ledger_path("uncertain");
        configure(&path, 1);
        let allocator = ScaleSetAllocator::open(&path);

        let guard = allocator
            .acquire(&permit_holder(7, 4242))
            .unwrap()
            .expect("grants");
        let attempt_token = guard.attempt_token().to_owned();
        guard.mark_uncertain_and_disarm();
        // Still occupied: the reservation is visible, not freed.
        assert_eq!(allocator.occupied().unwrap(), 1);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.holder_state(&permit_holder(7, 4242)).unwrap(),
            Some(PermitState::Uncertain)
        );
        // Recovery converges it once the residue is gone.
        release_permit_best_effort(&path, &permit_holder(7, 4242), &attempt_token);
        assert_eq!(allocator.occupied().unwrap(), 0);
    }

    #[test]
    fn no_per_scope_n_sets_share_one_authority() {
        let path = temp_ledger_path("shared-n");
        configure(&path, 2);
        let allocator = ScaleSetAllocator::open(&path);

        // Set 7 spends the whole N; set 9 is refused (no per-set reserve).
        let a = allocator
            .acquire(&permit_holder(7, 1))
            .unwrap()
            .expect("grants");
        let b = allocator
            .acquire(&permit_holder(7, 2))
            .unwrap()
            .expect("grants");
        assert!(allocator.acquire(&permit_holder(9, 3)).unwrap().is_none());
        // Confirm terminal cleanup for set 7/request 1. Drop would return
        // that older demand to Eligible, so it would correctly win again.
        a.release().unwrap();
        // The terminal demand no longer blocks set 9 from the freed slot.
        let c = allocator
            .acquire(&permit_holder(9, 3))
            .unwrap()
            .expect("freed permit is spendable by the other set");
        assert_eq!(allocator.occupied().unwrap(), 2);
        c.release().unwrap();
        b.release().unwrap();
        assert_eq!(allocator.occupied().unwrap(), 0);
    }

    #[test]
    fn startup_reconcile_attests_both_lanes_at_once() {
        let path = temp_ledger_path("startup");
        configure(&path, 4);
        let allocator = ScaleSetAllocator::open(&path);
        let _guard = allocator
            .acquire(&permit_holder(7, 4242))
            .unwrap()
            .expect("grants");
        let set_attempt_token = _guard.attempt_token().to_owned();
        // A native row from the sibling lane (mocked via the ledger API).
        let mut ledger = PermitLedger::open(&path).unwrap();
        let generation = ledger.generation().unwrap();
        let native_attempt_token = match ledger
            .acquire_attempt(
                "native/req-1",
                PermitLane::Native,
                PermitState::Running,
                generation,
                Some(std::process::id()),
            )
            .unwrap()
        {
            AcquireAttemptOutcome::Acquired { attempt_token } => attempt_token,
            outcome => panic!("unexpected native permit outcome: {outcome:?}"),
        };

        // New epoch (daemon restart), then ONE reconcile attesting both.
        let mut ledger = PermitLedger::open(&path).unwrap();
        ledger.begin_epoch().unwrap();
        let report = startup_reconcile(
            &path,
            &[("scaleset/7/4242", set_attempt_token.as_str())],
            &[("native/req-1", native_attempt_token.as_str())],
        )
        .unwrap();
        assert!(report.marked_uncertain.is_empty(), "{report:?}");
        assert_eq!(report.confirmed.len(), 2);
        assert_eq!(allocator.advertised_free().unwrap(), Some(2));
    }
}
