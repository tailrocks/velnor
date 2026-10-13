use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[cfg(unix)]
use std::{
    io::Read,
    os::fd::AsFd,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::Component,
};

use anyhow::{Context, Result};

use crate::args::{StorageArgs, StorageCommand};

#[cfg(unix)]
const BUILDKIT_STORAGE_ID_FILE: &str = ".velnor-buildkit-storage-id";
#[cfg(unix)]
const BUILDKIT_STORAGE_ID_LOCK_FILE: &str = ".velnor-buildkit-storage-id.lock";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedRunnerStorageLayout {
    layout: StorageLayout,
    /// Explicit base supplied by the daemon for a generated multi-slot worker.
    buildkit_identity_root: Option<PathBuf>,
}

static SELECTED_RUNNER_STORAGE_LAYOUT: OnceLock<SelectedRunnerStorageLayout> = OnceLock::new();
const LEGACY_TRUST_SCOPE_SUFFIX: &str = "__trust_scope_v1";

/// Storage layout selected for this runner process.
///
/// Runner entry points select the layout before starting job execution. Keeping
/// that choice process-local lets downstream facilities use explicit per-slot
/// config layouts as well as the canonical packaged layout.
pub fn selected_layout() -> Option<StorageLayout> {
    SELECTED_RUNNER_STORAGE_LAYOUT
        .get()
        .map(|selected| selected.layout.clone())
}

/// Resolve this process's storage layout using the runner's selected layout
/// before ambient environment configuration. Runner pressure and maintenance
/// paths must stay in the same domain chosen at startup.
pub(crate) fn selected_or_resolved_layout() -> Option<StorageLayout> {
    selected_layout().or_else(StorageLayout::resolve)
}

/// Resolve the storage layout used by cache and catalog paths. Unlike
/// `selected_or_resolved_layout`, this requires the user default when no
/// process or environment layout was selected and reports HOME-less
/// ambiguity to the caller.
pub(crate) fn resolve_required_layout() -> Result<StorageLayout> {
    resolve_required_layout_for_cli(None)
}

/// Resolve the standalone CLI layout without allowing its work-directory
/// override to influence the selected storage domain.
pub(crate) fn resolve_required_layout_for_cli(config_dir: Option<&Path>) -> Result<StorageLayout> {
    let cli_config = config_dir.map(StorageLayout::explicit_local);
    let env_config = std::env::var_os("VELNOR_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| StorageLayout::explicit_local(&path));
    resolve_required_layout_from(
        selected_layout(),
        StorageLayout::resolve(),
        cli_config,
        env_config,
        StorageLayout::user_cli,
    )
}

fn resolve_required_layout_from(
    selected: Option<StorageLayout>,
    storage_root: Option<StorageLayout>,
    cli_config: Option<StorageLayout>,
    env_config: Option<StorageLayout>,
    default_user: impl FnOnce() -> Result<StorageLayout>,
) -> Result<StorageLayout> {
    selected
        .or(storage_root)
        .or(cli_config)
        .or(env_config)
        .map(Ok)
        .unwrap_or_else(default_user)
}

/// Install the runner's chosen layout and optional shared BuildKit identity
/// root. The override is supplied only by daemon slot workers with a known
/// shared config base; path spelling alone never establishes that relationship.
pub(crate) fn install_selected_layout(
    layout: StorageLayout,
    buildkit_identity_root: Option<PathBuf>,
) -> Result<()> {
    let selected = SelectedRunnerStorageLayout {
        layout,
        buildkit_identity_root,
    };
    match SELECTED_RUNNER_STORAGE_LAYOUT.set(selected.clone()) {
        Ok(()) => Ok(()),
        Err(_) if SELECTED_RUNNER_STORAGE_LAYOUT.get() == Some(&selected) => Ok(()),
        Err(_) => anyhow::bail!(
            "runner process already selected a different storage layout: {:?}",
            SELECTED_RUNNER_STORAGE_LAYOUT
                .get()
                .map(|selected| &selected.layout)
        ),
    }
}

fn selected_buildkit_identity_root(
    selected: Option<&SelectedRunnerStorageLayout>,
    layout: &StorageLayout,
) -> PathBuf {
    let override_root = selected
        .filter(|selected| &selected.layout == layout)
        .and_then(|selected| selected.buildkit_identity_root.as_deref());
    layout.buildkit_identity_root_for_override(override_root)
}

pub fn run(args: StorageArgs) -> Result<()> {
    let layout = resolve_required_layout_for_cli(args.config_dir.as_deref())?;
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

    /// Storage layout for a runner started with an explicit local config
    /// directory and no `VELNOR_STORAGE_ROOT`.
    pub fn explicit_local(config_dir: &Path) -> Self {
        let run_base = config_dir
            .parent()
            .filter(|parent| parent.file_name().is_some_and(|name| name == "slots"))
            .and_then(Path::parent)
            .unwrap_or(config_dir);
        Self {
            cache_root: config_dir.join("cache"),
            lib_root: config_dir.to_path_buf(),
            run_root: run_base.join("run"),
            log_root: config_dir.join("logs"),
            mode: "explicit-config",
        }
    }

    /// Operator-selected root above generated cache descendants. This accepts
    /// only known layout modes, whose root comes from the storage-root setting,
    /// config-dir setting, or user default. Only that root may use a configured
    /// symlink alias; generated paths below it are opened one component at a
    /// time without following links.
    #[cfg(unix)]
    fn configured_root_path(&self) -> Option<PathBuf> {
        if self.mode == "explicit-config" {
            return (self.cache_root == self.lib_root.join("cache")).then(|| self.lib_root.clone());
        }
        if !matches!(self.mode, "explicit" | "user-storage-root") {
            return None;
        }

        let mut root = self.cache_root.clone();
        for expected in ["v1", "velnor", "cache"] {
            if root.file_name().is_none_or(|name| name != expected) {
                return None;
            }
            root = root.parent()?.to_path_buf();
        }
        Some(if root.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            root
        })
    }

    pub fn resolve() -> Option<Self> {
        std::env::var_os("VELNOR_STORAGE_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|prefix| Self::from_prefix(&prefix))
    }

    /// User storage layout without `VELNOR_STORAGE_ROOT`, matching the prefix
    /// exported by `velnorctl host start`.
    pub fn user_cli() -> Result<Self> {
        Self::user_layout_from_home(
            std::env::var_os("HOME"),
            velnor_client::default_user_storage_root,
        )
    }

    fn user_layout_from_home(
        home: Option<std::ffi::OsString>,
        default_root: impl FnOnce() -> PathBuf,
    ) -> Result<Self> {
        require_user_home(home)?;
        let mut layout = Self::from_prefix(&default_root());
        layout.mode = "user-storage-root";
        Ok(layout)
    }

    pub fn cache_class(&self, trust_scope: &str, class: &str) -> PathBuf {
        crate::trust_scope::filesystem_key_path(&self.cache_root, trust_scope).join(class)
    }

    /// Durable root selected for the BuildKit storage domain.
    ///
    /// A process-selected root override is honored only for the exact selected
    /// layout. Standalone layouts always use their own `lib_root`, even when
    /// its spelling resembles a generated daemon slot path.
    pub fn buildkit_identity_root(&self) -> PathBuf {
        selected_buildkit_identity_root(SELECTED_RUNNER_STORAGE_LAYOUT.get(), self)
    }

    pub(crate) fn buildkit_identity_root_for_override(
        &self,
        override_root: Option<&Path>,
    ) -> PathBuf {
        let root = override_root
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.lib_root.clone());
        normalize_buildkit_identity_root(&root, cfg!(target_os = "macos"))
    }
}

fn require_user_home(home: Option<std::ffi::OsString>) -> Result<()> {
    if home.is_none_or(|home| home.is_empty()) {
        anyhow::bail!(
            "HOME is not set; pass --config-dir or set VELNOR_CONFIG_DIR or VELNOR_STORAGE_ROOT"
        );
    }
    Ok(())
}

fn normalize_buildkit_identity_root(root: &Path, macos: bool) -> PathBuf {
    if macos && let Ok(remainder) = root.strip_prefix("/var") {
        return Path::new("/private/var").join(remainder);
    }
    root.to_path_buf()
}

/// Load or atomically initialize the durable identity for a BuildKit storage
/// root. Existing malformed or unsafe identity entries fail closed; they are
/// never replaced with a new identity.
pub fn ensure_buildkit_storage_identity(root: &Path) -> Result<String> {
    #[cfg(unix)]
    {
        ensure_buildkit_storage_identity_unix(root)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        anyhow::bail!(
            "durable BuildKit storage identity requires Unix no-follow filesystem support"
        )
    }
}

#[cfg(unix)]
fn ensure_buildkit_storage_identity_unix(root: &Path) -> Result<String> {
    if !root.is_absolute() {
        anyhow::bail!(
            "BuildKit storage identity root must be absolute: {}",
            root.display()
        );
    }
    if root
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        anyhow::bail!(
            "BuildKit storage identity root is not normalized: {}",
            root.display()
        );
    }
    let directory = crate::fs_copy::NoFollowDestinationDir::open_or_create_absolute_no_follow(root)
        .with_context(|| {
            format!(
                "securely open or create BuildKit storage identity root {}",
                root.display()
            )
        })?;

    // The lock has a stable pathname and lives beside the identity. O_NOFOLLOW
    // rejects a substituted symlink, O_NONBLOCK makes a substituted FIFO safe
    // to inspect. Opening relative to the secured directory keeps lock and
    // identity operations on the same inode even if the path is renamed.
    let lock = directory
        .open_or_create_lock_file(std::ffi::OsStr::new(BUILDKIT_STORAGE_ID_LOCK_FILE))
        .context("open BuildKit storage identity lock")?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
        .context("serialize BuildKit storage identity initialization")?;
    directory
        .sync_directory()
        .context("sync BuildKit storage identity root before inspection")?;

    let identity_path = Path::new(BUILDKIT_STORAGE_ID_FILE);
    if let Some(mut identity_file) = directory
        .open_relative_file_if_exists(identity_path)
        .context("inspect existing BuildKit storage identity")?
    {
        return read_buildkit_storage_identity(&mut identity_file);
    }

    let identity = uuid::Uuid::new_v4().hyphenated().to_string();
    let (mut staged, staged_name) = directory
        .create_temporary_file(".velnor-buildkit-storage-id")
        .context("stage BuildKit storage identity")?;
    use std::io::Write as _;
    let stage_result = (|| -> Result<()> {
        writeln!(staged, "{identity}").context("write staged BuildKit storage identity")?;
        staged
            .sync_all()
            .context("sync staged BuildKit storage identity")?;
        Ok(())
    })();
    drop(staged);
    if let Err(error) = stage_result {
        let _ = directory.remove_tree_entry(&staged_name);
        return Err(error);
    }
    if let Err(error) = directory.publish_temporary_file_no_replace(
        &staged_name,
        std::ffi::OsStr::new(BUILDKIT_STORAGE_ID_FILE),
    ) {
        let _ = directory.remove_tree_entry(&staged_name);
        return Err(error).context("publish BuildKit storage identity");
    }
    directory
        .sync_directory()
        .context("sync BuildKit storage identity directory")?;

    let mut identity_file = directory
        .open_relative_file(identity_path)
        .context("reopen published BuildKit storage identity")?;
    read_buildkit_storage_identity(&mut identity_file)
}

#[cfg(unix)]
fn read_buildkit_storage_identity(file: &mut fs::File) -> Result<String> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file
        .metadata()
        .context("inspect BuildKit storage identity file")?;
    if metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
        anyhow::bail!("BuildKit storage identity is hard-linked or accessible to group/other");
    }

    let mut contents = String::new();
    (&mut *file)
        .take(38)
        .read_to_string(&mut contents)
        .context("read BuildKit storage identity")?;
    if contents.len() != 36 && contents.len() != 37 {
        anyhow::bail!("BuildKit storage identity has invalid length");
    }
    let value = contents.strip_suffix('\n').unwrap_or(&contents);
    if value.contains('\n') || value.contains('\r') {
        anyhow::bail!("BuildKit storage identity has invalid line endings or extra data");
    }
    let parsed = uuid::Uuid::parse_str(value).context("parse BuildKit storage identity UUID")?;
    let canonical = parsed.hyphenated().to_string();
    if value != canonical {
        anyhow::bail!("BuildKit storage identity is not a canonical UUID");
    }
    Ok(canonical)
}

/// Resolve a trust-partitioned store class below the selected storage layout.
///
/// `trust_scope` is the scope in effect for the caller: the job's admitted
/// scope on the execution path, the pool scope or the untrusted floor on the
/// GC path. Callers without an explicit process snapshot resolve the host
/// default used by `host start` and fail if it cannot be determined.
pub fn cache_class_path(trust_scope: &str, class: &str) -> Result<PathBuf> {
    cache_class_path_with_layout(trust_scope, class, None)
}

pub fn cache_class_path_with_layout(
    trust_scope: &str,
    class: &str,
    layout: Option<&StorageLayout>,
) -> Result<PathBuf> {
    let resolved;
    let layout = match layout {
        Some(layout) => layout,
        None => {
            resolved = resolve_required_layout()?;
            &resolved
        }
    };
    Ok(layout.cache_class(crate::trust_scope::normalize_scope(trust_scope), class))
}

/// Versioned sibling name of the retired work-root store family. This remains
/// only so daemon startup can purge the old MBX root once; active store paths
/// never resolve through it.
pub(crate) fn legacy_store_root(legacy_work_root: &Path, legacy_name: &str) -> PathBuf {
    let mut name = std::ffi::OsString::from(legacy_name);
    name.push(LEGACY_TRUST_SCOPE_SUFFIX);
    legacy_work_root.join(name)
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
        // Identical concrete roots need no seeding.
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

/// The GC lease scope of a trust-partitioned store, relative to its class
/// root. Trust identity is already part of the selected class root.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub class: String,
    pub path: PathBuf,
    pub bytes: u64,
}

#[cfg(unix)]
pub fn catalog(layout: &StorageLayout) -> Result<Vec<CatalogEntry>> {
    catalog_with_before_trust_root_open(layout, |_| {})
}

#[cfg(not(unix))]
pub fn catalog(_layout: &StorageLayout) -> Result<Vec<CatalogEntry>> {
    anyhow::bail!("storage catalog requires Unix descriptor-relative no-follow traversal")
}

#[cfg(unix)]
fn catalog_with_before_trust_root_open(
    layout: &StorageLayout,
    mut before_trust_root_open: impl FnMut(&Path),
) -> Result<Vec<CatalogEntry>> {
    let mut entries = Vec::new();
    let trust_root = crate::trust_scope::filesystem_key_namespace(&layout.cache_root);
    if let Some(trust_directory) = open_catalog_directory_for_layout(
        layout,
        &trust_root,
        "trust catalog root",
        &mut before_trust_root_open,
    )? {
        for trust_name in
            read_catalog_directory(&trust_directory, &trust_root, "trust catalog root")?
        {
            let trust_path = trust_root.join(&trust_name);
            let Some(trust) = open_catalog_directory_at(
                &trust_directory,
                &trust_name,
                &trust_path,
                "trust scope root",
                &mut |_| {},
            )?
            else {
                continue;
            };
            for class_name in read_catalog_directory(&trust, &trust_path, "trust scope root")? {
                let class_path = trust_path.join(&class_name);
                let Some(class) = open_catalog_directory_at(
                    &trust,
                    &class_name,
                    &class_path,
                    "cache class root",
                    &mut |_| {},
                )?
                else {
                    continue;
                };
                entries.push(CatalogEntry {
                    class: format!(
                        "{}/{}",
                        trust_name.to_string_lossy(),
                        class_name.to_string_lossy()
                    ),
                    bytes: size_open_directory(&class, &class_path, &mut |_| {})?,
                    path: class_path,
                });
            }
        }
    }

    let gha_cache_root = crate::store_catalog::gha_cache_root(layout);
    if let Some(gha_cache) = open_catalog_directory_for_layout(
        layout,
        &gha_cache_root,
        "GitHub Actions cache root",
        &mut |_| {},
    )? {
        entries.push(CatalogEntry {
            class: crate::store_catalog::StoreClass::GhaCache.to_string(),
            bytes: size_open_directory(&gha_cache, &gha_cache_root, &mut |_| {})?,
            path: gha_cache_root,
        });
    }

    entries.sort_by(|a, b| a.class.cmp(&b.class));
    Ok(entries)
}

#[cfg(unix)]
fn open_catalog_directory_path(
    path: &Path,
    description: &str,
    before_open: &mut impl FnMut(&Path),
) -> Result<Option<fs::File>> {
    let normalized_path = path;
    let mut current = fs::File::from(
        rustix::fs::openat(
            rustix::fs::CWD,
            if normalized_path.is_absolute() {
                Path::new("/")
            } else {
                Path::new(".")
            },
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .with_context(|| format!("open path anchor for {description}"))?,
    );
    let mut display_component = if normalized_path.is_absolute() {
        PathBuf::from("/")
    } else {
        PathBuf::new()
    };
    for component in normalized_path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                display_component.push(name);
                let Some(directory) = open_catalog_directory_at(
                    &current,
                    name,
                    &display_component,
                    description,
                    before_open,
                )?
                else {
                    return Ok(None);
                };
                current = directory;
            }
            Component::ParentDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "path for {description} is not normalized: {}",
                    path.display()
                );
            }
        }
    }
    Ok(Some(current))
}

#[cfg(unix)]
fn open_catalog_directory_for_layout(
    layout: &StorageLayout,
    path: &Path,
    description: &str,
    before_open: &mut impl FnMut(&Path),
) -> Result<Option<fs::File>> {
    let Some(configured_root) = layout.configured_root_path() else {
        anyhow::bail!("cannot establish configured root for {description}");
    };
    let relative = path.strip_prefix(&configured_root).with_context(|| {
        format!(
            "{description} {} is outside configured storage root {}",
            path.display(),
            configured_root.display()
        )
    })?;
    let Some(mut current) =
        open_configured_root_directory(&configured_root, description, before_open)?
    else {
        return Ok(None);
    };
    let mut display_component = configured_root.clone();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                display_component.push(name);
                let Some(directory) = open_catalog_directory_at(
                    &current,
                    name,
                    &display_component,
                    description,
                    before_open,
                )?
                else {
                    return Ok(None);
                };
                current = directory;
            }
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                anyhow::bail!("path for {description} escapes configured storage root");
            }
        }
    }
    Ok(Some(current))
}

/// Canonicalization selects the target of the operator-configured storage
/// root, preserving intentional aliases from the storage-root setting,
/// config-dir setting, or user default. This is the root-selection point: the
/// configured root is trusted input. The canonical path is then walked from
/// `/` using no-follow descriptor opens, so swaps after selection and all
/// generated descendants are rejected rather than followed.
#[cfg(unix)]
fn open_configured_root_directory(
    configured_root: &Path,
    description: &str,
    before_open: &mut impl FnMut(&Path),
) -> Result<Option<fs::File>> {
    let canonical_root = match fs::canonicalize(configured_root) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("resolve configured root for {description}"));
        }
    };
    if !canonical_root.is_absolute() {
        anyhow::bail!("configured root for {description} is not absolute");
    }
    open_catalog_directory_path(&canonical_root, description, before_open)
}

#[cfg(unix)]
fn open_catalog_directory_at(
    parent: impl AsFd,
    name: &std::ffi::OsStr,
    display_path: &Path,
    description: &str,
    before_open: &mut impl FnMut(&Path),
) -> Result<Option<fs::File>> {
    let metadata =
        match rustix::fs::statat(parent.as_fd(), name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(metadata) => metadata,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(std::io::Error::from(error))
                    .with_context(|| format!("inspect {description} {}", display_path.display()));
            }
        };
    match rustix::fs::FileType::from_raw_mode(metadata.st_mode) {
        rustix::fs::FileType::Symlink => {
            anyhow::bail!("{description} {} is a symlink", display_path.display());
        }
        rustix::fs::FileType::Directory => {}
        _ => anyhow::bail!(
            "{description} {} is not a directory",
            display_path.display()
        ),
    }

    before_open(display_path);
    let directory = match rustix::fs::openat(
        parent.as_fd(),
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(directory) => fs::File::from(directory),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(rustix::io::Errno::LOOP) | Err(rustix::io::Errno::NOTDIR) => {
            anyhow::bail!(
                "{description} {} changed during secure open",
                display_path.display()
            );
        }
        Err(error) => {
            return Err(std::io::Error::from(error))
                .with_context(|| format!("read {description} {}", display_path.display()));
        }
    };
    let opened = rustix::fs::fstat(&directory)
        .map_err(std::io::Error::from)
        .with_context(|| format!("inspect opened {description} {}", display_path.display()))?;
    if rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::Directory
        || opened.st_dev != metadata.st_dev
        || opened.st_ino != metadata.st_ino
    {
        anyhow::bail!(
            "{description} {} changed during secure open",
            display_path.display()
        );
    }
    Ok(Some(directory))
}

#[cfg(unix)]
fn read_catalog_directory(
    directory: &fs::File,
    display_path: &Path,
    description: &str,
) -> Result<Vec<std::ffi::OsString>> {
    let entries = rustix::fs::Dir::read_from(directory)
        .map_err(std::io::Error::from)
        .with_context(|| format!("read {description} {}", display_path.display()))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(std::io::Error::from)
            .with_context(|| format!("read {description} {}", display_path.display()))?;
        let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(unix)]
pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    dir_size_with_after_child_stat(path, |_| {})
}

#[cfg(not(unix))]
pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    anyhow::bail!(
        "storage catalog sizing requires Unix descriptor-relative no-follow traversal: {}",
        path.display()
    )
}

/// Size a tree through pinned directory descriptors. The hook runs after a
/// child was observed and before it is sized or opened as a directory;
/// production uses a no-op and tests use it to force the swap window.
#[cfg(unix)]
fn dir_size_with_after_child_stat(path: &Path, after_child_stat: impl FnMut(&Path)) -> Result<u64> {
    let layout = selected_or_resolved_layout().or_else(|| resolve_required_layout().ok());
    dir_size_with_layout_after_child_stat(path, layout.as_ref(), after_child_stat)
}

#[cfg(unix)]
fn dir_size_with_layout_after_child_stat(
    path: &Path,
    layout: Option<&StorageLayout>,
    mut after_child_stat: impl FnMut(&Path),
) -> Result<u64> {
    if let Some((configured_root, relative)) = configured_storage_path_for(path, layout) {
        let Some(root) = open_configured_root_directory(
            &configured_root,
            "directory for sizing",
            &mut after_child_stat,
        )?
        else {
            return Ok(0);
        };
        return dir_size_below_configured_root(
            &root,
            &configured_root,
            &relative,
            path,
            &mut after_child_stat,
        );
    }
    dir_size_from_unconfigured_path(path, &mut after_child_stat)
}

#[cfg(unix)]
fn configured_storage_path_for(
    path: &Path,
    layout: Option<&StorageLayout>,
) -> Option<(PathBuf, PathBuf)> {
    let layout = layout?;
    let configured_root = layout.configured_root_path()?;
    let relative = path.strip_prefix(&configured_root).ok()?.to_path_buf();
    Some((configured_root, relative))
}

#[cfg(unix)]
fn dir_size_below_configured_root(
    root: &fs::File,
    configured_root_path: &Path,
    relative: &Path,
    display_path: &Path,
    after_child_stat: &mut impl FnMut(&Path),
) -> Result<u64> {
    let mut names = Vec::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                anyhow::bail!(
                    "directory for sizing escapes configured storage root: {}",
                    display_path.display()
                );
            }
        }
    }
    let Some((name, parent_names)) = names.split_last() else {
        return size_open_directory(root, display_path, after_child_stat);
    };

    let mut parent = root
        .try_clone()
        .context("clone configured storage root descriptor for sizing")?;
    let mut component_path = configured_root_path.to_path_buf();
    for parent_name in parent_names {
        component_path.push(parent_name);
        let Some(directory) = open_catalog_directory_at(
            &parent,
            parent_name,
            &component_path,
            "directory for sizing",
            after_child_stat,
        )?
        else {
            return Ok(0);
        };
        parent = directory;
    }
    dir_size_entry_at(&parent, name, display_path, after_child_stat)
}

#[cfg(unix)]
fn dir_size_from_unconfigured_path(
    path: &Path,
    after_child_stat: &mut impl FnMut(&Path),
) -> Result<u64> {
    let normalized_path = path_without_trailing_slashes(path);
    if normalized_path.as_os_str().is_empty() {
        return Ok(0);
    }
    let Some(name) = normalized_path.file_name() else {
        let Some(directory) =
            open_catalog_directory_path(path, "directory for sizing", after_child_stat)?
        else {
            return Ok(0);
        };
        return size_open_directory(&directory, path, after_child_stat);
    };
    let parent_path = normalized_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let Some(parent) =
        open_catalog_directory_path(parent_path, "directory for sizing", after_child_stat)?
    else {
        return Ok(0);
    };
    dir_size_entry_at(&parent, name, path, after_child_stat)
}

#[cfg(unix)]
fn dir_size_entry_at(
    parent: &fs::File,
    name: &std::ffi::OsStr,
    display_path: &Path,
    after_child_stat: &mut impl FnMut(&Path),
) -> Result<u64> {
    let metadata = match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT) => return Ok(0),
        Err(error) => {
            return Err(std::io::Error::from(error))
                .with_context(|| format!("stat {}", display_path.display()));
        }
    };

    match rustix::fs::FileType::from_raw_mode(metadata.st_mode) {
        rustix::fs::FileType::RegularFile => {
            after_child_stat(display_path);
            Ok(file_size_from_stat(metadata.st_size))
        }
        rustix::fs::FileType::Directory => {
            after_child_stat(display_path);
            let directory = match rustix::fs::openat(
                parent,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            ) {
                Ok(directory) => fs::File::from(directory),
                // The root vanished, changed type, or became a symlink after
                // the no-follow stat. Keep the historical zero-size behavior.
                Err(rustix::io::Errno::NOENT)
                | Err(rustix::io::Errno::NOTDIR)
                | Err(rustix::io::Errno::LOOP) => return Ok(0),
                Err(error) => {
                    return Err(std::io::Error::from(error))
                        .with_context(|| format!("open directory {}", display_path.display()));
                }
            };
            let opened = rustix::fs::fstat(&directory)
                .map_err(std::io::Error::from)
                .with_context(|| format!("inspect opened directory {}", display_path.display()))?;
            if rustix::fs::FileType::from_raw_mode(opened.st_mode)
                != rustix::fs::FileType::Directory
                || opened.st_dev != metadata.st_dev
                || opened.st_ino != metadata.st_ino
            {
                return Ok(0);
            }
            size_open_directory(&directory, display_path, after_child_stat)
        }
        // Symlinks, sockets, devices, and other special files contribute no
        // bytes, matching the catalog's established behavior.
        _ => Ok(0),
    }
}

#[cfg(unix)]
fn size_open_directory(
    directory: &fs::File,
    display_path: &Path,
    after_child_stat: &mut impl FnMut(&Path),
) -> Result<u64> {
    let entries = rustix::fs::Dir::read_from(directory)
        .map_err(std::io::Error::from)
        .with_context(|| format!("read {}", display_path.display()))?;
    let mut total = 0;
    for entry in entries {
        let entry = entry
            .map_err(std::io::Error::from)
            .with_context(|| format!("read {}", display_path.display()))?;
        let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if name == "." || name == ".." {
            continue;
        }
        let child_path = display_path.join(&name);
        let metadata =
            match rustix::fs::statat(directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) => metadata,
                Err(rustix::io::Errno::NOENT) => continue,
                Err(error) => {
                    return Err(std::io::Error::from(error))
                        .with_context(|| format!("stat {}", child_path.display()));
                }
            };
        match rustix::fs::FileType::from_raw_mode(metadata.st_mode) {
            rustix::fs::FileType::RegularFile => {
                after_child_stat(&child_path);
                total =
                    checked_add_size(total, file_size_from_stat(metadata.st_size), &child_path)?;
            }
            rustix::fs::FileType::Directory => {
                after_child_stat(&child_path);
                let child = match rustix::fs::openat(
                    directory,
                    &name,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                ) {
                    Ok(child) => fs::File::from(child),
                    // A queued directory may disappear or become a symlink
                    // after statat. Skip it; O_NOFOLLOW prevents traversing
                    // the symlink target during that race.
                    Err(rustix::io::Errno::NOENT)
                    | Err(rustix::io::Errno::NOTDIR)
                    | Err(rustix::io::Errno::LOOP) => continue,
                    Err(error) => {
                        return Err(std::io::Error::from(error))
                            .with_context(|| format!("open directory {}", child_path.display()));
                    }
                };
                let opened = rustix::fs::fstat(&child)
                    .map_err(std::io::Error::from)
                    .with_context(|| {
                        format!("inspect opened directory {}", child_path.display())
                    })?;
                if rustix::fs::FileType::from_raw_mode(opened.st_mode)
                    != rustix::fs::FileType::Directory
                    || opened.st_dev != metadata.st_dev
                    || opened.st_ino != metadata.st_ino
                {
                    continue;
                }
                let child_total = size_open_directory(&child, &child_path, after_child_stat)?;
                total = checked_add_size(total, child_total, &child_path)?;
            }
            // A symlink or special file is never opened and adds no bytes.
            _ => {}
        }
    }
    Ok(total)
}

#[cfg(unix)]
fn checked_add_size(total: u64, next: u64, path: &Path) -> Result<u64> {
    total
        .checked_add(next)
        .with_context(|| format!("storage size overflows u64 while sizing {}", path.display()))
}

#[cfg(unix)]
fn path_without_trailing_slashes(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let length = if bytes.is_empty() {
        0
    } else {
        bytes
            .iter()
            .rposition(|byte| *byte != b'/')
            .map_or(1, |index| index + 1)
    };
    PathBuf::from(std::ffi::OsString::from_vec(bytes[..length].to_vec()))
}

#[cfg(unix)]
fn file_size_from_stat(size: i64) -> u64 {
    u64::try_from(size).unwrap_or_default()
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
        let expected_identity_root = if cfg!(target_os = "macos") {
            Path::new("/private/var/lib/velnor")
        } else {
            Path::new("/var/lib/velnor")
        };
        assert_eq!(layout.buildkit_identity_root(), expected_identity_root);
    }

    #[test]
    fn explicit_local_layout_matches_the_runner_slot_layout() {
        let config = Path::new("/config/slots/slot-2");
        let layout = StorageLayout::explicit_local(config);
        assert_eq!(layout.cache_root, config.join("cache"));
        assert_eq!(layout.lib_root, config);
        assert_eq!(layout.run_root, Path::new("/config/run"));
        assert_eq!(layout.log_root, config.join("logs"));
        assert_eq!(layout.mode, "explicit-config");
    }

    #[test]
    fn required_layout_resolver_uses_default_and_rejects_home_less_resolution() {
        let default_layout = StorageLayout::from_prefix(Path::new("/home/user/.velnor-store"));
        let resolved =
            resolve_required_layout_from(None, None, None, None, || Ok(default_layout.clone()))
                .unwrap();
        assert_eq!(resolved, default_layout);

        let missing = resolve_required_layout_from(None, None, None, None, || {
            anyhow::bail!("HOME is not set")
        });
        assert!(missing.is_err());

        let explicit = StorageLayout::from_prefix(Path::new("/var"));
        let resolved =
            resolve_required_layout_from(Some(explicit.clone()), None, None, None, || {
                anyhow::bail!("default must not be consulted")
            })
            .unwrap();
        assert_eq!(resolved, explicit);
        assert!(require_user_home(None).is_err());
        assert!(require_user_home(Some("/home/user".into())).is_ok());

        let default_root_called = std::cell::Cell::new(false);
        let no_home = StorageLayout::user_layout_from_home(None, || {
            default_root_called.set(true);
            PathBuf::from("/home/user/.velnor")
        });
        assert!(no_home.is_err());
        assert!(
            !default_root_called.get(),
            "HOME-less resolution must fail before selecting a root"
        );

        let default_root = PathBuf::from("/home/user/.velnor");
        let user_layout = StorageLayout::user_layout_from_home(Some("/home/user".into()), || {
            default_root.clone()
        })
        .unwrap();
        let mut expected_user_layout = StorageLayout::from_prefix(&default_root);
        expected_user_layout.mode = "user-storage-root";
        assert_eq!(user_layout, expected_user_layout);
    }

    #[test]
    fn required_layout_resolver_uses_storage_then_cli_then_config_env_before_user_default() {
        let selected = StorageLayout::from_prefix(Path::new("/selected"));
        let storage = StorageLayout::from_prefix(Path::new("/var"));
        let cli = StorageLayout::explicit_local(Path::new("/cli/config"));
        let config_env = StorageLayout::explicit_local(Path::new("/env/config"));
        let default = StorageLayout::explicit_local(Path::new("/home/user/config"));
        let no_home = || anyhow::bail!("HOME is not set");

        let resolved = resolve_required_layout_from(
            Some(selected.clone()),
            Some(storage.clone()),
            Some(cli.clone()),
            Some(config_env.clone()),
            no_home,
        )
        .unwrap();
        assert_eq!(resolved, selected);

        let resolved = resolve_required_layout_from(
            None,
            Some(storage.clone()),
            Some(cli.clone()),
            Some(config_env.clone()),
            no_home,
        )
        .unwrap();
        assert_eq!(resolved, storage);

        let resolved = resolve_required_layout_from(
            None,
            None,
            Some(cli.clone()),
            Some(config_env.clone()),
            no_home,
        )
        .unwrap();
        assert_eq!(resolved, cli);

        let resolved =
            resolve_required_layout_from(None, None, None, Some(config_env.clone()), no_home)
                .unwrap();
        assert_eq!(resolved, config_env);

        assert_eq!(
            resolve_required_layout_from(None, None, None, None, || Ok(default.clone())).unwrap(),
            default
        );
    }

    #[test]
    fn gc_lease_scope_is_relative_to_the_canonical_class_root() {
        let layout = StorageLayout::from_prefix(Path::new("/tmp/velnor-storage"));
        let class_root = layout.cache_class("pool/a", "caches");
        let store = class_root.join("octo_repo/playwright");

        assert_eq!(
            gc_scope_below_root(&store, &class_root).unwrap(),
            "octo_repo/playwright"
        );
        assert!(gc_scope_below_root(&store, &layout.cache_root).is_err());
    }

    #[test]
    fn macos_buildkit_identity_normalizes_only_the_var_component() {
        assert_eq!(
            normalize_buildkit_identity_root(Path::new("/var/lib/velnor"), true),
            Path::new("/private/var/lib/velnor")
        );
        assert_eq!(
            normalize_buildkit_identity_root(Path::new("/var"), true),
            Path::new("/private/var")
        );
        assert_eq!(
            normalize_buildkit_identity_root(Path::new("/various/lib/velnor"), true),
            Path::new("/various/lib/velnor")
        );
        assert_eq!(
            normalize_buildkit_identity_root(Path::new("/var/lib/velnor"), false),
            Path::new("/var/lib/velnor")
        );
    }

    #[test]
    fn slot_looking_standalone_config_keeps_its_own_identity_root() {
        let layout = StorageLayout {
            cache_root: PathBuf::from("/config/slots/slot-2/cache"),
            lib_root: PathBuf::from("/config/slots/slot-2"),
            run_root: PathBuf::from("/config/run"),
            log_root: PathBuf::from("/config/slots/slot-2/logs"),
            mode: "explicit-config",
        };
        assert_eq!(
            layout.buildkit_identity_root(),
            PathBuf::from("/config/slots/slot-2")
        );

        let selected = SelectedRunnerStorageLayout {
            layout: layout.clone(),
            buildkit_identity_root: Some(PathBuf::from("/config")),
        };
        assert_eq!(
            selected_buildkit_identity_root(Some(&selected), &layout),
            PathBuf::from("/config")
        );

        let lookalike = StorageLayout {
            cache_root: PathBuf::from("/config/slots/slot-3/cache"),
            lib_root: PathBuf::from("/config/slots/slot-3"),
            run_root: PathBuf::from("/config/run"),
            log_root: PathBuf::from("/config/slots/slot-3/logs"),
            mode: "explicit-config",
        };
        assert_eq!(
            selected_buildkit_identity_root(Some(&selected), &lookalike),
            PathBuf::from("/config/slots/slot-3")
        );
    }

    #[cfg(unix)]
    #[test]
    fn relative_and_parented_buildkit_identity_roots_fail_before_creation() {
        let relative = PathBuf::from(format!(
            ".velnor-relative-buildkit-identity-{}",
            uuid::Uuid::new_v4()
        ));
        assert!(ensure_buildkit_storage_identity(&relative).is_err());
        assert!(!relative.exists());

        let base = seed_root("buildkit-identity-parent-component");
        let parented = base.join("must-not-create").join("..").join("identity");
        assert!(ensure_buildkit_storage_identity(&parented).is_err());
        assert!(!base.join("must-not-create").exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn buildkit_storage_identity_is_stable_and_scoped_by_root() {
        let first = seed_root("buildkit-identity-first");
        let second = seed_root("buildkit-identity-second");

        let first_id = ensure_buildkit_storage_identity(&first).unwrap();
        assert_eq!(ensure_buildkit_storage_identity(&first).unwrap(), first_id);
        assert_ne!(ensure_buildkit_storage_identity(&second).unwrap(), first_id);

        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn buildkit_storage_identity_creates_missing_root_without_following_symlinks() {
        use std::os::unix::fs::symlink;

        let base = seed_root("buildkit-identity-missing-root");
        let missing = base.join("created").join("lib");
        let id = ensure_buildkit_storage_identity(&missing).unwrap();
        assert_eq!(ensure_buildkit_storage_identity(&missing).unwrap(), id);
        assert!(missing.join(BUILDKIT_STORAGE_ID_FILE).is_file());

        let target = base.join("target");
        fs::create_dir(&target).unwrap();
        let linked = base.join("linked");
        symlink(&target, &linked).unwrap();
        assert!(ensure_buildkit_storage_identity(&linked).is_err());
        assert!(!target.join(BUILDKIT_STORAGE_ID_FILE).exists());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_buildkit_identity_initializers_share_one_uuid() {
        let root = seed_root("buildkit-identity-concurrent");
        let workers = 12;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(workers));
        let handles = (0..workers)
            .map(|_| {
                let root = root.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    ensure_buildkit_storage_identity(&root).unwrap()
                })
            })
            .collect::<Vec<_>>();
        let ids = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 1, "concurrent initializers diverged: {ids:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn malformed_or_unsafe_buildkit_identity_fails_closed() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let malformed_root = seed_root("buildkit-identity-malformed");
        let malformed_identity = malformed_root.join(BUILDKIT_STORAGE_ID_FILE);
        fs::write(&malformed_identity, b"not-a-uuid\n").unwrap();
        fs::set_permissions(&malformed_identity, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(ensure_buildkit_storage_identity(&malformed_root).is_err());
        assert_eq!(fs::read(malformed_identity).unwrap(), b"not-a-uuid\n");
        fs::remove_dir_all(malformed_root).unwrap();

        let symlink_root = seed_root("buildkit-identity-symlink");
        let outside = symlink_root.join("outside");
        fs::write(&outside, b"keep me").unwrap();
        symlink(&outside, symlink_root.join(BUILDKIT_STORAGE_ID_FILE)).unwrap();
        assert!(ensure_buildkit_storage_identity(&symlink_root).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"keep me");
        fs::remove_dir_all(symlink_root).unwrap();

        let fifo_root = seed_root("buildkit-identity-fifo");
        create_fifo(&fifo_root.join(BUILDKIT_STORAGE_ID_FILE));
        assert!(ensure_buildkit_storage_identity(&fifo_root).is_err());
        fs::remove_dir_all(fifo_root).unwrap();

        let lock_symlink_root = seed_root("buildkit-identity-lock-symlink");
        let lock_outside = lock_symlink_root.join("outside-lock");
        fs::write(&lock_outside, b"keep lock target").unwrap();
        symlink(
            &lock_outside,
            lock_symlink_root.join(BUILDKIT_STORAGE_ID_LOCK_FILE),
        )
        .unwrap();
        assert!(ensure_buildkit_storage_identity(&lock_symlink_root).is_err());
        assert_eq!(fs::read(lock_outside).unwrap(), b"keep lock target");
        fs::remove_dir_all(lock_symlink_root).unwrap();

        let lock_fifo_root = seed_root("buildkit-identity-lock-fifo");
        create_fifo(&lock_fifo_root.join(BUILDKIT_STORAGE_ID_LOCK_FILE));
        assert!(ensure_buildkit_storage_identity(&lock_fifo_root).is_err());
        fs::remove_dir_all(lock_fifo_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn linked_or_permissive_buildkit_identity_fails_closed() {
        use std::os::unix::fs::PermissionsExt;

        let linked_root = seed_root("buildkit-identity-hardlink");
        let linked_identity = linked_root.join(BUILDKIT_STORAGE_ID_FILE);
        fs::write(
            &linked_identity,
            format!("{}\n", uuid::Uuid::new_v4().hyphenated()),
        )
        .unwrap();
        fs::set_permissions(&linked_identity, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&linked_identity, linked_root.join("identity-copy")).unwrap();
        assert!(ensure_buildkit_storage_identity(&linked_root).is_err());
        fs::remove_dir_all(linked_root).unwrap();

        let permissive_root = seed_root("buildkit-identity-permissive");
        let permissive_identity = permissive_root.join(BUILDKIT_STORAGE_ID_FILE);
        fs::write(
            &permissive_identity,
            format!("{}\n", uuid::Uuid::new_v4().hyphenated()),
        )
        .unwrap();
        fs::set_permissions(&permissive_identity, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(ensure_buildkit_storage_identity(&permissive_root).is_err());
        fs::remove_dir_all(permissive_root).unwrap();
    }

    #[test]
    fn canonical_layout_never_falls_back_to_versioned_legacy_stores() {
        let root = seed_root("canonical-no-legacy-fallback");
        let work_root = root.join("work");
        let layout = StorageLayout::from_prefix(&root.join("canonical"));
        let scope = "pool/a";
        let trust_key = crate::trust_scope::filesystem_key(scope);

        let legacy_plain = work_root.join("_velnor_mbx__trust_scope_v1");
        let legacy_trust = legacy_plain.join(&trust_key);
        fs::create_dir_all(&legacy_plain).unwrap();
        fs::create_dir_all(&legacy_trust).unwrap();

        let plain = cache_class_path_with_layout(scope, "compiler/mbx", Some(&layout)).unwrap();
        let trust_specific = plain.clone();

        assert_eq!(plain, layout.cache_class(scope, "compiler/mbx"));
        assert_eq!(trust_specific, layout.cache_class(scope, "compiler/mbx"));
        assert_ne!(plain, legacy_plain);
        assert_ne!(trust_specific, legacy_trust);
        assert!(!plain.exists());
        assert!(!trust_specific.exists());
        assert!(legacy_plain.is_dir());
        assert!(legacy_trust.is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    fn seed_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-seed-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::canonicalize(root).unwrap()
    }

    #[cfg(unix)]
    fn create_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt as _;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `path` is a valid NUL-terminated path and mkfifo only creates
        // one test fixture entry with the requested mode.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
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

    #[cfg(unix)]
    #[test]
    fn catalog_reports_class_bytes() {
        let root = std::env::temp_dir().join(format!("velnor-catalog-{}", uuid::Uuid::new_v4()));
        let layout = StorageLayout::from_prefix(&root);
        let trust_key = crate::trust_scope::filesystem_key("trusted");
        let class = layout.cache_class("trusted", "targets");
        fs::create_dir_all(&class).unwrap();
        fs::write(class.join("artifact"), b"1234").unwrap();
        let git_mirrors = crate::store_catalog::StoreCatalog::git_mirrors_root(&layout, "trusted");
        fs::create_dir_all(git_mirrors.join("repo-key-v1-test")).unwrap();
        fs::write(git_mirrors.join("repo-key-v1-test/objects"), b"mirror").unwrap();
        let entries = catalog(&layout).unwrap();
        let targets = entries
            .iter()
            .find(|entry| entry.class == format!("{trust_key}/targets"))
            .unwrap();
        assert_eq!(targets.bytes, 4);
        let mirrors = entries
            .iter()
            .find(|entry| entry.class == format!("{trust_key}/git-mirrors"))
            .unwrap();
        assert_eq!(mirrors.bytes, 6);
        assert_eq!(mirrors.path, git_mirrors);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn catalog_reports_full_github_actions_cache_root_for_explicit_config() {
        let root =
            std::env::temp_dir().join(format!("velnor-catalog-gha-{}", uuid::Uuid::new_v4()));
        let config = root.join("config");
        let layout = StorageLayout::explicit_local(&config);
        let gha_cache_root = crate::store_catalog::gha_cache_root(&layout);
        let tenant_blob = gha_cache_root.join("tenants/tenant-a/blobs/cache-blob");
        let entry_lock = gha_cache_root.join("entry-locks/001.lock");
        fs::create_dir_all(tenant_blob.parent().unwrap()).unwrap();
        fs::create_dir_all(entry_lock.parent().unwrap()).unwrap();
        fs::write(&tenant_blob, b"tenant-data").unwrap();
        fs::write(&entry_lock, b"stable-lock-shard").unwrap();

        let entries = catalog(&layout).unwrap();
        assert_eq!(entries.len(), 1);
        let gha_cache = entries
            .iter()
            .find(|entry| entry.class == crate::store_catalog::StoreClass::GhaCache.to_string())
            .unwrap();
        assert_eq!(gha_cache.path, gha_cache_root);
        assert_eq!(
            gha_cache.bytes,
            b"tenant-data".len() as u64 + b"stable-lock-shard".len() as u64
        );
        assert_eq!(gha_cache.path, config.join("cache/gha-cache"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn configured_root_symlink_alias_is_allowed_but_descendants_stay_pinned() {
        use std::os::unix::fs::symlink;

        let root = seed_root("configured-root-alias");
        let configured_root = root.join("storage");
        let alias = root.with_file_name(format!(
            "{}-alias",
            root.file_name().unwrap().to_string_lossy()
        ));
        fs::create_dir(&configured_root).unwrap();
        symlink(&configured_root, &alias).unwrap();

        let layout = StorageLayout::from_prefix(&alias);
        let class = layout.cache_class("trusted", "targets");
        fs::create_dir_all(&class).unwrap();
        fs::write(class.join("artifact"), b"configured-root-data").unwrap();

        let entries = catalog(&layout).unwrap();
        let trust_key = crate::trust_scope::filesystem_key("trusted");
        let targets = entries
            .iter()
            .find(|entry| entry.class == format!("{trust_key}/targets"))
            .unwrap();
        assert_eq!(targets.bytes, b"configured-root-data".len() as u64);
        assert_eq!(
            dir_size_with_layout_after_child_stat(&class, Some(&layout), |_| {}).unwrap(),
            b"configured-root-data".len() as u64
        );

        fs::remove_file(&alias).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_keeps_static_symlinks_out_of_the_count() {
        use std::os::unix::fs::symlink;

        let root = seed_root("dir-size-static-symlink");
        let external = root.join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("canary"), b"outside-bytes").unwrap();
        let linked_root = root.join("linked-root");
        symlink(&external, &linked_root).unwrap();

        assert_eq!(dir_size(&linked_root).unwrap(), 0);

        let tree = root.join("tree");
        fs::create_dir(&tree).unwrap();
        fs::write(tree.join("inside"), b"safe").unwrap();
        symlink(&external, tree.join("linked-child")).unwrap();
        assert_eq!(dir_size(&tree).unwrap(), 4);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_does_not_follow_directory_swapped_for_symlink_before_open() {
        use std::os::unix::fs::symlink;

        let root = seed_root("dir-size-symlink-race");
        let tree = root.join("tree");
        let queued = tree.join("queued");
        let outside = root.join("outside");
        fs::create_dir_all(&queued).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("canary"), b"must-never-count").unwrap();

        let swapped = std::cell::Cell::new(false);
        let bytes = dir_size_with_after_child_stat(&tree, |child_path| {
            if child_path == queued.as_path() && !swapped.replace(true) {
                fs::rename(&queued, tree.join("queued-original")).unwrap();
                symlink(&outside, &queued).unwrap();
            }
        })
        .unwrap();

        assert!(swapped.get(), "the queued directory was not visited");
        assert_eq!(bytes, 0, "symlink target canary must not be counted");

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_uses_the_observed_regular_file_stat_after_symlink_swap() {
        use std::os::unix::fs::symlink;

        let root = seed_root("dir-size-file-symlink-race");
        let tree = root.join("tree");
        let queued = tree.join("queued");
        let canary = root.join("canary");
        fs::create_dir(&tree).unwrap();
        fs::write(&queued, b"small").unwrap();
        fs::write(&canary, vec![b'x'; 4_096]).unwrap();

        let swapped = std::cell::Cell::new(false);
        let bytes = dir_size_with_after_child_stat(&tree, |child_path| {
            if child_path == queued.as_path() && !swapped.replace(true) {
                fs::rename(&queued, tree.join("queued-original")).unwrap();
                symlink(&canary, &queued).unwrap();
            }
        })
        .unwrap();

        assert!(swapped.get(), "the queued file was not visited");
        assert_eq!(bytes, b"small".len() as u64);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_does_not_open_file_swapped_for_fifo_after_stat() {
        let root = seed_root("dir-size-file-fifo-race");
        let tree = root.join("tree");
        let queued = tree.join("queued");
        fs::create_dir(&tree).unwrap();
        fs::write(&queued, b"safe").unwrap();

        let swapped = std::cell::Cell::new(false);
        let bytes = dir_size_with_after_child_stat(&tree, |child_path| {
            if child_path == queued.as_path() && !swapped.replace(true) {
                fs::rename(&queued, tree.join("queued-original")).unwrap();
                create_fifo(&queued);
            }
        })
        .unwrap();

        assert!(swapped.get(), "the queued file was not visited");
        assert_eq!(bytes, b"safe".len() as u64);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_does_not_follow_ancestor_swapped_for_symlink_after_root_selection() {
        use std::os::unix::fs::symlink;

        let root = seed_root("dir-size-ancestor-symlink-race");
        let tree = root.join("tree");
        let external = root.with_file_name(format!(
            "{}-external",
            root.file_name().unwrap().to_string_lossy()
        ));
        let original = root.with_file_name(format!(
            "{}-original",
            root.file_name().unwrap().to_string_lossy()
        ));
        let selected_root = fs::canonicalize(&root).unwrap();
        fs::create_dir(&tree).unwrap();
        fs::create_dir(&external).unwrap();
        fs::write(external.join("canary"), vec![b'x'; 4_096]).unwrap();

        let swapped = std::cell::Cell::new(false);
        let error = dir_size_with_after_child_stat(&tree, |component_path| {
            if component_path == selected_root.as_path() && !swapped.replace(true) {
                fs::rename(&root, &original).unwrap();
                symlink(&external, &root).unwrap();
            }
        })
        .expect_err("ancestor swap must stop before the outside canary is sized");

        assert!(swapped.get(), "the selected ancestor was not visited");
        assert!(
            format!("{error:#}").contains("changed during secure open"),
            "unexpected failure before the no-follow open: {error:#}"
        );

        fs::remove_file(&root).unwrap();
        fs::remove_dir_all(original).unwrap();
        fs::remove_dir_all(external).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_ignores_current_dir_and_rejects_parent_components() {
        let root = seed_root("dir-size-dot-components");
        let tree = root.join("tree");
        fs::create_dir(&tree).unwrap();
        fs::write(tree.join("artifact"), b"safe").unwrap();

        assert_eq!(dir_size(&root.join("./tree")).unwrap(), 4);
        assert!(dir_size(&root.join("tree/../tree")).is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_rejects_a_symlinked_parent_outside_a_selected_layout() {
        use std::os::unix::fs::symlink;

        let root = seed_root("dir-size-symlink-parent");
        let external = root.with_file_name(format!(
            "{}-external",
            root.file_name().unwrap().to_string_lossy()
        ));
        let linked_parent = root.join("linked-parent");
        fs::create_dir_all(external.join("cargo")).unwrap();
        fs::write(external.join("cargo/canary"), vec![b'x'; 4_096]).unwrap();
        symlink(&external, &linked_parent).unwrap();

        assert!(dir_size(&linked_parent.join("cargo")).is_err());

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(external).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn storage_size_addition_rejects_u64_overflow() {
        let error = checked_add_size(u64::MAX, 1, Path::new("tree/child"))
            .expect_err("byte totals must not wrap");

        assert!(format!("{error:#}").contains("storage size overflows u64"));
    }

    #[cfg(unix)]
    #[test]
    fn catalog_rejects_symlink_roots_and_does_not_size_linked_directories() {
        use std::os::unix::fs::symlink;

        let root = seed_root("catalog-symlinks");
        let layout = StorageLayout::from_prefix(&root.join("storage"));
        let trust_root = crate::trust_scope::filesystem_key_namespace(&layout.cache_root);
        let external = root.join("external");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("outside"), b"outside-bytes").unwrap();

        fs::create_dir_all(trust_root.parent().unwrap()).unwrap();
        symlink(&external, &trust_root).unwrap();
        assert!(
            catalog(&layout).is_err(),
            "trust root symlink must fail closed"
        );
        fs::remove_file(&trust_root).unwrap();

        fs::create_dir_all(&trust_root).unwrap();
        let trust = trust_root.join(crate::trust_scope::filesystem_key("trusted"));
        fs::create_dir_all(&trust).unwrap();
        let class = layout.cache_class("trusted", "targets");
        symlink(&external, &class).unwrap();
        assert!(
            catalog(&layout).is_err(),
            "class root symlink must fail closed"
        );
        fs::remove_file(&class).unwrap();

        fs::create_dir_all(&class).unwrap();
        fs::write(class.join("artifact"), b"1234").unwrap();
        symlink(&external, class.join("linked-dir")).unwrap();
        let entries = catalog(&layout).unwrap();
        let targets = entries
            .iter()
            .find(|entry| {
                entry.class == format!("{}/targets", trust.file_name().unwrap().to_string_lossy())
            })
            .unwrap();
        assert_eq!(
            targets.bytes, 4,
            "directory symlink target must not be sized"
        );

        let gha_cache_root = crate::store_catalog::gha_cache_root(&layout);
        fs::create_dir_all(gha_cache_root.parent().unwrap()).unwrap();
        symlink(&external, &gha_cache_root).unwrap();
        assert!(
            catalog(&layout).is_err(),
            "GHA cache root symlink must fail closed"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn catalog_does_not_follow_trust_root_swapped_for_symlink_before_open() {
        use std::os::unix::fs::symlink;

        let root = seed_root("catalog-trust-root-symlink-race");
        let layout = StorageLayout::from_prefix(&root.join("storage"));
        let trust_root = crate::trust_scope::filesystem_key_namespace(&layout.cache_root);
        let external = root.join("external");
        let canary = external
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join("targets/canary");
        fs::create_dir_all(canary.parent().unwrap()).unwrap();
        fs::write(&canary, b"must-never-be-cataloged").unwrap();
        fs::create_dir_all(&trust_root).unwrap();

        let swapped = std::cell::Cell::new(false);
        let entries = catalog_with_before_trust_root_open(&layout, |path| {
            if path == trust_root.as_path() && !swapped.replace(true) {
                fs::rename(
                    &trust_root,
                    trust_root.with_file_name("trust-scopes-original"),
                )
                .unwrap();
                symlink(&external, &trust_root).unwrap();
            }
        });

        assert!(swapped.get(), "trust root was not visited");
        let error = entries.expect_err("catalog must stop before enumerating the canary tree");
        assert!(
            format!("{error:#}").contains("changed during secure open"),
            "unexpected failure before the no-follow open: {error:#}"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn catalog_rejects_symlinked_generated_ancestor_below_configured_root() {
        use std::os::unix::fs::symlink;

        let root = seed_root("catalog-generated-ancestor-symlink");
        let configured_root = root.join("storage");
        let layout = StorageLayout::from_prefix(&configured_root);
        let external_cache = root.join("external/cache");
        let canary = external_cache
            .join("velnor/v1__trust_scope_v1")
            .join(crate::trust_scope::filesystem_key("trusted"))
            .join("targets/canary");
        fs::create_dir_all(canary.parent().unwrap()).unwrap();
        fs::create_dir_all(&configured_root).unwrap();
        fs::write(&canary, b"must-never-be-cataloged").unwrap();
        symlink(&external_cache, configured_root.join("cache")).unwrap();

        let error =
            catalog(&layout).expect_err("generated cache ancestor symlink must fail closed");
        assert!(
            format!("{error:#}").contains("is a symlink"),
            "unexpected failure before rejecting the generated symlink: {error:#}"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn catalog_rejects_a_non_directory_trust_root() {
        let root = seed_root("catalog-non-directory");
        let layout = StorageLayout::from_prefix(&root.join("storage"));
        let trust_root = crate::trust_scope::filesystem_key_namespace(&layout.cache_root);
        fs::create_dir_all(trust_root.parent().unwrap()).unwrap();
        fs::write(&trust_root, b"not a directory").unwrap();

        assert!(catalog(&layout).is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn trust_store_paths_use_distinct_keys_in_the_selected_layout() {
        let root = seed_root("trust-keys");
        let layout = StorageLayout::from_prefix(&root.join("canonical"));
        let scopes = [
            "pool/a".to_owned(),
            "pool_a".to_owned(),
            "Pool_A".to_owned(),
            format!("{}a", "x".repeat(160)),
            format!("{}b", "x".repeat(160)),
        ];

        let canonical_roots = scopes
            .iter()
            .map(|scope| layout.cache_class(scope, "compiler/mbx"))
            .collect::<Vec<_>>();
        let resolved_roots = scopes
            .iter()
            .map(|scope| {
                cache_class_path_with_layout(scope, "compiler/mbx", Some(&layout)).unwrap()
            })
            .collect::<Vec<_>>();

        for index in 0..scopes.len() {
            for other in index + 1..scopes.len() {
                assert_ne!(canonical_roots[index], canonical_roots[other]);
                assert_ne!(resolved_roots[index], resolved_roots[other]);
            }
            assert_eq!(
                canonical_roots[index],
                crate::trust_scope::filesystem_key_path(&layout.cache_root, &scopes[index])
                    .join("compiler/mbx")
            );
            assert_eq!(resolved_roots[index], canonical_roots[index]);
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_scope_equal_to_another_scope_key_cannot_reuse_old_layout_data() {
        let root = seed_root("scope-key-alias");
        let layout = StorageLayout::from_prefix(&root.join("canonical"));
        let work_root = root.join("work");
        let raw_scope = crate::trust_scope::filesystem_key("pool/a");
        let old_component = crate::container::sanitize_store_key(&raw_scope);
        let old_canonical = layout
            .cache_root
            .join(&old_component)
            .join("compiler/mbx/42");
        let old_legacy = work_root
            .join("_velnor_mbx")
            .join(&old_component)
            .join("42");
        fs::create_dir_all(&old_canonical).unwrap();
        fs::write(old_canonical.join("ambiguous.rlib"), b"old canonical data").unwrap();
        fs::create_dir_all(&old_legacy).unwrap();
        fs::write(old_legacy.join("ambiguous.rlib"), b"old legacy data").unwrap();

        let canonical = layout.cache_class(&raw_scope, "compiler/mbx").join("42");
        let resolved = cache_class_path_with_layout(&raw_scope, "compiler/mbx", Some(&layout))
            .unwrap()
            .join("42");

        assert_eq!(old_component, raw_scope);
        assert_ne!(canonical, old_canonical);
        assert_eq!(resolved, canonical);
        assert_ne!(resolved, old_legacy);
        assert!(!canonical.starts_with(&old_canonical));
        assert!(!old_canonical.starts_with(&canonical));
        assert!(!resolved.starts_with(&old_legacy));
        assert!(!old_legacy.starts_with(&resolved));
        assert!(!canonical.exists());
        assert!(!resolved.exists());
        assert_eq!(
            fs::read(old_canonical.join("ambiguous.rlib")).unwrap(),
            b"old canonical data"
        );
        assert_eq!(
            fs::read(old_legacy.join("ambiguous.rlib")).unwrap(),
            b"old legacy data"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
