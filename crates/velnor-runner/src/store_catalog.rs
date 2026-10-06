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
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Directory name of the daemon-shared artifact store.
///
/// This literal exists once in the tree. `store_catalog` tests assert that,
/// because two spellings of a store root are exactly how a store becomes
/// invisible to GC.
const ARTIFACT_STORE_DIR: &str = velnor_storage_snapshot::ARTIFACT_STORE_DIR;
const LEGACY_MBX_DIR: &str = "_velnor_mbx";
const MBX_CLASS: &str = "compiler/mbx";
const GIT_MIRRORS_CLASS: &str = "git-mirrors";

/// The hosted GitHub Actions cache lives beside the other cache classes under
/// the canonical cache root, so `cache du`/`cache gc` account for it.
/// Stable, filesystem-safe key for a repository's persistent caches.
/// GitHub's numeric repository ID survives rename and casing changes; the
/// canonical server origin keeps equal numeric IDs on different GHES hosts
/// disjoint. Invalid identity never receives a persistent namespace.
pub(crate) fn repository_store_key(server_url: &str, repository_id: &str) -> Option<String> {
    let repository_id = repository_id.trim();
    if repository_id.is_empty() || !repository_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let repository_id = repository_id.parse::<u64>().ok().filter(|id| *id > 0)?;

    let server = url::Url::parse(server_url.trim()).ok()?;
    if !matches!(server.scheme(), "http" | "https")
        || server.host_str().is_none()
        || !server.username().is_empty()
        || server.password().is_some()
        || server.query().is_some()
        || server.fragment().is_some()
        || server.path() != "/"
    {
        return None;
    }
    let origin = server.origin().ascii_serialization().to_ascii_lowercase();

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"velnor-repository-store-key-v1\0");
    hasher.update(origin.as_bytes());
    hasher.update(b"\0");
    hasher.update(repository_id.to_string().as_bytes());
    Some(format!("repo-key-v1-{}", hasher.finalize().to_hex()))
}

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
    GitMirrors,
    StableWorkspace,
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
            Self::GitMirrors => "git-mirrors",
            Self::StableWorkspace => "stable-workspace",
            Self::Docker => return None,
        })
    }

    /// The compiler-acceleration stores (mbx and explicit sccache) share
    /// one host-level budget, [`crate::capacity::StoreBudgetPolicy`].
    pub(crate) fn is_compiler(self) -> bool {
        matches!(self, Self::Mbx | Self::Sccache)
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
            Self::GitMirrors => "git-mirrors",
            Self::StableWorkspace => "stable-workspace",
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
    layout: crate::storage::StorageLayout,
}

impl StoreCatalog {
    /// Catalog for a work root. A per-slot root (`…/work/slot-N`) is lifted to
    /// the daemon-shared root, so a slot-local caller and the GC agree.
    /// Catalog for a work root with its concrete storage-layout snapshot.
    ///
    /// Reclaim callers that already resolved configuration pass the same
    /// snapshot through every path constructor. This keeps the catalog
    /// deterministic when process configuration is changed by a test or
    /// embedding process.
    pub(crate) fn for_work_root_with_layout(
        root: impl Into<PathBuf>,
        layout: &crate::storage::StorageLayout,
    ) -> Self {
        Self {
            work_root: crate::container::daemon_shared_root(root.into()),
            layout: layout.clone(),
        }
    }

    /// Catalog for a running job, resolved from its host temp directory
    /// (`…/work/slot-N/<job>/temp`).
    ///
    /// Called by `executor::artifact_store_dir`, which previously built the
    /// artifact path itself with a different root helper and landed it one
    /// directory below where GC looks.
    pub(crate) fn for_job_temp(temp_host: &Path, layout: &crate::storage::StorageLayout) -> Self {
        Self {
            work_root: crate::container::daemon_store_root(temp_host),
            layout: layout.clone(),
        }
    }

    #[allow(
        dead_code,
        reason = "call sites are executor and cache catalog enumeration"
    )]
    pub(crate) fn work_root(&self) -> &Path {
        &self.work_root
    }

    /// Root of the cargo store class for one trust scope: the pool scope or
    /// the untrusted floor on the GC path.
    pub(crate) fn cargo(&self, trust_scope: &str) -> PathBuf {
        self.layout.cache_class(trust_scope, "cargo")
    }

    /// Root of the mise store class for one trust scope: the pool scope or
    /// the untrusted floor on the GC path.
    pub(crate) fn mise(&self, trust_scope: &str) -> PathBuf {
        self.layout.cache_class(trust_scope, "mise")
    }

    /// Root of the persistent-target store class for one trust scope: the
    /// pool scope or the untrusted floor on the GC path.
    pub(crate) fn targets(&self, trust_scope: &str) -> PathBuf {
        self.layout.cache_class(trust_scope, "targets")
    }

    /// Root of the actions-cache store class for one trust scope: the pool
    /// scope or the untrusted floor on the GC path.
    pub(crate) fn actions_cache(&self, trust_scope: &str) -> PathBuf {
        self.layout.cache_class(trust_scope, "caches")
    }

    /// Root of the artifact store.
    ///
    /// One expression, used by the uploader, the download path, and GC. It
    /// stays shared at the daemon work root across trust scopes and slots.
    pub(crate) fn artifacts(&self) -> PathBuf {
        velnor_storage_snapshot::StoreCatalogPaths::new(
            self.work_root.clone(),
            self.layout.cache_root.clone(),
        )
        .artifacts()
    }

    /// Historical artifacts written beneath a slot-local work root before
    /// artifact writers and GC shared this catalog. Stale-scope cleanup keeps
    /// these misplaced roots intact while it removes legacy cache data.
    pub(crate) fn artifacts_in_slot_work_root(slot_work_root: &Path) -> PathBuf {
        slot_work_root.join(ARTIFACT_STORE_DIR)
    }

    /// Artifact store bucket for one workflow run.
    pub(crate) fn artifacts_run(&self, run_key: &str) -> PathBuf {
        self.artifacts()
            .join(crate::container::sanitize_store_key(run_key))
    }

    /// Root of the mbx compiler store for one trust scope: the pool scope
    /// or the untrusted floor on the GC path, the job's admitted scope on
    /// the execution path. Below it, one store per repository id, laid out
    /// per slot by [`crate::mbx_store`].
    pub(crate) fn mbx(&self, trust_scope: &str) -> PathBuf {
        self.layout.cache_class(trust_scope, MBX_CLASS)
    }

    /// MBX class root for a key read from the filesystem. The value is already
    /// encoded by `trust_scope::filesystem_key`; it cannot be decoded into the
    /// original scope and must not pass through [`Self::mbx`] again.
    pub(crate) fn mbx_from_filesystem_key(&self, filesystem_key: &str) -> PathBuf {
        debug_assert!(crate::trust_scope::is_filesystem_key(filesystem_key));
        crate::trust_scope::filesystem_key_namespace(&self.layout.cache_root)
            .join(filesystem_key)
            .join(MBX_CLASS)
    }

    /// The versioned legacy mbx sibling root under the work root, with keyed
    /// trust namespaces below it.
    pub(crate) fn legacy_mbx_root(&self) -> PathBuf {
        crate::storage::legacy_store_root(&self.work_root, LEGACY_MBX_DIR)
    }

    /// The old, unversioned mbx root. Canonical startup migration deletes this
    /// historical store family; no writer or reader resolves paths below it.
    pub(crate) fn old_legacy_mbx_root(&self) -> PathBuf {
        self.work_root.join(LEGACY_MBX_DIR)
    }

    /// Root of the sccache compiler store for one trust scope: the pool
    /// scope or the untrusted floor on the GC path, the job's admitted scope
    /// on the execution path. Namespaced by the scope in both layouts.
    pub(crate) fn sccache(&self, trust_scope: &str) -> PathBuf {
        self.layout.cache_class(trust_scope, "compiler/sccache")
    }

    /// Trust-partitioned root for persistent Git mirrors. Git mirror writers
    /// and GC use this exact class path; repository identity is one child
    /// directory below it.
    pub(crate) fn git_mirrors_root(
        layout: &crate::storage::StorageLayout,
        trust_scope: &str,
    ) -> PathBuf {
        layout.cache_class(
            crate::trust_scope::normalize_scope(trust_scope),
            GIT_MIRRORS_CLASS,
        )
    }

    /// Root of a Git mirror class for this catalog's storage layout.
    pub(crate) fn git_mirrors(&self, trust_scope: &str) -> PathBuf {
        Self::git_mirrors_root(&self.layout, trust_scope)
    }

    /// Root of one repository's persistent Git mirror.
    pub(crate) fn git_mirror_repository_root(
        layout: &crate::storage::StorageLayout,
        trust_scope: &str,
        repository_key: &str,
    ) -> PathBuf {
        Self::git_mirrors_root(layout, trust_scope).join(repository_key)
    }

    /// Slot-local stable-workspace root. Unlike daemon-shared stores this must
    /// stay under the exact slot work directory supplied by the caller.
    pub(crate) fn stable_workspace_root(slot_work_dir: &Path) -> PathBuf {
        velnor_storage_snapshot::StoreCatalogPaths::stable_workspace_root(slot_work_dir)
    }

    /// Existing catalog path metadata for bounded local diagnostics.
    ///
    /// Cache classes are grouped by the collision-resistant trust-scope key;
    /// artifacts and the hosted cache keep their catalog-owned roots. Legacy
    /// work-root cache names are intentionally never consulted. Returned paths
    /// are lexical metadata and must not be reopened for traversal.
    pub(crate) fn local_diagnostic_roots(&self) -> Result<Vec<crate::LocalStorageSnapshotRoot>> {
        let cache_anchor = self
            .layout
            .local_snapshot_trusted_root()
            .context("cannot resolve configured storage root for diagnostics")?;
        let config = velnor_storage_snapshot::SnapshotCatalogConfig::from_resolved_layout(
            &self.work_root,
            &self.layout.cache_root,
            &cache_anchor,
        );
        velnor_storage_snapshot::discover_local_storage_roots(&config, &RunnerSnapshotFilesystem)
    }
}

struct RunnerSnapshotFilesystem;

impl velnor_storage_snapshot::SnapshotFilesystem for RunnerSnapshotFilesystem {
    type Anchor = crate::fs_copy::NoFollowDir;

    fn open_trusted_root(&self, path: &Path) -> Result<Option<Self::Anchor>> {
        open_optional_trusted_root(path)
    }

    fn path_kind(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
    ) -> Result<velnor_storage_snapshot::SnapshotPathKind> {
        use velnor_storage_snapshot::SnapshotPathKind;

        match anchor.open_source(relative)? {
            None => Ok(SnapshotPathKind::Missing),
            Some(crate::fs_copy::NoFollowSource::Directory(_)) => Ok(SnapshotPathKind::Directory),
            Some(crate::fs_copy::NoFollowSource::File(_)) => Ok(SnapshotPathKind::File),
        }
    }

    fn open_directory(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
    ) -> Result<Option<Self::Anchor>> {
        let Some(source) = anchor.open_source(relative)? else {
            return Ok(None);
        };
        let crate::fs_copy::NoFollowSource::Directory(directory) = source else {
            anyhow::bail!("snapshot path is not a directory: {}", relative.display());
        };
        Ok(Some(directory))
    }

    fn visit_directory_entries(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
        visit: &mut dyn FnMut(std::ffi::OsString) -> Result<bool>,
    ) -> Result<Option<()>> {
        let Some(source) = anchor.open_source(relative)? else {
            return Ok(None);
        };
        let crate::fs_copy::NoFollowSource::Directory(directory) = source else {
            anyhow::bail!("snapshot path is not a directory: {}", relative.display());
        };
        directory.visit_entry_names_until(|name| visit(name))?;
        Ok(Some(()))
    }
}

fn open_optional_trusted_root(path: &Path) -> Result<Option<crate::fs_copy::NoFollowDir>> {
    // Only the operator-selected anchor may resolve a configured alias. The
    // canonical path is then opened component by component without following
    // links, and every generated descendant stays descriptor-relative.
    let canonical = match fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "canonicalize configured diagnostic anchor {}",
                    path.display()
                )
            });
        }
    };
    crate::fs_copy::NoFollowDir::open_absolute(&canonical)
        .with_context(|| {
            format!(
                "securely open configured diagnostic anchor without following descendants {}",
                canonical.display()
            )
        })
        .map(Some)
}

#[cfg(test)]
fn open_optional_directory(
    anchor: &crate::fs_copy::NoFollowDir,
    path: &Path,
    trusted_root: &Path,
) -> Result<Option<crate::fs_copy::NoFollowDir>> {
    let relative = path.strip_prefix(trusted_root).with_context(|| {
        format!(
            "catalog storage root {} is outside its trusted anchor {}",
            path.display(),
            trusted_root.display()
        )
    })?;

    match anchor.open_source(relative)? {
        None => Ok(None),
        Some(crate::fs_copy::NoFollowSource::Directory(directory)) => Ok(Some(directory)),
        Some(crate::fs_copy::NoFollowSource::File(_)) => {
            anyhow::bail!(
                "canonical storage root is not a directory: {}",
                path.display()
            )
        }
    }
}

/// Root of the hosted GitHub Actions cache service storage.
///
/// It is a cache class like any other: one expression, reachable by GC.
pub(crate) fn gha_cache_root(layout: &crate::storage::StorageLayout) -> PathBuf {
    velnor_storage_snapshot::gha_cache_root(&layout.cache_root)
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
    fn repository_store_key_is_stable_case_safe_and_ghes_origin_scoped() {
        // Numeric identity survives display-slug rename/case changes; server
        // origin separates equal repository IDs on GitHub and GHES.
        let original = repository_store_key("https://github.com", "42").unwrap();
        let renamed = repository_store_key("https://GITHUB.com:443/", "00042").unwrap();
        let enterprise = repository_store_key("https://github.enterprise.example", "42").unwrap();

        assert_eq!(original, renamed);
        assert_ne!(original, enterprise);
        assert!(original.starts_with("repo-key-v1-"));
    }

    #[test]
    fn repository_store_key_refuses_untrusted_or_invalid_identity() {
        for repository_id in ["", "0", "-1", "42x", "18446744073709551616"] {
            assert!(
                repository_store_key("https://github.com", repository_id).is_none(),
                "accepted invalid repository id {repository_id:?}"
            );
        }
        for server_url in [
            "",
            "file:///tmp/repository",
            "https://user@github.com",
            "https://github.com/owner/repo",
            "https://github.com?redirect=elsewhere",
            "https://github.com/#fragment",
        ] {
            assert!(
                repository_store_key(server_url, "42").is_none(),
                "accepted invalid GitHub origin {server_url:?}"
            );
        }
    }

    /// The artifact store is written by the job executor and reclaimed by GC.
    /// Defect class: those two built the path differently, so artifacts grew
    /// unbounded at a location no collector knew about. Constructing both views
    /// through the catalog makes them equal by construction.
    #[test]
    fn artifact_path_from_job_temp_equals_the_gc_registration() {
        let work = PathBuf::from("/var/lib/velnor/work");
        let temp = work.join("slot-3").join("job-uuid").join("temp");
        let layout = crate::storage::StorageLayout::from_prefix(Path::new("/var"));
        let from_job = StoreCatalog::for_job_temp(&temp, &layout);
        let from_gc = StoreCatalog::for_work_root_with_layout(work.clone(), &layout);
        assert_eq!(from_job.work_root(), work.as_path());
        assert_eq!(from_job.artifacts(), from_gc.artifacts());
        for scope in ["trusted", crate::trust_scope::FAIL_CLOSED] {
            assert_eq!(from_job.actions_cache(scope), from_gc.actions_cache(scope));
            assert_eq!(from_job.targets(scope), from_gc.targets(scope));
            assert_eq!(from_job.cargo(scope), from_gc.cargo(scope));
            assert_eq!(from_job.mise(scope), from_gc.mise(scope));
            assert_eq!(
                StoreCatalog::git_mirrors_root(&from_job.layout, scope),
                StoreCatalog::git_mirrors_root(&from_gc.layout, scope)
            );
        }
    }

    #[test]
    fn git_mirror_catalog_roots_match_the_writer_layout_and_stay_trust_partitioned() {
        let layout = crate::storage::StorageLayout::from_prefix(Path::new("/var"));
        let repository = repository_store_key("https://github.com", "42").unwrap();
        let pool = StoreCatalog::git_mirrors_root(&layout, crate::trust_scope::TRUSTED);
        let floor = StoreCatalog::git_mirrors_root(&layout, crate::trust_scope::FAIL_CLOSED);
        let repository_root = StoreCatalog::git_mirror_repository_root(
            &layout,
            crate::trust_scope::TRUSTED,
            &repository,
        );

        assert_eq!(
            pool,
            layout.cache_class(crate::trust_scope::TRUSTED, GIT_MIRRORS_CLASS)
        );
        assert_eq!(repository_root, pool.join(repository));
        assert_ne!(pool, floor);
    }

    /// A per-slot root must not produce a per-slot store: that is the exact
    /// shape of the artifact defect.
    #[test]
    fn per_slot_root_is_lifted_to_the_daemon_shared_root() {
        let layout = crate::storage::StorageLayout::from_prefix(Path::new("/var"));
        let shared = StoreCatalog::for_work_root_with_layout("/var/lib/velnor/work", &layout);
        let per_slot =
            StoreCatalog::for_work_root_with_layout("/var/lib/velnor/work/slot-7", &layout);
        assert_eq!(shared, per_slot);
        assert!(
            !per_slot.artifacts().to_string_lossy().contains("slot-7"),
            "artifact store must not be slot-fragmented: {}",
            per_slot.artifacts().display()
        );
    }

    #[test]
    fn local_diagnostic_roots_include_catalog_stores_and_ignore_legacy_work_paths() {
        let root = std::env::temp_dir().join(format!(
            "velnor-catalog-diagnostics-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let catalog = StoreCatalog::for_work_root_with_layout(&work, &layout);
        let cargo = catalog.cargo("trusted");
        let sccache = catalog.sccache("pr");
        let actions_cache = catalog.actions_cache("untrusted");
        let git_mirrors = catalog.git_mirrors("trusted");
        let artifacts = catalog.artifacts();
        let gha_cache = gha_cache_root(&layout);
        let stable_root = StoreCatalog::stable_workspace_root(&work);
        let slot_work = work.join("slot-3");
        let slot_stable_root = StoreCatalog::stable_workspace_root(&slot_work);
        let unrelated_stable_root =
            StoreCatalog::stable_workspace_root(&work.join("other-slot-name"));

        for path in [
            &cargo,
            &sccache,
            &actions_cache,
            &git_mirrors,
            &artifacts,
            &gha_cache,
            &stable_root,
            &slot_stable_root,
            &unrelated_stable_root,
            &work.join("_velnor_caches"),
            &work.join("_velnor_sccache"),
        ] {
            fs::create_dir_all(path).unwrap();
        }

        let roots = catalog.local_diagnostic_roots().unwrap();
        let trusted_key_root = cargo.parent().unwrap();
        let pr_key_root = sccache.parent().unwrap().parent().unwrap();
        let untrusted_key_root = actions_cache.parent().unwrap();
        let cache_anchor = layout.local_snapshot_trusted_root().unwrap();
        let contains_path = |path: &Path| roots.iter().any(|root| root.path == path);
        assert!(contains_path(trusted_key_root));
        assert!(contains_path(pr_key_root));
        assert!(contains_path(untrusted_key_root));
        assert!(contains_path(git_mirrors.parent().unwrap()));
        assert!(contains_path(&artifacts));
        assert!(contains_path(&gha_cache));
        assert!(contains_path(&stable_root));
        assert!(contains_path(&slot_stable_root));
        assert!(!contains_path(&unrelated_stable_root));
        assert!(!contains_path(&work.join("_velnor_caches")));
        assert!(!contains_path(&work.join("_velnor_sccache")));
        assert!(roots
            .iter()
            .filter(|root| root.path == gha_cache || root.path == trusted_key_root)
            .all(|root| root.trusted_root == cache_anchor));
        assert!(roots
            .iter()
            .filter(|root| root.path == artifacts || root.path == stable_root)
            .all(|root| root.trusted_root == work));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_diagnostic_roots_reject_symlinked_store_roots() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "velnor-catalog-diagnostic-symlinks-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let catalog = StoreCatalog::for_work_root_with_layout(&work, &layout);
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();

        for path in [
            catalog.artifacts(),
            gha_cache_root(&layout),
            crate::trust_scope::filesystem_key_namespace(&layout.cache_root),
            StoreCatalog::stable_workspace_root(&work),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            symlink(&outside, &path).unwrap();

            let error = catalog.local_diagnostic_roots().unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("symlink"), "unexpected error: {message}");
            assert!(message.contains(&path.display().to_string()));

            fs::remove_file(path).unwrap();
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn diagnostic_descendant_open_stays_on_the_pinned_anchor_after_symlink_swap() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "velnor-catalog-diagnostic-anchor-swap-{}",
            uuid::Uuid::new_v4()
        ));
        let trusted_root = root.join("selected");
        let store = trusted_root.join("cache/store");
        let moved_root = root.join("selected-moved");
        let outside = root.join("outside");
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("inside"), "selected root").unwrap();
        fs::create_dir_all(outside.join("cache/store")).unwrap();
        fs::write(outside.join("cache/store/canary"), "outside root").unwrap();

        let anchor = open_optional_trusted_root(&trusted_root).unwrap().unwrap();
        fs::rename(&trusted_root, &moved_root).unwrap();
        symlink(&outside, &trusted_root).unwrap();

        let opened = open_optional_directory(&anchor, &store, &trusted_root)
            .unwrap()
            .unwrap();
        let mut entries = Vec::new();
        opened
            .for_each_entry_name(|name| {
                entries.push(name.to_string_lossy().into_owned());
                Ok(())
            })
            .unwrap();

        assert!(entries.iter().any(|name| name == "inside"));
        assert!(!entries.iter().any(|name| name == "canary"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_diagnostic_roots_propagate_namespace_key_and_class_errors() {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "velnor-catalog-diagnostic-errors-{}",
                uuid::Uuid::new_v4()
            ));
        fs::create_dir(&root).unwrap();
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let catalog = StoreCatalog::for_work_root_with_layout(root.join("work"), &layout);
        let keyed_root = crate::trust_scope::filesystem_key_namespace(&layout.cache_root);
        fs::create_dir_all(keyed_root.parent().unwrap()).unwrap();

        fs::write(&keyed_root, "not a directory").unwrap();
        let error = catalog.local_diagnostic_roots().unwrap_err();
        assert!(
            format!("{error:#}").contains(&velnor_storage_snapshot::snapshot_path_identity(
                &keyed_root
            ))
        );

        fs::remove_file(&keyed_root).unwrap();
        fs::create_dir_all(&keyed_root).unwrap();
        let key_path = keyed_root.join(crate::trust_scope::filesystem_key("trusted"));
        fs::write(&key_path, "not a directory").unwrap();
        let error = catalog.local_diagnostic_roots().unwrap_err();
        assert!(format!("{error:#}").contains("catalog storage root is not a directory"));
        assert!(format!("{error:#}")
            .contains(&velnor_storage_snapshot::snapshot_path_identity(&key_path)));

        fs::remove_file(&key_path).unwrap();
        fs::create_dir_all(&key_path).unwrap();
        let class_path = catalog.cargo("trusted");
        fs::write(&class_path, "not a directory").unwrap();
        let error = catalog.local_diagnostic_roots().unwrap_err();
        assert!(format!("{error:#}").contains("cache class is not a directory"));
        assert!(
            format!("{error:#}").contains(&velnor_storage_snapshot::snapshot_path_identity(
                &class_path
            ))
        );

        fs::remove_dir_all(root).unwrap();
    }

    /// The catalog is the only place a store directory name is spelled. If a
    /// second spelling appears anywhere in the crate, the two can drift and the
    /// store becomes invisible to a collector again.
    #[test]
    fn store_directory_names_are_constructed_only_in_the_catalog() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for name in [ARTIFACT_STORE_DIR] {
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
        // There is no exemption. `executor::artifact_store_dir` was the last
        // second spelling and now calls `StoreCatalog::artifacts_run`, so a
        // store directory name appearing outside this module is a regression:
        // two spellings are what let the artifact store drift one directory
        // below where GC looked.
        assert!(
            offenders.is_empty(),
            "store directory names must be constructed only in store_catalog.rs; found: {offenders:?}"
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
            StoreClass::GitMirrors,
            StoreClass::StableWorkspace,
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
