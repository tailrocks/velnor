//! Handle-relative, no-reparse traversal for local snapshots on Windows.

use std::{
    ffi::{c_void, OsStr, OsString},
    fs, io,
    mem::{self, offset_of},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::{Component, Path, Prefix},
    ptr, slice,
};

use windows_sys::Wdk::{
    Foundation::OBJECT_ATTRIBUTES,
    Storage::FileSystem::{
        FileBothDirectoryInformation, NtCreateFile, NtQueryDirectoryFile,
        FILE_BOTH_DIR_INFORMATION, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
        FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    },
};
use windows_sys::Win32::{
    Foundation::{
        HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, STATUS_NO_MORE_FILES, STATUS_SUCCESS,
        UNICODE_STRING,
    },
    Storage::FileSystem::{
        CreateFileW, FileAttributeTagInfo, GetDriveTypeW, GetFileInformationByHandleEx,
        GetFileType, FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_TYPE_DISK, OPEN_EXISTING, SYNCHRONIZE,
    },
    System::WindowsProgramming::{DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE},
    System::IO::IO_STATUS_BLOCK,
};

const DIRECTORY_BUFFER_WORDS: usize = 512;
const DIRECTORY_BUFFER_BYTES: usize = DIRECTORY_BUFFER_WORDS * mem::size_of::<u64>();
const STATUS_OBJECT_NAME_NOT_FOUND_NT: i32 = 0xc000_0034_u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND_NT: i32 = 0xc000_003a_u32 as i32;
const STATUS_NOT_A_DIRECTORY_NT: i32 = 0xc000_0103_u32 as i32;

impl velnor_storage_snapshot::SnapshotFilesystem for super::ToolSnapshotFilesystem {
    type Anchor = OwnedHandle;

    fn open_trusted_root(&self, trusted_root: &Path) -> anyhow::Result<Option<Self::Anchor>> {
        open_snapshot_trusted_root(trusted_root).map_err(Into::into)
    }

    fn path_kind(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
    ) -> anyhow::Result<velnor_storage_snapshot::SnapshotPathKind> {
        inspect_snapshot_relative_path(anchor, relative).map_err(Into::into)
    }

    fn open_directory(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
    ) -> anyhow::Result<Option<Self::Anchor>> {
        open_snapshot_relative_directory(anchor, relative)
            .map(Some)
            .map_err(Into::into)
    }

    fn visit_directory_entries(
        &self,
        anchor: &Self::Anchor,
        relative: &Path,
        visit: &mut dyn FnMut(OsString) -> anyhow::Result<bool>,
    ) -> anyhow::Result<Option<()>> {
        use velnor_storage_snapshot::SnapshotPathKind;

        match inspect_snapshot_relative_path(anchor, relative)? {
            SnapshotPathKind::Missing => return Ok(None),
            SnapshotPathKind::Directory => {}
            SnapshotPathKind::File => {
                anyhow::bail!(
                    "snapshot path is not a directory: {}",
                    velnor_storage_snapshot::snapshot_path_identity(relative)
                )
            }
            SnapshotPathKind::Link => {
                anyhow::bail!(
                    "snapshot path is a reparse point: {}",
                    velnor_storage_snapshot::snapshot_path_identity(relative)
                )
            }
            SnapshotPathKind::Other => {
                anyhow::bail!(
                    "snapshot path has an unsupported type: {}",
                    velnor_storage_snapshot::snapshot_path_identity(relative)
                )
            }
        }
        let directory = open_snapshot_relative_directory(anchor, relative)?;
        let mut restart_scan = true;
        while let Some(entry) = next_directory_entry(&directory, restart_scan)? {
            restart_scan = false;
            if entry.name == OsStr::new(".") || entry.name == OsStr::new("..") {
                continue;
            }
            if !visit(entry.name)? {
                break;
            }
        }
        Ok(Some(()))
    }
}

fn open_snapshot_trusted_root(trusted_root: &Path) -> io::Result<Option<OwnedHandle>> {
    let canonical_anchor = match fs::canonicalize(trusted_root) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !canonical_anchor.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot anchor is not absolute",
        ));
    }

    let mut directory = open_volume_root(&canonical_anchor)?;
    for component in canonical_anchor.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => directory = open_directory_child(&directory, name)?,
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "canonical snapshot anchor contains parent traversal",
                ));
            }
        }
    }
    Ok(Some(directory))
}

fn inspect_snapshot_relative_path(
    anchor: &OwnedHandle,
    relative: &Path,
) -> io::Result<velnor_storage_snapshot::SnapshotPathKind> {
    use velnor_storage_snapshot::SnapshotPathKind;

    let mut components = relative
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .peekable();
    if components.peek().is_none() {
        return Ok(SnapshotPathKind::Directory);
    }
    let mut directory = open_directory_child(anchor, OsStr::new("."))?;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot path is not a relative descendant of its anchor",
            ));
        };
        if components.peek().is_some() {
            directory = match open_directory_child(&directory, name) {
                Ok(child) => child,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(SnapshotPathKind::Missing);
                }
                Err(error) => return Err(error),
            };
        } else {
            return inspect_snapshot_child(&directory, name);
        }
    }
    Ok(SnapshotPathKind::Directory)
}

fn open_snapshot_relative_directory(
    anchor: &OwnedHandle,
    relative: &Path,
) -> io::Result<OwnedHandle> {
    let mut directory = open_directory_child(anchor, OsStr::new("."))?;
    for component in relative
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
    {
        let Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot path is not a relative descendant of its anchor",
            ));
        };
        directory = open_directory_child(&directory, name)?;
    }
    Ok(directory)
}

/// Collects at most `max_entries` entries plus one overflow probe.
pub(super) fn collect_dir_entries(
    root: &Path,
    trusted_root: &Path,
    depth: usize,
    max_entries: usize,
    result: &mut Vec<String>,
) -> io::Result<usize> {
    if depth == 0 || result.len() >= max_entries {
        return Ok(0);
    }
    let directory = open_snapshot_root(root, trusted_root)?;
    collect_dir_entries_from_handle(&directory, root, depth, max_entries, result)
}

fn open_snapshot_root(root: &Path, trusted_root: &Path) -> io::Result<OwnedHandle> {
    let relative_root = root.strip_prefix(trusted_root).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("snapshot root escaped anchor: {error}"),
        )
    })?;
    let canonical_anchor = fs::canonicalize(trusted_root)?;
    if !canonical_anchor.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "snapshot anchor is not absolute",
        ));
    }

    let mut directory = open_volume_root(&canonical_anchor)?;
    for component in canonical_anchor.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => directory = open_directory_child(&directory, name)?,
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "canonical snapshot anchor contains parent traversal",
                ));
            }
        }
    }
    for component in relative_root.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => directory = open_directory_child(&directory, name)?,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "snapshot path is not a relative descendant of its anchor",
                ));
            }
        }
    }
    Ok(directory)
}

fn open_volume_root(canonical_anchor: &Path) -> io::Result<OwnedHandle> {
    let prefix = canonical_anchor
        .components()
        .find_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix.kind()),
            _ => None,
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "missing drive prefix"))?;
    let (drive_root, root) = match prefix {
        Prefix::Disk(drive) => {
            let root = OsString::from(format!("{}:\\", char::from(drive)));
            (root.clone(), root)
        }
        Prefix::VerbatimDisk(drive) => (
            OsString::from(format!("{}:\\", char::from(drive))),
            OsString::from(format!("\\\\?\\{}:\\", char::from(drive))),
        ),
        Prefix::UNC(..) | Prefix::VerbatimUNC(..) | Prefix::Verbatim(..) | Prefix::DeviceNS(..) => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "safe snapshot traversal supports local drive roots only",
            ));
        }
    };
    let mut drive_wide: Vec<u16> = drive_root.encode_wide().chain(Some(0)).collect();
    // A drive letter can be mapped to a network share. Match the source-copy
    // boundary: only known local drive types may be traversed for snapshots.
    // SAFETY: `drive_wide` is NUL-terminated and alive for the call.
    let drive_type = unsafe { GetDriveTypeW(drive_wide.as_mut_ptr()) };
    if !is_local_drive_type(drive_type) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe snapshot traversal supports local Windows drives only",
        ));
    }
    let mut wide: Vec<u16> = root.encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and remains live through CreateFileW.
    let handle = unsafe {
        CreateFileW(
            wide.as_mut_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned an owned handle that is not INVALID_HANDLE_VALUE.
    let directory = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
    let handle = directory.as_raw_handle() as HANDLE;
    verify_directory_handle(handle)?;
    verify_local_filesystem_handle(handle)?;
    Ok(directory)
}

fn is_local_drive_type(drive_type: u32) -> bool {
    matches!(
        drive_type,
        DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_CDROM | DRIVE_RAMDISK
    )
}

fn is_local_volume_characteristics(characteristics: u32) -> bool {
    use windows_sys::Wdk::System::SystemServices::{
        FILE_CHARACTERISTIC_WEBDAV_DEVICE, FILE_REMOTE_DEVICE, FILE_REMOTE_DEVICE_VSMB,
    };

    characteristics
        & (FILE_REMOTE_DEVICE | FILE_REMOTE_DEVICE_VSMB | FILE_CHARACTERISTIC_WEBDAV_DEVICE)
        == 0
}

fn verify_local_filesystem_handle(handle: HANDLE) -> io::Result<()> {
    use windows_sys::Wdk::{
        Storage::FileSystem::{FileFsDeviceInformation, NtQueryVolumeInformationFile},
        System::SystemServices::FILE_FS_DEVICE_INFORMATION,
    };

    let mut io_status = IO_STATUS_BLOCK::default();
    let mut device_info = FILE_FS_DEVICE_INFORMATION::default();
    // SAFETY: `handle` is the pinned volume-root directory handle; both output
    // structures are writable and sized as required by FileFsDeviceInformation.
    let status = unsafe {
        NtQueryVolumeInformationFile(
            handle,
            &mut io_status,
            (&mut device_info as *mut FILE_FS_DEVICE_INFORMATION).cast(),
            mem::size_of::<FILE_FS_DEVICE_INFORMATION>() as u32,
            FileFsDeviceInformation,
        )
    };
    if status != STATUS_SUCCESS {
        return Err(nt_error("NtQueryVolumeInformationFile", status));
    }
    if io_status.Information < mem::size_of::<FILE_FS_DEVICE_INFORMATION>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "volume device query returned incomplete information",
        ));
    }
    // This attribute comes from the already-opened volume, so drive-letter
    // remapping after GetDriveTypeW cannot turn the check into a path race.
    if !is_local_volume_characteristics(device_info.Characteristics) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe snapshot traversal rejects remote filesystem volumes",
        ));
    }
    Ok(())
}

fn inspect_snapshot_child(
    parent: &OwnedHandle,
    name: &OsStr,
) -> io::Result<velnor_storage_snapshot::SnapshotPathKind> {
    use velnor_storage_snapshot::SnapshotPathKind;

    match open_child_handle(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_DIRECTORY_FILE,
    ) {
        Ok(handle) => snapshot_handle_kind(handle.as_raw_handle() as HANDLE),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SnapshotPathKind::Missing),
        Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
            match open_child_handle(
                parent,
                name,
                FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_NON_DIRECTORY_FILE,
            ) {
                Ok(handle) => snapshot_handle_kind(handle.as_raw_handle() as HANDLE),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Ok(SnapshotPathKind::Missing)
                }
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn open_directory_child(parent: &OwnedHandle, name: &OsStr) -> io::Result<OwnedHandle> {
    let directory = open_child_handle(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_DIRECTORY_FILE,
    )?;
    match snapshot_handle_kind(directory.as_raw_handle() as HANDLE)? {
        velnor_storage_snapshot::SnapshotPathKind::Directory => {}
        velnor_storage_snapshot::SnapshotPathKind::Link => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot path contains a reparse point",
            ));
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot component is not a plain disk directory",
            ));
        }
    }
    Ok(directory)
}

fn open_child_handle(
    parent: &OwnedHandle,
    name: &OsStr,
    desired_access: u32,
    create_options: u32,
) -> io::Result<OwnedHandle> {
    let mut wide: Vec<u16> = name.encode_wide().collect();
    if wide.is_empty()
        || wide
            .iter()
            .any(|unit| *unit == 0 || *unit == u16::from(b'\\') || *unit == u16::from(b'/'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory child name is not one path component",
        ));
    }
    let byte_length = u16::try_from(wide.len().saturating_mul(mem::size_of::<u16>()))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory name is too long"))?;
    let name_string = UNICODE_STRING {
        Length: byte_length,
        MaximumLength: byte_length,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &name_string,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut io_status = IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = ptr::null_mut();
    // SAFETY: all pointers refer to live local structures or the pinned parent
    // handle; the relative name is a single NUL-free component.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &attributes,
            &mut io_status,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            create_options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status != STATUS_SUCCESS {
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            // SAFETY: a non-null handle returned with a non-success status is
            // still an owned output from NtCreateFile and must be closed.
            drop(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) });
        }
        return Err(nt_error("NtCreateFile", status));
    }
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "NtCreateFile succeeded without a directory handle",
        ));
    }
    // SAFETY: successful NtCreateFile returned a new owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
}

fn verify_directory_handle(handle: HANDLE) -> io::Result<()> {
    let mut attributes = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `attributes` is writable storage of the declared size and handle is live.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&mut attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || attributes.FileAttributes & (FILE_ATTRIBUTE_DEVICE | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || unsafe { GetFileType(handle) } != FILE_TYPE_DISK
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "snapshot component is not a plain disk directory",
        ));
    }
    Ok(())
}

fn snapshot_handle_kind(handle: HANDLE) -> io::Result<velnor_storage_snapshot::SnapshotPathKind> {
    use velnor_storage_snapshot::SnapshotPathKind;

    let mut attributes = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `attributes` is writable storage of the declared size and handle is live.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&mut attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(SnapshotPathKind::Link);
    }
    if attributes.FileAttributes & FILE_ATTRIBUTE_DEVICE != 0
        || unsafe { GetFileType(handle) } != FILE_TYPE_DISK
    {
        return Ok(SnapshotPathKind::Other);
    }
    if attributes.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Ok(SnapshotPathKind::Directory)
    } else {
        Ok(SnapshotPathKind::File)
    }
}

fn collect_dir_entries_from_handle(
    directory: &OwnedHandle,
    display_root: &Path,
    depth: usize,
    max_entries: usize,
    result: &mut Vec<String>,
) -> io::Result<usize> {
    if depth == 0 || result.len() >= max_entries {
        return Ok(0);
    }
    let first_entry = result.len();
    let mut restart_scan = true;
    while result.len() < max_entries {
        let Some(entry) = next_directory_entry(directory, restart_scan)? else {
            break;
        };
        restart_scan = false;
        if entry.name == OsStr::new(".") || entry.name == OsStr::new("..") {
            continue;
        }
        let child_path = display_root.join(&entry.name);
        result.push(velnor_storage_snapshot::snapshot_path_identity(&child_path));
        if depth > 1
            && result.len() < max_entries
            && entry.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
            && entry.attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
        {
            let child = open_directory_child(directory, &entry.name)?;
            collect_dir_entries_from_handle(&child, &child_path, depth - 1, max_entries, result)?;
        }
    }
    Ok(result.len() - first_entry)
}

struct DirectoryEntry {
    name: OsString,
    attributes: u32,
}

fn next_directory_entry(
    directory: &OwnedHandle,
    restart_scan: bool,
) -> io::Result<Option<DirectoryEntry>> {
    let mut buffer = [0_u64; DIRECTORY_BUFFER_WORDS];
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: the output buffer is aligned, writable, and sized for a directory
    // record; the handle is a live synchronous directory handle.
    let status = unsafe {
        NtQueryDirectoryFile(
            directory.as_raw_handle() as HANDLE,
            ptr::null_mut(),
            None,
            ptr::null(),
            &mut io_status,
            buffer.as_mut_ptr().cast::<c_void>(),
            DIRECTORY_BUFFER_BYTES as u32,
            FileBothDirectoryInformation,
            true,
            ptr::null(),
            restart_scan,
        )
    };
    if status == STATUS_NO_MORE_FILES {
        return Ok(None);
    }
    if status != STATUS_SUCCESS {
        return Err(nt_error("NtQueryDirectoryFile", status));
    }

    // SAFETY: successful single-entry query initialized a FILE_BOTH_DIR_INFORMATION
    // record at the start of `buffer`.
    let info = unsafe { &*buffer.as_ptr().cast::<FILE_BOTH_DIR_INFORMATION>() };
    let name_byte_len = usize::try_from(info.FileNameLength)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid directory name length"))?;
    let name_offset = offset_of!(FILE_BOTH_DIR_INFORMATION, FileName);
    if name_byte_len == 0
        || name_byte_len % mem::size_of::<u16>() != 0
        || name_offset.saturating_add(name_byte_len) > DIRECTORY_BUFFER_BYTES
        || io_status.Information < name_offset.saturating_add(name_byte_len)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry name exceeded the query buffer",
        ));
    }
    let name_units = name_byte_len / mem::size_of::<u16>();
    // SAFETY: the preceding bounds check proves the flexible filename array
    // stays inside the initialized directory-query buffer.
    let name = unsafe { slice::from_raw_parts(info.FileName.as_ptr(), name_units) };
    Ok(Some(DirectoryEntry {
        name: OsString::from_wide(name),
        attributes: info.FileAttributes,
    }))
}

#[cfg(test)]
mod tests {
    use super::{is_local_drive_type, is_local_volume_characteristics};
    use std::{fs, path::Path, process::Command};
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_CDROM, DRIVE_FIXED, DRIVE_NO_ROOT_DIR, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
        DRIVE_UNKNOWN,
    };

    #[test]
    fn local_drive_check_matches_secure_source_copy_policy() {
        assert!(is_local_drive_type(DRIVE_FIXED));
        assert!(is_local_drive_type(DRIVE_REMOVABLE));
        assert!(is_local_drive_type(DRIVE_CDROM));
        assert!(is_local_drive_type(DRIVE_RAMDISK));
        assert!(!is_local_drive_type(DRIVE_UNKNOWN));
        assert!(!is_local_drive_type(DRIVE_NO_ROOT_DIR));
        assert!(!is_local_drive_type(DRIVE_REMOTE));
    }

    #[test]
    fn opened_volume_rejects_remote_device_characteristic() {
        use windows_sys::Wdk::System::SystemServices::{
            FILE_CHARACTERISTIC_WEBDAV_DEVICE, FILE_REMOTE_DEVICE, FILE_REMOTE_DEVICE_VSMB,
            FILE_REMOVABLE_MEDIA, FILE_VIRTUAL_VOLUME,
        };

        assert!(is_local_volume_characteristics(0));
        assert!(is_local_volume_characteristics(
            FILE_REMOVABLE_MEDIA | FILE_VIRTUAL_VOLUME
        ));
        assert!(!is_local_volume_characteristics(FILE_REMOTE_DEVICE));
        assert!(!is_local_volume_characteristics(FILE_REMOTE_DEVICE_VSMB));
        assert!(!is_local_volume_characteristics(
            FILE_CHARACTERISTIC_WEBDAV_DEVICE
        ));
        assert!(!is_local_volume_characteristics(
            FILE_REMOVABLE_MEDIA | FILE_REMOTE_DEVICE
        ));
    }

    fn create_directory_junction(target: &std::path::Path, junction: &std::path::Path) {
        let command = format!(
            "mklink /J \"{}\" \"{}\"",
            junction.display(),
            target.display()
        );
        let output = Command::new("cmd")
            .args(["/C", command.as_str()])
            .output()
            .expect("cmd.exe must be available for the Windows junction fixture");
        assert!(
            output.status.success(),
            "mklink /J failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn catalog_discovery_resolves_generated_names_case_insensitively() {
        let root =
            std::env::temp_dir().join(format!("velnor-snapshot-case-{}", uuid::Uuid::new_v4()));
        let trusted_root = root.join("storage");
        let cache_root = trusted_root.join("cache/velnor/v1");
        let work_root = root.join("work");
        fs::create_dir_all(cache_root.join("GHA-CACHE")).unwrap();
        let config = velnor_storage_snapshot::SnapshotCatalogConfig::from_resolved_layout(
            &work_root,
            &cache_root,
            &trusted_root,
        );

        let roots = velnor_storage_snapshot::discover_pinned_local_storage_roots(
            &config,
            &super::super::ToolSnapshotFilesystem,
        )
        .unwrap();
        assert!(roots
            .iter()
            .any(|root| root.root.path == cache_root.join("gha-cache")));

        drop(roots);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_discovery_rejects_directory_junctions() {
        let root =
            std::env::temp_dir().join(format!("velnor-snapshot-junction-{}", uuid::Uuid::new_v4()));
        let trusted_root = root.join("storage");
        let cache_root = trusted_root.join("cache/velnor/v1");
        let work_root = root.join("work");
        let outside = root.join("outside");
        let junction = cache_root.join("gha-cache");
        fs::create_dir_all(&cache_root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("outside-sentinel"), "outside").unwrap();
        create_directory_junction(&outside, &junction);

        let config = velnor_storage_snapshot::SnapshotCatalogConfig::from_resolved_layout(
            &work_root,
            &cache_root,
            &trusted_root,
        );
        assert!(
            velnor_storage_snapshot::discover_pinned_local_storage_roots(
                &config,
                &super::super::ToolSnapshotFilesystem,
            )
            .is_err()
        );

        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_renderer_lists_junction_as_leaf_and_keeps_sibling_entries() {
        let root = std::env::temp_dir().join(format!(
            "velnor-snapshot-render-junction-{}",
            uuid::Uuid::new_v4()
        ));
        let store = root.join("store");
        let outside = root.join("outside");
        let junction = store.join("external");
        fs::create_dir_all(&store).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(store.join("ordinary-sibling"), "inside").unwrap();
        fs::write(outside.join("secret-file"), "outside").unwrap();
        create_directory_junction(&outside, &junction);

        let filesystem = super::super::ToolSnapshotFilesystem;
        let anchor =
            velnor_storage_snapshot::SnapshotFilesystem::open_trusted_root(&filesystem, &root)
                .unwrap()
                .unwrap();
        let directory = velnor_storage_snapshot::SnapshotFilesystem::open_directory(
            &filesystem,
            &anchor,
            Path::new("store"),
        )
        .unwrap()
        .unwrap();
        let pinned = velnor_storage_snapshot::PinnedLocalStorageSnapshotRoot {
            root: velnor_storage_snapshot::LocalStorageSnapshotRoot {
                path: store.clone(),
                trusted_root: root.clone(),
            },
            directory,
        };
        let snapshot =
            super::super::evidence_local_storage_snapshot_from_roots(&root, 10, Ok(vec![pinned]));

        assert!(snapshot.contains("ordinary-sibling"));
        assert!(snapshot.contains(&velnor_storage_snapshot::snapshot_path_identity(&junction)));
        assert!(!snapshot.contains("secret-file"));
        assert!(!snapshot.contains("[listing unavailable: safe traversal failed]"));

        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

fn nt_error(operation: &str, status: i32) -> io::Error {
    let kind = match status {
        STATUS_OBJECT_NAME_NOT_FOUND_NT | STATUS_OBJECT_PATH_NOT_FOUND_NT => {
            io::ErrorKind::NotFound
        }
        STATUS_NOT_A_DIRECTORY_NT => io::ErrorKind::NotADirectory,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(
        kind,
        format!("{operation} returned NTSTATUS 0x{:08x}", status as u32),
    )
}
