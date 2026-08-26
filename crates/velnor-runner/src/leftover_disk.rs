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

pub const HARD_PRESSURE_PERCENT: u8 = 90;
pub const LIVE_JOB_NAME_PREFIX: &str = "velnor-job-";

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

pub fn orphan_job_workspace_paths(
    work_roots: &[PathBuf],
    live_job_ids: &BTreeSet<String>,
) -> Vec<PathBuf> {
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
                if live_job_ids.contains(job_name.as_ref()) {
                    continue;
                }
                orphans.push(job_path);
            }
        }
    }
    orphans.sort();
    orphans
}

pub fn disk_usage_percent_from_df(stdout: &str) -> Option<u8> {
    let line = stdout.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() >= 5 {
        if let Ok(percent) = cols[4].trim_end_matches('%').parse::<u8>() {
            return Some(percent);
        }
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
    mut docker: impl FnMut(&[String]) -> Result<String>,
    mut remove_dir: impl FnMut(&Path) -> Result<()>,
    prune_dangling_images: bool,
) -> Result<LeftoverReclaimReport> {
    let mut report = LeftoverReclaimReport::default();
    let orphans = orphan_job_workspace_paths(work_roots, live_job_ids);
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
    let work_roots = discover_daemon_work_roots();
    reclaim_leftover_after_velnor(
        &work_roots,
        &BTreeSet::new(),
        |_args| bail!("microvm leftover reclaim does not use host docker"),
        remove_dir_all,
        false,
    )
}

/// Hard-pressure reclaim that skips host Docker when the selected backend is
/// `microvm` or selection is unknown.
pub fn reclaim_production_if_hard_pressure_for(
    backend: Option<velnor_model::ExecutionBackendKind>,
    usage_percent: u8,
) -> Result<LeftoverReclaimReport> {
    if usage_percent < HARD_PRESSURE_PERCENT {
        return Ok(LeftoverReclaimReport::default());
    }
    if !velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend) {
        return reclaim_microvm_leftovers();
    }
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
    reclaim_if_hard_pressure(
        usage_percent,
        &work_roots,
        &live,
        host_docker_if_safe,
        remove_dir_all,
    )
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
        write_tree(&orphan);
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
        write_tree(&orphan);
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

    fn assert_leftover_docker_commands_are_safe(commands: &[Vec<String>]) {
        for command in commands {
            assert!(
                !leftover_docker_args_are_unsafe(command),
                "leftover reclaim issued unsafe docker command: {command:?}"
            );
        }
    }
}
