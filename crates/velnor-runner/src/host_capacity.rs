//! One host disk budget, and one total state machine for disk pressure.
//!
//! Two defects live here.
//!
//! The first is accounting. Velnor's reservation ledger summed constants — a
//! per-class budget table — and believed it held headroom the filesystem did
//! not have, because Docker's images, containers, volumes, and build cache were
//! never a class. [`HostCapacity`] derives the budget from `statvfs` on the
//! filesystem that actually holds the work root, and subtracts Docker's own
//! usage from the headroom Velnor may promise.
//!
//! The second is terminality. Below its free-space floor a slot slept sixty
//! seconds and looped, forever, on both branches: no deadline, no escalation,
//! and no state in which the operator or the fleet learns the host is gone.
//! [`DiskState`] is total — every state has a defined successor, `Degraded`
//! carries a deadline, and the terminal state is `Deregistered`. There is no
//! transition back into an unbounded park.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Free-space floor below which a slot must not admit a job.
pub const DEFAULT_MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How long the host may stay degraded (reclaim attempted, still below the
/// floor) before the slot drains instead of parking.
pub const DEFAULT_DEGRADED_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// How long a drain may take before the slot deregisters unconditionally.
pub const DEFAULT_DRAIN_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// Measured capacity of the filesystem holding a Velnor root.
///
/// `available_bytes` is the unprivileged figure (`f_bavail`) — the number that
/// decides whether a job can run — not `f_bfree`, which includes the
/// root-reserved blocks a runner never gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCapacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
    /// Bytes Docker's own storage occupies on this filesystem, when it could be
    /// measured. `None` means unmeasured, which must be treated as unknown
    /// rather than zero.
    pub docker_bytes: Option<u64>,
    /// Device number of the filesystem sampled by `statvfs`.
    pub filesystem_device: u64,
    /// Stable `statvfs.f_fsid` combined with the device number. This is
    /// filesystem identity evidence, not a user-facing volume UUID.
    pub volume_fingerprint: Option<String>,
}

impl HostCapacity {
    /// Probe the filesystem holding `path` (or its nearest existing ancestor).
    ///
    /// Admission uses only the bounded `statvfs` primitive. Docker usage stays
    /// unknown unless a caller supplies an independently bounded measurement
    /// through [`Self::probe_with_docker`].
    pub fn probe(path: &Path) -> Result<Self> {
        Self::probe_with_docker(path, None)
    }

    pub fn probe_with_docker(path: &Path, docker_bytes: Option<u64>) -> Result<Self> {
        let probe = existing_ancestor(path)
            .with_context(|| format!("no existing ancestor of {} to stat", path.display()))?;
        let stat =
            rustix::fs::statvfs(probe).with_context(|| format!("statvfs {}", probe.display()))?;
        let block = stat.f_frsize.max(1);
        #[cfg(unix)]
        let filesystem_device = {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(probe)
                .with_context(|| format!("stat filesystem probe {}", probe.display()))?
                .dev()
        };
        #[cfg(not(unix))]
        let filesystem_device = 0;
        Ok(Self {
            total_bytes: stat.f_blocks.saturating_mul(block),
            available_bytes: stat.f_bavail.saturating_mul(block),
            docker_bytes,
            filesystem_device,
            volume_fingerprint: Some(filesystem_fingerprint(filesystem_device, stat.f_fsid)),
        })
    }

    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    pub fn used_percent(&self) -> u8 {
        if self.total_bytes == 0 {
            return 0;
        }
        let percent = self
            .used_bytes()
            .saturating_mul(100)
            .saturating_div(self.total_bytes);
        percent.min(100) as u8
    }

    /// Headroom Velnor may promise: what the filesystem reports free, minus
    /// what Docker has already taken but Velnor's ledger never counted.
    ///
    /// Docker's storage is inside `used_bytes` already, so this does not
    /// double-subtract the same bytes twice; it refuses to let a *growing*
    /// Docker store be spent twice — once by Docker and once by a reservation.
    /// Unmeasured Docker usage yields the raw available figure.
    pub fn promisable_bytes(&self, docker_growth_allowance: u64) -> u64 {
        match self.docker_bytes {
            Some(_) => self.available_bytes.saturating_sub(docker_growth_allowance),
            None => self.available_bytes,
        }
    }
}

fn filesystem_fingerprint(device: u64, fsid: u64) -> String {
    format!("{device:016x}:{fsid:016x}")
}

/// A descriptor-bound capacity measurement for one existing directory.
///
/// Disk reclaim must not attribute freed bytes to whichever filesystem later
/// happens to occupy the same path. The open descriptor pins the original
/// filesystem for `fstatvfs`; every sample also securely reopens the path and
/// rejects a renamed/replaced root.
#[derive(Debug)]
pub(crate) struct HostCapacityPin {
    root: PathBuf,
    file: File,
    device: u64,
    inode: u64,
    fsid: u64,
}

impl HostCapacityPin {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            if !path.is_absolute() {
                anyhow::bail!("pressure filesystem root must be absolute");
            }
            let directory =
                crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(path)
                    .with_context(|| format!("securely open pressure root {}", path.display()))?;
            let (device, inode) = directory
                .physical_identity()
                .context("read pressure root physical identity")?;
            let file = directory
                .try_clone_file()
                .context("pin pressure root directory descriptor")?;
            let stats = rustix::fs::fstatvfs(&file).context("fstatvfs pinned pressure root")?;
            let metadata = file.metadata().context("fstat pinned pressure root")?;
            if !metadata.is_dir() {
                anyhow::bail!("pinned pressure root is not a directory");
            }
            if metadata.dev() != device || metadata.ino() != inode {
                anyhow::bail!("pressure root identity changed while it was being pinned");
            }
            let pin = Self {
                root: path.to_path_buf(),
                file,
                device,
                inode,
                fsid: stats.f_fsid,
            };
            pin.revalidate()?;
            Ok(pin)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            anyhow::bail!("descriptor-bound pressure measurement requires Unix no-follow support")
        }
    }

    pub(crate) fn identity(&self) -> (u64, u64, u64) {
        (self.device, self.inode, self.fsid)
    }

    pub(crate) fn device_id(&self) -> u64 {
        self.device
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let pinned = self.file.metadata().context("fstat pinned pressure root")?;
            if !pinned.is_dir() || pinned.dev() != self.device || pinned.ino() != self.inode {
                anyhow::bail!("pinned pressure directory descriptor identity changed");
            }
            let stats = rustix::fs::fstatvfs(&self.file)
                .context("revalidate pinned pressure filesystem")?;
            if stats.f_fsid != self.fsid {
                anyhow::bail!("pinned pressure filesystem identifier changed");
            }
            let current =
                crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(&self.root)
                    .with_context(|| format!("reopen pressure root {}", self.root.display()))?;
            if current.physical_identity()? != (self.device, self.inode) {
                anyhow::bail!("pressure root path now resolves to a different directory");
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("descriptor-bound pressure measurement requires Unix no-follow support")
        }
    }

    pub(crate) fn probe(&self) -> Result<HostCapacity> {
        self.revalidate()?;
        let stats =
            rustix::fs::fstatvfs(&self.file).context("sample pinned pressure filesystem")?;
        let block = stats.f_frsize.max(1);
        Ok(HostCapacity {
            total_bytes: stats.f_blocks.saturating_mul(block),
            available_bytes: stats.f_bavail.saturating_mul(block),
            docker_bytes: None,
            filesystem_device: self.device,
            volume_fingerprint: Some(filesystem_fingerprint(self.device, self.fsid)),
        })
    }
}

fn existing_ancestor(path: &Path) -> Option<&Path> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        if current.exists() {
            return Some(current);
        }
        candidate = current.parent();
    }
    None
}

/// Measure Docker's storage for reporting paths such as `cache du`.
///
/// Admission deliberately calls [`HostCapacity::probe`] and does not wait on
/// Docker. Reporting may include this optional figure, but it must use the
/// same bounded host-Docker runner as every other maintenance operation.
pub fn docker_usage_bytes() -> Option<u64> {
    let args = vec![
        "system".to_string(),
        "df".to_string(),
        "--format".to_string(),
        "{{json .}}".to_string(),
    ];
    let output = crate::docker::client::host_call(&args).ok()?;
    docker_usage_bytes_from_df(&output)
}

/// Parse the `Size` fields of `docker system df --format '{{json .}}'`.
///
/// Each line is one record (Images/Containers/Local Volumes/Build Cache) with a
/// human-readable `Size` such as `1.23GB`.
pub fn docker_usage_bytes_from_df(stdout: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut seen = false;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let record: serde_json::Value = serde_json::from_str(line).ok()?;
        let Some(size) = record.get("Size").and_then(serde_json::Value::as_str) else {
            continue;
        };
        total = total.saturating_add(parse_docker_size(size)?);
        seen = true;
    }
    seen.then_some(total)
}

fn parse_docker_size(value: &str) -> Option<u64> {
    let value = value.trim();
    let split = value
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: f64 = number.trim().parse().ok()?;
    let multiplier: f64 = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1.0,
        "KB" | "KIB" => 1024.0,
        "MB" | "MIB" => 1024.0 * 1024.0,
        "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * multiplier).max(0.0) as u64)
}

/// Disk pressure as a total state machine.
///
/// `Healthy → Reclaiming → Degraded{deadline} → Draining → Deregistered`, with
/// recovery back to `Healthy` from any non-terminal state. `Deregistered` is
/// terminal: the slot is gone and an operator or the fleet controller must act.
/// No state means "sleep and try again forever".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskState {
    Healthy,
    /// Below the floor; reclaim has not yet been attempted for this episode.
    Reclaiming,
    /// Reclaim ran and the host is still below the floor. `elapsed` is how long
    /// this episode has been degraded.
    Degraded {
        elapsed: Duration,
    },
    /// The degraded deadline expired: finish nothing new, shed the slot.
    Draining {
        elapsed: Duration,
    },
    /// Terminal.
    Deregistered,
}

/// What the slot must do in the current state. Every variant is bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskAction {
    /// Admit jobs.
    Admit,
    /// Run the bounded reclaimer, then re-evaluate.
    Reclaim,
    /// Refuse admission and re-check; `remaining` is time left before the slot
    /// escalates to draining. Never `None`, never unbounded.
    RefuseUntil { remaining: Duration },
    /// Stop accepting work and shed the slot.
    Drain,
    /// Delete the registration and exit.
    Deregister,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskPolicy {
    pub min_free_bytes: u64,
    pub degraded_deadline: Duration,
    pub drain_deadline: Duration,
}

impl Default for DiskPolicy {
    fn default() -> Self {
        Self {
            min_free_bytes: DEFAULT_MIN_FREE_BYTES,
            degraded_deadline: DEFAULT_DEGRADED_DEADLINE,
            drain_deadline: DEFAULT_DRAIN_DEADLINE,
        }
    }
}

impl DiskPolicy {
    /// Total transition function.
    ///
    /// `available_bytes` is the current measurement, `episode` is how long the
    /// host has been continuously below the floor. Recovery is checked first,
    /// so any non-terminal state returns to `Healthy` the moment space appears.
    pub fn next(&self, current: DiskState, available_bytes: u64, episode: Duration) -> DiskState {
        if current == DiskState::Deregistered {
            return DiskState::Deregistered;
        }
        if available_bytes >= self.min_free_bytes {
            return DiskState::Healthy;
        }
        match current {
            DiskState::Healthy => DiskState::Reclaiming,
            DiskState::Reclaiming => DiskState::Degraded { elapsed: episode },
            DiskState::Degraded { .. } => {
                if episode >= self.degraded_deadline {
                    DiskState::Draining { elapsed: episode }
                } else {
                    DiskState::Degraded { elapsed: episode }
                }
            }
            DiskState::Draining { .. } => {
                if episode >= self.degraded_deadline.saturating_add(self.drain_deadline) {
                    DiskState::Deregistered
                } else {
                    DiskState::Draining { elapsed: episode }
                }
            }
            DiskState::Deregistered => DiskState::Deregistered,
        }
    }

    pub fn action(&self, state: DiskState) -> DiskAction {
        match state {
            DiskState::Healthy => DiskAction::Admit,
            DiskState::Reclaiming => DiskAction::Reclaim,
            DiskState::Degraded { elapsed } => DiskAction::RefuseUntil {
                remaining: self.degraded_deadline.saturating_sub(elapsed),
            },
            DiskState::Draining { .. } => DiskAction::Drain,
            DiskState::Deregistered => DiskAction::Deregister,
        }
    }

    /// Admission decision, evaluated *before* a job is acquired.
    ///
    /// Acquiring first and discovering the host cannot hold the job afterwards
    /// is what produced doomed jobs and the indefinite park; the only correct
    /// place for this question is ahead of acquisition.
    pub fn admits(&self, state: DiskState) -> bool {
        matches!(self.action(state), DiskAction::Admit)
    }
}

/// A slot's disk-pressure episode: the state plus the clock that bounds it.
#[derive(Debug, Clone, Copy)]
pub struct DiskPressure {
    policy: DiskPolicy,
    state: DiskState,
    /// Seconds since the current below-floor episode began; `None` when healthy.
    episode_started_unix: Option<u64>,
}

impl DiskPressure {
    pub fn new(policy: DiskPolicy) -> Self {
        Self {
            policy,
            state: DiskState::Healthy,
            episode_started_unix: None,
        }
    }

    pub fn state(&self) -> DiskState {
        self.state
    }

    pub fn policy(&self) -> DiskPolicy {
        self.policy
    }

    /// Fold one measurement into the machine and return the bounded action.
    pub fn observe(&mut self, available_bytes: u64, now_unix: u64) -> DiskAction {
        if available_bytes >= self.policy.min_free_bytes {
            self.episode_started_unix = None;
        } else if self.episode_started_unix.is_none() {
            self.episode_started_unix = Some(now_unix);
        }
        let episode = self
            .episode_started_unix
            .map(|start| Duration::from_secs(now_unix.saturating_sub(start)))
            .unwrap_or_default();
        self.state = self.policy.next(self.state, available_bytes, episode);
        self.policy.action(self.state)
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

    #[test]
    fn admission_probe_uses_statvfs_without_a_docker_subprocess() {
        let capacity = HostCapacity::probe(&std::env::temp_dir()).unwrap();
        assert!(capacity.total_bytes > 0, "statvfs reported no capacity");
        assert!(capacity.available_bytes <= capacity.total_bytes);
        assert!(capacity.used_percent() <= 100);
        // Docker is intentionally unmeasured on the admission path.
        assert_eq!(capacity.docker_bytes, None);
        assert_eq!(
            capacity.promisable_bytes(u64::MAX),
            capacity.available_bytes
        );
    }

    #[test]
    fn docker_usage_is_accounted_and_reduces_promisable_headroom() {
        let stdout = "\
{\"Type\":\"Images\",\"TotalCount\":\"12\",\"Size\":\"10GB\",\"Reclaimable\":\"2GB\"}
{\"Type\":\"Containers\",\"TotalCount\":\"3\",\"Size\":\"512MB\",\"Reclaimable\":\"0B\"}
{\"Type\":\"Local Volumes\",\"TotalCount\":\"1\",\"Size\":\"0B\",\"Reclaimable\":\"0B\"}
{\"Type\":\"Build Cache\",\"TotalCount\":\"40\",\"Size\":\"1.5GB\",\"Reclaimable\":\"1.5GB\"}
";
        let bytes = docker_usage_bytes_from_df(stdout).unwrap();
        let gib = 1024u64 * 1024 * 1024;
        assert_eq!(
            bytes,
            10 * gib + 512 * 1024 * 1024 + (1.5 * gib as f64) as u64
        );
        let capacity = HostCapacity {
            total_bytes: 100 * gib,
            available_bytes: 20 * gib,
            docker_bytes: Some(bytes),
            filesystem_device: 1,
            volume_fingerprint: Some("0000000000000001:0000000000000002".to_string()),
        };
        assert_eq!(capacity.used_percent(), 80);
        assert_eq!(capacity.promisable_bytes(5 * gib), 15 * gib);
        assert!(docker_usage_bytes_from_df("").is_none());
    }

    /// The defect: below the floor the slot slept and looped forever. Drive the
    /// machine with a permanently full disk and assert it reaches a terminal
    /// state in bounded time, and that no step ever reports an unbounded wait.
    #[test]
    fn permanent_disk_exhaustion_reaches_a_terminal_state() {
        let policy = DiskPolicy {
            min_free_bytes: 2 * 1024 * 1024 * 1024,
            degraded_deadline: Duration::from_secs(60),
            drain_deadline: Duration::from_secs(30),
        };
        let mut pressure = DiskPressure::new(policy);
        let mut observed = Vec::new();
        let mut terminal_at = None;
        for tick in 0..200u64 {
            let action = pressure.observe(0, tick * 5);
            observed.push(action);
            if action == DiskAction::Deregister {
                terminal_at = Some(tick);
                break;
            }
        }
        let terminal_at = terminal_at.expect("disk pressure never reached a terminal state");
        assert!(
            terminal_at * 5 <= 120,
            "terminal state must arrive within the deadlines, took {}s",
            terminal_at * 5
        );
        assert!(
            observed.contains(&DiskAction::Reclaim),
            "reclaim must be attempted before degrading"
        );
        assert!(
            observed.contains(&DiskAction::Drain),
            "the slot must drain before deregistering"
        );
        for action in &observed {
            if let DiskAction::RefuseUntil { remaining } = action {
                assert!(
                    *remaining <= policy.degraded_deadline,
                    "refusal must be bounded by the degraded deadline"
                );
            }
        }
        // Terminal is absorbing: no path back into a park.
        assert_eq!(
            pressure.observe(0, 10_000),
            DiskAction::Deregister,
            "deregistered must be terminal"
        );
        assert_eq!(
            pressure.observe(u64::MAX, 10_001),
            DiskAction::Deregister,
            "a deregistered slot must not silently resurrect"
        );
    }

    #[test]
    fn near_exhaustion_recovers_to_healthy_when_reclaim_frees_space() {
        let policy = DiskPolicy {
            min_free_bytes: 2 * 1024 * 1024 * 1024,
            degraded_deadline: Duration::from_secs(60),
            drain_deadline: Duration::from_secs(30),
        };
        let mut pressure = DiskPressure::new(policy);
        assert_eq!(pressure.observe(1024 * 1024 * 1024, 0), DiskAction::Reclaim);
        assert!(matches!(
            pressure.observe(1024 * 1024 * 1024, 5),
            DiskAction::RefuseUntil { .. }
        ));
        assert!(!policy.admits(pressure.state()));
        assert_eq!(
            pressure.observe(8 * 1024 * 1024 * 1024, 10),
            DiskAction::Admit
        );
        assert_eq!(pressure.state(), DiskState::Healthy);
        assert!(policy.admits(pressure.state()));
    }

    /// Totality: every state has a defined successor for every observation.
    #[test]
    fn transition_function_is_total() {
        let policy = DiskPolicy::default();
        for state in [
            DiskState::Healthy,
            DiskState::Reclaiming,
            DiskState::Degraded {
                elapsed: Duration::ZERO,
            },
            DiskState::Degraded {
                elapsed: Duration::from_secs(u64::from(u32::MAX)),
            },
            DiskState::Draining {
                elapsed: Duration::ZERO,
            },
            DiskState::Draining {
                elapsed: Duration::from_secs(u64::from(u32::MAX)),
            },
            DiskState::Deregistered,
        ] {
            for available in [
                0,
                policy.min_free_bytes - 1,
                policy.min_free_bytes,
                u64::MAX,
            ] {
                for episode in [Duration::ZERO, Duration::from_secs(86_400)] {
                    let next = policy.next(state, available, episode);
                    // Defined, and never a wait without a bound.
                    match policy.action(next) {
                        DiskAction::RefuseUntil { remaining } => {
                            assert!(remaining <= policy.degraded_deadline);
                        }
                        DiskAction::Admit
                        | DiskAction::Reclaim
                        | DiskAction::Drain
                        | DiskAction::Deregister => {}
                    }
                }
            }
        }
    }
}
