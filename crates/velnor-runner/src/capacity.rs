use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const DEFAULT_DOCKER_LIFECYCLE_CONCURRENCY: usize = 2;
const MAX_DOCKER_LIFECYCLE_CONCURRENCY: usize = 8;
const DOCKER_LIFECYCLE_RETRY: Duration = Duration::from_millis(25);
/// Waits at or above this line are operator-visible: dockerd's control plane
/// turns an unbounded fan-out into 10–70s tail latency, so a wait past one
/// second means a peer is holding every slot through a slow mutation.
const DOCKER_LIFECYCLE_TELEMETRY_AFTER: Duration = Duration::from_secs(1);
// Versioned roots bind one persisted schema to its reader. Package upgrades
// drain every Velnor service before replacing the binary, so old records need
// no compatibility scan after owner metadata becomes mandatory.
const SCOPE_LEASE_ROOT: &str = "leases__trust_scope_v2";

/// Canonical UUID that owns a job-scoped lease. It is stored outside the
/// hashed scope so liveness can identify a workspace without decoding names.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub(crate) struct JobOwnerId(String);

impl JobOwnerId {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        let parsed = uuid::Uuid::parse_str(value).context("job owner id is not a UUID")?;
        let canonical = parsed.hyphenated().to_string();
        anyhow::ensure!(
            value == canonical,
            "job owner id is not a canonical lowercase UUID"
        );
        Ok(Self(canonical))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> serde::Deserialize<'de> for JobOwnerId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseRecord {
    scope: String,
    pid: u32,
    created_unix: u64,
    owner_job_id: RequiredOwnerJobId,
}

/// A present nullable field: explicit `null` means a generic lease, while an
/// omitted field is malformed evidence and must not be treated as ownerless.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct RequiredOwnerJobId(Option<JobOwnerId>);

impl<'de> Deserialize<'de> for RequiredOwnerJobId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct OwnerJobIdVisitor;

        impl<'de> serde::de::Visitor<'de> for OwnerJobIdVisitor {
            type Value = RequiredOwnerJobId;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("null or a canonical lowercase job UUID")
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(RequiredOwnerJobId(None))
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(RequiredOwnerJobId(None))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                JobOwnerId::parse(value)
                    .map(|owner| RequiredOwnerJobId(Some(owner)))
                    .map_err(E::custom)
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_any(OwnerJobIdVisitor)
    }
}

impl From<Option<JobOwnerId>> for RequiredOwnerJobId {
    fn from(owner_job_id: Option<JobOwnerId>) -> Self {
        Self(owner_job_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveScopeLease {
    pub(crate) scope: String,
    pub(crate) owner_job_id: Option<JobOwnerId>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReservationRecord {
    bytes: u64,
    pid: u32,
    created_unix: u64,
}

/// Maximum age for a job capacity reservation before it is treated as leaked.
///
/// Reservations must only live for the duration of an active job. Multi-slot
/// daemons share one PID across slots, so PID liveness alone cannot reap a
/// leaked file left behind after a job-path panic or incomplete Drop. Age is
/// the host-wide safety net. Override with `VELNOR_RESERVATION_TTL_SECS`.
pub fn reservation_ttl() -> Duration {
    reservation_ttl_from(std::env::var("VELNOR_RESERVATION_TTL_SECS").ok().as_deref())
}

fn reservation_ttl_from(value: Option<&str>) -> Duration {
    let secs = value
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(6 * 3600);
    Duration::from_secs(secs.max(60))
}

/// Retry interval while an acquired job waits for host disk peak.
pub const CAPACITY_WAIT_RETRY_SECS: u64 = 15;

/// Default filesystem bytes never available to new jobs
/// (`--emergency-reserve-bytes`, `VELNOR_EMERGENCY_RESERVE_BYTES`).
pub const DEFAULT_EMERGENCY_RESERVE_BYTES: u64 = 10 * GIB;

/// Default disk peak reserved for every active job
/// (`--job-peak-bytes`, `VELNOR_JOB_PEAK_BYTES`).
pub const DEFAULT_JOB_PEAK_BYTES: u64 = 30 * GIB;

const GIB: u64 = 1024 * 1024 * 1024;

/// The compiler stores may hold this fraction of the persistent allowance:
/// one half. They are the largest warm state a Rust host keeps (mbx's
/// content-addressed cache and managed targets, sccache's object cache);
/// the other half is for every other store class, Docker's own storage and
/// the operating system.
const COMPILER_STORE_SHARE_DIVISOR: u64 = 2;

/// The host-level store budget, derived from the same capacity policy that
/// admits jobs.
///
/// The admission ledger promises every active job `job_peak_bytes` above
/// `emergency_reserve_bytes` of free space. Persistent stores may therefore
/// hold at most what is left of the filesystem once every slot could run a
/// job at peak simultaneously — the *persistent allowance*. The compiler
/// class, which had no host-level bound at all (mbx's `MBX_GC_MAX_TOTAL_SIZE`
/// is per slot, and slots × repositories × daemon instances multiply it
/// without limit), is capped at a fixed share of that allowance.
///
/// One formula, one number, printed by `velnor-runner preflight` and by the
/// daemon at startup, enforced by the daemon at every admission and by
/// `cache gc`:
///
/// ```text
/// persistent_allowance = total − emergency_reserve − slots × job_peak
/// compiler_store_budget = persistent_allowance / 2
/// ```
///
/// Co-located daemon instances derive their own budget from their own slot
/// count; the smallest wins, because each enforces at its own admissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreBudgetPolicy {
    /// Capacity of the filesystem holding the stores (`statvfs` total).
    pub total_bytes: u64,
    pub emergency_reserve_bytes: u64,
    pub job_peak_bytes: u64,
    /// Slots this daemon advertises: the number of jobs that may hold a peak
    /// reservation at once.
    pub slots: u32,
}

impl StoreBudgetPolicy {
    /// Probe the filesystem holding `work_root` and bind the daemon's
    /// admission parameters to it.
    pub fn probe(
        work_root: &Path,
        emergency_reserve_bytes: u64,
        job_peak_bytes: u64,
        slots: u32,
    ) -> Result<Self> {
        let capacity = crate::host_capacity::HostCapacity::probe(work_root)?;
        Ok(Self {
            total_bytes: capacity.total_bytes,
            emergency_reserve_bytes,
            job_peak_bytes,
            slots,
        })
    }

    /// The policy as a process that has no daemon flags sees it: the
    /// packaged units export the same values as environment
    /// (`VELNOR_EMERGENCY_RESERVE_BYTES`, `VELNOR_JOB_PEAK_BYTES`,
    /// `VELNOR_SLOTS`), and the defaults are the daemon's flag defaults, so
    /// `preflight` and `cache gc` derive the daemon's number.
    pub fn probe_from_env(work_root: &Path) -> Result<Self> {
        Self::probe_from_lookup(work_root, |name| std::env::var(name).ok())
    }

    /// The policy of a packaged daemon instance: the same variables as
    /// [`Self::probe_from_env`], read from the instance's replayed unit
    /// environment (`daemon_instance`) instead of this process's. An operator
    /// pass over another daemon's stores must derive that daemon's number, not
    /// the number a process with no daemon flags would derive.
    pub fn probe_from_environment(
        work_root: &Path,
        environment: &std::collections::BTreeMap<String, String>,
    ) -> Result<Self> {
        Self::probe_from_lookup(work_root, |name| environment.get(name).cloned())
    }

    fn probe_from_lookup(
        work_root: &Path,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let var = |name: &str| lookup(name)?.trim().parse::<u64>().ok();
        Self::probe(
            work_root,
            var("VELNOR_EMERGENCY_RESERVE_BYTES").unwrap_or(DEFAULT_EMERGENCY_RESERVE_BYTES),
            var("VELNOR_JOB_PEAK_BYTES").unwrap_or(DEFAULT_JOB_PEAK_BYTES),
            var("VELNOR_SLOTS")
                .and_then(|slots| u32::try_from(slots).ok())
                .filter(|slots| *slots > 0)
                .unwrap_or(1),
        )
    }

    /// Bytes persistent stores may hold while every slot can still admit a
    /// job at peak: `total − emergency_reserve − slots × job_peak`,
    /// saturating at zero.
    pub fn persistent_allowance_bytes(&self) -> u64 {
        self.total_bytes
            .saturating_sub(self.emergency_reserve_bytes)
            .saturating_sub(self.job_peak_bytes.saturating_mul(u64::from(self.slots)))
    }

    /// The compiler-class (mbx + sccache) budget across every trust scope,
    /// repository and slot on this host.
    pub fn compiler_store_budget_bytes(&self) -> u64 {
        self.persistent_allowance_bytes() / COMPILER_STORE_SHARE_DIVISOR
    }

    /// Bytes the `pr`-scope Cargo store seed may add to a store that already
    /// holds `pr_cargo_store_bytes` (D18, [`crate::storage::seed_cargo_store`]).
    ///
    /// The seeded `pr` Cargo store counts against the same number as the
    /// compiler stores. Each seed pass applies the remaining allowance as a
    /// best-effort cap when its size measurements succeed, saturating at zero.
    /// One bound for every store class the daemon itself grows, derived from
    /// the admission policy that already promises each slot its peak.
    pub fn cargo_seed_headroom_bytes(&self, pr_cargo_store_bytes: u64) -> u64 {
        self.compiler_store_budget_bytes()
            .saturating_sub(pr_cargo_store_bytes)
    }

    /// One line an operator can read: the number and how it was derived.
    pub fn describe_compiler_store_budget(&self) -> String {
        format!(
            "{} = ({} total - {} emergency reserve - {} slot(s) x {} job peak) / {}",
            gib(self.compiler_store_budget_bytes()),
            gib(self.total_bytes),
            gib(self.emergency_reserve_bytes),
            self.slots,
            gib(self.job_peak_bytes),
            COMPILER_STORE_SHARE_DIVISOR,
        )
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// Default bound on the post-acquire disk-peak wait. Override with
/// `VELNOR_CAPACITY_WAIT_SECS`. Floor is one retry interval so a single
/// reclaim pass can finish. This is not an unbounded hang: once the bound
/// elapses the runner must complete the GitHub job Failed.
pub const DEFAULT_CAPACITY_WAIT_SECS: u64 = 120;

/// How long an already-acquired job may retry disk-peak reservation before
/// the runner fail-closes the GitHub job.
pub fn capacity_wait_timeout() -> Duration {
    let secs = std::env::var("VELNOR_CAPACITY_WAIT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CAPACITY_WAIT_SECS);
    Duration::from_secs(secs.max(CAPACITY_WAIT_RETRY_SECS))
}

/// Decision for the post-acquire, pre-step disk-peak wait.
///
/// There is no Success arm. The runner either retries while time remains or
/// times out and must complete GitHub **Failed** with a visible step/reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityWaitDecision {
    Retry { sleep: Duration },
    Timeout,
}

/// Bound the wait that used to hold run-service lock renewal with zero steps.
pub fn pre_execution_capacity_wait_decision(
    elapsed: Duration,
    timeout: Duration,
) -> CapacityWaitDecision {
    if elapsed >= timeout {
        CapacityWaitDecision::Timeout
    } else {
        let remaining = timeout.saturating_sub(elapsed);
        CapacityWaitDecision::Retry {
            sleep: remaining.min(Duration::from_secs(CAPACITY_WAIT_RETRY_SECS)),
        }
    }
}

/// Combined pre-execution wait: the acquire loop calls this every iteration.
/// Tests that only build completion payloads do not prove the loop reads the
/// flags; this function is the loop's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreExecutionWaitDecision {
    Reserved,
    RetryReserve { sleep: Duration },
    AbortRegistrationLost,
    AbortCanceled,
    AbortCapacityTimeout,
}

pub fn pre_execution_wait_decision(
    registration_lost: bool,
    canceled: bool,
    reserve_ok: bool,
    capacity: CapacityWaitDecision,
) -> PreExecutionWaitDecision {
    if registration_lost {
        return PreExecutionWaitDecision::AbortRegistrationLost;
    }
    if canceled {
        return PreExecutionWaitDecision::AbortCanceled;
    }
    if reserve_ok {
        return PreExecutionWaitDecision::Reserved;
    }
    match capacity {
        CapacityWaitDecision::Retry { sleep } => PreExecutionWaitDecision::RetryReserve { sleep },
        CapacityWaitDecision::Timeout => PreExecutionWaitDecision::AbortCapacityTimeout,
    }
}

/// Default bound on GitHub `queued` (unassigned) wait. Override with
/// `VELNOR_QUEUE_WAIT_SECS`. Floor is 15s. This is not job `timeout-minutes`
/// (that starts after assignment).
pub const DEFAULT_QUEUE_WAIT_SECS: u64 = 300;

pub fn queue_wait_timeout() -> Duration {
    let secs = std::env::var("VELNOR_QUEUE_WAIT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_QUEUE_WAIT_SECS);
    Duration::from_secs(secs.max(CAPACITY_WAIT_RETRY_SECS))
}

/// A GitHub Actions job that is still `queued` (no runner assigned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedUnassignedJob {
    pub run_id: u64,
    pub job_id: String,
    pub repository: String,
    pub queued_for: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuedUnassignedDecision {
    Wait,
    FailClosed,
}

/// Fail-closed bound for unassigned jobs. There is no Success arm.
pub fn queued_unassigned_decision(
    queued_for: Duration,
    timeout: Duration,
) -> QueuedUnassignedDecision {
    if queued_for >= timeout {
        QueuedUnassignedDecision::FailClosed
    } else {
        QueuedUnassignedDecision::Wait
    }
}

/// Queue timeout applies only while GitHub has not assigned a runner.
/// After assignment (`handle_job_request`) the job must execute (AC3).
pub fn queue_wait_decision(
    assigned: bool,
    queued_for: Duration,
    timeout: Duration,
) -> QueuedUnassignedDecision {
    if assigned {
        QueuedUnassignedDecision::Wait
    } else {
        queued_unassigned_decision(queued_for, timeout)
    }
}

/// Unassigned jobs waiting on `velnor-trusted` (not GitHub-hosted labels).
pub fn job_waits_on_trusted_fleet(labels: &[String]) -> bool {
    labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case("velnor-trusted"))
}

pub fn queue_timeout_reason(queued_for: Duration, timeout: Duration) -> String {
    format!(
        "timed out after {}s waiting for a healthy Velnor runner (queue limit {}s); job was never assigned to a ready slot",
        queued_for.as_secs(),
        timeout.as_secs()
    )
}

/// Jobs that have been `queued` past the bound and must fail-closed.
pub fn queued_unassigned_jobs_past_deadline(
    jobs: &[QueuedUnassignedJob],
    timeout: Duration,
) -> Vec<&QueuedUnassignedJob> {
    jobs.iter()
        .filter(|job| {
            queue_wait_decision(false, job.queued_for, timeout)
                == QueuedUnassignedDecision::FailClosed
        })
        .collect()
}

/// GitHub DELETE 422 / registry `offline+busy` with no live online session:
/// complete the leftover job so the lease can drop. `online+busy` is a live job.
pub fn stale_busy_lease_should_complete_job(status: Option<&str>, busy: Option<bool>) -> bool {
    busy == Some(true) && status != Some("online")
}

/// Operator-visible reason for a host-capacity timeout completion.
///
/// Empty `last_error` still yields a non-empty reason so GitHub cannot hide
/// the failure behind a zero-step job.
pub fn host_capacity_timeout_reason(
    elapsed: Duration,
    timeout: Duration,
    last_error: &str,
) -> String {
    let detail = last_error.trim();
    if detail.is_empty() {
        format!(
            "timed out after {}s waiting for host disk capacity (limit {}s)",
            elapsed.as_secs(),
            timeout.as_secs()
        )
    } else {
        format!(
            "timed out after {}s waiting for host disk capacity (limit {}s): {detail}",
            elapsed.as_secs(),
            timeout.as_secs()
        )
    }
}

#[derive(Debug)]
pub struct ScopeLease {
    path: PathBuf,
    // Keep the kernel lock for the whole job lifetime. A live holder remains
    // active regardless of record age, and kernel ownership avoids PID reuse
    // ambiguity.
    _file: fs::File,
}

/// Stable, per-lease serialization lock. This sibling is deliberately kept
/// after lease removal: deleting and recreating it would let contenders flock
/// different inodes and reopen the same pathname race.
#[derive(Debug)]
struct ScopeLeaseNameLock {
    _file: fs::File,
}

fn scope_lease_name_lock_path(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().context("scope lease path has no parent")?;
    let filename = path
        .file_name()
        .context("scope lease path has no file name")?
        .to_string_lossy();
    Ok(parent.join(format!(".{filename}.lock")))
}

fn lock_scope_lease_name(path: &Path) -> Result<ScopeLeaseNameLock> {
    let lock_path = scope_lease_name_lock_path(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open scope lease name lock {}", lock_path.display()))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .with_context(|| format!("lock scope lease name {}", path.display()))?;
    Ok(ScopeLeaseNameLock { _file: file })
}

/// Remove a name only when it still refers to the inode owned by `file`.
/// Callers serialize cooperative pathname changes with the persistent
/// per-name lock; this identity check also makes cleanup fail closed if a
/// pathname was replaced outside that protocol.
fn remove_path_if_same_inode(path: &Path, file: &fs::File) -> Result<bool> {
    let opened = file.metadata()?;
    let named = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("stat lease {}", path.display())),
    };
    if opened.dev() != named.dev() || opened.ino() != named.ino() {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove lease {}", path.display())),
    }
}

/// Serializes lease publication against destructive cache snapshots.
///
/// Lease files remain the fine-grained liveness authority. The coordinator
/// closes the cross-daemon race where a reaper snapshots those files while a
/// different daemon is publishing a lease for the same store.
#[derive(Debug)]
pub struct FilesystemCoordinator {
    _file: fs::File,
    path: PathBuf,
    exclusive: bool,
}

/// Coordinator holds registered by the current thread, per lock file.
#[derive(Default)]
struct ThreadHolds {
    exclusive: usize,
    shared: usize,
}

thread_local! {
    /// Coordinators this thread currently holds.
    ///
    /// `flock` locks belong to the open file description, so a second `open`
    /// of the same lock file from the thread that already holds a conflicting
    /// lock blocks until that thread releases — which it never can while
    /// blocked. That is a deadlock with no error and no log line (it is how
    /// `velnorctl cache gc` hung on itself: `run_gc` held the coordinator and
    /// the leftover reclaim it called locked it again). Cross-thread and
    /// cross-process contention is legitimate blocking and stays untouched;
    /// only same-thread conflicting re-entry is refused, with an error naming
    /// the fix. Holds are never carried across `.await`, so a thread-local is
    /// the right scope.
    static HELD: std::cell::RefCell<std::collections::BTreeMap<PathBuf, ThreadHolds>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

impl FilesystemCoordinator {
    pub fn lock_shared(run_root: &Path) -> Result<Self> {
        Self::lock(run_root, false)
    }

    pub fn lock_exclusive(run_root: &Path) -> Result<Self> {
        Self::lock(run_root, true)
    }

    fn lock(run_root: &Path, exclusive: bool) -> Result<Self> {
        fs::create_dir_all(run_root)?;
        let path = run_root.join("filesystem-coordinator.lock");
        let conflict = HELD.with_borrow(|held| {
            held.get(&path).and_then(|holds| {
                if holds.exclusive > 0 {
                    Some("exclusively")
                } else if exclusive && holds.shared > 0 {
                    Some("shared")
                } else {
                    None
                }
            })
        });
        if let Some(mode) = conflict {
            bail!(
                "filesystem coordinator {} is already held {mode} by this thread; \
                 a re-entrant {} flock would never return — pass the held coordinator down \
                 instead of locking again",
                path.display(),
                if exclusive { "exclusive" } else { "shared" },
            );
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let operation = if exclusive {
            rustix::fs::FlockOperation::LockExclusive
        } else {
            rustix::fs::FlockOperation::LockShared
        };
        rustix::fs::flock(&file, operation).context("lock filesystem coordinator")?;
        HELD.with_borrow_mut(|held| {
            let holds = held.entry(path.clone()).or_default();
            if exclusive {
                holds.exclusive += 1;
            } else {
                holds.shared += 1;
            }
        });
        Ok(Self {
            _file: file,
            path,
            exclusive,
        })
    }
}

impl Drop for FilesystemCoordinator {
    fn drop(&mut self) {
        HELD.with_borrow_mut(|held| {
            let Some(holds) = held.get_mut(&self.path) else {
                return;
            };
            if self.exclusive {
                holds.exclusive = holds.exclusive.saturating_sub(1);
            } else {
                holds.shared = holds.shared.saturating_sub(1);
            }
            if holds.exclusive == 0 && holds.shared == 0 {
                held.remove(&self.path);
            }
        });
    }
}

/// Default bound on the Docker lifecycle wait. Override with
/// `VELNOR_DOCKER_LIFECYCLE_WAIT_SECS`. Floor is 1s. Past the bound the
/// waiter proceeds WITHOUT a slot (degraded) rather than parking a
/// teardown behind a stuck peer forever — and says so on stderr.
pub const DEFAULT_DOCKER_LIFECYCLE_WAIT_SECS: u64 = 120;

pub fn docker_lifecycle_wait_timeout() -> Duration {
    let secs = std::env::var("VELNOR_DOCKER_LIFECYCLE_WAIT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DOCKER_LIFECYCLE_WAIT_SECS);
    Duration::from_secs(secs.max(1))
}

/// Bounds Docker control-plane lifecycle mutations across daemon processes on
/// one host. Job containers remain concurrent; create, start, and teardown
/// bursts use two host-wide permits by default because dockerd's control
/// plane turns an unbounded fan-out into 10–70s tail latency. Operators can
/// tune the bound with `VELNOR_DOCKER_LIFECYCLE_CONCURRENCY` (1–8).
///
/// The wait for a permit is bounded by [`docker_lifecycle_wait_timeout`]:
/// past the bound the guard degrades (no slot held) instead of parking the
/// caller invisibly, and every slow or degraded acquire names its stage on
/// stderr so a contended host is diagnosable from the daemon log.
pub struct DockerLifecycleGuard {
    _file: Option<fs::File>,
    // Test-only observability for the bound; production telemetry is the
    // `forensics.docker_lifecycle` stderr line emitted on slow/degraded
    // acquires.
    _waited: Duration,
    _degraded: bool,
}

impl DockerLifecycleGuard {
    pub fn lock_for_stage(run_root: &Path, stage: &str) -> Result<Self> {
        let concurrency = std::env::var("VELNOR_DOCKER_LIFECYCLE_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=MAX_DOCKER_LIFECYCLE_CONCURRENCY).contains(value))
            .unwrap_or(DEFAULT_DOCKER_LIFECYCLE_CONCURRENCY);
        Self::lock_with_concurrency(
            run_root,
            concurrency,
            stage,
            docker_lifecycle_wait_timeout(),
        )
    }

    /// True when the acquire timed out and the caller proceeds without a
    /// host-wide slot. Teardown still runs: a stuck peer must delay Docker
    /// mutations, never wedge a slot's turnover.
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
    pub fn degraded(&self) -> bool {
        self._degraded
    }

    /// How long the acquire waited before a slot freed or the bound elapsed.
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
    pub fn waited(&self) -> Duration {
        self._waited
    }

    fn lock_with_concurrency(
        run_root: &Path,
        concurrency: usize,
        stage: &str,
        timeout: Duration,
    ) -> Result<Self> {
        if concurrency == 0 {
            bail!("Docker lifecycle concurrency must be at least 1");
        }
        fs::create_dir_all(run_root)?;
        let start = std::time::Instant::now();
        loop {
            for slot in 0..concurrency {
                // Keep slot zero at the original path so an in-place upgrade
                // never makes a live old daemon invisible to the new guard.
                let path = if slot == 0 {
                    run_root.join("docker-lifecycle.lock")
                } else {
                    run_root.join(format!("docker-lifecycle-{slot}.lock"))
                };
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(path)?;
                match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                {
                    Ok(()) => {
                        let waited = start.elapsed();
                        if waited >= DOCKER_LIFECYCLE_TELEMETRY_AFTER {
                            eprintln!(
                                "forensics.docker_lifecycle event=acquired stage={stage} waited_ms={} slots={concurrency}",
                                waited.as_millis(),
                            );
                        }
                        return Ok(Self {
                            _file: Some(file),
                            _waited: waited,
                            _degraded: false,
                        });
                    }
                    Err(rustix::io::Errno::WOULDBLOCK) => {}
                    Err(error) => return Err(error).context("lock Docker lifecycle coordinator"),
                }
            }
            let waited = start.elapsed();
            if waited >= timeout {
                eprintln!(
                    "forensics.docker_lifecycle event=degraded stage={stage} waited_ms={} slots={concurrency} timeout_ms={} reason=all slots contended past the bound; proceeding without a permit",
                    waited.as_millis(),
                    timeout.as_millis(),
                );
                return Ok(Self {
                    _file: None,
                    _waited: waited,
                    _degraded: true,
                });
            }
            std::thread::sleep(DOCKER_LIFECYCLE_RETRY);
        }
    }
}

impl ScopeLease {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn acquire(
        run_root: &Path,
        class: &str,
        scope: &str,
        stale_after: Duration,
    ) -> Result<Self> {
        Self::acquire_with_job_owner(run_root, class, scope, None, stale_after)
    }

    /// Acquire a lease owned by one GitHub job. The raw job id is persisted
    /// separately from the privacy-preserving hashed scope component so
    /// workspace reclamation can identify live checkout directories.
    pub(crate) fn acquire_for_job(
        run_root: &Path,
        class: &str,
        scope: &str,
        owner_job_id: JobOwnerId,
        stale_after: Duration,
    ) -> Result<Self> {
        Self::acquire_with_job_owner(run_root, class, scope, Some(owner_job_id), stale_after)
    }

    fn acquire_with_job_owner(
        run_root: &Path,
        class: &str,
        scope: &str,
        owner_job_id: Option<JobOwnerId>,
        stale_after: Duration,
    ) -> Result<Self> {
        let _coordinator = FilesystemCoordinator::lock_shared(run_root)?;
        let dir = scope_lease_root(run_root).join(crate::container::sanitize_store_key(class));
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "{}.json",
            crate::trust_scope::filesystem_key(scope)
        ));
        // Serialize the entire open/check/reap/publish sequence. A flock on
        // the lease inode alone cannot do this: a waiter may have opened the
        // old inode before it is unlinked, then acquire that stale descriptor
        // after a new lease has been published at the same pathname.
        let _name_lock = lock_scope_lease_name(&path)?;
        reap_stale_lease_locked(&path, stale_after, &_name_lock)?;
        let record = LeaseRecord {
            scope: scope.to_string(),
            pid: std::process::id(),
            created_unix: unix_now(),
            owner_job_id: owner_job_id.into(),
        };
        let (temporary_path, file) = prepare_scope_lease(&path, &record)?;
        publish_scope_lease(&temporary_path, &path, file)
            .with_context(|| format!("publish scope lease: {class}/{scope}"))
            .map(|file| Self { path, _file: file })
    }
}

fn prepare_scope_lease(path: &Path, record: &LeaseRecord) -> Result<(PathBuf, fs::File)> {
    let parent = path.parent().context("scope lease path has no parent")?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("scope lease path has no file name")?;
    let temporary_path = parent.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&temporary_path)
        .context("create temporary scope lease")?;
    if let Err(error) = rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error).context("lock temporary scope lease");
    }
    let write_result = (|| -> Result<()> {
        serde_json::to_writer(&mut file, record).context("write scope lease record")?;
        file.write_all(b"\n")
            .context("terminate scope lease record")?;
        file.flush().context("flush scope lease record")?;
        file.sync_all().context("sync scope lease record")?;
        Ok(())
    })();
    if let Err(error) = write_result {
        drop(file);
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok((temporary_path, file))
}

fn publish_scope_lease(temporary_path: &Path, path: &Path, file: fs::File) -> Result<fs::File> {
    match fs::hard_link(temporary_path, path) {
        Ok(()) => {}
        Err(error) => {
            let _ = fs::remove_file(temporary_path);
            return Err(error).with_context(|| format!("publish lease at {}", path.display()));
        }
    }
    if let Err(error) = fs::remove_file(temporary_path) {
        let _ = remove_path_if_same_inode(path, &file);
        return Err(error).with_context(|| {
            format!(
                "remove temporary link for published lease {}",
                path.display()
            )
        });
    }
    Ok(file)
}

impl Drop for ScopeLease {
    fn drop(&mut self) {
        // Keep the lock through the inode check and unlink. If a replacement
        // lease ever owns this pathname, an old holder must not remove it.
        let Ok(_name_lock) = lock_scope_lease_name(&self.path) else {
            return;
        };
        let _ = remove_path_if_same_inode(&self.path, &self._file);
    }
}

/// Reap an expired lease only after taking its exclusive kernel lock. A live
/// ScopeLease retains that lock even if its record is more than a day old.
/// The persistent name lock must be held by the caller across open/check/unlink
/// so the lease path cannot be replaced between those operations.
fn reap_stale_lease_locked(
    path: &Path,
    stale_after: Duration,
    _name_lock: &ScopeLeaseNameLock,
) -> Result<bool> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("open lease {}", path.display())),
    };
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(rustix::io::Errno::WOULDBLOCK) => return Ok(false),
        Err(error) => return Err(error).context("lock scope lease for stale check"),
    }
    reap_stale_lease_file_locked(path, stale_after, &mut file, _name_lock)
}

fn reap_stale_lease_file_locked(
    path: &Path,
    stale_after: Duration,
    file: &mut fs::File,
    _name_lock: &ScopeLeaseNameLock,
) -> Result<bool> {
    file.seek(SeekFrom::Start(0))?;
    let record: LeaseRecord = serde_json::from_reader(&mut *file)?;
    let age_stale = unix_now().saturating_sub(record.created_unix) > stale_after.as_secs();
    let proc_root = Path::new("/proc");
    let pid_gone = proc_root.exists() && !proc_root.join(record.pid.to_string()).exists();
    if age_stale || pid_gone {
        remove_path_if_same_inode(path, file)
            .with_context(|| format!("remove stale lease {}", path.display()))
    } else {
        Ok(false)
    }
}

pub(crate) fn active_scope_leases(
    run_root: &Path,
    stale_after: Duration,
) -> Result<Vec<ActiveScopeLease>> {
    let root = scope_lease_root(run_root);
    let mut active = Vec::new();
    if !root.exists() {
        return Ok(active);
    }
    for class_entry in fs::read_dir(&root)? {
        let class = class_entry?;
        let class_path = class.path();
        if !class_path.is_dir() {
            continue;
        }
        let class = class.file_name().to_string_lossy().to_string();
        for entry in fs::read_dir(class_path)? {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let _name_lock = lock_scope_lease_name(&path)?;
            if reap_stale_lease_locked(&path, stale_after, &_name_lock)? {
                continue;
            }
            let record: LeaseRecord = match fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error).with_context(|| format!("read lease {}", path.display()))
                }
            };
            active.push(ActiveScopeLease {
                scope: format!("{class}/{}", record.scope),
                owner_job_id: record.owner_job_id.0,
            });
        }
    }
    Ok(active)
}

pub fn active_scopes(run_root: &Path, stale_after: Duration) -> Result<BTreeSet<String>> {
    Ok(active_scope_leases(run_root, stale_after)?
        .into_iter()
        .map(|lease| lease.scope)
        .collect())
}

fn scope_lease_root(run_root: &Path) -> PathBuf {
    run_root.join(SCOPE_LEASE_ROOT)
}

#[derive(Debug)]
pub struct Reservation {
    path: PathBuf,
    pub bytes: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Clone)]
pub struct CapacityController {
    pub run_root: PathBuf,
    pub emergency_reserve_bytes: u64,
    pub job_peak_bytes: u64,
}

impl CapacityController {
    pub fn reserve_with_free_bytes(&self, free_bytes: u64) -> Result<Reservation> {
        let dir = self.run_root.join("reservations");
        fs::create_dir_all(&dir)?;
        let lock_path = self.run_root.join("capacity.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
            .with_context(|| "serialize filesystem reservation update")?;
        let active = reservation_bytes(&dir)?;
        let backpressure = self.run_root.join("capacity-backpressure");
        let hysteresis = if backpressure.exists() {
            self.job_peak_bytes / 5
        } else {
            0
        };
        let required = self
            .emergency_reserve_bytes
            .saturating_add(active)
            .saturating_add(self.job_peak_bytes)
            .saturating_add(hysteresis);
        if free_bytes < required {
            fs::write(&backpressure, format!("{}\n", unix_now()))?;
            bail!(
                "capacity backpressure: free={free_bytes} required={required} emergency={} active={} job_peak={} hysteresis={hysteresis}",
                self.emergency_reserve_bytes,
                active,
                self.job_peak_bytes
            );
        }
        if backpressure.exists() {
            fs::remove_file(backpressure)?;
        }
        let id = uuid::Uuid::new_v4();
        let path = dir.join(format!("{id}.json"));
        let pending_path = dir.join(format!("{id}.tmp"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending_path)?;
        serde_json::to_writer(
            &mut file,
            &ReservationRecord {
                bytes: self.job_peak_bytes,
                pid: std::process::id(),
                created_unix: unix_now(),
            },
        )?;
        writeln!(file)?;
        file.sync_all()?;
        fs::rename(&pending_path, &path)?;
        Ok(Reservation {
            path,
            bytes: self.job_peak_bytes,
        })
    }
}

fn reservation_is_stale(record: &ReservationRecord, ttl: Duration) -> bool {
    let age_stale = unix_now().saturating_sub(record.created_unix) > ttl.as_secs();
    let proc_root = Path::new("/proc");
    let pid_gone = proc_root.exists() && !proc_root.join(record.pid.to_string()).exists();
    age_stale || pid_gone
}

fn reservation_bytes(dir: &Path) -> Result<u64> {
    reservation_bytes_with_ttl(dir, reservation_ttl())
}

fn reservation_bytes_with_ttl(dir: &Path, ttl: Duration) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let record: ReservationRecord = serde_json::from_slice(&fs::read(&path)?)?;
        if reservation_is_stale(&record, ttl) {
            fs::remove_file(path)?;
            continue;
        }
        total = total.saturating_add(record.bytes);
    }
    Ok(total)
}

pub fn reservation_summary(run_root: &Path) -> Result<(usize, u64)> {
    reservation_summary_with_ttl(run_root, reservation_ttl())
}

fn reservation_summary_with_ttl(run_root: &Path, ttl: Duration) -> Result<(usize, u64)> {
    let dir = run_root.join("reservations");
    if !dir.exists() {
        return Ok((0, 0));
    }
    let count = fs::read_dir(&dir)?.count();
    Ok((count, reservation_bytes_with_ttl(&dir, ttl)?))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

    /// The budget is one formula over the admission policy: what is left of
    /// the disk once every slot could run a job at peak above the emergency
    /// reserve, halved. Sentry's `velnor` instance (919 GiB, 10 GiB reserve,
    /// 4 slots × 16 GiB) derives 422.5 GiB — a bound the 513 GiB the host
    /// actually held would have tripped.
    #[test]
    fn compiler_store_budget_is_half_the_persistent_allowance() {
        let policy = StoreBudgetPolicy {
            total_bytes: 919 * GIB,
            emergency_reserve_bytes: 10 * GIB,
            job_peak_bytes: 16 * GIB,
            slots: 4,
        };
        assert_eq!(policy.persistent_allowance_bytes(), 845 * GIB);
        assert_eq!(policy.compiler_store_budget_bytes(), 845 * GIB / 2);
        assert!(policy.compiler_store_budget_bytes() < 513 * GIB);
        let line = policy.describe_compiler_store_budget();
        assert!(line.starts_with("422.5 GiB = ("), "{line}");
        assert!(line.contains("4 slot(s) x 16.0 GiB job peak"), "{line}");
    }

    /// More slots reserve more transient space and leave less for stores;
    /// a daemon whose slots alone exhaust the disk gets no compiler budget
    /// rather than a negative or wrapped one.
    #[test]
    fn compiler_store_budget_shrinks_with_slots_and_saturates_at_zero() {
        let base = StoreBudgetPolicy {
            total_bytes: 200 * GIB,
            emergency_reserve_bytes: 10 * GIB,
            job_peak_bytes: 30 * GIB,
            slots: 1,
        };
        let two = StoreBudgetPolicy { slots: 2, ..base };
        assert!(two.compiler_store_budget_bytes() < base.compiler_store_budget_bytes());
        assert_eq!(base.compiler_store_budget_bytes(), 80 * GIB);
        assert_eq!(two.compiler_store_budget_bytes(), 65 * GIB);
        let exhausted = StoreBudgetPolicy { slots: 10, ..base };
        assert_eq!(exhausted.persistent_allowance_bytes(), 0);
        assert_eq!(exhausted.compiler_store_budget_bytes(), 0);
    }

    /// The `pr` Cargo store seed fills up to the compiler-store number and
    /// counts what the store already holds against it.
    #[test]
    fn cargo_seed_headroom_is_the_compiler_budget_less_the_store() {
        let policy = StoreBudgetPolicy {
            total_bytes: 200 * GIB,
            emergency_reserve_bytes: 10 * GIB,
            job_peak_bytes: 30 * GIB,
            slots: 1,
        };
        assert_eq!(policy.cargo_seed_headroom_bytes(0), 80 * GIB);
        assert_eq!(policy.cargo_seed_headroom_bytes(30 * GIB), 50 * GIB);
        assert_eq!(policy.cargo_seed_headroom_bytes(80 * GIB), 0);
        assert_eq!(policy.cargo_seed_headroom_bytes(u64::MAX), 0);
    }

    /// `probe` binds the daemon's flags to the real filesystem; the number it
    /// derives is the one the daemon enforces and preflight prints.
    #[test]
    fn probe_derives_the_budget_from_the_filesystem_holding_the_work_root() {
        let policy = StoreBudgetPolicy::probe(
            &std::env::temp_dir(),
            DEFAULT_EMERGENCY_RESERVE_BYTES,
            DEFAULT_JOB_PEAK_BYTES,
            2,
        )
        .unwrap();
        assert!(policy.total_bytes > 0);
        assert_eq!(policy.slots, 2);
        assert_eq!(
            policy.compiler_store_budget_bytes(),
            policy
                .total_bytes
                .saturating_sub(DEFAULT_EMERGENCY_RESERVE_BYTES)
                .saturating_sub(2 * DEFAULT_JOB_PEAK_BYTES)
                / 2
        );
    }

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("velnor-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn job_owner_id_requires_canonical_lowercase_uuid() {
        let canonical = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        assert_eq!(JobOwnerId::parse(canonical).unwrap().as_str(), canonical);
        assert!(JobOwnerId::parse("AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE").is_err());
        assert!(JobOwnerId::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeee").is_err());
    }

    #[test]
    fn lease_owner_field_requires_presence_but_accepts_explicit_null() {
        #[derive(Deserialize)]
        struct Record {
            owner_job_id: RequiredOwnerJobId,
        }

        assert!(serde_json::from_str::<Record>("{}").is_err());
        assert!(serde_json::from_str::<Record>(r#"{"owner_job_id":null}"#)
            .unwrap()
            .owner_job_id
            .0
            .is_none());
        assert_eq!(
            serde_json::from_str::<Record>(
                r#"{"owner_job_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"}"#
            )
            .unwrap()
            .owner_job_id
            .0
            .unwrap()
            .as_str(),
            "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
        );
    }

    #[test]
    fn lease_excludes_second_acquirer_and_drop_releases() {
        let root = root("lease");
        let first =
            ScopeLease::acquire(&root, "targets", "trusted/repo", Duration::from_secs(60)).unwrap();
        assert!(
            ScopeLease::acquire(&root, "targets", "trusted/repo", Duration::from_secs(60)).is_err()
        );
        drop(first);
        assert!(
            ScopeLease::acquire(&root, "targets", "trusted/repo", Duration::from_secs(60)).is_ok()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_acquirers_leave_one_visible_lease_after_reaping_orphan() {
        const CONTENDERS: usize = 12;
        let root = root("lease-stale-contended");
        let scope = "trusted/racing";
        let path = scope_lease_root(&root).join("cargo").join(format!(
            "{}.json",
            crate::trust_scope::filesystem_key(scope)
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&LeaseRecord {
                scope: scope.to_string(),
                pid: std::process::id(),
                created_unix: unix_now().saturating_sub(2 * 24 * 60 * 60),
                owner_job_id: RequiredOwnerJobId(None),
            })
            .unwrap(),
        )
        .unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(CONTENDERS + 1));
        let handles: Vec<_> = (0..CONTENDERS)
            .map(|_| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    ScopeLease::acquire(&root, "cargo", scope, Duration::from_secs(24 * 3600))
                })
            })
            .collect();
        barrier.wait();
        let mut leases: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter_map(|result| result.ok())
            .collect();

        assert_eq!(leases.len(), 1, "exactly one contender owns the scope");
        assert_eq!(
            active_scopes(&root, Duration::from_secs(24 * 3600)).unwrap(),
            BTreeSet::from([format!("cargo/{scope}")]),
            "the winning lease must remain visible after every contender returns"
        );
        drop(leases.pop());
        assert!(active_scopes(&root, Duration::from_secs(24 * 3600))
            .unwrap()
            .is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scope_lease_name_lock_is_shared_across_processes() {
        const CHILD_MODE: &str = "VELNOR_TEST_SCOPE_LEASE_NAME_LOCK_CHILD";
        const LEASE_PATH: &str = "VELNOR_TEST_SCOPE_LEASE_NAME_LOCK_PATH";
        if std::env::var_os(CHILD_MODE).is_some() {
            let path = PathBuf::from(std::env::var_os(LEASE_PATH).unwrap());
            let lock_path = scope_lease_name_lock_path(&path).unwrap();
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_path)
                .unwrap();
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Err(rustix::io::Errno::WOULDBLOCK) => return,
                Ok(()) => panic!("parent process holds the scope lease name lock"),
                Err(error) => panic!("probe cross-process scope lease lock: {error}"),
            }
        }

        let root = root("lease-name-lock-process");
        let path = scope_lease_root(&root).join("cargo").join("scope.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _parent_lock = lock_scope_lease_name(&path).unwrap();
        let test_binary = std::env::current_exe().unwrap();
        let test_name = std::thread::current().name().unwrap().to_owned();
        let status = std::process::Command::new(test_binary)
            .args(["--exact", &test_name, "--nocapture"])
            .env(CHILD_MODE, "1")
            .env(LEASE_PATH, &path)
            .status()
            .unwrap();
        assert!(status.success());
        drop(_parent_lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_scope_lease_drop_does_not_unlink_replacement_inode() {
        let root = root("lease-drop-replacement");
        let scope = "trusted/replaced";
        let old =
            ScopeLease::acquire(&root, "cargo", scope, Duration::from_secs(24 * 3600)).unwrap();
        let path = old.path.clone();
        let displaced = path.with_file_name(format!(
            ".displaced-{}.lease",
            uuid::Uuid::new_v4().simple()
        ));
        // Model a pathname replacement while the old descriptor remains open.
        // Drop must compare the current name with the inode it owns.
        fs::rename(&path, &displaced).unwrap();
        let replacement =
            ScopeLease::acquire(&root, "cargo", scope, Duration::from_secs(24 * 3600)).unwrap();

        drop(old);
        assert!(path.exists(), "the replacement lease must remain published");
        assert_eq!(
            active_scopes(&root, Duration::from_secs(24 * 3600)).unwrap(),
            BTreeSet::from([format!("cargo/{scope}")])
        );

        drop(replacement);
        fs::remove_file(displaced).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_reaper_does_not_unlink_a_replacement_inode() {
        let root = root("lease-reap-replacement");
        let scope = "trusted/replaced";
        let path = scope_lease_root(&root).join("cargo").join(format!(
            "{}.json",
            crate::trust_scope::filesystem_key(scope)
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&LeaseRecord {
                scope: scope.to_string(),
                pid: std::process::id(),
                created_unix: unix_now().saturating_sub(2),
                owner_job_id: RequiredOwnerJobId(None),
            })
            .unwrap(),
        )
        .unwrap();
        let _name_lock = lock_scope_lease_name(&path).unwrap();
        let mut old_inode = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let displaced = path.with_file_name(format!(
            ".displaced-{}.lease",
            uuid::Uuid::new_v4().simple()
        ));
        fs::rename(&path, &displaced).unwrap();
        let replacement_created = unix_now();
        fs::write(
            &path,
            serde_json::to_vec(&LeaseRecord {
                scope: scope.to_string(),
                pid: std::process::id(),
                created_unix: replacement_created,
                owner_job_id: RequiredOwnerJobId(None),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(
            !reap_stale_lease_file_locked(&path, Duration::ZERO, &mut old_inode, &_name_lock,)
                .unwrap()
        );
        assert!(
            path.exists(),
            "stale cleanup must not remove the replacement"
        );
        let replacement: LeaseRecord = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(replacement.created_unix, replacement_created);
        fs::remove_file(&path).unwrap();
        fs::remove_file(displaced).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_scope_snapshot_sees_no_partial_lease_publication() {
        let root = root("lease-atomic-publication");
        let scope = "trusted/repo/job";
        let directory = scope_lease_root(&root).join("cargo");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!(
            "{}.json",
            crate::trust_scope::filesystem_key(scope)
        ));
        let record = LeaseRecord {
            scope: scope.to_owned(),
            pid: std::process::id(),
            created_unix: unix_now(),
            owner_job_id: RequiredOwnerJobId(None),
        };
        let (temporary_path, file) = prepare_scope_lease(&path, &record).unwrap();
        assert!(!path.exists(), "the final name stays hidden until complete");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader_root = root.clone();
        let reader_barrier = barrier.clone();
        let reader = std::thread::spawn(move || {
            reader_barrier.wait();
            sender
                .send(active_scopes(&reader_root, Duration::from_secs(60)))
                .unwrap();
            reader_barrier.wait();
            sender
                .send(active_scopes(&reader_root, Duration::from_secs(60)))
                .unwrap();
        });

        barrier.wait();
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap()
                .is_empty(),
            "readers must ignore the unpublished temporary file"
        );
        let mut file = publish_scope_lease(&temporary_path, &path, file).unwrap();
        barrier.wait();
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            BTreeSet::from([format!("cargo/{scope}")]),
            "the final name must expose the complete locked record"
        );
        reader.join().unwrap();
        file.set_len(0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        serde_json::to_writer(
            &mut file,
            &LeaseRecord {
                scope: scope.to_owned(),
                pid: std::process::id(),
                created_unix: unix_now().saturating_sub(2),
                owner_job_id: RequiredOwnerJobId(None),
            },
        )
        .unwrap();
        file.flush().unwrap();
        drop(file);
        assert!(active_scopes(&root, Duration::ZERO).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_scopes_are_typed_by_cache_class() {
        let root = root("typed-lease");
        let _cargo = ScopeLease::acquire(&root, "cargo", "cache", Duration::from_secs(60)).unwrap();
        let _mise = ScopeLease::acquire(&root, "mise", "cache", Duration::from_secs(60)).unwrap();

        assert_eq!(
            active_scopes(&root, Duration::from_secs(60)).unwrap(),
            BTreeSet::from(["cargo/cache".into(), "mise/cache".into()])
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn trust_key_lease_is_disjoint_from_a_valid_old_raw_scope() {
        let root = root("lease-key-collision");
        let trust_key = crate::trust_scope::filesystem_key("trusted");
        // This is the exact old collision: a valid raw scope had the same
        // spelling as the new hashed key for `trusted`, so both flat lease
        // grammars would publish the same filename under the old lease root.
        let raw_old_scope = trust_key.clone();
        let new_scope = "trusted";
        let legacy_root = root.join("leases");
        let old_path = legacy_root.join("actions-cache").join(format!(
            "{}.json",
            crate::container::sanitize_store_key(&raw_old_scope)
        ));
        fs::create_dir_all(old_path.parent().unwrap()).unwrap();
        fs::write(
            &old_path,
            serde_json::to_vec(&LeaseRecord {
                scope: raw_old_scope,
                pid: std::process::id(),
                // A supported package upgrade drains old writers first; this
                // is a stale pre-rotation record left on disk after upgrade.
                created_unix: 0,
                owner_job_id: RequiredOwnerJobId(None),
            })
            .unwrap(),
        )
        .unwrap();

        let lease = ScopeLease::acquire(&root, "actions-cache", new_scope, Duration::from_secs(60))
            .unwrap();
        let versioned_root = scope_lease_root(&root);
        assert_eq!(old_path.file_name(), lease.path.file_name());
        assert_ne!(lease.path, old_path);
        assert!(lease.path.starts_with(&versioned_root));
        assert!(!lease.path.starts_with(&legacy_root));
        assert!(!old_path.starts_with(&versioned_root));
        assert!(
            old_path.is_file(),
            "flat pre-rotation lease remains untouched"
        );
        assert_eq!(
            active_scopes(&root, Duration::from_secs(60)).unwrap(),
            BTreeSet::from([format!("actions-cache/{new_scope}")])
        );
        drop(lease);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn coordinator_blocks_lease_publication_during_reclaim_snapshot() {
        let root = root("coordinator");
        let coordinator = FilesystemCoordinator::lock_exclusive(&root).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread_root = root.clone();
        let handle = std::thread::spawn(move || {
            let lease =
                ScopeLease::acquire(&thread_root, "cargo", "registry", Duration::from_secs(60))
                    .unwrap();
            sender.send(lease).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(coordinator);
        let lease = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(lease);
        handle.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn coordinator_refuses_same_thread_reentry_instead_of_deadlocking() {
        let root = root("coordinator-reentry");
        let exclusive = FilesystemCoordinator::lock_exclusive(&root).unwrap();
        let again = FilesystemCoordinator::lock_exclusive(&root).unwrap_err();
        assert!(
            again
                .to_string()
                .contains("already held exclusively by this thread"),
            "{again:#}"
        );
        let shared_under_exclusive = FilesystemCoordinator::lock_shared(&root).unwrap_err();
        assert!(
            shared_under_exclusive
                .to_string()
                .contains("already held exclusively by this thread"),
            "{shared_under_exclusive:#}"
        );
        drop(exclusive);

        // Releasing clears the registration: the same thread can lock again.
        let shared = FilesystemCoordinator::lock_shared(&root).unwrap();
        // Shared holds do not conflict with each other on the same thread...
        let shared_twice = FilesystemCoordinator::lock_shared(&root).unwrap();
        // ...but an exclusive under a shared hold would block on itself.
        let exclusive_under_shared = FilesystemCoordinator::lock_exclusive(&root).unwrap_err();
        assert!(
            exclusive_under_shared
                .to_string()
                .contains("already held shared by this thread"),
            "{exclusive_under_shared:#}"
        );
        drop(shared_twice);
        drop(shared);
        drop(FilesystemCoordinator::lock_exclusive(&root).unwrap());

        // Different runtime roots are independent.
        let other = self::root("coordinator-reentry-other");
        let outer = FilesystemCoordinator::lock_exclusive(&root).unwrap();
        drop(FilesystemCoordinator::lock_exclusive(&other).unwrap());
        fs::remove_dir_all(other).unwrap();
        drop(outer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn docker_lifecycle_guard_bounds_cross_process_concurrency() {
        let root = root("docker-lifecycle");
        let timeout = Duration::from_secs(30);
        let first = DockerLifecycleGuard::lock_with_concurrency(&root, 2, "test", timeout).unwrap();
        let second =
            DockerLifecycleGuard::lock_with_concurrency(&root, 2, "test", timeout).unwrap();
        assert!(!first.degraded());
        assert!(!second.degraded());
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread_root = root.clone();
        let handle = std::thread::spawn(move || {
            let third =
                DockerLifecycleGuard::lock_with_concurrency(&thread_root, 2, "test", timeout)
                    .unwrap();
            sender.send(third.degraded()).unwrap();
            third
        });

        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        assert!(!receiver.recv_timeout(Duration::from_secs(2)).unwrap());
        drop(second);
        drop(handle.join().unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn docker_lifecycle_guard_degrades_past_the_bound_instead_of_parking() {
        let root = root("docker-lifecycle-bound");
        let timeout = Duration::from_secs(30);
        let _first =
            DockerLifecycleGuard::lock_with_concurrency(&root, 1, "test", timeout).unwrap();
        // Zero bound: one contended sweep, then degrade. Deterministic —
        // no sleep timing involved.
        let waited =
            DockerLifecycleGuard::lock_with_concurrency(&root, 1, "cleanup", Duration::ZERO)
                .unwrap();
        assert!(waited.degraded());
        // No parking: a single contended sweep, no sleep on this path.
        assert!(waited.waited() < Duration::from_secs(5));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_scope_lease_survives_a_record_older_than_24_hours() {
        let root = root("stale-live");
        let owner = JobOwnerId::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap();
        let mut lease = ScopeLease::acquire_for_job(
            &root,
            "cache",
            "trusted/active/hashed-holder",
            owner.clone(),
            Duration::ZERO,
        )
        .unwrap();
        lease._file.set_len(0).unwrap();
        lease._file.seek(SeekFrom::Start(0)).unwrap();
        serde_json::to_writer(
            &mut lease._file,
            &LeaseRecord {
                scope: "trusted/active/hashed-holder".to_string(),
                pid: std::process::id(),
                created_unix: unix_now().saturating_sub(25 * 60 * 60),
                owner_job_id: RequiredOwnerJobId(Some(owner.clone())),
            },
        )
        .unwrap();
        lease._file.flush().unwrap();

        let active = active_scopes(&root, Duration::from_secs(24 * 3600)).unwrap();
        assert!(active.contains("cache/trusted/active/hashed-holder"));
        let records = active_scope_leases(&root, Duration::from_secs(24 * 3600)).unwrap();
        assert_eq!(records[0].owner_job_id.as_ref(), Some(&owner));
        assert!(lease.path.exists());

        drop(lease);
        assert!(active_scopes(&root, Duration::ZERO).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_orphan_scope_lease_is_reaped_without_a_live_lock() {
        let root = root("stale-orphan");
        let scope = "trusted/orphan";
        let path = scope_lease_root(&root).join("cache").join(format!(
            "{}.json",
            crate::trust_scope::filesystem_key(scope)
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&LeaseRecord {
                scope: scope.to_string(),
                pid: std::process::id(),
                created_unix: unix_now().saturating_sub(2),
                owner_job_id: RequiredOwnerJobId(None),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(active_scopes(&root, Duration::ZERO).unwrap().is_empty());
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reservation_blocks_when_short_and_counts_active() {
        let root = root("capacity");
        let controller = CapacityController {
            run_root: root.clone(),
            emergency_reserve_bytes: 10,
            job_peak_bytes: 30,
        };
        assert!(controller.reserve_with_free_bytes(39).is_err());
        let first = controller.reserve_with_free_bytes(70).unwrap();
        assert!(controller.reserve_with_free_bytes(69).is_err());
        drop(first);
        assert!(controller.reserve_with_free_bytes(45).is_err());
        assert!(controller.reserve_with_free_bytes(46).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reservation_drop_releases_active_bytes() {
        let root = root("capacity-drop");
        let controller = CapacityController {
            run_root: root.clone(),
            emergency_reserve_bytes: 0,
            job_peak_bytes: 100,
        };
        let held = controller.reserve_with_free_bytes(100).unwrap();
        assert_eq!(reservation_summary(&root).unwrap(), (1, 100));
        drop(held);
        assert_eq!(reservation_summary(&root).unwrap(), (0, 0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn aged_out_reservation_is_reaped_even_when_pid_alive() {
        let root = root("capacity-age");
        let dir = root.join("reservations");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stale.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        serde_json::to_writer(
            &mut file,
            &ReservationRecord {
                bytes: 100,
                pid: std::process::id(),
                // Far in the past so any positive TTL reaps it.
                created_unix: 1,
            },
        )
        .unwrap();
        file.flush().unwrap();
        // TTL default is hours; use a short one for the test.
        assert_eq!(
            reservation_bytes_with_ttl(&dir, Duration::from_secs(60)).unwrap(),
            0
        );
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn leaked_reservation_older_than_ttl_is_reaped_from_summary_bytes() {
        let root = root("capacity-ttl-summary");
        let dir = root.join("reservations");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("leaked.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        serde_json::to_writer(
            &mut file,
            &ReservationRecord {
                bytes: 17179869184,
                pid: std::process::id(),
                created_unix: 1,
            },
        )
        .unwrap();
        file.flush().unwrap();
        let (count_before_reap, bytes) =
            reservation_summary_with_ttl(&root, Duration::from_secs(60)).unwrap();
        assert_eq!(count_before_reap, 1, "summary counts the file before reap");
        assert_eq!(bytes, 0, "stale reservation bytes must not block admission");
        assert!(!path.exists(), "leaked reservation file must be removed");
        assert_eq!(
            reservation_summary_with_ttl(&root, Duration::from_secs(60)).unwrap(),
            (0, 0)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn capacity_wait_is_bounded_then_times_out_never_success() {
        let timeout = Duration::from_secs(DEFAULT_CAPACITY_WAIT_SECS);
        assert_eq!(
            pre_execution_capacity_wait_decision(Duration::ZERO, timeout),
            CapacityWaitDecision::Retry {
                sleep: Duration::from_secs(CAPACITY_WAIT_RETRY_SECS)
            }
        );
        assert_eq!(
            pre_execution_capacity_wait_decision(
                timeout.saturating_sub(Duration::from_secs(1)),
                timeout
            ),
            CapacityWaitDecision::Retry {
                sleep: Duration::from_secs(1)
            }
        );
        assert_eq!(
            pre_execution_capacity_wait_decision(timeout, timeout),
            CapacityWaitDecision::Timeout
        );
        assert_eq!(
            pre_execution_capacity_wait_decision(timeout + Duration::from_secs(30), timeout),
            CapacityWaitDecision::Timeout
        );
        assert!(
            !matches!(
                pre_execution_capacity_wait_decision(timeout, timeout),
                CapacityWaitDecision::Retry { .. }
            ),
            "elapsed >= timeout must not keep retrying"
        );
        let reason = host_capacity_timeout_reason(
            timeout,
            timeout,
            "capacity backpressure: free=1 required=2",
        );
        assert!(!reason.trim().is_empty());
        assert!(reason.contains("capacity backpressure: free=1 required=2"));
        assert!(!host_capacity_timeout_reason(timeout, timeout, "   ")
            .trim()
            .is_empty());
    }

    #[test]
    fn pre_execution_wait_decision_reads_lost_cancel_and_capacity() {
        let retry = CapacityWaitDecision::Retry {
            sleep: Duration::from_secs(15),
        };
        assert_eq!(
            pre_execution_wait_decision(true, false, false, retry),
            PreExecutionWaitDecision::AbortRegistrationLost
        );
        assert_eq!(
            pre_execution_wait_decision(false, true, true, retry),
            PreExecutionWaitDecision::AbortCanceled
        );
        assert_eq!(
            pre_execution_wait_decision(false, false, true, retry),
            PreExecutionWaitDecision::Reserved
        );
        assert_eq!(
            pre_execution_wait_decision(false, false, false, retry),
            PreExecutionWaitDecision::RetryReserve {
                sleep: Duration::from_secs(15)
            }
        );
        assert_eq!(
            pre_execution_wait_decision(false, false, false, CapacityWaitDecision::Timeout),
            PreExecutionWaitDecision::AbortCapacityTimeout
        );
        assert_ne!(
            pre_execution_wait_decision(true, false, false, retry),
            PreExecutionWaitDecision::RetryReserve {
                sleep: Duration::from_secs(15)
            }
        );
    }

    #[test]
    fn queued_unassigned_jobs_fail_closed_after_bound_never_success() {
        let timeout = Duration::from_secs(DEFAULT_QUEUE_WAIT_SECS);
        assert_eq!(
            queued_unassigned_decision(Duration::ZERO, timeout),
            QueuedUnassignedDecision::Wait
        );
        assert_eq!(
            queued_unassigned_decision(timeout, timeout),
            QueuedUnassignedDecision::FailClosed
        );
        assert_eq!(
            queued_unassigned_decision(timeout + Duration::from_secs(1), timeout),
            QueuedUnassignedDecision::FailClosed
        );
        let jobs = [
            QueuedUnassignedJob {
                run_id: 1,
                job_id: "fresh".into(),
                repository: "jackin-project/jackin".into(),
                queued_for: Duration::from_secs(10),
            },
            QueuedUnassignedJob {
                run_id: 2,
                job_id: "stale".into(),
                repository: "jackin-project/jackin".into(),
                queued_for: timeout,
            },
        ];
        let expired = queued_unassigned_jobs_past_deadline(&jobs, timeout);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].job_id, "stale");
        let reason = queue_timeout_reason(timeout, timeout);
        assert!(reason.contains("never assigned"));
        assert!(!reason.trim().is_empty());
        assert_eq!(
            queue_wait_decision(true, timeout + Duration::from_secs(1), timeout),
            QueuedUnassignedDecision::Wait
        );
        assert_eq!(
            queue_wait_decision(false, timeout + Duration::from_secs(1), timeout),
            QueuedUnassignedDecision::FailClosed
        );
        assert!(job_waits_on_trusted_fleet(&[
            "self-hosted".into(),
            "velnor-trusted".into()
        ]));
        assert!(!job_waits_on_trusted_fleet(&["ubuntu-26.04".into()]));
    }

    #[test]
    fn stale_busy_offline_must_complete_job_online_busy_must_not() {
        assert!(stale_busy_lease_should_complete_job(
            Some("offline"),
            Some(true)
        ));
        assert!(stale_busy_lease_should_complete_job(None, Some(true)));
        assert!(!stale_busy_lease_should_complete_job(
            Some("online"),
            Some(true)
        ));
        assert!(!stale_busy_lease_should_complete_job(
            Some("offline"),
            Some(false)
        ));
        assert!(!stale_busy_lease_should_complete_job(
            Some("online"),
            Some(false)
        ));
    }
}
