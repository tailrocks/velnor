use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const DEFAULT_DOCKER_LIFECYCLE_CONCURRENCY: usize = 2;
const MAX_DOCKER_LIFECYCLE_CONCURRENCY: usize = 8;
const DOCKER_LIFECYCLE_RETRY: Duration = Duration::from_millis(25);

#[derive(Debug, Serialize, Deserialize)]
struct LeaseRecord {
    scope: String,
    pid: u32,
    created_unix: u64,
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
    let secs = std::env::var("VELNOR_RESERVATION_TTL_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(6 * 3600);
    Duration::from_secs(secs.max(60))
}

/// Retry interval while an acquired job waits for host disk peak.
pub const CAPACITY_WAIT_RETRY_SECS: u64 = 15;

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
}

/// Serializes lease publication against destructive cache snapshots.
///
/// Lease files remain the fine-grained liveness authority. The coordinator
/// closes the cross-daemon race where a reaper snapshots those files while a
/// different daemon is publishing a lease for the same store.
pub struct FilesystemCoordinator {
    _file: fs::File,
}

impl FilesystemCoordinator {
    pub fn lock_shared(run_root: &Path) -> Result<Self> {
        Self::lock(run_root, rustix::fs::FlockOperation::LockShared)
    }

    pub fn lock_exclusive(run_root: &Path) -> Result<Self> {
        Self::lock(run_root, rustix::fs::FlockOperation::LockExclusive)
    }

    fn lock(run_root: &Path, operation: rustix::fs::FlockOperation) -> Result<Self> {
        fs::create_dir_all(run_root)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(run_root.join("filesystem-coordinator.lock"))?;
        rustix::fs::flock(&file, operation).context("lock filesystem coordinator")?;
        Ok(Self { _file: file })
    }
}

/// Bounds Docker control-plane lifecycle mutations across daemon processes on
/// one host. Job containers remain concurrent; create, start, and teardown
/// bursts use two host-wide permits by default because dockerd's control
/// plane turns an unbounded fan-out into 10–70s tail latency. Operators can
/// tune the bound with `VELNOR_DOCKER_LIFECYCLE_CONCURRENCY` (1–8).
pub struct DockerLifecycleGuard {
    _file: fs::File,
}

impl DockerLifecycleGuard {
    pub fn lock(run_root: &Path) -> Result<Self> {
        let concurrency = std::env::var("VELNOR_DOCKER_LIFECYCLE_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=MAX_DOCKER_LIFECYCLE_CONCURRENCY).contains(value))
            .unwrap_or(DEFAULT_DOCKER_LIFECYCLE_CONCURRENCY);
        Self::lock_with_concurrency(run_root, concurrency)
    }

    fn lock_with_concurrency(run_root: &Path, concurrency: usize) -> Result<Self> {
        if concurrency == 0 {
            bail!("Docker lifecycle concurrency must be at least 1");
        }
        fs::create_dir_all(run_root)?;
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
                    Ok(()) => return Ok(Self { _file: file }),
                    Err(rustix::io::Errno::WOULDBLOCK) => {}
                    Err(error) => return Err(error).context("lock Docker lifecycle coordinator"),
                }
            }
            std::thread::sleep(DOCKER_LIFECYCLE_RETRY);
        }
    }
}

impl ScopeLease {
    pub fn acquire(
        run_root: &Path,
        class: &str,
        scope: &str,
        stale_after: Duration,
    ) -> Result<Self> {
        let _coordinator = FilesystemCoordinator::lock_shared(run_root)?;
        let dir = run_root
            .join("leases")
            .join(crate::container::sanitize_store_key(class));
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "{}.json",
            crate::container::sanitize_store_key(scope)
        ));
        if path.exists() && lease_is_stale(&path, stale_after)? {
            fs::remove_file(&path)
                .with_context(|| format!("remove stale lease {}", path.display()))?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("scope lease already held: {class}/{scope}"))?;
        serde_json::to_writer(
            &mut file,
            &LeaseRecord {
                scope: scope.to_string(),
                pid: std::process::id(),
                created_unix: unix_now(),
            },
        )?;
        file.flush()?;
        Ok(Self { path })
    }
}

impl Drop for ScopeLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lease_is_stale(path: &Path, stale_after: Duration) -> Result<bool> {
    let record: LeaseRecord = serde_json::from_slice(&fs::read(path)?)?;
    let age_stale = unix_now().saturating_sub(record.created_unix) > stale_after.as_secs();
    let proc_root = Path::new("/proc");
    let pid_gone = proc_root.exists() && !proc_root.join(record.pid.to_string()).exists();
    Ok(age_stale || pid_gone)
}

pub fn active_scopes(run_root: &Path, stale_after: Duration) -> Result<BTreeSet<String>> {
    let root = run_root.join("leases");
    let mut active = BTreeSet::new();
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
            if lease_is_stale(&path, stale_after)? {
                let _ = fs::remove_file(path);
                continue;
            }
            let record: LeaseRecord = serde_json::from_slice(&fs::read(path)?)?;
            active.insert(format!("{class}/{}", record.scope));
        }
    }
    Ok(active)
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
    let mut total = 0u64;
    let ttl = reservation_ttl();
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
    let dir = run_root.join("reservations");
    if !dir.exists() {
        return Ok((0, 0));
    }
    let count = fs::read_dir(&dir)?.count();
    Ok((count, reservation_bytes(&dir)?))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("velnor-{name}-{}", uuid::Uuid::new_v4()))
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
    fn docker_lifecycle_guard_bounds_cross_process_concurrency() {
        let root = root("docker-lifecycle");
        let first = DockerLifecycleGuard::lock_with_concurrency(&root, 2).unwrap();
        let second = DockerLifecycleGuard::lock_with_concurrency(&root, 2).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread_root = root.clone();
        let handle = std::thread::spawn(move || {
            let third = DockerLifecycleGuard::lock_with_concurrency(&thread_root, 2).unwrap();
            sender.send(()).unwrap();
            third
        });

        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        assert!(receiver.recv_timeout(Duration::from_secs(2)).is_ok());
        drop(second);
        drop(handle.join().unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_lease_is_reaped() {
        let root = root("stale");
        let lease = ScopeLease::acquire(&root, "cache", "trusted/old", Duration::ZERO).unwrap();
        let (release_sender, release_receiver) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            release_receiver.recv().unwrap();
            drop(lease);
        });
        std::thread::sleep(Duration::from_secs(1));
        assert!(active_scopes(&root, Duration::ZERO).unwrap().is_empty());
        release_sender.send(()).unwrap();
        holder.join().unwrap();
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
        // TTL default is hours; force a short one for the test.
        // SAFETY: single-threaded test, restored below.
        std::env::set_var("VELNOR_RESERVATION_TTL_SECS", "60");
        assert_eq!(reservation_bytes(&dir).unwrap(), 0);
        assert!(!path.exists());
        std::env::remove_var("VELNOR_RESERVATION_TTL_SECS");
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
        std::env::set_var("VELNOR_RESERVATION_TTL_SECS", "60");
        let (count_before_reap, bytes) = reservation_summary(&root).unwrap();
        assert_eq!(count_before_reap, 1, "summary counts the file before reap");
        assert_eq!(bytes, 0, "stale reservation bytes must not block admission");
        assert!(!path.exists(), "leaked reservation file must be removed");
        assert_eq!(reservation_summary(&root).unwrap(), (0, 0));
        std::env::remove_var("VELNOR_RESERVATION_TTL_SECS");
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
