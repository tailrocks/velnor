//! Leftover-after-Velnor disposable disk reclaim.
//!
//! Job UUID work trees and dangling untagged images outlive jobs because GC
//! scanned empty `$HOME/.velnor/runner/_work` while daemons write
//! `/var/lib/velnor*/work/slot-N/<job-uuid>`. Hard pressure (90% used) reclaims
//! those leftover classes only — never warm caches, never `docker system prune`,
//! `volume prune`, or `builder prune --all`.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const HARD_PRESSURE_PERCENT: u8 = 90;
pub const LIVE_JOB_NAME_PREFIX: &str = "velnor-job-";

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
        match crate::capacity::active_scopes(run_root, LEASE_STALE_AFTER) {
            Ok(scopes) => liveness.leased = job_ids_from_lease_scopes(&scopes),
            Err(error) => {
                eprintln!("leftover reclaim: cannot read store leases: {error:#}");
                liveness.evidence_incomplete = true;
            }
        }
        match held_job_claim_ids(run_root) {
            Ok(claimed) => liveness.claimed = claimed,
            Err(error) => {
                eprintln!("leftover reclaim: cannot read job claims: {error:#}");
                liveness.evidence_incomplete = true;
            }
        }
        liveness
    }

    pub fn is_live(&self, job_id: &str, workspace: &Path, now: SystemTime) -> bool {
        if self.running.contains(job_id)
            || self.leased.contains(job_id)
            || self.claimed.contains(job_id)
        {
            return true;
        }
        workspace_idle_for(workspace, now).is_none_or(|idle| idle < self.min_idle)
    }
}

/// Job ids named by active lease scopes.
///
/// Every job publishes its leases as `<class>/<store scope>/<job id>`, so the
/// job id is the final segment. A lease therefore proves the job is alive for
/// its whole store-holding lifetime, which is precisely the window `docker ps`
/// cannot see.
pub fn job_ids_from_lease_scopes(scopes: &BTreeSet<String>) -> BTreeSet<String> {
    scopes
        .iter()
        .filter_map(|scope| scope.rsplit('/').next())
        .filter(|id| looks_like_job_uuid(id))
        .map(ToOwned::to_owned)
        .collect()
}

/// Job ids whose host job-claim lock is currently held by a live process.
///
/// The claim is taken before the job's workspace exists and released only when
/// the job process exits, so it covers checkout and teardown. A claim file that
/// still locks is proof; one that can be locked is stale.
pub fn held_job_claim_ids(run_root: &Path) -> Result<BTreeSet<String>> {
    let claims = run_root.join("job-claims");
    let mut held = BTreeSet::new();
    let entries = match fs::read_dir(&claims) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(held),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", claims.display()));
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry in {}", claims.display()))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(&path) else {
            // Unreadable claim: assume held rather than delete its workspace.
            held.extend(job_uuids_in(&entry.file_name().to_string_lossy()));
            continue;
        };
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                let _ = rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock);
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                held.extend(job_uuids_in(&entry.file_name().to_string_lossy()));
            }
            Err(_) => held.extend(job_uuids_in(&entry.file_name().to_string_lossy())),
        }
    }
    Ok(held)
}

/// Every job-UUID-shaped substring of a claim file name (`<plan>-<job>`).
fn job_uuids_in(name: &str) -> BTreeSet<String> {
    let parts: Vec<&str> = name.split('-').collect();
    let mut found = BTreeSet::new();
    for window in parts.windows(5) {
        let candidate = window.join("-");
        if looks_like_job_uuid(&candidate) {
            found.insert(candidate);
        }
    }
    found
}

fn workspace_idle_for(workspace: &Path, now: SystemTime) -> Option<Duration> {
    let modified = newest_modification(workspace, 0)?;
    now.duration_since(modified).ok()
}

/// Newest mtime in the workspace, bounded to the top three levels: a job
/// between steps touches its top-level tree, and an unbounded walk would make
/// the reaper itself a disk-pressure event.
fn newest_modification(path: &Path, depth: usize) -> Option<SystemTime> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let mut newest = metadata.modified().ok()?;
    if depth >= 3 || !metadata.is_dir() {
        return Some(newest);
    }
    for entry in fs::read_dir(path).ok()?.flatten() {
        if let Some(child) = newest_modification(&entry.path(), depth + 1) {
            newest = newest.max(child);
        }
    }
    Some(newest)
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

/// Fleet work roots: `$VELNOR_STORAGE_ROOT/lib/velnor*/work`, else `/var/lib/velnor*/work`.
pub fn discover_daemon_work_roots() -> Vec<PathBuf> {
    match crate::storage::StorageLayout::resolve() {
        Some(layout) => discover_daemon_work_roots_for_layout(&layout),
        None => discover_daemon_work_roots_in(Path::new("/var/lib")),
    }
}

fn discover_daemon_work_roots_for_layout(layout: &crate::storage::StorageLayout) -> Vec<PathBuf> {
    let lib_parent = layout
        .lib_root
        .parent()
        .filter(|parent| *parent != Path::new("/"))
        .unwrap_or(&layout.lib_root);
    let mut roots = discover_daemon_work_roots_in(lib_parent);
    if roots.is_empty() {
        let work = layout.lib_root.join("work");
        if work.is_dir() {
            roots.push(work);
        }
    }
    roots
}

pub fn discover_daemon_work_roots_in(lib: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Ok(entries) = fs::read_dir(lib) else {
        return roots;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("velnor") {
            continue;
        }
        let work = entry.path().join("work");
        if work.is_dir() {
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
    if liveness.evidence_incomplete {
        eprintln!("leftover reclaim: liveness evidence incomplete; deleting nothing");
        return Vec::new();
    }
    let now = SystemTime::now();
    let mut orphans = Vec::new();
    for work in work_roots {
        let Ok(slots) = fs::read_dir(work) else {
            continue;
        };
        for slot in slots.flatten() {
            let slot_path = slot.path();
            let slot_name = slot.file_name();
            let slot_name = slot_name.to_string_lossy();
            if !slot_path.is_dir() || !slot_name.starts_with("slot-") {
                continue;
            }
            let Ok(jobs) = fs::read_dir(&slot_path) else {
                continue;
            };
            for job in jobs.flatten() {
                let job_path = job.path();
                let job_name = job.file_name();
                let job_name = job_name.to_string_lossy();
                if !job_path.is_dir() {
                    continue;
                }
                if job_name.starts_with("_velnor_") {
                    continue;
                }
                if !looks_like_job_uuid(&job_name) {
                    continue;
                }
                if liveness.is_live(job_name.as_ref(), &job_path, now) {
                    continue;
                }
                orphans.push(job_path);
            }
        }
    }
    orphans.sort();
    orphans
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

#[cfg(any(not(unix), test))]
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

pub fn reclaim_leftover_after_velnor(
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
    docker: impl FnMut(&[String]) -> Result<String>,
    remove_dir: impl FnMut(&Path) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    let liveness = match runtime_root() {
        // Hold the same coordinator the cache reclaimer takes, so no daemon can
        // publish a lease between the liveness snapshot and the deletions it
        // authorizes.
        Some(run_root) => {
            let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root)?;
            return reclaim_with_liveness(
                work_roots,
                &WorkspaceLiveness::collect(&run_root, live_job_ids.clone()),
                docker,
                remove_dir,
                prune_dangling_images,
            );
        }
        None => WorkspaceLiveness {
            running: live_job_ids.clone(),
            min_idle: WORKSPACE_MIN_IDLE,
            ..WorkspaceLiveness::default()
        },
    };
    reclaim_with_liveness(
        work_roots,
        &liveness,
        docker,
        remove_dir,
        prune_dangling_images,
    )
}

fn runtime_root() -> Option<PathBuf> {
    crate::storage::StorageLayout::resolve().map(|layout| layout.run_root)
}

/// Delete only workspaces that every liveness source agrees are dead.
pub fn reclaim_with_liveness(
    work_roots: &[PathBuf],
    liveness: &WorkspaceLiveness,
    mut docker: impl FnMut(&[String]) -> Result<String>,
    mut remove_dir: impl FnMut(&Path) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    let mut report = LeftoverReclaimReport::default();
    let orphans = orphan_job_workspace_paths_with_liveness(work_roots, liveness);
    for path in orphans {
        match remove_dir(&path) {
            Ok(()) => report.deleted_workspaces.push(path),
            Err(error) => {
                eprintln!(
                    "leftover workspace reclaim failed for {}: {error:#}",
                    path.display()
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

pub fn remove_dir_all(path: &Path) -> Result<()> {
    fs::remove_dir_all(path)
        .with_context(|| format!("remove leftover workspace {}", path.display()))
}

pub fn live_job_ids_from_host_docker() -> Result<BTreeSet<String>> {
    let listed = crate::docker_lease::run_host_docker(&list_live_job_names_args())?;
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

fn reclaim_production_leftovers(prune_dangling_images: bool) -> Result<LeftoverReclaimReport> {
    let work_roots = discover_daemon_work_roots();
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
    reclaim_leftover_after_velnor(
        &work_roots,
        &live,
        host_docker_if_safe,
        remove_dir_all,
        prune_dangling_images,
    )
}

/// Reclaim leftover workspaces. The microVM backend never lists or prunes
/// through the host Docker socket.
pub fn reclaim_production_leftovers_for(
    backend: velnor_model::ExecutionBackendKind,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    if backend.uses_host_docker_socket() {
        reclaim_production_leftovers(prune_dangling_images)
    } else {
        reclaim_microvm_leftovers()
    }
}

fn reclaim_microvm_leftovers() -> Result<LeftoverReclaimReport> {
    // Work roots are shared by all daemon pools. MicroVM selection alone does
    // not identify ownership, so deleting every UUID absent from the current
    // pool's live set can remove an active Docker job from another pool.
    // Leave reclamation to the coordinator until it supplies a pool-scoped
    // ownership lease.
    Ok(LeftoverReclaimReport::default())
}

/// Hard-pressure reclaim that skips host Docker when the selected backend is
/// `microvm` or selection is unknown.
pub fn reclaim_production_if_hard_pressure_for(
    backend: Option<velnor_model::ExecutionBackendKind>,
    usage_percent: u8,
) -> Result<LeftoverReclaimReport> {
    reclaim_production_if_hard_pressure_with(
        backend,
        usage_percent,
        &discover_daemon_work_roots(),
        host_docker_if_safe,
        remove_dir_all,
    )
}

/// Injectable hard-pressure reclaim. Host Docker listing and prune run only
/// when the selected backend permits host Docker maintenance.
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
    crate::docker_lease::run_host_docker(args)
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
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn write_tree(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("marker"), b"job").unwrap();
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
        let _leases = [
            crate::capacity::ScopeLease::acquire(
                &run_root,
                "actions-cache",
                &format!("repo/{uploading}"),
                Duration::from_secs(3600),
            )
            .unwrap(),
            crate::capacity::ScopeLease::acquire(
                &run_root,
                "targets",
                &format!("workspace-v2/repo/ci.yml/{publishing}"),
                Duration::from_secs(3600),
            )
            .unwrap(),
        ];
        // Mid checkout: no lease yet, but the host job claim is held.
        let claims = run_root.join("job-claims");
        fs::create_dir_all(&claims).unwrap();
        let claim_path = claims.join(format!("plan-uuid-{checking_out}"));
        let claim = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&claim_path)
            .unwrap();
        rustix::fs::flock(&claim, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();

        let liveness = WorkspaceLiveness::collect(&run_root, BTreeSet::new());
        assert!(!liveness.evidence_incomplete);
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

    #[test]
    fn lease_scopes_name_the_job_that_holds_them() {
        let scopes = BTreeSet::from([
            "targets/workspace-v2/repo/ci.yml/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
            "cargo/registry/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
            "mise/cache".to_string(),
        ]);
        assert_eq!(
            job_ids_from_lease_scopes(&scopes),
            BTreeSet::from(["aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string()])
        );
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
