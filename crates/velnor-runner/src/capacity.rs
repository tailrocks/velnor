use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

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
    fn stale_lease_is_reaped() {
        let root = root("stale");
        let lease = ScopeLease::acquire(&root, "cache", "trusted/old", Duration::ZERO).unwrap();
        std::mem::forget(lease);
        std::thread::sleep(Duration::from_secs(1));
        assert!(active_scopes(&root, Duration::ZERO).unwrap().is_empty());
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
}
