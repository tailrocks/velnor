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
            if let Some(quarantine) =
                QuarantineNamespace::open_existing_at(&anchor.root).map_err(raw_storage_io_error)?
            {
                reconcile_quarantine_namespace(&quarantine.file).map_err(raw_storage_io_error)?;
            }
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
            None,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupPoint {
    AfterIdentityCheck,
    AfterQuarantineCheck,
    BeforeDestructiveAction,
}

#[cfg(unix)]
type CleanupHook = dyn Fn(CleanupPoint, &File, &CStr) -> Result<(), RawStorageError> + Send + Sync;

#[cfg(unix)]
#[derive(Clone, Copy)]
enum LinkCount {
    Exact(u64),
    AtMost(u64),
}

#[cfg(unix)]
impl LinkCount {
    fn accepts(self, actual: u64) -> bool {
        match self {
            Self::Exact(expected) => actual == expected,
            Self::AtMost(maximum) => actual <= maximum,
        }
    }
}

#[cfg(unix)]
struct CleanupSpec<'a> {
    creator: &'a File,
    expected: FileIdentity,
    links: LinkCount,
    expected_bytes: Option<&'a [u8]>,
    expected_digest: Option<(&'a str, usize)>,
    max_bytes: Option<usize>,
    existing_namespace: Option<&'a File>,
    hook: Option<&'a CleanupHook>,
}

#[cfg(unix)]
const QUARANTINE_DIRECTORY_PREFIX: &[u8] = b".velnor-raw-quarantine-";

#[cfg(unix)]
const QUARANTINE_ENTRY_PREFIX: &[u8] = b".velnor-raw-entry-";

#[cfg(unix)]
const QUARANTINE_NAMESPACE_NAME: &[u8] = b".velnor-raw-quarantine";

#[cfg(unix)]
const QUARANTINE_MANIFEST_NAME: &[u8] = b".velnor-raw-manifest";

#[cfg(unix)]
const QUARANTINE_MANIFEST_MAGIC: &[u8] = b"VLNOR-RAW-QUARANTINE-V1\0";

#[cfg(unix)]
const MAX_QUARANTINE_MANIFEST_BYTES: usize = 512;

#[cfg(unix)]
static NEXT_QUARANTINE: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
struct QuarantineNamespace {
    file: File,
}

#[cfg(unix)]
struct QuarantineDirectory {
    namespace: FileIdentity,
    identity: FileIdentity,
    name: CString,
    file: File,
}

#[cfg(unix)]
struct QuarantineManifest {
    expected: FileIdentity,
    links: LinkCount,
    max_bytes: Option<usize>,
    expected_digest: Option<(usize, String)>,
}

#[cfg(unix)]
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
impl QuarantineManifest {
    fn from_cleanup(
        expected: FileIdentity,
        links: LinkCount,
        max_bytes: Option<usize>,
        expected_bytes: Option<&[u8]>,
        expected_digest: Option<(&str, usize)>,
    ) -> Result<Self, RawStorageError> {
        let expected_digest = match (expected_bytes, expected_digest) {
            (Some(bytes), None) => Some((bytes.len(), sha256_digest(bytes))),
            (None, Some((digest, length))) => Some((length, digest.to_owned())),
            (None, None) => None,
            (Some(_), Some(_)) => return Err(RawStorageError::Refused),
        };
        if expected_digest.is_some() && max_bytes.is_none() {
            return Err(RawStorageError::Refused);
        }
        Ok(Self {
            expected,
            links,
            max_bytes,
            expected_digest,
        })
    }

    fn encode(&self) -> Result<Vec<u8>, RawStorageError> {
        let mut bytes = QUARANTINE_MANIFEST_MAGIC.to_vec();
        bytes.extend_from_slice(&self.expected.device.to_le_bytes());
        bytes.extend_from_slice(&self.expected.inode.to_le_bytes());
        bytes.extend_from_slice(&self.expected.nlink.to_le_bytes());
        bytes.extend_from_slice(&self.expected.mode.to_le_bytes());
        match self.links {
            LinkCount::Exact(value) => {
                bytes.push(0);
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            LinkCount::AtMost(value) => {
                bytes.push(1);
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        let max_bytes = self
            .max_bytes
            .map(u64::try_from)
            .transpose()
            .map_err(|_| RawStorageError::Refused)?
            .unwrap_or(u64::MAX);
        bytes.extend_from_slice(&max_bytes.to_le_bytes());
        match &self.expected_digest {
            Some((length, digest)) => {
                bytes.push(1);
                bytes.extend_from_slice(
                    &u64::try_from(*length)
                        .map_err(|_| RawStorageError::Refused)?
                        .to_le_bytes(),
                );
                let digest = digest.as_bytes();
                let digest_length =
                    u8::try_from(digest.len()).map_err(|_| RawStorageError::Refused)?;
                bytes.push(digest_length);
                bytes.extend_from_slice(digest);
            }
            None => bytes.push(0),
        }
        if bytes.len() > MAX_QUARANTINE_MANIFEST_BYTES {
            return Err(RawStorageError::Refused);
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, RawStorageError> {
        if !bytes.starts_with(QUARANTINE_MANIFEST_MAGIC) {
            return Err(RawStorageError::Refused);
        }
        let mut offset = QUARANTINE_MANIFEST_MAGIC.len();
        let expected = FileIdentity {
            device: read_manifest_u64(bytes, &mut offset)?,
            inode: read_manifest_u64(bytes, &mut offset)?,
            nlink: read_manifest_u64(bytes, &mut offset)?,
            mode: read_manifest_u32(bytes, &mut offset)?,
        };
        let links = match read_manifest_u8(bytes, &mut offset)? {
            0 => LinkCount::Exact(read_manifest_u64(bytes, &mut offset)?),
            1 => LinkCount::AtMost(read_manifest_u64(bytes, &mut offset)?),
            _ => return Err(RawStorageError::Refused),
        };
        let max_bytes = match read_manifest_u64(bytes, &mut offset)? {
            u64::MAX => None,
            value => Some(usize::try_from(value).map_err(|_| RawStorageError::Refused)?),
        };
        let expected_digest = match read_manifest_u8(bytes, &mut offset)? {
            0 => None,
            1 => {
                let length = usize::try_from(read_manifest_u64(bytes, &mut offset)?)
                    .map_err(|_| RawStorageError::Refused)?;
                let digest_length = usize::from(read_manifest_u8(bytes, &mut offset)?);
                let end = offset
                    .checked_add(digest_length)
                    .ok_or(RawStorageError::Refused)?;
                let digest = bytes
                    .get(offset..end)
                    .and_then(|digest| std::str::from_utf8(digest).ok())
                    .filter(|digest| valid_digest(digest))
                    .ok_or(RawStorageError::Refused)?
                    .to_owned();
                offset = end;
                Some((length, digest))
            }
            _ => return Err(RawStorageError::Refused),
        };
        if offset != bytes.len() || expected_digest.is_some() && max_bytes.is_none() {
            return Err(RawStorageError::Refused);
        }
        Ok(Self {
            expected,
            links,
            max_bytes,
            expected_digest,
        })
    }

    fn matches(&self, file: &mut File) -> Result<bool, RawStorageError> {
        let actual = stat_fd(file).map_err(storage_io)?;
        if !actual.same_inode(self.expected)
            || actual.mode & libc::S_IFMT as u32 != libc::S_IFREG as u32
            || actual.mode & 0o7777 != self.expected.mode & 0o7777
            || !self.links.accepts(actual.nlink)
        {
            return Ok(false);
        }
        let Some(max_bytes) = self.max_bytes else {
            return Ok(self.expected_digest.is_none());
        };
        let bytes = match read_verified_fd(file, max_bytes) {
            Ok(bytes) => bytes,
            Err(RawStorageError::Refused | RawStorageError::Unbound) => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(self
            .expected_digest
            .as_ref()
            .is_none_or(|(length, digest)| {
                bytes.len() == *length && sha256_digest(&bytes) == *digest
            }))
    }
}

#[cfg(unix)]
fn read_manifest_u8(bytes: &[u8], offset: &mut usize) -> Result<u8, RawStorageError> {
    let value = *bytes.get(*offset).ok_or(RawStorageError::Refused)?;
    *offset = offset.checked_add(1).ok_or(RawStorageError::Refused)?;
    Ok(value)
}

#[cfg(unix)]
fn read_manifest_u32(bytes: &[u8], offset: &mut usize) -> Result<u32, RawStorageError> {
    let end = offset.checked_add(4).ok_or(RawStorageError::Refused)?;
    let value = bytes
        .get(*offset..end)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(RawStorageError::Refused)?;
    *offset = end;
    Ok(value)
}

#[cfg(unix)]
fn read_manifest_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, RawStorageError> {
    let end = offset.checked_add(8).ok_or(RawStorageError::Refused)?;
    let value = bytes
        .get(*offset..end)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or(RawStorageError::Refused)?;
    *offset = end;
    Ok(value)
}

#[cfg(unix)]
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
impl QuarantineNamespace {
    fn open_for(directory: &File) -> Result<Self, RawStorageError> {
        let parent = open_directory_at(directory, OsStr::new(".."), false).map_err(storage_io)?;
        Self::open_at(&parent, true)?.ok_or(RawStorageError::Unavailable)
    }

    fn open_existing_at(parent: &File) -> Result<Option<Self>, RawStorageError> {
        Self::open_at(parent, false)
    }

    fn open_at(parent: &File, create: bool) -> Result<Option<Self>, RawStorageError> {
        let name = CString::new(QUARANTINE_NAMESPACE_NAME).map_err(|_| RawStorageError::Refused)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        let mut fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        let mut created = false;
        if fd < 0 && create && io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
            let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EEXIST) {
                    return Err(storage_io(error));
                }
            } else {
                created = true;
            }
            fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 {
            let error = io::Error::last_os_error();
            if !create && error.raw_os_error() == Some(libc::ENOENT) {
                return Ok(None);
            }
            return Err(storage_io(error));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let identity = stat_fd(&file).map_err(storage_io)?;
        if identity.mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32
            || identity.mode & 0o7777 != 0o700
        {
            return Err(RawStorageError::Refused);
        }
        if created {
            parent.sync_all().map_err(storage_io)?;
        }
        Ok(Some(Self { file }))
    }
}

#[cfg(unix)]
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
impl QuarantineDirectory {
    fn create(namespace: &File) -> Result<Self, RawStorageError> {
        let namespace_identity = stat_fd(namespace).map_err(storage_io)?;
        let sequence = NEXT_QUARANTINE.fetch_add(1, Ordering::Relaxed);
        for attempt in 0..64_u32 {
            let name = CString::new(format!(
                ".velnor-raw-quarantine-{}-{sequence}-{attempt}",
                std::process::id()
            ))
            .map_err(|_| RawStorageError::Refused)?;
            let result = unsafe { libc::mkdirat(namespace.as_raw_fd(), name.as_ptr(), 0o700) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(storage_io(error));
            }
            let fd = unsafe {
                libc::openat(
                    namespace.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                let error = io::Error::last_os_error();
                // Keep the named directory for the next reconciliation pass;
                // without its creator FD, deleting it by name would risk
                // removing a replacement directory.
                return Err(storage_io(error));
            }
            let file = unsafe { File::from_raw_fd(fd) };
            let identity = stat_fd(&file).map_err(storage_io)?;
            if identity.mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32
                || identity.mode & 0o7777 != 0o700
            {
                return Err(RawStorageError::Refused);
            }
            sync_directory(namespace)?;
            return Ok(Self {
                namespace: namespace_identity,
                identity,
                name,
                file,
            });
        }
        Err(RawStorageError::Unavailable)
    }

    fn claim(&self, directory: &File, name: &CStr) -> Result<CString, RawStorageError> {
        for attempt in 0..64_u32 {
            let quarantine_name = quarantine_entry_name(attempt)?;
            match install_no_clobber(directory, name, &self.file, &quarantine_name) {
                Ok(()) => {
                    sync_directory(&self.file)?;
                    return Ok(quarantine_name);
                }
                Err(error) if error.raw_os_error() == Some(libc::EEXIST) => continue,
                Err(error) => return Err(storage_io(error)),
            }
        }
        Err(RawStorageError::Unavailable)
    }

    fn write_manifest(&self, manifest: &QuarantineManifest) -> Result<(), RawStorageError> {
        let name = CString::new(QUARANTINE_MANIFEST_NAME).map_err(|_| RawStorageError::Refused)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(storage_io(io::Error::last_os_error()));
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(&manifest.encode()?).map_err(storage_io)?;
        file.sync_all().map_err(storage_io)?;
        sync_directory(&self.file)
    }
}

#[cfg(unix)]
fn quarantine_entry_name(attempt: u32) -> Result<CString, RawStorageError> {
    let sequence = NEXT_QUARANTINE.fetch_add(1, Ordering::Relaxed);
    CString::new(format!(
        ".velnor-raw-entry-{}-{sequence}-{attempt}",
        std::process::id()
    ))
    .map_err(|_| RawStorageError::Refused)
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
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
            continue;
        };
        let Ok(expected_name) = transaction_name(raw_id) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
            continue;
        };
        if expected_name.as_bytes() != name.as_bytes() {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
            continue;
        }
        let transaction_payload = read_named(refs, &name, MAX_RAW_SIDECAR_BYTES)?;
        let Some(reference) =
            parse_transaction_journal(objects, originals, raw_id, &transaction_payload)
        else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
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
        remove_exact_file(
            refs,
            &name,
            &transaction_payload,
            MAX_RAW_SIDECAR_BYTES,
            None,
        )?;
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
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
            continue;
        };
        let Ok(expected_name) = raw_id_name(raw_id) else {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
            continue;
        };
        if expected_name.as_bytes() != name.as_bytes() {
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
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
            remove_private_named(refs, &name, Some(MAX_RAW_SIDECAR_BYTES), None)?;
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
            remove_private_named(directory, &name, Some(MAX_RAW_OBJECT_BYTES), None)?;
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
fn quarantine_and_remove(
    directory: &File,
    name: &CStr,
    spec: CleanupSpec<'_>,
) -> Result<(), RawStorageError> {
    let CleanupSpec {
        creator,
        expected,
        links,
        expected_bytes,
        expected_digest,
        max_bytes,
        existing_namespace,
        hook,
    } = spec;
    if let Some(hook) = hook {
        hook(CleanupPoint::AfterIdentityCheck, directory, name)?;
    }

    let namespace = match existing_namespace {
        Some(namespace) => namespace.try_clone().map_err(storage_io)?,
        None => QuarantineNamespace::open_for(directory)?.file,
    };
    let quarantine = QuarantineDirectory::create(&namespace)?;
    if !stat_fd(&namespace)
        .map_err(storage_io)?
        .same_directory(quarantine.namespace)
    {
        return Err(RawStorageError::Refused);
    }
    let manifest_expected = FileIdentity {
        mode: stat_fd(creator).map_err(storage_io)?.mode,
        ..expected
    };
    let manifest = QuarantineManifest::from_cleanup(
        manifest_expected,
        links,
        max_bytes,
        expected_bytes,
        expected_digest,
    )?;
    quarantine.write_manifest(&manifest)?;
    let quarantine_name = quarantine.claim(directory, name)?;
    sync_directory(directory)?;

    let Some(mut quarantined) = open_named(&quarantine.file, &quarantine_name)? else {
        return Err(RawStorageError::Unavailable);
    };
    let quarantined_identity = stat_fd(&quarantined).map_err(storage_io)?;
    let quarantined_bytes = max_bytes
        .map(|max_bytes| read_verified_fd(&mut quarantined, max_bytes))
        .transpose()?;
    let creator_identity = stat_fd(creator).map_err(storage_io)?;
    let first_matches = quarantined_identity.same_inode(expected)
        && creator_identity.same_inode(expected)
        && links.accepts(quarantined_identity.nlink)
        && links.accepts(creator_identity.nlink)
        && cleanup_content_matches(
            quarantined_bytes.as_deref(),
            expected_bytes,
            expected_digest,
        );
    if !first_matches {
        sync_directory(&quarantine.file)?;
        return Err(RawStorageError::Refused);
    }

    if let Some(hook) = hook {
        hook(CleanupPoint::AfterQuarantineCheck, directory, name)?;
    }

    // The entry is now addressed through the protected, creator-held
    // quarantine directory. Recheck it after the test seam and before the
    // destructive operation; a replacement is left recoverable in place.
    let Some(mut still_quarantined) = open_named(&quarantine.file, &quarantine_name)? else {
        return Err(RawStorageError::Unavailable);
    };
    let still_identity = stat_fd(&still_quarantined).map_err(storage_io)?;
    let still_bytes = max_bytes
        .map(|max_bytes| read_verified_fd(&mut still_quarantined, max_bytes))
        .transpose()?;
    let creator_identity = stat_fd(creator).map_err(storage_io)?;
    let second_matches = still_identity.same_inode(expected)
        && creator_identity.same_inode(expected)
        && links.accepts(still_identity.nlink)
        && links.accepts(creator_identity.nlink)
        && cleanup_content_matches(still_bytes.as_deref(), expected_bytes, expected_digest);
    if !second_matches {
        sync_directory(&quarantine.file)?;
        return Err(RawStorageError::Refused);
    }

    if let Some(hook) = hook {
        hook(CleanupPoint::BeforeDestructiveAction, directory, name)?;
    }

    if !stat_fd(&namespace)
        .map_err(storage_io)?
        .same_directory(quarantine.namespace)
        || !stat_fd(&quarantine.file)
            .map_err(storage_io)?
            .same_directory(quarantine.identity)
    {
        sync_directory(&quarantine.file)?;
        return Err(RawStorageError::Refused);
    }

    // The quarantine directory is intentionally retained as a named recovery
    // root. Its held FD is the only namespace used for this destructive
    // operation; the original pathname is never unlinked. A same-UID actor
    // that can enter this private directory is outside the guarantee: no
    // portable Unix API combines an identity check with unlink-by-FD. Any
    // observed identity/content mismatch above fails closed and preserves the
    // claimed entry for recovery.
    let result =
        unsafe { libc::unlinkat(quarantine.file.as_raw_fd(), quarantine_name.as_ptr(), 0) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Err(RawStorageError::Unavailable);
        }
        return Err(storage_io(error));
    }
    sync_directory(&quarantine.file)?;
    Ok(())
}

#[cfg(unix)]
fn cleanup_content_matches(
    actual: Option<&[u8]>,
    expected_bytes: Option<&[u8]>,
    expected_digest: Option<(&str, usize)>,
) -> bool {
    match (actual, expected_bytes, expected_digest) {
        (Some(actual), Some(expected), None) => actual == expected,
        (Some(actual), None, Some((digest, length))) => {
            actual.len() == length && sha256_digest(actual) == digest
        }
        (Some(_), None, None) | (None, None, None) => true,
        _ => false,
    }
}

#[cfg(unix)]
fn remove_private_named(
    directory: &File,
    name: &CStr,
    max_bytes: Option<usize>,
    hook: Option<&CleanupHook>,
) -> Result<(), RawStorageError> {
    remove_private_named_in_namespace(directory, name, max_bytes, None, hook)
}

#[cfg(unix)]
fn remove_private_named_in_namespace(
    directory: &File,
    name: &CStr,
    max_bytes: Option<usize>,
    existing_namespace: Option<&File>,
    hook: Option<&CleanupHook>,
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
    quarantine_and_remove(
        directory,
        name,
        CleanupSpec {
            creator: &file,
            expected: before,
            links: LinkCount::Exact(before.nlink),
            expected_bytes: None,
            expected_digest: None,
            max_bytes,
            existing_namespace,
            hook,
        },
    )
}

#[cfg(unix)]
fn remove_exact_file(
    directory: &File,
    name: &CStr,
    expected: &[u8],
    max_bytes: usize,
    hook: Option<&CleanupHook>,
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
    quarantine_and_remove(
        directory,
        name,
        CleanupSpec {
            creator: &file,
            expected: before,
            links: LinkCount::Exact(before.nlink),
            expected_bytes: Some(expected),
            expected_digest: None,
            max_bytes: Some(max_bytes),
            existing_namespace: None,
            hook,
        },
    )
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
        remove_private_named(directory, name, None, None)?;
    }
    Ok(())
}

#[cfg(unix)]
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
fn reconcile_quarantine_namespace(namespace: &File) -> Result<(), RawStorageError> {
    let identity = stat_fd(namespace).map_err(storage_io)?;
    if identity.mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32
        || identity.mode & 0o7777 != 0o700
    {
        return Err(RawStorageError::Refused);
    }
    for name in directory_names(namespace)? {
        if name.as_bytes().starts_with(QUARANTINE_DIRECTORY_PREFIX) {
            reconcile_quarantine_directory(namespace, &name)?;
        }
    }
    sync_directory(namespace)
}

#[cfg(unix)]
fn read_quarantine_manifest(
    directory: &File,
) -> Result<Option<QuarantineManifest>, RawStorageError> {
    let name = CString::new(QUARANTINE_MANIFEST_NAME).map_err(|_| RawStorageError::Refused)?;
    let Some(mut file) = (match open_named(directory, &name) {
        Ok(file) => file,
        Err(RawStorageError::Refused) => return Ok(None),
        Err(error) => return Err(error),
    }) else {
        return Ok(None);
    };
    let bytes = match read_verified_fd(&mut file, MAX_QUARANTINE_MANIFEST_BYTES) {
        Ok(bytes) => bytes,
        Err(RawStorageError::Refused | RawStorageError::Unbound) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(QuarantineManifest::decode(&bytes).ok())
}

#[cfg(unix)]
// Platform-dependent libc widths: casts required on macOS, redundant on Linux.
#[allow(clippy::unnecessary_cast)]
fn reconcile_quarantine_directory(namespace: &File, name: &CStr) -> Result<(), RawStorageError> {
    let directory = match open_directory_at(namespace, OsStr::from_bytes(name.to_bytes()), false) {
        Ok(directory) => directory,
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
        Err(error) => return Err(storage_io(error)),
    };
    let identity = stat_fd(&directory).map_err(storage_io)?;
    if identity.mode & libc::S_IFMT as u32 != libc::S_IFDIR as u32
        || identity.mode & 0o7777 != 0o700
    {
        return Err(RawStorageError::Refused);
    }
    let Some(manifest) = read_quarantine_manifest(&directory)? else {
        // A manifest is written and synced before any source entry is
        // claimed. Missing or malformed metadata therefore means the
        // quarantine cannot be proven safe after restart; preserve it.
        return Ok(());
    };
    let expected_digest = manifest
        .expected_digest
        .as_ref()
        .map(|(length, digest)| (digest.as_str(), *length));
    for entry in directory_names(&directory)? {
        if entry.as_bytes().starts_with(QUARANTINE_ENTRY_PREFIX) {
            let Some(mut file) = (match open_named(&directory, &entry) {
                Ok(file) => file,
                Err(RawStorageError::Refused) => None,
                Err(error) => return Err(error),
            }) else {
                continue;
            };
            let matches = match manifest.matches(&mut file) {
                Ok(matches) => matches,
                Err(RawStorageError::Refused | RawStorageError::Unbound) => false,
                Err(error) => return Err(error),
            };
            if !matches {
                sync_directory(&directory)?;
                continue;
            }
            quarantine_and_remove(
                &directory,
                &entry,
                CleanupSpec {
                    creator: &file,
                    expected: manifest.expected,
                    links: manifest.links,
                    expected_bytes: None,
                    expected_digest,
                    max_bytes: manifest.max_bytes,
                    existing_namespace: Some(namespace),
                    hook: None,
                },
            )?;
        }
    }
    sync_directory(&directory)
}

#[cfg(all(test, unix))]
struct TestPublishReplacement {
    directory: FileIdentity,
    bytes: Vec<u8>,
}

#[cfg(all(test, unix))]
static TEST_PUBLISH_REPLACEMENT: OnceLock<Mutex<Option<TestPublishReplacement>>> = OnceLock::new();

#[cfg(all(test, unix))]
static NEXT_TEST_REPLACEMENT: AtomicU64 = AtomicU64::new(0);

#[cfg(all(test, unix))]
pub struct TestPublishReplacementGuard {
    directory: FileIdentity,
}

#[cfg(all(test, unix))]
pub fn arm_test_publish_replacement(
    directory: &File,
    bytes: Vec<u8>,
) -> io::Result<TestPublishReplacementGuard> {
    let identity = stat_fd(directory)?;
    let slot = TEST_PUBLISH_REPLACEMENT.get_or_init(|| Mutex::new(None));
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "test publication replacement already armed",
        ));
    }
    *slot = Some(TestPublishReplacement {
        directory: identity,
        bytes,
    });
    Ok(TestPublishReplacementGuard {
        directory: identity,
    })
}

#[cfg(all(test, unix))]
impl Drop for TestPublishReplacementGuard {
    fn drop(&mut self) {
        let Some(slot) = TEST_PUBLISH_REPLACEMENT.get() else {
            return;
        };
        let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .as_ref()
            .is_some_and(|replacement| replacement.directory.same_directory(self.directory))
        {
            *slot = None;
        }
    }
}

#[cfg(all(test, unix))]
fn replace_test_temporary_if_armed(
    directory: &File,
    temporary: &CStr,
) -> Result<(), RawStorageError> {
    let directory_identity = stat_fd(directory).map_err(storage_io)?;
    let bytes = {
        let Some(slot) = TEST_PUBLISH_REPLACEMENT.get() else {
            return Ok(());
        };
        let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .as_ref()
            .is_some_and(|replacement| replacement.directory.same_directory(directory_identity))
        {
            slot.take().map(|replacement| replacement.bytes)
        } else {
            None
        }
    };
    let Some(bytes) = bytes else {
        return Ok(());
    };

    let sequence = NEXT_TEST_REPLACEMENT.fetch_add(1, Ordering::Relaxed);
    let replacement_name = CString::new(format!(
        ".velnor-raw-test-replacement-{}-{sequence}.tmp",
        std::process::id()
    ))
    .map_err(|_| RawStorageError::Refused)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            replacement_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o644,
        )
    };
    if fd < 0 {
        return Err(storage_io(io::Error::last_os_error()));
    }
    let mut replacement = unsafe { File::from_raw_fd(fd) };
    replacement.write_all(&bytes).map_err(storage_io)?;
    replacement.sync_all().map_err(storage_io)?;
    drop(replacement);

    let result = unsafe {
        libc::renameat(
            directory.as_raw_fd(),
            replacement_name.as_ptr(),
            directory.as_raw_fd(),
            temporary.as_ptr(),
        )
    };
    if result < 0 {
        return Err(storage_io(io::Error::last_os_error()));
    }
    sync_directory(directory)
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
    #[cfg(all(test, unix))]
    replace_test_temporary_if_armed(directory, &temporary.name)?;
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

        match install_no_clobber(directory, &temporary.name, directory, name) {
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
                let _ = temporary.remove_name(1, None);
                Err(RawStorageError::Refused)
            }
            Err(error) => Err(storage_io(error)),
        }
    })()
}

#[cfg(unix)]
fn install_no_clobber(
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

    fn remove_name(
        &mut self,
        expected_links: u64,
        hook: Option<&CleanupHook>,
    ) -> Result<(), RawStorageError> {
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
        quarantine_and_remove(
            self.directory,
            &self.name,
            CleanupSpec {
                creator: &self.file,
                expected: self.identity,
                links: LinkCount::Exact(expected_links),
                expected_bytes: None,
                expected_digest: None,
                max_bytes: None,
                existing_namespace: None,
                hook,
            },
        )?;
        self.name_removed = true;
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
        let _ = quarantine_and_remove(
            self.directory,
            &self.name,
            CleanupSpec {
                creator: &self.file,
                expected: self.identity,
                links: LinkCount::AtMost(2),
                expected_bytes: None,
                expected_digest: None,
                max_bytes: None,
                existing_namespace: None,
                hook: None,
            },
        );
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

#[cfg(all(test, unix))]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "cleanup race tests turn fixture setup failures into explicit test failures"
)]
mod cleanup_tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::current_dir()
            .expect("find cleanup test directory")
            .join(format!(
                ".github-raw-store-cleanup-{name}-{}-{sequence}",
                std::process::id()
            ));
        fs::create_dir(&parent).expect("create cleanup fixture parent");
        let path = parent.join("store");
        fs::create_dir(&path).expect("create cleanup fixture");
        path
    }

    fn fixture_parent(path: &Path) -> &Path {
        path.parent().expect("fixture has a private parent")
    }

    fn quarantine_namespace_path(path: &Path) -> PathBuf {
        fixture_parent(path).join(OsStr::from_bytes(QUARANTINE_NAMESPACE_NAME))
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .expect("create private cleanup file");
        file.write_all(bytes).expect("write cleanup file");
        file.sync_all().expect("sync cleanup file");
    }

    fn replace_named(directory: &File, name: &CStr, bytes: &[u8]) -> Result<(), RawStorageError> {
        let replacement_name = CString::new(format!(
            ".velnor-raw-test-attacker-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ))
        .map_err(|_| RawStorageError::Refused)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                replacement_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            return Err(storage_io(io::Error::last_os_error()));
        }
        let mut replacement = unsafe { File::from_raw_fd(fd) };
        replacement
            .write_all(bytes)
            .and_then(|_| replacement.sync_all())
            .map_err(storage_io)?;
        drop(replacement);
        let result = unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                replacement_name.as_ptr(),
                directory.as_raw_fd(),
                name.as_ptr(),
            )
        };
        if result < 0 {
            return Err(storage_io(io::Error::last_os_error()));
        }
        sync_directory(directory)
    }

    fn quarantined_bytes(path: &Path) -> Vec<u8> {
        let namespace = quarantine_namespace_path(path);
        let quarantine = fs::read_dir(namespace)
            .expect("read cleanup fixture")
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .as_bytes()
                    .starts_with(QUARANTINE_DIRECTORY_PREFIX)
            })
            .expect("find quarantine directory")
            .path();
        let entry = fs::read_dir(quarantine)
            .expect("read quarantine directory")
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .as_bytes()
                    .starts_with(QUARANTINE_ENTRY_PREFIX)
            })
            .expect("find quarantined entry")
            .path();
        fs::read(entry).expect("read quarantined entry")
    }

    #[test]
    fn replacement_before_quarantine_claim_stays_recoverable() {
        let root = fixture("before-claim");
        let target = root.join("target");
        let outside = root.join("outside");
        write_private(&target, b"creator");
        write_private(&outside, b"outside-sentinel");
        let directory = File::open(&root).expect("open cleanup directory");
        let name = CString::new("target").expect("target name");
        let hook =
            |point: CleanupPoint, directory: &File, name: &CStr| -> Result<(), RawStorageError> {
                if point == CleanupPoint::AfterIdentityCheck {
                    replace_named(directory, name, b"attacker-before-claim")?;
                }
                Ok(())
            };

        let result = remove_private_named(&directory, &name, Some(1024), Some(&hook));
        assert_eq!(result, Err(RawStorageError::Refused));
        assert_eq!(quarantined_bytes(&root), b"attacker-before-claim");
        assert_eq!(
            fs::read(outside).expect("read outside sentinel"),
            b"outside-sentinel"
        );
        let _ = fs::remove_dir_all(fixture_parent(&root));
    }

    #[test]
    fn replacement_after_final_quarantine_check_survives_original_path_cleanup() {
        let root = fixture("after-check");
        let target = root.join("target");
        let outside = root.join("outside");
        write_private(&target, b"creator");
        write_private(&outside, b"outside-sentinel");
        let directory = File::open(&root).expect("open cleanup directory");
        let name = CString::new("target").expect("target name");
        let hook =
            |point: CleanupPoint, directory: &File, name: &CStr| -> Result<(), RawStorageError> {
                if point == CleanupPoint::BeforeDestructiveAction {
                    replace_named(directory, name, b"attacker-after-check")?;
                }
                Ok(())
            };

        remove_private_named(&directory, &name, Some(1024), Some(&hook))
            .expect("remove verified creator");
        assert_eq!(
            fs::read(&target).expect("read replacement"),
            b"attacker-after-check"
        );
        assert_eq!(
            fs::read(outside).expect("read outside sentinel"),
            b"outside-sentinel"
        );
        assert!(fs::read_dir(quarantine_namespace_path(&root))
            .expect("read cleanup directory")
            .flatten()
            .all(|entry| {
                fs::read_dir(entry.path())
                    .expect("read retained quarantine directory")
                    .flatten()
                    .all(|entry| {
                        !entry
                            .file_name()
                            .as_bytes()
                            .starts_with(QUARANTINE_ENTRY_PREFIX)
                    })
            }));
        let _ = fs::remove_dir_all(fixture_parent(&root));
    }

    #[test]
    fn exact_and_temporary_cleanup_share_quarantine_claim() {
        let root = fixture("shared-claim");
        let exact = root.join("exact");
        write_private(&exact, b"exact");
        let directory = File::open(&root).expect("open cleanup directory");
        let exact_name = CString::new("exact").expect("exact name");
        remove_exact_file(&directory, &exact_name, b"exact", 1024, None)
            .expect("remove exact file");
        assert!(!exact.exists());

        let mut temporary = TemporaryFile::create(&directory).expect("create temporary file");
        temporary
            .file
            .write_all(b"temporary")
            .expect("write temporary file");
        temporary.file.sync_all().expect("sync temporary file");
        temporary
            .remove_name(1, None)
            .expect("remove temporary name");
        assert!(directory_names(&directory)
            .expect("read cleanup directory")
            .iter()
            .all(|name| !name.as_bytes().starts_with(QUARANTINE_DIRECTORY_PREFIX)));
        assert!(fs::read_dir(quarantine_namespace_path(&root))
            .expect("read retained quarantine namespace")
            .flatten()
            .all(|entry| {
                fs::read_dir(entry.path())
                    .expect("read retained quarantine directory")
                    .flatten()
                    .all(|entry| {
                        !entry
                            .file_name()
                            .as_bytes()
                            .starts_with(QUARANTINE_ENTRY_PREFIX)
                    })
            }));
        let _ = fs::remove_dir_all(fixture_parent(&root));
    }

    #[test]
    fn restart_reconciles_named_quarantine_directory() {
        let root = fixture("restart-recovery");
        let store = RawObjectFileStore::new(&root).expect("open recovery store");
        drop(store);

        let objects = root.join("sha256");
        let directory = File::open(&objects).expect("open object directory");
        let namespace = QuarantineNamespace::open_for(&directory).expect("open namespace");
        let quarantine = QuarantineDirectory::create(&namespace.file).expect("create quarantine");
        let source_name = CString::new(".velnor-raw-restart-source").expect("source name");
        let source_path = root.join(OsStr::from_bytes(source_name.as_bytes()));
        write_private(&source_path, b"crash-bytes");
        let source = open_named(
            &File::open(&root).expect("open source parent"),
            &source_name,
        )
        .expect("open restart source")
        .expect("restart source exists");
        let manifest = QuarantineManifest::from_cleanup(
            stat_fd(&source).expect("stat restart source"),
            LinkCount::Exact(1),
            Some(1024),
            Some(b"crash-bytes"),
            None,
        )
        .expect("create restart manifest");
        quarantine
            .write_manifest(&manifest)
            .expect("write restart manifest");
        let source_parent = File::open(&root).expect("open source parent");
        install_no_clobber(
            &source_parent,
            &source_name,
            &quarantine.file,
            &CString::new(".velnor-raw-entry-crash").expect("entry name"),
        )
        .expect("claim restart source");
        sync_directory(&quarantine.file).expect("sync restart quarantine");
        sync_directory(&source_parent).expect("sync restart source parent");
        drop(source);
        drop(quarantine);

        let reopened = RawObjectFileStore::new(&root).expect("reconcile recovery store");
        drop(reopened);
        assert!(
            fs::read_dir(root.join(OsStr::from_bytes(QUARANTINE_NAMESPACE_NAME)))
                .expect("read quarantine namespace after recovery")
                .flatten()
                .all(|entry| {
                    fs::read_dir(entry.path())
                        .expect("read recovered quarantine directory")
                        .flatten()
                        .all(|entry| {
                            !entry
                                .file_name()
                                .as_bytes()
                                .starts_with(QUARANTINE_ENTRY_PREFIX)
                        })
                })
        );
        let _ = fs::remove_dir_all(fixture_parent(&root));
    }

    #[test]
    fn restart_preserves_quarantine_identity_mismatch() {
        let root = fixture("restart-mismatch");
        let store = RawObjectFileStore::new(&root).expect("open recovery store");
        drop(store);

        let objects = root.join("sha256");
        let directory = File::open(&objects).expect("open object directory");
        let namespace = QuarantineNamespace::open_for(&directory).expect("open namespace");
        let quarantine = QuarantineDirectory::create(&namespace.file).expect("create quarantine");
        let quarantine_path = root
            .join(OsStr::from_bytes(QUARANTINE_NAMESPACE_NAME))
            .join(quarantine.name.to_str().expect("quarantine name is UTF-8"));
        let expected_path = root.join(".velnor-raw-expected");
        write_private(&expected_path, b"creator-bytes");
        let expected = File::open(&expected_path).expect("open expected creator");
        let manifest = QuarantineManifest::from_cleanup(
            stat_fd(&expected).expect("stat expected creator"),
            LinkCount::Exact(1),
            Some(1024),
            Some(b"creator-bytes"),
            None,
        )
        .expect("create mismatch manifest");
        quarantine
            .write_manifest(&manifest)
            .expect("write mismatch manifest");
        let entry_path = quarantine_path.join(OsStr::from_bytes(b".velnor-raw-entry-crash"));
        write_private(&entry_path, b"attacker-bytes");
        sync_directory(&quarantine.file).expect("sync mismatch quarantine");
        drop(expected);
        drop(quarantine);

        let reopened = RawObjectFileStore::new(&root).expect("reconcile mismatch store");
        drop(reopened);
        assert_eq!(
            fs::read(entry_path).expect("read preserved mismatch"),
            b"attacker-bytes"
        );
        let _ = fs::remove_dir_all(fixture_parent(&root));
    }
}
