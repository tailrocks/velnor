//! Descriptor-relative, immutable local storage for captured GitHub bytes.
//!
//! This module owns only the local raw-object boundary.  The collector is
//! responsible for measuring the source response before redaction and passes
//! that source digest/length separately from the safe bytes stored here.  The
//! store never follows a caller path after construction, never replaces an
//! existing object, and verifies bytes through the file descriptor it opened.
//! The shared acquisition helper supplies the sole canonical `sha256://` URI;
//! this boundary does not accept or emit URI aliases. The original digest and
//! length remain producer-owned measurements and are not authenticated by
//! this store without the producer's verified capture contract.

use crate::github_acquisition::{
    content_addressed_storage_ref, sha256_digest, RawObject, RawObjectRef, RawObjectStore,
    RawStorageError,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::json;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::ffi::{CStr, CString, OsStr, OsString};
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_RAW_OBJECT_BYTES: usize = 64 * 1024 * 1024;
const MAX_RAW_SIDECAR_BYTES: usize = MAX_RAW_OBJECT_BYTES * 2 + 4096;

/// Immutable local CAS for redacted/safe response bytes and provenance
/// sidecars.
pub struct RawObjectFileStore {
    root: PathBuf,
    #[cfg(unix)]
    objects: File,
    #[cfg(unix)]
    refs: File,
}

impl fmt::Debug for RawObjectFileStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawObjectFileStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl RawObjectFileStore {
    /// Open an explicitly selected store root without following any path
    /// component.  Missing components are created descriptor-relatively;
    /// existing symlink ancestors/children are refused by `O_NOFOLLOW`.
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        if root.as_os_str().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "raw store root must be explicit",
            ));
        }

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let root_directory = open_secure_directory(&root)?;
            let objects = open_directory_at(&root_directory, "sha256", true)?;
            let refs = open_directory_at(&root_directory, "refs", true)?;
            root_directory.sync_all()?;
            Ok(Self {
                root,
                objects,
                refs,
            })
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = root;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "raw store requires tested macOS or Linux atomic publication primitives",
            ))
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    #[cfg(unix)]
    fn store_unix(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
        if !valid_digest(&object.original_sha256) || object.bytes.len() > MAX_RAW_OBJECT_BYTES {
            return Err(RawStorageError::Refused);
        }

        // One directory FD lock serializes sidecar observation, object
        // publication, and sidecar publication across store instances and
        // processes. This is the transaction boundary for the bundle.
        let _publication_lock = DirectoryLock::acquire(&self.refs)?;
        let sidecar_name = raw_id_name(&object.raw_id)?;
        let safe_digest = sha256_digest(&object.bytes);
        let safe_length =
            u64::try_from(object.bytes.len()).map_err(|_| RawStorageError::Refused)?;
        let object_name = digest_name(&safe_digest)?;

        let reference = RawObjectRef {
            raw_id: object.raw_id,
            request_id: object.request_id,
            object_kind: object.object_kind,
            canonicalization: object.canonicalization,
            sha256: safe_digest.clone(),
            byte_length: safe_length,
            original_sha256: object.original_sha256,
            original_byte_length: object.original_byte_length,
            bytes_base64: BASE64.encode(&object.bytes),
            media_type: object.media_type,
            storage_ref: content_addressed_storage_ref(&safe_digest),
        };
        if !sidecar_size_within_limit(&reference) {
            return Err(RawStorageError::Refused);
        }
        let sidecar_bytes = sidecar_bytes(&reference)?;
        if sidecar_bytes.len() > MAX_RAW_SIDECAR_BYTES {
            return Err(RawStorageError::Refused);
        }
        // A valid raw ID can already be bound to a different response. Read
        // that sidecar before publishing a new CAS object so a collision
        // cannot create an unreferenced object as a side effect.
        ensure_sidecar_slot(&self.refs, &sidecar_name, &sidecar_bytes)?;
        publish_if_absent(
            &self.objects,
            &object_name,
            &object.bytes,
            MAX_RAW_OBJECT_BYTES,
        )?;
        publish_if_absent(
            &self.refs,
            &sidecar_name,
            &sidecar_bytes,
            MAX_RAW_SIDECAR_BYTES,
        )?;
        Ok(reference)
    }

    #[cfg(unix)]
    fn verify_unix(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
        if !valid_digest(&reference.original_sha256)
            || reference.storage_ref != content_addressed_storage_ref(&reference.sha256)
        {
            return Err(RawStorageError::Unbound);
        }
        if !sidecar_size_within_limit(reference) {
            return Err(RawStorageError::Unbound);
        }
        let expected_sidecar = sidecar_bytes(reference)?;
        if expected_sidecar.len() > MAX_RAW_SIDECAR_BYTES {
            return Err(RawStorageError::Unbound);
        }
        let object_name = digest_name(&reference.sha256)?;
        let safe_bytes = read_named(&self.objects, &object_name, MAX_RAW_OBJECT_BYTES)?;
        if safe_bytes.len() as u64 != reference.byte_length
            || sha256_digest(&safe_bytes) != reference.sha256
            || BASE64.encode(&safe_bytes) != reference.bytes_base64
        {
            return Err(RawStorageError::Unbound);
        }

        let sidecar_name = raw_id_name(&reference.raw_id)?;
        let actual_sidecar = read_named(&self.refs, &sidecar_name, MAX_RAW_SIDECAR_BYTES)?;
        if actual_sidecar != expected_sidecar {
            return Err(RawStorageError::Unbound);
        }
        Ok(())
    }
}

impl RawObjectStore for RawObjectFileStore {
    fn store(&mut self, object: RawObject) -> Result<RawObjectRef, RawStorageError> {
        #[cfg(unix)]
        {
            self.store_unix(object)
        }
        #[cfg(not(unix))]
        {
            let _ = object;
            Err(RawStorageError::Unavailable)
        }
    }

    fn verify(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
        #[cfg(unix)]
        {
            self.verify_unix(reference)
        }
        #[cfg(not(unix))]
        {
            let _ = reference;
            Err(RawStorageError::Unavailable)
        }
    }
}

fn valid_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn sidecar_bytes(reference: &RawObjectRef) -> Result<Vec<u8>, RawStorageError> {
    serde_json::to_vec(&json!({
        "raw_id": reference.raw_id,
        "request_id": reference.request_id,
        "object_kind": reference.object_kind,
        "canonicalization": reference.canonicalization,
        "sha256": reference.sha256,
        "byte_length": reference.byte_length,
        "original_sha256": reference.original_sha256,
        "original_byte_length": reference.original_byte_length,
        "bytes_base64": reference.bytes_base64,
        "media_type": reference.media_type,
        "storage_ref": reference.storage_ref,
    }))
    .map_err(|_| RawStorageError::Refused)
}

fn sidecar_size_within_limit(reference: &RawObjectRef) -> bool {
    // JSON string escaping can expand arbitrary metadata by at most six bytes
    // per source byte. Base64 is already restricted to one-byte alphabet
    // characters and is the largest normal field at the 64 MiB payload cap.
    let escaped_fields = [
        reference.raw_id.as_str(),
        reference.request_id.as_str(),
        reference.object_kind.as_str(),
        reference.canonicalization.as_str(),
        reference.sha256.as_str(),
        reference.original_sha256.as_str(),
        reference.media_type.as_str(),
        reference.storage_ref.as_str(),
    ];
    let Some(escaped_bytes) = escaped_fields.iter().try_fold(256_usize, |total, field| {
        total.checked_add(field.len().checked_mul(6)?)
    }) else {
        return false;
    };
    if !reference
        .bytes_base64
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return false;
    }
    escaped_bytes
        .checked_add(reference.bytes_base64.len())
        .is_some_and(|size| size <= MAX_RAW_SIDECAR_BYTES)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    nlink: u64,
    mode: u32,
}

#[cfg(unix)]
impl FileIdentity {
    fn is_regular_single_link(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32 && self.nlink == 1
    }

    fn same_inode(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

#[cfg(unix)]
fn stat_fd(file: &File) -> io::Result<FileIdentity> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    let stat = unsafe { stat.assume_init() };
    Ok(FileIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino,
        nlink: stat.st_nlink as u64,
        mode: stat.st_mode as u32,
    })
}

#[cfg(unix)]
fn stat_at(directory: &File, name: &CStr) -> Result<FileIdentity, RawStorageError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        return Err(storage_io(io::Error::last_os_error()));
    }
    let stat = unsafe { stat.assume_init() };
    Ok(FileIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino,
        nlink: stat.st_nlink as u64,
        mode: stat.st_mode as u32,
    })
}

#[cfg(unix)]
fn validate_directory(file: &File) -> io::Result<()> {
    let identity = stat_fd(file)?;
    if identity.mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "raw store path component is not a directory",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_start(path: &Path) -> io::Result<File> {
    let (start, flags) = if path.is_absolute() {
        ("/", libc::O_RDONLY | libc::O_DIRECTORY)
    } else {
        (".", libc::O_RDONLY | libc::O_DIRECTORY)
    };
    let start = CString::new(start)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "raw store start contains NUL"))?;
    let fd = unsafe { libc::open(start.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    validate_directory(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn open_secure_directory(path: &Path) -> io::Result<File> {
    let mut components = Vec::<OsString>::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(component) => {
                CString::new(component.as_bytes()).map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "raw store path component contains NUL",
                    )
                })?;
                components.push(component.to_os_string());
            }
            Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "raw store root may not contain parent components",
                ));
            }
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "raw store path prefix is unsupported",
                ));
            }
        }
    }

    let mut current = open_start(path)?;
    for component in components {
        current = open_directory_at(&current, component, true)?;
    }
    Ok(current)
}

#[cfg(unix)]
fn open_directory_at(
    parent: &File,
    component: impl AsRef<OsStr>,
    create: bool,
) -> io::Result<File> {
    let component = component.as_ref();
    let component = CString::new(component.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "raw store path component contains NUL",
        )
    })?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let mut fd = unsafe { libc::openat(parent.as_raw_fd(), component.as_ptr(), flags) };
    if fd < 0 && create && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        let mkdir = unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) };
        if mkdir < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(io::Error::last_os_error());
        }
        fd = unsafe { libc::openat(parent.as_raw_fd(), component.as_ptr(), flags) };
    }
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    validate_directory(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn digest_name(digest: &str) -> Result<CString, RawStorageError> {
    let hex = digest
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or(RawStorageError::Unbound)?;
    CString::new(hex).map_err(|_| RawStorageError::Unbound)
}

#[cfg(unix)]
fn raw_id_name(raw_id: &str) -> Result<CString, RawStorageError> {
    if raw_id.is_empty()
        || raw_id.len() > 160
        || !raw_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RawStorageError::Unbound);
    }
    CString::new(format!("{raw_id}.json")).map_err(|_| RawStorageError::Unbound)
}

#[cfg(unix)]
fn open_named(directory: &File, name: &CStr) -> Result<Option<File>, RawStorageError> {
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(storage_io(error));
    }
    Ok(Some(unsafe { File::from_raw_fd(fd) }))
}

#[cfg(unix)]
fn read_named(directory: &File, name: &CStr, max_bytes: usize) -> Result<Vec<u8>, RawStorageError> {
    let mut file = open_named(directory, name)?.ok_or(RawStorageError::Unavailable)?;
    read_verified_fd(&mut file, max_bytes)
}

#[cfg(unix)]
fn read_named_if_present(
    directory: &File,
    name: &CStr,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, RawStorageError> {
    let Some(mut file) = open_named(directory, name)? else {
        return Ok(None);
    };
    read_verified_fd(&mut file, max_bytes).map(Some)
}

#[cfg(unix)]
fn ensure_sidecar_slot(
    directory: &File,
    name: &CStr,
    expected: &[u8],
) -> Result<(), RawStorageError> {
    match read_named_if_present(directory, name, MAX_RAW_SIDECAR_BYTES)? {
        None => Ok(()),
        Some(actual) if actual == expected => Ok(()),
        Some(_) => Err(RawStorageError::Refused),
    }
}

#[cfg(unix)]
fn read_verified_fd(file: &mut File, max_bytes: usize) -> Result<Vec<u8>, RawStorageError> {
    let before = stat_fd(file).map_err(storage_io)?;
    if !before.is_regular_single_link() {
        return Err(RawStorageError::Unbound);
    }
    let mut bytes = Vec::new();
    file.take((max_bytes as u64) + 1)
        .read_to_end(&mut bytes)
        .map_err(storage_io)?;
    if bytes.len() > max_bytes {
        return Err(RawStorageError::Unbound);
    }
    let after = stat_fd(file).map_err(storage_io)?;
    if !after.is_regular_single_link() || !before.same_inode(after) {
        return Err(RawStorageError::Unbound);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn publish_if_absent(
    directory: &File,
    name: &CStr,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<(), RawStorageError> {
    if bytes.len() > max_bytes {
        return Err(RawStorageError::Refused);
    }
    match read_named_if_present(directory, name, max_bytes)? {
        Some(existing) if existing == bytes => return Ok(()),
        Some(_) => return Err(RawStorageError::Refused),
        None => {}
    }
    let mut temporary = TemporaryFile::create(directory)?;
    (|| {
        temporary
            .file
            .write_all(bytes)
            .and_then(|_| temporary.file.sync_all())
            .map_err(storage_io)?;
        temporary
            .file
            .seek(SeekFrom::Start(0))
            .map_err(storage_io)?;
        if read_verified_fd(&mut temporary.file, max_bytes)? != bytes {
            return Err(RawStorageError::Refused);
        }
        if unsafe { libc::fchmod(temporary.file.as_raw_fd(), 0o400) } < 0 {
            return Err(storage_io(io::Error::last_os_error()));
        }

        match install_no_clobber(directory, &temporary.name, name) {
            Ok(()) => {
                // Atomic rename/link moved the temporary name to the final
                // name. There is no cleanup pathname left to race.
                sync_directory(directory)?;
                let final_identity = stat_at(directory, name)?;
                let temporary_identity = stat_fd(&temporary.file).map_err(storage_io)?;
                if !temporary_identity.is_regular_single_link()
                    || !temporary_identity.same_inode(final_identity)
                {
                    return Err(RawStorageError::Refused);
                }
                temporary
                    .file
                    .seek(SeekFrom::Start(0))
                    .map_err(storage_io)?;
                if read_verified_fd(&mut temporary.file, max_bytes)? != bytes {
                    return Err(RawStorageError::Refused);
                }
                sync_directory(directory)?;
                Ok(())
            }
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                // A trusted publisher cannot reach this branch while the
                // refs-directory transaction lock is held. An external
                // writer raced the immutable install; leave the private temp
                // for bounded reconciliation instead of unlinking a pathname
                // whose identity may have changed.
                Err(RawStorageError::Refused)
            }
            Err(error) => Err(storage_io(error)),
        }
    })()
}

#[cfg(unix)]
fn install_no_clobber(directory: &File, temporary: &CStr, final_name: &CStr) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            temporary.as_ptr(),
            directory.as_raw_fd(),
            final_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };

    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory.as_raw_fd(),
            temporary.as_ptr(),
            directory.as_raw_fd(),
            final_name.as_ptr(),
            1_i32,
        ) as libc::c_int
    };

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = (directory, temporary, final_name);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "raw store atomic publication is unsupported on this Unix target",
        ));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(directory: &File) -> Result<(), RawStorageError> {
    directory.sync_all().map_err(storage_io)
}

#[cfg(unix)]
struct DirectoryLock<'a> {
    directory: &'a File,
}

#[cfg(unix)]
impl<'a> DirectoryLock<'a> {
    fn acquire(directory: &'a File) -> Result<Self, RawStorageError> {
        let result = unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX) };
        if result < 0 {
            return Err(storage_io(io::Error::last_os_error()));
        }
        Ok(Self { directory })
    }
}

#[cfg(unix)]
impl Drop for DirectoryLock<'_> {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.directory.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(unix)]
struct TemporaryFile {
    name: CString,
    file: File,
}

#[cfg(unix)]
impl TemporaryFile {
    fn create(directory: &File) -> Result<Self, RawStorageError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        for attempt in 0..64_u32 {
            let name = CString::new(format!(
                ".velnor-raw-{}-{sequence}-{attempt}.tmp",
                std::process::id()
            ))
            .map_err(|_| RawStorageError::Refused)?;
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_CLOEXEC
                        | libc::O_NOFOLLOW,
                    0o600,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(storage_io(error));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            let identity = stat_fd(&file).map_err(storage_io)?;
            if !identity.is_regular_single_link() {
                return Err(RawStorageError::Refused);
            }
            return Ok(Self { name, file });
        }
        Err(RawStorageError::Unavailable)
    }
}

#[cfg(unix)]
fn storage_io(error: io::Error) -> RawStorageError {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::EACCES | libc::EPERM | libc::EMLINK | libc::EISDIR) => {
            RawStorageError::Refused
        }
        Some(libc::ENOENT) => RawStorageError::Unavailable,
        _ => RawStorageError::Unavailable,
    }
}
