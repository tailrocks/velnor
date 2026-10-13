use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};

use crate::{
    args::{CacheArgs, CacheCommand, CacheGcArgs},
    config,
};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const LEASE_STALE_AFTER: Duration = Duration::from_secs(24 * 3600);

/// Minimum time a store must have been untouched before the *emergency*
/// reclaimer may delete it.
pub(crate) const EMERGENCY_MIN_IDLE: Duration = Duration::from_secs(15 * 60);

/// Builder-name prefix for every BuildKit builder Velnor owns. The concrete
/// builder always carries a `-<scope>` suffix, which is why inspecting the bare
/// prefix never matched and the disk-pressure BuildKit reclaim was dead code.
pub(crate) const OWNED_BUILDER_PREFIX: &str = "velnor-builder";
const PERSISTENT_TARGET_MAX_NODES: usize = 1_000_000;
const PERSISTENT_TARGET_MAX_DIRECTORIES: usize = 100_000;
const PERSISTENT_TARGET_MAX_DEPTH: usize = 256;
const PERSISTENT_TARGET_MAX_PATH_BYTES: u64 = 64 * 1024 * 1024;

/// Serializes one actions/cache generation across save, restore, and GC.
///
/// Repository-scope leases prevent normal eviction while a job is active.
/// This entry lock is the final integrity boundary: even if a reclaim pass
/// selected the generation before the job published its lease, it cannot
/// delete files while restore verification is reading them.
pub(crate) struct CacheEntryLock {
    _file: File,
}

impl CacheEntryLock {
    pub(crate) fn shared(cache_dir: &Path) -> Result<Self> {
        Self::acquire(cache_dir, rustix::fs::FlockOperation::LockShared)
    }

    pub(crate) fn exclusive(cache_dir: &Path) -> Result<Self> {
        Self::acquire(cache_dir, rustix::fs::FlockOperation::LockExclusive)
    }

    fn acquire(cache_dir: &Path, operation: rustix::fs::FlockOperation) -> Result<Self> {
        let store = cache_dir
            .parent()
            .context("cache entry has no store parent")?;
        let locks = store.join(".velnor-locks");
        fs::create_dir_all(&locks)
            .with_context(|| format!("create cache lock directory {}", locks.display()))?;
        let name = cache_dir.file_name().context("cache entry has no name")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(locks.join(name))
            .with_context(|| format!("open cache entry lock for {}", cache_dir.display()))?;
        rustix::fs::flock(&file, operation)
            .with_context(|| format!("lock cache entry {}", cache_dir.display()))?;
        Ok(Self { _file: file })
    }
}

pub(crate) fn run(args: CacheArgs) -> Result<()> {
    let budgets = BTreeMap::from([
        (CacheStore::Targets, args.budget_targets_bytes),
        (CacheStore::ActionsCache, args.budget_caches_bytes),
        (CacheStore::Artifacts, args.budget_artifacts_bytes),
        (CacheStore::Cargo, args.budget_cargo_bytes),
        (CacheStore::Mise, args.budget_mise_bytes),
    ]);
    let work_root = work_root(args.config_dir, args.work_dir)?;
    match args.command {
        CacheCommand::Du => run_du(&work_root, &budgets),
        CacheCommand::Gc(gc) => run_gc(&work_root, gc, budgets),
    }
}

fn work_root(config_dir: Option<PathBuf>, work_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(work_dir) = work_dir {
        return Ok(crate::container::daemon_shared_root(work_dir));
    }
    // Package units set VELNOR_STORAGE_ROOT=/var. Scanning
    // `$HOME/.velnor/runner/_work` reports 0 candidates while leftover job
    // UUID trees sit in `/var/lib/velnor*/work`.
    if let Some(layout) = crate::storage::StorageLayout::resolve() {
        let work = layout.lib_root.join("work");
        if work.is_dir() {
            return Ok(crate::container::daemon_shared_root(work));
        }
    }
    if let Some(first) = crate::leftover_disk::discover_daemon_work_roots()
        .into_iter()
        .next()
    {
        return Ok(crate::container::daemon_shared_root(first));
    }
    Ok(config::config_dir(config_dir)?.join("_work"))
}

fn run_du(work_root: &Path, budgets: &BTreeMap<CacheStore, u64>) -> Result<()> {
    let stores = store_roots(work_root);
    println!("work_dir\t{}", work_root.display());
    println!("kind\tlogical_bytes\tphysical_bytes\tbudget_bytes\tpressure\tpath");
    for store in &stores {
        let (logical, physical, _) = size_physical_and_modified(&store.path)?;
        let budget = budgets.get(&store.kind).copied().unwrap_or(0);
        println!(
            "store\t{}\t{}\t{}\t{}\t{}",
            logical,
            physical,
            budget,
            if budget > 0 && physical > budget {
                "HIGH"
            } else {
                "ok"
            },
            store.path.display()
        );
    }

    // Docker is not a Velnor store, but it spends the same filesystem. Leaving
    // it out of the report is what let the reservation ledger believe it held
    // headroom Docker had already taken.
    match crate::host_capacity::docker_usage_bytes() {
        Some(bytes) => println!(
            "store\t{}\t{bytes}\t{bytes}\t0\tunmanaged\tdocker",
            CacheStore::Docker
        ),
        None => println!("store\t{}\t0\t0\t0\tunmeasured\tdocker", CacheStore::Docker),
    }
    match crate::host_capacity::HostCapacity::probe(work_root) {
        Ok(capacity) => println!(
            "host\ttotal_bytes\t{}\tavailable_bytes\t{}\tused_percent\t{}",
            capacity.total_bytes,
            capacity.available_bytes,
            capacity.used_percent()
        ),
        Err(error) => eprintln!("host capacity probe failed: {error:#}"),
    }

    println!("scope\tstore\tbytes\tscope");
    for store in &stores {
        for (scope, bytes) in scoped_sizes(store)? {
            println!("scope\t{}\t{}\t{}", store.kind, bytes, scope);
        }
    }
    Ok(())
}

pub fn accounting_summary(work_root: &Path) -> Result<(u64, u64)> {
    let mut logical = 0u64;
    let mut physical = 0u64;
    for store in store_roots(work_root) {
        let (store_logical, store_physical, _) = size_physical_and_modified(&store.path)?;
        logical = logical.saturating_add(store_logical);
        physical = physical.saturating_add(store_physical);
    }
    Ok((logical, physical))
}

fn run_gc(
    work_root: &Path,
    args: CacheGcArgs,
    class_budgets: BTreeMap<CacheStore, u64>,
) -> Result<()> {
    if !args.dry_run && !args.yes {
        bail!("destructive cache gc requires --yes");
    }
    let storage_layout = crate::storage::StorageLayout::resolve();
    let run_root = storage_layout
        .as_ref()
        .map(|layout| layout.run_root.clone())
        .unwrap_or_else(|| work_root.join("_velnor_runtime"));
    let _gc_leader = if args.dry_run {
        None
    } else {
        Some(GcLeaderLock::acquire(&run_root)?)
    };
    let in_use_scopes = snapshot_active_scopes(
        &run_root,
        &BTreeSet::new(),
        args.force_no_lease_check,
    )?;

    let listing = cache_listing_with_layout(work_root, false, storage_layout.as_ref())?;
    let max_age = args
        .max_age_days
        .checked_mul(DAY.as_secs())
        .map(Duration::from_secs)
        .context("max-age-days overflowed Duration")?;
    let policy = EvictionPolicy {
        now: SystemTime::now(),
        keep_newest_per_target_scope: args.keep_newest_targets,
        max_age,
        max_total_bytes: args.max_size_bytes,
        class_budgets,
        in_use_scopes,
        protected_paths: pointer_protected_target_generations_with_layout(
            work_root,
            storage_layout.as_ref(),
        ),
    };
    let candidates = select_eviction_candidates(&listing, &policy);

    println!("dry_run\t{}", args.dry_run);
    println!("work_dir\t{}", work_root.display());
    println!("candidate_count\t{}", candidates.len());
    println!("store\tbytes\tscope\treason\tpath");
    if args.dry_run {
        for candidate in candidates {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                candidate.store,
                candidate.bytes,
                candidate.scope_key(),
                candidate.reason,
                candidate.path.display()
            );
        }
        print_leftover_workspace_candidates();
        return Ok(());
    }

    let log_root = storage_layout
        .as_ref()
        .map(|layout| layout.log_root.clone())
        .unwrap_or_else(|| work_root.join("_velnor_logs"));
    for candidate in candidates {
        let still_unclaimed = match cache_candidate_is_still_unclaimed(
            &run_root,
            &candidate,
            &policy.in_use_scopes,
            args.force_no_lease_check,
        ) {
            Ok(still_unclaimed) => still_unclaimed,
            Err(error) => {
                append_gc_history(&log_root, &candidate, Some(&policy), "failed")?;
                eprintln!(
                    "cache gc revalidation failed for {}: {error:#}",
                    candidate.path.display()
                );
                continue;
            }
        };
        if !still_unclaimed {
            append_gc_history(&log_root, &candidate, Some(&policy), "skipped-live")?;
            continue;
        }
        let result = remove_candidate(&candidate);
        let outcome = if result.is_ok() { "deleted" } else { "failed" };
        append_gc_history(&log_root, &candidate, Some(&policy), outcome)?;
        println!(
            "{}\t{}\t{}\t{}\t{}",
            candidate.store,
            candidate.bytes,
            candidate.scope_key(),
            candidate.reason,
            candidate.path.display()
        );
        if let Err(error) = result {
            eprintln!(
                "gc deletion failed for {}: {error}",
                candidate.path.display()
            );
        }
    }
    let backend = crate::execution::load_execution_file(std::path::Path::new("/etc/velnor"), None)
        .ok()
        .map(|file| file.backend());
    if let Some(reason) =
        velnor_model::ExecutionBackendKind::host_docker_maintenance_skip_reason(backend)
    {
        eprintln!("leftover-after-Velnor host Docker reclaim skipped: {reason}");
    }
    let reclaim_backend = backend.unwrap_or(velnor_model::ExecutionBackendKind::MicroVm);
    match crate::leftover_disk::reclaim_production_leftovers_for(reclaim_backend, false) {
        Ok(report) => {
            println!(
                "leftover_workspace_deleted\t{}",
                report.deleted_workspaces.len()
            );
        }
        Err(error) => eprintln!("leftover-after-Velnor reclaim failed: {error:#}"),
    }
    Ok(())
}

fn print_leftover_workspace_candidates() {
    let roots = crate::leftover_disk::discover_daemon_work_roots();
    println!("leftover_work_roots\t{}", roots.len());
    for root in &roots {
        println!("leftover_work_root\t{}", root.display());
    }
    let backend = crate::execution::load_execution_file(std::path::Path::new("/etc/velnor"), None)
        .ok()
        .map(|file| file.backend());
    if let Some(reason) =
        velnor_model::ExecutionBackendKind::host_docker_maintenance_skip_reason(backend)
    {
        eprintln!("leftover live-job listing skipped: {reason}");
    }
    let live = crate::leftover_disk::live_job_ids_for_reclaim(backend).unwrap_or_default();
    let orphans = crate::leftover_disk::orphan_job_workspace_paths(&roots, &live);
    println!("leftover_workspace_candidates\t{}", orphans.len());
    for path in orphans {
        println!("leftover-workspace\torphan-job-uuid\t{}", path.display());
    }
}

struct GcLeaderLock {
    _file: File,
}

impl GcLeaderLock {
    fn acquire(run_root: &Path) -> Result<Self> {
        fs::create_dir_all(run_root)
            .with_context(|| format!("create GC runtime dir {}", run_root.display()))?;
        let path = run_root.join("gc.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open GC leader lock {}", path.display()))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .with_context(|| "another gc holds the lock")?;
        Ok(Self { _file: file })
    }
}

/// Snapshot lease claims while blocking lease publication, then release the
/// filesystem-wide coordinator before any store traversal or mutation.
fn snapshot_active_scopes(
    run_root: &Path,
    in_use_scopes: &BTreeSet<String>,
    force_no_lease_check: bool,
) -> Result<BTreeSet<String>> {
    let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(run_root)?;
    let mut active_scopes = in_use_scopes.clone();
    match crate::capacity::active_scopes(run_root, LEASE_STALE_AFTER) {
        Ok(scopes) => active_scopes.extend(scopes),
        Err(error) if force_no_lease_check => {
            eprintln!("WARNING: bypassing active-scope lease check: {error:#}");
        }
        Err(error) => return Err(error).context("read active cache-scope leases"),
    }
    Ok(active_scopes)
}

fn append_gc_history(
    log_root: &Path,
    candidate: &EvictionCandidate,
    policy: Option<&EvictionPolicy>,
    outcome: &str,
) -> Result<()> {
    fs::create_dir_all(log_root)
        .with_context(|| format!("create GC log dir {}", log_root.display()))?;
    let path = log_root.join("gc-history.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let policy = policy.map_or_else(
        || serde_json::json!({ "mode": "reclaim-target" }),
        |policy| {
            serde_json::json!({
                "keep_newest_per_target_scope": policy.keep_newest_per_target_scope,
                "max_age_seconds": policy.max_age.as_secs(),
                "max_total_bytes": policy.max_total_bytes,
                "class_budgets": policy.class_budgets.iter().map(|(store, bytes)| {
                    (store.to_string(), *bytes)
                }).collect::<BTreeMap<_, _>>(),
            })
        },
    );
    let line = serde_json::json!({
        "store": candidate.store.to_string(),
        "scope": candidate.scope_key(),
        "logical_bytes": candidate.bytes,
        "reason": candidate.reason,
        "path": candidate.path,
        "outcome": outcome,
        "policy": policy,
    });
    writeln!(file, "{line}")?;
    eprintln!("gc.history {line}");
    Ok(())
}

#[derive(Debug, Clone)]
struct StoreRoot {
    kind: CacheStore,
    path: PathBuf,
    scope_prefix: Vec<String>,
    scope_depth: usize,
    candidate_depth: usize,
    gc_managed: bool,
    emergency_managed: bool,
}

fn store_roots(work_root: &Path) -> Vec<StoreRoot> {
    let layout = crate::storage::StorageLayout::resolve();
    store_roots_with_layout(work_root, layout.as_ref())
}

fn store_roots_with_layout(
    work_root: &Path,
    layout: Option<&crate::storage::StorageLayout>,
) -> Vec<StoreRoot> {
    // Every path below comes from the catalog. GC must never spell a store root
    // itself: that is exactly how the artifact store came to be written at
    // `<work>/slot-N/_velnor_artifacts` while GC swept `<work>/_velnor_artifacts`.
    let catalog = crate::store_catalog::StoreCatalog::for_work_root_with_layout(work_root, layout);
    let cargo = catalog.cargo();
    let cargo_bin = cargo.join("bin");
    let cargo_bin_legacy = is_legacy_store(&cargo);
    let mise = catalog.mise();
    let mise_legacy = is_legacy_store(&mise);
    let targets = catalog.targets();
    let targets_legacy = is_legacy_store(&targets);
    let actions_cache = catalog.actions_cache();
    let actions_cache_legacy = is_legacy_store(&actions_cache);
    let mut stores = vec![
        StoreRoot {
            kind: CacheStore::Cargo,
            path: cargo.join("registry"),
            scope_prefix: vec!["registry".into()],
            scope_depth: 0,
            candidate_depth: 0,
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Cargo,
            path: cargo.join("git"),
            scope_prefix: vec!["git".into()],
            scope_depth: 0,
            candidate_depth: 0,
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Cargo,
            path: cargo_bin,
            scope_prefix: vec!["bin".into()],
            scope_depth: if cargo_bin_legacy { 2 } else { 1 },
            candidate_depth: if cargo_bin_legacy { 2 } else { 1 },
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Mise,
            path: mise.join("cache"),
            scope_prefix: vec!["cache".into()],
            scope_depth: 0,
            candidate_depth: 0,
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Mise,
            path: mise.join("installs"),
            scope_prefix: vec!["installs".into()],
            scope_depth: if mise_legacy { 2 } else { 1 },
            candidate_depth: if mise_legacy { 2 } else { 1 },
            gc_managed: true,
            emergency_managed: true,
        },
        // Plan 008: persistent per-version mise binaries, same trust/repository
        // boundary and mise budget as installs.
        StoreRoot {
            kind: CacheStore::Mise,
            path: mise.join("binaries"),
            scope_prefix: vec!["binaries".into()],
            scope_depth: if mise_legacy { 2 } else { 1 },
            candidate_depth: if mise_legacy { 2 } else { 1 },
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Mise,
            path: mise.join("rustup"),
            scope_prefix: vec!["rustup".into()],
            scope_depth: if mise_legacy { 2 } else { 1 },
            candidate_depth: if mise_legacy { 2 } else { 1 },
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Targets,
            path: targets,
            scope_prefix: Vec::new(),
            // The existing job bucket remains the ownership scope. Immutable
            // target generations are one directory below it.
            scope_depth: if targets_legacy { 5 } else { 4 },
            candidate_depth: if targets_legacy { 6 } else { 5 },
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::ActionsCache,
            path: actions_cache,
            scope_prefix: Vec::new(),
            scope_depth: if actions_cache_legacy { 2 } else { 1 },
            candidate_depth: if actions_cache_legacy { 3 } else { 2 },
            gc_managed: true,
            emergency_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Artifacts,
            path: catalog.artifacts(),
            scope_prefix: Vec::new(),
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
            emergency_managed: true,
        },
    ];
    for (trust_class, trust_scope) in crate::store_catalog::TRUST_SCOPES {
        for (kind, path) in [
            (CacheStore::Mbx, catalog.mbx(trust_scope)),
            (CacheStore::Sccache, catalog.sccache(trust_class)),
        ] {
            stores.push(StoreRoot {
                kind,
                path,
                scope_prefix: Vec::new(),
                scope_depth: 1,
                candidate_depth: 1,
                gc_managed: false,
                emergency_managed: true,
            });
        }
    }
    // The hosted actions-cache service is durable storage like any other class.
    // It was previously invisible to `cache du` and to every collector, so each
    // tenant accumulated its own budget outside the ledger.
    if let Some(layout) = layout {
        stores.push(StoreRoot {
            kind: CacheStore::GhaCache,
            path: crate::store_catalog::gha_cache_root(layout).join("tenants"),
            scope_prefix: Vec::new(),
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
            emergency_managed: true,
        });
    }
    stores
}

fn is_legacy_store(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("_velnor_"))
}

fn scoped_sizes(store: &StoreRoot) -> Result<BTreeMap<String, u64>> {
    let mut sizes = BTreeMap::new();
    if !store.path.exists() {
        return Ok(sizes);
    }
    collect_scoped_sizes(
        &store.path,
        &store.path,
        store.scope_depth,
        &store.scope_prefix,
        &mut sizes,
    )?;
    Ok(sizes)
}

fn collect_scoped_sizes(
    root: &Path,
    path: &Path,
    scope_depth: usize,
    scope_prefix: &[String],
    sizes: &mut BTreeMap<String, u64>,
) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if metadata.is_file() {
        let scope = scope_for(root, path, scope_depth, scope_prefix);
        *sizes.entry(scope).or_default() += metadata.len();
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0;
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        total += collect_scoped_sizes(root, &entry?.path(), scope_depth, scope_prefix, sizes)?;
    }
    Ok(total)
}

fn cache_listing_with_layout(
    work_root: &Path,
    emergency: bool,
    layout: Option<&crate::storage::StorageLayout>,
) -> Result<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    for store in store_roots_with_layout(work_root, layout)
        .into_iter()
        .filter(|store| {
            if emergency {
                store.emergency_managed
            } else {
                store.gc_managed
            }
        })
    {
        collect_candidates(&store, &store.path, 0, &mut entries)?;
    }
    Ok(entries)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReclaimReport {
    pub freed_bytes: u64,
    pub deleted: Vec<PathBuf>,
    pub failures: Vec<String>,
}

pub fn reclaim(
    layout: &crate::storage::StorageLayout,
    target_bytes: u64,
    in_use_scopes: &BTreeSet<String>,
) -> Result<ReclaimReport> {
    reclaim_work_root_with_layout(
        &layout.cache_root,
        &layout.run_root,
        &layout.log_root,
        target_bytes,
        in_use_scopes,
        false,
        Some(layout),
    )
}

/// Reclaim cache storage across all discovered daemon work roots.
///
/// Disk pressure is best effort: one unavailable root must not prevent the
/// remaining roots from being reclaimed or make the caller fail open. Each
/// root retains the lease and filesystem coordination enforced by
/// [`reclaim_work_root`].
pub fn reclaim_for_disk_pressure(target_bytes: u64) -> ReclaimReport {
    let roots = crate::leftover_disk::discover_daemon_work_roots();
    let layout = crate::storage::StorageLayout::resolve();
    reclaim_for_disk_pressure_with_context(target_bytes, &roots, layout.as_ref())
}

/// Reclaim one work root using the current process storage layout.
///
/// The explicit-layout variant below is the implementation seam used by
/// callers that already hold a configuration snapshot. This wrapper preserves
/// the existing crate-local test and operator entry point for callers that
/// provide only filesystem roots.
#[allow(dead_code, reason = "crate-local tests exercise the wrapper")]
pub(crate) fn reclaim_work_root(
    work_root: &Path,
    run_root: &Path,
    log_root: &Path,
    target_bytes: u64,
    in_use_scopes: &BTreeSet<String>,
    emergency: bool,
) -> Result<ReclaimReport> {
    let layout = crate::storage::StorageLayout::resolve();
    reclaim_work_root_with_layout(
        work_root,
        run_root,
        log_root,
        target_bytes,
        in_use_scopes,
        emergency,
        layout.as_ref(),
    )
}

fn reclaim_for_disk_pressure_with_context(
    target_bytes: u64,
    roots: &[PathBuf],
    layout: Option<&crate::storage::StorageLayout>,
) -> ReclaimReport {
    let mut report = ReclaimReport::default();

    for work_root in roots {
        let (run_root, log_root) = layout
            .as_ref()
            .map(|layout| (layout.run_root.clone(), layout.log_root.clone()))
            .unwrap_or_else(|| {
                (
                    work_root.join("_velnor_runtime"),
                    work_root.join("_velnor_logs"),
                )
            });
        let remaining = target_bytes.saturating_sub(report.freed_bytes);
        if remaining == 0 {
            break;
        }
        match reclaim_work_root_with_layout(
            work_root,
            &run_root,
            &log_root,
            remaining,
            &BTreeSet::new(),
            true,
            layout,
        ) {
            Ok(root_report) => {
                report.freed_bytes = report.freed_bytes.saturating_add(root_report.freed_bytes);
                report.deleted.extend(root_report.deleted);
                report.failures.extend(root_report.failures);
            }
            Err(error) => report
                .failures
                .push(format!("{}: {error:#}", work_root.display())),
        }
    }

    report
}

fn reclaim_work_root_with_layout(
    work_root: &Path,
    run_root: &Path,
    log_root: &Path,
    target_bytes: u64,
    in_use_scopes: &BTreeSet<String>,
    emergency: bool,
    layout: Option<&crate::storage::StorageLayout>,
) -> Result<ReclaimReport> {
    let _lock = match GcLeaderLock::acquire(run_root) {
        Ok(lock) => lock,
        Err(error) if error.to_string().contains("another gc holds the lock") => {
            eprintln!("capacity reclaim already running in another daemon; rechecking later");
            return Ok(ReclaimReport::default());
        }
        Err(error) => return Err(error),
    };
    let active_scopes = snapshot_active_scopes(run_root, in_use_scopes, false)?;
    let mut entries = cache_listing_with_layout(work_root, emergency, layout)?;
    let policy = EvictionPolicy {
        now: SystemTime::now(),
        keep_newest_per_target_scope: 0,
        max_age: Duration::ZERO,
        max_total_bytes: None,
        class_budgets: BTreeMap::new(),
        in_use_scopes: active_scopes,
        protected_paths: pointer_protected_target_generations_with_layout(work_root, layout),
    };
    entries.retain(|entry| !in_use(entry, &policy) && !protected(entry, &policy));
    if emergency {
        // Leases are the primary liveness evidence, but the emergency path also
        // reaches classes whose lease has not been published for the job that
        // owns them. A store a live job is writing is never idle, so refuse to
        // delete anything touched inside the idle floor. This is the fail-safe
        // that stops emergency reclaim from deleting an unleased store out from
        // under a running job; the lease classes in `store_catalog` are the
        // primary fix.
        let now = policy.now;
        entries.retain(|entry| {
            // Never touch a class Velnor does not own by scope.
            entry.store.lease_class().is_some()
                && now
                    .duration_since(entry.modified)
                    .is_ok_and(|idle| idle >= EMERGENCY_MIN_IDLE)
        });
    }
    entries.sort_by(|left, right| {
        reclaim_priority(left.store)
            .cmp(&reclaim_priority(right.store))
            .then_with(|| left.modified.cmp(&right.modified))
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut report = ReclaimReport::default();
    for entry in entries {
        if report.freed_bytes >= target_bytes {
            break;
        }
        let candidate = EvictionCandidate {
            path: entry.path,
            store: entry.store,
            scope: entry.scope,
            bytes: entry.bytes,
            reason: "reclaim-target".into(),
        };
        if !cache_candidate_is_still_unclaimed(run_root, &candidate, &active_scopes, false)? {
            append_gc_history(log_root, &candidate, None, "skipped-live")?;
            continue;
        }
        match remove_candidate(&candidate) {
            Ok(()) => {
                report.freed_bytes = report.freed_bytes.saturating_add(candidate.bytes);
                report.deleted.push(candidate.path.clone());
                append_gc_history(log_root, &candidate, None, "deleted")?;
            }
            Err(error) => {
                report
                    .failures
                    .push(format!("{}: {error}", candidate.path.display()));
                append_gc_history(log_root, &candidate, None, "failed")?;
            }
        }
    }
    // Deliberate safety decision: emergency reclaim does not prune BuildKit.
    // `prune_owned_builder` can enumerate Velnor's builders, but this reclaimer
    // has no BuildKit lease or job-claim covering the builder's content. A
    // builder can therefore be live while its cache path is idle, and
    // `buildx prune --max-used-space 0B` is destructive in exactly that window.
    // Keep the explicit operator function available; only a future caller that
    // holds the matching BuildKit lease may invoke it.
    Ok(report)
}

fn cache_candidate_is_still_unclaimed(
    run_root: &Path,
    candidate: &EvictionCandidate,
    initially_active: &BTreeSet<String>,
    force_no_lease_check: bool,
) -> Result<bool> {
    let active_scopes = snapshot_active_scopes(run_root, initially_active, force_no_lease_check)?;
    Ok(!cache_scope_is_in_use(
        candidate.store,
        &candidate.scope,
        &active_scopes,
    ))
}

fn remove_candidate(candidate: &EvictionCandidate) -> Result<()> {
    let lock_path = if candidate.store == CacheStore::Targets {
        candidate.path.parent().unwrap_or(&candidate.path)
    } else {
        &candidate.path
    };
    let _entry_lock = (matches!(
        candidate.store,
        CacheStore::ActionsCache | CacheStore::Targets
    ))
    .then(|| CacheEntryLock::exclusive(lock_path))
    .transpose()?;
    if candidate.store == CacheStore::Targets {
        if target_generation_is_current(&candidate.path)? {
            bail!(
                "refusing to remove current target generation: {}",
                candidate.path.display()
            );
        }
        if !target_generation_is_complete(&candidate.path) {
            bail!(
                "target generation changed or became incomplete before removal: {}",
                candidate.path.display()
            );
        }
        let parent = candidate
            .path
            .parent()
            .context("target generation has no parent")?;
        let name = candidate
            .path
            .file_name()
            .context("target generation has no name")?;
        let parent = crate::fs_copy::NoFollowDestinationDir::open_absolute_no_follow(parent)?;
        return parent.remove_tree_entry(name);
    }
    fs::remove_dir_all(&candidate.path)
        .with_context(|| format!("remove cache candidate {}", candidate.path.display()))
}

/// Names of the buildx builders Velnor owns, from `docker buildx ls` output.
///
/// The builder name is always `velnor-builder-<scope>`; inspecting the literal
/// prefix `velnor-builder` therefore never succeeded, which made the whole
/// disk-pressure BuildKit reclaim a silent no-op. Ownership is the prefix, so
/// enumerate and match the prefix instead of guessing one name.
pub(crate) fn owned_builder_names(buildx_ls_stdout: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in buildx_ls_stdout.lines() {
        let Some(first) = line.split_whitespace().next() else {
            continue;
        };
        // `docker buildx ls` marks the selected builder with a trailing `*` and
        // indents each builder's nodes; nodes are not builders.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let name = first.trim_end_matches('*');
        if name.starts_with(OWNED_BUILDER_PREFIX) && !names.iter().any(|seen| seen == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// Explicitly prune every Velnor-owned BuildKit builder down to
/// `max_used_space_bytes`. Emergency reclaim intentionally does not call this:
/// the builder content has no lease/claim boundary yet.
#[allow(
    dead_code,
    reason = "operator action requires an explicit BuildKit lease"
)]
pub fn prune_owned_builder(max_used_space_bytes: u64) -> Result<bool> {
    let Ok(listed) = std::process::Command::new("docker")
        .args(["buildx", "ls"])
        .output()
    else {
        return Ok(false);
    };
    if !listed.status.success() {
        return Ok(false);
    }
    let builders = owned_builder_names(&String::from_utf8_lossy(&listed.stdout));
    let limit = format!("{max_used_space_bytes}B");
    let mut pruned = false;
    for builder in builders {
        let output = std::process::Command::new("docker")
            .args([
                "buildx",
                "prune",
                "--builder",
                &builder,
                "--force",
                "--max-used-space",
                &limit,
            ])
            .output()
            .context("prune Velnor-owned buildx builder")?;
        if output.status.success() {
            pruned = true;
            continue;
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No such container") || stderr.contains("no builder") {
            continue;
        }
        bail!(
            "Velnor-owned buildx builder prune failed for {builder}: {}: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(pruned)
}

fn reclaim_priority(store: CacheStore) -> u8 {
    match store {
        CacheStore::Artifacts => 0,
        CacheStore::GhaCache => 1,
        CacheStore::ActionsCache => 2,
        CacheStore::Targets => 3,
        CacheStore::Cargo => 4,
        CacheStore::Mise => 5,
        CacheStore::Mbx | CacheStore::Sccache => 6,
        // Never reclaimed by scope: Velnor does not own Docker's store beyond
        // its own builder. It is accounted, not evicted.
        CacheStore::Docker => u8::MAX,
    }
}

fn collect_candidates(
    store: &StoreRoot,
    path: &Path,
    depth: usize,
    entries: &mut Vec<CacheEntry>,
) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if !metadata.is_dir() {
        return Ok(());
    }
    if depth >= store.candidate_depth {
        if store.kind == CacheStore::Targets {
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                return Ok(());
            }
            let Some((bytes, modified)) = target_generation_size(path) else {
                return Ok(());
            };
            if bytes > 0 {
                entries.push(CacheEntry {
                    path: path.to_path_buf(),
                    store: store.kind,
                    scope: store
                        .scope_prefix
                        .iter()
                        .cloned()
                        .chain(scope_parts(&store.path, path, store.scope_depth))
                        .filter(|part| part != ".")
                        .collect(),
                    bytes,
                    modified,
                });
            }
            return Ok(());
        }
        let (bytes, modified) = size_and_modified(path)?;
        if bytes > 0 {
            entries.push(CacheEntry {
                path: path.to_path_buf(),
                store: store.kind,
                scope: store
                    .scope_prefix
                    .iter()
                    .cloned()
                    .chain(scope_parts(&store.path, path, store.scope_depth))
                    .filter(|part| part != ".")
                    .collect(),
                bytes,
                modified,
            });
        }
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            collect_candidates(store, &entry.path(), depth + 1, entries)?;
        }
    }
    Ok(())
}

fn size_physical_and_modified(path: &Path) -> Result<(u64, u64, SystemTime)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((0, 0, SystemTime::UNIX_EPOCH));
        }
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if metadata.is_file() {
        return Ok((
            metadata.len(),
            metadata.blocks().saturating_mul(512),
            modified,
        ));
    }
    if !metadata.is_dir() {
        return Ok((0, 0, modified));
    }
    let mut logical: u64 = 0;
    let mut physical: u64 = 0;
    let mut newest = modified;
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let (child_logical, child_physical, child_modified) =
            size_physical_and_modified(&entry?.path())?;
        logical = logical.saturating_add(child_logical);
        physical = physical.saturating_add(child_physical);
        newest = newest.max(child_modified);
    }
    Ok((logical, physical, newest))
}

fn size_and_modified(path: &Path) -> Result<(u64, SystemTime)> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((0, SystemTime::UNIX_EPOCH));
        }
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if metadata.is_file() {
        return Ok((metadata.len(), modified));
    }
    if !metadata.is_dir() {
        return Ok((0, modified));
    }

    let mut bytes = 0;
    let mut newest = modified;
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let (child_bytes, child_modified) = size_and_modified(&entry?.path())?;
        bytes += child_bytes;
        if child_modified > newest {
            newest = child_modified;
        }
    }
    Ok((bytes, newest))
}

fn scope_for(root: &Path, path: &Path, scope_depth: usize, scope_prefix: &[String]) -> String {
    scope_prefix
        .iter()
        .cloned()
        .chain(scope_parts(root, path, scope_depth))
        .filter(|part| part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn scope_parts(root: &Path, path: &Path, scope_depth: usize) -> Vec<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut parts: Vec<String> = relative
        .components()
        .take(scope_depth)
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();
    if parts.is_empty() {
        parts.push(".".to_string());
    }
    parts
}

/// GC's store classes are the catalog's store classes. Two enums would let the
/// collector recognize a class the catalog does not publish (or the reverse),
/// which is the same drift that hid the artifact store.
pub(crate) use crate::store_catalog::StoreClass as CacheStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheEntry {
    pub(crate) path: PathBuf,
    pub(crate) store: CacheStore,
    pub(crate) scope: Vec<String>,
    pub(crate) bytes: u64,
    pub(crate) modified: SystemTime,
}

impl CacheEntry {
    fn scope_key(&self) -> String {
        self.scope.join("/")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EvictionPolicy {
    pub(crate) now: SystemTime,
    pub(crate) keep_newest_per_target_scope: usize,
    pub(crate) max_age: Duration,
    pub(crate) max_total_bytes: Option<u64>,
    pub(crate) class_budgets: BTreeMap<CacheStore, u64>,
    pub(crate) in_use_scopes: BTreeSet<String>,
    pub(crate) protected_paths: BTreeSet<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvictionCandidate {
    pub(crate) path: PathBuf,
    pub(crate) store: CacheStore,
    pub(crate) scope: Vec<String>,
    pub(crate) bytes: u64,
    pub(crate) reason: String,
}

impl EvictionCandidate {
    fn scope_key(&self) -> String {
        self.scope.join("/")
    }
}

pub(crate) fn select_eviction_candidates(
    entries: &[CacheEntry],
    policy: &EvictionPolicy,
) -> Vec<EvictionCandidate> {
    let mut candidates: BTreeMap<PathBuf, EvictionCandidate> = BTreeMap::new();

    for entry in entries
        .iter()
        .filter(|entry| !in_use(entry, policy) && !protected(entry, policy))
    {
        if is_older_than(entry.modified, policy.now, policy.max_age) {
            add_candidate(&mut candidates, entry, "older-than-max-age");
        }
    }

    let mut target_scopes: BTreeMap<String, Vec<&CacheEntry>> = BTreeMap::new();
    for entry in entries.iter().filter(|entry| {
        entry.store == CacheStore::Targets && !in_use(entry, policy) && !protected(entry, policy)
    }) {
        target_scopes
            .entry(entry.scope_key())
            .or_default()
            .push(entry);
    }
    for scoped_entries in target_scopes.values_mut() {
        scoped_entries.sort_by(|left, right| {
            right
                .modified
                .cmp(&left.modified)
                .then_with(|| left.path.cmp(&right.path))
        });
        for entry in scoped_entries
            .iter()
            .skip(policy.keep_newest_per_target_scope)
        {
            add_candidate(&mut candidates, entry, "target-scope-retention");
        }
    }

    if let Some(max_total_bytes) = policy.max_total_bytes {
        let total: u64 = entries.iter().map(|entry| entry.bytes).sum();
        if total > max_total_bytes {
            let mut remaining = total;
            let mut oldest: Vec<&CacheEntry> = entries
                .iter()
                .filter(|entry| !in_use(entry, policy) && !protected(entry, policy))
                .collect();
            oldest.sort_by(|left, right| {
                left.modified
                    .cmp(&right.modified)
                    .then_with(|| left.path.cmp(&right.path))
            });
            for entry in oldest {
                if remaining <= max_total_bytes {
                    break;
                }
                remaining = remaining.saturating_sub(entry.bytes);
                add_candidate(&mut candidates, entry, "over-byte-ceiling");
            }
        }
    }

    for (store, budget) in &policy.class_budgets {
        if *budget == 0 {
            continue;
        }
        let mut remaining: u64 = entries
            .iter()
            .filter(|entry| entry.store == *store)
            .map(|entry| entry.bytes)
            .sum();
        let mut oldest: Vec<&CacheEntry> = entries
            .iter()
            .filter(|entry| {
                entry.store == *store && !in_use(entry, policy) && !protected(entry, policy)
            })
            .collect();
        oldest.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        });
        for entry in oldest {
            if remaining <= *budget {
                break;
            }
            remaining = remaining.saturating_sub(entry.bytes);
            add_candidate(&mut candidates, entry, "over-class-budget");
        }
    }

    candidates.into_values().collect()
}

fn protected(entry: &CacheEntry, policy: &EvictionPolicy) -> bool {
    policy.protected_paths.contains(&entry.path)
}

fn add_candidate(
    candidates: &mut BTreeMap<PathBuf, EvictionCandidate>,
    entry: &CacheEntry,
    reason: &str,
) {
    candidates
        .entry(entry.path.clone())
        .and_modify(|candidate| {
            if !candidate
                .reason
                .split(',')
                .any(|existing| existing == reason)
            {
                candidate.reason.push(',');
                candidate.reason.push_str(reason);
            }
        })
        .or_insert_with(|| EvictionCandidate {
            path: entry.path.clone(),
            store: entry.store,
            scope: entry.scope.clone(),
            bytes: entry.bytes,
            reason: reason.to_string(),
        });
}

fn in_use(entry: &CacheEntry, policy: &EvictionPolicy) -> bool {
    cache_scope_is_in_use(entry.store, &entry.scope, &policy.in_use_scopes)
}

fn cache_scope_is_in_use(
    store: CacheStore,
    scope: &[String],
    active_scopes: &BTreeSet<String>,
) -> bool {
    let candidate = format!("{store}/{}", scope.join("/"));
    active_scopes.iter().any(|active| {
        candidate == *active
            || candidate
                .strip_prefix(active)
                .is_some_and(|suffix| suffix.starts_with('/'))
            || active
                .strip_prefix(&candidate)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn is_older_than(modified: SystemTime, now: SystemTime, max_age: Duration) -> bool {
    now.duration_since(modified).is_ok_and(|age| age > max_age)
}

fn target_generation_is_complete(path: &Path) -> bool {
    target_generation_size(path).is_some()
}

fn target_generation_size(path: &Path) -> Option<(u64, SystemTime)> {
    let generation = crate::fs_copy::NoFollowDir::open_absolute(path).ok()?;
    let marker = generation
        .open_source(Path::new(".velnor-target-complete-v1"))
        .ok()??;
    if !matches!(marker, crate::fs_copy::NoFollowSource::File(_)) {
        return None;
    }
    let Some(crate::fs_copy::NoFollowSource::Directory(data)) =
        generation.open_source(Path::new("data")).ok()?
    else {
        return None;
    };
    secure_target_tree_size(&data).ok()
}

#[derive(Debug, Default)]
struct TargetTraversalBudget {
    nodes: usize,
    directories: usize,
    path_bytes: u64,
}

impl TargetTraversalBudget {
    fn visit(&mut self, relative: &Path, directory: bool) -> Result<()> {
        let depth = relative.components().count();
        if depth > PERSISTENT_TARGET_MAX_DEPTH {
            bail!(
                "persistent target path exceeds the {}-component depth limit",
                PERSISTENT_TARGET_MAX_DEPTH
            );
        }
        self.nodes = self
            .nodes
            .checked_add(1)
            .context("persistent target node count overflowed")?;
        if self.nodes > PERSISTENT_TARGET_MAX_NODES {
            bail!(
                "persistent target traversal visited more than the {}-node limit",
                PERSISTENT_TARGET_MAX_NODES
            );
        }
        self.path_bytes = self
            .path_bytes
            .checked_add(
                u64::try_from(relative.as_os_str().as_encoded_bytes().len()).unwrap_or(u64::MAX),
            )
            .context("persistent target path byte count overflowed")?;
        if self.path_bytes > PERSISTENT_TARGET_MAX_PATH_BYTES {
            bail!(
                "persistent target paths exceed the {}-byte limit",
                PERSISTENT_TARGET_MAX_PATH_BYTES
            );
        }
        if directory {
            self.directories = self
                .directories
                .checked_add(1)
                .context("persistent target directory count overflowed")?;
            if self.directories > PERSISTENT_TARGET_MAX_DIRECTORIES {
                bail!(
                    "persistent target traversal visited more than the {}-directory limit",
                    PERSISTENT_TARGET_MAX_DIRECTORIES
                );
            }
        }
        Ok(())
    }
}

fn secure_target_tree_size(directory: &crate::fs_copy::NoFollowDir) -> Result<(u64, SystemTime)> {
    let mut budget = TargetTraversalBudget::default();
    secure_target_tree_size_with_budget(directory, Path::new(""), &mut budget)
}

fn secure_target_tree_size_with_budget(
    directory: &crate::fs_copy::NoFollowDir,
    relative: &Path,
    budget: &mut TargetTraversalBudget,
) -> Result<(u64, SystemTime)> {
    budget.visit(relative, true)?;
    let mut bytes = 0u64;
    let mut newest = SystemTime::UNIX_EPOCH;
    directory.for_each_entry_filtered(
        |_| true,
        |entry| match entry.source {
            crate::fs_copy::NoFollowSource::File(file) => {
                budget.visit(&relative.join(&entry.name), false)?;
                let metadata = file.metadata().context("inspect target generation file")?;
                if !metadata.is_file() {
                    bail!("target generation contains a non-regular file");
                }
                bytes = bytes.saturating_add(metadata.len());
                newest = newest.max(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));
                Ok(())
            }
            crate::fs_copy::NoFollowSource::Directory(directory) => {
                let child_relative = relative.join(&entry.name);
                let (child_bytes, child_newest) =
                    secure_target_tree_size_with_budget(&directory, &child_relative, budget)?;
                bytes = bytes.saturating_add(child_bytes);
                newest = newest.max(child_newest);
                Ok(())
            }
        },
    )?;
    Ok((bytes, newest))
}

fn current_pointer_generation(path: &Path) -> Option<String> {
    current_pointer_generation_checked(path).ok().flatten()
}

fn current_pointer_generation_checked(path: &Path) -> Result<Option<String>> {
    let directory = crate::fs_copy::NoFollowDir::open_absolute(path)
        .with_context(|| format!("open target scope {}", path.display()))?;
    let Some(crate::fs_copy::NoFollowSource::File(pointer)) =
        directory.open_source(Path::new("current"))?
    else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    pointer
        .take(129)
        .read_to_end(&mut bytes)
        .context("read target current pointer")?;
    if bytes.len() > 128 {
        bail!("target current pointer exceeds the 128-byte limit");
    }
    let value = String::from_utf8(bytes).context("target current pointer is not UTF-8")?;
    let mut lines = value.lines();
    let Some(generation) = lines.next() else {
        bail!("target current pointer is empty");
    };
    if lines.next().is_some()
        || !value.ends_with('\n')
        || generation.is_empty()
        || generation.len() > 128
        || !generation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("target current pointer is malformed");
    }
    Ok(Some(generation.to_owned()))
}

fn target_generation_is_current(path: &Path) -> Result<bool> {
    let parent = path.parent().context("target generation has no parent")?;
    let name = path
        .file_name()
        .context("target generation has no name")?
        .to_string_lossy();
    Ok(current_pointer_generation_checked(parent)?.as_deref() == Some(name.as_ref()))
}

fn pointer_protected_target_generations_with_layout(
    work_root: &Path,
    layout: Option<&crate::storage::StorageLayout>,
) -> BTreeSet<PathBuf> {
    let mut protected = BTreeSet::new();
    for store in store_roots_with_layout(work_root, layout)
        .into_iter()
        .filter(|store| store.kind == CacheStore::Targets)
    {
        collect_pointer_protected(&store.path, store.scope_depth, &mut protected);
    }
    protected
}

fn collect_pointer_protected(path: &Path, depth: usize, protected: &mut BTreeSet<PathBuf>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    if depth == 0 {
        let Some(generation) = current_pointer_generation(path) else {
            return;
        };
        let generation_path = path.join(generation);
        if target_generation_is_complete(&generation_path) {
            protected.insert(generation_path);
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_pointer_protected(&entry.path(), depth - 1, protected);
        }
    }
}

#[cfg(test)]
pub(crate) mod test_clock {
    use std::path::Path;
    use std::time::Duration;

    /// Backdate every node under `path` so an emergency-reclaim fixture models
    /// a cold store rather than one a live job just touched.
    pub(crate) fn backdate(path: &Path, age: Duration) {
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && metadata.is_dir()
        {
            for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
                backdate(&entry.path(), age);
            }
        }
        let when = std::time::SystemTime::now() - age;
        let stamp = rustix::fs::Timespec {
            tv_sec: when
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            tv_nsec: 0,
        };
        let _ = rustix::fs::utimensat(
            rustix::fs::CWD,
            path,
            &rustix::fs::Timestamps {
                last_access: stamp,
                last_modification: stamp,
            },
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_clock::backdate;

    fn entry(
        path: &str,
        store: CacheStore,
        scope: &[&str],
        age_days: u64,
        bytes: u64,
    ) -> CacheEntry {
        CacheEntry {
            path: PathBuf::from(path),
            store,
            scope: scope.iter().map(|value| value.to_string()).collect(),
            bytes,
            modified: SystemTime::UNIX_EPOCH + DAY * (100 - age_days as u32),
        }
    }

    fn policy() -> EvictionPolicy {
        EvictionPolicy {
            now: SystemTime::UNIX_EPOCH + DAY * 100,
            keep_newest_per_target_scope: 2,
            max_age: DAY * 30,
            max_total_bytes: None,
            class_budgets: BTreeMap::new(),
            in_use_scopes: BTreeSet::new(),
            protected_paths: BTreeSet::new(),
        }
    }

    #[test]
    fn cache_du_scopes_include_store_prefixes() {
        let root = std::env::temp_dir().join(format!("velnor-du-scope-{}", uuid::Uuid::new_v4()));
        let registry = root.join("registry");
        fs::create_dir_all(registry.join("cache/index")).unwrap();
        fs::write(registry.join("cache/index/crate"), vec![0; 7]).unwrap();
        let registry_store = StoreRoot {
            kind: CacheStore::Cargo,
            path: registry,
            scope_prefix: vec!["registry".into()],
            scope_depth: 0,
            candidate_depth: 0,
            gc_managed: true,
            emergency_managed: true,
        };

        let bin = root.join("bin");
        fs::create_dir_all(bin.join("repository")).unwrap();
        fs::write(bin.join("repository/tool"), vec![0; 11]).unwrap();
        let bin_store = StoreRoot {
            kind: CacheStore::Cargo,
            path: bin,
            scope_prefix: vec!["bin".into()],
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
            emergency_managed: true,
        };

        assert_eq!(
            scoped_sizes(&registry_store).unwrap(),
            BTreeMap::from([("registry".to_string(), 7)])
        );
        assert_eq!(
            scoped_sizes(&bin_store).unwrap(),
            BTreeMap::from([("bin/repository".to_string(), 11)])
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_gc_keeps_newest_target_buckets_per_scope() {
        let entries = vec![
            entry(
                "/target/old",
                CacheStore::Targets,
                &["trusted", "repo", "wf", "job"],
                20,
                1,
            ),
            entry(
                "/target/new",
                CacheStore::Targets,
                &["trusted", "repo", "wf", "job"],
                1,
                1,
            ),
            entry(
                "/target/mid",
                CacheStore::Targets,
                &["trusted", "repo", "wf", "job"],
                10,
                1,
            ),
            entry(
                "/target/other",
                CacheStore::Targets,
                &["trusted", "repo", "wf", "other"],
                40,
                1,
            ),
        ];

        let candidates = select_eviction_candidates(&entries, &policy());

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("/target/old"), Path::new("/target/other")]
        );
        assert!(candidates[0].reason.contains("target-scope-retention"));
        assert!(candidates[1].reason.contains("older-than-max-age"));
    }

    #[test]
    fn cache_gc_uses_age_and_byte_ceiling() {
        let entries = vec![
            entry(
                "/cache/old",
                CacheStore::ActionsCache,
                &["trusted", "repo"],
                31,
                30,
            ),
            entry(
                "/cache/mid",
                CacheStore::ActionsCache,
                &["trusted", "repo"],
                20,
                50,
            ),
            entry(
                "/cache/new",
                CacheStore::ActionsCache,
                &["trusted", "repo"],
                1,
                40,
            ),
        ];
        let mut policy = policy();
        policy.max_total_bytes = Some(60);

        let candidates = select_eviction_candidates(&entries, &policy);

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.path.as_path())
                .collect::<Vec<_>>(),
            vec![Path::new("/cache/mid"), Path::new("/cache/old")]
        );
        assert!(candidates
            .iter()
            .any(|candidate| candidate.reason.contains("older-than-max-age")));
        assert!(candidates
            .iter()
            .any(|candidate| candidate.reason.contains("over-byte-ceiling")));
    }

    #[test]
    fn cache_gc_enforces_per_class_budget_oldest_first() {
        let entries = vec![
            entry("/cache/old", CacheStore::ActionsCache, &["old"], 10, 60),
            entry("/cache/new", CacheStore::ActionsCache, &["new"], 1, 50),
        ];
        let mut policy = policy();
        policy.max_age = DAY * 365;
        policy.class_budgets.insert(CacheStore::ActionsCache, 60);
        let candidates = select_eviction_candidates(&entries, &policy);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, Path::new("/cache/old"));
        assert!(candidates[0].reason.contains("over-class-budget"));
    }

    #[test]
    fn cache_gc_skips_in_use_scopes() {
        let entries = vec![
            entry(
                "/cache/active",
                CacheStore::ActionsCache,
                &["trusted", "active"],
                90,
                100,
            ),
            entry(
                "/cache/idle",
                CacheStore::ActionsCache,
                &["trusted", "idle"],
                90,
                100,
            ),
        ];
        let mut policy = policy();
        policy
            .in_use_scopes
            .insert("actions-cache/trusted/active".to_string());

        let candidates = select_eviction_candidates(&entries, &policy);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, PathBuf::from("/cache/idle"));
    }

    #[cfg(unix)]
    #[test]
    fn target_gc_ignores_incomplete_and_symlink_generations() {
        let temp_root =
            fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
        let root = temp_root.join(format!("velnor-target-gc-{}", uuid::Uuid::new_v4()));
        let store = root.join("targets");
        let class = store
            .join("workspace-v4-success-only")
            .join("repo")
            .join("workflow")
            .join("job");
        let complete = class.join("target-generation-complete");
        fs::create_dir_all(complete.join("data")).unwrap();
        fs::write(complete.join("data/output"), b"output").unwrap();
        fs::write(complete.join(".velnor-target-complete-v1"), b"complete\n").unwrap();

        let incomplete = class.join("target-generation-incomplete");
        fs::create_dir_all(incomplete.join("data")).unwrap();
        fs::write(incomplete.join("data/output"), b"incomplete").unwrap();

        let malformed = class.join("target-generation-malformed");
        fs::create_dir_all(&malformed).unwrap();
        fs::write(malformed.join(".velnor-target-complete-v1"), b"complete\n").unwrap();
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, malformed.join("data")).unwrap();

        let store_root = StoreRoot {
            kind: CacheStore::Targets,
            path: store.clone(),
            scope_prefix: Vec::new(),
            scope_depth: 4,
            candidate_depth: 5,
            gc_managed: true,
            emergency_managed: true,
        };
        let mut entries = Vec::new();
        assert!(target_generation_is_complete(&complete));
        collect_candidates(&store_root, &store, 0, &mut entries).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, complete);

        fs::write(class.join("current"), "target-generation-complete\n").unwrap();
        let mut policy = policy();
        let mut protected = BTreeSet::new();
        collect_pointer_protected(&class, 0, &mut protected);
        policy.protected_paths = protected;
        policy.keep_newest_per_target_scope = 0;
        // The current generation remains protected even when retention asks to
        // evict every target generation.
        assert!(select_eviction_candidates(&entries, &policy).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gc_leader_lock_excludes_second_reaper() {
        let root = std::env::temp_dir().join(format!("velnor-gc-lock-{}", uuid::Uuid::new_v4()));
        let first = GcLeaderLock::acquire(&root).unwrap();
        assert!(GcLeaderLock::acquire(&root).is_err());
        drop(first);
        assert!(GcLeaderLock::acquire(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn actions_cache_gc_waits_for_active_restore_lock() {
        let root = std::env::temp_dir().join(format!("velnor-entry-lock-{}", uuid::Uuid::new_v4()));
        let entry = root.join("repo/cache-key");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("payload"), b"cache").unwrap();
        let restore_lock = CacheEntryLock::shared(&entry).unwrap();
        let candidate = EvictionCandidate {
            path: entry.clone(),
            store: CacheStore::ActionsCache,
            scope: vec!["repo".into()],
            bytes: 5,
            reason: "test".into(),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let remover =
            std::thread::spawn(move || sender.send(remove_candidate(&candidate)).unwrap());

        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(entry.join("payload").is_file());
        drop(restore_lock);
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        remover.join().unwrap();
        assert!(!entry.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn target_gc_rechecks_current_under_publisher_lock() {
        let temp_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize temporary test root");
        let root = temp_root.join(format!("velnor-target-race-{}", uuid::Uuid::new_v4()));
        let scope = root.join("targets/trusted/workspace/repo/workflow/job");
        let generation = scope.join("target-generation-race");
        fs::create_dir_all(generation.join("data")).unwrap();
        fs::write(generation.join("data/output"), b"output").unwrap();
        fs::write(generation.join(".velnor-target-complete-v1"), b"complete\n").unwrap();

        let candidate = EvictionCandidate {
            path: generation.clone(),
            store: CacheStore::Targets,
            scope: vec![
                "trusted".into(),
                "repo".into(),
                "workflow".into(),
                "job".into(),
            ],
            bytes: 6,
            reason: "test".into(),
        };
        // The publisher and GC both lock the job bucket. Hold the publisher
        // side while GC has already selected the generation, then publish the
        // pointer. GC must re-read it after acquiring the same lock.
        let publisher_lock = CacheEntryLock::exclusive(&scope).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let remover =
            std::thread::spawn(move || sender.send(remove_candidate(&candidate)).unwrap());
        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        fs::write(scope.join("current"), b"target-generation-race\n").unwrap();
        drop(publisher_lock);

        let error = receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        remover.join().unwrap();
        assert!(error.to_string().contains("current target generation"));
        assert!(generation.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gc_without_yes_refuses() {
        let args = CacheGcArgs {
            dry_run: false,
            yes: false,
            force_no_lease_check: false,
            keep_newest_targets: 3,
            max_age_days: 30,
            max_size_bytes: None,
        };
        assert!(run_gc(Path::new("/does-not-matter"), args, BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("requires --yes"));
    }

    #[test]
    fn reclaim_stops_at_target_and_skips_in_use_scope() {
        let root = std::env::temp_dir().join(format!("velnor-reclaim-{}", uuid::Uuid::new_v4()));
        let work = root.join("work");
        let active = work.join("_velnor_caches/trusted/active/key");
        let first = work.join("_velnor_caches/trusted/first/key");
        let second = work.join("_velnor_caches/trusted/second/key");
        for path in [&active, &first, &second] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("data"), vec![0; 16]).unwrap();
        }
        let report = reclaim_work_root_with_layout(
            &work,
            &root.join("run"),
            &root.join("log"),
            16,
            &BTreeSet::from(["actions-cache/trusted/active".into()]),
            false,
            None,
        )
        .unwrap();
        assert_eq!(report.deleted.len(), 1);
        assert!(active.exists());
        assert_eq!(first.exists() as u8 + second.exists() as u8, 1);
        assert!(root.join("log/gc-history.jsonl").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_pressure_reclaimer_reclaims_discovered_work_root() {
        let root = std::env::temp_dir().join(format!(
            "velnor-disk-pressure-reclaim-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("lib/velnor-test/work");
        let cache = root.join("cache/velnor/v1/untrusted/caches/idle/key");
        let compiler_cache = work.join("_velnor_sccache/untrusted/idle/key");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::create_dir_all(&compiler_cache).unwrap();
        fs::write(cache.join("payload"), vec![0; 16]).unwrap();
        fs::write(compiler_cache.join("payload"), vec![0; 16]).unwrap();
        // Emergency reclaim refuses to delete a store a live job may be
        // writing. Model genuinely cold stores.
        backdate(&cache, EMERGENCY_MIN_IDLE * 2);
        backdate(compiler_cache.parent().unwrap(), EMERGENCY_MIN_IDLE * 2);

        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let work_roots = crate::leftover_disk::discover_daemon_work_roots_in(&root.join("lib"));
        assert_eq!(work_roots, vec![work.clone()]);
        let report = reclaim_for_disk_pressure_with_context(32, &work_roots, Some(&layout));

        assert_eq!(report.freed_bytes, 32);
        assert_eq!(
            report.deleted,
            vec![cache, compiler_cache.parent().unwrap().to_path_buf()]
        );
        assert!(report.failures.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// Emergency reclaim must not delete the store of a job that is merely
    /// between steps, uploading artifacts, or publishing a target generation —
    /// none of which show up as a running container, and two of which have no
    /// lease class published today.
    #[test]
    fn emergency_reclaim_keeps_stores_a_live_job_is_touching() {
        let root = std::env::temp_dir().join(format!("velnor-live-store-{}", uuid::Uuid::new_v4()));
        let work = root.join("lib/velnor-test/work");
        // Mid artifact upload and mid sccache write: touched right now.
        let artifacts = work.join("_velnor_artifacts/run-1");
        let sccache = work.join("_velnor_sccache/untrusted/hot/key");
        // A genuinely cold store, so the pass is not vacuously empty.
        let cold = work.join("_velnor_sccache/untrusted/cold");
        for dir in [&artifacts, &sccache, &cold.join("key")] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("payload"), vec![0; 16]).unwrap();
        }
        backdate(&cold, EMERGENCY_MIN_IDLE * 2);

        let run_root = root.join("run");
        let report = reclaim_work_root(
            &work,
            &run_root,
            &root.join("log"),
            u64::MAX,
            &BTreeSet::new(),
            true,
        )
        .unwrap();

        assert!(
            artifacts.exists(),
            "an artifact store being written must survive emergency reclaim"
        );
        assert!(
            sccache.exists(),
            "a compiler store being written must survive emergency reclaim"
        );
        assert!(
            report.deleted.iter().any(|path| path == &cold),
            "the cold store must still be reclaimed: {report:?}"
        );
        fs::remove_dir_all(root).ok();
    }

    /// The disk-pressure BuildKit reclaim inspected the literal name
    /// `velnor-builder`, but every real builder carries a `-<scope>` suffix, so
    /// the inspect never matched and the prune never ran.
    #[test]
    fn owned_builders_are_matched_by_prefix_not_by_a_bare_name() {
        let listing = "\
NAME/NODE                     DRIVER/ENDPOINT   STATUS    BUILDKIT   PLATFORMS
default *                     docker
  default                     default           running   v0.12.0    linux/arm64
velnor-builder-trusted        docker-container
  velnor-builder-trusted0     unix:///var/run/docker.sock running v0.12.0 linux/arm64
velnor-builder-untrusted      docker-container
  velnor-builder-untrusted0   unix:///var/run/docker.sock running v0.12.0 linux/arm64
someone-elses-builder         docker-container
";
        assert_eq!(
            owned_builder_names(listing),
            vec![
                "velnor-builder-trusted".to_string(),
                "velnor-builder-untrusted".to_string()
            ],
            "the bare name never exists; ownership is the prefix"
        );
        assert!(owned_builder_names(listing)
            .iter()
            .all(|name| name.starts_with(OWNED_BUILDER_PREFIX)));
        assert!(owned_builder_names("NAME/NODE\ndefault *\n").is_empty());
    }

    /// Every store the emergency reclaimer may delete must declare a lease
    /// class, or it can be deleted out from under the job that owns it.
    #[test]
    fn every_emergency_managed_store_has_a_lease_class() {
        let work = PathBuf::from("/var/lib/velnor/work");
        for store in store_roots(&work) {
            if store.emergency_managed || store.gc_managed {
                assert!(
                    store.kind.lease_class().is_some(),
                    "{} is reclaimable but declares no lease class",
                    store.kind
                );
            }
        }
    }

    #[test]
    fn split_store_roots_emit_exact_shared_and_repo_candidates() {
        let root =
            std::env::temp_dir().join(format!("velnor-split-store-{}", uuid::Uuid::new_v4()));
        let registry = root.join("cargo/registry");
        let canonical_bin = root.join("cargo/bin");
        let legacy_bin = root.join("legacy-bin");
        fs::create_dir_all(registry.join("cache/index")).unwrap();
        fs::write(registry.join("cache/index/crate"), b"crate").unwrap();
        fs::create_dir_all(canonical_bin.join("tailrocks_playground")).unwrap();
        fs::write(canonical_bin.join("tailrocks_playground/tool"), b"tool").unwrap();
        fs::create_dir_all(legacy_bin.join("trusted/tailrocks_playground")).unwrap();
        fs::write(
            legacy_bin.join("trusted/tailrocks_playground/tool"),
            b"tool",
        )
        .unwrap();

        let roots = [
            StoreRoot {
                kind: CacheStore::Cargo,
                path: registry.clone(),
                scope_prefix: vec!["registry".into()],
                scope_depth: 0,
                candidate_depth: 0,
                gc_managed: true,
                emergency_managed: true,
            },
            StoreRoot {
                kind: CacheStore::Cargo,
                path: canonical_bin,
                scope_prefix: vec!["bin".into()],
                scope_depth: 1,
                candidate_depth: 1,
                gc_managed: true,
                emergency_managed: true,
            },
            StoreRoot {
                kind: CacheStore::Cargo,
                path: legacy_bin,
                scope_prefix: vec!["bin".into()],
                scope_depth: 2,
                candidate_depth: 2,
                gc_managed: true,
                emergency_managed: true,
            },
        ];
        let mut entries = Vec::new();
        for store in &roots {
            collect_candidates(store, &store.path, 0, &mut entries).unwrap();
        }

        assert!(entries
            .iter()
            .any(|entry| { entry.path == registry && entry.scope_key() == "registry" }));
        assert!(entries
            .iter()
            .any(|entry| entry.scope_key() == "bin/tailrocks_playground"));
        assert!(entries
            .iter()
            .any(|entry| entry.scope_key() == "bin/trusted/tailrocks_playground"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_job_leases_protect_every_mounted_store_across_daemons() {
        let run_root =
            std::env::temp_dir().join(format!("velnor-active-stores-{}", uuid::Uuid::new_v4()));
        let stale_after = Duration::from_secs(60);
        let scopes = [
            ("targets", "workspace-v2/tailrocks_playground/ci.yml"),
            ("actions-cache", "tailrocks_playground"),
            ("cargo", "registry"),
            ("cargo", "git"),
            ("cargo", "bin/tailrocks_playground"),
            ("mise", "cache"),
            ("mise", "installs/tailrocks_playground"),
            ("mise", "binaries/tailrocks_playground"),
            ("mise", "rustup/tailrocks_playground"),
        ];
        let mut leases = Vec::new();
        // Four jobs from one repository must be able to hold every shared and
        // repository-local store concurrently.
        for holder in ["job-1", "job-2", "job-3", "job-4"] {
            for (class, scope) in scopes {
                leases.push(
                    crate::capacity::ScopeLease::acquire(
                        &run_root,
                        class,
                        &format!("{scope}/{holder}"),
                        stale_after,
                    )
                    .unwrap(),
                );
            }
        }
        // A concurrent job from another repository shares Cargo registry/git
        // and mise cache, while protecting its own executable/cache/target scopes.
        for (class, scope) in [
            ("targets", "workspace-v2/tailrocks_other/ci.yml"),
            ("actions-cache", "tailrocks_other"),
            ("cargo", "registry"),
            ("cargo", "git"),
            ("cargo", "bin/tailrocks_other"),
            ("mise", "cache"),
            ("mise", "installs/tailrocks_other"),
            ("mise", "binaries/tailrocks_other"),
            ("mise", "rustup/tailrocks_other"),
        ] {
            leases.push(
                crate::capacity::ScopeLease::acquire(
                    &run_root,
                    class,
                    &format!("{scope}/other-job"),
                    stale_after,
                )
                .unwrap(),
            );
        }
        let active = crate::capacity::active_scopes(&run_root, stale_after).unwrap();
        let entries = vec![
            entry(
                "/targets/playground",
                CacheStore::Targets,
                &["workspace-v2", "tailrocks_playground", "ci.yml"],
                90,
                10,
            ),
            entry(
                "/targets/other",
                CacheStore::Targets,
                &["workspace-v2", "tailrocks_other", "ci.yml"],
                90,
                10,
            ),
            entry(
                "/caches/playground",
                CacheStore::ActionsCache,
                &["tailrocks_playground"],
                90,
                10,
            ),
            entry(
                "/caches/other",
                CacheStore::ActionsCache,
                &["tailrocks_other"],
                90,
                10,
            ),
            entry("/cargo/registry", CacheStore::Cargo, &["registry"], 90, 10),
            entry("/cargo/git", CacheStore::Cargo, &["git"], 90, 10),
            entry(
                "/cargo/bin/playground",
                CacheStore::Cargo,
                &["bin", "tailrocks_playground"],
                90,
                10,
            ),
            entry(
                "/cargo/bin/other",
                CacheStore::Cargo,
                &["bin", "tailrocks_other"],
                90,
                10,
            ),
            entry("/mise/cache", CacheStore::Mise, &["cache"], 90, 10),
            entry(
                "/mise/installs/playground",
                CacheStore::Mise,
                &["installs", "tailrocks_playground"],
                90,
                10,
            ),
            entry(
                "/mise/installs/other",
                CacheStore::Mise,
                &["installs", "tailrocks_other"],
                90,
                10,
            ),
            entry(
                "/mise/binaries/playground",
                CacheStore::Mise,
                &["binaries", "tailrocks_playground"],
                90,
                10,
            ),
            entry(
                "/mise/binaries/other",
                CacheStore::Mise,
                &["binaries", "tailrocks_other"],
                90,
                10,
            ),
            entry(
                "/mise/rustup/playground",
                CacheStore::Mise,
                &["rustup", "tailrocks_playground"],
                90,
                10,
            ),
            entry(
                "/mise/rustup/other",
                CacheStore::Mise,
                &["rustup", "tailrocks_other"],
                90,
                10,
            ),
        ];
        let mut policy = policy();
        policy.in_use_scopes = active;

        let candidates = select_eviction_candidates(&entries, &policy);
        let paths: BTreeSet<_> = candidates
            .into_iter()
            .map(|candidate| candidate.path)
            .collect();
        assert!(paths.is_empty());
        drop(leases);
        fs::remove_dir_all(run_root).unwrap();
    }
}
