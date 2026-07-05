use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};

use crate::{
    cli::{CacheArgs, CacheCommand, CacheGcArgs},
    config,
};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) fn run(args: CacheArgs) -> Result<()> {
    let work_root = work_root(args.config_dir, args.work_dir)?;
    match args.command {
        CacheCommand::Du => run_du(&work_root),
        CacheCommand::Gc(gc) => run_gc(&work_root, gc),
    }
}

fn work_root(config_dir: Option<PathBuf>, work_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(work_dir) = work_dir {
        return Ok(crate::container::daemon_shared_root(work_dir));
    }
    Ok(config::config_dir(config_dir)?.join("_work"))
}

fn run_du(work_root: &Path) -> Result<()> {
    let stores = store_roots(work_root);
    println!("work_dir\t{}", work_root.display());
    println!("kind\tbytes\tpath");
    for store in &stores {
        let bytes = dir_size(&store.path)?;
        println!("store\t{}\t{}", bytes, store.path.display());
    }

    println!("scope\tstore\tbytes\tscope");
    for store in &stores {
        for (scope, bytes) in scoped_sizes(store)? {
            println!("scope\t{}\t{}\t{}", store.kind, bytes, scope);
        }
    }
    Ok(())
}

fn run_gc(work_root: &Path, args: CacheGcArgs) -> Result<()> {
    if !args.dry_run {
        bail!("destructive cache gc is not implemented in this spike; pass --dry-run");
    }

    let listing = cache_listing(work_root)?;
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
        in_use_scopes: BTreeSet::new(),
    };
    let candidates = select_eviction_candidates(&listing, &policy);

    println!("dry_run\ttrue");
    println!("work_dir\t{}", work_root.display());
    println!("candidate_count\t{}", candidates.len());
    println!("store\tbytes\tscope\treason\tpath");
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
    Ok(())
}

#[derive(Debug, Clone)]
struct StoreRoot {
    kind: CacheStore,
    path: PathBuf,
    scope_depth: usize,
    candidate_depth: usize,
    gc_managed: bool,
}

fn store_roots(work_root: &Path) -> Vec<StoreRoot> {
    vec![
        StoreRoot {
            kind: CacheStore::Cargo,
            path: crate::container::cargo_store_host(work_root),
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Mise,
            path: crate::container::mise_store_host(work_root),
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Targets,
            path: crate::container::cargo_target_store_host(work_root),
            scope_depth: 4,
            candidate_depth: 4,
            gc_managed: true,
        },
        StoreRoot {
            kind: CacheStore::ActionsCache,
            path: work_root.join("_velnor_caches"),
            scope_depth: 2,
            candidate_depth: 3,
            gc_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Artifacts,
            path: work_root.join("_velnor_artifacts"),
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: true,
        },
        StoreRoot {
            kind: CacheStore::Sccache,
            path: crate::container::sccache_host(work_root),
            scope_depth: 1,
            candidate_depth: 1,
            gc_managed: false,
        },
    ]
}

fn scoped_sizes(store: &StoreRoot) -> Result<BTreeMap<String, u64>> {
    let mut sizes = BTreeMap::new();
    if !store.path.exists() {
        return Ok(sizes);
    }
    collect_scoped_sizes(&store.path, &store.path, store.scope_depth, &mut sizes)?;
    Ok(sizes)
}

fn collect_scoped_sizes(
    root: &Path,
    path: &Path,
    scope_depth: usize,
    sizes: &mut BTreeMap<String, u64>,
) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if metadata.is_file() {
        let scope = scope_for(root, path, scope_depth);
        *sizes.entry(scope).or_default() += metadata.len();
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0;
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        total += collect_scoped_sizes(root, &entry?.path(), scope_depth, sizes)?;
    }
    Ok(total)
}

fn cache_listing(work_root: &Path) -> Result<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    for store in store_roots(work_root)
        .into_iter()
        .filter(|store| store.gc_managed)
    {
        collect_candidates(&store, &store.path, 0, &mut entries)?;
    }
    Ok(entries)
}

fn collect_candidates(
    store: &StoreRoot,
    path: &Path,
    depth: usize,
    entries: &mut Vec<CacheEntry>,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if depth >= store.candidate_depth {
        let (bytes, modified) = size_and_modified(path)?;
        if bytes > 0 {
            entries.push(CacheEntry {
                path: path.to_path_buf(),
                store: store.kind,
                scope: scope_parts(&store.path, path, store.scope_depth),
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

fn dir_size(path: &Path) -> Result<u64> {
    Ok(size_and_modified(path)?.0)
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

fn scope_for(root: &Path, path: &Path, scope_depth: usize) -> String {
    scope_parts(root, path, scope_depth).join("/")
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CacheStore {
    Cargo,
    Mise,
    Targets,
    ActionsCache,
    Artifacts,
    Sccache,
}

impl fmt::Display for CacheStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cargo => "cargo",
            Self::Mise => "mise",
            Self::Targets => "targets",
            Self::ActionsCache => "actions-cache",
            Self::Artifacts => "artifacts",
            Self::Sccache => "sccache",
        })
    }
}

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
    pub(crate) in_use_scopes: BTreeSet<String>,
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

    for entry in entries.iter().filter(|entry| !in_use(entry, policy)) {
        if is_older_than(entry.modified, policy.now, policy.max_age) {
            add_candidate(&mut candidates, entry, "older-than-max-age");
        }
    }

    let mut target_scopes: BTreeMap<String, Vec<&CacheEntry>> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.store == CacheStore::Targets && !in_use(entry, policy))
    {
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
                .filter(|entry| !in_use(entry, policy))
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

    candidates.into_values().collect()
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
    policy.in_use_scopes.contains(&entry.scope_key())
}

fn is_older_than(modified: SystemTime, now: SystemTime, max_age: Duration) -> bool {
    now.duration_since(modified).is_ok_and(|age| age > max_age)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            in_use_scopes: BTreeSet::new(),
        }
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
        policy.in_use_scopes.insert("trusted/active".to_string());

        let candidates = select_eviction_candidates(&entries, &policy);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].path, PathBuf::from("/cache/idle"));
    }
}
