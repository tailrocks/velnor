//! Descriptor-relative access to repository inputs and generated outputs.
//!
//! Every descendant is opened one component at a time with `O_NOFOLLOW`.
//! Checks and later reads/writes therefore use the same directory handles and
//! cannot be redirected by replacing a checked parent with a symlink.
//! POSIX rename and unlink APIs still address the parent by descriptor and the
//! leaf by name, so a same-user process can move a pinned parent or replace a
//! leaf between the final identity check and commit/cleanup. Binding checks
//! catch observed swaps; the API cannot atomically prevent that concurrent
//! same-user rename.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::fd::RawFd;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use super::{unique_suffix, FilePreimage, GeneratorError};

const DIRECTORY_FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
    .union(rustix::fs::OFlags::DIRECTORY)
    .union(rustix::fs::OFlags::CLOEXEC)
    .union(rustix::fs::OFlags::NOFOLLOW)
    .union(rustix::fs::OFlags::NONBLOCK);

const FILE_FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
    .union(rustix::fs::OFlags::CLOEXEC)
    .union(rustix::fs::OFlags::NOFOLLOW)
    .union(rustix::fs::OFlags::NONBLOCK);

#[allow(unsafe_code)]
pub(crate) mod pinned_command;

#[derive(Debug)]
pub(crate) struct SafeRoot {
    directory: fs::File,
    display_path: PathBuf,
    path_bound: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct SafeRootIdentity {
    canonical_path: PathBuf,
    device: u64,
    inode: u64,
    // Keep the observed inode alive for as long as its identity is used.
    // Otherwise a removed directory can have its inode recycled and make a
    // later replacement appear to match this saved identity.
    _identity_hold: Arc<fs::File>,
}

impl PartialEq for SafeRootIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_path == other.canonical_path
            && self.device == other.device
            && self.inode == other.inode
    }
}

impl Eq for SafeRootIdentity {}

impl SafeRootIdentity {
    pub(crate) fn matches_metadata(&self, metadata: &fs::Metadata) -> bool {
        self.device == metadata.dev() && self.inode == metadata.ino()
    }

    fn matches_stat(&self, stat: &rustix::fs::Stat) -> bool {
        self.device == stat.st_dev as u64 && self.inode == stat.st_ino
    }
}

#[derive(Debug)]
pub(crate) struct SafeDirEntry {
    pub(crate) name: OsString,
    pub(crate) kind: SafeEntryKind,
}

#[derive(Debug)]
pub(crate) enum SafeEntryKind {
    File,
    Directory(SafeRoot),
    Symlink,
    Other,
}

#[derive(Debug)]
struct SafeParent {
    directory: fs::File,
    display_path: PathBuf,
    name: OsString,
    path_bound: bool,
}

#[derive(Debug)]
struct StageFileGuard<'a> {
    parent: &'a SafeParent,
    file: fs::File,
    name: OsString,
    identity: Option<StageIdentity>,
    armed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StageIdentity {
    device: u64,
    inode: u64,
}

impl StageIdentity {
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino,
        }
    }

    fn matches(self, stat: &rustix::fs::Stat) -> bool {
        self.device == stat.st_dev as u64 && self.inode == stat.st_ino
    }
}

impl StageFileGuard<'_> {
    fn name(&self) -> &OsStr {
        &self.name
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StageFileGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let identity = self.identity.or_else(|| {
            rustix::fs::fstat(&self.file)
                .ok()
                .map(|stat| StageIdentity::from_stat(&stat))
        });
        let Some(identity) = identity else {
            return;
        };
        let Ok(entry) = rustix::fs::statat(
            &self.parent.directory,
            &self.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) else {
            return;
        };
        if rustix::fs::FileType::from_raw_mode(entry.st_mode) != rustix::fs::FileType::RegularFile
            || !identity.matches(&entry)
        {
            return;
        }
        // POSIX has no unlink-by-inode operation. The descriptor-relative
        // identity check prevents deleting a reused stage name in the
        // observed state; a same-user replacement can still race unlinkat.
        let _ = rustix::fs::unlinkat(
            &self.parent.directory,
            &self.name,
            rustix::fs::AtFlags::empty(),
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileVersion {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct OutputPathBinding {
    root_path: PathBuf,
    existing_prefix: PathBuf,
    device: u64,
    inode: u64,
    missing_suffix: Vec<OsString>,
    // Hold the prefix inode open so a same-device/inode check cannot accept
    // an ABA replacement after the original directory has been unlinked.
    _identity_hold: Arc<fs::File>,
}

impl PartialEq for OutputPathBinding {
    fn eq(&self, other: &Self) -> bool {
        self.root_path == other.root_path
            && self.existing_prefix == other.existing_prefix
            && self.device == other.device
            && self.inode == other.inode
            && self.missing_suffix == other.missing_suffix
    }
}

impl Eq for OutputPathBinding {}

impl OutputPathBinding {
    /// Capture the existing directory identity that anchors an output path.
    /// When the output root is absent, remember the nearest existing prefix
    /// and require the missing suffix to stay absent until the write lock is
    /// acquired.
    pub(crate) fn capture(root: &Path) -> Result<Self, GeneratorError> {
        let resolved = super::canonicalize_existing_output_prefix(root)?;
        if resolved != root {
            return Err(GeneratorError::usage(format!(
                "output path changed after resolution: {} now resolves to {}",
                root.display(),
                resolved.display()
            )));
        }
        if !root.is_absolute() {
            return Err(GeneratorError::usage(format!(
                "output path must be canonical and absolute: {}",
                root.display()
            )));
        }

        let components = root
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(name.to_os_string()),
                Component::RootDir => None,
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut existing_prefix = PathBuf::from("/");
        let mut missing_suffix = Vec::new();
        for (index, component) in components.iter().enumerate() {
            if !missing_suffix.is_empty() {
                missing_suffix.push(component.clone());
                continue;
            }
            let candidate = existing_prefix.join(component);
            match fs::symlink_metadata(&candidate) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(path_error("refusing symlinked output ancestor", &candidate));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(path_error(
                        "output path ancestor is not a directory",
                        &candidate,
                    ));
                }
                Ok(_) => existing_prefix = candidate,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    missing_suffix.extend(components[index..].iter().cloned());
                    break;
                }
                Err(error) => {
                    return Err(GeneratorError::io(
                        "inspect output path binding",
                        &candidate,
                        &error,
                    ));
                }
            }
        }
        let prefix = open_absolute_directory(&existing_prefix).map_err(|error| {
            GeneratorError::io(
                "open output path binding without following links",
                &existing_prefix,
                &error,
            )
        })?;
        let metadata = prefix.metadata().map_err(|error| {
            GeneratorError::io("inspect output path binding", &existing_prefix, &error)
        })?;
        Ok(Self {
            root_path: root.to_path_buf(),
            existing_prefix,
            device: metadata.dev(),
            inode: metadata.ino(),
            missing_suffix,
            _identity_hold: Arc::new(prefix),
        })
    }

    /// Capture an output path beneath a directory handle that was selected at
    /// command entry. Path-based resolution alone can accept a replacement
    /// directory installed at the same spelling before capture; walking from
    /// `anchor` keeps the first identity observation on the already selected
    /// tree. Later writes still re-open the displayed path and compare it to
    /// this pinned identity, so a moved or replaced path fails closed.
    pub(crate) fn capture_under(root: &Path, anchor: &SafeRoot) -> Result<Self, GeneratorError> {
        if !root.is_absolute() {
            return Err(GeneratorError::usage(format!(
                "output path must be canonical and absolute: {}",
                root.display()
            )));
        }
        let anchor_path = anchor.command_directory();
        let relative = root
            .strip_prefix(anchor_path)
            .map_err(|_| path_error("output path is outside its captured directory", root))?;
        let components = if relative.as_os_str().is_empty() {
            Vec::new()
        } else {
            relative_components(relative)?
        };

        anchor.validate_root_binding()?;
        let mut current = anchor.duplicate()?;
        let mut existing_prefix = anchor_path.to_path_buf();
        let mut missing_suffix = Vec::new();
        for (index, component) in components.iter().enumerate() {
            match current.open_child(component)? {
                ChildEntry::Directory(directory) => {
                    existing_prefix.push(component);
                    current = directory;
                }
                ChildEntry::Missing => {
                    missing_suffix.extend(components[index..].iter().cloned());
                    break;
                }
                ChildEntry::Symlink => {
                    return Err(path_error("refusing symlinked output ancestor", root));
                }
                ChildEntry::File | ChildEntry::Other => {
                    return Err(path_error("output path ancestor is not a directory", root));
                }
            }
        }
        current.validate_root_binding()?;
        let metadata = current.directory.metadata().map_err(|error| {
            GeneratorError::io("inspect output path binding", &existing_prefix, &error)
        })?;
        Ok(Self {
            root_path: root.to_path_buf(),
            existing_prefix,
            device: metadata.dev(),
            inode: metadata.ino(),
            missing_suffix,
            _identity_hold: Arc::new(current.clone_directory_handle()?),
        })
    }

    /// Require this output binding to name the exact repository directory
    /// captured for scanning when output and source paths are the same.
    pub(crate) fn ensure_same_root(&self, source: &SafeRootIdentity) -> Result<(), GeneratorError> {
        if self.root_path != source.canonical_path
            || self.device != source.device
            || self.inode != source.inode
        {
            return Err(path_error(
                "output root differs from the repository captured for scanning",
                &self.root_path,
            ));
        }
        Ok(())
    }
}

impl SafeRoot {
    pub(crate) fn identity(&self) -> Result<SafeRootIdentity, GeneratorError> {
        let metadata = self.directory.metadata().map_err(|error| {
            GeneratorError::io("inspect pinned repository root", &self.display_path, &error)
        })?;
        Ok(SafeRootIdentity {
            canonical_path: self.display_path.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
            _identity_hold: Arc::new(self.clone_directory_handle()?),
        })
    }

    /// Return a CLOEXEC duplicate for an explicitly managed child-process
    /// handoff. The launcher may clear CLOEXEC only in the child before exec;
    /// the parent descriptor stays private.
    pub(crate) fn clone_directory_handle(&self) -> Result<fs::File, GeneratorError> {
        let duplicate = rustix::io::fcntl_dupfd_cloexec(&self.directory, 0).map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io(
                "duplicate pinned directory handle",
                &self.display_path,
                &error,
            )
        })?;
        Ok(duplicate.into())
    }

    /// Duplicate this root's pinned directory while retaining its binding
    /// semantics and diagnostic path.
    pub(crate) fn duplicate(&self) -> Result<Self, GeneratorError> {
        Ok(Self {
            directory: self.clone_directory_handle()?,
            display_path: self.display_path.clone(),
            path_bound: self.path_bound,
        })
    }

    /// Change permissions through the captured directory handle. Candidate
    /// sandbox setup uses this only for the isolated source snapshot and its
    /// dedicated writable output mount; no path is reopened.
    pub(crate) fn set_mode(&self, mode: u16) -> Result<(), GeneratorError> {
        rustix::fs::fchmod(
            &self.directory,
            rustix::fs::Mode::from_raw_mode(mode.into()),
        )
        .map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io(
                "set pinned directory permissions",
                &self.display_path,
                &error,
            )
        })
    }

    /// Reopen a directory descriptor received from the trusted Linux child
    /// launcher. `display_path` is diagnostic only; this root performs all
    /// filesystem operations through the resulting handle and never reopens
    /// that path. `/proc/self/fd` validates the numeric descriptor through a
    /// normal open, so invalid or reused environment values fail without
    /// constructing a Rust handle from an unchecked descriptor number.
    pub(crate) fn from_inherited_directory_fd(
        raw_fd: RawFd,
        display_path: &Path,
    ) -> Result<Self, GeneratorError> {
        if raw_fd < 3 {
            return Err(GeneratorError::usage(format!(
                "inherited root descriptor is invalid: {}",
                display_path.display()
            )));
        }
        let directory = Self::open_inherited_directory_handle(raw_fd, display_path)?;
        let metadata = directory.metadata().map_err(|error| {
            GeneratorError::io("inspect inherited directory handle", display_path, &error)
        })?;
        if !metadata.is_dir() {
            return Err(GeneratorError::usage(format!(
                "inherited root descriptor is not a directory: {}",
                display_path.display()
            )));
        }
        Ok(Self {
            directory,
            display_path: display_path.to_path_buf(),
            path_bound: false,
        })
    }

    #[cfg(target_os = "linux")]
    fn open_inherited_directory_handle(
        raw_fd: RawFd,
        display_path: &Path,
    ) -> Result<fs::File, GeneratorError> {
        let descriptor_path = PathBuf::from(format!("/proc/self/fd/{raw_fd}"));
        let flags = rustix::fs::OFlags::RDONLY
            .union(rustix::fs::OFlags::DIRECTORY)
            .union(rustix::fs::OFlags::CLOEXEC)
            .union(rustix::fs::OFlags::NONBLOCK);
        rustix::fs::open(&descriptor_path, flags, rustix::fs::Mode::empty())
            .map(Into::into)
            .map_err(|error| {
                let error = io::Error::from(error);
                GeneratorError::io("open inherited directory handle", display_path, &error)
            })
    }

    #[cfg(not(target_os = "linux"))]
    fn open_inherited_directory_handle(
        _raw_fd: RawFd,
        display_path: &Path,
    ) -> Result<fs::File, GeneratorError> {
        Err(GeneratorError::usage(format!(
            "inherited directory handles are supported only on Linux: {}",
            display_path.display()
        )))
    }

    /// Open an existing repository directory after resolving only the trusted
    /// root alias. The final root entry and every canonical path component are
    /// opened without following links.
    pub(crate) fn open(root: &Path) -> Result<Self, GeneratorError> {
        // `lstat("alias/")` follows `alias` because the trailing slash asks
        // the kernel to resolve it as a directory. Rebuild the path from its
        // components before inspecting the final entry so `alias/` and
        // `alias/.` receive the same no-follow check as `alias`.
        let mut components = root.components().collect::<Vec<_>>();
        while components.len() > 1 && matches!(components.last(), Some(Component::CurDir)) {
            components.pop();
        }
        let mut root_entry = PathBuf::new();
        for component in components {
            root_entry.push(component.as_os_str());
        }
        if root_entry.as_os_str().is_empty() {
            root_entry.push(".");
        }
        let observed = fs::symlink_metadata(&root_entry)
            .map_err(|error| GeneratorError::io("inspect repository root", root, &error))?;
        if observed.file_type().is_symlink() {
            return Err(GeneratorError::usage(format!(
                "refusing symlinked repository root: {}",
                root.display()
            )));
        }
        if !observed.is_dir() {
            return Err(GeneratorError::usage(format!(
                "repository root is not a directory: {}",
                root.display()
            )));
        }
        let canonical = root
            .canonicalize()
            .map_err(|error| GeneratorError::io("canonicalize repository root", root, &error))?;
        let directory = open_absolute_directory(&canonical).map_err(|error| {
            GeneratorError::io(
                "open repository root without following links",
                &canonical,
                &error,
            )
        })?;
        let opened = directory.metadata().map_err(|error| {
            GeneratorError::io("inspect opened repository root", &canonical, &error)
        })?;
        if !same_object_metadata(&observed, &opened) {
            return Err(GeneratorError::usage(format!(
                "repository root changed while opening it: {}",
                root.display()
            )));
        }
        Ok(Self {
            directory,
            display_path: canonical,
            path_bound: true,
        })
    }

    /// Open or create a canonical output root. The resolved path must still
    /// equal the path approved by `resolve_output_path`; a new symlinked
    /// ancestor after that check is rejected.
    pub(crate) fn open_output(root: &Path) -> Result<Self, GeneratorError> {
        let resolved = super::canonicalize_existing_output_prefix(root)?;
        if resolved != root {
            return Err(GeneratorError::usage(format!(
                "output path changed after resolution: {} now resolves to {}",
                root.display(),
                resolved.display()
            )));
        }
        let directory = open_absolute_directory_create(root).map_err(|error| {
            GeneratorError::io("open output root without following links", root, &error)
        })?;
        let opened = directory
            .metadata()
            .map_err(|error| GeneratorError::io("inspect opened output root", root, &error))?;
        let observed = fs::symlink_metadata(root)
            .map_err(|error| GeneratorError::io("reinspect output root", root, &error))?;
        if observed.file_type().is_symlink()
            || !observed.is_dir()
            || !same_object_metadata(&opened, &observed)
        {
            return Err(GeneratorError::usage(format!(
                "output root changed while opening it: {}",
                root.display()
            )));
        }
        Ok(Self {
            directory,
            display_path: root.to_path_buf(),
            path_bound: true,
        })
    }

    /// Open the output path against the prefix identity captured during
    /// planning. Missing output components are created relative to that
    /// pinned prefix and must still be absent when generation begins.
    pub(crate) fn open_output_bound(
        root: &Path,
        binding: &OutputPathBinding,
    ) -> Result<Self, GeneratorError> {
        if binding.root_path != root {
            return Err(path_error("output path differs from reviewed plan", root));
        }
        let mut current = open_absolute_directory(&binding.existing_prefix)
            .map_err(|_| path_error("output path changed after planning", root))?;
        let prefix_metadata = current.metadata().map_err(|error| {
            GeneratorError::io(
                "inspect reviewed output prefix",
                &binding.existing_prefix,
                &error,
            )
        })?;
        if prefix_metadata.dev() != binding.device || prefix_metadata.ino() != binding.inode {
            return Err(path_error("output path changed after planning", root));
        }

        let mut display_path = binding.existing_prefix.clone();
        for component in &binding.missing_suffix {
            match rustix::fs::mkdirat(&current, component, rustix::fs::Mode::from_raw_mode(0o755)) {
                Ok(()) => (),
                Err(rustix::io::Errno::EXIST) => {
                    return Err(path_error("output path changed after planning", root));
                }
                Err(error) => {
                    let error = io::Error::from(error);
                    return Err(GeneratorError::io(
                        "create reviewed output path",
                        &display_path.join(component),
                        &error,
                    ));
                }
            }
            let descriptor = rustix::fs::openat(
                &current,
                component,
                DIRECTORY_FLAGS,
                rustix::fs::Mode::empty(),
            )
            .map_err(|error| {
                let error = io::Error::from(error);
                GeneratorError::io(
                    "open reviewed output path without following links",
                    &display_path.join(component),
                    &error,
                )
            })?;
            let directory: fs::File = descriptor.into();
            let observed =
                rustix::fs::statat(&current, component, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(|error| {
                        let error = io::Error::from(error);
                        GeneratorError::io(
                            "inspect reviewed output path",
                            &display_path.join(component),
                            &error,
                        )
                    })?;
            let opened = rustix::fs::fstat(&directory).map_err(|error| {
                let error = io::Error::from(error);
                GeneratorError::io(
                    "inspect opened reviewed output path",
                    &display_path.join(component),
                    &error,
                )
            })?;
            if !same_stat_object(&observed, &opened) {
                return Err(path_error(
                    "output path changed while opening",
                    &display_path.join(component),
                ));
            }
            current = directory;
            display_path.push(component);
        }

        let output = Self {
            directory: current,
            display_path: root.to_path_buf(),
            path_bound: true,
        };
        output.validate_root_binding()?;
        Ok(output)
    }

    /// Open an already-existing output root without creating it. `None` means
    /// the root itself or one of its ancestors is absent. A symlink introduced
    /// after output resolution changes `resolved` and is rejected.
    pub(crate) fn open_existing_output(root: &Path) -> Result<Option<Self>, GeneratorError> {
        let resolved = super::canonicalize_existing_output_prefix(root)?;
        if resolved != root {
            return Err(GeneratorError::usage(format!(
                "output path changed after resolution: {} now resolves to {}",
                root.display(),
                resolved.display()
            )));
        }
        let observed = match fs::symlink_metadata(root) {
            Ok(observed) => observed,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(GeneratorError::io("inspect output root", root, &error)),
        };
        if observed.file_type().is_symlink() || !observed.is_dir() {
            return Err(GeneratorError::usage(format!(
                "refusing non-directory output root: {}",
                root.display()
            )));
        }
        let directory = open_absolute_directory(root).map_err(|error| {
            GeneratorError::io("open output root without following links", root, &error)
        })?;
        let opened = directory
            .metadata()
            .map_err(|error| GeneratorError::io("inspect opened output root", root, &error))?;
        if !same_object_metadata(&observed, &opened) {
            return Err(GeneratorError::usage(format!(
                "output root changed while opening it: {}",
                root.display()
            )));
        }
        Ok(Some(Self {
            directory,
            display_path: root.to_path_buf(),
            path_bound: true,
        }))
    }

    /// Reopen the existing output root only if it still matches the path
    /// identity captured during planning. If the captured path was absent,
    /// return `None` only while every component of that absent suffix remains
    /// absent beneath the same pinned prefix.
    pub(crate) fn open_existing_output_bound(
        root: &Path,
        binding: &OutputPathBinding,
    ) -> Result<Option<Self>, GeneratorError> {
        if binding.root_path != root {
            return Err(path_error("output path differs from reviewed plan", root));
        }
        let directory = open_absolute_directory(&binding.existing_prefix)
            .map_err(|_| path_error("output path changed after planning", root))?;
        let metadata = directory.metadata().map_err(|error| {
            GeneratorError::io(
                "inspect reviewed output prefix",
                &binding.existing_prefix,
                &error,
            )
        })?;
        if metadata.dev() != binding.device || metadata.ino() != binding.inode {
            return Err(path_error("output path changed after planning", root));
        }
        let current = Self {
            directory,
            display_path: binding.existing_prefix.clone(),
            path_bound: true,
        };
        if binding.missing_suffix.is_empty() {
            current.validate_root_binding()?;
            return Ok(Some(current));
        }
        for component in &binding.missing_suffix {
            match current.open_child(component)? {
                ChildEntry::Missing => {
                    current.validate_root_binding()?;
                    return Ok(None);
                }
                ChildEntry::File
                | ChildEntry::Directory(_)
                | ChildEntry::Symlink
                | ChildEntry::Other => {
                    return Err(path_error("output path changed after planning", root));
                }
            }
        }
        Err(path_error("output path changed after planning", root))
    }

    pub(crate) fn lock(&self, root: &Path) -> Result<(), GeneratorError> {
        self.directory
            .lock()
            .map_err(|error| GeneratorError::io("lock output root for generation", root, &error))
    }

    /// Git accepts its working directory by path through `std::process`.
    /// Callers validate this binding before and after each subprocess, then
    /// open every returned path through this pinned directory handle.
    pub(crate) fn command_directory(&self) -> &Path {
        &self.display_path
    }

    pub(crate) fn entries(&self) -> Result<Vec<SafeDirEntry>, GeneratorError> {
        let entries = rustix::fs::Dir::read_from(&self.directory).map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("read repository directory", &self.display_path, &error)
        })?;
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                let error = io::Error::from(error);
                GeneratorError::io(
                    "read repository directory entry",
                    &self.display_path,
                    &error,
                )
            })?;
            let name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
            if name == "." || name == ".." {
                continue;
            }
            match self.open_child(&name)? {
                ChildEntry::Missing => (),
                ChildEntry::File => result.push(SafeDirEntry {
                    name,
                    kind: SafeEntryKind::File,
                }),
                ChildEntry::Directory(directory) => result.push(SafeDirEntry {
                    name,
                    kind: SafeEntryKind::Directory(directory),
                }),
                ChildEntry::Symlink => result.push(SafeDirEntry {
                    name,
                    kind: SafeEntryKind::Symlink,
                }),
                ChildEntry::Other => result.push(SafeDirEntry {
                    name,
                    kind: SafeEntryKind::Other,
                }),
            }
        }
        Ok(result)
    }

    pub(crate) fn open_directory(&self, relative: &Path) -> Result<Option<Self>, GeneratorError> {
        let components = relative_components(relative)?;
        if components.is_empty() {
            return Ok(Some(self.try_clone()?));
        }
        let mut current = self.try_clone()?;
        for component in components {
            match current.open_child(&component)? {
                ChildEntry::Directory(directory) => current = directory,
                ChildEntry::Missing => return Ok(None),
                ChildEntry::Symlink => {
                    return Err(path_error("refusing symlinked path ancestor", relative));
                }
                ChildEntry::File | ChildEntry::Other => {
                    return Err(path_error("path ancestor is not a directory", relative));
                }
            }
        }
        Ok(Some(current))
    }

    /// Open a direct directory child without resolving its name through the
    /// display path. `None` means the child is absent.
    pub(crate) fn open_directory_child(
        &self,
        name: &OsStr,
    ) -> Result<Option<Self>, GeneratorError> {
        validate_child_name(name, &self.display_path)?;
        match self.open_child(name)? {
            ChildEntry::Missing => Ok(None),
            ChildEntry::Directory(directory) => Ok(Some(directory)),
            ChildEntry::Symlink => Err(path_error(
                "refusing symlinked directory child",
                &self.display_path.join(name),
            )),
            ChildEntry::File | ChildEntry::Other => Err(path_error(
                "directory child is not a directory",
                &self.display_path.join(name),
            )),
        }
    }

    /// Create a new 0700 directory child and return the handle opened from the
    /// pinned parent. Existing entries are rejected so callers can use a
    /// unique scratch name without accepting a substitute.
    pub(crate) fn create_directory_child(&self, name: &OsStr) -> Result<Self, GeneratorError> {
        validate_child_name(name, &self.display_path)?;
        self.validate_root_binding()?;
        let path = self.display_path.join(name);
        rustix::fs::mkdirat(
            &self.directory,
            name,
            rustix::fs::Mode::from_raw_mode(0o700),
        )
        .map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("create pinned scratch directory", &path, &error)
        })?;

        let created =
            rustix::fs::statat(&self.directory, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| {
                    let error = io::Error::from(error);
                    GeneratorError::io("inspect created scratch directory", &path, &error)
                })?;
        if rustix::fs::FileType::from_raw_mode(created.st_mode) != rustix::fs::FileType::Directory {
            return Err(path_error(
                "created scratch directory was replaced; leaving replacement untouched",
                &path,
            ));
        }
        let directory = match self.open_child(name)? {
            ChildEntry::Directory(directory) => directory,
            _ => {
                return Err(path_error(
                    "created scratch directory changed while opening",
                    &path,
                ));
            }
        };
        let opened = rustix::fs::fstat(&directory.directory).map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("inspect opened scratch directory", &path, &error)
        })?;
        if !same_stat_object(&created, &opened) {
            return Err(path_error(
                "created scratch directory changed while opening",
                &path,
            ));
        }
        self.validate_root_binding()?;
        directory.validate_root_binding()?;
        Ok(directory)
    }

    /// Remove a named directory tree only if it is still the exact directory
    /// captured by `expected`. The parent and every descendant stay anchored
    /// by open directory handles; this never calls `remove_dir_all` or follows
    /// a symlink. POSIX has no unlink-by-inode operation, so a same-user rename
    /// can still race the final identity check and `unlinkat`.
    pub(crate) fn remove_named_tree_if_matches(
        &self,
        name: &OsStr,
        expected: &SafeRootIdentity,
    ) -> Result<(), GeneratorError> {
        self.remove_named_tree_if_matches_inner(name, expected, || Ok(()))
    }

    #[cfg(test)]
    fn remove_named_tree_if_matches_with_hook<F>(
        &self,
        name: &OsStr,
        expected: &SafeRootIdentity,
        after_contents_removed: F,
    ) -> Result<(), GeneratorError>
    where
        F: FnOnce() -> Result<(), GeneratorError>,
    {
        self.remove_named_tree_if_matches_inner(name, expected, after_contents_removed)
    }

    fn remove_named_tree_if_matches_inner<F>(
        &self,
        name: &OsStr,
        expected: &SafeRootIdentity,
        after_contents_removed: F,
    ) -> Result<(), GeneratorError>
    where
        F: FnOnce() -> Result<(), GeneratorError>,
    {
        validate_child_name(name, &self.display_path)?;
        let path = self.display_path.join(name);
        if expected.canonical_path != path {
            return Err(path_error(
                "captured directory identity does not belong to pinned parent",
                &path,
            ));
        }
        self.validate_root_binding()?;
        let root = match self.open_child(name)? {
            ChildEntry::Missing => return Ok(()),
            ChildEntry::Directory(root) => root,
            ChildEntry::File | ChildEntry::Symlink | ChildEntry::Other => {
                return Err(path_error(
                    "scratch directory changed; leaving replacement untouched",
                    &path,
                ));
            }
        };
        let opened_identity = root.identity()?;
        if opened_identity != *expected {
            return Err(path_error(
                "scratch directory changed; leaving replacement untouched",
                &path,
            ));
        }
        root.validate_root_binding()?;
        root.remove_tree_contents()?;
        // The test hook models a same-user swap after the captured root's
        // contents were cleaned but before the final directory entry checks.
        after_contents_removed()?;

        // Revalidate both displayed bindings before removing the root entry.
        // The final lstat/unlinkat pair remains subject to the POSIX same-user
        // rename race documented above.
        self.validate_root_binding()?;
        root.validate_root_binding()?;
        let current = match rustix::fs::statat(
            &self.directory,
            name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => {
                let error = io::Error::from(error);
                return Err(GeneratorError::io(
                    "inspect scratch directory before cleanup",
                    &path,
                    &error,
                ));
            }
        };
        if rustix::fs::FileType::from_raw_mode(current.st_mode) != rustix::fs::FileType::Directory
            || !expected.matches_stat(&current)
        {
            return Err(path_error(
                "scratch directory changed; leaving replacement untouched",
                &path,
            ));
        }
        self.validate_root_binding()?;
        root.validate_root_binding()?;
        rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::REMOVEDIR).map_err(
            |error| {
                let error = io::Error::from(error);
                GeneratorError::io("remove pinned scratch directory", &path, &error)
            },
        )
    }

    fn remove_tree_contents(&self) -> Result<(), GeneratorError> {
        self.validate_root_binding()?;
        for entry in self.entries()? {
            match entry.kind {
                SafeEntryKind::Directory(directory) => {
                    let expected = directory.identity()?;
                    self.remove_named_tree_if_matches(&entry.name, &expected)?;
                }
                SafeEntryKind::File | SafeEntryKind::Symlink | SafeEntryKind::Other => {
                    self.remove_leaf_if_not_directory(&entry.name)?;
                }
            }
        }
        Ok(())
    }

    fn remove_leaf_if_not_directory(&self, name: &OsStr) -> Result<(), GeneratorError> {
        let path = self.display_path.join(name);
        self.validate_root_binding()?;
        let current = match rustix::fs::statat(
            &self.directory,
            name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => {
                let error = io::Error::from(error);
                return Err(GeneratorError::io(
                    "inspect scratch entry before cleanup",
                    &path,
                    &error,
                ));
            }
        };
        if rustix::fs::FileType::from_raw_mode(current.st_mode) == rustix::fs::FileType::Directory {
            return Err(path_error(
                "scratch entry changed to a directory during cleanup",
                &path,
            ));
        }
        self.validate_root_binding()?;
        rustix::fs::unlinkat(&self.directory, name, rustix::fs::AtFlags::empty()).map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("remove pinned scratch entry", &path, &error)
        })
    }

    pub(crate) fn validate_parent(&self, relative: &Path) -> Result<(), GeneratorError> {
        let _ = self.open_parent(relative, false)?;
        Ok(())
    }

    /// True only when each path component was opened without following a
    /// symlink and the leaf is a regular file.
    pub(crate) fn has_regular_file(&self, relative: &Path) -> Result<bool, GeneratorError> {
        let components = relative_components(relative)?;
        if components.is_empty() {
            return Ok(false);
        }
        let mut current = self.try_clone()?;
        for (index, component) in components.iter().enumerate() {
            match current.open_child(component)? {
                ChildEntry::Directory(directory) if index + 1 < components.len() => {
                    current = directory;
                }
                ChildEntry::File if index + 1 == components.len() => return Ok(true),
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    pub(crate) fn capture_file_preimage(
        &self,
        relative: &Path,
    ) -> Result<FilePreimage, GeneratorError> {
        let Some(parent) = self.open_parent(relative, false)? else {
            return Ok(FilePreimage::Missing);
        };
        parent.capture_file_preimage()
    }

    #[cfg(test)]
    pub(crate) fn capture_file_preimage_with_hook<F>(
        &self,
        relative: &Path,
        after_open: F,
    ) -> Result<FilePreimage, GeneratorError>
    where
        F: FnOnce() -> Result<(), GeneratorError>,
    {
        let Some(parent) = self.open_parent(relative, false)? else {
            return Ok(FilePreimage::Missing);
        };
        parent.capture_file_preimage_with_hook(after_open)
    }

    pub(crate) fn read_file(&self, relative: &Path) -> Result<Vec<u8>, GeneratorError> {
        let preimage = self.capture_file_preimage(relative)?;
        match preimage {
            FilePreimage::Missing => Err(GeneratorError::io(
                "read repository file",
                &self.display_path.join(relative),
                &io::Error::from(io::ErrorKind::NotFound),
            )),
            FilePreimage::Regular { bytes, .. } => Ok(bytes.into_vec()),
        }
    }

    pub(crate) fn read_file_if_exists(
        &self,
        relative: &Path,
    ) -> Result<Option<Vec<u8>>, GeneratorError> {
        match self.capture_file_preimage(relative)? {
            FilePreimage::Missing => Ok(None),
            FilePreimage::Regular { bytes, .. } => Ok(Some(bytes.into_vec())),
        }
    }

    fn open_parent(
        &self,
        relative: &Path,
        create: bool,
    ) -> Result<Option<SafeParent>, GeneratorError> {
        let components = relative_components(relative)?;
        let Some((name, parents)) = components.split_last() else {
            return Err(GeneratorError::usage(format!(
                "repository path has no file name: {}",
                relative.display()
            )));
        };
        let mut current = self.try_clone()?;
        for component in parents {
            match current.open_child(component)? {
                ChildEntry::Directory(directory) => current = directory,
                ChildEntry::Missing if create => {
                    current.create_directory(component, relative)?;
                    match current.open_child(component)? {
                        ChildEntry::Directory(directory) => current = directory,
                        _ => {
                            return Err(path_error("managed ancestor is not a directory", relative))
                        }
                    }
                }
                ChildEntry::Missing => return Ok(None),
                ChildEntry::Symlink => {
                    return Err(path_error("refusing symlinked path ancestor", relative));
                }
                ChildEntry::File | ChildEntry::Other => {
                    return Err(path_error("path ancestor is not a directory", relative));
                }
            }
        }
        let display_path = current.display_path.clone();
        Ok(Some(SafeParent {
            directory: current.directory,
            display_path,
            name: name.clone(),
            path_bound: current.path_bound,
        }))
    }

    fn open_child(&self, name: &OsStr) -> Result<ChildEntry, GeneratorError> {
        validate_child_name(name, &self.display_path)?;
        let display = self.display_path.join(name);
        let observed = match rustix::fs::statat(
            &self.directory,
            name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(ChildEntry::Missing),
            Err(error) => {
                let error = io::Error::from(error);
                return Err(GeneratorError::io(
                    "inspect repository entry",
                    &display,
                    &error,
                ));
            }
        };
        match rustix::fs::FileType::from_raw_mode(observed.st_mode) {
            rustix::fs::FileType::Symlink => Ok(ChildEntry::Symlink),
            rustix::fs::FileType::Directory => {
                let descriptor = rustix::fs::openat(
                    &self.directory,
                    name,
                    DIRECTORY_FLAGS,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|error| {
                    let error = io::Error::from(error);
                    GeneratorError::io(
                        "open repository directory without following links",
                        &display,
                        &error,
                    )
                })?;
                let directory: fs::File = descriptor.into();
                let opened = rustix::fs::fstat(&directory).map_err(|error| {
                    let error = io::Error::from(error);
                    GeneratorError::io("inspect opened repository directory", &display, &error)
                })?;
                if !same_stat_object(&observed, &opened) {
                    return Err(path_error(
                        "repository directory changed while opening",
                        &display,
                    ));
                }
                Ok(ChildEntry::Directory(Self {
                    directory,
                    display_path: display,
                    path_bound: self.path_bound,
                }))
            }
            rustix::fs::FileType::RegularFile => {
                let descriptor = rustix::fs::openat(
                    &self.directory,
                    name,
                    FILE_FLAGS,
                    rustix::fs::Mode::empty(),
                )
                .map_err(|error| {
                    let error = io::Error::from(error);
                    GeneratorError::io(
                        "open repository file without following links",
                        &display,
                        &error,
                    )
                })?;
                let file: fs::File = descriptor.into();
                let opened = rustix::fs::fstat(&file).map_err(|error| {
                    let error = io::Error::from(error);
                    GeneratorError::io("inspect opened repository file", &display, &error)
                })?;
                if !same_stat_object(&observed, &opened) {
                    return Err(path_error(
                        "repository file changed while opening",
                        &display,
                    ));
                }
                Ok(ChildEntry::File)
            }
            _ => Ok(ChildEntry::Other),
        }
    }

    fn create_directory(&self, name: &OsStr, relative: &Path) -> Result<(), GeneratorError> {
        match rustix::fs::mkdirat(
            &self.directory,
            name,
            rustix::fs::Mode::from_raw_mode(0o755),
        ) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => Ok(()),
            Err(error) => {
                let error = io::Error::from(error);
                Err(GeneratorError::io(
                    "create managed output directory",
                    &self.display_path.join(relative),
                    &error,
                ))
            }
        }
    }

    fn try_clone(&self) -> Result<Self, GeneratorError> {
        Ok(Self {
            directory: self.directory.try_clone().map_err(|error| {
                GeneratorError::io(
                    "duplicate repository directory handle",
                    &self.display_path,
                    &error,
                )
            })?,
            display_path: self.display_path.clone(),
            path_bound: self.path_bound,
        })
    }

    fn same_path_parent(&self, parent: &SafeParent, relative: &Path) -> Result<(), GeneratorError> {
        self.validate_root_binding()?;
        let Some(current) = self.open_parent(relative, false)? else {
            return Err(path_error(
                "output parent disappeared after preflight",
                relative,
            ));
        };
        let pinned = parent.directory.metadata().map_err(|error| {
            GeneratorError::io("inspect pinned output parent", &parent.display_path, &error)
        })?;
        let current = current.directory.metadata().map_err(|error| {
            GeneratorError::io(
                "inspect current output parent",
                &current.display_path,
                &error,
            )
        })?;
        if !same_object_metadata(&pinned, &current) {
            return Err(path_error(
                "output parent changed after preflight",
                relative,
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_root_binding(&self) -> Result<(), GeneratorError> {
        if !self.path_bound {
            let metadata = self.directory.metadata().map_err(|error| {
                GeneratorError::io(
                    "inspect inherited directory handle",
                    &self.display_path,
                    &error,
                )
            })?;
            if !metadata.is_dir() {
                return Err(path_error(
                    "inherited repository root is not a directory",
                    &self.display_path,
                ));
            }
            return Ok(());
        }
        let current = open_absolute_directory(&self.display_path).map_err(|error| {
            GeneratorError::io(
                "reopen repository root without following links",
                &self.display_path,
                &error,
            )
        })?;
        let pinned = self.directory.metadata().map_err(|error| {
            GeneratorError::io("inspect pinned repository root", &self.display_path, &error)
        })?;
        let current = current.metadata().map_err(|error| {
            GeneratorError::io(
                "inspect current repository root",
                &self.display_path,
                &error,
            )
        })?;
        if !same_object_metadata(&pinned, &current) {
            return Err(path_error(
                "repository root changed since it was opened",
                &self.display_path,
            ));
        }
        Ok(())
    }
}

impl SafeParent {
    fn capture_file_preimage(&self) -> Result<FilePreimage, GeneratorError> {
        self.capture_file_preimage_with_hook(|| Ok(()))
    }

    fn capture_file_preimage_with_hook<F>(
        &self,
        after_open: F,
    ) -> Result<FilePreimage, GeneratorError>
    where
        F: FnOnce() -> Result<(), GeneratorError>,
    {
        let path = self.display_path.join(&self.name);
        let observed = match rustix::fs::statat(
            &self.directory,
            &self.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(FilePreimage::Missing),
            Err(error) => {
                let error = io::Error::from(error);
                return Err(GeneratorError::io("inspect generated file", &path, &error));
            }
        };
        let file_type = rustix::fs::FileType::from_raw_mode(observed.st_mode);
        if file_type == rustix::fs::FileType::Symlink {
            return Err(path_error("refusing symlinked repository file", &path));
        }
        if file_type != rustix::fs::FileType::RegularFile {
            return Err(GeneratorError::usage(format!(
                "refusing non-regular generated file: {}",
                path.display()
            )));
        }
        let descriptor = rustix::fs::openat(
            &self.directory,
            &self.name,
            FILE_FLAGS,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("open generated file without following links", &path, &error)
        })?;
        let mut file: fs::File = descriptor.into();
        let opened = rustix::fs::fstat(&file).map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("inspect opened generated file", &path, &error)
        })?;
        if !same_stat_object(&observed, &opened) {
            return Err(path_error("generated file changed during preflight", &path));
        }
        let opened_metadata = file
            .metadata()
            .map_err(|error| GeneratorError::io("inspect generated file handle", &path, &error))?;
        let identity = super::file_identity(&opened_metadata);
        let opened_version = file_version(&opened_metadata);
        after_open()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| GeneratorError::io("read generated file", &path, &error))?;
        let final_metadata = file.metadata().map_err(|error| {
            GeneratorError::io("reinspect generated file handle", &path, &error)
        })?;
        let final_entry = rustix::fs::statat(
            &self.directory,
            &self.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("reinspect generated file path", &path, &error)
        })?;
        if file_version(&final_metadata) != opened_version
            || file_version_from_stat(&final_entry) != opened_version
            || !same_stat_object(&opened, &final_entry)
            || rustix::fs::FileType::from_raw_mode(final_entry.st_mode)
                != rustix::fs::FileType::RegularFile
            || super::file_identity(&final_metadata) != identity
            || identity.length != bytes.len() as u64
        {
            return Err(path_error("generated file changed during preflight", &path));
        }
        Ok(FilePreimage::Regular {
            identity,
            bytes: bytes.into_boxed_slice(),
            _identity_hold: Some(Arc::new(file)),
        })
    }

    fn stage_file(
        &self,
        relative: &Path,
        content: &str,
    ) -> Result<(StageFileGuard<'_>, FilePreimage), GeneratorError> {
        self.stage_file_with(relative, content, |file, bytes| {
            file.write_all(bytes)?;
            file.sync_all()
        })
    }

    fn stage_file_with<F>(
        &self,
        relative: &Path,
        content: &str,
        mut write_and_sync: F,
    ) -> Result<(StageFileGuard<'_>, FilePreimage), GeneratorError>
    where
        F: FnMut(&mut fs::File, &[u8]) -> io::Result<()>,
    {
        let filename = self.name.to_string_lossy();
        for attempt in 0..16_u8 {
            let staged = OsString::from(format!(
                ".{filename}.stage-{}-{}-{attempt}",
                std::process::id(),
                unique_suffix()
            ));
            let descriptor = match rustix::fs::openat(
                &self.directory,
                &staged,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::from_raw_mode(0o666),
            ) {
                Ok(descriptor) => descriptor,
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => {
                    let error = io::Error::from(error);
                    return Err(GeneratorError::io(
                        "create staged generated file",
                        relative,
                        &error,
                    ));
                }
            };
            let file: fs::File = descriptor.into();
            let mut guard = StageFileGuard {
                parent: self,
                file,
                name: staged,
                identity: None,
                armed: true,
            };
            let opened = rustix::fs::fstat(&guard.file).map_err(|error| {
                let error = io::Error::from(error);
                GeneratorError::io("inspect staged generated file", relative, &error)
            })?;
            guard.identity = Some(StageIdentity::from_stat(&opened));

            if let Err(error) = write_and_sync(&mut guard.file, content.as_bytes()) {
                return Err(GeneratorError::io(
                    "write staged generated file",
                    relative,
                    &error,
                ));
            }
            let metadata = guard.file.metadata().map_err(|error| {
                GeneratorError::io("inspect staged generated file", relative, &error)
            })?;
            let identity = super::file_identity(&metadata);
            let version = file_version(&metadata);
            if identity.length != content.len() as u64 {
                return Err(path_error(
                    "staged generated file changed after write",
                    relative,
                ));
            }
            guard.file.seek(SeekFrom::Start(0)).map_err(|error| {
                GeneratorError::io("rewind staged generated file", relative, &error)
            })?;
            let mut bytes = Vec::new();
            guard.file.read_to_end(&mut bytes).map_err(|error| {
                GeneratorError::io("read staged generated file", relative, &error)
            })?;
            let final_metadata = guard.file.metadata().map_err(|error| {
                GeneratorError::io("reinspect staged generated file", relative, &error)
            })?;
            let final_entry = rustix::fs::statat(
                &self.directory,
                &guard.name,
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|error| {
                let error = io::Error::from(error);
                GeneratorError::io("reinspect staged generated file path", relative, &error)
            })?;
            if file_version(&final_metadata) != version
                || super::file_identity(&final_metadata) != identity
                || !guard.identity.is_some_and(|stage| {
                    stage.device == identity.device && stage.inode == identity.inode
                })
                || rustix::fs::FileType::from_raw_mode(final_entry.st_mode)
                    != rustix::fs::FileType::RegularFile
                || !same_stat_object(&opened, &final_entry)
                || file_version_from_stat(&final_entry) != version
                || bytes.len() as u64 != identity.length
                || bytes.as_slice() != content.as_bytes()
            {
                return Err(path_error(
                    "staged generated file changed after write",
                    relative,
                ));
            }
            return Ok((
                guard,
                FilePreimage::Regular {
                    identity,
                    bytes: bytes.into_boxed_slice(),
                    _identity_hold: None,
                },
            ));
        }
        Err(GeneratorError::usage(format!(
            "could not reserve a staged file for {}",
            relative.display()
        )))
    }

    fn reserve_backup_directory(
        &self,
        relative: &Path,
    ) -> Result<(OsString, SafeRoot), GeneratorError> {
        self.reserve_backup_directory_inner(relative, |_| Ok(()))
    }

    #[cfg(test)]
    fn reserve_backup_directory_with_hook<F>(
        &self,
        relative: &Path,
        after_create: F,
    ) -> Result<(OsString, SafeRoot), GeneratorError>
    where
        F: FnOnce(&OsStr) -> Result<(), GeneratorError>,
    {
        self.reserve_backup_directory_inner(relative, after_create)
    }

    fn reserve_backup_directory_inner<F>(
        &self,
        relative: &Path,
        after_create: F,
    ) -> Result<(OsString, SafeRoot), GeneratorError>
    where
        F: FnOnce(&OsStr) -> Result<(), GeneratorError>,
    {
        let mut after_create = Some(after_create);
        for attempt in 0..16_u8 {
            let name = OsString::from(format!(
                ".velnor-workflow-backup-{}-{}-{attempt}",
                std::process::id(),
                unique_suffix()
            ));
            match rustix::fs::mkdirat(
                &self.directory,
                &name,
                rustix::fs::Mode::from_raw_mode(0o700),
            ) {
                Ok(()) => {
                    let created = rustix::fs::statat(
                        &self.directory,
                        &name,
                        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                    )
                    .map_err(|error| {
                        let error = io::Error::from(error);
                        GeneratorError::io(
                            "inspect reserved generated backup directory",
                            relative,
                            &error,
                        )
                    })?;
                    if rustix::fs::FileType::from_raw_mode(created.st_mode)
                        != rustix::fs::FileType::Directory
                    {
                        return Err(path_error(
                            "generated backup directory changed while reserving",
                            relative,
                        ));
                    }
                    if let Some(after_create) = after_create.take() {
                        after_create(&name)?;
                    }
                    let descriptor = rustix::fs::openat(
                        &self.directory,
                        &name,
                        DIRECTORY_FLAGS,
                        rustix::fs::Mode::empty(),
                    )
                    .map_err(|error| {
                        let error = io::Error::from(error);
                        GeneratorError::io("open generated backup directory", relative, &error)
                    })?;
                    let directory: fs::File = descriptor.into();
                    let opened = rustix::fs::fstat(&directory).map_err(|error| {
                        let error = io::Error::from(error);
                        GeneratorError::io(
                            "inspect opened generated backup directory",
                            relative,
                            &error,
                        )
                    })?;
                    if !same_stat_object(&created, &opened) {
                        return Err(path_error(
                            "generated backup directory changed while opening",
                            relative,
                        ));
                    }
                    return Ok((
                        name.clone(),
                        SafeRoot {
                            directory,
                            display_path: self.display_path.join(name),
                            path_bound: self.path_bound,
                        },
                    ));
                }
                Err(rustix::io::Errno::EXIST) => (),
                Err(error) => {
                    let error = io::Error::from(error);
                    return Err(GeneratorError::io(
                        "reserve generated backup directory",
                        relative,
                        &error,
                    ));
                }
            }
        }
        Err(GeneratorError::usage(format!(
            "could not reserve a backup for {}",
            relative.display()
        )))
    }

    fn remove_backup_directory_if_matches(
        &self,
        backup_name: &OsStr,
        backup: &SafeRoot,
    ) -> Result<(), GeneratorError> {
        let path = self.display_path.join(backup_name);
        let pinned = rustix::fs::fstat(&backup.directory).map_err(|error| {
            let error = io::Error::from(error);
            GeneratorError::io("inspect pinned generated backup directory", &path, &error)
        })?;
        let current = match rustix::fs::statat(
            &self.directory,
            backup_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(()),
            Err(error) => {
                let error = io::Error::from(error);
                return Err(GeneratorError::io(
                    "inspect generated backup directory before cleanup",
                    &path,
                    &error,
                ));
            }
        };
        if rustix::fs::FileType::from_raw_mode(current.st_mode) != rustix::fs::FileType::Directory
            || !same_stat_object(&pinned, &current)
        {
            return Err(path_error(
                "generated backup directory changed; leaving replacement untouched",
                &path,
            ));
        }
        // POSIX has no unlink-by-inode operation. This refuses an already
        // observed substitution; a same-user rename can still race unlinkat.
        rustix::fs::unlinkat(&self.directory, backup_name, rustix::fs::AtFlags::REMOVEDIR).map_err(
            |error| {
                let error = io::Error::from(error);
                GeneratorError::io("remove generated backup directory", &path, &error)
            },
        )
    }

    fn cleanup_stage_if_matches(&self, staged: &OsStr, expected: &FilePreimage) {
        let Ok(directory) = self.directory.try_clone() else {
            return;
        };
        let staged_parent = SafeParent {
            directory,
            display_path: self.display_path.clone(),
            name: staged.to_os_string(),
            path_bound: self.path_bound,
        };
        if staged_parent
            .capture_file_preimage()
            .is_ok_and(|actual| actual == *expected)
        {
            let _ = rustix::fs::unlinkat(&self.directory, staged, rustix::fs::AtFlags::empty());
        }
    }

    fn cleanup_backup(
        &self,
        backup_name: &OsStr,
        backup: &SafeRoot,
        leaf_name: &OsStr,
        expected: &FilePreimage,
    ) {
        if let Ok(backup_directory) = backup.directory.try_clone() {
            let backup_parent = SafeParent {
                directory: backup_directory,
                display_path: backup.display_path.clone(),
                name: leaf_name.to_os_string(),
                path_bound: backup.path_bound,
            };
            if backup_parent
                .capture_file_preimage()
                .is_ok_and(|actual| actual == *expected)
            {
                let _ = rustix::fs::unlinkat(
                    &backup_parent.directory,
                    leaf_name,
                    rustix::fs::AtFlags::empty(),
                );
            }
        }
        let _ = self.remove_backup_directory_if_matches(backup_name, backup);
    }

    fn sync(&self, relative: &Path) -> Result<(), GeneratorError> {
        self.directory.sync_all().map_err(|error| {
            GeneratorError::io("sync generated output directory", relative, &error)
        })
    }
}

enum ChildEntry {
    Missing,
    File,
    Directory(SafeRoot),
    Symlink,
    Other,
}

pub(crate) fn write_reviewed_file_observed<F, C>(
    root: &SafeRoot,
    relative: &Path,
    content: &str,
    expected: &FilePreimage,
    before_replace: F,
    before_commit: C,
) -> Result<(), GeneratorError>
where
    F: FnOnce(&Path) -> Result<(), GeneratorError>,
    C: FnOnce(&Path) -> Result<(), GeneratorError>,
{
    write_reviewed_file_observed_after_install(
        root,
        relative,
        content,
        expected,
        before_replace,
        before_commit,
        |_| Ok(()),
    )
}

#[cfg(test)]
fn write_reviewed_file_with_after_install<F, C, A>(
    root: &SafeRoot,
    relative: &Path,
    content: &str,
    expected: &FilePreimage,
    before_replace: F,
    before_commit: C,
    after_install: A,
) -> Result<(), GeneratorError>
where
    F: FnOnce(&Path) -> Result<(), GeneratorError>,
    C: FnOnce(&Path) -> Result<(), GeneratorError>,
    A: FnOnce(&Path) -> Result<(), GeneratorError>,
{
    write_reviewed_file_observed_after_install(
        root,
        relative,
        content,
        expected,
        before_replace,
        before_commit,
        after_install,
    )
}

fn write_reviewed_file_observed_after_install<F, C, A>(
    root: &SafeRoot,
    relative: &Path,
    content: &str,
    expected: &FilePreimage,
    before_replace: F,
    before_commit: C,
    after_install: A,
) -> Result<(), GeneratorError>
where
    F: FnOnce(&Path) -> Result<(), GeneratorError>,
    C: FnOnce(&Path) -> Result<(), GeneratorError>,
    A: FnOnce(&Path) -> Result<(), GeneratorError>,
{
    let parent = root
        .open_parent(relative, true)?
        .ok_or_else(|| path_error("output parent disappeared", relative))?;
    if parent.capture_file_preimage()? != *expected {
        return Err(path_error(
            "generated file changed after preflight",
            relative,
        ));
    }
    let (mut stage_guard, staged_preimage) = parent.stage_file(relative, content)?;
    let staged = stage_guard.name().to_os_string();
    let stage_parent = SafeParent {
        directory: parent.directory.try_clone().map_err(|error| {
            GeneratorError::io("duplicate output directory handle", relative, &error)
        })?,
        display_path: parent.display_path.clone(),
        name: staged.clone(),
        path_bound: parent.path_bound,
    };
    if stage_parent.capture_file_preimage()? != staged_preimage {
        return Err(path_error(
            "staged generated file changed after write",
            relative,
        ));
    }
    let mut after_install = Some(after_install);
    let install_result = (|| {
        root.same_path_parent(&parent, relative)?;
        match expected {
            FilePreimage::Missing => {
                before_replace(&root.display_path.join(relative))?;
                root.same_path_parent(&parent, relative)?;
                if parent.capture_file_preimage()? != FilePreimage::Missing {
                    return Err(path_error(
                        "generated file appeared after preflight",
                        relative,
                    ));
                }
                if stage_parent.capture_file_preimage()? != staged_preimage {
                    return Err(path_error(
                        "staged generated file changed before install",
                        relative,
                    ));
                }
                before_commit(&parent.display_path.join(&staged))?;
                if stage_parent.capture_file_preimage()? != staged_preimage {
                    return Err(path_error(
                        "staged generated file changed before install",
                        relative,
                    ));
                }
                root.same_path_parent(&parent, relative)?;
                match rustix::fs::renameat_with(
                    &parent.directory,
                    &staged,
                    &parent.directory,
                    &parent.name,
                    rustix::fs::RenameFlags::NOREPLACE,
                ) {
                    Ok(()) => (),
                    Err(rustix::io::Errno::EXIST) => {
                        return Err(path_error(
                            "generated file appeared after preflight",
                            relative,
                        ));
                    }
                    Err(error) => {
                        let error = io::Error::from(error);
                        return Err(GeneratorError::io(
                            "install new generated file",
                            relative,
                            &error,
                        ));
                    }
                }
                let installed_path = parent.display_path.join(&parent.name);
                match parent.capture_file_preimage() {
                    Ok(actual) if actual == staged_preimage => (),
                    Ok(_) => {
                        return Err(error_with_recovery_path(
                            path_error("staged generated file changed during install", relative),
                            &installed_path,
                        ));
                    }
                    Err(error) => {
                        return Err(error_with_recovery_path(error, &installed_path));
                    }
                }
                root.same_path_parent(&parent, relative)
                    .map_err(|error| error_with_recovery_path(error, &installed_path))?;
                if let Some(after_install) = after_install.take()
                    && let Err(error) = after_install(&installed_path)
                {
                    return Err(error_with_recovery_path(error, &installed_path));
                }
                match parent.capture_file_preimage() {
                    Ok(actual) if actual == staged_preimage => (),
                    Ok(_) => {
                        return Err(error_with_recovery_path(
                            path_error("staged generated file changed during install", relative),
                            &installed_path,
                        ));
                    }
                    Err(error) => {
                        return Err(error_with_recovery_path(error, &installed_path));
                    }
                }
                if let Err(error) = root.same_path_parent(&parent, relative) {
                    // The write is already installed in the pinned parent.
                    // Do not try to roll it back through the stage name: a
                    // same-user process can replace that name between checks.
                    let recovery_path = parent.display_path.join(&parent.name);
                    return Err(error_with_recovery_path(error, &recovery_path));
                }
                // The rename is the commit point. A directory fsync failure
                // cannot be reported as a failed write after the target was
                // installed, because the caller would skip the ownership state.
                let _ = parent.sync(relative);
                Ok(())
            }
            FilePreimage::Regular { .. } => {
                let (backup_name, backup) = parent.reserve_backup_directory(relative)?;
                if let Err(error) = rustix::fs::linkat(
                    &parent.directory,
                    &parent.name,
                    &backup.directory,
                    &parent.name,
                    rustix::fs::AtFlags::empty(),
                ) {
                    let _ = parent.remove_backup_directory_if_matches(&backup_name, &backup);
                    let error = io::Error::from(error);
                    return Err(GeneratorError::io(
                        "back up reviewed generated file",
                        relative,
                        &error,
                    ));
                }
                let operation = (|| {
                    let backup_parent = SafeParent {
                        directory: backup.directory.try_clone().map_err(|error| {
                            GeneratorError::io(
                                "duplicate generated backup directory",
                                relative,
                                &error,
                            )
                        })?,
                        display_path: backup.display_path.clone(),
                        name: parent.name.clone(),
                        path_bound: backup.path_bound,
                    };
                    if backup_parent.capture_file_preimage()? != *expected
                        || parent.capture_file_preimage()? != *expected
                    {
                        return Err(path_error(
                            "generated file changed after preflight",
                            relative,
                        ));
                    }
                    before_replace(&root.display_path.join(relative))?;
                    root.same_path_parent(&parent, relative)?;
                    if parent.capture_file_preimage()? != *expected {
                        return Err(path_error(
                            "generated file changed after preflight",
                            relative,
                        ));
                    }
                    root.same_path_parent(&parent, relative)?;
                    if stage_parent.capture_file_preimage()? != staged_preimage {
                        return Err(path_error(
                            "staged generated file changed before replacement",
                            relative,
                        ));
                    }
                    before_commit(&parent.display_path.join(&staged))?;
                    if stage_parent.capture_file_preimage()? != staged_preimage {
                        return Err(path_error(
                            "staged generated file changed before replacement",
                            relative,
                        ));
                    }
                    root.same_path_parent(&parent, relative)?;
                    rustix::fs::renameat_with(
                        &parent.directory,
                        &staged,
                        &parent.directory,
                        &parent.name,
                        rustix::fs::RenameFlags::EXCHANGE,
                    )
                    .map_err(|error| {
                        let error = io::Error::from(error);
                        GeneratorError::io(
                            "atomically replace reviewed generated file",
                            relative,
                            &error,
                        )
                    })?;
                    let installed = parent.capture_file_preimage();
                    let displaced = stage_parent.capture_file_preimage();
                    let installed_is_staged = installed
                        .as_ref()
                        .is_ok_and(|actual| actual == &staged_preimage);
                    let displaced_is_reviewed =
                        displaced.as_ref().is_ok_and(|actual| actual == expected);
                    if !installed_is_staged || !displaced_is_reviewed {
                        // POSIX rename APIs address entries by name, not by
                        // open file handle. A same-user process can replace
                        // `staged` between the identity check and EXCHANGE.
                        // Never exchange that name back into the live path;
                        // keep the reviewed hard-link backup for recovery.
                        return Err(path_error(
                            "staged generated file or reviewed output changed during replacement",
                            relative,
                        ));
                    }
                    if let Some(after_install) = after_install.take() {
                        let installed_path = parent.display_path.join(&parent.name);
                        after_install(&installed_path)
                            .map_err(|error| error_with_recovery_path(error, &installed_path))?;
                    }
                    let installed_path = parent.display_path.join(&parent.name);
                    if parent.capture_file_preimage()? != staged_preimage {
                        return Err(error_with_recovery_path(
                            path_error("staged generated file changed during install", relative),
                            &installed_path,
                        ));
                    }
                    if let Err(error) = root.same_path_parent(&parent, relative) {
                        return Err(error_with_recovery_path(error, &installed_path));
                    }
                    parent.cleanup_stage_if_matches(&staged, expected);
                    parent.cleanup_backup(&backup_name, &backup, &parent.name, expected);
                    let _ = parent.sync(relative);
                    Ok(())
                })();
                // On failure, the backup may be the only remaining link to
                // the preflighted bytes (for example, if the target changed
                // during EXCHANGE). Preserve it for recovery.
                if let Err(error) = operation {
                    let recovery_path = backup.display_path.join(&parent.name);
                    return Err(error_with_recovery_path(error, &recovery_path));
                }
                Ok(())
            }
        }
    })();
    if install_result.is_ok() {
        stage_guard.disarm();
    }
    install_result
}

pub(crate) fn delete_reviewed_file(
    root: &SafeRoot,
    relative: &Path,
    expected: &FilePreimage,
) -> Result<(), GeneratorError> {
    delete_reviewed_file_inner(root, relative, expected, |_| Ok(()))
}

#[cfg(test)]
pub(crate) fn delete_reviewed_file_observed<F>(
    root: &SafeRoot,
    relative: &Path,
    expected: &FilePreimage,
    after_move: F,
) -> Result<(), GeneratorError>
where
    F: FnOnce(&Path) -> Result<(), GeneratorError>,
{
    delete_reviewed_file_inner(root, relative, expected, after_move)
}

fn delete_reviewed_file_inner<F>(
    root: &SafeRoot,
    relative: &Path,
    expected: &FilePreimage,
    after_move: F,
) -> Result<(), GeneratorError>
where
    F: FnOnce(&Path) -> Result<(), GeneratorError>,
{
    if matches!(expected, FilePreimage::Missing) {
        return Err(path_error(
            "generated file changed after preflight",
            relative,
        ));
    }
    let Some(parent) = root.open_parent(relative, false)? else {
        return Err(path_error(
            "generated file changed after preflight",
            relative,
        ));
    };
    if parent.capture_file_preimage()? != *expected {
        return Err(path_error(
            "generated file changed after preflight",
            relative,
        ));
    }
    root.same_path_parent(&parent, relative)?;
    let (backup_name, backup) = parent.reserve_backup_directory(relative)?;
    let moved_name = parent.name.clone();
    if let Err(error) = root.same_path_parent(&parent, relative) {
        parent.cleanup_backup(&backup_name, &backup, &moved_name, expected);
        return Err(error);
    }
    let moved = match rustix::fs::renameat_with(
        &parent.directory,
        &parent.name,
        &backup.directory,
        &moved_name,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => (),
        Err(error) => {
            parent.cleanup_backup(&backup_name, &backup, &moved_name, expected);
            let error = io::Error::from(error);
            return Err(GeneratorError::io(
                "reserve stale generated file",
                relative,
                &error,
            ));
        }
    };
    let _ = moved;
    if let Err(error) = after_move(&backup.display_path.join(&moved_name)) {
        return Err(error_with_restore(
            error,
            restore_reviewed_entry(&backup, &moved_name, &parent, &backup_name, relative),
        ));
    }
    let backup_directory = match backup.directory.try_clone() {
        Ok(directory) => directory,
        Err(error) => {
            let error =
                GeneratorError::io("duplicate generated backup directory", relative, &error);
            return Err(error_with_restore(
                error,
                restore_reviewed_entry(&backup, &moved_name, &parent, &backup_name, relative),
            ));
        }
    };
    let moved_parent = SafeParent {
        directory: backup_directory,
        display_path: backup.display_path.clone(),
        name: moved_name.clone(),
        path_bound: backup.path_bound,
    };
    let moved_preimage = moved_parent.capture_file_preimage();
    if !moved_preimage.is_ok_and(|actual| actual == *expected) {
        let error = path_error("stale generated file changed after preflight", relative);
        return Err(error_with_restore(
            error,
            restore_reviewed_entry(&backup, &moved_name, &parent, &backup_name, relative),
        ));
    }
    if let Err(error) = root.same_path_parent(&parent, relative) {
        return Err(error_with_restore(
            error,
            restore_reviewed_entry(&backup, &moved_name, &parent, &backup_name, relative),
        ));
    }
    match parent.capture_file_preimage() {
        Ok(FilePreimage::Missing) => (),
        Ok(_) => {
            let error = path_error("generated file appeared after stale-file removal", relative);
            return Err(error_with_restore(
                error,
                restore_reviewed_entry(&backup, &moved_name, &parent, &backup_name, relative),
            ));
        }
        Err(error) => {
            return Err(error_with_restore(
                error,
                restore_reviewed_entry(&backup, &moved_name, &parent, &backup_name, relative),
            ));
        }
    }
    if let Err(error) =
        rustix::fs::unlinkat(&backup.directory, &moved_name, rustix::fs::AtFlags::empty())
    {
        let error = io::Error::from(error);
        let error = GeneratorError::io("remove stale generated file", relative, &error);
        return Err(error_with_restore(
            error,
            restore_reviewed_entry(&backup, &moved_name, &parent, &backup_name, relative),
        ));
    }
    parent.cleanup_backup(&backup_name, &backup, &moved_name, expected);
    let _ = parent.sync(relative);
    Ok(())
}

fn restore_reviewed_entry(
    backup: &SafeRoot,
    moved_name: &OsStr,
    parent: &SafeParent,
    backup_name: &OsStr,
    relative: &Path,
) -> Result<(), GeneratorError> {
    let backup_path = backup.display_path.join(moved_name);
    rustix::fs::renameat_with(
        &backup.directory,
        moved_name,
        &parent.directory,
        &parent.name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        let error = io::Error::from(error);
        GeneratorError::usage(format!(
            "could not restore stale generated entry for {}: {error}; recovery entry remains under its pinned directory (reported path {} may be stale if the parent directory was moved or replaced)",
            relative.display(),
            backup_path.display()
        ))
    })?;

    parent
        .remove_backup_directory_if_matches(backup_name, backup)
        .map_err(|error| {
            GeneratorError::usage(format!(
                "restored stale generated entry for {} but could not remove recovery directory under its pinned root (reported path {} may be stale if the parent directory was moved or replaced): {error}",
                relative.display(),
                backup.display_path.display()
            ))
        })
}

fn error_with_restore(
    original: GeneratorError,
    recovery: Result<(), GeneratorError>,
) -> GeneratorError {
    match recovery {
        Ok(()) => original,
        Err(recovery) => GeneratorError::usage(format!("{original}; {recovery}")),
    }
}

fn error_with_recovery_path(error: GeneratorError, recovery_path: &Path) -> GeneratorError {
    GeneratorError::usage(format!(
        "{error}; recovery entry remains under its pinned output directory (reported path {} may be stale if the parent directory was moved or replaced)",
        recovery_path.display()
    ))
}

fn relative_components(relative: &Path) -> Result<Vec<OsString>, GeneratorError> {
    let raw = relative.to_str().ok_or_else(|| {
        GeneratorError::usage(format!(
            "repository path is not valid UTF-8: {}",
            relative.display()
        ))
    })?;
    let normalized = super::config::normalize_repository_relative_path(raw).ok_or_else(|| {
        GeneratorError::usage(format!(
            "unsafe repository-relative path: {}",
            relative.display()
        ))
    })?;
    let mut components = Vec::new();
    for component in normalized.components() {
        match component {
            Component::Normal(name) => components.push(name.to_os_string()),
            _ => {
                return Err(GeneratorError::usage(format!(
                    "unsafe repository-relative path: {}",
                    relative.display()
                )));
            }
        }
    }
    Ok(components)
}

fn validate_child_name(name: &OsStr, parent: &Path) -> Result<(), GeneratorError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.as_bytes().contains(&b'/')
        || name.as_bytes().contains(&b'\0')
    {
        return Err(GeneratorError::usage(format!(
            "unsafe repository path component below {}",
            parent.display()
        )));
    }
    Ok(())
}

fn open_absolute_directory(path: &Path) -> io::Result<fs::File> {
    open_absolute_directory_inner(path, false)
}

fn open_absolute_directory_create(path: &Path) -> io::Result<fs::File> {
    open_absolute_directory_inner(path, true)
}

fn open_absolute_directory_inner(path: &Path, create: bool) -> io::Result<fs::File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure directory path must be absolute",
        ));
    }
    let descriptor = rustix::fs::open(Path::new("/"), DIRECTORY_FLAGS, rustix::fs::Mode::empty())
        .map_err(io::Error::from)?;
    let mut current: fs::File = descriptor.into();
    for component in path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir | Component::CurDir) {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "secure directory path is not normalized",
            ));
        };
        let mut opened =
            rustix::fs::openat(&current, name, DIRECTORY_FLAGS, rustix::fs::Mode::empty());
        if create && matches!(opened, Err(rustix::io::Errno::NOENT)) {
            match rustix::fs::mkdirat(&current, name, rustix::fs::Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => (),
                Err(error) => return Err(io::Error::from(error)),
            }
            opened = rustix::fs::openat(&current, name, DIRECTORY_FLAGS, rustix::fs::Mode::empty());
        }
        current = opened.map_err(io::Error::from)?.into();
    }
    Ok(current)
}

fn same_stat_object(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn same_object_metadata(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn file_version(metadata: &fs::Metadata) -> FileVersion {
    FileVersion {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn file_version_from_stat(stat: &rustix::fs::Stat) -> FileVersion {
    FileVersion {
        device: stat.st_dev as u64,
        inode: stat.st_ino,
        length: stat.st_size as u64,
        modified_seconds: stat.st_mtime,
        modified_nanoseconds: stat.st_mtime_nsec as i64,
        changed_seconds: stat.st_ctime,
        changed_nanoseconds: stat.st_ctime_nsec as i64,
    }
}

fn path_error(message: &str, path: &Path) -> GeneratorError {
    GeneratorError::usage(format!("{message}: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::os::fd::AsRawFd as _;
    use std::process::Command;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "velnor-safe-fs-stage-{}-{}",
                std::process::id(),
                super::unique_suffix()
            ));
            fs::create_dir(&path).expect("create staged-write test directory");
            let path = fs::canonicalize(&path).expect("canonicalize staged-write test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn injected_sync_failure_removes_stage_file() {
        let directory = TestDirectory::new();
        let parent = SafeParent {
            directory: open_absolute_directory(&directory.0)
                .expect("open staged-write test directory"),
            display_path: directory.0.clone(),
            name: OsString::from("generated.yml"),
            path_bound: true,
        };

        let result =
            parent.stage_file_with(Path::new("generated.yml"), "staged bytes", |file, bytes| {
                file.write_all(bytes)?;
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected sync failure",
                ))
            });

        assert!(result.is_err(), "injected sync failure must fail the write");
        assert_eq!(
            fs::read_dir(&directory.0)
                .expect("read staged-write test directory")
                .count(),
            0,
            "failed stage must be removed"
        );
    }

    #[test]
    fn stage_rejects_same_length_in_place_mutation() {
        let directory = TestDirectory::new();
        let parent = SafeParent {
            directory: open_absolute_directory(&directory.0)
                .expect("open staged-write mutation directory"),
            display_path: directory.0.clone(),
            name: OsString::from("generated.yml"),
            path_bound: true,
        };

        let error = parent
            .stage_file_with(Path::new("generated.yml"), "staged bytes", |file, bytes| {
                file.write_all(bytes)?;
                file.sync_all()?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(b"broken bytes")?;
                file.sync_all()
            })
            .expect_err("same-length stage mutation must be rejected");

        assert!(
            error
                .to_string()
                .contains("staged generated file changed after write"),
            "{error}"
        );
        assert_eq!(
            fs::read_dir(&directory.0)
                .expect("read staged-write mutation directory")
                .count(),
            0,
            "a rejected stage must be removed"
        );
    }

    #[test]
    fn capture_rejects_leaf_replaced_after_open() {
        let directory = TestDirectory::new();
        let path = directory.0.join("input.txt");
        let moved = directory.0.join("input-reviewed.txt");
        fs::write(&path, "original\n").expect("write original input");
        let safe_root = SafeRoot::open(&directory.0).expect("open source root");

        let error = safe_root
            .capture_file_preimage_with_hook(Path::new("input.txt"), || {
                fs::rename(&path, &moved)
                    .map_err(|error| GeneratorError::io("replace input leaf", &path, &error))?;
                fs::write(&path, "replacement\n")
                    .map_err(|error| GeneratorError::io("write replacement leaf", &path, &error))
            })
            .expect_err("a renamed leaf must not return stale bytes");
        assert!(
            error.to_string().contains("changed during preflight"),
            "{error}"
        );
    }

    #[test]
    fn capture_rejects_same_length_in_place_mutation() {
        let directory = TestDirectory::new();
        let path = directory.0.join("input.txt");
        fs::write(&path, "before\n").expect("write original input");
        let safe_root = SafeRoot::open(&directory.0).expect("open source root");

        let error = safe_root
            .capture_file_preimage_with_hook(Path::new("input.txt"), || {
                fs::write(&path, "after!\n")
                    .map_err(|error| GeneratorError::io("mutate input leaf", &path, &error))
            })
            .expect_err("an in-place write must not return a mixed snapshot");
        assert!(
            error.to_string().contains("changed during preflight"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pinned_command_uses_captured_directory_after_path_replacement() {
        let source = TestDirectory::new();
        let replacement = TestDirectory::new();
        initialize_git(&source.0, "source.txt");
        initialize_git(&replacement.0, "replacement.txt");
        let safe_root = SafeRoot::open(&source.0).expect("open captured source root");
        safe_root
            .validate_root_binding()
            .expect("validate source root before transient replacement");
        let moved_source = source.0.with_extension("moved");
        fs::rename(&source.0, &moved_source).expect("move captured source root");
        std::os::unix::fs::symlink(&replacement.0, &source.0).expect("replace source pathname");

        let output = pinned_command::output(&safe_root, "git", &["ls-files", "-z"]);

        fs::remove_file(&source.0).expect("remove replacement link");
        fs::rename(&moved_source, &source.0).expect("restore captured source root");
        safe_root
            .validate_root_binding()
            .expect("post-check must accept the restored source path");
        let output = output.expect("run Git from captured root");
        assert!(
            output.status.success(),
            "Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"source.txt\0");
    }

    #[cfg(unix)]
    #[test]
    fn create_parent_swap_after_install_reports_pinned_recovery_path() {
        let directory = TestDirectory::new();
        let github = directory.0.join(".github");
        let workflows = github.join("workflows");
        let moved_parent = github.join("workflows-pinned");
        let replacement_parent = github.join("replacement");
        fs::create_dir_all(&workflows).expect("create output parent");
        fs::create_dir(&replacement_parent).expect("create replacement parent");
        let root = SafeRoot::open(&directory.0).expect("open output root");
        let relative = Path::new(".github/workflows/generated.yml");

        let error = write_reviewed_file_with_after_install(
            &root,
            relative,
            "generated content\n",
            &FilePreimage::Missing,
            |_| Ok(()),
            |_| Ok(()),
            |_| {
                fs::rename(&workflows, &moved_parent).map_err(|error| {
                    GeneratorError::io("move installed output parent", &workflows, &error)
                })?;
                std::os::unix::fs::symlink("replacement", &workflows).map_err(|error| {
                    GeneratorError::io("replace output parent", &workflows, &error)
                })
            },
        )
        .expect_err("a moved parent must fail the post-install binding check");

        let message = error.to_string();
        assert!(
            message.contains("recovery entry remains under its pinned output directory"),
            "{message}"
        );
        assert!(
            message.contains("reported path") && message.contains("may be stale"),
            "{message}"
        );
        assert_eq!(
            fs::read_to_string(moved_parent.join("generated.yml"))
                .expect("read installed file under pinned parent"),
            "generated content\n"
        );
        assert!(
            !replacement_parent.join("generated.yml").exists(),
            "the replacement parent must remain untouched"
        );
    }

    #[test]
    fn create_install_rejects_same_length_mutation_and_reports_recovery() {
        let directory = TestDirectory::new();
        let parent = directory.0.join(".github/workflows");
        fs::create_dir_all(&parent).expect("create output parent");
        let target = parent.join("generated.yml");
        let displaced = parent.join("generated-concurrent.yml");
        let root = SafeRoot::open(&directory.0).expect("open output root");
        let relative = Path::new(".github/workflows/generated.yml");
        assert_eq!(
            b"generated content\n".len(),
            b"corrupted content\n".len(),
            "the mutation must preserve file length"
        );

        let error = write_reviewed_file_with_after_install(
            &root,
            relative,
            "generated content\n",
            &FilePreimage::Missing,
            |_| Ok(()),
            |_| Ok(()),
            |installed| {
                fs::rename(installed, &displaced).map_err(|error| {
                    GeneratorError::io("displace installed output", installed, &error)
                })?;
                fs::write(installed, "corrupted content\n").map_err(|error| {
                    GeneratorError::io("replace installed output", installed, &error)
                })
            },
        )
        .expect_err("a replaced installed file must fail verification");

        let message = error.to_string();
        assert!(
            message.contains("staged generated file changed during install"),
            "{message}"
        );
        assert!(
            message.contains("recovery entry remains under its pinned output directory"),
            "{message}"
        );
        assert!(
            message.contains("reported path") && message.contains("may be stale"),
            "{message}"
        );
        assert!(message.contains(&target.display().to_string()), "{message}");
        assert_eq!(
            fs::read_to_string(&target).expect("read concurrent replacement"),
            "corrupted content\n"
        );
        assert_eq!(
            fs::read_to_string(&displaced).expect("read displaced installed bytes"),
            "generated content\n"
        );
    }

    #[test]
    fn backup_open_rejects_directory_replaced_after_creation() {
        let directory = TestDirectory::new();
        let parent = SafeParent {
            directory: open_absolute_directory(&directory.0).expect("open backup test directory"),
            display_path: directory.0.clone(),
            name: OsString::from("target.yml"),
            path_bound: true,
        };
        let moved_backup = directory.0.join("moved-backup");
        let mut replacement_path = None;

        let error = parent
            .reserve_backup_directory_with_hook(Path::new("target.yml"), |name| {
                let backup_path = directory.0.join(name);
                replacement_path = Some(backup_path.clone());
                fs::rename(&backup_path, &moved_backup).map_err(|error| {
                    GeneratorError::io("move reserved backup directory", &backup_path, &error)
                })?;
                fs::create_dir(&backup_path).map_err(|error| {
                    GeneratorError::io("replace reserved backup directory", &backup_path, &error)
                })?;
                fs::write(backup_path.join("replacement-marker"), "keep\n").map_err(|error| {
                    GeneratorError::io("mark replacement backup directory", &backup_path, &error)
                })
            })
            .expect_err("the opened replacement must not be accepted as the reserved backup");

        assert!(
            error
                .to_string()
                .contains("generated backup directory changed while opening"),
            "{error}"
        );
        assert!(moved_backup.is_dir(), "the original directory stays pinned");
        assert_eq!(
            fs::read_to_string(
                replacement_path
                    .expect("replacement path was captured")
                    .join("replacement-marker")
            )
            .expect("substituted directory remains untouched"),
            "keep\n"
        );
    }

    #[test]
    fn backup_cleanup_leaves_substituted_directory_untouched() {
        let directory = TestDirectory::new();
        let parent = SafeParent {
            directory: open_absolute_directory(&directory.0)
                .expect("open backup cleanup test directory"),
            display_path: directory.0.clone(),
            name: OsString::from("target.yml"),
            path_bound: true,
        };
        let (backup_name, backup) = parent
            .reserve_backup_directory(Path::new("target.yml"))
            .expect("reserve backup directory");
        let backup_path = directory.0.join(&backup_name);
        let moved_backup = directory.0.join("moved-original-backup");
        fs::rename(&backup_path, &moved_backup).expect("move pinned backup directory");
        fs::create_dir(&backup_path).expect("substitute backup directory name");
        fs::write(backup_path.join("replacement-marker"), "keep\n")
            .expect("write replacement marker");

        parent.cleanup_backup(
            &backup_name,
            &backup,
            OsStr::new("target.yml"),
            &FilePreimage::Missing,
        );

        assert_eq!(
            fs::read_to_string(backup_path.join("replacement-marker"))
                .expect("replacement directory remains untouched"),
            "keep\n"
        );
        assert!(moved_backup.is_dir(), "the pinned backup directory remains");
    }

    #[cfg(unix)]
    #[test]
    fn named_tree_cleanup_rejects_replacement_then_cleans_captured_tree() {
        let directory = TestDirectory::new();
        let parent = SafeRoot::open(&directory.0).expect("open pinned scratch parent");
        let scratch_name = OsString::from("scratch");
        let scratch_path = directory.0.join(&scratch_name);
        let moved_a = directory.0.join("captured-a-moved");
        let replacement_b = directory.0.join("replacement-b");
        let protected = directory.0.join("protected-outside-tree");

        let captured_a = parent
            .create_directory_child(&scratch_name)
            .expect("create scratch root below pinned parent");
        let nested_name = OsString::from("nested");
        let nested = captured_a
            .create_directory_child(&nested_name)
            .expect("create nested scratch directory by handle");
        write_reviewed_file_observed(
            &nested,
            Path::new("a.txt"),
            "captured A\n",
            &FilePreimage::Missing,
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("write captured tree marker through nested directory handle");
        fs::create_dir(&replacement_b).expect("create replacement B");
        fs::write(replacement_b.join("b.txt"), "replacement B\n")
            .expect("write replacement marker");
        fs::create_dir(&protected).expect("create external protected directory");
        fs::write(protected.join("keep.txt"), "keep\n").expect("write protected marker");
        std::os::unix::fs::symlink(&protected, scratch_path.join("escape"))
            .expect("add symlink out of captured tree");
        let captured_identity = captured_a.identity().expect("capture scratch identity");

        fs::rename(&scratch_path, &moved_a).expect("move captured A away from scratch name");
        fs::rename(&replacement_b, &scratch_path).expect("put B at scratch name");
        let error = parent
            .remove_named_tree_if_matches(&scratch_name, &captured_identity)
            .expect_err("cleanup must reject B while the scratch name points at B");
        assert!(
            error
                .to_string()
                .contains("scratch directory changed; leaving replacement untouched"),
            "{error}"
        );
        assert_eq!(
            fs::read_to_string(scratch_path.join("b.txt")).expect("read B after rejected cleanup"),
            "replacement B\n",
            "B must remain untouched"
        );

        fs::rename(&scratch_path, &replacement_b).expect("move B back out of scratch name");
        fs::rename(&moved_a, &scratch_path).expect("restore captured A at scratch name");
        parent
            .remove_named_tree_if_matches(&scratch_name, &captured_identity)
            .expect("clean captured A after restoring its name");

        assert!(!scratch_path.exists(), "captured A tree must be removed");
        assert_eq!(
            fs::read_to_string(replacement_b.join("b.txt")).expect("read B after A cleanup"),
            "replacement B\n",
            "B remains intact after captured A cleanup"
        );
        assert_eq!(
            fs::read_to_string(protected.join("keep.txt")).expect("read external target"),
            "keep\n",
            "cleanup removes the symlink itself and never traverses it"
        );
    }

    #[cfg(unix)]
    #[test]
    fn named_tree_cleanup_rejects_b_before_unlink_then_cleans_restored_a() {
        let directory = TestDirectory::new();
        let mut parent = SafeRoot::open(&directory.0).expect("open pinned scratch parent");
        // Exercise the descriptor-only binding mode used by an inherited
        // scratch parent, so the final name identity check is the gate under
        // test rather than a displayed-path mismatch.
        parent.path_bound = false;

        let scratch_name = OsString::from("scratch");
        let scratch_path = directory.0.join(&scratch_name);
        let moved_a = directory.0.join("captured-a-moved");
        let replacement_name = OsString::from("replacement-b");
        let replacement_path = directory.0.join(&replacement_name);
        let captured_a = parent
            .create_directory_child(&scratch_name)
            .expect("create captured scratch A");
        write_reviewed_file_observed(
            &captured_a,
            Path::new("captured.txt"),
            "captured A contents\n",
            &FilePreimage::Missing,
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("write captured A contents");
        let replacement_b = parent
            .create_directory_child(&replacement_name)
            .expect("create empty replacement B");
        let captured_identity = captured_a.identity().expect("capture scratch A identity");
        let replacement_identity = replacement_b.identity().expect("capture B identity");

        let error = parent
            .remove_named_tree_if_matches_with_hook(&scratch_name, &captured_identity, || {
                fs::rename(&scratch_path, &moved_a).map_err(|error| {
                    GeneratorError::io("move captured A during cleanup", &scratch_path, &error)
                })?;
                fs::rename(&replacement_path, &scratch_path).map_err(|error| {
                    GeneratorError::io("put replacement B at scratch path", &scratch_path, &error)
                })
            })
            .expect_err("cleanup must reject B after validating captured A");
        assert!(
            error
                .to_string()
                .contains("scratch directory changed; leaving replacement untouched"),
            "{error}"
        );
        let replacement_at_scratch =
            fs::symlink_metadata(&scratch_path).expect("B remains at scratch name");
        assert!(
            replacement_identity.matches_metadata(&replacement_at_scratch),
            "the final unlink check must preserve B's inode"
        );
        assert!(
            !moved_a.join("captured.txt").exists(),
            "cleanup may have removed captured A contents before rejecting B"
        );

        // Complete A→B→A, then retry cleanup against the same captured inode.
        fs::rename(&scratch_path, &replacement_path).expect("restore B to its own name");
        fs::rename(&moved_a, &scratch_path).expect("restore captured A to scratch name");
        parent
            .remove_named_tree_if_matches(&scratch_name, &captured_identity)
            .expect("clean captured A after restoring its name");

        assert!(!scratch_path.exists(), "captured A root must be removed");
        let replacement_after_cleanup =
            fs::symlink_metadata(&replacement_path).expect("replacement B remains after A cleanup");
        assert!(
            replacement_identity.matches_metadata(&replacement_after_cleanup),
            "B must remain the same directory after A cleanup"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inherited_output_root_writes_to_captured_a_during_a_to_b_to_a_swap() {
        let directory = TestDirectory::new();
        let scratch_path = directory.0.join("scratch");
        let replacement_b = directory.0.join("replacement-b");
        let moved_a = directory.0.join("captured-a-moved");
        fs::create_dir(&scratch_path).expect("create captured scratch A");
        fs::write(scratch_path.join("marker.txt"), "captured A\n")
            .expect("write captured A marker");
        fs::create_dir(&replacement_b).expect("create replacement B");
        fs::write(replacement_b.join("marker.txt"), "replacement B\n")
            .expect("write replacement B marker");

        let captured_a = SafeRoot::open(&scratch_path).expect("open captured output A");
        let inherited_handle = captured_a
            .clone_directory_handle()
            .expect("clone captured output handle");
        assert!(
            rustix::io::fcntl_getfd(&inherited_handle)
                .expect("inspect inherited handle flags")
                .contains(rustix::io::FdFlags::CLOEXEC),
            "the parent copy must not leak into unrelated children"
        );
        let output_root =
            SafeRoot::from_inherited_directory_fd(inherited_handle.as_raw_fd(), &scratch_path)
                .expect("reopen output root from inherited descriptor");

        fs::rename(&scratch_path, &moved_a).expect("move captured A");
        fs::rename(&replacement_b, &scratch_path).expect("put B at captured path");
        // The renderer writes while the display path names B. The inherited
        // output descriptor must still direct every write to captured A.
        write_reviewed_file_observed(
            &output_root,
            Path::new("generated.txt"),
            "written through inherited root\n",
            &FilePreimage::Missing,
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("write relative to inherited directory descriptor");
        // Restore A only after rendering has finished to complete A→B→A.
        fs::rename(&scratch_path, &replacement_b).expect("restore B path");
        fs::rename(&moved_a, &scratch_path).expect("restore captured A path");

        assert_eq!(
            fs::read_to_string(scratch_path.join("generated.txt"))
                .expect("read generated file from captured A"),
            "written through inherited root\n"
        );
        assert!(
            !replacement_b.join("generated.txt").exists(),
            "the pathname replacement B must not receive output"
        );
        assert_eq!(
            fs::read_to_string(replacement_b.join("marker.txt"))
                .expect("read replacement B marker"),
            "replacement B\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inherited_output_root_rejects_invalid_descriptor_without_raw_borrow() {
        let error = SafeRoot::from_inherited_directory_fd(i32::MAX, Path::new("/tmp/output"))
            .expect_err("an invalid descriptor number must fail closed");
        assert!(error
            .to_string()
            .contains("open inherited directory handle"));
    }

    #[test]
    fn delete_keeps_backup_when_a_new_target_appears_after_move() {
        let directory = TestDirectory::new();
        let parent = directory.0.join(".github/workflows");
        fs::create_dir_all(&parent).expect("create stale workflow parent");
        let target = parent.join("stale.yml");
        fs::write(&target, "reviewed stale bytes\n").expect("write stale workflow");
        let root = SafeRoot::open(&directory.0).expect("open output root");
        let relative = Path::new(".github/workflows/stale.yml");
        let expected = root
            .capture_file_preimage(relative)
            .expect("capture stale workflow preimage");

        let error = delete_reviewed_file_observed(&root, relative, &expected, |_| {
            fs::write(&target, "concurrent replacement\n")
                .map_err(|error| GeneratorError::io("create concurrent workflow", &target, &error))
        })
        .expect_err("a new target must block stale backup removal");
        let message = error.to_string();
        assert!(
            message.contains("generated file appeared after stale-file removal"),
            "{message}"
        );
        assert!(
            message.contains("recovery entry remains under its pinned directory"),
            "{message}"
        );
        assert_eq!(
            fs::read_to_string(&target).expect("read concurrent replacement"),
            "concurrent replacement\n"
        );
        let backup = fs::read_dir(&parent)
            .expect("read output parent")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .starts_with(".velnor-workflow-backup-")
                })
            })
            .expect("keep recovery backup directory");
        assert_eq!(
            fs::read_to_string(backup.join("stale.yml")).expect("read preserved backup"),
            "reviewed stale bytes\n"
        );
    }

    #[test]
    fn read_only_output_open_rejects_rebound_root_identity() {
        let directory = TestDirectory::new();
        let binding = OutputPathBinding::capture(&directory.0).expect("capture output binding");
        let moved = directory.0.with_extension("moved");
        fs::rename(&directory.0, &moved).expect("move reviewed output root");
        fs::create_dir(&directory.0).expect("replace reviewed output root");

        let error = SafeRoot::open_existing_output_bound(&directory.0, &binding)
            .expect_err("read-only validation must reject a replacement root");
        assert!(
            error
                .to_string()
                .contains("output path changed after planning"),
            "{error}"
        );

        fs::remove_dir(&directory.0).expect("remove replacement root");
        fs::rename(moved, &directory.0).expect("restore reviewed root");
    }

    #[test]
    fn read_only_output_open_requires_missing_suffix_to_stay_absent() {
        let directory = TestDirectory::new();
        let output = directory.0.join("new-output").join("nested");
        let binding = OutputPathBinding::capture(&output).expect("capture missing output binding");

        assert!(
            SafeRoot::open_existing_output_bound(&output, &binding)
                .expect("validate absent output suffix")
                .is_none(),
            "an unchanged absent suffix has no output root"
        );

        fs::create_dir_all(&output).expect("create output after planning");
        let error = SafeRoot::open_existing_output_bound(&output, &binding)
            .expect_err("read-only validation must reject a created suffix");
        assert!(
            error
                .to_string()
                .contains("output path changed after planning"),
            "{error}"
        );
    }

    fn initialize_git(root: &Path, file: &str) {
        let status = Command::new("git")
            .current_dir(root)
            .args(["init", "--quiet"])
            .status()
            .expect("initialize test repository");
        assert!(status.success(), "git init failed");
        fs::write(root.join(file), "tracked\n").expect("write test file");
        let status = Command::new("git")
            .current_dir(root)
            .args(["add", file])
            .status()
            .expect("stage test file");
        assert!(status.success(), "git add failed");
    }
}
