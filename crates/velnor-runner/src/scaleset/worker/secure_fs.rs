//! Descriptor-relative filesystem operations for host-owned worker files.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};

pub(super) fn open_absolute_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "host directory path must be absolute",
        ));
    }

    let root = rustix::fs::openat(
        rustix::fs::CWD,
        Path::new("/"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut current: File = root.into();
    verify_path_component(&current)?;

    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                let next = rustix::fs::openat(
                    &current,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io::Error::from)?;
                let next: File = next.into();
                verify_path_component(&next)?;
                current = next;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "host directory path is not normalized",
                ));
            }
        }
    }

    if !current.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "host path is not a directory",
        ));
    }
    Ok(current)
}

/// Resolve a configured worker-state root once and securely create missing
/// components. Relative paths are anchored to the current directory captured
/// by this call. Every existing component is opened without following links
/// and checked before it can become an ancestor of worker state. The final
/// root is tightened to mode `0700`; string-persisted roots must be UTF-8 and
/// may not be the filesystem root.
pub(super) fn prepare_state_root(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker state root path is empty",
        ));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let normalized = normalize_absolute_path(&absolute)?;
    if normalized == Path::new("/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker state root cannot be the filesystem root",
        ));
    }
    if normalized.to_str().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker state root must be UTF-8 because ownership records persist paths as strings",
        ));
    }
    let root = rustix::fs::openat(
        rustix::fs::CWD,
        Path::new("/"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut current: File = root.into();
    verify_path_component(&current)?;

    for component in normalized.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        let next = match rustix::fs::openat(
            &current,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => File::from(directory),
            Err(rustix::io::Errno::NOENT) => {
                match rustix::fs::mkdirat(&current, name, Mode::from_raw_mode(0o700)) {
                    Ok(()) => sync_directory(&current)?,
                    // Another creator may have won. Opening below still
                    // rejects symlinks and validates the actual directory.
                    // Sync here too: the winning creator may have returned
                    // before its parent fsync completed.
                    Err(rustix::io::Errno::EXIST) => sync_directory(&current)?,
                    Err(error) => return Err(io::Error::from(error)),
                }
                open_directory_at(&current, name)?
            }
            Err(error) => return Err(io::Error::from(error)),
        };
        verify_path_component(&next)?;
        current = next;
    }

    verify_host_parent(&current)?;
    rustix::fs::fchmod(&current, Mode::from_raw_mode(0o700)).map_err(io::Error::from)?;
    sync_directory(&current)?;
    Ok(normalized)
}

fn normalize_absolute_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker state root must resolve to an absolute path",
        ));
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => normalized.push(name),
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "worker state root has an unsupported path prefix",
                ));
            }
        }
    }
    Ok(normalized)
}

pub(super) fn verify_host_parent(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    let owner = rustix::process::geteuid().as_raw();
    if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "worker state parent is not a host-owned, non-writable directory",
        ));
    }
    Ok(())
}

pub(super) fn private_jit_directory_name(state_dir: &Path) -> io::Result<OsString> {
    let base = state_dir.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker state directory has no final component",
        )
    })?;
    validate_component(base)?;
    let mut name = OsString::from(".velnor-jit-");
    name.push(base);
    Ok(name)
}

pub(super) fn open_or_create_private_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    validate_component(name)?;
    let created = match rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700)) {
        Ok(()) => true,
        Err(rustix::io::Errno::EXIST) => false,
        Err(error) => return Err(io::Error::from(error)),
    };
    let directory = open_directory_at(parent, name)?;
    verify_host_owner(&directory)?;
    rustix::fs::fchmod(&directory, Mode::from_raw_mode(0o700)).map_err(io::Error::from)?;
    sync_directory(&directory)?;
    if created {
        sync_directory(parent)?;
    }
    Ok(directory)
}

pub(super) fn open_private_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    validate_component(name)?;
    let directory = open_directory_at(parent, name)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory is not a private host-owned directory",
        ));
    }
    Ok(directory)
}

pub(super) fn write_file_at(
    directory: &File,
    name: &OsStr,
    contents: &[u8],
    mode: u16,
) -> io::Result<()> {
    validate_component(name)?;
    let mut temporary_name = None;
    let mut temporary_file = None;
    for _ in 0..16 {
        let candidate = OsString::from(format!(
            ".velnor-write-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        match rustix::fs::openat(
            directory,
            &candidate,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode.into()),
        ) {
            Ok(file) => {
                temporary_name = Some(candidate);
                temporary_file = Some(File::from(file));
                break;
            }
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(io::Error::from(error)),
        }
    }

    let temporary_name = temporary_name.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique host-owned temporary file",
        )
    })?;
    let mut temporary_file = temporary_file.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary file descriptor was not allocated",
        )
    })?;
    let result = (|| {
        temporary_file.write_all(contents)?;
        rustix::fs::fchmod(&temporary_file, Mode::from_raw_mode(mode.into()))
            .map_err(io::Error::from)?;
        temporary_file.sync_all()?;
        rustix::fs::renameat(directory, &temporary_name, directory, name)
            .map_err(io::Error::from)?;
        rustix::fs::fsync(directory).map_err(io::Error::from)
    })();
    match result {
        Ok(()) => Ok(()),
        Err(write_error) => match rustix::fs::unlinkat(directory, &temporary_name, AtFlags::empty())
        {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Err(write_error),
            Err(cleanup_error) => Err(io::Error::other(format!(
                "host file write failed: {write_error}; removing temporary file {} also failed: {cleanup_error}",
                temporary_name.to_string_lossy()
            ))),
        },
    }
}

pub(super) fn read_file_at(directory: &File, name: &OsStr) -> io::Result<Option<Vec<u8>>> {
    validate_component(name)?;
    let file = match rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => file,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(io::Error::from(error)),
    };
    let mut file: File = file.into();
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
        || metadata.len() > 1024
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "file is not a small, private host-owned regular file",
        ));
    }
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut contents)?;
    Ok(Some(contents))
}

pub(super) fn remove_file_at(directory: &File, name: &OsStr) -> io::Result<()> {
    validate_component(name)?;
    let mut sync = sync_directory;
    unlink_entry_with_sync(directory, name, &mut sync)
}

pub(super) fn owner_only_regular_file_exists_at(
    directory: &File,
    name: &OsStr,
) -> io::Result<bool> {
    validate_component(name)?;
    let stat = match rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(error) => return Err(io::Error::from(error)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic is not an owner-only host regular file",
        ));
    }
    Ok(true)
}

/// Hash one owner-only regular file without following links or buffering its
/// full contents in memory. Diagnostic artifact receipts bind successful
/// captures to the exact bytes of their final atomic file.
pub(super) fn blake3_digest_owner_only_file_at(
    directory: &File,
    name: &OsStr,
) -> io::Result<Option<[u8; 32]>> {
    validate_component(name)?;
    let file = match rustix::fs::openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(file) => File::from(file),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(io::Error::from(error)),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic is not an owner-only host regular file",
        ));
    }

    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut file = file;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(Some(*hasher.finalize().as_bytes()))
}

pub(super) fn sync_directory(directory: &File) -> io::Result<()> {
    rustix::fs::fsync(directory).map_err(io::Error::from)
}

pub(super) fn directory_exists_at(parent: &File, name: &OsStr) -> io::Result<bool> {
    validate_component(name)?;
    match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::Directory => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "worker state path is not a real directory",
        )),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(io::Error::from(error)),
    }
}

/// Remove an untrusted tree relative to a pinned parent.
///
/// The caller must first stop every untrusted writer. Production worker
/// state is released only after owned containers are torn down. The walk
/// snapshots child names for active ancestors before mutating each directory,
/// so cleanup needs neither recursive calls nor a depth-sized descriptor
/// stack. It reads each directory once, then removes entries with
/// descriptor-relative calls. Snapshot memory is proportional to pending
/// entries on the active path, and can reach the tree's total entry count for
/// a very wide root. Name/vector growth uses fallible reservations; allocation
/// failure returns cleanup to the durable retry fence instead of aborting the
/// daemon. No scratch file is used, so a full filesystem does not block this
/// walk.
pub(super) fn remove_tree_at(parent: &File, name: &OsStr) -> io::Result<()> {
    let mut sync = sync_directory;
    remove_tree_at_with_sync(parent, name, &mut sync).map(|_| ())
}

fn remove_tree_at_with_sync(
    parent: &File,
    name: &OsStr,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
) -> io::Result<RemovalStats> {
    let mut mount_id = mount_id_for_directory;
    remove_tree_at_with_sync_and_mount_id(parent, name, sync, &mut mount_id)
}

fn remove_tree_at_with_sync_and_mount_id(
    parent: &File,
    name: &OsStr,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
    mount_id: &mut dyn FnMut(&File) -> io::Result<Option<u64>>,
) -> io::Result<RemovalStats> {
    validate_component(name)?;
    let Some(stat) = stat_entry_for_removal(parent, name, sync)? else {
        return Ok(RemovalStats::default());
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        unlink_entry_with_sync(parent, name, sync)?;
        return Ok(RemovalStats::default());
    }

    let root_directory = match open_directory_at(parent, name) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            sync_with_retry(parent, sync)?;
            return Ok(RemovalStats::default());
        }
        Err(error) => return Err(error),
    };
    let opened_root = rustix::fs::fstat(&root_directory).map_err(io::Error::from)?;
    if opened_root.st_dev != stat.st_dev || opened_root.st_ino != stat.st_ino {
        return Err(io::Error::other(
            "directory changed during host-owned cleanup",
        ));
    }
    let root_device = opened_root.st_dev as u64;
    let root_mount_id = mount_id(&root_directory)?;
    verify_mount_identity(
        rustix::fs::fstat(parent).map_err(io::Error::from)?.st_dev as u64,
        mount_id(parent)?,
        root_device,
        root_mount_id,
    )?;
    let (root_children, root_entries) =
        collect_directory_children(&root_directory, root_device, sync)?;
    let mut stats = RemovalStats {
        visited_entries: root_entries,
        directory_opens: 1,
        parent_opens: 0,
    };
    let mut stack = Vec::new();
    stack
        .try_reserve(1)
        .map_err(|error| io::Error::other(format!("grow worker-state traversal stack: {error}")))?;
    stack.push(RemovalFrame {
        name: copy_component(name)?,
        device: opened_root.st_dev as u64,
        inode: opened_root.st_ino,
        children: root_children,
    });
    let mut current_directory = root_directory;

    let removal = (|| -> io::Result<RemovalStats> {
        loop {
            let child = stack
                .last_mut()
                .ok_or_else(|| io::Error::other("worker-state removal stack is empty"))?
                .children
                .pop();

            if let Some(child) = child {
                let Some(stat) = stat_entry_for_removal(&current_directory, &child.name, sync)?
                else {
                    continue;
                };
                let is_directory = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
                if is_directory != child.is_directory
                    || stat.st_dev as u64 != child.device
                    || stat.st_ino != child.inode
                {
                    return Err(io::Error::other(
                        "directory changed during host-owned cleanup",
                    ));
                }
                if !is_directory {
                    match rustix::fs::unlinkat(&current_directory, &child.name, AtFlags::empty()) {
                        Ok(()) => {}
                        Err(rustix::io::Errno::NOENT) => sync(&current_directory)?,
                        Err(error) => return Err(io::Error::from(error)),
                    }
                    continue;
                }
                if child.device != root_device {
                    return Err(io::Error::other(
                        "refusing to remove contents across a filesystem mount boundary",
                    ));
                }
                let child_directory = match open_directory_at(&current_directory, &child.name) {
                    Ok(directory) => directory,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        sync_with_retry(&current_directory, sync)?;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                let opened = rustix::fs::fstat(&child_directory).map_err(io::Error::from)?;
                if opened.st_dev as u64 != child.device || opened.st_ino != child.inode {
                    return Err(io::Error::other(
                        "directory changed during host-owned cleanup",
                    ));
                }
                verify_mount_identity(
                    root_device,
                    root_mount_id,
                    opened.st_dev as u64,
                    mount_id(&child_directory)?,
                )?;
                stats.directory_opens += 1;
                let (children, entries) =
                    collect_directory_children(&child_directory, root_device, sync)?;
                stats.visited_entries += entries;
                stack.try_reserve(1).map_err(|error| {
                    io::Error::other(format!("grow worker-state traversal: {error}"))
                })?;
                // Keep mutations batched while the parent frame is active.
                // The directory gets one barrier before its own removal; if
                // this pass fails earlier, the durable cleanup-pending fence
                // makes retries safe and missing entries sync their parent.
                stack.push(RemovalFrame {
                    name: child.name,
                    device: opened.st_dev as u64,
                    inode: opened.st_ino,
                    children,
                });
                current_directory = child_directory;
                continue;
            }

            // Persist every mutation made within this directory in one barrier.
            // Retries reach this point even when an earlier unlink succeeded but
            // its enclosing directory sync failed.
            sync(&current_directory)?;
            let frame = stack
                .last()
                .ok_or_else(|| io::Error::other("worker-state removal stack is empty"))?;
            if stack.len() == 1 {
                remove_directory_from_parent(parent, frame, sync)?;
                sync_with_retry(parent, sync)?;
                return Ok(stats);
            }

            // Ascend through the directory's own `..` entry. This keeps the
            // traversal at constant FD depth even for trees deeper than PATH_MAX
            // and avoids reopening every ancestor from the root. The recorded
            // parent inode fences rename/swap attacks during the walk.
            let containing_directory = open_parent_directory(&current_directory)?;
            stats.parent_opens += 1;
            let expected_parent = stack
                .get(stack.len() - 2)
                .ok_or_else(|| io::Error::other("worker-state parent frame is missing"))?;
            verify_directory_identity(&containing_directory, expected_parent)?;
            remove_directory_from_parent(&containing_directory, frame, sync)?;
            stack.pop();
            current_directory = containing_directory;
        }
    })();

    match removal {
        Ok(stats) => Ok(stats),
        Err(error) => match sync(&current_directory) {
            Ok(()) => Err(error),
            Err(sync_error) => Err(io::Error::other(format!(
                "worker-state removal failed: {error}; syncing partial deletion also failed: {sync_error}"
            ))),
        },
    }
}

fn stat_entry_for_removal(
    directory: &File,
    name: &OsStr,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
) -> io::Result<Option<rustix::fs::Stat>> {
    match rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(rustix::io::Errno::NOENT) => {
            sync_with_retry(directory, sync)?;
            Ok(None)
        }
        Err(error) => Err(io::Error::from(error)),
    }
}

fn collect_directory_children(
    directory: &File,
    root_device: u64,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
) -> io::Result<(Vec<RemovalChild>, usize)> {
    let mut entries = Dir::read_from(directory).map_err(io::Error::from)?;
    let mut child_directories = Vec::new();
    let mut visited_entries = 0;
    while let Some(entry) = entries.read() {
        let entry = entry.map_err(io::Error::from)?;
        visited_entries += 1;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = copy_component(OsStr::from_bytes(name))?;
        validate_component(&name)?;
        let stat = match rustix::fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => {
                sync_with_retry(directory, sync)?;
                continue;
            }
            Err(error) => return Err(io::Error::from(error)),
        };
        let is_directory = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
        if is_directory && stat.st_dev as u64 != root_device {
            return Err(io::Error::other(
                "refusing to remove contents across a filesystem mount boundary",
            ));
        }
        child_directories.try_reserve(1).map_err(|error| {
            io::Error::other(format!("grow worker-state child snapshot: {error}"))
        })?;
        child_directories.push(RemovalChild {
            name,
            device: stat.st_dev as u64,
            inode: stat.st_ino,
            is_directory,
        });
    }
    Ok((child_directories, visited_entries))
}

#[cfg(target_os = "linux")]
fn mount_id_for_directory(directory: &File) -> io::Result<Option<u64>> {
    let stat = rustix::fs::statx(
        directory,
        OsStr::new(""),
        AtFlags::EMPTY_PATH,
        rustix::fs::StatxFlags::MNT_ID,
    )
    .map_err(io::Error::from)?;
    if stat.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem mount identity is unavailable",
        ));
    }
    Ok(Some(stat.stx_mnt_id))
}

#[cfg(not(target_os = "linux"))]
fn mount_id_for_directory(_directory: &File) -> io::Result<Option<u64>> {
    Ok(None)
}

fn verify_mount_identity(
    parent_device: u64,
    parent_mount_id: Option<u64>,
    directory_device: u64,
    directory_mount_id: Option<u64>,
) -> io::Result<()> {
    if parent_device != directory_device || parent_mount_id != directory_mount_id {
        return Err(io::Error::other(
            "refusing to remove contents across a filesystem mount boundary",
        ));
    }
    Ok(())
}

fn verify_directory_identity(directory: &File, expected: &RemovalFrame) -> io::Result<()> {
    let stat = rustix::fs::fstat(directory).map_err(io::Error::from)?;
    if stat.st_dev as u64 != expected.device || stat.st_ino != expected.inode {
        return Err(io::Error::other(
            "directory changed during host-owned cleanup",
        ));
    }
    Ok(())
}

fn open_parent_directory(directory: &File) -> io::Result<File> {
    rustix::fs::openat(
        directory,
        OsStr::new(".."),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(Into::into)
    .map_err(io::Error::from)
}

fn remove_directory_from_parent(
    parent: &File,
    frame: &RemovalFrame,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
) -> io::Result<()> {
    let current = match rustix::fs::statat(parent, &frame.name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(rustix::io::Errno::NOENT) => return sync_with_retry(parent, sync),
        Err(error) => return Err(io::Error::from(error)),
    };
    if current.st_dev as u64 != frame.device || current.st_ino != frame.inode {
        return Err(io::Error::other(
            "directory changed during host-owned cleanup",
        ));
    }
    match rustix::fs::unlinkat(parent, &frame.name, AtFlags::REMOVEDIR) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::NOENT) => sync_with_retry(parent, sync),
        Err(error) => Err(io::Error::from(error)),
    }
}

fn sync_with_retry(
    directory: &File,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
) -> io::Result<()> {
    match sync(directory) {
        Ok(()) => Ok(()),
        Err(first_error) => match sync(directory) {
            Ok(()) => Err(first_error),
            Err(retry_error) => Err(io::Error::other(format!(
                "directory sync failed: {first_error}; retry failed: {retry_error}"
            ))),
        },
    }
}

fn open_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(Into::into)
    .map_err(io::Error::from)
}

fn verify_host_owner(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "directory is not owned by the host daemon user",
        ));
    }
    Ok(())
}

fn verify_path_component(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    let daemon = rustix::process::geteuid().as_raw();
    let owner = metadata.uid();
    let mode = metadata.mode();
    let sticky = mode & 0o1000 != 0;
    // Shared sticky directories (for example `/tmp`) may contain entries
    // owned by this daemon without letting another user replace those
    // entries. Other writable ancestors can redirect a later path open.
    if !metadata.is_dir() || (owner != 0 && owner != daemon) || (mode & 0o022 != 0 && !sticky) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path ancestor can be replaced by an untrusted user",
        ));
    }
    Ok(())
}

fn validate_component(name: &OsStr) -> io::Result<()> {
    let mut components = Path::new(name).components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem entry name must be one normal path component",
        ))
    }
}

fn copy_component(name: &OsStr) -> io::Result<OsString> {
    let bytes = name.as_bytes();
    let mut owned = Vec::new();
    owned.try_reserve_exact(bytes.len()).map_err(|error| {
        io::Error::other(format!("allocate worker-state path component: {error}"))
    })?;
    owned.extend_from_slice(bytes);
    Ok(OsString::from_vec(owned))
}

struct RemovalFrame {
    name: OsString,
    device: u64,
    inode: u64,
    children: Vec<RemovalChild>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RemovalStats {
    visited_entries: usize,
    directory_opens: usize,
    parent_opens: usize,
}

struct RemovalChild {
    name: OsString,
    device: u64,
    inode: u64,
    is_directory: bool,
}

fn unlink_entry_with_sync(
    parent: &File,
    name: &OsStr,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
) -> io::Result<()> {
    match rustix::fs::unlinkat(parent, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => sync(parent),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests may panic")]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn prepare_state_root_anchors_relative_path_and_creates_missing_components() {
        assert!(prepare_state_root(Path::new("")).is_err());
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let base_name = format!("velnor-secure-fs-root-{}", uuid::Uuid::new_v4().simple());
        let relative = PathBuf::from(".")
            .join(&base_name)
            .join("discarded")
            .join("..")
            .join("workers");
        let expected = cwd.join(&base_name).join("workers");

        let prepared = prepare_state_root(&relative).unwrap();
        assert_eq!(prepared, expected);
        assert!(prepared.is_absolute());
        assert!(!cwd.join(&base_name).join("discarded").exists());
        assert_eq!(std::fs::metadata(&prepared).unwrap().mode() & 0o777, 0o700);

        std::fs::remove_dir_all(cwd.join(base_name)).unwrap();
    }

    #[test]
    fn prepare_state_root_rejects_root_and_non_utf8_paths_before_creating() {
        assert_eq!(
            prepare_state_root(Path::new("/")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let mut component =
            format!("velnor-secure-fs-non-utf8-{}", uuid::Uuid::new_v4()).into_bytes();
        component.push(0xff);
        let invalid_component = OsString::from_vec(component);
        let invalid_root = temp.join(&invalid_component);
        let configured = invalid_root.join("workers");

        assert_eq!(
            prepare_state_root(&configured).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!invalid_root.exists());
    }

    #[test]
    fn prepare_state_root_makes_existing_root_private() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-existing-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(prepare_state_root(&root).unwrap(), root);
        assert_eq!(std::fs::metadata(&root).unwrap().mode() & 0o777, 0o700);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepare_state_root_rejects_symlink_and_writable_ancestors() {
        use std::os::unix::fs::symlink;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-root-unsafe-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let victim = root.with_extension("victim");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&victim).unwrap();
        symlink(&victim, root.join("link")).unwrap();
        assert!(prepare_state_root(&root.join("link/worker-state")).is_err());
        assert!(!victim.join("worker-state").exists());
        std::fs::remove_file(root.join("link")).unwrap();

        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(prepare_state_root(&root.join("worker-state")).is_err());
        assert!(!root.join("worker-state").exists());

        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(victim).unwrap();
    }

    #[test]
    fn diagnostic_digest_is_content_bound_and_does_not_follow_links() {
        use std::os::unix::fs::symlink;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-digest-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = open_absolute_directory(&root).unwrap();
        let artifact = OsStr::new("capture.log");
        let contents = b"complete atomically published capture\n";
        write_file_at(&directory, artifact, contents, 0o600).unwrap();

        assert_eq!(
            blake3_digest_owner_only_file_at(&directory, artifact).unwrap(),
            Some(*blake3::hash(contents).as_bytes())
        );
        assert_eq!(
            blake3_digest_owner_only_file_at(&directory, OsStr::new("missing.log")).unwrap(),
            None
        );

        let target = root.join("target.log");
        std::fs::write(&target, contents).unwrap();
        symlink(&target, root.join("linked.log")).unwrap();
        assert!(blake3_digest_owner_only_file_at(&directory, OsStr::new("linked.log")).is_err());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_handles_remain_pinned_when_parent_path_is_swapped() {
        use std::os::unix::fs::symlink;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let parent = temp.join(format!(
            "velnor-secure-fs-parent-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let moved = parent.with_extension("moved");
        let victim = parent.with_extension("victim");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(&victim).unwrap();
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(victim.join("sentinel"), b"untouched").unwrap();

        let directory = open_absolute_directory(&parent).unwrap();
        verify_host_parent(&directory).unwrap();
        std::fs::rename(&parent, &moved).unwrap();
        symlink(&victim, &parent).unwrap();
        write_file_at(&directory, OsStr::new("host.log"), b"host bytes", 0o600).unwrap();

        assert_eq!(
            std::fs::read(victim.join("sentinel")).unwrap(),
            b"untouched"
        );
        assert!(!victim.join("host.log").exists());
        assert_eq!(
            std::fs::read(moved.join("host.log")).unwrap(),
            b"host bytes"
        );

        std::fs::remove_file(&parent).unwrap();
        std::fs::remove_dir_all(&moved).unwrap();
        std::fs::remove_dir_all(&victim).unwrap();
    }

    #[test]
    fn directory_walk_rejects_symlinked_parent_component() {
        use std::os::unix::fs::symlink;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-link-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let victim = root.with_extension("victim");
        let parent = root.join("parent");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&victim).unwrap();
        symlink(&victim, &parent).unwrap();
        std::fs::write(victim.join("sentinel"), b"untouched").unwrap();

        assert!(open_absolute_directory(&parent).is_err());
        assert_eq!(
            std::fs::read(victim.join("sentinel")).unwrap(),
            b"untouched"
        );

        std::fs::remove_file(parent).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(victim).unwrap();
    }

    #[test]
    fn iterative_removal_handles_trees_deeper_than_2048_directories() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-deep-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        let root_name = root.file_name().unwrap();
        let parent = open_absolute_directory(&temp).unwrap();
        let mut current = open_directory_at(&parent, root_name).unwrap();
        let depth = 4096;
        for _ in 0..depth {
            rustix::fs::mkdirat(&current, OsStr::new("d"), Mode::from_raw_mode(0o700)).unwrap();
            current = open_directory_at(&current, OsStr::new("d")).unwrap();
        }
        write_file_at(&current, OsStr::new("sentinel"), b"remove me", 0o600).unwrap();
        drop(current);

        let stats = remove_tree_at_with_sync(&parent, root_name, &mut |_| Ok(())).unwrap();
        assert_eq!(stats.visited_entries, depth * 3 + 3);
        assert_eq!(stats.directory_opens, depth + 1);
        assert_eq!(stats.parent_opens, depth);
        assert!(!root.exists());
    }

    #[test]
    fn iterative_removal_uses_bounded_fd_count_at_extreme_depth() {
        const CHILD: &str = "VELNOR_SECURE_FS_FD_LIMIT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "iterative_removal_uses_bounded_fd_count_at_extreme_depth",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success(), "low-FD cleanup child failed: {status}");
            return;
        }

        let nofile = rustix::process::Resource::Nofile;
        let original = rustix::process::getrlimit(nofile);
        let limit = original.maximum.unwrap_or(32).min(32);
        rustix::process::setrlimit(
            nofile,
            rustix::process::Rlimit {
                current: Some(limit),
                maximum: Some(limit),
            },
        )
        .unwrap();

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-low-fd-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        let root_name = root.file_name().unwrap();
        let parent = open_absolute_directory(&temp).unwrap();
        let mut current = open_directory_at(&parent, root_name).unwrap();
        for _ in 0..4096 {
            rustix::fs::mkdirat(&current, OsStr::new("d"), Mode::from_raw_mode(0o700)).unwrap();
            current = open_directory_at(&current, OsStr::new("d")).unwrap();
        }
        write_file_at(&current, OsStr::new("sentinel"), b"remove me", 0o600).unwrap();
        drop(current);

        remove_tree_at_with_sync(&parent, root_name, &mut |_| Ok(())).unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn retry_syncs_nested_directory_after_unlink_sync_failure() {
        use std::os::unix::fs::MetadataExt;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-sync-retry-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let nested = root.join("nested");
        let file = nested.join("secret");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&file, b"secret data").unwrap();

        let parent = open_absolute_directory(&temp).unwrap();
        let nested_inode = std::fs::metadata(&nested).unwrap().ino();
        let mut injected = false;
        let first =
            remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |directory| {
                if directory.metadata()?.ino() == nested_inode && !injected {
                    injected = true;
                    return Err(io::Error::other("injected nested-directory fsync failure"));
                }
                sync_directory(directory)
            });
        assert!(first.is_err());
        assert!(injected);
        assert!(!file.exists());
        assert!(nested.is_dir());

        let nested_directory = open_absolute_directory(&nested).unwrap();
        let mut retried_absent_entry_sync = false;
        unlink_entry_with_sync(&nested_directory, OsStr::new("secret"), &mut |directory| {
            retried_absent_entry_sync = true;
            sync_directory(directory)
        })
        .unwrap();
        assert!(
            retried_absent_entry_sync,
            "ENOENT retry must sync its parent"
        );

        let mut retried_nested_sync = false;
        remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |directory| {
            if directory.metadata()?.ino() == nested_inode {
                retried_nested_sync = true;
            }
            sync_directory(directory)
        })
        .unwrap();
        assert!(retried_nested_sync, "retry must sync the nested parent");
        assert!(!root.exists());
    }

    #[test]
    fn missing_tree_entries_sync_their_pinned_parent() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let parent = open_absolute_directory(&temp).unwrap();
        let missing_name = OsString::from(format!(
            "velnor-secure-fs-missing-entry-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut sync_attempts = 0;
        let missing = stat_entry_for_removal(&parent, &missing_name, &mut |directory| {
            sync_attempts += 1;
            if sync_attempts == 1 {
                Err(io::Error::other("injected parent-directory fsync failure"))
            } else {
                sync_directory(directory)
            }
        });
        assert!(missing.is_err(), "failed sync must remain visible to retry");
        assert_eq!(sync_attempts, 2, "missing entry sync retries once");

        let mut retry_synced = false;
        assert!(
            stat_entry_for_removal(&parent, &missing_name, &mut |directory| {
                retry_synced = true;
                sync_directory(directory)
            },)
            .unwrap()
            .is_none()
        );
        assert!(
            retry_synced,
            "tree walk missing-entry branch must sync its parent"
        );

        let frame = RemovalFrame {
            name: OsString::from(format!(
                "velnor-secure-fs-removed-directory-{}",
                uuid::Uuid::new_v4().simple()
            )),
            device: 0,
            inode: 0,
            children: Vec::new(),
        };
        let mut removed_directory_synced = false;
        remove_directory_from_parent(&parent, &frame, &mut |directory| {
            removed_directory_synced = true;
            sync_directory(directory)
        })
        .unwrap();
        assert!(
            removed_directory_synced,
            "missing directory removal must sync its containing directory"
        );
    }

    #[test]
    fn partial_unlinks_are_synced_before_retry_after_error() {
        use std::os::unix::fs::MetadataExt;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-partial-sync-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let nested = root.join("child");
        let file = nested.join("file");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&file, b"remove me").unwrap();

        let parent = open_absolute_directory(&temp).unwrap();
        let nested_inode = std::fs::metadata(&nested).unwrap().ino();
        let mut injected_failure = false;
        let mut partial_deletion_resynced = false;
        let first =
            remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |directory| {
                if directory.metadata()?.ino() == nested_inode && !file.exists() {
                    if !injected_failure {
                        injected_failure = true;
                        return Err(io::Error::other("injected nested-directory fsync failure"));
                    }
                    sync_directory(directory)?;
                    partial_deletion_resynced = true;
                    return Ok(());
                }
                sync_directory(directory)
            });

        assert!(first.is_err());
        assert!(injected_failure);
        assert!(partial_deletion_resynced);
        assert!(!file.exists());
        assert!(nested.is_dir());

        remove_tree_at(&parent, root.file_name().unwrap()).unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn iterative_removal_handles_wide_worker_directories() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-wide-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        for index in 0..2048 {
            std::fs::write(root.join(format!("entry-{index}")), b"remove me").unwrap();
        }

        let parent = open_absolute_directory(&temp).unwrap();
        let stats =
            remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |_| Ok(())).unwrap();
        assert_eq!(stats.visited_entries, 2050);
        assert_eq!(stats.directory_opens, 1);
        assert_eq!(stats.parent_opens, 0);
        assert!(!root.exists());
    }

    #[test]
    fn iterative_removal_visits_wide_child_directories_once() {
        use std::collections::HashMap;
        use std::os::unix::fs::MetadataExt;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-wide-dirs-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        let count = 1024;
        let root_inode = std::fs::metadata(&root).unwrap().ino();
        let mut child_inodes = Vec::with_capacity(count);
        for index in 0..count {
            let child = root.join(format!("child-{index}"));
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("sentinel"), b"remove me").unwrap();
            child_inodes.push(std::fs::metadata(child).unwrap().ino());
        }

        let parent = open_absolute_directory(&temp).unwrap();
        let parent_inode = parent.metadata().unwrap().ino();
        let mut syncs_by_directory = HashMap::<u64, usize>::new();
        let stats =
            remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |directory| {
                *syncs_by_directory
                    .entry(directory.metadata()?.ino())
                    .or_default() += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(stats.visited_entries, count * 4 + 2);
        assert_eq!(stats.directory_opens, count + 1);
        assert_eq!(stats.parent_opens, count);
        assert_eq!(syncs_by_directory.len(), count + 2);
        assert_eq!(syncs_by_directory.get(&root_inode), Some(&1));
        assert_eq!(syncs_by_directory.get(&parent_inode), Some(&1));
        assert!(child_inodes
            .iter()
            .all(|inode| syncs_by_directory.get(inode) == Some(&1)));
        assert!(!root.exists());
    }

    #[test]
    fn mount_identity_check_rejects_a_mounted_removal_root() {
        assert!(verify_mount_identity(7, Some(11), 7, Some(12)).is_err());
        assert!(verify_mount_identity(7, None, 8, None).is_err());
        verify_mount_identity(7, Some(11), 7, Some(11)).unwrap();
    }

    #[test]
    fn removal_walk_rejects_root_and_nested_mount_boundaries_before_mutation() {
        use std::os::unix::fs::MetadataExt;

        let temp = std::env::temp_dir().canonicalize().unwrap();

        let root = temp.join(format!(
            "velnor-secure-fs-mounted-root-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("sentinel"), b"keep root contents").unwrap();
        let root_inode = std::fs::metadata(&root).unwrap().ino();
        let parent = open_absolute_directory(&temp).unwrap();
        let mut root_mount_id = |directory: &File| {
            Ok(Some(if directory.metadata()?.ino() == root_inode {
                12
            } else {
                11
            }))
        };
        assert!(remove_tree_at_with_sync_and_mount_id(
            &parent,
            root.file_name().unwrap(),
            &mut |_| Ok(()),
            &mut root_mount_id,
        )
        .is_err());
        assert!(root.join("sentinel").exists());
        std::fs::remove_dir_all(&root).unwrap();

        let root = temp.join(format!(
            "velnor-secure-fs-mounted-child-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let child = root.join("child");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("sentinel"), b"keep mounted contents").unwrap();
        let child_inode = std::fs::metadata(&child).unwrap().ino();
        let parent = open_absolute_directory(&temp).unwrap();
        let mut child_mount_id = |directory: &File| {
            Ok(Some(if directory.metadata()?.ino() == child_inode {
                12
            } else {
                11
            }))
        };
        assert!(remove_tree_at_with_sync_and_mount_id(
            &parent,
            root.file_name().unwrap(),
            &mut |_| Ok(()),
            &mut child_mount_id,
        )
        .is_err());
        assert_eq!(
            std::fs::read(child.join("sentinel")).unwrap(),
            b"keep mounted contents"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn iterative_removal_syncs_parent_after_root_unlink() {
        use std::os::unix::fs::MetadataExt;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-parent-sync-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();

        let parent = open_absolute_directory(&temp).unwrap();
        let parent_inode = parent.metadata().unwrap().ino();
        let mut parent_was_synced = false;
        remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |directory| {
            if directory.metadata()?.ino() == parent_inode {
                parent_was_synced = true;
            }
            Ok(())
        })
        .unwrap();

        assert!(
            parent_was_synced,
            "root unlink must sync its containing dir"
        );
        assert!(!root.exists());
    }
}
