//! Race-resistant reads from the local immutable GitHub evidence store.
//!
//! A storage reference is only an identifier.  The checker must reopen the
//! bytes from an explicitly supplied local store and hash the bytes held by
//! the opened file descriptor.  This module deliberately has no network or
//! path-canonicalization fallback: an unavailable, symlinked, or malformed
//! object is an error.

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

pub(crate) const MAX_OBJECT_BYTES: u64 = 128 * 1024 * 1024;
const SHA256_HEX_LENGTH: usize = 64;

/// Error returned when a local CAS object cannot be proven to be the
/// requested immutable byte sequence.
#[derive(Debug)]
pub(crate) enum RawStoreError {
    InvalidReference,
    InvalidDigest,
    OpenRoot(io::Error),
    OpenObject(io::Error),
    NotRegular,
    TooLarge,
    Read(io::Error),
    Length { expected: u64, actual: u64 },
    Digest { expected: String, actual: String },
}

impl fmt::Display for RawStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference => {
                formatter.write_str("storage reference is not digest-addressed")
            }
            Self::InvalidDigest => formatter.write_str("expected digest is not a SHA-256 digest"),
            Self::OpenRoot(error) => write!(formatter, "cannot open evidence root: {error}"),
            Self::OpenObject(error) => write!(formatter, "cannot open CAS object: {error}"),
            Self::NotRegular => formatter.write_str("CAS object is not a regular file"),
            Self::TooLarge => formatter.write_str("CAS object exceeds the bounded size"),
            Self::Read(error) => write!(formatter, "cannot read CAS object: {error}"),
            Self::Length { expected, actual } => {
                write!(
                    formatter,
                    "CAS object length {actual} does not equal expected {expected}"
                )
            }
            Self::Digest { expected, actual } => {
                write!(
                    formatter,
                    "CAS object digest {actual} does not equal expected {expected}"
                )
            }
        }
    }
}

impl std::error::Error for RawStoreError {}

/// An opened evidence root.  Every object read is relative to this directory
/// descriptor; no caller-controlled path is resolved after construction.
#[derive(Debug)]
pub(crate) struct RawEvidenceStore {
    #[cfg(unix)]
    root: File,
}

impl RawEvidenceStore {
    /// Open an explicit local evidence root without following its final
    /// symlink.  Non-Unix targets fail closed because this verifier requires
    /// descriptor-relative, no-follow reads.
    pub(crate) fn open(root: &Path) -> Result<Self, RawStoreError> {
        #[cfg(unix)]
        {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(root)
                .map_err(RawStoreError::OpenRoot)?;
            let metadata = file.metadata().map_err(RawStoreError::OpenRoot)?;
            if !metadata.is_dir() {
                return Err(RawStoreError::OpenRoot(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "evidence root is not a directory",
                )));
            }
            Ok(Self { root: file })
        }
        #[cfg(not(unix))]
        {
            let _ = root;
            Err(RawStoreError::OpenRoot(io::Error::new(
                io::ErrorKind::Unsupported,
                "descriptor-relative CAS verification is unsupported on this platform",
            )))
        }
    }

    /// Read and hash one content-addressed object from the opened store.
    /// `expected_len` is checked against the bytes read from the same file
    /// descriptor, so metadata and content cannot silently refer to different
    /// paths during a concurrent replacement.
    pub(crate) fn read_verified(
        &self,
        storage_ref: &str,
        expected_digest: &str,
        expected_len: u64,
    ) -> Result<Vec<u8>, RawStoreError> {
        let hex = digest_hex(expected_digest).ok_or(RawStoreError::InvalidDigest)?;
        if !valid_storage_ref(storage_ref, hex) {
            return Err(RawStoreError::InvalidReference);
        }
        if expected_len > MAX_OBJECT_BYTES {
            return Err(RawStoreError::TooLarge);
        }

        #[cfg(unix)]
        {
            let objects = open_child(
                &self.root,
                "sha256",
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
            .map_err(RawStoreError::OpenObject)?;
            let object = open_child(&objects, hex, libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .map_err(RawStoreError::OpenObject)?;
            let metadata = object.metadata().map_err(RawStoreError::OpenObject)?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(RawStoreError::NotRegular);
            }
            if metadata.len() > MAX_OBJECT_BYTES {
                return Err(RawStoreError::TooLarge);
            }
            let bytes = read_bounded(object)?;
            let actual_len = bytes.len() as u64;
            if actual_len != expected_len {
                return Err(RawStoreError::Length {
                    expected: expected_len,
                    actual: actual_len,
                });
            }
            let actual_digest = sha256_digest(&bytes);
            if actual_digest != expected_digest {
                return Err(RawStoreError::Digest {
                    expected: expected_digest.to_owned(),
                    actual: actual_digest,
                });
            }
            Ok(bytes)
        }
        #[cfg(not(unix))]
        {
            let _ = (storage_ref, expected_digest, expected_len);
            Err(RawStoreError::OpenObject(io::Error::new(
                io::ErrorKind::Unsupported,
                "descriptor-relative CAS verification is unsupported on this platform",
            )))
        }
    }
}

fn digest_hex(value: &str) -> Option<&str> {
    let hex = value.strip_prefix("sha256:")?;
    (hex.len() == SHA256_HEX_LENGTH && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(hex)
}

fn valid_storage_ref(value: &str, hex: &str) -> bool {
    ["artifact://sha256/", "cas://sha256/"]
        .iter()
        .any(|prefix| value.strip_prefix(prefix) == Some(hex))
}

fn sha256_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

#[cfg(unix)]
fn open_child(parent: &File, name: &str, flags: i32) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "CAS path component contains NUL",
        )
    })?;
    // SAFETY: `parent` is a live directory descriptor owned by this module;
    // `name` is a NUL-terminated immutable component; no create/write flags
    // are passed, so the call cannot dereference a caller-provided mode.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is freshly returned by openat and ownership transfers to
    // this File exactly once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn read_bounded(mut file: File) -> Result<Vec<u8>, RawStoreError> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk).map_err(RawStoreError::Read)?;
        if read == 0 {
            break;
        }
        if bytes.len() as u64 + read as u64 > MAX_OBJECT_BYTES {
            return Err(RawStoreError::TooLarge);
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(bytes)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("velnor-g0-raw-store-{label}-{suffix}"))
    }

    fn put(root: &Path, bytes: &[u8]) -> String {
        fs::create_dir_all(root.join("sha256")).expect("objects");
        let digest = sha256_digest(bytes);
        fs::write(
            root.join("sha256")
                .join(digest.strip_prefix("sha256:").expect("digest")),
            bytes,
        )
        .expect("object");
        digest
    }

    #[test]
    fn descriptor_read_reopens_and_hashes_bytes() {
        let root = temp_root("valid");
        let bytes = b"captured response";
        let digest = put(&root, bytes);
        let store = RawEvidenceStore::open(&root).expect("open store");
        let read = store
            .read_verified(
                &format!(
                    "artifact://sha256/{}",
                    digest.strip_prefix("sha256:").expect("digest")
                ),
                &digest,
                bytes.len() as u64,
            )
            .expect("verified bytes");
        assert_eq!(read, bytes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn wrong_digest_address_is_rejected_even_when_object_exists() {
        let root = temp_root("wrong-ref");
        let digest = put(&root, b"captured response");
        let store = RawEvidenceStore::open(&root).expect("open store");
        let error = store
            .read_verified("https://example.test/object", &digest, 17)
            .expect_err("network references are not local CAS refs");
        assert!(matches!(error, RawStoreError::InvalidReference));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_object_is_rejected_by_descriptor_open() {
        let root = temp_root("symlink");
        fs::create_dir_all(root.join("sha256")).expect("objects");
        let outside = root.with_extension("outside");
        fs::write(&outside, b"outside").expect("outside");
        let digest = sha256_digest(b"captured response");
        std::os::unix::fs::symlink(
            &outside,
            root.join("sha256")
                .join(digest.strip_prefix("sha256:").expect("digest")),
        )
        .expect("symlink");
        let store = RawEvidenceStore::open(&root).expect("open store");
        let error = store
            .read_verified(
                &format!(
                    "cas://sha256/{}",
                    digest.strip_prefix("sha256:").expect("digest")
                ),
                &digest,
                17,
            )
            .expect_err("symlink must not be followed");
        assert!(matches!(error, RawStoreError::OpenObject(_)));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_file(outside);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_object_is_rejected_as_nonexclusive_store_entry() {
        let root = temp_root("hardlink");
        fs::create_dir_all(root.join("sha256")).expect("objects");
        let outside = root.with_extension("outside");
        let bytes = b"captured response";
        fs::write(&outside, bytes).expect("outside");
        let digest = sha256_digest(bytes);
        std::fs::hard_link(
            &outside,
            root.join("sha256")
                .join(digest.strip_prefix("sha256:").expect("digest")),
        )
        .expect("hardlink");
        let store = RawEvidenceStore::open(&root).expect("open store");
        let error = store
            .read_verified(
                &format!(
                    "artifact://sha256/{}",
                    digest.strip_prefix("sha256:").expect("digest")
                ),
                &digest,
                bytes.len() as u64,
            )
            .expect_err("hardlinked object must not be accepted");
        assert!(matches!(error, RawStoreError::NotRegular));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_file(outside);
    }
}
