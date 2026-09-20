//! Descriptor-relative, immutable local storage for captured GitHub bytes.
//!
//! This module owns the local raw-object boundary.  The collector passes the
//! exact response bytes before masking alongside the safe bytes; this store
//! computes both digest/length pairs itself and persists both immutable CAS
//! objects.  It never accepts caller-supplied provenance assertions, follows
//! no caller path after construction, replaces no existing object, and
//! verifies bytes through the file descriptors it opened. The shared
//! acquisition helper supplies the sole canonical `sha256://` URI; this
//! boundary does not accept or emit URI aliases.

use crate::github_acquisition::{
    content_addressed_storage_ref, sha256_digest, RawObject, RawObjectRef, RawObjectStore,
    RawStorageError,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::collections::{HashMap, HashSet};
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
#[cfg(unix)]
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

const MAX_RAW_OBJECT_BYTES: usize = 64 * 1024 * 1024;
const MAX_RAW_SIDECAR_BYTES: usize = MAX_RAW_OBJECT_BYTES * 2 + 4096;
const MAX_RETAINED_ENTRIES: usize = 128;
const MAX_RETAINED_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_RETAINED_BYTES: u64 = MAX_RAW_SIDECAR_BYTES as u64 + MAX_RETAINED_MANIFEST_BYTES as u64;
const RETENTION_MANIFEST_NAME: &[u8] = b"manifest.json\0";
const RETENTION_ENTRY_NAME: &[u8] = b"entry\0";

#[cfg(unix)]
const RETENTION_SCHEMA_VERSION: u32 = 1;

#[cfg(unix)]
// Retention invariants:
// * the anchored retention root contains only generated record directories;
// * every record has exactly this schema, a private manifest, and (when
//   present) one private entry whose FD-derived digest/length are recorded;
// * source quarantine directories are never renamed or removed, so a source
//   path race cannot orphan the original reachable identity;
// * admission counts records and bytes and refuses before mutation at either
//   bound. Recovery likewise fails closed on any unknown or malformed child.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionIdentity {
    device: u64,
    inode: u64,
    nlink: u64,
    mode: u32,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionEntry {
    name: String,
    identity: RetentionIdentity,
    byte_length: u64,
    sha256: String,
}

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionManifest {
    schema: u32,
    kind: String,
    record_name: String,
    source_namespace: String,
    source_name: String,
    source_parent: RetentionIdentity,
    quarantine: RetentionIdentity,
    entry: Option<RetentionEntry>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RetentionUsage {
    entries: usize,
    bytes: u64,
}

/// Immutable local CAS for original response bytes, redacted/safe bytes, and
/// provenance sidecars. Original bytes remain local and are never serialized
/// into metadata or formatted through this type.
pub struct RawObjectFileStore {
    root: PathBuf,
    #[cfg(unix)]
    anchor: NamespaceAnchor,
    #[cfg(unix)]
    objects: File,
    #[cfg(unix)]
    originals: File,
    #[cfg(unix)]
    refs: File,
    #[cfg(unix)]
    quarantine: File,
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
            let secure_root = open_secure_directory(&root)?;
            let setup_root = secure_root.file.try_clone()?;
            let setup_lock = NamespaceLock::acquire(&setup_root).map_err(raw_storage_io_error)?;
            restrict_directory(&secure_root.file)?;
            let objects = open_directory_at(&secure_root.file, "sha256", true)?;
            restrict_directory(&objects)?;
            let originals = open_directory_at(&secure_root.file, "original", true)?;
            restrict_directory(&originals)?;
            let refs = open_directory_at(&secure_root.file, "refs", true)?;
            restrict_directory(&refs)?;
            let quarantine = open_directory_at(&secure_root.file, ".velnor-raw-quarantine", true)?;
            restrict_directory(&quarantine)?;
            let anchor = NamespaceAnchor::new(
                secure_root.parent,
                secure_root.name,
                secure_root.file,
                &objects,
                &originals,
                &refs,
                &quarantine,
            )?;
            drop(setup_lock);
            drop(setup_root);
            let _reconcile_lock =
                NamespaceLock::acquire(&anchor.root).map_err(raw_storage_io_error)?;
            anchor.reconcile(&objects, &originals, &refs, &quarantine)?;
            anchor.root.sync_all()?;
            drop(_reconcile_lock);
            Ok(Self {
                root,
                anchor,
                objects,
                originals,
                refs,
                quarantine,
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
        if object.bytes.len() > MAX_RAW_OBJECT_BYTES
            || object.original_bytes.len() > MAX_RAW_OBJECT_BYTES
        {
            return Err(RawStorageError::Refused);
        }

        // One trusted-parent lock serializes sidecar observation, object
        // publication, and sidecar publication across store instances and
        // processes. The anchor check binds every descriptor to the named
        // root/children namespace before publication.
        let _publication_lock = NamespaceLock::acquire(&self.anchor.root)?;
        self.anchor
            .validate(&self.objects, &self.originals, &self.refs, &self.quarantine)?;
        let sidecar_name = raw_id_name(&object.raw_id)?;
        let safe_digest = sha256_digest(&object.bytes);
        let safe_length =
            u64::try_from(object.bytes.len()).map_err(|_| RawStorageError::Refused)?;
        let original_digest = sha256_digest(&object.original_bytes);
        let original_length =
            u64::try_from(object.original_bytes.len()).map_err(|_| RawStorageError::Refused)?;
        let object_name = digest_name(&safe_digest)?;
        let original_object_name = digest_name(&original_digest)?;

        let reference = RawObjectRef {
            raw_id: object.raw_id,
            request_id: object.request_id,
            object_kind: object.object_kind,
            canonicalization: object.canonicalization,
            sha256: safe_digest.clone(),
            byte_length: safe_length,
            original_sha256: original_digest.clone(),
            original_byte_length: original_length,
            bytes_base64: BASE64.encode(&object.bytes),
            media_type: object.media_type,
            storage_ref: content_addressed_storage_ref(&safe_digest),
            original_storage_ref: content_addressed_storage_ref(&original_digest),
        };
        if !sidecar_size_within_limit(&reference) {
            return Err(RawStorageError::Refused);
        }
        let sidecar_bytes = sidecar_bytes(&reference)?;
        if sidecar_bytes.len() > MAX_RAW_SIDECAR_BYTES {
            return Err(RawStorageError::Refused);
        }
        // A valid raw ID can already be bound to a different response. Read
        // that sidecar before publishing anything so a collision cannot
        // create an unreferenced object as a side effect. The journal is the
        // durable transaction intent; the public sidecar is installed only
        // after both immutable CAS objects are complete.
        ensure_sidecar_slot(&self.refs, &sidecar_name, &sidecar_bytes)?;
        let transaction_name = transaction_name(&reference.raw_id)?;
        ensure_sidecar_slot(&self.refs, &transaction_name, &sidecar_bytes)?;
        publish_if_absent(
            &self.refs,
            &transaction_name,
            &sidecar_bytes,
            MAX_RAW_SIDECAR_BYTES,
            &self.quarantine,
            "refs",
        )?;
        publish_if_absent(
            &self.originals,
            &original_object_name,
            &object.original_bytes,
            MAX_RAW_OBJECT_BYTES,
            &self.quarantine,
            "original",
        )?;
        publish_if_absent(
            &self.objects,
            &object_name,
            &object.bytes,
            MAX_RAW_OBJECT_BYTES,
            &self.quarantine,
            "sha256",
        )?;
        publish_if_absent(
            &self.refs,
            &sidecar_name,
            &sidecar_bytes,
            MAX_RAW_SIDECAR_BYTES,
            &self.quarantine,
            "refs",
        )?;
        remove_exact_file(
            &self.refs,
            &transaction_name,
            &sidecar_bytes,
            MAX_RAW_SIDECAR_BYTES,
            &self.quarantine,
            "refs",
        )?;
        self.anchor
            .validate(&self.objects, &self.originals, &self.refs, &self.quarantine)?;
        Ok(reference)
    }

    #[cfg(unix)]
    fn verify_unix(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
        let _namespace_lock = NamespaceLock::acquire(&self.anchor.root)?;
        self.anchor
            .validate(&self.objects, &self.originals, &self.refs, &self.quarantine)?;
        if !valid_digest(&reference.original_sha256)
            || reference.storage_ref != content_addressed_storage_ref(&reference.sha256)
            || reference.original_storage_ref
                != content_addressed_storage_ref(&reference.original_sha256)
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

        let original_name = digest_name(&reference.original_sha256)?;
        let original_bytes = read_named(&self.originals, &original_name, MAX_RAW_OBJECT_BYTES)?;
        if original_bytes.len() as u64 != reference.original_byte_length
            || sha256_digest(&original_bytes) != reference.original_sha256
        {
            return Err(RawStorageError::Unbound);
        }

        let sidecar_name = raw_id_name(&reference.raw_id)?;
        let actual_sidecar = read_named(&self.refs, &sidecar_name, MAX_RAW_SIDECAR_BYTES)?;
        if actual_sidecar != expected_sidecar {
            return Err(RawStorageError::Unbound);
        }
        self.anchor
            .validate(&self.objects, &self.originals, &self.refs, &self.quarantine)?;
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
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
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
        "original_storage_ref": reference.original_storage_ref,
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
        reference.original_storage_ref.as_str(),
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct FileIdentity {
    device: u64,
    inode: u64,
    nlink: u64,
    mode: u32,
}

#[cfg(unix)]
impl From<FileIdentity> for RetentionIdentity {
    fn from(identity: FileIdentity) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
            nlink: identity.nlink,
            mode: identity.mode,
        }
    }
}

#[cfg(unix)]
impl RetentionIdentity {
    fn file_identity(&self) -> FileIdentity {
        FileIdentity {
            device: self.device,
            inode: self.inode,
            nlink: self.nlink,
            mode: self.mode,
        }
    }
}

#[cfg(unix)]
impl FileIdentity {
    fn is_regular_single_link(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32 && self.nlink == 1
    }

    fn is_private_regular_single_link(self) -> bool {
        self.is_regular_single_link() && self.mode & 0o400 != 0 && self.mode & 0o077 == 0
    }

    fn is_private_regular(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
            && self.mode & 0o400 != 0
            && self.mode & 0o077 == 0
    }

    fn same_inode(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }

    fn same_directory(self, other: Self) -> bool {
        self.device == other.device
            && self.inode == other.inode
            && self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
            && other.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
            && self.mode & 0o7777 == other.mode & 0o7777
    }
}

#[cfg(unix)]
struct NamespaceAnchor {
    parent: File,
    root_name: CString,
    root: File,
    root_identity: FileIdentity,
    objects_identity: FileIdentity,
    originals_identity: FileIdentity,
    refs_identity: FileIdentity,
    quarantine_identity: FileIdentity,
}

#[cfg(unix)]
impl NamespaceAnchor {
    fn new(
        parent: File,
        root_name: CString,
        root: File,
        objects: &File,
        originals: &File,
        refs: &File,
        quarantine: &File,
    ) -> io::Result<Self> {
        let root_identity = stat_fd(&root)?;
        let objects_identity = stat_fd(objects)?;
        let originals_identity = stat_fd(originals)?;
        let refs_identity = stat_fd(refs)?;
        let quarantine_identity = stat_fd(quarantine)?;
        let expected = anchor_bytes([
            root_identity,
            objects_identity,
            originals_identity,
            refs_identity,
            quarantine_identity,
        ]);
        let marker_name = anchor_marker_name(&root_name)?;
        open_anchor_marker(&parent, &marker_name, &expected)?;
        Ok(Self {
            parent,
            root_name,
            root,
            root_identity,
            objects_identity,
            originals_identity,
            refs_identity,
            quarantine_identity,
        })
    }

    fn validate(
        &self,
        objects: &File,
        originals: &File,
        refs: &File,
        quarantine: &File,
    ) -> Result<(), RawStorageError> {
        let current_root = stat_fd(&self.root).map_err(storage_io)?;
        let current_objects = stat_fd(objects).map_err(storage_io)?;
        let current_originals = stat_fd(originals).map_err(storage_io)?;
        let current_refs = stat_fd(refs).map_err(storage_io)?;
        let current_quarantine = stat_fd(quarantine).map_err(storage_io)?;
        let named_root = stat_at(&self.parent, &self.root_name)?;
        let identities_match = current_root.same_directory(self.root_identity)
            && current_objects.same_directory(self.objects_identity)
            && current_originals.same_directory(self.originals_identity)
            && current_refs.same_directory(self.refs_identity)
            && current_quarantine.same_directory(self.quarantine_identity)
            && named_root.same_directory(self.root_identity);
        if !identities_match {
            return Err(RawStorageError::Unbound);
        }
        for (name, expected) in [
            (b"sha256\0".as_slice(), self.objects_identity),
            (b"original\0".as_slice(), self.originals_identity),
            (b"refs\0".as_slice(), self.refs_identity),
            (
                b".velnor-raw-quarantine\0".as_slice(),
                self.quarantine_identity,
            ),
        ] {
            let name = CStr::from_bytes_with_nul(name).map_err(|_| RawStorageError::Unbound)?;
            if !stat_at(&self.root, name)?.same_directory(expected) {
                return Err(RawStorageError::Unbound);
            }
        }
        Ok(())
    }

    fn reconcile(
        &self,
        objects: &File,
        originals: &File,
        refs: &File,
        quarantine: &File,
    ) -> io::Result<()> {
        reconcile_namespace(objects, originals, refs, quarantine).map_err(raw_storage_io_error)
    }
}

#[cfg(unix)]
fn anchor_marker_name(root_name: &CStr) -> io::Result<CString> {
    let mut marker = b".velnor-raw-anchor-".to_vec();
    for byte in root_name.to_bytes() {
        marker.extend(format!("{byte:02x}").bytes());
    }
    CString::new(marker)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "anchor name contains NUL"))
}

#[cfg(unix)]
fn anchor_bytes(identities: [FileIdentity; 5]) -> Vec<u8> {
    let mut bytes = b"VLNOR-RAW-ANCHOR-V2\0".to_vec();
    for identity in identities {
        bytes.extend(identity.device.to_le_bytes());
        bytes.extend(identity.inode.to_le_bytes());
        bytes.extend(identity.mode.to_le_bytes());
    }
    bytes
}

#[cfg(unix)]
fn open_anchor_marker(parent: &File, name: &CStr, expected: &[u8]) -> io::Result<File> {
    let flags = libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let mut fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    let created = if fd < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
        };
        true
    } else {
        false
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let identity = stat_fd(&file)?;
    if !identity.is_private_regular_single_link() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "raw store namespace anchor is not a private regular file",
        ));
    }
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if created {
        file.write_all(expected)?;
        file.sync_all()?;
        parent.sync_all()?;
        return Ok(file);
    }
    let actual = read_bounded_file(&mut file, expected.len())?;
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "raw store namespace anchor mismatch",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn read_bounded_file(file: &mut File, max_bytes: usize) -> io::Result<Vec<u8>> {
    let before = stat_fd(file)?;
    if !before.is_private_regular_single_link() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "raw store anchor is not a private regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take((max_bytes as u64) + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "raw store anchor is oversized",
        ));
    }
    let after = stat_fd(file)?;
    if after != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "raw store anchor changed during read",
        ));
    }
    Ok(bytes)
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
fn restrict_directory(directory: &File) -> io::Result<()> {
    if unsafe { libc::fchmod(directory.as_raw_fd(), 0o700) } < 0 {
        return Err(io::Error::last_os_error());
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
struct SecureDirectory {
    file: File,
    parent: File,
    name: CString,
}

#[cfg(unix)]
fn open_secure_directory(path: &Path) -> io::Result<SecureDirectory> {
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

    let name = components.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "raw store root must name a directory component",
        )
    })?;
    let name_c = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "raw store root component contains NUL",
        )
    })?;
    let mut parent = open_start(path)?;
    for component in components {
        parent = open_directory_at(&parent, component, true)?;
    }
    let file = open_directory_at(&parent, name, true)?;
    Ok(SecureDirectory {
        file,
        parent,
        name: name_c,
    })
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
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
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
fn transaction_name(raw_id: &str) -> Result<CString, RawStorageError> {
    if raw_id.is_empty()
        || raw_id.len() > 160
        || !raw_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RawStorageError::Unbound);
    }
    CString::new(format!("{raw_id}.txn")).map_err(|_| RawStorageError::Unbound)
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
fn open_directory_named(directory: &File, name: &CStr) -> Result<Option<File>, RawStorageError> {
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
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
fn is_quarantine_name(name: &CStr) -> bool {
    name.to_bytes().starts_with(b".velnor-raw-quarantine-")
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
    if !before.is_private_regular_single_link() {
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
    if !after.is_private_regular_single_link() || !before.same_inode(after) {
        return Err(RawStorageError::Unbound);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn reconcile_namespace(
    objects: &File,
    originals: &File,
    refs: &File,
    quarantine: &File,
) -> Result<(), RawStorageError> {
    // The retention namespace is a durable journal, not a best-effort cache.
    // Validate its complete bounded schema before touching public namespaces;
    // malformed or unknown children fail closed with no quota-driven deletion.
    retention_usage(quarantine)?;
    // Recover durable intents before scanning public references. A complete
    // intent can finish publication after a crash; an incomplete one is
    // discarded and its unreferenced objects are swept below.
    for name in directory_names(refs)? {
        let bytes = name.as_bytes();
        if bytes.starts_with(b".velnor-raw-") {
            reconcile_temporary(refs, &name, quarantine, "refs")?;
            continue;
        }
        let Some(raw_id_bytes) = bytes.strip_suffix(b".txn") else {
            continue;
        };
        let Ok(raw_id) = std::str::from_utf8(raw_id_bytes) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
            continue;
        };
        let Ok(expected_name) = transaction_name(raw_id) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
            continue;
        };
        if expected_name.as_bytes() != name.as_bytes() {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
            continue;
        }
        let transaction_bytes = read_named(refs, &name, MAX_RAW_SIDECAR_BYTES)?;
        let Some(reference) = parse_reference(raw_id, &transaction_bytes) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
            continue;
        };
        let sidecar_name = raw_id_name(raw_id)?;
        if object_bundle_matches(objects, originals, &reference) {
            let expected_sidecar = sidecar_bytes(&reference)?;
            if transaction_bytes != expected_sidecar {
                remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
                continue;
            }
            ensure_sidecar_slot(refs, &sidecar_name, &expected_sidecar)?;
            publish_if_absent(
                refs,
                &sidecar_name,
                &expected_sidecar,
                MAX_RAW_SIDECAR_BYTES,
                quarantine,
                "refs",
            )?;
        }
        remove_exact_file(
            refs,
            &name,
            &transaction_bytes,
            MAX_RAW_SIDECAR_BYTES,
            quarantine,
            "refs",
        )?;
    }

    let mut safe_names = HashSet::new();
    let mut original_names = HashSet::new();
    for name in directory_names(refs)? {
        let bytes = name.as_bytes();
        if bytes.starts_with(b".velnor-raw-") {
            reconcile_temporary(refs, &name, quarantine, "refs")?;
            continue;
        }
        if bytes.ends_with(b".txn") {
            continue;
        }
        let Some(raw_id_bytes) = bytes.strip_suffix(b".json") else {
            continue;
        };
        let Ok(raw_id) = std::str::from_utf8(raw_id_bytes) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
            continue;
        };
        let Ok(expected_name) = raw_id_name(raw_id) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
            continue;
        };
        if expected_name.as_bytes() != name.as_bytes() {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
            continue;
        }
        let valid = parse_reference(raw_id, &read_named(refs, &name, MAX_RAW_SIDECAR_BYTES)?)
            .is_some_and(|reference| {
                if !object_bundle_matches(objects, originals, &reference) {
                    return false;
                }
                let Some(safe_name) = reference.sha256.strip_prefix("sha256:") else {
                    return false;
                };
                let Some(original_name) = reference.original_sha256.strip_prefix("sha256:") else {
                    return false;
                };
                safe_names.insert(safe_name.to_owned());
                original_names.insert(original_name.to_owned());
                true
            });
        if !valid {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), quarantine, "refs")?;
        }
    }
    reconcile_object_directory(objects, &safe_names, quarantine, "sha256")?;
    reconcile_object_directory(originals, &original_names, quarantine, "original")?;
    sync_directory(objects)?;
    sync_directory(originals)?;
    sync_directory(refs)?;
    retention_usage(quarantine)?;
    sync_directory(quarantine)?;
    Ok(())
}

#[cfg(unix)]
fn parse_reference(raw_id: &str, bytes: &[u8]) -> Option<RawObjectRef> {
    let reference = serde_json::from_slice::<RawObjectRef>(bytes).ok()?;
    if reference.raw_id != raw_id
        || !valid_digest(&reference.sha256)
        || !valid_digest(&reference.original_sha256)
        || reference.storage_ref != content_addressed_storage_ref(&reference.sha256)
        || reference.original_storage_ref
            != content_addressed_storage_ref(&reference.original_sha256)
        || !sidecar_size_within_limit(&reference)
    {
        return None;
    }
    let expected = sidecar_bytes(&reference).ok()?;
    (bytes == expected).then_some(reference)
}

#[cfg(unix)]
fn object_bundle_matches(objects: &File, originals: &File, reference: &RawObjectRef) -> bool {
    object_matches(
        objects,
        &reference.sha256,
        reference.byte_length,
        Some(&reference.bytes_base64),
    ) && object_matches(
        originals,
        &reference.original_sha256,
        reference.original_byte_length,
        None,
    )
}

#[cfg(unix)]
fn object_matches(
    directory: &File,
    digest: &str,
    byte_length: u64,
    expected_base64: Option<&str>,
) -> bool {
    let Ok(name) = digest_name(digest) else {
        return false;
    };
    let Ok(bytes) = read_named(directory, &name, MAX_RAW_OBJECT_BYTES) else {
        return false;
    };
    bytes.len() as u64 == byte_length
        && sha256_digest(&bytes) == digest
        && expected_base64.is_none_or(|expected| BASE64.encode(&bytes) == expected)
}

#[cfg(unix)]
fn reconcile_object_directory(
    directory: &File,
    referenced: &HashSet<String>,
    quarantine: &File,
    source_namespace: &str,
) -> Result<(), RawStorageError> {
    for name in directory_names(directory)? {
        let bytes = name.as_bytes();
        let temporary = bytes.starts_with(b".velnor-raw-");
        let digest = if bytes.len() == 64
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            std::str::from_utf8(bytes).ok()
        } else {
            None
        };
        if temporary {
            reconcile_temporary(directory, &name, quarantine, source_namespace)?;
        } else if digest.is_some_and(|digest| !referenced.contains(digest)) {
            // A final object may leave its public namespace only when it is
            // still a private, single-link regular file. Hardlink/symlink
            // replacements fail closed and remain untouched; accepted or
            // raced entries move into descriptor-bound retention.
            remove_private_named(
                directory,
                &name,
                Some(MAX_RAW_OBJECT_BYTES),
                quarantine,
                source_namespace,
            )?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn directory_names(directory: &File) -> Result<Vec<CString>, RawStorageError> {
    let dot = CString::new(".").map_err(|_| RawStorageError::Refused)?;
    let duplicate = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            dot.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if duplicate < 0 {
        return Err(storage_io(io::Error::last_os_error()));
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(duplicate);
        }
        return Err(storage_io(error));
    }
    let mut names = Vec::new();
    loop {
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let name = CString::new(name.to_bytes()).map_err(|_| RawStorageError::Unbound)?;
        names.push(name);
    }
    if unsafe { libc::closedir(stream) } < 0 {
        return Err(storage_io(io::Error::last_os_error()));
    }
    Ok(names)
}

#[cfg(unix)]
fn remove_private_named(
    directory: &File,
    name: &CStr,
    max_bytes: Option<usize>,
    retention: &File,
    source_namespace: &str,
) -> Result<(), RawStorageError> {
    let Some(mut file) = (match open_named(directory, name) {
        Ok(file) => file,
        // Reconciliation is fail-closed for hostile namespace entries.  A
        // symlink, FIFO, device, or other non-regular entry is operator-owned
        // until an explicit policy handles it; never turn it into an init DoS
        // and never unlink it by name.
        Err(RawStorageError::Refused) => return Ok(()),
        Err(error) => return Err(error),
    }) else {
        return Ok(());
    };
    let before = stat_fd(&file).map_err(storage_io)?;
    if !before.is_private_regular_single_link() {
        return Ok(());
    }
    if let Some(max_bytes) = max_bytes {
        let _ = read_verified_fd(&mut file, max_bytes)?;
    }
    let after = stat_fd(&file).map_err(storage_io)?;
    if after != before {
        return Ok(());
    }
    match quarantine_remove(
        directory,
        name,
        before,
        max_bytes,
        None,
        Some(1),
        retention,
        source_namespace,
    )? {
        QuarantineResult::Retained | QuarantineResult::Left => Ok(()),
    }
}

#[cfg(unix)]
fn remove_exact_file(
    directory: &File,
    name: &CStr,
    expected: &[u8],
    max_bytes: usize,
    retention: &File,
    source_namespace: &str,
) -> Result<(), RawStorageError> {
    let Some(mut file) = open_named(directory, name)? else {
        return Ok(());
    };
    let before = stat_fd(&file).map_err(storage_io)?;
    if !before.is_private_regular_single_link()
        || read_verified_fd(&mut file, max_bytes)? != expected
    {
        return Err(RawStorageError::Refused);
    }
    let after = stat_fd(&file).map_err(storage_io)?;
    if after != before {
        return Err(RawStorageError::Refused);
    }
    match quarantine_remove(
        directory,
        name,
        before,
        Some(max_bytes),
        Some(expected),
        Some(1),
        retention,
        source_namespace,
    )? {
        QuarantineResult::Retained => Ok(()),
        // A replacement won the atomic move or changed the owned quarantine
        // entry. It remains quarantined and the caller must fail closed.
        QuarantineResult::Left => Err(RawStorageError::Refused),
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuarantineResult {
    Retained,
    Left,
}

/// Move a candidate into a fresh, private directory before journal retention.
///
/// `unlinkat(directory, name)` after an FD identity check is not an identity
/// operation: another writer can replace `name` between the check and the
/// unlink. The source pathname is therefore never unlinked. An atomic
/// no-clobber rename transfers the entry into a directory created by this
/// process, where the entry is re-opened and re-verified. A mismatched,
/// hostile, or otherwise unknown entry is left in that quarantine directory;
/// it is never deleted by pathname. The source quarantine remains reachable
/// by its descriptor and the store writes a separately validated immutable
/// retention record. This gives crash recovery a durable, descriptor-bound
/// owner without a final stat-then-unlink race or source-path replacement
/// window.
#[cfg(unix)]
#[allow(
    clippy::too_many_arguments,
    reason = "the cleanup identity and byte expectations are kept explicit at the descriptor boundary"
)]
fn quarantine_remove(
    directory: &File,
    name: &CStr,
    expected: FileIdentity,
    max_bytes: Option<usize>,
    expected_bytes: Option<&[u8]>,
    expected_links: Option<u64>,
    retention: &File,
    source_namespace: &str,
) -> Result<QuarantineResult, RawStorageError> {
    quarantine_admission(directory, retention)?;
    let (quarantine_name, quarantine) = create_quarantine_directory(directory)?;
    let entry_name = CString::new("entry").map_err(|_| RawStorageError::Refused)?;
    match rename_no_clobber(directory, name, &quarantine, &entry_name) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
            return retain_quarantine_result(
                directory,
                &quarantine_name,
                &quarantine,
                retention,
                source_namespace,
            );
        }
        Err(error) => {
            let _ = retain_quarantine_result(
                directory,
                &quarantine_name,
                &quarantine,
                retention,
                source_namespace,
            );
            return Err(storage_io(error));
        }
    }

    let Some(mut file) = (match open_named(&quarantine, &entry_name) {
        Ok(file) => file,
        // O_NOFOLLOW and O_NONBLOCK make symlink/FIFO replacements safe. Keep
        // the moved entry for operator inspection instead of deleting it.
        Err(RawStorageError::Refused) => {
            return retain_quarantine_result(
                directory,
                &quarantine_name,
                &quarantine,
                retention,
                source_namespace,
            )
        }
        Err(error) => {
            let _ = retain_quarantine_result(
                directory,
                &quarantine_name,
                &quarantine,
                retention,
                source_namespace,
            );
            return Err(error);
        }
    }) else {
        return retain_quarantine_result(
            directory,
            &quarantine_name,
            &quarantine,
            retention,
            source_namespace,
        );
    };
    let actual = stat_fd(&file).map_err(storage_io)?;
    if !actual.is_private_regular()
        || !actual.same_inode(expected)
        || expected_links.is_some_and(|links| actual.nlink != links)
    {
        return retain_quarantine_result(
            directory,
            &quarantine_name,
            &quarantine,
            retention,
            source_namespace,
        );
    }
    let bytes = if let Some(max_bytes) = max_bytes {
        match read_verified_fd(&mut file, max_bytes) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                let _ = retain_quarantine_result(
                    directory,
                    &quarantine_name,
                    &quarantine,
                    retention,
                    source_namespace,
                );
                return Err(error);
            }
        }
    } else {
        None
    };
    if expected_bytes.is_some_and(|expected| bytes.as_deref() != Some(expected)) {
        return retain_quarantine_result(
            directory,
            &quarantine_name,
            &quarantine,
            retention,
            source_namespace,
        );
    }
    let after = stat_fd(&file).map_err(storage_io)?;
    if after != actual
        || !after.same_inode(expected)
        || expected_links.is_some_and(|links| after.nlink != links)
    {
        return retain_quarantine_result(
            directory,
            &quarantine_name,
            &quarantine,
            retention,
            source_namespace,
        );
    }

    retain_quarantine_result(
        directory,
        &quarantine_name,
        &quarantine,
        retention,
        source_namespace,
    )
}

#[cfg(unix)]
fn quarantine_admission(directory: &File, retention: &File) -> Result<(), RawStorageError> {
    let usage = retention_usage(retention)?;
    if usage.entries >= MAX_RETAINED_ENTRIES {
        return Err(RawStorageError::Refused);
    }
    let pending = directory_names(directory)?
        .iter()
        .filter(|name| is_quarantine_name(name))
        .count();
    if pending >= MAX_RETAINED_ENTRIES {
        return Err(RawStorageError::Refused);
    }
    Ok(())
}

#[cfg(unix)]
fn create_quarantine_directory(directory: &File) -> Result<(CString, File), RawStorageError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    for attempt in 0..64_u32 {
        let name = CString::new(format!(
            ".velnor-raw-quarantine-{}-{sequence}-{attempt}",
            std::process::id()
        ))
        .map_err(|_| RawStorageError::Refused)?;
        let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                continue;
            }
            return Err(storage_io(error));
        }
        let Some(quarantine) = open_directory_named(directory, &name)? else {
            return Err(RawStorageError::Unavailable);
        };
        let expected = stat_fd(&quarantine).map_err(storage_io)?;
        let named = stat_at(directory, &name)?;
        if !named.same_directory(expected) {
            return Err(RawStorageError::Refused);
        }
        restrict_directory(&quarantine).map_err(storage_io)?;
        if !stat_fd(&quarantine)
            .map_err(storage_io)?
            .same_directory(expected)
        {
            return Err(RawStorageError::Refused);
        }
        return Ok((name, quarantine));
    }
    Err(RawStorageError::Unavailable)
}

#[cfg(unix)]
fn retain_quarantine_result(
    parent: &File,
    name: &CStr,
    quarantine: &File,
    retention: &File,
    source_namespace: &str,
) -> Result<QuarantineResult, RawStorageError> {
    let retained =
        materialize_retained_record(parent, name, quarantine, retention, source_namespace)?;
    Ok(if retained {
        QuarantineResult::Retained
    } else {
        QuarantineResult::Left
    })
}

#[cfg(unix)]
fn materialize_retained_record(
    parent: &File,
    name: &CStr,
    quarantine: &File,
    retention: &File,
    source_namespace: &str,
) -> Result<bool, RawStorageError> {
    let expected = stat_fd(quarantine).map_err(storage_io)?;
    if expected.mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32 || expected.mode & 0o777 != 0o700
    {
        return Ok(false);
    }
    let named = match stat_at(parent, name) {
        Ok(identity) => identity,
        Err(RawStorageError::Unavailable) => return Ok(false),
        Err(_) => return Ok(false),
    };
    if !named.same_directory(expected) {
        return Ok(false);
    }
    validate_quarantine_children(quarantine)?;
    let usage = retention_usage(retention)?;
    let source_name = source_name_string(name)?;
    let parent_identity = stat_fd(parent).map_err(storage_io)?;
    let manifest_name =
        CStr::from_bytes_with_nul(RETENTION_MANIFEST_NAME).map_err(|_| RawStorageError::Refused)?;
    let entry_name =
        CStr::from_bytes_with_nul(RETENTION_ENTRY_NAME).map_err(|_| RawStorageError::Refused)?;
    let entry = read_retained_entry(quarantine, entry_name)?;

    if let Some(manifest_bytes) =
        read_named_if_present(quarantine, manifest_name, MAX_RETAINED_MANIFEST_BYTES)?
    {
        let manifest = parse_retention_manifest(&manifest_bytes)?;
        validate_manifest_source(
            &manifest,
            &source_name,
            source_namespace,
            parent_identity,
            expected,
        )?;
        validate_manifest_entry(&manifest, entry.as_ref())?;
        if validate_retained_record_by_name(retention, &manifest.record_name, &manifest_bytes)? {
            return Ok(true);
        }
    }

    if let Some((record_name, manifest_bytes)) = find_matching_retained_record(
        retention,
        &source_name,
        source_namespace,
        parent_identity,
        expected,
    )? {
        let manifest = parse_retention_manifest(&manifest_bytes)?;
        validate_manifest_entry(&manifest, entry.as_ref())?;
        write_private_file(
            quarantine,
            manifest_name,
            &manifest_bytes,
            MAX_RETAINED_MANIFEST_BYTES,
        )?;
        sync_directory(quarantine)?;
        let _ = record_name;
        return Ok(true);
    }

    let record_name = create_retained_record_name(retention)?;
    let entry_bytes = entry.as_ref().map_or(0, |(_, bytes)| bytes.len() as u64);
    let entry_digest = entry.as_ref().map(|(_, bytes)| sha256_digest(bytes));
    let entry_length = entry.as_ref().map(|(_, bytes)| bytes.len() as u64);
    let manifest_without_entry = retention_manifest(
        &record_name,
        source_namespace,
        &source_name,
        parent_identity,
        expected,
        None,
    );
    let manifest_without_entry_bytes =
        serde_json::to_vec(&manifest_without_entry).map_err(|_| RawStorageError::Refused)?;
    if manifest_without_entry_bytes.len() > MAX_RETAINED_MANIFEST_BYTES {
        return Err(RawStorageError::Refused);
    }
    let next_bytes = usage
        .bytes
        .checked_add(entry_bytes)
        .and_then(|bytes| bytes.checked_add(MAX_RETAINED_MANIFEST_BYTES as u64))
        .ok_or(RawStorageError::Refused)?;
    if usage.entries >= MAX_RETAINED_ENTRIES || next_bytes > MAX_RETAINED_BYTES {
        return Err(RawStorageError::Refused);
    }

    let record = create_retained_record_directory(retention, &record_name)?;
    if let Some((_, bytes)) = &entry {
        write_private_file(&record, entry_name, bytes, MAX_RAW_SIDECAR_BYTES)?;
    }
    let retained_entry = if let (Some(digest), Some(byte_length)) = (entry_digest, entry_length) {
        let entry_file = open_named(&record, entry_name)?.ok_or(RawStorageError::Unavailable)?;
        let identity = stat_fd(&entry_file).map_err(storage_io)?;
        if !identity.is_private_regular_single_link() {
            return Err(RawStorageError::Refused);
        }
        Some(RetentionEntry {
            name: "entry".to_owned(),
            identity: identity.into(),
            byte_length,
            sha256: digest,
        })
    } else {
        None
    };
    let manifest = retention_manifest(
        &record_name,
        source_namespace,
        &source_name,
        parent_identity,
        expected,
        retained_entry,
    );
    let manifest_bytes = serde_json::to_vec(&manifest).map_err(|_| RawStorageError::Refused)?;
    if manifest_bytes.len() > MAX_RETAINED_MANIFEST_BYTES {
        return Err(RawStorageError::Refused);
    }
    write_private_file(
        &record,
        manifest_name,
        &manifest_bytes,
        MAX_RETAINED_MANIFEST_BYTES,
    )?;
    sync_directory(&record)?;
    sync_directory(retention)?;
    write_private_file(
        quarantine,
        manifest_name,
        &manifest_bytes,
        MAX_RETAINED_MANIFEST_BYTES,
    )?;
    sync_directory(quarantine)?;
    sync_directory(parent)?;
    Ok(true)
}

#[cfg(unix)]
fn source_name_string(name: &CStr) -> Result<String, RawStorageError> {
    let bytes = name.to_bytes();
    let value = std::str::from_utf8(bytes).map_err(|_| RawStorageError::Refused)?;
    if value.is_empty() || value.contains('/') || value == "." || value == ".." {
        return Err(RawStorageError::Refused);
    }
    Ok(value.to_owned())
}

#[cfg(unix)]
fn retention_manifest(
    record_name: &CStr,
    source_namespace: &str,
    source_name: &str,
    source_parent: FileIdentity,
    quarantine: FileIdentity,
    entry: Option<RetentionEntry>,
) -> RetentionManifest {
    RetentionManifest {
        schema: RETENTION_SCHEMA_VERSION,
        kind: "velnor-raw-retained-entry".to_owned(),
        record_name: record_name.to_string_lossy().into_owned(),
        source_namespace: source_namespace.to_owned(),
        source_name: source_name.to_owned(),
        source_parent: source_parent.into(),
        quarantine: quarantine.into(),
        entry,
    }
}

#[cfg(unix)]
fn parse_retention_manifest(bytes: &[u8]) -> Result<RetentionManifest, RawStorageError> {
    let manifest =
        serde_json::from_slice::<RetentionManifest>(bytes).map_err(|_| RawStorageError::Refused)?;
    if manifest.schema != RETENTION_SCHEMA_VERSION
        || manifest.kind != "velnor-raw-retained-entry"
        || !matches!(
            manifest.source_namespace.as_str(),
            "sha256" | "original" | "refs"
        )
        || manifest.source_name.is_empty()
        || manifest.source_name.contains('/')
        || matches!(manifest.source_name.as_str(), "." | "..")
        || !valid_retained_name(manifest.record_name.as_bytes())
        || manifest.source_parent.file_identity().mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32
        || manifest.source_parent.file_identity().mode & 0o777 != 0o700
        || manifest.quarantine.file_identity().mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32
        || manifest.quarantine.file_identity().mode & 0o777 != 0o700
    {
        return Err(RawStorageError::Refused);
    }
    if manifest.entry.as_ref().is_some_and(|entry| {
        entry.name != "entry"
            || !entry
                .identity
                .file_identity()
                .is_private_regular_single_link()
            || entry.byte_length > MAX_RAW_SIDECAR_BYTES as u64
            || !valid_digest(&entry.sha256)
    }) {
        return Err(RawStorageError::Refused);
    }
    Ok(manifest)
}

#[cfg(unix)]
fn validate_manifest_source(
    manifest: &RetentionManifest,
    source_name: &str,
    source_namespace: &str,
    parent: FileIdentity,
    quarantine: FileIdentity,
) -> Result<(), RawStorageError> {
    if manifest.source_name != source_name
        || manifest.source_namespace != source_namespace
        || !manifest
            .source_parent
            .file_identity()
            .same_directory(parent)
        || !manifest
            .quarantine
            .file_identity()
            .same_directory(quarantine)
    {
        return Err(RawStorageError::Refused);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_manifest_entry(
    manifest: &RetentionManifest,
    entry: Option<&(FileIdentity, Vec<u8>)>,
) -> Result<(), RawStorageError> {
    match (&manifest.entry, entry) {
        (None, None) => Ok(()),
        (Some(expected), Some((_, bytes)))
            if expected.byte_length == bytes.len() as u64
                && expected.sha256 == sha256_digest(bytes) =>
        {
            Ok(())
        }
        _ => Err(RawStorageError::Refused),
    }
}

#[cfg(unix)]
fn validate_quarantine_children(quarantine: &File) -> Result<(), RawStorageError> {
    for name in directory_names(quarantine)? {
        if !matches!(name.to_bytes(), b"entry" | b"manifest.json") {
            return Err(RawStorageError::Refused);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn read_retained_entry(
    quarantine: &File,
    entry_name: &CStr,
) -> Result<Option<(FileIdentity, Vec<u8>)>, RawStorageError> {
    let identity = match stat_at(quarantine, entry_name) {
        Ok(identity) => identity,
        Err(RawStorageError::Unavailable) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !identity.is_private_regular_single_link() {
        return Err(RawStorageError::Refused);
    }
    let Some(mut file) = open_named(quarantine, entry_name)? else {
        return Err(RawStorageError::Unavailable);
    };
    let before = stat_fd(&file).map_err(storage_io)?;
    let bytes = read_verified_fd(&mut file, MAX_RAW_SIDECAR_BYTES)?;
    let after = stat_fd(&file).map_err(storage_io)?;
    if before != identity || after != identity {
        return Err(RawStorageError::Refused);
    }
    Ok(Some((identity, bytes)))
}

#[cfg(unix)]
fn valid_retained_name(bytes: &[u8]) -> bool {
    let Some(rest) = bytes.strip_prefix(b".velnor-raw-retained-") else {
        return false;
    };
    let mut parts = rest.split(|byte| *byte == b'-');
    parts
        .next()
        .is_some_and(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
        && parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
        && parts
            .next()
            .is_some_and(|part| !part.is_empty() && part.iter().all(u8::is_ascii_digit))
        && parts.next().is_none()
}

#[cfg(unix)]
fn read_retained_record(
    retention: &File,
    name: &CStr,
) -> Result<(File, Vec<u8>, RetentionManifest, u64), RawStorageError> {
    if !valid_retained_name(name.to_bytes()) {
        return Err(RawStorageError::Refused);
    }
    let Some(record) = open_directory_named(retention, name)? else {
        return Err(RawStorageError::Unavailable);
    };
    let identity = stat_fd(&record).map_err(storage_io)?;
    if identity.mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32 || identity.mode & 0o777 != 0o700
    {
        return Err(RawStorageError::Refused);
    }
    let named = stat_at(retention, name)?;
    if !named.same_directory(identity) {
        return Err(RawStorageError::Refused);
    }
    let manifest_name =
        CStr::from_bytes_with_nul(RETENTION_MANIFEST_NAME).map_err(|_| RawStorageError::Refused)?;
    let manifest_bytes = read_named(&record, manifest_name, MAX_RETAINED_MANIFEST_BYTES)?;
    let manifest = parse_retention_manifest(&manifest_bytes)?;
    if manifest.record_name != name.to_string_lossy() {
        return Err(RawStorageError::Refused);
    }
    let entry_name =
        CStr::from_bytes_with_nul(RETENTION_ENTRY_NAME).map_err(|_| RawStorageError::Refused)?;
    let entry_bytes = if let Some(entry) = &manifest.entry {
        let Some(mut file) = open_named(&record, entry_name)? else {
            return Err(RawStorageError::Refused);
        };
        let actual = stat_fd(&file).map_err(storage_io)?;
        if actual != entry.identity.file_identity() {
            return Err(RawStorageError::Refused);
        }
        let bytes = read_verified_fd(&mut file, MAX_RAW_SIDECAR_BYTES)?;
        if bytes.len() as u64 != entry.byte_length || sha256_digest(&bytes) != entry.sha256 {
            return Err(RawStorageError::Refused);
        }
        bytes
    } else {
        match stat_at(&record, entry_name) {
            Ok(_) => return Err(RawStorageError::Refused),
            Err(RawStorageError::Unavailable) => {}
            Err(error) => return Err(error),
        }
        Vec::new()
    };
    let mut names = directory_names(&record)?;
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let expected_names = if manifest.entry.is_some() {
        vec![b"entry".as_slice(), b"manifest.json".as_slice()]
    } else {
        vec![b"manifest.json".as_slice()]
    };
    if names.iter().map(|name| name.to_bytes()).collect::<Vec<_>>() != expected_names {
        return Err(RawStorageError::Refused);
    }
    Ok((record, manifest_bytes, manifest, entry_bytes.len() as u64))
}

#[cfg(unix)]
fn validate_retained_record_by_name(
    retention: &File,
    name: &str,
    expected_manifest: &[u8],
) -> Result<bool, RawStorageError> {
    let name = CString::new(name).map_err(|_| RawStorageError::Refused)?;
    match read_retained_record(retention, &name) {
        Ok((_record, manifest_bytes, _manifest, _bytes)) => Ok(manifest_bytes == expected_manifest),
        Err(RawStorageError::Unavailable) => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn find_matching_retained_record(
    retention: &File,
    source_name: &str,
    source_namespace: &str,
    parent: FileIdentity,
    quarantine: FileIdentity,
) -> Result<Option<(CString, Vec<u8>)>, RawStorageError> {
    for name in directory_names(retention)? {
        if !valid_retained_name(name.to_bytes()) {
            return Err(RawStorageError::Refused);
        }
        let (_record, manifest_bytes, manifest, _bytes) = read_retained_record(retention, &name)?;
        if manifest.source_name == source_name
            && manifest.source_namespace == source_namespace
            && manifest
                .source_parent
                .file_identity()
                .same_directory(parent)
            && manifest
                .quarantine
                .file_identity()
                .same_directory(quarantine)
        {
            return Ok(Some((name, manifest_bytes)));
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn retention_usage(retention: &File) -> Result<RetentionUsage, RawStorageError> {
    let mut usage = RetentionUsage::default();
    for name in directory_names(retention)? {
        let (_record, manifest_bytes, _manifest, entry_bytes) =
            read_retained_record(retention, &name)?;
        usage.entries = usage
            .entries
            .checked_add(1)
            .ok_or(RawStorageError::Refused)?;
        usage.bytes = usage
            .bytes
            .checked_add(manifest_bytes.len() as u64)
            .and_then(|bytes| bytes.checked_add(entry_bytes))
            .ok_or(RawStorageError::Refused)?;
        if usage.entries > MAX_RETAINED_ENTRIES || usage.bytes > MAX_RETAINED_BYTES {
            return Err(RawStorageError::Refused);
        }
    }
    Ok(usage)
}

#[cfg(unix)]
fn create_retained_record_name(retention: &File) -> Result<CString, RawStorageError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    for attempt in 0..64_u32 {
        let name = CString::new(format!(
            ".velnor-raw-retained-{}-{sequence}-{attempt}",
            std::process::id()
        ))
        .map_err(|_| RawStorageError::Refused)?;
        match stat_at(retention, &name) {
            Err(RawStorageError::Unavailable) => return Ok(name),
            Ok(_) => {}
            Err(error) => return Err(error),
        }
    }
    Err(RawStorageError::Unavailable)
}

#[cfg(unix)]
fn create_retained_record_directory(
    retention: &File,
    name: &CStr,
) -> Result<File, RawStorageError> {
    let result = unsafe { libc::mkdirat(retention.as_raw_fd(), name.as_ptr(), 0o700) };
    if result < 0 {
        return Err(storage_io(io::Error::last_os_error()));
    }
    let Some(record) = open_directory_named(retention, name)? else {
        return Err(RawStorageError::Unavailable);
    };
    let expected = stat_fd(&record).map_err(storage_io)?;
    let named = stat_at(retention, name)?;
    if !named.same_directory(expected) || expected.mode & 0o777 != 0o700 {
        return Err(RawStorageError::Refused);
    }
    Ok(record)
}

#[cfg(unix)]
fn write_private_file(
    directory: &File,
    name: &CStr,
    bytes: &[u8],
    max_bytes: usize,
) -> Result<(), RawStorageError> {
    if bytes.len() > max_bytes {
        return Err(RawStorageError::Refused);
    }
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            let actual = read_named(directory, name, max_bytes)?;
            return if actual == bytes {
                Ok(())
            } else {
                Err(RawStorageError::Refused)
            };
        }
        return Err(storage_io(error));
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes).map_err(storage_io)?;
    file.sync_all().map_err(storage_io)?;
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } < 0 {
        return Err(storage_io(io::Error::last_os_error()));
    }
    file.seek(SeekFrom::Start(0)).map_err(storage_io)?;
    if read_verified_fd(&mut file, max_bytes)? != bytes {
        return Err(RawStorageError::Refused);
    }
    sync_directory(directory)?;
    Ok(())
}

#[cfg(unix)]
fn reconcile_temporary(
    directory: &File,
    name: &CStr,
    retention: &File,
    source_namespace: &str,
) -> Result<(), RawStorageError> {
    if is_quarantine_name(name) {
        let quarantine = match open_directory_named(directory, name) {
            Ok(Some(quarantine)) => quarantine,
            // A qname occupied by a symlink, FIFO, regular file, or other
            // unknown entry is operator-owned. Preserve it and continue
            // recovery rather than following it or turning startup into a
            // namespace-denial condition.
            Ok(None) | Err(_) => return Ok(()),
        };
        let _ =
            retain_quarantine_result(directory, name, &quarantine, retention, source_namespace)?;
        return Ok(());
    }
    let Some(file) = (match open_named(directory, name) {
        Ok(file) => file,
        // Leave symlinks and other non-regular hostile entries in place. The
        // store must not follow or unlink an unknown recovery pathname.
        Err(RawStorageError::Refused) => return Ok(()),
        Err(error) => return Err(error),
    }) else {
        return Ok(());
    };
    let identity = stat_fd(&file).map_err(storage_io)?;
    // Recovery has no surviving creator FD after a crash. Remove only the
    // private modes this store creates; leave a replaced/public entry for a
    // later operator decision instead of unlinking an attacker-controlled
    // regular file by name.
    let permissions = identity.mode & 0o777;
    if identity.is_private_regular_single_link() && matches!(permissions, 0o400 | 0o600) {
        remove_private_named(directory, name, None, retention, source_namespace)?;
    }
    Ok(())
}

#[cfg(unix)]
fn publish_if_absent(
    directory: &File,
    name: &CStr,
    bytes: &[u8],
    max_bytes: usize,
    retention: &File,
    source_namespace: &str,
) -> Result<(), RawStorageError> {
    if bytes.len() > max_bytes {
        return Err(RawStorageError::Refused);
    }
    match read_named_if_present(directory, name, max_bytes)? {
        Some(existing) if existing == bytes => return Ok(()),
        Some(_) => return Err(RawStorageError::Refused),
        None => {}
    }
    let mut temporary = TemporaryFile::create(directory, retention, source_namespace)?;
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
                temporary.name_removed = true;
                sync_directory(directory)?;
                let final_identity = stat_at(directory, name)?;
                let temporary_identity = stat_fd(&temporary.file).map_err(storage_io)?;
                if !temporary_identity.is_private_regular_single_link()
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
                // namespace transaction lock is held. An external writer
                // raced the immutable install; reclaim our name only after
                // an identity/link-count check. Drop repeats that check.
                let _ = temporary.remove_name(1);
                Err(RawStorageError::Refused)
            }
            Err(error) => Err(storage_io(error)),
        }
    })()
}

#[cfg(unix)]
fn install_no_clobber(directory: &File, temporary: &CStr, final_name: &CStr) -> io::Result<()> {
    rename_no_clobber(directory, temporary, directory, final_name)
}

#[cfg(unix)]
fn rename_no_clobber(
    source_directory: &File,
    source_name: &CStr,
    destination_directory: &File,
    destination_name: &CStr,
) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            source_directory.as_raw_fd(),
            source_name.as_ptr(),
            destination_directory.as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };

    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            source_directory.as_raw_fd(),
            source_name.as_ptr(),
            destination_directory.as_raw_fd(),
            destination_name.as_ptr(),
            1_i32,
        ) as libc::c_int
    };

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = (
        source_directory,
        source_name,
        destination_directory,
        destination_name,
    );

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
struct ProcessNamespaceLock {
    held: Mutex<bool>,
    wake: Condvar,
}

#[cfg(unix)]
struct ProcessNamespaceGuard {
    lock: Arc<ProcessNamespaceLock>,
}

#[cfg(unix)]
type ProcessLockRegistry = Mutex<HashMap<(u64, u64), Weak<ProcessNamespaceLock>>>;

#[cfg(unix)]
impl ProcessNamespaceGuard {
    fn acquire(lock: Arc<ProcessNamespaceLock>) -> Self {
        let mut held = lock
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *held {
            held = lock
                .wake
                .wait(held)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *held = true;
        drop(held);
        Self { lock }
    }
}

#[cfg(unix)]
impl Drop for ProcessNamespaceGuard {
    fn drop(&mut self) {
        let mut held = self
            .lock
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *held = false;
        self.lock.wake.notify_one();
    }
}

#[cfg(unix)]
fn process_namespace_lock(identity: FileIdentity) -> Arc<ProcessNamespaceLock> {
    static PROCESS_LOCKS: OnceLock<ProcessLockRegistry> = OnceLock::new();
    let registry = PROCESS_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.retain(|_, lock| lock.strong_count() != 0);
    let key = (identity.device, identity.inode);
    if let Some(lock) = registry.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(ProcessNamespaceLock {
        held: Mutex::new(false),
        wake: Condvar::new(),
    });
    registry.insert(key, Arc::downgrade(&lock));
    lock
}

#[cfg(unix)]
struct NamespaceLock<'a> {
    directory: &'a File,
    _process_guard: ProcessNamespaceGuard,
}

#[cfg(unix)]
impl<'a> NamespaceLock<'a> {
    fn acquire(directory: &'a File) -> Result<Self, RawStorageError> {
        let identity = stat_fd(directory).map_err(storage_io)?;
        let process_guard = ProcessNamespaceGuard::acquire(process_namespace_lock(identity));
        let result = unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX) };
        if result < 0 {
            return Err(storage_io(io::Error::last_os_error()));
        }
        Ok(Self {
            directory,
            _process_guard: process_guard,
        })
    }
}

#[cfg(unix)]
impl Drop for NamespaceLock<'_> {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.directory.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(unix)]
struct TemporaryFile<'a> {
    directory: &'a File,
    retention: &'a File,
    source_namespace: &'a str,
    name: CString,
    identity: FileIdentity,
    file: File,
    name_removed: bool,
}

#[cfg(unix)]
impl<'a> TemporaryFile<'a> {
    fn create(
        directory: &'a File,
        retention: &'a File,
        source_namespace: &'a str,
    ) -> Result<Self, RawStorageError> {
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
            if !identity.is_private_regular_single_link() {
                return Err(RawStorageError::Refused);
            }
            return Ok(Self {
                directory,
                retention,
                source_namespace,
                name,
                identity,
                file,
                name_removed: false,
            });
        }
        Err(RawStorageError::Unavailable)
    }

    fn remove_name(&mut self, expected_links: u64) -> Result<(), RawStorageError> {
        let Some(named_file) = open_named(self.directory, &self.name)? else {
            return Err(RawStorageError::Unavailable);
        };
        let named_identity = stat_fd(&named_file).map_err(storage_io)?;
        let current_identity = stat_fd(&self.file).map_err(storage_io)?;
        if !named_identity.same_inode(self.identity)
            || !current_identity.same_inode(self.identity)
            || named_identity.nlink != expected_links
            || current_identity.nlink != expected_links
        {
            return Err(RawStorageError::Refused);
        }
        match quarantine_remove(
            self.directory,
            &self.name,
            self.identity,
            None,
            None,
            Some(expected_links),
            self.retention,
            self.source_namespace,
        )? {
            QuarantineResult::Retained => {
                self.name_removed = true;
                Ok(())
            }
            QuarantineResult::Left => Err(RawStorageError::Refused),
        }
    }
}

#[cfg(unix)]
impl Drop for TemporaryFile<'_> {
    fn drop(&mut self) {
        if self.name_removed {
            return;
        }
        let Ok(Some(named_file)) = open_named(self.directory, &self.name) else {
            return;
        };
        let Ok(named_identity) = stat_fd(&named_file) else {
            return;
        };
        let Ok(current_identity) = stat_fd(&self.file) else {
            return;
        };
        // A successful no-clobber install may transiently have two links on
        // platforms using link-based publication. Never remove a pathname
        // after an identity/link-count mismatch; the name may have been
        // replaced by another writer. Cleanup transfers the entry into the
        // retention namespace instead of unlinking it.
        if !named_identity.same_inode(self.identity)
            || !current_identity.same_inode(self.identity)
            || named_identity.nlink > 2
            || current_identity.nlink > 2
        {
            return;
        }
        if let Ok(QuarantineResult::Retained) = quarantine_remove(
            self.directory,
            &self.name,
            self.identity,
            None,
            None,
            Some(named_identity.nlink),
            self.retention,
            self.source_namespace,
        ) {
            self.name_removed = true;
        }
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

#[cfg(unix)]
fn raw_storage_io_error(error: RawStorageError) -> io::Error {
    let kind = match error {
        RawStorageError::Unavailable => io::ErrorKind::Other,
        RawStorageError::Refused => io::ErrorKind::PermissionDenied,
        RawStorageError::Unbound => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, "raw store namespace reconciliation failed")
}
