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

/// One read-through store layer for the Docker backend (D18).
///
/// `lower` is the trusted scope's store, read beneath `upper`, the PR scope's
/// store that receives every write. The layer is an overlay the *daemon*
/// mounts — a `local` volume with `type=overlay` — so the runner needs no
/// `CAP_SYS_ADMIN` of its own and a VM-hosted daemon (OrbStack, Docker
/// Desktop) mounts it inside the VM kernel that also runs the job. The job
/// container sees one merged tree at `target`; the host still sees `upper`
/// as the plain PR-scope directory the storage catalog, leases and GC know.
///
/// Whether the daemon can back an overlay upper on the shared store
/// filesystem is a probed capability
/// ([`crate::execution::store_overlay_support`]); admission only asks for a
/// layer when the probe passed, so a failure to mount one at job start is a
/// job error, never a silent fallback to `upper` alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreOverlay {
    /// Daemon volume name; carries the job-id label so job-owned reclaim
    /// removes it with the rest of the job's Docker resources.
    pub volume: String,
    /// Host-visible trusted store directory read beneath `upper`.
    pub lower: PathBuf,
    /// Host-visible PR-scope store directory that receives the writes.
    pub upper: PathBuf,
    /// overlayfs work directory: same filesystem as `upper`, outside every
    /// mounted subtree, owned by this job alone.
    pub work: PathBuf,
    /// Container path the merged tree is mounted at.
    pub target: String,
}

/// Directory under a store root that holds the per-job overlay work
/// directories. Never a mount source or a GC root, so it neither leaks into a
/// container nor counts as a reclaimable store scope.
pub const OVERLAY_WORK_DIR: &str = ".velnor-overlay-work";

impl StoreOverlay {
    /// The overlay layers of the Cargo store for one job: every daemon-shared
    /// Cargo subtree gets `upper_scope` layered over `lower_scope`.
    ///
    /// Pure path arithmetic; the executor creates the directories and the
    /// volume when the job starts.
    #[must_use]
    pub fn cargo_layers(
        container_name: &str,
        temp_host: &Path,
        upper_scope: &str,
        lower_scope: &str,
    ) -> Vec<Self> {
        let upper_root = crate::container::cargo_store_host(temp_host, upper_scope);
        let lower_root = crate::container::cargo_store_host(temp_host, lower_scope);
        let work_root = upper_root
            .join(OVERLAY_WORK_DIR)
            .join(crate::container::sanitize_store_key(container_name));
        crate::container::CARGO_STORE_LAYERS
            .iter()
            .map(|(subpath, target)| {
                let key = crate::container::sanitize_store_key(subpath);
                Self {
                    volume: format!("{container_name}-overlay-cargo-{key}"),
                    lower: lower_root.join(subpath),
                    upper: upper_root.join(subpath),
                    work: work_root.join(key),
                    target: (*target).to_owned(),
                }
            })
            .collect()
    }

    /// `docker volume create` argv for this layer. `daemon_visible` maps a
    /// host-visible path to the path the daemon mounts (identity on a native
    /// Linux daemon; the VM mapping for Docker Desktop/OrbStack).
    ///
    /// # Errors
    /// A layer path could not be mapped into the daemon's view.
    pub fn create_volume_args<F>(
        &self,
        labels: &[(&str, &str)],
        mut daemon_visible: F,
    ) -> std::io::Result<Vec<String>>
    where
        F: FnMut(&Path, &str) -> std::io::Result<PathBuf>,
    {
        let lower = daemon_visible(&self.lower, "store overlay lower")?;
        let upper = daemon_visible(&self.upper, "store overlay upper")?;
        let work = daemon_visible(&self.work, "store overlay work")?;
        let mut args = vec![
            "volume".to_owned(),
            "create".to_owned(),
            "--driver".to_owned(),
            "local".to_owned(),
        ];
        for (key, value) in labels {
            args.push("--label".to_owned());
            args.push(format!("{key}={value}"));
        }
        args.extend([
            "--opt".to_owned(),
            "type=overlay".to_owned(),
            "--opt".to_owned(),
            "device=overlay".to_owned(),
            "--opt".to_owned(),
            format!("o={}", overlay_mount_options(&lower, &upper, &work)),
            self.volume.clone(),
        ]);
        Ok(args)
    }

    /// The `-v` operand that mounts the merged tree into the job container.
    #[must_use]
    pub fn mount_operand(&self) -> String {
        format!("{}:{}", self.volume, self.target)
    }
}

/// overlayfs `-o` options for one lower/upper/work triple.
#[must_use]
pub fn overlay_mount_options(lower: &Path, upper: &Path, work: &Path) -> String {
    format!(
        "lowerdir={},upperdir={},workdir={}",
        lower.display(),
        upper.display(),
        work.display()
    )
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
