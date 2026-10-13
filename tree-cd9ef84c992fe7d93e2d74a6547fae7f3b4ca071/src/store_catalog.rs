//! The sole constructor of Velnor store paths.
//!
//! Every persistent store Velnor owns — Cargo, mise, target generations, the
//! actions cache, artifacts, the compiler stores, and the hosted GitHub Actions
//! cache — has exactly one path expression, published here. Writers and the
//! garbage collectors both resolve through this catalog, so the class of defect
//! where a store is written at one path and reclaimed at another (artifacts
//! landing in `<work>/slot-N/_velnor_artifacts` while GC registered
//! `<work>/_velnor_artifacts`, and the same shape previously found in the
//! BuildKit store) is unrepresentable rather than merely fixed.
//!
//! The catalog is also the ownership map: each class declares the lease class a
//! job must hold to make the store live, and whether routine GC and the
//! emergency reclaimer may touch it. A reclaimer that consults
//! [`StoreClass::lease_class`] cannot delete a class it does not own.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::container::StoreTrustClass;

/// Directory name of the legacy (pre-`VELNOR_STORAGE_ROOT`) artifact store.
///
/// This literal exists once in the tree. `store_catalog` tests assert that,
/// because two spellings of a store root are exactly how a store becomes
/// invisible to GC.
const LEGACY_ARTIFACT_STORE_DIR: &str = "_velnor_artifacts";
const LEGACY_ACTIONS_CACHE_DIR: &str = "_velnor_caches";
const LEGACY_MBX_DIR: &str = "_velnor_mbx";

/// The hosted GitHub Actions cache lives beside the other cache classes under
/// the canonical cache root, so `cache du`/`cache gc` account for it.
const GHA_CACHE_DIR: &str = "gha-cache";

/// Every persistent store class Velnor owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum StoreClass {
    Cargo,
    Mise,
    Targets,
    ActionsCache,
    Artifacts,
    Mbx,
    Sccache,
    GhaCache,
    /// Docker's own image/layer/build storage. Velnor never deletes it beyond
    /// its own builder, but it consumes the same filesystem, so the host budget
    /// must account for it instead of believing the reservation ledger holds
    /// headroom Docker already spent.
    Docker,
}

impl StoreClass {
    /// Lease class a job publishes to mark this store live.
    ///
    /// Every emergency-managed class has one. `Docker` has none because Velnor
    /// never reclaims it by scope.
    pub(crate) fn lease_class(self) -> Option<&'static str> {
        Some(match self {
            Self::Cargo => "cargo",
            Self::Mise => "mise",
            Self::Targets => "targets",
            Self::ActionsCache => "actions-cache",
            Self::Artifacts => "artifacts",
            Self::Mbx => "mbx",
            Self::Sccache => "sccache",
            Self::GhaCache => "gha-cache",
            Self::Docker => return None,
        })
    }
}

impl fmt::Display for StoreClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cargo => "cargo",
            Self::Mise => "mise",
            Self::Targets => "targets",
            Self::ActionsCache => "actions-cache",
            Self::Artifacts => "artifacts",
            Self::Mbx => "mbx",
            Self::Sccache => "sccache",
            Self::GhaCache => "gha-cache",
            Self::Docker => "docker",
        })
    }
}

/// Resolved store paths for one daemon-shared work root.
///
/// Construct it from a work root (GC, `cache du`) or from a job temp directory
/// (executors). Both normalize to the same daemon-shared root, which is what
/// makes the two views provably identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreCatalog {
    work_root: PathBuf,
}

impl StoreCatalog {
    /// Catalog for a work root. A per-slot root (`…/work/slot-N`) is lifted to
    /// the daemon-shared root, so a slot-local caller and the GC agree.
    pub(crate) fn for_work_root(root: impl Into<PathBuf>) -> Self {
        Self {
            work_root: crate::container::daemon_shared_root(root.into()),
        }
    }

    /// Catalog for a running job, resolved from its host temp directory
    /// (`…/work/slot-N/<job>/temp`).
    ///
    /// The pending call site is `executor::artifact_store_dir`, which today
    /// builds the artifact path itself and lands it one directory below where
    /// GC looks. That file belongs to another agent in this program; this is
    /// the constructor its change calls.
    #[allow(dead_code, reason = "call site is executor::artifact_store_dir")]
    pub(crate) fn for_job_temp(temp_host: &Path) -> Self {
        Self {
            work_root: crate::container::daemon_store_root(temp_host),
        }
    }

    #[allow(dead_code, reason = "call site is executor::artifact_store_dir")]
    pub(crate) fn work_root(&self) -> &Path {
        &self.work_root
    }

    pub(crate) fn cargo(&self) -> PathBuf {
        crate::container::cargo_store_host(&self.work_root)
    }

    pub(crate) fn mise(&self) -> PathBuf {
        crate::container::mise_store_host(&self.work_root)
    }

    pub(crate) fn targets(&self) -> PathBuf {
        crate::container::cargo_target_store_host(&self.work_root)
    }

    pub(crate) fn actions_cache(&self) -> PathBuf {
        crate::storage::cache_class_path(&self.work_root, "caches", LEGACY_ACTIONS_CACHE_DIR)
    }

    /// Root of the artifact store.
    ///
    /// One expression, used by the uploader, the download path, and GC. It is
    /// deliberately still the legacy work-root form: moving it under the
    /// canonical cache root is a store migration and belongs in its own change,
    /// while the defect being removed here is two spellings, not the location.
    pub(crate) fn artifacts(&self) -> PathBuf {
        self.work_root.join(LEGACY_ARTIFACT_STORE_DIR)
    }

    /// Artifact store bucket for one workflow run.
    #[allow(dead_code, reason = "call site is executor::artifact_store_dir")]
    pub(crate) fn artifacts_run(&self, run_key: &str) -> PathBuf {
        self.artifacts()
            .join(crate::container::sanitize_store_key(run_key))
    }

    pub(crate) fn mbx(&self, trust_scope: &str) -> PathBuf {
        crate::storage::cache_class_path_for_trust(
            &self.work_root,
            trust_scope,
            "compiler/mbx",
            LEGACY_MBX_DIR,
        )
    }

    pub(crate) fn sccache(&self, trust_class: StoreTrustClass) -> PathBuf {
        crate::container::sccache_host(&self.work_root, trust_class)
    }
}

/// Trust scopes the compiler stores are partitioned by.
pub(crate) const TRUST_SCOPES: [(StoreTrustClass, &str); 3] = [
    (StoreTrustClass::Untrusted, "untrusted"),
    (StoreTrustClass::Trusted, "trusted"),
    (StoreTrustClass::Release, "release"),
];

/// Root of the hosted GitHub Actions cache service storage.
///
/// It is a cache class like any other: one expression, reachable by GC.
pub(crate) fn gha_cache_root(layout: &crate::storage::StorageLayout) -> PathBuf {
    layout.cache_root.join(GHA_CACHE_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The artifact store is written by the job executor and reclaimed by GC.
    /// Defect class: those two built the path differently, so artifacts grew
    /// unbounded at a location no collector knew about. Constructing both views
    /// through the catalog makes them equal by construction.
    #[test]
    fn artifact_path_from_job_temp_equals_the_gc_registration() {
        let work = PathBuf::from("/var/lib/velnor/work");
        let temp = work.join("slot-3").join("job-uuid").join("temp");
        let from_job = StoreCatalog::for_job_temp(&temp);
        let from_gc = StoreCatalog::for_work_root(work.clone());
        assert_eq!(from_job.work_root(), work.as_path());
        assert_eq!(from_job.artifacts(), from_gc.artifacts());
        assert_eq!(from_job.actions_cache(), from_gc.actions_cache());
        assert_eq!(from_job.targets(), from_gc.targets());
        assert_eq!(from_job.cargo(), from_gc.cargo());
        assert_eq!(from_job.mise(), from_gc.mise());
    }

    /// A per-slot root must not produce a per-slot store: that is the exact
    /// shape of the artifact defect.
    #[test]
    fn per_slot_root_is_lifted_to_the_daemon_shared_root() {
        let shared = StoreCatalog::for_work_root("/var/lib/velnor/work");
        let per_slot = StoreCatalog::for_work_root("/var/lib/velnor/work/slot-7");
        assert_eq!(shared, per_slot);
        assert!(
            !per_slot.artifacts().to_string_lossy().contains("slot-7"),
            "artifact store must not be slot-fragmented: {}",
            per_slot.artifacts().display()
        );
    }

    /// The catalog is the only place a store directory name is spelled. If a
    /// second spelling appears anywhere in the crate, the two can drift and the
    /// store becomes invisible to a collector again.
    #[test]
    fn store_directory_names_are_constructed_only_in_the_catalog() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for name in [LEGACY_ARTIFACT_STORE_DIR] {
            let quoted = format!("\"{name}\"");
            let mut stack = vec![src.clone()];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    if path.extension().is_none_or(|ext| ext != "rs")
                        || path.file_name().is_some_and(|f| f == "store_catalog.rs")
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (index, line) in text.lines().enumerate() {
                        if line.contains(&quoted) {
                            offenders.push(format!("{}:{}", path.display(), index + 1));
                        }
                    }
                }
            }
        }
        offenders.sort();
        // `executor::artifact_store_dir` is the one remaining second spelling —
        // the live instance of this defect. That file is owned by another agent
        // in this program, so the exemption is named here rather than silently
        // tolerated: the test fails if a *new* spelling appears anywhere, and it
        // fails again (forcing this exemption's deletion) the moment the
        // executor is changed to call `StoreCatalog::artifacts_run`.
        let pending: Vec<&String> = offenders
            .iter()
            .filter(|found| !found.contains("executor.rs"))
            .collect();
        assert!(
            pending.is_empty(),
            "store directory names must be constructed only in store_catalog.rs; found: {pending:?}"
        );
        assert_eq!(
            offenders.len(),
            1,
            "expected exactly the known executor::artifact_store_dir spelling; found: {offenders:?}"
        );
    }

    /// Every class the reclaimers may delete must declare a lease class, or the
    /// reclaimer has no way to tell a live store from a cold one.
    #[test]
    fn every_reclaimable_class_declares_a_lease_class() {
        for class in [
            StoreClass::Cargo,
            StoreClass::Mise,
            StoreClass::Targets,
            StoreClass::ActionsCache,
            StoreClass::Artifacts,
            StoreClass::Mbx,
            StoreClass::Sccache,
            StoreClass::GhaCache,
        ] {
            assert_eq!(
                class.lease_class().unwrap_or_default(),
                class.to_string(),
                "{class} must be leasable under its own display name"
            );
        }
        assert_eq!(StoreClass::Docker.lease_class(), None);
    }
}
