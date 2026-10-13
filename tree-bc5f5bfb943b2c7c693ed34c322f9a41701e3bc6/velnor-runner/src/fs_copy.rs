use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

#[cfg(unix)]
use std::{
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    path::{Component, PathBuf},
};

use anyhow::{bail, Context, Result};

const MAX_SECURE_CLEANUP_DEPTH: usize = 256;

#[cfg(unix)]
const TEMPORARY_FILE_IN_STAGING_DIRECTORY: &str = "payload";

#[derive(Debug)]
pub(crate) enum NoFollowSource {
    File(fs::File),
    Directory(NoFollowDir),
}

#[derive(Debug)]
pub(crate) struct NoFollowDirEntry {
    pub name: OsString,
    pub source: NoFollowSource,
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct NoFollowDir {
    file: fs::File,
    display_path: PathBuf,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct NoFollowDir;

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct NoFollowDestinationDir {
    file: fs::File,
    display_path: PathBuf,
    staging_parent: fs::File,
    staging_parent_path: PathBuf,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct NoFollowDestinationDir;

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct StagedFile {
    file: fs::File,
    staging_parent: NoFollowDestinationDir,
    staging_directory: NoFollowDestinationDir,
    staging_name: OsString,
    destination_parent: NoFollowDestinationDir,
}

#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct StagedFile;

#[cfg(unix)]
impl NoFollowDir {
    pub fn open_absolute(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            bail!(
                "approved artifact source root must be absolute: {}",
                path.display()
            );
        }

        let root = rustix::fs::openat(
            rustix::fs::CWD,
            Path::new("/"),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)
        .context("open filesystem root for artifact source")?;
        let mut current = Self {
            file: root.into(),
            display_path: PathBuf::from("/"),
        };

        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => {
                    let display_path = current.display_path.join(name);
                    current = match current.open_entry(name)? {
                        Some(NoFollowSource::Directory(directory)) => directory,
                        Some(NoFollowSource::File(_)) => bail!(
                            "approved artifact source root has a non-directory ancestor: {}",
                            display_path.display()
                        ),
                        None => bail!(
                            "approved artifact source root does not exist: {}",
                            display_path.display()
                        ),
                    };
                }
                Component::ParentDir | Component::Prefix(_) => bail!(
                    "approved artifact source root is not normalized: {}",
                    path.display()
                ),
            }
        }
        Ok(current)
    }

    /// Opens a trusted root from daemon configuration after resolving host aliases.
    ///
    /// This is only for a root whose complete path was already admitted by trusted
    /// configuration, such as macOS `/var`. Workflow-provided paths must use
    /// [`Self::open_absolute`] or [`Self::open_source`] so their symlinks are never
    /// followed.
    pub fn open_trusted_configured_root(configured_root: &Path) -> Result<Self> {
        if !configured_root.is_absolute() {
            bail!(
                "trusted configured artifact source root must be absolute: {}",
                configured_root.display()
            );
        }

        let canonical_root = fs::canonicalize(configured_root).with_context(|| {
            format!(
                "canonicalize trusted configured artifact source root {}",
                configured_root.display()
            )
        })?;
        Self::open_absolute(&canonical_root).with_context(|| {
            format!(
                "securely open canonical trusted configured artifact source root {}",
                canonical_root.display()
            )
        })
    }

    pub fn open_source(&self, relative: &Path) -> Result<Option<NoFollowSource>> {
        let mut components = relative
            .components()
            .filter(|component| !matches!(component, Component::CurDir))
            .peekable();
        if components.peek().is_none() {
            return self
                .try_clone()
                .map(|directory| Some(NoFollowSource::Directory(directory)));
        }

        let mut current = self.try_clone()?;
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                bail!(
                    "artifact source path is not a normalized relative path: {}",
                    relative.display()
                );
            };
            let source = current.open_entry(name)?;
            if components.peek().is_none() {
                return Ok(source);
            }
            current = match source {
                Some(NoFollowSource::Directory(directory)) => directory,
                Some(NoFollowSource::File(_)) => bail!(
                    "artifact source path has a non-directory ancestor: {}",
                    current.display_path.join(name).display()
                ),
                None => return Ok(None),
            };
        }
        Ok(None)
    }

    pub fn for_each_entry_filtered(
        &self,
        mut include: impl FnMut(&OsStr) -> bool,
        mut visit: impl FnMut(NoFollowDirEntry) -> Result<()>,
    ) -> Result<()> {
        let entries = rustix::fs::Dir::read_from(&self.file)
            .map_err(std::io::Error::from)
            .with_context(|| {
                format!(
                    "read artifact source directory {}",
                    self.display_path.display()
                )
            })?;
        for entry in entries {
            let entry = entry.map_err(std::io::Error::from).with_context(|| {
                format!(
                    "read artifact source directory {}",
                    self.display_path.display()
                )
            })?;
            let name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
            if name == "." || name == ".." {
                continue;
            }
            if !include(&name) {
                continue;
            }
            let source = self.open_entry(&name)?.with_context(|| {
                format!(
                    "artifact source disappeared during secure enumeration: {}",
                    self.display_path.join(&name).display()
                )
            })?;
            visit(NoFollowDirEntry { name, source })?;
        }
        Ok(())
    }

    pub fn for_each_entry_name(&self, mut visit: impl FnMut(OsString) -> Result<()>) -> Result<()> {
        let entries = rustix::fs::Dir::read_from(&self.file)
            .map_err(std::io::Error::from)
            .with_context(|| {
                format!(
                    "read artifact source directory {}",
                    self.display_path.display()
                )
            })?;
        for entry in entries {
            let entry = entry.map_err(std::io::Error::from).with_context(|| {
                format!(
                    "read artifact source directory {}",
                    self.display_path.display()
                )
            })?;
            let name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
            if name == "." || name == ".." {
                continue;
            }
            visit(name)?;
        }
        Ok(())
    }

    pub fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            file: self.file.try_clone().with_context(|| {
                format!(
                    "duplicate artifact source directory {}",
                    self.display_path.display()
                )
            })?,
            display_path: self.display_path.clone(),
        })
    }

    fn open_entry(&self, name: &std::ffi::OsStr) -> Result<Option<NoFollowSource>> {
        let display_path = self.display_path.join(name);
        let stat = match rustix::fs::statat(&self.file, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(std::io::Error::from(error)).with_context(|| {
                    format!("inspect artifact source {}", display_path.display())
                });
            }
        };
        match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
            rustix::fs::FileType::Symlink => {
                bail!("artifact source is a symlink: {}", display_path.display())
            }
            rustix::fs::FileType::Directory => {
                let file = rustix::fs::openat(
                    &self.file,
                    name,
                    rustix::fs::OFlags::RDONLY
                        | rustix::fs::OFlags::DIRECTORY
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(std::io::Error::from)
                .with_context(|| {
                    format!(
                        "open artifact source directory without following links: {}",
                        display_path.display()
                    )
                })?;
                let opened = rustix::fs::fstat(&file)
                    .map_err(std::io::Error::from)
                    .with_context(|| {
                        format!(
                            "inspect opened artifact source directory {}",
                            display_path.display()
                        )
                    })?;
                if rustix::fs::FileType::from_raw_mode(opened.st_mode)
                    != rustix::fs::FileType::Directory
                    || !same_file_identity(opened.st_dev, opened.st_ino, stat.st_dev, stat.st_ino)
                {
                    bail!(
                        "artifact source directory changed during secure open: {}",
                        display_path.display()
                    );
                }
                Ok(Some(NoFollowSource::Directory(Self {
                    file: file.into(),
                    display_path,
                })))
            }
            rustix::fs::FileType::RegularFile => {
                let file = open_source_file_nonblocking_no_follow_at(&self.file, Path::new(name))
                    .with_context(|| {
                    format!(
                        "open artifact source file without following links: {}",
                        display_path.display()
                    )
                })?;
                let opened = rustix::fs::fstat(&file)
                    .map_err(std::io::Error::from)
                    .with_context(|| {
                        format!("inspect opened artifact source {}", display_path.display())
                    })?;
                if rustix::fs::FileType::from_raw_mode(opened.st_mode)
                    != rustix::fs::FileType::RegularFile
                    || !same_file_identity(opened.st_dev, opened.st_ino, stat.st_dev, stat.st_ino)
                {
                    bail!(
                        "artifact source changed during secure open: {}",
                        display_path.display()
                    );
                }
                Ok(Some(NoFollowSource::File(file)))
            }
            _ => bail!(
                "artifact source has unsupported file type: {}",
                display_path.display()
            ),
        }
    }
}

#[cfg(unix)]
impl NoFollowDestinationDir {
    /// Stable physical identity for a secured directory descriptor. Unlike a
    /// path string this remains the same through mount aliases, while a copied
    /// storage root receives a different device/inode pair.
    pub(crate) fn physical_identity(&self) -> Result<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = self
            .file
            .metadata()
            .context("inspect secured artifact destination directory")?;
        if !metadata.is_dir() {
            bail!("secured artifact destination descriptor is not a directory");
        }
        Ok((metadata.dev(), metadata.ino()))
    }

    /// Open or create an absolute directory by walking from `/` with
    /// descriptor-relative no-follow operations. Unlike the trusted-root
    /// helper, this accepts a root whose final directories do not exist yet.
    pub(crate) fn open_or_create_absolute_no_follow(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            bail!(
                "artifact destination root must be absolute: {}",
                path.display()
            );
        }
        for component in path.components() {
            if matches!(component, Component::ParentDir | Component::Prefix(_)) {
                bail!(
                    "artifact destination root is not normalized: {}",
                    path.display()
                );
            }
        }

        let root = NoFollowDir::open_absolute(Path::new("/"))?;
        let root_file = root.file;
        let mut current = Self {
            file: root_file.try_clone().context("duplicate filesystem root")?,
            display_path: root.display_path,
            staging_parent: root_file,
            staging_parent_path: PathBuf::from("/"),
        };
        for component in path.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => {
                    let child = current.open_or_create_directory(name)?;
                    current.sync_directory().context(
                        "sync parent directory while opening or creating artifact destination root",
                    )?;
                    current = child;
                }
                Component::ParentDir | Component::Prefix(_) => bail!(
                    "artifact destination root is not normalized: {}",
                    path.display()
                ),
            }
        }
        current.staging_parent = current
            .file
            .try_clone()
            .context("duplicate artifact destination root for staging")?;
        current.staging_parent_path = current.display_path.clone();
        Ok(current)
    }

    /// Open an existing absolute directory without resolving any symlink in
    /// the path. Snapshot publication uses this for the store root: the root
    /// is runner-owned, but it must still be descriptor-bound before any
    /// generation or pointer mutation.
    pub(crate) fn open_absolute_no_follow(path: &Path) -> Result<Self> {
        let root = NoFollowDir::open_absolute(path)?;
        let staging_parent = root
            .file
            .try_clone()
            .context("duplicate artifact destination root for staging")?;
        Ok(Self {
            file: root.file,
            display_path: root.display_path.clone(),
            staging_parent,
            staging_parent_path: root.display_path,
        })
    }

    /// Opens a workflow-relative destination below a trusted configured root.
    ///
    /// Canonicalization is intentionally limited to `trusted_root`. The untrusted
    /// `relative` suffix is validated before side effects, then walked with
    /// descriptor-relative, no-follow operations.
    ///
    /// # Errors
    ///
    /// Returns an error when the trusted root is not absolute, cannot be securely
    /// opened as a directory, or the relative path is absolute, contains a parent
    /// component, or encounters a symlink or non-directory descendant.
    pub fn open_trusted_rooted_destination(trusted_root: &Path, relative: &Path) -> Result<Self> {
        Self::open_trusted_rooted_destination_with_staging_parent(
            trusted_root,
            relative,
            trusted_root,
        )
    }

    /// Opens a workflow destination while staging new files under a separate,
    /// runner-private directory on the same filesystem. `staging_parent` must
    /// not be inside a guest-visible bind mount.
    pub(crate) fn open_trusted_rooted_destination_with_staging_parent(
        trusted_root: &Path,
        relative: &Path,
        staging_parent: &Path,
    ) -> Result<Self> {
        if !trusted_root.is_absolute() {
            bail!(
                "trusted configured artifact destination root must be absolute: {}",
                trusted_root.display()
            );
        }
        if !staging_parent.is_absolute() {
            bail!(
                "private artifact staging root must be absolute: {}",
                staging_parent.display()
            );
        }

        validate_relative_components(relative, "artifact destination")?;

        let canonical_root = fs::canonicalize(trusted_root).with_context(|| {
            format!(
                "canonicalize trusted configured artifact destination root {}",
                trusted_root.display()
            )
        })?;
        let strict_root = NoFollowDir::open_absolute(&canonical_root).with_context(|| {
            format!(
                "securely open canonical trusted configured artifact destination root {}",
                canonical_root.display()
            )
        })?;
        let canonical_staging_parent = fs::canonicalize(staging_parent).with_context(|| {
            format!(
                "canonicalize private artifact staging root {}",
                staging_parent.display()
            )
        })?;
        let strict_staging_parent = NoFollowDir::open_absolute(&canonical_staging_parent)
            .with_context(|| {
                format!(
                    "securely open private artifact staging root {}",
                    canonical_staging_parent.display()
                )
            })?;
        let staging_parent_file = strict_staging_parent.file;
        let mut current = Self {
            file: strict_root.file,
            display_path: strict_root.display_path,
            staging_parent: staging_parent_file
                .try_clone()
                .context("duplicate private artifact staging root")?,
            staging_parent_path: strict_staging_parent.display_path,
        };
        current.ensure_staging_device_matches_destination()?;

        for component in relative.components() {
            if let Component::Normal(name) = component {
                current.ensure_staging_device_matches_destination()?;
                current = current.open_or_create_directory(name)?;
            }
        }
        Ok(current)
    }

    pub fn clone_or_copy_file(&self, source: &fs::File, relative: &Path) -> Result<u64> {
        self.clone_or_copy_file_with_method(source, relative)
            .map(|(bytes, _)| bytes)
    }

    /// Atomically publishes a private staging directory at `destination_name`.
    ///
    /// Both names are resolved relative to this already-open parent directory.
    /// An existing destination is moved aside first, and is removed only after
    /// the staged tree is visible. Any failed publication attempts to restore
    /// the original destination.
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
    pub fn publish_staged_directory(
        &self,
        staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        self.publish_staged_directory_from(self, staging_name, destination_name)
    }

    /// Atomically publishes a private staging directory from another already-open
    /// parent directory. The source and destination parents stay descriptor-bound
    /// for the entire exchange; no path is re-resolved during publication.
    pub fn publish_staged_directory_from(
        &self,
        staging_parent: &Self,
        staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        validate_single_component(staging_name, "staging directory")?;
        validate_single_component(destination_name, "destination directory")?;

        let staging_parent_stat = rustix::fs::fstat(&staging_parent.file)
            .map_err(std::io::Error::from)
            .context("inspect staged artifact parent filesystem")?;
        let destination_parent_stat = rustix::fs::fstat(&self.file)
            .map_err(std::io::Error::from)
            .context("inspect artifact destination parent filesystem")?;
        if staging_parent_stat.st_dev != destination_parent_stat.st_dev {
            bail!(
                "artifact staging parent {} and destination {} are on different filesystems",
                staging_parent.display_path.display(),
                self.display_path.display()
            );
        }

        let staging_stat = rustix::fs::statat(
            &staging_parent.file,
            staging_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!(
                "inspect staged artifact directory {}",
                staging_parent.display_path.join(staging_name).display()
            )
        })?;
        if rustix::fs::FileType::from_raw_mode(staging_stat.st_mode)
            != rustix::fs::FileType::Directory
        {
            bail!(
                "staged artifact is not a directory: {}",
                staging_parent.display_path.join(staging_name).display()
            );
        }

        for _ in 0..16 {
            let destination_exists = match rustix::fs::statat(
                &self.file,
                destination_name,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Ok(stat) => {
                    if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                        != rustix::fs::FileType::Directory
                    {
                        bail!(
                            "artifact destination is not a directory: {}",
                            self.display_path.join(destination_name).display()
                        );
                    }
                    true
                }
                Err(rustix::io::Errno::NOENT) => false,
                Err(error) => {
                    return Err(std::io::Error::from(error)).with_context(|| {
                        format!(
                            "inspect artifact destination {}",
                            self.display_path.join(destination_name).display()
                        )
                    });
                }
            };

            let result = if destination_exists {
                preflight_tree_removal(&self.file, destination_name).with_context(|| {
                    format!(
                        "preflight displaced artifact tree removal {}",
                        self.display_path.join(destination_name).display()
                    )
                })?;
                rustix::fs::renameat_with(
                    &staging_parent.file,
                    staging_name,
                    &self.file,
                    destination_name,
                    rustix::fs::RenameFlags::EXCHANGE,
                )
            } else {
                rustix::fs::renameat_with(
                    &staging_parent.file,
                    staging_name,
                    &self.file,
                    destination_name,
                    rustix::fs::RenameFlags::NOREPLACE,
                )
            };
            match result {
                Ok(()) => {
                    if destination_exists
                        && let Err(cleanup_error) =
                            remove_tree_at(&staging_parent.file, staging_name)
                    {
                        let rollback = rustix::fs::renameat_with(
                            &staging_parent.file,
                            staging_name,
                            &self.file,
                            destination_name,
                            rustix::fs::RenameFlags::EXCHANGE,
                        );
                        match rollback {
                            Ok(()) => {
                                if let Err(quarantine_error) =
                                    remove_tree_at(&staging_parent.file, staging_name)
                                {
                                    bail!(
                                            "artifact directory publication rolled back after replaced-tree cleanup failed ({cleanup_error}); new tree remains quarantined at {} because cleanup also failed ({quarantine_error})",
                                            staging_parent
                                                .display_path
                                                .join(staging_name)
                                                .display()
                                        );
                                }
                                return Err(cleanup_error).with_context(|| {
                                        format!(
                                            "artifact directory publication rolled back after replaced-tree cleanup failed at {}",
                                            staging_parent
                                                .display_path
                                                .join(staging_name)
                                                .display()
                                        )
                                    });
                            }
                            Err(rollback_error) => {
                                bail!(
                                        "artifact directory publication committed-partial: new tree is published at {}; previous tree remains quarantined at {}; cleanup failed ({cleanup_error}) and rollback failed ({})",
                                        self.display_path.join(destination_name).display(),
                                        staging_parent
                                            .display_path
                                            .join(staging_name)
                                            .display(),
                                        std::io::Error::from(rollback_error)
                                    );
                            }
                        }
                    }
                    return Ok(());
                }
                Err(rustix::io::Errno::EXIST) | Err(rustix::io::Errno::NOENT) => continue,
                Err(error) => {
                    return Err(std::io::Error::from(error)).with_context(|| {
                        format!(
                            "atomically publish staged artifact directory {}",
                            self.display_path.join(destination_name).display()
                        )
                    });
                }
            }
        }
        bail!("could not atomically publish staged artifact directory")
    }

    pub(crate) fn remove_tree_entry(&self, name: &OsStr) -> Result<()> {
        validate_single_component(name, "artifact tree entry")?;
        remove_tree_at(&self.file, name).with_context(|| {
            format!(
                "remove artifact tree entry {}",
                self.display_path.join(name).display()
            )
        })
    }

    /// Open or create a stable lock file relative to this already-open
    /// directory. The no-follow and nonblocking flags make substituted links
    /// and FIFOs fail safely without escaping the directory descriptor.
    pub(crate) fn open_or_create_lock_file(&self, name: &OsStr) -> Result<fs::File> {
        validate_single_component(name, "artifact lock file")?;
        let parent_metadata = rustix::fs::fstat(&self.file)
            .map_err(std::io::Error::from)
            .context("inspect artifact lock-file parent")?;
        if rustix::fs::FileType::from_raw_mode(parent_metadata.st_mode)
            != rustix::fs::FileType::Directory
            || parent_metadata.st_uid != rustix::process::geteuid().as_raw()
            || parent_metadata.st_mode & 0o022 != 0
        {
            bail!(
                "artifact lock-file parent is not owned by this user or is writable by others: {}",
                self.display_path.display()
            );
        }
        let file = rustix::fs::openat(
            &self.file,
            name,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::NOCTTY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(std::io::Error::from)
        .with_context(|| format!("open lock file {}", self.display_path.join(name).display()))?;
        let metadata = rustix::fs::fstat(&file)
            .map_err(std::io::Error::from)
            .context("inspect opened lock file")?;
        if rustix::fs::FileType::from_raw_mode(metadata.st_mode)
            != rustix::fs::FileType::RegularFile
        {
            bail!(
                "lock file is not a regular file: {}",
                self.display_path.join(name).display()
            );
        }
        if metadata.st_nlink != 1 || metadata.st_mode & 0o077 != 0 {
            bail!(
                "lock file is hard-linked or accessible to group/other: {}",
                self.display_path.join(name).display()
            );
        }
        Ok(file.into())
    }

    /// Atomically publish a staged regular file only when its destination is
    /// absent. Existing files, symlinks and special entries are never replaced.
    pub(crate) fn publish_temporary_file_no_replace(
        &self,
        staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        self.ensure_local_staging_parent()?;
        validate_single_component(staging_name, "staging file")?;
        validate_single_component(destination_name, "destination file")?;
        self.validate_destination_file(destination_name)?;
        let staging_directory = self.open_private_staging_directory(staging_name)?;
        let staged_file = staging_directory
            .open_relative_file(Path::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY))
            .with_context(|| {
                format!(
                    "open staged file in {}",
                    staging_directory.display_path.display()
                )
            })?;
        drop(staged_file);
        rustix::fs::renameat_with(
            &staging_directory.file,
            Path::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY),
            &self.file,
            destination_name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!(
                "publish staged file without replacement {}",
                self.display_path.join(destination_name).display()
            )
        })?;
        self.remove_empty_staging_directory(staging_name, &staging_directory)
    }

    /// Persist directory-entry changes made through this descriptor.
    pub(crate) fn sync_directory(&self) -> Result<()> {
        self.file
            .sync_all()
            .with_context(|| format!("sync artifact directory {}", self.display_path.display()))
    }

    pub fn open_relative_directory(&self, relative: &Path) -> Result<Self> {
        validate_relative_components(relative, "artifact destination")?;
        let mut current = self.try_clone()?;
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(name) => {
                    current.ensure_staging_device_matches_destination()?;
                    current = current.open_or_create_directory(name)?;
                }
                Component::RootDir | Component::ParentDir | Component::Prefix(_) => bail!(
                    "artifact destination is not a normalized relative path: {}",
                    relative.display()
                ),
            }
        }
        Ok(current)
    }

    pub fn open_relative_file(&self, relative: &Path) -> Result<fs::File> {
        self.open_relative_file_if_exists(relative)?
            .with_context(|| format!("artifact file does not exist: {}", relative.display()))
    }

    pub fn open_relative_file_if_exists(&self, relative: &Path) -> Result<Option<fs::File>> {
        let mut components = relative
            .components()
            .filter(|component| !matches!(component, Component::CurDir))
            .peekable();
        let mut parent = self.try_clone()?;
        let file_name = loop {
            let Some(component) = components.next() else {
                bail!(
                    "artifact file path has no file name: {}",
                    relative.display()
                );
            };
            let Component::Normal(name) = component else {
                bail!(
                    "artifact file path is not a normalized relative path: {}",
                    relative.display()
                );
            };
            if components.peek().is_none() {
                break name.to_os_string();
            }
            parent = parent.open_existing_directory(name)?;
        };
        let stat = match rustix::fs::statat(
            &parent.file,
            &file_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(std::io::Error::from(error))
                    .with_context(|| format!("inspect artifact file {}", relative.display()))
            }
        };
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
            bail!(
                "artifact file is not a regular file: {}",
                relative.display()
            );
        }
        let file = open_source_file_nonblocking_no_follow_at(&parent.file, Path::new(&file_name))
            .with_context(|| format!("open artifact file {}", relative.display()))?;
        let opened = rustix::fs::fstat(&file)
            .map_err(std::io::Error::from)
            .with_context(|| format!("inspect opened artifact file {}", relative.display()))?;
        if rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::RegularFile
            || !same_file_identity(opened.st_dev, opened.st_ino, stat.st_dev, stat.st_ino)
        {
            bail!(
                "artifact file changed during secure open: {}",
                relative.display()
            );
        }
        Ok(Some(file))
    }

    pub fn create_unique_directory(&self, prefix: &str) -> Result<(Self, OsString)> {
        self.ensure_safe_unique_directory_parent()?;
        for _ in 0..16 {
            let name = OsString::from(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            match rustix::fs::mkdirat(&self.file, &name, rustix::fs::Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let mut directory = match self.open_existing_directory(&name) {
                        Ok(directory) => directory,
                        Err(error) => {
                            let cleanup = self.cleanup_created_staging_directory(&name, None);
                            return match cleanup {
                                Ok(()) => Err(error),
                                Err(cleanup_error) => Err(error).with_context(|| {
                                    format!(
                                        "temporary directory cleanup failed ({cleanup_error:#})"
                                    )
                                }),
                            };
                        }
                    };
                    let setup = (|| -> Result<()> {
                        self.verify_staging_directory_name(&name, &directory)?;
                        let metadata = rustix::fs::fstat(&directory.file)
                            .map_err(std::io::Error::from)
                            .context("inspect new temporary directory")?;
                        if metadata.st_uid != rustix::process::geteuid().as_raw() {
                            bail!(
                                "temporary directory has an unexpected owner: {}",
                                self.display_path.join(&name).display()
                            );
                        }
                        rustix::fs::fchmod(&directory.file, rustix::fs::Mode::from_raw_mode(0o700))
                            .map_err(std::io::Error::from)
                            .context("restrict new temporary directory permissions")?;
                        self.verify_staging_directory_name(&name, &directory)?;
                        let metadata = rustix::fs::fstat(&directory.file)
                            .map_err(std::io::Error::from)
                            .context("verify new temporary directory permissions")?;
                        if metadata.st_mode & 0o7777 != 0o700 {
                            bail!(
                                "temporary directory is not mode 0700: {}",
                                self.display_path.join(&name).display()
                            );
                        }
                        Ok(())
                    })();
                    if let Err(error) = setup {
                        let cleanup =
                            self.cleanup_created_staging_directory(&name, Some(&directory));
                        return match cleanup {
                            Ok(()) => Err(error),
                            Err(cleanup_error) => Err(error).with_context(|| {
                                format!("temporary directory cleanup failed ({cleanup_error:#})")
                            }),
                        };
                    }
                    if let Err(error) = directory.use_self_as_staging_parent() {
                        let cleanup =
                            self.cleanup_created_staging_directory(&name, Some(&directory));
                        return match cleanup {
                            Ok(()) => Err(error),
                            Err(cleanup_error) => Err(error).with_context(|| {
                                format!("temporary directory cleanup failed ({cleanup_error:#})")
                            }),
                        };
                    }
                    return Ok((directory, name));
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => {
                    return Err(std::io::Error::from(error)).with_context(|| {
                        format!(
                            "create secure temporary directory {}",
                            name.to_string_lossy()
                        )
                    })
                }
            }
        }
        bail!("could not allocate a unique secure temporary directory")
    }

    pub fn create_unlinked_temporary_file(&self, prefix: &str) -> Result<fs::File> {
        for _ in 0..16 {
            let name = OsString::from(format!("{prefix}-{}.tmp", uuid::Uuid::new_v4()));
            let file = match rustix::fs::openat(
                &self.file,
                &name,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::from_raw_mode(0o600),
            ) {
                Ok(file) => file,
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => {
                    return Err(std::io::Error::from(error))
                        .with_context(|| format!("create secure temporary file {name:?}"))
                }
            };
            rustix::fs::unlinkat(&self.file, &name, rustix::fs::AtFlags::empty())
                .map_err(std::io::Error::from)
                .context("unlink secure temporary file name")?;
            return Ok(file.into());
        }
        bail!("could not allocate a unique secure temporary file")
    }

    fn ensure_private_staging_parent(&self) -> Result<()> {
        let metadata = rustix::fs::fstat(&self.file)
            .map_err(std::io::Error::from)
            .context("inspect temporary staging parent")?;
        if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::Directory
            || metadata.st_uid != rustix::process::geteuid().as_raw()
            || metadata.st_mode & 0o022 != 0
        {
            bail!(
                "temporary staging parent is not owned by this user or is writable by others: {}",
                self.display_path.display()
            );
        }
        Ok(())
    }

    fn ensure_safe_unique_directory_parent(&self) -> Result<()> {
        let metadata = rustix::fs::fstat(&self.file)
            .map_err(std::io::Error::from)
            .context("inspect temporary directory parent")?;
        let mode = metadata.st_mode;
        let owner = metadata.st_uid;
        let effective_user = rustix::process::geteuid().as_raw();
        let private_parent = owner == effective_user && mode & 0o022 == 0;
        let trusted_sticky_parent = mode & 0o1000 != 0 && (owner == 0 || owner == effective_user);
        if rustix::fs::FileType::from_raw_mode(mode) != rustix::fs::FileType::Directory
            || (!private_parent && !trusted_sticky_parent)
        {
            bail!(
                "temporary directory parent is neither private nor a trusted sticky directory: {}",
                self.display_path.display()
            );
        }
        Ok(())
    }

    fn ensure_configured_staging_parent(&self) -> Result<()> {
        let metadata = rustix::fs::fstat(&self.staging_parent)
            .map_err(std::io::Error::from)
            .context("inspect configured private artifact staging parent")?;
        if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::Directory
            || metadata.st_uid != rustix::process::geteuid().as_raw()
            || metadata.st_mode & 0o022 != 0
        {
            bail!(
                "configured artifact staging parent is not owned by this user or is writable by others: {}",
                self.staging_parent_path.display()
            );
        }
        Ok(())
    }

    fn ensure_staging_device_matches_destination(&self) -> Result<()> {
        let staging = rustix::fs::fstat(&self.staging_parent)
            .map_err(std::io::Error::from)
            .context("inspect artifact staging filesystem")?;
        let destination = rustix::fs::fstat(&self.file)
            .map_err(std::io::Error::from)
            .context("inspect artifact destination filesystem")?;
        if staging.st_dev != destination.st_dev {
            bail!(
                "artifact staging root {} and destination {} are on different filesystems",
                self.staging_parent_path.display(),
                self.display_path.display()
            );
        }
        Ok(())
    }

    fn ensure_local_staging_parent(&self) -> Result<()> {
        let staging = rustix::fs::fstat(&self.staging_parent)
            .map_err(std::io::Error::from)
            .context("inspect local artifact staging parent")?;
        let destination = rustix::fs::fstat(&self.file)
            .map_err(std::io::Error::from)
            .context("inspect local artifact destination parent")?;
        if staging.st_dev != destination.st_dev || staging.st_ino != destination.st_ino {
            bail!(
                "name-based temporary artifact operations require the staging and destination parents to be the same directory: {}",
                self.display_path.display()
            );
        }
        Ok(())
    }

    fn use_self_as_staging_parent(&mut self) -> Result<()> {
        self.staging_parent = self
            .file
            .try_clone()
            .context("duplicate private artifact directory for staging")?;
        self.staging_parent_path = self.display_path.clone();
        Ok(())
    }

    fn staging_parent_directory(&self) -> Result<Self> {
        let file = self.staging_parent.try_clone().with_context(|| {
            format!(
                "duplicate artifact staging parent {}",
                self.staging_parent_path.display()
            )
        })?;
        Ok(Self {
            staging_parent: file
                .try_clone()
                .context("duplicate artifact staging parent")?,
            staging_parent_path: self.staging_parent_path.clone(),
            file,
            display_path: self.staging_parent_path.clone(),
        })
    }

    fn create_private_staging_directory(&self, prefix: &str) -> Result<(Self, OsString)> {
        self.ensure_private_staging_parent()?;
        for _ in 0..16 {
            let name = OsString::from(format!("{prefix}-{}.tmp", uuid::Uuid::new_v4()));
            validate_single_component(&name, "staging directory")?;
            match rustix::fs::mkdirat(&self.file, &name, rustix::fs::Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let directory = match self.open_existing_directory(&name) {
                        Ok(directory) => directory,
                        Err(error) => {
                            let cleanup = self.cleanup_created_staging_directory(&name, None);
                            return match cleanup {
                                Ok(()) => Err(error),
                                Err(cleanup_error) => Err(error).with_context(|| {
                                    format!(
                                        "temporary staging directory cleanup failed ({cleanup_error:#})"
                                    )
                                }),
                            };
                        }
                    };
                    let setup = (|| -> Result<()> {
                        self.verify_staging_directory_name(&name, &directory)?;
                        let metadata = rustix::fs::fstat(&directory.file)
                            .map_err(std::io::Error::from)
                            .context("inspect new temporary staging directory")?;
                        if metadata.st_uid != rustix::process::geteuid().as_raw() {
                            bail!(
                                "temporary staging directory has an unexpected owner: {}",
                                self.display_path.join(&name).display()
                            );
                        }
                        rustix::fs::fchmod(&directory.file, rustix::fs::Mode::from_raw_mode(0o700))
                            .map_err(std::io::Error::from)
                            .context("restrict temporary staging directory permissions")?;
                        self.verify_staging_directory_name(&name, &directory)?;
                        let metadata = rustix::fs::fstat(&directory.file)
                            .map_err(std::io::Error::from)
                            .context("verify temporary staging directory permissions")?;
                        if metadata.st_mode & 0o7777 != 0o700 {
                            bail!(
                                "temporary staging directory is not mode 0700: {}",
                                self.display_path.join(&name).display()
                            );
                        }
                        Ok(())
                    })();
                    return match setup {
                        Ok(()) => Ok((directory, name)),
                        Err(error) => {
                            let cleanup =
                                self.cleanup_created_staging_directory(&name, Some(&directory));
                            match cleanup {
                                Ok(()) => Err(error),
                                Err(cleanup_error) => Err(error).with_context(|| {
                                    format!(
                                        "temporary staging directory cleanup failed ({cleanup_error:#})"
                                    )
                                }),
                            }
                        }
                    };
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => {
                    return Err(std::io::Error::from(error)).with_context(|| {
                        format!(
                            "create private temporary staging directory {}",
                            self.display_path.join(&name).display()
                        )
                    });
                }
            }
        }
        bail!("could not allocate a private temporary staging directory")
    }

    fn cleanup_created_staging_directory(
        &self,
        name: &OsStr,
        directory: Option<&Self>,
    ) -> Result<()> {
        if let Some(directory) = directory {
            match rustix::fs::fstat(&directory.file) {
                Ok(opened) => {
                    let named = match rustix::fs::statat(
                        &self.file,
                        name,
                        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                    ) {
                        Ok(named) => named,
                        Err(rustix::io::Errno::NOENT) => return Ok(()),
                        Err(error) => {
                            return Err(std::io::Error::from(error))
                                .context("inspect new staging name during cleanup");
                        }
                    };
                    if rustix::fs::FileType::from_raw_mode(named.st_mode)
                        != rustix::fs::FileType::Directory
                        || opened.st_dev != named.st_dev
                        || opened.st_ino != named.st_ino
                    {
                        bail!(
                            "new temporary staging directory changed before cleanup: {}",
                            self.display_path.join(name).display()
                        );
                    }
                    return rustix::fs::unlinkat(&self.file, name, rustix::fs::AtFlags::REMOVEDIR)
                        .map_err(std::io::Error::from)
                        .with_context(|| {
                            format!(
                                "remove newly created staging directory {}",
                                self.display_path.join(name).display()
                            )
                        });
                }
                Err(_) => {
                    // `mkdirat` just created this randomized entry under an
                    // owned, non-writable-by-others parent. `AT_REMOVEDIR`
                    // cannot follow a substituted symlink and only removes an
                    // empty directory.
                }
            }
        }
        match rustix::fs::unlinkat(&self.file, name, rustix::fs::AtFlags::REMOVEDIR) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(std::io::Error::from(error)).with_context(|| {
                format!(
                    "remove newly created staging directory {}",
                    self.display_path.join(name).display()
                )
            }),
        }
    }

    fn open_private_staging_directory(&self, name: &OsStr) -> Result<Self> {
        validate_single_component(name, "staging directory")?;
        self.ensure_private_staging_parent()?;
        let directory = self.open_existing_directory(name)?;
        self.verify_staging_directory_name(name, &directory)?;
        let metadata = rustix::fs::fstat(&directory.file)
            .map_err(std::io::Error::from)
            .context("inspect temporary staging directory")?;
        if metadata.st_uid != rustix::process::geteuid().as_raw()
            || metadata.st_mode & 0o7777 != 0o700
        {
            bail!(
                "temporary staging directory is not private: {}",
                self.display_path.join(name).display()
            );
        }
        Ok(directory)
    }

    fn remove_empty_staging_directory(&self, name: &OsStr, directory: &Self) -> Result<()> {
        self.verify_staging_directory_name(name, directory)?;
        rustix::fs::unlinkat(&self.file, name, rustix::fs::AtFlags::REMOVEDIR)
            .map_err(std::io::Error::from)
            .with_context(|| {
                format!(
                    "remove empty temporary staging directory {}",
                    self.display_path.join(name).display()
                )
            })
    }

    fn verify_staging_directory_name(&self, name: &OsStr, directory: &Self) -> Result<()> {
        let opened = rustix::fs::fstat(&directory.file)
            .map_err(std::io::Error::from)
            .context("inspect opened temporary staging directory")?;
        let named = rustix::fs::statat(&self.file, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(std::io::Error::from)
            .with_context(|| {
                format!(
                    "inspect temporary staging directory name {}",
                    self.display_path.join(name).display()
                )
            })?;
        if rustix::fs::FileType::from_raw_mode(named.st_mode) != rustix::fs::FileType::Directory
            || opened.st_dev != named.st_dev
            || opened.st_ino != named.st_ino
        {
            bail!(
                "temporary staging directory changed before cleanup: {}",
                self.display_path.join(name).display()
            );
        }
        Ok(())
    }

    fn create_staged_file_with<T>(
        &self,
        prefix: &str,
        create_file: impl FnOnce(&fs::File, &OsStr) -> std::io::Result<(fs::File, T)>,
    ) -> Result<(StagedFile, T)> {
        let destination_parent = self.try_clone()?;
        self.ensure_configured_staging_parent()?;
        self.ensure_staging_device_matches_destination()?;
        let staging_parent = self.staging_parent_directory()?;
        let (staging_directory, staging_name) =
            staging_parent.create_private_staging_directory(prefix)?;
        let (file, extra) = match create_file(
            &staging_directory.file,
            OsStr::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY),
        ) {
            Ok(created) => created,
            Err(error) => {
                let cleanup = staging_parent
                    .cleanup_created_staging_directory(&staging_name, Some(&staging_directory));
                return match cleanup {
                    Ok(()) => Err(std::io::Error::from(error))
                        .context("create temporary artifact destination"),
                    Err(cleanup_error) => Err(std::io::Error::from(error)).with_context(|| {
                        format!(
                            "create temporary artifact destination; staging cleanup failed ({cleanup_error:#})"
                        )
                    }),
                };
            }
        };
        let staged = StagedFile {
            file,
            staging_parent,
            staging_directory,
            staging_name,
            destination_parent,
        };
        Ok((staged, extra))
    }

    /// Create a file in an unmounted, mode-0700 staging directory. The returned
    /// capability owns the staging identity and the pinned destination parent;
    /// callers cannot publish by re-resolving a stage path.
    pub(crate) fn create_staged_temporary_file(&self, prefix: &str) -> Result<StagedFile> {
        self.create_staged_file_with(prefix, |parent, name| {
            create_temporary_file(parent, name).map(|file| (file, ()))
        })
        .map(|(file, ())| file)
    }

    fn create_staged_clone_or_file(
        &self,
        source: &fs::File,
        prefix: &str,
    ) -> Result<(StagedFile, bool)> {
        self.create_staged_file_with(prefix, |parent, name| {
            create_temporary_clone_or_file(source, parent, name)
        })
    }

    /// Create a file in a private mode-0700 staging directory. The returned
    /// name identifies that directory and is accepted by the publication and
    /// cleanup methods on this descriptor.
    pub fn create_temporary_file(&self, prefix: &str) -> Result<(fs::File, OsString)> {
        self.ensure_local_staging_parent()?;
        let (staging_directory, staging_name) = self.create_private_staging_directory(prefix)?;
        match create_temporary_file(
            &staging_directory.file,
            OsStr::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY),
        ) {
            Ok(file) => Ok((file, staging_name)),
            Err(error) => {
                let message = format!("create secure temporary file in {staging_name:?}");
                if let Err(cleanup_error) =
                    self.cleanup_created_staging_directory(&staging_name, Some(&staging_directory))
                {
                    return Err(std::io::Error::from(error)).with_context(|| {
                        format!("{message}; staging cleanup failed: {cleanup_error:#}")
                    });
                }
                Err(std::io::Error::from(error)).context(message)
            }
        }
    }

    pub fn publish_temporary_file(
        &self,
        staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        self.ensure_local_staging_parent()?;
        validate_single_component(staging_name, "staging file")?;
        validate_single_component(destination_name, "destination file")?;
        self.validate_destination_file(destination_name)?;
        let staging_directory = self.open_private_staging_directory(staging_name)?;
        let staged_file = staging_directory
            .open_relative_file(Path::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY))
            .with_context(|| {
                format!(
                    "open staged file in {}",
                    staging_directory.display_path.display()
                )
            })?;
        drop(staged_file);
        rustix::fs::renameat(
            &staging_directory.file,
            Path::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY),
            &self.file,
            destination_name,
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!(
                "publish temporary artifact file {}",
                self.display_path.join(destination_name).display()
            )
        })?;
        self.remove_empty_staging_directory(staging_name, &staging_directory)
    }

    pub(crate) fn set_mode(&self, mode: u16) -> Result<()> {
        rustix::fs::fchmod(
            &self.file,
            rustix::fs::Mode::from_raw_mode(rustix::fs::RawMode::from(mode)),
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!(
                "set artifact directory mode {}",
                self.display_path.display()
            )
        })
    }

    pub fn write_file_from_reader(
        &self,
        reader: &mut impl Read,
        relative: &Path,
        expected_size: u64,
        mode: u16,
    ) -> Result<u64> {
        validate_relative_components(relative, "artifact destination")?;
        let mut components = relative
            .components()
            .filter(|component| !matches!(component, Component::CurDir))
            .peekable();
        if components.peek().is_none() {
            bail!(
                "artifact destination has no file name: {}",
                relative.display()
            );
        }

        let mut parent = self.try_clone()?;
        let file_name = loop {
            let Some(component) = components.next() else {
                bail!(
                    "artifact destination has no file name: {}",
                    relative.display()
                );
            };
            let Component::Normal(name) = component else {
                bail!(
                    "artifact destination is not a normalized relative path: {}",
                    relative.display()
                );
            };
            if components.peek().is_none() {
                break name.to_os_string();
            }
            parent.ensure_staging_device_matches_destination()?;
            parent = parent.open_or_create_directory(name)?;
        };
        parent.validate_destination_file(&file_name)?;
        parent.ensure_configured_staging_parent()?;
        parent.ensure_staging_device_matches_destination()?;
        let mut destination_file = parent.create_staged_temporary_file(".velnor-copy")?;
        let write_result = (|| -> Result<u64> {
            let copied = {
                let mut limited = (&mut *reader).take(expected_size.saturating_add(1));
                std::io::copy(&mut limited, &mut destination_file)
                    .context("copy reader to temporary artifact destination")?
            };
            if copied != expected_size {
                bail!(
                    "artifact source size changed while copying {}",
                    relative.display()
                );
            }
            destination_file
                .flush()
                .context("flush artifact destination")?;
            rustix::fs::fchmod(
                destination_file.file()?,
                rustix::fs::Mode::from_raw_mode(rustix::fs::RawMode::from(mode)),
            )
            .map_err(std::io::Error::from)
            .context("set artifact destination mode")?;
            Ok(copied)
        })();
        let copied = write_result?;
        destination_file.publish(&file_name)?;
        Ok(copied)
    }

    fn clone_or_copy_file_with_method(
        &self,
        source: &fs::File,
        relative: &Path,
    ) -> Result<(u64, bool)> {
        validate_relative_components(relative, "artifact destination")?;
        let mut components = relative
            .components()
            .filter(|component| !matches!(component, Component::CurDir))
            .peekable();
        if components.peek().is_none() {
            bail!(
                "artifact destination has no file name: {}",
                relative.display()
            );
        }

        let mut parent = self.try_clone()?;
        let file_name = loop {
            let Some(component) = components.next() else {
                bail!(
                    "artifact destination has no file name: {}",
                    relative.display()
                );
            };
            let Component::Normal(name) = component else {
                bail!(
                    "artifact destination is not a normalized relative path: {}",
                    relative.display()
                );
            };
            if components.peek().is_none() {
                break name.to_os_string();
            }
            parent.ensure_staging_device_matches_destination()?;
            parent = parent.open_or_create_directory(name)?;
        };
        let destination_path = parent.display_path.join(&file_name);
        parent.validate_destination_file(&file_name)?;

        let metadata = source.metadata().context("inspect opened copy source")?;
        if !metadata.is_file() {
            bail!("copy source is not a regular file");
        }

        parent.ensure_configured_staging_parent()?;
        parent.ensure_staging_device_matches_destination()?;
        let (mut destination_file, used_reflink) =
            parent.create_staged_clone_or_file(source, ".velnor-copy")?;

        let write_result = (|| -> Result<u64> {
            let bytes = if used_reflink {
                let actual = destination_file
                    .file()?
                    .metadata()
                    .context("inspect reflink artifact destination")?
                    .len();
                if actual != metadata.len() {
                    bail!("reflink artifact source size changed while copying");
                }
                actual
            } else {
                destination_file
                    .file()?
                    .set_len(0)
                    .context("reset temporary artifact destination")?;
                destination_file
                    .file()?
                    .seek(SeekFrom::Start(0))
                    .context("rewind temporary artifact destination")?;
                let mut source = source
                    .try_clone()
                    .context("duplicate source file for copy")?;
                source
                    .seek(SeekFrom::Start(0))
                    .context("rewind source file for copy")?;
                let expected_size = metadata.len();
                let mut bounded_source = source.take(expected_size.saturating_add(1));
                let copied = std::io::copy(&mut bounded_source, &mut destination_file)
                    .with_context(|| {
                        format!("copy opened source to {}", destination_path.display())
                    })?;
                if copied != expected_size {
                    bail!("artifact source size changed while copying");
                }
                copied
            };
            destination_file.flush().with_context(|| {
                format!(
                    "flush temporary artifact destination for {}",
                    destination_path.display()
                )
            })?;
            #[allow(clippy::useless_conversion)]
            let raw_mode: rustix::fs::RawMode = metadata
                .permissions()
                .mode()
                .try_into()
                .context("convert opened copy source mode")?;
            rustix::fs::fchmod(
                destination_file.file()?,
                rustix::fs::Mode::from_raw_mode(raw_mode),
            )
            .map_err(std::io::Error::from)
            .with_context(|| {
                format!(
                    "set temporary artifact destination mode for {}",
                    destination_path.display()
                )
            })?;
            Ok(bytes)
        })();
        let bytes = write_result?;
        destination_file.publish(&file_name).with_context(|| {
            format!(
                "atomically replace artifact destination {}",
                destination_path.display()
            )
        })?;
        Ok((bytes, used_reflink))
    }

    fn try_clone(&self) -> Result<Self> {
        Ok(Self {
            file: self.file.try_clone().with_context(|| {
                format!(
                    "duplicate artifact destination directory {}",
                    self.display_path.display()
                )
            })?,
            display_path: self.display_path.clone(),
            staging_parent: self.staging_parent.try_clone().with_context(|| {
                format!(
                    "duplicate artifact staging parent {}",
                    self.staging_parent_path.display()
                )
            })?,
            staging_parent_path: self.staging_parent_path.clone(),
        })
    }

    fn validate_destination_file(&self, name: &OsStr) -> Result<()> {
        let display_path = self.display_path.join(name);
        let stat = match rustix::fs::statat(&self.file, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => {
                return Err(std::io::Error::from(error)).with_context(|| {
                    format!("inspect artifact destination {}", display_path.display())
                });
            }
        };
        match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
            rustix::fs::FileType::RegularFile => Ok(()),
            rustix::fs::FileType::Symlink => {
                bail!(
                    "artifact destination is a symlink: {}",
                    display_path.display()
                )
            }
            _ => bail!(
                "artifact destination is not a regular file: {}",
                display_path.display()
            ),
        }
    }

    fn open_or_create_directory(&self, name: &OsStr) -> Result<Self> {
        let display_path = self.display_path.join(name);
        let open = || {
            rustix::fs::openat(
                &self.file,
                name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
        };
        let file = match open() {
            Ok(file) => file,
            Err(rustix::io::Errno::NOENT) => {
                match rustix::fs::mkdirat(&self.file, name, rustix::fs::Mode::from_raw_mode(0o755))
                {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => {
                        return Err(std::io::Error::from(error)).with_context(|| {
                            format!(
                                "create artifact destination directory {}",
                                display_path.display()
                            )
                        });
                    }
                }
                match open() {
                    Ok(file) => file,
                    Err(rustix::io::Errno::LOOP) => {
                        bail!(
                            "artifact destination is a symlink: {}",
                            display_path.display()
                        )
                    }
                    Err(error) => {
                        if matches!(error, rustix::io::Errno::NOTDIR)
                            && destination_entry_is_symlink(&self.file, name)
                        {
                            bail!(
                                "artifact destination is a symlink: {}",
                                display_path.display()
                            );
                        }
                        return Err(std::io::Error::from(error)).with_context(|| {
                            format!(
                                "open created artifact destination directory without following links: {}",
                                display_path.display()
                            )
                        });
                    }
                }
            }
            Err(rustix::io::Errno::LOOP) => {
                bail!(
                    "artifact destination is a symlink: {}",
                    display_path.display()
                )
            }
            Err(error) => {
                if matches!(error, rustix::io::Errno::NOTDIR)
                    && destination_entry_is_symlink(&self.file, name)
                {
                    bail!(
                        "artifact destination is a symlink: {}",
                        display_path.display()
                    );
                }
                return Err(std::io::Error::from(error)).with_context(|| {
                    format!(
                        "open artifact destination directory without following links: {}",
                        display_path.display()
                    )
                });
            }
        };
        Ok(Self {
            file: file.into(),
            display_path,
            staging_parent: self
                .staging_parent
                .try_clone()
                .context("duplicate artifact staging parent")?,
            staging_parent_path: self.staging_parent_path.clone(),
        })
    }

    fn open_existing_directory(&self, name: &OsStr) -> Result<Self> {
        let display_path = self.display_path.join(name);
        let file = rustix::fs::openat(
            &self.file,
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "artifact destination is a symlink: {}",
                        display_path.display()
                    ),
                )
            } else {
                std::io::Error::from(error)
            }
        })
        .with_context(|| format!("open artifact directory {}", display_path.display()))?;
        Ok(Self {
            file: file.into(),
            display_path,
            staging_parent: self
                .staging_parent
                .try_clone()
                .context("duplicate artifact staging parent")?,
            staging_parent_path: self.staging_parent_path.clone(),
        })
    }
}

#[cfg(unix)]
fn validate_single_component(name: &OsStr, label: &str) -> Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        bail!("{label} name is not a single normalized path component")
    }
    Ok(())
}

#[cfg(unix)]
fn validate_relative_components(relative: &Path, label: &str) -> Result<()> {
    for component in relative.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => bail!(
                "{label} is not a normalized relative path: {}",
                relative.display()
            ),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn destination_entry_is_symlink(parent: &fs::File, name: &OsStr) -> bool {
    rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map(|stat| {
            rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Symlink
        })
        .unwrap_or(false)
}

/// Validate every operation required by `remove_tree_at` before a directory
/// exchange makes the tree the rollback target. Cleanup is destructive and
/// cannot be used as its own preflight. A concurrent mutation can still
/// invalidate this result; runtime rollback/quarantine handles that case.
#[cfg(unix)]
fn preflight_tree_removal(parent: &fs::File, name: &OsStr) -> Result<()> {
    preflight_tree_removal_at(parent, name, 0)
}

#[cfg(unix)]
fn preflight_tree_removal_at(parent: &fs::File, name: &OsStr, depth: usize) -> Result<()> {
    if depth > MAX_SECURE_CLEANUP_DEPTH {
        bail!(
            "artifact tree exceeds the {}-component secure cleanup depth",
            MAX_SECURE_CLEANUP_DEPTH
        );
    }

    rustix::fs::accessat(
        parent,
        Path::new("."),
        rustix::fs::Access::WRITE_OK | rustix::fs::Access::EXEC_OK,
        rustix::fs::AtFlags::empty(),
    )
    .map_err(std::io::Error::from)
    .context("verify artifact tree removal permissions")?;

    let stat = rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(std::io::Error::from)
        .context("inspect artifact tree entry for removal preflight")?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory {
        return Ok(());
    }

    let directory = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .context("open artifact tree for removal preflight")?;
    let directory: fs::File = directory.into();
    let entries = rustix::fs::Dir::read_from(&directory)
        .map_err(std::io::Error::from)
        .context("read artifact tree for removal preflight")?;
    for entry in entries {
        let entry = entry.map_err(std::io::Error::from)?;
        let entry_name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if entry_name == "." || entry_name == ".." {
            continue;
        }
        preflight_tree_removal_at(&directory, &entry_name, depth + 1)?;
    }
    Ok(())
}

#[cfg(unix)]
fn remove_tree_at(parent: &fs::File, name: &OsStr) -> Result<()> {
    remove_tree_at_depth(parent, name, 0)
}

#[cfg(unix)]
fn remove_tree_at_depth(parent: &fs::File, name: &OsStr, depth: usize) -> Result<()> {
    if depth > MAX_SECURE_CLEANUP_DEPTH {
        bail!(
            "artifact tree exceeds the {}-component secure cleanup depth",
            MAX_SECURE_CLEANUP_DEPTH
        );
    }
    let stat = match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory {
        match rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }

    let directory = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let directory: fs::File = directory.into();
    let entries = rustix::fs::Dir::read_from(&directory)
        .map_err(std::io::Error::from)
        .context("read artifact tree for secure cleanup")?;
    for entry in entries {
        let entry = entry.map_err(std::io::Error::from)?;
        let entry_name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if entry_name == "." || entry_name == ".." {
            continue;
        }
        remove_tree_at_depth(&directory, &entry_name, depth + 1)?;
    }
    match rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::REMOVEDIR) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn create_temporary_clone_or_file(
    source: &fs::File,
    parent: &fs::File,
    temporary_name: &OsStr,
) -> std::io::Result<(fs::File, bool)> {
    let destination = create_temporary_file(parent, temporary_name)?;
    #[cfg(target_os = "linux")]
    let used_reflink = rustix::fs::ioctl_ficlone(&destination, source).is_ok();
    #[cfg(not(target_os = "linux"))]
    let used_reflink = false;
    Ok((destination, used_reflink))
}

#[cfg(target_os = "macos")]
fn create_temporary_clone_or_file(
    source: &fs::File,
    parent: &fs::File,
    temporary_name: &OsStr,
) -> std::io::Result<(fs::File, bool)> {
    match rustix::fs::fclonefileat(
        source,
        parent,
        temporary_name,
        rustix::fs::CloneFlags::empty(),
    ) {
        Ok(()) => reopen_created_clone_with(parent, temporary_name, |parent, temporary_name| {
            rustix::fs::openat(
                parent,
                temporary_name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map(Into::into)
            .map_err(Into::into)
        }),
        Err(rustix::io::Errno::EXIST) => Err(std::io::Error::from(rustix::io::Errno::EXIST)),
        Err(_) => {
            remove_temporary_file(parent, temporary_name);
            create_temporary_file(parent, temporary_name).map(|file| (file, false))
        }
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn reopen_created_clone_with(
    parent: &fs::File,
    temporary_name: &OsStr,
    reopen: impl FnOnce(&fs::File, &OsStr) -> std::io::Result<fs::File>,
) -> std::io::Result<(fs::File, bool)> {
    let created = rustix::fs::statat(
        parent,
        temporary_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)?;
    if rustix::fs::FileType::from_raw_mode(created.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "cloned staging payload is not a regular file",
        ));
    }

    let result = (|| {
        let destination = reopen(parent, temporary_name)?;
        let opened = rustix::fs::fstat(&destination).map_err(std::io::Error::from)?;
        if rustix::fs::FileType::from_raw_mode(opened.st_mode) != rustix::fs::FileType::RegularFile
            || !same_file_identity(opened.st_dev, opened.st_ino, created.st_dev, created.st_ino)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "cloned staging payload changed during secure open",
            ));
        }
        Ok(destination)
    })();

    match result {
        Ok(destination) => Ok((destination, true)),
        Err(error) => {
            let cleanup = unlink_created_clone_if_same_identity(parent, temporary_name, &created);
            match cleanup {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(std::io::Error::new(
                    error.kind(),
                    format!("{error}; cloned payload cleanup failed: {cleanup_error}"),
                )),
            }
        }
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn unlink_created_clone_if_same_identity(
    parent: &fs::File,
    temporary_name: &OsStr,
    created: &rustix::fs::Stat,
) -> std::io::Result<()> {
    let named = match rustix::fs::statat(
        parent,
        temporary_name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(named) => named,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(std::io::Error::from(error)),
    };
    if rustix::fs::FileType::from_raw_mode(named.st_mode) != rustix::fs::FileType::RegularFile
        || !same_file_identity(named.st_dev, named.st_ino, created.st_dev, created.st_ino)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "cloned staging payload changed before cleanup",
        ));
    }
    match rustix::fs::unlinkat(parent, temporary_name, rustix::fs::AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(std::io::Error::from(error)),
    }
}

#[cfg(unix)]
fn create_temporary_file(parent: &fs::File, temporary_name: &OsStr) -> std::io::Result<fs::File> {
    rustix::fs::openat(
        parent,
        temporary_name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map(Into::into)
    .map_err(Into::into)
}

#[cfg(unix)]
fn remove_temporary_file(parent: &fs::File, temporary_name: &OsStr) {
    let _ = rustix::fs::unlinkat(parent, temporary_name, rustix::fs::AtFlags::empty());
}

#[cfg(not(unix))]
impl NoFollowDir {
    pub fn open_absolute(path: &Path) -> Result<Self> {
        bail!(
            "secure artifact source copying is unsupported on this platform for {}",
            path.display()
        )
    }

    pub fn open_trusted_configured_root(configured_root: &Path) -> Result<Self> {
        bail!(
            "secure trusted configured artifact source copying is unsupported on this platform for {}",
            configured_root.display()
        )
    }

    pub fn open_source(&self, _relative: &Path) -> Result<Option<NoFollowSource>> {
        bail!("secure artifact source copying is unsupported on this platform")
    }

    pub fn for_each_entry_filtered(
        &self,
        _include: impl FnMut(&OsStr) -> bool,
        _visit: impl FnMut(NoFollowDirEntry) -> Result<()>,
    ) -> Result<()> {
        bail!("secure artifact source copying is unsupported on this platform")
    }

    pub fn for_each_entry_name(&self, _visit: impl FnMut(OsString) -> Result<()>) -> Result<()> {
        bail!("secure artifact source copying is unsupported on this platform")
    }

    pub fn try_clone(&self) -> Result<Self> {
        bail!("secure artifact source copying is unsupported on this platform")
    }
}

#[cfg(unix)]
impl StagedFile {
    pub(crate) fn file(&self) -> Result<&fs::File> {
        Ok(&self.file)
    }

    fn verify_staged_payload_name(&self) -> Result<()> {
        let opened = rustix::fs::fstat(&self.file)
            .map_err(std::io::Error::from)
            .context("inspect opened staged artifact file")?;
        let named = rustix::fs::statat(
            &self.staging_directory.file,
            OsStr::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(std::io::Error::from)
        .context("inspect staged artifact file name")?;
        if rustix::fs::FileType::from_raw_mode(named.st_mode) != rustix::fs::FileType::RegularFile
            || opened.st_dev != named.st_dev
            || opened.st_ino != named.st_ino
            || opened.st_nlink != named.st_nlink
        {
            bail!("staged artifact file changed before publication");
        }
        Ok(())
    }

    pub(crate) fn publish(self, destination_name: &OsStr) -> Result<()> {
        validate_single_component(destination_name, "destination file")?;
        self.destination_parent
            .validate_destination_file(destination_name)?;
        self.staging_parent.ensure_private_staging_parent()?;
        self.staging_parent
            .verify_staging_directory_name(&self.staging_name, &self.staging_directory)?;
        self.verify_staged_payload_name()?;
        let staging = rustix::fs::fstat(&self.staging_directory.file)
            .map_err(std::io::Error::from)
            .context("inspect staged artifact filesystem")?;
        let destination = rustix::fs::fstat(&self.destination_parent.file)
            .map_err(std::io::Error::from)
            .context("inspect artifact publication destination filesystem")?;
        if staging.st_dev != destination.st_dev {
            bail!(
                "artifact staging root {} and destination {} are on different filesystems",
                self.staging_parent.display_path.display(),
                self.destination_parent.display_path.display()
            );
        }
        rustix::fs::renameat(
            &self.staging_directory.file,
            Path::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY),
            &self.destination_parent.file,
            destination_name,
        )
        .map_err(std::io::Error::from)
        .with_context(|| {
            format!(
                "publish staged artifact file {}",
                self.destination_parent
                    .display_path
                    .join(destination_name)
                    .display()
            )
        })?;
        self.staging_parent
            .remove_empty_staging_directory(&self.staging_name, &self.staging_directory)
    }

    fn cleanup(&mut self) {
        let name = OsStr::new(TEMPORARY_FILE_IN_STAGING_DIRECTORY);
        match rustix::fs::statat(
            &self.staging_directory.file,
            name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(named) => {
                if let Ok(opened) = rustix::fs::fstat(&self.file)
                    && rustix::fs::FileType::from_raw_mode(named.st_mode)
                        == rustix::fs::FileType::RegularFile
                    && opened.st_dev == named.st_dev
                    && opened.st_ino == named.st_ino
                {
                    let _ = rustix::fs::unlinkat(
                        &self.staging_directory.file,
                        name,
                        rustix::fs::AtFlags::empty(),
                    );
                }
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(_) => return,
        }
        let _ = self
            .staging_parent
            .remove_empty_staging_directory(&self.staging_name, &self.staging_directory);
    }
}

#[cfg(unix)]
impl Drop for StagedFile {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(unix)]
impl Write for StagedFile {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(not(unix))]
impl NoFollowDestinationDir {
    pub(crate) fn physical_identity(&self) -> Result<(u64, u64)> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "physical directory identity requires Unix no-follow filesystem support",
        )
        .into())
    }

    pub(crate) fn open_or_create_absolute_no_follow(path: &Path) -> Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "secure artifact destination creation is unsupported on this platform for {}",
                path.display()
            ),
        )
        .into())
    }

    pub(crate) fn open_absolute_no_follow(path: &Path) -> Result<Self> {
        bail!(
            "secure artifact destination copying is unsupported on this platform for {}",
            path.display()
        )
    }

    pub fn open_trusted_rooted_destination(trusted_root: &Path, relative: &Path) -> Result<Self> {
        bail!(
            "secure trusted-rooted artifact destination copying is unsupported on this platform for {} below {}",
            relative.display(),
            trusted_root.display()
        )
    }

    pub(crate) fn open_trusted_rooted_destination_with_staging_parent(
        trusted_root: &Path,
        relative: &Path,
        staging_parent: &Path,
    ) -> Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "secure artifact staging is unsupported on this platform for {} below {} using {}",
                relative.display(),
                trusted_root.display(),
                staging_parent.display()
            ),
        )
        .into())
    }

    pub fn clone_or_copy_file(&self, _source: &fs::File, relative: &Path) -> Result<u64> {
        bail!(
            "secure artifact destination copying is unsupported on this platform for {}",
            relative.display()
        )
    }

    pub fn open_relative_directory(&self, relative: &Path) -> Result<Self> {
        bail!(
            "secure artifact destination copying is unsupported on this platform for {}",
            relative.display()
        )
    }

    pub fn open_relative_file(&self, relative: &Path) -> Result<fs::File> {
        bail!(
            "secure artifact destination copying is unsupported on this platform for {}",
            relative.display()
        )
    }

    pub fn open_relative_file_if_exists(&self, relative: &Path) -> Result<Option<fs::File>> {
        bail!(
            "secure artifact destination copying is unsupported on this platform for {}",
            relative.display()
        )
    }

    pub fn create_unique_directory(&self, prefix: &str) -> Result<(Self, OsString)> {
        bail!("secure artifact destination copying is unsupported on this platform for {prefix}")
    }

    pub fn create_unlinked_temporary_file(&self, prefix: &str) -> Result<fs::File> {
        bail!("secure artifact destination copying is unsupported on this platform for {prefix}")
    }

    pub fn create_temporary_file(&self, prefix: &str) -> Result<(fs::File, OsString)> {
        bail!("secure artifact destination copying is unsupported on this platform for {prefix}")
    }

    pub(crate) fn create_staged_temporary_file(&self, prefix: &str) -> Result<StagedFile> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("secure staged artifact files are unsupported on this platform for {prefix}"),
        )
        .into())
    }

    pub fn publish_temporary_file(
        &self,
        _staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        bail!(
            "secure artifact destination copying is unsupported on this platform for {}",
            destination_name.to_string_lossy()
        )
    }

    pub(crate) fn open_or_create_lock_file(&self, name: &OsStr) -> Result<fs::File> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "secure artifact lock files are unsupported on this platform for {}",
                name.to_string_lossy()
            ),
        )
        .into())
    }

    pub(crate) fn publish_temporary_file_no_replace(
        &self,
        staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "secure artifact publication is unsupported on this platform for {} to {}",
                staging_name.to_string_lossy(),
                destination_name.to_string_lossy()
            ),
        )
        .into())
    }

    pub(crate) fn sync_directory(&self) -> Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure artifact directory sync is unsupported on this platform",
        )
        .into())
    }

    pub(crate) fn set_mode(&self, _mode: u16) -> Result<()> {
        bail!("secure artifact destination copying is unsupported on this platform")
    }

    pub fn write_file_from_reader(
        &self,
        _reader: &mut impl Read,
        relative: &Path,
        _expected_size: u64,
        _mode: u16,
    ) -> Result<u64> {
        bail!(
            "secure artifact destination copying is unsupported on this platform for {}",
            relative.display()
        )
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
    pub fn publish_staged_directory(
        &self,
        _staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        bail!(
            "secure artifact destination publishing is unsupported on this platform for {}",
            destination_name.to_string_lossy()
        )
    }

    pub fn publish_staged_directory_from(
        &self,
        _staging_parent: &Self,
        _staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        bail!(
            "secure artifact destination publishing is unsupported on this platform for {}",
            destination_name.to_string_lossy()
        )
    }

    pub(crate) fn remove_tree_entry(&self, name: &OsStr) -> Result<()> {
        bail!(
            "secure artifact tree cleanup is unsupported on this platform for {}",
            name.to_string_lossy()
        )
    }
}

#[cfg(not(unix))]
impl StagedFile {
    pub(crate) fn file(&self) -> Result<&fs::File> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure staged artifact files are unsupported on this platform",
        )
        .into())
    }

    pub(crate) fn publish(self, destination_name: &OsStr) -> Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "secure staged artifact publication is unsupported on this platform for {}",
                destination_name.to_string_lossy()
            ),
        )
        .into())
    }
}

#[cfg(not(unix))]
impl Write for StagedFile {
    fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure staged artifact files are unsupported on this platform",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure staged artifact files are unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
fn open_source_file_nonblocking_no_follow_at(
    directory: impl rustix::fd::AsFd,
    source: &Path,
) -> std::io::Result<fs::File> {
    // Never retry without NONBLOCK: an unsupported flag must fail closed because
    // the entry can become a FIFO between the caller's type check and this open.
    rustix::fs::openat(
        directory,
        source,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(Into::into)
    .map_err(Into::into)
}

#[cfg(unix)]
fn same_file_identity<Device: PartialEq, Inode: PartialEq>(
    left_device: Device,
    left_inode: Inode,
    right_device: Device,
    right_inode: Inode,
) -> bool {
    left_device == right_device && left_inode == right_inode
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
    #[cfg(unix)]
    use std::{io::Read, sync::mpsc, thread, time::Duration};

    use super::*;

    #[cfg(unix)]
    fn open_regular_source(path: &Path) -> fs::File {
        let parent = NoFollowDir::open_trusted_configured_root(path.parent().unwrap()).unwrap();
        let Some(NoFollowSource::File(file)) = parent
            .open_source(Path::new(path.file_name().unwrap()))
            .unwrap()
        else {
            panic!("test source must be a regular file");
        };
        file
    }

    #[cfg(not(unix))]
    fn open_regular_source(path: &Path) -> fs::File {
        fs::File::open(path).unwrap()
    }

    #[cfg(not(unix))]
    #[test]
    fn unix_only_destination_operations_fail_with_unsupported() {
        fn assert_unsupported(error: anyhow::Error) {
            assert_eq!(
                error
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind),
                Some(std::io::ErrorKind::Unsupported)
            );
        }

        let directory = NoFollowDestinationDir;
        assert_unsupported(
            NoFollowDestinationDir::open_or_create_absolute_no_follow(Path::new("/")).unwrap_err(),
        );
        assert_unsupported(
            NoFollowDestinationDir::open_trusted_rooted_destination_with_staging_parent(
                Path::new("/"),
                Path::new(""),
                Path::new("/"),
            )
            .unwrap_err(),
        );
        assert_unsupported(directory.physical_identity().unwrap_err());
        assert_unsupported(directory.create_staged_temporary_file("stage").unwrap_err());
        assert_unsupported(
            directory
                .open_or_create_lock_file(std::ffi::OsStr::new("lock"))
                .unwrap_err(),
        );
        assert_unsupported(
            directory
                .publish_temporary_file_no_replace(
                    std::ffi::OsStr::new("stage"),
                    std::ffi::OsStr::new("destination"),
                )
                .unwrap_err(),
        );
        assert_unsupported(directory.sync_directory().unwrap_err());
    }

    #[cfg(unix)]
    #[test]
    fn invalid_parent_components_do_not_create_partial_destination_parents() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("source"), b"source").unwrap();
        let destination =
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, Path::new("")).unwrap();
        let invalid_relative = Path::new("would-create/../artifact");

        assert!(destination
            .open_relative_directory(invalid_relative)
            .is_err());
        let mut reader = &b"artifact"[..];
        assert!(destination
            .write_file_from_reader(&mut reader, invalid_relative, 8, 0o644)
            .is_err());
        let source = open_regular_source(&root.join("source"));
        assert!(destination
            .clone_or_copy_file(&source, invalid_relative)
            .is_err());
        assert!(
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, invalid_relative)
                .is_err()
        );

        assert!(!root.join("would-create").exists());
        assert!(!root.join("artifact").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_staging_directory_symlink_cannot_publish_or_escape() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let temp = root.join("temp");
        let outside = root.join("outside");
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(
            outside.join(TEMPORARY_FILE_IN_STAGING_DIRECTORY),
            b"outside",
        )
        .unwrap();
        let destination =
            NoFollowDestinationDir::open_trusted_rooted_destination_with_staging_parent(
                &temp,
                Path::new(""),
                &root,
            )
            .unwrap();
        let mut staged_file = destination.create_staged_temporary_file("stage").unwrap();
        staged_file.write_all(b"staged").unwrap();

        let staging_path = root.join(&staged_file.staging_name);
        let moved_stage = root.join("moved-stage");
        fs::rename(&staging_path, &moved_stage).unwrap();
        std::os::unix::fs::symlink(&outside, &staging_path).unwrap();

        assert!(staged_file.publish(OsStr::new("destination")).is_err());
        assert_eq!(
            fs::read(outside.join(TEMPORARY_FILE_IN_STAGING_DIRECTORY)).unwrap(),
            b"outside"
        );
        assert!(!temp.join("destination").exists());

        fs::remove_file(staging_path).unwrap();
        fs::remove_dir(moved_stage).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_staged_payload_symlink_cannot_publish_or_escape() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let temp = root.join("temp");
        let outside = root.join("outside");
        fs::create_dir_all(&temp).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("approved");
        fs::write(&outside_file, b"outside").unwrap();
        let destination =
            NoFollowDestinationDir::open_trusted_rooted_destination_with_staging_parent(
                &temp,
                Path::new(""),
                &root,
            )
            .unwrap();
        let mut staged_file = destination.create_staged_temporary_file("stage").unwrap();
        staged_file.write_all(b"staged").unwrap();

        let staging_path = root.join(&staged_file.staging_name);
        let payload_path = staging_path.join(TEMPORARY_FILE_IN_STAGING_DIRECTORY);
        let moved_payload = root.join("moved-payload");
        fs::rename(&payload_path, &moved_payload).unwrap();
        std::os::unix::fs::symlink(&outside_file, &payload_path).unwrap();

        assert!(staged_file.publish(OsStr::new("destination")).is_err());
        assert_eq!(fs::read(&outside_file).unwrap(), b"outside");
        assert!(!temp.join("destination").exists());

        fs::remove_file(&payload_path).unwrap();
        fs::remove_file(moved_payload).unwrap();
        fs::remove_dir(staging_path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_staging_root_supports_world_writable_temp_without_staging_in_temp() {
        use std::os::unix::fs::PermissionsExt as _;

        let job_dir = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let temp = job_dir.join("temp");
        fs::create_dir_all(&temp).unwrap();
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o1777)).unwrap();
        let destination =
            NoFollowDestinationDir::open_trusted_rooted_destination_with_staging_parent(
                &temp,
                Path::new(""),
                &job_dir,
            )
            .unwrap();

        let mut reader = &b"script step payload"[..];
        destination
            .write_file_from_reader(&mut reader, Path::new("step.json"), 19, 0o644)
            .unwrap();

        assert_eq!(
            fs::read(temp.join("step.json")).unwrap(),
            b"script step payload"
        );
        assert_eq!(fs::read_dir(&temp).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&job_dir).unwrap().count(), 1);
        fs::remove_dir_all(job_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_temp_can_create_a_private_download_staging_root() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp).unwrap();
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o1777)).unwrap();
        let parent =
            NoFollowDestinationDir::open_trusted_rooted_destination(&temp, Path::new("")).unwrap();
        let (staging_root, staging_name) =
            parent.create_unique_directory("artifact-download").unwrap();

        let mut reader = &b"downloaded artifact"[..];
        staging_root
            .write_file_from_reader(&mut reader, Path::new("payload"), 19, 0o644)
            .unwrap();

        assert_eq!(
            fs::read(temp.join(&staging_name).join("payload")).unwrap(),
            b"downloaded artifact"
        );
        assert_eq!(
            fs::metadata(temp.join(&staging_name))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o700
        );

        let unsafe_parent = temp.join("non-sticky");
        fs::create_dir(&unsafe_parent).unwrap();
        fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777)).unwrap();
        let unsafe_parent =
            NoFollowDestinationDir::open_trusted_rooted_destination(&unsafe_parent, Path::new(""))
                .unwrap();
        assert!(unsafe_parent.create_unique_directory("stage").is_err());

        fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn absolute_no_follow_directory_creation_rejects_symlink_parent() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();
        let outside = canonical_root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, canonical_root.join("targets")).unwrap();

        assert!(NoFollowDestinationDir::open_or_create_absolute_no_follow(
            &canonical_root.join("targets/slot")
        )
        .is_err());
        assert!(!outside.join("slot").exists());
        assert!(fs::symlink_metadata(canonical_root.join("targets"))
            .unwrap()
            .file_type()
            .is_symlink());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn source_leaf_replacement_changes_opened_identity() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source_path = root.join("source");
        let replacement_path = root.join("replacement");
        fs::write(&source_path, b"approved source").unwrap();
        let source_parent = NoFollowDir::open_absolute(&root).unwrap();
        let expected = rustix::fs::statat(
            &source_parent.file,
            OsStr::new("source"),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .unwrap();

        fs::write(&replacement_path, b"replacement source").unwrap();
        fs::rename(&replacement_path, &source_path).unwrap();
        let opened =
            open_source_file_nonblocking_no_follow_at(&source_parent.file, Path::new("source"))
                .unwrap();
        let actual = rustix::fs::fstat(&opened).unwrap();

        assert!(!same_file_identity(
            expected.st_dev,
            expected.st_ino,
            actual.st_dev,
            actual.st_ino
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_source_accepts_curdir_relative_path() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("dist")).unwrap();
        fs::write(root.join("dist/artifact.txt"), b"artifact").unwrap();

        let source = fs::canonicalize(&root).unwrap();
        let source = NoFollowDir::open_absolute(&source).unwrap();
        let Some(NoFollowSource::File(mut file)) = source
            .open_source(Path::new("./dist/artifact.txt"))
            .unwrap()
        else {
            panic!("CurDir-relative artifact path must open its file");
        };
        let mut content = String::new();
        file.read_to_string(&mut content).unwrap();

        assert_eq!(content, "artifact");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn trusted_configured_root_resolves_var_style_alias_only_at_root() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let canonical_root = root.join("private/var");
        let configured_root = root.join("var");
        fs::create_dir_all(&canonical_root).unwrap();
        fs::write(canonical_root.join("artifact"), b"artifact").unwrap();
        std::os::unix::fs::symlink(&canonical_root, &configured_root).unwrap();
        std::os::unix::fs::symlink("artifact", canonical_root.join("workflow-link")).unwrap();

        let strict_error = NoFollowDir::open_absolute(&configured_root).unwrap_err();
        assert!(strict_error
            .to_string()
            .contains("artifact source is a symlink"));

        let trusted = NoFollowDir::open_trusted_configured_root(&configured_root).unwrap();
        let Some(NoFollowSource::File(mut artifact)) =
            trusted.open_source(Path::new("artifact")).unwrap()
        else {
            panic!("trusted configured root must open its regular-file descendant");
        };
        let mut content = String::new();
        artifact.read_to_string(&mut content).unwrap();
        assert_eq!(content, "artifact");

        let descendant_error = trusted.open_source(Path::new("workflow-link")).unwrap_err();
        assert!(descendant_error
            .to_string()
            .contains("artifact source is a symlink"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn source_file_open_uses_nonblock_for_fifo() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let fifo = root.join("source.fifo");
        let status = std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(&fifo)
            .status()
            .expect("mkfifo must be available on Unix");
        assert!(status.success(), "create FIFO: {status}");

        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = open_source_file_nonblocking_no_follow_at(rustix::fs::CWD, &fifo);
            sender.send(result).unwrap();
        });
        let file = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("opening a FIFO source must not block")
            .unwrap();
        worker.join().unwrap();

        assert!(!file.metadata().unwrap().is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_destination_rejects_symlink() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination_root = root.join("destination");
        let outside = root.join("outside");
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(&source, b"source").unwrap();
        fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, destination_root.join("artifact")).unwrap();

        let source = open_regular_source(&source);
        let destination = NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("destination"),
        )
        .unwrap();
        let error = destination
            .clone_or_copy_file(&source, Path::new("artifact"))
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("artifact destination is a symlink"));
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        assert!(fs::symlink_metadata(destination_root.join("artifact"))
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_destination_creates_missing_parents() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination_root = root.join("missing/root");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"source").unwrap();

        let source = open_regular_source(&source);
        let destination = NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("missing/root"),
        )
        .unwrap();
        destination
            .clone_or_copy_file(&source, Path::new("nested/artifact"))
            .unwrap();

        assert_eq!(
            fs::read(destination_root.join("nested/artifact")).unwrap(),
            b"source"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_destination_atomically_replaces_regular_file() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination_root = root.join("destination");
        let destination_path = destination_root.join("artifact");
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(&destination_path, b"old").unwrap();
        let mut opened_old_destination = fs::File::open(&destination_path).unwrap();

        let source = open_regular_source(&source);
        let destination = NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("destination"),
        )
        .unwrap();
        destination
            .clone_or_copy_file(&source, Path::new("artifact"))
            .unwrap();

        let mut old_content = String::new();
        opened_old_destination
            .read_to_string(&mut old_content)
            .unwrap();
        assert_eq!(fs::read(&destination_path).unwrap(), b"new");
        assert_eq!(old_content, "old");
        assert!(fs::read_dir(&destination_root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".velnor-copy-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_destination_atomically_publishes_and_cleans_staged_tree() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let destination_path = root.join("artifact");
        let outside = root.join("outside");
        fs::create_dir_all(&destination_path).unwrap();
        fs::write(destination_path.join("stale"), b"stale").unwrap();
        fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, destination_path.join("link")).unwrap();

        let parent =
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, Path::new("")).unwrap();
        let (staged, staging_name) = parent.create_unique_directory(".staged").unwrap();
        staged
            .write_file_from_reader(&mut &b"published"[..], Path::new("new/file"), 9, 0o644)
            .unwrap();

        parent
            .publish_staged_directory(&staging_name, OsStr::new("artifact"))
            .unwrap();

        assert_eq!(
            fs::read(destination_path.join("new/file")).unwrap(),
            b"published"
        );
        assert!(!destination_path.join("stale").exists());
        assert_eq!(fs::read(&outside).unwrap(), b"outside");
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".velnor-replaced-artifact-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_destination_atomically_publishes_from_private_parent() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let private_path = root.join("private");
        let destination_path = root.join("artifact");
        fs::create_dir_all(&destination_path).unwrap();
        fs::create_dir_all(&private_path).unwrap();
        fs::write(destination_path.join("stale"), b"stale").unwrap();

        let destination_parent =
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, Path::new("")).unwrap();
        let private_parent =
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, Path::new("private"))
                .unwrap();
        let (staged, staging_name) = private_parent.create_unique_directory(".staged").unwrap();
        staged
            .write_file_from_reader(&mut &b"published"[..], Path::new("new/file"), 9, 0o644)
            .unwrap();

        destination_parent
            .publish_staged_directory_from(&private_parent, &staging_name, OsStr::new("artifact"))
            .unwrap();

        assert_eq!(
            fs::read(destination_path.join("new/file")).unwrap(),
            b"published"
        );
        assert!(!destination_path.join("stale").exists());
        assert!(fs::read_dir(&private_path).unwrap().next().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_destination_copies_opened_regular_file() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination_root = root.join("destination");
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(&source, b"artifact").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();

        let source = open_regular_source(&source);
        let destination = NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("destination"),
        )
        .unwrap();
        let bytes = destination
            .clone_or_copy_file(&source, Path::new("artifact"))
            .unwrap();

        let destination_path = destination_root.join("artifact");
        assert_eq!(bytes, 8);
        assert_eq!(fs::read(&destination_path).unwrap(), b"artifact");
        assert_eq!(
            fs::metadata(destination_path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_destination_rejects_unsafe_components() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination_root = root.join("destination");
        fs::create_dir_all(&destination_root).unwrap();
        fs::write(&source, b"source").unwrap();
        fs::write(destination_root.join("not-a-directory"), b"file").unwrap();

        let source = open_regular_source(&source);
        assert!(NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("created-before/../escape")
        )
        .is_err());
        assert!(!root.join("created-before").exists());
        assert!(NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            &root.join("absolute")
        )
        .is_err());
        assert!(NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("destination/not-a-directory/nested")
        )
        .is_err());

        let destination = NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("destination"),
        )
        .unwrap();

        assert!(destination
            .clone_or_copy_file(&source, Path::new("../escape"))
            .is_err());
        assert!(destination
            .clone_or_copy_file(&source, &root.join("absolute"))
            .is_err());
        assert!(destination
            .clone_or_copy_file(&source, Path::new("not-a-directory/artifact"))
            .is_err());
        assert!(!root.join("escape").exists());
        assert!(!root.join("absolute").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn trusted_rooted_destination_resolves_var_style_alias_only_at_root() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let canonical_root = root.join("private/var");
        let configured_root = root.join("var");
        let outside = root.join("outside");
        let source = root.join("source");
        fs::create_dir_all(&canonical_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(&source, b"artifact").unwrap();
        std::os::unix::fs::symlink(&canonical_root, &configured_root).unwrap();
        std::os::unix::fs::symlink(&outside, canonical_root.join("workflow-link")).unwrap();

        let source = open_regular_source(&source);
        let destination = NoFollowDestinationDir::open_trusted_rooted_destination(
            &configured_root,
            Path::new("workflow/nested"),
        )
        .unwrap();
        destination
            .clone_or_copy_file(&source, Path::new("artifact"))
            .unwrap();
        assert_eq!(
            fs::read(canonical_root.join("workflow/nested/artifact")).unwrap(),
            b"artifact"
        );

        assert!(NoFollowDestinationDir::open_trusted_rooted_destination(
            &configured_root,
            Path::new("workflow-link/nested")
        )
        .is_err());
        assert!(!outside.join("nested").exists());
        assert!(fs::symlink_metadata(canonical_root.join("workflow-link"))
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_iteration_streams_large_directory() {
        const FILE_COUNT: usize = 1_024;

        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        for index in 0..FILE_COUNT {
            fs::write(source.join(format!("file-{index:04}")), b"content").unwrap();
        }

        let source = fs::canonicalize(source).unwrap();
        let source = NoFollowDir::open_absolute(&source).unwrap();
        let destination_root = NoFollowDestinationDir::open_trusted_rooted_destination(
            &root,
            Path::new("destination"),
        )
        .unwrap();
        let mut copied = 0;
        source
            .for_each_entry_filtered(
                |_| true,
                |entry| {
                    let NoFollowSource::File(file) = entry.source else {
                        panic!("large flat fixture contains only files");
                    };
                    destination_root.clone_or_copy_file(&file, Path::new(&entry.name))?;
                    copied += 1;
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(copied, FILE_COUNT);
        assert_eq!(fs::read_dir(&destination).unwrap().count(), FILE_COUNT);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_iteration_filters_before_opening_and_recurses() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("included"), b"included").unwrap();
        fs::write(source.join("nested/child"), b"child").unwrap();
        std::os::unix::fs::symlink("included", source.join(".excluded")).unwrap();

        let source = fs::canonicalize(source).unwrap();
        let source = NoFollowDir::open_absolute(&source).unwrap();
        let mut paths = Vec::new();
        source
            .for_each_entry_filtered(
                |name| !name.as_encoded_bytes().starts_with(b"."),
                |entry| {
                    match entry.source {
                        NoFollowSource::File(_) => paths.push(PathBuf::from(entry.name)),
                        NoFollowSource::Directory(directory) => {
                            let parent = PathBuf::from(entry.name);
                            directory.for_each_entry_filtered(
                                |_| true,
                                |entry| {
                                    let NoFollowSource::File(_) = entry.source else {
                                        panic!("nested fixture contains only one file");
                                    };
                                    paths.push(parent.join(entry.name));
                                    Ok(())
                                },
                            )?;
                        }
                    }
                    Ok(())
                },
            )
            .unwrap();

        paths.sort();
        assert_eq!(
            paths,
            [PathBuf::from("included"), PathBuf::from("nested/child")]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clone_reopen_failure_removes_payload_before_private_stage_cleanup() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let parent =
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, Path::new("")).unwrap();
        let (staging, staging_name) = parent.create_private_staging_directory(".clone").unwrap();
        let payload = create_temporary_file(&staging.file, OsStr::new("payload")).unwrap();
        drop(payload);

        let error = reopen_created_clone_with(&staging.file, OsStr::new("payload"), |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected reopen failure",
            ))
        })
        .unwrap_err();

        assert!(error.to_string().contains("injected reopen failure"));
        assert!(matches!(
            rustix::fs::statat(
                &staging.file,
                OsStr::new("payload"),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            ),
            Err(rustix::io::Errno::NOENT)
        ));
        parent
            .cleanup_created_staging_directory(&staging_name, Some(&staging))
            .unwrap();
        assert!(!root.join(&staging_name).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clone_or_copy_preserves_content_and_length() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination = root.join("nested/destination");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"reflink-or-copy").unwrap();

        let source_file = open_regular_source(&source);
        let destination_root =
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, Path::new("nested"))
                .unwrap();
        let bytes = destination_root
            .clone_or_copy_file(&source_file, Path::new("destination"))
            .unwrap();
        assert_eq!(bytes, 15);
        assert_eq!(fs::read(&destination).unwrap(), b"reflink-or-copy");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clone_or_copy_file_replaces_existing_destination() {
        let root = std::env::temp_dir().join(format!("velnor-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"stale-long-value").unwrap();

        let source = open_regular_source(&source);
        let destination_root =
            NoFollowDestinationDir::open_trusted_rooted_destination(&root, Path::new(".")).unwrap();
        destination_root
            .clone_or_copy_file(&source, Path::new("destination"))
            .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        fs::remove_dir_all(root).unwrap();
    }
}
