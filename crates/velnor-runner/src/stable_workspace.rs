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
//! <slot-work-dir>/stable-workspaces/<scope>/<repository-id>/workspace
//! ```
//!
//! * `<scope>` is the job's admitted trust scope, sanitized for the
//!   filesystem. Fork-PR and unknown jobs land under the untrusted floor,
//!   exactly like the compiler stores, so an untrusted job can neither read
//!   nor poison a trusted workspace.
//! * `<repository-id>` is the numeric `github.repository_id`, mirroring the
//!   compiler-store namespacing. Jobs without a valid id fall back to an
//!   ephemeral workspace rather than sharing one.
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
//! the same warm-target allowance mbx enjoys). Enforcement runs only when a
//! new scope is created — the only event that grows the tree, since an
//! existing scope's `target/` is bounded by the repository's finite build
//! closure — and evicts whole idle scopes, least-recently-used first. The
//! LRU clock is a marker file refreshed on every allocation, because
//! directory mtimes do not track deep file writes. Eviction is hygiene, not
//! safety: any error warns and continues, and the capacity reservation
//! (which measures real free disk) remains the fail-closed guard.

use anyhow::Context as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Leaf directory holding every stable workspace of one slot.
pub(crate) const STABLE_WORKSPACES_DIR: &str = "stable-workspaces";

/// LRU marker inside each scope directory. Its mtime is the scope's
/// last-use clock, refreshed by every allocation that lands on the scope.
const STABLE_SCOPE_LAST_USE: &str = ".velnor-last-use";

/// Recorded checkout destinations for one scope, as workspace-relative
/// keys (`""` for the workspace root, `"subdir"` otherwise). Read before
/// checkout to delete destinations the previous job left behind.
const STABLE_SCOPE_DESTINATIONS: &str = ".velnor-destinations";

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
    repository_id: u64,
) -> StableWorkspace {
    let scope =
        crate::container::sanitize_store_key(crate::trust_scope::normalize_scope(trust_scope));
    let scope_dir = slot_work_dir
        .join(STABLE_WORKSPACES_DIR)
        .join(scope)
        .join(repository_id.to_string());
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

fn write_destination_record(record_path: &Path, current: &std::collections::BTreeSet<String>) {
    let list: Vec<&String> = current.iter().collect();
    let text = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
    let tmp = record_path.with_extension(format!("tmp-{}", std::process::id()));
    let result = fs::write(&tmp, &text).and_then(|()| fs::rename(&tmp, record_path));
    if let Err(error) = result {
        eprintln!(
            "forensics.lifecycle: stable destinations unwritable at {}: {error:#}",
            record_path.display()
        );
        fs::remove_file(&tmp).ok();
    }
}

fn clear_workspace(workspace: &Path) -> anyhow::Result<()> {
    if workspace.exists() {
        fs::remove_dir_all(workspace)
            .with_context(|| format!("clear stable workspace {}", workspace.display()))?;
    }
    fs::create_dir_all(workspace)
        .with_context(|| format!("recreate stable workspace {}", workspace.display()))?;
    Ok(())
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
/// failures only warn: the current job already pruned correctly, and the
/// next job fails closed on a missing record by clearing.
pub(crate) fn prune_stale_destinations(
    scope_dir: &Path,
    workspace: &Path,
    current_destinations: &[PathBuf],
) -> anyhow::Result<()> {
    let mut current = std::collections::BTreeSet::new();
    for destination in current_destinations {
        if let Some(key) = destination_key(workspace, destination) {
            current.insert(key);
        }
    }
    let record_path = scope_dir.join(STABLE_SCOPE_DESTINATIONS);
    let previous: std::collections::BTreeSet<String> = match fs::read_to_string(&record_path) {
        Ok(text) => match serde_json::from_str::<Vec<String>>(&text) {
            Ok(list) => list.into_iter().collect(),
            Err(error) => {
                eprintln!(
                    "forensics.lifecycle: stable destinations unreadable at {} ({error:#}), clearing workspace",
                    record_path.display()
                );
                clear_workspace(workspace)?;
                write_destination_record(&record_path, &current);
                return Ok(());
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_destination_record(&record_path, &current);
            return Ok(());
        }
        Err(error) => {
            eprintln!(
                "forensics.lifecycle: stable destinations unreadable at {} ({error:#}), clearing workspace",
                record_path.display()
            );
            clear_workspace(workspace)?;
            write_destination_record(&record_path, &current);
            return Ok(());
        }
    };
    let stale: Vec<String> = previous.difference(&current).cloned().collect();
    if stale.is_empty() {
        write_destination_record(&record_path, &current);
        return Ok(());
    }
    if stale.iter().any(|key| key.is_empty()) {
        eprintln!(
            "forensics.lifecycle: stable workspace clearing {} (previous root checkout absent)",
            workspace.display()
        );
        clear_workspace(workspace)?;
        write_destination_record(&record_path, &current);
        return Ok(());
    }
    for key in stale {
        let Some(path) = destination_path(workspace, &key) else {
            eprintln!(
                "forensics.lifecycle: stable workspace skipping unsafe recorded destination {key:?}"
            );
            continue;
        };
        if path.exists() {
            eprintln!(
                "forensics.lifecycle: stable workspace removing stale destination {}",
                path.display()
            );
            fs::remove_dir_all(&path)
                .with_context(|| format!("remove stale stable destination {}", path.display()))?;
        }
    }
    write_destination_record(&record_path, &current);
    Ok(())
}

/// Allocate the stable workspace for one job: create the directories,
/// refresh the scope's LRU clock, and enforce the slot budget when the
/// scope is new (or was never clocked, which is the same growth event).
pub(crate) fn prepare(
    slot_work_dir: &Path,
    trust_scope: &str,
    repository_id: u64,
) -> anyhow::Result<StableWorkspace> {
    let mut stable = resolve(slot_work_dir, trust_scope, repository_id);
    stable.fresh_scope = !stable.scope_dir.is_dir();
    let marker = stable.scope_dir.join(STABLE_SCOPE_LAST_USE);
    // A missing clock means this allocation grows the tree: either the scope
    // is new, or a previous process never managed to clock it. Both cases
    // enforce the budget; a clocked scope only refreshes its timestamp.
    let unclocked = !marker.is_file();
    fs::create_dir_all(&stable.workspace)
        .with_context(|| format!("create stable workspace {}", stable.workspace.display()))?;
    if let Err(error) = fs::write(&marker, "velnor stable workspace scope\n") {
        eprintln!(
            "forensics.lifecycle: stable workspace clock unwritable at {}: {error:#}",
            marker.display()
        );
    }
    if unclocked {
        enforce_budget(
            &slot_work_dir.join(STABLE_WORKSPACES_DIR),
            &stable.scope_dir,
            STABLE_WORKSPACES_BUDGET_BYTES,
        );
    }
    Ok(stable)
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
/// stable tree fits the budget. Best-effort hygiene: any error warns and
/// stops, never fails the job the allocation serves. The current scope is
/// never a victim.
fn enforce_budget(stable_root: &Path, current_scope: &Path, budget_bytes: u64) {
    let mut scopes = Vec::new();
    let mut total = 0u64;
    let Ok(scope_dirs) = fs::read_dir(stable_root) else {
        return;
    };
    for scope in scope_dirs.flatten() {
        let scope_path = scope.path();
        if !scope_path.is_dir() {
            continue;
        }
        let Ok(repos) = fs::read_dir(&scope_path) else {
            continue;
        };
        for repo in repos.flatten() {
            let dir = repo.path();
            if !dir.is_dir() || dir == current_scope {
                continue;
            }
            // Only directories this allocator could have made are victims:
            // a scope always carries its clock or its workspace. Anything
            // else is operator-owned and must survive.
            let looks_like_scope =
                dir.join(STABLE_SCOPE_LAST_USE).is_file() || dir.join("workspace").is_dir();
            if !looks_like_scope {
                continue;
            }
            let last_use = fs::metadata(dir.join(STABLE_SCOPE_LAST_USE))
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let bytes = match crate::storage::dir_size(&dir) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!(
                        "forensics.lifecycle: stable workspace size unreadable at {}: {error:#}",
                        dir.display()
                    );
                    continue;
                }
            };
            total = total.saturating_add(bytes);
            scopes.push(EvictionScope {
                dir,
                last_use,
                bytes,
            });
        }
    }
    // The current scope counts toward the total but is never a victim: a
    // freshly created scope holds no build output yet (cheap to measure),
    // while an adopted scope can be arbitrarily large and must not hide
    // behind its own exclusion.
    if current_scope.is_dir() {
        match crate::storage::dir_size(current_scope) {
            Ok(bytes) => total = total.saturating_add(bytes),
            Err(error) => eprintln!(
                "forensics.lifecycle: stable workspace size unreadable at {}: {error:#}",
                current_scope.display()
            ),
        }
    }
    if total <= budget_bytes {
        return;
    }
    for victim in victim_order(scopes) {
        if total <= budget_bytes {
            break;
        }
        match fs::remove_dir_all(&victim.dir) {
            Ok(()) => {
                eprintln!(
                    "forensics.lifecycle: evicted idle stable workspace scope {} ({} bytes, over {} budget)",
                    victim.dir.display(),
                    victim.bytes,
                    budget_bytes,
                );
                total = total.saturating_sub(victim.bytes);
            }
            Err(error) => {
                eprintln!(
                    "forensics.lifecycle: stable workspace eviction failed at {}: {error:#}",
                    victim.dir.display()
                );
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("velnor-stable-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn layout_is_namespaced_and_reaper_safe() {
        let slot = slot_root("layout");
        let job_dir = slot.join("550e8400-e29b-41d4-a716-446655440000");
        let stable = resolve(&slot, "trusted", 41);
        assert_eq!(
            stable.workspace,
            slot.join("stable-workspaces")
                .join("trusted")
                .join("41")
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
    fn scope_segment_is_sanitized_and_fail_closed() {
        let slot = slot_root("scope");
        let traversal = resolve(&slot, "../../etc", 7);
        assert_eq!(
            traversal.scope_dir,
            slot.join(STABLE_WORKSPACES_DIR).join(".._.._etc").join("7")
        );
        assert!(traversal
            .scope_dir
            .starts_with(slot.join(STABLE_WORKSPACES_DIR)));
        let blank = resolve(&slot, "   ", 7);
        assert_eq!(
            blank.scope_dir,
            slot.join(STABLE_WORKSPACES_DIR)
                .join(crate::trust_scope::FAIL_CLOSED)
                .join("7")
        );
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prepare_creates_and_clocks_the_scope() {
        let slot = slot_root("prepare");
        let first = prepare(&slot, "trusted", 41).unwrap();
        assert!(first.fresh_scope);
        assert!(first.workspace.is_dir());
        assert!(first.scope_dir.join(STABLE_SCOPE_LAST_USE).is_file());
        let second = prepare(&slot, "trusted", 41).unwrap();
        assert!(!second.fresh_scope);
        assert_eq!(first.workspace, second.workspace);
        fs::remove_dir_all(&slot).ok();
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
        let dir = root.join(scope).join(repo);
        let target = dir.join("workspace").join("repo").join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("artifact"), vec![7u8; bytes]).unwrap();
        if clocked {
            fs::write(dir.join(STABLE_SCOPE_LAST_USE), "clocked\n").unwrap();
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
        enforce_budget(&root, &current, 50);
        assert!(!oldest.exists(), "oldest idle scope must go first");
        assert!(!mid.exists(), "second scope must go to fit the budget");
        assert!(current.exists(), "the current scope is never a victim");
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn eviction_stops_once_under_budget_and_spares_junk() {
        let slot = slot_root("evict-partial");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        let oldest = seed_scope(&root, "trusted", "1", 100, false);
        let newer = seed_scope(&root, "trusted", "2", 100, true);
        let current = seed_scope(&root, "public", "9", 100, true);
        let junk_dir = root.join("trusted").join("operator-notes");
        fs::create_dir_all(&junk_dir).unwrap();
        fs::write(junk_dir.join("readme"), b"not a scope").unwrap();
        enforce_budget(&root, &current, 250);
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
    fn current_scope_survives_even_when_it_alone_exceeds_budget() {
        let slot = slot_root("evict-current");
        let root = slot.join(STABLE_WORKSPACES_DIR);
        let current = seed_scope(&root, "trusted", "1", 100, true);
        enforce_budget(&root, &current, 10);
        assert!(current.exists());
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn stable_detection_matches_only_the_stable_tree() {
        let slot = slot_root("detect");
        let stable = resolve(&slot, "trusted", 41);
        assert!(is_stable_workspace(&stable.workspace));
        assert!(!is_stable_workspace(
            &slot.join("550e8400-e29b-41d4-a716-446655440000/workspace")
        ));
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prune_removes_destinations_absent_from_the_current_job() {
        let slot = slot_root("prune");
        let stable = prepare(&slot, "trusted", 41).unwrap();
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
        fs::remove_dir_all(&slot).ok();
    }

    #[test]
    fn prune_clears_the_workspace_when_the_root_checkout_goes_away() {
        let slot = slot_root("prune-root");
        let stable = prepare(&slot, "trusted", 41).unwrap();
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
        fs::remove_dir_all(&slot).ok();
    }
}
