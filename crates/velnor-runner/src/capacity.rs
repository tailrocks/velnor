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

#[derive(Debug)]
pub struct ScopeLease {
    path: PathBuf,
}

impl ScopeLease {
    pub fn acquire(
        run_root: &Path,
        class: &str,
        scope: &str,
        stale_after: Duration,
    ) -> Result<Self> {
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
    for class in fs::read_dir(&root)? {
        let class = class?.path();
        if !class.is_dir() {
            continue;
        }
        for entry in fs::read_dir(class)? {
            let path = entry?.path();
            if lease_is_stale(&path, stale_after)? {
                let _ = fs::remove_file(path);
                continue;
            }
            let record: LeaseRecord = serde_json::from_slice(&fs::read(path)?)?;
            let parts: Vec<_> = record.scope.split('/').collect();
            for length in 2..=parts.len() {
                active.insert(parts[..length].join("/"));
            }
            if parts.len() > 1 {
                for length in 1..parts.len() {
                    active.insert(parts[1..=length].join("/"));
                }
            }
            active.insert(record.scope);
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

fn reservation_bytes(dir: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let record: ReservationRecord = serde_json::from_slice(&fs::read(&path)?)?;
        if Path::new("/proc").exists() && !Path::new("/proc").join(record.pid.to_string()).exists()
        {
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
}
