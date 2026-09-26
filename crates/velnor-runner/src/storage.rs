use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::args::{StorageArgs, StorageCommand};

pub fn run(args: StorageArgs) -> Result<()> {
    let layout = match StorageLayout::resolve() {
        Some(layout) => layout,
        None => {
            if args.config_dir.is_some() {
                let config = crate::config::config_dir(args.config_dir)?;
                StorageLayout {
                    cache_root: config.join("cache"),
                    lib_root: config.clone(),
                    run_root: config.join("run"),
                    log_root: config.join("log"),
                    mode: "explicit-config",
                }
            } else {
                StorageLayout::user_cli()?
            }
        }
    };
    match args.command {
        StorageCommand::Paths => {
            println!("mode\t{}", layout.mode);
            println!("cache\t{}", layout.cache_root.display());
            println!("lib\t{}", layout.lib_root.display());
            println!("run\t{}", layout.run_root.display());
            println!("log\t{}", layout.log_root.display());
        }
        StorageCommand::Status => {
            println!("class\tbytes\tpath");
            for entry in catalog(&layout)? {
                println!("{}\t{}\t{}", entry.class, entry.bytes, entry.path.display());
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLayout {
    pub cache_root: PathBuf,
    pub lib_root: PathBuf,
    pub run_root: PathBuf,
    pub log_root: PathBuf,
    pub mode: &'static str,
}

impl StorageLayout {
    pub fn from_prefix(prefix: &Path) -> Self {
        let run_root = if prefix == Path::new("/var") {
            PathBuf::from("/run/velnor")
        } else {
            prefix.join("run/velnor")
        };
        Self {
            cache_root: prefix.join("cache/velnor/v1"),
            lib_root: prefix.join("lib/velnor"),
            run_root,
            log_root: prefix.join("log/velnor"),
            mode: "explicit",
        }
    }

    pub fn resolve() -> Option<Self> {
        std::env::var_os("VELNOR_STORAGE_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|prefix| Self::from_prefix(&prefix))
    }

    /// Interactive CLI without `VELNOR_STORAGE_ROOT`: XDG cache/state/runtime,
    /// never `$HOME/.velnor`.
    pub fn user_cli() -> Result<Self> {
        let home = std::env::var_os("HOME");
        let state =
            crate::config::user_state_dir(std::env::var_os("XDG_STATE_HOME"), home.clone())?;
        let cache = crate::config::user_cache_dir(std::env::var_os("XDG_CACHE_HOME"), home)?;
        let runtime = crate::config::user_runtime_dir(std::env::var_os("XDG_RUNTIME_DIR"));
        Ok(Self {
            cache_root: cache.join("velnor"),
            lib_root: state.join("velnor"),
            run_root: runtime.join("velnor"),
            log_root: state.join("velnor").join("log"),
            mode: "xdg-user",
        })
    }

    pub fn cache_class(&self, trust_scope: &str, class: &str) -> PathBuf {
        self.cache_root
            .join(crate::container::sanitize_store_key(trust_scope))
            .join(class)
    }
}

/// Resolve the root of a trust-partitioned store class.
///
/// `trust_scope` is the scope in effect for the caller: the job's admitted
/// scope on the execution path, the pool scope or the untrusted floor on the
/// GC path. There is no ambient read here — a caller that guessed would hand
/// one job's stores to another class. In the legacy layout the root carries
/// no trust segment (trust appends below it, per store); in the canonical
/// layout the root is namespaced by the scope.
pub fn cache_class_path(
    legacy_work_root: &Path,
    trust_scope: &str,
    class: &str,
    legacy_name: &str,
) -> PathBuf {
    let layout = StorageLayout::resolve();
    cache_class_path_with_layout(
        legacy_work_root,
        trust_scope,
        class,
        legacy_name,
        layout.as_ref(),
    )
}

pub fn cache_class_path_with_layout(
    legacy_work_root: &Path,
    trust_scope: &str,
    class: &str,
    legacy_name: &str,
    layout: Option<&StorageLayout>,
) -> PathBuf {
    let legacy = legacy_work_root.join(legacy_name);
    let Some(layout) = layout else {
        return legacy;
    };
    let canonical = layout.cache_class(crate::trust_scope::normalize_scope(trust_scope), class);
    prefer_canonical_or_existing_legacy(canonical, legacy)
}

/// Resolve a trust-scoped store path below its class root, without consulting
/// process-global trust state. Takes the scope in effect for the caller, like
/// [`cache_class_path`]; unlike the root, the legacy form also carries the
/// trust segment, so both layouts namespace the store by the scope.
pub fn cache_class_path_for_trust(
    legacy_work_root: &Path,
    trust_scope: &str,
    class: &str,
    legacy_name: &str,
) -> PathBuf {
    let layout = StorageLayout::resolve();
    cache_class_path_for_trust_with_layout(
        legacy_work_root,
        trust_scope,
        class,
        legacy_name,
        layout.as_ref(),
    )
}

pub fn cache_class_path_for_trust_with_layout(
    legacy_work_root: &Path,
    trust_scope: &str,
    class: &str,
    legacy_name: &str,
    layout: Option<&StorageLayout>,
) -> PathBuf {
    let trust = crate::container::sanitize_store_key(trust_scope);
    let legacy = legacy_work_root.join(legacy_name).join(&trust);
    let Some(layout) = layout else {
        return legacy;
    };
    let canonical = layout.cache_class(&trust, class);
    prefer_canonical_or_existing_legacy(canonical, legacy)
}

pub fn prefer_canonical_or_existing_legacy(canonical: PathBuf, legacy: PathBuf) -> PathBuf {
    if canonical.exists() || !legacy.exists() {
        canonical
    } else {
        legacy
    }
}

/// Prefix of the temporary name a seed copy is written under, beside its
/// destination, before it is linked into place.
pub const SEED_TEMP_PREFIX: &str = ".velnor-seed-";

static SEED_TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What one Cargo store seed did ([`seed_cargo_store`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CargoStoreSeedReport {
    /// Regular files copied into the destination store.
    pub files: usize,
    /// Bytes those files hold.
    pub bytes: u64,
    /// Seed units the budget refused (files under `registry/`, whole bare
    /// repositories under `git/db`).
    pub skipped_units: usize,
    /// Files those refused units would have copied.
    pub skipped_files: usize,
    /// Bytes those refused units would have copied.
    pub skipped_bytes: u64,
    pub elapsed: std::time::Duration,
}

impl CargoStoreSeedReport {
    /// The one daemon log line every pr-scope job admission prints.
    #[must_use]
    pub fn summary_line(&self, to_scope: &str, from_scope: &str) -> String {
        format!(
            "seeded {to_scope} cargo store from {from_scope}: {} files, {} bytes, {} ms",
            self.files,
            self.bytes,
            self.elapsed.as_millis()
        )
    }

    /// The daemon log line naming what the budget refused, when anything.
    #[must_use]
    pub fn skipped_line(&self, to_scope: &str, budget_bytes: u64) -> Option<String> {
        (self.skipped_units > 0).then(|| {
            format!(
                "{to_scope} cargo store seed skipped {} unit(s) ({} files, {} bytes): the store budget left {budget_bytes} bytes of headroom; newest entries were seeded first",
                self.skipped_units, self.skipped_files, self.skipped_bytes
            )
        })
    }
}

/// Seed the daemon-shared Cargo subtrees of the store at `to` from the store
/// at `from`, by copy (D18).
///
/// Same-repo PR jobs on a trusted pool write the `pr` scope's Cargo store,
/// which is a persistent host directory bind-mounted read-write into every
/// such job like every other `pr`-scope store; concurrent jobs share it under
/// Cargo's own package-cache locking. Before a PR job's container starts, the
/// daemon fills that store with whatever the `trusted` store already holds,
/// so a PR build starts as warm as a trusted one and one `Prepare Cargo` job
/// warms every unit that follows it on the host.
///
/// The seed is a **copy**, never a hard link and never an overlay: a hard
/// link would give PR code an inode it shares with the trusted store, and a
/// file modified in place through the `pr` mount would then be modified in
/// `trusted` — exactly the poisoning D18 exists to prevent. (The filesystem
/// may share blocks copy-on-write beneath the copy, as APFS clones and
/// `copy_file_range` reflinks do; it never shares the inode, so a write
/// through one path is never visible through the other.) An overlay cannot
/// share one upper between the concurrent mounts of several slots, so its
/// writes could never be the shared, persistent `pr` store this seed fills.
///
/// For every regular file below `from/<subtree>` (the subtrees in
/// [`crate::container::CARGO_STORE_SUBTREES`]) that is absent in `to` —
/// absent as anything: an existing file, directory or symlink is never
/// touched — the file is copied to a [`SEED_TEMP_PREFIX`] name in the
/// destination directory and linked into place with a no-replace rename
/// (`link(2)` of the temp onto the destination name, then `unlink` of the
/// temp): readers see either no file or a complete one, a concurrent seed or
/// job that created the name first wins, and nothing in `to` is ever
/// overwritten. Symlinks in `from` are skipped, not followed: a link out of
/// the trusted store is not trusted content.
///
/// `budget_bytes` bounds what the seed may add. Seed units — one file under
/// `registry/`, one whole bare repository under `git/db` (a partially copied
/// repository is corrupt to git, so a repository is copied whole or not at
/// all) — are taken newest first by modification time; a unit that does not
/// fit the remaining budget is skipped and counted in the report. `None` is
/// unbounded.
///
/// # Errors
/// A store subtree could not be read, or a copy could not be written. A
/// missing `from` subtree is not an error: there is nothing to seed from.
pub fn seed_cargo_store(
    from: &Path,
    to: &Path,
    budget_bytes: Option<u64>,
) -> std::io::Result<CargoStoreSeedReport> {
    let started = std::time::Instant::now();
    if from == to {
        // One store root for both scopes (the legacy layout carries no
        // trust segment on the Cargo root): nothing is missing from itself.
        return Ok(CargoStoreSeedReport {
            elapsed: started.elapsed(),
            ..CargoStoreSeedReport::default()
        });
    }
    let mut units = Vec::new();
    for (subtree, _) in crate::container::CARGO_STORE_SUBTREES {
        collect_seed_units(from, to, Path::new(subtree), &mut units)?;
    }
    // Newest first, so a budget too small for everything keeps the entries a
    // current build is most likely to need.
    units.sort_by(|a, b| {
        b.newest
            .cmp(&a.newest)
            .then_with(|| a.files[0].relative.cmp(&b.files[0].relative))
    });
    let mut report = CargoStoreSeedReport::default();
    let mut remaining = budget_bytes;
    for unit in units {
        if let Some(headroom) = remaining
            && unit.bytes > headroom
        {
            report.skipped_units += 1;
            report.skipped_files += unit.files.len();
            report.skipped_bytes += unit.bytes;
            continue;
        }
        for file in &unit.files {
            if let Some(bytes) = seed_file(&from.join(&file.relative), &to.join(&file.relative))? {
                report.files += 1;
                report.bytes += bytes;
                if let Some(headroom) = remaining.as_mut() {
                    *headroom = headroom.saturating_sub(bytes);
                }
            }
        }
    }
    report.elapsed = started.elapsed();
    Ok(report)
}

/// One file the seed would copy: its path relative to the store root and
/// what the source's metadata says about it.
#[derive(Debug)]
struct SeedFile {
    relative: PathBuf,
    bytes: u64,
    modified: std::time::SystemTime,
}

/// The all-or-nothing unit the budget decides on.
#[derive(Debug)]
struct SeedUnit {
    files: Vec<SeedFile>,
    bytes: u64,
    newest: std::time::SystemTime,
}

impl SeedUnit {
    fn new(files: Vec<SeedFile>) -> Self {
        let bytes = files.iter().map(|file| file.bytes).sum();
        let newest = files
            .iter()
            .map(|file| file.modified)
            .max()
            .unwrap_or(std::time::UNIX_EPOCH);
        Self {
            files,
            bytes,
            newest,
        }
    }
}

/// The relative paths under `git/db` are bare repositories, one directory
/// each; they are seeded whole.
const CARGO_GIT_DB_SUBTREE: &str = "git/db";

fn collect_seed_units(
    from: &Path,
    to: &Path,
    subtree: &Path,
    units: &mut Vec<SeedUnit>,
) -> std::io::Result<()> {
    let source_root = from.join(subtree);
    let entries = match fs::read_dir(&source_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if subtree == Path::new(CARGO_GIT_DB_SUBTREE) {
        for entry in entries {
            let entry = entry?;
            let relative = subtree.join(entry.file_name());
            let file_type = entry.file_type()?;
            let mut files = Vec::new();
            if file_type.is_dir() {
                collect_missing_files(from, to, &relative, &mut files)?;
                // Objects before refs: a reader that sees a ref sees the
                // objects it points at.
                files.sort_by_key(|file| (git_copy_rank(&file.relative), file.relative.clone()));
            } else if file_type.is_file()
                && let Some(file) = missing_file(from, to, &relative)?
            {
                files.push(file);
            }
            if !files.is_empty() {
                units.push(SeedUnit::new(files));
            }
        }
        return Ok(());
    }
    let mut files = Vec::new();
    collect_missing_files(from, to, subtree, &mut files)?;
    units.extend(files.into_iter().map(|file| SeedUnit::new(vec![file])));
    Ok(())
}

/// Every regular file below `from/<relative>` that `to` lacks. Symlinks are
/// neither followed nor copied.
fn collect_missing_files(
    from: &Path,
    to: &Path,
    relative: &Path,
    files: &mut Vec<SeedFile>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(from.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_missing_files(from, to, &child, files)?;
        } else if file_type.is_file()
            && let Some(file) = missing_file(from, to, &child)?
        {
            files.push(file);
        }
    }
    Ok(())
}

fn missing_file(from: &Path, to: &Path, relative: &Path) -> std::io::Result<Option<SeedFile>> {
    if fs::symlink_metadata(to.join(relative)).is_ok() {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(from.join(relative))?;
    if !metadata.is_file() {
        return Ok(None);
    }
    Ok(Some(SeedFile {
        relative: relative.to_path_buf(),
        bytes: metadata.len(),
        modified: metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
    }))
}

/// Copy order inside one bare repository: objects first, refs last.
fn git_copy_rank(relative: &Path) -> u8 {
    let name = relative
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let in_refs = relative
        .components()
        .any(|component| component.as_os_str() == "refs");
    if in_refs
        || matches!(
            name.as_ref(),
            "HEAD" | "FETCH_HEAD" | "ORIG_HEAD" | "packed-refs"
        )
    {
        2
    } else if relative
        .components()
        .any(|component| component.as_os_str() == "objects")
    {
        0
    } else {
        1
    }
}

/// The temporary name a seed of `dest` is written under: a sibling in the
/// same directory, so the final link is within one directory and one
/// filesystem.
fn seed_temp_path(dest: &Path) -> PathBuf {
    let sequence = SEED_TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("{SEED_TEMP_PREFIX}{}-{sequence}", std::process::id());
    match dest.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Copy `src` to `dest` without ever replacing an existing `dest`.
///
/// Returns the bytes copied, or `None` when `dest` came into existence
/// first (a concurrent seed or a job's own Cargo write); the temp copy is
/// removed on every path.
fn seed_file(src: &Path, dest: &Path) -> std::io::Result<Option<u64>> {
    if fs::symlink_metadata(dest).is_ok() {
        return Ok(None);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = seed_temp_path(dest);
    let bytes = match fs::copy(src, &temp) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
    };
    // `link(2)` fails with EEXIST instead of replacing, which `rename(2)`
    // would do: this is the no-replace rename. The link is temp → dest
    // inside the destination store; nothing here ever links to `src`.
    let linked = fs::hard_link(&temp, dest);
    let _ = fs::remove_file(&temp);
    match linked {
        Ok(()) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn append_legacy_trust(root: PathBuf, trust_scope: &str) -> PathBuf {
    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("_velnor_"))
    {
        root.join(crate::container::sanitize_store_key(trust_scope))
    } else {
        root
    }
}

/// The GC lease scope of a trust-partitioned store: its path relative to
/// its class root, in `/`-separated form (`bin/<trust>/<repo>` in the
/// legacy layout, `bin/<repo>` in the canonical one).
///
/// The one spelling of lease-scope derivation, shared by the runner's lease
/// publication and the lease-vs-mount conformance test. Both sides call the
/// same store-path helpers with the job's admitted scope and strip the same
/// class root, so a lease cannot name a namespace the mounts do not write —
/// the custom-pool divergence (`bin/public-forks/<repo>` leased while the
/// job mounts a class namespace) is inexpressible, not merely untested.
/// Fails when the store is not below the root rather than leasing a scope
/// that names nothing.
pub fn gc_scope_below_root(store: &Path, class_root: &Path) -> Result<String> {
    store
        .strip_prefix(class_root)
        .with_context(|| {
            format!(
                "store {} is not below its class root {}",
                store.display(),
                class_root.display()
            )
        })
        .map(|relative| relative.to_string_lossy().to_string())
}

pub fn child_with_legacy_trust(root: PathBuf, child: &str, trust_scope: &str) -> PathBuf {
    let child = root.join(child);
    if root
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("_velnor_"))
    {
        child.join(crate::container::sanitize_store_key(trust_scope))
    } else {
        child
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub class: String,
    pub path: PathBuf,
    pub bytes: u64,
}

pub fn catalog(layout: &StorageLayout) -> Result<Vec<CatalogEntry>> {
    let mut entries = Vec::new();
    if !layout.cache_root.exists() {
        return Ok(entries);
    }
    for trust in fs::read_dir(&layout.cache_root)
        .with_context(|| format!("read {}", layout.cache_root.display()))?
    {
        let trust = trust?.path();
        if !trust.is_dir() {
            continue;
        }
        for class in fs::read_dir(&trust).with_context(|| format!("read {}", trust.display()))? {
            let path = class?.path();
            if !path.is_dir() {
                continue;
            }
            entries.push(CatalogEntry {
                class: format!(
                    "{}/{}",
                    trust.file_name().unwrap_or_default().to_string_lossy(),
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                bytes: dir_size(&path)?,
                path,
            });
        }
    }
    entries.sort_by(|a, b| a.class.cmp(&b.class));
    Ok(entries)
}

pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0;
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        total += dir_size(&entry?.path())?;
    }
    Ok(total)
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
    fn system_prefix_uses_canonical_linux_layout() {
        let layout = StorageLayout::from_prefix(Path::new("/var"));
        assert_eq!(layout.cache_root, Path::new("/var/cache/velnor/v1"));
        assert_eq!(layout.lib_root, Path::new("/var/lib/velnor"));
        assert_eq!(layout.run_root, Path::new("/run/velnor"));
        assert_eq!(layout.log_root, Path::new("/var/log/velnor"));
        assert_ne!(layout.lib_root, Path::new("/root/.velnor/runner"));
    }

    #[test]
    fn legacy_store_remains_readable_until_migrated() {
        let root = std::env::temp_dir().join(format!("velnor-storage-{}", uuid::Uuid::new_v4()));
        let legacy = root.join("legacy");
        let canonical = root.join("canonical");
        fs::create_dir_all(&legacy).unwrap();
        assert_eq!(
            prefer_canonical_or_existing_legacy(canonical.clone(), legacy.clone()),
            legacy
        );
        fs::create_dir_all(&canonical).unwrap();
        assert_eq!(
            prefer_canonical_or_existing_legacy(canonical.clone(), legacy),
            canonical
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn seed_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-seed-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, contents: &[u8]) -> PathBuf {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn set_modified(path: &Path, seconds_ago: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds_ago);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn temp_files_below(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(SEED_TEMP_PREFIX))
                {
                    found.push(path.clone());
                }
                if path.is_dir() {
                    walk(&path, found);
                }
            }
        }
        walk(root, &mut found);
        found
    }

    #[test]
    fn seed_copies_only_missing_files_and_never_overwrites() {
        let root = seed_root("missing");
        let trusted = root.join("trusted");
        let pr = root.join("pr");
        write(&trusted, "registry/cache/index/a-1.0.0.crate", b"a-trusted");
        write(&trusted, "registry/cache/index/b-1.0.0.crate", b"b-trusted");
        write(&trusted, "registry/index/index/.cache/3/a/abc", b"index-a");
        write(&trusted, "registry/index/index/config.json", b"{}");
        // Outside the daemon-shared subtrees: never seeded.
        write(&trusted, "registry/src/index/a-1.0.0/lib.rs", b"src");
        write(&trusted, "bin/cargo-nextest", b"exe");
        write(&pr, "registry/cache/index/b-1.0.0.crate", b"b-pr-modified");
        // A `pr` directory or symlink where trusted has a file is also
        // "present": nothing is replaced by the seed.
        fs::create_dir_all(pr.join("registry/index/index/config.json")).unwrap();

        let report = seed_cargo_store(&trusted, &pr, None).unwrap();
        assert_eq!(report.files, 2, "{report:?}");
        assert_eq!(
            report.bytes,
            b"a-trusted".len() as u64 + b"index-a".len() as u64
        );
        assert_eq!(report.skipped_units, 0);
        assert_eq!(
            fs::read(pr.join("registry/cache/index/a-1.0.0.crate")).unwrap(),
            b"a-trusted"
        );
        assert_eq!(
            fs::read(pr.join("registry/cache/index/b-1.0.0.crate")).unwrap(),
            b"b-pr-modified",
            "an existing pr file is never overwritten"
        );
        assert_eq!(
            fs::read(pr.join("registry/index/index/.cache/3/a/abc")).unwrap(),
            b"index-a"
        );
        assert!(pr.join("registry/index/index/config.json").is_dir());
        assert!(!pr.join("registry/src").exists());
        assert!(!pr.join("bin").exists());
        assert_eq!(
            report.summary_line("pr", "trusted"),
            format!(
                "seeded pr cargo store from trusted: 2 files, {} bytes, {} ms",
                report.bytes,
                report.elapsed.as_millis()
            )
        );

        // Idempotent: a second seed finds nothing missing.
        let again = seed_cargo_store(&trusted, &pr, None).unwrap();
        assert_eq!((again.files, again.bytes), (0, 0));
        assert_eq!(
            again.summary_line("pr", "trusted"),
            format!(
                "seeded pr cargo store from trusted: 0 files, 0 bytes, {} ms",
                again.elapsed.as_millis()
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn seed_writes_a_same_directory_temp_and_leaves_none_behind() {
        let dest = Path::new("/store/pr/registry/cache/index/a-1.0.0.crate");
        let temp = seed_temp_path(dest);
        assert_eq!(temp.parent(), dest.parent());
        assert!(temp
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(SEED_TEMP_PREFIX));
        assert_ne!(seed_temp_path(dest), temp, "temp names are unique");

        let root = seed_root("atomic");
        let trusted = root.join("trusted");
        let pr = root.join("pr");
        for index in 0..5 {
            write(
                &trusted,
                &format!("registry/cache/index/crate-{index}.crate"),
                &[index; 64],
            );
        }
        write(&trusted, "git/db/dep-abc/objects/aa/bb", b"object");
        write(&trusted, "git/db/dep-abc/refs/heads/main", b"ref");
        write(&trusted, "git/db/dep-abc/HEAD", b"ref: refs/heads/main");
        let report = seed_cargo_store(&trusted, &pr, None).unwrap();
        assert_eq!(report.files, 8);
        assert_eq!(temp_files_below(&pr), Vec::<PathBuf>::new());
        assert_eq!(temp_files_below(&trusted), Vec::<PathBuf>::new());

        // A destination the copy cannot be written into is an error, and
        // still leaves no temp behind.
        let blocked_root = seed_root("blocked");
        let blocked_trusted = blocked_root.join("trusted");
        let blocked_pr = blocked_root.join("pr");
        write(&blocked_trusted, "registry/cache/index/a.crate", b"a");
        write(&blocked_pr, "registry/cache/index", b"not a directory");
        assert!(seed_cargo_store(&blocked_trusted, &blocked_pr, None).is_err());
        assert_eq!(temp_files_below(&blocked_pr), Vec::<PathBuf>::new());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(blocked_root).unwrap();
    }

    #[test]
    fn seed_skips_symlinks_and_never_follows_them() {
        let root = seed_root("symlinks");
        let trusted = root.join("trusted");
        let pr = root.join("pr");
        let outside = write(&root, "outside/secret.crate", b"outside the store");
        write(&trusted, "registry/cache/index/real.crate", b"real");
        std::os::unix::fs::symlink(&outside, trusted.join("registry/cache/index/linked.crate"))
            .unwrap();
        std::os::unix::fs::symlink(
            root.join("outside"),
            trusted.join("registry/cache/linked-dir"),
        )
        .unwrap();
        let report = seed_cargo_store(&trusted, &pr, None).unwrap();
        assert_eq!(report.files, 1, "{report:?}");
        assert!(pr.join("registry/cache/index/real.crate").is_file());
        assert!(fs::symlink_metadata(pr.join("registry/cache/index/linked.crate")).is_err());
        assert!(fs::symlink_metadata(pr.join("registry/cache/linked-dir")).is_err());
        assert!(!pr.join("registry/cache/linked-dir/secret.crate").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn modifying_a_seeded_pr_file_leaves_the_trusted_file_unchanged() {
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;

        // The poisoning D18 forbids: PR code rewrites a seeded file in place.
        // The seed is a copy, so the trusted bytes and inode are untouched.
        let root = seed_root("poison");
        let trusted = root.join("trusted");
        let pr = root.join("pr");
        let trusted_file = write(&trusted, "registry/cache/index/dep-1.0.0.crate", b"trusted");
        let trusted_git = write(&trusted, "git/db/dep-abc/objects/aa/bb", b"trusted object");
        seed_cargo_store(&trusted, &pr, None).unwrap();
        let pr_file = pr.join("registry/cache/index/dep-1.0.0.crate");
        let pr_git = pr.join("git/db/dep-abc/objects/aa/bb");
        for (seeded, source) in [(&pr_file, &trusted_file), (&pr_git, &trusted_git)] {
            let seeded_meta = fs::metadata(seeded).unwrap();
            let source_meta = fs::metadata(source).unwrap();
            assert_ne!(seeded_meta.ino(), source_meta.ino(), "{}", seeded.display());
            assert_eq!(source_meta.nlink(), 1, "{}", source.display());
            // In-place rewrite through the pr path, as a hostile build
            // script would do with the read-write mount.
            fs::OpenOptions::new()
                .write(true)
                .truncate(false)
                .open(seeded)
                .unwrap()
                .write_all(b"POISON")
                .unwrap();
        }
        assert_eq!(fs::read(&trusted_file).unwrap(), b"trusted");
        assert_eq!(fs::read(&trusted_git).unwrap(), b"trusted object");
        assert!(fs::read(&pr_file).unwrap().starts_with(b"POISON"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn seed_budget_takes_newest_units_first_and_reports_what_it_skipped() {
        let root = seed_root("budget");
        let trusted = root.join("trusted");
        let pr = root.join("pr");
        let old = write(&trusted, "registry/cache/index/old.crate", &[0; 100]);
        let mid = write(&trusted, "registry/cache/index/mid.crate", &[0; 100]);
        let new = write(&trusted, "registry/cache/index/new.crate", &[0; 100]);
        set_modified(&old, 3_000);
        set_modified(&mid, 2_000);
        set_modified(&new, 1_000);
        // A bare repository is one unit: 150 bytes across three files that
        // are copied whole or not at all.
        let object = write(&trusted, "git/db/dep-abc/objects/aa/bb", &[0; 100]);
        let head = write(&trusted, "git/db/dep-abc/HEAD", &[0; 25]);
        let reference = write(&trusted, "git/db/dep-abc/refs/heads/main", &[0; 25]);
        for path in [&object, &head, &reference] {
            set_modified(path, 1_500);
        }

        // 250 bytes: the newest crate (100), then the repository (150,
        // newest 1_500 s ago); `mid` and `old` no longer fit.
        let report = seed_cargo_store(&trusted, &pr, Some(250)).unwrap();
        assert_eq!(report.files, 4, "{report:?}");
        assert_eq!(report.bytes, 250);
        assert_eq!(report.skipped_units, 2);
        assert_eq!(report.skipped_files, 2);
        assert_eq!(report.skipped_bytes, 200);
        assert!(pr.join("registry/cache/index/new.crate").is_file());
        assert!(pr.join("git/db/dep-abc/objects/aa/bb").is_file());
        assert!(pr.join("git/db/dep-abc/refs/heads/main").is_file());
        assert!(!pr.join("registry/cache/index/mid.crate").exists());
        assert!(!pr.join("registry/cache/index/old.crate").exists());
        let skipped = report.skipped_line("pr", 250).unwrap();
        assert!(skipped.starts_with("pr cargo store seed skipped 2 unit(s) (2 files, 200 bytes)"));
        assert!(skipped.contains("250 bytes of headroom"), "{skipped}");

        // A budget below one whole repository copies none of it: no
        // partial bare repository ever lands in the pr store.
        let partial_root = seed_root("partial");
        let partial_trusted = partial_root.join("trusted");
        let partial_pr = partial_root.join("pr");
        write(&partial_trusted, "git/db/dep-abc/objects/aa/bb", &[0; 100]);
        write(&partial_trusted, "git/db/dep-abc/HEAD", &[0; 25]);
        let report = seed_cargo_store(&partial_trusted, &partial_pr, Some(100)).unwrap();
        assert_eq!(report.files, 0);
        assert_eq!(report.skipped_units, 1);
        assert!(!partial_pr.join("git/db/dep-abc").exists());
        assert!(seed_cargo_store(&partial_trusted, &partial_pr, Some(0))
            .unwrap()
            .skipped_line("pr", 0)
            .is_some());
        assert!(report.skipped_line("pr", 100).is_some());
        assert!(CargoStoreSeedReport::default()
            .skipped_line("pr", 100)
            .is_none());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(partial_root).unwrap();
    }

    #[test]
    fn seed_orders_a_repository_objects_first_and_refs_last() {
        assert_eq!(git_copy_rank(Path::new("git/db/dep/objects/aa/bb")), 0);
        assert_eq!(
            git_copy_rank(Path::new("git/db/dep/objects/pack/p.pack")),
            0
        );
        assert_eq!(git_copy_rank(Path::new("git/db/dep/config")), 1);
        assert_eq!(git_copy_rank(Path::new("git/db/dep/description")), 1);
        assert_eq!(git_copy_rank(Path::new("git/db/dep/refs/heads/main")), 2);
        assert_eq!(git_copy_rank(Path::new("git/db/dep/packed-refs")), 2);
        assert_eq!(git_copy_rank(Path::new("git/db/dep/HEAD")), 2);
        assert_eq!(git_copy_rank(Path::new("git/db/dep/FETCH_HEAD")), 2);
    }

    #[test]
    fn catalog_reports_class_bytes() {
        let root = std::env::temp_dir().join(format!("velnor-catalog-{}", uuid::Uuid::new_v4()));
        let layout = StorageLayout::from_prefix(&root);
        let class = layout.cache_root.join("trusted/targets");
        fs::create_dir_all(&class).unwrap();
        fs::write(class.join("artifact"), b"1234").unwrap();
        let entries = catalog(&layout).unwrap();
        assert_eq!(entries[0].class, "trusted/targets");
        assert_eq!(entries[0].bytes, 4);
        fs::remove_dir_all(root).unwrap();
    }
}
