//! Native lane binding for the host-wide `max_jobs=N` permit ledger.
//!
//! One top-level native job holds one ledger permit from acquisition-commit
//! until terminal work and owned cleanup are confirmed. Idle registered
//! slots hold nothing: the permit is acquired in `handle_v2_message`
//! beside the durable acquisition intent, transitions to running when the
//! job starts executing, and is released after teardown confirms owned
//! cleanup — or retained as uncertain when cleanup itself fails.
//!
//! The guard releases on drop, so every early return, error, and unwind
//! frees capacity it spent. Only the explicit cleanup-failure paths mark
//! the permit uncertain instead: a cleanup failure retains a visible
//! reservation rather than releasing fictitious capacity. Crash recovery
//! converges retained rows: the daemon reconciles durable occupancy
//! against slot in-flight markers at startup, sweeps dead attempts, and
//! the recorded-job recovery path releases the crashed attempt's permit
//! after it completes and cleans the job.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use velnor_control::permit_ledger::{
    AcquireOutcome, AdoptOutcome, LedgerError, PermitLane, PermitLedger, PermitState,
};

/// Environment override for the host-wide ledger database.
pub(crate) const PERMIT_LEDGER_ENV: &str = "VELNOR_PERMIT_LEDGER";

/// Ledger file name next to the operational state db.
pub(crate) const PERMIT_LEDGER_FILE: &str = "permit-ledger.db";

/// Holder namespace for native acquisitions: `native/<broker request id>`.
/// Broker request ids are unique per broker; redelivery of the same request
/// maps to the same holder, so duplicate delivery holds once.
pub(crate) fn native_permit_holder(runner_request_id: &str) -> String {
    format!("native/{runner_request_id}")
}

/// Default host-wide ledger path: `permit-ledger.db` next to the
/// operational state db (shared by every daemon on the host), or
/// `VELNOR_PERMIT_LEDGER` when set.
pub(crate) fn default_permit_ledger_path() -> PathBuf {
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
pub(crate) fn resolve_permit_ledger_path(explicit: Option<&Path>) -> PathBuf {
    explicit.map_or_else(default_permit_ledger_path, Path::to_path_buf)
}

/// Host-wide `N`: an explicit positive `--max-jobs` wins, otherwise the
/// daemon's slot count (correct only for single-daemon hosts). Never zero.
pub(crate) fn resolve_max_jobs(max_jobs: Option<u32>, slots: usize) -> u32 {
    max_jobs
        .filter(|max| *max > 0)
        .or_else(|| u32::try_from(slots).ok().filter(|slots| *slots > 0))
        .unwrap_or(1)
}

/// Whether a host pid names a live process. Pid reuse reads as alive: the
/// sweep's error direction is retention, never a double-spend.
pub(crate) fn pid_alive(pid: u32) -> bool {
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

/// One native acquisition's held permit. Releases on drop unless disarmed
/// by an explicit terminal call.
pub(crate) struct NativePermitGuard {
    ledger_path: PathBuf,
    holder: String,
    /// False when this attempt never spent a permit (duplicate delivery
    /// onto a live hold): drop does nothing.
    owns_permit: bool,
    disarmed: bool,
}

impl NativePermitGuard {
    pub(crate) fn holder(&self) -> &str {
        &self.holder
    }

    pub(crate) fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    /// Acquire the host-wide permit for one acquisition attempt.
    ///
    /// Returns `Ok(None)` when the ledger is full (the caller skips the
    /// acquisition; the broker redelivers). A duplicate delivery onto a
    /// hold whose attempt died adopts the row (same holder, new pid);
    /// onto a live hold it returns a guard that owns nothing: the attempt
    /// proceeds and releases nothing.
    pub(crate) fn acquire(ledger_path: &Path, holder: String) -> Result<Option<Self>, GuardError> {
        let mut ledger = PermitLedger::open(ledger_path).map_err(GuardError::Storage)?;
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
                AcquireOutcome::Acquired => {
                    return Ok(Some(Self::owned(ledger_path, holder)));
                }
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
                AcquireOutcome::Full => return Ok(None),
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
            disarmed: false,
        }
    }

    fn unowned(ledger_path: &Path, holder: String) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder,
            owns_permit: false,
            disarmed: false,
        }
    }

    /// Release handle for the teardown thread: `Some` only when this
    /// attempt owns the permit. A duplicate delivery onto a live hold
    /// must never release its winner's row.
    pub(crate) fn teardown_release(&self) -> Option<TeardownPermitRelease> {
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
    pub(crate) fn transition_running(&self) {
        if !self.owns_permit {
            return;
        }
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

    /// Terminal success: owned cleanup is confirmed, free the permit.
    /// Best-effort with a loud warning: a failed release converges via the
    /// next reconcile (uncertain) and sweep (dead pid, no marker).
    pub(crate) fn release(mut self) {
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
    /// instead of releasing fictitious capacity. Lifecycle reconciliation
    /// converges it; the next daemon start sweeps it only when no
    /// in-flight marker still references the holder.
    pub(crate) fn mark_uncertain_and_disarm(mut self) {
        self.disarmed = true;
        if !self.owns_permit {
            return;
        }
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => match ledger.generation() {
                Ok(generation) => {
                    if let Err(error) =
                        ledger.transition(&self.holder, PermitState::Uncertain, generation)
                    {
                        eprintln!(
                            "Warning: permit ledger transition to uncertain failed for {}: {error}",
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
        // Every non-terminal return frees what this attempt spent. Never
        // panics: drop runs on unwind paths too.
        if let Err(error) = release_permit(&self.ledger_path, &self.holder) {
            eprintln!(
                "Warning: permit ledger release failed for {}: {error}",
                self.holder
            );
        }
    }
}

fn release_permit(ledger_path: &Path, holder: &str) -> Result<bool, LedgerError> {
    let ledger = PermitLedger::open(ledger_path)?;
    ledger.release(holder)
}

/// Release one holder's permit from outside the attempt that acquired it
/// (recorded-job recovery). Best-effort with a loud warning: whatever is
/// missed converges via the next reconcile and sweep.
pub(crate) fn release_permit_best_effort(ledger_path: &Path, holder: &str) {
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

/// Reconcile durable occupancy against this daemon's live work and sweep
/// dead attempts. Runs at daemon startup after the generation bump:
/// `alive` carries the in-flight holders observed from slot markers.
pub(crate) fn reconcile_and_sweep(
    ledger_path: &Path,
    alive: &[String],
) -> Result<(velnor_control::permit_ledger::ReconcileReport, Vec<String>), LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    let alive_refs: Vec<(&str, PermitLane, PermitState)> = alive
        .iter()
        .map(|holder| (holder.as_str(), PermitLane::Native, PermitState::Running))
        .collect();
    let report = ledger.reconcile(&alive_refs)?;
    let protected: BTreeSet<String> = alive.iter().cloned().collect();
    let swept = ledger.sweep_dead_uncertain(&pid_alive, &protected)?;
    Ok((report, swept))
}

/// Owned cleanup is confirmed inside the teardown thread: release the
/// attempt's permit there. The thread retries teardown until it succeeds,
/// so this runs exactly when cleanup is confirmed — including after a join
/// timeout retained the row as uncertain.
#[derive(Debug, Clone)]
pub(crate) struct TeardownPermitRelease {
    ledger_path: PathBuf,
    holder: String,
}

impl TeardownPermitRelease {
    pub(crate) fn release_confirmed_cleanup(&self) {
        release_permit_best_effort(&self.ledger_path, &self.holder);
    }
}

/// Why a guard acquisition failed. Every variant fails the acquisition
/// closed: the attempt is skipped and the broker redelivers.
#[derive(Debug)]
pub(crate) enum GuardError {
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

        let guard = NativePermitGuard::acquire(&path, native_permit_holder("req-1"))
            .unwrap()
            .expect("first acquire grants");
        assert!(guard.owns_permit);
        // Full: the second attempt is refused, not queued.
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("req-2"))
                .unwrap()
                .is_none()
        );
        // Duplicate delivery of the same request holds once and owns nothing.
        let duplicate = NativePermitGuard::acquire(&path, native_permit_holder("req-1"))
            .unwrap()
            .expect("duplicate delivery proceeds");
        assert!(!duplicate.owns_permit);
        drop(duplicate);
        // Dropping the duplicate released nothing: still full.
        assert!(
            NativePermitGuard::acquire(&path, native_permit_holder("req-3"))
                .unwrap()
                .is_none()
        );
        // Dropping the owner frees the permit.
        drop(guard);
        let reopened = NativePermitGuard::acquire(&path, native_permit_holder("req-3"))
            .unwrap()
            .expect("released permit is spendable again");
        reopened.release();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn guard_transition_running_and_uncertain_retention() {
        let path = temp_ledger_path("states");
        configure(&path, 2);

        let guard = NativePermitGuard::acquire(&path, native_permit_holder("req-1"))
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
        let adopted = NativePermitGuard::acquire(&path, native_permit_holder("crashed"))
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
        let owner = NativePermitGuard::acquire(&path, native_permit_holder("live"))
            .unwrap()
            .unwrap();
        let duplicate = NativePermitGuard::acquire(&path, native_permit_holder("live"))
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
    fn reconcile_and_sweep_converge_dead_attempts() {
        let path = temp_ledger_path("sweep");
        configure(&path, 4);
        // A live attempt and a crashed attempt (impossible pid).
        let live = NativePermitGuard::acquire(&path, native_permit_holder("live"))
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
        // New epoch (daemon restart): the live pid is gone from this
        // test's view only for "crashed" (u32::MAX is never alive).
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            ledger.begin_epoch().unwrap();
        }
        let (report, swept) = reconcile_and_sweep(&path, &[native_permit_holder("live")]).unwrap();
        assert_eq!(report.confirmed, vec![native_permit_holder("live")]);
        assert_eq!(
            report.marked_uncertain,
            vec![native_permit_holder("crashed")]
        );
        assert_eq!(swept, vec![native_permit_holder("crashed")]);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        live.release();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
