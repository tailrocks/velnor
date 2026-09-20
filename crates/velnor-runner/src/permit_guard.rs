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
//! Startup may attest locally observed work, but never sweeps an unattested
//! reservation using one daemon's local roots; recorded-job recovery releases
//! a crashed attempt only after it completes and cleans the job.

use std::path::{Path, PathBuf};

use velnor_control::permit_ledger::{
    unix_now, AcquireOutcome, AdoptOutcome, LedgerError, PermitLane, PermitLedger, PermitState,
};

/// Environment override for the host-wide ledger database.
pub const PERMIT_LEDGER_ENV: &str = "VELNOR_PERMIT_LEDGER";

/// Ledger file name next to the operational state db.
pub const PERMIT_LEDGER_FILE: &str = "permit-ledger.db";

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

/// Whether a host pid names a live process. Pid reuse reads as alive: the
/// sweep's error direction is retention, never a double-spend.
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
    /// False when this attempt never spent a permit (duplicate delivery
    /// onto a live hold): drop does nothing.
    owns_permit: bool,
    /// A run-service acquire may have committed or a job may be executing.
    /// Until cleanup/handoff is confirmed, unexpected drop retains the row.
    retain_on_drop: bool,
    disarmed: bool,
}

impl NativePermitGuard {
    pub fn holder(&self) -> &str {
        &self.holder
    }

    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    /// Run-service acquire may reach the service. Unexpected drop retains
    /// uncertain occupancy instead of exposing capacity before a terminal
    /// outcome, cleanup, or handoff is confirmed.
    pub fn retain_until_terminal(&mut self) {
        if self.owns_permit {
            self.retain_on_drop = true;
        }
    }

    /// Acquire the host-wide permit for one acquisition attempt.
    ///
    /// Returns `Ok(None)` when the ledger is full (the caller skips the
    /// acquisition; the broker redelivers). A duplicate delivery onto a
    /// hold whose attempt died adopts the row (same holder, new pid);
    /// onto a live hold it returns a guard that owns nothing: the attempt
    /// proceeds and releases nothing.
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
        for _ in 0..3 {
            let generation = ledger.generation().map_err(GuardError::Storage)?;
            match ledger
                .acquire(
                    &holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(pid),
                )
                .map_err(GuardError::Storage)?
            {
                AcquireOutcome::Acquired => return Ok(Some(Self::owned(ledger_path, holder))),
                AcquireOutcome::AlreadyHeld => {
                    match ledger
                        .adopt_if_pid_dead(
                            &holder,
                            PermitLane::Native,
                            PermitState::Acquiring,
                            generation,
                            pid,
                            &pid_alive,
                        )
                        .map_err(GuardError::Storage)?
                    {
                        AdoptOutcome::Adopted => {
                            return Ok(Some(Self::owned(ledger_path, holder)));
                        }
                        AdoptOutcome::LiveHolder => {
                            return Ok(Some(Self::unowned(ledger_path, holder)));
                        }
                        AdoptOutcome::Missing | AdoptOutcome::StaleGeneration => continue,
                    }
                }
                AcquireOutcome::Full | AcquireOutcome::Deferred | AcquireOutcome::Closed => {
                    return Ok(None)
                }
                AcquireOutcome::StaleGeneration => continue,
                AcquireOutcome::NotConfigured => {
                    return Err(GuardError::NotConfigured);
                }
            }
        }
        Err(GuardError::Contended)
    }

    fn owned(ledger_path: &Path, holder: String) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder,
            owns_permit: true,
            retain_on_drop: false,
            disarmed: false,
        }
    }

    fn unowned(ledger_path: &Path, holder: String) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder,
            owns_permit: false,
            retain_on_drop: false,
            disarmed: false,
        }
    }

    /// Release handle for the teardown thread: `Some` only when this
    /// attempt owns the permit. A duplicate delivery onto a live hold
    /// must never release its winner's row.
    pub fn teardown_release(&self) -> Option<TeardownPermitRelease> {
        if self.owns_permit {
            Some(TeardownPermitRelease {
                ledger_path: self.ledger_path.clone(),
                holder: self.holder.clone(),
            })
        } else {
            None
        }
    }

    /// The job started executing: the acquiring permit is now running.
    /// Best-effort: occupancy never depended on the state spelling.
    pub fn transition_running(&mut self) {
        if !self.owns_permit {
            return;
        }
        // Recording an in-flight job is the durable recovery boundary.
        // Any later unexpected exit must preserve occupancy until the
        // cleanup owner confirms teardown or handoff.
        self.retain_on_drop = true;
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) =
                        ledger.transition(&self.holder, PermitState::Running, generation)
                    {
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

    /// Terminal success: owned cleanup is confirmed, atomically close the
    /// demand and free the permit. Best-effort with a loud warning.
    pub fn release(mut self) {
        self.disarmed = true;
        if !self.owns_permit {
            return;
        }
        if let Err(error) = release_permit(&self.ledger_path, &self.holder) {
            eprintln!(
                "Warning: permit ledger release failed for {}: {error}",
                self.holder
            );
        }
    }

    /// Terminal cleanup failure: retain a visible uncertain reservation
    /// instead of releasing fictitious capacity. The demand status and
    /// permit state transition commit atomically.
    pub fn mark_uncertain_and_disarm(mut self) {
        self.disarmed = true;
        if !self.owns_permit {
            return;
        }
        self.retain_uncertain_best_effort();
    }

    /// Confirm the upstream request is no longer eligible; atomically
    /// release its hold and close demand so it cannot block the queue.
    pub fn release_cancelled(mut self) {
        self.disarmed = true;
        if !self.owns_permit {
            return;
        }
        if let Err(error) = release_permit_cancelled(&self.ledger_path, &self.holder) {
            eprintln!(
                "Warning: cancelled permit release failed for {}: {error}",
                self.holder
            );
        }
    }

    fn retain_uncertain_best_effort(&self) {
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) = ledger.retain_uncertain(&self.holder, generation) {
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
}

impl Drop for NativePermitGuard {
    fn drop(&mut self) {
        if self.disarmed || !self.owns_permit {
            return;
        }
        if self.retain_on_drop {
            // Run-service may have committed assignment, or this attempt
            // may have started workload. Keep the permit counted until the
            // owning lifecycle proves cleanup or handoff.
            self.retain_uncertain_best_effort();
        } else {
            // Before a job is acquired, a non-terminal return is safe to
            // redeliver; preserve its original position in the queue.
            if let Err(error) = release_permit_to_eligible(&self.ledger_path, &self.holder) {
                eprintln!(
                    "Warning: permit ledger release failed for {}: {error}",
                    self.holder
                );
            }
        }
    }
}

fn release_permit(ledger_path: &Path, holder: &str) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release(holder)
}

fn release_permit_to_eligible(ledger_path: &Path, holder: &str) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_to_eligible(holder)
}

fn release_permit_cancelled(ledger_path: &Path, holder: &str) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_cancelled(holder)
}

/// Release one holder's permit from outside the attempt that acquired it
/// (recorded-job recovery). Best-effort with a loud warning. Recovery runs
/// after the job completed and cleaned, so permit release and demand
/// terminalization commit together.
pub fn release_permit_best_effort(ledger_path: &Path, holder: &str) {
    match release_permit(ledger_path, holder) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("permit ledger: holder {holder} already released; nothing to converge");
        }
        Err(error) => {
            eprintln!("Warning: permit ledger release failed for {holder}: {error}");
        }
    }
}

/// Owned cleanup is confirmed inside the teardown thread: release the
/// attempt's permit there. The thread retries teardown until it succeeds,
/// so this runs exactly when cleanup is confirmed — including after a join
/// timeout retained the row as uncertain.
#[derive(Debug, Clone)]
pub struct TeardownPermitRelease {
    ledger_path: PathBuf,
    holder: String,
}

impl TeardownPermitRelease {
    pub fn release_confirmed_cleanup(&self) {
        // The demand row exists (acquire upserts it); terminalization and
        // permit release share one transaction.
        release_permit_best_effort(&self.ledger_path, &self.holder);
    }
}

/// Why a guard acquisition failed. Every variant fails the acquisition
/// closed: the attempt is skipped and the broker redelivers.
#[derive(Debug)]
pub enum GuardError {
    Storage(LedgerError),
    /// No `max_jobs` was ever configured: the daemon never opened the
    /// ledger. Acquiring without an authority would spend uncapped.
    NotConfigured,
    /// The generation moved twice during one acquire; retry the poll.
    Contended,
}

impl std::fmt::Display for GuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::NotConfigured => write!(
                f,
                "no max_jobs configured; start the daemon (or set VELNOR_MAX_JOBS) before acquiring"
            ),
            Self::Contended => write!(
                f,
                "permit ledger generation moved twice during acquire; retry the poll"
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
        ledger.reconcile(&[]).unwrap();
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
        assert!(guard.owns_permit);
        // Full: the second attempt is refused, not queued.
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("req-2"), "test-scope")
                .unwrap()
                .is_none()
        );
        // Duplicate delivery of the same request holds once and owns nothing.
        let duplicate =
            NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
                .unwrap()
                .expect("duplicate delivery proceeds");
        assert!(!duplicate.owns_permit);
        drop(duplicate);
        // Dropping the duplicate released nothing: still full.
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
        reopened.release();
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
    fn confirmed_unavailable_request_cancels_demand_on_release() {
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("cancel-demand");
        configure(&path, 1);
        let guard = NativePermitGuard::acquire(&path, native_permit_holder("req-1"), "test-scope")
            .unwrap()
            .unwrap();
        guard.release_cancelled();

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
    fn redelivery_adopts_dead_attempts_and_not_live_ones() {
        use velnor_control::permit_ledger::PermitLedger;
        let path = temp_ledger_path("adopt");
        configure(&path, 2);
        // A row left by a dead attempt (impossible pid): redelivery adopts
        // it and owns the permit.
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            let generation = ledger.generation().unwrap();
            ledger
                .acquire(
                    &native_permit_holder("crashed"),
                    PermitLane::Native,
                    PermitState::Running,
                    generation,
                    Some(u32::MAX),
                )
                .unwrap();
        }
        let adopted =
            NativePermitGuard::acquire(&path, native_permit_holder("crashed"), "test-scope")
                .unwrap()
                .expect("redelivery proceeds");
        assert!(adopted.owns_permit);
        assert!(adopted.teardown_release().is_some());
        // Occupancy unchanged: the same row, not a second permit.
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);
        adopted.release();

        // A row held by this live test process: redelivery proceeds but
        // owns nothing and releases nothing.
        let owner = NativePermitGuard::acquire(&path, native_permit_holder("live"), "test-scope")
            .unwrap()
            .unwrap();
        let duplicate =
            NativePermitGuard::acquire(&path, native_permit_holder("live"), "test-scope")
                .unwrap()
                .unwrap();
        assert!(!duplicate.owns_permit);
        assert!(duplicate.teardown_release().is_none());
        drop(duplicate);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);
        owner.release();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_reconcile_retains_uncertain_attempts_without_teardown_proof() {
        let path = temp_ledger_path("sweep");
        configure(&path, 4);
        // A live attempt and a crashed attempt (impossible pid).
        let live = NativePermitGuard::acquire(&path, native_permit_holder("live"), "test-scope")
            .unwrap()
            .unwrap();
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            let generation = ledger.generation().unwrap();
            ledger
                .acquire(
                    &native_permit_holder("crashed"),
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(u32::MAX),
                )
                .unwrap();
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
        let (report, swept) =
            crate::scaleset::allocator::startup_reconcile(&path, &[], &[&live_holder], &pid_alive)
                .unwrap();
        assert_eq!(report.confirmed, vec![native_permit_holder("live")]);
        assert_eq!(
            report.marked_uncertain,
            vec![native_permit_holder("crashed")]
        );
        // Native uncertain with a recorded dead pid is the one startup sweep
        // that deletes: the live attested row stays.
        assert_eq!(swept, vec![native_permit_holder("crashed")]);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert!(ledger
            .holder_state(&native_permit_holder("crashed"))
            .unwrap()
            .is_none());
        live.release();
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
            assert_eq!(
                ledger
                    .acquire(
                        &peer_holder,
                        PermitLane::Native,
                        PermitState::Running,
                        generation,
                        Some(std::process::id()),
                    )
                    .unwrap(),
                AcquireOutcome::Acquired
            );
        }

        // A second daemon starts with roots that cannot attest the peer's
        // slot. Its host-wide reconcile may mark the row uncertain, but
        // local absence cannot prove peer teardown or release its permit.
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            ledger.begin_epoch().unwrap();
        }
        let (report, swept) =
            crate::scaleset::allocator::startup_reconcile(&path, &[], &[], &pid_alive).unwrap();
        assert!(report.confirmed.is_empty());
        assert!(report.adopted.is_empty());
        assert_eq!(report.marked_uncertain, vec![peer_holder.clone()]);
        assert!(swept.is_empty());

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
        retry.release();

        // Terminal release serves the demand.
        let guard = NativePermitGuard::acquire(&path, native_permit_holder("b"), "scope-b")
            .unwrap()
            .unwrap();
        guard.release();
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
        let duplicate = NativePermitGuard::acquire(&path, native_permit_holder("d"), "scope-a")
            .unwrap()
            .unwrap();
        drop(duplicate);
        let after = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("d"))
            .unwrap()
            .unwrap();
        assert_eq!(before.first_seen_unix, after.first_seen_unix);
        assert_eq!(before.sequence, after.sequence);
        assert_eq!(before.state, after.state);
        owner.release();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn teardown_and_recovery_releases_serve_the_demand() {
        use velnor_control::permit_ledger::DemandState;
        let path = temp_ledger_path("demand-terminal");
        configure(&path, 4);

        let guard = NativePermitGuard::acquire(&path, native_permit_holder("t"), "scope-a")
            .unwrap()
            .unwrap();
        let teardown = guard.teardown_release().unwrap();
        // The teardown thread confirms cleanup after the guard was
        // disarmed by a join timeout: terminal either way.
        std::mem::forget(guard);
        teardown.release_confirmed_cleanup();
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("t"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Terminal);

        // Recorded-job recovery completes and cleans, then releases.
        let guard = NativePermitGuard::acquire(&path, native_permit_holder("r"), "scope-a")
            .unwrap()
            .unwrap();
        let holder = guard.holder().to_owned();
        let ledger = guard.ledger_path().to_owned();
        std::mem::forget(guard);
        release_permit_best_effort(&ledger, &holder);
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("r"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Terminal);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
