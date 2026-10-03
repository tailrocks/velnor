//! The one spelling of the mbx store layout, and the one-shot migration that
//! removes every layout the current code does not produce.
//!
//! One mbx store belongs to one (trust scope, repository) pair —
//! `StoreCatalog::mbx(scope)/<repository-id>` — and is mounted into every job
//! container at `/var/cache/mbx`. Inside it the layout is per slot, because
//! mbx serialises on a registrar flock and Cargo on a target-dir lock, so two
//! concurrent slots must never share either tree:
//!
//! ```text
//! <store>/slots/<slot>            MBX_CACHE_DIR    (registrar, leases, incremental cache)
//! <store>/targets/slots/<slot>    MBX_TARGET_ROOT  (managed Cargo targets)
//! ```
//!
//! Before the per-slot layout, `MBX_CACHE_DIR` was the store root and
//! `MBX_TARGET_ROOT` was `<store>/targets`. Those trees were never candidates
//! for any collector once the code moved on — a live host carried ~144 GiB of
//! them beside the per-slot trees — so [`migrate_at_daemon_start`] deletes
//! every entry of a store that is not `slots/` or `targets/slots/`, and every
//! historical unversioned `_velnor_mbx` work-root store once canonical
//! storage is in effect, and before pruning the versioned store without
//! canonical storage.
//! Nothing reads the old trees: mbx is content-addressed and a managed target
//! is a Cargo target, so deleting them costs one cold build per slot at most.
//!
//! The per-slot directories are also the units GC reasons about
//! ([`gc_roots`]): each `slots/<slot>` and `targets/slots/<slot>` is one
//! eviction candidate, scoped to its repository so a lease on the repository
//! protects all of them.

use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{
    ffi::{OsStr, OsString},
    io,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use anyhow::Context as _;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

#[cfg(target_os = "macos")]
use std::{cell::RefCell, collections::HashMap};

#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_MIGRATION_DEPTH: usize = 256;

/// Per-slot mbx cache directories below a store.
pub(crate) const SLOTS_DIR: &str = "slots";
/// Managed-target root below a store; per-slot trees live under
/// `targets/slots`.
pub(crate) const TARGETS_DIR: &str = "targets";

/// `MBX_CACHE_DIR` of one slot, below a store root (host or container side).
pub(crate) fn slot_cache_dir(store: &Path, slot_key: &str) -> PathBuf {
    store.join(SLOTS_DIR).join(slot_key)
}

/// `MBX_TARGET_ROOT` of one slot, below a store root (host or container side).
pub(crate) fn slot_target_dir(store: &Path, slot_key: &str) -> PathBuf {
    store.join(TARGETS_DIR).join(SLOTS_DIR).join(slot_key)
}

/// The GC roots below one mbx class root (`StoreCatalog::mbx(scope)`): for
/// every repository store, its `slots` and `targets/slots` directories, each
/// tagged with the repository id that is the store's lease scope. A class
/// root that does not exist yields nothing.
pub(crate) fn gc_roots(class_root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(repositories) = fs::read_dir(class_root) else {
        return Vec::new();
    };
    let mut roots: Vec<(String, PathBuf)> = repositories
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .flat_map(|entry| {
            let repository = entry.file_name().to_string_lossy().into_owned();
            let store = entry.path();
            [
                (repository.clone(), store.join(SLOTS_DIR)),
                (repository, store.join(TARGETS_DIR).join(SLOTS_DIR)),
            ]
        })
        .collect();
    roots.sort();
    roots
}

/// What the one-shot migration removed or reclaimed before a later entry failed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct MigrationReport {
    /// Paths fully removed, with bytes for inodes whose last link was removed.
    pub(crate) removed: Vec<(PathBuf, u64)>,
    /// Bytes reclaimed before a later recursive cleanup error stopped the walk.
    pub(crate) partial: Vec<(PathBuf, u64)>,
    /// Paths that could not be removed, with the error.
    pub(crate) failures: Vec<(PathBuf, String)>,
}

impl MigrationReport {
    pub(crate) fn total_bytes(&self) -> u64 {
        self.removed
            .iter()
            .chain(&self.partial)
            .map(|(_, bytes)| *bytes)
            .sum()
    }
}

/// Remove every mbx store tree the current layout does not produce.
///
/// * The historical unversioned `<work>/_velnor_mbx` root is removed whole in
///   either layout mode. Canonical startup also removes the versioned legacy
///   sibling because canonical store resolution never falls back to it.
/// * Under every mbx class root (`<cache-root>__trust_scope_v1/<filesystem-key>/compiler/mbx`,
///   or `<work>/_velnor_mbx__trust_scope_v1/<filesystem-key>` without canonical storage), every
///   repository store is pruned to `slots/` and `targets/slots/`: anything else
///   at the store root is the pre-slot `MBX_CACHE_DIR`, anything else under
///   `targets/` is the pre-slot `MBX_TARGET_ROOT`, and a non-directory entry
///   under either `slots/` directory was never written by the runner.
///
/// Idempotent and best-effort: a failure to remove one path is reported and
/// does not stop the rest.
pub(crate) fn migrate_at_daemon_start(
    work_root: &Path,
    layout: Option<&crate::storage::StorageLayout>,
) -> MigrationReport {
    let catalog = crate::store_catalog::StoreCatalog::for_work_root_with_layout(work_root, layout);
    let mut report = MigrationReport::default();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    migrate_at_daemon_start_secure(work_root, layout, &catalog, &mut report);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    report.failures.push((
        work_root.to_path_buf(),
        "secure MBX migration requires native mount-identity and no-follow descriptor operations"
            .to_owned(),
    ));
    report
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn migrate_at_daemon_start_secure(
    work_root: &Path,
    layout: Option<&crate::storage::StorageLayout>,
    catalog: &crate::store_catalog::StoreCatalog,
    report: &mut MigrationReport,
) {
    match layout {
        Some(layout) => {
            match MbxDir::open_configured_root(work_root) {
                Ok(Some(work)) => {
                    for stale_root in [catalog.old_legacy_mbx_root(), catalog.legacy_mbx_root()] {
                        if let Some(name) = stale_root.file_name() {
                            remove_reported(&work, name, &stale_root, report);
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => record_failure(work_root, error, report),
            }

            match open_filesystem_key_namespace(&layout.cache_root) {
                Ok(Some(namespace)) => migrate_keyed_namespace(
                    &namespace,
                    Path::new("compiler/mbx"),
                    catalog,
                    crate::trust_scope::filesystem_key_namespace(&layout.cache_root),
                    report,
                ),
                Ok(None) => {}
                Err(error) => record_failure(
                    &crate::trust_scope::filesystem_key_namespace(&layout.cache_root),
                    error,
                    report,
                ),
            }
        }
        None => match MbxDir::open_configured_root(work_root) {
            Ok(Some(work)) => {
                let old_legacy = catalog.old_legacy_mbx_root();
                if let Some(name) = old_legacy.file_name() {
                    remove_reported(&work, name, &old_legacy, report);
                }

                let legacy_path = catalog.legacy_mbx_root();
                let Some(name) = legacy_path.file_name() else {
                    record_failure(
                        &legacy_path,
                        anyhow::anyhow!("versioned MBX root has no path component"),
                        report,
                    );
                    return;
                };
                match work.open_directory(name) {
                    Ok(Some(namespace)) => migrate_keyed_namespace(
                        &namespace,
                        Path::new("."),
                        catalog,
                        legacy_path,
                        report,
                    ),
                    Ok(None) => {}
                    Err(error) => record_failure(&legacy_path, error, report),
                }
            }
            Ok(None) => {}
            Err(error) => record_failure(work_root, error, report),
        },
    }
}

/// Open the versioned canonical namespace as one no-follow child of the
/// configured cache-root parent. The cache-root parent is trusted config;
/// everything below it is opened relative to descriptors and rejects links
/// and mount-boundary crossings.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_filesystem_key_namespace(cache_root: &Path) -> anyhow::Result<Option<MbxDir>> {
    let namespace_path = crate::trust_scope::filesystem_key_namespace(cache_root);
    let parent_path = namespace_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("canonical trust namespace has no parent"))?;
    let name = namespace_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("canonical trust namespace has no final component"))?;
    let Some(parent) = MbxDir::open_configured_root(parent_path)? else {
        return Ok(None);
    };
    parent.open_directory(name)
}

/// Enumerate exact one-way key components. Never feed an encoded component
/// back through `StoreCatalog::mbx`, which would hash it a second time. The
/// catalog's encoded-key constructor is used only for the display path.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn migrate_keyed_namespace(
    namespace: &MbxDir,
    class_suffix: &Path,
    catalog: &crate::store_catalog::StoreCatalog,
    display_namespace: PathBuf,
    report: &mut MigrationReport,
) {
    let names = match namespace.entry_names() {
        Ok(names) => names,
        Err(error) => {
            record_failure(&display_namespace, error, report);
            return;
        }
    };
    for key in names {
        let Some(key_text) = key.to_str() else {
            continue;
        };
        if !crate::trust_scope::is_filesystem_key(key_text) {
            continue;
        }
        let class_path = catalog.mbx_from_filesystem_key(key_text);
        let key_path = if class_suffix == Path::new(".") {
            class_path.clone()
        } else {
            display_namespace.join(&key)
        };
        match namespace.entry_is_directory(&key) {
            Ok(Some(true)) => {}
            Ok(Some(false) | None) => continue,
            Err(error) => {
                record_failure(&key_path, error, report);
                continue;
            }
        }
        let key_dir = match namespace.open_directory(&key) {
            Ok(Some(directory)) => directory,
            Ok(None) => continue,
            Err(error) => {
                record_failure(&key_path, error, report);
                continue;
            }
        };
        match key_dir.open_relative_directory(class_suffix) {
            Ok(Some(class_root)) => {
                migrate_class_root(&class_root, &class_path, report);
            }
            Ok(None) => {}
            Err(error) => record_failure(&key_path.join(class_suffix), error, report),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn migrate_class_root(class_root: &MbxDir, display_path: &Path, report: &mut MigrationReport) {
    let names = match class_root.entry_names() {
        Ok(names) => names,
        Err(error) => {
            record_failure(display_path, error, report);
            return;
        }
    };
    for name in names {
        let path = display_path.join(&name);
        match class_root.entry_is_directory(&name) {
            Ok(Some(true)) => match class_root.open_directory(&name) {
                Ok(Some(store)) => migrate_store(&store, &path, report),
                Ok(None) => {}
                Err(error) => record_failure(&path, error, report),
            },
            Ok(Some(false)) => remove_reported(class_root, &name, &path, report),
            Ok(None) => {}
            Err(error) => record_failure(&path, error, report),
        }
    }
}

/// Prune one repository store to the per-slot layout.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn migrate_store(store: &MbxDir, display_path: &Path, report: &mut MigrationReport) {
    let names = match store.entry_names() {
        Ok(names) => names,
        Err(error) => {
            record_failure(display_path, error, report);
            return;
        }
    };
    for name in names {
        let path = display_path.join(&name);
        if name == SLOTS_DIR {
            match open_migration_directory(store, &name, &path, report) {
                Some(slots) => prune_to_slot_directories(&slots, &path, report),
                None => {}
            }
        } else if name == TARGETS_DIR {
            let Some(targets) = open_migration_directory(store, &name, &path, report) else {
                continue;
            };
            let children = match targets.entry_names() {
                Ok(children) => children,
                Err(error) => {
                    record_failure(&path, error, report);
                    continue;
                }
            };
            for child in children {
                let child_path = path.join(&child);
                if child == SLOTS_DIR {
                    if let Some(slots) =
                        open_migration_directory(&targets, &child, &child_path, report)
                    {
                        prune_to_slot_directories(&slots, &child_path, report);
                    }
                } else {
                    remove_reported(&targets, &child, &child_path, report);
                }
            }
        } else {
            remove_reported(store, &name, &path, report);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_migration_directory(
    parent: &MbxDir,
    name: &OsStr,
    display_path: &Path,
    report: &mut MigrationReport,
) -> Option<MbxDir> {
    match parent.entry_is_directory(name) {
        Ok(Some(true)) => match parent.open_directory(name) {
            Ok(Some(directory)) => Some(directory),
            Ok(None) => None,
            Err(error) => {
                record_failure(display_path, error, report);
                None
            }
        },
        Ok(Some(false)) => {
            remove_reported(parent, name, display_path, report);
            None
        }
        Ok(None) => None,
        Err(error) => {
            record_failure(display_path, error, report);
            None
        }
    }
}

/// A `slots/` directory holds slot directories and nothing else. Symlinked
/// slot parents are rejected and left in place; the cleanup never descends
/// through them.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prune_to_slot_directories(slots: &MbxDir, display_path: &Path, report: &mut MigrationReport) {
    let names = match slots.entry_names() {
        Ok(names) => names,
        Err(error) => {
            record_failure(display_path, error, report);
            return;
        }
    };
    for name in names {
        let path = display_path.join(&name);
        match slots.entry_is_directory(&name) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => remove_reported(slots, &name, &path, report),
            Ok(None) => {}
            Err(error) => record_failure(&path, error, report),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_reported(
    parent: &MbxDir,
    name: &OsStr,
    display_path: &Path,
    report: &mut MigrationReport,
) {
    let mut bytes_removed = 0;
    let result = preflight_tree_entry(parent, name, 0)
        .and_then(|()| parent.remove_tree_entry(name, &mut bytes_removed));
    record_removal_result(display_path, result, bytes_removed, report);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn record_removal_result(
    display_path: &Path,
    result: anyhow::Result<bool>,
    bytes_removed: u64,
    report: &mut MigrationReport,
) {
    match result {
        Ok(true) => report
            .removed
            .push((display_path.to_path_buf(), bytes_removed)),
        Ok(false) if bytes_removed > 0 => report
            .partial
            .push((display_path.to_path_buf(), bytes_removed)),
        Ok(false) => {}
        Err(error) => {
            if bytes_removed > 0 {
                report
                    .partial
                    .push((display_path.to_path_buf(), bytes_removed));
            }
            if !is_not_found(&error) {
                record_failure(display_path, error, report);
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn record_failure(path: &Path, error: anyhow::Error, report: &mut MigrationReport) {
    report
        .failures
        .push((path.to_path_buf(), format!("{error:#}")));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug)]
struct MbxDir {
    file: fs::File,
    display_path: PathBuf,
    #[cfg(target_os = "linux")]
    mount_id: u64,
    #[cfg(target_os = "macos")]
    mount_identity: crate::leftover_disk::MacOsMountIdentity,
    #[cfg(target_os = "macos")]
    cached_mount_entries: RefCell<Option<HashMap<OsString, (bool, bool)>>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, PartialEq, Eq)]
enum MbxEntryKind {
    Directory,
    RegularFile,
    Symlink,
    Other,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy)]
struct MbxEntryStat {
    kind: MbxEntryKind,
    device_major: u32,
    device_minor: u32,
    inode: u64,
    link_count: u64,
    size: u64,
    is_mountpoint: bool,
    #[cfg(target_os = "linux")]
    mount_id: u64,
}

#[cfg(target_os = "linux")]
impl MbxEntryStat {
    fn from_statx(stat: rustix::fs::Statx) -> anyhow::Result<Self> {
        if !stat.stx_mask.contains(rustix::fs::StatxFlags::MNT_ID) {
            anyhow::bail!("filesystem does not provide MBX mount identity");
        }
        let kind = rustix::fs::FileType::from_raw_mode(stat.stx_mode);
        Ok(Self {
            kind: if kind == rustix::fs::FileType::Directory {
                MbxEntryKind::Directory
            } else if kind == rustix::fs::FileType::RegularFile {
                MbxEntryKind::RegularFile
            } else if kind == rustix::fs::FileType::Symlink {
                MbxEntryKind::Symlink
            } else {
                MbxEntryKind::Other
            },
            device_major: stat.stx_dev_major,
            device_minor: stat.stx_dev_minor,
            inode: stat.stx_ino,
            link_count: u64::from(stat.stx_nlink),
            size: stat.stx_size,
            is_mountpoint: false,
            mount_id: stat.stx_mnt_id,
        })
    }

    fn same_object(self, other: Self) -> bool {
        self.kind == other.kind
            && self.device_major == other.device_major
            && self.device_minor == other.device_minor
            && self.inode == other.inode
            && self.mount_id == other.mount_id
    }

    fn same_file(self, other: Self) -> bool {
        self.same_object(other) && self.size == other.size
    }
}

#[cfg(target_os = "linux")]
impl MbxDir {
    fn open_configured_root(path: &Path) -> anyhow::Result<Option<Self>> {
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "canonicalize configured MBX migration root {}",
                        path.display()
                    )
                });
            }
        };
        Self::open_absolute_no_follow(&canonical).map(Some)
    }

    fn open_absolute_no_follow(path: &Path) -> anyhow::Result<Self> {
        if !path.is_absolute() {
            anyhow::bail!("MBX migration root is not absolute: {}", path.display());
        }
        let root = rustix::fs::openat(
            rustix::fs::CWD,
            Path::new("/"),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(io::Error::from)
        .context("open filesystem root for MBX migration")?;
        let root: fs::File = root.into();
        let mut current = Self {
            mount_id: mount_id_for_fd(&root)?,
            file: root,
            display_path: PathBuf::from("/"),
        };
        for component in path.components() {
            if let std::path::Component::Normal(name) = component {
                current = current
                    .open_configured_directory(name)?
                    .with_context(|| format!("open MBX migration root {}", path.display()))?;
            }
        }
        Ok(current)
    }

    /// Cross mountpoints only while opening the configured host root. This
    /// establishes the mount identity that every guest-writable descendant
    /// must keep.
    fn open_configured_directory(&self, name: &OsStr) -> anyhow::Result<Option<Self>> {
        let display_path = self.display_path.join(name);
        let Some(named) = self.entry_stat(name)? else {
            return Ok(None);
        };
        if named.kind == MbxEntryKind::Symlink {
            anyhow::bail!(
                "MBX migration refuses symlink configured root {}",
                display_path.display()
            );
        }
        if named.kind != MbxEntryKind::Directory {
            anyhow::bail!(
                "MBX migration root is not a directory: {}",
                display_path.display()
            );
        }
        let file = rustix::fs::openat(
            &self.file,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(io::Error::from)
        .with_context(|| format!("open configured MBX root {}", display_path.display()))?;
        let file: fs::File = file.into();
        let opened = stat_for_fd(&file)?;
        if !named.same_object(opened) {
            anyhow::bail!(
                "configured MBX root changed during open: {}",
                display_path.display()
            );
        }
        Ok(Some(Self {
            file,
            display_path,
            mount_id: opened.mount_id,
        }))
    }

    fn open_directory(&self, name: &OsStr) -> anyhow::Result<Option<Self>> {
        let display_path = self.display_path.join(name);
        let Some(named) = self.entry_stat(name)? else {
            return Ok(None);
        };
        if named.kind == MbxEntryKind::Symlink {
            anyhow::bail!(
                "MBX migration refuses symlink parent {}",
                display_path.display()
            );
        }
        if named.kind != MbxEntryKind::Directory {
            anyhow::bail!(
                "MBX migration parent is not a directory: {}",
                display_path.display()
            );
        }
        if named.is_mountpoint || named.mount_id != self.mount_id {
            anyhow::bail!(
                "MBX migration refuses mountpoint parent {}",
                display_path.display()
            );
        }
        let file = rustix::fs::openat2(
            &self.file,
            Path::new(name),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
            rustix::fs::ResolveFlags::BENEATH
                | rustix::fs::ResolveFlags::NO_XDEV
                | rustix::fs::ResolveFlags::NO_SYMLINKS
                | rustix::fs::ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(io::Error::from)
        .with_context(|| {
            format!(
                "open MBX migration directory without crossing mounts {}",
                display_path.display()
            )
        })?;
        let file: fs::File = file.into();
        let opened = stat_for_fd(&file)?;
        if !named.same_object(opened) || opened.kind != MbxEntryKind::Directory {
            anyhow::bail!(
                "MBX migration directory changed during open: {}",
                display_path.display()
            );
        }
        Ok(Some(Self {
            file,
            display_path,
            mount_id: self.mount_id,
        }))
    }

    fn open_relative_directory(&self, relative: &Path) -> anyhow::Result<Option<Self>> {
        let mut current = self.try_clone()?;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                if matches!(component, std::path::Component::CurDir) {
                    continue;
                }
                anyhow::bail!(
                    "invalid MBX migration relative path: {}",
                    relative.display()
                );
            };
            match current.open_directory(name)? {
                Some(child) => current = child,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    fn entry_names(&self) -> anyhow::Result<Vec<OsString>> {
        let entries = rustix::fs::Dir::read_from(&self.file)
            .map_err(io::Error::from)
            .with_context(|| {
                format!(
                    "read MBX migration directory {}",
                    self.display_path.display()
                )
            })?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io::Error::from).with_context(|| {
                format!(
                    "read MBX migration directory {}",
                    self.display_path.display()
                )
            })?;
            let name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
            if name != "." && name != ".." {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    fn entry_is_directory(&self, name: &OsStr) -> anyhow::Result<Option<bool>> {
        let Some(stat) = self.entry_stat(name)? else {
            return Ok(None);
        };
        if stat.kind == MbxEntryKind::Symlink {
            anyhow::bail!(
                "MBX migration refuses symlink parent {}",
                self.display_path.join(name).display()
            );
        }
        if stat.is_mountpoint || stat.mount_id != self.mount_id {
            anyhow::bail!(
                "MBX migration refuses mountpoint entry {}",
                self.display_path.join(name).display()
            );
        }
        Ok(Some(stat.kind == MbxEntryKind::Directory))
    }

    fn entry_stat(&self, name: &OsStr) -> anyhow::Result<Option<MbxEntryStat>> {
        match statx_at(
            &self.file,
            Path::new(name),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => {
                let mut entry = MbxEntryStat::from_statx(stat)?;
                entry.is_mountpoint = entry.mount_id != self.mount_id;
                Ok(Some(entry))
            }
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(io::Error::from(error)).with_context(|| {
                format!(
                    "inspect MBX migration entry {}",
                    self.display_path.join(name).display()
                )
            }),
        }
    }

    fn try_clone(&self) -> anyhow::Result<Self> {
        Ok(Self {
            file: self
                .file
                .try_clone()
                .context("duplicate MBX migration directory")?,
            display_path: self.display_path.clone(),
            mount_id: self.mount_id,
        })
    }

    fn remove_tree_entry(&self, name: &OsStr, bytes_removed: &mut u64) -> anyhow::Result<bool> {
        validate_entry_name(name)?;
        remove_tree_at(self, name, 0, bytes_removed).with_context(|| {
            format!(
                "remove MBX migration entry {}",
                self.display_path.join(name).display()
            )
        })
    }
}

#[cfg(target_os = "macos")]
impl MbxEntryStat {
    fn from_macos_stat(stat: &libc::stat) -> Self {
        let kind_bits = stat.st_mode & libc::S_IFMT;
        let kind = if kind_bits == libc::S_IFDIR {
            MbxEntryKind::Directory
        } else if kind_bits == libc::S_IFREG {
            MbxEntryKind::RegularFile
        } else if kind_bits == libc::S_IFLNK {
            MbxEntryKind::Symlink
        } else {
            MbxEntryKind::Other
        };
        Self {
            kind,
            device_major: (stat.st_dev as u64 >> 32) as u32,
            device_minor: stat.st_dev as u32,
            inode: stat.st_ino as u64,
            link_count: stat.st_nlink as u64,
            size: stat.st_size.max(0) as u64,
            is_mountpoint: false,
        }
    }

    fn same_object(self, other: Self) -> bool {
        self.kind == other.kind
            && self.device_major == other.device_major
            && self.device_minor == other.device_minor
            && self.inode == other.inode
    }

    fn same_file(self, other: Self) -> bool {
        self.same_object(other) && self.size == other.size
    }
}

#[cfg(target_os = "macos")]
impl MbxDir {
    fn open_configured_root(path: &Path) -> anyhow::Result<Option<Self>> {
        let canonical = match fs::canonicalize(path) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "canonicalize configured MBX migration root {}",
                        path.display()
                    )
                });
            }
        };
        Self::open_absolute_no_follow(&canonical).map(Some)
    }

    fn open_absolute_no_follow(path: &Path) -> anyhow::Result<Self> {
        use std::os::fd::FromRawFd as _;

        if !path.is_absolute() {
            anyhow::bail!("MBX migration root is not absolute: {}", path.display());
        }
        let root = std::ffi::CString::new("/").expect("static root path has no nul");
        // SAFETY: `root` is a valid terminated path string.
        let root_fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if root_fd < 0 {
            return Err(io::Error::last_os_error())
                .context("open filesystem root for MBX migration");
        }
        // SAFETY: open returned a new owned descriptor.
        let root = unsafe { fs::File::from_raw_fd(root_fd) };
        let root_mount = crate::leftover_disk::macos_directory_mount_identity(&root)
            .context("read filesystem root mount identity for MBX migration")?;
        let mut current = Self {
            file: root,
            display_path: PathBuf::from("/"),
            mount_identity: root_mount,
            cached_mount_entries: RefCell::new(None),
        };
        for component in path.components() {
            if let std::path::Component::Normal(name) = component {
                current = current
                    .open_configured_directory(name)?
                    .with_context(|| format!("open MBX migration root {}", path.display()))?;
            }
        }
        Ok(current)
    }

    /// Follow configured root mountpoints, but never configured symlinks.
    /// Once this root is established, descendants must retain its mount identity.
    fn open_configured_directory(&self, name: &OsStr) -> anyhow::Result<Option<Self>> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let display_path = self.display_path.join(name);
        let Some(named) = self.entry_stat(name)? else {
            return Ok(None);
        };
        if named.kind == MbxEntryKind::Symlink {
            anyhow::bail!(
                "MBX migration refuses symlink configured root {}",
                display_path.display()
            );
        }
        if named.kind != MbxEntryKind::Directory {
            anyhow::bail!(
                "MBX migration root is not a directory: {}",
                display_path.display()
            );
        }
        let name_c = c_name(name)?;
        // SAFETY: the parent descriptor is live and `name_c` is terminated.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("open configured MBX root {}", display_path.display()));
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let opened = stat_for_fd(&file)?;
        if !named.same_object(opened) || opened.kind != MbxEntryKind::Directory {
            anyhow::bail!(
                "configured MBX root changed during open: {}",
                display_path.display()
            );
        }
        let mount_identity = crate::leftover_disk::macos_directory_mount_identity(&file)?;
        Ok(Some(Self {
            file,
            display_path,
            mount_identity,
            cached_mount_entries: RefCell::new(None),
        }))
    }

    fn open_directory(&self, name: &OsStr) -> anyhow::Result<Option<Self>> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let display_path = self.display_path.join(name);
        let Some(named) = self.entry_stat(name)? else {
            return Ok(None);
        };
        if named.kind == MbxEntryKind::Symlink {
            anyhow::bail!(
                "MBX migration refuses symlink parent {}",
                display_path.display()
            );
        }
        if named.kind != MbxEntryKind::Directory {
            anyhow::bail!(
                "MBX migration parent is not a directory: {}",
                display_path.display()
            );
        }
        if named.is_mountpoint {
            anyhow::bail!(
                "MBX migration refuses mountpoint parent {}",
                display_path.display()
            );
        }
        let name_c = c_name(name)?;
        // SAFETY: the parent descriptor is live and `name_c` is terminated.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error()).with_context(|| {
                format!("open MBX migration directory {}", display_path.display())
            });
        }
        // SAFETY: openat returned a new owned descriptor.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let opened = stat_for_fd(&file)?;
        let opened_mount = crate::leftover_disk::macos_directory_mount_identity(&file)?;
        if !named.same_object(opened)
            || opened.kind != MbxEntryKind::Directory
            || opened_mount != self.mount_identity
        {
            anyhow::bail!(
                "MBX migration directory changed mount or identity: {}",
                display_path.display()
            );
        }
        Ok(Some(Self {
            file,
            display_path,
            mount_identity: self.mount_identity.clone(),
            cached_mount_entries: RefCell::new(None),
        }))
    }

    fn open_relative_directory(&self, relative: &Path) -> anyhow::Result<Option<Self>> {
        let mut current = self.try_clone()?;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                if matches!(component, std::path::Component::CurDir) {
                    continue;
                }
                anyhow::bail!(
                    "invalid MBX migration relative path: {}",
                    relative.display()
                );
            };
            match current.open_directory(name)? {
                Some(child) => current = child,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    fn entry_names(&self) -> anyhow::Result<Vec<OsString>> {
        self.refresh_mount_status_entries()?;
        let entries = self.cached_mount_entries.borrow();
        let Some(entries) = entries.as_ref() else {
            anyhow::bail!("MBX mount-status scan returned no entries");
        };
        let mut names: Vec<_> = entries.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    fn entry_is_directory(&self, name: &OsStr) -> anyhow::Result<Option<bool>> {
        let Some(stat) = self.entry_stat(name)? else {
            return Ok(None);
        };
        if stat.kind == MbxEntryKind::Symlink {
            anyhow::bail!(
                "MBX migration refuses symlink parent {}",
                self.display_path.join(name).display()
            );
        }
        if stat.is_mountpoint {
            anyhow::bail!(
                "MBX migration refuses mountpoint entry {}",
                self.display_path.join(name).display()
            );
        }
        Ok(Some(stat.kind == MbxEntryKind::Directory))
    }

    fn entry_stat(&self, name: &OsStr) -> anyhow::Result<Option<MbxEntryStat>> {
        use std::os::fd::AsRawFd as _;

        validate_entry_name(name)?;
        let name_c = c_name(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `stat` points to writable storage and the descriptor/name are valid.
        let result = unsafe {
            libc::fstatat(
                self.file.as_raw_fd(),
                name_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error).with_context(|| {
                format!(
                    "inspect MBX migration entry {}",
                    self.display_path.join(name).display()
                )
            });
        }
        // SAFETY: fstatat initialized the struct on success.
        let stat = unsafe { stat.assume_init() };
        let mut entry = MbxEntryStat::from_macos_stat(&stat);
        if entry.kind == MbxEntryKind::Directory {
            let (is_directory, is_mountpoint) = self.bulk_entry(name)?.with_context(|| {
                format!(
                    "macOS bulk listing omitted MBX entry {}",
                    self.display_path.join(name).display()
                )
            })?;
            if !is_directory {
                anyhow::bail!("MBX entry type changed during mount-status scan");
            }
            entry.is_mountpoint = is_mountpoint;
        }
        Ok(Some(entry))
    }

    fn bulk_entry(&self, name: &OsStr) -> anyhow::Result<Option<(bool, bool)>> {
        self.mount_status_entries()?;
        Ok(self
            .cached_mount_entries
            .borrow()
            .as_ref()
            .and_then(|entries| entries.get(name).copied()))
    }

    fn mount_status_entries(&self) -> anyhow::Result<()> {
        if self.cached_mount_entries.borrow().is_none() {
            self.refresh_mount_status_entries()?;
        }
        Ok(())
    }

    fn refresh_mount_status_entries(&self) -> anyhow::Result<()> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};

        let dot = std::ffi::CString::new(".").expect("static dot path has no nul");
        // SAFETY: parent descriptor is live and `dot` is a terminated path.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                dot.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("open fresh MBX mount-status fd");
        }
        // SAFETY: openat returned a new owned descriptor.
        let scan = unsafe { fs::File::from_raw_fd(fd) };
        let entries = crate::leftover_disk::macos_bulk_directory_entries(&scan)
            .context("read MBX directory entries with mount status")?;
        *self.cached_mount_entries.borrow_mut() = Some(
            entries
                .into_iter()
                .map(|entry| (entry.name, (entry.is_directory, entry.is_mountpoint)))
                .collect(),
        );
        Ok(())
    }

    fn try_clone(&self) -> anyhow::Result<Self> {
        Ok(Self {
            file: self
                .file
                .try_clone()
                .context("duplicate MBX migration directory")?,
            display_path: self.display_path.clone(),
            mount_identity: self.mount_identity.clone(),
            cached_mount_entries: RefCell::new(None),
        })
    }

    fn remove_tree_entry(&self, name: &OsStr, bytes_removed: &mut u64) -> anyhow::Result<bool> {
        validate_entry_name(name)?;
        remove_tree_at(self, name, 0, bytes_removed).with_context(|| {
            format!(
                "remove MBX migration entry {}",
                self.display_path.join(name).display()
            )
        })
    }
}

#[cfg(target_os = "macos")]
fn c_name(name: &OsStr) -> anyhow::Result<std::ffi::CString> {
    std::ffi::CString::new(name.as_bytes()).context("MBX entry name contains NUL")
}

#[cfg(target_os = "macos")]
fn stat_for_fd(file: &fs::File) -> anyhow::Result<MbxEntryStat> {
    use std::os::fd::AsRawFd as _;

    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `stat` points to writable storage and the descriptor is valid.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error()).context("read MBX descriptor identity");
    }
    // SAFETY: fstat initialized the struct on success.
    Ok(MbxEntryStat::from_macos_stat(&unsafe {
        stat.assume_init()
    }))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn validate_entry_name(name: &OsStr) -> anyhow::Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        anyhow::bail!("invalid MBX migration directory entry name");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn statx_at(
    directory: &fs::File,
    path: impl rustix::path::Arg,
    flags: rustix::fs::AtFlags,
) -> Result<rustix::fs::Statx, rustix::io::Errno> {
    let stat = rustix::fs::statx(
        directory,
        path,
        flags,
        rustix::fs::StatxFlags::BASIC_STATS | rustix::fs::StatxFlags::MNT_ID,
    )?;
    if !stat.stx_mask.contains(rustix::fs::StatxFlags::MNT_ID) {
        return Err(rustix::io::Errno::NOSYS);
    }
    Ok(stat)
}

#[cfg(target_os = "linux")]
fn stat_for_fd(file: &fs::File) -> anyhow::Result<MbxEntryStat> {
    let stat = statx_at(file, Path::new(""), rustix::fs::AtFlags::EMPTY_PATH)
        .map_err(io::Error::from)
        .context("read MBX descriptor mount identity")?;
    MbxEntryStat::from_statx(stat)
}

#[cfg(target_os = "linux")]
fn mount_id_at(
    directory: &fs::File,
    path: impl rustix::path::Arg,
    flags: rustix::fs::AtFlags,
) -> anyhow::Result<u64> {
    statx_at(directory, path, flags)
        .map(|stat| stat.stx_mnt_id)
        .map_err(io::Error::from)
        .context("read MBX mount identity")
}

#[cfg(target_os = "linux")]
fn mount_id_for_fd(file: &fs::File) -> anyhow::Result<u64> {
    mount_id_at(file, Path::new(""), rustix::fs::AtFlags::EMPTY_PATH)
}

/// Validate a whole removal subtree before deleting any of it. This keeps an
/// existing nested mountpoint from causing a partial cleanup of its siblings;
/// the deletion walk repeats each identity check to catch changes after this
/// pass.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn preflight_tree_entry(parent: &MbxDir, name: &OsStr, depth: usize) -> anyhow::Result<()> {
    preflight_tree_entry_with_mountpoint(parent, name, depth, &|_, stat| stat.is_mountpoint)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn preflight_tree_entry_with_mountpoint(
    parent: &MbxDir,
    name: &OsStr,
    depth: usize,
    is_mountpoint: &impl Fn(&OsStr, MbxEntryStat) -> bool,
) -> anyhow::Result<()> {
    if depth > MAX_MIGRATION_DEPTH {
        anyhow::bail!("MBX entry exceeds secure preflight depth {MAX_MIGRATION_DEPTH}");
    }
    let Some(stat) = parent.entry_stat(name)? else {
        return Ok(());
    };
    if is_mountpoint(name, stat) {
        anyhow::bail!(
            "MBX migration refuses mountpoint entry {}",
            parent.display_path.join(name).display()
        );
    }
    if stat.kind != MbxEntryKind::Directory {
        return Ok(());
    }
    let directory = parent
        .open_directory(name)?
        .ok_or_else(|| anyhow::anyhow!("MBX directory disappeared during preflight"))?;
    for child in directory.entry_names()? {
        preflight_tree_entry_with_mountpoint(&directory, &child, depth + 1, is_mountpoint)?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_tree_at(
    parent: &MbxDir,
    name: &OsStr,
    depth: usize,
    bytes_removed: &mut u64,
) -> anyhow::Result<bool> {
    remove_tree_at_with_mountpoint(parent, name, depth, bytes_removed, &|_, stat| {
        stat.is_mountpoint
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_tree_at_with_mountpoint(
    parent: &MbxDir,
    name: &OsStr,
    depth: usize,
    bytes_removed: &mut u64,
    is_mountpoint: &impl Fn(&OsStr, MbxEntryStat) -> bool,
) -> anyhow::Result<bool> {
    if depth > MAX_MIGRATION_DEPTH {
        anyhow::bail!("MBX entry exceeds secure cleanup depth {MAX_MIGRATION_DEPTH}");
    }
    let Some(stat) = parent.entry_stat(name)? else {
        return Ok(false);
    };
    if is_mountpoint(name, stat) {
        anyhow::bail!(
            "MBX migration refuses mountpoint entry {}",
            parent.display_path.join(name).display()
        );
    }

    if stat.kind == MbxEntryKind::RegularFile {
        return remove_regular_file(parent, name, stat, bytes_removed);
    }

    if stat.kind != MbxEntryKind::Directory {
        verify_entry_unchanged(parent, name, stat, false)?;
        return unlink_entry(parent, name, false);
    }

    let directory = parent
        .open_directory(name)?
        .ok_or_else(|| anyhow::anyhow!("MBX directory disappeared during cleanup"))?;
    let entries = directory.entry_names()?;
    for child in entries {
        remove_tree_at_with_mountpoint(
            &directory,
            &child,
            depth + 1,
            bytes_removed,
            is_mountpoint,
        )?;
    }

    let Some(current) = parent.entry_stat(name)? else {
        anyhow::bail!("MBX directory disappeared during secure cleanup");
    };
    if !stat.same_object(current) {
        anyhow::bail!("MBX directory changed during secure cleanup");
    }
    unlink_entry(parent, name, true)
}

#[cfg(target_os = "linux")]
fn remove_regular_file(
    parent: &MbxDir,
    name: &OsStr,
    named: MbxEntryStat,
    bytes_removed: &mut u64,
) -> anyhow::Result<bool> {
    let file = match rustix::fs::openat2(
        &parent.file,
        Path::new(name),
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
        rustix::fs::ResolveFlags::BENEATH
            | rustix::fs::ResolveFlags::NO_XDEV
            | rustix::fs::ResolveFlags::NO_SYMLINKS
            | rustix::fs::ResolveFlags::NO_MAGICLINKS,
    ) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(error) => {
            return Err(io::Error::from(error))
                .context("open MBX file identity without following links or crossing mounts");
        }
    };
    let file: fs::File = file.into();
    let opened = stat_for_fd(&file)?;
    if !named.same_file(opened) {
        anyhow::bail!("MBX file changed during secure open");
    }
    verify_entry_unchanged(parent, name, opened, true)?;
    if !unlink_entry(parent, name, false)? {
        return Ok(false);
    }
    let after =
        stat_for_fd(&file).context("measure unlinked MBX file for reclaimed-byte report")?;
    if after.link_count == 0 {
        *bytes_removed = (*bytes_removed).saturating_add(after.size);
    }
    Ok(true)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verify_entry_unchanged(
    parent: &MbxDir,
    name: &OsStr,
    expected: MbxEntryStat,
    compare_size: bool,
) -> anyhow::Result<()> {
    let Some(current) = parent.entry_stat(name)? else {
        anyhow::bail!("MBX entry disappeared during secure cleanup");
    };
    let unchanged = if compare_size {
        expected.same_file(current)
    } else {
        expected.same_object(current)
    };
    if !unchanged || current.is_mountpoint {
        anyhow::bail!("MBX entry changed during secure cleanup");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn unlink_entry(parent: &MbxDir, name: &OsStr, directory: bool) -> anyhow::Result<bool> {
    let flags = if directory {
        rustix::fs::AtFlags::REMOVEDIR
    } else {
        rustix::fs::AtFlags::empty()
    };
    match rustix::fs::unlinkat(&parent.file, name, flags) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(io::Error::from(error)).context("unlink MBX migration entry"),
    }
}

#[cfg(target_os = "macos")]
fn unlink_entry(parent: &MbxDir, name: &OsStr, directory: bool) -> anyhow::Result<bool> {
    use std::os::fd::AsRawFd as _;

    let name = c_name(name)?;
    let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: parent descriptor is live and name is a validated component.
    let result = unsafe { libc::unlinkat(parent.file.as_raw_fd(), name.as_ptr(), flags) };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(error).context("unlink MBX migration entry")
    }
}

#[cfg(target_os = "macos")]
fn remove_regular_file(
    parent: &MbxDir,
    name: &OsStr,
    named: MbxEntryStat,
    bytes_removed: &mut u64,
) -> anyhow::Result<bool> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let name_c = c_name(name)?;
    // O_NONBLOCK prevents a race that replaces the entry with a FIFO from
    // hanging daemon startup.
    // SAFETY: parent descriptor is live and name_c is a validated component.
    let fd = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name_c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(false);
        }
        return Err(error).context("open MBX file without following symlinks");
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let opened = stat_for_fd(&file)?;
    let opened_mount = crate::leftover_disk::macos_directory_mount_identity(&file)?;
    if !named.same_file(opened)
        || opened.kind != MbxEntryKind::RegularFile
        || opened_mount != parent.mount_identity
    {
        anyhow::bail!("MBX file changed during secure open");
    }
    verify_entry_unchanged(parent, name, opened, true)?;
    if !unlink_entry(parent, name, false)? {
        return Ok(false);
    }
    let after =
        stat_for_fd(&file).context("measure unlinked MBX file for reclaimed-byte report")?;
    if after.link_count == 0 {
        *bytes_removed = (*bytes_removed).saturating_add(after.size);
    }
    Ok(true)
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

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("velnor-mbx-store-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; bytes]).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_report_keeps_bytes_removed_before_a_later_failure() {
        let path = PathBuf::from("store/obsolete");
        let mut report = MigrationReport::default();

        record_removal_result(
            &path,
            Err(anyhow::anyhow!("later entry failed")),
            23,
            &mut report,
        );

        assert!(report.removed.is_empty());
        assert_eq!(report.partial, vec![(path.clone(), 23)]);
        assert_eq!(report.total_bytes(), 23);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, path);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_counts_a_hardlinked_file_only_when_its_last_link_is_removed() {
        use std::ffi::OsStr;

        let prefix = temp_root("hardlinked-bytes");
        let root = prefix.join("root");
        let candidate = root.join("obsolete-tree");
        let first_link = candidate.join("a-first");
        let second_link = candidate.join("b-second");
        write(&first_link, 37);
        fs::hard_link(&first_link, &second_link).unwrap();
        let parent = MbxDir::open_configured_root(&root).unwrap().unwrap();

        let mut bytes_removed = 0;
        let removed = parent
            .remove_tree_entry(OsStr::new("obsolete-tree"), &mut bytes_removed)
            .unwrap();

        assert!(removed);
        assert_eq!(bytes_removed, 37);
        assert!(!candidate.exists());
        fs::remove_dir_all(&prefix).ok();
    }

    fn layout(prefix: &Path) -> crate::storage::StorageLayout {
        crate::storage::StorageLayout::from_prefix(prefix)
    }

    /// The layout the runner produces, spelled once here and consumed by the
    /// container spec: the executor's mounts and the GC roots must agree.
    #[test]
    fn slot_directories_are_the_only_layout() {
        let store = Path::new("/var/cache/mbx");
        assert_eq!(
            slot_cache_dir(store, "slot-3"),
            Path::new("/var/cache/mbx/slots/slot-3")
        );
        assert_eq!(
            slot_target_dir(store, "slot-3"),
            Path::new("/var/cache/mbx/targets/slots/slot-3")
        );
    }

    /// The defect: the pre-slot `MBX_CACHE_DIR=<store>` and
    /// `MBX_TARGET_ROOT=<store>/targets` trees coexisted with the per-slot
    /// trees and no collector ever saw them. The migration removes exactly
    /// those and keeps every per-slot tree byte for byte.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_removes_pre_slot_trees_and_keeps_per_slot_trees() {
        let prefix = temp_root("pre-slot");
        let layout = layout(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let store = layout
            .cache_class("trusted", "compiler/mbx")
            .join("1255367013");
        // Pre-slot cache dir contents at the store root.
        write(&store.join("registrar.lock"), 1);
        write(&store.join("incremental/abc/lib.rlib"), 1000);
        write(&store.join("leases/xyz"), 10);
        // Pre-slot target root.
        write(&store.join("targets/debug/deps/foo.rlib"), 2000);
        write(&store.join("targets/CACHEDIR.TAG"), 5);
        write(&store.join("targets/.rustc_info.json"), 7);
        // Per-slot trees: the current layout.
        write(&store.join("slots/slot-1/incremental/abc/lib.rlib"), 300);
        write(&store.join("slots/slot-2/registrar.lock"), 1);
        write(&store.join("targets/slots/slot-1/debug/deps/foo.rlib"), 400);
        // A stray file where only slot directories belong.
        write(&store.join("slots/.DS_Store"), 3);
        write(&store.join("targets/slots/stray"), 4);
        // A second scope with a store that is already clean.
        let clean = layout
            .cache_class("untrusted", "compiler/mbx")
            .join("829618808");
        write(&clean.join("slots/slot-1/x"), 11);
        write(&clean.join("targets/slots/slot-1/y"), 12);

        let report = migrate_at_daemon_start(&work_root, Some(&layout));

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        let removed: Vec<PathBuf> = report.removed.iter().map(|(p, _)| p.clone()).collect();
        for legacy in [
            store.join("registrar.lock"),
            store.join("incremental"),
            store.join("leases"),
            store.join("targets/debug"),
            store.join("targets/CACHEDIR.TAG"),
            store.join("targets/.rustc_info.json"),
            store.join("slots/.DS_Store"),
            store.join("targets/slots/stray"),
        ] {
            assert!(
                removed.contains(&legacy),
                "{} not removed",
                legacy.display()
            );
            assert!(!legacy.exists());
        }
        assert_eq!(report.total_bytes(), 1 + 1000 + 10 + 2000 + 5 + 7 + 3 + 4);
        for kept in [
            store.join("slots/slot-1/incremental/abc/lib.rlib"),
            store.join("slots/slot-2/registrar.lock"),
            store.join("targets/slots/slot-1/debug/deps/foo.rlib"),
            clean.join("slots/slot-1/x"),
            clean.join("targets/slots/slot-1/y"),
        ] {
            assert!(kept.is_file(), "{} must survive", kept.display());
        }
        // Idempotent: a second pass finds nothing.
        assert_eq!(
            migrate_at_daemon_start(&work_root, Some(&layout)),
            MigrationReport::default()
        );
        fs::remove_dir_all(&prefix).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_uses_encoded_namespaces_and_leaves_ambiguous_aliases_to_package_purge() {
        let prefix = temp_root("encoded-namespaces");
        let layout = layout(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let scopes = ["pool/a", "pool_a"];
        let canonical_stores =
            scopes.map(|scope| layout.cache_class(scope, "compiler/mbx").join("1255367013"));
        for store in &canonical_stores {
            write(&store.join("incremental/old.rlib"), 8);
            write(&store.join("slots/slot-1/current.rlib"), 16);
        }
        // This old lossy alias is shared by both scope spellings. Startup MBX
        // migration must not consume it; the drained-upgrade package purge
        // owns removal of ambiguous pre-key canonical roots.
        let canonical_old_alias = layout
            .cache_root
            .join("pool_a/compiler/mbx/1255367013/incremental/ambiguous.rlib");
        write(&canonical_old_alias, 4);

        let canonical_report = migrate_at_daemon_start(&work_root, Some(&layout));

        assert!(canonical_report.failures.is_empty());
        for store in &canonical_stores {
            assert!(!store.join("incremental").exists());
            assert!(store.join("slots/slot-1/current.rlib").is_file());
        }
        assert!(
            canonical_old_alias.is_file(),
            "startup migration must leave ambiguous canonical aliases to the drained-upgrade purge"
        );

        let legacy_work_root = prefix.join("legacy/work");
        let legacy_stores = scopes.map(|scope| {
            crate::storage::legacy_store_root(&legacy_work_root, "_velnor_mbx")
                .join(crate::trust_scope::filesystem_key(scope))
                .join("1255367013")
        });
        for store in &legacy_stores {
            write(&store.join("incremental/old.rlib"), 8);
            write(&store.join("slots/slot-1/current.rlib"), 16);
        }
        let legacy_old_alias =
            legacy_work_root.join("_velnor_mbx/pool_a/1255367013/incremental/ambiguous.rlib");
        write(&legacy_old_alias, 4);

        let legacy_report = migrate_at_daemon_start(&legacy_work_root, None);

        assert!(legacy_report.failures.is_empty());
        for store in &legacy_stores {
            assert!(!store.join("incremental").exists());
            assert!(store.join("slots/slot-1/current.rlib").is_file());
        }
        assert!(
            !legacy_old_alias.exists(),
            "no-layout startup must purge the obsolete historical root"
        );
        fs::remove_dir_all(&prefix).ok();
    }

    /// With canonical storage in effect both historical and versioned legacy
    /// roots are purged; canonical resolution no longer falls back to them.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_removes_the_legacy_work_root_store_under_canonical_storage() {
        let prefix = temp_root("legacy-root");
        let layout = layout(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let legacy = work_root.join("_velnor_mbx/trusted/1255367013");
        write(&legacy.join("slots/slot-1/a"), 500);
        write(&legacy.join("incremental/b"), 700);
        let versioned_root = crate::storage::legacy_store_root(&work_root, "_velnor_mbx");
        let versioned = versioned_root
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join("1255367013");
        write(&versioned.join("incremental/isolated.rlib"), 9);
        fs::create_dir_all(&layout.cache_root).unwrap();

        let report = migrate_at_daemon_start(&work_root, Some(&layout));

        assert_eq!(
            report.removed,
            vec![
                (work_root.join("_velnor_mbx"), 1200),
                (versioned_root.clone(), 9)
            ]
        );
        assert!(!work_root.join("_velnor_mbx").exists());
        assert!(!versioned_root.exists());
        assert!(!versioned.join("incremental/isolated.rlib").exists());
        fs::remove_dir_all(&prefix).ok();
    }

    /// Without canonical storage the legacy root *is* the layout the code
    /// produces; it is pruned in place, not deleted.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_prunes_the_legacy_root_in_place_without_canonical_storage() {
        let prefix = temp_root("no-layout");
        let work_root = prefix.join("work");
        let store = work_root
            .join("_velnor_mbx__trust_scope_v1")
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join("42");
        write(&store.join("slots/slot-1/a"), 500);
        write(&store.join("incremental/b"), 700);

        let report = migrate_at_daemon_start(&work_root, None);

        assert_eq!(report.removed, vec![(store.join("incremental"), 700)]);
        assert!(store.join("slots/slot-1/a").is_file());
        fs::remove_dir_all(&prefix).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_removes_the_historical_unversioned_root_without_canonical_storage() {
        let prefix = temp_root("no-layout-old-root");
        let work_root = prefix.join("work");
        let old_store = work_root.join("_velnor_mbx/trusted/42");
        write(&old_store.join("incremental/old.rlib"), 700);

        let current_store = work_root
            .join("_velnor_mbx__trust_scope_v1")
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join("42");
        write(&current_store.join("slots/slot-1/current.rlib"), 500);
        write(&current_store.join("incremental/old.rlib"), 300);

        let report = migrate_at_daemon_start(&work_root, None);

        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(report
            .removed
            .contains(&(work_root.join("_velnor_mbx"), 700)));
        assert_eq!(report.total_bytes(), 1000);
        assert!(!work_root.join("_velnor_mbx").exists());
        assert!(!current_store.join("incremental").exists());
        assert!(current_store.join("slots/slot-1/current.rlib").is_file());
        fs::remove_dir_all(&prefix).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn migration_does_not_follow_symlinked_guest_store_parents_or_size_targets() {
        use std::os::unix::fs::symlink;

        let prefix = temp_root("symlink-parents");
        let layout = layout(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let outside = prefix.join("outside");
        write(&outside.join("host-marker"), 64);
        let store = layout
            .cache_class("trusted", "compiler/mbx")
            .join("1255367013");
        fs::create_dir_all(&store).unwrap();
        symlink(&outside, store.join(SLOTS_DIR)).unwrap();
        symlink(&outside, store.join("obsolete-link")).unwrap();

        let report = migrate_at_daemon_start(&work_root, Some(&layout));

        assert!(
            report
                .failures
                .iter()
                .any(|(path, _)| path == &store.join(SLOTS_DIR)),
            "symlinked slots parent must be rejected: {:?}",
            report.failures
        );
        assert_eq!(
            fs::read(outside.join("host-marker")).unwrap(),
            vec![1u8; 64]
        );
        assert!(store.join(SLOTS_DIR).is_symlink());
        assert!(!store.join("obsolete-link").exists());
        assert!(
            report.removed.contains(&(store.join("obsolete-link"), 0)),
            "sizing a removed symlink must not follow/count its target"
        );
        fs::remove_dir_all(&prefix).ok();
    }

    #[cfg(target_os = "linux")]
    struct TestMount(PathBuf);

    #[cfg(target_os = "linux")]
    impl Drop for TestMount {
        fn drop(&mut self) {
            use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

            let Ok(path) = CString::new(self.0.as_os_str().as_bytes()) else {
                return;
            };
            // SAFETY: `path` is a valid NUL-terminated path for this mounted
            // test fixture; the guard unmounts only the mount it created.
            unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) };
        }
    }

    #[cfg(target_os = "linux")]
    fn mount_test_tmpfs(path: &Path) -> Option<TestMount> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

        let source = CString::new("tmpfs").unwrap();
        let filesystem = CString::new("tmpfs").unwrap();
        let target = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: all pointers refer to NUL-terminated strings for this call;
        // data is null as tmpfs requires no mount options for the fixture.
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                filesystem.as_ptr(),
                libc::MS_NODEV | libc::MS_NOSUID,
                std::ptr::null(),
            )
        };
        if result == 0 {
            return Some(TestMount(path.to_path_buf()));
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::EINVAL) => None,
            _ => panic!("mount test tmpfs: {}", io::Error::last_os_error()),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn migration_refuses_nested_mountpoints_before_reading_or_removing_them() {
        let prefix = temp_root("nested-mount");
        let layout = layout(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let store = layout
            .cache_class("trusted", "compiler/mbx")
            .join("1255367013");
        let mountpoint = store.join("obsolete-mount");
        fs::create_dir_all(&mountpoint).unwrap();
        let Some(_mount) = mount_test_tmpfs(&mountpoint) else {
            fs::remove_dir_all(&prefix).ok();
            return;
        };
        let marker = mountpoint.join("outside-marker");
        fs::write(&marker, b"keep mounted data").unwrap();

        let report = migrate_at_daemon_start(&work_root, Some(&layout));

        assert!(
            report
                .failures
                .iter()
                .any(|(path, error)| path == &mountpoint && error.contains("mountpoint")),
            "nested mountpoint must fail closed: {:?}",
            report.failures
        );
        assert_eq!(fs::read(&marker).unwrap(), b"keep mounted data");
        drop(_mount);
        fs::remove_dir_all(&prefix).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn partial_recursive_cleanup_reports_bytes_before_mountpoint_failure() {
        use std::ffi::OsStr;

        let prefix = temp_root("partial-mount-race");
        let root = prefix.join("root");
        let candidate = root.join("obsolete-tree");
        let removed_file = candidate.join("a-first");
        let mountpoint = candidate.join("z-mounted");
        write(&removed_file, 37);
        fs::create_dir_all(&mountpoint).unwrap();
        let marker = mountpoint.join("outside-marker");
        fs::write(&marker, b"keep mounted data").unwrap();
        let parent = MbxDir::open_configured_root(&root).unwrap().unwrap();

        // Preflight sees an ordinary directory. Inject the mount-status result
        // for a later child during deletion to exercise the race path without
        // requiring CAP_SYS_ADMIN on the test runner.
        preflight_tree_entry(&parent, OsStr::new("obsolete-tree"), 0).unwrap();

        let mut bytes_removed = 0;
        let error = remove_tree_at_with_mountpoint(
            &parent,
            OsStr::new("obsolete-tree"),
            0,
            &mut bytes_removed,
            &|name, stat| stat.is_mountpoint || name == OsStr::new("z-mounted"),
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("mountpoint"), "{error:#}");
        assert_eq!(bytes_removed, 37);
        assert!(!removed_file.exists());
        assert_eq!(fs::read(&marker).unwrap(), b"keep mounted data");

        let mut report = MigrationReport::default();
        record_removal_result(&candidate, Err(error), bytes_removed, &mut report);
        assert_eq!(report.partial, vec![(candidate, 37)]);
        assert_eq!(report.total_bytes(), 37);

        fs::remove_dir_all(&prefix).ok();
    }

    /// GC candidates are the per-slot directories, each scoped to its
    /// repository, and nothing else — so the migration's invariant (only
    /// `slots/` and `targets/slots/` exist) is exactly what GC enumerates.
    #[test]
    fn gc_roots_are_the_per_slot_directories_scoped_by_repository() {
        let root = temp_root("gc-roots");
        write(&root.join("7/slots/slot-1/a"), 1);
        write(&root.join("7/targets/slots/slot-1/b"), 1);
        write(&root.join("9/slots/slot-2/c"), 1);
        write(&root.join("not-a-store"), 1);
        assert_eq!(
            gc_roots(&root),
            vec![
                ("7".to_owned(), root.join("7/slots")),
                ("7".to_owned(), root.join("7/targets/slots")),
                ("9".to_owned(), root.join("9/slots")),
                ("9".to_owned(), root.join("9/targets/slots")),
            ]
        );
        assert!(gc_roots(&root.join("missing")).is_empty());
        fs::remove_dir_all(&root).ok();
    }
}
