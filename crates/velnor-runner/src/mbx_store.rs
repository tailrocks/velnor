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
//! legacy `_velnor_mbx` work-root store once canonical storage is in effect.
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

/// What the one-shot migration deleted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct MigrationReport {
    /// Each removed path with the bytes it held.
    pub(crate) removed: Vec<(PathBuf, u64)>,
    /// Paths that could not be removed, with the error.
    pub(crate) failures: Vec<(PathBuf, String)>,
}

impl MigrationReport {
    pub(crate) fn total_bytes(&self) -> u64 {
        self.removed.iter().map(|(_, bytes)| *bytes).sum()
    }
}

/// Remove every mbx store tree the current layout does not produce.
///
/// * With canonical storage in effect, the legacy `<work>/_velnor_mbx` root
///   is removed whole: the catalog resolves to the canonical root for every
///   new store, and a legacy root that still exists is exactly the second
///   spelling that hides a store from GC.
/// * Under every mbx class root (`<cache-root>/<scope>/compiler/mbx`, or
///   the legacy root when there is no canonical storage), every repository
///   store is pruned to `slots/` and `targets/slots/`: anything else at the
///   store root is the pre-slot `MBX_CACHE_DIR`, anything else under
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
    let mut class_roots = Vec::new();
    match layout {
        Some(layout) => {
            let legacy = catalog.legacy_mbx_root();
            if legacy.exists() {
                remove_reported(&legacy, &mut report);
            }
            class_roots.extend(scope_dirs(&layout.cache_root).map(|scope| catalog.mbx(&scope)));
        }
        None => {
            let legacy = catalog.legacy_mbx_root();
            class_roots.extend(scope_dirs(&legacy).map(|scope| catalog.mbx(&scope)));
        }
    }
    class_roots.sort();
    class_roots.dedup();
    for class_root in class_roots {
        migrate_class_root(&class_root, &mut report);
    }
    report
}

/// Directory names directly below `root`: the trust scopes that have stores.
fn scope_dirs(root: &Path) -> impl Iterator<Item = String> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
}

fn migrate_class_root(class_root: &Path, report: &mut MigrationReport) {
    let Ok(repositories) = fs::read_dir(class_root) else {
        return;
    };
    for store in repositories.flatten().map(|entry| entry.path()) {
        if !store.is_dir() {
            // A file at the class root was never a store.
            remove_reported(&store, report);
            continue;
        }
        migrate_store(&store, report);
    }
}

/// Prune one repository store to the per-slot layout.
fn migrate_store(store: &Path, report: &mut MigrationReport) {
    for entry in fs::read_dir(store).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == SLOTS_DIR {
            prune_to_slot_directories(&path, report);
        } else if name == TARGETS_DIR {
            for target_entry in fs::read_dir(&path).into_iter().flatten().flatten() {
                let target_path = target_entry.path();
                if target_entry.file_name() == SLOTS_DIR {
                    prune_to_slot_directories(&target_path, report);
                } else {
                    remove_reported(&target_path, report);
                }
            }
        } else {
            remove_reported(&path, report);
        }
    }
}

/// A `slots/` directory holds slot directories and nothing else.
fn prune_to_slot_directories(slots: &Path, report: &mut MigrationReport) {
    if !slots.is_dir() {
        remove_reported(slots, report);
        return;
    }
    for entry in fs::read_dir(slots).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_dir() {
            remove_reported(&path, report);
        }
    }
}

fn remove_reported(path: &Path, report: &mut MigrationReport) {
    let bytes = crate::storage::dir_size(path).unwrap_or(0);
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    match result {
        Ok(()) => report.removed.push((path.to_path_buf(), bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => report
            .failures
            .push((path.to_path_buf(), format!("{error:#}"))),
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

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("velnor-mbx-store-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; bytes]).unwrap();
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
    #[test]
    fn migration_removes_pre_slot_trees_and_keeps_per_slot_trees() {
        let prefix = temp_root("pre-slot");
        let layout = layout(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let store = layout.cache_root.join("trusted/compiler/mbx/1255367013");
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
        let clean = layout.cache_root.join("untrusted/compiler/mbx/829618808");
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

    /// With canonical storage in effect the legacy work-root store is a
    /// second spelling of the store root and goes whole, with its size.
    #[test]
    fn migration_removes_the_legacy_work_root_store_under_canonical_storage() {
        let prefix = temp_root("legacy-root");
        let layout = layout(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let legacy = work_root.join("_velnor_mbx/trusted/1255367013");
        write(&legacy.join("slots/slot-1/a"), 500);
        write(&legacy.join("incremental/b"), 700);
        fs::create_dir_all(&layout.cache_root).unwrap();

        let report = migrate_at_daemon_start(&work_root, Some(&layout));

        assert_eq!(report.removed, vec![(work_root.join("_velnor_mbx"), 1200)]);
        assert!(!work_root.join("_velnor_mbx").exists());
        fs::remove_dir_all(&prefix).ok();
    }

    /// Without canonical storage the legacy root *is* the layout the code
    /// produces; it is pruned in place, not deleted.
    #[test]
    fn migration_prunes_the_legacy_root_in_place_without_canonical_storage() {
        let prefix = temp_root("no-layout");
        let work_root = prefix.join("work");
        let store = work_root.join("_velnor_mbx/trusted/42");
        write(&store.join("slots/slot-1/a"), 500);
        write(&store.join("incremental/b"), 700);

        let report = migrate_at_daemon_start(&work_root, None);

        assert_eq!(report.removed, vec![(store.join("incremental"), 700)]);
        assert!(store.join("slots/slot-1/a").is_file());
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
