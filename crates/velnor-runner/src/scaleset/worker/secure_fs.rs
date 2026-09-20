//! Descriptor-relative filesystem operations for host-owned worker files.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

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
            Mode::from_raw_mode(mode),
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
        rustix::fs::fchmod(&temporary_file, Mode::from_raw_mode(mode)).map_err(io::Error::from)?;
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
/// descriptor-relative calls.
pub(super) fn remove_tree_at(parent: &File, name: &OsStr) -> io::Result<()> {
    let mut sync = sync_directory;
    remove_tree_at_with_sync(parent, name, &mut sync).map(|_| ())
}

fn remove_tree_at_with_sync(
    parent: &File,
    name: &OsStr,
    sync: &mut dyn FnMut(&File) -> io::Result<()>,
) -> io::Result<RemovalStats> {
    validate_component(name)?;
    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => {
            sync(parent)?;
            return Ok(RemovalStats::default());
        }
        Err(error) => return Err(io::Error::from(error)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        unlink_entry_with_sync(parent, name, sync)?;
        return Ok(RemovalStats::default());
    }

    let root_directory = match open_directory_at(parent, name) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            sync(parent)?;
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
    let root_mount_id = mount_id_for_directory(&root_directory)?;
    verify_mount_identity(
        rustix::fs::fstat(parent).map_err(io::Error::from)?.st_dev as u64,
        mount_id_for_directory(parent)?,
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
    let mut stack = vec![RemovalFrame {
        name: name.to_os_string(),
        device: opened_root.st_dev as u64,
        inode: opened_root.st_ino,
        children: root_children,
    }];
    let mut current_directory = root_directory;

    let removal = (|| -> io::Result<RemovalStats> {
        loop {
            let child = stack
                .last_mut()
                .ok_or_else(|| io::Error::other("worker-state removal stack is empty"))?
                .children
                .pop();

            if let Some(child) = child {
                let stat = match rustix::fs::statat(
                    &current_directory,
                    &child.name,
                    AtFlags::SYMLINK_NOFOLLOW,
                ) {
                    Ok(stat) => stat,
                    Err(rustix::io::Errno::NOENT) => {
                        sync(&current_directory)?;
                        continue;
                    }
                    Err(error) => return Err(io::Error::from(error)),
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
                        sync(&current_directory)?;
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
                    mount_id_for_directory(&child_directory)?,
                )?;
                stats.directory_opens += 1;
                let (children, entries) =
                    collect_directory_children(&child_directory, root_device, sync)?;
                stats.visited_entries += entries;
                stack.try_reserve(1).map_err(|error| {
                    io::Error::other(format!("grow worker-state traversal: {error}"))
                })?;
                // A directory may already contain removed siblings. Persist those
                // changes before dropping its descriptor as the walk descends.
                sync(&current_directory)?;
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
        let name = OsString::from_vec(name.to_vec());
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
    child_directories.sort_unstable_by(|left, right| left.name.cmp(&right.name));
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
    fn partial_unlinks_sync_before_descent_and_after_error() {
        use std::os::unix::fs::MetadataExt;

        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-partial-sync-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let nested = root.join("a-child");
        let file = root.join("z-file");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(&file, b"remove me").unwrap();

        let parent = open_absolute_directory(&temp).unwrap();
        let root_inode = std::fs::metadata(&root).unwrap().ino();
        let mut injected_failure = false;
        let mut partial_deletion_resynced = false;
        let first =
            remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |directory| {
                if directory.metadata()?.ino() == root_inode && !file.exists() {
                    if !injected_failure {
                        injected_failure = true;
                        return Err(io::Error::other("injected parent-directory fsync failure"));
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
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let root = temp.join(format!(
            "velnor-secure-fs-wide-dirs-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&root).unwrap();
        let count = 1024;
        for index in 0..count {
            let child = root.join(format!("child-{index}"));
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("sentinel"), b"remove me").unwrap();
        }

        let parent = open_absolute_directory(&temp).unwrap();
        let stats =
            remove_tree_at_with_sync(&parent, root.file_name().unwrap(), &mut |_| Ok(())).unwrap();
        assert_eq!(stats.visited_entries, count * 4 + 2);
        assert_eq!(stats.directory_opens, count + 1);
        assert_eq!(stats.parent_opens, count);
        assert!(!root.exists());
    }

    #[test]
    fn mount_identity_check_rejects_a_mounted_removal_root() {
        assert!(verify_mount_identity(7, Some(11), 7, Some(12)).is_err());
        assert!(verify_mount_identity(7, None, 8, None).is_err());
        verify_mount_identity(7, Some(11), 7, Some(11)).unwrap();
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
