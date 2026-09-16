//! Host-wide ownership of one run-service job.
//!
//! GitHub can deliver the same runner request to sibling JIT slots. The run
//! service acquisition is not a sufficient exclusion boundary, so a slot claims
//! the plan/job pair locally — an exclusive `flock` on
//! `<run root>/job-claims/<plan>-<job>` — before it touches the job's
//! deterministic workspace, and holds it through teardown.
//!
//! This module owns the claim's whole lifetime. The claim used to be only a
//! lock: dropping it released the `flock` and left the file behind, so a host
//! accumulated one dead file per job forever (thousands under `/run/velnor`
//! on Sentry). A claim now unlinks its file while it still holds the lock, and
//! the liveness sweep unlinks claims nobody holds. Both removals are safe
//! against a concurrent claimant because of two rules:
//!
//! * A claimant that locked a file confirms the path still names that inode.
//!   If the holder unlinked between the claimant's `open` and `flock`, the
//!   claimant locked an orphan and retries on the live path.
//! * The sweep decides "unheld" and unlinks under the exclusive
//!   `job-claims/.sweep.lock`, which every claimant holds shared across its
//!   `open`/`flock`/verify. A claimant can therefore never be mid-acquire on a
//!   file the sweep is removing, and once a claim is held the sweep's probe
//!   sees `WOULDBLOCK` and leaves it.
//!
//! The sweep's probe is the only thing besides a claimant that ever locks a
//! claim file, and it does so only when no claimant is inside the acquire
//! section, so a probe cannot make a real claimant see a spurious duplicate.
//!
//! Neither removal is serialized against a claimant's `open(O_CREAT)` of the
//! same name, and that is deliberate: the holder's unlink runs outside the
//! sweep lock. Native Linux filesystems make lookup-or-create atomic against
//! unlink under the parent's inode lock, but the run root is not guaranteed
//! to be one — a virtiofs/FUSE-backed directory (a bind mount shared from a
//! macOS host) answers the race with `ENOENT` about a third of the time
//! (measured on OrbStack: 12968 of 42717 opens). So `ENOENT` from the claim
//! open is the same event as the orphan inode — the previous owner released
//! between our lookup and our create — and is retried the same way, and the
//! sweep treats a claim that vanished between `readdir` and `open` as gone,
//! not as held.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rustix::fs::FlockOperation;

const CLAIMS_DIR: &str = "job-claims";
const SWEEP_LOCK: &str = ".sweep.lock";

/// Held for one job from acceptance through teardown; released and unlinked
/// on drop, on every path the owning scope exits.
pub struct JobClaim {
    file: File,
    path: PathBuf,
}

impl std::fmt::Debug for JobClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobClaim")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl JobClaim {
    /// Claim `plan_id`/`job_id` for this process, or `None` when another
    /// slot on this host already owns it.
    pub fn try_acquire(run_root: &Path, plan_id: &str, job_id: &str) -> Result<Option<Self>> {
        let claims = claims_dir(run_root)?;
        let path = claims.join(claim_file_name(plan_id, job_id));
        loop {
            let _acquiring = SweepLock::shared(&claims)?;
            let file = match open_claim(&path) {
                Ok(file) => file,
                // The previous owner unlinked its claim between our lookup
                // and our create (a filesystem whose lookup-or-create is not
                // atomic against unlink). Same event as the orphan inode
                // below: the path is free now, try again on it.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("open host job claim {}", path.display()));
                }
            };
            match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {}
                Err(rustix::io::Errno::WOULDBLOCK) => return Ok(None),
                Err(error) => return Err(error).context("lock host job claim"),
            }
            if names_inode(&path, &file)? {
                return Ok(Some(Self { file, path }));
            }
            // The previous owner unlinked its claim between our open and our
            // lock: we hold an orphan inode. Try again on the live path.
        }
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for JobClaim {
    fn drop(&mut self) {
        // Unlink while the lock is still held (`file` closes after this body),
        // so no claimant can observe an unlocked file at this path.
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!(
                "Warning: could not remove job claim {}: {error}",
                self.path.display()
            ),
        }
        let _ = &self.file;
    }
}

/// Job ids whose host job-claim lock is currently held by a live process.
///
/// The claim is taken before the job's workspace exists and released only when
/// the job process exits, so it covers checkout and teardown. A claim file that
/// still locks is proof; one that can be locked is stale — left by a process
/// that died holding it — and is unlinked here so stale claims cannot pile up.
pub fn held_job_claim_ids(run_root: &Path) -> Result<BTreeSet<String>> {
    let claims = run_root.join(CLAIMS_DIR);
    let mut held = BTreeSet::new();
    let entries = match fs::read_dir(&claims) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(held),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", claims.display()));
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry in {}", claims.display()))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == SWEEP_LOCK || !path.is_file() {
            continue;
        }
        // Exclusive: no claimant is between its open and its lock while we
        // decide whether this file is unheld and remove it.
        let _sweeping = SweepLock::exclusive(&claims)?;
        let file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            // Its owner released and unlinked it after `readdir` listed it:
            // nothing holds a claim that no longer exists.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                // Unreadable claim: assume held rather than delete its workspace.
                held.extend(job_uuids_in(&name));
                continue;
            }
        };
        match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                // Nobody holds it. Remove it under our lock; a claimant that
                // opens the path after this creates a fresh inode.
                if let Err(error) = fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!(
                        "Warning: could not remove stale job claim {}: {error}",
                        path.display()
                    );
                }
            }
            Err(rustix::io::Errno::WOULDBLOCK) => held.extend(job_uuids_in(&name)),
            Err(_) => held.extend(job_uuids_in(&name)),
        }
    }
    Ok(held)
}

fn claims_dir(run_root: &Path) -> Result<PathBuf> {
    let claims = run_root.join(CLAIMS_DIR);
    fs::create_dir_all(&claims)
        .with_context(|| format!("create job claim directory {}", claims.display()))?;
    Ok(claims)
}

fn claim_file_name(plan_id: &str, job_id: &str) -> String {
    crate::container::sanitize_store_key(&format!("{plan_id}-{job_id}"))
}

fn open_claim(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Whether `path` currently names the inode behind `file`.
fn names_inode(path: &Path, file: &File) -> Result<bool> {
    let by_fd = file
        .metadata()
        .with_context(|| format!("stat held job claim {}", path.display()))?;
    match fs::metadata(path) {
        Ok(by_path) => Ok(by_path.ino() == by_fd.ino() && by_path.dev() == by_fd.dev()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stat job claim path {}", path.display())),
    }
}

/// Every job-UUID-shaped substring of a claim file name (`<plan>-<job>`).
fn job_uuids_in(name: &str) -> BTreeSet<String> {
    let parts: Vec<&str> = name.split('-').collect();
    let mut found = BTreeSet::new();
    for window in parts.windows(5) {
        let candidate = window.join("-");
        if crate::leftover_disk::looks_like_job_uuid(&candidate) {
            found.insert(candidate);
        }
    }
    found
}

/// Serializes claim acquisition (shared) against stale-claim removal
/// (exclusive). Held for microseconds per file on either side.
struct SweepLock {
    _file: File,
}

impl SweepLock {
    fn shared(claims: &Path) -> Result<Self> {
        Self::acquire(claims, FlockOperation::LockShared)
    }

    fn exclusive(claims: &Path) -> Result<Self> {
        Self::acquire(claims, FlockOperation::LockExclusive)
    }

    fn acquire(claims: &Path, operation: FlockOperation) -> Result<Self> {
        let path = claims.join(SWEEP_LOCK);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open job claim sweep lock {}", path.display()))?;
        rustix::fs::flock(&file, operation)
            .with_context(|| format!("lock job claim sweep lock {}", path.display()))?;
        Ok(Self { _file: file })
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn temp_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("velnor-job-claim-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn claim_files(root: &Path) -> Vec<String> {
        let mut names = fs::read_dir(root.join(CLAIMS_DIR))
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .filter(|name| name != SWEEP_LOCK)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn claim_excludes_duplicate_slots_until_owner_drops() {
        let root = temp_root("exclusive");
        let owner = JobClaim::try_acquire(&root, "plan", "job").unwrap();
        assert!(owner.is_some());
        assert!(JobClaim::try_acquire(&root, "plan", "job")
            .unwrap()
            .is_none());
        assert!(JobClaim::try_acquire(&root, "plan", "other-job")
            .unwrap()
            .is_some());
        drop(owner);
        assert!(JobClaim::try_acquire(&root, "plan", "job")
            .unwrap()
            .is_some());
        fs::remove_dir_all(root).unwrap();
    }

    /// The Sentry defect: a released claim must not leave its file behind, on
    /// the completion path (drop at scope end), the failure path (drop during
    /// unwinding), and the teardown hand-off (drop on another thread).
    #[test]
    fn released_claim_unlinks_its_file_on_every_exit_path() {
        let root = temp_root("unlink");

        // Completion: drop at the end of the owning scope.
        {
            let claim = JobClaim::try_acquire(&root, "plan", "done")
                .unwrap()
                .unwrap();
            assert!(claim.path().exists());
            assert_eq!(claim_files(&root), ["plan-done"]);
        }
        assert!(
            claim_files(&root).is_empty(),
            "completion left a claim file"
        );

        // Failure: drop while a panic unwinds through the owning frame.
        let root_for_panic = root.clone();
        let outcome = std::panic::catch_unwind(move || {
            let _claim = JobClaim::try_acquire(&root_for_panic, "plan", "failed")
                .unwrap()
                .unwrap();
            panic!("job execution failed");
        });
        assert!(outcome.is_err());
        assert!(
            claim_files(&root).is_empty(),
            "the failure path left a claim file"
        );

        // Teardown hand-off: the claim moves to the teardown thread and is
        // released, and unlinked, when that thread finishes.
        let claim = JobClaim::try_acquire(&root, "plan", "torn-down")
            .unwrap()
            .unwrap();
        let release = Arc::new(AtomicBool::new(false));
        let release_in_teardown = Arc::clone(&release);
        let teardown = std::thread::spawn(move || {
            let _claim = claim;
            while !release_in_teardown.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
        });
        assert!(JobClaim::try_acquire(&root, "plan", "torn-down")
            .unwrap()
            .is_none());
        assert_eq!(claim_files(&root), ["plan-torn-down"]);
        release.store(true, Ordering::SeqCst);
        teardown.join().unwrap();
        assert!(claim_files(&root).is_empty(), "teardown left a claim file");
        assert!(JobClaim::try_acquire(&root, "plan", "torn-down")
            .unwrap()
            .is_some());
        fs::remove_dir_all(root).unwrap();
    }

    /// A claimant that raced the owner's unlink (open before, lock after) has
    /// locked an orphan inode; it must end up owning the live path, and a
    /// second claimant must then see it as a duplicate.
    #[test]
    fn claimant_that_locked_an_orphan_inode_retries_on_the_live_path() {
        let root = temp_root("orphan");
        let claims = claims_dir(&root).unwrap();
        let path = claims.join(claim_file_name("plan", "job"));
        // Stage the race outcome directly: an unlocked orphan inode open at
        // the old path, with the path itself gone.
        let orphan = open_claim(&path).unwrap();
        fs::remove_file(&path).unwrap();
        rustix::fs::flock(&orphan, FlockOperation::NonBlockingLockExclusive).unwrap();
        assert!(!names_inode(&path, &orphan).unwrap());

        let claim = JobClaim::try_acquire(&root, "plan", "job")
            .unwrap()
            .unwrap();
        assert!(names_inode(&path, &claim.file).unwrap());
        assert!(JobClaim::try_acquire(&root, "plan", "job")
            .unwrap()
            .is_none());
        drop(orphan);
        drop(claim);
        fs::remove_dir_all(root).unwrap();
    }

    /// Stale claims (a process died holding one) do not count as live and are
    /// swept; held claims count and stay.
    #[test]
    fn sweep_reports_held_claims_and_removes_stale_ones() {
        let root = temp_root("sweep");
        let held_job = "11111111-2222-5333-8444-555555555555";
        let stale_job = "aaaaaaaa-bbbb-5ccc-8ddd-eeeeeeeeeeee";
        let held = JobClaim::try_acquire(&root, "plan", held_job)
            .unwrap()
            .unwrap();
        // A crashed owner: the file exists, nothing locks it.
        let stale_path = root
            .join(CLAIMS_DIR)
            .join(claim_file_name("plan", stale_job));
        drop(open_claim(&stale_path).unwrap());
        assert_eq!(claim_files(&root).len(), 2);

        let live = held_job_claim_ids(&root).unwrap();
        assert_eq!(live, BTreeSet::from([held_job.to_owned()]));
        assert_eq!(
            claim_files(&root),
            [claim_file_name("plan", held_job)],
            "the stale claim must be swept and the held one kept"
        );
        // The held claim is untouched by the sweep's probe.
        assert!(JobClaim::try_acquire(&root, "plan", held_job)
            .unwrap()
            .is_none());
        drop(held);
        assert!(claim_files(&root).is_empty());
        assert!(held_job_claim_ids(&root).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// Claimants and the sweep running concurrently never lose a claim: at
    /// most one claimant owns a given job at any time, and every claim file
    /// is gone once all owners have dropped.
    #[test]
    fn concurrent_claimants_and_sweeps_keep_exactly_one_owner() {
        let root = temp_root("concurrent");
        let job = "01234567-89ab-5cde-8f01-23456789abcd";
        let owners = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let overlapped = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        for _ in 0..4 {
            let root = root.clone();
            let owners = Arc::clone(&owners);
            let overlapped = Arc::clone(&overlapped);
            let stop = Arc::clone(&stop);
            threads.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    if let Some(claim) = JobClaim::try_acquire(&root, "plan", job).unwrap() {
                        if owners.fetch_add(1, Ordering::SeqCst) != 0 {
                            overlapped.store(true, Ordering::SeqCst);
                        }
                        std::thread::yield_now();
                        owners.fetch_sub(1, Ordering::SeqCst);
                        drop(claim);
                    }
                }
                stop.store(true, Ordering::SeqCst);
            }));
        }
        let sweeper = {
            let root = root.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    held_job_claim_ids(&root).unwrap();
                }
            })
        };
        for thread in threads {
            thread.join().unwrap();
        }
        sweeper.join().unwrap();
        assert!(
            !overlapped.load(Ordering::SeqCst),
            "two claimants owned one job"
        );
        assert!(claim_files(&root).is_empty(), "{:?}", claim_files(&root));
        fs::remove_dir_all(root).unwrap();
    }
}
