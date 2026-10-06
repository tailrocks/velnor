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
#[cfg(unix)]
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
            let anchor = NamespaceAnchor::new(
                secure_root.parent,
                secure_root.name,
                secure_root.file,
                &objects,
                &originals,
                &refs,
            )?;
            drop(setup_lock);
            drop(setup_root);
            let _reconcile_lock =
                NamespaceLock::acquire(&anchor.root).map_err(raw_storage_io_error)?;
            anchor.reconcile(&objects, &originals, &refs)?;
            anchor.root.sync_all()?;
            drop(_reconcile_lock);
            Ok(Self {
                root,
                anchor,
                objects,
                originals,
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
            .validate(&self.objects, &self.originals, &self.refs)?;
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
        // The transaction journal carries only metadata and digest claims. The
        // immutable CAS objects remain the sole payload, so recovery never
        // writes a second payload-sized journal before installing the sidecar.
        let transaction_payload = transaction_bytes(&reference)?;
        if transaction_payload.len() > MAX_RAW_SIDECAR_BYTES {
            return Err(RawStorageError::Refused);
        }
        // A valid raw ID can already be bound to a different response. Read
        // that sidecar before publishing anything so a collision cannot
        // create an unreferenced object as a side effect. The journal is the
        // durable transaction intent; the public sidecar is installed only
        // after both immutable CAS objects are complete.
        ensure_sidecar_slot(&self.refs, &sidecar_name, &sidecar_bytes)?;
        let transaction_name = transaction_name(&reference.raw_id)?;
        ensure_sidecar_slot(&self.refs, &transaction_name, &transaction_payload)?;
        publish_if_absent(
            &self.refs,
            &transaction_name,
            &transaction_payload,
            MAX_RAW_SIDECAR_BYTES,
        )?;
        publish_if_absent(
            &self.originals,
            &original_object_name,
            &object.original_bytes,
            MAX_RAW_OBJECT_BYTES,
        )?;
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
        remove_exact_file(
            &self.refs,
            &transaction_name,
            &transaction_payload,
            MAX_RAW_SIDECAR_BYTES,
        )?;
        self.anchor
            .validate(&self.objects, &self.originals, &self.refs)?;
        Ok(reference)
    }

    #[cfg(unix)]
    fn verify_unix(&self, reference: &RawObjectRef) -> Result<(), RawStorageError> {
        let _namespace_lock = NamespaceLock::acquire(&self.anchor.root)?;
        self.anchor
            .validate(&self.objects, &self.originals, &self.refs)?;
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
            .validate(&self.objects, &self.originals, &self.refs)?;
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

#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTransaction {
    raw_id: String,
    request_id: String,
    object_kind: String,
    canonicalization: String,
    sha256: String,
    byte_length: u64,
    original_sha256: String,
    original_byte_length: u64,
    media_type: String,
    storage_ref: String,
    original_storage_ref: String,
}

#[cfg(unix)]
fn transaction_bytes(reference: &RawObjectRef) -> Result<Vec<u8>, RawStorageError> {
    serde_json::to_vec(&RawTransaction {
        raw_id: reference.raw_id.clone(),
        request_id: reference.request_id.clone(),
        object_kind: reference.object_kind.clone(),
        canonicalization: reference.canonicalization.clone(),
        sha256: reference.sha256.clone(),
        byte_length: reference.byte_length,
        original_sha256: reference.original_sha256.clone(),
        original_byte_length: reference.original_byte_length,
        media_type: reference.media_type.clone(),
        storage_ref: reference.storage_ref.clone(),
        original_storage_ref: reference.original_storage_ref.clone(),
    })
    .map_err(|_| RawStorageError::Refused)
}

#[cfg(unix)]
fn parse_transaction(raw_id: &str, bytes: &[u8]) -> Option<RawTransaction> {
    // Legit compact journals are metadata-only (<4 KiB in practice). Bound the
    // shape so a pathological over-cap journal is swept as debris instead of
    // aborting reconcile on post-completion publish and bricking open().
    // Legacy full-sidecar journals safely fall through to parse_reference.
    if bytes.len() > 64 * 1024 {
        return None;
    }
    let transaction = serde_json::from_slice::<RawTransaction>(bytes).ok()?;
    if transaction.raw_id != raw_id
        || transaction.byte_length > MAX_RAW_OBJECT_BYTES as u64
        || transaction.original_byte_length > MAX_RAW_OBJECT_BYTES as u64
        || !valid_digest(&transaction.sha256)
        || !valid_digest(&transaction.original_sha256)
        || transaction.storage_ref != content_addressed_storage_ref(&transaction.sha256)
        || transaction.original_storage_ref
            != content_addressed_storage_ref(&transaction.original_sha256)
    {
        return None;
    }
    Some(transaction)
}

#[cfg(unix)]
fn complete_transaction_reference(
    objects: &File,
    originals: &File,
    transaction: &RawTransaction,
) -> Option<RawObjectRef> {
    let object_name = digest_name(&transaction.sha256).ok()?;
    let safe_bytes = read_named(objects, &object_name, MAX_RAW_OBJECT_BYTES).ok()?;
    if safe_bytes.len() as u64 != transaction.byte_length
        || sha256_digest(&safe_bytes) != transaction.sha256
    {
        return None;
    }
    let original_name = digest_name(&transaction.original_sha256).ok()?;
    let original_bytes = read_named(originals, &original_name, MAX_RAW_OBJECT_BYTES).ok()?;
    if original_bytes.len() as u64 != transaction.original_byte_length
        || sha256_digest(&original_bytes) != transaction.original_sha256
    {
        return None;
    }
    Some(RawObjectRef {
        raw_id: transaction.raw_id.clone(),
        request_id: transaction.request_id.clone(),
        object_kind: transaction.object_kind.clone(),
        canonicalization: transaction.canonicalization.clone(),
        sha256: transaction.sha256.clone(),
        byte_length: transaction.byte_length,
        original_sha256: transaction.original_sha256.clone(),
        original_byte_length: transaction.original_byte_length,
        bytes_base64: BASE64.encode(safe_bytes),
        media_type: transaction.media_type.clone(),
        storage_ref: transaction.storage_ref.clone(),
        original_storage_ref: transaction.original_storage_ref.clone(),
    })
}

#[cfg(unix)]
fn parse_transaction_journal(
    objects: &File,
    originals: &File,
    raw_id: &str,
    bytes: &[u8],
) -> Option<RawObjectRef> {
    if let Some(transaction) = parse_transaction(raw_id, bytes) {
        return complete_transaction_reference(objects, originals, &transaction);
    }
    // Upgrade window: journals written before the compact format carry the
    // full sidecar. Both shapes deny unknown fields, so they never overlap.
    parse_reference(raw_id, bytes)
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    nlink: u64,
    mode: u32,
}

#[cfg(unix)]
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
impl FileIdentity {
    fn is_regular_single_link(self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32 && self.nlink == 1
    }

    fn is_private_regular_single_link(self) -> bool {
        self.is_regular_single_link() && self.mode & 0o400 != 0 && self.mode & 0o077 == 0
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
    ) -> io::Result<Self> {
        let root_identity = stat_fd(&root)?;
        let objects_identity = stat_fd(objects)?;
        let originals_identity = stat_fd(originals)?;
        let refs_identity = stat_fd(refs)?;
        let expected = anchor_bytes([
            root_identity,
            objects_identity,
            originals_identity,
            refs_identity,
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
        })
    }

    fn validate(
        &self,
        objects: &File,
        originals: &File,
        refs: &File,
    ) -> Result<(), RawStorageError> {
        let current_root = stat_fd(&self.root).map_err(storage_io)?;
        let current_objects = stat_fd(objects).map_err(storage_io)?;
        let current_originals = stat_fd(originals).map_err(storage_io)?;
        let current_refs = stat_fd(refs).map_err(storage_io)?;
        let named_root = stat_at(&self.parent, &self.root_name)?;
        let identities_match = current_root.same_directory(self.root_identity)
            && current_objects.same_directory(self.objects_identity)
            && current_originals.same_directory(self.originals_identity)
            && current_refs.same_directory(self.refs_identity)
            && named_root.same_directory(self.root_identity);
        if !identities_match {
            return Err(RawStorageError::Unbound);
        }
        for (name, expected) in [
            (b"sha256\0".as_slice(), self.objects_identity),
            (b"original\0".as_slice(), self.originals_identity),
            (b"refs\0".as_slice(), self.refs_identity),
        ] {
            let name = CStr::from_bytes_with_nul(name).map_err(|_| RawStorageError::Unbound)?;
            if !stat_at(&self.root, name)?.same_directory(expected) {
                return Err(RawStorageError::Unbound);
            }
        }
        Ok(())
    }

    fn reconcile(&self, objects: &File, originals: &File, refs: &File) -> io::Result<()> {
        reconcile_namespace(objects, originals, refs).map_err(raw_storage_io_error)
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
fn anchor_bytes(identities: [FileIdentity; 4]) -> Vec<u8> {
    let mut bytes = b"VLNOR-RAW-ANCHOR-V1\0".to_vec();
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
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
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
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
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
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
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
) -> Result<(), RawStorageError> {
    // Recover durable intents before scanning public references. A complete
    // intent can finish publication after a crash; an incomplete one is
    // discarded and its unreferenced objects are swept below.
    for name in directory_names(refs)? {
        let bytes = name.as_bytes();
        if bytes.starts_with(b".velnor-raw-") {
            reconcile_temporary(refs, &name)?;
            continue;
        }
        let Some(raw_id_bytes) = bytes.strip_suffix(b".txn") else {
            continue;
        };
        let Ok(raw_id) = std::str::from_utf8(raw_id_bytes) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
            continue;
        };
        let Ok(expected_name) = transaction_name(raw_id) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
            continue;
        };
        if expected_name.as_bytes() != name.as_bytes() {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
            continue;
        }
        let transaction_payload = read_named(refs, &name, MAX_RAW_SIDECAR_BYTES)?;
        let Some(reference) =
            parse_transaction_journal(objects, originals, raw_id, &transaction_payload)
        else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
            continue;
        };
        let sidecar_name = raw_id_name(raw_id)?;
        // Belt-and-suspenders: complete_transaction_reference already hashed
        // both CAS objects; this re-read re-verifies the bundle before the
        // sidecar is published. Recovery-path only.
        if object_bundle_matches(objects, originals, &reference) {
            // Complete the public sidecar from descriptor-verified CAS bytes;
            // the compact journal itself is intentionally never copied as a
            // payload-sized recovery temporary.
            let expected_sidecar = sidecar_bytes(&reference)?;
            ensure_sidecar_slot(refs, &sidecar_name, &expected_sidecar)?;
            publish_if_absent(
                refs,
                &sidecar_name,
                &expected_sidecar,
                MAX_RAW_SIDECAR_BYTES,
            )?;
        }
        remove_exact_file(refs, &name, &transaction_payload, MAX_RAW_SIDECAR_BYTES)?;
    }

    let mut safe_names = HashSet::new();
    let mut original_names = HashSet::new();
    for name in directory_names(refs)? {
        let bytes = name.as_bytes();
        if bytes.starts_with(b".velnor-raw-") {
            reconcile_temporary(refs, &name)?;
            continue;
        }
        if bytes.ends_with(b".txn") {
            continue;
        }
        let Some(raw_id_bytes) = bytes.strip_suffix(b".json") else {
            continue;
        };
        let Ok(raw_id) = std::str::from_utf8(raw_id_bytes) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
            continue;
        };
        let Ok(expected_name) = raw_id_name(raw_id) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
            continue;
        };
        if expected_name.as_bytes() != name.as_bytes() {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
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
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES))?;
        }
    }
    reconcile_object_directory(objects, &safe_names)?;
    reconcile_object_directory(originals, &original_names)?;
    sync_directory(objects)?;
    sync_directory(originals)?;
    sync_directory(refs)?;
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
            reconcile_temporary(directory, &name)?;
        } else if digest.is_some_and(|digest| !referenced.contains(digest)) {
            // A final object may be removed only when it is still a private,
            // single-link regular file. Hardlink/symlink replacements fail
            // closed and remain untouched.
            remove_private_named(directory, &name, Some(MAX_RAW_OBJECT_BYTES))?;
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
) -> Result<(), RawStorageError> {
    let Some(mut file) = open_named(directory, name)? else {
        return Ok(());
    };
    let before = stat_fd(&file).map_err(storage_io)?;
    if !before.is_regular_single_link() {
        return Err(RawStorageError::Refused);
    }
    if let Some(max_bytes) = max_bytes {
        let _ = read_verified_fd(&mut file, max_bytes)?;
    }
    let after = stat_fd(&file).map_err(storage_io)?;
    if after != before {
        return Err(RawStorageError::Refused);
    }
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        return Err(storage_io(error));
    }
    Ok(())
}

#[cfg(unix)]
fn remove_exact_file(
    directory: &File,
    name: &CStr,
    expected: &[u8],
    max_bytes: usize,
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
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(());
        }
        return Err(storage_io(error));
    }
    Ok(())
}

#[cfg(unix)]
fn reconcile_temporary(directory: &File, name: &CStr) -> Result<(), RawStorageError> {
    let Some(file) = open_named(directory, name)? else {
        return Ok(());
    };
    let identity = stat_fd(&file).map_err(storage_io)?;
    // Recovery has no surviving creator FD after a crash. Remove only the
    // private modes this store creates; leave a replaced/public entry for a
    // later operator decision instead of unlinking an attacker-controlled
    // regular file by name.
    let permissions = identity.mode & 0o777;
    if identity.is_private_regular_single_link() && matches!(permissions, 0o400 | 0o600) {
        remove_private_named(directory, name, None)?;
    }
    Ok(())
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
    name: CString,
    identity: FileIdentity,
    file: File,
    name_removed: bool,
}

#[cfg(unix)]
impl<'a> TemporaryFile<'a> {
    fn create(directory: &'a File) -> Result<Self, RawStorageError> {
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
        let result = unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
        if result < 0 {
            return Err(storage_io(io::Error::last_os_error()));
        }
        self.name_removed = true;
        sync_directory(self.directory)?;
        Ok(())
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
        // platforms using link-based publication. Never unlink a pathname
        // after an identity/link-count mismatch; the name may have been
        // replaced by another writer.
        if !named_identity.same_inode(self.identity)
            || !current_identity.same_inode(self.identity)
            || named_identity.nlink > 2
            || current_identity.nlink > 2
        {
            return;
        }
        let _ = unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
        let _ = self.directory.sync_all();
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
