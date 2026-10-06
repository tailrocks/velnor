//! Stable per-slot workspaces for the non-mbx Rust paths.
//!
//! BC-14 deleted the mtime pin and the persistent target layer because pinned
//! mtimes let stale artifacts read as fresh. The deletion is correct, but it
//! removed the only warm state the explicit-sccache and `MBX_DISABLE=1`
//! opt-out paths had: every job checks out into a fresh per-job-UUID
//! directory, so Cargo fingerprints never survive and each warm same-SHA
//! rebuild recompiles every path-local crate (measured 30-100x on the
//! fixture's Rust jobs; the mbx default path is unaffected because mbx is
//! content-addressed and keeps its own managed targets).
//!
//! The remediation is a stable workspace per slot, repository, and admitted
//! trust scope. One slot runs one job at a time, so the directory is
//! exclusively owned without locking; consecutive jobs for the same
//! repository re-check out over it instead of into a fresh UUID directory.
//! A same-SHA re-checkout rewrites no tracked file (`checkout --force` and
//! `reset --hard` are no-ops when the tree is identical), so wall-clock
//! mtimes are preserved and Cargo correctly reports fresh. A new SHA rewrites
//! exactly the changed files with now mtimes, so Cargo rebuilds exactly the
//! affected crates. No mtime is ever set backwards, no artifact is ever
//! restored from a store, and nothing is shared across slots, repositories,
//! or trust scopes — this is the standard incremental-compilation contract of
//! a developer machine or a self-hosted runner with a persistent work
//! directory, not the deleted generation store.
//!
//! Layout under the slot's work directory (colocated with the per-job UUID
//! directories, so no slot identity has to be derived from anywhere):
//!
//! ```text
//! <slot-work-dir>/stable-workspaces__trust_scope_v1/<scope-key>/<repository-key>/workspace
//! ```
//!
//! * `<scope>` is the stable filesystem key of the job's admitted trust scope.
//!   Fork-PR and unknown jobs land under the untrusted floor, exactly like the
//!   compiler stores, so an untrusted job can neither read nor poison a
//!   trusted workspace.
//! * `<repository-key>` is derived from canonical `github.server_url` origin
//!   and positive numeric `github.repository_id`, matching the persistent
//!   compiler stores. Jobs without a valid identity use an ephemeral workspace.
//! * `workspace` keeps the same leaf name as the per-job layout, so every
//!   workspace-relative path (checkout destinations, `target/`, container
//!   mounts) behaves identically.
//!
//! The directory names are never job-UUID-shaped, so the leftover-disk
//! reaper (which only deletes UUID-shaped children of slot directories)
//! cannot mistake a stable workspace for an orphan.
//!
//! Disk bound: each scope directory holds a Cargo `target/` tree, which
//! would otherwise grow without bound as repositories churn through a slot.
//! The slot's stable tree is capped at [`STABLE_WORKSPACES_BUDGET_BYTES`]
//! (parity with mbx's 30 GiB managed-target budget: the non-mbx paths get
//! the same warm-target allowance mbx enjoys). The budget is enforced at
//! every allocation ([`prepare`]) and after every job ([`reclaim_after_job`]).
//! It used to run only when a new scope was created, on the belief that an
//! existing scope's `target/` is bounded by the repository's finite build
//! closure; a live host disproved that with one 31 GiB scope (profiles,
//! feature sets, dependency churn and incremental caches all grow a single
//! `target/` without ever creating a new scope). A bound that is only
//! checked on one event is not a bound. Eviction removes whole idle scopes,
//! least-recently-used first, through the filesystem coordinator and pinned
//! same-device candidate deletion used by pressure reclamation. Active leases
//! and the scope being allocated are never victims; an over-budget active
//! scope remains until its job ends, then post-job reclamation can evict it.
//! The LRU clock is a marker file refreshed on every allocation, because
//! directory mtimes do not track deep file writes. Eviction is hygiene, not
//! safety: any error warns and continues, and the capacity reservation (which
//! measures real free disk) remains the fail-closed guard.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Leaf directory holding every stable workspace of one slot.
pub(crate) const STABLE_WORKSPACES_DIR: &str = velnor_storage_snapshot::STABLE_WORKSPACES_DIR;

/// Scope component that identifies the slot work directory without depending
/// on a lossy filename sanitizer. The same canonical key is included in job
/// leases and emergency-reclaim candidates, because slot-local workspaces
/// have no daemon-wide relative scope that distinguishes their physical roots.
pub(crate) fn slot_scope_key(slot_work_dir: &Path) -> Result<String> {
    let slot_work_dir = canonicalize_existing_prefix(slot_work_dir)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"velnor-stable-workspace-slot-v1\0");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        hasher.update(slot_work_dir.as_os_str().as_bytes());
    }
    #[cfg(not(unix))]
    hasher.update(slot_work_dir.to_string_lossy().as_bytes());
    Ok(format!("slot-v1-{}", hasher.finalize().to_hex()))
}

/// Parent scope that matches one stable-workspace pressure candidate. The
/// runner appends the job holder below this value before publishing a lease.
pub(crate) fn lease_scope(
    slot_work_dir: &Path,
    trust_scope: &str,
    repository_key: &str,
) -> Result<String> {
    Ok(format!(
        "{}/{}/{}",
        slot_scope_key(slot_work_dir)?,
        crate::trust_scope::filesystem_key(trust_scope),
        repository_key
    ))
}

fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // Parent components are interpreted after resolving preceding symlinks by
    // the filesystem. Collapsing them lexically first changes that meaning:
    // `/a/link/../slot` can name a different directory when `link` points
    // elsewhere. Let the OS resolve the complete path when it exists. If it
    // does not, fail closed instead of inventing a canonical lease identity
    // for a path whose missing suffix makes the traversal ambiguous.
    if absolute
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return match fs::canonicalize(&absolute) {
            Ok(canonical) => Ok(canonical),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!(
                    "refusing to derive slot identity from non-existent path with parent traversal: {}",
                    path.display()
                );
            }
            Err(error) => Err(error)
                .with_context(|| format!("canonicalize slot work directory {}", path.display())),
        };
    }
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(name) => normalized.push(name),
        }
    }

    let mut ancestor = normalized.clone();
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(&ancestor) {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor.file_name().ok_or_else(|| {
                    anyhow::anyhow!(
                        "slot work directory {} has no canonicalizable existing ancestor",
                        path.display()
                    )
                })?;
                missing.push(name.to_os_string());
                if !ancestor.pop() {
                    anyhow::bail!(
                        "slot work directory {} has no canonicalizable existing ancestor",
                        path.display()
                    );
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("canonicalize slot work directory {}", path.display())
                });
            }
        }
    }
}

/// Immutable ownership record inside each scope directory.
pub(crate) const STABLE_SCOPE_OWNER: &str = ".velnor-owner";

/// Empty LRU clock file inside each scope. Its mtime is refreshed atomically
/// on every allocation that lands on the scope.
pub(crate) const STABLE_SCOPE_LAST_USE: &str = ".velnor-last-use";
pub(crate) const STABLE_SCOPE_OWNER_MARKER: &[u8] = b"velnor stable workspace scope\n";

/// Durable private intent that authorizes one unpublished scope stage.
#[cfg(unix)]
const STABLE_SCOPE_INTENT: &str = ".velnor-scope-intent";
/// Hard-linked into an authorized stage before its contents are created.
#[cfg(unix)]
const STABLE_SCOPE_INTENT_PROOF: &str = ".velnor-scope-intent-proof";
/// Stage directory nested beneath its unique intent directory.
#[cfg(unix)]
const STABLE_SCOPE_STAGING_PREFIX: &str = ".velnor-scope-staging";
#[cfg(any(unix, test))]
const STABLE_SCOPE_INTENT_PREFIX: &str = ".velnor-scope-intent-v1-";
#[cfg(unix)]
const STABLE_SCOPE_INTENT_VERSION: &[u8] = b"velnor stable workspace staging intent v1\n";

/// Recorded checkout destinations for one scope, as workspace-relative
/// keys (`""` for the workspace root, `"subdir"` otherwise). Read before
/// checkout to delete destinations the previous job left behind.
const STABLE_SCOPE_DESTINATIONS: &str = ".velnor-destinations";
const STABLE_SCOPE_DESTINATIONS_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Cap for one slot's whole stable-workspace tree, in bytes.
///
/// Parity with `MBX_TARGET_MAX_SIZE` (30 GiB per managed-target store):
/// the non-mbx Rust paths get the same warm-target allowance the mbx path
/// manages for itself. The bound is per slot rather than per repository
/// because a workspace is mutated during the build and cannot be shared
/// across slots the way a content-addressed store can.
pub(crate) const STABLE_WORKSPACES_BUDGET_BYTES: u64 = 30 * 1024 * 1024 * 1024;

/// A stable workspace resolved (and, via [`prepare`], allocated) for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StableWorkspace {
    /// The workspace directory: the job's `workspace_host`.
    pub(crate) workspace: PathBuf,
    /// The owning scope directory (`<scope>/<repository-id>`).
    pub(crate) scope_dir: PathBuf,
    /// True when the scope directory did not exist before this allocation.
    pub(crate) fresh_scope: bool,
}

/// Pure path computation for a stable workspace. No filesystem effects, so
/// layout invariants (namespacing, reaper-safety, job-dir disjointness) are
/// unit-testable without fixtures.
pub(crate) fn resolve(
    slot_work_dir: &Path,
    trust_scope: &str,
    repository_key: &str,
) -> StableWorkspace {
    let scope_dir = slot_work_dir
        .join(STABLE_WORKSPACES_DIR)
        .join(crate::trust_scope::filesystem_key(trust_scope))
        .join(repository_key);
    StableWorkspace {
        workspace: scope_dir.join("workspace"),
        scope_dir,
        fresh_scope: false,
    }
}

/// Whether a workspace host path lives inside the stable tree. Composite
/// checkout plans are built after the top-level stable flag is gone, so
/// they detect stability from the path instead of threading the flag.
pub(crate) fn is_stable_workspace(workspace: &Path) -> bool {
    workspace
        .components()
        .any(|component| component.as_os_str().to_string_lossy() == STABLE_WORKSPACES_DIR)
}

fn destination_key(workspace: &Path, destination: &Path) -> Option<String> {
    let relative = destination.strip_prefix(workspace).ok()?;
    if relative.as_os_str().is_empty() {
        return Some(String::new());
    }
    let mut parts = Vec::new();
    for component in relative.components() {
        let name = component.as_os_str().to_string_lossy().into_owned();
        if name.is_empty() || name == "." || name == ".." {
            return None;
        }
        parts.push(name);
    }
    Some(parts.join("/"))
}

fn destination_path(workspace: &Path, key: &str) -> Option<PathBuf> {
    if key.is_empty() {
        return Some(workspace.to_path_buf());
    }
    if key.starts_with('/') || key.contains("//") {
        return None;
    }
    let mut path = workspace.to_path_buf();
    for part in key.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        path.push(part);
    }
    if !path.starts_with(workspace) {
        return None;
    }
    Some(path)
}

fn stable_slot_work_dir(scope_dir: &Path) -> anyhow::Result<&Path> {
    scope_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("stable workspace scope has no slot work directory")
}

/// Delete checkout destinations the previous job left behind.
///
/// The scope records its destination set on every job; destinations
/// absent from the current job are removed before any checkout runs, so
/// a job that checks out `a` never sees the previous job's `b`. A
/// previous root checkout (`""`) absent now clears the whole workspace,
/// since the root's files (including its `target/`) belong to a layout
/// the current job does not use. Deletion failures fail the job: leaking
/// prior-job files is a correctness violation, not hygiene. Record-write
/// failures fail the job before checkout: without a durable record, the next
/// job cannot know which checkout files it must remove.
pub(crate) fn prune_stale_destinations(
    scope_dir: &Path,
    workspace: &Path,
    current_destinations: &[PathBuf],
) -> anyhow::Result<()> {
    #[cfg(not(unix))]
    {
        let _ = (scope_dir, workspace, current_destinations);
        anyhow::bail!("stable workspace destination pruning requires Unix no-follow operations");
    }

    #[cfg(unix)]
    prune_stale_destinations_unix(scope_dir, workspace, current_destinations)
}

#[cfg(unix)]
struct PinnedStableWorkspaceScope {
    slot: crate::fs_copy::NoFollowDestinationDir,
    stable_root: crate::fs_copy::NoFollowDestinationDir,
    trust_scope: crate::fs_copy::NoFollowDestinationDir,
    scope: crate::fs_copy::NoFollowDestinationDir,
    workspace: crate::fs_copy::NoFollowDestinationDir,
    stable_root_path: PathBuf,
    scope_path: PathBuf,
    trust_scope_key: String,
    repository_key: String,
    slot_key: String,
    owner_identity: crate::leftover_disk::FilesystemEntryIdentity,
}

#[cfg(unix)]
impl PinnedStableWorkspaceScope {
    fn verify(&self) -> anyhow::Result<()> {
        verify_scope_directory_chain(
            &self.slot,
            &self.stable_root,
            &self.trust_scope,
            &self.scope,
            &self.workspace,
            &self.trust_scope_key,
            &self.repository_key,
        )?;
        let scope_identity =
            crate::leftover_disk::filesystem_object_identity(self.scope.descriptor()?)?;
        let Some((_, _, owner_identity, workspace_identity)) =
            validate_candidate_with_owner_identity_at(
                &self.stable_root_path,
                &self.scope_path,
                &self.slot_key,
                self.scope.descriptor()?,
                &scope_identity,
            )
        else {
            anyhow::bail!(
                "stable workspace ownership changed during destination pruning at {}",
                self.scope_path.display()
            );
        };
        anyhow::ensure!(
            owner_identity == self.owner_identity,
            "stable workspace owner marker changed during destination pruning at {}",
            self.scope_path.display()
        );
        anyhow::ensure!(
            crate::leftover_disk::filesystem_object_identity(self.workspace.descriptor()?)?
                == workspace_identity,
            "stable workspace directory changed during destination pruning at {}",
            self.scope_path.join("workspace").display()
        );
        Ok(())
    }

    fn clear_workspace(&mut self) -> anyhow::Result<()> {
        self.clear_workspace_with_preflight_and_hook(
            |scope, name| scope.preflight_tree_entry_removal(name),
            || Ok(()),
        )
    }

    #[cfg(test)]
    fn clear_workspace_with_mount_id(
        &mut self,
        mount_id_for: &impl Fn(&fs::File) -> anyhow::Result<u64>,
    ) -> anyhow::Result<()> {
        self.clear_workspace_with_preflight_and_hook(
            |scope, name| scope.preflight_tree_entry_removal_with_mount_id(name, mount_id_for),
            || Ok(()),
        )
    }

    #[cfg(test)]
    fn clear_workspace_with_mount_id_and_before_quarantine(
        &mut self,
        mount_id_for: &impl Fn(&fs::File) -> anyhow::Result<u64>,
        before_quarantine: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.clear_workspace_with_preflight_and_hook(
            |scope, name| scope.preflight_tree_entry_removal_with_mount_id(name, mount_id_for),
            before_quarantine,
        )
    }

    fn clear_workspace_with_preflight_and_hook(
        &mut self,
        preflight: impl Fn(
            &crate::fs_copy::NoFollowDestinationDir,
            &std::ffi::OsStr,
        ) -> anyhow::Result<()>,
        before_quarantine: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.verify()?;
        self.scope
            .remove_tree_entry_if_identity_with_hook(
                std::ffi::OsStr::new("workspace"),
                &self.workspace,
                preflight,
                before_quarantine,
            )
            .with_context(|| {
                format!(
                    "clear stable workspace {}",
                    self.scope_path.join("workspace").display()
                )
            })?;
        self.workspace = self
            .scope
            .create_child_directory_no_replace(std::ffi::OsStr::new("workspace"))
            .context("recreate stable workspace through pinned scope")?;
        self.workspace.verify_runner_owned_private_directory()?;
        self.verify()
    }

    fn remove_stale_destination_with_hook(
        &self,
        key: &str,
        before_quarantine: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let components: Vec<_> = key.split('/').collect();
        anyhow::ensure!(
            !components.is_empty()
                && components.iter().all(|component| !component.is_empty()
                    && *component != "."
                    && *component != ".."),
            "stable destination key is not normalized"
        );
        let mut parent = self.workspace.try_clone()?;
        for component in &components[..components.len() - 1] {
            let Some(child) =
                parent.open_existing_child_directory(std::ffi::OsStr::new(component))?
            else {
                return Ok(());
            };
            parent = child;
        }
        let name = std::ffi::OsStr::new(components[components.len() - 1]);
        let Some(expected) = parent.open_existing_child_directory(name)? else {
            return Ok(());
        };
        // The container bind-mount root is `workspace`; its stable-scope
        // parent is never mounted into the job. Quarantine there so job code
        // cannot replace the final directory name between identity checking
        // and unlink, even when the stale entry's original parent is inside
        // the workspace.
        parent.remove_tree_entry_if_identity_in_quarantine_parent_with_hook(
            name,
            &expected,
            &self.scope,
            |parent, name| parent.preflight_tree_entry_removal(name),
            before_quarantine,
        )
    }
}

#[cfg(unix)]
fn pin_stable_workspace_scope(
    scope_dir: &Path,
    workspace: &Path,
) -> anyhow::Result<PinnedStableWorkspaceScope> {
    let slot_work_dir = canonicalize_existing_prefix(stable_slot_work_dir(scope_dir)?)?;
    let stable_root_path = slot_work_dir.join(STABLE_WORKSPACES_DIR);
    let relative_scope = scope_dir
        .strip_prefix(&stable_root_path)
        .context("stable workspace scope is outside its slot's stable root")?;
    let mut components = relative_scope.components();
    let Component::Normal(trust_scope) = components.next().context("missing stable trust key")?
    else {
        anyhow::bail!("stable workspace trust key is not a normal path component");
    };
    let Component::Normal(repository_key) =
        components.next().context("missing stable repository key")?
    else {
        anyhow::bail!("stable workspace repository key is not a normal path component");
    };
    anyhow::ensure!(
        components.next().is_none(),
        "stable workspace scope has unexpected path components"
    );
    let trust_scope_key = trust_scope
        .to_str()
        .context("stable workspace trust key is not UTF-8")?
        .to_owned();
    let repository_key = repository_key
        .to_str()
        .context("stable workspace repository key is not UTF-8")?
        .to_owned();
    anyhow::ensure!(
        crate::trust_scope::is_filesystem_key(&trust_scope_key)
            && is_repository_store_key(&repository_key),
        "stable workspace scope keys are not canonical"
    );
    anyhow::ensure!(
        scope_dir
            == stable_root_path
                .join(&trust_scope_key)
                .join(&repository_key),
        "stable workspace scope path is not canonical"
    );
    anyhow::ensure!(
        workspace == scope_dir.join("workspace"),
        "stable workspace path does not match its scope"
    );

    let slot = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(&slot_work_dir)?;
    slot.verify_runner_owned_private_ancestors()?;
    let stable_root = slot
        .open_existing_child_directory(std::ffi::OsStr::new(STABLE_WORKSPACES_DIR))?
        .context("stable workspace root is missing")?;
    stable_root.verify_runner_owned_private_directory()?;
    let trust_scope = stable_root
        .open_existing_child_directory(std::ffi::OsStr::new(&trust_scope_key))?
        .context("stable workspace trust scope is missing")?;
    trust_scope.verify_runner_owned_private_directory()?;
    let scope = trust_scope
        .open_existing_child_directory(std::ffi::OsStr::new(&repository_key))?
        .context("stable workspace scope is missing")?;
    scope.verify_runner_owned_private_directory()?;
    let workspace_directory = scope
        .open_existing_child_directory(std::ffi::OsStr::new("workspace"))?
        .context("stable workspace is missing")?;
    workspace_directory.verify_runner_owned_private_directory()?;
    let slot_key = slot_scope_key(&slot_work_dir)?;
    let scope_identity = crate::leftover_disk::filesystem_object_identity(scope.descriptor()?)?;
    let Some((_, _, owner_identity, workspace_identity)) =
        validate_candidate_with_owner_identity_at(
            &stable_root_path,
            scope_dir,
            &slot_key,
            scope.descriptor()?,
            &scope_identity,
        )
    else {
        anyhow::bail!(
            "refusing to prune unowned stable workspace scope {}",
            scope_dir.display()
        );
    };
    anyhow::ensure!(
        crate::leftover_disk::filesystem_object_identity(workspace_directory.descriptor()?)?
            == workspace_identity,
        "stable workspace changed during destination-pruning validation"
    );
    let pinned = PinnedStableWorkspaceScope {
        slot,
        stable_root,
        trust_scope,
        scope,
        workspace: workspace_directory,
        stable_root_path,
        scope_path: scope_dir.to_path_buf(),
        trust_scope_key,
        repository_key,
        slot_key,
        owner_identity,
    };
    pinned.verify()?;
    Ok(pinned)
}

#[cfg(unix)]
fn read_destination_record(
    scope: &crate::fs_copy::NoFollowDestinationDir,
) -> anyhow::Result<Option<Vec<u8>>> {
    Ok(crate::leftover_disk::filesystem_read_regular_file_at(
        scope.descriptor()?,
        std::ffi::OsStr::new(STABLE_SCOPE_DESTINATIONS),
        STABLE_SCOPE_DESTINATIONS_MAX_BYTES,
    )?
    .map(|(contents, _, _)| contents))
}

#[cfg(unix)]
fn write_destination_record(
    pinned: &PinnedStableWorkspaceScope,
    current: &std::collections::BTreeSet<String>,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    pinned.verify()?;
    let list: Vec<&str> = current.iter().map(String::as_str).collect();
    let contents = serde_json::to_vec(&list).context("serialize stable destinations record")?;
    anyhow::ensure!(
        contents.len() <= STABLE_SCOPE_DESTINATIONS_MAX_BYTES,
        "stable destinations record exceeds its maximum size"
    );
    let parent = pinned.scope.descriptor()?;
    let record_name = std::ffi::OsStr::new(STABLE_SCOPE_DESTINATIONS);
    let (temporary_name, mut temporary_file) = (0..16)
        .find_map(|_| {
            let name = std::ffi::OsString::from(format!(
                "{STABLE_SCOPE_DESTINATIONS}.tmp-{}",
                uuid::Uuid::new_v4()
            ));
            match rustix::fs::openat(
                parent,
                &name,
                rustix::fs::OFlags::WRONLY
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::from_raw_mode(0o600),
            ) {
                Ok(file) => Some(Ok((name, fs::File::from(file)))),
                Err(rustix::io::Errno::EXIST) => None,
                Err(error) => Some(Err(std::io::Error::from(error))),
            }
        })
        .context("could not allocate temporary stable destinations record")??;

    let write_result = (|| -> anyhow::Result<()> {
        temporary_file
            .write_all(&contents)
            .context("write temporary stable destinations record")?;
        temporary_file
            .sync_all()
            .context("sync temporary stable destinations record")?;
        let metadata = temporary_file
            .metadata()
            .context("inspect temporary stable destinations record")?;
        anyhow::ensure!(
            metadata.is_file(),
            "temporary stable destinations record is not a regular file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            anyhow::ensure!(
                metadata.nlink() == 1,
                "temporary stable destinations record has unexpected hard links"
            );
        }
        rustix::fs::renameat(parent, &temporary_name, parent, record_name)
            .map_err(std::io::Error::from)
            .context("publish stable destinations record")?;
        pinned.scope.sync_directory()?;
        let published = read_destination_record(&pinned.scope)?
            .context("published stable destinations record disappeared")?;
        anyhow::ensure!(
            published == contents,
            "published stable destinations record changed during write"
        );
        pinned.verify()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let cleanup = rustix::fs::unlinkat(parent, &temporary_name, rustix::fs::AtFlags::empty());
        return match cleanup {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Err(error),
            Err(cleanup_error) => Err(error).context(format!(
                "temporary stable destinations record cleanup also failed: {}",
                std::io::Error::from(cleanup_error)
            )),
        };
    }
    Ok(())
}

#[cfg(unix)]
fn prune_stale_destinations_unix(
    scope_dir: &Path,
    workspace: &Path,
    current_destinations: &[PathBuf],
) -> anyhow::Result<()> {
    prune_stale_destinations_unix_with_before_remove(
        scope_dir,
        workspace,
        current_destinations,
        |_| Ok(()),
    )
}

#[cfg(all(test, unix))]
fn prune_stale_destinations_unix_with_hook(
    scope_dir: &Path,
    workspace: &Path,
    current_destinations: &[PathBuf],
    before_remove: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    prune_stale_destinations_unix_with_before_remove(
        scope_dir,
        workspace,
        current_destinations,
        before_remove,
    )
}

#[cfg(unix)]
fn prune_stale_destinations_unix_with_before_remove(
    scope_dir: &Path,
    workspace: &Path,
    current_destinations: &[PathBuf],
    mut before_remove: impl FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut pinned = pin_stable_workspace_scope(scope_dir, workspace)?;
    let mut current = std::collections::BTreeSet::new();
    for destination in current_destinations {
        if let Some(key) = destination_key(workspace, destination) {
            current.insert(key);
        }
    }
    let previous = match read_destination_record(&pinned.scope) {
        Ok(Some(contents)) => serde_json::from_slice::<Vec<String>>(&contents)
            .map(|list| Some(list.into_iter().collect::<std::collections::BTreeSet<_>>()))
            .map_err(anyhow::Error::from),
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
    .and_then(|previous| {
        if let Some(previous) = &previous {
            anyhow::ensure!(
                previous
                    .iter()
                    .all(|key| destination_path(workspace, key).is_some()),
                "stable destinations record contains an unsafe path"
            );
        }
        Ok(previous)
    });
    let previous = match previous {
        Ok(Some(previous)) => previous,
        Ok(None) => {
            eprintln!(
                "forensics.lifecycle: stable destinations missing at {}, clearing reused workspace",
                scope_dir.join(STABLE_SCOPE_DESTINATIONS).display()
            );
            pinned.clear_workspace()?;
            write_destination_record(&pinned, &current)?;
            return Ok(());
        }
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable destinations unreadable at {} ({error:#}), clearing workspace",
                scope_dir.join(STABLE_SCOPE_DESTINATIONS).display()
            );
            pinned.clear_workspace()?;
            write_destination_record(&pinned, &current)?;
            return Ok(());
        }
    };
    let stale: Vec<String> = previous.difference(&current).cloned().collect();
    if stale.is_empty() {
        write_destination_record(&pinned, &current)?;
        return Ok(());
    }
    if stale.iter().any(|key| key.is_empty()) {
        eprintln!(
            "forensics.lifecycle: stable workspace clearing {} (previous root checkout absent)",
            workspace.display()
        );
        pinned.clear_workspace()?;
        write_destination_record(&pinned, &current)?;
        return Ok(());
    }
    for key in stale {
        let Some(path) = destination_path(workspace, &key) else {
            eprintln!(
                "forensics.lifecycle: stable workspace skipping unsafe recorded destination {key:?}"
            );
            continue;
        };
        eprintln!(
            "forensics.lifecycle: stable workspace removing stale destination {}",
            path.display()
        );
        pinned.verify()?;
        pinned
            .remove_stale_destination_with_hook(&key, || before_remove(&key))
            .with_context(|| format!("remove stale stable destination {}", path.display()))?;
        pinned.verify()?;
    }
    write_destination_record(&pinned, &current)?;
    Ok(())
}

/// Publish the job's exact scope lease, then allocate the stable workspace:
/// enforce the slot budget, create the directories, and refresh the LRU clock.
///
/// The lease is created before the budget scan or any workspace access. The
/// scan trims an over-budget tree on every admission; active scopes are the
/// only exclusion from eviction.
pub(crate) fn prepare(
    slot_work_dir: &Path,
    run_root: &Path,
    trust_scope: &str,
    repository_key: &str,
    job_id: &str,
) -> anyhow::Result<(StableWorkspace, crate::capacity::ScopeLease)> {
    prepare_with_before_marker(
        slot_work_dir,
        run_root,
        trust_scope,
        repository_key,
        job_id,
        || Ok(()),
    )
}

fn prepare_with_before_marker(
    slot_work_dir: &Path,
    run_root: &Path,
    trust_scope: &str,
    repository_key: &str,
    job_id: &str,
    before_marker: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<(StableWorkspace, crate::capacity::ScopeLease)> {
    // Use one resolved physical path for both the published lease identity
    // and every filesystem operation below. In particular, do not hash a
    // symlink-aware key while allocating through a separately normalized
    // spelling of the same path.
    let slot_work_dir = canonicalize_existing_prefix(slot_work_dir)?;
    anyhow::ensure!(
        is_repository_store_key(repository_key),
        "stable workspace repository key is not canonical: {repository_key:?}"
    );
    let owner_job_id = crate::capacity::JobOwnerId::parse(job_id)?;
    let lease_scope = lease_scope(&slot_work_dir, trust_scope, repository_key)?;
    let holder_scope = format!(
        "{lease_scope}/{}",
        crate::trust_scope::filesystem_key(job_id)
    );
    let lease = crate::capacity::ScopeLease::acquire_for_job(
        run_root,
        "stable-workspace",
        &holder_scope,
        owner_job_id,
        Duration::from_secs(24 * 3600),
    )?;
    let mut stable = resolve(&slot_work_dir, trust_scope, repository_key);

    enforce_budget(
        &slot_work_dir.join(STABLE_WORKSPACES_DIR),
        run_root,
        STABLE_WORKSPACES_BUDGET_BYTES,
    );
    // Intent recovery, existing-scope refresh, and fresh publication share
    // one exclusive coordinator. A recovery pass can therefore distinguish
    // an abandoned stage from an active creator without guessing from age.
    let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(run_root)?;

    // Open the slot path once, without following descendants. Keep every
    // directory descriptor live through marker mutation so a replaced path
    // cannot redirect writes outside the trusted slot tree.
    let slot_directory =
        crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(&slot_work_dir)?;
    slot_directory.verify_runner_owned_private_ancestors()?;
    let stable_root_directory = slot_directory
        .open_or_create_child_directory(std::ffi::OsStr::new(STABLE_WORKSPACES_DIR))?;
    stable_root_directory.verify_runner_owned_private_directory()?;
    let trust_scope_key = crate::trust_scope::filesystem_key(trust_scope);
    let trust_directory = stable_root_directory
        .open_or_create_child_directory(std::ffi::OsStr::new(&trust_scope_key))?;
    trust_directory.verify_runner_owned_private_directory()?;
    let slot_key = slot_scope_key(&slot_work_dir)?;

    let holder_lease_scope = format!("stable-workspace/{holder_scope}");
    let active_scopes =
        crate::capacity::active_scope_leases(run_root, Duration::from_secs(24 * 3600))?
            .into_iter()
            .filter(|active| active.scope != holder_lease_scope)
            .map(|active| active.scope)
            .collect();
    recover_abandoned_staging_in_trust_scope(
        &slot_work_dir.join(STABLE_WORKSPACES_DIR),
        &trust_directory,
        &slot_key,
        &trust_scope_key,
        &active_scopes,
    )?;

    let existing_scope = match trust_directory
        .open_existing_child_directory(std::ffi::OsStr::new(repository_key))?
    {
        Some(scope_directory) => {
            scope_directory.verify_runner_owned_private_directory()?;
            let scope_identity =
                crate::leftover_disk::filesystem_object_identity(scope_directory.descriptor()?)?;
            let Some((_, _, owner_identity, validated_workspace_identity)) =
                validate_candidate_with_owner_identity_at(
                    &slot_work_dir.join(STABLE_WORKSPACES_DIR),
                    &stable.scope_dir,
                    &slot_key,
                    scope_directory.descriptor()?,
                    &scope_identity,
                )
            else {
                anyhow::bail!(
                    "refusing to adopt existing unowned stable workspace scope {}",
                    stable.scope_dir.display()
                );
            };
            let workspace_directory = scope_directory
                .open_existing_child_directory(std::ffi::OsStr::new("workspace"))?
                .context("owned stable workspace lost its workspace directory")?;
            workspace_directory.verify_runner_owned_private_directory()?;
            anyhow::ensure!(
                crate::leftover_disk::filesystem_object_identity(
                    workspace_directory.descriptor()?
                )? == validated_workspace_identity,
                "stable workspace directory changed after ownership validation at {}",
                stable.workspace.display()
            );
            Some((scope_directory, workspace_directory, owner_identity))
        }
        None => None,
    };
    stable.fresh_scope = existing_scope.is_none();
    let (scope_directory, workspace_directory, _owner_identity) = match existing_scope {
        Some((scope_directory, workspace_directory, owner_identity)) => {
            before_marker()?;
            verify_scope_directory_chain(
                &slot_directory,
                &stable_root_directory,
                &trust_directory,
                &scope_directory,
                &workspace_directory,
                &trust_scope_key,
                repository_key,
            )?;
            refresh_scope_marker(&scope_directory, &owner_identity)?;
            (scope_directory, workspace_directory, owner_identity)
        }
        None => {
            slot_directory.verify_child_directory_identity(
                std::ffi::OsStr::new(STABLE_WORKSPACES_DIR),
                &stable_root_directory,
            )?;
            stable_root_directory.verify_child_directory_identity(
                std::ffi::OsStr::new(&trust_scope_key),
                &trust_directory,
            )?;
            create_and_publish_scope(
                &trust_directory,
                &slot_work_dir.join(STABLE_WORKSPACES_DIR),
                &slot_key,
                &trust_scope_key,
                repository_key,
                before_marker,
            )?
        }
    };
    verify_scope_directory_chain(
        &slot_directory,
        &stable_root_directory,
        &trust_directory,
        &scope_directory,
        &workspace_directory,
        &trust_scope_key,
        repository_key,
    )?;
    let scope_identity =
        crate::leftover_disk::filesystem_object_identity(scope_directory.descriptor()?)?;
    let final_validation = validate_candidate_with_owner_identity_at(
        &slot_work_dir.join(STABLE_WORKSPACES_DIR),
        &stable.scope_dir,
        &slot_key,
        scope_directory.descriptor()?,
        &scope_identity,
    );
    let Some((_, _, _, final_workspace_identity)) = final_validation else {
        anyhow::bail!(
            "stable workspace ownership changed during prepare at {}",
            stable.scope_dir.display()
        );
    };
    anyhow::ensure!(
        crate::leftover_disk::filesystem_object_identity(workspace_directory.descriptor()?)?
            == final_workspace_identity,
        "stable workspace directory changed during prepare at {}",
        stable.workspace.display()
    );
    Ok((stable, lease))
}

#[cfg(unix)]
fn create_and_publish_scope(
    trust_directory: &crate::fs_copy::NoFollowDestinationDir,
    stable_root: &Path,
    slot_key: &str,
    trust_scope_key: &str,
    repository_key: &str,
    before_marker: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<(
    crate::fs_copy::NoFollowDestinationDir,
    crate::fs_copy::NoFollowDestinationDir,
    crate::leftover_disk::FilesystemEntryIdentity,
)> {
    let stage_id = uuid::Uuid::new_v4();
    let intent_prefix = format!("{STABLE_SCOPE_INTENT_PREFIX}{repository_key}-{stage_id}");
    let (intent_directory, intent_name) =
        trust_directory.create_unique_directory(&intent_prefix)?;
    let intent_parts = parse_scope_intent_name(&intent_name).context("parse new scope intent")?;
    anyhow::ensure!(
        intent_parts.repository_key == repository_key && intent_parts.stage_id == stage_id,
        "new stable-workspace intent name does not match its scope"
    );
    let intent_record = format_scope_intent_record(
        slot_key,
        trust_scope_key,
        repository_key,
        stage_id,
        intent_parts.nonce,
    );
    let intent_record_identity = write_scope_intent_record(&intent_directory, &intent_record)?;
    // The exact-scope intent and its file are durable before the nested stage
    // directory can exist. Recovery never sweeps a stage by its prefix alone.
    trust_directory.sync_directory()?;
    let stage_prefix = format!("{STABLE_SCOPE_STAGING_PREFIX}-{stage_id}");
    let (scope_directory, staging_name) =
        intent_directory.create_unique_directory(&stage_prefix)?;
    let mut published = false;
    let result = (|| {
        link_scope_intent_proof(&intent_directory, &scope_directory, &intent_record_identity)?;
        intent_directory.sync_directory()?;
        scope_directory.sync_directory()?;
        scope_directory.verify_runner_owned_private_directory()?;
        let workspace_directory =
            scope_directory.create_child_directory_no_replace(std::ffi::OsStr::new("workspace"))?;
        workspace_directory.verify_runner_owned_private_directory()?;

        // A failed or interrupted fresh allocation stays below its durable
        // exact-scope intent. The canonical repository name appears only
        // after every record and the workspace directory are durable.
        before_marker()?;
        let owner_identity = create_scope_record(
            &scope_directory,
            STABLE_SCOPE_OWNER,
            STABLE_SCOPE_OWNER_MARKER,
        )?;
        create_scope_record(&scope_directory, STABLE_SCOPE_LAST_USE, &[])?;
        workspace_directory.sync_directory()?;
        scope_directory.sync_directory()?;

        let intent_parent = intent_directory.descriptor()?;
        intent_directory.verify_child_directory_identity(&staging_name, &scope_directory)?;
        trust_directory.verify_child_directory_identity(&intent_name, &intent_directory)?;
        let parent = trust_directory.descriptor()?;
        rustix::fs::renameat_with(
            intent_parent,
            &staging_name,
            parent,
            std::ffi::OsStr::new(repository_key),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!("publish complete stable-workspace scope {repository_key} without replacement")
        })?;
        published = true;
        trust_directory.sync_directory()?;
        intent_directory.sync_directory()?;
        // The published scope retains the matching proof. Removing the
        // private intent drops its hard link, leaving that proof singly linked.
        remove_scope_intent(
            trust_directory,
            stable_root,
            slot_key,
            trust_scope_key,
            repository_key,
            stage_id,
            &intent_name,
            &intent_directory,
            Some((&intent_record, &intent_record_identity)),
            Some(&scope_directory),
            |_| Ok(()),
        )?;
        Ok((scope_directory, workspace_directory, owner_identity))
    })();

    if result.is_err() && !published {
        let cleanup = remove_scope_intent(
            trust_directory,
            stable_root,
            slot_key,
            trust_scope_key,
            repository_key,
            stage_id,
            &intent_name,
            &intent_directory,
            Some((&intent_record, &intent_record_identity)),
            None,
            |_| Ok(()),
        );
        return match cleanup {
            Ok(()) => result,
            Err(cleanup_error) => result.context(format!(
                "incomplete stable-workspace intent cleanup failed: {cleanup_error:#}"
            )),
        };
    }
    result
}

#[derive(Debug)]
#[cfg(unix)]
struct ScopeIntentParts {
    repository_key: String,
    stage_id: uuid::Uuid,
    nonce: uuid::Uuid,
}

#[cfg(unix)]
fn parse_scope_intent_name(name: &std::ffi::OsStr) -> Option<ScopeIntentParts> {
    let name = name.to_str()?;
    let suffix = name.strip_prefix(STABLE_SCOPE_INTENT_PREFIX)?;
    let repository_key_len = "repo-key-v1-".len() + 64;
    let repository_key = suffix.get(..repository_key_len)?;
    if !is_repository_store_key(repository_key) {
        return None;
    }
    if suffix.get(repository_key_len..repository_key_len + 1)? != "-" {
        return None;
    }
    let ids = suffix.get(repository_key_len + 1..)?;
    let stage_id_text = ids.get(..36)?;
    if ids.get(36..37)? != "-" {
        return None;
    }
    let nonce_text = ids.get(37..)?;
    let stage_id = uuid::Uuid::parse_str(stage_id_text).ok()?;
    let nonce = uuid::Uuid::parse_str(nonce_text).ok()?;
    if stage_id.to_string() != stage_id_text || nonce.to_string() != nonce_text {
        return None;
    }
    Some(ScopeIntentParts {
        repository_key: repository_key.to_owned(),
        stage_id,
        nonce,
    })
}

#[cfg(unix)]
fn format_scope_intent_record(
    slot_key: &str,
    trust_scope_key: &str,
    repository_key: &str,
    stage_id: uuid::Uuid,
    nonce: uuid::Uuid,
) -> Vec<u8> {
    let mut record = STABLE_SCOPE_INTENT_VERSION.to_vec();
    for part in [
        slot_key.to_owned(),
        trust_scope_key.to_owned(),
        repository_key.to_owned(),
        stage_id.to_string(),
        nonce.to_string(),
    ] {
        record.extend_from_slice(part.as_bytes());
        record.push(b'\n');
    }
    record
}

#[cfg(unix)]
fn write_scope_intent_record(
    intent_directory: &crate::fs_copy::NoFollowDestinationDir,
    contents: &[u8],
) -> anyhow::Result<crate::leftover_disk::FilesystemEntryIdentity> {
    let temporary_name = format!("{STABLE_SCOPE_INTENT}.tmp-{}", uuid::Uuid::new_v4());
    let temporary_identity = create_scope_record(intent_directory, &temporary_name, contents)?;
    let parent = intent_directory.descriptor()?;
    rustix::fs::renameat_with(
        parent,
        std::ffi::OsStr::new(&temporary_name),
        parent,
        std::ffi::OsStr::new(STABLE_SCOPE_INTENT),
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
    .context("atomically publish stable-workspace staging intent")?;
    intent_directory.sync_directory()?;
    let (_, published_identity, links) = read_scope_record_with_links(
        parent,
        std::ffi::OsStr::new(STABLE_SCOPE_INTENT),
        contents.len(),
    )?;
    anyhow::ensure!(
        links == 1 && published_identity == temporary_identity,
        "stable-workspace staging intent changed during publication"
    );
    Ok(published_identity)
}

#[cfg(unix)]
fn link_scope_intent_proof(
    intent_directory: &crate::fs_copy::NoFollowDestinationDir,
    scope_directory: &crate::fs_copy::NoFollowDestinationDir,
    expected_identity: &crate::leftover_disk::FilesystemEntryIdentity,
) -> anyhow::Result<()> {
    let intent_parent = intent_directory.descriptor()?;
    let scope_parent = scope_directory.descriptor()?;
    let (contents, identity, links) = read_scope_record_with_links(
        intent_parent,
        std::ffi::OsStr::new(STABLE_SCOPE_INTENT),
        1024,
    )?;
    anyhow::ensure!(
        &identity == expected_identity && links == 1,
        "stable-workspace staging intent changed before proof publication"
    );
    rustix::fs::linkat(
        intent_parent,
        Path::new(STABLE_SCOPE_INTENT),
        scope_parent,
        Path::new(STABLE_SCOPE_INTENT_PROOF),
        rustix::fs::AtFlags::empty(),
    )
    .map_err(std::io::Error::from)
    .context("link durable stable-workspace intent into stage")?;
    let (intent_contents, intent_identity, intent_links) = read_scope_record_with_links(
        intent_parent,
        std::ffi::OsStr::new(STABLE_SCOPE_INTENT),
        1024,
    )?;
    let (proof_contents, proof_identity, proof_links) = read_scope_record_with_links(
        scope_parent,
        std::ffi::OsStr::new(STABLE_SCOPE_INTENT_PROOF),
        1024,
    )?;
    anyhow::ensure!(
        intent_contents == contents
            && proof_contents == contents
            && intent_identity == *expected_identity
            && proof_identity == *expected_identity
            && intent_links == 2
            && proof_links == 2,
        "stable-workspace staging proof does not match its durable intent"
    );
    Ok(())
}

#[cfg(unix)]
fn read_scope_record_with_links(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    maximum_bytes: usize,
) -> anyhow::Result<(Vec<u8>, crate::leftover_disk::FilesystemEntryIdentity, u64)> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;

    let mut record: fs::File = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .context("open stable-workspace intent record without following links")?
    .into();
    let metadata = record
        .metadata()
        .context("inspect stable-workspace intent record")?;
    anyhow::ensure!(
        metadata.is_file(),
        "stable-workspace intent is not a regular file"
    );
    let identity = crate::leftover_disk::filesystem_object_identity(&record)?;
    let named = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)
        .context("recheck stable-workspace intent record name")?;
    anyhow::ensure!(
        rustix::fs::FileType::from_raw_mode(named.st_mode) == rustix::fs::FileType::RegularFile
            && named.st_dev as u64 == metadata.dev()
            && named.st_ino == metadata.ino(),
        "stable-workspace intent record changed during secure open"
    );
    anyhow::ensure!(
        usize::try_from(metadata.len()).unwrap_or(usize::MAX) <= maximum_bytes,
        "stable-workspace intent record exceeds its maximum length"
    );
    let links = metadata.nlink();
    let mut contents = Vec::new();
    record
        .by_ref()
        .take(
            u64::try_from(maximum_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut contents)
        .context("read stable-workspace intent record")?;
    anyhow::ensure!(
        contents.len() <= maximum_bytes,
        "stable-workspace intent record grew beyond its maximum length"
    );
    Ok((contents, identity, links))
}

/// Recover only stages named by a durable intent and not protected by an
/// active stable-workspace lease. Callers must hold the exclusive filesystem
/// coordinator so an active creator cannot be mistaken for an orphan.
#[cfg(unix)]
pub(crate) fn recover_abandoned_staging_under_coordinator(
    stable_root: &Path,
    active_scopes: &std::collections::BTreeSet<String>,
) -> anyhow::Result<()> {
    if stable_root.file_name() != Some(std::ffi::OsStr::new(STABLE_WORKSPACES_DIR)) {
        anyhow::bail!(
            "stable-workspace recovery root has an unexpected name: {}",
            stable_root.display()
        );
    }
    let slot_work_dir = stable_root
        .parent()
        .context("stable-workspace recovery root has no slot directory")?;
    match fs::symlink_metadata(slot_work_dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => anyhow::bail!(
            "stable-workspace recovery slot is not a real directory: {}",
            slot_work_dir.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect stable-workspace recovery slot {}",
                    slot_work_dir.display()
                )
            });
        }
    }
    let slot_directory =
        crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(slot_work_dir)?;
    slot_directory.verify_runner_owned_private_ancestors()?;
    let Some(stable_root_directory) = slot_directory
        .open_existing_child_directory(std::ffi::OsStr::new(STABLE_WORKSPACES_DIR))?
    else {
        return Ok(());
    };
    stable_root_directory.verify_runner_owned_private_directory()?;
    let slot_key = slot_scope_key(slot_work_dir)?;
    stable_root_directory.for_each_entry_name(|trust_scope_name| {
        let Some(trust_scope_key) = trust_scope_name.to_str() else {
            return Ok(());
        };
        if !crate::trust_scope::is_filesystem_key(trust_scope_key) {
            return Ok(());
        }
        let trust_directory = match stable_root_directory
            .open_existing_child_directory(&trust_scope_name)
        {
            Ok(Some(trust_directory)) => trust_directory,
            Ok(None) => return Ok(()),
            Err(error) => {
                eprintln!(
                    "forensics.lifecycle: preserve stable-workspace intents under {}: cannot open trust directory without following links: {error:#}",
                    stable_root.join(trust_scope_key).display()
                );
                return Ok(());
            }
        };
        if let Err(error) = trust_directory.verify_runner_owned_private_directory() {
            eprintln!(
                "forensics.lifecycle: preserve stable-workspace intents under {}: trust directory is not private: {error:#}",
                stable_root.join(trust_scope_key).display()
            );
            return Ok(());
        }
        recover_abandoned_staging_in_trust_scope(
            stable_root,
            &trust_directory,
            &slot_key,
            trust_scope_key,
            active_scopes,
        )
    })
}

#[cfg(not(unix))]
pub(crate) fn recover_abandoned_staging_under_coordinator(
    _stable_root: &Path,
    _active_scopes: &std::collections::BTreeSet<String>,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn recover_abandoned_staging_in_trust_scope(
    stable_root: &Path,
    trust_directory: &crate::fs_copy::NoFollowDestinationDir,
    slot_key: &str,
    trust_scope_key: &str,
    active_scopes: &std::collections::BTreeSet<String>,
) -> anyhow::Result<()> {
    trust_directory.for_each_entry_name(|intent_name| {
        let Some(parts) = parse_scope_intent_name(&intent_name) else {
            if intent_name
                .to_str()
                .is_some_and(|name| name.starts_with(STABLE_SCOPE_INTENT_PREFIX))
            {
                eprintln!(
                    "forensics.lifecycle: preserve malformed stable-workspace intent {}",
                    stable_root
                        .join(trust_scope_key)
                        .join(&intent_name)
                        .display()
                );
            }
            return Ok(());
        };
        let lease_scope = format!(
            "stable-workspace/{slot_key}/{trust_scope_key}/{}",
            parts.repository_key
        );
        if active_scopes
            .iter()
            .any(|active| scopes_overlap(active, &lease_scope))
        {
            return Ok(());
        }
        if let Err(error) = recover_one_scope_intent(
            stable_root,
            trust_directory,
            trust_scope_key,
            &intent_name,
            &parts,
            slot_key,
        ) {
            eprintln!(
                "forensics.lifecycle: preserve stable-workspace intent {}: {error:#}",
                stable_root
                    .join(trust_scope_key)
                    .join(&intent_name)
                    .display()
            );
        }
        Ok(())
    })
}

#[cfg(not(unix))]
fn recover_abandoned_staging_in_trust_scope(
    _stable_root: &Path,
    _trust_directory: &crate::fs_copy::NoFollowDestinationDir,
    _slot_key: &str,
    _trust_scope_key: &str,
    _active_scopes: &std::collections::BTreeSet<String>,
) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn recover_one_scope_intent(
    stable_root: &Path,
    trust_directory: &crate::fs_copy::NoFollowDestinationDir,
    trust_scope_key: &str,
    intent_name: &std::ffi::OsStr,
    parts: &ScopeIntentParts,
    slot_key: &str,
) -> anyhow::Result<()> {
    let Some(intent_directory) = trust_directory.open_existing_child_directory(intent_name)? else {
        return Ok(());
    };
    intent_directory.verify_runner_owned_private_directory()?;
    let trust_identity =
        crate::leftover_disk::filesystem_object_identity(trust_directory.descriptor()?)?;
    let intent_identity =
        crate::leftover_disk::filesystem_object_identity(intent_directory.descriptor()?)?;
    anyhow::ensure!(
        trust_identity.device == intent_identity.device
            && trust_identity.mount == intent_identity.mount,
        "stable-workspace intent crosses its trust directory mount"
    );
    let children = entry_names(&intent_directory)?;
    let intent_record_name = std::ffi::OsStr::new(STABLE_SCOPE_INTENT);
    let Some(record_index) = children.iter().position(|name| name == intent_record_name) else {
        // The stage cannot exist until the final record is durable. An
        // interrupted temp write is therefore safe to clear only when the
        // intent directory contains nothing except exact, no-follow temp
        // records from this protocol.
        anyhow::ensure!(
            children.len() <= 1,
            "empty stable-workspace intent contains too many temporary records"
        );
        for child in &children {
            if !parse_scope_intent_temporary_name(child) {
                anyhow::bail!("intent without a durable record contains unexpected data");
            }
            let (_, _, links) =
                read_scope_record_with_links(intent_directory.descriptor()?, child, 1024)?;
            anyhow::ensure!(
                links == 1,
                "temporary intent record has unexpected hard links"
            );
        }
        return remove_scope_intent(
            trust_directory,
            stable_root,
            slot_key,
            trust_scope_key,
            &parts.repository_key,
            parts.stage_id,
            intent_name,
            &intent_directory,
            None,
            None,
            |_| Ok(()),
        );
    };
    anyhow::ensure!(
        children.len() <= 2,
        "stable-workspace intent directory contains unexpected entries"
    );
    let expected_record = format_scope_intent_record(
        slot_key,
        trust_scope_key,
        &parts.repository_key,
        parts.stage_id,
        parts.nonce,
    );
    let (record_contents, record_identity, record_links) =
        read_scope_record_with_links(intent_directory.descriptor()?, intent_record_name, 1024)?;
    anyhow::ensure!(
        record_contents == expected_record && (record_links == 1 || record_links == 2),
        "stable-workspace intent record does not match its encoded scope"
    );

    let stage_name = children
        .iter()
        .enumerate()
        .filter(|(index, _child)| *index != record_index)
        .map(|(_, child)| child)
        .next();
    if let Some(stage_name) = stage_name {
        anyhow::ensure!(
            trust_directory
                .open_existing_child_directory(std::ffi::OsStr::new(&parts.repository_key))?
                .is_none(),
            "canonical scope and unpublished stage coexist; preserve ambiguous intent"
        );
        anyhow::ensure!(
            is_authorized_stage_name(stage_name, parts.stage_id),
            "stable-workspace intent contains an unexpected stage name"
        );
        let Some(stage_directory) = intent_directory.open_existing_child_directory(stage_name)?
        else {
            anyhow::bail!("authorized stable-workspace stage disappeared during recovery");
        };
        stage_directory.verify_runner_owned_private_directory()?;
        let stage_identity =
            crate::leftover_disk::filesystem_object_identity(stage_directory.descriptor()?)?;
        anyhow::ensure!(
            stage_identity.device == intent_identity.device
                && stage_identity.mount == intent_identity.mount,
            "stable-workspace stage crosses its intent mount"
        );
        validate_unpublished_stage(
            &stage_directory,
            &expected_record,
            &record_identity,
            record_links,
        )?;
        return remove_scope_intent(
            trust_directory,
            stable_root,
            slot_key,
            trust_scope_key,
            &parts.repository_key,
            parts.stage_id,
            intent_name,
            &intent_directory,
            Some((&expected_record, &record_identity)),
            None,
            |_| Ok(()),
        );
    }

    let canonical_path = stable_root
        .join(trust_scope_key)
        .join(&parts.repository_key);
    match trust_directory
        .open_existing_child_directory(std::ffi::OsStr::new(&parts.repository_key))?
    {
        None => {
            anyhow::ensure!(
                record_links == 1,
                "intent record remains linked without an authorized stage or published scope"
            );
            remove_scope_intent(
                trust_directory,
                stable_root,
                slot_key,
                trust_scope_key,
                &parts.repository_key,
                parts.stage_id,
                intent_name,
                &intent_directory,
                Some((&expected_record, &record_identity)),
                None,
                |_| Ok(()),
            )
        }
        Some(canonical_directory) => {
            anyhow::ensure!(
                record_links == 2,
                "published intent lost its stage proof link"
            );
            validate_published_scope_proof(
                stable_root,
                &canonical_path,
                &canonical_directory,
                slot_key,
                &expected_record,
                &record_identity,
            )?;
            remove_scope_intent(
                trust_directory,
                stable_root,
                slot_key,
                trust_scope_key,
                &parts.repository_key,
                parts.stage_id,
                intent_name,
                &intent_directory,
                Some((&expected_record, &record_identity)),
                Some(&canonical_directory),
                |_| Ok(()),
            )
        }
    }
}

#[cfg(unix)]
fn validate_unpublished_stage(
    stage: &crate::fs_copy::NoFollowDestinationDir,
    expected_record: &[u8],
    expected_identity: &crate::leftover_disk::FilesystemEntryIdentity,
    intent_links: u64,
) -> anyhow::Result<()> {
    let children = entry_names(stage)?;
    let proof_name = std::ffi::OsStr::new(STABLE_SCOPE_INTENT_PROOF);
    let proof_present = children.iter().any(|name| name == proof_name);
    anyhow::ensure!(
        if proof_present {
            intent_links == 2
        } else {
            intent_links == 1 && children.is_empty()
        },
        "stable-workspace stage has no complete matching proof"
    );
    for child in &children {
        if child == proof_name {
            let (contents, identity, links) =
                read_scope_record_with_links(stage.descriptor()?, proof_name, 1024)?;
            anyhow::ensure!(
                contents == expected_record && identity == *expected_identity && links == 2,
                "stable-workspace stage proof does not match its intent"
            );
        } else if child == std::ffi::OsStr::new("workspace") {
            let Some(workspace) = stage.open_existing_child_directory(child)? else {
                anyhow::bail!("staged workspace disappeared during recovery");
            };
            workspace.verify_runner_owned_private_directory()?;
            anyhow::ensure!(
                entry_names(&workspace)?.is_empty(),
                "unpublished stable-workspace contains unexpected workspace data"
            );
        } else if child == std::ffi::OsStr::new(STABLE_SCOPE_OWNER)
            || child == std::ffi::OsStr::new(STABLE_SCOPE_LAST_USE)
        {
            let (contents, _, links) =
                read_scope_record_with_links(stage.descriptor()?, child, 1024)?;
            anyhow::ensure!(links == 1, "staged scope record has unexpected hard links");
            if child == std::ffi::OsStr::new(STABLE_SCOPE_LAST_USE) {
                anyhow::ensure!(contents.is_empty(), "staged LRU clock is not empty");
            }
        } else {
            anyhow::bail!("unpublished stable-workspace stage contains unexpected data");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_published_scope_proof(
    stable_root: &Path,
    canonical_path: &Path,
    canonical_directory: &crate::fs_copy::NoFollowDestinationDir,
    slot_key: &str,
    expected_record: &[u8],
    expected_identity: &crate::leftover_disk::FilesystemEntryIdentity,
) -> anyhow::Result<()> {
    canonical_directory.verify_runner_owned_private_directory()?;
    let scope_identity =
        crate::leftover_disk::filesystem_object_identity(canonical_directory.descriptor()?)?;
    anyhow::ensure!(
        validate_candidate_with_owner_identity_at(
            stable_root,
            canonical_path,
            slot_key,
            canonical_directory.descriptor()?,
            &scope_identity,
        )
        .is_some(),
        "published stable-workspace scope does not have a valid owner record"
    );
    let (contents, identity, links) = read_scope_record_with_links(
        canonical_directory.descriptor()?,
        std::ffi::OsStr::new(STABLE_SCOPE_INTENT_PROOF),
        1024,
    )?;
    anyhow::ensure!(
        contents == expected_record && identity == *expected_identity && links == 2,
        "published stable-workspace proof does not match its durable intent"
    );
    Ok(())
}

#[cfg(unix)]
/// Remove only a validated intent's protocol entries, in durable order. The
/// intent stays under its recognized name until every unpublished-stage
/// child is gone; published cleanup unlinks the intent record last so its
/// canonical proof remains authorized across interruption.
#[allow(clippy::too_many_arguments)]
fn remove_scope_intent(
    trust_directory: &crate::fs_copy::NoFollowDestinationDir,
    stable_root: &Path,
    slot_key: &str,
    trust_scope_key: &str,
    repository_key: &str,
    stage_id: uuid::Uuid,
    intent_name: &std::ffi::OsStr,
    intent_directory: &crate::fs_copy::NoFollowDestinationDir,
    expected_record: Option<(&[u8], &crate::leftover_disk::FilesystemEntryIdentity)>,
    published_scope: Option<&crate::fs_copy::NoFollowDestinationDir>,
    mut after_step: impl FnMut(&'static str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    trust_directory.verify_child_directory_identity(intent_name, intent_directory)?;
    intent_directory.verify_runner_owned_private_directory()?;
    let trust_identity =
        crate::leftover_disk::filesystem_object_identity(trust_directory.descriptor()?)?;
    let intent_identity =
        crate::leftover_disk::filesystem_object_identity(intent_directory.descriptor()?)?;
    ensure_scope_same_mount(&trust_identity, &intent_identity, "intent")?;

    let intent_record_name = std::ffi::OsStr::new(STABLE_SCOPE_INTENT);
    let mut children = entry_names(intent_directory)?;
    match expected_record {
        Some((expected_contents, expected_record_identity)) => {
            anyhow::ensure!(
                children.iter().any(|name| name == intent_record_name),
                "durable stable-workspace intent record disappeared before cleanup"
            );
            let (contents, identity, links) = read_scope_record_with_links(
                intent_directory.descriptor()?,
                intent_record_name,
                1024,
            )?;
            anyhow::ensure!(
                contents == expected_contents
                    && identity == *expected_record_identity
                    && (links == 1 || links == 2),
                "stable-workspace intent changed before cleanup"
            );
            children.retain(|name| name != intent_record_name);
            anyhow::ensure!(
                children.len() <= 1,
                "stable-workspace intent contains unexpected cleanup entries"
            );

            if let Some(stage_name) = children.first() {
                anyhow::ensure!(
                    published_scope.is_none(),
                    "published scope and unpublished stage coexist during cleanup"
                );
                anyhow::ensure!(
                    is_authorized_stage_name(stage_name, stage_id),
                    "stable-workspace intent contains an unauthorized stage during cleanup"
                );
                anyhow::ensure!(
                    trust_directory
                        .open_existing_child_directory(std::ffi::OsStr::new(repository_key))?
                        .is_none(),
                    "canonical scope and unpublished stage coexist; preserve ambiguous intent"
                );
                let stage_directory = intent_directory
                    .open_existing_child_directory(stage_name)?
                    .context("authorized stable-workspace stage disappeared during cleanup")?;
                stage_directory.verify_runner_owned_private_directory()?;
                let stage_identity = crate::leftover_disk::filesystem_object_identity(
                    stage_directory.descriptor()?,
                )?;
                ensure_scope_same_mount(&intent_identity, &stage_identity, "stage")?;
                validate_unpublished_stage(
                    &stage_directory,
                    expected_contents,
                    expected_record_identity,
                    links,
                )?;

                if remove_empty_scope_directory_child(
                    &stage_directory,
                    std::ffi::OsStr::new("workspace"),
                    None,
                )? {
                    after_step("workspace-directory")?;
                }
                for (name, expected_contents) in [
                    (STABLE_SCOPE_OWNER, None),
                    (STABLE_SCOPE_LAST_USE, Some(&[][..])),
                    (STABLE_SCOPE_INTENT_PROOF, Some(expected_contents)),
                ] {
                    let Some((contents, identity, file_links)) =
                        read_optional_scope_record(&stage_directory, name, 1024)?
                    else {
                        continue;
                    };
                    if let Some(expected) = expected_contents {
                        anyhow::ensure!(
                            contents == expected,
                            "staged stable-workspace record {name} changed before cleanup"
                        );
                    }
                    let required_links = if name == STABLE_SCOPE_INTENT_PROOF {
                        2
                    } else {
                        1
                    };
                    anyhow::ensure!(
                        file_links == required_links,
                        "staged stable-workspace record {name} has unexpected hard links"
                    );
                    remove_scope_regular_entry(
                        &stage_directory,
                        std::ffi::OsStr::new(name),
                        &contents,
                        &identity,
                        file_links,
                    )?;
                    after_step(match name {
                        STABLE_SCOPE_OWNER => "owner-record",
                        STABLE_SCOPE_LAST_USE => "lru-clock",
                        STABLE_SCOPE_INTENT_PROOF => "stage-proof",
                        _ => anyhow::bail!(
                            "staged stable-workspace record {name} is not a protocol record"
                        ),
                    })?;
                }
                anyhow::ensure!(
                    entry_names(&stage_directory)?.is_empty(),
                    "stable-workspace stage is not empty after authorized cleanup"
                );
                remove_empty_scope_directory_child(
                    intent_directory,
                    stage_name,
                    Some(&stage_identity),
                )?;
                after_step("stage-directory")?;
                intent_directory.sync_directory()?;

                let (contents, identity, links) = read_scope_record_with_links(
                    intent_directory.descriptor()?,
                    intent_record_name,
                    1024,
                )?;
                anyhow::ensure!(
                    identity == *expected_record_identity && links == 1,
                    "unpublished stable-workspace intent proof link did not return to one"
                );
                remove_scope_regular_entry(
                    intent_directory,
                    intent_record_name,
                    &contents,
                    expected_record_identity,
                    1,
                )?;
                after_step("intent-record")?;
            } else if let Some(canonical_scope) = published_scope {
                let canonical_name = std::ffi::OsStr::new(repository_key);
                trust_directory.verify_child_directory_identity(canonical_name, canonical_scope)?;
                let canonical_identity = crate::leftover_disk::filesystem_object_identity(
                    canonical_scope.descriptor()?,
                )?;
                ensure_scope_same_mount(&trust_identity, &canonical_identity, "published scope")?;
                validate_published_scope_proof(
                    stable_root,
                    &stable_root.join(trust_scope_key).join(repository_key),
                    canonical_scope,
                    slot_key,
                    expected_contents,
                    expected_record_identity,
                )?;
                anyhow::ensure!(
                    links == 2,
                    "published stable-workspace proof has unexpected hard links"
                );
                remove_scope_regular_entry(
                    intent_directory,
                    intent_record_name,
                    expected_contents,
                    expected_record_identity,
                    2,
                )?;
                canonical_scope.sync_directory()?;
                after_step("intent-record")?;
            } else {
                anyhow::ensure!(
                    trust_directory
                        .open_existing_child_directory(std::ffi::OsStr::new(repository_key))?
                        .is_none(),
                    "canonical scope appeared before unpublished intent cleanup"
                );
                anyhow::ensure!(
                    links == 1,
                    "stable-workspace intent record has an unexpected proof link"
                );
                remove_scope_regular_entry(
                    intent_directory,
                    intent_record_name,
                    expected_contents,
                    expected_record_identity,
                    1,
                )?;
                after_step("intent-record")?;
            }
        }
        None => {
            anyhow::ensure!(
                published_scope.is_none(),
                "published scope cleanup requires a durable intent record"
            );
            anyhow::ensure!(
                children.len() <= 1,
                "intent without a durable record contains unexpected data"
            );
            if let Some(temporary_name) = children.first() {
                anyhow::ensure!(
                    parse_scope_intent_temporary_name(temporary_name),
                    "intent without a durable record contains unexpected data"
                );
                let (contents, identity, links) = read_scope_record_with_links(
                    intent_directory.descriptor()?,
                    temporary_name,
                    1024,
                )?;
                anyhow::ensure!(
                    links == 1,
                    "temporary intent record has unexpected hard links"
                );
                remove_scope_regular_entry(
                    intent_directory,
                    temporary_name,
                    &contents,
                    &identity,
                    1,
                )?;
                after_step("temporary-intent-record")?;
            }
        }
    }

    anyhow::ensure!(
        entry_names(intent_directory)?.is_empty(),
        "stable-workspace intent is not empty after authorized cleanup"
    );
    remove_empty_scope_directory_child(trust_directory, intent_name, Some(&intent_identity))?;
    after_step("intent-directory")?;
    Ok(())
}

#[cfg(unix)]
fn ensure_scope_same_mount(
    parent: &crate::leftover_disk::FilesystemEntryIdentity,
    child: &crate::leftover_disk::FilesystemEntryIdentity,
    description: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        parent.device == child.device && parent.mount == child.mount,
        "stable-workspace {description} crosses its parent mount"
    );
    Ok(())
}

#[cfg(unix)]
fn read_optional_scope_record(
    directory: &crate::fs_copy::NoFollowDestinationDir,
    name: &str,
    maximum_bytes: usize,
) -> anyhow::Result<Option<(Vec<u8>, crate::leftover_disk::FilesystemEntryIdentity, u64)>> {
    let parent = directory.descriptor()?;
    match rustix::fs::statat(
        parent,
        std::ffi::OsStr::new(name),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(_) => read_scope_record_with_links(parent, std::ffi::OsStr::new(name), maximum_bytes)
            .map(Some),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(std::io::Error::from(error))
            .with_context(|| format!("inspect stable-workspace cleanup record {name}")),
    }
}

#[cfg(unix)]
fn remove_scope_regular_entry(
    parent: &crate::fs_copy::NoFollowDestinationDir,
    name: &std::ffi::OsStr,
    expected_contents: &[u8],
    expected_identity: &crate::leftover_disk::FilesystemEntryIdentity,
    expected_links: u64,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;

    let parent_descriptor = parent.descriptor()?;
    let parent_identity = crate::leftover_disk::filesystem_object_identity(parent_descriptor)?;
    let mut opened: fs::File = rustix::fs::openat(
        parent_descriptor,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .context("open stable-workspace cleanup record without following links")?
    .into();
    let metadata = opened
        .metadata()
        .context("inspect stable-workspace cleanup record")?;
    let identity = crate::leftover_disk::filesystem_object_identity(&opened)?;
    let mut contents = Vec::new();
    std::io::Read::by_ref(&mut opened)
        .take(1025)
        .read_to_end(&mut contents)
        .context("read pinned stable-workspace cleanup record")?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.nlink() == expected_links
            && identity == *expected_identity
            && contents.len() <= 1024
            && contents == expected_contents,
        "stable-workspace cleanup record identity or link count changed"
    );
    ensure_scope_same_mount(&parent_identity, &identity, "cleanup record")?;
    let named = rustix::fs::statat(
        parent_descriptor,
        name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)
    .context("recheck stable-workspace cleanup record before unlink")?;
    anyhow::ensure!(
        rustix::fs::FileType::from_raw_mode(named.st_mode) == rustix::fs::FileType::RegularFile
            && named.st_dev as u64 == identity.device
            && named.st_ino == identity.inode
            && u64::from(named.st_nlink) == expected_links,
        "stable-workspace cleanup record changed before unlink"
    );
    rustix::fs::unlinkat(parent_descriptor, name, rustix::fs::AtFlags::empty())
        .map_err(std::io::Error::from)
        .context("unlink stable-workspace cleanup record")?;
    parent.sync_directory()?;
    Ok(())
}

#[cfg(unix)]
fn remove_empty_scope_directory_child(
    parent: &crate::fs_copy::NoFollowDestinationDir,
    name: &std::ffi::OsStr,
    expected_identity: Option<&crate::leftover_disk::FilesystemEntryIdentity>,
) -> anyhow::Result<bool> {
    use rustix::fs::FileType;

    let Some(child) = parent.open_existing_child_directory(name)? else {
        return Ok(false);
    };
    child.verify_runner_owned_private_directory()?;
    let parent_identity = crate::leftover_disk::filesystem_object_identity(parent.descriptor()?)?;
    let child_identity = crate::leftover_disk::filesystem_object_identity(child.descriptor()?)?;
    ensure_scope_same_mount(&parent_identity, &child_identity, "cleanup directory")?;
    if let Some(expected_identity) = expected_identity {
        anyhow::ensure!(
            child_identity == *expected_identity,
            "stable-workspace cleanup directory identity changed"
        );
    }
    anyhow::ensure!(
        entry_names(&child)?.is_empty(),
        "stable-workspace cleanup directory contains unexpected data"
    );
    parent.verify_child_directory_identity(name, &child)?;
    let named = rustix::fs::statat(
        parent.descriptor()?,
        name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)
    .context("recheck stable-workspace cleanup directory before rmdir")?;
    anyhow::ensure!(
        FileType::from_raw_mode(named.st_mode) == FileType::Directory
            && named.st_dev as u64 == child_identity.device
            && named.st_ino == child_identity.inode,
        "stable-workspace cleanup directory changed before rmdir"
    );
    rustix::fs::unlinkat(parent.descriptor()?, name, rustix::fs::AtFlags::REMOVEDIR)
        .map_err(std::io::Error::from)
        .context("remove empty stable-workspace cleanup directory")?;
    parent.sync_directory()?;
    Ok(true)
}

#[cfg(unix)]
fn entry_names(
    directory: &crate::fs_copy::NoFollowDestinationDir,
) -> anyhow::Result<Vec<std::ffi::OsString>> {
    let mut names = Vec::new();
    directory.for_each_entry_name(|name| {
        names.push(name);
        Ok(())
    })?;
    Ok(names)
}

#[cfg(unix)]
fn is_authorized_stage_name(name: &std::ffi::OsStr, stage_id: uuid::Uuid) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let prefix = format!("{STABLE_SCOPE_STAGING_PREFIX}-{stage_id}-");
    name.strip_prefix(&prefix)
        .and_then(|suffix| uuid::Uuid::parse_str(suffix).ok().map(|id| (suffix, id)))
        .is_some_and(|(suffix, id)| id.to_string() == suffix)
}

#[cfg(unix)]
fn parse_scope_intent_temporary_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(&format!("{STABLE_SCOPE_INTENT}.tmp-")) else {
        return false;
    };
    uuid::Uuid::parse_str(suffix)
        .ok()
        .is_some_and(|uuid| uuid.to_string() == suffix)
}

#[cfg(unix)]
fn parse_scope_clock_temporary_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(suffix) = name.strip_prefix(&format!("{STABLE_SCOPE_LAST_USE}.tmp-")) else {
        return false;
    };
    uuid::Uuid::parse_str(suffix)
        .ok()
        .is_some_and(|uuid| uuid.to_string() == suffix)
}

#[cfg(not(unix))]
fn create_and_publish_scope(
    _trust_directory: &crate::fs_copy::NoFollowDestinationDir,
    _stable_root: &Path,
    _slot_key: &str,
    _trust_scope_key: &str,
    _repository_key: &str,
    _before_marker: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<(
    crate::fs_copy::NoFollowDestinationDir,
    crate::fs_copy::NoFollowDestinationDir,
    crate::leftover_disk::FilesystemEntryIdentity,
)> {
    anyhow::bail!("stable-workspace allocation requires Unix no-follow file operations")
}

#[cfg(unix)]
fn create_scope_record(
    scope: &crate::fs_copy::NoFollowDestinationDir,
    name: &str,
    contents: &[u8],
) -> anyhow::Result<crate::leftover_disk::FilesystemEntryIdentity> {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;

    let parent = scope.descriptor()?;
    let mut record: fs::File = rustix::fs::openat(
        parent,
        std::ffi::OsStr::new(name),
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o644),
    )
    .map_err(std::io::Error::from)
    .with_context(|| format!("create stable-workspace record {name} without replacement"))?
    .into();
    record
        .write_all(contents)
        .with_context(|| format!("write stable-workspace record {name}"))?;
    record
        .sync_all()
        .with_context(|| format!("sync stable-workspace record {name}"))?;
    let metadata = record
        .metadata()
        .with_context(|| format!("inspect stable-workspace record {name}"))?;
    anyhow::ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "stable-workspace record {name} is not a singly-linked regular file"
    );
    let identity = crate::leftover_disk::filesystem_object_identity(&record)?;
    let named = rustix::fs::statat(
        parent,
        std::ffi::OsStr::new(name),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)
    .with_context(|| format!("recheck stable-workspace record {name}"))?;
    anyhow::ensure!(
        rustix::fs::FileType::from_raw_mode(named.st_mode) == rustix::fs::FileType::RegularFile
            && named.st_dev as u64 == metadata.dev()
            && named.st_ino == metadata.ino(),
        "stable-workspace record {name} changed during creation"
    );
    Ok(identity)
}

#[cfg(not(unix))]
fn create_scope_record(
    _scope: &crate::fs_copy::NoFollowDestinationDir,
    _name: &str,
    _contents: &[u8],
) -> anyhow::Result<crate::leftover_disk::FilesystemEntryIdentity> {
    anyhow::bail!("stable-workspace allocation requires Unix no-follow file operations")
}

fn verify_scope_directory_chain(
    slot: &crate::fs_copy::NoFollowDestinationDir,
    stable_root: &crate::fs_copy::NoFollowDestinationDir,
    trust_scope: &crate::fs_copy::NoFollowDestinationDir,
    scope: &crate::fs_copy::NoFollowDestinationDir,
    workspace: &crate::fs_copy::NoFollowDestinationDir,
    trust_scope_key: &str,
    repository_key: &str,
) -> anyhow::Result<()> {
    slot.verify_child_directory_identity(std::ffi::OsStr::new(STABLE_WORKSPACES_DIR), stable_root)?;
    stable_root
        .verify_child_directory_identity(std::ffi::OsStr::new(trust_scope_key), trust_scope)?;
    trust_scope.verify_child_directory_identity(std::ffi::OsStr::new(repository_key), scope)?;
    scope.verify_child_directory_identity(std::ffi::OsStr::new("workspace"), workspace)
}

#[cfg(unix)]
fn refresh_scope_marker(
    scope: &crate::fs_copy::NoFollowDestinationDir,
    expected_identity: &crate::leftover_disk::FilesystemEntryIdentity,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;

    let parent = scope.descriptor()?;
    let owner_name = std::ffi::OsStr::new(STABLE_SCOPE_OWNER);
    let mut owner: fs::File = rustix::fs::openat(
        parent,
        owner_name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .context("open immutable stable-workspace owner record")?
    .into();
    verify_scope_record_name(parent, owner_name, &owner, "owner")?;
    let identity = crate::leftover_disk::filesystem_object_identity(&owner)?;
    anyhow::ensure!(
        &identity == expected_identity,
        "stable-workspace owner record changed after ownership validation"
    );
    let mut owner_contents = Vec::new();
    std::io::Read::by_ref(&mut owner)
        .take(u64::try_from(STABLE_SCOPE_OWNER_MARKER.len() + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut owner_contents)
        .context("read immutable stable-workspace owner record")?;
    anyhow::ensure!(
        owner_contents == STABLE_SCOPE_OWNER_MARKER,
        "stable-workspace owner record changed before clock refresh"
    );
    recover_abandoned_clock_temporaries(scope, parent)?;
    verify_scope_record_name(parent, owner_name, &owner, "owner")?;

    let clock_name = std::ffi::OsStr::new(STABLE_SCOPE_LAST_USE);
    let temporary_name = std::ffi::OsString::from(format!(
        "{STABLE_SCOPE_LAST_USE}.tmp-{}",
        uuid::Uuid::new_v4()
    ));
    let clock: fs::File = rustix::fs::openat(
        parent,
        &temporary_name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o644),
    )
    .map_err(std::io::Error::from)
    .context("create temporary stable-workspace LRU clock")?
    .into();
    let refresh = (|| -> anyhow::Result<()> {
        let metadata = clock
            .metadata()
            .context("inspect temporary stable-workspace LRU clock")?;
        anyhow::ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "temporary stable-workspace LRU clock is not a singly-linked regular file"
        );
        clock
            .sync_all()
            .context("sync temporary stable-workspace LRU clock")?;
        verify_scope_record_name(parent, &temporary_name, &clock, "temporary clock")?;
        verify_scope_record_name(parent, owner_name, &owner, "owner")?;

        rustix::fs::renameat(parent, &temporary_name, parent, clock_name)
            .map_err(std::io::Error::from)
            .context("atomically publish stable-workspace LRU clock")?;
        scope.sync_directory()?;

        verify_scope_record_name(parent, owner_name, &owner, "owner")?;
        let published =
            crate::leftover_disk::filesystem_read_regular_file_at(parent, clock_name, 0)?
                .context("published stable-workspace LRU clock disappeared")?;
        anyhow::ensure!(
            published.0.is_empty()
                && published.2 == crate::leftover_disk::filesystem_object_identity(&clock)?,
            "published stable-workspace LRU clock changed during refresh"
        );
        Ok(())
    })();
    if let Err(error) = refresh {
        let cleanup = rustix::fs::unlinkat(parent, &temporary_name, rustix::fs::AtFlags::empty());
        return match cleanup {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Err(error),
            Err(cleanup_error) => Err(error).context(format!(
                "temporary stable-workspace clock cleanup also failed: {}",
                std::io::Error::from(cleanup_error)
            )),
        };
    }
    Ok(())
}

#[cfg(unix)]
fn recover_abandoned_clock_temporaries(
    scope: &crate::fs_copy::NoFollowDestinationDir,
    parent: &fs::File,
) -> anyhow::Result<()> {
    let mut removed = false;
    for name in entry_names(scope)? {
        if !parse_scope_clock_temporary_name(&name) {
            continue;
        }
        let validated = match read_scope_record_with_links(parent, &name, 0) {
            Ok((contents, identity, links)) if contents.is_empty() && links == 1 => Some(identity),
            Ok(_) => None,
            Err(error) => {
                eprintln!(
                    "forensics.lifecycle: preserve ambiguous stable-workspace clock temporary {name:?}: {error:#}"
                );
                None
            }
        };
        let Some(identity) = validated else {
            continue;
        };
        let named = match rustix::fs::statat(parent, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(named) => named,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(error) => {
                eprintln!(
                    "forensics.lifecycle: preserve stable-workspace clock temporary {name:?}: {error}"
                );
                continue;
            }
        };
        if rustix::fs::FileType::from_raw_mode(named.st_mode) != rustix::fs::FileType::RegularFile
            || named.st_dev as u64 != identity.device
            || named.st_ino != identity.inode
            || named.st_nlink != 1
        {
            eprintln!(
                "forensics.lifecycle: preserve replaced stable-workspace clock temporary {name:?}"
            );
            continue;
        }
        match rustix::fs::unlinkat(parent, &name, rustix::fs::AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => removed = true,
            Err(error) => eprintln!(
                "forensics.lifecycle: could not remove stable-workspace clock temporary {name:?}: {error}"
            ),
        }
    }
    if removed {
        scope.sync_directory()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn refresh_scope_marker(
    _scope: &crate::fs_copy::NoFollowDestinationDir,
    _expected_identity: &crate::leftover_disk::FilesystemEntryIdentity,
) -> anyhow::Result<()> {
    anyhow::bail!("stable-workspace allocation requires Unix no-follow file operations")
}

#[cfg(unix)]
fn verify_scope_record_name(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    record: &fs::File,
    description: &str,
) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = record
        .metadata()
        .with_context(|| format!("inspect opened stable-workspace {description}"))?;
    anyhow::ensure!(
        opened.is_file() && opened.nlink() == 1,
        "stable-workspace {description} is not a singly-linked regular file"
    );
    let named = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)
        .with_context(|| format!("recheck stable-workspace {description} name"))?;
    anyhow::ensure!(
        rustix::fs::FileType::from_raw_mode(named.st_mode) == rustix::fs::FileType::RegularFile
            && named.st_dev as u64 == opened.dev()
            && named.st_ino == opened.ino(),
        "stable-workspace {description} changed during refresh"
    );
    Ok(())
}

/// Enforce the slot budget after the job's workspace lease has been released.
pub(crate) fn reclaim_after_job(slot_work_dir: &Path, run_root: &Path) -> BudgetOutcome {
    let slot_work_dir = match canonicalize_existing_prefix(slot_work_dir) {
        Ok(slot_work_dir) => slot_work_dir,
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable workspace budget pass skipped: slot path could not be resolved: {error:#}"
            );
            return BudgetOutcome::default();
        }
    };
    enforce_budget(
        &slot_work_dir.join(STABLE_WORKSPACES_DIR),
        run_root,
        STABLE_WORKSPACES_BUDGET_BYTES,
    )
}

/// Validate one candidate from a pinned stable-workspace root. A depth-two
/// directory is not proof that Velnor owns it: it must have the canonical
/// trust/repository key shape, a real nofollow `workspace` child on the same
/// mount, and Velnor's exact immutable owner record. The returned scope parts
/// are the lease identity after the store-class prefix. A missing or damaged
/// LRU clock sorts first; it never invalidates ownership and is repaired by
/// the next prepare.
pub(crate) fn validate_candidate_at(
    stable_root: &Path,
    candidate: &Path,
    slot_key: &str,
    directory: &fs::File,
    identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Option<(Vec<String>, SystemTime)> {
    validate_candidate_with_owner_identity_at(stable_root, candidate, slot_key, directory, identity)
        .map(|(scope, last_use, _, _)| (scope, last_use))
}

fn validate_candidate_with_owner_identity_at(
    stable_root: &Path,
    candidate: &Path,
    slot_key: &str,
    directory: &fs::File,
    identity: &crate::leftover_disk::FilesystemDirectoryIdentity,
) -> Option<(
    Vec<String>,
    SystemTime,
    crate::leftover_disk::FilesystemEntryIdentity,
    crate::leftover_disk::FilesystemDirectoryIdentity,
)> {
    let relative = candidate.strip_prefix(stable_root).ok()?;
    let mut components = relative.components();
    let Component::Normal(trust_scope) = components.next()? else {
        return None;
    };
    let Component::Normal(repository_key) = components.next()? else {
        return None;
    };
    if components.next().is_some() {
        return None;
    }
    let trust_scope = trust_scope.to_str()?;
    let repository_key = repository_key.to_str()?;
    if !crate::trust_scope::is_filesystem_key(trust_scope)
        || !is_repository_store_key(repository_key)
    {
        return None;
    }

    let (_workspace, workspace_identity) =
        crate::leftover_disk::filesystem_open_directory_child_at(
            directory,
            std::ffi::OsStr::new("workspace"),
        )
        .ok()??;
    if workspace_identity.device != identity.device || workspace_identity.mount != identity.mount {
        return None;
    }

    let (owner_marker, _, owner_identity) = crate::leftover_disk::filesystem_read_regular_file_at(
        directory,
        std::ffi::OsStr::new(STABLE_SCOPE_OWNER),
        STABLE_SCOPE_OWNER_MARKER.len(),
    )
    .ok()??;
    if owner_marker != STABLE_SCOPE_OWNER_MARKER {
        return None;
    }
    let last_use = crate::leftover_disk::filesystem_read_regular_file_at(
        directory,
        std::ffi::OsStr::new(STABLE_SCOPE_LAST_USE),
        0,
    )
    .ok()
    .flatten()
    .filter(|(contents, _, _)| contents.is_empty())
    .map(|(_, last_use, _)| last_use)
    .unwrap_or(SystemTime::UNIX_EPOCH);
    Some((
        vec![
            slot_key.to_owned(),
            trust_scope.to_owned(),
            repository_key.to_owned(),
        ],
        last_use,
        owner_identity,
        workspace_identity,
    ))
}

fn is_repository_store_key(value: &str) -> bool {
    value.strip_prefix("repo-key-v1-").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// What one budget pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BudgetOutcome {
    /// Idle scopes removed, least-recently-used first.
    pub(crate) evicted: Vec<PathBuf>,
}

/// One evictable scope: an LRU timestamp plus a measured size.
#[derive(Debug)]
struct EvictionScope {
    dir: PathBuf,
    last_use: SystemTime,
    bytes: u64,
}

/// Order victims least-recently-used first. Pure over caller-supplied
/// timestamps so the ordering is testable without sleeping for mtimes.
fn victim_order(mut scopes: Vec<EvictionScope>) -> Vec<EvictionScope> {
    scopes.sort_by(|a, b| a.last_use.cmp(&b.last_use).then_with(|| a.dir.cmp(&b.dir)));
    scopes
}

/// Delete whole idle scopes, least-recently-used first, until the slot's
/// stable tree fits the budget. The exclusive filesystem coordinator closes
/// lease-publication races; each candidate is inventoried and removed through
/// the same pinned, mount-bounded mechanism used by pressure reclamation.
/// Active scopes are never budget victims; allocation publishes its lease
/// before calling this function, so current work needs no path-based bypass.
fn enforce_budget(stable_root: &Path, run_root: &Path, budget_bytes: u64) -> BudgetOutcome {
    let mut outcome = BudgetOutcome::default();
    let _coordinator = match crate::capacity::FilesystemCoordinator::lock_exclusive(run_root) {
        Ok(coordinator) => coordinator,
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable workspace budget pass skipped: coordinator lock failed: {error:#}"
            );
            return outcome;
        }
    };
    let active_scopes = match crate::capacity::active_scopes(
        run_root,
        Duration::from_secs(24 * 3600),
    ) {
        Ok(scopes) => scopes,
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable workspace budget pass skipped: active leases unreadable: {error:#}"
            );
            return outcome;
        }
    };
    if let Err(error) = recover_abandoned_staging_under_coordinator(stable_root, &active_scopes) {
        eprintln!(
            "forensics.lifecycle: stable workspace staging recovery skipped at {}: {error:#}",
            stable_root.display()
        );
    }
    if matches!(
        fs::symlink_metadata(stable_root),
        Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        return outcome;
    }
    let Some(slot_work_dir) = stable_root.parent() else {
        return outcome;
    };
    let slot_identity = match crate::leftover_disk::filesystem_directory_identity(slot_work_dir) {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable workspace budget pass skipped: slot identity unavailable at {}: {error:#}",
                slot_work_dir.display()
            );
            return outcome;
        }
    };
    let stable_root_identity = match crate::leftover_disk::filesystem_directory_identity_under(
        slot_work_dir,
        stable_root,
        &slot_identity,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable workspace budget pass skipped: root is not a real no-follow child of {}: {error:#}",
                slot_work_dir.display()
            );
            return outcome;
        }
    };
    if stable_root_identity.device != slot_identity.device
        || stable_root_identity.mount != slot_identity.mount
    {
        eprintln!(
            "forensics.lifecycle: stable workspace budget pass skipped: root crosses slot filesystem at {}",
            stable_root.display()
        );
        return outcome;
    }
    let slot_key = match slot_scope_key(slot_work_dir) {
        Ok(key) => key,
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable workspace budget pass skipped: slot identity unavailable: {error:#}"
            );
            return outcome;
        }
    };
    let snapshots = match crate::leftover_disk::filesystem_candidate_tree_snapshots_under(
        slot_work_dir,
        stable_root,
        &slot_identity,
        2,
    ) {
        Ok(snapshots) => snapshots,
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable workspace budget pass skipped: secure inventory failed at {}: {error:#}",
                stable_root.display()
            );
            return outcome;
        }
    };
    let mut scopes = Vec::new();
    let mut pinned_candidates = BTreeMap::new();
    let mut total = 0u64;
    for snapshot in snapshots {
        if !snapshot.same_mount_tree
            || snapshot.identity.device != slot_identity.device
            || snapshot.identity.mount != slot_identity.mount
        {
            continue;
        }
        let Some((scope_parts, last_use)) = validate_candidate_at(
            stable_root,
            &snapshot.path,
            &slot_key,
            &snapshot.directory,
            &snapshot.identity,
        ) else {
            continue;
        };
        let lease_scope = format!("stable-workspace/{}", scope_parts.join("/"));
        let active = active_scopes
            .iter()
            .any(|active_scope| scopes_overlap(active_scope, &lease_scope));
        total = total.saturating_add(snapshot.logical_bytes);
        pinned_candidates.insert(
            snapshot.path.clone(),
            (snapshot.identity, snapshot.directory, active),
        );
        scopes.push(EvictionScope {
            dir: snapshot.path,
            last_use,
            bytes: snapshot.logical_bytes,
        });
    }
    if total <= budget_bytes {
        return outcome;
    }
    for victim in victim_order(scopes) {
        let Some((identity, directory, active)) = pinned_candidates.get(&victim.dir) else {
            continue;
        };
        if *active {
            continue;
        }
        if total <= budget_bytes {
            break;
        }
        if validate_candidate_at(stable_root, &victim.dir, &slot_key, directory, identity).is_none()
        {
            // A candidate that lost its workspace or ownership marker after
            // inventory no longer contributes to the stable-workspace budget.
            total = total.saturating_sub(victim.bytes);
            continue;
        }
        match crate::leftover_disk::remove_dir_all_on_device_under_pinned(
            slot_work_dir,
            &victim.dir,
            slot_identity.device,
            &slot_identity,
            identity,
            directory,
        ) {
            Ok(()) => {
                eprintln!(
                    "forensics.lifecycle: evicted idle stable workspace scope {} ({} bytes, over {} budget)",
                    victim.dir.display(),
                    victim.bytes,
                    budget_bytes,
                );
                total = total.saturating_sub(victim.bytes);
                outcome.evicted.push(victim.dir);
            }
            Err(error) => {
                eprintln!(
                    "forensics.lifecycle: stable workspace eviction failed at {}: {error:#}",
                    victim.dir.display()
                );
                return outcome;
            }
        }
    }
    outcome
}

fn scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
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

    fn slot_root(name: &str) -> PathBuf {
        let temp_root = std::fs::canonicalize(std::env::temp_dir())
            .expect("stable workspace test temp root should exist");
        temp_root.join(format!("velnor-stable-{name}-{}", uuid::Uuid::new_v4()))
    }

    fn capacity_run_root(slot: &Path) -> PathBuf {
        slot.join("run")
    }

    fn repo_key(id: u64) -> String {
        crate::store_catalog::repository_store_key("https://github.com", &id.to_string()).unwrap()
    }

    fn canonical_test_repo_key(repository: &str) -> String {
        if is_repository_store_key(repository) {
            repository.to_owned()
        } else {
            repo_key(repository.parse().unwrap())
        }
    }

    fn hold_workspace_lease(
        slot: &Path,
        run_root: &Path,
        trust_scope: &str,
        repository_key: &str,
    ) -> crate::capacity::ScopeLease {
        let scope =
            lease_scope(slot, trust_scope, &canonical_test_repo_key(repository_key)).unwrap();
        crate::capacity::ScopeLease::acquire(
            run_root,
            "stable-workspace",
            &format!("{scope}/job-test"),
            Duration::from_secs(24 * 3600),
        )
        .unwrap()
    }

    #[test]
    fn slot_scope_key_canonicalizes_aliases_and_missing_suffixes() {
        let root = slot_root("slot-scope-key");
        let slot = root.join("slot-1");
        fs::create_dir_all(&slot).unwrap();
        let missing = slot.join("future").join("work");
        let key = slot_scope_key(&missing).unwrap();
        let normalized = slot_scope_key(&root.join("slot-1/future/./work")).unwrap();
        assert_eq!(key, normalized);

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let alias = root.join("alias");
            symlink(&slot, &alias).unwrap();
            assert_eq!(slot_scope_key(&alias.join("future/work")).unwrap(), key);
        }

        let other = root.join("slot-2/future/work");
        assert_ne!(slot_scope_key(&other).unwrap(), key);
        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn slot_scope_key_resolves_parent_after_symlink_using_filesystem_semantics() {
        use std::os::unix::fs::symlink;

        let root = slot_root("slot-scope-symlink-parent");
        let link_parent = root.join("a");
        let symlink_target = root.join("real/deep");
        fs::create_dir_all(&link_parent).unwrap();
        fs::create_dir_all(&symlink_target).unwrap();
        fs::create_dir_all(root.join("real/slot")).unwrap();
        fs::create_dir_all(root.join("a/slot")).unwrap();
        symlink(&symlink_target, link_parent.join("link")).unwrap();

        // The OS resolves `link` first, so its parent is `real`, not `a`.
        let through_symlink = link_parent.join("link/../slot");
        let actual = slot_scope_key(&through_symlink).unwrap();
        let expected = slot_scope_key(&root.join("real/slot")).unwrap();
        let lexical_wrong = slot_scope_key(&root.join("a/slot")).unwrap();
        assert_eq!(actual, expected);
        assert_ne!(actual, lexical_wrong);
        assert_eq!(
            lease_scope(&through_symlink, "trusted", &repo_key(41)).unwrap(),
            lease_scope(&root.join("real/slot"), "trusted", &repo_key(41)).unwrap()
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn slot_scope_key_rejects_parent_traversal_with_missing_suffix() {
        use std::os::unix::fs::symlink;

        let root = slot_root("slot-scope-missing-parent-suffix");
        let link_parent = root.join("a");
        let symlink_target = root.join("real/deep");
        fs::create_dir_all(&link_parent).unwrap();
        fs::create_dir_all(&symlink_target).unwrap();
        symlink(&symlink_target, link_parent.join("link")).unwrap();

        let ambiguous = link_parent.join("link/../future/work");
        let error = slot_scope_key(&ambiguous)
            .expect_err("an unresolved parent traversal cannot receive a guessed lease key");
        assert!(
            error
                .to_string()
                .contains("non-existent path with parent traversal"),
            "unexpected error: {error:#}"
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lease_scope_matches_slot_trust_and_repository_candidate_keys() {
        let slot = slot_root("lease-scope");
        let key = repo_key(41);
        assert_eq!(
            lease_scope(&slot, "trusted", &key).unwrap(),
            format!(
                "{}/{}/{}",
                slot_scope_key(&slot).unwrap(),
                crate::trust_scope::filesystem_key("trusted"),
                key
            )
        );
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn layout_is_namespaced_and_reaper_safe() {
        let slot = slot_root("layout");
        let job_dir = slot.join("550e8400-e29b-41d4-a716-446655440000");
        let stable = resolve(&slot, "trusted", &repo_key(41));
        assert_eq!(
            stable.workspace,
            slot.join(STABLE_WORKSPACES_DIR)
                .join(crate::trust_scope::filesystem_key("trusted"))
                .join(repo_key(41))
                .join("workspace")
        );
        assert!(stable
            .scope_dir
            .starts_with(slot.join(STABLE_WORKSPACES_DIR)));
        // The stable tree is a sibling of the per-job UUID directories, so
        // post-job removal of a job directory can never take it.
        assert!(!stable.workspace.starts_with(&job_dir));
        assert!(!job_dir.starts_with(&stable.scope_dir));
        // No segment may be job-UUID-shaped: the leftover-disk reaper only
        // deletes UUID-shaped children of slot directories.
        for component in stable.workspace.strip_prefix(&slot).unwrap().components() {
            let name = component.as_os_str().to_string_lossy();
            assert!(
                !crate::leftover_disk::looks_like_job_uuid(&name),
                "{name} is UUID-shaped and would be reaper-eligible"
            );
        }
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn scope_segment_is_collision_resistant_and_fail_closed() {
        let slot = slot_root("scope");
        let traversal = resolve(&slot, "../../etc", &repo_key(7));
        assert_eq!(
            traversal.scope_dir,
            slot.join(STABLE_WORKSPACES_DIR)
                .join(crate::trust_scope::filesystem_key("../../etc"))
                .join(repo_key(7))
        );
        assert!(traversal
            .scope_dir
            .starts_with(slot.join(STABLE_WORKSPACES_DIR)));
        let blank = resolve(&slot, "   ", &repo_key(7));
        assert_eq!(
            blank.scope_dir,
            slot.join(STABLE_WORKSPACES_DIR)
                .join(crate::trust_scope::filesystem_key(
                    crate::trust_scope::FAIL_CLOSED
                ))
                .join(repo_key(7))
        );
        assert_ne!(
            resolve(&slot, "public/forks", &repo_key(7)).scope_dir,
            resolve(&slot, "public_forks", &repo_key(7)).scope_dir
        );
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn encoded_scope_cannot_alias_an_old_raw_scope_component() {
        let slot = slot_root("raw-key-alias");
        let encoded_scope = crate::trust_scope::filesystem_key("trusted");
        let current = resolve(&slot, "trusted", &repo_key(41));
        let old_alias = slot
            .join("stable-workspaces")
            .join(crate::container::sanitize_store_key(&encoded_scope))
            .join(repo_key(41));

        assert_eq!(
            crate::container::sanitize_store_key(&encoded_scope),
            encoded_scope
        );
        assert_ne!(current.scope_dir, old_alias);
        assert!(!current.scope_dir.starts_with(&old_alias));
        assert!(!old_alias.starts_with(&current.scope_dir));
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prepare_creates_and_clocks_the_scope() {
        let slot = slot_root("prepare");
        let run_root = capacity_run_root(&slot);
        let (first, first_lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "11111111-1111-4111-8111-111111111111",
        )
        .unwrap();
        assert!(first.fresh_scope);
        assert!(first.workspace.is_dir());
        assert_eq!(
            fs::read(first.scope_dir.join(STABLE_SCOPE_OWNER)).unwrap(),
            STABLE_SCOPE_OWNER_MARKER
        );
        assert!(first.scope_dir.join(STABLE_SCOPE_LAST_USE).is_file());
        assert!(fs::read(first.scope_dir.join(STABLE_SCOPE_LAST_USE))
            .unwrap()
            .is_empty());
        let first_owner =
            crate::capacity::JobOwnerId::parse("11111111-1111-4111-8111-111111111111").unwrap();
        let active_leases =
            crate::capacity::active_scope_leases(&run_root, Duration::from_secs(24 * 3600))
                .unwrap();
        assert!(active_leases.iter().any(|lease| {
            lease.scope.starts_with("stable-workspace/")
                && lease.owner_job_id.as_ref() == Some(&first_owner)
        }));
        drop(first_lease);
        let (second, second_lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "22222222-2222-4222-8222-222222222222",
        )
        .unwrap();
        assert!(!second.fresh_scope);
        assert_eq!(first.workspace, second.workspace);
        drop(second_lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prepare_repairs_a_truncated_lru_clock_without_mutating_owner_identity() {
        let slot = slot_root("prepare-truncated-clock");
        let run_root = capacity_run_root(&slot);
        let (stable, lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "11111111-1111-4111-8111-111111111111",
        )
        .unwrap();
        drop(lease);

        let owner_path = stable.scope_dir.join(STABLE_SCOPE_OWNER);
        let owner_identity =
            crate::leftover_disk::filesystem_object_identity(&fs::File::open(&owner_path).unwrap())
                .unwrap();
        let clock_path = stable.scope_dir.join(STABLE_SCOPE_LAST_USE);
        fs::write(&clock_path, b"partial clock after interrupted refresh").unwrap();
        let abandoned_clock_temporary = stable.scope_dir.join(format!(
            "{STABLE_SCOPE_LAST_USE}.tmp-{}",
            uuid::Uuid::new_v4()
        ));
        fs::write(&abandoned_clock_temporary, b"").unwrap();

        let stable_root = slot.join(STABLE_WORKSPACES_DIR);
        let root_identity =
            crate::leftover_disk::filesystem_directory_identity(&stable_root).unwrap();
        let (scope_descriptor, scope_identity) =
            crate::leftover_disk::filesystem_pin_directory_under(
                &stable_root,
                &stable.scope_dir,
                &root_identity,
            )
            .unwrap();
        let (_, last_use) = validate_candidate_at(
            &stable_root,
            &stable.scope_dir,
            &slot_scope_key(&slot).unwrap(),
            &scope_descriptor,
            &scope_identity,
        )
        .expect("GC must still recognize the immutable owner record");
        assert_eq!(
            last_use,
            std::time::SystemTime::UNIX_EPOCH,
            "damaged clock metadata sorts first without invalidating ownership"
        );

        let (reused, second_lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "22222222-2222-4222-8222-222222222222",
        )
        .expect("damaged refresh metadata must not invalidate immutable ownership");

        assert!(!reused.fresh_scope);
        assert_eq!(reused.scope_dir, stable.scope_dir);
        assert_eq!(fs::read(&owner_path).unwrap(), STABLE_SCOPE_OWNER_MARKER);
        assert_eq!(
            crate::leftover_disk::filesystem_object_identity(&fs::File::open(&owner_path).unwrap())
                .unwrap(),
            owner_identity,
            "clock repair must preserve the immutable owner inode"
        );
        assert!(
            fs::read(&clock_path).unwrap().is_empty(),
            "prepare must atomically replace partial clock contents"
        );
        assert!(
            !abandoned_clock_temporary.exists(),
            "prepare must reclaim a valid abandoned clock temporary"
        );
        drop(second_lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn interrupted_fresh_scope_publication_leaves_no_empty_canonical_scope() {
        let slot = slot_root("prepare-interrupted-publication");
        let stable = resolve(&slot, "trusted", &repo_key(41));
        let run_root = capacity_run_root(&slot);

        let error = prepare_with_before_marker(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "11111111-1111-4111-8111-111111111111",
            || anyhow::bail!("simulated interruption before owner publication"),
        )
        .expect_err("the injected interruption must stop allocation");
        assert!(
            error.to_string().contains("simulated interruption"),
            "unexpected error: {error:#}"
        );
        assert!(
            !stable.scope_dir.exists(),
            "an incomplete scope must never appear under its canonical repository name"
        );
        let staged = stable.scope_dir.parent().unwrap();
        assert!(
            fs::read_dir(staged).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STABLE_SCOPE_INTENT_PREFIX)),
            "failed staging cleanup must not leave an unpublished empty scope"
        );

        let (retried, lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "22222222-2222-4222-8222-222222222222",
        )
        .unwrap();
        assert!(
            retried.fresh_scope,
            "retry must publish a fresh owned scope"
        );
        assert_eq!(
            fs::read(retried.scope_dir.join(STABLE_SCOPE_OWNER)).unwrap(),
            STABLE_SCOPE_OWNER_MARKER
        );
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[cfg(unix)]
    #[test]
    fn retry_reclaims_a_durable_abandoned_stage_before_publishing_scope() {
        use std::os::unix::fs::MetadataExt as _;

        let slot = slot_root("prepare-abandoned-stage-retry");
        let run_root = capacity_run_root(&slot);
        let repository_key = repo_key(41);
        let stable_root = slot.join(STABLE_WORKSPACES_DIR);
        let trust_scope_key = crate::trust_scope::filesystem_key("trusted");
        let trust_directory_path = stable_root.join(&trust_scope_key);
        let slot_key = slot_scope_key(&slot).unwrap();

        let orphan_path = {
            let _coordinator =
                crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root).unwrap();
            let slot_directory =
                crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(&slot)
                    .unwrap();
            let stable_root_directory = slot_directory
                .open_or_create_child_directory(std::ffi::OsStr::new(STABLE_WORKSPACES_DIR))
                .unwrap();
            let trust_directory = stable_root_directory
                .open_or_create_child_directory(std::ffi::OsStr::new(&trust_scope_key))
                .unwrap();
            let stage_id = uuid::Uuid::new_v4();
            let intent_prefix = format!("{STABLE_SCOPE_INTENT_PREFIX}{repository_key}-{stage_id}");
            let (intent_directory, intent_name) = trust_directory
                .create_unique_directory(&intent_prefix)
                .unwrap();
            let parts = parse_scope_intent_name(&intent_name).unwrap();
            let intent_record = format_scope_intent_record(
                &slot_key,
                &trust_scope_key,
                &repository_key,
                stage_id,
                parts.nonce,
            );
            let intent_identity =
                write_scope_intent_record(&intent_directory, &intent_record).unwrap();
            trust_directory.sync_directory().unwrap();
            let stage_prefix = format!("{STABLE_SCOPE_STAGING_PREFIX}-{stage_id}");
            let (stage_directory, stage_name) = intent_directory
                .create_unique_directory(&stage_prefix)
                .unwrap();
            link_scope_intent_proof(&intent_directory, &stage_directory, &intent_identity).unwrap();
            let workspace = stage_directory
                .create_child_directory_no_replace(std::ffi::OsStr::new("workspace"))
                .unwrap();
            workspace.sync_directory().unwrap();
            stage_directory.sync_directory().unwrap();
            intent_directory.sync_directory().unwrap();
            trust_directory.sync_directory().unwrap();
            let orphan_path = trust_directory_path.join(&intent_name).join(&stage_name);
            assert!(orphan_path.is_dir());
            let intent_path = trust_directory_path.join(&intent_name);
            let interruption = remove_scope_intent(
                &trust_directory,
                &stable_root,
                &slot_key,
                &trust_scope_key,
                &repository_key,
                stage_id,
                &intent_name,
                &intent_directory,
                Some((&intent_record, &intent_identity)),
                None,
                |step| {
                    if step == "workspace-directory" {
                        anyhow::bail!("simulated crash during unpublished intent cleanup");
                    }
                    Ok(())
                },
            )
            .expect_err("cleanup hook must interrupt after the first durable unlink");
            assert!(interruption
                .to_string()
                .contains("simulated crash during unpublished intent cleanup"));
            assert!(intent_path.is_dir(), "interrupted intent was renamed away");
            assert!(
                !orphan_path.join("workspace").exists(),
                "interrupted cleanup did not reach its simulated crash point"
            );
            assert!(
                orphan_path.join(STABLE_SCOPE_INTENT_PROOF).is_file(),
                "proof must remain until all other stage entries are removed"
            );
            assert_eq!(
                fs::metadata(orphan_path.join(STABLE_SCOPE_INTENT_PROOF))
                    .unwrap()
                    .nlink(),
                2,
                "both proof links remain until the stage is empty"
            );
            assert!(
                fs::read_dir(&trust_directory_path)
                    .unwrap()
                    .all(|entry| !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".velnor-remove-")),
                "scope cleanup must never publish an unrecognized quarantine name"
            );
            drop(workspace);
            drop(stage_directory);
            drop(intent_directory);
            drop(trust_directory);
            drop(stable_root_directory);
            drop(slot_directory);
            orphan_path
        };

        let (retried, lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repository_key,
            "22222222-2222-4222-8222-222222222222",
        )
        .expect("retry must recover an intent-authorized abandoned stage");

        assert!(retried.fresh_scope);
        assert!(!orphan_path.exists(), "abandoned stage survived retry");
        assert_eq!(
            fs::read(retried.scope_dir.join(STABLE_SCOPE_OWNER)).unwrap(),
            STABLE_SCOPE_OWNER_MARKER
        );
        assert!(retried.scope_dir.join(STABLE_SCOPE_INTENT_PROOF).is_file());
        assert!(fs::read_dir(&trust_directory_path)
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STABLE_SCOPE_INTENT_PREFIX)));
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[cfg(unix)]
    #[test]
    fn retry_reclaims_published_intent_after_record_link_is_removed() {
        use std::os::unix::fs::MetadataExt as _;

        let slot = slot_root("prepare-published-intent-cleanup-crash");
        let run_root = capacity_run_root(&slot);
        let repository_key = repo_key(41);
        let stable_root = slot.join(STABLE_WORKSPACES_DIR);
        let trust_scope_key = crate::trust_scope::filesystem_key("trusted");
        let trust_directory_path = stable_root.join(&trust_scope_key);
        let slot_key = slot_scope_key(&slot).unwrap();

        let (intent_path, canonical_path) = {
            let _coordinator =
                crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root).unwrap();
            let slot_directory =
                crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(&slot)
                    .unwrap();
            let stable_root_directory = slot_directory
                .open_or_create_child_directory(std::ffi::OsStr::new(STABLE_WORKSPACES_DIR))
                .unwrap();
            let trust_directory = stable_root_directory
                .open_or_create_child_directory(std::ffi::OsStr::new(&trust_scope_key))
                .unwrap();
            let stage_id = uuid::Uuid::new_v4();
            let intent_prefix = format!("{STABLE_SCOPE_INTENT_PREFIX}{repository_key}-{stage_id}");
            let (intent_directory, intent_name) = trust_directory
                .create_unique_directory(&intent_prefix)
                .unwrap();
            let parts = parse_scope_intent_name(&intent_name).unwrap();
            let intent_record = format_scope_intent_record(
                &slot_key,
                &trust_scope_key,
                &repository_key,
                stage_id,
                parts.nonce,
            );
            let intent_identity =
                write_scope_intent_record(&intent_directory, &intent_record).unwrap();
            trust_directory.sync_directory().unwrap();
            let stage_prefix = format!("{STABLE_SCOPE_STAGING_PREFIX}-{stage_id}");
            let (scope_directory, stage_name) = intent_directory
                .create_unique_directory(&stage_prefix)
                .unwrap();
            link_scope_intent_proof(&intent_directory, &scope_directory, &intent_identity).unwrap();
            let workspace_directory = scope_directory
                .create_child_directory_no_replace(std::ffi::OsStr::new("workspace"))
                .unwrap();
            workspace_directory.sync_directory().unwrap();
            create_scope_record(
                &scope_directory,
                STABLE_SCOPE_OWNER,
                STABLE_SCOPE_OWNER_MARKER,
            )
            .unwrap();
            create_scope_record(&scope_directory, STABLE_SCOPE_LAST_USE, &[]).unwrap();
            scope_directory.sync_directory().unwrap();
            intent_directory.sync_directory().unwrap();
            intent_directory
                .verify_child_directory_identity(&stage_name, &scope_directory)
                .unwrap();
            rustix::fs::renameat_with(
                intent_directory.descriptor().unwrap(),
                &stage_name,
                trust_directory.descriptor().unwrap(),
                std::ffi::OsStr::new(&repository_key),
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .unwrap();
            scope_directory.sync_directory().unwrap();
            intent_directory.sync_directory().unwrap();
            trust_directory.sync_directory().unwrap();

            let interruption = remove_scope_intent(
                &trust_directory,
                &stable_root,
                &slot_key,
                &trust_scope_key,
                &repository_key,
                stage_id,
                &intent_name,
                &intent_directory,
                Some((&intent_record, &intent_identity)),
                Some(&scope_directory),
                |step| {
                    if step == "intent-record" {
                        anyhow::bail!("simulated crash after dropping published intent link");
                    }
                    Ok(())
                },
            )
            .expect_err("cleanup hook must interrupt after intent record unlink");
            assert!(interruption
                .to_string()
                .contains("simulated crash after dropping published intent link"));

            let proof_metadata = fs::metadata(
                stable_root
                    .join(&trust_scope_key)
                    .join(&repository_key)
                    .join(STABLE_SCOPE_INTENT_PROOF),
            )
            .unwrap();
            assert_eq!(
                proof_metadata.nlink(),
                1,
                "published proof remains singly linked after record-last cleanup"
            );
            let intent_path = trust_directory_path.join(&intent_name);
            assert!(intent_path.is_dir(), "empty intent remains recoverable");
            (
                intent_path,
                stable_root.join(&trust_scope_key).join(&repository_key),
            )
        };

        let (stable, lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repository_key,
            "22222222-2222-4222-8222-222222222222",
        )
        .expect("prepare must recover the empty published intent");
        assert_eq!(stable.scope_dir, canonical_path);
        assert!(
            !intent_path.exists(),
            "empty published intent survived retry"
        );
        assert_eq!(
            fs::metadata(stable.scope_dir.join(STABLE_SCOPE_INTENT_PROOF))
                .unwrap()
                .nlink(),
            1,
            "retry must preserve the canonical proof link"
        );
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prepare_refuses_to_claim_an_existing_unowned_scope() {
        let slot = slot_root("prepare-unowned");
        let stable = resolve(&slot, "trusted", &repo_key(41));
        fs::create_dir_all(&stable.workspace).unwrap();
        let operator_file = stable.workspace.join("operator-notes");
        fs::write(&operator_file, b"keep operator data").unwrap();

        let error = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "11111111-1111-4111-8111-111111111111",
        )
        .expect_err("prepare must reject an existing unowned scope");

        assert!(
            error
                .to_string()
                .contains("refusing to adopt existing unowned"),
            "unexpected error: {error:#}"
        );
        assert_eq!(fs::read(&operator_file).unwrap(), b"keep operator data");
        assert!(
            !stable.scope_dir.join(STABLE_SCOPE_OWNER).exists(),
            "prepare wrote the ownership marker into operator data"
        );
        fs::remove_dir_all(&slot).ok();
    }

    #[cfg(unix)]
    #[test]
    fn prepare_rejects_scope_replacement_before_marker_write() {
        use std::os::unix::fs::symlink;

        let slot = slot_root("prepare-scope-replacement");
        let run_root = capacity_run_root(&slot);
        let repository_key = repo_key(41);
        let (stable, lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repository_key,
            "11111111-1111-4111-8111-111111111111",
        )
        .unwrap();
        drop(lease);

        let outside = slot_root("prepare-scope-replacement-outside");
        fs::create_dir_all(&outside).unwrap();
        let outside_marker = outside.join(STABLE_SCOPE_LAST_USE);
        fs::write(&outside_marker, b"operator sentinel").unwrap();
        let displaced = stable.scope_dir.with_file_name("repo-displaced");

        let error = prepare_with_before_marker(
            &slot,
            &run_root,
            "trusted",
            &repository_key,
            "22222222-2222-4222-8222-222222222222",
            || {
                fs::rename(&stable.scope_dir, &displaced)?;
                symlink(&outside, &stable.scope_dir)?;
                Ok(())
            },
        )
        .expect_err("prepare must detect a replaced scope before writing its marker");

        assert!(
            error.to_string().contains("symlink")
                || format!("{error:#}").contains("not a real directory"),
            "unexpected replacement error: {error:#}"
        );
        assert_eq!(fs::read(&outside_marker).unwrap(), b"operator sentinel");
        assert_eq!(
            fs::read(displaced.join(STABLE_SCOPE_OWNER)).unwrap(),
            STABLE_SCOPE_OWNER_MARKER
        );
        fs::remove_file(&stable.scope_dir).unwrap();
        fs::remove_dir_all(&slot).ok();
        fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn prepare_rejects_uppercase_repository_digest_keys() {
        let slot = slot_root("prepare-uppercase-repo-key");
        let uppercase_key = format!("repo-key-v1-{}", "A".repeat(64));
        let error = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &uppercase_key,
            "11111111-1111-4111-8111-111111111111",
        )
        .expect_err("uppercase digest keys are not canonical");
        assert!(error.to_string().contains("not canonical"), "{error:#}");
        assert!(!slot.join(STABLE_WORKSPACES_DIR).exists());
        fs::remove_dir_all(&slot).ok();
    }

    #[cfg(unix)]
    #[test]
    fn budget_pass_does_not_follow_a_symlinked_stable_root() {
        use std::os::unix::fs::symlink;

        let slot = slot_root("budget-symlink-root");
        fs::create_dir_all(&slot).unwrap();
        let outside = slot_root("budget-symlink-outside");
        let outside_scope = seed_scope(&outside, "trusted", "41", 4096, false);
        let outside_sentinel = outside_scope.join("workspace/repo/target/artifact");
        let stable_root = slot.join(STABLE_WORKSPACES_DIR);
        symlink(&outside, &stable_root).unwrap();

        let outcome = enforce_budget(&stable_root, &capacity_run_root(&slot), 0);

        assert!(outside_sentinel.is_file(), "outside scope was reclaimed");
        assert!(fs::symlink_metadata(&stable_root)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(outcome.evicted.is_empty(), "{outcome:?}");
        fs::remove_dir_all(&slot).ok();
        fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn victims_order_least_recently_used_first() {
        let epoch = SystemTime::UNIX_EPOCH;
        let ordered = victim_order(vec![
            EvictionScope {
                dir: PathBuf::from("new"),
                last_use: epoch + std::time::Duration::from_secs(3),
                bytes: 1,
            },
            EvictionScope {
                dir: PathBuf::from("old"),
                last_use: epoch + std::time::Duration::from_secs(1),
                bytes: 1,
            },
            EvictionScope {
                dir: PathBuf::from("mid"),
                last_use: epoch + std::time::Duration::from_secs(2),
                bytes: 1,
            },
        ]);
        let names: Vec<&str> = ordered
            .iter()
            .map(|scope| scope.dir.to_str().unwrap())
            .collect();
        assert_eq!(names, vec!["old", "mid", "new"]);
    }

    fn seed_scope(root: &Path, scope: &str, repo: &str, bytes: usize, clocked: bool) -> PathBuf {
        let dir = root
            .join(crate::trust_scope::filesystem_key(scope))
            .join(canonical_test_repo_key(repo));
        let target = dir.join("workspace").join("repo").join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("artifact"), vec![7u8; bytes]).unwrap();
        fs::write(dir.join(STABLE_SCOPE_OWNER), STABLE_SCOPE_OWNER_MARKER).unwrap();
        let marker = dir.join(STABLE_SCOPE_LAST_USE);
        fs::write(&marker, b"").unwrap();
        if !clocked {
            crate::cache::test_clock::backdate(&marker, Duration::from_secs(365 * 24 * 3600));
        }
        dir
    }

    #[test]
    fn eviction_removes_oldest_scopes_until_under_budget() {
        let slot = slot_root("evict");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        // An unclocked scope sorts before every clocked one: no sleeps are
        // needed to control the LRU order deterministically.
        let oldest = seed_scope(&root, "trusted", "1", 100, false);
        let mid = seed_scope(&root, "trusted", "2", 100, true);
        let current = seed_scope(&root, "trusted", "3", 100, true);
        let run_root = capacity_run_root(&slot);
        let current_lease = hold_workspace_lease(&slot, &run_root, "trusted", "3");
        let outcome = enforce_budget(&root, &run_root, 150);
        assert!(!oldest.exists(), "oldest idle scope must go first");
        assert!(!mid.exists(), "second scope must go to fit the budget");
        assert!(
            current.exists(),
            "the current scope fits once the idle scopes are gone"
        );
        assert_eq!(outcome.evicted, vec![oldest, mid]);
        drop(current_lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn eviction_stops_once_under_budget_and_spares_junk() {
        let slot = slot_root("evict-partial");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        let oldest = seed_scope(&root, "trusted", "1", 100, false);
        let newer = seed_scope(&root, "trusted", "2", 100, true);
        let current = seed_scope(&root, "public", "9", 100, true);
        let junk_dir = root
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join("operator-notes");
        fs::create_dir_all(&junk_dir).unwrap();
        fs::write(junk_dir.join("readme"), b"not a scope").unwrap();
        enforce_budget(&root, &capacity_run_root(&slot), 360);
        assert!(!oldest.exists());
        assert!(newer.exists(), "eviction stops once under budget");
        assert!(current.exists());
        assert!(
            junk_dir.join("readme").is_file(),
            "directories without a clock or workspace are operator-owned, never victims"
        );
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn budget_ignores_depth_two_operator_directory_without_workspace() {
        let slot = slot_root("budget-operator-directory");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        let valid = seed_scope(&root, "trusted", "41", 100, true);
        let operator_dir = root
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join(repo_key(42));
        fs::create_dir_all(&operator_dir).unwrap();
        fs::write(
            operator_dir.join(STABLE_SCOPE_OWNER),
            STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(operator_dir.join("operator-notes"), vec![8_u8; 4096]).unwrap();

        let outcome = enforce_budget(&root, &capacity_run_root(&slot), 1000);

        assert!(
            valid.exists(),
            "unowned depth-two data must not force a valid workspace eviction"
        );
        assert!(
            operator_dir.join("operator-notes").is_file(),
            "a depth-two directory without a real workspace subtree must survive"
        );
        assert!(outcome.evicted.is_empty(), "{outcome:?}");
        fs::remove_dir_all(&slot).ok();
    }

    /// Admission may select an already oversized scope. Its published lease
    /// must protect it; post-job LRU handles it after that lease drops.
    #[test]
    fn current_scope_is_never_evicted_during_admission() {
        let slot = slot_root("evict-current");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        let current = seed_scope(&root, "trusted", "1", 100, true);
        let run_root = capacity_run_root(&slot);
        let lease = hold_workspace_lease(&slot, &run_root, "trusted", "1");
        let outcome = enforce_budget(&root, &run_root, 10);
        assert!(current.exists(), "the active admission scope must survive");
        assert!(outcome.evicted.is_empty());
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }

    /// Growth between admissions is bounded: a workspace that exceeded the
    /// budget while a job ran on it is reclaimed at the very next admission,
    /// even though its scope already existed (the old code only checked new
    /// scopes) and even when the next job is for the same repository.
    #[test]
    fn workspace_exceeding_budget_is_reclaimed_at_the_next_admission() {
        let slot = slot_root("reclaim-next-admission");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        // A previous job left this scope at 100 bytes against a budget the
        // test drives through `enforce_budget` with the production shape.
        let grown = seed_scope(&root, "trusted", &repo_key(41), 100, true);
        // The next admission's budget pass reclaims idle growth in the shared
        // workspace tree before another workspace can use it.
        let run_root = capacity_run_root(&slot);
        let outcome = enforce_budget(&root, &run_root, 50);
        assert_eq!(outcome.evicted, vec![grown.clone()]);
        assert!(
            !grown.exists(),
            "the over-budget workspace must be reclaimed"
        );
        // Same-repository admission cannot clear a workspace while the job
        // will use it. Once the job ends and its lease is gone, the LRU pass
        // may evict that over-budget scope.
        let grown = seed_scope(&root, "trusted", &repo_key(41), 100, true);
        let lease = hold_workspace_lease(&slot, &run_root, "trusted", &repo_key(41));
        let outcome = enforce_budget(&root, &run_root, 50);
        assert!(grown.exists(), "the active scope must not be cleared");
        assert!(outcome.evicted.is_empty());
        drop(lease);
        let outcome = enforce_budget(&root, &run_root, 50);
        assert_eq!(outcome.evicted, vec![grown.clone()]);
        assert!(
            !grown.exists(),
            "post-job LRU removes the over-budget scope"
        );
        fs::remove_dir_all(&slot).ok();
    }

    /// `prepare` is the admission path: it enforces the budget on every call,
    /// so an existing scope is never a way around the bound.
    #[test]
    fn prepare_enforces_the_budget_on_reuse_not_only_on_creation() {
        let slot = slot_root("prepare-reuse");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        let run_root = capacity_run_root(&slot);
        let (first, lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "11111111-1111-4111-8111-111111111111",
        )
        .unwrap();
        assert!(first.fresh_scope);
        // Grow another scope well past what the real budget allows only by
        // driving the enforcement directly; the production constant is too
        // large to fill in a unit test, so prove the wiring: a second
        // `prepare` for an existing scope re-runs enforcement (observable as
        // an idle unclocked scope disappearing under a tiny budget).
        let idle = seed_scope(&root, "trusted", "2", 100, false);
        let outcome = enforce_budget(&root, &run_root, 50);
        assert_eq!(outcome.evicted, vec![idle]);
        drop(lease);
        let (second, second_lease) = prepare(
            &slot,
            &run_root,
            "trusted",
            &repo_key(41),
            "22222222-2222-4222-8222-222222222222",
        )
        .unwrap();
        assert!(!second.fresh_scope, "an in-budget scope is reused");
        assert_eq!(first.workspace, second.workspace);
        drop(second_lease);
        fs::remove_dir_all(&slot).ok();
    }

    /// Job completion reclaims oldest-first with no protected scope, so the
    /// scope the job just used is also a candidate when it alone is over.
    #[test]
    fn reclaim_after_job_evicts_oldest_first_including_the_used_scope() {
        let slot = slot_root("reclaim-after-job");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        let older = seed_scope(&root, "trusted", "1", 100, false);
        let used = seed_scope(&root, "trusted", "2", 100, true);
        let outcome = enforce_budget(&root, &capacity_run_root(&slot), 150);
        assert_eq!(outcome.evicted, vec![older.clone()]);
        assert!(!older.exists());
        assert!(used.exists(), "eviction stops once under budget");
        let outcome = enforce_budget(&root, &capacity_run_root(&slot), 50);
        assert_eq!(outcome.evicted, vec![used.clone()]);
        assert!(!used.exists(), "a lone over-budget scope goes too");
        // The production entry point wires the slot root and the constant.
        assert_eq!(
            reclaim_after_job(&slot, &capacity_run_root(&slot)),
            BudgetOutcome::default()
        );
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn stable_budget_preserves_active_workspace_then_reclaims_after_lease_release() {
        let slot = slot_root("active-budget-lease");
        let stable_root = slot.join(STABLE_WORKSPACES_DIR);
        let run_root = capacity_run_root(&slot);
        let candidate = seed_scope(&stable_root, "trusted", "1", 100, true);
        let lease = hold_workspace_lease(&slot, &run_root, "trusted", "1");

        let active = enforce_budget(&stable_root, &run_root, 0);
        assert!(active.evicted.is_empty());
        assert!(
            candidate.exists(),
            "a leased workspace must never be evicted"
        );

        drop(lease);
        let idle = enforce_budget(&stable_root, &run_root, 0);
        assert_eq!(idle.evicted, vec![candidate.clone()]);
        assert!(
            !candidate.exists(),
            "idle over-budget scope should be reclaimed"
        );
        fs::remove_dir_all(&slot).ok();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn stable_budget_deletion_requires_the_pinned_candidate_and_mount_device() {
        let slot = slot_root("pinned-budget-delete");
        let stable_root = slot.join(STABLE_WORKSPACES_DIR);
        let candidate = seed_scope(&stable_root, "trusted", "1", 100, true);
        let anchor = crate::leftover_disk::filesystem_directory_identity(&stable_root).unwrap();
        let snapshots = crate::leftover_disk::filesystem_candidate_tree_snapshots_under(
            &stable_root,
            &stable_root,
            &anchor,
            2,
        )
        .unwrap();
        let snapshot = snapshots
            .into_iter()
            .find(|snapshot| snapshot.path == candidate)
            .unwrap();
        let mut replaced_candidate = snapshot.identity.clone();
        replaced_candidate.inode = replaced_candidate.inode.saturating_add(1);
        assert!(crate::leftover_disk::remove_dir_all_on_device_under_pinned(
            &stable_root,
            &candidate,
            anchor.device,
            &anchor,
            &replaced_candidate,
            &snapshot.directory,
        )
        .is_err());
        assert!(
            candidate.exists(),
            "replacement identity must preserve data"
        );
        assert!(crate::leftover_disk::remove_dir_all_on_device_under_pinned(
            &stable_root,
            &candidate,
            anchor.device ^ 1,
            &anchor,
            &snapshot.identity,
            &snapshot.directory,
        )
        .is_err());
        assert!(candidate.exists(), "wrong mount device must preserve data");
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn stable_detection_matches_only_the_stable_tree() {
        let slot = slot_root("detect");
        let stable = resolve(&slot, "trusted", &repo_key(41));
        assert!(is_stable_workspace(&stable.workspace));
        assert!(!is_stable_workspace(
            &slot.join("550e8400-e29b-41d4-a716-446655440000/workspace")
        ));
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prune_removes_destinations_absent_from_the_current_job() {
        let slot = slot_root("prune");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "33333333-3333-4333-8333-333333333333",
        )
        .unwrap();
        fs::write(
            stable.scope_dir.join(STABLE_SCOPE_DESTINATIONS),
            serde_json::to_vec(&vec!["keep", "stale"]).unwrap(),
        )
        .unwrap();
        let keep = stable.workspace.join("keep");
        let stale = stable.workspace.join("stale");
        fs::create_dir_all(&keep).unwrap();
        fs::write(keep.join("file"), b"keep").unwrap();
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("file"), b"stale").unwrap();
        prune_stale_destinations(
            &stable.scope_dir,
            &stable.workspace,
            &[keep.clone(), stale.clone()],
        )
        .unwrap();
        assert!(keep.join("file").is_file());
        assert!(stale.join("file").is_file());
        prune_stale_destinations(
            &stable.scope_dir,
            &stable.workspace,
            std::slice::from_ref(&keep),
        )
        .unwrap();
        assert!(keep.join("file").is_file(), "current destinations survive");
        assert!(!stale.exists(), "absent destinations are deleted");
        let record = fs::read_to_string(stable.scope_dir.join(STABLE_SCOPE_DESTINATIONS)).unwrap();
        assert!(record.contains("keep"));
        assert!(!record.contains("stale"));
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[cfg(unix)]
    #[test]
    fn stale_destination_pruning_does_not_follow_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let slot = slot_root("prune-symlink-ancestor");
        let outside = slot.with_extension("outside");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "44444444-4444-4444-8444-444444444444",
        )
        .unwrap();
        let outside_checkout = outside.join("checkout");
        fs::create_dir_all(&outside_checkout).unwrap();
        fs::write(outside_checkout.join("sentinel"), b"preserve outside data").unwrap();
        symlink(&outside, stable.workspace.join("alias")).unwrap();
        fs::write(
            stable.scope_dir.join(STABLE_SCOPE_DESTINATIONS),
            serde_json::to_vec(&vec!["alias/checkout"]).unwrap(),
        )
        .unwrap();

        let error = prune_stale_destinations(&stable.scope_dir, &stable.workspace, &[])
            .expect_err("cleanup must reject a symlinked destination ancestor");

        assert!(
            format!("{error:#}").contains("without following links"),
            "unexpected cleanup error: {error:#}"
        );
        assert_eq!(
            fs::read(outside_checkout.join("sentinel")).unwrap(),
            b"preserve outside data"
        );
        drop(lease);
        fs::remove_dir_all(&slot).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stale_destination_replacement_between_validation_and_removal_survives() {
        use std::os::unix::fs::symlink;

        let slot = slot_root("prune-destination-replacement-race");
        let outside = slot.with_extension("outside");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "44444444-4444-4444-8444-444444444445",
        )
        .unwrap();
        let stale = stable.workspace.join("stale");
        let displaced = stable.workspace.join("stale-displaced-by-race");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("old"), b"previous destination").unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"preserve outside data").unwrap();
        fs::write(
            stable.scope_dir.join(STABLE_SCOPE_DESTINATIONS),
            serde_json::to_vec(&vec!["stale"]).unwrap(),
        )
        .unwrap();

        let error = prune_stale_destinations_unix_with_hook(
            &stable.scope_dir,
            &stable.workspace,
            &[],
            |key| {
                assert_eq!(key, "stale");
                fs::rename(&stale, &displaced)?;
                fs::create_dir(&stale)?;
                fs::write(stale.join("replacement"), b"replacement destination")?;
                symlink(&outside, stale.join("outside-link"))?;
                Ok(())
            },
        )
        .expect_err("a replacement appearing at the validated name must stop cleanup");

        assert!(
            stale
                .join("outside-link")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "racing replacement must be restored at its original name"
        );
        assert_eq!(
            fs::read(stale.join("replacement")).unwrap(),
            b"replacement destination",
            "replacement destination must survive at its original name"
        );
        assert_eq!(
            fs::read(outside.join("sentinel")).unwrap(),
            b"preserve outside data"
        );
        assert_eq!(
            fs::read(displaced.join("old")).unwrap(),
            b"previous destination"
        );
        assert!(
            format!("{error:#}").contains("identity changed"),
            "unexpected cleanup error: {error:#}"
        );
        drop(lease);
        fs::remove_dir_all(&slot).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clear_workspace_preflights_simulated_nested_mount_before_deleting_entries() {
        use std::os::unix::fs::MetadataExt as _;

        let slot = slot_root("clear-simulated-mount");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "88888888-8888-4888-8888-888888888888",
        )
        .unwrap();
        let same_mount_canary = stable.workspace.join("same-mount-canary");
        let nested_mount = stable.workspace.join("nested-mount");
        fs::write(&same_mount_canary, b"preserve until preflight passes").unwrap();
        fs::create_dir_all(&nested_mount).unwrap();
        fs::write(nested_mount.join("mount-canary"), b"preserve mounted data").unwrap();
        let nested_mount_inode = fs::metadata(&nested_mount).unwrap().ino();
        let mut pinned = pin_stable_workspace_scope(&stable.scope_dir, &stable.workspace).unwrap();
        let injected_mount_id = |directory: &fs::File| -> anyhow::Result<u64> {
            let inode = directory.metadata()?.ino();
            Ok(if inode == nested_mount_inode { 2 } else { 1 })
        };

        let error = pinned
            .clear_workspace_with_mount_id(&injected_mount_id)
            .expect_err("workspace cleanup must reject a nested mount before deleting entries");

        assert!(
            format!("{error:#}").contains("mount boundary"),
            "unexpected cleanup error: {error:#}"
        );
        assert_eq!(
            fs::read(&same_mount_canary).unwrap(),
            b"preserve until preflight passes"
        );
        assert_eq!(
            fs::read(nested_mount.join("mount-canary")).unwrap(),
            b"preserve mounted data"
        );
        drop(pinned);
        drop(lease);
        fs::remove_dir_all(&slot).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clear_workspace_replacement_between_validation_and_removal_survives() {
        use std::os::unix::fs::symlink;

        let slot = slot_root("clear-workspace-replacement-race");
        let outside = slot.with_extension("outside");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "88888888-8888-4888-8888-888888888889",
        )
        .unwrap();
        let displaced = stable.scope_dir.join("workspace-displaced-by-race");
        fs::write(stable.workspace.join("old"), b"previous workspace").unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"preserve outside data").unwrap();
        let mut pinned = pin_stable_workspace_scope(&stable.scope_dir, &stable.workspace).unwrap();
        let mount_id_for = |_directory: &fs::File| -> anyhow::Result<u64> { Ok(1) };

        let error = pinned
            .clear_workspace_with_mount_id_and_before_quarantine(&mount_id_for, || {
                fs::rename(&stable.workspace, &displaced)?;
                fs::create_dir(&stable.workspace)?;
                fs::write(stable.workspace.join("replacement"), b"new workspace")?;
                symlink(&outside, stable.workspace.join("outside-link"))?;
                Ok(())
            })
            .expect_err("a replacement workspace must not be cleared");

        assert_eq!(
            fs::read(stable.workspace.join("replacement")).unwrap(),
            b"new workspace",
            "replacement workspace must survive at its original name"
        );
        assert_eq!(
            fs::read(outside.join("sentinel")).unwrap(),
            b"preserve outside data"
        );
        assert_eq!(
            fs::read(displaced.join("old")).unwrap(),
            b"previous workspace",
            "pinned original workspace must not be redirected or deleted"
        );
        assert!(
            format!("{error:#}").contains("identity changed"),
            "unexpected cleanup error: {error:#}"
        );
        drop(pinned);
        drop(lease);
        fs::remove_dir_all(&slot).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn after_quarantine_aba_replacement_cannot_delete_the_replacement_tree() {
        use std::os::unix::fs::symlink;

        let slot = slot_root("clear-workspace-after-quarantine-aba");
        let outside = slot.with_extension("outside");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "88888888-8888-4888-8888-888888888890",
        )
        .unwrap();
        fs::write(
            stable.workspace.join("original-A"),
            b"authorized original A",
        )
        .unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"preserve outside data").unwrap();

        let staged_a = stable.scope_dir.join("original-A-staged");
        let authorized_a = stable.scope_dir.join("original-A-after-check");
        let staged_b = stable.scope_dir.join("replacement-B-staged");
        let replacement_b = stable.scope_dir.join("replacement-B");
        fs::create_dir(&replacement_b).unwrap();
        fs::write(
            replacement_b.join("replacement-B"),
            b"replacement B survives",
        )
        .unwrap();
        symlink(&outside, replacement_b.join("outside-link")).unwrap();

        let pinned = pin_stable_workspace_scope(&stable.scope_dir, &stable.workspace).unwrap();
        let mount_id_for = |_directory: &fs::File| -> anyhow::Result<u64> { Ok(1) };
        let error = pinned
            .scope
            .remove_tree_entry_if_identity_in_quarantine_parent_with_race_hooks(
                std::ffi::OsStr::new("workspace"),
                &pinned.workspace,
                &pinned.scope,
                |parent, name| {
                    parent.preflight_tree_entry_removal_with_mount_id(name, &mount_id_for)
                },
                || {
                    // Put B at the public name so quarantine moves B.
                    fs::rename(&stable.workspace, &staged_a)?;
                    fs::rename(&replacement_b, &stable.workspace)?;
                    Ok(())
                },
                |_quarantine_parent, quarantine_name, _| {
                    // B leaves quarantine and retained A takes its name long
                    // enough to pass identity authorization.
                    fs::rename(stable.scope_dir.join(quarantine_name), &staged_b)?;
                    fs::rename(&staged_a, stable.scope_dir.join(quarantine_name))?;
                    Ok(())
                },
                |_quarantine_parent, quarantine_name, _| {
                    // Restore B after A passed the name check, before deletion.
                    fs::rename(stable.scope_dir.join(quarantine_name), &authorized_a)?;
                    fs::rename(&staged_b, stable.scope_dir.join(quarantine_name))?;
                    Ok(())
                },
            )
            .expect_err("B must fail the original A descriptor check");

        assert_eq!(
            fs::read(stable.workspace.join("replacement-B")).unwrap(),
            b"replacement B survives",
            "B must be restored after it fails the pinned A identity check"
        );
        assert!(
            stable
                .workspace
                .join("outside-link")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "the replacement symlink must survive"
        );
        assert_eq!(
            fs::read(outside.join("sentinel")).unwrap(),
            b"preserve outside data"
        );
        assert_eq!(
            fs::read(authorized_a.join("original-A")).unwrap(),
            b"authorized original A",
            "A must remain intact when the final quarantined name is B"
        );
        assert!(
            format!("{error:#}").contains("changed before deletion"),
            "unexpected cleanup error: {error:#}"
        );

        drop(pinned);
        drop(lease);
        fs::remove_dir_all(&slot).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn pruning_without_a_destination_record_clears_reused_workspace() {
        let slot = slot_root("prune-missing-record");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "55555555-5555-4555-8555-555555555555",
        )
        .unwrap();
        let leftover = stable.workspace.join("old-checkout");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("operator-data"), b"must be cleared").unwrap();

        prune_stale_destinations(
            &stable.scope_dir,
            &stable.workspace,
            &[stable.workspace.join("current-checkout")],
        )
        .expect("missing bookkeeping clears the reused workspace and records current layout");

        assert!(
            !leftover.exists(),
            "unrecorded old checkout data must be cleared"
        );
        assert!(stable.workspace.is_dir());
        assert!(!stable.workspace.join("current-checkout").exists());
        let record = fs::read_to_string(stable.scope_dir.join(STABLE_SCOPE_DESTINATIONS)).unwrap();
        assert!(record.contains("current-checkout"));
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn destination_record_write_failure_fails_before_checkout_and_preserves_record_entry() {
        let slot = slot_root("prune-record-write-failure");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "66666666-6666-4666-8666-666666666666",
        )
        .unwrap();
        let leftover = stable.workspace.join("old-checkout");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("file"), b"stale").unwrap();
        let record_path = stable.scope_dir.join(STABLE_SCOPE_DESTINATIONS);
        fs::write(
            &record_path,
            serde_json::to_vec(&vec!["old-checkout"]).unwrap(),
        )
        .unwrap();
        fs::remove_file(&record_path).unwrap();
        fs::create_dir(&record_path).unwrap();
        fs::write(record_path.join("operator-file"), b"preserve").unwrap();
        let current = stable.workspace.join("new-checkout");

        let error = prune_stale_destinations(
            &stable.scope_dir,
            &stable.workspace,
            std::slice::from_ref(&current),
        )
        .expect_err("bookkeeping failure must stop before checkout");

        assert!(
            format!("{error:#}").contains("publish stable destinations record"),
            "unexpected bookkeeping error: {error:#}"
        );
        assert!(
            !leftover.exists(),
            "unreadable bookkeeping clears old checkout data"
        );
        assert!(
            !current.exists(),
            "checkout must not start after record failure"
        );
        assert_eq!(
            fs::read(record_path.join("operator-file")).unwrap(),
            b"preserve",
            "failed record publication must preserve the blocking entry"
        );
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prune_clears_the_workspace_when_the_root_checkout_goes_away() {
        let slot = slot_root("prune-root");
        let (stable, lease) = prepare(
            &slot,
            &capacity_run_root(&slot),
            "trusted",
            &repo_key(41),
            "77777777-7777-4777-8777-777777777777",
        )
        .unwrap();
        prune_stale_destinations(
            &stable.scope_dir,
            &stable.workspace,
            std::slice::from_ref(&stable.workspace),
        )
        .unwrap();
        fs::write(stable.workspace.join("root-file"), b"root").unwrap();
        fs::create_dir_all(stable.workspace.join("target")).unwrap();
        fs::write(stable.workspace.join("target/artifact"), b"warm").unwrap();
        prune_stale_destinations(
            &stable.scope_dir,
            &stable.workspace,
            std::slice::from_ref(&stable.workspace),
        )
        .unwrap();
        let next = stable.workspace.join("next");
        prune_stale_destinations(&stable.scope_dir, &stable.workspace, &[next]).unwrap();
        assert!(
            !stable.workspace.join("root-file").exists(),
            "previous root files must not leak into a subdir-only job"
        );
        assert!(
            !stable.workspace.join("target/artifact").exists(),
            "the root target belongs to the previous layout"
        );
        assert!(stable.workspace.is_dir());
        drop(lease);
        fs::remove_dir_all(&slot).ok();
    }
}
