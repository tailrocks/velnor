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
    store_catalog::StoreCatalog,
};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Minimum time a store must have been untouched before the *emergency*
/// reclaimer may delete it.
pub(crate) const EMERGENCY_MIN_IDLE: Duration = Duration::from_secs(15 * 60);

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
#[derive(Debug)]
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

    /// Exclusive lock with a bounded wait: poll non-blocking flock until
    /// `timeout`, then fail loud instead of parking the holder forever. A
    /// re-entrant acquisition in one process (a second open of the same lock
    /// file) times out rather than deadlocking, which is what lets callers
    /// prove their critical sections end before slow work starts.
    pub(crate) fn exclusive_timeout(cache_dir: &Path, timeout: Duration) -> Result<Self> {
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
        let start = std::time::Instant::now();
        loop {
            match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(_) if start.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error).context(format!(
                        "lock cache entry {} within {}ms",
                        cache_dir.display(),
                        timeout.as_millis()
                    )));
                }
            }
        }
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

    fn exclusive_under_anchor(
        cache_dir: &Path,
        trusted_anchor: &Path,
        expected_anchor: &crate::leftover_disk::FilesystemDirectoryIdentity,
    ) -> Result<Self> {
        let store = cache_dir
            .parent()
            .context("cache entry has no store parent")?;
        let name = cache_dir.file_name().context("cache entry has no name")?;
        if &crate::leftover_disk::filesystem_directory_identity(trusted_anchor)? != expected_anchor
        {
            bail!("cache lock anchor changed since secure inventory");
        }
        let file = crate::leftover_disk::filesystem_open_cache_lock_file_under_anchor(
            trusted_anchor,
            store,
            name,
            expected_anchor,
        )
        .with_context(|| format!("open cache entry lock for {}", cache_dir.display()))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .with_context(|| format!("lock cache entry {}", cache_dir.display()))?;
        Ok(Self { _file: file })
    }
}

/// The storage scope one cache pass inspects: the storage layout and pool
/// trust boundary of the daemon whose stores are being enumerated.
///
/// Store paths are namespaced by both, so a pass that read them from its own
/// process environment could only ever see the stores of a daemon configured
/// exactly like itself. `velnorctl cache du` on a packaged host did exactly
/// that and reported the untrusted namespace of a storage root no daemon ran
/// in. The daemon's own reclaim paths use [`StoreScope::current`]; operator
/// passes build one per packaged instance from `daemon_instance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreScope {
    pub(crate) layout: Option<crate::storage::StorageLayout>,
    pub(crate) pool_trust_scope: String,
    /// The inspected daemon's unit environment when the pass is over a
    /// packaged instance; `None` when the pass inspects this process's own
    /// daemon, whose parameters are this process's environment.
    pub(crate) daemon_environment: Option<BTreeMap<String, String>>,
}

impl StoreScope {
    /// This process's own resolution: `VELNOR_STORAGE_ROOT` and the trust
    /// boundary clap published at startup.
    pub(crate) fn current() -> Self {
        Self {
            layout: crate::storage::selected_or_resolved_layout(),
            pool_trust_scope: crate::trust_scope::current(),
            daemon_environment: None,
        }
    }

    fn with_layout(layout: &crate::storage::StorageLayout) -> Self {
        Self {
            layout: Some(layout.clone()),
            pool_trust_scope: crate::trust_scope::current(),
            daemon_environment: None,
        }
    }

    fn for_instance(instance: &crate::daemon_instance::DaemonInstance) -> Self {
        Self {
            layout: Some(instance.storage_layout()),
            pool_trust_scope: instance.trust_scope.clone(),
            daemon_environment: Some(instance.environment.clone()),
        }
    }

    fn layout(&self) -> Option<&crate::storage::StorageLayout> {
        self.layout.as_ref()
    }

    /// The compiler store budget the inspected daemon enforces: its
    /// admission parameters (`VELNOR_SLOTS`, `VELNOR_JOB_PEAK_BYTES`,
    /// `VELNOR_EMERGENCY_RESERVE_BYTES`) bound to the filesystem holding
    /// `work_root`. A packaged instance's parameters come from its unit
    /// environment, not from the operator process running the pass.
    fn store_budget(&self, work_root: &Path) -> Result<crate::capacity::StoreBudgetPolicy> {
        match &self.daemon_environment {
            Some(environment) => {
                crate::capacity::StoreBudgetPolicy::probe_from_environment(work_root, environment)
            }
            None => crate::capacity::StoreBudgetPolicy::probe_from_env(work_root),
        }
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
    let targets = if let Some(selector) = args.instance.as_deref() {
        let instance = crate::daemon_instance::resolve(selector)?;
        let work_root = instance_work_root(&instance, args.work_dir.clone());
        vec![(Some(instance), work_root, None)]
    } else if args.work_dir.is_none() {
        let instances = crate::daemon_instance::enumerate()?;
        if instances.is_empty() {
            let layout = standalone_layout(args.config_dir.clone())?;
            let work_root = work_root(args.config_dir.clone(), None)?;
            vec![(None, work_root, Some(layout))]
        } else {
            instances
                .into_iter()
                .map(|instance| {
                    let work_root = instance_work_root(&instance, None);
                    (Some(instance), work_root, None)
                })
                .collect()
        }
    } else {
        let layout = standalone_layout(args.config_dir.clone())?;
        let work_root = work_root(args.config_dir.clone(), args.work_dir)?;
        vec![(None, work_root, Some(layout))]
    };
    let multiple = targets.len() > 1;
    for (index, (instance, work_root, standalone_layout)) in targets.into_iter().enumerate() {
        let scope = instance
            .as_ref()
            .map(StoreScope::for_instance)
            .unwrap_or_else(|| {
                standalone_layout
                    .as_ref()
                    .map(StoreScope::with_layout)
                    .unwrap_or_else(StoreScope::current)
            });
        if let Some(instance) = &instance {
            if index > 0 {
                println!();
            }
            println!(
                "instance\t{}\t{}\t{}\t{}",
                instance.instance,
                instance.unit,
                instance.storage_root.display(),
                instance.trust_scope
            );
        }
        let result = match &args.command {
            CacheCommand::Du => run_du(&work_root, &budgets, &scope),
            CacheCommand::Gc(gc) => run_gc(&work_root, gc.clone(), budgets.clone(), &scope),
        };
        if multiple {
            // Every instance gets its pass; one failing instance is reported
            // in place and does not hide the others.
            if let Err(error) = result {
                eprintln!(
                    "cache {} failed for instance {}: {error:#}",
                    match &args.command {
                        CacheCommand::Du => "du",
                        CacheCommand::Gc(_) => "gc",
                    },
                    instance
                        .map(|instance| instance.instance)
                        .unwrap_or_default()
                );
            }
        } else {
            result?;
        }
    }
    Ok(())
}

/// A packaged instance's daemon-shared store root: its `VELNOR_WORK_DIR`
/// (or an operator override), climbed to the shared root exactly as the
/// daemon climbs it.
fn instance_work_root(
    instance: &crate::daemon_instance::DaemonInstance,
    work_dir: Option<PathBuf>,
) -> PathBuf {
    crate::container::daemon_shared_root(work_dir.unwrap_or_else(|| instance.work_dir.clone()))
}

fn standalone_layout(config_dir: Option<PathBuf>) -> Result<crate::storage::StorageLayout> {
    crate::storage::resolve_required_layout_for_cli(config_dir.as_deref())
}

fn work_root(config_dir: Option<PathBuf>, work_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(work_dir) = work_dir {
        return Ok(crate::container::daemon_shared_root(work_dir));
    }
    let config_dir = config::config_dir(config_dir)?;
    Ok(crate::container::daemon_shared_root(
        config_dir.join("_work"),
    ))
}

fn run_du(work_root: &Path, budgets: &BTreeMap<CacheStore, u64>, scope: &StoreScope) -> Result<()> {
    let stores = store_roots(work_root, scope)?;
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
    for store in store_roots(work_root, &StoreScope::current())? {
        let (store_logical, store_physical, _) = size_physical_and_modified(&store.path)?;
        logical = logical.saturating_add(store_logical);
        physical = physical.saturating_add(store_physical);
    }
    Ok((logical, physical))
}

fn required_scope_layout(scope: &StoreScope) -> Result<crate::storage::StorageLayout> {
    match scope.layout() {
        Some(layout) => Ok(layout.clone()),
        None => crate::storage::resolve_required_layout(),
    }
}

fn run_gc(
    work_root: &Path,
    args: CacheGcArgs,
    class_budgets: BTreeMap<CacheStore, u64>,
    scope: &StoreScope,
) -> Result<()> {
    if !args.dry_run && !args.yes {
        bail!("destructive cache gc requires --yes");
    }
    let layout = required_scope_layout(scope)?;
    let mut resolved_scope = scope.clone();
    resolved_scope.layout = Some(layout.clone());
    let backend = crate::execution::load_execution_file(std::path::Path::new("/etc/velnor"), None)
        .ok()
        .map(|file| file.backend());
    if let Some(reason) =
        velnor_model::ExecutionBackendKind::host_docker_maintenance_skip_reason(backend)
    {
        eprintln!("leftover-after-Velnor host Docker reclaim skipped: {reason}");
    }
    let reclaim_backend = backend.unwrap_or(velnor_model::ExecutionBackendKind::MicroVm);
    let daemon_work_roots = crate::leftover_disk::discover_daemon_work_roots_for_layout(&layout);
    run_gc_with(
        work_root,
        args,
        class_budgets,
        &resolved_scope,
        |coordinator, run_root| {
            crate::leftover_disk::reclaim_production_leftovers_under_coordinator(
                coordinator,
                run_root,
                &daemon_work_roots,
                reclaim_backend,
                false,
            )
        },
    )
}

/// `cache gc` with its environment made explicit.
///
/// Destructive gc holds the [`GcLeaderLock`] and the exclusive
/// [`crate::capacity::FilesystemCoordinator`] of the runtime root for its whole
/// run — eviction and the leftover-workspace reclaim that follows it. The
/// coordinator is a blocking `flock`; taking it a second time from the thread
/// that holds it never returns, so `reclaim_leftover` receives the held
/// coordinator instead of resolving and locking the runtime root itself.
pub(crate) fn run_gc_with(
    work_root: &Path,
    args: CacheGcArgs,
    class_budgets: BTreeMap<CacheStore, u64>,
    scope: &StoreScope,
    reclaim_leftover: impl FnOnce(
        &crate::capacity::FilesystemCoordinator,
        &Path,
    ) -> Result<crate::leftover_disk::LeftoverReclaimReport>,
) -> Result<()> {
    if !args.dry_run && !args.yes {
        bail!("destructive cache gc requires --yes");
    }
    let storage_layout = required_scope_layout(scope)?;
    let mut resolved_scope = scope.clone();
    resolved_scope.layout = Some(storage_layout.clone());
    let run_root = storage_layout.run_root.clone();
    let destructive_locks = if args.dry_run {
        None
    } else {
        Some((
            GcLeaderLock::acquire(&run_root)?,
            crate::capacity::FilesystemCoordinator::lock_exclusive(&run_root)?,
        ))
    };
    let in_use_scopes = match crate::capacity::active_scopes(&run_root, Duration::from_secs(86400))
    {
        Ok(scopes) => scopes,
        Err(error) if args.force_no_lease_check => {
            eprintln!("WARNING: bypassing active-scope lease check: {error:#}");
            BTreeSet::new()
        }
        Err(error) => return Err(error).context("read active cache-scope leases"),
    };

    let inventory = pinned_cache_inventory(work_root, false, &resolved_scope, false)?;
    for failure in &inventory.failures {
        eprintln!("cache gc: {failure}");
    }
    let listing = inventory.entries.clone();
    let max_age = args
        .max_age_days
        .checked_mul(DAY.as_secs())
        .map(Duration::from_secs)
        .context("max-age-days overflowed Duration")?;
    // The compiler classes are bounded by the host capacity policy, not by a
    // flag: the same number the daemon enforces at admission and preflight
    // prints, derived here from the filesystem and the inspected daemon's
    // environment (the instance's unit environment on a packaged host).
    let store_budget = resolved_scope
        .store_budget(work_root)
        .context("derive the compiler store budget from host capacity")?;
    let policy = EvictionPolicy {
        now: SystemTime::now(),
        keep_newest_per_target_scope: args.keep_newest_targets,
        max_age,
        max_total_bytes: args.max_size_bytes,
        class_budgets,
        compiler_budget_bytes: Some(store_budget.compiler_store_budget_bytes()),
        in_use_scopes,
        protected_paths: pointer_protected_target_generations(work_root, &resolved_scope)?,
    };
    let candidates = select_eviction_candidates(&listing, &policy);

    println!("dry_run\t{}", args.dry_run);
    println!("work_dir\t{}", work_root.display());
    println!(
        "compiler_store_budget\t{}",
        store_budget.describe_compiler_store_budget()
    );
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
        print_leftover_workspace_candidates(&storage_layout);
        return Ok(());
    }

    let Some((_leader, coordinator)) = destructive_locks.as_ref() else {
        bail!("destructive cache gc reached deletion without holding its locks");
    };
    let log_root = storage_layout.log_root.clone();
    for candidate in candidates {
        let result = inventory
            .candidates
            .get(&(candidate.store, candidate.path.clone()))
            .with_context(|| {
                format!(
                    "cache candidate was not retained by secure inventory: {}",
                    candidate.path.display()
                )
            })
            .and_then(|pinned| {
                remove_candidate(&candidate, pinned, pinned.anchor_identity.device, &|_| {
                    Ok(())
                })
            });
        let outcome = match &result {
            Ok(CandidateRemovalOutcome::Removed) => "deleted",
            Ok(CandidateRemovalOutcome::SkippedBusy) => "skipped",
            Err(_) => "failed",
        };
        append_gc_history(&log_root, &candidate, Some(&policy), outcome)?;
        println!(
            "{}\t{}\t{}\t{}\t{}",
            candidate.store,
            candidate.bytes,
            candidate.scope_key(),
            candidate.reason,
            candidate.path.display()
        );
        match result {
            Ok(CandidateRemovalOutcome::Removed) => {}
            Ok(CandidateRemovalOutcome::SkippedBusy) => eprintln!(
                "gc skipped busy GHA tenant {}; retry on a later pass",
                candidate.path.display()
            ),
            Err(error) => eprintln!(
                "gc deletion failed for {}: {error}",
                candidate.path.display()
            ),
        }
    }
    match reclaim_leftover(coordinator, &run_root) {
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

fn print_leftover_workspace_candidates(layout: &crate::storage::StorageLayout) {
    let roots = crate::leftover_disk::discover_daemon_work_roots_for_layout(layout);
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

#[derive(Debug)]
struct GcLeaderLock {
    _file: File,
}

/// Another daemon holds the GC leader lock: contention to back off from, not
/// a failure. Produced at the flock boundary from the `WOULDBLOCK` errno so
/// the reclaim caller matches on the type instead of error text.
#[derive(Debug)]
struct GcLeaderLockHeld {
    path: PathBuf,
}

impl std::fmt::Display for GcLeaderLockHeld {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "another gc holds the lock ({})",
            self.path.display()
        )
    }
}

impl std::error::Error for GcLeaderLockHeld {}

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
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Self { _file: file }),
            Err(rustix::io::Errno::WOULDBLOCK) => Err(GcLeaderLockHeld { path }.into()),
            // Any other flock errno keeps its context and aborts (fail
            // closed — an unexpected lock error is not proven contention).
            Err(other) => {
                Err(anyhow::Error::new(other).context(format!("lock GC leader {}", path.display())))
            }
        }
    }
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

fn store_roots(work_root: &Path, scope: &StoreScope) -> Result<Vec<StoreRoot>> {
    // Every path below comes from the catalog. GC must never spell a store root
    // itself: that is exactly how the artifact store came to be written at
    // `<work>/slot-N/_velnor_artifacts` while GC swept `<work>/_velnor_artifacts`.
    let resolved_layout = required_scope_layout(scope)?;
    let catalog =
        crate::store_catalog::StoreCatalog::for_work_root_with_layout(work_root, &resolved_layout);
    let pool_scope = scope.pool_trust_scope.as_str();
    let trust_partitioned_roots = |root: fn(&StoreCatalog, &str) -> PathBuf| {
        trust_partitioned_roots(&catalog, pool_scope, root)
    };
    let mut stores = Vec::new();
    // Enumerate the pool, fail-closed floor, and PR namespaces written by
    // admitted jobs. De-duplicate equal scopes so every mounted namespace is
    // available to accounting and reclamation exactly once.
    for (store_trust_scope, cargo) in trust_partitioned_roots(StoreCatalog::cargo) {
        let trust_key = crate::trust_scope::filesystem_key(&store_trust_scope);
        stores.extend([
            StoreRoot {
                kind: CacheStore::Cargo,
                path: cargo.join("registry"),
                scope_prefix: vec![trust_key.clone(), "registry".into()],
                scope_depth: 0,
                candidate_depth: 0,
                gc_managed: true,
                emergency_managed: true,
            },
            StoreRoot {
                kind: CacheStore::Cargo,
                path: cargo.join("git"),
                scope_prefix: vec![trust_key.clone(), "git".into()],
                scope_depth: 0,
                candidate_depth: 0,
                gc_managed: true,
                emergency_managed: true,
            },
            StoreRoot {
                kind: CacheStore::Cargo,
                path: cargo.join("bin"),
                scope_prefix: vec![trust_key, "bin".into()],
                scope_depth: 1,
                candidate_depth: 1,
                gc_managed: true,
                emergency_managed: true,
            },
        ]);
    }
    for (store_trust_scope, mise) in trust_partitioned_roots(StoreCatalog::mise) {
        let trust_key = crate::trust_scope::filesystem_key(&store_trust_scope);
        stores.extend([
            StoreRoot {
                kind: CacheStore::Mise,
                path: mise.join("cache"),
                scope_prefix: vec![trust_key.clone(), "cache".into()],
                scope_depth: 0,
                candidate_depth: 0,
                gc_managed: true,
                emergency_managed: true,
            },
            StoreRoot {
                kind: CacheStore::Mise,
                path: mise.join("installs"),
                scope_prefix: vec![trust_key.clone(), "installs".into()],
                scope_depth: 1,
                candidate_depth: 1,
                gc_managed: true,
                emergency_managed: true,
            },
            // Plan 008: persistent per-version mise binaries, same trust/repository
            // boundary and mise budget as installs.
            StoreRoot {
                kind: CacheStore::Mise,
                path: mise.join("binaries"),
                scope_prefix: vec![trust_key.clone(), "binaries".into()],
                scope_depth: 1,
                candidate_depth: 1,
                gc_managed: true,
                emergency_managed: true,
            },
            StoreRoot {
                kind: CacheStore::Mise,
                path: mise.join("rustup"),
                scope_prefix: vec![trust_key, "rustup".into()],
                scope_depth: 1,
                candidate_depth: 1,
                gc_managed: true,
                emergency_managed: true,
            },
        ]);
    }
    for (store_trust_scope, targets) in trust_partitioned_roots(StoreCatalog::targets) {
        stores.push(StoreRoot {
            kind: CacheStore::Targets,
            path: targets,
            scope_prefix: vec![crate::trust_scope::filesystem_key(&store_trust_scope)],
            // The existing job bucket remains the ownership scope. Immutable
            // target generations are one directory below it.
            scope_depth: 4,
            candidate_depth: 5,
            gc_managed: true,
            emergency_managed: true,
        });
    }
    for (store_trust_scope, actions_cache) in trust_partitioned_roots(StoreCatalog::actions_cache) {
        stores.push(StoreRoot {
            kind: CacheStore::ActionsCache,
            path: actions_cache,
            scope_prefix: vec![crate::trust_scope::filesystem_key(&store_trust_scope)],
            scope_depth: 1,
            candidate_depth: 2,
            gc_managed: true,
            emergency_managed: true,
        });
    }
    stores.push(StoreRoot {
        kind: CacheStore::Artifacts,
        path: catalog.artifacts(),
        scope_prefix: Vec::new(),
        scope_depth: 1,
        candidate_depth: 1,
        gc_managed: true,
        emergency_managed: true,
    });
    // The compiler stores are partitioned by the job's admitted scope like
    // every other trust-partitioned class: trusted jobs on a custom pool
    // write under the pool scope, fork and unknown jobs under the floor.
    // Both are routinely GC-managed under the host-level compiler budget
    // (`StoreBudgetPolicy`): they used to be emergency-only, which left the
    // largest class on a Rust host with no bound at all between emergencies.
    //
    // An mbx store is laid out per slot (`mbx_store`), so its candidates are
    // the per-slot cache and target trees, each scoped to the repository id
    // the job leases — one cold slot is evicted, never a whole repository.
    for (store_trust_scope, mbx) in trust_partitioned_roots(StoreCatalog::mbx) {
        let trust_key = crate::trust_scope::filesystem_key(&store_trust_scope);
        for (repository, path) in crate::mbx_store::gc_roots(&mbx) {
            stores.push(StoreRoot {
                kind: CacheStore::Mbx,
                path,
                scope_prefix: vec![trust_key.clone(), repository],
                scope_depth: 0,
                candidate_depth: 1,
                gc_managed: true,
                emergency_managed: true,
            });
        }
    }
    for (store_trust_scope, sccache) in trust_partitioned_roots(StoreCatalog::sccache) {
        stores.push(StoreRoot {
            kind: CacheStore::Sccache,
            path: sccache,
            scope_prefix: vec![crate::trust_scope::filesystem_key(&store_trust_scope)],
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
            emergency_managed: true,
        });
    }
    // Persistent Git mirrors are indexed by the canonical repository key. A
    // repository root is one GC candidate; the trust key is part of the lease
    // scope so sibling trust domains never protect or evict each other.
    for (store_trust_scope, git_mirrors) in trust_partitioned_roots(StoreCatalog::git_mirrors) {
        stores.push(StoreRoot {
            kind: CacheStore::GitMirrors,
            path: git_mirrors,
            scope_prefix: vec![crate::trust_scope::filesystem_key(&store_trust_scope)],
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
            emergency_managed: true,
        });
    }
    // The hosted actions-cache service is durable storage like any other class.
    // It was previously invisible to `cache du` and to every collector, so each
    // tenant accumulated its own budget outside the ledger.
    stores.push(StoreRoot {
        kind: CacheStore::GhaCache,
        path: crate::store_catalog::gha_cache_root(&resolved_layout).join("tenants"),
        scope_prefix: Vec::new(),
        scope_depth: 1,
        candidate_depth: 1,
        gc_managed: true,
        emergency_managed: true,
    });
    stores.extend(stable_workspace_store_roots(work_root, &catalog)?);
    Ok(stores)
}

/// Check whether a journaled path under `work_root` belongs to a catalogued
/// cache store. Recovery uses this to scan cache quarantines anchored at the
/// shared work root without treating workspace quarantines as cache data.
pub(crate) fn is_catalog_cache_candidate_path_for_recovery(
    work_root: &Path,
    layout: &crate::storage::StorageLayout,
    relative_path: &Path,
) -> Result<bool> {
    let candidate = work_root.join(relative_path);
    Ok(store_roots(work_root, &StoreScope::with_layout(layout))?
        .iter()
        .any(|store| candidate.strip_prefix(&store.path).is_ok()))
}

/// Stable workspaces live below each slot work directory rather than the
/// daemon-shared work root. Enumerate both the single-slot root and numbered
/// slot roots through the catalog, or accept the exact stable-workspace root
/// supplied by the disk-pressure path. Each candidate's scope is the same
/// slot/trust/repository identity published by the runner lease.
fn stable_workspace_store_roots(
    work_root: &Path,
    catalog: &StoreCatalog,
) -> Result<Vec<StoreRoot>> {
    let stable_dir = crate::stable_workspace::STABLE_WORKSPACES_DIR;
    let slot_roots = if work_root.file_name() == Some(std::ffi::OsStr::new(stable_dir)) {
        vec![(
            work_root
                .parent()
                .context("stable-workspace root has no slot work directory")?
                .to_path_buf(),
            work_root.to_path_buf(),
        )]
    } else {
        let shared_work_root = catalog.work_root();
        let mut slot_work_dirs = BTreeSet::from([shared_work_root.to_path_buf()]);
        let entries = match fs::read_dir(shared_work_root) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "read slot work directories under {}",
                        shared_work_root.display()
                    )
                });
            }
        };
        if let Some(entries) = entries {
            for entry in entries {
                let entry = entry.with_context(|| {
                    format!(
                        "read slot work directory under {}",
                        shared_work_root.display()
                    )
                })?;
                if !entry.file_type()?.is_dir() || !is_numbered_slot_work_dir(&entry.file_name()) {
                    continue;
                }
                slot_work_dirs.insert(entry.path());
            }
        }
        slot_work_dirs
            .into_iter()
            .map(|slot_work_dir| {
                let stable_root = StoreCatalog::stable_workspace_root(&slot_work_dir);
                (slot_work_dir, stable_root)
            })
            .collect()
    };

    slot_roots
        .into_iter()
        .map(|(slot_work_dir, path)| {
            let slot_key =
                crate::stable_workspace::slot_scope_key(&slot_work_dir).with_context(|| {
                    format!(
                        "derive stable-workspace slot scope for {}",
                        slot_work_dir.display()
                    )
                })?;
            Ok(StoreRoot {
                kind: CacheStore::StableWorkspace,
                path,
                scope_prefix: vec![slot_key],
                scope_depth: 2,
                candidate_depth: 2,
                // Workspace state stays warm under ordinary cache gc; only
                // pinned emergency pressure may evict an idle repository.
                gc_managed: false,
                emergency_managed: true,
            })
        })
        .collect()
}

fn is_numbered_slot_work_dir(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix("slot-"))
        .is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// The roots of one trust-partitioned store class: pool, fail-closed, and PR
/// scopes, deduped by path.
///
/// The pool boundary comes from the inspected daemon ([`StoreScope`]); the
/// other two namespaces are written by jobs regardless of pool trust.
fn trust_partitioned_roots(
    catalog: &StoreCatalog,
    pool_scope: &str,
    root: impl Fn(&StoreCatalog, &str) -> PathBuf,
) -> Vec<(String, PathBuf)> {
    let mut roots = vec![(pool_scope.to_owned(), root(catalog, pool_scope))];
    for trust_scope in [
        crate::trust_scope::FAIL_CLOSED,
        crate::trust_scope::PR_STORE_SCOPE,
    ] {
        let path = root(catalog, trust_scope);
        if !roots.iter().any(|(_, existing)| existing == &path) {
            roots.push((trust_scope.to_owned(), path));
        }
    }
    roots
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

#[cfg_attr(not(test), allow(dead_code))]
fn cache_listing(work_root: &Path, emergency: bool, scope: &StoreScope) -> Result<Vec<CacheEntry>> {
    let inventory = pinned_cache_inventory(work_root, emergency, scope, false)?;
    report_inventory_failures(&inventory.failures, "cache listing");
    Ok(inventory.entries)
}

#[derive(Default)]
struct PinnedCacheInventory {
    entries: Vec<CacheEntry>,
    candidates: BTreeMap<(CacheStore, PathBuf), PinnedCacheCandidate>,
    failures: Vec<String>,
}

struct PinnedCacheCandidate {
    trusted_anchor: PathBuf,
    anchor_identity: crate::leftover_disk::FilesystemDirectoryIdentity,
    candidate_identity: crate::leftover_disk::FilesystemDirectoryIdentity,
    directory: fs::File,
}

/// Build a destructive cache listing from descriptor-relative nofollow
/// inventory. Each returned path keeps the candidate descriptor and the
/// anchor identity needed by quarantine deletion, so later path replacement
/// cannot redirect cleanup outside the inspected tree.
fn pinned_cache_inventory(
    work_root: &Path,
    emergency: bool,
    scope: &StoreScope,
    include_stable_workspace: bool,
) -> Result<PinnedCacheInventory> {
    let layout = required_scope_layout(scope)?;
    let roots = store_roots(work_root, scope)?;
    let mut inventory = PinnedCacheInventory::default();
    let mut anchor_identities =
        BTreeMap::<PathBuf, Option<crate::leftover_disk::FilesystemDirectoryIdentity>>::new();

    for store in roots.iter().filter(|store| {
        if store.kind == CacheStore::StableWorkspace && !include_stable_workspace {
            false
        } else if emergency {
            store.emergency_managed
        } else {
            store.gc_managed
        }
    }) {
        let Some(anchor) = trusted_catalog_anchor(work_root, &layout, &roots, &store.path) else {
            inventory.failures.push(format!(
                "skip cache root {}: no trusted catalog anchor",
                store.path.display()
            ));
            continue;
        };
        let anchor_identity = if let Some(identity) = anchor_identities.get(&anchor) {
            identity.clone()
        } else {
            let identity = match crate::leftover_disk::filesystem_directory_identity(&anchor) {
                Ok(identity) => Some(identity),
                Err(error) if is_not_found_error(&error) => None,
                Err(error) => {
                    inventory.failures.push(format!(
                        "skip cache root {}: trusted anchor cannot be proven: {error:#}",
                        store.path.display()
                    ));
                    None
                }
            };
            anchor_identities.insert(anchor.clone(), identity.clone());
            identity
        };
        let Some(anchor_identity) = anchor_identity else {
            continue;
        };
        let snapshots = match crate::leftover_disk::filesystem_candidate_tree_snapshots_under(
            &anchor,
            &store.path,
            &anchor_identity,
            store.candidate_depth,
        ) {
            Ok(snapshots) => snapshots,
            Err(error) => {
                inventory.failures.push(format!(
                    "skip cache root {}: secure inventory failed: {error:#}",
                    store.path.display()
                ));
                continue;
            }
        };
        for snapshot in snapshots {
            if !snapshot.same_mount_tree
                || snapshot.identity.device != anchor_identity.device
                || snapshot.identity.mount != anchor_identity.mount
            {
                inventory.failures.push(format!(
                    "skip cache candidate {}: tree crosses its catalog anchor mount",
                    snapshot.path.display()
                ));
                continue;
            }
            if snapshot.logical_bytes == 0 {
                continue;
            }
            let target_measurement = if store.kind == CacheStore::Targets {
                if snapshot
                    .path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
                    || !target_generation_is_complete_at(&snapshot.directory)?
                {
                    continue;
                }
                let Some(measurement) = target_generation_size(&snapshot.path) else {
                    continue;
                };
                Some(measurement)
            } else {
                None
            };
            let (stable_workspace_scope, stable_workspace_last_use) = if store.kind
                == CacheStore::StableWorkspace
            {
                let Some(slot_key) = store.scope_prefix.first() else {
                    continue;
                };
                let Some((scope_parts, last_use)) = crate::stable_workspace::validate_candidate_at(
                    &store.path,
                    &snapshot.path,
                    slot_key,
                    &snapshot.directory,
                    &snapshot.identity,
                ) else {
                    continue;
                };
                (Some(scope_parts), Some(last_use))
            } else {
                (None, None)
            };
            let (bytes, modified) = target_measurement.unwrap_or((
                snapshot.logical_bytes,
                stable_workspace_last_use.unwrap_or(snapshot.newest_modified),
            ));
            if bytes == 0 {
                continue;
            }
            let candidate_path = snapshot.path.clone();
            let key = (store.kind, candidate_path.clone());
            if inventory.candidates.contains_key(&key) {
                inventory.failures.push(format!(
                    "skip duplicate cache candidate {}",
                    candidate_path.display()
                ));
                continue;
            }
            inventory.entries.push(CacheEntry {
                path: candidate_path.clone(),
                store: store.kind,
                scope: stable_workspace_scope.unwrap_or_else(|| {
                    store
                        .scope_prefix
                        .iter()
                        .cloned()
                        .chain(scope_parts(&store.path, &candidate_path, store.scope_depth))
                        .filter(|part| part != ".")
                        .collect()
                }),
                bytes,
                modified,
            });
            inventory.candidates.insert(
                key,
                PinnedCacheCandidate {
                    trusted_anchor: anchor.clone(),
                    anchor_identity: anchor_identity.clone(),
                    candidate_identity: snapshot.identity,
                    directory: snapshot.directory,
                },
            );
        }
    }
    Ok(inventory)
}

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    })
}

fn report_inventory_failures(failures: &[String], operation: &str) {
    for failure in failures {
        eprintln!("{operation}: {failure}");
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReclaimReport {
    pub freed_bytes: u64,
    pub deleted: Vec<PathBuf>,
    pub failures: Vec<String>,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn reclaim(
    layout: &crate::storage::StorageLayout,
    work_root: &Path,
    target_bytes: u64,
    in_use_scopes: &BTreeSet<String>,
) -> Result<ReclaimReport> {
    reclaim_work_root_with_layout(
        work_root,
        &layout.run_root,
        &layout.log_root,
        target_bytes,
        in_use_scopes,
        false,
        layout,
        None,
    )
}

/// Reclaim until the pinned filesystem reaches an absolute free-space floor.
/// The floor is evaluated from a fresh sample after both reclaim locks are held.
pub(crate) fn reclaim_for_capacity_floor_with_pin(
    layout: &crate::storage::StorageLayout,
    work_root: &Path,
    pressure_path: &Path,
    required_free_bytes: u64,
    in_use_scopes: &BTreeSet<String>,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
) -> Result<ReclaimReport> {
    if required_free_bytes == 0 {
        return Ok(ReclaimReport::default());
    }
    let pressure = pin_pressure_path(pressure_path, expected_pressure)?;
    let expected_volume_uuid = pressure_volume_uuid(&pressure)?;
    let pressure_device = expected_pressure.device_id();
    let pressure_sample = |_: &Path| {
        pressure
            .probe()
            .ok()
            .filter(|capacity| {
                capacity.filesystem_device == pressure_device
                    && capacity.volume_fingerprint.as_deref() == Some(&expected_volume_uuid)
            })
            .map(PressureSample::from_capacity)
    };
    reclaim_work_root_with_layout_on_device(
        work_root,
        &layout.run_root,
        &layout.log_root,
        ReclaimGoal::AvailableFloor(required_free_bytes),
        in_use_scopes,
        false,
        layout,
        None,
        Some(pressure_device),
        &candidate_device_id,
        Some(pressure_path),
        Some(&pressure),
        &pressure_sample,
    )
}

/// Reclaim until the pinned filesystem reaches `minimum_available_bytes`.
/// Every capacity measurement revalidates that same root identity before
/// crediting filesystem free space.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn reclaim_for_disk_pressure_with_pin(
    pressure_path: &Path,
    minimum_available_bytes: u64,
    roots: &[PathBuf],
    layout: &crate::storage::StorageLayout,
    backend: Option<velnor_model::ExecutionBackendKind>,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
) -> ReclaimReport {
    if minimum_available_bytes == 0 {
        return ReclaimReport::default();
    }
    reclaim_for_disk_pressure_with_pin_goal(
        pressure_path,
        ReclaimGoal::AvailableFloor(minimum_available_bytes),
        roots,
        layout,
        backend,
        expected_pressure,
    )
}

/// Reclaim while either the unprivileged free-space floor or hard utilization
/// threshold remains breached. This keeps cache candidates in the same
/// pressure pass when free space is sufficient but the filesystem is still
/// nearly full.
pub(crate) fn reclaim_for_disk_pressure_with_usage_pressure_pin(
    pressure_path: &Path,
    minimum_available_bytes: u64,
    hard_pressure_percent: u8,
    roots: &[PathBuf],
    layout: &crate::storage::StorageLayout,
    backend: Option<velnor_model::ExecutionBackendKind>,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
) -> ReclaimReport {
    if minimum_available_bytes == 0 || hard_pressure_percent == 0 {
        return ReclaimReport::default();
    }
    reclaim_for_disk_pressure_with_pin_goal(
        pressure_path,
        ReclaimGoal::DiskPressure {
            minimum_available_bytes,
            hard_pressure_percent,
        },
        roots,
        layout,
        backend,
        expected_pressure,
    )
}

fn reclaim_for_disk_pressure_with_pin_goal(
    pressure_path: &Path,
    goal: ReclaimGoal,
    roots: &[PathBuf],
    layout: &crate::storage::StorageLayout,
    backend: Option<velnor_model::ExecutionBackendKind>,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
) -> ReclaimReport {
    let pressure = match pin_pressure_path(pressure_path, expected_pressure) {
        Ok(pressure) => pressure,
        Err(error) => {
            return ReclaimReport {
                failures: vec![format!(
                    "skip disk-pressure cache reclaim: pressured filesystem identity changed at {}: {error:#}",
                    pressure_path.display()
                )],
                ..ReclaimReport::default()
            };
        }
    };
    let expected_volume_uuid = match pressure_volume_uuid(&pressure) {
        Ok(uuid) => uuid,
        Err(error) => {
            return ReclaimReport {
                failures: vec![format!("skip disk-pressure cache reclaim: {error:#}")],
                ..ReclaimReport::default()
            };
        }
    };
    let pressure_device = expected_pressure.device_id();
    let pressure_sample = |_: &Path| {
        pressure
            .probe()
            .ok()
            .filter(|capacity| {
                capacity.filesystem_device == pressure_device
                    && capacity.volume_fingerprint.as_deref() == Some(&expected_volume_uuid)
            })
            .map(PressureSample::from_capacity)
    };
    reclaim_for_disk_pressure_on_device(
        pressure_path,
        goal,
        roots,
        layout,
        backend,
        pressure_device,
        &candidate_device_id,
        Some(&pressure),
        &pressure_sample,
    )
}

#[derive(Clone, Copy, Debug)]
struct PressureSample {
    available_bytes: u64,
    used_percent: u8,
}

impl PressureSample {
    fn from_capacity(capacity: crate::host_capacity::HostCapacity) -> Self {
        Self {
            available_bytes: capacity.available_bytes,
            used_percent: capacity.used_percent(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ReclaimGoal {
    #[allow(dead_code)]
    Amount(u64),
    AvailableFloor(u64),
    DiskPressure {
        minimum_available_bytes: u64,
        hard_pressure_percent: u8,
    },
}

impl ReclaimGoal {
    fn still_pressured(self, sample: PressureSample, freed: u64) -> bool {
        match self {
            Self::Amount(target_bytes) => freed < target_bytes,
            Self::AvailableFloor(required_free_bytes) => {
                sample.available_bytes < required_free_bytes
            }
            Self::DiskPressure {
                minimum_available_bytes,
                hard_pressure_percent,
            } => {
                sample.available_bytes < minimum_available_bytes
                    || sample.used_percent >= hard_pressure_percent
            }
        }
    }

    fn remaining_bytes(self, sample: PressureSample, freed: u64) -> u64 {
        match self {
            Self::Amount(target_bytes) => target_bytes.saturating_sub(freed),
            Self::AvailableFloor(required_free_bytes) => {
                required_free_bytes.saturating_sub(sample.available_bytes)
            }
            Self::DiskPressure {
                minimum_available_bytes,
                hard_pressure_percent,
            } => {
                let free_deficit = minimum_available_bytes.saturating_sub(sample.available_bytes);
                if free_deficit > 0 {
                    free_deficit
                } else if sample.used_percent >= hard_pressure_percent {
                    1
                } else {
                    0
                }
            }
        }
    }
}

/// Control-flow stop: pressure cleared at an unlink boundary before this
/// candidate's own deletions covered the baseline deficit, so the clear
/// came from elsewhere. The removal helper restores the quarantined
/// remainder; the reclaim loop stops without recording a failure.
#[derive(Debug)]
struct PressureClearedAtUnlinkBoundary {
    path: PathBuf,
}

impl std::fmt::Display for PressureClearedAtUnlinkBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pressure cleared before deleting {}",
            self.path.display()
        )
    }
}

impl std::error::Error for PressureClearedAtUnlinkBoundary {}

fn pressure_volume_uuid(pressure: &crate::host_capacity::HostCapacityPin) -> Result<String> {
    let capacity = pressure
        .probe()
        .context("probe pinned pressure filesystem")?;
    if capacity.filesystem_device != pressure.device_id() {
        bail!("pinned pressure filesystem device changed during capacity probe");
    }
    capacity
        .volume_fingerprint
        .filter(|uuid| !uuid.is_empty())
        .context("pinned pressure filesystem has no stable UUID")
}

fn pin_pressure_path(
    pressure_path: &Path,
    expected_pressure: &crate::host_capacity::HostCapacityPin,
) -> Result<crate::host_capacity::HostCapacityPin> {
    expected_pressure
        .revalidate()
        .context("revalidate pressure root captured by admission")?;
    let pressure = crate::host_capacity::HostCapacityPin::open(pressure_path)
        .with_context(|| format!("pin pressured filesystem at {}", pressure_path.display()))?;
    if pressure.identity() != expected_pressure.identity() {
        bail!(
            "pressure path {} no longer identifies the admitted filesystem root",
            pressure_path.display()
        );
    }
    Ok(pressure)
}

#[allow(clippy::too_many_arguments)]
fn reclaim_for_disk_pressure_on_device(
    pressure_path: &Path,
    goal: ReclaimGoal,
    roots: &[PathBuf],
    layout: &crate::storage::StorageLayout,
    backend: Option<velnor_model::ExecutionBackendKind>,
    pressure_device: u64,
    candidate_device: &impl Fn(&Path, u64) -> Option<u64>,
    expected_pressure: Option<&crate::host_capacity::HostCapacityPin>,
    pressure_sample: &impl Fn(&Path) -> Option<PressureSample>,
) -> ReclaimReport {
    let measurement_failed = std::cell::Cell::new(false);
    let pressure_stopped = std::cell::Cell::new(false);
    let pass_baseline = std::cell::Cell::new(None::<PressureSample>);
    let measure_pressure = |path: &Path| {
        let sample = pressure_sample(path);
        if sample.is_none() {
            measurement_failed.set(true);
        }
        if let (Some(current), Some(baseline)) = (sample, pass_baseline.get()) {
            let freed = observed_net_freed_bytes(baseline.available_bytes, current.available_bytes);
            if !goal.still_pressured(current, freed) {
                pressure_stopped.set(true);
            }
        }
        sample
    };
    let mut report = ReclaimReport::default();
    if let Err(error) =
        crate::leftover_disk::recover_cache_quarantines_if_pending(&layout.run_root, layout, roots)
    {
        report.failures.push(format!(
            "defer disk-pressure cache reclaim until interrupted cleanup recovery succeeds: {error:#}"
        ));
        return report;
    }
    let baseline = match measure_pressure(pressure_path) {
        Some(sample) => sample,
        None => {
            report.failures.push(format!(
                "skip disk-pressure cache reclaim: cannot measure pressured filesystem at {}",
                pressure_path.display()
            ));
            return report;
        }
    };
    pass_baseline.set(Some(baseline));
    if !goal.still_pressured(baseline, 0) {
        pressure_stopped.set(true);
        return report;
    }

    for work_root in roots {
        if pressure_stopped.get() {
            break;
        }
        let run_root = layout.run_root.clone();
        let log_root = layout.log_root.clone();
        let current = match measure_pressure(pressure_path) {
            Some(current) => {
                report.freed_bytes =
                    observed_net_freed_bytes(baseline.available_bytes, current.available_bytes);
                current
            }
            None => {
                report.freed_bytes = 0;
                report.failures.push(format!(
                    "stop disk-pressure cache reclaim: cannot remeasure pressured filesystem at {}",
                    pressure_path.display()
                ));
                break;
            }
        };
        if !goal.still_pressured(current, report.freed_bytes) {
            pressure_stopped.set(true);
            break;
        }
        let remaining = goal.remaining_bytes(current, report.freed_bytes);
        if remaining == 0 {
            break;
        }
        match reclaim_work_root_with_layout_on_device(
            work_root,
            &run_root,
            &log_root,
            goal,
            &BTreeSet::new(),
            true,
            layout,
            backend,
            Some(pressure_device),
            candidate_device,
            Some(pressure_path),
            expected_pressure,
            &measure_pressure,
        ) {
            Ok(root_report) => {
                report.deleted.extend(root_report.deleted);
                report.failures.extend(root_report.failures);
                if measurement_failed.get() {
                    report.freed_bytes = 0;
                    report.failures.push(format!(
                        "stop disk-pressure cache reclaim: pressured filesystem identity or capacity probe failed at {}",
                        pressure_path.display()
                    ));
                    break;
                }
                if pressure_stopped.get() {
                    break;
                }
                match measure_pressure(pressure_path) {
                    Some(current) => {
                        report.freed_bytes = observed_net_freed_bytes(
                            baseline.available_bytes,
                            current.available_bytes,
                        );
                    }
                    None => {
                        report.freed_bytes = 0;
                        report.failures.push(format!(
                            "cache reclaim completed for {} but pressured filesystem could not be remeasured at {}",
                            work_root.display(),
                            pressure_path.display()
                        ));
                    }
                }
                if pressure_stopped.get() {
                    break;
                }
            }
            Err(error) => {
                if pressure_stopped.get() {
                    break;
                }
                report
                    .failures
                    .push(format!("{}: {error:#}", work_root.display()));
            }
        }
    }

    if measurement_failed.get() {
        report.freed_bytes = 0;
    } else if let Some(current) = measure_pressure(pressure_path) {
        report.freed_bytes =
            observed_net_freed_bytes(baseline.available_bytes, current.available_bytes);
    } else {
        report.freed_bytes = 0;
        report.failures.push(format!(
            "disk-pressure cache reclaim completed but pressured filesystem could not be remeasured at {}",
            pressure_path.display()
        ));
    }

    report
}

#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
fn reclaim_work_root_with_layout(
    work_root: &Path,
    run_root: &Path,
    log_root: &Path,
    target_bytes: u64,
    in_use_scopes: &BTreeSet<String>,
    emergency: bool,
    layout: &crate::storage::StorageLayout,
    backend: Option<velnor_model::ExecutionBackendKind>,
) -> Result<ReclaimReport> {
    reclaim_work_root_with_layout_on_device(
        work_root,
        run_root,
        log_root,
        ReclaimGoal::Amount(target_bytes),
        in_use_scopes,
        emergency,
        layout,
        backend,
        None,
        &candidate_device_id,
        None,
        None,
        &pressure_available_bytes,
    )
}

#[allow(clippy::too_many_arguments)]
fn reclaim_work_root_with_layout_on_device(
    work_root: &Path,
    run_root: &Path,
    log_root: &Path,
    goal: ReclaimGoal,
    in_use_scopes: &BTreeSet<String>,
    emergency: bool,
    layout: &crate::storage::StorageLayout,
    backend: Option<velnor_model::ExecutionBackendKind>,
    pressure_device: Option<u64>,
    candidate_device: &impl Fn(&Path, u64) -> Option<u64>,
    pressure_path: Option<&Path>,
    expected_pressure: Option<&crate::host_capacity::HostCapacityPin>,
    pressure_sample: &impl Fn(&Path) -> Option<PressureSample>,
) -> Result<ReclaimReport> {
    let _lock = match GcLeaderLock::acquire(run_root) {
        Ok(lock) => lock,
        Err(error) if error.downcast_ref::<GcLeaderLockHeld>().is_some() => {
            eprintln!("capacity reclaim already running in another daemon; rechecking later");
            return Ok(ReclaimReport::default());
        }
        Err(error) => return Err(error),
    };
    // Publish/snapshot leases under one filesystem-wide coordinator. A daemon
    // starting a job cannot race between this snapshot and candidate deletion.
    let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(run_root)?;
    let recovery_work_roots = [work_root.to_path_buf()];
    crate::leftover_disk::recover_cache_quarantines_under_coordinator(
        run_root,
        layout,
        &recovery_work_roots,
    )
    .context("recover interrupted cache cleanup quarantines")?;
    let mut active_scopes = in_use_scopes.clone();
    active_scopes.extend(crate::capacity::active_scopes(
        run_root,
        Duration::from_secs(24 * 3600),
    )?);
    let scope = StoreScope::with_layout(layout);
    let mut report = ReclaimReport::default();
    // Stable workspaces are persistent user build state. Generic emergency
    // reclaim is not sufficient authority to remove them: only pressure
    // reclaim with a pinned, device-matched filesystem identity may inventory
    // stable-workspace candidates. `expected_pressure` is supplied only by
    // the public pressure entry points after they revalidate the requested
    // path against the admitted pin.
    let reclaim_stable_workspaces = emergency
        && pressure_path.is_some()
        && expected_pressure.is_some_and(|pin| Some(pin.device_id()) == pressure_device);
    if reclaim_stable_workspaces {
        for stable_root in store_roots(work_root, &scope)?
            .into_iter()
            .filter(|store| store.kind == CacheStore::StableWorkspace)
            .map(|store| store.path)
        {
            if let Err(error) = crate::stable_workspace::recover_abandoned_staging_under_coordinator(
                &stable_root,
                &active_scopes,
            ) {
                report.failures.push(format!(
                    "stable-workspace staging recovery skipped at {}: {error:#}",
                    stable_root.display()
                ));
            }
        }
    }
    let inventory =
        pinned_cache_inventory(work_root, emergency, &scope, reclaim_stable_workspaces)?;
    report.failures.extend(inventory.failures.iter().cloned());
    let mut entries = inventory.entries.clone();
    if let Some(expected_device) = pressure_device {
        entries.retain(|entry| {
            inventory
                .candidates
                .get(&(entry.store, entry.path.clone()))
                .is_some_and(|pinned| {
                    pinned.candidate_identity.device == expected_device
                        && candidate_device(&entry.path, pinned.candidate_identity.device)
                            == Some(expected_device)
                })
        });
    }
    let policy = EvictionPolicy {
        now: SystemTime::now(),
        keep_newest_per_target_scope: 0,
        max_age: Duration::ZERO,
        max_total_bytes: None,
        class_budgets: BTreeMap::new(),
        compiler_budget_bytes: None,
        in_use_scopes: active_scopes,
        protected_paths: pointer_protected_target_generations(work_root, &scope)?,
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
    let pressure_pass_baseline = if let Some(pressure_path) = pressure_path {
        match pressure_sample(pressure_path) {
            Some(sample) => Some(sample),
            None => {
                report.failures.push(format!(
                    "skip cache reclaim: cannot measure pressured filesystem at {}",
                    pressure_path.display()
                ));
                return Ok(report);
            }
        }
    } else {
        None
    };
    if let Some(baseline) = pressure_pass_baseline
        && !goal.still_pressured(baseline, 0)
    {
        return Ok(report);
    }
    let pressure_measurements_valid =
        std::cell::Cell::new(pressure_pass_baseline.is_some() || pressure_path.is_none());
    let pressure_cleared = std::cell::Cell::new(false);
    // Pressureless amount reclaim accounts candidate logical bytes after each
    // deletion. Pressure-driven passes instead stop from fresh filesystem
    // samples, so keep this local byte bound limited to the pressureless path.
    let pressureless_amount_target = match (pressure_path, goal) {
        (None, ReclaimGoal::Amount(target_bytes)) => Some(target_bytes),
        _ => None,
    };
    for entry in entries {
        if !pressure_measurements_valid.get() {
            break;
        }
        if pressureless_amount_target.is_some_and(|target| report.freed_bytes >= target) {
            break;
        }
        if let (Some(pressure_path), Some(baseline)) = (pressure_path, pressure_pass_baseline) {
            match pressure_sample(pressure_path) {
                Some(current) => {
                    report.freed_bytes =
                        observed_net_freed_bytes(baseline.available_bytes, current.available_bytes);
                    if !goal.still_pressured(current, report.freed_bytes) {
                        pressure_cleared.set(true);
                        break;
                    }
                }
                None => {
                    report.freed_bytes = 0;
                    pressure_measurements_valid.set(false);
                    report.failures.push(format!(
                        "stop cache reclaim before deletion: pressured filesystem could not be revalidated at {}",
                        pressure_path.display()
                    ));
                    break;
                }
            }
        }
        let candidate = EvictionCandidate {
            path: entry.path,
            store: entry.store,
            scope: entry.scope,
            bytes: entry.bytes,
            reason: "reclaim-target".into(),
        };
        let validate_pressure_before_delete = |_unlinking: &Path| {
            if let (Some(pressure_path), Some(baseline)) = (pressure_path, pressure_pass_baseline) {
                match pressure_sample(pressure_path) {
                    Some(current) => {
                        let freed = observed_net_freed_bytes(
                            baseline.available_bytes,
                            current.available_bytes,
                        );
                        if !goal.still_pressured(current, freed) {
                            // This hook runs before every unlink of the
                            // in-progress candidate, whose paths may already
                            // be quarantine-renamed. When the candidate
                            // alone covers the baseline deficit, its own
                            // unlinks presumably cleared the goal: finish
                            // the remainder, since aborting now would leave
                            // a half-removed directory that is neither
                            // reported nor accounted. Otherwise the clear
                            // came from elsewhere: bail so the quarantined
                            // remainder is restored and the pass stops
                            // before touching another candidate.
                            pressure_cleared.set(true);
                            if candidate.bytes < goal.remaining_bytes(baseline, 0) {
                                return Err(anyhow::anyhow!(PressureClearedAtUnlinkBoundary {
                                    path: candidate.path.clone(),
                                }));
                            }
                        }
                    }
                    None => {
                        pressure_measurements_valid.set(false);
                        bail!(
                            "pressured filesystem could not be revalidated before deleting {}",
                            candidate.path.display()
                        );
                    }
                }
            }
            Ok(())
        };
        let removed = inventory
            .candidates
            .get(&(candidate.store, candidate.path.clone()))
            .with_context(|| {
                format!(
                    "cache candidate descriptor was not retained: {}",
                    candidate.path.display()
                )
            })
            .and_then(|pinned| {
                let expected_device = pressure_device.unwrap_or(pinned.anchor_identity.device);
                remove_candidate(
                    &candidate,
                    pinned,
                    expected_device,
                    &validate_pressure_before_delete,
                )
            });
        match removed {
            Ok(CandidateRemovalOutcome::Removed) => {
                report.deleted.push(candidate.path.clone());
                let stop_after_delete = if let Some(pressure_path) = pressure_path {
                    match (pressure_pass_baseline, pressure_sample(pressure_path)) {
                        (Some(baseline), Some(after)) => {
                            report.freed_bytes = observed_net_freed_bytes(
                                baseline.available_bytes,
                                after.available_bytes,
                            );
                            if !goal.still_pressured(after, report.freed_bytes) {
                                pressure_cleared.set(true);
                                true
                            } else {
                                false
                            }
                        }
                        _ => {
                            report.freed_bytes = 0;
                            pressure_measurements_valid.set(false);
                            report.failures.push(format!(
                                "cache candidate deleted but pressure filesystem could not be remeasured at {}",
                                pressure_path.display()
                            ));
                            true
                        }
                    }
                } else {
                    report.freed_bytes = report.freed_bytes.saturating_add(candidate.bytes);
                    false
                };
                if let Err(error) = append_gc_history(log_root, &candidate, None, "deleted") {
                    report.failures.push(format!(
                        "{}: deleted but could not append GC history: {error:#}",
                        candidate.path.display()
                    ));
                }
                if stop_after_delete {
                    break;
                }
            }
            Ok(CandidateRemovalOutcome::SkippedBusy) => {
                if let Err(error) = append_gc_history(log_root, &candidate, None, "skipped") {
                    report.failures.push(format!(
                        "{}: could not append skipped GC history: {error:#}",
                        candidate.path.display()
                    ));
                }
            }
            Err(error) => {
                if error
                    .downcast_ref::<PressureClearedAtUnlinkBoundary>()
                    .is_some()
                {
                    break;
                }
                report
                    .failures
                    .push(format!("{}: {error}", candidate.path.display()));
                if let Err(history_error) = append_gc_history(log_root, &candidate, None, "failed")
                {
                    report.failures.push(format!(
                        "{}: could not append GC failure history: {history_error:#}",
                        candidate.path.display()
                    ));
                }
            }
        }
        if !pressure_measurements_valid.get() {
            break;
        }
    }
    if emergency
        && pressure_device.is_some()
        && expected_pressure.is_some()
        && pressure_measurements_valid.get()
        && !pressure_cleared.get()
    {
        if let (Some(pressure_path), Some(baseline)) = (pressure_path, pressure_pass_baseline) {
            match pressure_sample(pressure_path) {
                Some(current) => {
                    report.freed_bytes =
                        observed_net_freed_bytes(baseline.available_bytes, current.available_bytes);
                }
                None => {
                    report.freed_bytes = 0;
                    pressure_measurements_valid.set(false);
                    report.failures.push(format!(
                        "skip BuildKit reclaim: pressured filesystem could not be revalidated at {}",
                        pressure_path.display()
                    ));
                }
            }
        } else {
            pressure_measurements_valid.set(false);
        }
    }
    // The claim boundary this path used to lack now exists: builders with any
    // job hold are skipped no matter how large, and holds from vanished job
    // containers are repaired first. Only when the file stores cannot clear
    // the current pressure does emergency reclaim prune unclaimed builders,
    // largest first — bounded, measured, and cold-only. This is what makes
    // the claim-aware emergency BuildKit reclaim live.
    let still_pressured = match (pressure_path, pressure_pass_baseline) {
        (Some(path), Some(baseline)) if pressure_measurements_valid.get() => {
            match pressure_sample(path) {
                Some(current) => {
                    report.freed_bytes =
                        observed_net_freed_bytes(baseline.available_bytes, current.available_bytes);
                    let still_pressured = goal.still_pressured(current, report.freed_bytes);
                    if !still_pressured {
                        pressure_cleared.set(true);
                    }
                    still_pressured
                }
                None => {
                    report.freed_bytes = 0;
                    pressure_measurements_valid.set(false);
                    report.failures.push(format!(
                        "skip BuildKit reclaim: pressured filesystem could not be revalidated at {}",
                        path.display()
                    ));
                    false
                }
            }
        }
        _ => false,
    };
    if emergency
        && pressure_device.is_some()
        && pressure_measurements_valid.get()
        && !pressure_cleared.get()
        && still_pressured
        && let Some(expected_pressure) = expected_pressure
        && let Some(baseline_for_goal) = pressure_pass_baseline
    {
        let pressure_predicate = |capacity: &crate::host_capacity::HostCapacity| {
            goal.still_pressured(
                PressureSample::from_capacity(capacity.clone()),
                observed_net_freed_bytes(
                    baseline_for_goal.available_bytes,
                    capacity.available_bytes,
                ),
            )
        };
        let pruned = pressure_prune_buildkit_with_backend(
            backend,
            layout,
            Some(expected_pressure),
            &pressure_predicate,
            |domain, pressure_pin, pressure_predicate| {
                match crate::buildkit::reclaim_domain_buildkit_for_device(
                    domain,
                    pressure_pin,
                    pressure_predicate,
                ) {
                    Ok(report) => report,
                    Err(error) => crate::buildkit::PressurePruneReport {
                        failures: vec![format!("skip BuildKit pressure reclaim: {error:#}")],
                        ..crate::buildkit::PressurePruneReport::default()
                    },
                }
            },
        );
        report.freed_bytes = if let (Some(pressure_path), Some(baseline)) =
            (pressure_path, pressure_pass_baseline)
        {
            match pressure_sample(pressure_path) {
                Some(after) => after
                    .available_bytes
                    .saturating_sub(baseline.available_bytes),
                None => {
                    report.freed_bytes = 0;
                    report.failures.push(format!(
                        "BuildKit reclaim ran but pressured filesystem could not be remeasured at {}",
                        pressure_path.display()
                    ));
                    report.freed_bytes
                }
            }
        } else {
            report.freed_bytes.saturating_add(pruned.freed_bytes)
        };
        for builder in pruned.pruned {
            tracing::info!(builder = %builder, "emergency reclaim pruned unclaimed BuildKit builder");
        }
        report.failures.extend(pruned.failures);
    }
    Ok(report)
}

fn pressure_prune_buildkit_with_backend(
    backend: Option<velnor_model::ExecutionBackendKind>,
    layout: &crate::storage::StorageLayout,
    expected_pressure: Option<&crate::host_capacity::HostCapacityPin>,
    pressure_predicate: &impl Fn(&crate::host_capacity::HostCapacity) -> bool,
    prune: impl FnOnce(
        &crate::buildkit::PersistentBuildKitDomain,
        &crate::host_capacity::HostCapacityPin,
        &dyn Fn(&crate::host_capacity::HostCapacity) -> bool,
    ) -> crate::buildkit::PressurePruneReport,
) -> crate::buildkit::PressurePruneReport {
    let Some(expected_pressure) = expected_pressure else {
        return crate::buildkit::PressurePruneReport::default();
    };
    if !velnor_model::ExecutionBackendKind::permits_host_docker_maintenance(backend) {
        return crate::buildkit::PressurePruneReport::default();
    }
    match expected_pressure.probe() {
        Ok(capacity) if pressure_predicate(&capacity) => {}
        Ok(_) => return crate::buildkit::PressurePruneReport::default(),
        Err(error) => {
            return crate::buildkit::PressurePruneReport {
                failures: vec![format!("skip BuildKit pressure prune: {error:#}")],
                ..crate::buildkit::PressurePruneReport::default()
            };
        }
    }
    let resolved_domain =
        crate::buildkit::PersistentBuildKitDomain::resolve_from_layout(layout.clone()).map(Some);
    match resolved_domain {
        Ok(Some(domain)) => prune(&domain, expected_pressure, pressure_predicate),
        Ok(None) => crate::buildkit::PressurePruneReport::default(),
        Err(error) => crate::buildkit::PressurePruneReport {
            failures: vec![format!("skip BuildKit pressure prune: {error:#}")],
            ..crate::buildkit::PressurePruneReport::default()
        },
    }
}

/// Bring the compiler stores (mbx + sccache, every scope, repository and
/// slot under `work_root`) back under `budget_bytes`, evicting the
/// least-recently-modified per-slot trees first.
///
/// This is the daemon's enforcement of [`crate::capacity::StoreBudgetPolicy`]:
/// it runs at daemon start and at every job admission (after the job has
/// published its own store leases, so the store it is about to use is never a
/// victim). Leases of every live job on the host protect their repositories;
/// the GC leader lock and the filesystem coordinator serialise it against
/// `cache gc` and the emergency reclaimer exactly like every other pass.
/// Another daemon already holding the leader lock is contention, not
/// failure: the pass reports nothing and the next admission tries again.
pub(crate) fn enforce_compiler_store_budget(
    work_root: &Path,
    layout: &crate::storage::StorageLayout,
    budget_bytes: u64,
) -> Result<ReclaimReport> {
    let _lock = match GcLeaderLock::acquire(&layout.run_root) {
        Ok(lock) => lock,
        Err(error) if error.downcast_ref::<GcLeaderLockHeld>().is_some() => {
            eprintln!("compiler store budget pass skipped: {error}");
            return Ok(ReclaimReport::default());
        }
        Err(error) => return Err(error),
    };
    let _coordinator = crate::capacity::FilesystemCoordinator::lock_exclusive(&layout.run_root)?;
    let in_use_scopes =
        crate::capacity::active_scopes(&layout.run_root, Duration::from_secs(24 * 3600))?;
    let inventory =
        pinned_cache_inventory(work_root, false, &StoreScope::with_layout(layout), false)?;
    report_inventory_failures(&inventory.failures, "compiler store budget");
    let entries: Vec<CacheEntry> = inventory
        .entries
        .iter()
        .filter(|entry| entry.store.is_compiler())
        .cloned()
        .collect();
    let policy = EvictionPolicy {
        now: SystemTime::now(),
        keep_newest_per_target_scope: 0,
        max_age: Duration::MAX,
        max_total_bytes: None,
        class_budgets: BTreeMap::new(),
        compiler_budget_bytes: Some(budget_bytes),
        in_use_scopes,
        protected_paths: BTreeSet::new(),
    };
    let mut report = ReclaimReport {
        failures: inventory.failures.clone(),
        ..ReclaimReport::default()
    };
    for candidate in select_eviction_candidates(&entries, &policy) {
        let removed = inventory
            .candidates
            .get(&(candidate.store, candidate.path.clone()))
            .with_context(|| {
                format!(
                    "compiler cache candidate descriptor was not retained: {}",
                    candidate.path.display()
                )
            })
            .and_then(|pinned| {
                remove_candidate(&candidate, pinned, pinned.anchor_identity.device, &|_| {
                    Ok(())
                })
            });
        match removed {
            Ok(CandidateRemovalOutcome::Removed) => {
                report.freed_bytes = report.freed_bytes.saturating_add(candidate.bytes);
                report.deleted.push(candidate.path.clone());
                append_gc_history(&layout.log_root, &candidate, Some(&policy), "deleted")?;
            }
            Ok(CandidateRemovalOutcome::SkippedBusy) => {
                append_gc_history(&layout.log_root, &candidate, Some(&policy), "skipped")?;
            }
            Err(error) => {
                report
                    .failures
                    .push(format!("{}: {error}", candidate.path.display()));
                append_gc_history(&layout.log_root, &candidate, Some(&policy), "failed")?;
            }
        }
    }
    Ok(report)
}

fn remove_candidate(
    candidate: &EvictionCandidate,
    pinned: &PinnedCacheCandidate,
    expected_device: u64,
    before_delete: &impl Fn(&Path) -> Result<()>,
) -> Result<CandidateRemovalOutcome> {
    if candidate.store == CacheStore::StableWorkspace {
        let stable_root = candidate
            .path
            .parent()
            .and_then(Path::parent)
            .context("stable-workspace candidate is outside its scope/repository layout")?;
        let slot_key = candidate
            .scope
            .first()
            .context("stable-workspace candidate has no slot lease key")?;
        if crate::stable_workspace::validate_candidate_at(
            stable_root,
            &candidate.path,
            slot_key,
            &pinned.directory,
            &pinned.candidate_identity,
        )
        .is_none()
        {
            bail!(
                "stable-workspace candidate lost its ownership marker or workspace subtree: {}",
                candidate.path.display()
            );
        }
    }
    let lock_path = if candidate.store == CacheStore::Targets {
        candidate.path.parent().unwrap_or(&candidate.path)
    } else {
        &candidate.path
    };
    let _entry_lock = (matches!(
        candidate.store,
        CacheStore::ActionsCache | CacheStore::Artifacts | CacheStore::Targets
    ))
    .then(|| {
        CacheEntryLock::exclusive_under_anchor(
            lock_path,
            &pinned.trusted_anchor,
            &pinned.anchor_identity,
        )
    })
    .transpose()?;
    let _gha_tenant_gc_guard = if candidate.store == CacheStore::GhaCache {
        let namespace = candidate
            .path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .context("GHA cache candidate has no UTF-8 tenant namespace")?;
        let tenants_directory = candidate
            .path
            .parent()
            .context("GHA cache candidate has no tenants directory")?;
        let cache_root = tenants_directory
            .parent()
            .context("GHA cache tenants directory has no cache root")?;
        match crate::gha_cache::try_lock_tenant_for_cache_gc(
            cache_root,
            namespace,
            &candidate.path,
            &pinned.directory,
        )? {
            Some(guard) => Some(guard),
            // The GHA guard also reports preserved or unproven file
            // quarantines as busy, so whole-tenant deletion cannot erase
            // recovery data.
            None => return Ok(CandidateRemovalOutcome::SkippedBusy),
        }
    } else {
        None
    };
    if candidate.store == CacheStore::Targets {
        if target_generation_is_current(&candidate.path)? {
            bail!(
                "refusing to remove current target generation: {}",
                candidate.path.display()
            );
        }
        let complete = target_generation_is_complete_at(&pinned.directory)?;
        if !complete {
            bail!(
                "target generation changed or became incomplete before removal: {}",
                candidate.path.display()
            );
        }
        crate::leftover_disk::remove_dir_all_on_device_under_pinned_with_pre_unlink(
            &pinned.trusted_anchor,
            &candidate.path,
            expected_device,
            &pinned.anchor_identity,
            &pinned.candidate_identity,
            &pinned.directory,
            &|path| before_delete(path),
        )?;
        return Ok(CandidateRemovalOutcome::Removed);
    }
    crate::leftover_disk::remove_dir_all_on_device_under_pinned_with_pre_unlink(
        &pinned.trusted_anchor,
        &candidate.path,
        expected_device,
        &pinned.anchor_identity,
        &pinned.candidate_identity,
        &pinned.directory,
        &|path| before_delete(path),
    )?;
    Ok(CandidateRemovalOutcome::Removed)
}

fn trusted_catalog_anchor(
    work_root: &Path,
    layout: &crate::storage::StorageLayout,
    catalog_roots: &[StoreRoot],
    candidate: &Path,
) -> Option<PathBuf> {
    if !catalog_roots
        .iter()
        .any(|root| candidate.strip_prefix(&root.path).is_ok())
    {
        return None;
    }
    if let Some(stable_root) = catalog_roots.iter().find(|root| {
        root.kind == CacheStore::StableWorkspace
            && root.path == work_root
            && candidate.strip_prefix(&root.path).is_ok()
    }) {
        // Exact-root pressure callers can pass the stable root itself as the
        // work root. Anchoring there would canonicalize a final-component
        // symlink and bless its outside target; anchor at the slot directory
        // so secure inventory opens the stable root as a no-follow child.
        return stable_root.path.parent().map(Path::to_path_buf);
    }
    let trust_scope_cache_root = crate::trust_scope::filesystem_key_namespace(&layout.cache_root);
    if candidate.strip_prefix(&trust_scope_cache_root).is_ok() {
        return Some(trust_scope_cache_root);
    }
    if candidate.strip_prefix(&layout.cache_root).is_ok() {
        return Some(layout.cache_root.clone());
    }
    candidate
        .strip_prefix(work_root)
        .is_ok()
        .then(|| work_root.to_path_buf())
}

/// Test helper for the nearest-existing-ancestor behavior of pressure pins.
#[cfg(test)]
pub(crate) fn filesystem_device_id(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;

    let mut probe = path;
    loop {
        match fs::metadata(probe) {
            Ok(metadata) => return Some(metadata.dev()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                probe = probe.parent()?;
            }
            Err(_) => return None,
        }
    }
}

fn observed_net_freed_bytes(baseline: u64, current: u64) -> u64 {
    current.saturating_sub(baseline)
}

/// Test seam for candidate-device filtering. Production uses the device
/// captured by the descriptor-relative inventory, never a later path lookup.
fn candidate_device_id(_path: &Path, captured_device: u64) -> Option<u64> {
    Some(captured_device)
}

#[cfg_attr(not(test), allow(dead_code))]
fn pressure_available_bytes(path: &Path) -> Option<PressureSample> {
    crate::host_capacity::HostCapacityPin::open(path)
        .ok()?
        .probe()
        .ok()
        .map(PressureSample::from_capacity)
}
fn reclaim_priority(store: CacheStore) -> u8 {
    match store {
        CacheStore::Artifacts => 0,
        CacheStore::GhaCache => 1,
        CacheStore::GitMirrors => 2,
        CacheStore::ActionsCache => 3,
        CacheStore::Targets => 4,
        CacheStore::Cargo => 5,
        CacheStore::Mise => 6,
        CacheStore::Mbx | CacheStore::Sccache => 7,
        CacheStore::StableWorkspace => 8,
        // Never reclaimed by scope: Velnor does not own Docker's store beyond
        // its own builder. It is accounted, not evicted.
        CacheStore::Docker => u8::MAX,
    }
}

#[cfg(test)]
fn collect_candidates(
    store: &StoreRoot,
    path: &Path,
    depth: usize,
    entries: &mut Vec<CacheEntry>,
) -> Result<()> {
    // Stable workspaces may only enter a deletion set through the pinned
    // emergency-pressure inventory below. This path-recursive collector does
    // not carry a trusted root descriptor or an ownership-marker proof.
    if store.kind == CacheStore::StableWorkspace {
        return Ok(());
    }
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

#[cfg(test)]
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
    /// Host-level bound on the compiler classes together (mbx + sccache,
    /// every scope, repository and slot), from
    /// [`crate::capacity::StoreBudgetPolicy`]. Unlike `class_budgets`, zero
    /// is a real budget: a host whose slots alone exhaust the disk keeps no
    /// compiler store. `None` leaves the classes to the other rules.
    pub(crate) compiler_budget_bytes: Option<u64>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateRemovalOutcome {
    Removed,
    SkippedBusy,
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

    if let Some(budget) = policy.compiler_budget_bytes {
        let mut remaining: u64 = entries
            .iter()
            .filter(|entry| entry.store.is_compiler())
            .map(|entry| entry.bytes)
            .sum();
        let mut oldest: Vec<&CacheEntry> = entries
            .iter()
            .filter(|entry| {
                entry.store.is_compiler() && !in_use(entry, policy) && !protected(entry, policy)
            })
            .collect();
        oldest.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        });
        for entry in oldest {
            if remaining <= budget {
                break;
            }
            remaining = remaining.saturating_sub(entry.bytes);
            add_candidate(&mut candidates, entry, "over-compiler-budget");
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
    let candidate = format!("{}/{}", entry.store, entry.scope_key());
    policy.in_use_scopes.iter().any(|active| {
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

fn target_generation_is_complete_at(directory: &fs::File) -> Result<bool> {
    let entries = crate::leftover_disk::filesystem_entries_at(directory)?;
    let marker = entries.iter().any(|entry| {
        entry.name == ".velnor-target-complete-v1"
            && entry.kind == crate::leftover_disk::FilesystemEntryKind::RegularFile
            && !entry.is_mountpoint
    });
    let data = entries.iter().any(|entry| {
        entry.name == "data"
            && entry.kind == crate::leftover_disk::FilesystemEntryKind::Directory
            && !entry.is_mountpoint
    });
    Ok(marker && data)
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

fn pointer_protected_target_generations(
    work_root: &Path,
    scope: &StoreScope,
) -> Result<BTreeSet<PathBuf>> {
    let mut protected = BTreeSet::new();
    for store in store_roots(work_root, scope)?
        .into_iter()
        .filter(|store| store.kind == CacheStore::Targets)
    {
        collect_pointer_protected(&store.path, store.scope_depth, &mut protected);
    }
    Ok(protected)
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
pub(crate) mod test_clock {
    use std::path::Path;
    use std::time::Duration;
    #[cfg(target_os = "linux")]
    use std::time::SystemTime;

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

    #[cfg(target_os = "linux")]
    pub(crate) fn set_modified(path: &Path, modified: SystemTime) {
        let stamp = rustix::fs::Timespec {
            tv_sec: modified
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
    use test_clock::backdate;
    #[cfg(target_os = "linux")]
    use test_clock::set_modified;

    fn sccache_root(
        work_root: &Path,
        trust_scope: &str,
        layout: &crate::storage::StorageLayout,
    ) -> PathBuf {
        crate::store_catalog::StoreCatalog::for_work_root_with_layout(work_root, layout)
            .sccache(trust_scope)
    }

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

    fn pin_cache_candidate(anchor: &Path, path: &Path) -> PinnedCacheCandidate {
        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(anchor).unwrap();
        let (directory, candidate_identity) =
            crate::leftover_disk::filesystem_pin_directory_under(anchor, path, &anchor_identity)
                .unwrap();
        PinnedCacheCandidate {
            trusted_anchor: anchor.to_path_buf(),
            anchor_identity,
            candidate_identity,
            directory,
        }
    }

    fn policy() -> EvictionPolicy {
        EvictionPolicy {
            now: SystemTime::UNIX_EPOCH + DAY * 100,
            keep_newest_per_target_scope: 2,
            max_age: DAY * 30,
            max_total_bytes: None,
            class_budgets: BTreeMap::new(),
            compiler_budget_bytes: None,
            in_use_scopes: BTreeSet::new(),
            protected_paths: BTreeSet::new(),
        }
    }

    #[test]
    fn work_dir_override_changes_only_the_shared_work_root() {
        let prefix =
            std::env::temp_dir().join(format!("velnor-work-override-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&prefix.join("storage"));
        let explicit_work = prefix.join("custom-work/slot-4");
        let shared_work = super::work_root(None, Some(explicit_work.clone())).unwrap();
        let expected_work = crate::container::daemon_shared_root(explicit_work);
        assert_eq!(shared_work, expected_work);

        let scope = StoreScope {
            layout: Some(layout.clone()),
            pool_trust_scope: crate::trust_scope::TRUSTED.to_owned(),
            daemon_environment: None,
        };
        let roots = store_roots(&shared_work, &scope).unwrap();
        assert!(roots.iter().any(|root| {
            root.kind == CacheStore::Cargo
                && root.path
                    == layout
                        .cache_class(crate::trust_scope::TRUSTED, "cargo")
                        .join("registry")
        }));
        assert!(roots.iter().any(|root| {
            root.kind == CacheStore::GhaCache
                && root.path == crate::store_catalog::gha_cache_root(&layout).join("tenants")
        }));
        assert!(roots.iter().any(|root| {
            root.kind == CacheStore::Artifacts
                && root.path
                    == crate::store_catalog::StoreCatalog::for_work_root_with_layout(
                        &expected_work,
                        &layout,
                    )
                    .artifacts()
        }));
    }

    #[test]
    fn standalone_cache_default_work_root_is_config_local_for_both_layouts() {
        let prefix = std::env::temp_dir().join(format!(
            "velnor-cache-default-work-{}",
            uuid::Uuid::new_v4()
        ));

        let canonical_config = prefix.join("canonical/runner");
        let canonical_layout =
            crate::storage::StorageLayout::from_prefix(&prefix.join("canonical"));
        assert_eq!(canonical_layout.mode, "explicit");
        let canonical_work = super::work_root(Some(canonical_config.clone()), None).unwrap();
        assert_eq!(
            canonical_work,
            crate::container::daemon_shared_root(canonical_config.join("_work"))
        );

        let explicit_config = prefix.join("local-config");
        let explicit_layout = crate::storage::StorageLayout::explicit_local(&explicit_config);
        assert_eq!(explicit_layout.mode, "explicit-config");
        let explicit_work = super::work_root(Some(explicit_config.clone()), None).unwrap();
        assert_eq!(
            explicit_work,
            crate::container::daemon_shared_root(explicit_config.join("_work"))
        );
    }

    /// The compiler budget is one bound over mbx and sccache together: the
    /// oldest per-slot trees go first across both classes and every
    /// repository, a leased repository's trees are never victims, and zero
    /// is a real budget (every idle compiler store goes).
    #[test]
    fn cache_gc_enforces_the_compiler_budget_across_both_classes_oldest_first() {
        // entry(path, store, scope, age_days, bytes)
        let entries = vec![
            entry("/mbx/7/slots/slot-1", CacheStore::Mbx, &["7"], 20, 40),
            entry(
                "/mbx/7/targets/slots/slot-1",
                CacheStore::Mbx,
                &["7"],
                2,
                40,
            ),
            entry("/mbx/9/slots/slot-2", CacheStore::Mbx, &["9"], 10, 30),
            entry("/sccache/11", CacheStore::Sccache, &["11"], 15, 20),
            // Another class over its own size is not the compiler budget's
            // business.
            entry("/cargo/registry", CacheStore::Cargo, &["registry"], 1, 500),
        ];
        let mut policy = policy();
        policy.max_age = Duration::MAX;
        policy.compiler_budget_bytes = Some(60);
        let candidates = select_eviction_candidates(&entries, &policy);
        let paths: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.path.to_str().unwrap())
            .collect();
        // 130 held; evict oldest first until <= 60: slot-1 cache (40, 20d)
        // then sccache (20, 15d) -> 70, then repo 9 (30, 10d) -> 40.
        assert_eq!(
            paths,
            vec!["/mbx/7/slots/slot-1", "/mbx/9/slots/slot-2", "/sccache/11"]
        );
        assert!(candidates
            .iter()
            .all(|candidate| candidate.reason == "over-compiler-budget"));

        // A lease on repository 7 protects both its trees; the budget then
        // falls on the others.
        policy.in_use_scopes = BTreeSet::from(["mbx/7/job-holder".to_string()]);
        let candidates = select_eviction_candidates(&entries, &policy);
        let paths: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.path.to_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["/mbx/9/slots/slot-2", "/sccache/11"]);

        // Zero is a budget, not "unset".
        policy.in_use_scopes.clear();
        policy.compiler_budget_bytes = Some(0);
        let candidates = select_eviction_candidates(&entries, &policy);
        assert_eq!(candidates.len(), 4);
        assert!(candidates
            .iter()
            .all(|candidate| candidate.store.is_compiler()));

        // Unset leaves the compiler classes to the other rules.
        policy.compiler_budget_bytes = None;
        assert!(select_eviction_candidates(&entries, &policy).is_empty());
    }

    /// The mbx roots GC enumerates are the per-slot trees the layout
    /// produces, scoped by repository, and they are routinely GC-managed —
    /// the whole point of the migration's layout invariant.
    #[test]
    fn mbx_store_roots_are_per_slot_repository_scoped_and_gc_managed() {
        let prefix =
            std::env::temp_dir().join(format!("velnor-mbx-roots-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let scope = crate::trust_scope::FAIL_CLOSED;
        let catalog = StoreCatalog::for_work_root_with_layout(&work_root, &layout);
        let store = catalog.mbx(scope).join("1197700841");
        for path in [
            store.join("slots/slot-1/incremental/a"),
            store.join("slots/slot-2/incremental/b"),
            store.join("targets/slots/slot-1/debug/c"),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![9u8; 16]).unwrap();
        }
        let roots: Vec<StoreRoot> = store_roots(&work_root, &StoreScope::with_layout(&layout))
            .unwrap()
            .into_iter()
            .filter(|root| root.kind == CacheStore::Mbx)
            .collect();
        assert_eq!(roots.len(), 2, "{roots:?}");
        let trust_key = crate::trust_scope::filesystem_key(scope);
        for root in &roots {
            assert!(root.gc_managed, "mbx must be routinely GC-managed");
            assert!(root.emergency_managed);
            assert_eq!(
                root.scope_prefix,
                vec![trust_key.clone(), "1197700841".to_string()]
            );
        }
        let mut entries = Vec::new();
        for root in &roots {
            collect_candidates(root, &root.path, 0, &mut entries).unwrap();
        }
        let mut paths: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                store.join("slots/slot-1"),
                store.join("slots/slot-2"),
                store.join("targets/slots/slot-1"),
            ]
        );
        assert!(entries
            .iter()
            .all(|entry| entry.scope == vec![trust_key.clone(), "1197700841".to_string()]));
        fs::remove_dir_all(&prefix).ok();
    }

    #[test]
    fn stable_workspace_candidates_match_slot_lease_scope_and_are_pressure_only() {
        let prefix = std::env::temp_dir().join(format!(
            "velnor-stable-workspace-roots-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&prefix.join("storage"));
        let work_root = prefix.join("lib/velnor/work");
        let slot_work_dir = work_root.join("slot-7");
        let trust_scope = crate::trust_scope::FAIL_CLOSED;
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let stable_root = StoreCatalog::stable_workspace_root(&slot_work_dir);
        let candidate = stable_root
            .join(crate::trust_scope::filesystem_key(trust_scope))
            .join(&repository_key);
        fs::create_dir_all(candidate.join("workspace/target/debug")).unwrap();
        fs::write(
            candidate.join("workspace/target/debug/output"),
            b"warm workspace",
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_OWNER),
            crate::stable_workspace::STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_LAST_USE),
            b"",
        )
        .unwrap();

        let roots: Vec<_> = store_roots(&work_root, &StoreScope::with_layout(&layout))
            .unwrap()
            .into_iter()
            .filter(|store| store.kind == CacheStore::StableWorkspace && store.path == stable_root)
            .collect();
        assert_eq!(roots.len(), 1, "{roots:?}");
        let root = &roots[0];
        assert!(
            !root.gc_managed,
            "ordinary cache gc must preserve warm workspaces"
        );
        assert!(
            root.emergency_managed,
            "pressure reclaim must see workspaces"
        );
        assert_eq!(root.scope_depth, 2);
        assert_eq!(root.candidate_depth, 2);

        // The legacy path-recursive collector cannot prove workspace
        // ownership, so stable roots are only selected through secure pinned
        // inventory.
        let mut entries = Vec::new();
        collect_candidates(root, &root.path, 0, &mut entries).unwrap();
        assert!(entries.is_empty(), "{entries:?}");
        let anchor = crate::leftover_disk::filesystem_directory_identity(&stable_root).unwrap();
        let snapshots = crate::leftover_disk::filesystem_candidate_tree_snapshots_under(
            &stable_root,
            &stable_root,
            &anchor,
            root.candidate_depth,
        )
        .unwrap();
        let snapshot = snapshots
            .iter()
            .find(|snapshot| snapshot.path == candidate)
            .unwrap();
        let (scope, _) = crate::stable_workspace::validate_candidate_at(
            &stable_root,
            &candidate,
            root.scope_prefix.first().unwrap(),
            &snapshot.directory,
            &snapshot.identity,
        )
        .unwrap();
        assert_eq!(
            format!("stable-workspace/{}", scope.join("/")),
            format!(
                "stable-workspace/{}",
                crate::stable_workspace::lease_scope(&slot_work_dir, trust_scope, &repository_key)
                    .unwrap()
            )
        );

        let pressure_roots: Vec<_> = store_roots(&stable_root, &StoreScope::with_layout(&layout))
            .unwrap()
            .into_iter()
            .filter(|store| store.kind == CacheStore::StableWorkspace)
            .collect();
        assert_eq!(pressure_roots.len(), 1, "{pressure_roots:?}");
        assert_eq!(pressure_roots[0].path, stable_root);
        fs::remove_dir_all(prefix).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_never_removes_a_leased_stable_workspace() {
        let root = std::env::temp_dir().join(format!(
            "velnor-stable-workspace-pressure-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let slot_work_dir = work_root.join("slot-7");
        let trust_scope = crate::trust_scope::FAIL_CLOSED;
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let candidate = StoreCatalog::stable_workspace_root(&slot_work_dir)
            .join(crate::trust_scope::filesystem_key(trust_scope))
            .join(&repository_key);
        fs::create_dir_all(candidate.join("workspace/target/debug")).unwrap();
        fs::write(
            candidate.join("workspace/target/debug/output"),
            vec![4; 4096],
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_OWNER),
            crate::stable_workspace::STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_LAST_USE),
            b"",
        )
        .unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);

        let lease_scope =
            crate::stable_workspace::lease_scope(&slot_work_dir, trust_scope, &repository_key)
                .unwrap();
        let _lease = crate::capacity::ScopeLease::acquire(
            &layout.run_root,
            "stable-workspace",
            &format!("{lease_scope}/job-holder"),
            Duration::from_secs(24 * 3600),
        )
        .unwrap();
        let pressure_device = filesystem_device_id(&root).unwrap();
        let pressure_pin = crate::host_capacity::HostCapacityPin::open(&root).unwrap();
        let pressure_sample = |_: &Path| {
            Some(PressureSample {
                available_bytes: if candidate.exists() { 0 } else { 64 },
                used_percent: 0,
            })
        };

        let leased_report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::Amount(1),
            std::slice::from_ref(&work_root),
            &layout,
            None,
            pressure_device,
            &candidate_device_id,
            Some(&pressure_pin),
            &pressure_sample,
        );
        assert!(candidate.exists(), "active stable workspace was reclaimed");
        assert!(
            !leased_report.deleted.contains(&candidate),
            "active stable workspace appeared in deletion report: {leased_report:?}"
        );
        drop(_lease);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_deletes_only_an_idle_stable_workspace_candidate() {
        let root = std::env::temp_dir().join(format!(
            "velnor-stable-workspace-stale-pressure-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let slot_work_dir = work_root.join("slot-7");
        let trust_scope = crate::trust_scope::FAIL_CLOSED;
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let candidate = StoreCatalog::stable_workspace_root(&slot_work_dir)
            .join(crate::trust_scope::filesystem_key(trust_scope))
            .join(&repository_key);
        fs::create_dir_all(candidate.join("workspace/target/debug")).unwrap();
        fs::write(
            candidate.join("workspace/target/debug/output"),
            vec![4; 4096],
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_OWNER),
            crate::stable_workspace::STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_LAST_USE),
            b"interrupted clock write",
        )
        .unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);
        let future_checkout_file = candidate.join("workspace/target/debug/output");
        set_modified(
            &future_checkout_file,
            SystemTime::now() + Duration::from_secs(24 * 3600),
        );
        assert!(
            fs::metadata(&future_checkout_file)
                .unwrap()
                .modified()
                .unwrap()
                > SystemTime::now(),
            "fixture must model a future checkout mtime"
        );

        let pressure_device = filesystem_device_id(&root).unwrap();
        let pressure_pin = crate::host_capacity::HostCapacityPin::open(&root).unwrap();
        let pressure_sample = |_: &Path| {
            Some(PressureSample {
                available_bytes: if candidate.exists() { 0 } else { 64 },
                used_percent: 0,
            })
        };
        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::Amount(1),
            std::slice::from_ref(&work_root),
            &layout,
            None,
            pressure_device,
            &candidate_device_id,
            Some(&pressure_pin),
            &pressure_sample,
        );

        assert!(
            !candidate.exists(),
            "idle workspace candidate survived reclaim"
        );
        assert_eq!(report.deleted.as_slice(), std::slice::from_ref(&candidate));
        assert!(report.failures.is_empty(), "{report:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_ignores_depth_two_operator_directory_without_workspace() {
        let root = std::env::temp_dir().join(format!(
            "velnor-stable-workspace-operator-pressure-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let slot_work_dir = work_root.join("slot-7");
        let trust_scope = crate::trust_scope::FAIL_CLOSED;
        let trust_key = crate::trust_scope::filesystem_key(trust_scope);
        let stable_root = StoreCatalog::stable_workspace_root(&slot_work_dir);
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let candidate = stable_root.join(&trust_key).join(&repository_key);
        fs::create_dir_all(candidate.join("workspace/target/debug")).unwrap();
        fs::write(
            candidate.join("workspace/target/debug/output"),
            vec![4; 4096],
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_OWNER),
            crate::stable_workspace::STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(
            candidate.join(crate::stable_workspace::STABLE_SCOPE_LAST_USE),
            b"",
        )
        .unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);

        // Same depth and canonical key shape, with a valid marker and cold
        // payload, but no real workspace subtree: this remains operator data.
        let operator_repo =
            crate::store_catalog::repository_store_key("https://github.com", "43").unwrap();
        let operator_dir = stable_root.join(&trust_key).join(operator_repo);
        fs::create_dir_all(&operator_dir).unwrap();
        fs::write(
            operator_dir.join(crate::stable_workspace::STABLE_SCOPE_OWNER),
            crate::stable_workspace::STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(
            operator_dir.join(crate::stable_workspace::STABLE_SCOPE_LAST_USE),
            b"",
        )
        .unwrap();
        fs::write(operator_dir.join("operator-notes"), vec![8; 4096]).unwrap();
        backdate(&operator_dir, EMERGENCY_MIN_IDLE * 2);

        let pressure_device = filesystem_device_id(&root).unwrap();
        let pressure_pin = crate::host_capacity::HostCapacityPin::open(&root).unwrap();
        let pressure_sample = |_: &Path| {
            Some(PressureSample {
                available_bytes: if candidate.exists() { 0 } else { 64 },
                used_percent: 0,
            })
        };
        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::Amount(1),
            std::slice::from_ref(&work_root),
            &layout,
            None,
            pressure_device,
            &candidate_device_id,
            Some(&pressure_pin),
            &pressure_sample,
        );

        assert!(
            !candidate.exists(),
            "valid idle workspace was not reclaimed"
        );
        assert!(
            operator_dir.join("operator-notes").is_file(),
            "unowned depth-two operator directory was reclaimed"
        );
        assert_eq!(report.deleted.as_slice(), std::slice::from_ref(&candidate));
        assert!(report.failures.is_empty(), "{report:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stable_workspace_prepare_cannot_claim_operator_scope_for_forced_pressure_reclaim() {
        let root = std::env::temp_dir().join(format!(
            "velnor-stable-workspace-prepare-pressure-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let slot_work_dir = work_root.join("slot-7");
        let trust_scope = crate::trust_scope::FAIL_CLOSED;
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let candidate = StoreCatalog::stable_workspace_root(&slot_work_dir)
            .join(crate::trust_scope::filesystem_key(trust_scope))
            .join(&repository_key);
        fs::create_dir_all(candidate.join("workspace/target/debug")).unwrap();
        let operator_file = candidate.join("workspace/target/debug/operator-data");
        fs::write(&operator_file, vec![4; 4096]).unwrap();

        let error = crate::stable_workspace::prepare(
            &slot_work_dir,
            &layout.run_root,
            trust_scope,
            &repository_key,
            "job-operator-scope",
        )
        .expect_err("prepare must not adopt an operator-created scope");
        assert!(error
            .to_string()
            .contains("refusing to adopt existing unowned"));
        assert!(
            !candidate
                .join(crate::stable_workspace::STABLE_SCOPE_OWNER)
                .exists(),
            "failed prepare wrote an ownership marker"
        );
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);

        let pressure_device = filesystem_device_id(&root).unwrap();
        let pressure_pin = crate::host_capacity::HostCapacityPin::open(&root).unwrap();
        let pressure_sample = |_: &Path| {
            Some(PressureSample {
                available_bytes: if candidate.exists() { 0 } else { 64 },
                used_percent: 0,
            })
        };
        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::Amount(1),
            std::slice::from_ref(&work_root),
            &layout,
            None,
            pressure_device,
            &candidate_device_id,
            Some(&pressure_pin),
            &pressure_sample,
        );

        assert!(
            operator_file.is_file(),
            "operator workspace data was reclaimed"
        );
        assert!(candidate.is_dir(), "operator scope was reclaimed");
        assert!(
            !report.deleted.contains(&candidate),
            "unowned operator scope appeared in deletion report: {report:?}"
        );
        assert!(report.failures.is_empty(), "{report:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn emergency_unpinned_collection_never_selects_stable_workspaces() {
        let root = std::env::temp_dir().join(format!(
            "velnor-stable-workspace-unpinned-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let slot_work_dir = work_root.join("slot-7");
        let trust_scope = crate::trust_scope::FAIL_CLOSED;
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let stable_root = StoreCatalog::stable_workspace_root(&slot_work_dir);
        let owned = stable_root
            .join(crate::trust_scope::filesystem_key(trust_scope))
            .join(&repository_key);
        fs::create_dir_all(owned.join("workspace/target/debug")).unwrap();
        fs::write(owned.join("workspace/target/debug/output"), vec![4; 4096]).unwrap();
        fs::write(
            owned.join(crate::stable_workspace::STABLE_SCOPE_OWNER),
            crate::stable_workspace::STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(
            owned.join(crate::stable_workspace::STABLE_SCOPE_LAST_USE),
            b"",
        )
        .unwrap();
        backdate(&owned, EMERGENCY_MIN_IDLE * 2);

        let unowned = stable_root
            .join(crate::trust_scope::filesystem_key(trust_scope))
            .join(crate::store_catalog::repository_store_key("https://github.com", "43").unwrap());
        fs::create_dir_all(unowned.join("workspace")).unwrap();
        let operator_file = unowned.join("workspace/operator-notes");
        fs::write(&operator_file, vec![8; 4096]).unwrap();
        backdate(&unowned, EMERGENCY_MIN_IDLE * 2);

        let entries = cache_listing(&work_root, true, &StoreScope::with_layout(&layout)).unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| entry.store != CacheStore::StableWorkspace),
            "unpinned emergency collection selected stable scopes: {entries:?}"
        );
        let report = reclaim_work_root_with_layout(
            &work_root,
            &layout.run_root,
            &layout.log_root,
            u64::MAX,
            &BTreeSet::new(),
            true,
            &layout,
            None,
        )
        .unwrap();

        assert!(
            owned.is_dir(),
            "unpinned emergency reclaim deleted a stable scope"
        );
        assert!(
            operator_file.is_file(),
            "unpinned emergency reclaim deleted operator data"
        );
        assert!(
            report
                .deleted
                .iter()
                .all(|path| !path.starts_with(&stable_root)),
            "stable scope appeared in unpinned deletion report: {report:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_does_not_follow_exact_stable_root_symlink() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "velnor-stable-workspace-root-symlink-pressure-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let slot_work_dir = root.join("work/slot-7");
        fs::create_dir_all(&slot_work_dir).unwrap();
        let stable_root = StoreCatalog::stable_workspace_root(&slot_work_dir);
        let outside = root.join("outside-stable-data");
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let trust_key = crate::trust_scope::filesystem_key(crate::trust_scope::FAIL_CLOSED);
        let outside_scope = outside.join(&trust_key).join(repository_key);
        fs::create_dir_all(outside_scope.join("workspace/target/debug")).unwrap();
        let outside_sentinel = outside_scope.join("workspace/target/debug/sentinel");
        fs::write(&outside_sentinel, vec![7; 4096]).unwrap();
        fs::write(
            outside_scope.join(crate::stable_workspace::STABLE_SCOPE_OWNER),
            crate::stable_workspace::STABLE_SCOPE_OWNER_MARKER,
        )
        .unwrap();
        fs::write(
            outside_scope.join(crate::stable_workspace::STABLE_SCOPE_LAST_USE),
            b"",
        )
        .unwrap();
        backdate(&outside_scope, EMERGENCY_MIN_IDLE * 2);
        symlink(&outside, &stable_root).unwrap();

        let roots = store_roots(&stable_root, &StoreScope::with_layout(&layout)).unwrap();
        let stable_store = roots
            .iter()
            .find(|store| store.kind == CacheStore::StableWorkspace)
            .unwrap();
        let stable_candidate = stable_root
            .join(&trust_key)
            .join(crate::store_catalog::repository_store_key("https://github.com", "99").unwrap());
        assert_eq!(
            trusted_catalog_anchor(&stable_root, &layout, &roots, &stable_candidate),
            Some(slot_work_dir.clone()),
            "exact-root stable-workspace inventory must anchor above the final component"
        );
        assert_eq!(stable_store.path, stable_root);

        let pressure_device = filesystem_device_id(&root).unwrap();
        let pressure_pin = crate::host_capacity::HostCapacityPin::open(&root).unwrap();
        let pressure_sample = |_: &Path| {
            Some(PressureSample {
                available_bytes: 0,
                used_percent: 99,
            })
        };
        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::Amount(1),
            std::slice::from_ref(&stable_root),
            &layout,
            None,
            pressure_device,
            &candidate_device_id,
            Some(&pressure_pin),
            &pressure_sample,
        );

        assert!(outside_sentinel.is_file(), "outside sentinel was reclaimed");
        assert!(
            report.deleted.is_empty(),
            "symlink target was reported deleted: {report:?}"
        );
        assert!(fs::symlink_metadata(&stable_root)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&stable_root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_mirror_candidates_match_trust_partitioned_repository_leases() {
        let prefix =
            std::env::temp_dir().join(format!("velnor-git-mirror-roots-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&prefix.join("storage"));
        let work_root = prefix.join("lib/velnor/work");
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "42").unwrap();
        let trust_scopes = [
            crate::trust_scope::TRUSTED,
            crate::trust_scope::FAIL_CLOSED,
            crate::trust_scope::PR_STORE_SCOPE,
        ];
        for trust_scope in trust_scopes {
            let repository = crate::store_catalog::StoreCatalog::git_mirror_repository_root(
                &layout,
                trust_scope,
                &repository_key,
            );
            fs::create_dir_all(repository.join("objects/pack")).unwrap();
            fs::write(repository.join("objects/pack/pack"), b"mirror bytes").unwrap();
        }

        let scope = StoreScope {
            layout: Some(layout.clone()),
            pool_trust_scope: crate::trust_scope::TRUSTED.to_owned(),
            daemon_environment: None,
        };
        let stores: Vec<_> = store_roots(&work_root, &scope)
            .unwrap()
            .into_iter()
            .filter(|store| store.kind == CacheStore::GitMirrors)
            .collect();
        assert_eq!(stores.len(), trust_scopes.len(), "{stores:?}");

        let mut entries = Vec::new();
        for store in &stores {
            assert!(store.gc_managed && store.emergency_managed);
            assert_eq!(store.scope_depth, 1);
            assert_eq!(store.candidate_depth, 1);
            collect_candidates(store, &store.path, 0, &mut entries).unwrap();
        }
        assert_eq!(entries.len(), trust_scopes.len(), "{entries:?}");
        for trust_scope in trust_scopes {
            let trust_key = crate::trust_scope::filesystem_key(trust_scope);
            assert!(entries.iter().any(|entry| {
                entry.path
                    == crate::store_catalog::StoreCatalog::git_mirror_repository_root(
                        &layout,
                        trust_scope,
                        &repository_key,
                    )
                    && entry.scope_key() == format!("{trust_key}/{repository_key}")
            }));
        }

        // The active trusted repository lease protects that one candidate.
        // Equal repository IDs in the fail-closed/PR roots remain reclaimable.
        let mut policy = policy();
        policy.class_budgets.insert(CacheStore::GitMirrors, 1);
        let trusted_key = crate::trust_scope::filesystem_key(crate::trust_scope::TRUSTED);
        policy.in_use_scopes.insert(format!(
            "git-mirrors/{trusted_key}/{repository_key}/{}",
            crate::trust_scope::filesystem_key("active-job")
        ));
        let victims = select_eviction_candidates(&entries, &policy);
        assert_eq!(victims.len(), 2, "{victims:?}");
        assert!(victims
            .iter()
            .all(|candidate| candidate.scope != vec![trusted_key.clone(), repository_key.clone()]));
        fs::remove_dir_all(prefix).unwrap();
    }

    /// The daemon's enforcement entry point: over budget, the oldest idle
    /// slot trees are deleted and logged; a leased repository survives.
    #[test]
    fn enforce_compiler_store_budget_deletes_oldest_idle_slot_trees() {
        let prefix =
            std::env::temp_dir().join(format!("velnor-mbx-budget-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&prefix);
        let work_root = prefix.join("lib/velnor/work");
        let scope = crate::trust_scope::FAIL_CLOSED;
        let catalog = StoreCatalog::for_work_root_with_layout(&work_root, &layout);
        let old = catalog.mbx(scope).join("1/slots/slot-1");
        let leased = catalog.mbx(scope).join("2/slots/slot-1");
        let new = catalog.mbx(scope).join("3/targets/slots/slot-1");
        for (dir, age_days) in [(&old, 30u32), (&leased, 20), (&new, 1)] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("blob"), vec![0u8; 100]).unwrap();
            backdate(dir, DAY * age_days);
        }
        fs::create_dir_all(&layout.run_root).unwrap();
        let _lease = crate::capacity::ScopeLease::acquire(
            &layout.run_root,
            "mbx",
            "2/job-holder",
            Duration::from_secs(600),
        )
        .unwrap();

        // 300 held (the leased store counts, it just cannot be a victim);
        // the oldest idle tree brings it to 200, under a 250 budget.
        let report = enforce_compiler_store_budget(&work_root, &layout, 250).unwrap();

        assert_eq!(report.deleted, vec![old.clone()], "{report:?}");
        assert!(report.failures.is_empty());
        assert!(!old.exists());
        assert!(leased.exists(), "a leased repository is never a victim");
        assert!(new.exists(), "eviction stops once under budget");
        let history = fs::read_to_string(layout.log_root.join("gc-history.jsonl")).unwrap();
        assert!(history.contains("over-compiler-budget"), "{history}");
        fs::remove_dir_all(&prefix).ok();
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
        let trust_key = crate::trust_scope::filesystem_key("trusted");
        let entries = vec![
            entry(
                "/cache/active",
                CacheStore::ActionsCache,
                &[trust_key.as_str(), "active"],
                90,
                100,
            ),
            entry(
                "/cache/idle",
                CacheStore::ActionsCache,
                &[trust_key.as_str(), "idle"],
                90,
                100,
            ),
        ];
        let mut policy = policy();
        policy
            .in_use_scopes
            .insert(format!("actions-cache/{trust_key}/active"));

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
        // Contention is typed at the flock boundary: the reclaim caller
        // backs off on this type, never on error text.
        let contention = GcLeaderLock::acquire(&root).unwrap_err();
        assert!(
            contention.downcast_ref::<GcLeaderLockHeld>().is_some(),
            "expected typed contention, got {contention:#}"
        );
        drop(first);
        assert!(GcLeaderLock::acquire(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exclusive_timeout_fails_loud_on_a_held_lock() {
        let root = std::env::temp_dir().join(format!(
            "velnor-entry-lock-timeout-{}",
            uuid::Uuid::new_v4()
        ));
        let entry = root.join("scope/entry.json");
        fs::create_dir_all(entry.parent().unwrap()).unwrap();
        fs::write(&entry, b"{}").unwrap();
        let _held = CacheEntryLock::exclusive(&entry).unwrap();
        let start = std::time::Instant::now();
        let error =
            CacheEntryLock::exclusive_timeout(&entry, Duration::from_millis(50)).unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "wait was not bounded"
        );
        assert!(
            format!("{error:#}").contains("within 50ms"),
            "unexpected: {error:#}"
        );
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
        let pinned = pin_cache_candidate(&root, &entry);
        let expected_device = pinned.anchor_identity.device;
        let (sender, receiver) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            sender
                .send(remove_candidate(
                    &candidate,
                    &pinned,
                    expected_device,
                    &|_| Ok(()),
                ))
                .unwrap()
        });

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
    fn artifacts_gc_waits_for_active_restore_lock() {
        let root =
            std::env::temp_dir().join(format!("velnor-artifact-lock-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let run_bucket =
            crate::store_catalog::StoreCatalog::for_work_root_with_layout(&root, &layout)
                .artifacts_run("run-1");
        fs::create_dir_all(&run_bucket).unwrap();
        fs::write(run_bucket.join("artifact"), b"artifact").unwrap();
        let restore_lock = CacheEntryLock::shared(&run_bucket).unwrap();
        let candidate = EvictionCandidate {
            path: run_bucket.clone(),
            store: CacheStore::Artifacts,
            scope: vec!["run-1".into()],
            bytes: 8,
            reason: "test".into(),
        };
        let pinned = pin_cache_candidate(&root, &run_bucket);
        let expected_device = pinned.anchor_identity.device;
        let (sender, receiver) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            sender
                .send(remove_candidate(
                    &candidate,
                    &pinned,
                    expected_device,
                    &|_| Ok(()),
                ))
                .unwrap()
        });

        assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(run_bucket.join("artifact").is_file());
        drop(restore_lock);
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        remover.join().unwrap();
        assert!(!run_bucket.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn target_gc_rechecks_current_under_publisher_lock() {
        let temp_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize temporary test root");
        let root = temp_root.join(format!("velnor-target-race-{}", uuid::Uuid::new_v4()));
        let scope = root
            .join("targets")
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join("workspace/repo/workflow/job");
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
        let pinned = pin_cache_candidate(&root, &generation);
        let expected_device = pinned.anchor_identity.device;
        // The publisher and GC both lock the job bucket. Hold the publisher
        // side while GC has already selected the generation, then publish the
        // pointer. GC must re-read it after acquiring the same lock.
        let publisher_lock = CacheEntryLock::exclusive(&scope).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            sender
                .send(remove_candidate(
                    &candidate,
                    &pinned,
                    expected_device,
                    &|_| Ok(()),
                ))
                .unwrap()
        });
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

    #[cfg(unix)]
    #[test]
    fn cache_gc_rejects_swapped_ancestor_symlink_without_deleting_outside_data() {
        use std::os::unix::fs::symlink;

        let temp_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize temporary test root");
        let root = temp_root.join(format!("velnor-cache-gc-symlink-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let candidate = StoreCatalog::for_work_root_with_layout(&work_root, &layout)
            .actions_cache(crate::trust_scope::FAIL_CLOSED)
            .join("repo-key/cache-key");
        fs::create_dir_all(&candidate).unwrap();
        fs::write(candidate.join("payload"), b"cache data").unwrap();

        let outside = root.join("outside");
        fs::create_dir_all(outside.join("cache-key")).unwrap();
        fs::write(outside.join("cache-key/secret"), b"outside data").unwrap();

        let scope = StoreScope::with_layout(&layout);
        let inventory = pinned_cache_inventory(&work_root, false, &scope, false).unwrap();
        let key = (CacheStore::ActionsCache, candidate.clone());
        assert!(inventory.candidates.contains_key(&key));
        let pinned = inventory.candidates.get(&key).unwrap();
        let expected_device = pinned.anchor_identity.device;
        let eviction = EvictionCandidate {
            path: candidate.clone(),
            store: CacheStore::ActionsCache,
            scope: vec!["repository".into()],
            bytes: 10,
            reason: "test".into(),
        };

        let candidate_parent = candidate.parent().unwrap();
        let saved_parent = root.join("saved-repository");
        fs::rename(candidate_parent, &saved_parent).unwrap();
        symlink(&outside, candidate_parent).unwrap();

        let error = remove_candidate(&eviction, pinned, expected_device, &|_| Ok(())).unwrap_err();
        assert!(
            format!("{error:#}").contains("symlink")
                || format!("{error:#}").contains("without following links"),
            "unexpected refusal: {error:#}"
        );
        assert_eq!(
            fs::read(outside.join("cache-key/secret")).unwrap(),
            b"outside data"
        );
        assert!(saved_parent.join("cache-key/payload").is_file());
        assert!(
            !outside.join(".velnor-locks").exists(),
            "cache GC must not create a lock file through the swapped ancestor"
        );
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
        assert!(run_gc(
            Path::new("/does-not-matter"),
            args,
            BTreeMap::new(),
            &StoreScope::current(),
        )
        .unwrap_err()
        .to_string()
        .contains("requires --yes"));
    }

    /// `velnorctl cache gc` end to end against a temp runtime root: the
    /// destructive pass holds the coordinator, then reclaims leftover
    /// workspaces under that same hold. Before the fix the reclaim re-locked
    /// the coordinator on a second descriptor and the process hung on itself
    /// (with the re-entry guard it would now surface as an error instead).
    /// Either way the leftover workspace would survive; here it must go.
    #[test]
    fn destructive_gc_reclaims_leftovers_without_conflicting_with_itself() {
        let prefix = std::env::temp_dir().join(format!("velnor-gc-e2e-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&prefix);
        let work = layout.lib_root.join("work");
        let orphan_id = "11111111-2222-3333-4444-555555555555";
        let orphan = work.join("slot-1").join(orphan_id);
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("marker"), b"job").unwrap();
        test_clock::backdate(&orphan, crate::leftover_disk::WORKSPACE_MIN_IDLE * 2);
        // A cache entry old enough to evict, so the eviction pass has work too.
        let stale_cache =
            crate::store_catalog::StoreCatalog::for_work_root_with_layout(&work, &layout)
                .actions_cache("trusted")
                .join("stale/key");
        fs::create_dir_all(&stale_cache).unwrap();
        fs::write(stale_cache.join("data"), vec![0; 16]).unwrap();
        test_clock::backdate(&stale_cache, DAY * 40);

        let args = CacheGcArgs {
            dry_run: false,
            yes: true,
            force_no_lease_check: false,
            keep_newest_targets: 3,
            max_age_days: 30,
            max_size_bytes: None,
        };
        let reclaim_ran = std::cell::Cell::new(false);
        let run_root = layout.run_root.clone();
        let scope = StoreScope {
            layout: Some(layout.clone()),
            pool_trust_scope: crate::trust_scope::TRUSTED.to_owned(),
            daemon_environment: None,
        };
        run_gc_with(
            &work,
            args,
            BTreeMap::new(),
            &scope,
            |coordinator, reclaim_root| {
                reclaim_ran.set(true);
                assert_eq!(reclaim_root, run_root.as_path());
                // The production reclaim under the held coordinator, with the
                // host Docker socket replaced by a fake.
                crate::leftover_disk::reclaim_leftover_under_coordinator(
                    coordinator,
                    reclaim_root,
                    std::slice::from_ref(&work),
                    &BTreeSet::new(),
                    |_| Ok(String::new()),
                    |path| {
                        fs::remove_dir_all(path)?;
                        Ok(())
                    },
                    false,
                )
            },
        )
        .unwrap();

        assert!(reclaim_ran.get(), "gc must run the leftover reclaim");
        assert!(!orphan.exists(), "leftover workspace must be reclaimed");
        assert!(!stale_cache.exists(), "stale cache entry must be evicted");
        // Both locks are released once gc returns: another gc can start. The
        // coordinator re-entry guard would refuse immediately if this thread
        // still held it; the blocking flock then only waits out a sibling
        // test's fork window (a child briefly inherits every open fd).
        drop(crate::capacity::FilesystemCoordinator::lock_exclusive(&layout.run_root).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match GcLeaderLock::acquire(&layout.run_root) {
                Ok(leader) => break drop(leader),
                Err(error) if std::time::Instant::now() < deadline => {
                    assert!(
                        error.downcast_ref::<GcLeaderLockHeld>().is_some(),
                        "{error:#}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("gc leader lock still held after gc returned: {error:#}"),
            }
        }
        fs::remove_dir_all(prefix).unwrap();
    }

    #[test]
    fn reclaim_stops_at_target_and_skips_in_use_scope() {
        let _serial = crate::trust_scope::test_support::serialized();
        let _scope = crate::trust_scope::resolve("trusted");
        let root = std::env::temp_dir().join(format!("velnor-reclaim-{}", uuid::Uuid::new_v4()));
        let work = root.join("work");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let trust_key = crate::trust_scope::filesystem_key("trusted");
        let actions_cache =
            StoreCatalog::for_work_root_with_layout(&work, &layout).actions_cache("trusted");
        let active = actions_cache.join("active/key");
        let first = actions_cache.join("first/key");
        let second = actions_cache.join("second/key");
        for path in [&active, &first, &second] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("data"), vec![0; 16]).unwrap();
        }
        let zero_target_report = reclaim(&layout, &work, 0, &BTreeSet::new()).unwrap();
        assert!(zero_target_report.deleted.is_empty());
        assert!(first.exists() && second.exists());

        let report = reclaim_work_root_with_layout(
            &work,
            &root.join("run"),
            &root.join("log"),
            16,
            &BTreeSet::from([format!("actions-cache/{trust_key}/active")]),
            false,
            &layout,
            None,
        )
        .unwrap();
        assert_eq!(report.deleted.len(), 1);
        assert!(active.exists());
        assert_eq!(first.exists() as u8 + second.exists() as u8, 1);
        assert!(root.join("log/gc-history.jsonl").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn capacity_floor_reclaims_work_root_artifacts_not_cache_root_decoy() {
        let root = std::env::temp_dir().join(format!(
            "velnor-capacity-floor-artifacts-root-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let work_catalog =
            crate::store_catalog::StoreCatalog::for_work_root_with_layout(&work_root, &layout);
        let cache_root_catalog = crate::store_catalog::StoreCatalog::for_work_root_with_layout(
            &layout.cache_root,
            &layout,
        );
        let actual = work_catalog.artifacts().join("actual");
        let decoy = cache_root_catalog.artifacts().join("decoy");
        for candidate in [&actual, &decoy] {
            fs::create_dir_all(candidate).unwrap();
            fs::write(candidate.join("payload"), vec![0; 8 * 1024 * 1024]).unwrap();
            backdate(candidate, EMERGENCY_MIN_IDLE * 2);
        }
        assert_eq!(work_catalog.artifacts(), actual.parent().unwrap());
        // Existence-driven pressure, not a live-statfs floor: a one-byte
        // absolute margin on a live volume races background filesystem
        // activity. Removal quarantines the candidate before unlinking, so
        // the first gone sample only proves a rename; the pass clears on
        // the second consecutive gone sample, once our own unlinks have
        // covered the deficit and the remainder must be finished.
        let gone_streak = std::cell::Cell::new(0u32);
        let pressure_sample = |_: &Path| {
            if actual.exists() {
                gone_streak.set(0);
            } else {
                gone_streak.set(gone_streak.get().saturating_add(1));
            }
            Some(if gone_streak.get() >= 2 {
                PressureSample {
                    available_bytes: 100,
                    used_percent: 0,
                }
            } else {
                PressureSample {
                    available_bytes: 10,
                    used_percent: 0,
                }
            })
        };
        let pin = crate::host_capacity::HostCapacityPin::open(&work_root).unwrap();

        let report = reclaim_for_disk_pressure_on_device(
            &work_root,
            ReclaimGoal::AvailableFloor(100),
            &[work_root.clone()],
            &layout,
            None,
            pin.device_id(),
            &candidate_device_id,
            None,
            &pressure_sample,
        );

        assert!(
            !actual.exists(),
            "actual work-root artifact candidate survived: {report:?}"
        );
        assert!(
            decoy.exists(),
            "cache-root decoy was treated as work-root artifacts"
        );
        assert!(
            report.freed_bytes > 0,
            "no capacity was reclaimed: {report:?}"
        );
        assert!(report.failures.is_empty(), "{report:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn capacity_pressure_reclaims_work_root_artifacts_not_cache_root_decoy() {
        let root = std::env::temp_dir().join(format!(
            "velnor-capacity-pressure-artifacts-root-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work_root = root.join("work");
        let work_catalog =
            crate::store_catalog::StoreCatalog::for_work_root_with_layout(&work_root, &layout);
        let cache_root_catalog = crate::store_catalog::StoreCatalog::for_work_root_with_layout(
            &layout.cache_root,
            &layout,
        );
        let actual = work_catalog.artifacts().join("actual");
        let decoy = cache_root_catalog.artifacts().join("decoy");
        for candidate in [&actual, &decoy] {
            fs::create_dir_all(candidate).unwrap();
            fs::write(candidate.join("payload"), vec![0; 8 * 1024 * 1024]).unwrap();
            backdate(candidate, EMERGENCY_MIN_IDLE * 2);
        }
        assert_eq!(work_catalog.artifacts(), actual.parent().unwrap());
        // Existence-driven pressure, not a live-statfs amount goal: a
        // one-byte target on a live volume races background filesystem
        // activity. Removal quarantines the candidate before unlinking, so
        // the first gone sample only proves a rename; the pass clears on
        // the second consecutive gone sample, once our own unlinks have
        // covered the deficit and the remainder must be finished.
        let gone_streak = std::cell::Cell::new(0u32);
        let pressure_sample = |_: &Path| {
            if actual.exists() {
                gone_streak.set(0);
            } else {
                gone_streak.set(gone_streak.get().saturating_add(1));
            }
            Some(if gone_streak.get() >= 2 {
                PressureSample {
                    available_bytes: 100,
                    used_percent: 0,
                }
            } else {
                PressureSample {
                    available_bytes: 10,
                    used_percent: 0,
                }
            })
        };
        let pin = crate::host_capacity::HostCapacityPin::open(&work_root).unwrap();

        let report = reclaim_for_disk_pressure_on_device(
            &work_root,
            ReclaimGoal::Amount(1),
            &[work_root.clone()],
            &layout,
            None,
            pin.device_id(),
            &candidate_device_id,
            None,
            &pressure_sample,
        );

        assert!(
            !actual.exists(),
            "actual work-root artifact candidate survived: {report:?}"
        );
        assert!(
            decoy.exists(),
            "cache-root decoy was treated as work-root artifacts"
        );
        assert!(
            report.freed_bytes > 0,
            "no capacity was reclaimed: {report:?}"
        );
        assert!(report.failures.is_empty(), "{report:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gc_sweeps_both_pool_and_fork_namespaces_on_a_trusted_pool() {
        // Fork and unknown jobs run under the untrusted floor on every pool,
        // so a trusted pool accumulates stores in two canonical namespaces.
        // Both must be visible to the collector; without the second root the
        // fork-job stores would grow unbounded.
        let _serial = crate::trust_scope::test_support::serialized();
        assert_eq!(crate::trust_scope::resolve("trusted").as_str(), "trusted");

        let root = std::env::temp_dir().join(format!(
            "velnor-gc-trust-namespaces-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("lib/velnor-test/work");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let trusted = layout.cache_class("trusted", "caches").join("repo/key");
        let untrusted = layout.cache_class("untrusted", "caches").join("repo/key");
        for path in [&trusted, &untrusted] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("data"), vec![0; 16]).unwrap();
        }

        let report = reclaim_work_root_with_layout(
            &work,
            &root.join("run"),
            &root.join("log"),
            32,
            &BTreeSet::new(),
            false,
            &layout,
            None,
        )
        .unwrap();

        assert!(!trusted.exists(), "pool namespace was not swept");
        assert!(!untrusted.exists(), "fork-job namespace was not swept");
        assert!(report.failures.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gc_keeps_colliding_custom_trust_namespaces_separate() {
        let _serial = crate::trust_scope::test_support::serialized();
        let pool_scope = "pool/a";
        assert_eq!(crate::trust_scope::resolve(pool_scope).as_str(), pool_scope);

        let root = std::env::temp_dir().join(format!(
            "velnor-gc-trust-collision-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("lib/velnor-test/work");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let pool = layout.cache_class(pool_scope, "caches").join("repo/key");
        let colliding = layout.cache_class("pool_a", "caches").join("repo/key");
        let floor = layout
            .cache_class(crate::trust_scope::FAIL_CLOSED, "caches")
            .join("repo/key");
        for path in [&pool, &colliding, &floor] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("data"), vec![0; 16]).unwrap();
        }

        let report = reclaim_work_root_with_layout(
            &work,
            &root.join("run"),
            &root.join("log"),
            32,
            &BTreeSet::new(),
            false,
            &layout,
            None,
        )
        .unwrap();

        assert!(!pool.exists(), "selected pool namespace was not swept");
        assert!(!floor.exists(), "fork-job namespace was not swept");
        assert!(
            colliding.exists(),
            "a scope whose old lossy spelling collided was swept"
        );
        assert!(report.failures.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_pressure_reclaimer_reclaims_discovered_work_root() {
        let root = std::env::temp_dir().join(format!(
            "velnor-disk-pressure-reclaim-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("lib/velnor-test/work");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let cache = layout.cache_class("untrusted", "caches").join("idle/key");
        let compiler_cache = sccache_root(&work, "untrusted", &layout).join("idle/key");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::create_dir_all(&compiler_cache).unwrap();
        fs::write(cache.join("payload"), vec![0; 16]).unwrap();
        fs::write(compiler_cache.join("payload"), vec![0; 16]).unwrap();
        // Emergency reclaim refuses to delete a store a live job may be
        // writing. Model genuinely cold stores.
        backdate(&cache, EMERGENCY_MIN_IDLE * 2);
        backdate(compiler_cache.parent().unwrap(), EMERGENCY_MIN_IDLE * 2);

        let work_roots = crate::leftover_disk::discover_daemon_work_roots_in(&root.join("lib"));
        assert_eq!(work_roots, vec![work.clone()]);
        // Inject a different device for candidates while preserving the real
        // pressure device used by the descriptor-bound deleter.
        let pressure_device = filesystem_device_id(&root).unwrap();
        let off_device =
            |_path: &Path, _captured_device: u64| Some(pressure_device.saturating_add(1));
        let no_measurement = |_path: &Path| {
            Some(PressureSample {
                available_bytes: 0,
                used_percent: 0,
            })
        };
        let skipped = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::AvailableFloor(16),
            &work_roots,
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            pressure_device,
            &off_device,
            None,
            &no_measurement,
        );
        assert_eq!(skipped.freed_bytes, 0);
        assert!(skipped.deleted.is_empty());
        assert!(cache.exists());
        assert!(compiler_cache.exists());

        // The cache candidate is modeled off-device; the compiler candidate
        // stays on the pressured device.
        let off_device_cache = cache.clone();
        let device_of = |path: &Path, _captured_device: u64| {
            if path.starts_with(&off_device_cache) {
                Some(pressure_device.saturating_add(1))
            } else {
                Some(pressure_device)
            }
        };
        let measure = |_: &Path| {
            Some(PressureSample {
                available_bytes: 10_000
                    + if compiler_cache.parent().unwrap().exists() {
                        0
                    } else {
                        4096
                    },
                used_percent: 0,
            })
        };
        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::AvailableFloor(10_016),
            &work_roots,
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            pressure_device,
            &device_of,
            None,
            &measure,
        );

        assert_eq!(report.freed_bytes, 4096);
        assert_eq!(
            report.deleted,
            vec![compiler_cache.parent().unwrap().to_path_buf()]
        );
        assert!(cache.exists(), "off-device cache candidate was deleted");
        assert!(!compiler_cache.parent().unwrap().exists());
        assert!(report.failures.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_pressure_goal_keeps_reclaiming_until_both_thresholds_clear() {
        let goal = ReclaimGoal::DiskPressure {
            minimum_available_bytes: 200,
            hard_pressure_percent: 90,
        };
        assert!(goal.still_pressured(
            PressureSample {
                available_bytes: 500,
                used_percent: 90,
            },
            0,
        ));
        assert!(!goal.still_pressured(
            PressureSample {
                available_bytes: 500,
                used_percent: 89,
            },
            0,
        ));
        assert!(goal.still_pressured(
            PressureSample {
                available_bytes: 199,
                used_percent: 89,
            },
            0,
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hard_utilization_reclaims_cache_even_when_free_space_meets_floor() {
        let root = std::env::temp_dir().join(format!(
            "velnor-hard-usage-pressure-reclaim-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("lib/velnor-test/work");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let candidate = sccache_root(&work, "untrusted", &layout).join("idle");
        fs::create_dir_all(candidate.join("key")).unwrap();
        fs::write(candidate.join("key/payload"), vec![0; 64]).unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);
        let pressure_device = filesystem_device_id(&root).unwrap_or_else(|| {
            fs::create_dir_all(&root).unwrap();
            filesystem_device_id(&root).unwrap()
        });
        let pressure_sample = |_: &Path| {
            Some(PressureSample {
                // Free space already exceeds the 2 GiB floor. Only utilization
                // keeps this pass active.
                available_bytes: 4 * 1024 * 1024 * 1024,
                used_percent: if candidate.exists() { 95 } else { 89 },
            })
        };

        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::DiskPressure {
                minimum_available_bytes: 2 * 1024 * 1024 * 1024,
                hard_pressure_percent: 90,
            },
            std::slice::from_ref(&work),
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            pressure_device,
            &candidate_device_id,
            None,
            &pressure_sample,
        );

        assert_eq!(report.deleted, vec![candidate.clone()]);
        assert!(
            !candidate.exists(),
            "high-usage pass skipped the cache candidate"
        );
        assert!(report.failures.is_empty(), "{report:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_skips_eviction_when_locked_baseline_meets_floor() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-floor-baseline-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let work = root.join("work");
        let candidate = sccache_root(&work, "untrusted", &layout).join("idle");
        fs::create_dir_all(candidate.join("key")).unwrap();
        fs::write(candidate.join("key/payload"), vec![0; 32]).unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);
        let pressure_device = filesystem_device_id(&root).unwrap();
        let measurements = std::cell::Cell::new(0_usize);
        let available = |_: &Path| {
            let sample = measurements.get();
            measurements.set(sample + 1);
            // The outer caller sees pressure twice. The third sample is the
            // fresh pinned baseline inside the coordinator-locked reclaim.
            // Model capacity recovering while this pass waited for the lock.
            if sample == 2 {
                let reentry =
                    crate::capacity::FilesystemCoordinator::lock_exclusive(&layout.run_root)
                        .unwrap_err();
                assert!(
                    reentry
                        .to_string()
                        .contains("already held exclusively by this thread"),
                    "fresh baseline was not measured under the coordinator: {reentry:#}"
                );
            }
            Some(PressureSample {
                available_bytes: if sample < 2 { 100 } else { 500 },
                used_percent: 0,
            })
        };

        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::AvailableFloor(200),
            std::slice::from_ref(&work),
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            pressure_device,
            &candidate_device_id,
            None,
            &available,
        );

        assert!(report.deleted.is_empty());
        assert!(candidate.exists(), "cold cache candidate was evicted");
        assert_eq!(measurements.get(), 5);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_skips_cache_eviction_when_pressure_recovers_before_delete() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-cache-recovered-{}",
            uuid::Uuid::new_v4()
        ));
        let pressure_path = root.join("pressure");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let work = root.join("work");
        let candidate = sccache_root(&work, "untrusted", &layout).join("idle");
        fs::create_dir_all(&pressure_path).unwrap();
        fs::create_dir_all(candidate.join("key")).unwrap();
        fs::write(candidate.join("key/payload"), b"preserve").unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);
        let pin = crate::host_capacity::HostCapacityPin::open(&pressure_path).unwrap();
        let pressure_device = pin.device_id();
        let samples = std::cell::Cell::new(0_usize);
        let pressure_sample = |_: &Path| {
            pin.probe().unwrap();
            let sample = samples.get();
            samples.set(sample + 1);
            Some(PressureSample {
                available_bytes: if sample < 2 { 10 } else { 100 },
                used_percent: 0,
            })
        };

        let report = reclaim_work_root_with_layout_on_device(
            &work,
            &root.join("run"),
            &root.join("log"),
            ReclaimGoal::AvailableFloor(100),
            &BTreeSet::new(),
            true,
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            Some(pressure_device),
            &candidate_device_id,
            Some(&pressure_path),
            Some(&pin),
            &pressure_sample,
        )
        .unwrap();

        assert!(report.deleted.is_empty());
        assert!(
            candidate.exists(),
            "cache candidate was deleted after recovery"
        );
        assert!(
            samples.get() >= 3,
            "missing fresh pre-delete pressure sample"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pressure_reclaim_stops_all_roots_after_recovery_at_unlink_boundary() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-reclaim-stop-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let first_work = root.join("first-work");
        let second_work = root.join("second-work");
        let first_candidate = sccache_root(&first_work, "untrusted", &layout).join("idle");
        let second_candidate = sccache_root(&second_work, "untrusted", &layout).join("idle");
        for candidate in [&first_candidate, &second_candidate] {
            fs::create_dir_all(candidate.join("key")).unwrap();
            fs::write(candidate.join("key/payload"), b"preserve").unwrap();
            backdate(candidate, EMERGENCY_MIN_IDLE * 2);
        }

        let pressure_device = filesystem_device_id(&root).unwrap_or_else(|| {
            fs::create_dir_all(&root).unwrap();
            filesystem_device_id(&root).unwrap()
        });
        let recovery_sampled = std::cell::Cell::new(false);
        let pressure_sample = |_: &Path| {
            if !first_candidate.exists() && !recovery_sampled.replace(true) {
                Some(PressureSample {
                    available_bytes: 100,
                    used_percent: 0,
                })
            } else {
                // Pressure returns after the failing callback. The reclaim
                // pass must stay stopped rather than entering another root.
                Some(PressureSample {
                    available_bytes: 10,
                    used_percent: 0,
                })
            }
        };

        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::AvailableFloor(100),
            &[first_work, second_work],
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            pressure_device,
            &candidate_device_id,
            None,
            &pressure_sample,
        );

        assert!(report.deleted.is_empty());
        assert!(first_candidate.exists(), "recovered candidate was deleted");
        assert!(
            second_candidate.exists(),
            "reclaim continued into a later work root after pressure recovered"
        );
        assert!(recovery_sampled.get(), "unlink boundary was not sampled");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pressure_device_uses_nearest_existing_ancestor() {
        use std::os::unix::fs::MetadataExt;

        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-device-ancestor-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let expected_device = fs::metadata(&root).unwrap().dev();
        let not_created_yet = root.join("new/cache/root");

        assert_eq!(
            filesystem_device_id(&not_created_yet),
            Some(expected_device)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pressure_capacity_pin_rejects_replaced_path_identity() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-capacity-pin-{}",
            uuid::Uuid::new_v4()
        ));
        let pressure = root.join("pressure");
        let replacement = root.join("replacement");
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let work = root.join("work");
        let candidate = sccache_root(&work, "untrusted", &layout).join("idle");
        fs::create_dir_all(&pressure).unwrap();
        fs::create_dir_all(&replacement).unwrap();
        fs::create_dir_all(candidate.join("key")).unwrap();
        fs::write(candidate.join("key/payload"), b"preserve").unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);
        let pin = crate::host_capacity::HostCapacityPin::open(&pressure).unwrap();
        assert!(pin.probe().is_ok());

        let displaced = root.join("pressure-pinned");
        fs::rename(&pressure, &displaced).unwrap();
        symlink(&replacement, &pressure).unwrap();
        assert!(pin.revalidate().is_err());
        assert!(pin.probe().is_err());
        let report = reclaim_for_disk_pressure_with_pin(
            &pressure,
            16,
            std::slice::from_ref(&work),
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            &pin,
        );
        assert!(report.deleted.is_empty());
        assert!(!report.failures.is_empty());
        assert!(
            candidate.exists(),
            "same-device replacement path deleted cache"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn net_pressure_delta_uses_one_pass_baseline() {
        assert_eq!(observed_net_freed_bytes(1_000, 1_100), 100);
        // A concurrent allocator consumes 50 bytes after the initial gain.
        // Rechecking against the pass baseline reports 50, not 100.
        assert_eq!(observed_net_freed_bytes(1_000, 1_050), 50);
        assert_eq!(observed_net_freed_bytes(1_000, 900), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_reports_net_capacity_after_concurrent_allocation() {
        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-net-capacity-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let work = root.join("work");
        let candidate = sccache_root(&work, "untrusted", &layout).join("idle");
        fs::create_dir_all(candidate.join("key")).unwrap();
        fs::write(candidate.join("key/payload"), vec![0; 32]).unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);
        let pressure_device = filesystem_device_id(&root).unwrap_or_else(|| {
            fs::create_dir_all(&root).unwrap();
            filesystem_device_id(&root).unwrap()
        });
        let post_delete_samples = std::cell::Cell::new(0_usize);
        let available = |_: &Path| {
            if candidate.exists() {
                Some(PressureSample {
                    available_bytes: 1_000,
                    used_percent: 0,
                })
            } else {
                let sample = post_delete_samples.get();
                post_delete_samples.set(sample + 1);
                Some(PressureSample {
                    available_bytes: if sample == 0 { 1_100 } else { 1_040 },
                    used_percent: 0,
                })
            }
        };

        let report = reclaim_for_disk_pressure_on_device(
            &root,
            ReclaimGoal::AvailableFloor(1_500),
            std::slice::from_ref(&work),
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            pressure_device,
            &candidate_device_id,
            None,
            &available,
        );

        assert_eq!(report.deleted, vec![candidate]);
        assert_eq!(report.freed_bytes, 40);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn pressure_inventory_captures_candidate_tree_without_following_links() {
        use std::os::unix::fs::{symlink, MetadataExt as _};

        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-tree-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        let anchor = root.join("anchor");
        let store = anchor.join("store");
        let candidate = store.join("scope/candidate");
        let outside = root.join("outside");
        fs::create_dir_all(&candidate).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(candidate.join("payload"), b"owned").unwrap();
        fs::write(outside.join("secret"), vec![0; 128]).unwrap();
        symlink(&outside, candidate.join("external")).unwrap();

        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(&anchor).unwrap();
        let snapshots = crate::leftover_disk::filesystem_candidate_tree_snapshots_under(
            &anchor,
            &store,
            &anchor_identity,
            1,
        )
        .unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].path, store.join("scope"));
        assert_eq!(snapshots[0].logical_bytes, 5);
        assert!(snapshots[0].same_mount_tree);
        let identity =
            crate::leftover_disk::filesystem_object_identity(&snapshots[0].directory).unwrap();
        assert_eq!(identity, snapshots[0].identity);
        assert_eq!(
            identity.inode,
            fs::metadata(store.join("scope")).unwrap().ino()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pressure_reclaim_keeps_deleted_report_when_history_append_fails() {
        use std::os::unix::fs::MetadataExt as _;

        let root = std::env::temp_dir().join(format!(
            "velnor-pressure-history-failure-{}",
            uuid::Uuid::new_v4()
        ));
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let work = root.join("work");
        let candidate = sccache_root(&work, "untrusted", &layout).join("idle");
        fs::create_dir_all(candidate.join("key")).unwrap();
        fs::write(candidate.join("key/payload"), vec![0; 32]).unwrap();
        backdate(&candidate, EMERGENCY_MIN_IDLE * 2);
        let run_root = root.join("run");
        let log_root = root.join("log");
        fs::write(&log_root, b"not a directory").unwrap();
        let pressure_device = fs::metadata(&root).unwrap().dev();
        let available = |_: &Path| {
            Some(PressureSample {
                available_bytes: if candidate.exists() { 10_000 } else { 10_100 },
                used_percent: 0,
            })
        };

        let report = reclaim_work_root_with_layout_on_device(
            &work,
            &run_root,
            &log_root,
            ReclaimGoal::Amount(50),
            &BTreeSet::new(),
            true,
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            Some(pressure_device),
            &candidate_device_id,
            Some(&root),
            None,
            &available,
        )
        .unwrap();

        assert!(!candidate.exists());
        assert_eq!(report.freed_bytes, 100);
        assert_eq!(report.deleted, vec![candidate]);
        assert!(report
            .failures
            .iter()
            .any(|failure| failure.contains("deleted but could not append GC history")));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn explicit_config_pressure_reclaims_selected_work_root_and_layout() {
        let root = std::env::temp_dir().join(format!(
            "velnor-explicit-config-pressure-{}",
            uuid::Uuid::new_v4()
        ));
        let config = root.join("daemon-config");
        let work = config.join("_work");
        let layout = crate::storage::StorageLayout {
            cache_root: config.join("cache"),
            lib_root: config.clone(),
            run_root: config.join("run"),
            log_root: config.join("log"),
            mode: "explicit-config",
        };
        let selected_cache = sccache_root(&work, "untrusted", &layout).join("idle/key");
        let decoy_work = root.join("other-domain/work");
        let decoy_layout = crate::storage::StorageLayout::from_prefix(&root.join("other-domain"));
        let decoy_cache = sccache_root(&decoy_work, "untrusted", &decoy_layout).join("idle/key");
        fs::create_dir_all(&selected_cache).unwrap();
        fs::create_dir_all(&decoy_cache).unwrap();
        fs::write(selected_cache.join("payload"), vec![0; 16]).unwrap();
        fs::write(decoy_cache.join("payload"), vec![0; 16]).unwrap();
        backdate(selected_cache.parent().unwrap(), EMERGENCY_MIN_IDLE * 2);
        backdate(decoy_cache.parent().unwrap(), EMERGENCY_MIN_IDLE * 2);
        let pressure = crate::host_capacity::HostCapacityPin::open(&config).unwrap();
        let report = reclaim_for_disk_pressure_with_pin(
            &config,
            pressure
                .probe()
                .unwrap()
                .available_bytes
                .saturating_add(128),
            &[work],
            &layout,
            Some(velnor_model::ExecutionBackendKind::MicroVm),
            &pressure,
        );

        assert!(!selected_cache.exists());
        assert!(decoy_cache.exists());
        assert_eq!(report.deleted.len(), 1);
        assert!(config.join("log/gc-history.jsonl").exists());
        assert!(report.failures.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn microvm_and_unknown_pressure_skip_buildkit_before_domain_resolution() {
        for backend in [None, Some(velnor_model::ExecutionBackendKind::MicroVm)] {
            let mut prune_calls = 0;
            let report = pressure_prune_buildkit_with_backend(
                backend,
                &crate::storage::StorageLayout::from_prefix(Path::new("/tmp/velnor-buildkit-test")),
                None,
                &|_| true,
                |_, _, _| {
                    prune_calls += 1;
                    crate::buildkit::PressurePruneReport::default()
                },
            );
            assert_eq!(prune_calls, 0, "backend {backend:?} reached host BuildKit");
            assert!(report.failures.is_empty());
        }
    }

    /// Emergency reclaim must not delete the store of a job that is merely
    /// between steps, uploading artifacts, or publishing a target generation —
    /// none of which show up as a running container, and two of which have no
    /// lease class published today.
    #[test]
    fn emergency_reclaim_keeps_stores_a_live_job_is_touching() {
        let root = std::env::temp_dir().join(format!("velnor-live-store-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        let work = root.join("lib/velnor-test/work");
        // Mid artifact upload and mid sccache write: touched right now.
        let artifacts =
            crate::store_catalog::StoreCatalog::for_work_root_with_layout(&work, &layout)
                .artifacts_run("run-1");
        let sccache_store_root = sccache_root(&work, "untrusted", &layout);
        let sccache = sccache_store_root.join("hot/key");
        // A genuinely cold store, so the pass is not vacuously empty.
        let cold = sccache_store_root.join("cold");
        for dir in [&artifacts, &sccache, &cold.join("key")] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("payload"), vec![0; 16]).unwrap();
        }
        backdate(&cold, EMERGENCY_MIN_IDLE * 2);

        let run_root = root.join("run");
        let report = reclaim_work_root_with_layout(
            &work,
            &run_root,
            &root.join("log"),
            u64::MAX,
            &BTreeSet::new(),
            true,
            &layout,
            None,
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

    /// Every store the emergency reclaimer may delete must declare a lease
    /// class, or it can be deleted out from under the job that owns it.
    #[test]
    fn every_emergency_managed_store_has_a_lease_class() {
        let work = PathBuf::from("/var/lib/velnor/work");
        let root = std::env::temp_dir().join(format!("velnor-root-lease-{}", uuid::Uuid::new_v4()));
        let layout = crate::storage::StorageLayout::from_prefix(&root);
        for store in store_roots(&work, &StoreScope::with_layout(&layout)).unwrap() {
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
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let work = root.join("work");
        let trust_scope = crate::trust_scope::TRUSTED;
        let trust_key = crate::trust_scope::filesystem_key(trust_scope);
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "1").unwrap();
        let catalog = StoreCatalog::for_work_root_with_layout(&work, &layout);
        let registry = catalog.cargo(trust_scope).join("registry");
        // Store roots are unsplit: per-repo candidates descend from the
        // whole `bin` root at collection depth, like every other
        // repository-scoped class.
        let bin_root = catalog.cargo(trust_scope).join("bin");
        let canonical_bin = bin_root.join(&repository_key);
        fs::create_dir_all(registry.join("cache/index")).unwrap();
        fs::write(registry.join("cache/index/crate"), b"crate").unwrap();
        fs::create_dir_all(&canonical_bin).unwrap();
        fs::write(canonical_bin.join("tool"), b"tool").unwrap();

        let scope = StoreScope {
            layout: Some(layout.clone()),
            pool_trust_scope: trust_scope.to_owned(),
            daemon_environment: None,
        };
        let roots: Vec<_> = store_roots(&work, &scope)
            .unwrap()
            .into_iter()
            .filter(|store| {
                store.kind == CacheStore::Cargo
                    && (store.path == registry || store.path == bin_root)
            })
            .collect();
        let mut entries = Vec::new();
        for store in &roots {
            collect_candidates(store, &store.path, 0, &mut entries).unwrap();
        }

        assert!(entries.iter().any(|entry| {
            entry.path == registry && entry.scope_key() == format!("{trust_key}/registry")
        }));
        assert!(entries
            .iter()
            .any(|entry| entry.scope_key() == format!("{trust_key}/bin/{repository_key}")));
        assert!(entries
            .iter()
            .all(|entry| !entry.scope_key().contains("tailrocks_playground")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_job_leases_protect_every_mounted_store_across_daemons() {
        let run_root =
            std::env::temp_dir().join(format!("velnor-active-stores-{}", uuid::Uuid::new_v4()));
        let stale_after = Duration::from_secs(60);
        let trust_key = crate::trust_scope::filesystem_key(crate::trust_scope::TRUSTED);
        let playground_key =
            crate::store_catalog::repository_store_key("https://github.com", "1").unwrap();
        let other_key =
            crate::store_catalog::repository_store_key("https://github.com", "2").unwrap();
        let scopes = [
            (
                "targets",
                format!("{trust_key}/workspace-v2/{playground_key}/ci.yml"),
            ),
            ("actions-cache", format!("{trust_key}/{playground_key}")),
            ("cargo", format!("{trust_key}/registry")),
            ("cargo", format!("{trust_key}/git")),
            ("cargo", format!("{trust_key}/bin/{playground_key}")),
            ("mise", format!("{trust_key}/cache")),
            ("mise", format!("{trust_key}/installs/{playground_key}")),
            ("mise", format!("{trust_key}/binaries/{playground_key}")),
            ("mise", format!("{trust_key}/rustup/{playground_key}")),
        ];
        let mut leases = Vec::new();
        // Four jobs from one repository must be able to hold every shared and
        // repository-local store concurrently.
        for holder in ["job-1", "job-2", "job-3", "job-4"] {
            for (class, scope) in &scopes {
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
            (
                "targets",
                format!("{trust_key}/workspace-v2/{other_key}/ci.yml"),
            ),
            ("actions-cache", format!("{trust_key}/{other_key}")),
            ("cargo", format!("{trust_key}/registry")),
            ("cargo", format!("{trust_key}/git")),
            ("cargo", format!("{trust_key}/bin/{other_key}")),
            ("mise", format!("{trust_key}/cache")),
            ("mise", format!("{trust_key}/installs/{other_key}")),
            ("mise", format!("{trust_key}/binaries/{other_key}")),
            ("mise", format!("{trust_key}/rustup/{other_key}")),
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
                &[
                    trust_key.as_str(),
                    "workspace-v2",
                    playground_key.as_str(),
                    "ci.yml",
                ],
                90,
                10,
            ),
            entry(
                "/targets/other",
                CacheStore::Targets,
                &[
                    trust_key.as_str(),
                    "workspace-v2",
                    other_key.as_str(),
                    "ci.yml",
                ],
                90,
                10,
            ),
            entry(
                "/caches/playground",
                CacheStore::ActionsCache,
                &[trust_key.as_str(), playground_key.as_str()],
                90,
                10,
            ),
            entry(
                "/caches/other",
                CacheStore::ActionsCache,
                &[trust_key.as_str(), other_key.as_str()],
                90,
                10,
            ),
            entry(
                "/cargo/registry",
                CacheStore::Cargo,
                &[trust_key.as_str(), "registry"],
                90,
                10,
            ),
            entry(
                "/cargo/git",
                CacheStore::Cargo,
                &[trust_key.as_str(), "git"],
                90,
                10,
            ),
            entry(
                "/cargo/bin/playground",
                CacheStore::Cargo,
                &[trust_key.as_str(), "bin", playground_key.as_str()],
                90,
                10,
            ),
            entry(
                "/cargo/bin/other",
                CacheStore::Cargo,
                &[trust_key.as_str(), "bin", other_key.as_str()],
                90,
                10,
            ),
            entry(
                "/mise/cache",
                CacheStore::Mise,
                &[trust_key.as_str(), "cache"],
                90,
                10,
            ),
            entry(
                "/mise/installs/playground",
                CacheStore::Mise,
                &[trust_key.as_str(), "installs", playground_key.as_str()],
                90,
                10,
            ),
            entry(
                "/mise/installs/other",
                CacheStore::Mise,
                &[trust_key.as_str(), "installs", other_key.as_str()],
                90,
                10,
            ),
            entry(
                "/mise/binaries/playground",
                CacheStore::Mise,
                &[trust_key.as_str(), "binaries", playground_key.as_str()],
                90,
                10,
            ),
            entry(
                "/mise/binaries/other",
                CacheStore::Mise,
                &[trust_key.as_str(), "binaries", other_key.as_str()],
                90,
                10,
            ),
            entry(
                "/mise/rustup/playground",
                CacheStore::Mise,
                &[trust_key.as_str(), "rustup", playground_key.as_str()],
                90,
                10,
            ),
            entry(
                "/mise/rustup/other",
                CacheStore::Mise,
                &[trust_key.as_str(), "rustup", other_key.as_str()],
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

    #[test]
    fn trust_partitioned_repository_lease_protects_only_its_canonical_scope() {
        let root = std::env::temp_dir().join(format!(
            "velnor-trust-scoped-repository-lease-{}",
            uuid::Uuid::new_v4()
        ));
        let work = root.join("work");
        let run_root = root.join("run");
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let catalog = StoreCatalog::for_work_root_with_layout(&work, &layout);
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "123").unwrap();
        let trusted_key = crate::trust_scope::filesystem_key(crate::trust_scope::TRUSTED);
        let untrusted_key = crate::trust_scope::filesystem_key(crate::trust_scope::FAIL_CLOSED);
        let trusted_candidate = catalog
            .actions_cache(crate::trust_scope::TRUSTED)
            .join(&repository_key)
            .join("v1");
        let untrusted_candidate = catalog
            .actions_cache(crate::trust_scope::FAIL_CLOSED)
            .join(&repository_key)
            .join("v1");
        for candidate in [&trusted_candidate, &untrusted_candidate] {
            fs::create_dir_all(candidate).unwrap();
            fs::write(candidate.join("payload"), vec![0; 64]).unwrap();
        }
        backdate(&trusted_candidate, DAY * 3);
        backdate(&untrusted_candidate, DAY * 2);
        let scope = StoreScope {
            layout: Some(layout),
            pool_trust_scope: crate::trust_scope::TRUSTED.to_owned(),
            daemon_environment: None,
        };
        let listing = cache_listing(&work, false, &scope).unwrap();
        let trusted_entry = listing
            .iter()
            .find(|entry| entry.path == trusted_candidate)
            .unwrap();
        let untrusted_entry = listing
            .iter()
            .find(|entry| entry.path == untrusted_candidate)
            .unwrap();
        assert_eq!(
            trusted_entry.scope_key(),
            format!("{trusted_key}/{repository_key}")
        );
        assert_eq!(
            untrusted_entry.scope_key(),
            format!("{untrusted_key}/{repository_key}")
        );

        let stale_after = Duration::from_secs(60);
        let trusted_lease = crate::capacity::ScopeLease::acquire(
            &run_root,
            "actions-cache",
            &format!("{trusted_key}/{repository_key}/job-trusted"),
            stale_after,
        )
        .unwrap();
        let mut policy = policy();
        policy.class_budgets = BTreeMap::from([(CacheStore::ActionsCache, 64)]);
        policy.in_use_scopes = crate::capacity::active_scopes(&run_root, stale_after).unwrap();
        let candidates = select_eviction_candidates(&listing, &policy);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.path.as_path())
                .collect::<Vec<_>>(),
            vec![untrusted_candidate.as_path()],
            "a trusted lease must not protect the fail-closed candidate"
        );

        drop(trusted_lease);
        let _untrusted_lease = crate::capacity::ScopeLease::acquire(
            &run_root,
            "actions-cache",
            &format!("{untrusted_key}/{repository_key}/job-untrusted"),
            stale_after,
        )
        .unwrap();
        policy.in_use_scopes = crate::capacity::active_scopes(&run_root, stale_after).unwrap();
        let candidates = select_eviction_candidates(&listing, &policy);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.path.as_path())
                .collect::<Vec<_>>(),
            vec![trusted_candidate.as_path()],
            "a fail-closed lease must not protect the trusted candidate"
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A trusted job on a custom pool mounts executable stores under its
    /// admitted scope, and the runner leases exactly that scope. Reclaim must
    /// cover pool, floor, and PR-seeded namespaces while preserving leased
    /// stores in the pool and PR scopes.
    #[test]
    fn custom_pool_leases_protect_pool_and_pr_stores_from_gc() {
        let root =
            std::env::temp_dir().join(format!("velnor-custom-pool-lease-{}", uuid::Uuid::new_v4()));
        let work = root.join("work");
        let run_root = root.join("run");
        let layout = crate::storage::StorageLayout::from_prefix(&root.join("storage"));
        let trust = crate::trust_class::AdmittedTrust::narrow(
            crate::trust_class::TrustClass::Trusted,
            "public-forks",
        );
        let effective = trust.effective_scope();
        assert_eq!(effective, "public-forks");
        let repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "1").unwrap();
        let idle_repository_key =
            crate::store_catalog::repository_store_key("https://github.com", "2").unwrap();
        let catalog = StoreCatalog::for_work_root_with_layout(&work, &layout);
        let cargo_root = catalog.cargo(effective);
        let mise_root = catalog.mise(effective);
        let floor_cargo_root = catalog.cargo(crate::trust_scope::FAIL_CLOSED);
        let pr_cargo_root = catalog.cargo(crate::trust_scope::PR_STORE_SCOPE);
        let cargo_bin_root = cargo_root.join("bin");
        let cargo_floor_bin_root = floor_cargo_root.join("bin");
        let cargo_pr_bin_root = pr_cargo_root.join("bin");
        let mise_install_root = mise_root.join("installs");
        let mise_binary_root = mise_root.join("binaries");
        let live_bin = cargo_bin_root.join(&repository_key);
        assert_eq!(
            live_bin,
            cargo_bin_root.join(&repository_key),
            "a trusted custom-pool job mounts its admitted namespace"
        );
        let live_installs = mise_install_root.join(&repository_key);
        let live_binaries = mise_binary_root.join(&repository_key);
        let pr_live_bin = cargo_pr_bin_root.join(&repository_key);
        let pr_idle_bin = cargo_pr_bin_root.join(&idle_repository_key);

        // An idle same-pool store and an idle floor store, so the pass is not
        // vacuously empty: both must be evictable while the live store stands.
        let idle_bin = cargo_bin_root.join(&idle_repository_key);
        let floor_bin = cargo_floor_bin_root.join(&idle_repository_key);
        let idle_installs = mise_install_root.join(&idle_repository_key);
        let idle_binaries = mise_binary_root.join(&idle_repository_key);
        for path in [
            &live_bin,
            &live_installs,
            &live_binaries,
            &idle_bin,
            &floor_bin,
            &idle_installs,
            &idle_binaries,
            &pr_live_bin,
            &pr_idle_bin,
        ] {
            fs::create_dir_all(path).unwrap();
            fs::write(path.join("tool"), vec![0; 16]).unwrap();
        }
        // Idle stores sort before the live ones under the oldest-first
        // budget walk, deterministically.
        backdate(&idle_bin, DAY * 3);
        backdate(&floor_bin, DAY * 2);
        backdate(&idle_installs, DAY * 3);
        backdate(&idle_binaries, DAY * 3);
        backdate(&pr_idle_bin, DAY * 3);

        // The runner's lease publication, through the shared derivation: one
        // holder lease per live scope, read back through the real round-trip.
        let stale_after = Duration::from_secs(60);
        let holder = "job-1";
        let trust_key = crate::trust_scope::filesystem_key(effective);
        let pr_trust_key = crate::trust_scope::filesystem_key(crate::trust_scope::PR_STORE_SCOPE);
        let prefixed_scope = |trust_key: &str, path: &Path, root: &Path| {
            format!(
                "{trust_key}/{}",
                crate::storage::gc_scope_below_root(path, root).unwrap()
            )
        };
        let scopes = [
            ("cargo", prefixed_scope(&trust_key, &live_bin, &cargo_root)),
            (
                "mise",
                prefixed_scope(&trust_key, &live_installs, &mise_root),
            ),
            (
                "mise",
                prefixed_scope(&trust_key, &live_binaries, &mise_root),
            ),
            (
                "cargo",
                prefixed_scope(&pr_trust_key, &pr_live_bin, &pr_cargo_root),
            ),
        ];
        assert_eq!(scopes[0].1, format!("{trust_key}/bin/{repository_key}"));
        assert_eq!(
            scopes[1].1,
            format!("{trust_key}/installs/{repository_key}")
        );
        assert_eq!(
            scopes[2].1,
            format!("{trust_key}/binaries/{repository_key}")
        );
        assert_eq!(scopes[3].1, format!("{pr_trust_key}/bin/{repository_key}"));
        assert_eq!(
            prefixed_scope(
                &crate::trust_scope::filesystem_key(crate::trust_scope::FAIL_CLOSED),
                &floor_bin,
                &floor_cargo_root,
            ),
            format!(
                "{}/bin/{idle_repository_key}",
                crate::trust_scope::filesystem_key(crate::trust_scope::FAIL_CLOSED),
            )
        );
        let leases: Vec<_> = scopes
            .iter()
            .map(|(class, scope)| {
                crate::capacity::ScopeLease::acquire(
                    &run_root,
                    class,
                    &format!("{scope}/{holder}"),
                    stale_after,
                )
                .unwrap()
            })
            .collect();
        let active = crate::capacity::active_scopes(&run_root, stale_after).unwrap();

        let scope = StoreScope {
            layout: Some(layout.clone()),
            pool_trust_scope: effective.to_owned(),
            daemon_environment: None,
        };
        let listing = cache_listing(&work, false, &scope).unwrap();
        assert!(listing.iter().any(|entry| entry.path == pr_idle_bin));
        let mut policy = policy();
        policy.in_use_scopes = active;
        // Bind each class budget to its live bytes: every idle store must go,
        // the live ones must stand.
        policy.class_budgets = BTreeMap::from([(CacheStore::Cargo, 32), (CacheStore::Mise, 32)]);
        let candidates = select_eviction_candidates(&listing, &policy);
        let evicted: BTreeSet<_> = candidates
            .into_iter()
            .map(|candidate| candidate.path)
            .collect();
        for idle in [
            &idle_bin,
            &floor_bin,
            &pr_idle_bin,
            &idle_installs,
            &idle_binaries,
        ] {
            assert!(
                evicted.contains(idle),
                "idle store {} must be evicted: {evicted:?}",
                idle.display()
            );
        }
        for live in [&live_bin, &live_installs, &live_binaries, &pr_live_bin] {
            assert!(
                !evicted.contains(live),
                "live custom-pool store {} is leased and must survive: {evicted:?}",
                live.display()
            );
        }
        drop(leases);
        fs::remove_dir_all(root).unwrap();
    }
}
