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
}

#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct NoFollowDestinationDir;

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
                if !file
                    .metadata()
                    .with_context(|| {
                        format!("inspect opened artifact source {}", display_path.display())
                    })?
                    .is_file()
                {
                    bail!(
                        "artifact source changed type during secure open: {}",
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
        if !trusted_root.is_absolute() {
            bail!(
                "trusted configured artifact destination root must be absolute: {}",
                trusted_root.display()
            );
        }

        for component in relative.components() {
            match component {
                Component::CurDir | Component::Normal(_) => {}
                Component::RootDir | Component::ParentDir | Component::Prefix(_) => bail!(
                    "artifact destination is not a normalized relative path: {}",
                    relative.display()
                ),
            }
        }

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
        let mut current = Self {
            file: strict_root.file,
            display_path: strict_root.display_path,
        };

        for component in relative.components() {
            if let Component::Normal(name) = component {
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

    pub fn open_relative_directory(&self, relative: &Path) -> Result<Self> {
        let mut current = self.try_clone()?;
        for component in relative.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(name) => {
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
        if !file
            .metadata()
            .with_context(|| format!("inspect opened artifact file {}", relative.display()))?
            .is_file()
        {
            bail!(
                "artifact file changed type during secure open: {}",
                relative.display()
            );
        }
        Ok(Some(file))
    }

    pub fn create_unique_directory(&self, prefix: &str) -> Result<(Self, OsString)> {
        for _ in 0..16 {
            let name = OsString::from(format!("{prefix}-{}", uuid::Uuid::new_v4()));
            match rustix::fs::mkdirat(&self.file, &name, rustix::fs::Mode::from_raw_mode(0o700)) {
                Ok(()) => return Ok((self.open_or_create_directory(&name)?, name)),
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

    pub fn create_temporary_file(&self, prefix: &str) -> Result<(fs::File, OsString)> {
        for _ in 0..16 {
            let name = OsString::from(format!("{prefix}-{}.tmp", uuid::Uuid::new_v4()));
            match create_temporary_file(&self.file, &name) {
                Ok(file) => return Ok((file, name)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("create secure temporary file {name:?}"))
                }
            }
        }
        bail!("could not allocate a unique named temporary file")
    }

    pub fn publish_temporary_file(
        &self,
        staging_name: &OsStr,
        destination_name: &OsStr,
    ) -> Result<()> {
        validate_single_component(staging_name, "staging file")?;
        validate_single_component(destination_name, "destination file")?;
        self.validate_destination_file(destination_name)?;
        rustix::fs::renameat(&self.file, staging_name, &self.file, destination_name)
            .map_err(std::io::Error::from)
            .with_context(|| {
                format!(
                    "publish temporary artifact file {}",
                    self.display_path.join(destination_name).display()
                )
            })
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
            parent = parent.open_or_create_directory(name)?;
        };
        parent.validate_destination_file(&file_name)?;

        for _ in 0..16 {
            let temporary_name =
                OsString::from(format!(".velnor-copy-{}.tmp", uuid::Uuid::new_v4()));
            let mut destination_file = match create_temporary_file(&parent.file, &temporary_name) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("create temporary artifact destination"),
            };
            let copied = {
                let mut limited = (&mut *reader).take(expected_size.saturating_add(1));
                std::io::copy(&mut limited, &mut destination_file)
                    .context("copy reader to temporary artifact destination")?
            };
            if copied != expected_size {
                drop(destination_file);
                remove_temporary_file(&parent.file, &temporary_name);
                bail!(
                    "artifact source size changed while copying {}",
                    relative.display()
                );
            }
            destination_file
                .flush()
                .context("flush artifact destination")?;
            rustix::fs::fchmod(
                &destination_file,
                rustix::fs::Mode::from_raw_mode(rustix::fs::RawMode::from(mode)),
            )
            .map_err(std::io::Error::from)
            .context("set artifact destination mode")?;
            drop(destination_file);
            if let Err(error) =
                rustix::fs::renameat(&parent.file, &temporary_name, &parent.file, &file_name)
            {
                remove_temporary_file(&parent.file, &temporary_name);
                return Err(std::io::Error::from(error))
                    .context("atomically replace artifact destination");
            }
            return Ok(copied);
        }

        bail!("could not allocate a unique temporary artifact destination")
    }

    fn clone_or_copy_file_with_method(
        &self,
        source: &fs::File,
        relative: &Path,
    ) -> Result<(u64, bool)> {
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
            parent = parent.open_or_create_directory(name)?;
        };
        let destination_path = parent.display_path.join(&file_name);
        parent.validate_destination_file(&file_name)?;

        let metadata = source.metadata().context("inspect opened copy source")?;
        if !metadata.is_file() {
            bail!("copy source is not a regular file");
        }

        for _ in 0..16 {
            let temporary_name =
                OsString::from(format!(".velnor-copy-{}.tmp", uuid::Uuid::new_v4()));
            let (mut destination_file, used_reflink) =
                match create_temporary_clone_or_file(source, &parent.file, &temporary_name) {
                    Ok(created) => created,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => {
                        remove_temporary_file(&parent.file, &temporary_name);
                        return Err(error).with_context(|| {
                            format!(
                                "create temporary artifact destination in {}",
                                parent.display_path.display()
                            )
                        });
                    }
                };

            let write_result = (|| -> Result<u64> {
                let bytes = if used_reflink {
                    let actual = destination_file
                        .metadata()
                        .context("inspect reflink artifact destination")?
                        .len();
                    if actual != metadata.len() {
                        bail!("reflink artifact source size changed while copying");
                    }
                    actual
                } else {
                    destination_file
                        .set_len(0)
                        .context("reset temporary artifact destination")?;
                    destination_file
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
                rustix::fs::fchmod(&destination_file, rustix::fs::Mode::from_raw_mode(raw_mode))
                    .map_err(std::io::Error::from)
                    .with_context(|| {
                        format!(
                            "set temporary artifact destination mode for {}",
                            destination_path.display()
                        )
                    })?;
                Ok(bytes)
            })();

            let bytes = match write_result {
                Ok(bytes) => bytes,
                Err(error) => {
                    drop(destination_file);
                    remove_temporary_file(&parent.file, &temporary_name);
                    return Err(error);
                }
            };
            drop(destination_file);

            if let Err(error) =
                rustix::fs::renameat(&parent.file, &temporary_name, &parent.file, &file_name)
            {
                remove_temporary_file(&parent.file, &temporary_name);
                return Err(std::io::Error::from(error)).with_context(|| {
                    format!(
                        "atomically replace artifact destination {}",
                        destination_path.display()
                    )
                });
            }
            return Ok((bytes, used_reflink));
        }

        bail!(
            "could not allocate a unique temporary artifact destination in {}",
            parent.display_path.display()
        )
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
        Ok(()) => {
            let destination = rustix::fs::openat(
                parent,
                temporary_name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )?;
            Ok((destination.into(), true))
        }
        Err(rustix::io::Errno::EXIST) => Err(std::io::Error::from(rustix::io::Errno::EXIST)),
        Err(_) => {
            remove_temporary_file(parent, temporary_name);
            create_temporary_file(parent, temporary_name).map(|file| (file, false))
        }
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

#[cfg(not(unix))]
impl NoFollowDestinationDir {
    pub fn open_trusted_rooted_destination(trusted_root: &Path, relative: &Path) -> Result<Self> {
        bail!(
            "secure trusted-rooted artifact destination copying is unsupported on this platform for {} below {}",
            relative.display(),
            trusted_root.display()
        )
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

#[cfg(test)]
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
