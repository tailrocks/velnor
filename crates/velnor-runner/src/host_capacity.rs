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
//! carries a deadline, and the terminal state is `Deregistered`. Durable
//! episodes are keyed by canonical service instance and
//! `unix-device:<st_dev>`. A native filesystem UUID binds that runtime device
//! key to one volume incarnation; root-path hashes carry unpinnable roots
//! until their physical identity is known. Config and effective work roots are
//! checked independently, then aliases on one device share one episode.
//! There is no transition back into an unbounded park.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use velnor_control::journal::DiskPressureFilesystemSample;

/// Free-space floor below which a slot must not admit a job.
pub const DEFAULT_MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How long the host may stay degraded (reclaim attempted, still below the
/// floor) before the slot drains instead of parking.
pub const DEFAULT_DEGRADED_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// How long a drain may take before the slot deregisters unconditionally.
pub const DEFAULT_DRAIN_DEADLINE: Duration = Duration::from_secs(5 * 60);

pub const PRESSURE_SERVICE_INSTANCE_ENV: &str = "VELNOR_DISK_PRESSURE_INSTANCE";
pub const PRESSURE_LAUNCH_NONCE_ENV: &str = "VELNOR_DISK_PRESSURE_LAUNCH_NONCE";
pub const PRESSURE_JOURNAL_PATH_ENV: &str = "VELNOR_DISK_PRESSURE_JOURNAL";
pub const PRESSURE_SLOT_ID_ENV: &str = "VELNOR_DISK_PRESSURE_SLOT_ID";
pub const PRESSURE_GENERATION_ENV: &str = "VELNOR_DISK_PRESSURE_GENERATION";
pub const PRESSURE_LAUNCH_LOCK_FD_ENV: &str = "VELNOR_DISK_PRESSURE_LAUNCH_LOCK_FD";

/// Measured capacity of the filesystem holding a Velnor root.
///
/// `available_bytes` is the unprivileged figure (`f_bavail`) — the number that
/// decides whether a job can run — not `f_bfree`, which includes the
/// root-reserved blocks a runner never gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCapacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub filesystem_device: u64,
    /// Episode identity derived from Unix `st_dev`; paths on the same device
    /// share a global `f_bavail` pool and one reclaim claim. Per-directory
    /// quotas are not distinguished.
    pub filesystem_id: String,
    /// Persistent external filesystem UUID from the pinned descriptor. Some
    /// filesystems do not expose one; those samples cannot clear or reclaim.
    pub volume_fingerprint: Option<String>,
    /// Bytes Docker's own storage occupies on this filesystem, when it could be
    /// measured. `None` means unmeasured, which must be treated as unknown
    /// rather than zero.
    pub docker_bytes: Option<u64>,
}

/// A pressure directory pinned to one opened descriptor. Configured symlink
/// aliases are allowed: the alias is resolved before opening, then re-resolved
/// around each sample to reject replacement or retargeting. Capacity and
/// device identity reads use the pinned descriptor for that resolved target.
#[derive(Debug, Clone)]
pub(crate) struct HostCapacityPin {
    requested_path: PathBuf,
    resolved_path: PathBuf,
    descriptor: Arc<File>,
    identity: crate::leftover_disk::FilesystemEntryIdentity,
}

impl HostCapacityPin {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let (resolved_path, descriptor, identity) = open_pressure_directory(path)?;
        let pin = Self {
            requested_path: path.to_path_buf(),
            resolved_path,
            descriptor: Arc::new(descriptor),
            identity,
        };
        pin.revalidate()?;
        Ok(pin)
    }

    pub(crate) fn identity(&self) -> &crate::leftover_disk::FilesystemEntryIdentity {
        &self.identity
    }

    pub(crate) fn device_id(&self) -> u64 {
        self.identity.device
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        let (resolved_path, _descriptor, current) = open_pressure_directory(&self.requested_path)
            .with_context(|| {
            format!(
                "revalidate pinned pressure root {}",
                self.requested_path.display()
            )
        })?;
        if resolved_path != self.resolved_path || current != self.identity {
            anyhow::bail!(
                "pressure root {} changed identity after it was pinned",
                self.requested_path.display()
            );
        }
        Ok(())
    }

    pub(crate) fn probe(&self) -> Result<HostCapacity> {
        self.revalidate()?;
        // Revalidation follows the configured alias. All capacity and UUID
        // reads below stay on this descriptor for its resolved target.
        let before = crate::leftover_disk::filesystem_object_identity(&self.descriptor)?;
        if before != self.identity {
            anyhow::bail!("pinned pressure descriptor identity changed");
        }
        let stat = statvfs_descriptor(&self.descriptor)?;
        let volume_fingerprint = stable_volume_fingerprint(&self.descriptor)?;
        let after = crate::leftover_disk::filesystem_object_identity(&self.descriptor)?;
        if after != self.identity {
            anyhow::bail!("pinned pressure descriptor identity changed during sample");
        }
        self.revalidate()?;
        let block = if stat.fragment_size == 0 {
            stat.block_size.max(1)
        } else {
            stat.fragment_size
        };
        Ok(HostCapacity {
            total_bytes: stat.blocks.saturating_mul(block),
            available_bytes: stat.available_blocks.saturating_mul(block),
            filesystem_device: self.identity.device,
            filesystem_id: format!("unix-device:{:x}", self.identity.device),
            volume_fingerprint,
            docker_bytes: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PressureFilesystem {
    pub root: PathBuf,
    /// Pinned device key used by the durable episode and reclaim claim.
    pub(crate) root_id: String,
    /// Configured-root keys represented by this device; these carry episodes
    /// created while one of the roots was unpinnable.
    pub(crate) root_ids: Vec<String>,
    pub capacity: HostCapacity,
    /// `false` means the root identity was pinned but free capacity was not
    /// measurable. It is persisted as low pressure and keeps admission closed.
    pub measurable: bool,
    /// Every configured root collapsed into this one device episode.
    pub(crate) pins: Vec<Arc<HostCapacityPin>>,
}

impl PressureFilesystem {
    pub(crate) fn revalidate(&self) -> Result<()> {
        for pin in &self.pins {
            pin.revalidate()?;
        }
        Ok(())
    }

    pub(crate) fn primary_pin(&self) -> Option<&HostCapacityPin> {
        self.pins.first().map(Arc::as_ref)
    }
}

#[cfg(test)]
pub(crate) fn with_revalidated_pressure_filesystem<T>(
    filesystem: &PressureFilesystem,
    reclaim: impl FnOnce(Option<&HostCapacityPin>) -> Result<T>,
) -> Result<T> {
    filesystem.revalidate()?;
    reclaim(filesystem.primary_pin())
}

#[derive(Debug, Clone)]
pub struct DurablePressureObservation {
    pub action: DiskAction,
    pub filesystems: Vec<PressureFilesystem>,
    /// Roots whose durable episode claimed its one reclaim before cleanup.
    pub reclaim_filesystems: Vec<PressureFilesystem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmeasurablePressureRoot {
    pub root: PathBuf,
    pub(crate) root_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct PressureRootProbeBatch {
    pub filesystems: Vec<PressureFilesystem>,
    /// Roots that could not be pinned to a stable filesystem identity.
    pub unmeasurable_roots: Vec<UnmeasurablePressureRoot>,
}

impl HostCapacity {
    /// Probe the filesystem holding `path`. The path itself must exist; a
    /// missing leaf fails so callers can record the root unmeasurable.
    ///
    /// Admission uses only the bounded `statvfs` primitive. Docker usage stays
    /// unknown unless a caller supplies an independently bounded measurement
    /// through [`Self::probe_with_docker`].
    pub fn probe(path: &Path) -> Result<Self> {
        Self::probe_with_docker(path, None)
    }

    pub fn probe_with_docker(path: &Path, docker_bytes: Option<u64>) -> Result<Self> {
        let mut capacity = HostCapacityPin::open(path)?.probe()?;
        capacity.docker_bytes = docker_bytes;
        Ok(capacity)
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

/// Filesystems whose free space can block this slot's job admission.
///
/// `disk_space_problem` checks the daemon config root and the work root. Keep
/// that root set shared with the controller so terminal episodes are cleared
/// only after every checked filesystem is healthy.
pub fn pressure_roots(config_base: &Path, work_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![config_base.to_path_buf()];
    let default_work_dir = config_base.join("_work");
    let work_dir = work_dir.unwrap_or(&default_work_dir);
    if work_dir != config_base {
        roots.push(work_dir.to_path_buf());
    }
    roots
}

/// Probe every admission root, folding aliases on the same filesystem into a
/// single durable episode identity.
pub fn probe_pressure_roots(roots: &[PathBuf]) -> Result<PressureRootProbeBatch> {
    let mut filesystems: Vec<PressureFilesystem> = Vec::new();
    let mut unmeasurable_roots = Vec::new();
    if roots.is_empty() {
        anyhow::bail!("disk pressure has no configured filesystem roots");
    }
    for root in roots {
        let pin = match HostCapacityPin::open(root) {
            Ok(pin) => Arc::new(pin),
            Err(error) => {
                unmeasurable_roots.push(UnmeasurablePressureRoot {
                    root: root.clone(),
                    root_id: pressure_root_id(root),
                    reason: format!("pin pressure root: {error:#}"),
                });
                continue;
            }
        };
        match sample_pinned_capacity(pin.as_ref(), HostCapacityPin::probe) {
            Ok((capacity, measurable)) => {
                merge_pressure_filesystem(&mut filesystems, root, capacity, measurable, Some(pin));
            }
            Err(error) => {
                unmeasurable_roots.push(UnmeasurablePressureRoot {
                    root: root.clone(),
                    root_id: pressure_root_id(root),
                    reason: format!("pin identity changed while measuring: {error:#}"),
                });
            }
        }
    }
    Ok(PressureRootProbeBatch {
        filesystems,
        unmeasurable_roots,
    })
}

fn sample_pinned_capacity(
    pin: &HostCapacityPin,
    probe: impl FnOnce(&HostCapacityPin) -> Result<HostCapacity>,
) -> Result<(HostCapacity, bool)> {
    match probe(pin) {
        Ok(capacity) => {
            let measurable = capacity.volume_fingerprint.is_some();
            Ok((capacity, measurable))
        }
        Err(error) => {
            // A failed fstatvfs sample still has a safe durable key only while
            // the configured path resolves to this exact pinned root.
            pin.revalidate().with_context(|| {
                format!("revalidate root after capacity probe failed: {error:#}")
            })?;
            Ok((
                HostCapacity {
                    total_bytes: 0,
                    available_bytes: 0,
                    filesystem_device: pin.device_id(),
                    filesystem_id: format!("unix-device:{:x}", pin.device_id()),
                    volume_fingerprint: stable_volume_fingerprint(&pin.descriptor).ok().flatten(),
                    docker_bytes: None,
                },
                false,
            ))
        }
    }
}

fn merge_pressure_filesystem(
    filesystems: &mut Vec<PressureFilesystem>,
    root: &Path,
    capacity: HostCapacity,
    measurable: bool,
    pin: Option<Arc<HostCapacityPin>>,
) {
    let root_id = capacity.filesystem_id.clone();
    let configured_root_id = pressure_root_id(root);
    if let Some(existing) = filesystems
        .iter_mut()
        .find(|existing| existing.capacity.filesystem_id == capacity.filesystem_id)
    {
        if !measurable {
            existing.measurable = false;
            existing.capacity.total_bytes = 0;
            existing.capacity.available_bytes = 0;
        } else if existing.measurable {
            if existing.capacity.volume_fingerprint != capacity.volume_fingerprint {
                existing.measurable = false;
                existing.capacity.available_bytes = 0;
                existing.capacity.total_bytes = 0;
                existing.capacity.volume_fingerprint = None;
            }
            existing.capacity.available_bytes = existing
                .capacity
                .available_bytes
                .min(capacity.available_bytes);
        }
        if let Some(pin) = pin {
            existing.pins.push(pin);
        }
        if !existing.root_ids.contains(&configured_root_id) {
            existing.root_ids.push(configured_root_id);
        }
    } else {
        filesystems.push(PressureFilesystem {
            root: root.to_path_buf(),
            root_id,
            root_ids: vec![configured_root_id],
            capacity,
            measurable,
            pins: pin.into_iter().collect(),
        });
    }
}

#[cfg(test)]
fn probe_pressure_roots_with(
    roots: &[PathBuf],
    mut probe: impl FnMut(&Path) -> Result<HostCapacity>,
) -> Result<Vec<PressureFilesystem>> {
    probe_pressure_roots_with_status(roots, |root| probe(root).map(|capacity| (capacity, true)))
}

#[cfg(test)]
fn probe_pressure_roots_with_status(
    roots: &[PathBuf],
    mut probe: impl FnMut(&Path) -> Result<(HostCapacity, bool)>,
) -> Result<Vec<PressureFilesystem>> {
    let mut filesystems: Vec<PressureFilesystem> = Vec::new();
    for root in roots {
        let (capacity, measurable) = probe(root)
            .with_context(|| format!("measure disk pressure root {}", root.display()))?;
        merge_pressure_filesystem(&mut filesystems, root, capacity, measurable, None);
    }
    if filesystems.is_empty() {
        anyhow::bail!("disk pressure has no configured filesystem roots");
    }
    Ok(filesystems)
}

/// Preserve both free-space and high-utilization pressure as one bounded
/// filesystem episode. `u64::MAX` is a sentinel floor for the 90%-used guard;
/// controller and worker derive it from the same measured capacity.
pub fn pressure_floor_bytes(capacity: &HostCapacity, min_free_bytes: u64) -> u64 {
    if capacity.used_percent() >= crate::leftover_disk::HARD_PRESSURE_PERCENT {
        u64::MAX
    } else {
        min_free_bytes
    }
}

fn existing_ancestor(path: &Path) -> Result<&Path> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        match fs::metadata(current) {
            Ok(_) => return Ok(current),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = current.parent();
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect pressure root ancestor {}", current.display())
                });
            }
        }
    }
    anyhow::bail!("pressure root {} has no existing ancestor", path.display())
}

fn open_pressure_directory(
    requested_path: &Path,
) -> Result<(PathBuf, File, crate::leftover_disk::FilesystemEntryIdentity)> {
    // Resolve configured aliases, including macOS /var -> /private/var, before
    // opening. The descriptor pins that target; revalidation rejects retargets.
    // A missing leaf must fail here (callers record it unmeasurable) rather
    // than silently pinning the parent filesystem.
    let probe = existing_ancestor(requested_path)?;
    if probe != requested_path {
        anyhow::bail!("pressure root {} does not exist", requested_path.display());
    }
    let resolved_path = fs::canonicalize(probe)
        .with_context(|| format!("resolve pressure root ancestor {}", probe.display()))?;
    let descriptor = open_directory_descriptor(&resolved_path)
        .with_context(|| format!("pin pressure root ancestor {}", resolved_path.display()))?;
    if !descriptor.metadata()?.is_dir() {
        anyhow::bail!(
            "pressure root ancestor {} is not a directory",
            resolved_path.display()
        );
    }
    let identity = crate::leftover_disk::filesystem_object_identity(&descriptor)?;
    Ok((resolved_path, descriptor, identity))
}

#[cfg(unix)]
fn open_directory_descriptor(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("open directory without following links: {}", path.display()))
}

#[cfg(not(unix))]
fn open_directory_descriptor(path: &Path) -> Result<File> {
    File::open(path).with_context(|| format!("open pressure root: {}", path.display()))
}

struct PressureStat {
    blocks: u64,
    available_blocks: u64,
    fragment_size: u64,
    block_size: u64,
}

#[cfg(unix)]
// statvfs counters are narrower on macOS; the widening conversions are load-bearing there.
#[allow(clippy::useless_conversion)]
fn statvfs_descriptor(descriptor: &File) -> Result<PressureStat> {
    use std::os::fd::AsRawFd as _;

    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: descriptor remains live and stats is writable storage for libc.
    if unsafe { libc::fstatvfs(descriptor.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("fstatvfs pinned pressure root");
    }
    // SAFETY: fstatvfs initialized stats on success.
    let stats = unsafe { stats.assume_init() };
    Ok(PressureStat {
        blocks: u64::from(stats.f_blocks),
        available_blocks: u64::from(stats.f_bavail),
        fragment_size: stats.f_frsize,
        block_size: stats.f_bsize,
    })
}

#[cfg(not(unix))]
fn statvfs_descriptor(_descriptor: &File) -> Result<PressureStat> {
    anyhow::bail!("pinned filesystem capacity probes require Unix fstatvfs")
}

fn stable_volume_fingerprint(descriptor: &File) -> Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd as _;

        #[repr(C)]
        struct FsUuid2 {
            len: u8,
            uuid: [u8; 16],
        }
        let mut fs_uuid = FsUuid2 {
            len: 0,
            uuid: [0; 16],
        };
        // Linux UAPI: FS_IOC_GETFSUUID = _IOR(0x15, 0, struct fsuuid2).
        // Unsupported kernels/filesystems produce no fingerprint and remain
        // fail-closed; st_dev and mount IDs are only runtime path guards.
        const FS_IOC_GETFSUUID: libc::c_ulong = 0x8011_1500;
        // SAFETY: ioctl receives a valid descriptor and initialized output
        // storage with the exact Linux UAPI fsuuid2 layout.
        let result = unsafe {
            libc::ioctl(
                descriptor.as_raw_fd(),
                FS_IOC_GETFSUUID,
                &mut fs_uuid as *mut FsUuid2,
            )
        };
        if result != 0 || fs_uuid.len == 0 || usize::from(fs_uuid.len) > fs_uuid.uuid.len() {
            return Ok(None);
        }
        Ok(Some(format_volume_uuid(
            &fs_uuid.uuid[..usize::from(fs_uuid.len)],
        )))
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd as _;

        // getattrlist(2) requires ATTR_VOL_INFO for every volume attribute;
        // it is a selector requirement and contributes no output bytes.
        const ATTR_VOL_INFO: u32 = 0x8000_0000;
        const ATTR_VOL_UUID: u32 = 0x0004_0000;
        const UUID_LENGTH: usize = 16;
        let mut attrs = libc::attrlist {
            bitmapcount: 5,
            reserved: 0,
            commonattr: 0,
            volattr: ATTR_VOL_INFO | ATTR_VOL_UUID,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let mut buffer = [0u8; 64];
        // SAFETY: fgetattrlist writes into this live buffer and receives a
        // descriptor opened on the pinned filesystem directory.
        let result = unsafe {
            libc::fgetattrlist(
                descriptor.as_raw_fd(),
                (&mut attrs as *mut libc::attrlist).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
            )
        };
        if result != 0 {
            return Ok(None);
        }
        let returned = u32::from_ne_bytes(buffer[0..4].try_into().unwrap_or_default()) as usize;
        let uuid_start = std::mem::size_of::<u32>();
        let uuid_end = uuid_start + UUID_LENGTH;
        if returned < uuid_end || returned > buffer.len() {
            return Ok(None);
        }
        // getattrlist starts with its u32 returned length. ATTR_VOL_INFO is
        // required in the selector but has no payload; ATTR_VOL_UUID follows.
        let uuid = &buffer[uuid_start..uuid_end];
        if uuid.iter().all(|byte| *byte == 0) {
            return Ok(None);
        }
        Ok(Some(format_volume_uuid(uuid)))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = descriptor;
        Ok(None)
    }
}

fn format_volume_uuid(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing to String cannot fail.
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub(crate) fn pressure_root_id(root: &Path) -> String {
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        hasher.update(root.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    hasher.update(root.to_string_lossy().as_bytes());
    format!("root:{}", format_volume_uuid(&hasher.finalize()))
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

    fn action_from_durable_observation(
        &mut self,
        observation: velnor_control::journal::DiskPressureObservation,
        now_unix: u64,
    ) -> DiskAction {
        let reclaim_needed = observation.reclaim_needed;
        let Some(episode) = observation.episode else {
            self.episode_started_unix = None;
            self.state = DiskState::Healthy;
            return DiskAction::Admit;
        };
        self.episode_started_unix = Some(episode.started_unix);
        let elapsed = Duration::from_secs(now_unix.saturating_sub(episode.started_unix));
        if episode.terminal {
            self.state = DiskState::Deregistered;
            DiskAction::Deregister
        } else if episode.draining {
            self.state = DiskState::Draining { elapsed };
            DiskAction::Drain
        } else if reclaim_needed {
            self.state = DiskState::Reclaiming;
            DiskAction::Reclaim
        } else {
            self.state = DiskState::Degraded { elapsed };
            DiskAction::RefuseUntil {
                remaining: Duration::from_secs(episode.deadline_unix.saturating_sub(now_unix)),
            }
        }
    }
}

#[derive(Debug)]
enum PressureLaunchContext {
    Managed {
        service_instance: String,
        launch_nonce: String,
    },
    Unmanaged,
    Invalid,
}

/// Partial launch identity cannot safely select either local or durable state.
#[derive(Debug)]
pub(crate) struct InvalidDiskPressureLaunchIdentity;

impl std::fmt::Display for InvalidDiskPressureLaunchIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("incomplete durable disk-pressure launch identity")
    }
}

impl std::error::Error for InvalidDiskPressureLaunchIdentity {}

/// Worker-owned bridge from capacity probes into the durable journal episode.
/// The controller supplies an instance key and one fresh nonce per process
/// launch; absence of either fails closed when measured free space is low.
pub struct DurableDiskPressure {
    service_instance: Option<String>,
    launch_nonce: Option<String>,
    journal_path: PathBuf,
    journal: Option<velnor_control::journal::Journal>,
}

impl DurableDiskPressure {
    pub fn from_environment(journal_path: impl Into<PathBuf>) -> Self {
        Self {
            service_instance: std::env::var(PRESSURE_SERVICE_INSTANCE_ENV).ok(),
            launch_nonce: std::env::var(PRESSURE_LAUNCH_NONCE_ENV).ok(),
            journal_path: std::env::var_os(PRESSURE_JOURNAL_PATH_ENV)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| journal_path.into()),
            journal: None,
        }
    }

    fn launch_context(&self) -> PressureLaunchContext {
        match (self.service_instance.as_ref(), self.launch_nonce.as_ref()) {
            (Some(service_instance), Some(launch_nonce))
                if !service_instance.trim().is_empty() && !launch_nonce.trim().is_empty() =>
            {
                PressureLaunchContext::Managed {
                    service_instance: service_instance.clone(),
                    launch_nonce: launch_nonce.clone(),
                }
            }
            (None, None) => PressureLaunchContext::Unmanaged,
            _ => PressureLaunchContext::Invalid,
        }
    }

    pub fn observe(
        &mut self,
        pressure: &mut DiskPressure,
        root: &Path,
        slot_id: &velnor_model::SlotId,
        generation: velnor_model::Generation,
        now_unix: u64,
    ) -> anyhow::Result<DiskAction> {
        Ok(self
            .observe_roots(
                pressure,
                &[root.to_path_buf()],
                slot_id,
                generation,
                now_unix,
            )?
            .action)
    }

    pub fn observe_roots(
        &mut self,
        pressure: &mut DiskPressure,
        roots: &[PathBuf],
        slot_id: &velnor_model::SlotId,
        generation: velnor_model::Generation,
        now_unix: u64,
    ) -> anyhow::Result<DurablePressureObservation> {
        let batch = probe_pressure_roots(roots)?;
        self.observe_filesystems(
            pressure,
            batch.filesystems,
            batch.unmeasurable_roots,
            slot_id,
            generation,
            now_unix,
        )
    }

    fn observe_filesystems(
        &mut self,
        pressure: &mut DiskPressure,
        filesystems: Vec<PressureFilesystem>,
        unmeasurable_roots: Vec<UnmeasurablePressureRoot>,
        slot_id: &velnor_model::SlotId,
        generation: velnor_model::Generation,
        now_unix: u64,
    ) -> anyhow::Result<DurablePressureObservation> {
        let launch_context = self.launch_context();
        if matches!(launch_context, PressureLaunchContext::Invalid) {
            return Err(InvalidDiskPressureLaunchIdentity.into());
        }
        for filesystem in &filesystems {
            filesystem.revalidate()?;
        }
        let min_free_bytes = pressure.policy.min_free_bytes;
        let filesystem_is_healthy = |filesystem: &PressureFilesystem| {
            filesystem.measurable
                && filesystem.capacity.volume_fingerprint.is_some()
                && filesystem.capacity.available_bytes
                    >= pressure_floor_bytes(&filesystem.capacity, min_free_bytes)
        };
        let healthy = filesystems.iter().all(&filesystem_is_healthy)
            && unmeasurable_roots.is_empty()
            && !filesystems.is_empty();
        let (service_instance, launch_nonce) = match launch_context {
            PressureLaunchContext::Unmanaged => {
                let reclaim_filesystems = if healthy {
                    Vec::new()
                } else {
                    filesystems
                        .iter()
                        .filter(|filesystem| !filesystem_is_healthy(filesystem))
                        .cloned()
                        .collect()
                };
                let action = pressure.observe(if healthy { min_free_bytes } else { 0 }, now_unix);
                return Ok(DurablePressureObservation {
                    action,
                    filesystems,
                    reclaim_filesystems: if action == DiskAction::Reclaim {
                        reclaim_filesystems
                    } else {
                        Vec::new()
                    },
                });
            }
            PressureLaunchContext::Managed {
                service_instance,
                launch_nonce,
            } => (service_instance, launch_nonce),
            PressureLaunchContext::Invalid => {
                return Err(InvalidDiskPressureLaunchIdentity.into());
            }
        };

        if self.journal.is_none() {
            self.journal = Some(
                velnor_control::journal::Journal::open_for_launch(
                    &self.journal_path,
                    &service_instance,
                    slot_id,
                    generation,
                    &launch_nonce,
                )
                .context("durable pressure journal unavailable")?,
            );
        }
        let journal = self
            .journal
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("durable pressure journal unavailable"))?;
        let mut samples = Vec::new();
        for filesystem in &filesystems {
            let identity_measurable =
                filesystem.measurable && filesystem.capacity.volume_fingerprint.is_some();
            samples.push(DiskPressureFilesystemSample {
                alias_ids: filesystem.root_ids.clone(),
                filesystem_id: filesystem.root_id.clone(),
                available_bytes: identity_measurable.then_some(filesystem.capacity.available_bytes),
                min_free_bytes: pressure_floor_bytes(
                    &filesystem.capacity,
                    pressure.policy.min_free_bytes,
                ),
                volume_fingerprint: filesystem.capacity.volume_fingerprint.clone(),
            });
        }
        samples.extend(
            unmeasurable_roots
                .iter()
                .map(|root| DiskPressureFilesystemSample {
                    filesystem_id: root.root_id.clone(),
                    alias_ids: Vec::new(),
                    available_bytes: None,
                    min_free_bytes: pressure.policy.min_free_bytes,
                    volume_fingerprint: None,
                }),
        );
        for filesystem in &filesystems {
            filesystem.revalidate()?;
        }
        let observations = journal.observe_disk_pressure_roots(
            &service_instance,
            slot_id,
            generation,
            &launch_nonce,
            &samples,
            pressure.policy.degraded_deadline.as_secs(),
            pressure.policy.drain_deadline.as_secs(),
            now_unix,
        )?;
        let mut actions = Vec::with_capacity(samples.len());
        let mut reclaim_filesystems = Vec::new();
        for (filesystem_id, observation) in observations {
            let action = pressure.action_from_durable_observation(observation, now_unix);
            if action == DiskAction::Reclaim
                && let Some(filesystem) = filesystems
                    .iter()
                    .find(|filesystem| filesystem.root_id == filesystem_id)
            {
                reclaim_filesystems.push(filesystem.clone());
            }
            actions.push(action);
        }
        Ok(DurablePressureObservation {
            action: combine_pressure_actions(actions),
            filesystems,
            reclaim_filesystems,
        })
    }
}

fn combine_pressure_actions(actions: Vec<DiskAction>) -> DiskAction {
    if actions.contains(&DiskAction::Deregister) {
        return DiskAction::Deregister;
    }
    if actions.contains(&DiskAction::Drain) {
        return DiskAction::Drain;
    }
    if actions.contains(&DiskAction::Reclaim) {
        return DiskAction::Reclaim;
    }
    actions
        .into_iter()
        .filter_map(|action| match action {
            DiskAction::RefuseUntil { remaining } => Some(remaining),
            DiskAction::Admit
            | DiskAction::Reclaim
            | DiskAction::Drain
            | DiskAction::Deregister => None,
        })
        .min()
        .map(|remaining| DiskAction::RefuseUntil { remaining })
        .unwrap_or(DiskAction::Admit)
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

    struct TestTempDir(PathBuf);

    impl TestTempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "velnor-host-capacity-{label}-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn admission_probe_uses_statvfs_without_a_docker_subprocess() {
        let capacity = HostCapacity::probe(&std::env::temp_dir()).unwrap();
        assert!(capacity.total_bytes > 0, "statvfs reported no capacity");
        assert!(capacity.available_bytes <= capacity.total_bytes);
        assert!(!capacity.filesystem_id.is_empty());
        assert!(capacity.used_percent() <= 100);
        // Docker is intentionally unmeasured on the admission path.
        assert_eq!(capacity.docker_bytes, None);
        assert_eq!(
            capacity.promisable_bytes(u64::MAX),
            capacity.available_bytes
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_root_probe_reads_a_well_formed_volume_uuid() {
        assert_eq!(std::mem::size_of::<libc::attrlist>(), 24);
        assert_eq!(std::mem::offset_of!(libc::attrlist, volattr), 8);

        let pin = HostCapacityPin::open(Path::new("/")).unwrap();
        let fingerprint = stable_volume_fingerprint(&pin.descriptor)
            .unwrap()
            .expect("macOS root volume exposes ATTR_VOL_UUID");
        assert_eq!(fingerprint.len(), 32);
        assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));

        let capacity = pin.probe().unwrap();
        assert_eq!(
            capacity.volume_fingerprint.as_deref(),
            Some(fingerprint.as_str())
        );
    }

    #[test]
    fn replacing_pressure_root_rejects_probe_identity_before_reclaim() {
        let temp = TestTempDir::new("identity");
        let root = temp.path().join("configured-root");
        let moved = temp.path().join("original-root");
        std::fs::create_dir(&root).unwrap();
        let batch = probe_pressure_roots(std::slice::from_ref(&root)).unwrap();
        let filesystems = batch.filesystems;
        assert!(batch.unmeasurable_roots.is_empty());

        std::fs::rename(&root, &moved).unwrap();
        std::fs::create_dir(&root).unwrap();

        assert!(filesystems[0].primary_pin().unwrap().probe().is_err());
        let mut reclaim_started = false;
        let result = with_revalidated_pressure_filesystem(&filesystems[0], |_pin| {
            reclaim_started = true;
            Ok(())
        });
        assert!(result.is_err());
        assert!(!reclaim_started);
    }

    #[cfg(unix)]
    #[test]
    fn configured_symlink_alias_is_pinned_and_retarget_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = TestTempDir::new("symlink-alias");
        let target = temp.path().join("target");
        let replacement = temp.path().join("replacement");
        let alias = temp.path().join("configured-alias");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&replacement).unwrap();
        symlink(&target, &alias).unwrap();

        let pin = HostCapacityPin::open(&alias).unwrap();
        assert_eq!(pin.resolved_path, std::fs::canonicalize(&target).unwrap());
        assert!(pin.revalidate().is_ok());
        assert_eq!(pin.probe().unwrap().filesystem_device, pin.device_id());

        std::fs::remove_file(&alias).unwrap();
        symlink(&replacement, &alias).unwrap();
        assert!(pin.revalidate().is_err());
        assert!(pin.probe().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn pressure_root_open_rejects_fifo_and_regular_file() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;

        let temp = TestTempDir::new("not-directory");
        let fifo = temp.path().join("pressure-fifo");
        let fifo_cstr = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is a valid NUL-terminated string and mode is valid.
        assert_eq!(unsafe { libc::mkfifo(fifo_cstr.as_ptr(), 0o600) }, 0);
        assert!(HostCapacityPin::open(&fifo).is_err());

        let regular_file = temp.path().join("pressure-file");
        std::fs::write(&regular_file, b"not a directory").unwrap();
        assert!(HostCapacityPin::open(&regular_file).is_err());
    }

    #[test]
    fn config_and_work_roots_keep_distinct_filesystem_pressure() {
        let config_base = PathBuf::from("/daemon/config");
        let work_dir = PathBuf::from("/scratch/work");
        assert_eq!(
            pressure_roots(&config_base, None),
            vec![config_base.clone(), config_base.join("_work")]
        );
        assert_eq!(
            pressure_roots(&config_base, Some(&config_base)),
            vec![config_base.clone()]
        );
        let roots = pressure_roots(&config_base, Some(&work_dir));
        assert_eq!(roots, vec![config_base.clone(), work_dir.clone()]);

        let filesystems = probe_pressure_roots_with(&roots, |root| {
            let (device, available_bytes) = if root == config_base.as_path() {
                (1, 0)
            } else {
                (2, 8 * 1024 * 1024 * 1024)
            };
            Ok(HostCapacity {
                total_bytes: 16 * 1024 * 1024 * 1024,
                available_bytes,
                filesystem_device: device,
                filesystem_id: format!("unix-device:{device:x}"),
                volume_fingerprint: Some(format!("volume-{device}")),
                docker_bytes: None,
            })
        })
        .unwrap();

        assert_eq!(filesystems.len(), 2);
        assert_eq!(filesystems[0].root, config_base);
        assert_eq!(filesystems[0].root_id, "unix-device:1");
        assert_eq!(
            filesystems[0].root_ids,
            vec![pressure_root_id(Path::new("/daemon/config"))]
        );
        assert_eq!(filesystems[0].capacity.filesystem_id, "unix-device:1");
        assert_eq!(filesystems[0].capacity.available_bytes, 0);
        assert_eq!(filesystems[1].root, work_dir);
        assert_eq!(filesystems[1].capacity.filesystem_id, "unix-device:2");
        assert!(filesystems[1].capacity.available_bytes > DEFAULT_MIN_FREE_BYTES);
    }

    #[test]
    fn unmeasurable_config_root_keeps_its_device_episode_separate_from_work() {
        let config_base = PathBuf::from("/daemon/config");
        let work_dir = PathBuf::from("/scratch/work");
        let roots = pressure_roots(&config_base, Some(&work_dir));
        let filesystems = probe_pressure_roots_with_status(&roots, |root| {
            let (device, total_bytes, available_bytes, measurable) =
                if root == config_base.as_path() {
                    (1, 0, 0, false)
                } else {
                    (2, 16 * 1024 * 1024 * 1024, 8 * 1024 * 1024 * 1024, true)
                };
            Ok((
                HostCapacity {
                    total_bytes,
                    available_bytes,
                    filesystem_device: device,
                    filesystem_id: format!("unix-device:{device:x}"),
                    volume_fingerprint: Some(format!("volume-{device}")),
                    docker_bytes: None,
                },
                measurable,
            ))
        })
        .unwrap();

        assert_eq!(filesystems.len(), 2);
        assert_eq!(filesystems[0].capacity.filesystem_id, "unix-device:1");
        assert_eq!(filesystems[0].capacity.available_bytes, 0);
        assert!(!filesystems[0].measurable);
        assert_eq!(filesystems[1].capacity.filesystem_id, "unix-device:2");
        assert!(filesystems[1].capacity.available_bytes > DEFAULT_MIN_FREE_BYTES);
        assert!(filesystems[1].measurable);
    }

    #[test]
    fn failed_capacity_sample_persists_only_while_pinned_path_identity_matches() {
        let temp = TestTempDir::new("sample");
        let root = temp.path().join("pressure-root");
        std::fs::create_dir(&root).unwrap();
        let pin = HostCapacityPin::open(&root).unwrap();
        let expected_device = pin.device_id();

        let (capacity, measurable) =
            sample_pinned_capacity(&pin, |_| anyhow::bail!("injected fstatvfs failure")).unwrap();

        assert!(!measurable);
        assert_eq!(capacity.filesystem_device, expected_device);
        assert_eq!(
            capacity.filesystem_id,
            format!("unix-device:{expected_device:x}")
        );
        assert_eq!(capacity.available_bytes, 0);
    }

    #[test]
    fn unpinnable_config_root_does_not_discard_measurable_work_root() {
        let temp = TestTempDir::new("unmeasurable");
        let missing_config = temp.path().join("missing-config");
        let work = temp.path().join("work");
        std::fs::create_dir(&work).unwrap();

        let batch = probe_pressure_roots(&[missing_config.clone(), work.clone()]).unwrap();

        assert_eq!(batch.unmeasurable_roots.len(), 1);
        assert_eq!(batch.unmeasurable_roots[0].root, missing_config);
        assert_eq!(batch.filesystems.len(), 1);
        assert_eq!(batch.filesystems[0].root, work);
        assert!(batch.filesystems[0].measurable);
    }

    #[test]
    fn pressure_roots_on_same_filesystem_share_one_identity() {
        let roots = vec![
            PathBuf::from("/daemon/config"),
            PathBuf::from("/daemon/work"),
        ];
        let filesystems = probe_pressure_roots_with(&roots, |root| {
            let available_bytes = if root.ends_with("work") { 1_000 } else { 2_000 };
            Ok(HostCapacity {
                total_bytes: 10_000,
                available_bytes,
                filesystem_device: 7,
                filesystem_id: "unix-device:7".to_owned(),
                volume_fingerprint: Some("volume-7".to_owned()),
                docker_bytes: None,
            })
        })
        .unwrap();
        assert_eq!(filesystems.len(), 1);
        assert_eq!(filesystems[0].capacity.available_bytes, 1_000);
        assert_eq!(filesystems[0].root_id, "unix-device:7");
        assert_eq!(filesystems[0].root_ids.len(), 2);
    }

    #[test]
    fn unmanaged_pressure_progresses_across_all_roots() {
        let gib = 1024u64 * 1024 * 1024;
        let filesystems = vec![
            PressureFilesystem {
                root: PathBuf::from("/daemon/config"),
                root_id: "unix-device:1".to_owned(),
                root_ids: vec![pressure_root_id(Path::new("/daemon/config"))],
                capacity: HostCapacity {
                    total_bytes: 100 * gib,
                    available_bytes: 20 * gib,
                    filesystem_device: 1,
                    filesystem_id: "unix-device:1".to_owned(),
                    volume_fingerprint: Some("volume-1".to_owned()),
                    docker_bytes: None,
                },
                measurable: true,
                pins: Vec::new(),
            },
            PressureFilesystem {
                root: PathBuf::from("/scratch/work"),
                root_id: "unix-device:2".to_owned(),
                root_ids: vec![pressure_root_id(Path::new("/scratch/work"))],
                capacity: HostCapacity {
                    total_bytes: 100 * gib,
                    available_bytes: 0,
                    filesystem_device: 2,
                    filesystem_id: "unix-device:2".to_owned(),
                    volume_fingerprint: Some("volume-2".to_owned()),
                    docker_bytes: None,
                },
                measurable: true,
                pins: Vec::new(),
            },
        ];
        let slot_id = velnor_model::SlotId("scope-1".to_owned());
        let generation = velnor_model::Generation(1);
        let policy = DiskPolicy {
            min_free_bytes: 2 * gib,
            degraded_deadline: Duration::from_secs(10),
            drain_deadline: Duration::from_secs(5),
        };
        let mut pressure = DiskPressure::new(policy);
        let mut unmanaged = DurableDiskPressure {
            service_instance: None,
            launch_nonce: None,
            journal_path: PathBuf::from("/unused/journal.db"),
            journal: None,
        };
        let observation = unmanaged
            .observe_filesystems(
                &mut pressure,
                filesystems.clone(),
                Vec::new(),
                &slot_id,
                generation,
                100,
            )
            .unwrap();
        assert_eq!(observation.action, DiskAction::Reclaim);
        assert_eq!(observation.reclaim_filesystems.len(), 1);
        assert_eq!(
            observation.reclaim_filesystems[0].root,
            PathBuf::from("/scratch/work")
        );

        let observation = unmanaged
            .observe_filesystems(
                &mut pressure,
                filesystems.clone(),
                Vec::new(),
                &slot_id,
                generation,
                105,
            )
            .unwrap();
        assert_eq!(
            observation.action,
            DiskAction::RefuseUntil {
                remaining: Duration::from_secs(5)
            }
        );

        let observation = unmanaged
            .observe_filesystems(
                &mut pressure,
                filesystems.clone(),
                Vec::new(),
                &slot_id,
                generation,
                110,
            )
            .unwrap();
        assert_eq!(observation.action, DiskAction::Drain);

        let observation = unmanaged
            .observe_filesystems(
                &mut pressure,
                filesystems,
                Vec::new(),
                &slot_id,
                generation,
                115,
            )
            .unwrap();
        assert_eq!(observation.action, DiskAction::Deregister);
    }

    #[test]
    fn incomplete_or_blank_launch_identity_is_a_typed_error() {
        for (service_instance, launch_nonce) in [
            (Some("service-one"), None),
            (Some("service-one"), Some("")),
            (Some(""), Some("")),
            (Some(" "), Some("\t")),
        ] {
            let mut durable = DurableDiskPressure {
                service_instance: service_instance.map(str::to_owned),
                launch_nonce: launch_nonce.map(str::to_owned),
                journal_path: PathBuf::from("/unused/journal.db"),
                journal: None,
            };
            let mut pressure = DiskPressure::new(DiskPolicy::default());
            let error = durable
                .observe_filesystems(
                    &mut pressure,
                    Vec::new(),
                    Vec::new(),
                    &velnor_model::SlotId("scope-1".to_owned()),
                    velnor_model::Generation(1),
                    100,
                )
                .unwrap_err();

            assert!(error
                .downcast_ref::<InvalidDiskPressureLaunchIdentity>()
                .is_some());
            assert_eq!(pressure.state(), DiskState::Healthy);
        }
    }

    #[test]
    fn durable_journal_failure_remains_an_error() {
        let gib = 1024u64 * 1024 * 1024;
        let filesystems = vec![PressureFilesystem {
            root: PathBuf::from("/daemon/config"),
            root_id: "unix-device:1".to_owned(),
            root_ids: vec![pressure_root_id(Path::new("/daemon/config"))],
            capacity: HostCapacity {
                total_bytes: 100 * gib,
                available_bytes: 0,
                filesystem_device: 1,
                filesystem_id: "unix-device:1".to_owned(),
                volume_fingerprint: Some("volume-1".to_owned()),
                docker_bytes: None,
            },
            measurable: true,
            pins: Vec::new(),
        }];
        let mut pressure = DiskPressure::new(DiskPolicy::default());
        let missing_parent = std::env::temp_dir().join(format!(
            "velnor-pressure-missing-journal-{}",
            uuid::Uuid::new_v4()
        ));
        let mut missing_journal = DurableDiskPressure {
            service_instance: Some("service-one".to_owned()),
            launch_nonce: Some("launch-one".to_owned()),
            journal_path: missing_parent.join("journal.db"),
            journal: None,
        };
        let error = missing_journal
            .observe_filesystems(
                &mut pressure,
                filesystems,
                Vec::new(),
                &velnor_model::SlotId("scope-1".to_owned()),
                velnor_model::Generation(1),
                100,
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("durable pressure journal unavailable"));
    }

    #[test]
    fn replaced_launch_fence_survives_journal_open_context() {
        use velnor_control::journal::{Event, Journal};
        use velnor_model::{Generation, SlotId};

        let temp = TestTempDir::new("stale-launch-open");
        let journal_path = temp.path().join("journal.db");
        let service_instance = std::fs::canonicalize(temp.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let slot_id = SlotId("scope-1".to_owned());
        let generation = Generation(1);
        let mut journal =
            Journal::open_for_service_instance(&journal_path, &service_instance).unwrap();
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ExecutorProven {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::SessionLive {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::RegistrationIntended {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::Registered {
                slot_id: slot_id.clone(),
                generation,
            },
            Event::ReadyAttempt {
                slot_id: slot_id.clone(),
                generation,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let stale_nonce = journal
            .issue_disk_pressure_launch(&service_instance, &slot_id, generation, 1)
            .unwrap();
        let _current_nonce = journal
            .issue_disk_pressure_launch(&service_instance, &slot_id, generation, 2)
            .unwrap();
        drop(journal);

        let mut durable = DurableDiskPressure {
            service_instance: Some(service_instance),
            launch_nonce: Some(stale_nonce),
            journal_path,
            journal: None,
        };
        let mut pressure = DiskPressure::new(DiskPolicy::default());
        let error = durable
            .observe_filesystems(
                &mut pressure,
                Vec::new(),
                Vec::new(),
                &slot_id,
                generation,
                100,
            )
            .unwrap_err();

        let store_error = error
            .downcast_ref::<velnor_control::store::error::StoreError>()
            .expect("launch fence StoreError remains available through context");
        assert_eq!(
            store_error.envelope.reason,
            "journal.disk_pressure.launch.fenced"
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
            filesystem_device: 42,
            filesystem_id: "unix-device:2a".to_owned(),
            volume_fingerprint: Some("volume-42".to_owned()),
            docker_bytes: Some(bytes),
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
