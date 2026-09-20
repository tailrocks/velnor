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

use sha2::{Digest, Sha256};
use velnor_control::permit_ledger::{
    unix_now_parts, AcquireOutcome, AdoptOutcome, LedgerError, PermitLane, PermitLedger,
    PermitState,
};

/// Environment override for the host-wide ledger database.
pub const PERMIT_LEDGER_ENV: &str = "VELNOR_PERMIT_LEDGER";

/// Ledger file name next to the operational state db.
pub const PERMIT_LEDGER_FILE: &str = "permit-ledger.db";

/// Holder namespace for native acquisitions. Scope and request identity both
/// participate so two GitHub scopes cannot collide on an identical request id.
pub fn native_permit_holder(canonical_scope: &str, runner_request_id: &str) -> String {
    let digest = Sha256::digest(canonical_scope.as_bytes());
    let scope_hash = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("native/v1/{scope_hash}/{runner_request_id}")
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

/// Configure capacity on an open ledger. Existing host-wide capacity is
/// preserved unless the operator supplies an explicit positive override.
pub fn apply_max_jobs(
    ledger: &mut PermitLedger,
    explicit: Option<u32>,
    slots: usize,
) -> Result<(u32, bool), LedgerError> {
    let configured = ledger.max_jobs()?;
    if let Some(n) = explicit.filter(|max| *max > 0) {
        if configured == Some(n) {
            return Ok((n, false));
        }
        ledger.set_max_jobs(n)?;
        return Ok((n, true));
    }

    if let Some(n) = configured {
        return Ok((n, false));
    }

    let fallback = resolve_max_jobs(None, slots);
    let adopted = ledger.set_max_jobs_if_unset(fallback)?;
    let effective = ledger.max_jobs()?.unwrap_or(fallback);
    Ok((effective, adopted))
}

/// Whether a host pid can be proved live. Pid reuse reads as alive, and only
/// `ESRCH` proves the old holder is gone during redelivery adoption.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // ESRCH (no such process) is the only "dead" verdict. EPERM and
        // any other error keep the reservation: failing to signal is not
        // proof the process is gone. A zombie still answers the probe and
        // is retained until it is reaped.
        //
        // Invalid identity evidence is unknown, not proof of death.
        // (Truncating u32::MAX to pid_t would probe -1, the whole process
        // group.) Fail closed by treating it as live.
        if pid == 0 || pid > i32::MAX as u32 {
            return true;
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
    /// Immutable acquisition identity; unlike the host epoch this never
    /// changes when the permit state or daemon generation changes.
    permit_generation: u64,
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

    /// Immutable generation identifying this exact acquired permit lease.
    #[must_use]
    pub fn permit_generation(&self) -> u64 {
        self.permit_generation
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
    /// acquisition; the broker redelivers). A duplicate delivery adopts only
    /// an exact dead, pre-acquire lease. A live owner returns a non-owning
    /// guard; unknown or disallowed states stay closed for recovery.
    ///
    /// `scope` labels a first observation when ingress did not already
    /// persist the demand age.
    pub fn acquire(
        ledger_path: &Path,
        holder: String,
        scope: &str,
    ) -> Result<Option<Self>, GuardError> {
        let observed_at = unix_now_parts();
        Self::acquire_with_observed_at(ledger_path, holder, scope, observed_at)
    }

    fn acquire_with_observed_at(
        ledger_path: &Path,
        holder: String,
        scope: &str,
        observed_at: (u64, u32),
    ) -> Result<Option<Self>, GuardError> {
        let mut ledger = PermitLedger::open(ledger_path).map_err(GuardError::Storage)?;
        // PermitLedger resolves the input before opening SQLite. Keep that
        // exact path for every later transition, release, and journal lease
        // record; reopening the caller's mutable symlink could hit a new DB.
        let pinned_ledger_path = ledger.path().to_path_buf();
        let (observed, observed_subsec_nanos) = observed_at;
        ledger
            .observe_demand_with_subsecond(
                &holder,
                PermitLane::Native,
                scope,
                observed,
                observed_subsec_nanos,
                observed,
            )
            .map_err(GuardError::Storage)?;
        let pid = std::process::id();
        let pid_identity = crate::node::cleanup::process_identity(pid)
            .map_err(|error| {
                GuardError::Storage(LedgerError::DemandSourcesUnready(format!(
                    "cannot record native permit owner process identity: {error}"
                )))
            })?
            .ok_or_else(|| {
                GuardError::Storage(LedgerError::DemandSourcesUnready(
                    "native permit owner process disappeared before acquisition".to_owned(),
                ))
            })?;
        for _ in 0..3 {
            let generation = ledger.generation().map_err(GuardError::Storage)?;
            let (outcome, lease_generation) = ledger
                .acquire_with_lease_generation(
                    &holder,
                    PermitLane::Native,
                    PermitState::Acquiring,
                    generation,
                    Some(pid),
                    Some(&pid_identity),
                )
                .map_err(GuardError::Storage)?;
            match outcome {
                AcquireOutcome::Acquired => {
                    let lease_generation = lease_generation.ok_or_else(|| {
                        GuardError::Storage(LedgerError::DemandSourcesUnready(
                            "new permit acquisition returned no lease identity".to_owned(),
                        ))
                    })?;
                    return Ok(Some(Self::owned(
                        &pinned_ledger_path,
                        holder,
                        lease_generation,
                    )));
                }
                AcquireOutcome::AlreadyHeld => {
                    let expected_lease_generation = lease_generation.ok_or_else(|| {
                        GuardError::Storage(LedgerError::DemandSourcesUnready(
                            "existing permit acquisition returned no lease identity".to_owned(),
                        ))
                    })?;
                    match ledger
                        .adopt_if_pid_dead_with_lease_generation(
                            &holder,
                            PermitLane::Native,
                            PermitState::Acquiring,
                            generation,
                            expected_lease_generation,
                            pid,
                            &pid_identity,
                            &|owner_pid, expected_identity| {
                                crate::node::cleanup::process_matches_identity(
                                    owner_pid,
                                    expected_identity,
                                )
                                .unwrap_or(true)
                            },
                        )
                        .map_err(GuardError::Storage)?
                    {
                        (AdoptOutcome::Adopted, Some(lease_generation)) => {
                            return Ok(Some(Self::owned(
                                &pinned_ledger_path,
                                holder,
                                lease_generation,
                            )));
                        }
                        (AdoptOutcome::LiveHolder, _) => {
                            // A live owner still holds this exact lease. Keep
                            // its identity on the non-owning guard so any
                            // marker it helps publish stays correlated; it
                            // cannot release the lease through this handle.
                            return Ok(Some(Self::unowned(
                                &pinned_ledger_path,
                                holder,
                                expected_lease_generation,
                            )));
                        }
                        (AdoptOutcome::Adopted, None) => {
                            return Err(GuardError::Storage(LedgerError::DemandSourcesUnready(
                                "permit adoption returned no lease identity".to_owned(),
                            )));
                        }
                        (AdoptOutcome::Missing | AdoptOutcome::StaleGeneration, _) => continue,
                        (AdoptOutcome::NotAdoptable, _) => return Ok(None),
                    }
                }
                AcquireOutcome::Full
                | AcquireOutcome::Deferred
                | AcquireOutcome::NotReady
                | AcquireOutcome::Closed => return Ok(None),
                AcquireOutcome::StaleGeneration => continue,
                AcquireOutcome::NotConfigured => {
                    return Err(GuardError::NotConfigured);
                }
            }
        }
        Err(GuardError::Contended)
    }

    fn owned(ledger_path: &Path, holder: String, permit_generation: u64) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder,
            permit_generation,
            owns_permit: true,
            retain_on_drop: false,
            disarmed: false,
        }
    }

    fn unowned(ledger_path: &Path, holder: String, permit_generation: u64) -> Self {
        Self {
            ledger_path: ledger_path.to_path_buf(),
            holder,
            permit_generation,
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
                permit_generation: self.permit_generation(),
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
            Ok(mut ledger) => match ledger.transition_if_generation(
                &self.holder,
                PermitState::Running,
                self.permit_generation,
            ) {
                Ok(true) | Ok(false) => {}
                Err(error) => eprintln!(
                    "Warning: permit ledger transition to running failed for {}: {error}",
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
    /// demand and free the permit. Already-released and non-owning guards are
    /// successful no-ops; storage errors remain visible to the caller.
    pub fn release(mut self) -> Result<bool, LedgerError> {
        self.try_release()
    }

    /// A job request that was known never to have been sent stays eligible
    /// for broker redelivery. This differs from terminal release and from a
    /// typed cancellation reply, both of which close the demand.
    pub fn release_to_eligible(mut self) -> Result<bool, LedgerError> {
        if self.disarmed || !self.owns_permit {
            self.disarmed = true;
            return Ok(false);
        }
        // If this exact release fails, Drop retries the eligible transition;
        // it must not turn a known-unsent request into uncertain occupancy.
        self.retain_on_drop = false;
        let released = release_permit_to_eligible_if_generation(
            &self.ledger_path,
            &self.holder,
            self.permit_generation(),
        )?;
        if !released {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "pre-acquire cleanup did not confirm release of native permit {} lease {}",
                self.holder, self.permit_generation
            )));
        }
        self.disarmed = true;
        Ok(true)
    }

    /// Release without consuming the guard so marker cleanup can happen only
    /// after the ledger transaction commits. Keep ownership armed on error:
    /// `Drop` then retains uncertain occupancy after a terminal attempt.
    pub fn try_release(&mut self) -> Result<bool, LedgerError> {
        if self.disarmed || !self.owns_permit {
            self.disarmed = true;
            return Ok(false);
        }
        let released = release_permit_if_generation(
            &self.ledger_path,
            &self.holder,
            self.permit_generation(),
        )?;
        if !released {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "terminal cleanup did not confirm release of native permit {} lease {}",
                self.holder, self.permit_generation
            )));
        }
        self.disarmed = true;
        Ok(true)
    }

    /// Disarm after the teardown thread has released this guard's cloned
    /// release handle and removed the marker. No second ledger write is
    /// needed after that final durable cleanup step.
    pub(crate) fn disarm_after_confirmed_teardown(&mut self) {
        self.disarmed = true;
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
    pub fn release_cancelled(mut self) -> Result<bool, LedgerError> {
        if !self.owns_permit {
            self.disarmed = true;
            return Ok(false);
        }
        // On failure, Drop must retain the hold as uncertain instead of
        // requeueing a request the service has already declared gone.
        self.retain_on_drop = true;
        let released = release_permit_cancelled_if_generation(
            &self.ledger_path,
            &self.holder,
            self.permit_generation(),
        )?;
        if !released {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "cancellation cleanup did not confirm release of native permit {} lease {}",
                self.holder, self.permit_generation
            )));
        }
        self.disarmed = true;
        Ok(true)
    }

    fn retain_uncertain_best_effort(&self) {
        match PermitLedger::open(&self.ledger_path) {
            Ok(mut ledger) => {
                match ledger.retain_uncertain_if_generation(&self.holder, self.permit_generation) {
                    Ok(true) | Ok(false) => {}
                    Err(error) => eprintln!(
                        "Warning: uncertain permit retention failed for {}: {error}",
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
            if let Err(error) = release_permit_to_eligible_if_generation(
                &self.ledger_path,
                &self.holder,
                self.permit_generation,
            ) {
                eprintln!(
                    "Warning: permit ledger release failed for {}: {error}",
                    self.holder
                );
            }
        }
    }
}

pub(crate) fn release_permit_if_generation(
    ledger_path: &Path,
    holder: &str,
    expected_generation: u64,
) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_if_generation(holder, expected_generation)
}

fn release_permit_to_eligible_if_generation(
    ledger_path: &Path,
    holder: &str,
    expected_generation: u64,
) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_to_eligible_if_generation(holder, expected_generation)
}

pub(crate) fn release_permit_cancelled_if_generation(
    ledger_path: &Path,
    holder: &str,
    expected_generation: u64,
) -> Result<bool, LedgerError> {
    let mut ledger = PermitLedger::open(ledger_path)?;
    ledger.release_cancelled_if_generation(holder, expected_generation)
}

/// Owned cleanup is confirmed inside the teardown thread: release the
/// attempt's permit there. The thread retries teardown until it succeeds,
/// so this runs exactly when cleanup is confirmed — including after a join
/// timeout retained the row as uncertain.
#[derive(Debug, Clone)]
pub struct TeardownPermitRelease {
    ledger_path: PathBuf,
    holder: String,
    permit_generation: u64,
}

impl TeardownPermitRelease {
    pub fn holder(&self) -> &str {
        &self.holder
    }

    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    pub fn permit_generation(&self) -> u64 {
        self.permit_generation
    }

    pub fn release_confirmed_cleanup(&self) -> Result<bool, LedgerError> {
        // The demand row exists (acquire upserts it); terminalization and
        // permit release share one transaction. The holder generation fences
        // a replay from releasing a later acquisition with the same request id.
        let released =
            release_permit_if_generation(&self.ledger_path, &self.holder, self.permit_generation)?;
        if !released {
            return Err(LedgerError::DemandSourcesUnready(format!(
                "teardown did not confirm release of native permit {} lease {}",
                self.holder, self.permit_generation
            )));
        }
        Ok(true)
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
        let generation = ledger.begin_epoch().unwrap();
        let roster = velnor_control::permit_ledger::read_demand_source_roster(path).unwrap();
        ledger
            .reconcile_host_roster(&roster, generation, &[])
            .unwrap();
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
        let holder = native_permit_holder("test-scope", "req-1");
        assert!(holder.starts_with("native/v1/"));
        assert!(holder.ends_with("/req-1"));
        assert_eq!(holder.len(), "native/v1/".len() + 64 + 1 + "req-1".len());
        assert_ne!(
            native_permit_holder("scope-a", "req-1"),
            native_permit_holder("scope-b", "req-1")
        );
    }

    #[test]
    fn same_request_id_is_scoped_and_running_redelivery_is_closed() {
        let path = temp_ledger_path("scope-running-redelivery");
        configure(&path, 2);
        let scope_a_holder = native_permit_holder("scope-a", "same-request");
        let scope_b_holder = native_permit_holder("scope-b", "same-request");
        assert_ne!(scope_a_holder, scope_b_holder);

        let mut first = NativePermitGuard::acquire(&path, scope_a_holder.clone(), "scope-a")
            .unwrap()
            .expect("first scoped request obtains its permit");
        first.transition_running();

        assert!(
            NativePermitGuard::acquire(&path, scope_a_holder, "scope-a")
                .unwrap()
                .is_none(),
            "running lease redelivery is closed before another Run Service call"
        );
        let second = NativePermitGuard::acquire(&path, scope_b_holder, "scope-b")
            .unwrap()
            .expect("the same request ID in another scope has its own holder");
        assert!(second.owns_permit);

        first.release().unwrap();
        second.release().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn acquired_native_guard_keeps_ledger_target_after_symlink_retarget() {
        use std::os::unix::fs::symlink;

        let first_path = temp_ledger_path("pinned-ledger-first");
        let second_path = temp_ledger_path("pinned-ledger-second");
        configure(&first_path, 1);
        configure(&second_path, 1);
        let alias = first_path.parent().unwrap().join("ledger-alias.db");
        symlink(&first_path, &alias).unwrap();

        let holder = native_permit_holder("scope", "same-holder");
        let guard = NativePermitGuard::acquire(&alias, holder.clone(), "scope")
            .unwrap()
            .expect("first ledger grants");
        assert_eq!(guard.ledger_path(), first_path.canonicalize().unwrap());
        let second_guard = NativePermitGuard::acquire(&second_path, holder, "scope")
            .unwrap()
            .expect("second ledger grants independently");
        let second_generation = second_guard.permit_generation();

        std::fs::remove_file(&alias).unwrap();
        symlink(&second_path, &alias).unwrap();
        guard.release().unwrap();

        let first_ledger = PermitLedger::open(&first_path).unwrap();
        assert_eq!(first_ledger.occupied().unwrap(), 0);
        let second_ledger = PermitLedger::open(&second_path).unwrap();
        assert_eq!(second_ledger.occupied().unwrap(), 1);
        assert_eq!(
            second_ledger
                .permit_lease_generation(&native_permit_holder("scope", "same-holder"))
                .unwrap(),
            Some(second_generation)
        );

        second_guard.release().unwrap();
        std::fs::remove_dir_all(first_path.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(second_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn guard_acquire_full_and_drop_release() {
        let path = temp_ledger_path("cycle");
        configure(&path, 1);

        let mut guard = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-1"),
            "test-scope",
        )
        .unwrap()
        .expect("first acquire grants");
        assert!(guard.owns_permit);
        // Full: the second attempt is refused, not queued.
        assert!(NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-2"),
            "test-scope"
        )
        .unwrap()
        .is_none());
        // Duplicate delivery of the same request holds once and owns nothing.
        let duplicate = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-1"),
            "test-scope",
        )
        .unwrap()
        .expect("duplicate delivery proceeds");
        assert!(!duplicate.owns_permit);
        drop(duplicate);
        // Dropping the duplicate released nothing: still full.
        assert!(NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-3"),
            "test-scope"
        )
        .unwrap()
        .is_none());
        // Dropping the owner frees the permit and preserves its original
        // priority. Its retry acquires before younger pending requests.
        drop(guard);
        let reopened = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-1"),
            "test-scope",
        )
        .unwrap()
        .expect("older retry keeps its place");
        reopened.release().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn guard_transition_running_and_uncertain_retention() {
        let path = temp_ledger_path("states");
        configure(&path, 2);

        let mut guard = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-1"),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        guard.transition_running();
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger
                .holder_state(&native_permit_holder("test-scope", "req-1"))
                .unwrap(),
            Some(PermitState::Running)
        );
        drop(ledger);
        // Cleanup failure retains a visible reservation.
        guard.mark_uncertain_and_disarm();
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(
            ledger
                .holder_state(&native_permit_holder("test-scope", "req-1"))
                .unwrap(),
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
        let mut guard = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-1"),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        // Simulate an acquire response lost after the service may have
        // committed. The guard must retain its row when this attempt exits.
        guard.retain_until_terminal();
        drop(guard);

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        assert_eq!(
            ledger
                .holder_state(&native_permit_holder("test-scope", "req-1"))
                .unwrap(),
            Some(PermitState::Uncertain)
        );
        assert_eq!(
            ledger
                .demand(&native_permit_holder("test-scope", "req-1"))
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
        let guard = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-1"),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        guard.release_cancelled().unwrap();

        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 0);
        assert_eq!(
            ledger
                .demand(&native_permit_holder("test-scope", "req-1"))
                .unwrap()
                .unwrap()
                .state,
            DemandState::Cancelled
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn known_unsent_acquire_requeues_demand_as_eligible() {
        use velnor_control::permit_ledger::DemandState;

        let path = temp_ledger_path("unsent-eligible");
        configure(&path, 1);
        let mut guard = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "req-1"),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        guard.retain_until_terminal();
        guard.release_to_eligible().unwrap();

        let ledger = PermitLedger::open(&path).unwrap();
        let demand = ledger
            .demand(&native_permit_holder("test-scope", "req-1"))
            .unwrap()
            .unwrap();
        assert_eq!(demand.state, DemandState::Eligible);
        assert_eq!(ledger.occupied().unwrap(), 0);
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
            let holder = native_permit_holder("test-scope", "crashed");
            let observed = velnor_control::permit_ledger::unix_now();
            ledger
                .observe_demand(
                    &holder,
                    PermitLane::Native,
                    "test-scope",
                    observed,
                    observed,
                )
                .unwrap();
            assert_eq!(
                ledger
                    .acquire_with_lease_generation(
                        &holder,
                        PermitLane::Native,
                        PermitState::Acquiring,
                        generation,
                        Some(u32::MAX),
                        Some("dead-process-identity"),
                    )
                    .unwrap()
                    .0,
                AcquireOutcome::Acquired
            );
        }
        let adopted = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "crashed"),
            "test-scope",
        )
        .unwrap()
        .expect("redelivery proceeds");
        assert!(adopted.owns_permit);
        assert!(adopted.teardown_release().is_some());
        // Occupancy unchanged: the same row, not a second permit.
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);
        adopted.release().unwrap();

        // A row held by this live test process: redelivery proceeds but
        // owns nothing and releases nothing.
        let owner = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "live"),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        let duplicate = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "live"),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        assert!(!duplicate.owns_permit);
        assert_eq!(
            duplicate.permit_generation(),
            owner.permit_generation(),
            "a non-owning duplicate still records the winner's exact lease identity"
        );
        assert!(duplicate.teardown_release().is_none());
        drop(duplicate);
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 1);
        drop(ledger);
        owner.release().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_reconcile_retains_uncertain_attempts_without_teardown_proof() {
        let path = temp_ledger_path("sweep");
        configure(&path, 4);
        // A live attempt and a crashed attempt (impossible pid).
        let live = NativePermitGuard::acquire(
            &path,
            native_permit_holder("test-scope", "live"),
            "test-scope",
        )
        .unwrap()
        .unwrap();
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            let generation = ledger.generation().unwrap();
            ledger
                .acquire(
                    &native_permit_holder("test-scope", "crashed"),
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
        let live_holder = native_permit_holder("test-scope", "live");
        let report =
            crate::scaleset::allocator::startup_reconcile(&path, &[], &[&live_holder]).unwrap();
        assert_eq!(
            report.confirmed,
            vec![native_permit_holder("test-scope", "live")]
        );
        assert_eq!(
            report.marked_uncertain,
            vec![native_permit_holder("test-scope", "crashed")]
        );
        let ledger = PermitLedger::open(&path).unwrap();
        assert_eq!(ledger.occupied().unwrap(), 2);
        assert_eq!(
            ledger
                .holder_state(&native_permit_holder("test-scope", "crashed"))
                .unwrap(),
            Some(PermitState::Uncertain)
        );
        live.release().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_from_local_roots_retains_live_peer_permit() {
        let path = temp_ledger_path("peer-root-isolation");
        configure(&path, 2);
        assert!(pid_alive(std::process::id()));
        let peer_holder = native_permit_holder("test-scope", "peer-daemon");
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
        let report = crate::scaleset::allocator::startup_reconcile(&path, &[], &[]).unwrap();
        assert!(report.confirmed.is_empty());
        assert!(report.adopted.is_empty());
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
    fn apply_max_jobs_configures_once_and_respects_explicit_override() {
        let path = temp_ledger_path("max-jobs-policy");
        let mut ledger = PermitLedger::open(&path).unwrap();

        // Empty capacity resolves from slots, but zero slots still means one.
        assert_eq!(apply_max_jobs(&mut ledger, None, 0).unwrap(), (1, true));
        assert_eq!(apply_max_jobs(&mut ledger, None, 4).unwrap(), (1, false));
        // Explicit operator intent may raise or lower host-wide capacity.
        assert_eq!(
            apply_max_jobs(&mut ledger, Some(32), 4).unwrap(),
            (32, true)
        );
        assert_eq!(
            apply_max_jobs(&mut ledger, Some(32), 4).unwrap(),
            (32, false)
        );
        assert_eq!(
            apply_max_jobs(&mut ledger, Some(0), 4).unwrap(),
            (32, false)
        );
        assert_eq!(
            apply_max_jobs(&mut ledger, Some(48), 4).unwrap(),
            (48, true)
        );
        assert_eq!(apply_max_jobs(&mut ledger, Some(2), 4).unwrap(), (2, true));
        assert_eq!(ledger.max_jobs().unwrap(), Some(2));

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
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
                .demand(&native_permit_holder("test-scope", request))
                .unwrap()
                .map(|row| row.state)
        };

        // Acquire grants the demand (late insert: no fence submit here).
        let guard =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "a"), "scope-a")
                .unwrap()
                .unwrap();
        assert_eq!(demand_state("a"), Some(DemandState::Granted));
        // Non-terminal drop makes the demand re-grantable, original age.
        let age = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("test-scope", "a"))
            .unwrap()
            .unwrap()
            .first_seen_unix;
        drop(guard);
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("test-scope", "a"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Eligible);
        assert_eq!(row.first_seen_unix, age);

        // The older retry reacquires first and completes before the next
        // independent demand can proceed.
        let retry =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "a"), "scope-a")
                .unwrap()
                .unwrap();
        retry.release().unwrap();

        // Terminal release serves the demand.
        let guard =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "b"), "scope-b")
                .unwrap()
                .unwrap();
        guard.release().unwrap();
        assert_eq!(demand_state("b"), Some(DemandState::Terminal));

        // Cleanup failure still served the demand; retention is ledger-side.
        let guard =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "c"), "scope-a")
                .unwrap()
                .unwrap();
        guard.mark_uncertain_and_disarm();
        assert_eq!(demand_state("c"), Some(DemandState::Terminal));

        // Duplicate delivery onto a live hold owns nothing and touches
        // neither the permit nor the demand row.
        let owner =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "d"), "scope-a")
                .unwrap()
                .unwrap();
        let before = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("test-scope", "d"))
            .unwrap()
            .unwrap();
        let duplicate =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "d"), "scope-a")
                .unwrap()
                .unwrap();
        drop(duplicate);
        let after = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("test-scope", "d"))
            .unwrap()
            .unwrap();
        assert_eq!(before.first_seen_unix, after.first_seen_unix);
        assert_eq!(before.sequence, after.sequence);
        assert_eq!(before.state, after.state);
        owner.release().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn ingress_subsecond_survives_ledger_open_crossing_second_boundary() {
        let path = temp_ledger_path("ingress-subsecond-rollover");
        configure(&path, 1);
        let now = velnor_control::permit_ledger::unix_now();
        let ingress_second = now.saturating_sub(1);
        let older_holder = crate::scaleset::permit_holder(7, 401);
        let native_holder = native_permit_holder("test-scope", "rollover");
        {
            let mut ledger = PermitLedger::open(&path).unwrap();
            ledger
                .observe_demand_with_subsecond(
                    &older_holder,
                    PermitLane::ScaleSet,
                    "scaleset/7",
                    ingress_second,
                    900_000_000,
                    now,
                )
                .unwrap();
        }

        // This is the timestamp captured when the broker offer arrived.
        // Opening the ledger after it is captured models crossing into the
        // next second before SQLite records the observation.
        let guard = NativePermitGuard::acquire_with_observed_at(
            &path,
            native_holder.clone(),
            "scope",
            (ingress_second, 990_000_000),
        )
        .unwrap();
        assert!(guard.is_none(), "the older Scale Set offer must win");

        let ledger = PermitLedger::open(&path).unwrap();
        let scale_set = ledger.demand(&older_holder).unwrap().unwrap();
        let native = ledger.demand(&native_holder).unwrap().unwrap();
        assert_eq!(scale_set.first_seen_unix, ingress_second);
        assert_eq!(scale_set.first_seen_subsec_nanos, 900_000_000);
        assert_eq!(native.first_seen_unix, ingress_second);
        assert_eq!(native.first_seen_subsec_nanos, 990_000_000);
        assert!(scale_set.sequence < native.sequence);
    }

    #[test]
    fn teardown_and_recovery_releases_serve_the_demand() {
        use velnor_control::permit_ledger::DemandState;
        let path = temp_ledger_path("demand-terminal");
        configure(&path, 4);

        let guard =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "t"), "scope-a")
                .unwrap()
                .unwrap();
        let teardown = guard.teardown_release().unwrap();
        // The teardown thread confirms cleanup after the guard was
        // disarmed by a join timeout: terminal either way.
        std::mem::forget(guard);
        teardown.release_confirmed_cleanup().unwrap();
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("test-scope", "t"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Terminal);

        // Recorded-job recovery completes and cleans, then releases.
        let guard =
            NativePermitGuard::acquire(&path, native_permit_holder("test-scope", "r"), "scope-a")
                .unwrap()
                .unwrap();
        let holder = guard.holder().to_owned();
        let ledger = guard.ledger_path().to_owned();
        let lease_generation = guard.permit_generation();
        std::mem::forget(guard);
        assert!(release_permit_if_generation(&ledger, &holder, lease_generation).unwrap());
        let row = PermitLedger::open(&path)
            .unwrap()
            .demand(&native_permit_holder("test-scope", "r"))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DemandState::Terminal);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
