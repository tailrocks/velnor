//! Leftover-after-Velnor disposable disk reclaim.
//!
//! Job UUID work trees outlive jobs because GC can scan a different root than
//! daemons use. Ordinary hard pressure (90% used) reclaims only idle leftover
//! workspaces and may prune dangling images when authorized. The separate
//! emergency free-space path can reclaim eligible cache classes and scoped
//! builder cache; it does not use this module's broad Docker prune commands.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const HARD_PRESSURE_PERCENT: u8 = 90;
pub const LIVE_JOB_NAME_PREFIX: &str = "velnor-job-";

const RECLAIM_QUARANTINE_PREFIX: &str = ".velnor-reclaim-";
const RECLAIM_QUARANTINE_ENTRY: &str = "entry";
const RECLAIM_QUARANTINE_JOURNAL: &str = "transaction";
const RECLAIM_QUARANTINE_JOURNAL_TEMP: &str = "transaction.tmp";

/// A job workspace untouched for less than this is presumed live regardless of
/// any other evidence.
///
/// `docker ps` alone was the liveness test, so a job was invisible to the
/// reaper during checkout, artifact upload, target publish, and deferred
/// BuildKit teardown — every window in which no job container is running but
/// the workspace is still being read and written.
pub const WORKSPACE_MIN_IDLE: Duration = Duration::from_secs(30 * 60);

/// How long a scope lease may go unrefreshed before it is treated as dead.
const LEASE_STALE_AFTER: Duration = Duration::from_secs(24 * 3600);

/// Every source of evidence that a job workspace is live.
///
/// Liveness is a disjunction and it fails closed: if a source cannot be read,
/// nothing it would have protected is deleted.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceLiveness {
    /// Job ids with a running container (`docker ps`).
    pub running: BTreeSet<String>,
    /// Job ids named by an active store lease. A job holds these from before
    /// its first step until after its last publish.
    pub leased: BTreeSet<String>,
    /// Job ids whose host job-claim lock is still held. This covers the window
    /// before any lease exists — notably checkout.
    pub claimed: BTreeSet<String>,
    /// Workspaces modified more recently than this are presumed live.
    pub min_idle: Duration,
    /// Set when an evidence source could not be read. The reaper must delete
    /// nothing while true.
    pub evidence_incomplete: bool,
}

impl WorkspaceLiveness {
    /// Collect every source of evidence for one runtime root.
    ///
    /// The caller must already hold the filesystem coordinator, so that no
    /// daemon can publish a lease between this snapshot and the deletions it
    /// authorizes.
    pub fn collect(run_root: &Path, running: BTreeSet<String>) -> Self {
        let mut liveness = Self {
            running,
            min_idle: WORKSPACE_MIN_IDLE,
            ..Self::default()
        };
        match crate::capacity::active_scope_leases(run_root, LEASE_STALE_AFTER) {
            Ok(leases) => {
                liveness.leased = leases
                    .into_iter()
                    .filter_map(|lease| lease.owner_job_id)
                    .map(|owner| owner.as_str().to_owned())
                    .collect();
            }
            Err(error) => {
                eprintln!("leftover reclaim: cannot read store leases: {error:#}");
                liveness.evidence_incomplete = true;
            }
        }
        match crate::job_claim::held_job_claim_ids(run_root) {
            Ok(claimed) => liveness.claimed = claimed,
            Err(error) => {
                eprintln!("leftover reclaim: cannot read job claims: {error:#}");
                liveness.evidence_incomplete = true;
            }
        }
        liveness
    }

    fn is_live_with_modified(
        &self,
        job_id: &str,
        modified: Option<SystemTime>,
        now: SystemTime,
    ) -> bool {
        if self.running.contains(job_id)
            || self.leased.contains(job_id)
            || self.claimed.contains(job_id)
        {
            return true;
        }
        modified
            .and_then(|modified| now.duration_since(modified).ok())
            .is_none_or(|idle| idle < self.min_idle)
    }
}

pub fn list_live_job_names_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("name={LIVE_JOB_NAME_PREFIX}"),
        "--format".into(),
        "{{.Names}}".into(),
    ]
}

/// Dangling untagged layers only. Never `-a`, never `system`/`volume`/`builder`.
pub fn dangling_image_prune_args() -> Vec<String> {
    vec!["image".into(), "prune".into(), "-f".into()]
}

pub fn live_job_ids_from_docker_ps(formatted: &str) -> BTreeSet<String> {
    formatted
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next().unwrap_or(line).trim();
            name.strip_prefix(LIVE_JOB_NAME_PREFIX)
                .filter(|id| looks_like_job_uuid(id))
                .map(ToOwned::to_owned)
        })
        .collect()
}

pub fn looks_like_job_uuid(name: &str) -> bool {
    let mut parts = name.split('-');
    let expected = [8_usize, 4, 4, 4, 12];
    for len in expected {
        let Some(part) = parts.next() else {
            return false;
        };
        if part.len() != len || !part.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return false;
        }
    }
    parts.next().is_none()
}

pub(crate) fn discover_daemon_work_roots_for_layout(
    layout: &crate::storage::StorageLayout,
) -> Vec<PathBuf> {
    if layout.mode == "explicit-config" {
        let work = layout.lib_root.join("_work");
        return is_real_directory(&work)
            .then(|| crate::container::daemon_shared_root(work))
            .into_iter()
            .collect();
    }
    let lib_parent = layout
        .lib_root
        .parent()
        .filter(|parent| *parent != Path::new("/"))
        .unwrap_or(&layout.lib_root);
    let mut roots = discover_daemon_work_roots_in(lib_parent);
    if roots.is_empty() {
        let work = layout.lib_root.join("work");
        if is_real_directory(&work) {
            roots.push(work);
        }
    }
    roots
}

pub fn discover_daemon_work_roots_in(lib: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    // `lib` is the configured discovery anchor (and may be `/var/lib` on
    // macOS, where `/var` aliases `/private/var`). Resolve that anchor once;
    // never follow a discovered service/work directory symlink.
    let Ok(lib) = fs::canonicalize(lib) else {
        return roots;
    };
    if !is_real_directory(&lib) {
        return roots;
    }
    let Ok(entries) = fs::read_dir(&lib) else {
        return roots;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("velnor") {
            continue;
        }
        let work = entry.path().join("work");
        if is_real_directory(&work) {
            roots.push(work);
        }
    }
    roots.sort();
    roots
}

/// Orphan workspaces under `work_roots`, judged against every liveness source.
///
/// A workspace is deleted only when no evidence says it is live and all
/// evidence sources were readable.
pub fn orphan_job_workspace_paths_with_liveness(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
) -> Vec<PathBuf> {
    authorized_orphan_workspaces_with_liveness(work_roots, liveness)
        .into_iter()
        .map(|workspace| workspace.path)
        .collect()
}

#[derive(Debug)]
struct AuthorizedWorkspace {
    path: PathBuf,
    trusted_anchor: PathBuf,
    anchor_identity: FilesystemDirectoryIdentity,
    candidate_identity: FilesystemDirectoryIdentity,
}

fn authorized_orphan_workspaces_with_liveness(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
) -> Vec<AuthorizedWorkspace> {
    if liveness.evidence_incomplete {
        eprintln!("leftover reclaim: liveness evidence incomplete; deleting nothing");
        return Vec::new();
    }
    let now = SystemTime::now();
    let mut orphans = Vec::new();
    for work in work_roots {
        // The work root is the trusted boundary for all discovered descendants.
        // Canonicalize its configured parent (which may include `/var` aliases),
        // then open the work-root leaf with O_NOFOLLOW so a replacement link is
        // never adopted as the authority for deletion.
        let Ok(work_directory) = open_configured_directory_leaf_nofollow(work) else {
            continue;
        };
        let Ok(anchor_identity) = directory_identity(&work_directory) else {
            continue;
        };
        let Ok(slots) = secure_directory_entries(&work_directory) else {
            continue;
        };
        for slot in slots {
            let slot_name = slot.name;
            if !slot.is_directory
                || slot.is_mountpoint
                || !slot_name.to_string_lossy().starts_with("slot-")
            {
                continue;
            }
            let Ok(slot_directory) = open_directory_child(&work_directory, &slot_name) else {
                continue;
            };
            let Ok(slot_identity) = directory_identity(&slot_directory) else {
                continue;
            };
            if slot_identity.device != anchor_identity.device
                || slot_identity.mount != anchor_identity.mount
            {
                continue;
            }
            let Ok(jobs) = secure_directory_entries(&slot_directory) else {
                continue;
            };
            for job in jobs {
                if !job.is_directory || job.is_mountpoint {
                    continue;
                }
                let job_name = job.name;
                let Some(job_name_text) = job_name.to_str() else {
                    continue;
                };
                if job_name_text.starts_with("_velnor_") {
                    continue;
                }
                if !looks_like_job_uuid(job_name_text) {
                    continue;
                }
                let Ok(job_directory) = open_directory_child(&slot_directory, &job_name) else {
                    continue;
                };
                let Ok(job_identity) = directory_identity(&job_directory) else {
                    continue;
                };
                if job_identity.device != anchor_identity.device
                    || job_identity.mount != anchor_identity.mount
                {
                    continue;
                }
                let job_path = work.join(&slot_name).join(&job_name);
                if liveness.is_live_with_modified(
                    job_name_text,
                    newest_modification_at(&job_directory, 0),
                    now,
                ) {
                    continue;
                }
                orphans.push(AuthorizedWorkspace {
                    path: job_path,
                    trusted_anchor: work.clone(),
                    anchor_identity: anchor_identity.clone(),
                    candidate_identity: job_identity,
                });
            }
        }
    }
    orphans.sort_by(|left, right| left.path.cmp(&right.path));
    orphans
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn newest_modification_at(directory: &fs::File, depth: usize) -> Option<SystemTime> {
    let modified = directory.metadata().ok()?.modified().ok()?;
    if depth >= 3 {
        return Some(modified);
    }
    let entries = secure_directory_entries(directory).ok()?;
    let current_mount = mount_identity_of_directory(directory).ok()?;
    let mut newest = modified;
    for entry in entries {
        let child_modified = if entry.is_directory {
            if entry.is_mountpoint {
                return None;
            }
            let child = open_directory_child(directory, &entry.name).ok()?;
            if mount_identity_of_directory(&child).ok()? != current_mount {
                return None;
            }
            newest_modification_at(&child, depth + 1)?
        } else {
            modification_time_at(directory, &entry.name).ok()?
        };
        newest = newest.max(child_modified);
    }
    Some(newest)
}

fn modification_time_at(parent: &fs::File, name: &std::ffi::OsStr) -> Result<SystemTime> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let name =
        std::ffi::CString::new(name.as_bytes()).context("workspace entry name contains nul")?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: parent is live, name is terminated, and stat is writable.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("read workspace entry timestamp");
    }
    // SAFETY: fstatat initialized the complete struct on success.
    let stat = unsafe { stat.assume_init() };
    #[cfg(target_os = "linux")]
    let (seconds, nanoseconds) = (stat.st_mtime, stat.st_mtime_nsec);
    #[cfg(target_os = "macos")]
    let (seconds, nanoseconds) = (stat.st_mtime, stat.st_mtime_nsec);
    let nanos = u32::try_from(nanoseconds).context("invalid workspace entry timestamp")?;
    let fraction = std::time::Duration::from_nanos(u64::from(nanos));
    if seconds >= 0 {
        SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(seconds as u64))
            .and_then(|time| time.checked_add(fraction))
            .context("workspace entry timestamp is out of range")
    } else {
        SystemTime::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(seconds.unsigned_abs()))
            .and_then(|time| time.checked_add(fraction))
            .context("workspace entry timestamp is out of range")
    }
}

#[derive(Debug)]
struct SecureDirectoryEntry {
    name: std::ffi::OsString,
    is_directory: bool,
    is_mountpoint: bool,
}

/// Open one configured anchor. Canonicalization is limited to the trusted
/// anchor itself; descendants are always opened relative to this descriptor.
fn open_configured_directory(path: &Path) -> Result<fs::File> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("resolve configured cleanup anchor {}", path.display()))?;
    #[cfg(target_os = "linux")]
    {
        open_absolute_directory(&canonical)
    }
    #[cfg(target_os = "macos")]
    {
        open_macos_absolute_directory(&canonical)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = canonical;
        bail!("refusing directory traversal without native no-follow support")
    }
}

/// Open a configured work root without following its final component. Its
/// parent may contain an operator-configured alias such as macOS `/var`, but
/// the work-root directory itself must remain the exact directory we inspect.
fn open_configured_directory_leaf_nofollow(path: &Path) -> Result<fs::File> {
    if !path.is_absolute() {
        bail!("configured work root is not absolute: {}", path.display());
    }
    let parent = path
        .parent()
        .context("configured work root has no parent")?;
    let name = path
        .file_name()
        .context("configured work root has no final component")?;
    open_directory_child(&open_configured_directory(parent)?, name)
}

fn open_directory_child(parent: &fs::File, name: &std::ffi::OsStr) -> Result<fs::File> {
    #[cfg(target_os = "linux")]
    {
        let child = rustix::fs::openat(
            parent,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .context("open directory component without following links")?;
        Ok(child.into())
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::ffi::OsStrExt as _;

        let name =
            std::ffi::CString::new(name.as_bytes()).context("directory component contains nul")?;
        // SAFETY: parent is live and name is a terminated single component.
        let child_fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if child_fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open directory component without following links");
        }
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { fs::File::from_raw_fd(child_fd) })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (parent, name);
        bail!("refusing directory traversal without native no-follow support")
    }
}

fn secure_directory_entries(directory: &fs::File) -> Result<Vec<SecureDirectoryEntry>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt as _;

        let FilesystemMountIdentity::LinuxMountId(parent_mount_id) =
            mount_identity_of_directory(directory)?;
        // `fdopendir` advances a directory stream cursor. Open `.` relative
        // to the pinned descriptor so every scan gets an independent cursor.
        let scan_directory = open_directory_child(directory, std::ffi::OsStr::new("."))?;
        let entries = rustix::fs::Dir::read_from(&scan_directory)
            .map_err(std::io::Error::from)
            .context("read anchored directory")?;
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(std::io::Error::from)
                .context("read directory entry")?;
            let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
            if name == "." || name == ".." {
                continue;
            }
            let stat = rustix::fs::statat(directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .map_err(std::io::Error::from)
                .context("inspect anchored directory entry")?;
            let is_directory = rustix::fs::FileType::from_raw_mode(stat.st_mode)
                == rustix::fs::FileType::Directory;
            let is_mountpoint = if is_directory {
                mount_id_at(directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?
                    != parent_mount_id
            } else {
                false
            };
            result.push(SecureDirectoryEntry {
                name,
                is_directory,
                is_mountpoint,
            });
        }
        Ok(result)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(macos_bulk_directory_entries(directory)?
            .into_iter()
            .map(|entry| SecureDirectoryEntry {
                name: entry.name,
                is_directory: entry.is_directory,
                is_mountpoint: entry.is_mountpoint,
            })
            .collect())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = directory;
        bail!("refusing directory traversal without native mount proof")
    }
}

fn mount_identity_of_directory(directory: &fs::File) -> Result<FilesystemMountIdentity> {
    #[cfg(target_os = "linux")]
    {
        Ok(FilesystemMountIdentity::LinuxMountId(mount_id_at(
            directory,
            Path::new(""),
            rustix::fs::AtFlags::EMPTY_PATH,
        )?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(FilesystemMountIdentity::MacOs(
            macos_directory_mount_identity(directory)?,
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = directory;
        bail!("refusing directory traversal without native mount proof")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilesystemEntryIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) mount: FilesystemMountIdentity,
}

pub(crate) type FilesystemDirectoryIdentity = FilesystemEntryIdentity;

fn directory_identity(directory: &fs::File) -> Result<FilesystemDirectoryIdentity> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = directory
            .metadata()
            .context("inspect trusted directory descriptor")?;
        if !metadata.is_dir() {
            bail!("trusted directory descriptor is not a directory");
        }
        Ok(FilesystemEntryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            mount: mount_identity_of_directory(directory)?,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = directory;
        bail!("refusing directory identity without native mount proof")
    }
}

pub(crate) fn filesystem_object_identity(object: &fs::File) -> Result<FilesystemEntryIdentity> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = object
            .metadata()
            .context("inspect pinned filesystem object descriptor")?;
        Ok(FilesystemEntryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            mount: mount_identity_of_directory(object)?,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = object;
        bail!("refusing filesystem identity without native mount proof")
    }
}

/// Identity for a configured cleanup anchor, captured before enumeration and
/// checked again before deletion.
pub(crate) fn filesystem_directory_identity(
    trusted_anchor: &Path,
) -> Result<FilesystemDirectoryIdentity> {
    directory_identity(&open_configured_directory(trusted_anchor)?)
}

/// Capture a candidate directory identity beneath a pinned configured anchor.
/// Every ancestor is opened without following links and must stay on the
/// anchor's device and mount; the candidate itself is opened the same way.
pub(crate) fn filesystem_directory_identity_under(
    trusted_anchor: &Path,
    path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<FilesystemDirectoryIdentity> {
    filesystem_pin_directory_under(trusted_anchor, path, expected_anchor)
        .map(|(_, identity)| identity)
}

/// Pin a real directory beneath a trusted anchor without following any
/// descendant symlink. The returned descriptor and identity can be passed to
/// [`remove_dir_all_on_device_under_pinned`] so cleanup remains rooted in the
/// same nofollow walk used for inventory.
pub(crate) fn filesystem_pin_directory_under(
    trusted_anchor: &Path,
    path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<(fs::File, FilesystemDirectoryIdentity)> {
    let (parent, name, _, anchor_identity) =
        open_parent_beneath_anchor(trusted_anchor, path, Some(expected_anchor))?;
    let candidate = open_directory_child(&parent, &name)
        .context("open candidate directory without following links")?;
    let candidate_identity = directory_identity(&candidate)?;
    if candidate_identity.device != anchor_identity.device
        || candidate_identity.mount != anchor_identity.mount
    {
        bail!("candidate directory crosses a mount boundary below its trusted anchor");
    }
    Ok((candidate, candidate_identity))
}

/// Open the existing cache-entry sidecar lock below a pinned catalog anchor.
/// The store directory is reopened through the same nofollow walk as cache
/// inventory, and both the lock directory and lock file are descriptor
/// relative so a replaced ancestor cannot redirect lock creation outside the
/// catalog tree.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn filesystem_open_cache_lock_file_under_anchor(
    trusted_anchor: &Path,
    store_directory_path: &Path,
    lock_name: &std::ffi::OsStr,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<fs::File> {
    use std::os::unix::fs::MetadataExt as _;

    let components: Vec<_> = Path::new(lock_name).components().collect();
    if !matches!(components.as_slice(), [std::path::Component::Normal(_)]) {
        bail!("cache lock name is not one normalized path component");
    }
    let (store_directory, anchor_identity) =
        open_directory_under_anchor(trusted_anchor, store_directory_path, expected_anchor)?;
    let lock_directory_name = std::ffi::OsStr::new(".velnor-locks");
    let lock_directory = match open_directory_child(&store_directory, lock_directory_name) {
        Ok(directory) => directory,
        Err(error) if is_not_found_error(&error) => {
            match rustix::fs::mkdirat(
                &store_directory,
                lock_directory_name,
                rustix::fs::Mode::from_raw_mode(0o755),
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => {
                    return Err(std::io::Error::from(error))
                        .context("create cache entry lock directory beneath pinned store");
                }
            }
            open_directory_child(&store_directory, lock_directory_name)
                .context("open created cache entry lock directory without following links")?
        }
        Err(error) => return Err(error).context("open cache entry lock directory"),
    };
    let lock_directory_identity = directory_identity(&lock_directory)?;
    if lock_directory_identity.device != anchor_identity.device
        || lock_directory_identity.mount != anchor_identity.mount
    {
        bail!("cache entry lock directory crosses its pinned catalog mount");
    }
    let lock_directory_metadata = lock_directory
        .metadata()
        .context("inspect pinned cache entry lock directory")?;
    if lock_directory_metadata.uid() != rustix::process::geteuid().as_raw()
        || lock_directory_metadata.mode() & 0o022 != 0
    {
        bail!("cache entry lock directory is not runner-owned and private");
    }
    let lock_file = rustix::fs::openat(
        &lock_directory,
        lock_name,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOCTTY
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(std::io::Error::from)
    .context("open cache entry lock file without following links")?;
    let lock_file: fs::File = lock_file.into();
    let lock_metadata = lock_file
        .metadata()
        .context("inspect opened cache entry lock file")?;
    if !lock_metadata.is_file() || lock_metadata.nlink() != 1 {
        bail!("cache entry lock is not a single-link regular file");
    }
    Ok(lock_file)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn filesystem_open_cache_lock_file_under_anchor(
    trusted_anchor: &Path,
    store_directory_path: &Path,
    lock_name: &std::ffi::OsStr,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<fs::File> {
    let _ = (
        trusted_anchor,
        store_directory_path,
        lock_name,
        expected_anchor,
    );
    bail!("refusing cache lock traversal without native nofollow support")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilesystemDirectoryChild {
    pub(crate) name: std::ffi::OsString,
    pub(crate) identity: FilesystemDirectoryIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilesystemEntryKind {
    Directory,
    RegularFile,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilesystemEntry {
    pub(crate) name: std::ffi::OsString,
    pub(crate) kind: FilesystemEntryKind,
    pub(crate) identity: FilesystemEntryIdentity,
    pub(crate) is_mountpoint: bool,
}

pub(crate) struct FilesystemCandidateSnapshot {
    pub(crate) path: PathBuf,
    pub(crate) identity: FilesystemDirectoryIdentity,
    pub(crate) logical_bytes: u64,
    pub(crate) newest_modified: SystemTime,
    pub(crate) same_mount_tree: bool,
    pub(crate) directory: fs::File,
}

#[derive(Clone, Debug)]
struct FilesystemEntrySnapshot {
    entry: FilesystemEntry,
    logical_bytes: u64,
    modified: SystemTime,
}

/// Inventory direct entries through a descriptor beneath a pinned configured
/// anchor. The name, nofollow kind, inode/device, and mount identity are
/// captured from that descriptor listing; callers can retain and compare the
/// exact snapshot across lock waits.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn filesystem_entries_under(
    trusted_anchor: &Path,
    directory_path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<Vec<FilesystemEntry>> {
    let (directory, _) =
        open_directory_under_anchor(trusted_anchor, directory_path, expected_anchor)?;
    Ok(secure_filesystem_entries(&directory)?
        .into_iter()
        .map(|snapshot| snapshot.entry)
        .collect())
}

pub(crate) fn filesystem_entries_at(directory: &fs::File) -> Result<Vec<FilesystemEntry>> {
    Ok(secure_filesystem_entries(directory)?
        .into_iter()
        .map(|snapshot| snapshot.entry)
        .collect())
}

/// Open one direct child directory from an already-pinned directory. The
/// child name is resolved relative to the descriptor and `O_NOFOLLOW` keeps a
/// candidate walk from adopting a symlink introduced below the anchor.
pub(crate) fn filesystem_open_directory_child_at(
    directory: &fs::File,
    name: &std::ffi::OsStr,
) -> Result<Option<(fs::File, FilesystemDirectoryIdentity)>> {
    match open_directory_child(directory, name) {
        Ok(child) => {
            let identity = directory_identity(&child)?;
            Ok(Some((child, identity)))
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Read a direct regular-file child through a nofollow descriptor. Returns
/// its contents and mtime from the opened inode; no host pathname is
/// resolved after the candidate directory was pinned.
pub(crate) fn filesystem_read_regular_file_at(
    directory: &fs::File,
    name: &std::ffi::OsStr,
    maximum_bytes: usize,
) -> Result<Option<(Vec<u8>, SystemTime, FilesystemEntryIdentity)>> {
    use std::io::Read as _;

    let file = match open_regular_file_child(directory, name) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata().context("inspect opened marker file")?;
    if !metadata.is_file() {
        bail!("pinned marker entry is not a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            bail!("pinned marker file has unexpected hard links");
        }
    }
    let identity =
        filesystem_object_identity(&file).context("capture pinned marker file identity")?;
    if usize::try_from(metadata.len()).unwrap_or(usize::MAX) > maximum_bytes {
        bail!("pinned marker file exceeds its maximum length");
    }
    let modified = metadata
        .modified()
        .context("read opened marker file timestamp")?;
    let mut contents = Vec::new();
    let read_limit = u64::try_from(maximum_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(read_limit)
        .read_to_end(&mut contents)
        .context("read opened marker file")?;
    if contents.len() > maximum_bytes {
        bail!("pinned marker file grew beyond its maximum length");
    }
    Ok(Some((contents, modified, identity)))
}

fn open_regular_file_child(parent: &fs::File, name: &std::ffi::OsStr) -> Result<fs::File> {
    #[cfg(target_os = "linux")]
    {
        let file = rustix::fs::openat(
            parent,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .context("open marker entry without following links")?;
        Ok(file.into())
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::ffi::OsStrExt as _;

        let name = std::ffi::CString::new(name.as_bytes()).context("marker name contains nul")?;
        // SAFETY: parent is live and name is one directory entry. NOFOLLOW
        // rejects a symlink and NONBLOCK avoids hanging on an operator FIFO.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .context("open marker entry without following links");
        }
        // SAFETY: openat returned a new owned descriptor.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (parent, name);
        bail!("refusing marker read without native nofollow support")
    }
}

/// Discover candidates at an exact depth and retain one descriptor per
/// candidate. Identity, size, and newest mtime all come from this anchored
/// nofollow walk; deletion can compare the renamed entry against the retained
/// descriptor before touching its contents.
pub(crate) fn filesystem_candidate_tree_snapshots_under(
    trusted_anchor: &Path,
    directory_path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
    candidate_depth: usize,
) -> Result<Vec<FilesystemCandidateSnapshot>> {
    let Some((directory, anchor_identity)) =
        open_directory_under_anchor_if_exists(trusted_anchor, directory_path, expected_anchor)?
    else {
        return Ok(Vec::new());
    };
    let mut candidates = Vec::new();
    discover_candidate_tree_snapshots(
        &directory,
        directory_path,
        &anchor_identity,
        0,
        candidate_depth,
        &mut candidates,
    )?;
    Ok(candidates)
}

fn discover_candidate_tree_snapshots(
    directory: &fs::File,
    path: &Path,
    anchor_identity: &FilesystemDirectoryIdentity,
    depth: usize,
    candidate_depth: usize,
    candidates: &mut Vec<FilesystemCandidateSnapshot>,
) -> Result<()> {
    if depth == candidate_depth {
        let identity = directory_identity(directory)?;
        let (logical_bytes, newest_modified, same_mount_tree) =
            snapshot_directory_contents(directory, anchor_identity, 0)?;
        let pinned_directory = open_directory_child(directory, std::ffi::OsStr::new("."))
            .context("pin discovered cache candidate directory")?;
        if directory_identity(&pinned_directory)? != identity {
            bail!("cache candidate changed while pinning descriptor");
        }
        candidates.push(FilesystemCandidateSnapshot {
            path: path.to_path_buf(),
            identity,
            logical_bytes,
            newest_modified,
            same_mount_tree,
            directory: pinned_directory,
        });
        return Ok(());
    }
    const MAX_SECURE_TREE_DEPTH: usize = 256;
    if depth > MAX_SECURE_TREE_DEPTH {
        bail!("cache tree exceeds secure inventory depth {MAX_SECURE_TREE_DEPTH}");
    }
    for child_snapshot in secure_filesystem_entries(directory)? {
        let child = child_snapshot.entry;
        if child.kind != FilesystemEntryKind::Directory
            || child.is_mountpoint
            || child.identity.device != anchor_identity.device
            || child.identity.mount != anchor_identity.mount
        {
            continue;
        }
        let child_directory = open_directory_child(directory, &child.name)
            .context("open cache inventory ancestor without following links")?;
        if directory_identity(&child_directory)? != child.identity {
            bail!("cache inventory ancestor changed during secure open");
        }
        discover_candidate_tree_snapshots(
            &child_directory,
            &path.join(&child.name),
            anchor_identity,
            depth + 1,
            candidate_depth,
            candidates,
        )?;
    }
    Ok(())
}

fn snapshot_directory_contents(
    directory: &fs::File,
    anchor_identity: &FilesystemDirectoryIdentity,
    depth: usize,
) -> Result<(u64, SystemTime, bool)> {
    const MAX_SECURE_TREE_DEPTH: usize = 256;
    if depth > MAX_SECURE_TREE_DEPTH {
        bail!("cache tree exceeds secure inventory depth {MAX_SECURE_TREE_DEPTH}");
    }
    let identity = directory_identity(directory)?;
    if identity.device != anchor_identity.device || identity.mount != anchor_identity.mount {
        bail!("cache tree inventory crossed its pinned anchor mount");
    }
    let mut logical_bytes = 0_u64;
    let mut newest_modified = directory
        .metadata()
        .context("inspect cache directory descriptor")?
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut same_mount_tree = true;
    for child_snapshot in secure_filesystem_entries(directory)? {
        let child = child_snapshot.entry;
        if !filesystem_entry_matches_mount(&child, anchor_identity) {
            same_mount_tree = false;
            continue;
        }
        newest_modified = newest_modified.max(child_snapshot.modified);
        if child.kind == FilesystemEntryKind::RegularFile {
            logical_bytes = logical_bytes.saturating_add(child_snapshot.logical_bytes);
            continue;
        }
        if child.kind != FilesystemEntryKind::Directory {
            continue;
        }
        let child_directory = open_directory_child(directory, &child.name)
            .context("open cache directory without following links")?;
        if directory_identity(&child_directory)? != child.identity {
            bail!("cache directory changed during descriptor-relative inventory");
        }
        let (child_bytes, child_modified, child_same_mount) =
            snapshot_directory_contents(&child_directory, anchor_identity, depth + 1)?;
        logical_bytes = logical_bytes.saturating_add(child_bytes);
        newest_modified = newest_modified.max(child_modified);
        same_mount_tree &= child_same_mount;
    }
    Ok((logical_bytes, newest_modified, same_mount_tree))
}

fn filesystem_entry_matches_mount(
    entry: &FilesystemEntry,
    anchor_identity: &FilesystemDirectoryIdentity,
) -> bool {
    !entry.is_mountpoint
        && entry.identity.device == anchor_identity.device
        && entry.identity.mount == anchor_identity.mount
}

fn open_directory_under_anchor(
    trusted_anchor: &Path,
    directory_path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<(fs::File, FilesystemDirectoryIdentity)> {
    open_directory_under_anchor_if_exists(trusted_anchor, directory_path, expected_anchor)?
        .context("configured directory is absent beneath trusted anchor")
}

fn open_directory_under_anchor_if_exists(
    trusted_anchor: &Path,
    directory_path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<Option<(fs::File, FilesystemDirectoryIdentity)>> {
    use std::path::Component;

    if !trusted_anchor.is_absolute() || !directory_path.is_absolute() {
        bail!("directory inventory requires absolute anchor and path");
    }
    let relative = directory_path
        .strip_prefix(trusted_anchor)
        .with_context(|| {
            format!(
                "directory {} is outside trusted anchor {}",
                directory_path.display(),
                trusted_anchor.display()
            )
        })?;
    let mut directory = match open_configured_directory(trusted_anchor) {
        Ok(directory) => directory,
        Err(error) if is_not_found_error(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    let anchor_identity = directory_identity(&directory)?;
    if &anchor_identity != expected_anchor {
        bail!("trusted cleanup anchor changed since inventory");
    }
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                directory = match open_directory_child(&directory, name) {
                    Ok(directory) => directory,
                    Err(error) if is_not_found_error(&error) => return Ok(None),
                    Err(error) => {
                        return Err(error)
                            .context("open inventory ancestor without following links");
                    }
                };
                let identity = directory_identity(&directory)?;
                if identity.device != anchor_identity.device
                    || identity.mount != anchor_identity.mount
                {
                    bail!("directory inventory crosses a mount boundary below its trusted anchor");
                }
            }
            Component::CurDir => {}
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "directory inventory path is not normalized: {}",
                    directory_path.display()
                );
            }
        }
    }
    Ok(Some((directory, anchor_identity)))
}

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    })
}

fn secure_filesystem_entries(directory: &fs::File) -> Result<Vec<FilesystemEntrySnapshot>> {
    let directory_identity = directory_identity(directory)?;
    let entries = secure_inventory_entries(directory)?;
    let mut snapshots = Vec::with_capacity(entries.len());
    for entry in entries {
        let snapshot = filesystem_entry_snapshot(directory, entry, &directory_identity)?;
        snapshots.push(snapshot);
    }
    Ok(snapshots)
}

fn secure_inventory_entries(directory: &fs::File) -> Result<Vec<SecureDirectoryEntry>> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt as _;

        let scan_directory = open_directory_child(directory, std::ffi::OsStr::new("."))?;
        let entries = rustix::fs::Dir::read_from(&scan_directory)
            .map_err(std::io::Error::from)
            .context("read descriptor-relative inventory directory")?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(std::io::Error::from)
                .context("read descriptor-relative inventory name")?;
            let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
            if name == "." || name == ".." {
                continue;
            }
            names.push(SecureDirectoryEntry {
                name,
                is_directory: false,
                is_mountpoint: false,
            });
        }
        Ok(names)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(macos_bulk_directory_entries(directory)?
            .into_iter()
            .map(|entry| SecureDirectoryEntry {
                name: entry.name,
                is_directory: entry.is_directory,
                is_mountpoint: entry.is_mountpoint,
            })
            .collect())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = directory;
        bail!("refusing directory inventory without native nofollow support")
    }
}

fn filesystem_entry_snapshot(
    parent: &fs::File,
    entry: SecureDirectoryEntry,
    parent_identity: &FilesystemDirectoryIdentity,
) -> Result<FilesystemEntrySnapshot> {
    #[cfg(target_os = "linux")]
    {
        let stat = linux_stat_at(parent, &entry.name)?;
        let mode = u32::from(stat.stx_mode) & libc::S_IFMT;
        let kind = if mode == libc::S_IFDIR {
            FilesystemEntryKind::Directory
        } else if mode == libc::S_IFREG {
            FilesystemEntryKind::RegularFile
        } else if mode == libc::S_IFLNK {
            FilesystemEntryKind::Symlink
        } else {
            FilesystemEntryKind::Other
        };
        let mount_id = stat.stx_mnt_id;
        let mount = FilesystemMountIdentity::LinuxMountId(mount_id);
        let identity = FilesystemEntryIdentity {
            device: rustix::fs::makedev(stat.stx_dev_major, stat.stx_dev_minor) as u64,
            inode: stat.stx_ino,
            mount,
        };
        let logical_bytes = if kind == FilesystemEntryKind::RegularFile {
            stat.stx_size
        } else {
            0
        };
        let modified = modified_time_from_linux_statx(&stat)?;
        Ok(FilesystemEntrySnapshot {
            entry: FilesystemEntry {
                name: entry.name,
                kind,
                identity,
                is_mountpoint: entry.is_mountpoint
                    || mount_id
                        != match &parent_identity.mount {
                            FilesystemMountIdentity::LinuxMountId(parent_mount) => *parent_mount,
                            #[allow(unreachable_patterns)]
                            _ => {
                                return Err(anyhow::anyhow!(
                                    "inventory mount identity platform mismatch"
                                ))
                            }
                        },
            },
            logical_bytes,
            modified,
        })
    }
    #[cfg(target_os = "macos")]
    {
        let stat = macos_stat_at(parent, &entry.name, Path::new("<inventory entry>"))?;
        let mode = stat.st_mode & libc::S_IFMT;
        let kind = if mode == libc::S_IFDIR {
            FilesystemEntryKind::Directory
        } else if mode == libc::S_IFREG {
            FilesystemEntryKind::RegularFile
        } else if mode == libc::S_IFLNK {
            FilesystemEntryKind::Symlink
        } else {
            FilesystemEntryKind::Other
        };
        let opened_identity = if kind == FilesystemEntryKind::Directory {
            let identity = directory_identity(&open_directory_child(parent, &entry.name)?)?;
            if identity.device != stat.st_dev as u64 || identity.inode != stat.st_ino as u64 {
                bail!("macOS inventory directory changed while opening descriptor");
            }
            Some(identity)
        } else {
            None
        };
        let identity = opened_identity.unwrap_or_else(|| FilesystemEntryIdentity {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
            mount: parent_identity.mount.clone(),
        });
        let modified = modified_time_from_stat(&stat)?;
        let is_mountpoint = entry.is_mountpoint
            || identity.device != parent_identity.device
            || identity.mount != parent_identity.mount;
        Ok(FilesystemEntrySnapshot {
            entry: FilesystemEntry {
                name: entry.name,
                kind,
                identity,
                is_mountpoint,
            },
            logical_bytes: if kind == FilesystemEntryKind::RegularFile {
                u64::try_from(stat.st_size).unwrap_or(0)
            } else {
                0
            },
            modified,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (parent, entry, parent_identity);
        bail!("refusing directory inventory without native mount proof")
    }
}

#[cfg(target_os = "linux")]
fn linux_stat_at(parent: &fs::File, name: &std::ffi::OsStr) -> Result<rustix::fs::Statx> {
    let stat = rustix::fs::statx(
        parent,
        name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        rustix::fs::StatxFlags::BASIC_STATS | rustix::fs::StatxFlags::MNT_ID,
    )
    .map_err(std::io::Error::from)
    .context("inspect nofollow inventory entry")?;
    let required_mask =
        (rustix::fs::StatxFlags::BASIC_STATS | rustix::fs::StatxFlags::MNT_ID).bits();
    if stat.stx_mask & required_mask != required_mask {
        bail!("filesystem omitted required inventory identity or metadata");
    }
    Ok(stat)
}

#[cfg(target_os = "macos")]
fn modified_time_from_stat(stat: &libc::stat) -> Result<SystemTime> {
    #[cfg(target_os = "linux")]
    let (seconds, nanoseconds) = (stat.st_mtime, stat.st_mtime_nsec);
    #[cfg(target_os = "macos")]
    let (seconds, nanoseconds) = (stat.st_mtime, stat.st_mtime_nsec);
    let nanos = u32::try_from(nanoseconds).context("invalid inventory entry timestamp")?;
    let fraction = std::time::Duration::from_nanos(u64::from(nanos));
    if seconds >= 0 {
        SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(seconds as u64))
            .and_then(|time| time.checked_add(fraction))
            .context("inventory entry timestamp is out of range")
    } else {
        SystemTime::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(seconds.unsigned_abs()))
            .and_then(|time| time.checked_add(fraction))
            .context("inventory entry timestamp is out of range")
    }
}

#[cfg(target_os = "linux")]
fn modified_time_from_linux_statx(stat: &rustix::fs::Statx) -> Result<SystemTime> {
    let seconds = stat.stx_mtime.tv_sec;
    let fraction = std::time::Duration::from_nanos(u64::from(stat.stx_mtime.tv_nsec));
    if seconds >= 0 {
        SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(seconds as u64))
            .and_then(|time| time.checked_add(fraction))
            .context("inventory entry timestamp is out of range")
    } else {
        SystemTime::UNIX_EPOCH
            .checked_sub(std::time::Duration::from_secs(seconds.unsigned_abs()))
            .and_then(|time| time.checked_add(fraction))
            .context("inventory entry timestamp is out of range")
    }
}

/// Enumerate real, same-mount child directories beneath a pinned anchor.
/// Reads names through a descriptor, rejects symlink ancestors, and returns
/// each child's identity captured from its nofollow-opened descriptor.
pub(crate) fn filesystem_directory_children_under(
    trusted_anchor: &Path,
    directory_path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<Vec<FilesystemDirectoryChild>> {
    use std::path::Component;

    if !trusted_anchor.is_absolute() || !directory_path.is_absolute() {
        bail!("directory inventory requires absolute anchor and path");
    }
    let relative = directory_path
        .strip_prefix(trusted_anchor)
        .with_context(|| {
            format!(
                "directory {} is outside trusted anchor {}",
                directory_path.display(),
                trusted_anchor.display()
            )
        })?;
    let mut directory = open_configured_directory(trusted_anchor)?;
    let anchor_identity = directory_identity(&directory)?;
    if &anchor_identity != expected_anchor {
        bail!("trusted cleanup anchor changed since inventory");
    }
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                directory = open_directory_child(&directory, name)
                    .context("open inventory ancestor without following links")?;
                let identity = directory_identity(&directory)?;
                if identity.device != anchor_identity.device
                    || identity.mount != anchor_identity.mount
                {
                    bail!("directory inventory crosses a mount boundary below its trusted anchor");
                }
            }
            Component::CurDir => {}
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "directory inventory path is not normalized: {}",
                    directory_path.display()
                );
            }
        }
    }

    let mut children = Vec::new();
    for entry in secure_directory_entries(&directory)? {
        if !entry.is_directory || entry.is_mountpoint {
            continue;
        }
        let child = match open_directory_child(&directory, &entry.name) {
            Ok(child) => child,
            Err(_) => continue,
        };
        let identity = directory_identity(&child)?;
        if identity.device == anchor_identity.device && identity.mount == anchor_identity.mount {
            children.push(FilesystemDirectoryChild {
                name: entry.name,
                identity,
            });
        }
    }
    Ok(children)
}

/// `docker ps`-only view, retained for callers that have no runtime root.
///
/// Prefer [`orphan_job_workspace_paths_with_liveness`]: container liveness on
/// its own cannot see a job that is checking out, uploading artifacts,
/// publishing a target generation, or tearing BuildKit down.
pub fn orphan_job_workspace_paths(
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
) -> Vec<PathBuf> {
    orphan_job_workspace_paths_with_liveness(
        work_roots,
        &WorkspaceLiveness {
            running: live_job_ids.clone(),
            min_idle: WORKSPACE_MIN_IDLE,
            ..WorkspaceLiveness::default()
        },
    )
}

#[cfg(test)]
pub fn disk_usage_percent_from_df(stdout: &str) -> Option<u8> {
    let line = stdout.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() >= 5
        && let Ok(percent) = cols[4].trim_end_matches('%').parse::<u8>()
    {
        return Some(percent);
    }
    if cols.len() >= 3 {
        let total: u64 = cols[1].parse().ok()?;
        let used: u64 = cols[2].parse().ok()?;
        if total == 0 {
            return Some(0);
        }
        return Some(used.saturating_mul(100).saturating_div(total) as u8);
    }
    None
}

#[cfg(test)]
pub fn disk_usage_percent(path: &Path) -> Option<u8> {
    let probe = if path.exists() {
        path
    } else {
        path.parent().filter(|parent| parent.exists())?
    };
    #[cfg(unix)]
    {
        let stat = rustix::fs::statvfs(probe).ok()?;
        disk_usage_percent_from_statvfs(stat.f_blocks, stat.f_bavail)
    }
    #[cfg(not(unix))]
    {
        let output = std::process::Command::new("df")
            .arg("-Pk")
            .arg(probe)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        disk_usage_percent_from_df(&String::from_utf8_lossy(&output.stdout))
    }
}

#[cfg(test)]
fn disk_usage_percent_from_statvfs(total_blocks: u64, available_blocks: u64) -> Option<u8> {
    if total_blocks == 0 {
        return None;
    }
    let used_blocks = total_blocks.saturating_sub(available_blocks);
    let percent = used_blocks
        .saturating_mul(100)
        .saturating_div(total_blocks)
        .min(100);
    u8::try_from(percent).ok()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LeftoverReclaimReport {
    pub deleted_workspaces: Vec<PathBuf>,
    pub kept_live: Vec<PathBuf>,
    pub docker_commands: Vec<Vec<String>>,
    pub skipped_docker: bool,
}

/// Reclaim leftover workspaces, acquiring the filesystem coordinator here.
///
/// This is the entry point for callers that hold no coordinator (the daemon's
/// disk-pressure paths). A caller that already holds it must use
/// [`reclaim_leftover_under_coordinator`]: the coordinator is a blocking
/// `flock`, and a second open of the same lock file from the thread that
/// holds it never returns, which is how `velnorctl cache gc` deadlocked on
/// itself. [`crate::capacity::FilesystemCoordinator`] refuses that re-entry
/// with an error, so the mistake is loud, but the fix is to lock once.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reclaim_leftover_after_velnor(
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
    docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&Path) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    let mut remove_dir = remove_dir;
    reclaim_leftover_after_velnor_authorized(
        work_roots,
        live_job_ids,
        docker,
        move |workspace| remove_dir(&workspace.path),
        prune_dangling_images,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn reclaim_leftover_after_velnor_authorized(
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
    docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&AuthorizedWorkspace) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    match runtime_root() {
        // Hold the same coordinator the cache reclaimer takes, so no daemon can
        // publish a lease between the liveness snapshot and the deletions it
        // authorizes.
        Some(run_root) => {
            let coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root)?;
            reclaim_leftover_under_coordinator_authorized(
                &coordinator,
                &run_root,
                work_roots,
                live_job_ids,
                docker,
                remove_dir,
                prune_dangling_images,
            )
        }
        None => reclaim_with_liveness_authorized(
            work_roots,
            &WorkspaceLiveness {
                running: live_job_ids.clone(),
                min_idle: WORKSPACE_MIN_IDLE,
                ..WorkspaceLiveness::default()
            },
            docker,
            remove_dir,
            prune_dangling_images,
        ),
    }
}
/// Reclaim leftover workspaces under a coordinator the caller already holds.
///
/// `_coordinator` is the proof of ownership: a [`FilesystemCoordinator`] can
/// only be obtained by locking, so this function cannot be reached without
/// the lock and never takes it again.
///
/// [`FilesystemCoordinator`]: crate::capacity::FilesystemCoordinator
#[cfg_attr(not(test), allow(dead_code))]
pub fn reclaim_leftover_under_coordinator(
    _coordinator: &crate::capacity::FilesystemCoordinator,
    run_root: &Path,
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
    docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&Path) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    let mut remove_dir = remove_dir;
    reclaim_leftover_under_coordinator_authorized(
        _coordinator,
        run_root,
        work_roots,
        live_job_ids,
        docker,
        move |workspace| remove_dir(&workspace.path),
        prune_dangling_images,
    )
}

fn reclaim_leftover_under_coordinator_authorized(
    _coordinator: &crate::capacity::FilesystemCoordinator,
    run_root: &Path,
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
    docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&AuthorizedWorkspace) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    let liveness = WorkspaceLiveness::collect(run_root, live_job_ids.clone());
    recover_workspace_private_quarantines_for_roots(work_roots, &liveness);
    reclaim_with_liveness_authorized(
        work_roots,
        &liveness,
        docker,
        remove_dir,
        prune_dangling_images,
    )
}

fn runtime_root() -> Option<PathBuf> {
    crate::storage::selected_or_resolved_layout().map(|layout| layout.run_root)
}

/// Delete only workspaces that every liveness source agrees are dead.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reclaim_with_liveness(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
    docker: impl FnMut(&[String]) -> Result<String>,
    mut remove_dir: impl FnMut(&Path) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    reclaim_with_liveness_authorized(
        work_roots,
        liveness,
        docker,
        move |workspace| remove_dir(&workspace.path),
        prune_dangling_images,
    )
}

fn reclaim_with_liveness_authorized(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
    mut docker: impl FnMut(&[String]) -> Result<String>,
    mut remove_dir: impl FnMut(&AuthorizedWorkspace) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    let mut report = LeftoverReclaimReport::default();
    let orphans = authorized_orphan_workspaces_with_liveness(work_roots, liveness);
    for workspace in orphans {
        match remove_dir(&workspace) {
            Ok(()) => report.deleted_workspaces.push(workspace.path),
            Err(error) => {
                eprintln!(
                    "leftover workspace reclaim failed for {}: {error:#}",
                    workspace.path.display()
                );
            }
        }
    }
    if prune_dangling_images {
        let args = dangling_image_prune_args();
        report.docker_commands.push(args.clone());
        if let Err(error) = docker(&args) {
            report.skipped_docker = true;
            eprintln!("dangling image prune failed: {error:#}");
        }
    }
    Ok(report)
}

/// Hard-pressure path: at >= 90% used, reclaim leftover-after-Velnor
/// disposable classes. Warm caches and unowned Docker stay.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reclaim_if_hard_pressure(
    usage_percent: u8,
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
    docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&Path) -> Result<()>,
) -> Result<LeftoverReclaimReport> {
    if usage_percent < HARD_PRESSURE_PERCENT {
        return Ok(LeftoverReclaimReport::default());
    }
    reclaim_leftover_after_velnor(work_roots, live_job_ids, docker, remove_dir, true)
}

/// Remove one candidate below an operator-selected anchor. Only the trusted
/// anchor is canonicalized (to support aliases such as macOS `/var`); every
/// candidate-relative ancestor is opened from its pinned descriptor with
/// no-follow semantics.
#[cfg(test)]
pub(crate) fn remove_dir_all_on_device_under_identity(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<()> {
    remove_dir_all_on_device_under_pins(
        trusted_anchor,
        path,
        expected_device,
        expected_anchor,
        None,
        None,
        None,
        None,
        &|_| Ok(()),
    )
}

/// Remove a candidate only if both its trusted anchor and the candidate root
/// still match identities captured during inventory.
pub(crate) fn remove_dir_all_on_device_under_identities(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_candidate: &FilesystemDirectoryIdentity,
) -> Result<()> {
    remove_dir_all_on_device_under_pins(
        trusted_anchor,
        path,
        expected_device,
        expected_anchor,
        None,
        None,
        Some(expected_candidate),
        None,
        &|_| Ok(()),
    )
}

pub(crate) fn remove_dir_all_on_device_under_identities_with_pre_unlink(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_candidate: &FilesystemDirectoryIdentity,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    remove_dir_all_on_device_under_pins(
        trusted_anchor,
        path,
        expected_device,
        expected_anchor,
        None,
        None,
        Some(expected_candidate),
        None,
        before_unlink,
    )
}

pub(crate) fn remove_dir_all_on_device_under_pinned(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_candidate: &FilesystemDirectoryIdentity,
    pinned_candidate: &fs::File,
) -> Result<()> {
    remove_dir_all_on_device_under_pinned_with_pre_unlink(
        trusted_anchor,
        path,
        expected_device,
        expected_anchor,
        expected_candidate,
        pinned_candidate,
        &|_| Ok(()),
    )
}

pub(crate) fn remove_dir_all_on_device_under_pinned_with_pre_unlink(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_candidate: &FilesystemDirectoryIdentity,
    pinned_candidate: &fs::File,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    remove_dir_all_on_device_under_pins(
        trusted_anchor,
        path,
        expected_device,
        expected_anchor,
        None,
        None,
        Some(expected_candidate),
        Some(pinned_candidate),
        before_unlink,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn remove_dir_all_on_device_under_pinned_parent_with_pre_unlink(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_parent: &FilesystemDirectoryIdentity,
    pinned_parent: &fs::File,
    expected_candidate: &FilesystemDirectoryIdentity,
    pinned_candidate: &fs::File,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    remove_dir_all_on_device_under_pins(
        trusted_anchor,
        path,
        expected_device,
        expected_anchor,
        Some(expected_parent),
        Some(pinned_parent),
        Some(expected_candidate),
        Some(pinned_candidate),
        before_unlink,
    )
}

/// Remove one empty directory after pinning and revalidating both its parent
/// path and child identity beneath a trusted anchor. `rmdir` remains the final
/// emptiness check, so a concurrently added entry is preserved.
pub(crate) fn remove_empty_directory_under_pinned_parent(
    trusted_anchor: &Path,
    path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_parent: &FilesystemDirectoryIdentity,
    pinned_parent: &fs::File,
    expected_candidate: &FilesystemDirectoryIdentity,
    pinned_candidate: &fs::File,
) -> Result<bool> {
    remove_empty_directory_under_pinned_parent_with_hook(
        trusted_anchor,
        path,
        expected_anchor,
        expected_parent,
        pinned_parent,
        expected_candidate,
        pinned_candidate,
        |_, _| Ok(()),
    )
}

#[allow(clippy::too_many_arguments)]
fn remove_empty_directory_under_pinned_parent_with_hook(
    trusted_anchor: &Path,
    path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_parent: &FilesystemDirectoryIdentity,
    pinned_parent: &fs::File,
    expected_candidate: &FilesystemDirectoryIdentity,
    pinned_candidate: &fs::File,
    after_move: impl FnOnce(&fs::File, &std::ffi::OsStr) -> Result<()>,
) -> Result<bool> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let anchor = open_configured_directory(trusted_anchor)?;
        if &directory_identity(&anchor)? != expected_anchor {
            bail!("trusted empty-directory anchor changed since inventory");
        }
        let pinned_parent_identity = directory_identity(pinned_parent)?;
        if &pinned_parent_identity != expected_parent {
            bail!("pinned cleanup parent changed since inventory");
        }
        if expected_parent.device != expected_anchor.device
            || expected_parent.mount != expected_anchor.mount
            || expected_candidate.device != expected_anchor.device
            || expected_candidate.mount != expected_anchor.mount
        {
            bail!("empty cleanup path crossed the trusted anchor filesystem or mount");
        }
        let pinned_candidate_identity = directory_identity(pinned_candidate)?;
        if &pinned_candidate_identity != expected_candidate {
            bail!("pinned empty directory changed since inventory");
        }

        let (parent, name, _root_path, anchor_identity) =
            open_parent_beneath_anchor(trusted_anchor, path, Some(expected_anchor))?;
        if &anchor_identity != expected_anchor || &directory_identity(&parent)? != expected_parent {
            bail!("empty-directory parent changed since inventory");
        }
        let candidate = open_directory_child(&parent, &name)
            .context("open empty cleanup directory without following links")?;
        if directory_identity(&candidate)? != pinned_candidate_identity {
            bail!("empty cleanup directory changed since inventory");
        }
        if !secure_directory_entries(&candidate)?.is_empty() {
            return Ok(false);
        }

        let source_relative_path = path
            .strip_prefix(trusted_anchor)
            .context("empty cleanup directory is outside its trusted anchor")?;
        let (quarantine, quarantine_name) = create_private_quarantine_with_journal(
            &anchor,
            expected_anchor,
            source_relative_path,
            &parent,
            expected_candidate,
        )?;
        let quarantine_identity = directory_identity(&quarantine)?;
        let quarantine_entry = std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY);
        let moved = match move_candidate_to_quarantine(
            &parent,
            &name,
            &quarantine,
            quarantine_entry,
            expected_candidate,
            pinned_candidate,
            || Ok(()),
        ) {
            Ok(moved) => moved,
            Err(error) => {
                let cleanup = remove_quarantine_record_and_directory(
                    &anchor,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                );
                return Err(error)
                    .context(format!("quarantine empty directory (cleanup: {cleanup:?})"));
            }
        };
        if let Err(error) = persist_quarantine_move(&parent, &quarantine) {
            let restore = restore_then_clear_quarantine(
                &anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &moved,
                expected_candidate,
                &parent,
                &name,
            );
            return Err(error).context(format!(
                "persist empty-directory quarantine (restore: {restore:?})"
            ));
        }
        if let Err(error) = after_move(&quarantine, quarantine_entry) {
            let restore = restore_then_clear_quarantine(
                &anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &moved,
                expected_candidate,
                &parent,
                &name,
            );
            return Err(error).context(format!(
                "restore empty directory after quarantine hook (restore: {restore:?})"
            ));
        }

        let moved_identity = directory_identity(&moved)?;
        if &moved_identity != expected_candidate {
            let restore = restore_then_clear_quarantine(
                &anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &moved,
                expected_candidate,
                &parent,
                &name,
            );
            return Err(anyhow::anyhow!(
                "quarantined empty directory changed identity"
            ))
            .context(format!(
                "preserve changed empty directory (restore: {restore:?})"
            ));
        }
        let current = open_directory_child(&quarantine, quarantine_entry)
            .context("reopen quarantined empty directory without following links")?;
        if directory_identity(&current)? != *expected_candidate {
            let restore = restore_then_clear_quarantine(
                &anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &moved,
                expected_candidate,
                &parent,
                &name,
            );
            return Err(anyhow::anyhow!(
                "quarantined empty directory name changed identity"
            ))
            .context(format!(
                "preserve changed empty directory (restore: {restore:?})"
            ));
        }
        if !secure_directory_entries(&current)?.is_empty() {
            restore_then_clear_quarantine(
                &anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &moved,
                expected_candidate,
                &parent,
                &name,
            )?;
            return Ok(false);
        }
        match rustix::fs::unlinkat(
            &quarantine,
            quarantine_entry,
            rustix::fs::AtFlags::REMOVEDIR,
        )
        .map_err(std::io::Error::from)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                restore_then_clear_quarantine(
                    &anchor,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                    &moved,
                    expected_candidate,
                    &parent,
                    &name,
                )?;
                return Ok(false);
            }
            Err(error) => {
                let restore = restore_then_clear_quarantine(
                    &anchor,
                    &quarantine,
                    &quarantine_name,
                    &quarantine_identity,
                    &moved,
                    expected_candidate,
                    &parent,
                    &name,
                );
                return Err(error).context(format!(
                    "remove quarantined empty directory (restore: {restore:?})"
                ));
            }
        }
        sync_directory(&quarantine).context("persist quarantined empty-directory removal")?;
        drop(moved);
        remove_quarantine_record_and_directory(
            &anchor,
            &quarantine,
            &quarantine_name,
            &quarantine_identity,
        )?;
        Ok(true)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (
            trusted_anchor,
            path,
            expected_anchor,
            expected_parent,
            pinned_parent,
            expected_candidate,
            pinned_candidate,
            after_move,
        );
        bail!("refusing empty-directory cleanup without native mount-identity proof")
    }
}

#[allow(clippy::too_many_arguments)]
fn remove_dir_all_on_device_under_pins(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_anchor: &FilesystemDirectoryIdentity,
    expected_parent: Option<&FilesystemDirectoryIdentity>,
    pinned_parent: Option<&fs::File>,
    expected_candidate: Option<&FilesystemDirectoryIdentity>,
    pinned_candidate: Option<&fs::File>,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt as _;

        if expected_parent.is_some() != pinned_parent.is_some() {
            bail!("pinned cleanup parent identity and descriptor must be supplied together");
        }
        if let (Some(expected), Some(pinned)) = (expected_parent, pinned_parent)
            && &directory_identity(pinned)? != expected
        {
            bail!("pinned cleanup parent changed since inventory");
        }

        let anchor = open_configured_directory(trusted_anchor)?;
        let anchor_identity = directory_identity(&anchor)?;
        if &anchor_identity != expected_anchor {
            bail!("trusted cleanup anchor changed since workspace discovery");
        }
        let (parent, name, root_path, opened_anchor_identity) =
            open_parent_beneath_anchor(trusted_anchor, path, Some(expected_anchor))?;
        if let Some(expected) = expected_parent
            && &directory_identity(&parent)? != expected
        {
            bail!("cleanup candidate parent changed since inventory");
        }
        let source_relative_path = path
            .strip_prefix(trusted_anchor)
            .context("leftover workspace is outside its trusted cleanup anchor")?;
        if opened_anchor_identity != anchor_identity {
            bail!("trusted cleanup anchor changed while opening candidate parent");
        }
        let parent_device = parent
            .metadata()
            .context("inspect anchored leftover parent")?
            .dev();
        if parent_device != expected_device {
            bail!(
                "leftover workspace parent is on device {parent_device}, expected {expected_device}"
            );
        }
        remove_dir_all_at(
            &parent,
            &name,
            &anchor,
            &anchor_identity,
            &root_path,
            source_relative_path,
            expected_device,
            anchor_identity.mount.clone(),
            expected_candidate,
            pinned_candidate,
            before_unlink,
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (
            trusted_anchor,
            path,
            expected_device,
            expected_anchor,
            expected_parent,
            pinned_parent,
            expected_candidate,
            pinned_candidate,
        );
        bail!("refusing leftover cleanup without native mount-identity proof")
    }
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn remove_dir_all_on_device_and_mount_under(
    trusted_anchor: &Path,
    path: &Path,
    expected_device: u64,
    expected_mount: FilesystemMountIdentity,
) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt as _;

        let anchor = open_configured_directory(trusted_anchor)?;
        let anchor_identity = directory_identity(&anchor)?;
        let (parent, name, root_path, opened_anchor_identity) =
            open_parent_beneath_anchor(trusted_anchor, path, Some(&anchor_identity))?;
        let source_relative_path = path
            .strip_prefix(trusted_anchor)
            .context("leftover workspace is outside its trusted cleanup anchor")?;
        if opened_anchor_identity != anchor_identity {
            bail!("trusted cleanup anchor changed while opening candidate parent");
        }
        let parent_device = parent
            .metadata()
            .context("inspect anchored leftover parent")?
            .dev();
        if parent_device != expected_device {
            bail!(
                "leftover workspace parent is on device {parent_device}, expected {expected_device}"
            );
        }
        if anchor_identity.mount != expected_mount {
            bail!("trusted cleanup anchor changed filesystem mount");
        }
        remove_dir_all_at(
            &parent,
            &name,
            &anchor,
            &anchor_identity,
            &root_path,
            source_relative_path,
            expected_device,
            expected_mount.clone(),
            None,
            None,
            &|_| Ok(()),
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (trusted_anchor, path, expected_device, expected_mount);
        bail!("refusing leftover cleanup without native mount-identity proof")
    }
}

#[allow(clippy::too_many_arguments)]
fn remove_dir_all_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    quarantine_anchor: &fs::File,
    expected_anchor: &FilesystemDirectoryIdentity,
    root_path: &Path,
    source_relative_path: &Path,
    expected_device: u64,
    expected_mount: FilesystemMountIdentity,
    expected_candidate: Option<&FilesystemDirectoryIdentity>,
    pinned_candidate: Option<&fs::File>,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let FilesystemMountIdentity::LinuxMountId(expected_mount_id) = expected_mount;
        remove_dir_all_with_identity_at(
            parent,
            name,
            quarantine_anchor,
            expected_anchor,
            root_path,
            source_relative_path,
            expected_device,
            Some(expected_mount_id),
            expected_candidate,
            pinned_candidate,
            &|_, device, mount_id| (device, mount_id),
            before_unlink,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let FilesystemMountIdentity::MacOs(expected_mount) = expected_mount;
        remove_dir_all_macos_at(
            parent,
            name,
            quarantine_anchor,
            expected_anchor,
            root_path,
            source_relative_path,
            expected_device,
            &expected_mount,
            expected_candidate,
            pinned_candidate,
            before_unlink,
        )
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (
            parent,
            name,
            quarantine_anchor,
            expected_anchor,
            root_path,
            source_relative_path,
            expected_device,
            expected_mount,
            expected_candidate,
            pinned_candidate,
            before_unlink,
        );
        bail!("refusing leftover cleanup without native mount-identity proof")
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
static QUARANTINE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_private_quarantine(
    anchor: &fs::File,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<(fs::File, std::ffi::OsString)> {
    create_private_quarantine_with_hook(anchor, expected_anchor, || Ok(()))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_private_quarantine_with_hook(
    anchor: &fs::File,
    expected_anchor: &FilesystemDirectoryIdentity,
    after_mkdir: impl FnOnce() -> Result<()>,
) -> Result<(fs::File, std::ffi::OsString)> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStringExt as _;

    let anchor_identity = directory_identity(anchor)?;
    if &anchor_identity != expected_anchor {
        bail!("trusted cleanup anchor changed before quarantine creation");
    }
    let mut after_mkdir = Some(after_mkdir);
    for _ in 0..64 {
        let sequence = QUARANTINE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!(
            "{RECLAIM_QUARANTINE_PREFIX}{}-{sequence}",
            std::process::id()
        );
        let encoded = std::ffi::CString::new(name.as_bytes())?;
        // SAFETY: `anchor` is a live directory fd and `encoded` is a single
        // terminated child name. mkdirat is exclusive and applies mode 0700.
        let created = unsafe { libc::mkdirat(anchor.as_raw_fd(), encoded.as_ptr(), 0o700) };
        if created != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                continue;
            }
            return Err(error).context("create private cleanup quarantine");
        }
        let name_os = std::ffi::OsString::from_vec(name.into_bytes());
        let setup = (|| {
            let hook = after_mkdir.take().ok_or_else(|| {
                anyhow::anyhow!("post-mkdir hook runs only after exclusive creation")
            })?;
            hook()?;
            sync_directory(anchor).context("persist created cleanup quarantine")?;
            let quarantine = open_directory_child(anchor, &name_os)
                .context("open private cleanup quarantine")?;
            let identity = directory_identity(&quarantine)?;
            if identity.device != expected_anchor.device || identity.mount != expected_anchor.mount
            {
                bail!("cleanup quarantine crossed its trusted anchor mount");
            }
            ensure_private_quarantine(&quarantine)?;
            Ok(quarantine)
        })();
        return match setup {
            Ok(quarantine) => Ok((quarantine, name_os)),
            Err(error) => {
                let cleanup = remove_private_quarantine(anchor, &name_os);
                Err(error).context(format!(
                    "validate created cleanup quarantine (empty-directory cleanup: {cleanup:?})"
                ))
            }
        };
    }
    bail!("could not allocate a unique cleanup quarantine")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_private_quarantine_with_journal(
    anchor: &fs::File,
    expected_anchor: &FilesystemDirectoryIdentity,
    source_relative_path: &Path,
    source_parent: &fs::File,
    source_identity: &FilesystemDirectoryIdentity,
) -> Result<(fs::File, std::ffi::OsString)> {
    let source_parent_identity = directory_identity(source_parent)?;
    if source_parent_identity.device != expected_anchor.device
        || source_parent_identity.mount != expected_anchor.mount
    {
        bail!("cleanup source parent crossed its trusted anchor mount");
    }
    let (quarantine, name) = create_private_quarantine(anchor, expected_anchor)?;
    let _identity = match directory_identity(&quarantine) {
        Ok(identity) => identity,
        Err(error) => {
            let cleanup = remove_private_quarantine(anchor, &name);
            return Err(error).context(format!(
                "pin cleanup quarantine before journaling (cleanup: {cleanup:?})"
            ));
        }
    };
    if let Err(error) = write_quarantine_journal(
        &quarantine,
        expected_anchor,
        &source_parent_identity,
        source_relative_path,
        source_identity,
    ) {
        let cleanup = remove_private_quarantine_unrecorded(anchor, &name);
        return Err(error).context(format!(
            "record cleanup quarantine intent (empty-directory cleanup: {cleanup:?})"
        ));
    }
    Ok((quarantine, name))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_quarantine_journal(
    quarantine: &fs::File,
    anchor_identity: &FilesystemDirectoryIdentity,
    source_parent_identity: &FilesystemDirectoryIdentity,
    source_relative_path: &Path,
    source_identity: &FilesystemDirectoryIdentity,
) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::ffi::OsStrExt as _;

    let path_bytes = source_relative_path.as_os_str().as_bytes();
    validate_quarantine_relative_path(path_bytes)?;
    if anchor_identity.device != source_parent_identity.device
        || anchor_identity.mount != source_parent_identity.mount
    {
        bail!("cleanup source parent crossed its trusted anchor mount");
    }
    let contents = format!(
        "velnor-reclaim-v2\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        anchor_identity.device,
        anchor_identity.inode,
        source_parent_identity.device,
        source_parent_identity.inode,
        source_identity.device,
        source_identity.inode,
        hex_encode(path_bytes),
    );
    let temporary_name = std::ffi::OsStr::new(RECLAIM_QUARANTINE_JOURNAL_TEMP);
    let journal_name = std::ffi::OsStr::new(RECLAIM_QUARANTINE_JOURNAL);
    let mut temporary = create_quarantine_file(quarantine, temporary_name)?;
    temporary
        .write_all(contents.as_bytes())
        .context("write cleanup quarantine transaction journal")?;
    temporary
        .sync_all()
        .context("persist cleanup quarantine transaction journal")?;
    rename_entry_noreplace(quarantine, temporary_name, quarantine, journal_name)
        .context("publish cleanup quarantine transaction journal")?;
    sync_directory(quarantine).context("persist published cleanup quarantine journal")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_quarantine_file(parent: &fs::File, name: &std::ffi::OsStr) -> Result<fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let name = std::ffi::CString::new(name.as_bytes())?;
    // SAFETY: parent is live and name is a terminated single component.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .context("create cleanup quarantine journal file");
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { fs::File::from_raw_fd(descriptor) };
    // SAFETY: the new descriptor refers to the exclusively created journal.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("set private cleanup quarantine journal mode");
    }
    Ok(file)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_quarantine_file(parent: &fs::File, name: &std::ffi::OsStr) -> Result<fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let name = std::ffi::CString::new(name.as_bytes())?;
    // SAFETY: parent is live and name is a terminated single component.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error()).context("open cleanup quarantine journal");
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { fs::File::from_raw_fd(descriptor) })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct QuarantineJournal {
    anchor_device: u64,
    anchor_inode: u64,
    source_parent_device: u64,
    source_parent_inode: u64,
    candidate_device: u64,
    candidate_inode: u64,
    relative_path: PathBuf,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn parse_quarantine_journal(quarantine: &fs::File) -> Result<QuarantineJournal> {
    use std::io::Read as _;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::MetadataExt as _;

    let mut journal =
        open_quarantine_file(quarantine, std::ffi::OsStr::new(RECLAIM_QUARANTINE_JOURNAL))?;
    let metadata = journal
        .metadata()
        .context("inspect cleanup quarantine journal")?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > 8192
    {
        bail!("cleanup quarantine journal is not a bounded private regular file");
    }
    let mut contents = String::with_capacity(metadata.len() as usize);
    journal
        .read_to_string(&mut contents)
        .context("read cleanup quarantine journal")?;
    let mut lines = contents.lines();
    if lines.next() != Some("velnor-reclaim-v2") {
        bail!("unknown cleanup quarantine journal version");
    }
    let mut next_identity = |field: &str| -> Result<u64> {
        lines
            .next()
            .with_context(|| format!("journal omitted {field}"))?
            .parse::<u64>()
            .with_context(|| format!("journal {field} is invalid"))
    };
    let anchor_device = next_identity("anchor device")?;
    let anchor_inode = next_identity("anchor inode")?;
    let source_parent_device = next_identity("source-parent device")?;
    let source_parent_inode = next_identity("source-parent inode")?;
    let candidate_device = next_identity("candidate device")?;
    let candidate_inode = next_identity("candidate inode")?;
    let path_hex = lines.next().context("journal omitted candidate path")?;
    if lines.next().is_some() {
        bail!("cleanup quarantine journal has trailing fields");
    }
    let path_bytes = hex_decode(path_hex)?;
    validate_quarantine_relative_path(&path_bytes)?;
    Ok(QuarantineJournal {
        anchor_device,
        anchor_inode,
        source_parent_device,
        source_parent_inode,
        candidate_device,
        candidate_inode,
        relative_path: PathBuf::from(std::ffi::OsString::from_vec(path_bytes)),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hex_decode(encoded: &str) -> Result<Vec<u8>> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
        bail!("cleanup quarantine journal path encoding is invalid");
    }
    let mut decoded = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().as_chunks::<2>().0 {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => bail!("cleanup quarantine journal path encoding is invalid"),
        };
        decoded.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    Ok(decoded)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_quarantine_relative_path(path: &[u8]) -> Result<()> {
    if path.is_empty() || path.starts_with(b"/") || path.contains(&0) {
        bail!("cleanup quarantine journal path is not relative and normalized");
    }
    let components = path.split(|byte| *byte == b'/').collect::<Vec<_>>();
    if components.is_empty()
        || components.len() > 32
        || components
            .iter()
            .any(|component| component.is_empty() || *component == b"." || *component == b"..")
        || std::str::from_utf8(components[components.len() - 1]).is_err()
    {
        bail!("cleanup quarantine journal path is not a normalized UTF-8 path");
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ensure_private_quarantine(quarantine: &fs::File) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = quarantine
        .metadata()
        .context("inspect cleanup quarantine")?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        bail!("cleanup quarantine is not private to the daemon account");
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sync_directory(directory: &fs::File) -> Result<()> {
    directory
        .sync_all()
        .context("persist cleanup directory transaction")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sync_two_directories_in_order(
    first: &fs::File,
    second: &fs::File,
    mut sync: impl FnMut(&fs::File) -> Result<()>,
) -> Result<()> {
    sync(first)?;
    sync(second)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn persist_quarantine_move(source_parent: &fs::File, quarantine: &fs::File) -> Result<()> {
    sync_two_directories_in_order(quarantine, source_parent, sync_directory)
        .context("persist quarantine destination before source removal")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn persist_quarantine_restore(quarantine: &fs::File, source_parent: &fs::File) -> Result<()> {
    sync_two_directories_in_order(source_parent, quarantine, sync_directory)
        .context("persist restored source before quarantine removal")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unlink_private_quarantine_file_if_present(
    quarantine: &fs::File,
    name: &std::ffi::OsStr,
) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::MetadataExt as _;

    let file = match open_quarantine_file(quarantine, name) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(())
        }
        Err(error) => return Err(error),
    };
    let metadata = file
        .metadata()
        .context("inspect private cleanup journal file")?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
    {
        bail!("refusing to remove an untrusted cleanup journal file");
    }
    let name = std::ffi::CString::new(name.as_bytes())?;
    // SAFETY: quarantine is live and name is one component.
    if unsafe { libc::unlinkat(quarantine.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error).context("remove cleanup quarantine journal file");
        }
    }
    sync_directory(quarantine)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_quarantine_record_and_directory(
    anchor: &fs::File,
    quarantine: &fs::File,
    name: &std::ffi::OsStr,
    expected_identity: &FilesystemDirectoryIdentity,
) -> Result<()> {
    ensure_private_quarantine(quarantine)?;
    if &directory_identity(quarantine)? != expected_identity {
        bail!("cleanup quarantine identity changed before transaction cleanup");
    }
    for entry in secure_directory_entries(quarantine)? {
        if entry.name == RECLAIM_QUARANTINE_ENTRY {
            bail!("refusing to remove cleanup quarantine while its workspace remains");
        }
        if entry.name != RECLAIM_QUARANTINE_JOURNAL && entry.name != RECLAIM_QUARANTINE_JOURNAL_TEMP
        {
            bail!("cleanup quarantine contains an unrecognized entry; freezing it");
        }
    }
    unlink_private_quarantine_file_if_present(
        quarantine,
        std::ffi::OsStr::new(RECLAIM_QUARANTINE_JOURNAL_TEMP),
    )?;
    unlink_private_quarantine_file_if_present(
        quarantine,
        std::ffi::OsStr::new(RECLAIM_QUARANTINE_JOURNAL),
    )?;
    remove_private_quarantine_checked(anchor, quarantine, name, expected_identity)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
fn restore_then_clear_quarantine(
    anchor: &fs::File,
    quarantine: &fs::File,
    quarantine_name: &std::ffi::OsStr,
    quarantine_identity: &FilesystemDirectoryIdentity,
    pinned_entry: &fs::File,
    entry_identity: &FilesystemDirectoryIdentity,
    source_parent: &fs::File,
    source_name: &std::ffi::OsStr,
) -> Result<()> {
    if &directory_identity(pinned_entry)? != entry_identity {
        bail!("refusing to restore a cleanup entry with uncertain identity");
    }
    restore_quarantined_entry(
        quarantine,
        std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY),
        source_parent,
        source_name,
        entry_identity,
    )?;
    remove_quarantine_record_and_directory(anchor, quarantine, quarantine_name, quarantine_identity)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn recover_private_quarantines_for_roots(work_roots: &[PathBuf], liveness: &WorkspaceLiveness) {
    if liveness.evidence_incomplete {
        return;
    }
    for work_root in work_roots {
        recover_private_quarantines_for_root(work_root, liveness, &|_| Ok(true));
    }
}

fn recover_workspace_private_quarantines_for_roots(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if liveness.evidence_incomplete {
            return;
        }
        let Some(layout) = crate::storage::selected_or_resolved_layout() else {
            eprintln!("leftover reclaim: cannot classify work-root cleanup quarantines without storage layout");
            return;
        };
        for work_root in work_roots {
            let allow_workspace = |relative_path: &Path| {
                crate::cache::is_catalog_cache_candidate_path_for_recovery(
                    work_root,
                    &layout,
                    relative_path,
                )
                .map(|is_cache_candidate| !is_cache_candidate)
            };
            recover_private_quarantines_for_root(work_root, liveness, &allow_workspace);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (work_roots, liveness);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recover_private_quarantines_for_root(
    work_root: &Path,
    liveness: &WorkspaceLiveness,
    allow_candidate: &impl Fn(&Path) -> Result<bool>,
) {
    if liveness.evidence_incomplete {
        return;
    }
    let work = match open_configured_directory_leaf_nofollow(work_root) {
        Ok(work) => work,
        Err(error) => {
            eprintln!(
                "leftover reclaim: cannot inspect recovery root {}: {error:#}",
                work_root.display()
            );
            return;
        }
    };
    let identity = match directory_identity(&work) {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!(
                "leftover reclaim: cannot pin recovery root {}: {error:#}",
                work_root.display()
            );
            return;
        }
    };
    recover_private_quarantines(&work, work_root, &identity, liveness, allow_candidate);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recover_private_quarantines(
    anchor: &fs::File,
    anchor_path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
    liveness: &WorkspaceLiveness,
    allow_candidate: &impl Fn(&Path) -> Result<bool>,
) {
    if liveness.evidence_incomplete {
        return;
    }
    let entries = match secure_directory_entries(anchor) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("leftover reclaim: cannot inspect quarantine entries: {error:#}");
            return;
        }
    };
    for entry in entries.into_iter().filter(|entry| {
        entry
            .name
            .to_str()
            .is_some_and(|name| name.starts_with(RECLAIM_QUARANTINE_PREFIX))
    }) {
        let path = anchor_path.join(&entry.name);
        let result = recover_one_private_quarantine(
            anchor,
            &entry,
            expected_anchor,
            liveness,
            allow_candidate,
        );
        if let Err(error) = result {
            eprintln!(
                "leftover reclaim: frozen cleanup quarantine {}: {error:#}",
                path.display()
            );
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recover_one_private_quarantine(
    anchor: &fs::File,
    entry: &SecureDirectoryEntry,
    expected_anchor: &FilesystemDirectoryIdentity,
    liveness: &WorkspaceLiveness,
    allow_candidate: &impl Fn(&Path) -> Result<bool>,
) -> Result<()> {
    let name_text = entry
        .name
        .to_str()
        .context("cleanup quarantine name is not UTF-8")?;
    let allocation = name_text
        .strip_prefix(RECLAIM_QUARANTINE_PREFIX)
        .context("invalid cleanup quarantine prefix")?;
    let (process, sequence) = allocation
        .split_once('-')
        .context("cleanup quarantine name has no process and sequence")?;
    if process.parse::<u32>().is_err() || sequence.parse::<u64>().is_err() {
        bail!("cleanup quarantine name is not an allocated Velnor quarantine");
    }
    if !entry.is_directory || entry.is_mountpoint {
        bail!("cleanup quarantine entry is not a same-mount directory");
    }
    let quarantine = open_directory_child(anchor, &entry.name)?;
    ensure_private_quarantine(&quarantine)?;
    let quarantine_identity = directory_identity(&quarantine)?;
    if quarantine_identity.device != expected_anchor.device
        || quarantine_identity.mount != expected_anchor.mount
    {
        bail!("cleanup quarantine crossed its trusted work-root mount");
    }
    let contents = secure_directory_entries(&quarantine)?;
    let has_journal = contents
        .iter()
        .any(|entry| entry.name == RECLAIM_QUARANTINE_JOURNAL);
    let has_temporary = contents
        .iter()
        .any(|entry| entry.name == RECLAIM_QUARANTINE_JOURNAL_TEMP);
    let has_entry = contents
        .iter()
        .any(|entry| entry.name == RECLAIM_QUARANTINE_ENTRY);
    if contents.iter().any(|entry| {
        entry.name != RECLAIM_QUARANTINE_JOURNAL
            && entry.name != RECLAIM_QUARANTINE_JOURNAL_TEMP
            && entry.name != RECLAIM_QUARANTINE_ENTRY
    }) {
        bail!("cleanup quarantine contains unrecognized entries");
    }
    if !has_journal {
        if has_entry {
            bail!("cleanup quarantine contains an unjournaled workspace; identity is uncertain");
        }
        if has_temporary {
            unlink_private_quarantine_file_if_present(
                &quarantine,
                std::ffi::OsStr::new(RECLAIM_QUARANTINE_JOURNAL_TEMP),
            )?;
        }
        remove_private_quarantine_checked(anchor, &quarantine, &entry.name, &quarantine_identity)?;
        return Ok(());
    }
    if has_temporary {
        bail!("cleanup quarantine has both published and temporary journals");
    }

    let journal = parse_quarantine_journal(&quarantine)?;
    if !allow_candidate(&journal.relative_path)? {
        return Ok(());
    }
    if journal.anchor_device != expected_anchor.device
        || journal.anchor_inode != expected_anchor.inode
    {
        bail!("cleanup quarantine anchor does not match its journaled identity");
    }
    let (source_parent, source_name) =
        open_quarantine_source_parent(anchor, &journal.relative_path, expected_anchor)?;
    let source_parent_identity = directory_identity(&source_parent)?;
    if source_parent_identity.device != journal.source_parent_device
        || source_parent_identity.inode != journal.source_parent_inode
        || source_parent_identity.mount != expected_anchor.mount
    {
        bail!("cleanup source parent does not match its journaled identity");
    }
    let job_id = source_name
        .to_str()
        .context("journaled workspace name is not UTF-8")?;
    if liveness.running.contains(job_id)
        || liveness.leased.contains(job_id)
        || liveness.claimed.contains(job_id)
    {
        bail!("journaled workspace is live; leaving its quarantine frozen");
    }
    let source_entries = secure_directory_entries(&source_parent)?;
    let source_entry = source_entries
        .iter()
        .find(|entry| entry.name == source_name);
    let source_identity = match source_entry {
        Some(entry) if entry.is_directory && !entry.is_mountpoint => {
            let directory = open_directory_child(&source_parent, &source_name)?;
            Some(directory_identity(&directory)?)
        }
        Some(_) => bail!("journaled workspace destination is occupied by an unexpected entry"),
        None => None,
    };
    let quarantined_entry = contents
        .iter()
        .find(|entry| entry.name == RECLAIM_QUARANTINE_ENTRY);
    let quarantined_directory = match quarantined_entry {
        Some(entry) if entry.is_directory && !entry.is_mountpoint => {
            let directory =
                open_directory_child(&quarantine, std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY))?;
            let identity = directory_identity(&directory)?;
            if identity.device != journal.candidate_device
                || identity.inode != journal.candidate_inode
                || identity.mount != expected_anchor.mount
            {
                bail!("quarantined workspace does not match its journaled identity");
            }
            Some(directory)
        }
        Some(_) => bail!("quarantine entry is not the journaled workspace directory"),
        None => None,
    };
    if quarantined_directory.is_some() && source_identity.is_some() {
        bail!("both quarantine and workspace names are occupied; freezing uncertain state");
    }
    if let Some(quarantined_directory) = quarantined_directory {
        let expected_identity = FilesystemEntryIdentity {
            device: journal.candidate_device,
            inode: journal.candidate_inode,
            mount: expected_anchor.mount.clone(),
        };
        restore_quarantined_entry(
            &quarantine,
            std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY),
            &source_parent,
            &source_name,
            &expected_identity,
        )?;
        if directory_identity(&quarantined_directory)? != expected_identity {
            bail!("restored workspace changed identity during recovery");
        }
    } else if let Some(identity) = source_identity
        && (identity.device != journal.candidate_device
            || identity.inode != journal.candidate_inode
            || identity.mount != expected_anchor.mount)
    {
        bail!("restored workspace does not match its journaled identity");
    }
    remove_quarantine_record_and_directory(anchor, &quarantine, &entry.name, &quarantine_identity)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_quarantine_source_parent(
    anchor: &fs::File,
    relative_path: &Path,
    expected_anchor: &FilesystemDirectoryIdentity,
) -> Result<(fs::File, std::ffi::OsString)> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::ffi::OsStringExt as _;

    let bytes = relative_path.as_os_str().as_bytes();
    validate_quarantine_relative_path(bytes)?;
    let mut components = bytes
        .split(|byte| *byte == b'/')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let name = std::ffi::OsString::from_vec(
        components
            .pop()
            .context("journaled workspace path has no final component")?,
    );
    let mut parent = open_directory_child(anchor, std::ffi::OsStr::new("."))?;
    for component in components {
        let child = std::ffi::OsString::from_vec(component);
        parent = open_directory_child(&parent, &child)
            .context("open journaled workspace parent without following links")?;
        let identity = directory_identity(&parent)?;
        if identity.device != expected_anchor.device || identity.mount != expected_anchor.mount {
            bail!("journaled workspace parent crosses its trusted work-root mount");
        }
    }
    Ok((parent, name))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_entry_noreplace(
    source_parent: &fs::File,
    source_name: &std::ffi::OsStr,
    destination_parent: &fs::File,
    destination_name: &std::ffi::OsStr,
) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let source_name = std::ffi::CString::new(source_name.as_bytes())?;
    let destination_name = std::ffi::CString::new(destination_name.as_bytes())?;
    #[cfg(target_os = "linux")]
    {
        // SAFETY: both live descriptors are directories, both names are
        // terminated single components, and RENAME_NOREPLACE forbids replace.
        let status = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                source_parent.as_raw_fd(),
                source_name.as_ptr(),
                destination_parent.as_raw_fd(),
                destination_name.as_ptr(),
                1_u32,
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error())
                .context("atomically quarantine cleanup entry");
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: both live descriptors are directories, both names are
        // terminated single components, and RENAME_EXCL forbids replacement.
        if unsafe {
            libc::renameatx_np(
                source_parent.as_raw_fd(),
                source_name.as_ptr(),
                destination_parent.as_raw_fd(),
                destination_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("atomically quarantine cleanup entry");
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_private_quarantine(anchor: &fs::File, name: &std::ffi::OsStr) -> Result<()> {
    let quarantine = open_directory_child(anchor, name)
        .context("reopen private cleanup quarantine before removal")?;
    let identity = directory_identity(&quarantine)?;
    remove_private_quarantine_checked(anchor, &quarantine, name, &identity)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_private_quarantine_checked(
    anchor: &fs::File,
    quarantine: &fs::File,
    name: &std::ffi::OsStr,
    expected_identity: &FilesystemDirectoryIdentity,
) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    ensure_private_quarantine(quarantine)?;
    if &directory_identity(quarantine)? != expected_identity {
        bail!("cleanup quarantine descriptor changed before removal");
    }
    let current = open_directory_child(anchor, name)
        .context("reopen cleanup quarantine entry before removal")?;
    if &directory_identity(&current)? != expected_identity {
        bail!("cleanup quarantine entry changed before removal");
    }
    if !secure_directory_entries(&current)?.is_empty() {
        bail!("refusing to remove non-empty cleanup quarantine");
    }
    let name = std::ffi::CString::new(name.as_bytes())?;
    // SAFETY: `anchor` is live and `name` is a single child component. The
    // descriptor and current entry were checked to be the same empty directory.
    if unsafe { libc::unlinkat(anchor.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        return Err(std::io::Error::last_os_error()).context("remove private cleanup quarantine");
    }
    sync_directory(anchor).context("persist removal of cleanup quarantine")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_private_quarantine_unrecorded(anchor: &fs::File, name: &std::ffi::OsStr) -> Result<()> {
    let quarantine = open_directory_child(anchor, name)
        .context("open unrecorded cleanup quarantine for rollback")?;
    ensure_private_quarantine(&quarantine)?;
    let entries = secure_directory_entries(&quarantine)?;
    if !entries.is_empty() {
        bail!("unrecorded cleanup quarantine is not empty; refusing cleanup");
    }
    let identity = directory_identity(&quarantine)?;
    remove_private_quarantine_checked(anchor, &quarantine, name, &identity)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn restore_quarantined_entry(
    quarantine: &fs::File,
    quarantine_name: &std::ffi::OsStr,
    source_parent: &fs::File,
    source_name: &std::ffi::OsStr,
    expected_identity: &FilesystemDirectoryIdentity,
) -> Result<()> {
    let pinned = open_directory_child(quarantine, quarantine_name)
        .context("open quarantined cleanup entry before restore")?;
    if &directory_identity(&pinned)? != expected_identity {
        bail!("quarantined cleanup entry identity changed before restore");
    }
    rename_entry_noreplace(quarantine, quarantine_name, source_parent, source_name)
        .context("restore preserved cleanup entry without replacing its source name")?;
    let restored =
        open_directory_child(source_parent, source_name).context("open restored cleanup entry")?;
    if &directory_identity(&restored)? != expected_identity {
        bail!("restored cleanup entry did not match its journaled identity");
    }
    persist_quarantine_restore(quarantine, source_parent)?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn move_candidate_to_quarantine(
    source_parent: &fs::File,
    source_name: &std::ffi::OsStr,
    quarantine: &fs::File,
    quarantine_name: &std::ffi::OsStr,
    expected_identity: &FilesystemDirectoryIdentity,
    pinned_candidate: &fs::File,
    before_move: impl FnOnce() -> Result<()>,
) -> Result<fs::File> {
    if &directory_identity(pinned_candidate)? != expected_identity {
        bail!("pinned cleanup candidate changed before quarantine move");
    }
    before_move()?;
    rename_entry_noreplace(source_parent, source_name, quarantine, quarantine_name)?;
    let moved = match open_directory_child(quarantine, quarantine_name) {
        Ok(moved) => moved,
        Err(error) => {
            restore_quarantined_entry(
                quarantine,
                quarantine_name,
                source_parent,
                source_name,
                expected_identity,
            )?;
            return Err(error).context("open quarantined candidate without following links");
        }
    };
    let moved_identity = match directory_identity(&moved) {
        Ok(identity) => identity,
        Err(error) => {
            restore_quarantined_entry(
                quarantine,
                quarantine_name,
                source_parent,
                source_name,
                expected_identity,
            )?;
            return Err(error).context("verify quarantined cleanup candidate identity");
        }
    };
    if moved_identity != *expected_identity {
        restore_quarantined_entry(
            quarantine,
            quarantine_name,
            source_parent,
            source_name,
            expected_identity,
        )?;
        bail!("quarantined cleanup candidate did not match its pinned descriptor");
    }
    Ok(moved)
}

fn open_parent_beneath_anchor(
    trusted_anchor: &Path,
    path: &Path,
    expected_anchor: Option<&FilesystemDirectoryIdentity>,
) -> Result<(
    fs::File,
    std::ffi::OsString,
    PathBuf,
    FilesystemDirectoryIdentity,
)> {
    use std::path::Component;

    if !trusted_anchor.is_absolute() || !path.is_absolute() {
        bail!("leftover cleanup requires absolute anchor and candidate paths");
    }
    let relative = path.strip_prefix(trusted_anchor).with_context(|| {
        format!(
            "leftover candidate {} is outside trusted anchor {}",
            path.display(),
            trusted_anchor.display()
        )
    })?;
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => components.push(name.to_os_string()),
            Component::CurDir => {}
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "leftover candidate path is not normalized: {}",
                    path.display()
                );
            }
        }
    }
    let name = components
        .pop()
        .context("leftover workspace path has no final component")?;
    let mut parent = open_configured_directory(trusted_anchor)?;
    let anchor_identity = directory_identity(&parent)?;
    if expected_anchor.is_some_and(|expected| expected != &anchor_identity) {
        bail!("trusted cleanup anchor changed since workspace discovery");
    }
    for component in components {
        parent = open_directory_child(&parent, &component).with_context(|| {
            format!(
                "open leftover ancestor below trusted anchor {}",
                trusted_anchor.display()
            )
        })?;
        let opened_identity = directory_identity(&parent)?;
        if opened_identity.device != anchor_identity.device
            || opened_identity.mount != anchor_identity.mount
        {
            bail!("leftover path crosses a mount boundary below its trusted anchor");
        }
    }
    Ok((parent, name, path.to_path_buf(), anchor_identity))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FilesystemMountIdentity {
    #[cfg(target_os = "linux")]
    LinuxMountId(u64),
    #[cfg(target_os = "macos")]
    MacOs(MacOsMountIdentity),
}

#[cfg(target_os = "macos")]
/// macOS filesystem identity. Equality compares both `f_fsid` and
/// `f_mntonname`, so a same-device mounted subtree does not match its parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MacOsMountIdentity {
    fsid: Vec<u8>,
    mountpoint: PathBuf,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MacOsDirectoryEntry {
    pub(crate) name: std::ffi::OsString,
    pub(crate) is_directory: bool,
    pub(crate) is_mountpoint: bool,
}

#[cfg(target_os = "linux")]
#[cfg_attr(not(test), allow(dead_code))]
fn remove_dir_all_with_identity(
    path: &Path,
    expected_device: u64,
    expected_root_mount_id: Option<u64>,
    identity_of: &impl Fn(&Path, u64, u64) -> (u64, u64),
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let anchor_path = Path::new("/");
        let anchor = open_configured_directory(anchor_path)?;
        let anchor_identity = directory_identity(&anchor)?;
        let (parent, name, root_path, _) =
            open_parent_beneath_anchor(anchor_path, path, Some(&anchor_identity))?;
        let source_relative_path = path
            .strip_prefix(anchor_path)
            .context("leftover workspace is outside its trusted cleanup anchor")?;
        remove_dir_all_with_identity_at(
            &parent,
            &name,
            &anchor,
            &anchor_identity,
            &root_path,
            source_relative_path,
            expected_device,
            expected_root_mount_id,
            None,
            None,
            identity_of,
            &|_| Ok(()),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, expected_device, expected_root_mount_id, identity_of);
        bail!("refusing leftover cleanup without Linux mount-identity proof")
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn remove_dir_all_with_identity_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    quarantine_anchor: &fs::File,
    expected_anchor: &FilesystemDirectoryIdentity,
    root_path: &Path,
    source_relative_path: &Path,
    expected_device: u64,
    expected_root_mount_id: Option<u64>,
    expected_candidate: Option<&FilesystemDirectoryIdentity>,
    pinned_candidate: Option<&fs::File>,
    identity_of: &impl Fn(&Path, u64, u64) -> (u64, u64),
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    let anchor_mount_id = match &expected_anchor.mount {
        FilesystemMountIdentity::LinuxMountId(mount_id) => *mount_id,
        #[allow(unreachable_patterns)]
        _ => bail!("refusing Linux cleanup with non-Linux anchor identity"),
    };
    let parent_mount_id = mount_id_at(parent, Path::new(""), rustix::fs::AtFlags::EMPTY_PATH)?;
    if parent_mount_id != anchor_mount_id {
        bail!("leftover workspace parent crossed the trusted anchor mount");
    }
    let root_stat = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)
        .with_context(|| format!("inspect leftover workspace {}", root_path.display()))?;
    if rustix::fs::FileType::from_raw_mode(root_stat.st_mode) != rustix::fs::FileType::Directory {
        bail!(
            "leftover workspace is not a directory: {}",
            root_path.display()
        );
    }
    let root_mount = mount_id_at(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
    if expected_root_mount_id.is_some_and(|expected| expected != root_mount)
        || root_mount != anchor_mount_id
        || root_stat.st_dev != expected_device
    {
        bail!(
            "leftover workspace changed filesystem mount: {}",
            root_path.display()
        );
    }
    let source_candidate = open_root_directory(parent, name, root_path, &root_stat, root_mount)?;
    let source_identity = directory_identity(&source_candidate)?;
    if expected_candidate.is_some_and(|expected| expected != &source_identity) {
        bail!(
            "leftover workspace changed since inventory: {}",
            root_path.display()
        );
    }
    if let Some(pinned) = pinned_candidate
        && directory_identity(pinned)? != source_identity
    {
        bail!(
            "leftover workspace descriptor changed since inventory: {}",
            root_path.display()
        );
    }
    let root_identity = ensure_identity(
        root_path,
        root_stat.st_dev,
        root_mount,
        expected_device,
        expected_root_mount_id,
        identity_of,
    )?;
    let (quarantine, quarantine_name) = create_private_quarantine_with_journal(
        quarantine_anchor,
        expected_anchor,
        source_relative_path,
        parent,
        &source_identity,
    )?;
    let quarantine_identity = directory_identity(&quarantine)?;
    let quarantined_name = std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY);
    sync_directory(quarantine_anchor).context("persist cleanup quarantine directory entry")?;
    let quarantined_root = match move_candidate_to_quarantine(
        parent,
        name,
        &quarantine,
        quarantined_name,
        &source_identity,
        pinned_candidate.unwrap_or(&source_candidate),
        || Ok(()),
    ) {
        Ok(root) => root,
        Err(error) => {
            let cleanup = remove_quarantine_record_and_directory(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
            );
            return Err(error).context(format!(
                "move authorized workspace {} into quarantine (cleanup: {cleanup:?})",
                root_path.display()
            ));
        }
    };
    if let Err(error) = persist_quarantine_move(parent, &quarantine) {
        let restore = restore_then_clear_quarantine(
            quarantine_anchor,
            &quarantine,
            &quarantine_name,
            &quarantine_identity,
            &quarantined_root,
            &source_identity,
            parent,
            name,
        );
        return Err(error).context(format!("persist quarantine move (restore: {restore:?})"));
    }
    let quarantined_stat = match rustix::fs::statat(
        &quarantine,
        quarantined_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) => stat,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(std::io::Error::from(error)).context(format!(
                "inspect quarantined cleanup entry (restore: {restore:?})"
            ));
        }
    };
    let quarantined_mount = match mount_id_at(
        &quarantine,
        quarantined_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(mount) => mount,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(error).context(format!("inspect quarantined mount (restore: {restore:?})"));
        }
    };
    let mut preflight_deletion_started = false;
    let preflight = walk_directory_tree(
        &quarantined_root,
        root_path,
        expected_device,
        root_identity.1,
        identity_of,
        0,
        false,
        &mut preflight_deletion_started,
        &|_| Ok(()),
    );
    if let Err(error) = preflight {
        let restore = restore_then_clear_quarantine(
            quarantine_anchor,
            &quarantine,
            &quarantine_name,
            &quarantine_identity,
            &quarantined_root,
            &source_identity,
            parent,
            name,
        );
        return Err(error).context(format!(
            "preflight quarantined cleanup tree (restore: {restore:?})"
        ));
    }
    let deletion_root = match open_root_directory(
        &quarantine,
        quarantined_name,
        root_path,
        &quarantined_stat,
        quarantined_mount,
    ) {
        Ok(root) => root,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(error).context(format!("reopen quarantined entry (restore: {restore:?})"));
        }
    };
    let deletion_identity = match directory_identity(&deletion_root) {
        Ok(identity) => identity,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(error).context(format!(
                "verify quarantined cleanup entry before deletion (restore: {restore:?})"
            ));
        }
    };
    if deletion_identity != source_identity {
        bail!(
            "quarantined workspace changed before deletion: {}",
            root_path.display()
        );
    }
    let mut deletion_started = false;
    if let Err(error) = walk_directory_tree(
        &deletion_root,
        root_path,
        expected_device,
        root_identity.1,
        identity_of,
        0,
        true,
        &mut deletion_started,
        before_unlink,
    ) {
        let restore = restore_then_clear_quarantine(
            quarantine_anchor,
            &quarantine,
            &quarantine_name,
            &quarantine_identity,
            &quarantined_root,
            &source_identity,
            parent,
            name,
        );
        return Err(error).context(format!(
            "delete quarantined workspace tree (partial={deletion_started}, restore: {restore:?})"
        ));
    }
    drop(deletion_root);
    drop(quarantined_root);
    if let Err(error) = unlink_checked_directory(
        &quarantine,
        quarantined_name,
        root_path,
        &quarantined_stat,
        expected_device,
        root_identity.1,
        identity_of,
        before_unlink,
    ) {
        let entry = open_directory_child(&quarantine, quarantined_name);
        let restore = match entry {
            Ok(entry) => restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &entry,
                &source_identity,
                parent,
                name,
            ),
            Err(_) => remove_quarantine_record_and_directory(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
            ),
        };
        return Err(error).context(format!(
            "remove quarantined workspace root (restore: {restore:?})"
        ));
    }
    remove_quarantine_record_and_directory(
        quarantine_anchor,
        &quarantine,
        &quarantine_name,
        &quarantine_identity,
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
/// Mount identity read from the already-open directory descriptor.
pub(crate) fn macos_directory_mount_identity(directory: &fs::File) -> Result<MacOsMountIdentity> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStringExt as _;

    let mut stats = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `stats` points to writable storage for libc::statfs, and the
    // descriptor remains alive for the duration of the call.
    if unsafe { libc::fstatfs(directory.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("read macOS mount identity");
    }
    // SAFETY: fstatfs initialized the complete struct on success.
    let stats = unsafe { stats.assume_init() };
    let mountpoint_end = stats
        .f_mntonname
        .iter()
        .position(|byte| *byte == 0)
        .context("macOS fstatfs returned an unterminated mountpoint")?;
    let mountpoint = PathBuf::from(std::ffi::OsString::from_vec(
        stats.f_mntonname[..mountpoint_end]
            .iter()
            .map(|byte| *byte as u8)
            .collect(),
    ));
    if !mountpoint.is_absolute() {
        bail!("macOS fstatfs returned a non-absolute mountpoint");
    }
    let fsid_size = std::mem::size_of::<libc::fsid_t>();
    if fsid_size != 8 {
        bail!("unexpected macOS filesystem identity size {fsid_size}");
    }
    let mut fsid = vec![0_u8; fsid_size];
    // SAFETY: f_fsid is initialized by fstatfs and this copies exactly its
    // ABI-sized object representation into byte storage.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&stats.f_fsid as *const libc::fsid_t).cast::<u8>(),
            fsid.as_mut_ptr(),
            fsid_size,
        );
    }
    Ok(MacOsMountIdentity { fsid, mountpoint })
}

#[cfg(target_os = "macos")]
/// Enumerate one directory with `getattrlistbulk`; pass a freshly opened FD.
/// This consumes its bulk offset and must not be mixed with `readdir`.
pub(crate) fn macos_bulk_directory_entries(
    directory: &fs::File,
) -> Result<Vec<MacOsDirectoryEntry>> {
    let mut entries = Vec::new();
    macos_for_each_bulk_directory_entry(directory, |entry| {
        entries.push(entry);
        Ok(())
    })?;
    Ok(entries)
}

#[cfg(target_os = "macos")]
fn macos_for_each_bulk_directory_entry(
    directory: &fs::File,
    mut visit: impl FnMut(MacOsDirectoryEntry) -> Result<()>,
) -> Result<()> {
    use std::os::fd::AsRawFd as _;

    const ATTR_CMN_ERROR_LOCAL: u32 = 0x2000_0000;
    const MAX_DIRECTORY_ENTRIES: usize = 1_000_000;
    let initial_offset = unsafe { libc::lseek(directory.as_raw_fd(), 0, libc::SEEK_CUR) };
    if initial_offset != 0 {
        bail!("macOS bulk directory scan requires a fresh descriptor at offset zero");
    }
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | libc::ATTR_CMN_NAME
            | ATTR_CMN_ERROR_LOCAL
            | libc::ATTR_CMN_OBJTYPE,
        volattr: 0,
        dirattr: libc::ATTR_DIR_MOUNTSTATUS,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = vec![0_u64; 4096];
    let mut total_entries = 0_usize;
    loop {
        // SAFETY: the attrlist matches the SDK ABI, the buffer is writable and
        // 8-byte aligned, and its byte length is passed to the kernel.
        let count = unsafe {
            libc::getattrlistbulk(
                directory.as_raw_fd(),
                (&mut attributes as *mut libc::attrlist).cast(),
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(buffer.as_slice()),
                libc::FSOPT_PACK_INVAL_ATTRS as u64,
            )
        };
        if count < 0 {
            return Err(std::io::Error::last_os_error())
                .context("enumerate macOS directory mount attributes");
        }
        if count == 0 {
            return Ok(());
        }
        let count = count as usize;
        total_entries = total_entries.saturating_add(count);
        if total_entries > MAX_DIRECTORY_ENTRIES {
            bail!("macOS directory exceeds secure cleanup entry limit");
        }
        // SAFETY: the buffer is initialized, contiguous byte storage and was
        // filled by getattrlistbulk before parsing.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>(),
                std::mem::size_of_val(buffer.as_slice()),
            )
        };
        for entry in parse_macos_bulk_entries(bytes, count)? {
            visit(entry)?;
        }
    }
}

#[cfg(target_os = "macos")]
fn parse_macos_bulk_entries(buffer: &[u8], count: usize) -> Result<Vec<MacOsDirectoryEntry>> {
    use std::os::unix::ffi::OsStringExt as _;

    const ATTR_CMN_ERROR_LOCAL: u32 = 0x2000_0000;
    const MACOS_VDIR: u32 = 2;
    const COMMON_ATTRIBUTES_SIZE: usize = 5 * std::mem::size_of::<u32>();
    const MIN_RECORD_SIZE: usize = std::mem::size_of::<u32>()
        + COMMON_ATTRIBUTES_SIZE
        + std::mem::size_of::<u32>()
        + 2 * std::mem::size_of::<u32>()
        + std::mem::size_of::<u32>()
        + std::mem::size_of::<u32>();

    fn read_u32(buffer: &[u8], offset: usize, end: usize, field: &str) -> Result<u32> {
        let field_end = offset
            .checked_add(std::mem::size_of::<u32>())
            .context("macOS directory attribute offset overflow")?;
        if field_end > end {
            bail!("macOS directory attribute record is truncated at {field}");
        }
        Ok(u32::from_ne_bytes(
            buffer[offset..field_end]
                .try_into()
                .context("read macOS directory attribute word")?,
        ))
    }

    let mut entries = Vec::with_capacity(count.min(4096));
    let mut cursor = 0_usize;
    for _ in 0..count {
        let record_length = read_u32(buffer, cursor, buffer.len(), "record length")? as usize;
        if record_length < MIN_RECORD_SIZE {
            bail!("macOS directory attribute record is too short");
        }
        let record_end = cursor
            .checked_add(record_length)
            .context("macOS directory record length overflow")?;
        if record_end > buffer.len() {
            bail!("macOS directory attribute record exceeds returned buffer");
        }

        let returned_common = read_u32(buffer, cursor + 4, record_end, "returned common attrs")?;
        let returned_directory =
            read_u32(buffer, cursor + 12, record_end, "returned directory attrs")?;
        if returned_common & (libc::ATTR_CMN_NAME | ATTR_CMN_ERROR_LOCAL | libc::ATTR_CMN_OBJTYPE)
            != (libc::ATTR_CMN_NAME | ATTR_CMN_ERROR_LOCAL | libc::ATTR_CMN_OBJTYPE)
        {
            bail!("macOS did not return required common directory attributes");
        }
        let entry_error = read_u32(
            buffer,
            cursor + 4 + COMMON_ATTRIBUTES_SIZE,
            record_end,
            "per-entry error",
        )?;
        if entry_error != 0 {
            return Err(std::io::Error::from_raw_os_error(entry_error as i32))
                .context("macOS could not attest one directory entry");
        }

        let name_reference_offset = cursor + 4 + COMMON_ATTRIBUTES_SIZE + 4;
        let data_offset = read_u32(
            buffer,
            name_reference_offset,
            record_end,
            "name reference offset",
        )? as i32;
        let name_length = read_u32(
            buffer,
            name_reference_offset + 4,
            record_end,
            "name reference length",
        )? as usize;
        if name_length == 0 || name_length > 4096 {
            bail!("macOS directory entry has an invalid name length");
        }
        let name_start_signed = (name_reference_offset as i64)
            .checked_add(i64::from(data_offset))
            .context("macOS directory name offset overflow")?;
        if name_start_signed < cursor as i64 {
            bail!("macOS directory name points before its attribute record");
        }
        let name_start = name_start_signed as usize;
        let name_end = name_start
            .checked_add(name_length)
            .context("macOS directory name length overflow")?;
        if name_end > record_end {
            bail!("macOS directory name exceeds its attribute record");
        }
        let encoded_name = &buffer[name_start..name_end];
        if encoded_name.last() != Some(&0) || encoded_name[..name_length - 1].contains(&0) {
            bail!("macOS directory name is not a single terminated name");
        }
        let name = std::ffi::OsString::from_vec(encoded_name[..name_length - 1].to_vec());
        if name.is_empty() || name == "." || name == ".." || name.as_encoded_bytes().contains(&b'/')
        {
            bail!("macOS returned an unsafe directory entry name");
        }

        let object_type = read_u32(buffer, name_reference_offset + 8, record_end, "object type")?;
        let is_directory = object_type == MACOS_VDIR;
        let is_mountpoint = if is_directory {
            if returned_directory & libc::ATTR_DIR_MOUNTSTATUS == 0 {
                bail!("macOS omitted directory mount status");
            }
            let status = read_u32(
                buffer,
                name_reference_offset + 12,
                record_end,
                "directory mount status",
            )?;
            if status & !libc::DIR_MNTSTATUS_MNTPOINT != 0 {
                bail!("macOS reported an unsupported directory mount status");
            }
            status & libc::DIR_MNTSTATUS_MNTPOINT != 0
        } else {
            false
        };
        entries.push(MacOsDirectoryEntry {
            name,
            is_directory,
            is_mountpoint,
        });
        cursor = record_end;
    }
    Ok(entries)
}

#[cfg(target_os = "macos")]
impl MacOsDirectoryEntry {
    fn require_same_mount(&self, path: &Path) -> Result<()> {
        if self.is_mountpoint {
            bail!(
                "leftover workspace crosses mount boundary at {}",
                path.display()
            );
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn remove_dir_all_macos_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    quarantine_anchor: &fs::File,
    expected_anchor: &FilesystemDirectoryIdentity,
    root_path: &Path,
    source_relative_path: &Path,
    expected_device: u64,
    expected_mount: &MacOsMountIdentity,
    expected_candidate: Option<&FilesystemDirectoryIdentity>,
    pinned_candidate: Option<&fs::File>,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    let parent_stat = macos_stat_fd(parent)?;
    if parent_stat.st_dev as u64 != expected_device
        || macos_directory_mount_identity(parent)? != *expected_mount
    {
        bail!("leftover workspace parent changed filesystem mount");
    }
    let root_entry = macos_bulk_directory_entries(parent)?
        .into_iter()
        .find(|entry| entry.name == name)
        .context("macOS bulk listing omitted leftover workspace")?;
    root_entry.require_same_mount(root_path)?;
    if !root_entry.is_directory {
        bail!(
            "leftover workspace is not a directory: {}",
            root_path.display()
        );
    }
    let root_stat = macos_stat_at(parent, name, root_path)?;
    if !macos_stat_is_directory(&root_stat) || root_stat.st_dev as u64 != expected_device {
        bail!(
            "leftover workspace changed filesystem: {}",
            root_path.display()
        );
    }
    let preflight_root = open_macos_directory_at(
        parent,
        name,
        root_path,
        &root_stat,
        expected_device,
        expected_mount,
    )?;
    let source_identity = directory_identity(&preflight_root)?;
    if expected_candidate.is_some_and(|expected| expected != &source_identity) {
        bail!(
            "leftover workspace changed since inventory: {}",
            root_path.display()
        );
    }
    if let Some(pinned) = pinned_candidate
        && directory_identity(pinned)? != source_identity
    {
        bail!(
            "leftover workspace descriptor changed since inventory: {}",
            root_path.display()
        );
    }
    let (quarantine, quarantine_name) = create_private_quarantine_with_journal(
        quarantine_anchor,
        expected_anchor,
        source_relative_path,
        parent,
        &source_identity,
    )?;
    let quarantine_identity = directory_identity(&quarantine)?;
    let quarantined_name = std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY);
    sync_directory(quarantine_anchor).context("persist cleanup quarantine directory entry")?;
    let quarantined_root = match move_candidate_to_quarantine(
        parent,
        name,
        &quarantine,
        quarantined_name,
        &source_identity,
        pinned_candidate.unwrap_or(&preflight_root),
        || Ok(()),
    ) {
        Ok(root) => root,
        Err(error) => {
            let cleanup = remove_quarantine_record_and_directory(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
            );
            return Err(error).context(format!(
                "move authorized workspace {} into quarantine (cleanup: {cleanup:?})",
                root_path.display()
            ));
        }
    };
    if let Err(error) = persist_quarantine_move(parent, &quarantine) {
        let restore = restore_then_clear_quarantine(
            quarantine_anchor,
            &quarantine,
            &quarantine_name,
            &quarantine_identity,
            &quarantined_root,
            &source_identity,
            parent,
            name,
        );
        return Err(error).context(format!("persist quarantine move (restore: {restore:?})"));
    }
    let quarantined_stat = match macos_stat_at(&quarantine, quarantined_name, root_path) {
        Ok(stat) => stat,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(error).context(format!(
                "inspect quarantined macOS cleanup entry (restore: {restore:?})"
            ));
        }
    };
    let quarantined_identity = match directory_identity(&quarantined_root) {
        Ok(identity) => identity,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(error).context(format!(
                "verify quarantined macOS cleanup entry (restore: {restore:?})"
            ));
        }
    };
    if quarantined_identity != source_identity {
        bail!(
            "quarantined workspace did not match pinned inventory: {}",
            root_path.display()
        );
    }
    let mut preflight_mutated = false;
    if let Err(error) = walk_macos_directory_tree(
        &quarantined_root,
        root_path,
        expected_device,
        expected_mount,
        0,
        false,
        &mut preflight_mutated,
        &|_| Ok(()),
    ) {
        let restore = restore_then_clear_quarantine(
            quarantine_anchor,
            &quarantine,
            &quarantine_name,
            &quarantine_identity,
            &quarantined_root,
            &source_identity,
            parent,
            name,
        );
        return Err(error).context(format!(
            "preflight quarantined macOS cleanup tree (restore: {restore:?})"
        ));
    }
    let deletion_root = match open_macos_directory_at(
        &quarantine,
        quarantined_name,
        root_path,
        &quarantined_stat,
        expected_device,
        expected_mount,
    ) {
        Ok(root) => root,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(error).context(format!(
                "reopen quarantined macOS cleanup entry (restore: {restore:?})"
            ));
        }
    };
    let deletion_identity = match directory_identity(&deletion_root) {
        Ok(identity) => identity,
        Err(error) => {
            let restore = restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            );
            return Err(error).context(format!(
                "verify quarantined macOS cleanup entry before deletion (restore: {restore:?})"
            ));
        }
    };
    if deletion_identity != source_identity {
        bail!(
            "quarantined workspace changed before deletion: {}",
            root_path.display()
        );
    }
    let mut deletion_started = false;
    if let Err(error) = walk_macos_directory_tree(
        &deletion_root,
        root_path,
        expected_device,
        expected_mount,
        0,
        true,
        &mut deletion_started,
        before_unlink,
    ) {
        return finish_macos_delete_error(error, deletion_started, || {
            restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &quarantined_root,
                &source_identity,
                parent,
                name,
            )
        });
    }
    drop(deletion_root);
    drop(quarantined_root);
    if let Err(error) = unlink_macos_directory(
        &quarantine,
        quarantined_name,
        root_path,
        &quarantined_stat,
        expected_device,
        Some(&source_identity),
        before_unlink,
    ) {
        let entry = open_directory_child(&quarantine, quarantined_name);
        let restore = match entry {
            Ok(entry) => restore_then_clear_quarantine(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
                &entry,
                &source_identity,
                parent,
                name,
            ),
            Err(_) => remove_quarantine_record_and_directory(
                quarantine_anchor,
                &quarantine,
                &quarantine_name,
                &quarantine_identity,
            ),
        };
        return Err(error).context(format!(
            "remove quarantined workspace root (restore: {restore:?})"
        ));
    }
    remove_quarantine_record_and_directory(
        quarantine_anchor,
        &quarantine,
        &quarantine_name,
        &quarantine_identity,
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn finish_macos_delete_error(
    error: anyhow::Error,
    deletion_started: bool,
    restore_unchanged_candidate: impl FnOnce() -> Result<()>,
) -> Result<()> {
    restore_unchanged_candidate().context("restore preserved quarantined macOS cleanup entry")?;
    let context = if deletion_started {
        "delete macOS workspace tree after partial cleanup; remaining tree restored"
    } else {
        "delete quarantined macOS workspace tree"
    };
    Err(error).context(context)
}

#[cfg(target_os = "macos")]
fn open_macos_absolute_directory(path: &Path) -> Result<fs::File> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Component;

    if !path.is_absolute() {
        bail!(
            "leftover workspace parent is not absolute: {}",
            path.display()
        );
    }
    let root = c"/";
    // SAFETY: `root` is a valid terminated path string.
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("open filesystem root for cleanup");
    }
    // SAFETY: open returned a new owned descriptor.
    let mut directory = unsafe { fs::File::from_raw_fd(root_fd) };
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                let name = std::ffi::CString::new(name.as_bytes())
                    .context("leftover workspace parent contains nul")?;
                // SAFETY: the parent descriptor is live and name is terminated.
                let child_fd = unsafe {
                    libc::openat(
                        std::os::fd::AsRawFd::as_raw_fd(&directory),
                        name.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                };
                if child_fd < 0 {
                    return Err(std::io::Error::last_os_error()).with_context(|| {
                        format!("open leftover workspace parent {}", path.display())
                    });
                }
                // SAFETY: openat returned a new owned descriptor.
                directory = unsafe { fs::File::from_raw_fd(child_fd) };
            }
            Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "leftover workspace parent is not normalized: {}",
                    path.display()
                );
            }
        }
    }
    Ok(directory)
}

#[cfg(target_os = "macos")]
fn macos_stat_fd(directory: &fs::File) -> Result<libc::stat> {
    use std::os::fd::AsRawFd as _;

    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `stat` points to writable storage and the descriptor is live.
    if unsafe { libc::fstat(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("inspect leftover directory fd");
    }
    // SAFETY: fstat initialized the complete struct on success.
    Ok(unsafe { stat.assume_init() })
}

#[cfg(target_os = "macos")]
fn macos_stat_at(parent: &fs::File, name: &std::ffi::OsStr, path: &Path) -> Result<libc::stat> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let name = std::ffi::CString::new(name.as_bytes())
        .with_context(|| format!("leftover path contains nul: {}", path.display()))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: the parent descriptor and terminated name are valid; stat is
    // writable storage for the returned no-follow identity.
    if unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("inspect leftover entry {}", path.display()));
    }
    // SAFETY: fstatat initialized the complete struct on success.
    Ok(unsafe { stat.assume_init() })
}

#[cfg(target_os = "macos")]
fn macos_stat_is_directory(stat: &libc::stat) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFDIR
}

#[cfg(target_os = "macos")]
fn open_macos_directory_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &libc::stat,
    expected_device: u64,
    expected_mount: &MacOsMountIdentity,
) -> Result<fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    let name = std::ffi::CString::new(name.as_bytes())
        .with_context(|| format!("leftover path contains nul: {}", path.display()))?;
    // SAFETY: the parent descriptor is live and name is terminated.
    let child_fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if child_fd < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "open leftover directory without following links {}",
                path.display()
            )
        });
    }
    // SAFETY: openat returned a new owned descriptor.
    let child = unsafe { fs::File::from_raw_fd(child_fd) };
    let opened = macos_stat_fd(&child)?;
    if !macos_stat_is_directory(&opened)
        || opened.st_dev != expected_stat.st_dev
        || opened.st_ino != expected_stat.st_ino
        || opened.st_dev as u64 != expected_device
    {
        bail!(
            "leftover directory changed during secure open: {}",
            path.display()
        );
    }
    if macos_directory_mount_identity(&child)? != *expected_mount {
        bail!(
            "leftover directory changed mount during secure open: {}",
            path.display()
        );
    }
    Ok(child)
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn walk_macos_directory_tree(
    directory: &fs::File,
    path: &Path,
    expected_device: u64,
    expected_mount: &MacOsMountIdentity,
    depth: usize,
    remove: bool,
    deletion_started: &mut bool,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    const MAX_DEPTH: usize = 256;
    if depth > MAX_DEPTH {
        bail!(
            "leftover workspace exceeds secure cleanup depth {MAX_DEPTH}: {}",
            path.display()
        );
    }
    let opened = macos_stat_fd(directory)?;
    if !macos_stat_is_directory(&opened)
        || opened.st_dev as u64 != expected_device
        || macos_directory_mount_identity(directory)? != *expected_mount
    {
        bail!(
            "leftover directory changed filesystem mount: {}",
            path.display()
        );
    }
    macos_for_each_bulk_directory_entry(directory, |entry| {
        let child_path = path.join(&entry.name);
        entry.require_same_mount(&child_path)?;
        let child_stat = macos_stat_at(directory, &entry.name, &child_path)?;
        if child_stat.st_dev as u64 != expected_device {
            bail!(
                "leftover workspace crosses filesystem boundary at {}",
                child_path.display()
            );
        }
        if entry.is_directory {
            if !macos_stat_is_directory(&child_stat) {
                bail!("leftover directory changed type: {}", child_path.display());
            }
            let child = open_macos_directory_at(
                directory,
                &entry.name,
                &child_path,
                &child_stat,
                expected_device,
                expected_mount,
            )?;
            let child_identity = directory_identity(&child)?;
            walk_macos_directory_tree(
                &child,
                &child_path,
                expected_device,
                expected_mount,
                depth + 1,
                remove,
                deletion_started,
                before_unlink,
            )?;
            if remove {
                unlink_macos_directory(
                    directory,
                    &entry.name,
                    &child_path,
                    &child_stat,
                    expected_device,
                    Some(&child_identity),
                    before_unlink,
                )?;
                *deletion_started = true;
            }
        } else if remove {
            unlink_macos_entry(
                directory,
                &entry.name,
                &child_path,
                &child_stat,
                expected_device,
                before_unlink,
            )?;
            *deletion_started = true;
        }
        Ok(())
    })
}

#[cfg(target_os = "macos")]
fn unlink_macos_entry(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &libc::stat,
    expected_device: u64,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let current = macos_stat_at(parent, name, path)?;
    if current.st_dev != expected_stat.st_dev
        || current.st_ino != expected_stat.st_ino
        || current.st_mode & libc::S_IFMT != expected_stat.st_mode & libc::S_IFMT
        || current.st_dev as u64 != expected_device
    {
        bail!("leftover entry changed before removal: {}", path.display());
    }
    let name_cstring = std::ffi::CString::new(name.as_bytes())
        .with_context(|| format!("leftover path contains nul: {}", path.display()))?;
    before_unlink(path)?;
    let after = macos_stat_at(parent, name, path)?;
    if after.st_dev != expected_stat.st_dev
        || after.st_ino != expected_stat.st_ino
        || after.st_mode & libc::S_IFMT != expected_stat.st_mode & libc::S_IFMT
        || after.st_dev as u64 != expected_device
    {
        bail!("leftover entry changed before removal: {}", path.display());
    }
    // SAFETY: the parent descriptor is live and name is terminated.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name_cstring.as_ptr(), 0) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("remove leftover entry {}", path.display()));
    }
    sync_directory(parent).context("persist removal of leftover entry")
}

#[cfg(target_os = "macos")]
fn reopen_macos_directory_for_bulk_scan(directory: &fs::File) -> Result<fs::File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let expected_identity = directory_identity(directory)?;
    let dot = c".";
    // SAFETY: `directory` is a live directory descriptor and `dot` is a
    // terminated single-component relative path. A new open description gives
    // getattrlistbulk an independent offset while staying anchored to the fd.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .context("reopen macOS directory for a fresh bulk scan");
    }
    // SAFETY: openat returned a new owned descriptor.
    let reopened = unsafe { fs::File::from_raw_fd(descriptor) };
    if directory_identity(&reopened)? != expected_identity {
        bail!("macOS parent directory changed while opening a fresh bulk-scan descriptor");
    }
    Ok(reopened)
}

#[cfg(target_os = "macos")]
fn unlink_macos_directory(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &libc::stat,
    expected_device: u64,
    expected_candidate: Option<&FilesystemDirectoryIdentity>,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::ffi::OsStrExt as _;

    let parent_identity = directory_identity(parent)?;
    if parent_identity.device != expected_device {
        bail!("leftover workspace parent changed filesystem before removal");
    }
    let fresh_parent = reopen_macos_directory_for_bulk_scan(parent)?;
    if directory_identity(&fresh_parent)? != parent_identity {
        bail!("leftover workspace parent changed before directory unlink");
    }
    let parent_mount = macos_directory_mount_identity(&fresh_parent)?;
    let entry = macos_bulk_directory_entries(&fresh_parent)?
        .into_iter()
        .find(|entry| entry.name == name)
        .context("macOS bulk listing omitted leftover workspace before unlink")?;
    entry.require_same_mount(path)?;
    if !entry.is_directory {
        bail!(
            "leftover workspace changed type before removal: {}",
            path.display()
        );
    }
    let current = macos_stat_at(&fresh_parent, name, path)?;
    if !macos_stat_is_directory(&current)
        || current.st_dev != expected_stat.st_dev
        || current.st_ino != expected_stat.st_ino
        || current.st_dev as u64 != expected_device
    {
        bail!(
            "leftover directory changed before removal: {}",
            path.display()
        );
    }
    let opened_child = open_macos_directory_at(
        &fresh_parent,
        name,
        path,
        &current,
        expected_device,
        &parent_mount,
    )?;
    let opened_identity = directory_identity(&opened_child)?;
    if expected_candidate.is_some_and(|expected| expected != &opened_identity) {
        bail!(
            "leftover directory identity changed before removal: {}",
            path.display()
        );
    }
    let final_stat = macos_stat_at(&fresh_parent, name, path)?;
    if !macos_stat_is_directory(&final_stat)
        || final_stat.st_dev as u64 != opened_identity.device
        || final_stat.st_ino as u64 != opened_identity.inode
    {
        bail!(
            "leftover directory changed after secure open: {}",
            path.display()
        );
    }
    if directory_identity(&fresh_parent)? != parent_identity
        || macos_directory_mount_identity(&opened_child)? != parent_mount
    {
        bail!(
            "leftover directory parent or mount changed before removal: {}",
            path.display()
        );
    }
    let name_cstring = std::ffi::CString::new(name.as_bytes())
        .with_context(|| format!("leftover path contains nul: {}", path.display()))?;
    before_unlink(path)?;
    // `getattrlistbulk` consumes this open description's directory offset.
    // Reopen from the pinned parent after the hook so the post-hook scan starts
    // at offset zero; otherwise a partial-delete rollback can mistake an
    // exhausted scan for an empty quarantine and lose the remaining tree.
    let post_parent = reopen_macos_directory_for_bulk_scan(parent)?;
    if directory_identity(&post_parent)? != parent_identity
        || macos_directory_mount_identity(&post_parent)? != parent_mount
    {
        bail!(
            "leftover workspace parent or mount changed before removal: {}",
            path.display()
        );
    }
    let post_entry = macos_bulk_directory_entries(&post_parent)?
        .into_iter()
        .find(|entry| entry.name == name)
        .context("macOS bulk listing omitted leftover workspace after pre-unlink hook")?;
    post_entry.require_same_mount(path)?;
    if !post_entry.is_directory {
        bail!(
            "leftover workspace changed type before removal: {}",
            path.display()
        );
    }
    let post_stat = macos_stat_at(&post_parent, name, path)?;
    if !macos_stat_is_directory(&post_stat)
        || post_stat.st_dev != expected_stat.st_dev
        || post_stat.st_ino != expected_stat.st_ino
        || post_stat.st_dev as u64 != expected_device
    {
        bail!(
            "leftover directory changed before removal: {}",
            path.display()
        );
    }
    let post_child = open_macos_directory_at(
        &post_parent,
        name,
        path,
        &post_stat,
        expected_device,
        &parent_mount,
    )?;
    if directory_identity(&post_child)? != opened_identity
        || directory_identity(&post_parent)? != parent_identity
        || macos_directory_mount_identity(&post_child)? != parent_mount
    {
        bail!(
            "leftover directory or parent changed before removal: {}",
            path.display()
        );
    }
    // SAFETY: the fresh parent descriptor is live and name is terminated.
    if unsafe {
        libc::unlinkat(
            post_parent.as_raw_fd(),
            name_cstring.as_ptr(),
            libc::AT_REMOVEDIR,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("remove leftover directory {}", path.display()));
    }
    sync_directory(&post_parent).context("persist removal of leftover directory")
}

#[cfg(target_os = "linux")]
fn open_absolute_directory(path: &Path) -> Result<fs::File> {
    use std::path::Component;

    if !path.is_absolute() {
        bail!(
            "leftover workspace parent is not absolute: {}",
            path.display()
        );
    }
    let mut directory: fs::File = rustix::fs::openat(
        rustix::fs::CWD,
        Path::new("/"),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .context("open filesystem root for leftover cleanup")?
    .into();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                let child = rustix::fs::openat(
                    &directory,
                    name,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(std::io::Error::from)
                .with_context(|| format!("open leftover workspace parent {}", path.display()))?;
                directory = child.into();
            }
            Component::ParentDir | Component::Prefix(_) => {
                bail!(
                    "leftover workspace parent is not normalized: {}",
                    path.display()
                );
            }
        }
    }
    Ok(directory)
}

#[cfg(target_os = "linux")]
fn mount_id_at(
    directory: &fs::File,
    path: impl rustix::path::Arg,
    flags: rustix::fs::AtFlags,
) -> Result<u64> {
    let stat = rustix::fs::statx(
        directory,
        path,
        flags,
        rustix::fs::StatxFlags::BASIC_STATS | rustix::fs::StatxFlags::MNT_ID,
    )
    .map_err(std::io::Error::from)
    .context("read mount identity for leftover cleanup")?;
    let required_mask = rustix::fs::StatxFlags::MNT_ID.bits();
    if stat.stx_mask & required_mask != required_mask {
        bail!("filesystem does not provide mount identity for leftover cleanup");
    }
    Ok(stat.stx_mnt_id)
}

#[cfg(target_os = "linux")]
pub(crate) fn filesystem_mount_id(path: &Path) -> Option<u64> {
    let mut probe = path;
    loop {
        match fs::metadata(probe) {
            Ok(_) => {
                let stat = rustix::fs::statx(
                    rustix::fs::CWD,
                    probe,
                    rustix::fs::AtFlags::empty(),
                    rustix::fs::StatxFlags::MNT_ID,
                )
                .ok()?;
                let required_mask = rustix::fs::StatxFlags::MNT_ID.bits();
                return (stat.stx_mask & required_mask == required_mask).then_some(stat.stx_mnt_id);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                probe = probe.parent()?;
            }
            Err(_) => return None,
        }
    }
}

#[allow(dead_code)]
pub(crate) fn filesystem_mount_identity(path: &Path) -> Option<FilesystemMountIdentity> {
    #[cfg(target_os = "linux")]
    {
        filesystem_mount_id(path).map(FilesystemMountIdentity::LinuxMountId)
    }
    #[cfg(target_os = "macos")]
    {
        let mut probe = path;
        loop {
            match fs::metadata(probe) {
                Ok(metadata) if metadata.is_dir() => {
                    let directory = fs::File::open(probe).ok()?;
                    return macos_directory_mount_identity(&directory)
                        .ok()
                        .map(FilesystemMountIdentity::MacOs);
                }
                Ok(_) => probe = probe.parent()?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    probe = probe.parent()?;
                }
                Err(_) => return None,
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        None
    }
}

#[cfg(target_os = "linux")]
fn ensure_identity(
    path: &Path,
    actual_device: u64,
    actual_mount_id: u64,
    expected_device: u64,
    expected_mount_id: Option<u64>,
    identity_of: &impl Fn(&Path, u64, u64) -> (u64, u64),
) -> Result<(u64, u64)> {
    let (device, mount_id) = identity_of(path, actual_device, actual_mount_id);
    if device != expected_device {
        bail!(
            "leftover workspace crosses filesystem boundary at {} (device {device}, expected {expected_device})",
            path.display()
        );
    }
    if expected_mount_id.is_some_and(|expected| mount_id != expected) {
        bail!(
            "leftover workspace crosses mount boundary at {} (mount {mount_id}, expected {})",
            path.display(),
            expected_mount_id.unwrap_or_default()
        );
    }
    Ok((device, mount_id))
}

#[cfg(target_os = "linux")]
fn open_child_directory(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &rustix::fs::Stat,
    expected_mount_id: u64,
) -> Result<fs::File> {
    let child = rustix::fs::openat2(
        parent,
        Path::new(name),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
        rustix::fs::ResolveFlags::BENEATH
            | rustix::fs::ResolveFlags::NO_XDEV
            | rustix::fs::ResolveFlags::NO_SYMLINKS
            | rustix::fs::ResolveFlags::NO_MAGICLINKS,
    )
    .map_err(std::io::Error::from)
    .with_context(|| {
        format!(
            "open leftover directory without crossing mounts {}",
            path.display()
        )
    })?;
    let child: fs::File = child.into();
    let opened = rustix::fs::fstat(&child)
        .map_err(std::io::Error::from)
        .with_context(|| format!("inspect opened leftover directory {}", path.display()))?;
    if opened.st_dev != expected_stat.st_dev
        || opened.st_ino != expected_stat.st_ino
        || rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::Directory
    {
        bail!(
            "leftover directory changed during secure open: {}",
            path.display()
        );
    }
    let opened_mount = mount_id_at(&child, Path::new(""), rustix::fs::AtFlags::EMPTY_PATH)?;
    if opened_mount != expected_mount_id {
        bail!(
            "leftover directory changed mount during secure open: {}",
            path.display()
        );
    }
    Ok(child)
}

#[cfg(target_os = "linux")]
fn open_root_directory(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &rustix::fs::Stat,
    expected_mount_id: u64,
) -> Result<fs::File> {
    let root = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .with_context(|| {
        format!(
            "open leftover workspace without following links {}",
            path.display()
        )
    })?;
    let root: fs::File = root.into();
    let opened = rustix::fs::fstat(&root)
        .map_err(std::io::Error::from)
        .with_context(|| format!("inspect opened leftover workspace {}", path.display()))?;
    let opened_mount = mount_id_at(&root, Path::new(""), rustix::fs::AtFlags::EMPTY_PATH)?;
    if opened.st_dev != expected_stat.st_dev
        || opened.st_ino != expected_stat.st_ino
        || opened_mount != expected_mount_id
        || rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::Directory
    {
        bail!(
            "leftover workspace changed during secure open: {}",
            path.display()
        );
    }
    Ok(root)
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn walk_directory_tree(
    directory: &fs::File,
    path: &Path,
    expected_device: u64,
    expected_mount_id: u64,
    identity_of: &impl Fn(&Path, u64, u64) -> (u64, u64),
    depth: usize,
    remove: bool,
    deletion_started: &mut bool,
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    use std::os::unix::ffi::OsStringExt as _;

    const MAX_DEPTH: usize = 256;
    if depth > MAX_DEPTH {
        bail!(
            "leftover workspace exceeds secure cleanup depth {MAX_DEPTH}: {}",
            path.display()
        );
    }
    let entries = rustix::fs::Dir::read_from(directory)
        .map_err(std::io::Error::from)
        .with_context(|| format!("read leftover workspace {}", path.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(std::io::Error::from)
            .with_context(|| format!("read leftover workspace {}", path.display()))?;
        let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if name == "." || name == ".." {
            continue;
        }
        let child_path = path.join(&name);
        let stat = match rustix::fs::statat(directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(error) => {
                return Err(std::io::Error::from(error))
                    .with_context(|| format!("inspect leftover entry {}", child_path.display()));
            }
        };
        let child_mount = mount_id_at(directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
        ensure_identity(
            &child_path,
            stat.st_dev,
            child_mount,
            expected_device,
            Some(expected_mount_id),
            identity_of,
        )?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Directory {
            let child = open_child_directory(directory, &name, &child_path, &stat, child_mount)?;
            let opened = rustix::fs::fstat(&child)
                .map_err(std::io::Error::from)
                .with_context(|| format!("inspect leftover directory {}", child_path.display()))?;
            let opened_mount = mount_id_at(&child, Path::new(""), rustix::fs::AtFlags::EMPTY_PATH)?;
            ensure_identity(
                &child_path,
                opened.st_dev,
                opened_mount,
                expected_device,
                Some(expected_mount_id),
                identity_of,
            )?;
            walk_directory_tree(
                &child,
                &child_path,
                expected_device,
                expected_mount_id,
                identity_of,
                depth + 1,
                remove,
                deletion_started,
                before_unlink,
            )?;
            if remove {
                unlink_checked_directory(
                    directory,
                    &name,
                    &child_path,
                    &stat,
                    expected_device,
                    expected_mount_id,
                    identity_of,
                    before_unlink,
                )?;
                *deletion_started = true;
            }
        } else {
            if remove {
                unlink_checked_entry(
                    directory,
                    &name,
                    &child_path,
                    &stat,
                    expected_device,
                    expected_mount_id,
                    identity_of,
                    before_unlink,
                )?;
                *deletion_started = true;
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn checked_current_stat(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &rustix::fs::Stat,
    expected_device: u64,
    expected_mount_id: u64,
    identity_of: &impl Fn(&Path, u64, u64) -> (u64, u64),
) -> Result<()> {
    let current = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)
        .with_context(|| format!("recheck leftover entry {}", path.display()))?;
    let current_mount = mount_id_at(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)?;
    ensure_identity(
        path,
        current.st_dev,
        current_mount,
        expected_device,
        Some(expected_mount_id),
        identity_of,
    )?;
    if current.st_dev != expected_stat.st_dev
        || current.st_ino != expected_stat.st_ino
        || rustix::fs::FileType::from_raw_mode(current.st_mode)
            != rustix::fs::FileType::from_raw_mode(expected_stat.st_mode)
    {
        bail!("leftover entry changed before removal: {}", path.display());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn unlink_checked_entry(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &rustix::fs::Stat,
    expected_device: u64,
    expected_mount_id: u64,
    identity_of: &impl Fn(&Path, u64, u64) -> (u64, u64),
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    checked_current_stat(
        parent,
        name,
        path,
        expected_stat,
        expected_device,
        expected_mount_id,
        identity_of,
    )?;
    before_unlink(path)?;
    checked_current_stat(
        parent,
        name,
        path,
        expected_stat,
        expected_device,
        expected_mount_id,
        identity_of,
    )?;
    rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty())
        .map_err(std::io::Error::from)
        .with_context(|| format!("remove leftover entry {}", path.display()))?;
    sync_directory(parent).context("persist removal of leftover entry")
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn unlink_checked_directory(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    path: &Path,
    expected_stat: &rustix::fs::Stat,
    expected_device: u64,
    expected_mount_id: u64,
    identity_of: &impl Fn(&Path, u64, u64) -> (u64, u64),
    before_unlink: &impl Fn(&Path) -> Result<()>,
) -> Result<()> {
    checked_current_stat(
        parent,
        name,
        path,
        expected_stat,
        expected_device,
        expected_mount_id,
        identity_of,
    )?;
    before_unlink(path)?;
    checked_current_stat(
        parent,
        name,
        path,
        expected_stat,
        expected_device,
        expected_mount_id,
        identity_of,
    )?;
    rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::REMOVEDIR)
        .map_err(std::io::Error::from)
        .with_context(|| format!("remove leftover directory {}", path.display()))?;
    sync_directory(parent).context("persist removal of leftover directory")
}

pub fn live_job_ids_from_host_docker() -> Result<BTreeSet<String>> {
    let listed = crate::docker::client::host_call(&list_live_job_names_args())?;
    Ok(live_job_ids_from_docker_ps(&listed))
}

/// List live job IDs from host Docker only when that backend is selected.
/// Missing selection and `microvm` return an empty set and never open the socket.
pub fn live_job_ids_for_reclaim(
    backend: Option<velnor_model::ExecutionBackendKind>,
) -> Result<BTreeSet<String>> {
    if velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend) {
        live_job_ids_from_host_docker()
    } else {
        Ok(BTreeSet::new())
    }
}

/// Recover only durable cleanup transactions during ordinary daemon polls.
///
/// A pressure-triggered reclaim may leave a quarantine when the process exits
/// after a durable rename or partial unlink. Pressure can clear before the
/// next poll, so recovery must run independently of the destructive reclaim
/// path. The filesystem coordinator and complete liveness snapshot still fence
/// restoration against a workspace that has become live again.
pub(crate) fn recover_production_quarantines_for_roots(
    backend: Option<velnor_model::ExecutionBackendKind>,
    work_roots: &[PathBuf],
) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let layout = crate::storage::selected_or_resolved_layout()
            .context("resolve storage layout for cleanup recovery")?;
        recover_quarantines_for_backend(
            backend,
            &layout.run_root,
            work_roots,
            &layout,
            live_job_ids_from_host_docker,
        )?;
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (backend, work_roots);
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recover_quarantines_for_backend(
    backend: Option<velnor_model::ExecutionBackendKind>,
    run_root: &Path,
    work_roots: &[PathBuf],
    layout: &crate::storage::StorageLayout,
    live_job_ids: impl FnOnce() -> Result<BTreeSet<String>>,
) -> Result<()> {
    // Cache transactions are independent of Docker/workspace liveness. Recover
    // them first so a Docker outage cannot strand already-deleted cache data.
    recover_cache_quarantines_if_pending(run_root, layout, work_roots)?;
    if velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend) {
        recover_workspace_quarantines_if_pending(run_root, work_roots, live_job_ids)?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn private_quarantine_cache_roots(layout: &crate::storage::StorageLayout) -> Vec<PathBuf> {
    let mut roots = vec![layout.cache_root.clone()];
    roots.push(crate::trust_scope::filesystem_key_namespace(
        &layout.cache_root,
    ));
    roots.sort();
    roots.dedup();
    roots
}

/// Recover cache-root transactions while the caller holds the filesystem
/// coordinator. Lease and claim evidence protects in-use entries; Docker
/// liveness is unnecessary because these paths are catalogued cache stores.
pub(crate) fn recover_cache_quarantines_under_coordinator(
    run_root: &Path,
    layout: &crate::storage::StorageLayout,
    work_roots: &[PathBuf],
) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let cache_roots = private_quarantine_cache_roots(layout);
        let mut recovery_roots = cache_roots.clone();
        recovery_roots.extend(work_roots.iter().cloned());
        if !work_roots_have_private_quarantines(&recovery_roots)? {
            return Ok(());
        }
        let liveness = WorkspaceLiveness::collect(run_root, BTreeSet::new());
        if liveness.evidence_incomplete {
            bail!("cannot recover cache quarantines with incomplete lease or claim evidence");
        }
        for cache_root in &cache_roots {
            recover_private_quarantines_for_root(cache_root, &liveness, &|_| Ok(true));
        }
        for work_root in work_roots {
            let allow_cache_candidate = |relative_path: &Path| {
                crate::cache::is_catalog_cache_candidate_path_for_recovery(
                    work_root,
                    layout,
                    relative_path,
                )
            };
            recover_private_quarantines_for_root(work_root, &liveness, &allow_cache_candidate);
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (run_root, layout, work_roots);
    Ok(())
}

pub(crate) fn recover_cache_quarantines_if_pending(
    run_root: &Path,
    layout: &crate::storage::StorageLayout,
    work_roots: &[PathBuf],
) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut recovery_roots = private_quarantine_cache_roots(layout);
        recovery_roots.extend(work_roots.iter().cloned());
        if !work_roots_have_private_quarantines(&recovery_roots)? {
            Ok(())
        } else {
            let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(run_root)?;
            recover_cache_quarantines_under_coordinator(run_root, layout, work_roots)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (run_root, layout, work_roots);
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recover_workspace_quarantines_if_pending(
    run_root: &Path,
    work_roots: &[PathBuf],
    live_job_ids: impl FnOnce() -> Result<BTreeSet<String>>,
) -> Result<()> {
    if !work_roots_have_private_quarantines(work_roots)? {
        return Ok(());
    }
    let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(run_root)?;
    let running = live_job_ids().context("collect running jobs for workspace recovery")?;
    let liveness = WorkspaceLiveness::collect(run_root, running);
    if liveness.evidence_incomplete {
        bail!("cannot recover workspace quarantines with incomplete liveness evidence");
    }
    recover_workspace_private_quarantines_for_roots(work_roots, &liveness);
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn recover_private_quarantines_if_pending(
    run_root: &Path,
    work_roots: &[PathBuf],
    live_job_ids: impl FnOnce() -> Result<BTreeSet<String>>,
) -> Result<()> {
    if !work_roots_have_private_quarantines(work_roots)? {
        return Ok(());
    }

    // Hold the coordinator before collecting either Docker or filesystem
    // liveness, then keep it through recovery. This matches workspace reclaim's
    // lease publication fence and prevents recovery from racing a new claim.
    let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(run_root)?;
    let running = live_job_ids().context("collect running jobs for cleanup recovery")?;
    let liveness = WorkspaceLiveness::collect(run_root, running);
    if liveness.evidence_incomplete {
        bail!("cannot recover cleanup quarantines with incomplete liveness evidence");
    }
    recover_private_quarantines_for_roots(work_roots, &liveness);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn work_roots_have_private_quarantines(work_roots: &[PathBuf]) -> Result<bool> {
    for work_root in work_roots {
        let anchor = match open_configured_directory_leaf_nofollow(work_root) {
            Ok(anchor) => anchor,
            Err(error) if is_not_found_error(&error) => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect cleanup recovery root {}", work_root.display())
                });
            }
        };
        let entries = secure_directory_entries(&anchor)
            .with_context(|| format!("list cleanup recovery root {}", work_root.display()))?;
        if entries.iter().any(|entry| {
            entry
                .name
                .to_str()
                .is_some_and(|name| name.starts_with(RECLAIM_QUARANTINE_PREFIX))
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn reclaim_production_leftovers_for_roots_with_pin(
    backend: Option<velnor_model::ExecutionBackendKind>,
    work_roots: &[PathBuf],
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    minimum_free_bytes: u64,
) -> Result<LeftoverReclaimReport> {
    reclaim_production_leftovers_for_roots_with_pressure(
        backend,
        work_roots,
        expected_pressure,
        move |capacity| capacity.available_bytes < minimum_free_bytes,
    )
}

pub(crate) fn reclaim_production_leftovers_for_roots_with_usage_pressure_pin(
    backend: Option<velnor_model::ExecutionBackendKind>,
    work_roots: &[PathBuf],
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    hard_used_percent: u8,
) -> Result<LeftoverReclaimReport> {
    if hard_used_percent > 100 {
        bail!("invalid hard utilization threshold: {hard_used_percent}%");
    }
    reclaim_production_leftovers_for_roots_with_pressure(
        backend,
        work_roots,
        expected_pressure,
        move |capacity| capacity.used_percent() >= hard_used_percent,
    )
}

fn reclaim_production_leftovers_for_roots_with_pressure(
    backend: Option<velnor_model::ExecutionBackendKind>,
    work_roots: &[PathBuf],
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    pressure_predicate: impl Fn(&crate::host_capacity::HostCapacity) -> bool + Copy,
) -> Result<LeftoverReclaimReport> {
    if !velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend) {
        return Ok(LeftoverReclaimReport::default());
    }
    let expected_volume_uuid = pressure_volume_uuid(expected_pressure)?;
    let pressure_capacity =
        || pressure_sample_matches_pin(expected_pressure, &expected_volume_uuid);
    if !pressure_predicate(&pressure_capacity()?) {
        return Ok(LeftoverReclaimReport::default());
    }
    let pressure_device = expected_pressure.device_id();
    let live = match live_job_ids_from_host_docker() {
        Ok(ids) => ids,
        Err(error) => {
            eprintln!("leftover workspace reclaim skipped (cannot list live jobs): {error:#}");
            return Ok(LeftoverReclaimReport {
                skipped_docker: true,
                ..LeftoverReclaimReport::default()
            });
        }
    };
    let pressure_recovered = std::cell::Cell::new(false);
    let pressure_invalid = std::cell::Cell::new(false);
    let still_pressured = || {
        if pressure_recovered.get() || pressure_invalid.get() {
            return Ok(false);
        }
        match pressure_capacity() {
            Ok(capacity) => {
                let active = pressure_predicate(&capacity);
                if !active {
                    pressure_recovered.set(true);
                }
                Ok(active)
            }
            Err(error) => {
                pressure_invalid.set(true);
                Err(error)
            }
        }
    };
    reclaim_leftover_after_velnor_authorized_while_pressured(
        work_roots,
        &live,
        still_pressured,
        |workspace| {
            remove_authorized_workspace_for_pressure(workspace, pressure_device, &|| {
                if pressure_recovered.get() || pressure_invalid.get() {
                    bail!("workspace pressure was no longer valid before unlink");
                }
                match pressure_capacity() {
                    Ok(capacity) if pressure_predicate(&capacity) => Ok(()),
                    Ok(_) => {
                        pressure_recovered.set(true);
                        bail!("workspace pressure cleared before unlink")
                    }
                    Err(error) => {
                        pressure_invalid.set(true);
                        Err(error).context("revalidate pressure filesystem before unlink")
                    }
                }
            })
        },
    )
}

fn pressure_volume_uuid(pressure: &crate::host_capacity::HostCapacityPin) -> Result<String> {
    let capacity = pressure
        .probe()
        .context("probe pinned pressure filesystem")?;
    if capacity.filesystem_device != pressure.device_id() {
        bail!("pinned pressure filesystem device changed during capacity probe");
    }
    capacity
        .volume_fingerprint
        .filter(|uuid| !uuid.is_empty())
        .context("pinned pressure filesystem has no stable UUID")
}

fn pressure_sample_matches_pin(
    expected_pressure: &crate::host_capacity::HostCapacityPin,
    expected_volume_uuid: &str,
) -> Result<crate::host_capacity::HostCapacity> {
    let capacity = expected_pressure
        .probe()
        .context("probe pinned pressure filesystem")?;
    if capacity.filesystem_device != expected_pressure.device_id()
        || capacity.volume_fingerprint.as_deref() != Some(expected_volume_uuid)
    {
        bail!("pinned pressure filesystem UUID or device changed");
    }
    Ok(capacity)
}

fn reclaim_leftover_after_velnor_authorized_while_pressured(
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
    still_pressured: impl FnMut() -> Result<bool>,
    remove_dir: impl FnMut(&AuthorizedWorkspace) -> Result<()>,
) -> Result<LeftoverReclaimReport> {
    match runtime_root() {
        Some(run_root) => {
            let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root)?;
            reclaim_with_liveness_authorized_while_pressured(
                work_roots,
                &WorkspaceLiveness::collect(&run_root, live_job_ids.clone()),
                still_pressured,
                remove_dir,
            )
        }
        None => reclaim_with_liveness_authorized_while_pressured(
            work_roots,
            &WorkspaceLiveness {
                running: live_job_ids.clone(),
                min_idle: WORKSPACE_MIN_IDLE,
                ..WorkspaceLiveness::default()
            },
            still_pressured,
            remove_dir,
        ),
    }
}

#[cfg(test)]
fn reclaim_with_liveness_authorized_until_free_space(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
    minimum_free_bytes: u64,
    mut available_bytes: impl FnMut() -> Result<u64>,
    remove_dir: impl FnMut(&AuthorizedWorkspace) -> Result<()>,
) -> Result<LeftoverReclaimReport> {
    reclaim_with_liveness_authorized_while_pressured(
        work_roots,
        liveness,
        || Ok(available_bytes()? < minimum_free_bytes),
        remove_dir,
    )
}

fn reclaim_with_liveness_authorized_while_pressured(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
    mut still_pressured: impl FnMut() -> Result<bool>,
    mut remove_dir: impl FnMut(&AuthorizedWorkspace) -> Result<()>,
) -> Result<LeftoverReclaimReport> {
    let mut report = LeftoverReclaimReport::default();
    recover_workspace_private_quarantines_for_roots(work_roots, liveness);
    let orphans = authorized_orphan_workspaces_with_liveness(work_roots, liveness);

    // Inventory and liveness checks can take time. Sample the pinned
    // filesystem after them, immediately before the first possible deletion.
    if !still_pressured()? {
        return Ok(report);
    }

    for workspace in orphans {
        if !still_pressured()? {
            break;
        }
        match remove_dir(&workspace) {
            Ok(()) => report.deleted_workspaces.push(workspace.path),
            Err(error) => {
                eprintln!(
                    "leftover workspace reclaim failed for {}: {error:#}",
                    workspace.path.display()
                );
            }
        }

        // A fresh sample after each candidate attempt both decides whether
        // more space is needed and becomes the pre-delete sample for the next
        // candidate.
        if !still_pressured()? {
            break;
        }
    }
    Ok(report)
}

fn remove_authorized_workspace(
    workspace: &AuthorizedWorkspace,
    expected_device: Option<u64>,
) -> Result<()> {
    remove_dir_all_on_device_under_identities(
        &workspace.trusted_anchor,
        &workspace.path,
        expected_device.unwrap_or(workspace.anchor_identity.device),
        &workspace.anchor_identity,
        &workspace.candidate_identity,
    )
}

fn remove_authorized_workspace_for_pressure(
    workspace: &AuthorizedWorkspace,
    pressure_device: u64,
    validate_pressure: &impl Fn() -> Result<()>,
) -> Result<()> {
    if workspace.anchor_identity.device != pressure_device
        || workspace.candidate_identity.device != pressure_device
    {
        bail!(
            "skip leftover workspace {}: its pinned filesystem does not match pressure device {pressure_device}",
            workspace.path.display()
        );
    }
    remove_dir_all_on_device_under_identities_with_pre_unlink(
        &workspace.trusted_anchor,
        &workspace.path,
        pressure_device,
        &workspace.anchor_identity,
        &workspace.candidate_identity,
        &|_| validate_pressure(),
    )
}

#[cfg(test)]
fn reclaim_production_leftovers_for_roots_with(
    backend: Option<velnor_model::ExecutionBackendKind>,
    work_roots: &[PathBuf],
    prune_dangling_images: bool,
    live_job_ids: impl FnOnce() -> Result<BTreeSet<String>>,
    docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&Path) -> Result<()>,
) -> Result<LeftoverReclaimReport> {
    if !velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend) {
        return reclaim_microvm_leftovers();
    }
    let live = match live_job_ids() {
        Ok(ids) => ids,
        Err(error) => {
            eprintln!("leftover workspace reclaim skipped (cannot list live jobs): {error:#}");
            return Ok(LeftoverReclaimReport {
                skipped_docker: true,
                ..LeftoverReclaimReport::default()
            });
        }
    };
    reclaim_leftover_after_velnor(work_roots, &live, docker, remove_dir, prune_dangling_images)
}

/// Reclaim leftover workspaces for a caller that already holds the filesystem
/// coordinator of `run_root` (the destructive `cache gc` path, which takes it
/// before its own eviction pass and keeps it through this reclaim).
pub fn reclaim_production_leftovers_under_coordinator(
    coordinator: &crate::capacity::FilesystemCoordinator,
    run_root: &Path,
    work_roots: &[PathBuf],
    backend: velnor_model::ExecutionBackendKind,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    if !backend.uses_host_docker_socket() {
        return reclaim_microvm_leftovers();
    }
    let live = match live_job_ids_from_host_docker() {
        Ok(ids) => ids,
        Err(error) => {
            eprintln!("leftover workspace reclaim skipped (cannot list live jobs): {error:#}");
            return Ok(LeftoverReclaimReport {
                skipped_docker: true,
                ..LeftoverReclaimReport::default()
            });
        }
    };
    reclaim_leftover_under_coordinator_authorized(
        coordinator,
        run_root,
        work_roots,
        &live,
        host_docker_if_safe,
        |workspace| remove_authorized_workspace(workspace, None),
        prune_dangling_images,
    )
}

fn reclaim_microvm_leftovers() -> Result<LeftoverReclaimReport> {
    // Work roots are shared by all daemon pools. MicroVM selection alone does
    // not identify ownership, so deleting every UUID absent from the current
    // pool's live set can remove an active Docker job from another pool.
    // Leave reclamation to the coordinator until it supplies a pool-scoped
    // ownership lease.
    Ok(LeftoverReclaimReport::default())
}

/// Injectable hard-pressure reclaim. Host Docker listing and prune run only
/// when the selected backend permits host Docker maintenance.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reclaim_production_if_hard_pressure_with(
    backend: Option<velnor_model::ExecutionBackendKind>,
    usage_percent: u8,
    work_roots: &[PathBuf],
    mut docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&Path) -> Result<()>,
) -> Result<LeftoverReclaimReport> {
    if usage_percent < HARD_PRESSURE_PERCENT {
        return Ok(LeftoverReclaimReport::default());
    }
    if !velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend) {
        // A backend choice is not an ownership proof: `work_roots` can contain
        // active jobs belonging to another daemon/pool. Fail closed until the
        // cross-daemon coordinator exposes pool-scoped leases here.
        let _ = (work_roots, remove_dir);
        return Ok(LeftoverReclaimReport::default());
    }
    let live = match docker(&list_live_job_names_args()) {
        Ok(listed) => live_job_ids_from_docker_ps(&listed),
        Err(error) => {
            eprintln!("leftover workspace reclaim skipped (cannot list live jobs): {error:#}");
            return Ok(LeftoverReclaimReport {
                skipped_docker: true,
                ..LeftoverReclaimReport::default()
            });
        }
    };
    reclaim_if_hard_pressure(usage_percent, work_roots, &live, docker, remove_dir)
}

fn host_docker_if_safe(args: &[String]) -> Result<String> {
    if leftover_docker_args_are_unsafe(args) {
        bail!("refusing unsafe docker reclaim {args:?}");
    }
    crate::docker::client::host_call(args)
}

fn leftover_docker_args_are_unsafe(args: &[String]) -> bool {
    let first = args.first().map(String::as_str);
    let second = args.get(1).map(String::as_str);
    first == Some("system")
        || (first == Some("volume") && second == Some("prune"))
        || first == Some("builder")
        || (first == Some("image")
            && second == Some("prune")
            && args.iter().any(|arg| arg == "-a" || arg == "--all"))
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
    use std::sync::{Arc, Mutex};

    fn write_tree(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("marker"), b"job").unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn empty_directory_removal_uses_pinned_parent_and_quarantine() {
        let root = std::env::temp_dir().join(format!(
            "velnor-empty-directory-quarantine-{}",
            uuid::Uuid::new_v4()
        ));
        let anchor = root.join("cache");
        let parent_path = anchor.join("admission-leases");
        let candidate_path = parent_path.join("namespace");
        fs::create_dir_all(&candidate_path).unwrap();
        let anchor_identity = filesystem_directory_identity(&anchor).unwrap();
        let (pinned_parent, parent_identity) =
            filesystem_pin_directory_under(&anchor, &parent_path, &anchor_identity).unwrap();
        let (pinned_candidate, candidate_identity) =
            filesystem_pin_directory_under(&anchor, &candidate_path, &anchor_identity).unwrap();

        assert!(remove_empty_directory_under_pinned_parent(
            &anchor,
            &candidate_path,
            &anchor_identity,
            &parent_identity,
            &pinned_parent,
            &candidate_identity,
            &pinned_candidate,
        )
        .unwrap());
        assert!(!candidate_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn empty_directory_quarantine_preserves_inode_replacement() {
        let root = std::env::temp_dir().join(format!(
            "velnor-empty-directory-quarantine-race-{}",
            uuid::Uuid::new_v4()
        ));
        let anchor = root.join("cache");
        let parent_path = anchor.join("admission-leases");
        let candidate_path = parent_path.join("namespace");
        fs::create_dir_all(&candidate_path).unwrap();
        let anchor_identity = filesystem_directory_identity(&anchor).unwrap();
        let (pinned_parent, parent_identity) =
            filesystem_pin_directory_under(&anchor, &parent_path, &anchor_identity).unwrap();
        let (pinned_candidate, candidate_identity) =
            filesystem_pin_directory_under(&anchor, &candidate_path, &anchor_identity).unwrap();
        let mut pinned_quarantine = None;

        let result = remove_empty_directory_under_pinned_parent_with_hook(
            &anchor,
            &candidate_path,
            &anchor_identity,
            &parent_identity,
            &pinned_parent,
            &candidate_identity,
            &pinned_candidate,
            |quarantine, entry| {
                pinned_quarantine = Some(quarantine.try_clone()?);
                let retained = std::ffi::OsStr::new("retained-original");
                rustix::fs::renameat_with(
                    quarantine,
                    entry,
                    quarantine,
                    retained,
                    rustix::fs::RenameFlags::NOREPLACE,
                )
                .map_err(std::io::Error::from)?;
                rustix::fs::mkdirat(quarantine, entry, rustix::fs::Mode::from_raw_mode(0o700))
                    .map_err(std::io::Error::from)?;
                Ok(())
            },
        );

        assert!(result.is_err(), "replacement inode must fail closed");
        let quarantine = pinned_quarantine.expect("quarantine descriptor captured");
        let retained =
            open_directory_child(&quarantine, std::ffi::OsStr::new("retained-original")).unwrap();
        assert_eq!(directory_identity(&retained).unwrap(), candidate_identity);
        let replacement = open_directory_child(&quarantine, std::ffi::OsStr::new("entry")).unwrap();
        assert_ne!(
            directory_identity(&replacement).unwrap(),
            candidate_identity
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inventory_rejects_regular_file_on_distinct_mount_id_even_with_same_device() {
        let anchor_identity = FilesystemDirectoryIdentity {
            device: 7,
            inode: 11,
            mount: FilesystemMountIdentity::LinuxMountId(101),
        };
        let mounted_regular_file = FilesystemEntry {
            name: std::ffi::OsString::from("mounted-file"),
            kind: FilesystemEntryKind::RegularFile,
            identity: FilesystemEntryIdentity {
                device: anchor_identity.device,
                inode: 12,
                mount: FilesystemMountIdentity::LinuxMountId(102),
            },
            is_mountpoint: false,
        };

        assert!(!filesystem_entry_matches_mount(
            &mounted_regular_file,
            &anchor_identity
        ));

        let same_mount_regular_file = FilesystemEntry {
            identity: FilesystemEntryIdentity {
                mount: anchor_identity.mount.clone(),
                ..mounted_regular_file.identity.clone()
            },
            ..mounted_regular_file.clone()
        };
        assert!(filesystem_entry_matches_mount(
            &same_mount_regular_file,
            &anchor_identity
        ));
        assert!(!filesystem_entry_matches_mount(
            &FilesystemEntry {
                is_mountpoint: true,
                ..same_mount_regular_file
            },
            &anchor_identity
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pinned_pressure_cleanup_skips_off_device_and_removes_matching_workspace() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-leftover-device-{}",
            uuid::Uuid::new_v4()
        ));
        let pressure_root = root.join("pressure");
        let trusted_anchor = root.join("work");
        let candidate = trusted_anchor.join("slot-1/job-12345678");
        fs::create_dir_all(&pressure_root).unwrap();
        write_tree(&candidate);

        let pressure = crate::host_capacity::HostCapacityPin::open(&pressure_root).unwrap();
        let anchor_identity = filesystem_directory_identity(&trusted_anchor).unwrap();
        let candidate_identity =
            filesystem_directory_identity_under(&trusted_anchor, &candidate, &anchor_identity)
                .unwrap();
        let workspace = AuthorizedWorkspace {
            path: candidate.clone(),
            trusted_anchor,
            anchor_identity: anchor_identity.clone(),
            candidate_identity,
        };
        let pressure_device = pressure.device_id();
        let validate_pressure = || pressure.probe().map(|_| ());

        let off_device_error = remove_authorized_workspace_for_pressure(
            &workspace,
            pressure_device ^ 1,
            &validate_pressure,
        )
        .expect_err("candidate on another device must be excluded");
        assert!(format!("{off_device_error:#}").contains("does not match pressure device"));
        assert!(candidate.join("marker").exists());

        remove_authorized_workspace_for_pressure(&workspace, pressure_device, &validate_pressure)
            .unwrap();
        assert!(!candidate.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn workspace_pressure_recovery_after_secure_preflight_preserves_candidate() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-workspace-boundary-{}",
            uuid::Uuid::new_v4()
        ));
        let pressure_root = root.join("pressure");
        let trusted_anchor = root.join("work");
        let candidate = trusted_anchor.join("slot-1/job-12345678");
        fs::create_dir_all(&pressure_root).unwrap();
        write_tree(&candidate);

        let pressure = crate::host_capacity::HostCapacityPin::open(&pressure_root).unwrap();
        let anchor_identity = filesystem_directory_identity(&trusted_anchor).unwrap();
        let candidate_identity =
            filesystem_directory_identity_under(&trusted_anchor, &candidate, &anchor_identity)
                .unwrap();
        let workspace = AuthorizedWorkspace {
            path: candidate.clone(),
            trusted_anchor,
            anchor_identity,
            candidate_identity,
        };
        let samples = std::cell::Cell::new(0_usize);
        let pressure_recovered = || {
            pressure.probe()?;
            samples.set(samples.get() + 1);
            bail!("injected pressure recovery at unlink boundary")
        };

        let error = remove_authorized_workspace_for_pressure(
            &workspace,
            pressure.device_id(),
            &pressure_recovered,
        )
        .expect_err("pressure recovery at deletion boundary must stop removal");

        assert!(format!("{error:#}").contains("injected pressure recovery"));
        assert_eq!(
            samples.get(),
            1,
            "pressure must be resampled at unlink boundary"
        );
        assert!(
            candidate.join("marker").exists(),
            "recovered candidate was deleted"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pressure_workspace_reclaim_stops_at_fresh_free_space_floor() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-leftover-floor-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let first = work
            .join("slot-1")
            .join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        let second = work
            .join("slot-1")
            .join("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");
        cold_tree(&first);
        cold_tree(&second);

        let mut probe_calls = 0;
        let report = reclaim_with_liveness_authorized_until_free_space(
            std::slice::from_ref(&work),
            &container_liveness(&BTreeSet::new()),
            100,
            || {
                probe_calls += 1;
                Ok(if probe_calls <= 2 { 99 } else { 100 })
            },
            |workspace| remove_authorized_workspace(workspace, None),
        )
        .unwrap();

        assert_eq!(
            probe_calls, 3,
            "probe after inventory, immediately before deletion, and after first candidate"
        );
        assert_eq!(report.deleted_workspaces.len(), 1);
        assert_eq!(first.exists() as u8 + second.exists() as u8, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn hard_utilization_reclaims_workspaces_with_more_than_two_gib_free() {
        let root = std::env::temp_dir().join(format!(
            "velnor-hard-utilization-workspace-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let first = work
            .join("slot-1")
            .join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        let second = work
            .join("slot-1")
            .join("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");
        cold_tree(&first);
        cold_tree(&second);
        let free_bytes = 3_u64 * 1024 * 1024 * 1024;

        let report = reclaim_with_liveness_authorized_while_pressured(
            std::slice::from_ref(&work),
            &container_liveness(&BTreeSet::new()),
            || {
                assert!(free_bytes > 2_u64 * 1024 * 1024 * 1024);
                Ok(first.exists() || second.exists())
            },
            |workspace| remove_authorized_workspace(workspace, None),
        )
        .unwrap();

        assert_eq!(report.deleted_workspaces.len(), 2);
        assert!(!first.exists());
        assert!(!second.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pressure_reclaim_skips_workspace_when_pressure_recovers_after_inventory() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-workspace-recovered-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let candidate = work
            .join("slot-1")
            .join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        cold_tree(&candidate);
        let samples = std::cell::Cell::new(0);

        let report = reclaim_with_liveness_authorized_while_pressured(
            std::slice::from_ref(&work),
            &container_liveness(&BTreeSet::new()),
            || {
                let sample = samples.get();
                samples.set(sample + 1);
                Ok(sample == 0)
            },
            |workspace| remove_authorized_workspace(workspace, None),
        )
        .unwrap();

        assert!(report.deleted_workspaces.is_empty());
        assert!(candidate.exists());
        assert_eq!(samples.get(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn entry_inventory_captures_nofollow_kinds_and_identities() {
        use std::os::unix::fs::{symlink, MetadataExt as _};

        let root =
            std::env::temp_dir().join(format!("velnor-entry-inventory-{}", uuid::Uuid::new_v4()));
        let anchor = root.join("anchor");
        let directory = anchor.join("leases/class");
        let outside = root.join("outside");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(directory.join("holder.json"), b"lease").unwrap();
        fs::write(outside.join("secret.json"), b"outside").unwrap();
        symlink(&outside, directory.join("external-link")).unwrap();

        let anchor_identity = filesystem_directory_identity(&anchor).unwrap();
        let entries = filesystem_entries_under(&anchor, &directory, &anchor_identity).unwrap();
        let record = entries
            .iter()
            .find(|entry| entry.name == "holder.json")
            .unwrap();
        let record_metadata = fs::symlink_metadata(directory.join("holder.json")).unwrap();
        assert_eq!(record.kind, FilesystemEntryKind::RegularFile);
        assert_eq!(record.identity.device, record_metadata.dev());
        assert_eq!(record.identity.inode, record_metadata.ino());
        let link = entries
            .iter()
            .find(|entry| entry.name == "external-link")
            .unwrap();
        let link_metadata = fs::symlink_metadata(directory.join("external-link")).unwrap();
        assert_eq!(link.kind, FilesystemEntryKind::Symlink);
        assert_eq!(link.identity.device, link_metadata.dev());
        assert_eq!(link.identity.inode, link_metadata.ino());
        assert!(!entries.iter().any(|entry| entry.name == "secret.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn quarantine_move_preserves_same_parent_replacement() {
        let root =
            std::env::temp_dir().join(format!("velnor-quarantine-swap-{}", uuid::Uuid::new_v4()));
        let anchor_path = root.join("anchor");
        let parent_path = anchor_path.join("cache");
        let candidate_path = parent_path.join("candidate");
        let displaced_path = parent_path.join("authorized-original");
        fs::create_dir_all(&candidate_path).unwrap();
        fs::write(candidate_path.join("payload"), b"authorized").unwrap();

        let anchor = open_configured_directory(&anchor_path).unwrap();
        let anchor_identity = directory_identity(&anchor).unwrap();
        let parent = open_directory_child(&anchor, std::ffi::OsStr::new("cache")).unwrap();
        let candidate_name = std::ffi::OsStr::new("candidate");
        let pinned = open_directory_child(&parent, candidate_name).unwrap();
        let candidate_identity = directory_identity(&pinned).unwrap();
        let (quarantine, quarantine_name) =
            create_private_quarantine(&anchor, &anchor_identity).unwrap();

        let error = move_candidate_to_quarantine(
            &parent,
            candidate_name,
            &quarantine,
            std::ffi::OsStr::new("entry"),
            &candidate_identity,
            &pinned,
            || {
                fs::rename(&candidate_path, &displaced_path)?;
                fs::create_dir(&candidate_path)?;
                fs::write(candidate_path.join("replacement"), b"decoy")?;
                Ok(())
            },
        )
        .expect_err("replacement inode must fail the pinned identity check");

        assert!(format!("{error:#}").contains("did not match its pinned descriptor"));
        assert!(displaced_path.join("payload").exists());
        assert_eq!(
            fs::read(candidate_path.join("replacement")).unwrap(),
            b"decoy"
        );
        assert!(secure_directory_entries(&quarantine).unwrap().is_empty());
        drop(quarantine);
        remove_private_quarantine(&anchor, &quarantine_name).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn discovery_and_cleanup_refuse_a_symlinked_slot_ancestor() {
        use std::os::unix::fs::{symlink, MetadataExt as _};

        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-symlink-slot-{}",
            uuid::Uuid::new_v4()
        ));
        let configured_root = root.join("configured");
        let work = configured_root.join("work");
        let outside = root.join("outside");
        let job_id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        write_tree(&outside.join(job_id));
        fs::create_dir_all(&work).unwrap();
        symlink(&outside, work.join("slot-1")).unwrap();

        let orphans = orphan_job_workspace_paths_with_liveness(
            std::slice::from_ref(&work),
            &WorkspaceLiveness {
                min_idle: Duration::ZERO,
                ..WorkspaceLiveness::default()
            },
        );
        assert!(orphans.is_empty(), "slot symlink must not be enumerated");

        let candidate = work.join("slot-1").join(job_id);
        let device = fs::metadata(&candidate).unwrap().dev();
        let anchor_identity = filesystem_directory_identity(&configured_root).unwrap();
        let error = remove_dir_all_on_device_under_identity(
            &configured_root,
            &candidate,
            device,
            &anchor_identity,
        )
        .expect_err("symlinked slot ancestor must fail closed");
        assert!(format!("{error:#}").contains("open directory component"));
        assert!(outside.join(job_id).join("marker").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn discovery_refuses_a_symlinked_work_root() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-symlink-work-root-{}",
            uuid::Uuid::new_v4()
        ));
        let configured_root = root.join("configured");
        let outside = root.join("outside-work");
        let work = configured_root.join("work");
        let job_id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        write_tree(&outside.join("slot-1").join(job_id));
        fs::create_dir_all(&configured_root).unwrap();
        symlink(&outside, &work).unwrap();

        let orphans = orphan_job_workspace_paths_with_liveness(
            std::slice::from_ref(&work),
            &WorkspaceLiveness {
                min_idle: Duration::ZERO,
                ..WorkspaceLiveness::default()
            },
        );
        assert!(
            orphans.is_empty(),
            "work-root symlink must not be enumerated"
        );
        assert!(outside.join("slot-1").join(job_id).join("marker").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cleanup_refuses_a_workspace_replaced_after_inventory() {
        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-replaced-workspace-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("configured-work");
        let job_id = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
        let workspace = work.join("slot-1").join(job_id);
        let replaced_workspace = work.join("slot-1").join("original-inventory-entry");
        write_tree(&workspace);

        let mut authorized = authorized_orphan_workspaces_with_liveness(
            std::slice::from_ref(&work),
            &WorkspaceLiveness {
                min_idle: Duration::ZERO,
                ..WorkspaceLiveness::default()
            },
        );
        assert_eq!(authorized.len(), 1);
        assert_eq!(authorized[0].path, workspace);

        fs::rename(&workspace, &replaced_workspace).unwrap();
        write_tree(&workspace);
        let error = remove_authorized_workspace(&authorized.remove(0), None)
            .expect_err("same-mount replacement must not inherit deletion authority");
        assert!(format!("{error:#}").contains("changed since inventory"));
        assert!(workspace.join("marker").exists());
        assert!(replaced_workspace.join("marker").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn trusted_anchor_alias_is_resolved_without_following_candidate_links() {
        use std::os::unix::fs::{symlink, MetadataExt as _};

        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-trusted-anchor-alias-{}",
            uuid::Uuid::new_v4()
        ));
        let real_anchor = root.join("configured-real");
        let anchor_alias = root.join("configured-alias");
        let job_id = "cccccccc-cccc-cccc-cccc-cccccccccccc";
        let workspace = real_anchor.join("work/slot-1").join(job_id);
        write_tree(&workspace);
        fs::create_dir_all(&root).unwrap();
        symlink(&real_anchor, &anchor_alias).unwrap();

        let alias_workspace = anchor_alias.join("work/slot-1").join(job_id);
        let device = fs::metadata(&workspace).unwrap().dev();
        let anchor_identity = filesystem_directory_identity(&anchor_alias).unwrap();
        remove_dir_all_on_device_under_identity(
            &anchor_alias,
            &alias_workspace,
            device,
            &anchor_identity,
        )
        .unwrap();
        assert!(!workspace.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn cleanup_rejects_anchor_mount_identity_change_before_mutation() {
        use std::os::unix::fs::MetadataExt as _;

        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-anchor-mount-mismatch-{}",
            uuid::Uuid::new_v4()
        ));
        let anchor = root.join("configured");
        let job_id = "dddddddd-dddd-dddd-dddd-dddddddddddd";
        let workspace = anchor.join("work/slot-1").join(job_id);
        write_tree(&workspace);
        let device = fs::metadata(&workspace).unwrap().dev();
        let mut wrong_identity = filesystem_directory_identity(&anchor).unwrap();
        match &mut wrong_identity.mount {
            #[cfg(target_os = "linux")]
            FilesystemMountIdentity::LinuxMountId(mount_id) => {
                *mount_id = mount_id.wrapping_add(1);
            }
            #[cfg(target_os = "macos")]
            FilesystemMountIdentity::MacOs(identity) => {
                identity.mountpoint.push("mismatch");
            }
        }

        let error =
            remove_dir_all_on_device_under_identity(&anchor, &workspace, device, &wrong_identity)
                .expect_err("changed anchor mount identity must fail closed");
        assert!(format!("{error:#}").contains("anchor changed"));
        assert!(workspace.join("marker").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    fn synthetic_macos_bulk_entry(
        name: &[u8],
        object_type: u32,
        returned_directory: u32,
        mount_status: u32,
    ) -> Vec<u8> {
        const NAME_REFERENCE_OFFSET: usize = 28;
        const NAME_OFFSET: usize = 44;
        const ATTR_CMN_ERROR_LOCAL: u32 = 0x2000_0000;
        let name = [name, &[0]].concat();
        let record_length = (NAME_OFFSET + name.len() + 7) & !7;
        let mut record = vec![0_u8; record_length];
        let write_word = |bytes: &mut [u8], offset: usize, value: u32| {
            bytes[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        };
        write_word(&mut record, 0, record_length as u32);
        write_word(
            &mut record,
            4,
            libc::ATTR_CMN_RETURNED_ATTRS
                | libc::ATTR_CMN_NAME
                | ATTR_CMN_ERROR_LOCAL
                | libc::ATTR_CMN_OBJTYPE,
        );
        write_word(&mut record, 12, returned_directory);
        write_word(&mut record, 24, 0);
        write_word(
            &mut record,
            NAME_REFERENCE_OFFSET,
            (NAME_OFFSET - NAME_REFERENCE_OFFSET) as u32,
        );
        write_word(&mut record, NAME_REFERENCE_OFFSET + 4, name.len() as u32);
        write_word(&mut record, NAME_REFERENCE_OFFSET + 8, object_type);
        write_word(&mut record, NAME_REFERENCE_OFFSET + 12, mount_status);
        record[NAME_OFFSET..NAME_OFFSET + name.len()].copy_from_slice(&name);
        record
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn synthetic_mac_mountpoint_attribute_fails_closed() {
        let record = synthetic_macos_bulk_entry(
            b"mounted-child",
            2,
            libc::ATTR_DIR_MOUNTSTATUS,
            libc::DIR_MNTSTATUS_MNTPOINT,
        );
        let mut entries = parse_macos_bulk_entries(&record, 1).unwrap();
        let entry = entries.pop().unwrap();
        assert!(entry.is_directory);
        assert!(entry.is_mountpoint);
        assert!(entry
            .require_same_mount(Path::new("mounted-child"))
            .is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn synthetic_mac_directory_without_mount_status_fails_closed() {
        let record = synthetic_macos_bulk_entry(b"child", 2, 0, 0);
        let error = parse_macos_bulk_entries(&record, 1)
            .expect_err("missing mount status must fail closed");
        assert!(format!("{error:#}").contains("omitted directory mount status"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_partial_cleanup_error_restores_remaining_tree() {
        let restore_called = std::cell::Cell::new(false);
        let error = finish_macos_delete_error(
            anyhow::anyhow!("injected failure after child removal"),
            true,
            || {
                restore_called.set(true);
                Ok(())
            },
        )
        .expect_err("partial cleanup must report its abort after restoring");
        assert!(format!("{error:#}").contains("remaining tree restored"));
        assert!(restore_called.get());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_nested_tree_cleanup_reopens_consumed_parent_descriptors() {
        let root = std::env::temp_dir().join(format!(
            "velnor-macos-nested-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        let anchor = root.join("configured");
        let workspace = anchor.join("work/slot-1/job-12345678");
        write_tree(&workspace.join("outer/deep/nested"));
        fs::write(workspace.join("outer/sibling"), b"sibling").unwrap();

        let anchor_identity = filesystem_directory_identity(&anchor).unwrap();
        let (pinned, opened_anchor) =
            open_directory_under_anchor(&anchor, &workspace, &anchor_identity).unwrap();
        assert_eq!(opened_anchor, anchor_identity);
        let candidate_identity = directory_identity(&pinned).unwrap();
        remove_dir_all_on_device_under_pinned(
            &anchor,
            &workspace,
            anchor_identity.device,
            &anchor_identity,
            &candidate_identity,
            &pinned,
        )
        .expect("nested macOS cleanup should use fresh bulk-scan descriptors");
        assert!(
            !workspace.exists(),
            "partial tree was restored after deletion"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_data_mount_is_reported_as_mountpoint() {
        use std::fs::File;

        let parent = File::open("/System/Volumes").unwrap();
        let entries = macos_bulk_directory_entries(&parent).unwrap();
        let data = entries
            .iter()
            .find(|entry| entry.name == "Data")
            .expect("macOS Data volume entry");
        assert!(data.is_directory);
        assert!(data.is_mountpoint);

        let system =
            macos_directory_mount_identity(&File::open("/System/Volumes").unwrap()).unwrap();
        let data_mount =
            macos_directory_mount_identity(&File::open("/System/Volumes/Data").unwrap()).unwrap();
        assert_ne!(system, data_mount);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pressure_abort_after_partial_unlink_restores_workspace_and_clears_quarantine() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-partial-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let candidate = work.join("slot-1/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        fs::create_dir_all(&candidate).unwrap();
        fs::write(candidate.join("first"), b"first").unwrap();
        fs::write(candidate.join("later"), b"later").unwrap();

        let anchor_identity = filesystem_directory_identity(&work).unwrap();
        let candidate_identity =
            filesystem_directory_identity_under(&work, &candidate, &anchor_identity).unwrap();
        let unlink_boundaries = std::cell::Cell::new(0_usize);
        let pressure_cleared_before_next_unlink = |_: &Path| -> Result<()> {
            let boundary = unlink_boundaries.get();
            unlink_boundaries.set(boundary + 1);
            if boundary == 1 {
                bail!("injected pressure recovery at second unlink boundary");
            }
            Ok(())
        };

        let error = remove_dir_all_on_device_under_identities_with_pre_unlink(
            &work,
            &candidate,
            anchor_identity.device,
            &anchor_identity,
            &candidate_identity,
            &pressure_cleared_before_next_unlink,
        )
        .expect_err("pressure recovery must abort partial deletion");

        assert!(format!("{error:#}").contains("injected pressure recovery"));
        assert!(
            candidate.exists(),
            "partially deleted workspace was stranded"
        );
        assert_eq!(
            fs::read_dir(&candidate).unwrap().count(),
            1,
            "one child is already deleted; the remaining tree must be restored"
        );
        assert!(
            fs::read_dir(&work).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(RECLAIM_QUARANTINE_PREFIX)),
            "completed rollback must remove its empty quarantine"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pre_unlink_replacement_is_rechecked_and_preserved() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pre-unlink-replacement-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let candidate = work.join("slot-1/eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee");
        fs::create_dir_all(&candidate).unwrap();
        fs::write(candidate.join("victim"), b"original").unwrap();

        let anchor_identity = filesystem_directory_identity(&work).unwrap();
        let candidate_identity =
            filesystem_directory_identity_under(&work, &candidate, &anchor_identity).unwrap();
        let swapped = std::cell::Cell::new(false);
        let before_unlink = |_: &Path| -> Result<()> {
            if swapped.replace(true) {
                return Ok(());
            }
            let quarantine = fs::read_dir(&work)?
                .filter_map(|entry| entry.ok())
                .find(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with(RECLAIM_QUARANTINE_PREFIX))
                })
                .context("cleanup callback cannot find its quarantine")?
                .path()
                .join(RECLAIM_QUARANTINE_ENTRY);
            let victim = quarantine.join("victim");
            fs::rename(&victim, root.join("preserved-original"))?;
            fs::write(victim, b"replacement")?;
            Ok(())
        };

        let error = remove_dir_all_on_device_under_identities_with_pre_unlink(
            &work,
            &candidate,
            anchor_identity.device,
            &anchor_identity,
            &candidate_identity,
            &before_unlink,
        )
        .expect_err("identity replacement in the callback must stop unlink");

        assert!(format!("{error:#}").contains("changed before removal"));
        assert_eq!(fs::read(candidate.join("victim")).unwrap(), b"replacement");
        assert_eq!(
            fs::read(root.join("preserved-original")).unwrap(),
            b"original"
        );
        assert!(fs::read_dir(&work).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(RECLAIM_QUARANTINE_PREFIX)));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn orphan_scan_recovers_durable_partial_quarantine_by_journaled_identity() {
        let root = std::env::temp_dir().join(format!(
            "velnor-quarantine-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let job_id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let candidate = work.join("slot-1").join(job_id);
        write_tree(&candidate);
        fs::write(candidate.join("already-removed"), b"partial").unwrap();

        let anchor = open_configured_directory(&work).unwrap();
        let anchor_identity = directory_identity(&anchor).unwrap();
        let (parent, name, _, opened_anchor_identity) =
            open_parent_beneath_anchor(&work, &candidate, Some(&anchor_identity)).unwrap();
        assert_eq!(opened_anchor_identity, anchor_identity);
        let pinned_candidate = open_directory_child(&parent, &name).unwrap();
        let candidate_identity = directory_identity(&pinned_candidate).unwrap();
        let (quarantine, quarantine_name) =
            create_private_quarantine(&anchor, &anchor_identity).unwrap();
        write_quarantine_journal(
            &quarantine,
            &anchor_identity,
            &directory_identity(&parent).unwrap(),
            Path::new("slot-1").join(job_id).as_path(),
            &candidate_identity,
        )
        .unwrap();
        let moved = move_candidate_to_quarantine(
            &parent,
            &name,
            &quarantine,
            std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY),
            &candidate_identity,
            &pinned_candidate,
            || Ok(()),
        )
        .unwrap();
        persist_quarantine_move(&parent, &quarantine).unwrap();
        fs::remove_file(
            work.join(&quarantine_name)
                .join(RECLAIM_QUARANTINE_ENTRY)
                .join("already-removed"),
        )
        .unwrap();
        sync_directory(&moved).unwrap();
        drop(moved);
        drop(pinned_candidate);
        drop(parent);
        drop(quarantine);
        drop(anchor);

        recover_private_quarantines_if_pending(
            &root.join("run"),
            std::slice::from_ref(&work),
            || Ok(BTreeSet::new()),
        )
        .unwrap();

        assert!(candidate.join("marker").exists());
        assert!(!candidate.join("already-removed").exists());
        assert!(!work.join(quarantine_name).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn recovery_freezes_quarantine_when_source_parent_identity_changes() {
        let root = std::env::temp_dir().join(format!(
            "velnor-quarantine-parent-identity-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let job_id = "dddddddd-dddd-dddd-dddd-dddddddddddd";
        let candidate = work.join("slot-1").join(job_id);
        write_tree(&candidate);

        let anchor = open_configured_directory(&work).unwrap();
        let anchor_identity = directory_identity(&anchor).unwrap();
        let (parent, name, _, _) =
            open_parent_beneath_anchor(&work, &candidate, Some(&anchor_identity)).unwrap();
        let source_parent_identity = directory_identity(&parent).unwrap();
        let pinned_candidate = open_directory_child(&parent, &name).unwrap();
        let candidate_identity = directory_identity(&pinned_candidate).unwrap();
        let (quarantine, quarantine_name) =
            create_private_quarantine(&anchor, &anchor_identity).unwrap();
        write_quarantine_journal(
            &quarantine,
            &anchor_identity,
            &source_parent_identity,
            Path::new("slot-1").join(job_id).as_path(),
            &candidate_identity,
        )
        .unwrap();
        move_candidate_to_quarantine(
            &parent,
            &name,
            &quarantine,
            std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY),
            &candidate_identity,
            &pinned_candidate,
            || Ok(()),
        )
        .unwrap();
        persist_quarantine_move(&parent, &quarantine).unwrap();
        let q_entry = work.join(&quarantine_name).join(RECLAIM_QUARANTINE_ENTRY);
        drop(pinned_candidate);
        drop(parent);
        drop(quarantine);
        drop(anchor);

        fs::rename(work.join("slot-1"), root.join("displaced-slot-1")).unwrap();
        fs::create_dir(work.join("slot-1")).unwrap();
        recover_private_quarantines_if_pending(
            &root.join("run"),
            std::slice::from_ref(&work),
            || Ok(BTreeSet::new()),
        )
        .unwrap();

        assert!(
            !candidate.exists(),
            "replacement parent must not receive stale data"
        );
        assert!(
            q_entry.join("marker").exists(),
            "uncertain q must stay frozen"
        );
        assert!(root.join("displaced-slot-1").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn microvm_no_pressure_recovery_scans_cache_anchors_without_docker() {
        let root = std::env::temp_dir().join(format!(
            "velnor-cache-quarantine-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        let config_base = root.join("config");
        let layout = crate::storage::StorageLayout::explicit_local(&config_base);
        let keyed_anchor = crate::trust_scope::filesystem_key_namespace(&layout.cache_root);
        let keyed_candidate = keyed_anchor.join("scope-key").join("store-entry");
        let direct_candidate = layout.cache_root.join("gha-cache").join("store-entry");
        let work_roots = crate::runner::daemon_pressure_work_roots(&config_base, None, 2);
        let work_root = config_base.join("slots/slot-2/_work");
        assert!(work_roots.contains(&work_root));
        let work_catalog =
            crate::store_catalog::StoreCatalog::for_work_root_with_layout(&work_root, &layout);
        let work_cache_candidate = work_catalog.artifacts().join("store-entry");
        let workspace_candidate = work_root.join("ffffffff-ffff-ffff-ffff-ffffffffffff");
        let prepare_quarantine = |anchor_path: &Path, candidate: &Path| -> Result<PathBuf> {
            write_tree(candidate);
            let anchor = open_configured_directory(anchor_path)?;
            let anchor_identity = directory_identity(&anchor)?;
            let (parent, name, _, _) =
                open_parent_beneath_anchor(anchor_path, candidate, Some(&anchor_identity))?;
            let source_parent_identity = directory_identity(&parent)?;
            let pinned_candidate = open_directory_child(&parent, &name)?;
            let candidate_identity = directory_identity(&pinned_candidate)?;
            let (quarantine, quarantine_name) =
                create_private_quarantine(&anchor, &anchor_identity)?;
            write_quarantine_journal(
                &quarantine,
                &anchor_identity,
                &source_parent_identity,
                candidate.strip_prefix(anchor_path)?,
                &candidate_identity,
            )?;
            move_candidate_to_quarantine(
                &parent,
                &name,
                &quarantine,
                std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY),
                &candidate_identity,
                &pinned_candidate,
                || Ok(()),
            )?;
            persist_quarantine_move(&parent, &quarantine)?;
            drop(pinned_candidate);
            drop(parent);
            drop(quarantine);
            drop(anchor);
            Ok(anchor_path.join(quarantine_name))
        };
        let keyed_quarantine = prepare_quarantine(&keyed_anchor, &keyed_candidate).unwrap();
        let direct_quarantine = prepare_quarantine(&layout.cache_root, &direct_candidate).unwrap();
        let work_cache_quarantine = prepare_quarantine(&work_root, &work_cache_candidate).unwrap();
        let workspace_quarantine = prepare_quarantine(&work_root, &workspace_candidate).unwrap();

        let docker_was_queried = std::cell::Cell::new(false);
        recover_quarantines_for_backend(
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            &layout.run_root,
            &work_roots,
            &layout,
            || {
                docker_was_queried.set(true);
                bail!("MicroVM cache recovery must not query host Docker")
            },
        )
        .unwrap();

        assert!(!docker_was_queried.get());
        assert!(keyed_candidate.join("marker").exists());
        assert!(direct_candidate.join("marker").exists());
        assert!(work_cache_candidate.join("marker").exists());
        assert!(!keyed_quarantine.exists());
        assert!(!direct_quarantine.exists());
        assert!(!work_cache_quarantine.exists());
        assert!(workspace_quarantine
            .join(RECLAIM_QUARANTINE_ENTRY)
            .join("marker")
            .exists());
        assert!(!workspace_candidate.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn no_pressure_recovery_skips_liveness_collection_without_quarantines() {
        let root = std::env::temp_dir().join(format!(
            "velnor-quarantine-noop-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let run_root = root.join("run");
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        let listed_live_jobs = std::cell::Cell::new(false);

        recover_private_quarantines_if_pending(&run_root, std::slice::from_ref(&work), || {
            listed_live_jobs.set(true);
            Ok(BTreeSet::new())
        })
        .unwrap();

        assert!(!listed_live_jobs.get());
        assert!(
            !run_root.exists(),
            "no-op scan must not acquire coordinator"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn quarantine_rename_sync_orders_are_fault_injected_and_recoverable() {
        let root = std::env::temp_dir().join(format!(
            "velnor-quarantine-sync-order-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(root.join("source")).unwrap();
        fs::create_dir_all(root.join("quarantine")).unwrap();
        let source = fs::File::open(root.join("source")).unwrap();
        let quarantine = fs::File::open(root.join("quarantine")).unwrap();
        let source_inode = directory_identity(&source).unwrap().inode;
        let quarantine_inode = directory_identity(&quarantine).unwrap().inode;

        let mut move_calls = Vec::new();
        let move_error = sync_two_directories_in_order(&quarantine, &source, |directory| {
            move_calls.push(directory_identity(directory)?.inode);
            if move_calls.len() == 2 {
                bail!("injected source-parent sync failure after durable destination");
            }
            Ok(())
        })
        .expect_err("second move sync failure must stop the transaction");
        assert!(format!("{move_error:#}").contains("injected source-parent sync failure"));
        assert_eq!(move_calls, vec![quarantine_inode, source_inode]);

        let mut restore_calls = Vec::new();
        let restore_error = sync_two_directories_in_order(&source, &quarantine, |directory| {
            restore_calls.push(directory_identity(directory)?.inode);
            if restore_calls.len() == 2 {
                bail!("injected quarantine sync failure after durable source");
            }
            Ok(())
        })
        .expect_err("second restore sync failure must stop the transaction");
        assert!(format!("{restore_error:#}").contains("injected quarantine sync failure"));
        assert_eq!(restore_calls, vec![source_inode, quarantine_inode]);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn quarantine_setup_failure_after_mkdir_removes_empty_directory() {
        let root = std::env::temp_dir().join(format!(
            "velnor-quarantine-setup-failure-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let anchor = open_configured_directory(&root).unwrap();
        let anchor_identity = directory_identity(&anchor).unwrap();

        let error = create_private_quarantine_with_hook(&anchor, &anchor_identity, || {
            bail!("injected setup failure after mkdirat")
        })
        .expect_err("post-mkdir setup failure must clean up");

        assert!(format!("{error:#}").contains("injected setup failure after mkdirat"));
        assert!(secure_directory_entries(&anchor).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn no_pressure_recovery_cleans_empty_quarantine_left_by_crash_after_mkdir() {
        use std::os::unix::fs::DirBuilderExt as _;

        let root = std::env::temp_dir().join(format!(
            "velnor-quarantine-empty-recovery-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let name = format!(
            "{RECLAIM_QUARANTINE_PREFIX}{}-{}",
            std::process::id(),
            QUARANTINE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let quarantine_path = root.join(&name);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&quarantine_path)
            .unwrap();

        recover_private_quarantines_if_pending(
            &root.join("run"),
            std::slice::from_ref(&root),
            || Ok(BTreeSet::new()),
        )
        .unwrap();

        assert!(!quarantine_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn orphan_scan_freezes_quarantine_when_journaled_identity_changes() {
        let root = std::env::temp_dir().join(format!(
            "velnor-quarantine-identity-freeze-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let job_id = "cccccccc-cccc-cccc-cccc-cccccccccccc";
        let candidate = work.join("slot-1").join(job_id);
        write_tree(&candidate);

        let anchor = open_configured_directory(&work).unwrap();
        let anchor_identity = directory_identity(&anchor).unwrap();
        let (parent, name, _, _) =
            open_parent_beneath_anchor(&work, &candidate, Some(&anchor_identity)).unwrap();
        let pinned_candidate = open_directory_child(&parent, &name).unwrap();
        let candidate_identity = directory_identity(&pinned_candidate).unwrap();
        let (quarantine, quarantine_name) =
            create_private_quarantine(&anchor, &anchor_identity).unwrap();
        write_quarantine_journal(
            &quarantine,
            &anchor_identity,
            &directory_identity(&parent).unwrap(),
            Path::new("slot-1").join(job_id).as_path(),
            &candidate_identity,
        )
        .unwrap();
        move_candidate_to_quarantine(
            &parent,
            &name,
            &quarantine,
            std::ffi::OsStr::new(RECLAIM_QUARANTINE_ENTRY),
            &candidate_identity,
            &pinned_candidate,
            || Ok(()),
        )
        .unwrap();
        let quarantine = work.join(&quarantine_name);
        let q_entry = quarantine.join(RECLAIM_QUARANTINE_ENTRY);
        fs::rename(&q_entry, root.join("displaced-entry")).unwrap();
        write_tree(&q_entry);
        drop(pinned_candidate);
        drop(parent);
        drop(quarantine);
        drop(anchor);

        let report = reclaim_with_liveness_authorized_while_pressured(
            std::slice::from_ref(&work),
            &WorkspaceLiveness {
                min_idle: Duration::ZERO,
                ..WorkspaceLiveness::default()
            },
            || Ok(false),
            |_| Ok(()),
        )
        .unwrap();

        assert!(
            !candidate.exists(),
            "uncertain identity must not be restored"
        );
        assert!(
            q_entry.join("marker").exists(),
            "uncertain quarantine was altered"
        );
        assert!(report.deleted_workspaces.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_preflights_and_preserves_a_nested_off_device_tree() {
        use std::os::unix::fs::MetadataExt as _;

        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-mount-boundary-{}",
            uuid::Uuid::new_v4()
        ));
        let workspace = root.join("work/job");
        let mounted = workspace.join("nested-mount");
        write_tree(&mounted);
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("sibling"), b"on pressure filesystem").unwrap();

        let pressure_device = fs::metadata(&workspace).unwrap().dev();
        let identify = |path: &Path, device: u64, mount_id: u64| {
            if path.starts_with(&mounted) {
                (device, mount_id.wrapping_add(1))
            } else {
                (device, mount_id)
            }
        };
        let error = remove_dir_all_with_identity(&workspace, pressure_device, None, &identify)
            .expect_err("off-device nested tree must fail preflight before deletion");

        assert!(format!("{error:#}").contains("mount boundary"));
        assert!(mounted.join("marker").exists());
        assert!(
            workspace.join("sibling").exists(),
            "preflight must precede mutation"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn same_device_bind_mounted_workspace_is_rejected_in_private_mount_namespace() {
        const CHILD: &str = "VELNOR_TEST_BIND_MOUNT_CHILD";
        if std::env::var_os(CHILD).is_some() {
            run_same_device_bind_mount_cleanup_check();
            return;
        }

        let executable = std::env::current_exe().unwrap();
        let test_name = std::thread::current().name().unwrap().to_owned();
        let output = match std::process::Command::new("unshare")
            .args(["--mount", "--propagation", "private", "--"])
            .arg(executable)
            .args(["--exact", &test_name, "--nocapture"])
            .env(CHILD, "1")
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("start isolated bind-mount test: {error}"),
        };
        let detail = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success()
            && (detail.contains("Operation not permitted") || detail.contains("unshare failed"))
        {
            return;
        }
        assert!(
            output.status.success(),
            "isolated bind-mount test failed: {detail}"
        );
    }

    #[cfg(target_os = "linux")]
    fn run_same_device_bind_mount_cleanup_check() {
        use std::os::unix::fs::MetadataExt as _;

        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-bind-mount-{}",
            uuid::Uuid::new_v4()
        ));
        let slot = root.join("work/slot-1");
        let workspace = slot.join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        let source = root.join("source");
        fs::create_dir_all(&workspace).unwrap();
        write_tree(&source);
        let mount = match std::process::Command::new("mount")
            .arg("--bind")
            .arg(&source)
            .arg(&workspace)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::remove_dir_all(root).unwrap();
                return;
            }
            Err(error) => panic!("start bind mount: {error}"),
        };
        let detail = format!(
            "{}{}",
            String::from_utf8_lossy(&mount.stdout),
            String::from_utf8_lossy(&mount.stderr)
        );
        if !mount.status.success()
            && (detail.contains("Operation not permitted") || detail.contains("permission denied"))
        {
            fs::remove_dir_all(root).unwrap();
            return;
        }
        assert!(mount.status.success(), "create bind mount: {detail}");
        assert_eq!(
            fs::metadata(&workspace).unwrap().dev(),
            fs::metadata(&slot).unwrap().dev(),
            "bind mount fixture must share the workspace device"
        );

        let pressure_device = fs::metadata(&slot).unwrap().dev();
        let Some(slot_mount) = filesystem_mount_identity(&slot) else {
            let _ = std::process::Command::new("umount")
                .arg(&workspace)
                .output();
            fs::remove_dir_all(root).unwrap();
            return;
        };
        let error = remove_dir_all_on_device_and_mount_under(
            &root,
            &workspace,
            pressure_device,
            slot_mount,
        )
        .expect_err("same-device bind-mounted workspace must stop cleanup");
        let detail = format!("{error:#}");
        if detail.contains("does not provide mount identity") {
            let _ = std::process::Command::new("umount")
                .arg(&workspace)
                .output();
            fs::remove_dir_all(root).unwrap();
            return;
        }
        assert!(
            detail.contains("mount boundary"),
            "unexpected refusal: {detail}"
        );
        assert!(
            workspace.join("marker").exists(),
            "mounted data was deleted"
        );
        let unmount = std::process::Command::new("umount")
            .arg(&workspace)
            .output()
            .unwrap();
        assert!(
            unmount.status.success(),
            "unmount fixture before cleanup: {}",
            String::from_utf8_lossy(&unmount.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A leftover workspace is, by definition, one nothing has touched for a
    /// long time. Fixtures that mean "orphan" must say so.
    fn cold_tree(path: &Path) {
        write_tree(path);
        crate::cache::test_clock::backdate(path, WORKSPACE_MIN_IDLE * 2);
    }

    fn container_liveness(live: &BTreeSet<String>) -> WorkspaceLiveness {
        WorkspaceLiveness {
            running: live.clone(),
            min_idle: WORKSPACE_MIN_IDLE,
            ..WorkspaceLiveness::default()
        }
    }

    #[test]
    fn live_job_ids_parse_docker_ps_names() {
        let formatted = "\
velnor-job-d0f5aa1f-402c-5590-9414-c95e721539c1\n\
buildx_buildkit_velnor-builder-abc\n\
velnor-job-not-a-uuid\n\
";
        let ids = live_job_ids_from_docker_ps(formatted);
        assert_eq!(
            ids,
            BTreeSet::from(["d0f5aa1f-402c-5590-9414-c95e721539c1".into()])
        );
    }

    #[test]
    fn orphan_uuid_dir_is_deleted_live_scope_kept() {
        let root = std::env::temp_dir().join(format!("velnor-leftover-ws-{}", std::process::id()));
        let work = root.join("velnor-tailrocks/work");
        let live_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let orphan_id = "11111111-2222-3333-4444-555555555555";
        let live = work.join("slot-2").join(live_id);
        let orphan = work.join("slot-2").join(orphan_id);
        let cache = work.join("_velnor_targets");
        write_tree(&live);
        cold_tree(&orphan);
        write_tree(&cache);
        let live_ids = BTreeSet::from([live_id.to_string()]);
        let roots = discover_daemon_work_roots_in(&root);
        assert_eq!(roots, vec![work.clone()]);
        let orphans = orphan_job_workspace_paths(&roots, &live_ids);
        assert_eq!(orphans, vec![orphan.clone()]);
        let deleted = Arc::new(Mutex::new(Vec::new()));
        let docker_calls = Arc::new(Mutex::new(Vec::new()));
        let report = reclaim_leftover_after_velnor(
            &roots,
            &live_ids,
            {
                let docker_calls = Arc::clone(&docker_calls);
                move |args| {
                    docker_calls.lock().unwrap().push(args.to_vec());
                    Ok(String::new())
                }
            },
            {
                let deleted = Arc::clone(&deleted);
                move |path| {
                    fs::remove_dir_all(path)?;
                    deleted.lock().unwrap().push(path.to_path_buf());
                    Ok(())
                }
            },
            true,
        )
        .unwrap();
        assert_eq!(report.deleted_workspaces, vec![orphan.clone()]);
        assert!(!orphan.exists());
        assert!(live.exists(), "live job workspace must stay");
        assert!(cache.exists(), "warm _velnor_targets must stay");
        let commands = docker_calls.lock().unwrap().clone();
        assert_leftover_docker_commands_are_safe(&commands);
        assert_eq!(commands, vec![dangling_image_prune_args()]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn hard_pressure_90_reclaims_leftovers_without_system_or_volume_prune() {
        let root = std::env::temp_dir().join(format!("velnor-leftover-p90-{}", std::process::id()));
        let work = root.join("velnor-fixture/work");
        let orphan = work
            .join("slot-1")
            .join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        cold_tree(&orphan);
        let docker_calls = Arc::new(Mutex::new(Vec::new()));
        let below = reclaim_if_hard_pressure(
            89,
            std::slice::from_ref(&work),
            &BTreeSet::new(),
            |_| Ok(String::new()),
            |_| Ok(()),
        )
        .unwrap();
        assert!(below.deleted_workspaces.is_empty());
        assert!(below.docker_commands.is_empty());
        assert!(orphan.exists());
        let report = reclaim_if_hard_pressure(
            HARD_PRESSURE_PERCENT,
            std::slice::from_ref(&work),
            &BTreeSet::new(),
            {
                let docker_calls = Arc::clone(&docker_calls);
                move |args| {
                    docker_calls.lock().unwrap().push(args.to_vec());
                    Ok(String::new())
                }
            },
            |path| {
                fs::remove_dir_all(path)?;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(report.deleted_workspaces, vec![orphan.clone()]);
        assert!(!orphan.exists());
        let commands = docker_calls.lock().unwrap().clone();
        assert_leftover_docker_commands_are_safe(&commands);
        assert!(
            commands
                .iter()
                .all(|cmd| cmd == &dangling_image_prune_args()),
            "90% path must only prune dangling images, got {commands:?}"
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn microvm_reclaim_does_not_invoke_host_docker() {
        let report = reclaim_leftover_after_velnor(
            &[],
            &BTreeSet::new(),
            |_| panic!("microvm leftover reclaim must not use host docker"),
            |_| Ok(()),
            false,
        )
        .unwrap();
        assert!(report.docker_commands.is_empty());
        assert!(!report.skipped_docker);
    }

    #[test]
    fn live_job_ids_for_reclaim_skip_host_docker_when_unselected_or_microvm() {
        assert!(live_job_ids_for_reclaim(None).unwrap().is_empty());
        assert!(
            live_job_ids_for_reclaim(Some(velnor_model::ExecutionBackendKind::MicroVm))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn selected_root_leftover_reclaim_keeps_microvm_and_unknown_off_host_docker() {
        let roots = [PathBuf::from("/selected/daemon/_work")];
        for backend in [None, Some(velnor_model::ExecutionBackendKind::MicroVm)] {
            let calls = std::cell::Cell::new(0);
            let report = reclaim_production_leftovers_for_roots_with(
                backend,
                &roots,
                false,
                || {
                    calls.set(calls.get() + 1);
                    panic!("host Docker job listing must not run for {backend:?}")
                },
                |_| {
                    calls.set(calls.get() + 1);
                    panic!("host Docker commands must not run for {backend:?}")
                },
                |_| {
                    calls.set(calls.get() + 1);
                    panic!("workspace deletion must not run for {backend:?}")
                },
            )
            .unwrap();
            assert_eq!(calls.get(), 0, "{backend:?}");
            assert!(report.docker_commands.is_empty(), "{backend:?}");
            assert!(!report.skipped_docker, "{backend:?}");
        }
    }

    #[test]
    fn hard_pressure_microvm_and_missing_never_invoke_host_docker() {
        for backend in [None, Some(velnor_model::ExecutionBackendKind::MicroVm)] {
            let mut removed = Vec::new();
            let report = reclaim_production_if_hard_pressure_with(
                backend,
                HARD_PRESSURE_PERCENT,
                &[PathBuf::from("/var/lib/velnor-trusted/work/active-job")],
                |_| panic!("host docker must not run for {backend:?}"),
                |path| {
                    removed.push(path.to_path_buf());
                    Ok(())
                },
            )
            .unwrap();
            assert!(report.docker_commands.is_empty(), "{backend:?}");
            assert!(!report.skipped_docker, "{backend:?}");
            assert!(
                removed.is_empty(),
                "cross-pool workspace was reclaimed: {removed:?}"
            );
        }
    }

    #[test]
    fn hard_pressure_docker_lists_live_jobs_through_injected_docker() {
        let mut calls = Vec::new();
        let report = reclaim_production_if_hard_pressure_with(
            Some(velnor_model::ExecutionBackendKind::Docker),
            HARD_PRESSURE_PERCENT,
            &[],
            |args| {
                calls.push(args.to_vec());
                Ok(String::new())
            },
            |_| Ok(()),
        )
        .unwrap();
        assert!(
            calls.iter().any(|args| args
                .windows(2)
                .any(|w| w == ["ps".to_string(), "--all".to_string()]
                    || w == ["image".to_string(), "prune".to_string()])),
            "docker hard-pressure reclaim must list or prune via host docker, got {calls:?}"
        );
        assert_leftover_docker_commands_are_safe(&report.docker_commands);
    }

    #[test]
    fn df_capacity_column_is_hard_pressure_input() {
        let stdout = "\
Filesystem     1024-blocks      Used Available Capacity Mounted on
/dev/md3         963379200 857000000 106000000      89% /
";
        assert_eq!(disk_usage_percent_from_df(stdout), Some(89));
        let stdout = "\
Filesystem     1024-blocks      Used Available Capacity Mounted on
/dev/md3         963379200 867000000  96000000      90% /
";
        assert_eq!(
            disk_usage_percent_from_df(stdout),
            Some(HARD_PRESSURE_PERCENT)
        );
    }

    #[test]
    fn statvfs_capacity_uses_unprivileged_available_blocks() {
        assert_eq!(disk_usage_percent_from_statvfs(100, 11), Some(89));
        assert_eq!(disk_usage_percent_from_statvfs(100, 10), Some(90));
        assert_eq!(disk_usage_percent_from_statvfs(0, 0), None);
        assert_eq!(disk_usage_percent_from_statvfs(100, 101), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn disk_usage_probe_reads_the_platform_filesystem_without_df() {
        let path = std::env::temp_dir();
        let stat = rustix::fs::statvfs(&path).unwrap();
        assert_eq!(
            disk_usage_percent(&path),
            disk_usage_percent_from_statvfs(stat.f_blocks, stat.f_bavail)
        );
    }

    #[test]
    fn cache_gc_work_root_uses_storage_layout_not_empty_home() {
        let prefix =
            std::env::temp_dir().join(format!("velnor-leftover-root-{}", std::process::id()));
        let work = prefix.join("lib/velnor/work");
        fs::create_dir_all(&work).unwrap();
        let layout = crate::storage::StorageLayout::from_prefix(&prefix);
        assert_eq!(layout.lib_root.join("work"), work);
        assert_ne!(
            work,
            PathBuf::from("/root/.velnor/runner/_work"),
            "production work is not the empty default home path"
        );
        let fixture = prefix.join("lib/velnor-fixture/work");
        fs::create_dir_all(&fixture).unwrap();
        let roots = discover_daemon_work_roots_for_layout(&layout);
        assert_eq!(roots, vec![work, fixture]);
        assert!(
            roots
                .iter()
                .all(|root| root != Path::new("/root/.velnor/runner/_work")),
            "leftover scan must not use the empty default home path"
        );
        fs::remove_dir_all(prefix).ok();
    }

    #[test]
    fn explicit_config_discovers_its_work_root_without_scanning_var_lib() {
        let root = std::env::temp_dir().join(format!(
            "velnor-explicit-config-work-root-{}",
            uuid::Uuid::new_v4()
        ));
        let config = root.join("daemon-config");
        let selected_work = config.join("_work");
        let unrelated = root.join("var/lib/velnor-other/work");
        fs::create_dir_all(&selected_work).unwrap();
        fs::create_dir_all(&unrelated).unwrap();
        let layout = crate::storage::StorageLayout {
            cache_root: config.join("cache"),
            lib_root: config.clone(),
            run_root: config.join("run"),
            log_root: config.join("log"),
            mode: "explicit-config",
        };

        assert_eq!(
            discover_daemon_work_roots_for_layout(&layout),
            vec![selected_work]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_var_lib_scope_is_not_root_dot_velnor_leftover() {
        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-home-vs-var-{}",
            std::process::id()
        ));
        let lib = root.join("lib");
        let live_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let live = lib.join("velnor-fixture/work/slot-1").join(live_id);
        let home_orphan = root
            .join("root/.velnor/runner/_work/slot-1")
            .join("11111111-2222-3333-4444-555555555555");
        write_tree(&live);
        write_tree(&home_orphan);
        let live_ids = BTreeSet::from([live_id.to_string()]);
        let roots = discover_daemon_work_roots_in(&lib);
        let orphans = orphan_job_workspace_paths(&roots, &live_ids);
        assert!(
            orphans.is_empty(),
            "live /var/lib job must stay, got {orphans:?}"
        );
        assert!(live.exists());
        assert!(
            home_orphan.exists(),
            "/root/.velnor leftover is not a /var/lib work root and must not be swept as job GC"
        );
        fs::remove_dir_all(root).ok();
    }

    /// The defect: liveness was `docker ps` alone, so a job was invisible to
    /// the reaper in every window between containers — checking out, uploading
    /// artifacts, publishing a target generation, tearing BuildKit down. Each
    /// of those jobs holds a lease, holds its claim, or is actively writing;
    /// none of them is running a container.
    #[test]
    fn a_job_between_containers_is_not_reaped() {
        let root =
            std::env::temp_dir().join(format!("velnor-leftover-liveness-{}", uuid::Uuid::new_v4()));
        let work = root.join("velnor-fixture/work/slot-1");
        let run_root = root.join("run");
        let checking_out = "11111111-1111-1111-1111-111111111111";
        let uploading = "22222222-2222-2222-2222-222222222222";
        let publishing = "33333333-3333-3333-3333-333333333333";
        let abandoned = "44444444-4444-4444-4444-444444444444";
        for id in [checking_out, uploading, publishing, abandoned] {
            write_tree(&work.join(id));
        }
        // Every workspace is old enough that the idle floor alone would not
        // save it: only lease and claim evidence can.
        for id in [checking_out, uploading, publishing, abandoned] {
            crate::cache::test_clock::backdate(&work.join(id), WORKSPACE_MIN_IDLE * 2);
        }

        // Mid artifact upload and mid target publish: store leases are held.
        let leases = [
            crate::capacity::ScopeLease::acquire_for_job(
                &run_root,
                "actions-cache",
                &format!("repo/{}", crate::trust_scope::filesystem_key(uploading)),
                crate::capacity::JobOwnerId::parse(uploading).unwrap(),
                Duration::from_secs(3600),
            )
            .unwrap(),
            crate::capacity::ScopeLease::acquire_for_job(
                &run_root,
                "targets",
                &format!(
                    "workspace-v2/repo/ci.yml/{}",
                    crate::trust_scope::filesystem_key(publishing)
                ),
                crate::capacity::JobOwnerId::parse(publishing).unwrap(),
                Duration::from_secs(3600),
            )
            .unwrap(),
        ];
        // Mid checkout: no lease yet, but the host job claim is held.
        let claim = crate::job_claim::JobClaim::try_acquire(&run_root, "plan-uuid", checking_out)
            .unwrap()
            .unwrap();

        let coordinator =
            crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root).unwrap();
        let liveness = WorkspaceLiveness::collect(&run_root, BTreeSet::new());
        assert!(!liveness.evidence_incomplete);
        assert_eq!(
            liveness.leased,
            BTreeSet::from([uploading.to_string(), publishing.to_string()]),
            "active persisted leases must expose their explicit owner ids"
        );
        let orphans = orphan_job_workspace_paths_with_liveness(
            std::slice::from_ref(&root.join("velnor-fixture/work")),
            &liveness,
        );

        assert_eq!(
            orphans,
            vec![work.join(abandoned)],
            "only the workspace with no container, no lease, no claim and no \
             recent writes may be reclaimed"
        );
        drop(claim);
        drop(coordinator);
        drop(leases);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn generic_lease_with_uuid_scope_suffix_does_not_prove_job_liveness() {
        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-generic-lease-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("velnor-fixture/work/slot-1");
        let run_root = root.join("run");
        let job_id = "55555555-5555-4555-8555-555555555555";
        let workspace = work.join(job_id);
        write_tree(&workspace);
        crate::cache::test_clock::backdate(&workspace, WORKSPACE_MIN_IDLE * 2);
        let lease = crate::capacity::ScopeLease::acquire(
            &run_root,
            "cargo",
            &format!("registry/{job_id}"),
            Duration::from_secs(3600),
        )
        .unwrap();

        let coordinator =
            crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root).unwrap();
        let liveness = WorkspaceLiveness::collect(&run_root, BTreeSet::new());
        assert!(!liveness.evidence_incomplete);
        assert!(
            liveness.leased.is_empty(),
            "generic leases have no owner id"
        );
        assert_eq!(
            orphan_job_workspace_paths_with_liveness(
                std::slice::from_ref(&root.join("velnor-fixture/work")),
                &liveness,
            ),
            vec![workspace.clone()],
            "a UUID-looking scope suffix cannot substitute for persisted ownership"
        );
        drop(coordinator);
        drop(lease);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn malformed_lease_owner_evidence_blocks_workspace_reclamation() {
        let root = std::env::temp_dir().join(format!(
            "velnor-leftover-malformed-lease-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("velnor-fixture/work/slot-1");
        let run_root = root.join("run");
        let job_id = "66666666-6666-4666-8666-666666666666";
        let workspace = work.join(job_id);
        write_tree(&workspace);
        crate::cache::test_clock::backdate(&workspace, WORKSPACE_MIN_IDLE * 2);
        let scope = format!("registry/{}", crate::trust_scope::filesystem_key(job_id));
        let lease_path = run_root.join("leases__trust_scope_v2/cargo").join(format!(
            "{}.json",
            crate::trust_scope::filesystem_key(&scope)
        ));
        fs::create_dir_all(lease_path.parent().unwrap()).unwrap();
        fs::write(
            &lease_path,
            serde_json::json!({
                "scope": scope,
                "pid": std::process::id(),
                "created_unix": 1_800_000_000_u64,
            })
            .to_string(),
        )
        .unwrap();

        let coordinator =
            crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root).unwrap();
        let liveness = WorkspaceLiveness::collect(&run_root, BTreeSet::new());
        assert!(liveness.evidence_incomplete);
        assert!(
            orphan_job_workspace_paths_with_liveness(
                std::slice::from_ref(&root.join("velnor-fixture/work")),
                &liveness,
            )
            .is_empty(),
            "malformed lease metadata must make the reaper delete nothing"
        );
        assert!(workspace.exists());
        drop(coordinator);
        fs::remove_dir_all(root).ok();
    }

    /// A workspace a job is actively writing is live even with no lease and no
    /// claim: the reaper must never race a job that is between steps.
    #[test]
    fn a_workspace_being_written_is_live_without_any_lease() {
        let root =
            std::env::temp_dir().join(format!("velnor-leftover-hot-{}", uuid::Uuid::new_v4()));
        let work = root.join("velnor-fixture/work");
        let hot = work.join("slot-0/55555555-5555-5555-5555-555555555555");
        write_tree(&hot);
        let orphans = orphan_job_workspace_paths_with_liveness(
            std::slice::from_ref(&work),
            &container_liveness(&BTreeSet::new()),
        );
        assert!(orphans.is_empty(), "hot workspace was reaped: {orphans:?}");
        assert!(hot.exists());
        fs::remove_dir_all(root).ok();
    }

    /// Unreadable evidence must delete nothing. A reaper that cannot see the
    /// leases has no basis to call anything dead.
    #[test]
    fn incomplete_liveness_evidence_deletes_nothing() {
        let root =
            std::env::temp_dir().join(format!("velnor-leftover-blind-{}", uuid::Uuid::new_v4()));
        let work = root.join("velnor-fixture/work");
        let orphan = work.join("slot-0/66666666-6666-6666-6666-666666666666");
        cold_tree(&orphan);
        let orphans = orphan_job_workspace_paths_with_liveness(
            std::slice::from_ref(&work),
            &WorkspaceLiveness {
                evidence_incomplete: true,
                ..WorkspaceLiveness::default()
            },
        );
        assert!(orphans.is_empty());
        assert!(orphan.exists());
        fs::remove_dir_all(root).ok();
    }

    fn assert_leftover_docker_commands_are_safe(commands: &[Vec<String>]) {
        for command in commands {
            assert!(
                !leftover_docker_args_are_unsafe(command),
                "leftover reclaim issued unsafe docker command: {command:?}"
            );
        }
    }
}
