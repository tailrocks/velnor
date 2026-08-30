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

pub fn cache_class_path(legacy_work_root: &Path, class: &str, legacy_name: &str) -> PathBuf {
    let legacy = legacy_work_root.join(legacy_name);
    let Some(layout) = StorageLayout::resolve() else {
        return legacy;
    };
    let canonical = layout.cache_class(&crate::github_adapter::cargo_target_trust_scope(), class);
    prefer_canonical_or_existing_legacy(canonical, legacy)
}

/// Resolve a trust-scoped cache path without consulting process-global trust
/// state. Compiler caches use this boundary because the admitted trust class
/// belongs to the job plan, while the other stores retain [`cache_class_path`]
/// and their existing operator-selected scope.
pub fn cache_class_path_for_trust(
    legacy_work_root: &Path,
    trust_scope: &str,
    class: &str,
    legacy_name: &str,
) -> PathBuf {
    let trust = crate::container::sanitize_store_key(trust_scope);
    let legacy = legacy_work_root.join(legacy_name).join(&trust);
    let Some(layout) = StorageLayout::resolve() else {
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

fn dir_size(path: &Path) -> Result<u64> {
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
