//! Portable store catalog paths and secure snapshot-root discovery.
//!
//! Filesystem access is supplied by a platform adapter. The adapter may
//! canonicalize only the operator-selected trusted root; it must open every
//! descendant relative to pinned directory handles and reject links/reparse
//! points. Discovery never follows a path supplied by an untrusted store.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub const ARTIFACT_STORE_DIR: &str = "_velnor_artifacts";
pub const GHA_CACHE_DIR: &str = "gha-cache";
pub const STABLE_WORKSPACES_DIR: &str = "stable-workspaces__trust_scope_v1";

const TRUST_SCOPE_NAMESPACE: &str = "trust-scope-v1";
const TRUST_SCOPE_NAMESPACE_SUFFIX: &str = "__trust_scope_v1";
const TRUST_SCOPE_KEY_PREFIX: &str = "trust-scope-v1-";
const DEFAULT_MAX_DISCOVERY_ENTRIES: usize = 100_000;
const DEFAULT_MAX_PINNED_ROOTS: usize = 256;

/// Logical catalog path and the operator-selected path used as its anchor.
///
/// These paths are metadata for display and diagnostics. They may retain
/// lexical aliases and are not canonical or safe to reopen. Secure traversal
/// requires the pinned directory handle in PinnedLocalStorageSnapshotRoot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalStorageSnapshotRoot {
    pub path: PathBuf,
    pub trusted_root: PathBuf,
}

/// A discovered catalog root paired with its still-pinned directory handle.
#[derive(Debug)]
pub struct PinnedLocalStorageSnapshotRoot<A> {
    pub root: LocalStorageSnapshotRoot,
    /// Pinned handle for the discovered store directory. Snapshot readers use
    /// this handle directly, so a retargeted trusted-root alias cannot redirect
    /// a later listing to another tree.
    pub directory: A,
}

/// Hard limits applied while discovering catalog roots.
///
/// max_entries_visited bounds the names inspected across all discovery
/// directory streams, including names that do not match catalog patterns.
/// One additional name may be delivered to the callback to prove the scan
/// exceeded the limit; discovery then fails as incomplete. max_pinned_roots
/// caps retained catalog directory handles. Discovery keeps at most two
/// trusted-root handles and two transient cache-discovery handles in addition
/// to these pinned roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotDiscoveryLimits {
    pub max_entries_visited: usize,
    pub max_pinned_roots: usize,
}

impl Default for SnapshotDiscoveryLimits {
    fn default() -> Self {
        Self {
            max_entries_visited: DEFAULT_MAX_DISCOVERY_ENTRIES,
            max_pinned_roots: DEFAULT_MAX_PINNED_ROOTS,
        }
    }
}

/// Catalog-owned paths needed for a local storage snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCatalogConfig {
    pub work_root: PathBuf,
    pub cache_root: PathBuf,
    pub cache_trusted_root: PathBuf,
}

impl SnapshotCatalogConfig {
    /// Resolve the standalone tool's storage layout using runner precedence:
    /// `VELNOR_STORAGE_ROOT`, then `VELNOR_CONFIG_DIR`, then the user root.
    pub fn from_environment(work_root: &Path) -> Result<Self> {
        Self::from_environment_values(
            work_root,
            std::env::var_os("VELNOR_STORAGE_ROOT"),
            std::env::var_os("VELNOR_CONFIG_DIR"),
            std::env::var_os("HOME"),
            velnor_client::default_user_storage_root,
        )
    }

    fn from_environment_values(
        work_root: &Path,
        storage_root: Option<OsString>,
        config_dir: Option<OsString>,
        home: Option<OsString>,
        default_user_storage_root: impl FnOnce() -> PathBuf,
    ) -> Result<Self> {
        if let Some(prefix) = storage_root.filter(|value| !value.is_empty()) {
            let prefix = PathBuf::from(prefix);
            return Ok(Self::new(work_root, prefix.join("cache/velnor/v1"), prefix));
        }

        if let Some(config_dir) = config_dir.filter(|value| !value.is_empty()) {
            let config_dir = PathBuf::from(config_dir);
            return Ok(Self::new(work_root, config_dir.join("cache"), config_dir));
        }

        if home.is_none_or(|home| home.is_empty()) {
            anyhow::bail!("HOME is not set; set VELNOR_CONFIG_DIR or VELNOR_STORAGE_ROOT");
        }
        let prefix = default_user_storage_root();
        Ok(Self::new(work_root, prefix.join("cache/velnor/v1"), prefix))
    }

    /// Build a snapshot configuration from the already resolved runner layout.
    #[must_use]
    pub fn from_resolved_layout(
        work_root: &Path,
        cache_root: &Path,
        cache_trusted_root: &Path,
    ) -> Self {
        Self::new(
            work_root,
            cache_root.to_path_buf(),
            cache_trusted_root.to_path_buf(),
        )
    }

    fn new(work_root: &Path, cache_root: PathBuf, cache_trusted_root: PathBuf) -> Self {
        Self {
            work_root: daemon_shared_work_root(work_root.to_path_buf()),
            cache_root,
            cache_trusted_root,
        }
    }

    #[must_use]
    pub fn catalog_paths(&self) -> StoreCatalogPaths {
        StoreCatalogPaths {
            work_root: self.work_root.clone(),
            cache_root: self.cache_root.clone(),
        }
    }
}

/// Lexical catalog path expressions shared by the runner catalog and snapshot API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCatalogPaths {
    work_root: PathBuf,
    cache_root: PathBuf,
}

impl StoreCatalogPaths {
    #[must_use]
    pub fn new(work_root: impl Into<PathBuf>, cache_root: impl Into<PathBuf>) -> Self {
        Self {
            work_root: daemon_shared_work_root(work_root.into()),
            cache_root: cache_root.into(),
        }
    }

    #[must_use]
    pub fn work_root(&self) -> &Path {
        &self.work_root
    }

    #[must_use]
    pub fn artifacts(&self) -> PathBuf {
        artifacts_root(&self.work_root)
    }

    #[must_use]
    pub fn gha_cache(&self) -> PathBuf {
        gha_cache_root(&self.cache_root)
    }

    #[must_use]
    pub fn keyed_cache_namespace(&self) -> PathBuf {
        filesystem_key_namespace(&self.cache_root)
    }

    #[must_use]
    pub fn stable_workspace_root(slot_work_dir: &Path) -> PathBuf {
        slot_work_dir.join(STABLE_WORKSPACES_DIR)
    }
}

/// Catalog artifact path for a daemon-shared work directory.
#[must_use]
pub fn artifacts_root(work_root: &Path) -> PathBuf {
    daemon_shared_work_root(work_root.to_path_buf()).join(ARTIFACT_STORE_DIR)
}

/// Catalog hosted Actions cache path for a resolved cache root.
#[must_use]
pub fn gha_cache_root(cache_root: &Path) -> PathBuf {
    cache_root.join(GHA_CACHE_DIR)
}

/// Encode a path as an unambiguous ASCII identity for JSON or Markdown output.
///
/// Forward-slash separators remain visible. Every other byte on Unix or
/// UTF-16 code unit on Windows outside the conservative alphanumeric, slash,
/// dot, and dash set is percent-encoded. This is display metadata only, never
/// a path to reopen.
#[must_use]
pub fn snapshot_path_identity(path: &Path) -> String {
    use std::fmt::Write as _;

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let mut identity = String::with_capacity(path.as_os_str().as_bytes().len());
        for byte in path.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'.' | b'-') {
                identity.push(char::from(*byte));
            } else {
                let _ = write!(identity, "%{byte:02X}");
            }
        }
        identity
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        let mut identity = String::with_capacity(wide.len());
        for unit in wide {
            if (unit <= 0x7f && (unit as u8).is_ascii_alphanumeric())
                || matches!(unit, 0x2f | 0x2e | 0x2d)
            {
                identity.push(char::from_u32(u32::from(unit)).unwrap_or('?'));
            } else {
                let _ = write!(identity, "%u{unit:04X}");
            }
        }
        identity
    }

    #[cfg(not(any(unix, windows)))]
    {
        let bytes = path.as_os_str().as_encoded_bytes();
        let mut identity = String::with_capacity(bytes.len());
        for byte in bytes {
            if byte.is_ascii_alphanumeric() || matches!(*byte, b'/' | b'.' | b'-') {
                identity.push(char::from(*byte));
            } else {
                let _ = write!(identity, "%{byte:02X}");
            }
        }
        identity
    }
}

/// Result of a handle-relative no-follow path inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotPathKind {
    Missing,
    Directory,
    File,
    Link,
    Other,
}

/// Platform boundary for root discovery.
///
/// Implementations must canonicalize only `trusted_root`, then inspect/open
/// every generated path component relative to pinned directory handles. They
/// must reject symlink/reparse components instead of following them. Returning
/// `Missing` is reserved for a genuinely absent path; permissions, links,
/// unexpected types, and I/O errors must fail discovery.
pub trait SnapshotFilesystem {
    type Anchor;

    /// Canonicalize only the selected root and return a pinned no-follow handle.
    fn open_trusted_root(&self, trusted_root: &Path) -> Result<Option<Self::Anchor>>;

    /// Inspect a descendant relative to the pinned trusted-root handle.
    fn path_kind(&self, anchor: &Self::Anchor, relative: &Path) -> Result<SnapshotPathKind>;

    /// Open a catalog directory relative to the pinned trusted-root handle.
    /// The returned handle remains pinned for any later snapshot traversal.
    fn open_directory(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
    ) -> Result<Option<Self::Anchor>>;

    /// Visit names from a descendant directory without buffering the complete
    /// directory. Return `false` from `visit` to stop enumeration early.
    fn visit_directory_entries(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
        visit: &mut dyn FnMut(OsString) -> Result<bool>,
    ) -> Result<Option<()>>;
}

/// Discover existing catalog roots, returning lexical metadata only.
///
/// Returned paths are not canonical and are unsafe to reopen. Use pinned
/// discovery for traversal. Callers supply the secure filesystem adapter;
/// this function never performs path-based filesystem IO.
pub fn discover_local_storage_roots<F: SnapshotFilesystem>(
    config: &SnapshotCatalogConfig,
    filesystem: &F,
) -> Result<Vec<LocalStorageSnapshotRoot>> {
    Ok(discover_pinned_local_storage_roots(config, filesystem)?
        .into_iter()
        .map(|root| root.root)
        .collect())
}

/// Discover catalog roots and retain their opened handles for a later bounded
/// snapshot traversal. The caller must traverse `directory` directly rather
/// than reopening `root.path` through `root.trusted_root`.
pub fn discover_pinned_local_storage_roots<F: SnapshotFilesystem>(
    config: &SnapshotCatalogConfig,
    filesystem: &F,
) -> Result<Vec<PinnedLocalStorageSnapshotRoot<F::Anchor>>> {
    discover_pinned_local_storage_roots_with_limits(
        config,
        filesystem,
        SnapshotDiscoveryLimits::default(),
    )
}

/// Discover roots with explicit work and pinned-handle limits.
///
/// Hitting either limit returns an error stating that discovery is incomplete;
/// callers must not present a partial root set as complete.
pub fn discover_pinned_local_storage_roots_with_limits<F: SnapshotFilesystem>(
    config: &SnapshotCatalogConfig,
    filesystem: &F,
    limits: SnapshotDiscoveryLimits,
) -> Result<Vec<PinnedLocalStorageSnapshotRoot<F::Anchor>>> {
    let paths = config.catalog_paths();
    let mut roots: Vec<PinnedLocalStorageSnapshotRoot<F::Anchor>> = Vec::new();
    let mut budget = DiscoveryBudget::new(limits);

    let cache_anchor = filesystem.open_trusted_root(&config.cache_trusted_root)?;
    let work_anchor = filesystem.open_trusted_root(&config.work_root)?;

    if let Some(work_anchor) = work_anchor.as_ref() {
        let artifacts = paths.artifacts();
        if let Some(directory) = optional_directory(
            filesystem,
            work_anchor,
            &artifacts,
            &config.work_root,
            &mut budget,
            true,
        )? {
            roots.push(PinnedLocalStorageSnapshotRoot {
                root: LocalStorageSnapshotRoot {
                    path: artifacts,
                    trusted_root: config.work_root.clone(),
                },
                directory,
            });
        }

        let stable = StoreCatalogPaths::stable_workspace_root(&config.work_root);
        if let Some(directory) = optional_directory(
            filesystem,
            work_anchor,
            &stable,
            &config.work_root,
            &mut budget,
            true,
        )? {
            roots.push(PinnedLocalStorageSnapshotRoot {
                root: LocalStorageSnapshotRoot {
                    path: stable,
                    trusted_root: config.work_root.clone(),
                },
                directory,
            });
        }

        filesystem
            .visit_directory_entries(work_anchor, Path::new(""), &mut |name| {
                budget.visit_entry()?;
                if !is_numbered_slot_work_dir(&name) {
                    return Ok(true);
                }
                let slot_work = config.work_root.join(&name);
                let slot_relative = relative_path(&slot_work, &config.work_root)?;
                match filesystem.path_kind(work_anchor, &slot_relative)? {
                    SnapshotPathKind::Directory => {
                        let stable = StoreCatalogPaths::stable_workspace_root(&slot_work);
                        if let Some(directory) = optional_directory(
                            filesystem,
                            work_anchor,
                            &stable,
                            &config.work_root,
                            &mut budget,
                            true,
                        )? {
                            roots.push(PinnedLocalStorageSnapshotRoot {
                                root: LocalStorageSnapshotRoot {
                                    path: stable,
                                    trusted_root: config.work_root.clone(),
                                },
                                directory,
                            });
                        }
                    }
                    SnapshotPathKind::File => {}
                    SnapshotPathKind::Link => anyhow::bail!(
                        "numbered slot work root is a symlink or reparse point: {}",
                        snapshot_path_identity(&slot_work)
                    ),
                    SnapshotPathKind::Other => anyhow::bail!(
                        "numbered slot work root has an unsupported filesystem type: {}",
                        snapshot_path_identity(&slot_work)
                    ),
                    SnapshotPathKind::Missing => anyhow::bail!(
                        "numbered slot work root disappeared during secure enumeration: {}",
                        snapshot_path_identity(&slot_work)
                    ),
                }
                Ok(true)
            })?
            .context("work root disappeared during secure discovery")?;
    }

    if let Some(cache_anchor) = cache_anchor.as_ref() {
        let gha_cache = paths.gha_cache();
        if let Some(directory) = optional_directory(
            filesystem,
            cache_anchor,
            &gha_cache,
            &config.cache_trusted_root,
            &mut budget,
            true,
        )? {
            roots.push(PinnedLocalStorageSnapshotRoot {
                root: LocalStorageSnapshotRoot {
                    path: gha_cache,
                    trusted_root: config.cache_trusted_root.clone(),
                },
                directory,
            });
        }

        let keyed_root = paths.keyed_cache_namespace();
        if let Some(keyed_root_directory) = optional_directory(
            filesystem,
            cache_anchor,
            &keyed_root,
            &config.cache_trusted_root,
            &mut budget,
            false,
        )? {
            filesystem
                .visit_directory_entries(&keyed_root_directory, Path::new(""), &mut |key| {
                    budget.visit_entry()?;
                    if !key.to_str().is_some_and(is_filesystem_key) {
                        return Ok(true);
                    }
                    let key_path = keyed_root.join(&key);
                    let key_directory = optional_directory(
                        filesystem,
                        &keyed_root_directory,
                        &key_path,
                        &keyed_root,
                        &mut budget,
                        false,
                    )?
                    .with_context(|| {
                        format!(
                            "keyed cache root disappeared during secure enumeration: {}",
                            snapshot_path_identity(&key_path)
                        )
                    })?;

                    let mut has_store = false;
                    filesystem
                        .visit_directory_entries(&key_directory, Path::new(""), &mut |class| {
                            budget.visit_entry()?;
                            let class_path = key_path.join(&class);
                            if filesystem.path_kind(&key_directory, Path::new(&class))?
                                != SnapshotPathKind::Directory
                            {
                                anyhow::bail!(
                                    "cache class is not a directory: {}",
                                    snapshot_path_identity(&class_path)
                                );
                            }
                            has_store = true;
                            Ok(true)
                        })?
                        .with_context(|| {
                            format!(
                                "keyed cache root disappeared during secure enumeration: {}",
                                snapshot_path_identity(&key_path)
                            )
                        })?;
                    if has_store {
                        budget.reserve_root()?;
                        roots.push(PinnedLocalStorageSnapshotRoot {
                            root: LocalStorageSnapshotRoot {
                                path: key_path,
                                trusted_root: config.cache_trusted_root.clone(),
                            },
                            directory: key_directory,
                        });
                    }
                    Ok(true)
                })?
                .context("keyed cache namespace disappeared during secure enumeration")?;
        }
    }

    roots.sort_by(|left, right| left.root.path.cmp(&right.root.path));
    roots.dedup_by(|left, right| left.root.path == right.root.path);
    Ok(roots)
}

struct DiscoveryBudget {
    limits: SnapshotDiscoveryLimits,
    entries_visited: usize,
    pinned_roots: usize,
}

impl DiscoveryBudget {
    fn new(limits: SnapshotDiscoveryLimits) -> Self {
        Self {
            limits,
            entries_visited: 0,
            pinned_roots: 0,
        }
    }

    fn visit_entry(&mut self) -> Result<()> {
        if self.entries_visited >= self.limits.max_entries_visited {
            anyhow::bail!(
                "snapshot discovery incomplete: exceeded the total entry limit ({})",
                self.limits.max_entries_visited
            );
        }
        self.entries_visited += 1;
        Ok(())
    }

    fn reserve_root(&mut self) -> Result<()> {
        if self.pinned_roots >= self.limits.max_pinned_roots {
            anyhow::bail!(
                "snapshot discovery incomplete: exceeded the pinned root handle limit ({})",
                self.limits.max_pinned_roots
            );
        }
        self.pinned_roots += 1;
        Ok(())
    }
}

fn optional_directory<F: SnapshotFilesystem>(
    filesystem: &F,
    anchor: &F::Anchor,
    path: &Path,
    trusted_root: &Path,
    budget: &mut DiscoveryBudget,
    reserve_root: bool,
) -> Result<Option<F::Anchor>> {
    let relative = relative_path(path, trusted_root)?;
    match filesystem.path_kind(anchor, &relative)? {
        SnapshotPathKind::Directory => {
            if reserve_root {
                budget.reserve_root()?;
            }
            Ok(Some(
                filesystem
                    .open_directory(anchor, &relative)?
                    .with_context(|| {
                        format!(
                            "catalog storage root disappeared during secure open: {}",
                            snapshot_path_identity(path)
                        )
                    })?,
            ))
        }
        SnapshotPathKind::Missing => Ok(None),
        SnapshotPathKind::File => anyhow::bail!(
            "catalog storage root is not a directory: {}",
            snapshot_path_identity(path)
        ),
        SnapshotPathKind::Link => anyhow::bail!(
            "catalog storage root is a symlink or reparse point: {}",
            snapshot_path_identity(path)
        ),
        SnapshotPathKind::Other => anyhow::bail!(
            "catalog storage root has an unsupported filesystem type: {}",
            snapshot_path_identity(path)
        ),
    }
}

fn relative_path(path: &Path, trusted_root: &Path) -> Result<PathBuf> {
    path.strip_prefix(trusted_root)
        .map(Path::to_path_buf)
        .with_context(|| {
            format!(
                "catalog storage root {} is outside its trusted anchor {}",
                snapshot_path_identity(path),
                snapshot_path_identity(trusted_root)
            )
        })
}

/// Convert a per-slot work root (`…/work/slot-N`) to its shared root.
#[must_use]
pub fn daemon_shared_work_root(root: PathBuf) -> PathBuf {
    let is_slot_dir = root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("slot-"))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        });
    if is_slot_dir {
        root.parent().map(Path::to_path_buf).unwrap_or(root)
    } else {
        root
    }
}

/// Sibling root for versioned trust-scope path keys.
#[must_use]
pub fn filesystem_key_namespace(root: &Path) -> PathBuf {
    let Some(name) = root.file_name() else {
        return root.join(format!(
            "{TRUST_SCOPE_NAMESPACE}{TRUST_SCOPE_NAMESPACE_SUFFIX}"
        ));
    };
    let mut namespace = name.to_os_string();
    namespace.push(TRUST_SCOPE_NAMESPACE_SUFFIX);
    root.with_file_name(namespace)
}

/// Validate one complete, versioned trust-scope key component.
#[must_use]
pub fn is_filesystem_key(value: &str) -> bool {
    value
        .strip_prefix(TRUST_SCOPE_KEY_PREFIX)
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn is_numbered_slot_work_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix("slot-"))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking assertions"
)]
mod tests {
    use std::{
        cell::Cell,
        collections::{HashMap, HashSet},
        ffi::OsString,
    };

    use super::*;

    #[derive(Debug)]
    struct MockAnchor(PathBuf);

    #[derive(Default)]
    struct MockFilesystem {
        directories: HashSet<PathBuf>,
        files: HashSet<PathBuf>,
        links: HashSet<PathBuf>,
        entries: HashMap<PathBuf, Vec<OsString>>,
        streamed_names: Cell<usize>,
        opened_directories: Cell<usize>,
    }

    impl MockFilesystem {
        fn add_directory(&mut self, path: PathBuf) {
            self.directories.insert(path.clone());
            if let Some(parent) = path.parent()
                && let Some(name) = path.file_name()
            {
                self.entries
                    .entry(parent.to_path_buf())
                    .or_default()
                    .push(name.to_os_string());
            }
        }

        fn set_entry_names(&mut self, directory: &Path, names: &[&str]) {
            self.entries.insert(
                directory.to_path_buf(),
                names.iter().map(OsString::from).collect(),
            );
        }

        fn config() -> SnapshotCatalogConfig {
            SnapshotCatalogConfig::from_resolved_layout(
                Path::new("/work"),
                Path::new("/cache/cache/velnor/v1"),
                Path::new("/cache"),
            )
        }

        fn with_anchor_directories() -> Self {
            let mut filesystem = Self::default();
            filesystem.add_directory(PathBuf::from("/work"));
            filesystem.add_directory(PathBuf::from("/cache"));
            filesystem
        }
    }

    impl SnapshotFilesystem for MockFilesystem {
        type Anchor = MockAnchor;

        fn open_trusted_root(&self, trusted_root: &Path) -> Result<Option<Self::Anchor>> {
            Ok(Some(MockAnchor(trusted_root.to_path_buf())))
        }

        fn path_kind(&self, anchor: &Self::Anchor, relative: &Path) -> Result<SnapshotPathKind> {
            let path = anchor.0.join(relative);
            Ok(if self.directories.contains(&path) {
                SnapshotPathKind::Directory
            } else if self.files.contains(&path) {
                SnapshotPathKind::File
            } else if self.links.contains(&path) {
                SnapshotPathKind::Link
            } else {
                SnapshotPathKind::Missing
            })
        }

        fn open_directory(
            &self,
            anchor: &Self::Anchor,
            relative: &Path,
        ) -> Result<Option<Self::Anchor>> {
            let path = anchor.0.join(relative);
            self.opened_directories
                .set(self.opened_directories.get() + 1);
            Ok(self.directories.contains(&path).then_some(MockAnchor(path)))
        }

        fn visit_directory_entries(
            &self,
            anchor: &Self::Anchor,
            relative: &Path,
            visit: &mut dyn FnMut(OsString) -> Result<bool>,
        ) -> Result<Option<()>> {
            let path = anchor.0.join(relative);
            if !self.directories.contains(&path) {
                return Ok(None);
            }
            if let Some(names) = self.entries.get(&path) {
                for name in names {
                    self.streamed_names.set(self.streamed_names.get() + 1);
                    if !visit(name.clone())? {
                        break;
                    }
                }
            }
            Ok(Some(()))
        }
    }

    #[test]
    fn work_discovery_stream_stops_at_global_entry_bound_and_reports_incomplete() {
        let mut filesystem = MockFilesystem::with_anchor_directories();
        filesystem.set_entry_names(
            Path::new("/work"),
            &[
                "not-a-slot-1",
                "not-a-slot-2",
                "not-a-slot-3",
                "not-a-slot-4",
            ],
        );

        let error = discover_pinned_local_storage_roots_with_limits(
            &MockFilesystem::config(),
            &filesystem,
            SnapshotDiscoveryLimits {
                max_entries_visited: 2,
                max_pinned_roots: 10,
            },
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("discovery incomplete"));
        assert_eq!(filesystem.streamed_names.get(), 3);
    }

    #[test]
    fn keyed_and_class_discovery_share_the_global_stream_bound() {
        let mut filesystem = MockFilesystem::with_anchor_directories();
        let config = MockFilesystem::config();
        let keyed_root = config.catalog_paths().keyed_cache_namespace();
        let key = format!("{TRUST_SCOPE_KEY_PREFIX}{}", "a".repeat(64));
        let key_path = keyed_root.join(&key);
        filesystem.add_directory(keyed_root.clone());
        filesystem.add_directory(key_path.clone());
        filesystem.add_directory(key_path.join("caches"));
        filesystem.add_directory(key_path.join("git-mirrors"));

        let error = discover_pinned_local_storage_roots_with_limits(
            &config,
            &filesystem,
            SnapshotDiscoveryLimits {
                max_entries_visited: 2,
                max_pinned_roots: 10,
            },
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("discovery incomplete"));
        assert_eq!(filesystem.streamed_names.get(), 3);
    }

    #[test]
    fn pinned_root_handle_limit_fails_before_opening_an_extra_store() {
        let mut filesystem = MockFilesystem::with_anchor_directories();
        filesystem.add_directory(PathBuf::from("/work/_velnor_artifacts"));
        filesystem.add_directory(PathBuf::from("/work/stable-workspaces__trust_scope_v1"));

        let error = discover_pinned_local_storage_roots_with_limits(
            &MockFilesystem::config(),
            &filesystem,
            SnapshotDiscoveryLimits {
                max_entries_visited: 10,
                max_pinned_roots: 1,
            },
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("pinned root handle limit"));
        assert_eq!(filesystem.opened_directories.get(), 1);
    }

    #[test]
    fn environment_layout_precedence_is_testable_without_mutating_process_environment() {
        let work = Path::new("/work/slot-3");
        let default_calls = Cell::new(0);
        let storage = SnapshotCatalogConfig::from_environment_values(
            work,
            Some(OsString::from("/storage")),
            Some(OsString::from("/config")),
            None,
            || {
                default_calls.set(default_calls.get() + 1);
                PathBuf::from("/wrong-default")
            },
        )
        .unwrap();
        assert_eq!(default_calls.get(), 0);
        assert_eq!(storage.work_root, PathBuf::from("/work"));
        assert_eq!(
            storage.cache_root,
            PathBuf::from("/storage/cache/velnor/v1")
        );
        assert_eq!(storage.cache_trusted_root, PathBuf::from("/storage"));

        let configured = SnapshotCatalogConfig::from_environment_values(
            work,
            None,
            Some(OsString::from("/config")),
            Some(OsString::from("/home")),
            || {
                default_calls.set(default_calls.get() + 1);
                PathBuf::from("/wrong-default")
            },
        )
        .unwrap();
        assert_eq!(default_calls.get(), 0);
        assert_eq!(configured.cache_root, PathBuf::from("/config/cache"));
        assert_eq!(configured.cache_trusted_root, PathBuf::from("/config"));

        let user = SnapshotCatalogConfig::from_environment_values(
            work,
            None,
            None,
            Some(OsString::from("/home")),
            || PathBuf::from("/user-default"),
        )
        .unwrap();
        assert_eq!(
            user.cache_root,
            PathBuf::from("/user-default/cache/velnor/v1")
        );

        assert!(
            SnapshotCatalogConfig::from_environment_values(work, None, None, None, || {
                PathBuf::from("/unused")
            })
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_identity_distinguishes_non_utf8_names_that_display_lossily() {
        use std::os::unix::ffi::OsStringExt as _;

        let first = PathBuf::from(OsString::from_vec(b"/store/name-\xff".to_vec()));
        let second = PathBuf::from(OsString::from_vec(b"/store/name-\xfe".to_vec()));

        assert_eq!(first.display().to_string(), second.display().to_string());
        let first_identity = snapshot_path_identity(&first);
        let second_identity = snapshot_path_identity(&second);
        assert_ne!(first_identity, second_identity);
        assert!(first_identity.ends_with("%FF"));
        assert!(second_identity.ends_with("%FE"));
    }
}
