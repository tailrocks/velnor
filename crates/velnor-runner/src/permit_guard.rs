//! Native lane binding for the host-wide `max_jobs=N` permit ledger.
//!
//! One top-level native job holds one ledger permit from acquisition-commit
//! until terminal work and owned cleanup are confirmed. Idle registered
//! slots hold nothing: the permit is acquired in `handle_v2_message`
//! beside the durable acquisition intent, transitions to running when the
//! job starts executing, and is released after teardown confirms owned
//! cleanup — or retained as uncertain when cleanup itself fails.
//!
//! Before a run-service acquire can be sent, dropping the guard releases
//! capacity and returns demand to its original queue position. Once the
//! acquire may have reached the service, unexpected drop retains uncertain
//! occupancy until the owning lifecycle confirms a terminal result, cleanup,
//! or handoff. Cleanup failure therefore cannot release fictitious capacity.
//! Startup may attest locally observed work, but retains every unattested
//! reservation until recorded-job recovery proves terminal completion and
//! cleanup for that exact attempt.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use velnor_control::permit_ledger::{
    deferred_wait, unix_now, AcquireAttemptOutcome, AdoptOutcome, LedgerError,
    OwnedReleaseOutcome as LedgerReleaseOutcome, PermitLane, PermitLedger, PermitState,
    DEFERRED_WAIT_BUDGET,
};

/// Environment override for the host-wide ledger database.
pub const PERMIT_LEDGER_ENV: &str = "VELNOR_PERMIT_LEDGER";

/// Ledger file name next to the operational state db.
pub const PERMIT_LEDGER_FILE: &str = "permit-ledger.db";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CleanupClaimKey {
    ledger_path: PathBuf,
    holder: String,
    attempt_token: String,
}

#[derive(Debug)]
struct ActiveCleanupClaim {
    key: CleanupClaimKey,
    active: AtomicBool,
}

static ACTIVE_CLEANUP_CLAIMS: OnceLock<Mutex<HashMap<CleanupClaimKey, Weak<ActiveCleanupClaim>>>> =
    OnceLock::new();

fn cleanup_claim_registry() -> &'static Mutex<HashMap<CleanupClaimKey, Weak<ActiveCleanupClaim>>> {
    ACTIVE_CLEANUP_CLAIMS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_cleanup_claim_registry(
) -> std::sync::MutexGuard<'static, HashMap<CleanupClaimKey, Weak<ActiveCleanupClaim>>> {
    cleanup_claim_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn canonical_ledger_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn cleanup_claim_is_active_for_holder(
    registry: &mut HashMap<CleanupClaimKey, Weak<ActiveCleanupClaim>>,
    ledger_path: &Path,
    holder: &str,
) -> bool {
    registry.retain(|_, claim| claim.strong_count() > 0);
    let ledger_path = canonical_ledger_path(ledger_path);
    registry.iter().any(|(key, claim)| {
        key.ledger_path == ledger_path
            && key.holder == holder
            && claim
                .upgrade()
                .is_some_and(|claim| claim.active.load(Ordering::Acquire))
    })
}

impl ActiveCleanupClaim {
    fn new(ledger_path: &Path, holder: String, attempt_token: String) -> Arc<Self> {
        Arc::new(Self {
            key: CleanupClaimKey {
                ledger_path: canonical_ledger_path(ledger_path),
                holder,
                attempt_token,
            },
            active: AtomicBool::new(false),
        })
    }

    fn activate(self: &Arc<Self>) {
        let mut registry = lock_cleanup_claim_registry();
        registry.retain(|_, claim| claim.strong_count() > 0);
        self.active.store(true, Ordering::Release);
        registry.insert(self.key.clone(), Arc::downgrade(self));
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
        lock_cleanup_claim_registry().remove(&self.key);
    }
}

/// Holder namespace for native acquisitions: `native/<broker request id>`.
/// Broker request ids are unique per broker; redelivery of the same request
/// maps to the same holder, so duplicate delivery holds once.
pub fn native_permit_holder(runner_request_id: &str) -> String {
    format!("native/{runner_request_id}")
}

/// Default host-wide ledger path: `permit-ledger.db` next to the
/// operational state db (shared by every daemon on the host), or
/// `VELNOR_PERMIT_LEDGER` when set.
pub fn default_permit_ledger_path() -> PathBuf {
    if let Some(path) = std::env::var_os(PERMIT_LEDGER_ENV)
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    let state_db = crate::ops::state_db_path();
    state_db
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(PERMIT_LEDGER_FILE)
}

/// Explicit daemon flag wins; otherwise the host-wide default.
pub fn resolve_permit_ledger_path(explicit: Option<&Path>) -> PathBuf {
    explicit.map_or_else(default_permit_ledger_path, Path::to_path_buf)
}

/// Host-wide `N`: an explicit positive `--max-jobs` wins, otherwise the
/// daemon's slot count (correct only for single-daemon hosts). Never zero.
pub fn resolve_max_jobs(max_jobs: Option<u32>, slots: usize) -> u32 {
    max_jobs
        .filter(|max| *max > 0)
        .or_else(|| u32::try_from(slots).ok().filter(|slots| *slots > 0))
        .unwrap_or(1)
}

/// What a daemon start does with the configured host-wide `N`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptMaxJobs {
    /// No `N` was ever configured, or the operator passed an explicit
    /// `--max-jobs` that differs: write `n`.
    Set(u32),
    /// Another daemon already configured `n` and this start carries no
    /// explicit override: keep it. A second scope daemon's slot count must
    /// never rewrite host capacity — registration is authorization, not
    /// capacity.
    Keep(u32),
}

/// Adopt, don't clobber: the first daemon start configures `N`; later
/// starts keep the configured value unless the operator passed an explicit
/// `--max-jobs`, which always wins as deliberate intent.
pub fn adopt_max_jobs(
    configured: Option<u32>,
    explicit: Option<u32>,
    slots: usize,
) -> AdoptMaxJobs {
    let resolved = resolve_max_jobs(explicit, slots);
    match configured {
        None => AdoptMaxJobs::Set(resolved),
        Some(current) if current == resolved => AdoptMaxJobs::Keep(current),
        Some(_) if explicit.is_some_and(|max| max > 0) => AdoptMaxJobs::Set(resolved),
        Some(current) => AdoptMaxJobs::Keep(current),
    }
}

/// Apply [`adopt_max_jobs`] to an open ledger. Returns the effective `N`
/// and whether this call wrote it.
pub fn apply_max_jobs(
    ledger: &mut PermitLedger,
    explicit: Option<u32>,
    slots: usize,
) -> Result<(u32, bool), LedgerError> {
    let configured = ledger.max_jobs()?;
    match adopt_max_jobs(configured, explicit, slots) {
        AdoptMaxJobs::Set(n) if configured.is_none() => {
            // Two first daemons racing must not both write: the INSERT-if-unset
            // is the atomic adopt. The loser reads the winner's N.
            let adopted = ledger.set_max_jobs_if_unset(n)?;
            let effective = ledger.max_jobs()?.unwrap_or(n);
            Ok((effective, adopted))
        }
        AdoptMaxJobs::Set(n) => {
            ledger.set_max_jobs(n)?;
            Ok((n, true))
        }
        AdoptMaxJobs::Keep(n) => Ok((n, false)),
    }
}

/// Whether a host pid names a live process. Pid reuse reads as alive, so
/// recovery remains conservative when process ownership is ambiguous.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // ESRCH (no such process) is the only "dead" verdict. EPERM and
        // any other error keep the reservation: failing to signal is not
        // proof the process is gone. A zombie still answers the probe and
        // is retained until it is reaped.
        //
        // A pid that cannot name a process (zero, or wider than pid_t)
        // is dead by construction. (Truncating u32::MAX to pid_t would
        // probe -1, the whole process group — always "alive".)
        if pid == 0 || pid > i32::MAX as u32 {
            return false;
        }
        // SAFETY: signal 0 sends nothing; the pid is only probed.
        loop {
            let probed = unsafe { libc::kill(pid as libc::pid_t, 0) };
            if probed == 0 {
                return true;
            }
            match std::io::Error::last_os_error().raw_os_error() {
                Some(code) if code == libc::ESRCH => return false,
                Some(code) if code == libc::EINTR => continue,
                _ => return true,
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// One native acquisition's held permit. Before run-service acquire begins,
/// drop requeues demand. After acquire may have reached the service, drop
/// retains uncertain occupancy unless an explicit terminal call releases it.
///
/// Demand and permit transitions use the same host-wide ledger transaction.
/// Duplicate deliveries that own nothing never release the winner's row.
pub struct NativePermitGuard {
    ledger_path: PathBuf,
    holder: String,
    /// Unique token for the attempt that currently owns the permit.
    attempt_token: String,
    /// Same-process authority still held by an in-flight teardown worker.
    /// PID liveness cannot distinguish that worker from a fresh redelivery.
    cleanup_claim: Arc<ActiveCleanupClaim>,
    /// A run-service acquire may have committed or a job may be executing.
    /// Until cleanup/handoff is confirmed, unexpected drop retains the row.
    retain_on_drop: bool,
    /// Local resource cleanup is confirmed, so a failed terminal release may
    /// demote an exact-token Cleaning row for same-process redelivery.
    cleanup_confirmed: bool,
    disarmed: bool,
}

impl NativePermitGuard {
    pub fn holder(&self) -> &str {
        &self.holder
    }

    /// Token fencing this guard's permit mutations.
    pub fn attempt_token(&self) -> &str {
        &self.attempt_token
    }

    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    /// Run-service acquire may reach the service. Unexpected drop retains
    /// uncertain occupancy instead of exposing capacity before a terminal
    /// outcome, cleanup, or handoff is confirmed.
    pub fn retain_until_terminal(&mut self) {
        self.retain_on_drop = true;
    }

    /// Acquire the host-wide permit for one acquisition attempt.
    ///
    /// Returns `Ok(None)` when admission is unavailable or the same request
    /// already has a live owner. A duplicate delivery onto a hold whose
    /// attempt died, or which this process retained as uncertain, adopts the
    /// row (same holder, new pid). A live duplicate must wait for redelivery;
    /// it cannot execute or write a marker without owning the permit.
    ///
    /// `scope` labels a first observation when ingress did not already
    /// persist the demand age.
    pub fn acquire(
        ledger_path: &Path,
        holder: String,
        scope: &str,
    ) -> Result<Option<Self>, GuardError> {
        let mut ledger = PermitLedger::open(ledger_path).map_err(GuardError::Storage)?;
        let observed = unix_now();
        ledger
            .observe_demand(&holder, PermitLane::Native, scope, observed, observed)
            .map_err(GuardError::Storage)?;
        let pid = std::process::id();
        // Younger waiters must outlive the older attempts ahead of them
        // (see DEFERRED_WAIT_BUDGET); every departure below parks its
        // demand so it cannot head-block younger work.
        let deadline = std::time::Instant::now() + DEFERRED_WAIT_BUDGET;
        let mut deferred_attempts = 0u32;
        loop {
            let generation = ledger.generation().map_err(GuardError::Storage)?;
            let mut claim_registry = lock_cleanup_claim_registry();
            if cleanup_claim_is_active_for_holder(&mut claim_registry, ledger_path, &holder) {
                drop(claim_registry);
                park_departed(&mut ledger, &holder);
                return Ok(None);
            }
            match ledger
                .acquire_attempt(
                    &holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(pid),
                )
                .map_err(GuardError::Storage)?
            {
                AcquireAttemptOutcome::Acquired { attempt_token } => {
                    return Ok(Some(Self::owned(ledger_path, holder, attempt_token)));
                }
                AcquireAttemptOutcome::AlreadyHeld => {
                    let adoption = ledger
                        .adopt_for_redelivery(
                            &holder,
                            PermitLane::Native,
                            PermitState::Acquiring,
                            generation,
                            pid,
                            &pid_alive,
                        )
                        .map_err(GuardError::Storage)?;
                    drop(claim_registry);
                    match adoption {
                        AdoptOutcome::Adopted { attempt_token } => {
                            return Ok(Some(Self::owned(ledger_path, holder, attempt_token)));
                        }
                        AdoptOutcome::LiveHolder => {
                            park_departed(&mut ledger, &holder);
                            return Ok(None);
                        }
                        AdoptOutcome::Missing | AdoptOutcome::StaleGeneration => {
                            if std::time::Instant::now() >= deadline {
                                park_departed(&mut ledger, &holder);
                                return Ok(None);
                            }
                        }
                    }
                }
                AcquireAttemptOutcome::Full | AcquireAttemptOutcome::Closed => {
                    drop(claim_registry);
                    park_departed(&mut ledger, &holder);
                    return Ok(None);
                }
                AcquireAttemptOutcome::Deferred => {
                    drop(claim_registry);
                    if std::time::Instant::now() >= deadline {
                        park_departed(&mut ledger, &holder);
                        return Ok(None);
                    }
                    std::thread::sleep(deferred_wait(deferred_attempts));
                    deferred_attempts = deferred_attempts.saturating_add(1);
                }
                AcquireAttemptOutcome::StaleGeneration => {
                    drop(claim_registry);
                    if std::time::Instant::now() >= deadline {
                        park_departed(&mut ledger, &holder);
                        return Ok(None);
                    }
                }
                AcquireAttemptOutcome::NotConfigured => {
                    return Err(GuardError::NotConfigured);
                }
            }
        }
    }

    fn owned(ledger_path: &Path, holder: String, attempt_token: String) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            cleanup_claim: ActiveCleanupClaim::new(
                ledger_path,
                holder.clone(),
                attempt_token.clone(),
            ),
            holder,
            attempt_token,
            retain_on_drop: false,
            cleanup_confirmed: false,
            disarmed: false,
        }
    }

    /// Release handle for the teardown thread: `Some` only when this
    /// attempt owns the permit. A duplicate delivery onto a live hold
    /// always fences its release to the current attempt token.
    pub fn teardown_release(&self) -> Option<TeardownPermitRelease> {
        self.cleanup_claim.activate();
        Some(TeardownPermitRelease {
            ledger_path: self.ledger_path.clone(),
            holder: self.holder.clone(),
            attempt_token: self.attempt_token.clone(),
            cleanup_claim: Arc::clone(&self.cleanup_claim),
        })
    }

    /// Keep same-process redelivery blocked while a worker performs cleanup
    /// but must preserve the permit for later service-response recovery.
    pub fn teardown_cleanup_claim(&self) -> TeardownCleanupClaim {
        self.cleanup_claim.activate();
        TeardownCleanupClaim {
            cleanup_claim: Arc::clone(&self.cleanup_claim),
        }
    }

    /// The job started executing: the acquiring permit is now running.
    /// Best-effort: occupancy never depended on the state spelling.
    pub fn transition_running(&mut self) {
        // Recording an in-flight job is the durable recovery boundary.
        // Any later unexpected exit must preserve occupancy until the
        // cleanup owner confirms teardown or handoff.
        self.retain_on_drop = true;
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) = ledger.transition_owned(
                        &self.holder,
                        PermitState::Running,
                        generation,
                        &self.attempt_token,
                    ) {
                        eprintln!(
                            "Warning: permit ledger transition to running failed for {}: {error}",
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

    /// Job execution completed or failed and container/service/volume
    /// cleanup is beginning: transition the permit to cleaning.
    /// Best-effort: occupancy never depended on the state spelling.
    pub fn transition_cleaning(&mut self) {
        self.retain_on_drop = true;
        self.cleanup_claim.activate();
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) = ledger.transition_owned(
                        &self.holder,
                        PermitState::Cleaning,
                        generation,
                        &self.attempt_token,
                    ) {
                        eprintln!(
                            "Warning: permit ledger transition to cleaning failed for {}: {error}",
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

    /// Terminal success: owned cleanup is confirmed, atomically close the
    /// demand and free the permit. Callers can keep local recovery evidence
    /// when release fails or a newer attempt owns the row.
    pub fn release(mut self) -> Result<PermitReleaseOutcome, LedgerError> {
        if self.disarmed {
            return Ok(PermitReleaseOutcome::AlreadyAbsent);
        }
        // If the terminal write fails, retain uncertainty on Drop so a
        // same-process redelivery can adopt the exact row after this guard
        // exits. Disarming before the fallible write leaves an active-PID
        // Acquiring/Cleaning row that no retry can safely adopt.
        self.retain_on_drop = true;
        self.cleanup_confirmed = true;
        let outcome = release_permit_owned(&self.ledger_path, &self.holder, &self.attempt_token)?;
        if outcome == PermitReleaseOutcome::StaleAttempt {
            return Ok(outcome);
        }
        self.disarmed = true;
        self.cleanup_claim.deactivate();
        Ok(outcome)
    }

    /// Release the exact attempt once, while a caller holds the in-flight
    /// marker lock and will clear the matching marker afterward. Disarming
    /// here prevents a later guard drop or completion path from issuing a
    /// second absent-row terminal mutation against newer demand.
    pub(crate) fn release_for_terminal_marker(
        &mut self,
    ) -> Result<PermitReleaseOutcome, LedgerError> {
        if self.disarmed {
            return Ok(PermitReleaseOutcome::AlreadyAbsent);
        }
        self.retain_on_drop = true;
        self.cleanup_confirmed = true;
        let outcome = release_permit_owned(&self.ledger_path, &self.holder, &self.attempt_token)?;
        if outcome != PermitReleaseOutcome::StaleAttempt {
            self.disarmed = true;
            self.cleanup_claim.deactivate();
        }
        Ok(outcome)
    }

    /// Stop this handler-owned guard from mutating the ledger after a teardown
    /// worker has already confirmed release of the same attempt.
    pub(crate) fn disarm_after_confirmed_teardown(&mut self) {
        self.disarmed = true;
        self.cleanup_confirmed = true;
        self.cleanup_claim.deactivate();
    }

    /// Terminal cleanup failure: retain a visible uncertain reservation
    /// instead of releasing fictitious capacity. The demand status and
    /// permit state transition commit atomically.
    pub fn mark_uncertain_and_disarm(mut self) {
        self.disarmed = true;
        if self.cleanup_confirmed {
            self.retain_uncertain_after_cleanup_best_effort();
        } else {
            self.retain_uncertain_best_effort();
        }
    }

    /// Confirm the upstream request is no longer eligible; atomically
    /// release its hold and close demand so it cannot block the queue.
    pub fn release_cancelled(mut self) -> Result<PermitReleaseOutcome, LedgerError> {
        // A failed cancellation release must not strand a live-PID
        // Acquiring row. Let Drop retain it as Uncertain/Terminal so the
        // next exact redelivery can adopt it after this attempt is gone.
        self.retain_on_drop = true;
        let outcome =
            release_permit_cancelled_owned(&self.ledger_path, &self.holder, &self.attempt_token)?;
        if outcome == PermitReleaseOutcome::StaleAttempt {
            return Ok(outcome);
        }
        self.disarmed = true;
        self.cleanup_claim.deactivate();
        Ok(outcome)
    }

    fn retain_uncertain_best_effort(&self) {
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) =
                        ledger.retain_uncertain_owned(&self.holder, generation, &self.attempt_token)
                    {
                        eprintln!(
                            "Warning: uncertain permit retention failed for {}: {error}",
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

    fn retain_uncertain_after_cleanup_best_effort(&self) {
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => {
                if let Err(error) =
                    ledger.retain_uncertain_after_cleanup_owned(&self.holder, &self.attempt_token)
                {
                    eprintln!(
                        "Warning: cleaned permit retention failed for {}: {error}",
                        self.holder
                    );
                }
            }
            Err(error) => eprintln!(
                "Warning: permit ledger open failed for {}: {error}",
                self.holder
            ),
        }
    }
}

impl Drop for NativePermitGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        if self.retain_on_drop {
            // Run-service may have committed assignment, or this attempt
            // may have started workload. Keep the permit counted until the
            // owning lifecycle proves cleanup or handoff.
            if self.cleanup_confirmed {
                self.retain_uncertain_after_cleanup_best_effort();
            } else {
                self.retain_uncertain_best_effort();
            }
        } else {
            // Before a job is acquired, a non-terminal return is safe to
            // redeliver; preserve its original position in the queue.
            if let Err(error) = release_permit_to_eligible_owned(
                &self.ledger_path,
                &self.holder,
                &self.attempt_token,
            ) {
                eprintln!(
                    "Warning: permit ledger release failed for {}: {error}",
                    self.holder
                );
                // A failed delete must not strand this live-PID Acquiring
                // row: redelivery can adopt only an Uncertain/Terminal row
                // from this process. Retain occupancy under the exact token
                // so failed pre-acquire cleanup remains recoverable without
                // letting an old guard mutate a newer attempt.
                self.retain_uncertain_best_effort();
            }
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

pub fn release_permit_owned(
    ledger_path: &Path,
    holder: &str,
    attempt_token: &str,
) -> Result<PermitReleaseOutcome, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    Ok(
        match ledger.release_native_terminal_owned(holder, attempt_token)? {
            LedgerReleaseOutcome::Released => PermitReleaseOutcome::Released,
            LedgerReleaseOutcome::AlreadyAbsent => PermitReleaseOutcome::AlreadyAbsent,
            LedgerReleaseOutcome::StaleAttempt => PermitReleaseOutcome::StaleAttempt,
        },
    )
}

fn release_permit_to_eligible_owned(
    ledger_path: &Path,
    holder: &str,
    attempt_token: &str,
) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_to_eligible_owned(holder, attempt_token)
}

fn release_permit_cancelled_owned(
    ledger_path: &Path,
    holder: &str,
    attempt_token: &str,
) -> Result<PermitReleaseOutcome, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    Ok(
        match ledger.release_native_cancelled_owned(holder, attempt_token)? {
            LedgerReleaseOutcome::Released => PermitReleaseOutcome::Released,
            LedgerReleaseOutcome::AlreadyAbsent => PermitReleaseOutcome::AlreadyAbsent,
            LedgerReleaseOutcome::StaleAttempt => PermitReleaseOutcome::StaleAttempt,
        },
    )
}

/// Owned cleanup is confirmed inside the teardown thread: release the
/// attempt's permit there. The thread retries teardown until it succeeds,
/// so this runs exactly when cleanup is confirmed — including after a join
/// timeout retained the row as uncertain.
#[derive(Debug, Clone)]
pub struct TeardownPermitRelease {
    ledger_path: PathBuf,
    holder: String,
    attempt_token: String,
    cleanup_claim: Arc<ActiveCleanupClaim>,
}

/// Same-process cleanup ownership without authority to release the permit.
/// Use when local teardown can finish but the service result remains
/// unacknowledged, so the durable marker and permit must stay for recovery.
#[derive(Debug, Clone)]
pub struct TeardownCleanupClaim {
    cleanup_claim: Arc<ActiveCleanupClaim>,
}

impl TeardownCleanupClaim {
    /// Keep the exact permit in uncertain occupancy after owned cleanup is
    /// confirmed, then release in-process exclusion. The marker and permit
    /// remain for a later service-response redelivery.
    pub fn cleanup_confirmed(&self) -> Result<(), LedgerError> {
        let mut ledger = PermitLedger::open(&self.cleanup_claim.key.ledger_path)?;
        ledger.retain_uncertain_after_cleanup_owned(
            &self.cleanup_claim.key.holder,
            &self.cleanup_claim.key.attempt_token,
        )?;
        self.cleanup_claim.deactivate();
        Ok(())
    }
}

impl TeardownPermitRelease {
    pub fn holder(&self) -> &str {
        &self.holder
    }

    pub fn attempt_token(&self) -> &str {
        &self.attempt_token
    }

    pub fn release_confirmed_cleanup(&self) -> Result<PermitReleaseOutcome, LedgerError> {
        // The demand row exists (acquire upserts it); terminalization and
        // permit release share one transaction.
        let outcome = release_permit_owned(&self.ledger_path, &self.holder, &self.attempt_token)?;
        if outcome != PermitReleaseOutcome::StaleAttempt {
            self.cleanup_claim.deactivate();
        }
        Ok(outcome)
    }
}

/// Outcome of releasing one token-owned permit. `AlreadyAbsent` supports an
/// idempotent retry after permit release succeeded but marker clear failed;
/// `StaleAttempt` means another token still occupies the holder and must be
/// preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermitReleaseOutcome {
    Released,
    AlreadyAbsent,
    StaleAttempt,
}

/// Why a guard acquisition failed. Every variant fails the acquisition
/// closed: the attempt is skipped and the broker redelivers.
#[derive(Debug)]
pub enum GuardError {
    Storage(LedgerError),
    /// No `max_jobs` was ever configured: the daemon never opened the
    /// ledger. Acquiring without an authority would spend uncapped.
    NotConfigured,
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::NotConfigured => write!(
                f,
                "no max_jobs configured; start the daemon (or set VELNOR_MAX_JOBS) before acquiring"
            ),
        }
    }
}

impl std::error::Error for GuardError {}

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

    fn temp_ledger_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-permit-guard-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(PERMIT_LEDGER_FILE)
    }

    fn configure(path: &Path, max_jobs: u32) {
        let mut ledger = PermitLedger::open(path).unwrap();
        ledger.set_max_jobs(max_jobs).unwrap();
        ledger.begin_epoch().unwrap();
        ledger.reconcile_attempts(&[]).unwrap();
    }

    #[test]
    fn resolve_max_jobs_prefers_explicit_positive_n() {
        assert_eq!(resolve_max_jobs(Some(32), 4), 32);
        assert_eq!(resolve_max_jobs(Some(0), 4), 4);
        assert_eq!(resolve_max_jobs(None, 4), 4);
        assert_eq!(resolve_max_jobs(None, 0), 1);
    }

    #[test]
    fn native_holder_namespaces_broker_requests() {
        assert_eq!(native_permit_holder("req-1"), "native/req-1");
    }

    #[test]
    fn guard_acquire_full_and_drop_release() {
        let path = temp_ledger_path("cycle");
        configure(&path, 1);

        let guard = NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
            .unwrap()
            .expect("first acquire grants");
        assert!(guard.teardown_release().is_some());
        // Full: the second attempt is refused, not queued.
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("req-2"), "test-scope")
                .unwrap()
                .is_none()
        );
        // A live duplicate is skipped; it cannot proceed without owning the
        // permit or create a tokenless recovery marker.
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                .unwrap()
                .is_none()
        );
        // The duplicate attempt changed nothing: still full.
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("req-3"), "test-scope")
                .unwrap()
                .is_none()
        );
        // Dropping the owner frees the permit and preserves its original
        // priority. Its retry acquires before younger pending requests.
        drop(guard);
        let reopened =
            NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                .unwrap()
                .expect("older retry keeps its place");
        assert_eq!(reopened.release().unwrap(), PermitReleaseOutcome::Released);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn guard_transition_running_and_uncertain_retention() {
        let path = temp_ledger_path("states");
        configure(&path, 2);

        let mut guard =
            NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                .unwrap()
                .unwrap();
        guard.transition_running();
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
            Some(PermitState::Running)
        );
        drop(ledger);
        // Cleanup failure retains a visible reservation.
        guard.mark_uncertain_and_disarm();
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn guard_transition_cleaning() {
        let path = temp_ledger_path("cleaning-state");
        configure(&path, 2);

        let mut guard =
            NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                .unwrap()
                .unwrap();
        guard.transition_running();
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
            Some(PermitState::Running)
        );
        drop(ledger);

        guard.transition_cleaning();
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
            Some(PermitState::Cleaning)
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);

        assert_eq!(guard.release().unwrap(), PermitReleaseOutcome::Released);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
            None
        );
        assert_eq!(ledger.occupied().unwrap(), 0);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn ambiguous_acquire_guard_drop_retains_an_uncertain_permit() {
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("acquired-drop-uncertain");
        configure(&path, 1);
        let mut guard =
            NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                .unwrap()
                .unwrap();
        // Simulate an acquire response lost after the service may have
        // committed. The guard must retain its row when this attempt exits.
        guard.retain_until_terminal();
        drop(guard);

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger
                .demand(&native_permit_holder("req-1"))
                .unwrap()
                .unwrap()
                .state,
            DemandState::Terminal
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn exact_redelivery_adopts_uncertain_same_process_hold_and_releases_it() {
        use velnor_control::permit_ledger::DemandState;

        for (case, cancelled) in [(200, false), (404, true)] {
            let path = temp_ledger_path(&format!("redelivery-{case}"));
            configure(&path, 2);

            let mut ambiguous =
                NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                    .unwrap()
                    .unwrap();
            ambiguous.retain_until_terminal();
            let previous_token = ambiguous.attempt_token().to_owned();
            drop(ambiguous);

            // Another request remains actively owned by this same process.
            let mut unrelated =
                NativePermitGuard::acquire(&path, native_permit_holder("req-live"), "test-scope")
                    .unwrap()
                    .unwrap();
            unrelated.transition_running();

            let redelivery =
                NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                    .unwrap()
                    .expect("redelivery takes ownership of the uncertain row");
            assert!(redelivery.teardown_release().is_some());

            // A delayed cleanup from the original attempt carries its old
            // token and cannot release the row after redelivery adopted it.
            assert_eq!(
                release_permit_owned(&path, &native_permit_holder("req-1"), &previous_token,)
                    .unwrap(),
                PermitReleaseOutcome::StaleAttempt
            );
            let ledger = PermitLedger::open(&path).unwrap();
            assert_eq!(ledger.occupied().unwrap(), 2);
            assert_eq!(
                ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
                Some(PermitState::Acquiring)
            );
            drop(ledger);

            if cancelled {
                assert_eq!(
                    redelivery.release_cancelled().unwrap(),
                    PermitReleaseOutcome::Released
                );
            } else {
                assert_eq!(
                    redelivery.release().unwrap(),
                    PermitReleaseOutcome::Released
                );
            }

            let ledger = PermitLedger::open(&path).unwrap();
            assert_eq!(ledger.occupied().unwrap(), 1);
            assert_eq!(
                ledger.holder_state(&native_permit_holder("req-1")).unwrap(),
                None
            );
            assert_eq!(
                ledger
                    .holder_state(&native_permit_holder("req-live"))
                    .unwrap(),
                Some(PermitState::Running)
            );
            assert_eq!(
                ledger
                    .demand(&native_permit_holder("req-1"))
                    .unwrap()
                    .unwrap()
                    .state,
                if cancelled {
                    DemandState::Cancelled
                } else {
                    DemandState::Terminal
                }
            );
            drop(ledger);
            assert_eq!(unrelated.release().unwrap(), PermitReleaseOutcome::Released);
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn live_timed_out_teardown_claim_blocks_same_process_redelivery() {
        use std::sync::{Arc, Barrier};
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("live-teardown-claim");
        configure(&path, 2);
        let holder = native_permit_holder("req-1");
        let mut ambiguous = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .unwrap();
        ambiguous.retain_until_terminal();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_cleaning_transition
                 BEFORE UPDATE OF state ON permits
                 WHEN NEW.state = 'cleaning'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced cleaning persistence failure');
                 END;",
            )
            .unwrap();
        // Transition remains best-effort, but it must publish a process-local
        // cleanup claim before trying to write Cleaning.
        ambiguous.transition_cleaning();
        let teardown = ambiguous
            .teardown_release()
            .expect("the attempt owns its cleanup release");
        let old_token = ambiguous.attempt_token().to_owned();
        // The failed transition leaves the detached worker's attempt claim
        // live while guard drop moves the row to Uncertain/Terminal.
        drop(ambiguous);

        let worker_started = Arc::new(Barrier::new(2));
        let finish_cleanup = Arc::new(Barrier::new(2));
        let started = Arc::clone(&worker_started);
        let finish = Arc::clone(&finish_cleanup);
        let worker = std::thread::spawn(move || {
            started.wait();
            finish.wait();
            teardown.release_confirmed_cleanup().unwrap()
        });

        worker_started.wait();
        let ledger = PermitLedger::open(&path).unwrap();
        assert!(ledger.is_current_attempt(&holder, &old_token).unwrap());
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger.demand(&holder).unwrap().unwrap().state,
            DemandState::Terminal
        );
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);

        // Same-PID liveness alone would allow this exact redelivery to steal
        // an Uncertain/Terminal row. The worker's attempt claim blocks it.
        assert!(
            NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
                .unwrap()
                .is_none()
        );
        let ledger = PermitLedger::open(&path).unwrap();
        assert!(ledger.is_current_attempt(&holder, &old_token).unwrap());
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);

        finish_cleanup.wait();
        assert_eq!(worker.join().unwrap(), PermitReleaseOutcome::Released);
        let ledger = PermitLedger::open(&path).unwrap();
        assert!(!ledger.has_permit(&holder).unwrap());
        assert_eq!(ledger.occupied().unwrap(), 0);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn preserve_recovery_worker_claim_blocks_until_cleanup_confirmation() {
        use std::sync::{Arc, Barrier};

        let path = temp_ledger_path("preserve-recovery-cleanup-claim");
        configure(&path, 1);
        let holder = native_permit_holder("req-1");
        let mut ambiguous = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .unwrap();
        ambiguous.retain_until_terminal();
        let cleanup_claim = ambiguous.teardown_cleanup_claim();
        drop(ambiguous);

        let worker_started = Arc::new(Barrier::new(2));
        let finish_cleanup = Arc::new(Barrier::new(2));
        let started = Arc::clone(&worker_started);
        let finish = Arc::clone(&finish_cleanup);
        let worker = std::thread::spawn(move || {
            started.wait();
            finish.wait();
            cleanup_claim.cleanup_confirmed().unwrap();
        });

        worker_started.wait();
        assert!(
            NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
                .unwrap()
                .is_none()
        );
        finish_cleanup.wait();
        worker.join().unwrap();

        // The worker only confirmed local cleanup. The uncertain permit stays
        // for exact redelivery to resolve the service result.
        let redelivery = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .expect("redelivery adopts after the cleanup worker finishes");
        assert_eq!(
            redelivery.release().unwrap(),
            PermitReleaseOutcome::Released
        );
        let ledger = PermitLedger::open(&path).unwrap();
        assert!(!ledger.has_permit(&holder).unwrap());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn confirmed_unavailable_request_cancels_demand_on_release() {
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("cancel-demand");
        configure(&path, 1);
        let guard = NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
            .unwrap()
            .unwrap();
        assert_eq!(
            guard.release_cancelled().unwrap(),
            PermitReleaseOutcome::Released
        );

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 0);
        assert_eq!(
            ledger
                .demand(&native_permit_holder("req-1"))
                .unwrap()
                .unwrap()
                .state,
            DemandState::Cancelled
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_cancelled_release_retains_adoptable_same_process_attempt() {
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("cancel-release-failure");
        configure(&path, 1);
        let holder = native_permit_holder("req-cancel-failure");
        let guard = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .unwrap();
        let old_token = guard.attempt_token().to_owned();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_cancelled_release
                 BEFORE DELETE ON permits
                 BEGIN
                     SELECT RAISE(ABORT, 'forced cancelled release failure');
                 END;",
            )
            .unwrap();

        let error = guard.release_cancelled().unwrap_err();
        assert!(error
            .to_string()
            .contains("forced cancelled release failure"));
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TRIGGER reject_cancelled_release;")
            .unwrap();

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert!(ledger.is_current_attempt(&holder, &old_token).unwrap());
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger.demand(&holder).unwrap().unwrap().state,
            DemandState::Terminal
        );
        drop(ledger);

        let redelivery = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .expect("same-process redelivery adopts after failed release owner drops");
        assert_ne!(redelivery.attempt_token(), old_token);
        assert_eq!(
            redelivery.release().unwrap(),
            PermitReleaseOutcome::Released
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_pre_acquire_drop_retains_adoptable_same_process_attempt() {
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("pre-acquire-drop-release-failure");
        configure(&path, 1);
        let holder = native_permit_holder("req-pre-acquire-drop-failure");
        let guard = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .unwrap();
        let old_token = guard.attempt_token().to_owned();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_pre_acquire_drop_release
                 BEFORE DELETE ON permits
                 BEGIN
                     SELECT RAISE(ABORT, 'forced pre-acquire drop release failure');
                 END;",
            )
            .unwrap();

        drop(guard);

        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TRIGGER reject_pre_acquire_drop_release;")
            .unwrap();
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert!(ledger.is_current_attempt(&holder, &old_token).unwrap());
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger.demand(&holder).unwrap().unwrap().state,
            DemandState::Terminal
        );
        drop(ledger);

        let redelivery = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .expect("same-process redelivery adopts the uncertain pre-acquire attempt");
        assert_ne!(redelivery.attempt_token(), old_token);
        assert_eq!(
            redelivery.release().unwrap(),
            PermitReleaseOutcome::Released
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_cleaning_release_demotes_exact_row_for_same_process_redelivery() {
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("cleaning-release-failure");
        configure(&path, 1);
        let holder = native_permit_holder("req-cleaning-release-failure");
        let mut guard = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .unwrap();
        guard.retain_until_terminal();
        guard.transition_cleaning();
        let old_token = guard.attempt_token().to_owned();
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_cleaned_release
                 BEFORE DELETE ON permits
                 BEGIN
                     SELECT RAISE(ABORT, 'forced cleaned release failure');
                 END;",
            )
            .unwrap();

        let error = guard.release().unwrap_err();
        assert!(error.to_string().contains("forced cleaned release failure"));
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TRIGGER reject_cleaned_release;")
            .unwrap();

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert!(ledger.is_current_attempt(&holder, &old_token).unwrap());
        assert_eq!(
            ledger.holder_state(&holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger.demand(&holder).unwrap().unwrap().state,
            DemandState::Terminal
        );
        drop(ledger);

        let redelivery = NativePermitGuard::acquire(&path, holder.clone(), "test-scope")
            .unwrap()
            .expect("redelivery adopts the cleaned uncertain row");
        assert_ne!(redelivery.attempt_token(), old_token);
        assert_eq!(
            redelivery.release().unwrap(),
            PermitReleaseOutcome::Released
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn redelivery_adopts_dead_attempts_and_not_live_ones() {
        use velnor_control::permit_ledger::PermitLedger;
        let path = temp_ledger_path("adopt");
        configure(&path, 2);
        // A row left by a dead attempt (impossible pid): redelivery adopts
        // it and owns the permit.
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            let generation = ledger.generation().unwrap();
            assert!(matches!(
                ledger
                    .acquire_attempt(
                        &native_permit_holder("crashed"),
                        PermitLane::Native,
                        PermitState::Running,
                        generation,
                        Some(u32::MAX),
                    )
                    .unwrap(),
                AcquireAttemptOutcome::Acquired { .. }
            ));
        }
        let adopted =
            NativePermitGuard::acquire(&path, native_permit_holder("crashed"), "test-scope")
                .unwrap()
                .expect("redelivery proceeds");
        assert!(adopted.teardown_release().is_some());
        // Occupancy unchanged: the same row, not a second permit.
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);
        assert_eq!(adopted.release().unwrap(), PermitReleaseOutcome::Released);

        // A row held by this live test process: redelivery cannot steal it
        // or proceed without ownership.
        let owner = NativePermitGuard::acquire(&path, native_permit_holder("live"), "test-scope")
            .unwrap()
            .unwrap();
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("live"), "test-scope")
                .unwrap()
                .is_none()
        );
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);
        assert_eq!(owner.release().unwrap(), PermitReleaseOutcome::Released);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_reconcile_retains_uncertain_attempts_without_teardown_proof() {
        let path = temp_ledger_path("startup-retain");
        configure(&path, 2);
        // A live attempt and a crashed attempt (impossible pid).
        let live = NativePermitGuard::acquire(&path, native_permit_holder("live"), "test-scope")
            .unwrap()
            .unwrap();
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            let generation = ledger.generation().unwrap();
            assert!(matches!(
                ledger
                    .acquire_attempt(
                        &native_permit_holder("crashed"),
                        PermitLane::Native,
                        PermitState::Acquiring,
                        generation,
                        Some(u32::MAX),
                    )
                    .unwrap(),
                AcquireAttemptOutcome::Acquired { .. }
            ));
        }
        // New epoch (daemon restart): process death alone cannot attest
        // that owned cleanup or peer-daemon handoff completed.
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            ledger.begin_epoch().unwrap();
        }
        // The one startup call site both lanes share (no scale-set
        // attestation on this native-only host).
        let live_holder = native_permit_holder("live");
        let report = crate::scaleset::allocator::startup_reconcile(
            &path,
            &[],
            &[(live_holder.as_str(), live.attempt_token())],
        )
        .unwrap();
        assert_eq!(report.confirmed, vec![native_permit_holder("live")]);
        assert_eq!(
            report.marked_uncertain,
            vec![native_permit_holder("crashed")]
        );
        // Process death is not cleanup proof. Keep the exact native row until
        // its owner completes terminal recovery and releases it.
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 2);
        assert_eq!(
            ledger
                .holder_state(&native_permit_holder("crashed"))
                .unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            crate::scaleset::allocator::ScaleSetAllocator::open(&path)
                .advertised_free()
                .unwrap(),
            Some(0)
        );
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("new"), "test-scope")
                .unwrap()
                .is_none()
        );
        assert_eq!(live.release().unwrap(), PermitReleaseOutcome::Released);
        assert_eq!(ledger.occupied().unwrap(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_from_local_roots_retains_live_peer_permit() {
        let path = temp_ledger_path("peer-root-isolation");
        configure(&path, 2);
        assert!(pid_alive(std::process::id()));
        let peer_holder = native_permit_holder("peer-daemon");
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            let generation = ledger.generation().unwrap();
            assert!(matches!(
                ledger
                    .acquire_attempt(
                        &peer_holder,
                        PermitLane::Native,
                        PermitState::Running,
                        generation,
                        Some(std::process::id()),
                    )
                    .unwrap(),
                AcquireAttemptOutcome::Acquired { .. }
            ));
        }

        // A second daemon starts with roots that cannot attest the peer's
        // slot. Its host-wide reconcile may mark the row uncertain, but
        // local absence cannot prove peer teardown or release its permit.
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            ledger.begin_epoch().unwrap();
        }
        let report = crate::scaleset::allocator::startup_reconcile(&path, &[], &[]).unwrap();
        assert!(report.confirmed.is_empty());
        assert_eq!(report.marked_uncertain, vec![peer_holder.clone()]);

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger.holder_state(&peer_holder).unwrap(),
            Some(PermitState::Uncertain)
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn adopt_max_jobs_sets_once_then_keeps_without_explicit_override() {
        // First start on a fresh ledger: configure.
        assert_eq!(adopt_max_jobs(None, Some(32), 4), AdoptMaxJobs::Set(32));
        assert_eq!(adopt_max_jobs(None, None, 4), AdoptMaxJobs::Set(4));
        assert_eq!(adopt_max_jobs(None, None, 0), AdoptMaxJobs::Set(1));
        // Same value: keep (no write either way).
        assert_eq!(
            adopt_max_jobs(Some(32), Some(32), 4),
            AdoptMaxJobs::Keep(32)
        );
        assert_eq!(adopt_max_jobs(Some(4), None, 4), AdoptMaxJobs::Keep(4));
        // Explicit operator intent always wins, up or down.
        assert_eq!(adopt_max_jobs(Some(32), Some(48), 4), AdoptMaxJobs::Set(48));
        assert_eq!(adopt_max_jobs(Some(32), Some(2), 4), AdoptMaxJobs::Set(2));
        // A second scope daemon's slot fallback must never rewrite host
        // capacity: registration is authorization, not capacity.
        assert_eq!(adopt_max_jobs(Some(32), None, 4), AdoptMaxJobs::Keep(32));
        assert_eq!(adopt_max_jobs(Some(32), Some(0), 4), AdoptMaxJobs::Keep(32));
    }

    #[test]
    fn apply_max_jobs_second_daemon_adopts_first_daemon_n() {
        let path = temp_ledger_path("adopt-n");
        // Daemon A (first scope) starts with explicit N=32.
        let mut ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            apply_max_jobs(&mut ledger, Some(32), 4).unwrap(),
            (32, true)
        );
        assert_eq!(ledger.max_jobs().unwrap(), Some(32));
        drop(ledger);
        // Daemon B (second scope) starts with slots only: it adopts 32
        // instead of clobbering host capacity with its slot count.
        let mut ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(apply_max_jobs(&mut ledger, None, 4).unwrap(), (32, false));
        assert_eq!(ledger.max_jobs().unwrap(), Some(32));
        // An explicit resize still applies.
        assert_eq!(
            apply_max_jobs(&mut ledger, Some(48), 4).unwrap(),
            (48, true)
        );
        assert_eq!(ledger.max_jobs().unwrap(), Some(48));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn concurrent_first_daemon_capacity_adopt_is_atomic() {
        use std::sync::{Arc, Barrier};

        let path = temp_ledger_path("concurrent-adopt-n");
        let barrier = Arc::new(Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let first_path = path.clone();
        let first = std::thread::spawn(move || {
            let mut ledger = PermitLedger::open(&first_path).unwrap();
            first_barrier.wait();
            apply_max_jobs(&mut ledger, None, 4).unwrap()
        });
        let second_barrier = Arc::clone(&barrier);
        let second_path = path.clone();
        let second = std::thread::spawn(move || {
            let mut ledger = PermitLedger::open(&second_path).unwrap();
            second_barrier.wait();
            apply_max_jobs(&mut ledger, None, 8).unwrap()
        });
        barrier.wait();

        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(first.0, second.0);
        assert_ne!(first.1, second.1);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.max_jobs().unwrap(), Some(first.0));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn guard_lifecycle_keeps_demand_in_lockstep() {
        use velnor_control::permit_ledger::DemandState;
        let path = temp_ledger_path("demand-lockstep");
        configure(&path, 4);
        let demand_state = |request: &str| {
            PermitLedger::open(&path)
                .unwrap()
                .demand(&native_permit_holder(request))
                .unwrap()
                .map(|row| row.state)
        };

        // Acquire grants the demand (late insert: no fence submit here).
        let guard = NativePermitGuard::acquire(&path, native_permit_holder("a"), "scope-a")
            .unwrap()
            .unwrap();
        assert_eq!(demand_state("a"), Some(DemandState::Granted));
        // Non-terminal drop makes the demand re-grantable, original age.
        let age = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("a"))
            .unwrap()
            .unwrap()
            .first_seen_unix;
        drop(guard);
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("a"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Eligible);
        assert_eq!(row.first_seen_unix, age);

        // The older retry reacquires first and completes before the next
        // independent demand can proceed.
        let retry = NativePermitGuard::acquire(&path, native_permit_holder("a"), "scope-a")
            .unwrap()
            .unwrap();
        assert_eq!(retry.release().unwrap(), PermitReleaseOutcome::Released);

        // Terminal release serves the demand.
        let guard = NativePermitGuard::acquire(&path, native_permit_holder("b"), "scope-b")
            .unwrap()
            .unwrap();
        assert_eq!(guard.release().unwrap(), PermitReleaseOutcome::Released);
        assert_eq!(demand_state("b"), Some(DemandState::Terminal));

        // Cleanup failure still served the demand; retention is ledger-side.
        let guard = NativePermitGuard::acquire(&path, native_permit_holder("c"), "scope-a")
            .unwrap()
            .unwrap();
        guard.mark_uncertain_and_disarm();
        assert_eq!(demand_state("c"), Some(DemandState::Terminal));

        // Duplicate delivery onto a live hold owns nothing and touches
        // neither the permit nor the demand row.
        let owner = NativePermitGuard::acquire(&path, native_permit_holder("d"), "scope-a")
            .unwrap()
            .unwrap();
        let before = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("d"))
            .unwrap()
            .unwrap();
        let duplicate =
            NativePermitGuard::acquire(&path, native_permit_holder("d"), "scope-a").unwrap();
        assert!(duplicate.is_none());
        let after = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("d"))
            .unwrap()
            .unwrap();
        assert_eq!(before.first_seen_unix, after.first_seen_unix);
        assert_eq!(before.sequence, after.sequence);
        assert_eq!(before.state, after.state);
        assert_eq!(owner.release().unwrap(), PermitReleaseOutcome::Released);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn teardown_and_recovery_releases_serve_the_demand() {
        use velnor_control::permit_ledger::DemandState;
        let path = temp_ledger_path("demand-terminal");
        configure(&path, 4);

        let mut guard = NativePermitGuard::acquire(&path, native_permit_holder("t"), "scope-a")
            .unwrap()
            .unwrap();
        guard.retain_until_terminal();
        let teardown = guard.teardown_release().unwrap();
        // The teardown thread confirms cleanup after the guard was
        // disarmed by a join timeout: terminal either way.
        drop(guard);
        assert_eq!(
            teardown.release_confirmed_cleanup().unwrap(),
            PermitReleaseOutcome::Released
        );
        assert_eq!(
            teardown.release_confirmed_cleanup().unwrap(),
            PermitReleaseOutcome::AlreadyAbsent
        );
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("t"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Terminal);

        // Recorded-job recovery completes and cleans, then releases.
        let mut guard = NativePermitGuard::acquire(&path, native_permit_holder("r"), "scope-a")
            .unwrap()
            .unwrap();
        guard.retain_until_terminal();
        let holder = guard.holder().to_owned();
        let ledger = guard.ledger_path().to_owned();
        let attempt_token = guard.attempt_token().to_owned();
        drop(guard);
        assert_eq!(
            release_permit_owned(&ledger, &holder, &attempt_token).unwrap(),
            PermitReleaseOutcome::Released
        );
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("r"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Terminal);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
