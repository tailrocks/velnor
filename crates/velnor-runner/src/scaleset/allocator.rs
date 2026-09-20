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
//! Scale-set holders record no pid (`None`): workers are containers, not
//! host processes, so pid liveness can neither adopt nor sweep them.
//! Crash recovery for this lane is reconcile-by-holder, never pid-based.

use std::path::{Path, PathBuf};

use velnor_control::permit_ledger::{
    AcquireOutcome, LedgerError, PermitLane, PermitLedger, PermitState, ReconcileReport,
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
    /// * `Ok(Some(guard))` — granted (or duplicate delivery onto an
    ///   existing hold, in which case the guard owns nothing and its drop
    ///   releases nothing).
    /// * `Ok(None)` — ledger full; the offer stays queued with its age.
    /// * `Err` — storage failure, unconfigured ledger, or a generation
    ///   that moved twice under one acquire (retry the poll).
    pub fn acquire(&self, holder: &str) -> Result<Option<ScaleSetPermitGuard>, AllocatorError> {
        let mut ledger = PermitLedger::open(&self.ledger_path).map_err(AllocatorError::Storage)?;
        // Retain the resolved DB identity used by this connection. A guard
        // must never follow a later retarget of the allocator's raw path.
        let pinned_ledger_path = ledger.path().to_path_buf();
        let observed = velnor_control::permit_ledger::unix_now();
        ledger
            .observe_demand(holder, PermitLane::ScaleSet, "", observed, observed)
            .map_err(AllocatorError::Storage)?;
        for _ in 0..3 {
            let generation = ledger.generation().map_err(AllocatorError::Storage)?;
            match ledger
                .acquire_with_lease_generation(
                    holder,
                    PermitLane::ScaleSet,
                    PermitState::Acquiring,
                    generation,
                    None,
                    None,
                )
                .map_err(AllocatorError::Storage)?
            {
                (AcquireOutcome::Acquired, Some(lease_generation)) => {
                    return Ok(Some(ScaleSetPermitGuard::owned(
                        &pinned_ledger_path,
                        holder,
                        lease_generation,
                    )));
                }
                // Duplicate delivery holds once: the attempt proceeds on
                // the existing row and releases nothing. (No pid adoption:
                // scale-set holders are requests, not processes; holder
                // liveness is the worker record's job.)
                (AcquireOutcome::AlreadyHeld, Some(lease_generation)) => {
                    return Ok(Some(ScaleSetPermitGuard::unowned(
                        &pinned_ledger_path,
                        holder,
                        lease_generation,
                    )));
                }
                (AcquireOutcome::Acquired | AcquireOutcome::AlreadyHeld, None) => {
                    return Err(AllocatorError::Storage(LedgerError::DemandSourcesUnready(
                        "successful Scale Set acquire returned no lease generation".to_owned(),
                    )));
                }
                (
                    AcquireOutcome::Full
                    | AcquireOutcome::Deferred
                    | AcquireOutcome::NotReady
                    | AcquireOutcome::Closed,
                    _,
                ) => return Ok(None),
                (AcquireOutcome::StaleGeneration, _) => continue,
                (AcquireOutcome::NotConfigured, _) => return Err(AllocatorError::NotConfigured),
            }
        }
        Err(AllocatorError::Contended)
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
/// * `scaleset_alive`: `(holder, state)` attested from the worker
///   registry (recorded workers that are not terminally cleaned).
/// * `native_alive`: holders attested from native in-flight markers.
///
/// Process death or absence from one daemon's local roots does not prove
/// that owned cleanup or peer-daemon handoff completed. Unattested rows
/// stay uncertain and counted until their owning lifecycle releases them.
///
/// This single call attests both lanes at once; separate reconciles would
/// mark the other lane's live rows uncertain.
pub fn startup_reconcile(
    ledger_path: &Path,
    scaleset_alive: &[(&str, PermitState)],
    native_alive: &[&str],
) -> Result<ReconcileReport, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    let mut attested: Vec<(&str, PermitLane, PermitState)> = scaleset_alive
        .iter()
        .map(|(holder, state)| (*holder, PermitLane::ScaleSet, *state))
        .collect();
    attested.extend(
        native_alive
            .iter()
            .map(|holder| (*holder, PermitLane::Native, PermitState::Running)),
    );
    let generation = ledger.generation()?;
    let roster = velnor_control::permit_ledger::read_demand_source_roster(ledger_path)?;
    ledger.reconcile_host_roster(&roster, generation, &attested)
}

/// One scale-set acquisition's held permit. Releases on drop unless
/// disarmed by an explicit terminal call.
#[derive(Debug)]
pub struct ScaleSetPermitGuard {
    ledger_path: PathBuf,
    holder: String,
    /// Immutable identity for the exact row this handle acquired or observed.
    permit_generation: u64,
    /// False when this attempt never spent a permit (duplicate delivery
    /// onto an existing hold): drop does nothing.
    owns_permit: bool,
    disarmed: bool,
}

impl ScaleSetPermitGuard {
    #[must_use]
    pub fn holder(&self) -> &str {
        &self.holder
    }

    #[must_use]
    pub fn permit_generation(&self) -> u64 {
        self.permit_generation
    }

    fn owned(ledger_path: &Path, holder: &str, permit_generation: u64) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder: holder.to_string(),
            permit_generation,
            owns_permit: true,
            disarmed: false,
        }
    }

    fn unowned(ledger_path: &Path, holder: &str, permit_generation: u64) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder: holder.to_string(),
            permit_generation,
            owns_permit: false,
            disarmed: false,
        }
    }

    /// Best-effort state transition; occupancy never depended on the
    /// state spelling, so failures warn loudly and continue.
    fn transition_best_effort(&self, state: PermitState) {
        if !self.owns_permit {
            return;
        }
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => {
                match ledger.transition_if_generation(&self.holder, state, self.permit_generation) {
                    Ok(true) | Ok(false) => {}
                    Err(error) => eprintln!(
                        "Warning: scale-set permit transition to {state:?} failed for {}: {error}",
                        self.holder
                    ),
                }
            }
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
    /// the next reconcile (uncertain) and holder-based recovery.
    pub fn release(mut self) {
        self.disarmed = true;
        if !self.owns_permit {
            return;
        }
        if let Err(error) = release_permit(&self.ledger_path, &self.holder, self.permit_generation)
        {
            eprintln!(
                "Warning: permit ledger release failed for {}: {error}",
                self.holder
            );
        }
    }

    /// Terminal cleanup failure: retain a visible uncertain reservation
    /// instead of releasing fictitious capacity. Recovery converges it
    /// once the residue is actually gone.
    pub fn mark_uncertain_and_disarm(mut self) {
        self.disarmed = true;
        if !self.owns_permit {
            return;
        }
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => {
                match ledger.retain_uncertain_if_generation(&self.holder, self.permit_generation) {
                    Ok(true) | Ok(false) => {}
                    Err(error) => eprintln!(
                        "Warning: uncertain scale-set permit retention failed for {}: {error}",
                        self.holder
                    ),
                }
            }
            Err(error) => eprintln!(
                "Warning: permit ledger open failed for {}: {error}",
                self.holder
            ),
        }
    }
}

impl Drop for ScaleSetPermitGuard {
    fn drop(&mut self) {
        if self.disarmed || !self.owns_permit {
            return;
        }
        // Every non-terminal return makes this demand regrantable with its
        // original age. The permit and demand transition share one
        // transaction. Callers retain uncertain holds after cleanup fails.
        if let Err(error) =
            release_permit_to_eligible(&self.ledger_path, &self.holder, self.permit_generation)
        {
            eprintln!(
                "Warning: permit ledger release failed for {}: {error}",
                self.holder
            );
        }
    }
}

fn release_permit(
    ledger_path: &Path,
    holder: &str,
    expected_lease_generation: u64,
) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_if_generation(holder, expected_lease_generation)
}

fn release_permit_to_eligible(
    ledger_path: &Path,
    holder: &str,
    expected_lease_generation: u64,
) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_to_eligible_if_generation(holder, expected_lease_generation)
}

/// Release one holder's permit from outside the acquiring attempt
/// (worker recovery converging a crashed attempt after it completes and
/// cleans the job). Best-effort: whatever is missed converges via the
/// next reconcile.
pub fn release_permit_best_effort(
    ledger_path: &Path,
    holder: &str,
    expected_lease_generation: u64,
) {
    match release_permit(ledger_path, holder, expected_lease_generation) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("permit ledger: holder {holder} already released; nothing to converge");
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
    /// No `max_jobs` was ever configured: the daemon never opened the
    /// ledger. Acquiring without an authority would spend uncapped.
    NotConfigured,
    /// The generation moved twice during one acquire; retry the poll.
    Contended,
}

impl std::fmt::Display for AllocatorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::NotConfigured => write!(
                f,
                "no max_jobs configured; start the daemon before scale-set acquisition"
            ),
            Self::Contended => write!(
                f,
                "permit ledger generation moved twice during acquire; retry the poll"
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
        let generation = ledger.begin_epoch().unwrap();
        let roster = velnor_control::permit_ledger::read_demand_source_roster(path).unwrap();
        ledger
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
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
        guard.release();
        assert_eq!(allocator.occupied().unwrap(), 0);
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
    fn duplicate_delivery_holds_once_and_releases_nothing() {
        let path = temp_ledger_path("dup");
        configure(&path, 2);
        let allocator = ScaleSetAllocator::open(&path);

        let guard = allocator
            .acquire(&permit_holder(7, 4242))
            .unwrap()
            .expect("grants");
        let duplicate = allocator
            .acquire(&permit_holder(7, 4242))
            .unwrap()
            .expect("duplicate proceeds");
        assert_eq!(allocator.occupied().unwrap(), 1);
        drop(duplicate);
        assert_eq!(allocator.occupied().unwrap(), 1);
        drop(guard);
        assert_eq!(allocator.occupied().unwrap(), 0);
    }

    #[test]
    fn stale_same_holder_guard_cannot_release_replacement_lease() {
        let path = temp_ledger_path("stale-guard");
        configure(&path, 1);
        let allocator = ScaleSetAllocator::open(&path);
        let holder = permit_holder(7, 9001);
        let first = allocator.acquire(&holder).unwrap().unwrap();
        let old_generation = first.permit_generation();
        std::mem::forget(first);

        let mut ledger = PermitLedger::open(&path).unwrap();
        assert!(ledger
            .release_to_eligible_if_generation(&holder, old_generation)
            .unwrap());
        drop(ledger);
        let replacement = allocator.acquire(&holder).unwrap().unwrap();
        let replacement_generation = replacement.permit_generation();
        assert_ne!(old_generation, replacement_generation);

        // Simulate the delayed Drop of an old guard after same-holder
        // reacquisition. It must leave the replacement occupied.
        drop(ScaleSetPermitGuard::owned(&path, &holder, old_generation));
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.permit_lease_generation(&holder).unwrap(),
            Some(replacement_generation)
        );
        assert_eq!(allocator.occupied().unwrap(), 1);
        drop(replacement);
        assert_eq!(allocator.occupied().unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn acquired_scaleset_guard_keeps_ledger_target_after_symlink_retarget() {
        use std::os::unix::fs::symlink;

        let first_path = temp_ledger_path("pinned-ledger-first");
        let second_path = temp_ledger_path("pinned-ledger-second");
        configure(&first_path, 1);
        configure(&second_path, 1);
        let alias = first_path.parent().unwrap().join("ledger-alias.db");
        symlink(&first_path, &alias).unwrap();

        let holder = permit_holder(7, 9002);
        let first_guard = ScaleSetAllocator::open(&alias)
            .acquire(&holder)
            .unwrap()
            .expect("first ledger grants");
        let second_guard = ScaleSetAllocator::open(&second_path)
            .acquire(&holder)
            .unwrap()
            .expect("second ledger grants independently");
        let second_generation = second_guard.permit_generation();

        std::fs::remove_file(&alias).unwrap();
        symlink(&second_path, &alias).unwrap();
        drop(first_guard);

        let first_ledger = PermitLedger::open(&first_path).unwrap();
        assert_eq!(first_ledger.occupied().unwrap(), 0);
        let second_ledger = PermitLedger::open(&second_path).unwrap();
        assert_eq!(second_ledger.occupied().unwrap(), 1);
        assert_eq!(
            second_ledger.permit_lease_generation(&holder).unwrap(),
            Some(second_generation)
        );

        drop(second_guard);
        std::fs::remove_dir_all(first_path.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(second_path.parent().unwrap()).unwrap();
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

        let report = startup_reconcile(&path, &[], &[]).unwrap();
        assert!(report.adopted.is_empty());
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
        let lease_generation = guard.permit_generation();
        guard.mark_uncertain_and_disarm();
        // Still occupied: the reservation is visible, not freed.
        assert_eq!(allocator.occupied().unwrap(), 1);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.holder_state(&permit_holder(7, 4242)).unwrap(),
            Some(PermitState::Uncertain)
        );
        // Recovery converges it once the residue is gone.
        release_permit_best_effort(&path, &permit_holder(7, 4242), lease_generation);
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
        a.release();
        // The terminal demand no longer blocks set 9 from the freed slot.
        let c = allocator
            .acquire(&permit_holder(9, 3))
            .unwrap()
            .expect("freed permit is spendable by the other set");
        assert_eq!(allocator.occupied().unwrap(), 2);
        c.release();
        b.release();
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
        // A native row from the sibling lane (mocked via the ledger API).
        let mut ledger = PermitLedger::open(&path).unwrap();
        let generation = ledger.generation().unwrap();
        assert_eq!(
            ledger
                .acquire(
                    "native/req-1",
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    None
                )
                .unwrap(),
            AcquireOutcome::Acquired
        );

        // New epoch (daemon restart), then ONE reconcile attesting both.
        let mut ledger = PermitLedger::open(&path).unwrap();
        ledger.begin_epoch().unwrap();
        let report = startup_reconcile(
            &path,
            &[("scaleset/7/4242", PermitState::Running)],
            &["native/req-1"],
        )
        .unwrap();
        assert!(report.marked_uncertain.is_empty(), "{report:?}");
        assert_eq!(report.confirmed.len(), 2);
        assert_eq!(allocator.advertised_free().unwrap(), Some(2));
    }
}
