//! Small, durable content-addressed storage primitive.
//!
//! Objects are written to a two-level digest path with temp-file + fsync +
//! atomic rename. Reads always verify BLAKE3 before returning bytes. Tree
//! manifests make lazy subset materialization explicit without exposing a
//! mutable reference to stored content.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(unix))]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
    sync::Arc,
};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;
use velnor_action_model::{canonical_json_bytes, Digest, DigestError};

const MAX_TREE_ENTRIES: usize = 100_000;
const MAX_TREE_DIRECTORIES: usize = 100_000;
const MAX_TREE_NODES: usize = 200_000;
const MAX_TREE_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_TREE_FILE_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_TREE_TOTAL_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MAX_TREE_PATH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TREE_PATH_DEPTH: usize = 256;
const MAX_TREE_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FULL_READ_BYTES: u64 = 64 * 1024 * 1024;
const STREAM_BUFFER_BYTES: usize = 64 * 1024;

/// Normalized reason for a cache miss or rejected object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissReason {
    KeyAbsent,
    CorruptEntryRejected,
}

/// Materialization primitive selected for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub enum MaterializationMethod {
    ApfsClone,
    Reflink,
    VerifiedCopy,
}

/// Operation-scoped physical-byte evidence for one durable CAS object or
/// materialized file.
///
/// `shared_bytes` describes durable bytes reused by the operation, while
/// `newly_allocated_bytes` describes durable bytes allocated by it. The two
/// fields are intentionally separate: callers must not add them to a logical
/// length, include directory metadata, or infer either value when measurement
/// is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PhysicalByteAccounting {
    known: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    shared_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    newly_allocated_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PhysicalByteAccountingWire {
    known: bool,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_optional_non_null")]
    shared_bytes: Option<u64>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_optional_non_null")]
    newly_allocated_bytes: Option<u64>,
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| D::Error::custom("physical-byte fields must be omitted instead of null"))
}

impl PhysicalByteAccounting {
    /// Construct an unknown observation. Unknown observations omit both byte
    /// components when serialized.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            known: false,
            shared_bytes: None,
            newly_allocated_bytes: None,
        }
    }

    /// Construct known, disjoint shared and newly allocated components.
    #[must_use]
    pub const fn known(shared_bytes: u64, newly_allocated_bytes: u64) -> Self {
        Self {
            known: true,
            shared_bytes: Some(shared_bytes),
            newly_allocated_bytes: Some(newly_allocated_bytes),
        }
    }

    /// Whether both physical-byte components were observed truthfully.
    #[must_use]
    pub const fn is_known(self) -> bool {
        self.known
    }

    /// Bytes reused by this operation, when known.
    #[must_use]
    pub const fn shared_bytes(self) -> Option<u64> {
        self.shared_bytes
    }

    /// Bytes newly allocated by this operation, when known.
    #[must_use]
    pub const fn newly_allocated_bytes(self) -> Option<u64> {
        self.newly_allocated_bytes
    }

    fn from_wire(wire: PhysicalByteAccountingWire) -> Result<Self, &'static str> {
        match (wire.known, wire.shared_bytes, wire.newly_allocated_bytes) {
            (true, Some(shared_bytes), Some(newly_allocated_bytes)) => {
                Ok(Self::known(shared_bytes, newly_allocated_bytes))
            }
            (true, _, _) => Err("known physical-byte accounting requires both byte fields"),
            (false, None, None) => Ok(Self::unknown()),
            (false, _, _) => Err("unknown physical-byte accounting must omit both byte fields"),
        }
    }

    fn combine(self, other: Self) -> Self {
        match (
            self.shared_bytes,
            self.newly_allocated_bytes,
            other.shared_bytes,
            other.newly_allocated_bytes,
        ) {
            (Some(shared), Some(newly), Some(other_shared), Some(other_newly)) => {
                let Some(shared) = shared.checked_add(other_shared) else {
                    return Self::unknown();
                };
                let Some(newly) = newly.checked_add(other_newly) else {
                    return Self::unknown();
                };
                Self::known(shared, newly)
            }
            _ => Self::unknown(),
        }
    }
}

impl<'de> Deserialize<'de> for PhysicalByteAccounting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        PhysicalByteAccountingWire::deserialize(deserializer)
            .and_then(|wire| Self::from_wire(wire).map_err(D::Error::custom))
    }
}

/// Aggregate evidence from one lazy materialization operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializationReport {
    pub paths: Vec<PathBuf>,
    pub methods: std::collections::BTreeMap<MaterializationMethod, usize>,
    /// Physical-byte evidence for the successfully materialized file objects.
    pub accounting: PhysicalByteAccounting,
}

/// Hook for TASK-028's bounded-store policy.
pub trait BudgetCallback {
    /// Observe one newly requested object reservation in bytes.
    fn reserve(&self, bytes: u64) -> Result<(), String>;

    /// Release a reservation when publication fails.
    fn release(&self, bytes: u64);
}

/// CAS operation failure.
#[derive(Debug, Error)]
pub enum CasError {
    #[error("CAS I/O failure: {0}")]
    Io(#[from] io::Error),
    #[error("CAS JSON failure: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CAS digest failure: {0}")]
    Digest(#[from] DigestError),
    #[error("CAS canonicalization failure: {0}")]
    Canonical(#[from] velnor_action_model::CanonicalizationError),
    #[error("CAS object is absent: {digest}")]
    Absent { digest: Digest },
    #[error("CAS object {expected} failed integrity verification; got {actual}")]
    Corrupt { expected: Digest, actual: Digest },
    #[error("CAS budget rejected object: {0}")]
    BudgetRejected(String),
    #[error("invalid tree manifest: {0}")]
    InvalidManifest(String),
    #[error("unsafe CAS path: {0}")]
    UnsafePath(String),
}

impl CasError {
    /// Return the machine-readable miss dimension, when applicable.
    #[must_use]
    pub fn miss_reason(&self) -> Option<MissReason> {
        match self {
            Self::Absent { .. } => Some(MissReason::KeyAbsent),
            Self::Corrupt { .. } => Some(MissReason::CorruptEntryRejected),
            _ => None,
        }
    }
}

/// A tree entry stored as a separate CAS object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeEntry {
    pub path: String,
    pub digest: Digest,
    pub class: FileClass,
    #[serde(default = "default_file_mode")]
    pub mode: u32,
}

fn default_file_mode() -> u32 {
    0o644
}

/// File class used by lazy materialization selectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileClass {
    Executable,
    Runtime,
    Bundle,
    Metadata,
}

/// Canonical root manifest for a materializable tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeManifest {
    pub entries: Vec<TreeEntry>,
}

impl TreeManifest {
    fn validate(&self) -> Result<(), CasError> {
        if self.entries.len() > MAX_TREE_ENTRIES {
            return Err(CasError::InvalidManifest(format!(
                "tree contains {} entries, exceeding the {MAX_TREE_ENTRIES}-entry limit",
                self.entries.len()
            )));
        }

        let mut paths = std::collections::BTreeSet::new();
        let mut path_bytes = 0_u64;
        for entry in &self.entries {
            let identity = canonical_manifest_path(&entry.path)?;
            path_bytes = path_bytes
                .checked_add(entry.path.len() as u64)
                .ok_or_else(|| {
                    CasError::InvalidManifest("tree path byte count overflowed".into())
                })?;
            if path_bytes > MAX_TREE_PATH_BYTES {
                return Err(CasError::InvalidManifest(format!(
                    "tree paths exceed the {MAX_TREE_PATH_BYTES}-byte limit"
                )));
            }
            if entry.mode & !0o777 != 0 || !paths.insert(identity) {
                return Err(CasError::InvalidManifest(format!(
                    "entry path must be unique, portable, relative, and regular: '{}'",
                    entry.path
                )));
            }
        }

        let mut previous: Option<&str> = None;
        for path in &paths {
            if let Some(parent) = previous
                && path.starts_with(parent)
                && path.as_bytes().get(parent.len()) == Some(&b'/')
            {
                return Err(CasError::InvalidManifest(format!(
                    "tree cannot contain a file and a descendant: '{parent}' and '{path}'"
                )));
            }
            previous = Some(path.as_str());
        }
        Ok(())
    }
}

fn canonical_manifest_path(path: &str) -> Result<String, CasError> {
    let path_ref = Path::new(path);
    if path.is_empty()
        || path.len() as u64 > MAX_TREE_PATH_BYTES
        || path.contains('\\')
        || path.contains('\0')
        || path.chars().any(char::is_control)
        || path_ref.is_absolute()
    {
        return Err(CasError::InvalidManifest(format!(
            "entry path must be portable and relative: '{path}'"
        )));
    }
    if path_ref.components().count() > MAX_TREE_PATH_DEPTH {
        return Err(CasError::InvalidManifest(format!(
            "entry path exceeds the {MAX_TREE_PATH_DEPTH}-component depth limit: '{path}'"
        )));
    }
    let components = path_ref
        .components()
        .map(|component| match component {
            Component::Normal(name) => name.to_string_lossy().into_owned(),
            _ => String::new(),
        })
        .collect::<Vec<_>>();
    if components.iter().any(String::is_empty) {
        return Err(CasError::InvalidManifest(format!(
            "entry path contains a non-normal component: '{path}'"
        )));
    }
    let canonical = components.join("/");
    if canonical != path {
        return Err(CasError::InvalidManifest(format!(
            "entry path is not canonical: '{path}'"
        )));
    }
    Ok(canonical.nfc().collect::<String>().to_lowercase())
}

/// Lazy materialization selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubsetSelector {
    ExecutablesOnly,
    RuntimeFiles,
    BundleOnly,
    MetadataOnly,
}

impl SubsetSelector {
    fn matches(self, class: FileClass) -> bool {
        match self {
            Self::ExecutablesOnly => class == FileClass::Executable,
            Self::RuntimeFiles => matches!(class, FileClass::Executable | FileClass::Runtime),
            Self::BundleOnly => class == FileClass::Bundle,
            Self::MetadataOnly => class == FileClass::Metadata,
        }
    }
}

/// Digest-verified local CAS.
#[derive(Debug, Clone)]
pub struct CasStore {
    root: PathBuf,
    #[cfg(unix)]
    root_dir: Arc<File>,
}

impl CasStore {
    /// Open or create a CAS rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, CasError> {
        let root = root.into();
        #[cfg(unix)]
        {
            let root = canonical_creation_path(&root)?;
            let root_dir = Arc::new(open_secure_directory(&root)?);
            Ok(Self { root, root_dir })
        }
        #[cfg(not(unix))]
        {
            if let Ok(metadata) = fs::symlink_metadata(&root) {
                if metadata.file_type().is_symlink() {
                    return Err(CasError::UnsafePath(format!(
                        "CAS root is a symlink: {}",
                        root.display()
                    )));
                }
            }
            fs::create_dir_all(&root)?;
            if fs::symlink_metadata(&root)?.file_type().is_symlink() {
                return Err(CasError::UnsafePath(format!(
                    "CAS root became a symlink: {}",
                    root.display()
                )));
            }
            Ok(Self { root })
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store bytes and return their BLAKE3 digest.
    pub fn put(&self, bytes: &[u8]) -> Result<Digest, CasError> {
        self.put_inner(bytes, None).map(|(digest, _)| digest)
    }

    /// Store bytes and return their digest with post-publication physical-byte
    /// evidence for this CAS operation.
    pub fn put_with_accounting(
        &self,
        bytes: &[u8],
    ) -> Result<(Digest, PhysicalByteAccounting), CasError> {
        self.put_inner(bytes, None)
    }

    /// Store bytes while notifying the bounded-store policy hook.
    pub fn put_with_budget(
        &self,
        bytes: &[u8],
        budget: &dyn BudgetCallback,
    ) -> Result<Digest, CasError> {
        self.put_inner(bytes, Some(budget))
            .map(|(digest, _)| digest)
    }

    fn put_inner(
        &self,
        bytes: &[u8],
        budget: Option<&dyn BudgetCallback>,
    ) -> Result<(Digest, PhysicalByteAccounting), CasError> {
        #[cfg(unix)]
        {
            self.put_inner_secure(bytes, budget)
        }
        #[cfg(not(unix))]
        {
            self.put_inner_path(bytes, budget)
        }
    }

    #[cfg(unix)]
    fn put_inner_secure(
        &self,
        bytes: &[u8],
        budget: Option<&dyn BudgetCallback>,
    ) -> Result<(Digest, PhysicalByteAccounting), CasError> {
        let digest = Digest::from_bytes(bytes);
        let bucket = self.open_bucket(&digest, true)?;
        let object_name = &digest.as_str()[2..];
        if let Some(file) = open_existing_object(&bucket, object_name, &digest)? {
            self.verify_object_bounded(&digest, bytes.len() as u64)?;
            return Ok((digest, file_accounting(&file, true)));
        }

        let mut reservation = if let Some(budget) = budget {
            budget
                .reserve(bytes.len() as u64)
                .map_err(CasError::BudgetRejected)?;
            Some(BudgetReservation {
                callback: budget,
                bytes: bytes.len() as u64,
                committed: false,
            })
        } else {
            None
        };

        let temp_name = format!(
            ".{}.{}.tmp",
            digest,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let mut file = open_created_file(&bucket, OsStr::new(&temp_name))?;
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            if let Err(cleanup_error) = remove_temp_secure(&bucket, &temp_name) {
                if let Some(reservation) = &mut reservation {
                    reservation.committed = true;
                }
                return Err(cleanup_error.into());
            }
            return Err(error.into());
        }
        drop(file);
        match rustix::fs::renameat_with(
            &bucket,
            &temp_name,
            &bucket,
            object_name,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::EXIST => {
                if let Err(cleanup_error) = remove_temp_secure(&bucket, &temp_name) {
                    if let Some(reservation) = &mut reservation {
                        reservation.committed = true;
                    }
                    return Err(cleanup_error.into());
                }
                self.verify_object_bounded(&digest, bytes.len() as u64)?;
                let accounting = self.object_accounting(&digest, true);
                return Ok((digest, accounting));
            }
            Err(error) => {
                if let Err(cleanup_error) = remove_temp_secure(&bucket, &temp_name) {
                    if let Some(reservation) = &mut reservation {
                        reservation.committed = true;
                    }
                    return Err(cleanup_error.into());
                }
                return Err(io::Error::from(error).into());
            }
        }
        if let Some(reservation) = &mut reservation {
            // The object is now visible under its immutable digest name. Do
            // not release the reservation if the directory sync below fails:
            // the object remains allocated and may survive this process.
            reservation.committed = true;
        }
        bucket.sync_all()?;
        Ok((digest.clone(), self.object_accounting(&digest, false)))
    }

    #[cfg(not(unix))]
    fn put_inner_path(
        &self,
        bytes: &[u8],
        budget: Option<&dyn BudgetCallback>,
    ) -> Result<(Digest, PhysicalByteAccounting), CasError> {
        let digest = Digest::from_bytes(bytes);
        let path = self.checked_object_path(&digest)?;
        if path.exists() {
            self.verify_object_bounded(&digest, bytes.len() as u64)?;
            return Ok((digest, self.object_accounting(&digest, true)));
        }

        let mut reservation = if let Some(budget) = budget {
            budget
                .reserve(bytes.len() as u64)
                .map_err(CasError::BudgetRejected)?;
            Some(BudgetReservation {
                callback: budget,
                bytes: bytes.len() as u64,
                committed: false,
            })
        } else {
            None
        };

        let parent = path
            .parent()
            .ok_or_else(|| CasError::InvalidManifest("CAS object has no parent".into()))?;
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(
            ".{}.{}.tmp",
            digest,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        drop(file);
        match fs::rename(&temp, &path) {
            Ok(()) => {}
            Err(_error) if path.exists() => {
                let _ = fs::remove_file(&temp);
                self.verify_object_bounded(&digest, bytes.len() as u64)?;
                let accounting = self.object_accounting(&digest, true);
                return Ok((digest, accounting));
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(error.into());
            }
        }
        if let Some(reservation) = &mut reservation {
            reservation.committed = true;
        }
        sync_directory(parent)?;
        Ok((digest.clone(), self.object_accounting(&digest, false)))
    }

    fn put_file_with_limit(
        &self,
        source: &mut File,
        max_bytes: u64,
    ) -> Result<(Digest, u64), CasError> {
        let metadata = source.metadata()?;
        if !metadata.file_type().is_file() {
            return Err(CasError::UnsafePath(
                "tree source entry is not a regular file".into(),
            ));
        }
        let expected_bytes = metadata.len();
        if expected_bytes > max_bytes {
            return Err(CasError::InvalidManifest(format!(
                "tree file is {expected_bytes} bytes, exceeding the {max_bytes}-byte limit"
            )));
        }

        source.seek(SeekFrom::Start(0))?;
        let (digest, read_bytes) = digest_reader(source, max_bytes)?;
        if read_bytes != expected_bytes {
            return Err(CasError::InvalidManifest(
                "tree source changed while it was being read".into(),
            ));
        }
        source.seek(SeekFrom::Start(0))?;
        self.put_reader_with_digest(source, &digest, read_bytes, max_bytes)?;
        Ok((digest, read_bytes))
    }

    #[cfg(unix)]
    fn put_reader_with_digest<R: Read>(
        &self,
        reader: &mut R,
        expected: &Digest,
        expected_bytes: u64,
        max_bytes: u64,
    ) -> Result<(), CasError> {
        let bucket = self.open_bucket(expected, true)?;
        let object_name = &expected.as_str()[2..];
        if open_existing_object(&bucket, object_name, expected)?.is_some() {
            self.verify_object_bounded(expected, max_bytes)?;
            return Ok(());
        }

        let temp_name = format!(
            ".{}.{}.tmp",
            expected,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let mut file = open_created_file(&bucket, OsStr::new(&temp_name))?;
        let (actual, written_bytes) = match copy_reader_with_digest(
            reader,
            &mut file,
            max_bytes,
        ) {
            Ok(result) => result,
            Err(error) => {
                drop(file);
                let _ = remove_temp_secure(&bucket, &temp_name);
                return Err(error);
            }
        };
        if written_bytes != expected_bytes || actual != *expected {
            drop(file);
            let _ = remove_temp_secure(&bucket, &temp_name);
            return Err(CasError::Corrupt {
                expected: expected.clone(),
                actual,
            });
        }
        if let Err(error) = file.sync_all() {
            drop(file);
            let _ = remove_temp_secure(&bucket, &temp_name);
            return Err(error.into());
        }
        drop(file);

        match rustix::fs::renameat_with(
            &bucket,
            &temp_name,
            &bucket,
            object_name,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => bucket.sync_all()?,
            Err(error) if error == rustix::io::Errno::EXIST => {
                remove_temp_secure(&bucket, &temp_name)?;
                self.verify_object_bounded(expected, max_bytes)?;
            }
            Err(error) => {
                let cleanup = remove_temp_secure(&bucket, &temp_name);
                if let Err(cleanup_error) = cleanup {
                    return Err(cleanup_error.into());
                }
                return Err(io::Error::from(error).into());
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn put_reader_with_digest<R: Read>(
        &self,
        reader: &mut R,
        expected: &Digest,
        expected_bytes: u64,
        max_bytes: u64,
    ) -> Result<(), CasError> {
        let path = self.checked_object_path(expected)?;
        if path.exists() {
            self.verify_object_bounded(expected, max_bytes)?;
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| CasError::InvalidManifest("CAS object has no parent".into()))?;
        fs::create_dir_all(parent)?;
        let temp = parent.join(format!(
            ".{}.{}.tmp",
            expected,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        let (actual, written_bytes) = match copy_reader_with_digest(reader, &mut file, max_bytes) {
            Ok(result) => result,
            Err(error) => {
                drop(file);
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
        };
        if written_bytes != expected_bytes || actual != *expected {
            drop(file);
            let _ = fs::remove_file(&temp);
            return Err(CasError::Corrupt {
                expected: expected.clone(),
                actual,
            });
        }
        file.sync_all()?;
        drop(file);
        match fs::rename(&temp, &path) {
            Ok(()) => sync_directory(parent)?,
            Err(error) if path.exists() => {
                fs::remove_file(&temp)?;
                self.verify_object_bounded(expected, max_bytes)?;
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(error.into());
            }
        }
        Ok(())
    }

    fn verify_object_bounded(&self, digest: &Digest, max_bytes: u64) -> Result<u64, CasError> {
        let mut file = self.open_object(digest)?;
        let size = file.metadata()?.len();
        if size > max_bytes {
            return Err(CasError::InvalidManifest(format!(
                "CAS object {digest} is {size} bytes, exceeding the {max_bytes}-byte limit"
            )));
        }
        let (actual, read_bytes) = digest_reader(&mut file, max_bytes)?;
        if actual != *digest {
            return Err(CasError::Corrupt {
                expected: digest.clone(),
                actual,
            });
        }
        Ok(read_bytes)
    }

    /// Read and verify an object in full when it fits the bounded read API.
    ///
    /// Use [`Self::stream`] for larger objects. This keeps accidental full
    /// reads from allocating multi-gigabyte buffers.
    pub fn get(&self, digest: &Digest) -> Result<Vec<u8>, CasError> {
        let mut file = self.open_object(digest)?;
        read_verified_bytes(&mut file, digest, MAX_FULL_READ_BYTES)
    }

    /// Open a reader that verifies the object when EOF is reached.
    pub fn stream(&self, digest: &Digest) -> Result<VerifiedReader, CasError> {
        let file = self.open_object(digest)?;
        Ok(VerifiedReader {
            file,
            expected: digest.clone(),
            hasher: blake3::Hasher::new(),
            verified: false,
        })
    }

    /// Store a tree manifest and return its root digest.
    pub fn put_tree(&self, manifest: &TreeManifest) -> Result<Digest, CasError> {
        self.put_tree_with_accounting(manifest)
            .map(|(digest, _)| digest)
    }

    /// Store a tree manifest and return its digest with physical-byte evidence.
    pub fn put_tree_with_accounting(
        &self,
        manifest: &TreeManifest,
    ) -> Result<(Digest, PhysicalByteAccounting), CasError> {
        manifest.validate()?;
        let mut canonical_manifest = manifest.clone();
        canonical_manifest
            .entries
            .sort_by(|left, right| left.path.cmp(&right.path));
        let bytes = canonical_json_bytes(&canonical_manifest)?;
        if bytes.len() as u64 > MAX_TREE_MANIFEST_BYTES {
            return Err(CasError::InvalidManifest(format!(
                "tree manifest is {} bytes, exceeding the {MAX_TREE_MANIFEST_BYTES}-byte limit",
                bytes.len()
            )));
        }
        self.put_with_accounting(&bytes)
    }

    /// Store a regular directory as an immutable, digest-addressed tree.
    ///
    /// Every source component is opened without following symlinks. The
    /// resulting manifest contains only regular files; directories are
    /// represented by their file paths and the manifest itself is the tree
    /// root. This is the only filesystem-to-tree boundary used by compiler
    /// output publication.
    pub fn put_directory_tree(&self, root: &Path) -> Result<Digest, CasError> {
        let metadata = fs::symlink_metadata(root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CasError::UnsafePath(format!(
                "tree source is not a regular directory: {}",
                root.display()
            )));
        }

        let mut entries = Vec::new();
        let mut budget = TreeTraversalBudget::new(TreeTraversalLimits::production());
        #[cfg(unix)]
        {
            let root_dir = open_existing_secure_directory(root)?;
            collect_directory_tree(
                &root_dir,
                Path::new(""),
                self,
                &mut entries,
                &mut budget,
            )?;
        }
        #[cfg(not(unix))]
        collect_directory_tree_path(root, root, self, &mut entries, &mut budget)?;

        self.put_tree(&TreeManifest { entries })
    }

    /// Validate a tree root and every referenced file object.
    pub fn validate_tree(&self, root_digest: &Digest) -> Result<TreeManifest, CasError> {
        let manifest = self.read_tree_manifest(root_digest)?;
        for entry in &manifest.entries {
            self.verify_object_bounded(&entry.digest, MAX_TREE_FILE_BYTES)?;
        }
        Ok(manifest)
    }

    /// Materialize only the selected file classes from a tree manifest.
    pub fn materialize_subset(
        &self,
        root_digest: &Digest,
        selector: SubsetSelector,
        destination: &Path,
    ) -> Result<Vec<PathBuf>, CasError> {
        Ok(self
            .materialize_subset_report(root_digest, selector, destination)?
            .paths)
    }

    /// Materialize a subset and report the probed primitive used for each file.
    pub fn materialize_subset_report(
        &self,
        root_digest: &Digest,
        selector: SubsetSelector,
        destination: &Path,
    ) -> Result<MaterializationReport, CasError> {
        let manifest = self.read_tree_manifest(root_digest)?;
        let mut materialized = Vec::new();
        let mut methods = std::collections::BTreeMap::new();
        #[cfg(unix)]
        let mut shared_digests = std::collections::BTreeSet::new();
        #[cfg(unix)]
        let destination_root = open_secure_directory(destination).map_err(|error| {
            if matches!(
                error.raw_os_error(),
                Some(code) if code == libc::ELOOP || code == libc::ENOTDIR
            ) {
                CasError::UnsafePath(format!(
                    "materialization destination contains a symlink: {}",
                    destination.display()
                ))
            } else {
                CasError::Io(error)
            }
        })?;
        let mut accounting = if cfg!(unix) {
            PhysicalByteAccounting::known(0, 0)
        } else {
            PhysicalByteAccounting::unknown()
        };
        for entry in manifest
            .entries
            .iter()
            .filter(|entry| selector.matches(entry.class))
        {
            #[cfg(unix)]
            let target = secure_destination_target(&destination_root, destination, &entry.path)?;
            #[cfg(unix)]
            let source = self.open_object(&entry.digest)?;
            #[cfg(unix)]
            let (method, materialized_file) = materialize_file(
                &source,
                &target.parent,
                &target.name,
                &entry.digest,
                entry.mode,
            )?;
            #[cfg(unix)]
            let path = target.path;
            #[cfg(not(unix))]
            let bytes = self.read_object_bounded(&entry.digest, MAX_TREE_FILE_BYTES)?;
            #[cfg(not(unix))]
            let path = safe_destination_path(destination, &entry.path)?;
            #[cfg(not(unix))]
            let source = self.checked_object_path(&entry.digest)?;
            #[cfg(not(unix))]
            let method = materialize_file(&source, &path, &bytes, &entry.digest, entry.mode)?;
            #[cfg(unix)]
            let file_accounting = if matches!(
                method,
                MaterializationMethod::ApfsClone | MaterializationMethod::Reflink
            ) {
                if shared_digests.insert(entry.digest.clone()) {
                    file_accounting(&materialized_file, true)
                } else {
                    PhysicalByteAccounting::known(0, 0)
                }
            } else {
                file_accounting(&materialized_file, false)
            };
            #[cfg(not(unix))]
            let file_accounting = physical_accounting_for_materialization(&path, method);
            accounting = accounting.combine(file_accounting);
            *methods.entry(method).or_insert(0) += 1;
            #[cfg(unix)]
            materialized.push(path);
            #[cfg(not(unix))]
            materialized.push(path);
        }
        Ok(MaterializationReport {
            paths: materialized,
            methods,
            accounting,
        })
    }

    fn read_tree_manifest(&self, root_digest: &Digest) -> Result<TreeManifest, CasError> {
        let bytes = self.read_object_bounded(root_digest, MAX_TREE_MANIFEST_BYTES)?;
        let manifest: TreeManifest = serde_json::from_slice(&bytes).map_err(|error| {
            CasError::InvalidManifest(format!("tree manifest JSON is invalid: {error}"))
        })?;
        manifest.validate()?;

        let mut total_bytes = 0_u64;
        for entry in &manifest.entries {
            let object = self.open_object(&entry.digest)?;
            let size = object.metadata()?.len();
            if size > MAX_TREE_FILE_BYTES {
                return Err(CasError::InvalidManifest(format!(
                    "tree file {} is {size} bytes, exceeding the {MAX_TREE_FILE_BYTES}-byte limit",
                    entry.path
                )));
            }
            total_bytes = total_bytes.checked_add(size).ok_or_else(|| {
                CasError::InvalidManifest("tree file byte count overflowed".into())
            })?;
            if total_bytes > MAX_TREE_TOTAL_BYTES {
                return Err(CasError::InvalidManifest(format!(
                    "tree files exceed the {MAX_TREE_TOTAL_BYTES}-byte limit"
                )));
            }
        }
        Ok(manifest)
    }

    fn read_object_bounded(&self, digest: &Digest, max_bytes: u64) -> Result<Vec<u8>, CasError> {
        let file = self.open_object(digest)?;
        let size = file.metadata()?.len();
        if size > max_bytes {
            return Err(CasError::InvalidManifest(format!(
                "CAS object {digest} is {size} bytes, exceeding the {max_bytes}-byte limit"
            )));
        }

        read_verified_bytes(file, digest, max_bytes)
    }

    fn open_object(&self, digest: &Digest) -> Result<File, CasError> {
        #[cfg(unix)]
        {
            let bucket = self.open_bucket(digest, false)?;
            let object_name = &digest.as_str()[2..];
            let file = open_existing_object(&bucket, object_name, digest)?.ok_or_else(|| {
                CasError::Absent {
                    digest: digest.clone(),
                }
            })?;
            Ok(file)
        }
        #[cfg(not(unix))]
        {
            let path = self.checked_object_path(digest)?;
            open_readonly_nofollow(&path).map_err(|error| {
                if error.kind() == io::ErrorKind::NotFound {
                    CasError::Absent {
                        digest: digest.clone(),
                    }
                } else {
                    CasError::Io(error)
                }
            })
        }
    }

    #[cfg(unix)]
    fn open_bucket(&self, digest: &Digest, create: bool) -> Result<File, CasError> {
        let bucket_name = &digest.as_str()[..2];
        let result = if create {
            open_or_create_directory(&self.root_dir, OsStr::new(bucket_name))
        } else {
            open_directory_child(&self.root_dir, OsStr::new(bucket_name))
        };
        result.map_err(|error| {
            if !create && error.raw_os_error() == Some(libc::ENOENT) {
                CasError::Absent {
                    digest: digest.clone(),
                }
            } else if error.raw_os_error() == Some(libc::ELOOP) {
                CasError::UnsafePath(format!("CAS bucket is a symlink: {bucket_name}"))
            } else {
                CasError::Io(error)
            }
        })
    }

    #[cfg(any(test, not(unix)))]
    fn object_path(&self, digest: &Digest) -> PathBuf {
        self.root
            .join(&digest.as_str()[..2])
            .join(&digest.as_str()[2..])
    }

    #[cfg(not(unix))]
    fn checked_object_path(&self, digest: &Digest) -> Result<PathBuf, CasError> {
        let path = self.object_path(digest);
        for component in [
            self.root.as_path(),
            path.parent().expect("digest bucket has parent"),
            path.as_path(),
        ] {
            if let Ok(metadata) = fs::symlink_metadata(component) {
                if metadata.file_type().is_symlink() {
                    return Err(CasError::UnsafePath(format!(
                        "CAS component is a symlink: {}",
                        component.display()
                    )));
                }
            }
        }
        Ok(path)
    }

    fn object_accounting(&self, digest: &Digest, shared: bool) -> PhysicalByteAccounting {
        #[cfg(unix)]
        {
            let Ok(bucket) = self.open_bucket(digest, false) else {
                return PhysicalByteAccounting::unknown();
            };
            let Ok(Some(file)) = open_existing_object(&bucket, &digest.as_str()[2..], digest)
            else {
                return PhysicalByteAccounting::unknown();
            };
            file_accounting(&file, shared)
        }
        #[cfg(not(unix))]
        {
            let Ok(path) = self.checked_object_path(digest) else {
                return PhysicalByteAccounting::unknown();
            };
            let Ok(metadata) = fs::symlink_metadata(path) else {
                return PhysicalByteAccounting::unknown();
            };
            metadata_accounting(&metadata, shared)
        }
    }
}

fn digest_reader<R: Read>(
    reader: &mut R,
    max_bytes: u64,
) -> Result<(Digest, u64), CasError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok((Digest::from_hash(hasher.finalize()), total));
        }
        total = total.checked_add(count as u64).ok_or_else(|| {
            CasError::InvalidManifest("stream byte count overflowed".into())
        })?;
        if total > max_bytes {
            return Err(CasError::InvalidManifest(format!(
                "stream exceeds the {max_bytes}-byte limit"
            )));
        }
        hasher.update(&buffer[..count]);
    }
}

fn copy_reader_with_digest<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
) -> Result<(Digest, u64), CasError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok((Digest::from_hash(hasher.finalize()), total));
        }
        total = total.checked_add(count as u64).ok_or_else(|| {
            CasError::InvalidManifest("stream byte count overflowed".into())
        })?;
        if total > max_bytes {
            return Err(CasError::InvalidManifest(format!(
                "stream exceeds the {max_bytes}-byte limit"
            )));
        }
        writer.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
    }
}

fn read_verified_bytes<R: Read>(
    mut reader: R,
    expected: &Digest,
    max_bytes: u64,
) -> Result<Vec<u8>, CasError> {
    let mut bytes = Vec::new();
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            let actual = Digest::from_hash(hasher.finalize());
            if actual != *expected {
                return Err(CasError::Corrupt {
                    expected: expected.clone(),
                    actual,
                });
            }
            return Ok(bytes);
        }
        total = total.checked_add(count as u64).ok_or_else(|| {
            CasError::InvalidManifest("object byte count overflowed".into())
        })?;
        if total > max_bytes {
            return Err(CasError::InvalidManifest(format!(
                "CAS object {expected} exceeds the {max_bytes}-byte limit"
            )));
        }
        bytes.extend_from_slice(&buffer[..count]);
        hasher.update(&buffer[..count]);
    }
}

struct BudgetReservation<'a> {
    callback: &'a dyn BudgetCallback,
    bytes: u64,
    committed: bool,
}

impl Drop for BudgetReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.callback.release(self.bytes);
        }
    }
}

fn file_accounting(file: &File, shared: bool) -> PhysicalByteAccounting {
    file.metadata()
        .ok()
        .and_then(|metadata| physical_allocation_bytes(&metadata))
        .map_or_else(PhysicalByteAccounting::unknown, |bytes| {
            if shared {
                PhysicalByteAccounting::known(bytes, 0)
            } else {
                PhysicalByteAccounting::known(0, bytes)
            }
        })
}

#[cfg(not(unix))]
fn metadata_accounting(metadata: &fs::Metadata, shared: bool) -> PhysicalByteAccounting {
    let Some(bytes) = physical_allocation_bytes(metadata) else {
        return PhysicalByteAccounting::unknown();
    };
    if shared {
        PhysicalByteAccounting::known(bytes, 0)
    } else {
        PhysicalByteAccounting::known(0, bytes)
    }
}

#[cfg(not(unix))]
fn physical_accounting_for_materialization(
    path: &Path,
    method: MaterializationMethod,
) -> PhysicalByteAccounting {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return PhysicalByteAccounting::unknown();
    };
    let Some(bytes) = physical_allocation_bytes(&metadata) else {
        return PhysicalByteAccounting::unknown();
    };
    match method {
        MaterializationMethod::ApfsClone | MaterializationMethod::Reflink => {
            PhysicalByteAccounting::known(bytes, 0)
        }
        MaterializationMethod::VerifiedCopy => PhysicalByteAccounting::known(0, bytes),
    }
}

fn physical_allocation_bytes(metadata: &fs::Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        metadata.blocks().checked_mul(512)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

#[cfg(unix)]
struct MaterializationTarget {
    path: PathBuf,
    parent: File,
    name: OsString,
}

#[cfg(unix)]
fn directory_flags() -> rustix::fs::OFlags {
    rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW
}

#[cfg(unix)]
fn open_directory_child(parent: &File, name: &OsStr) -> io::Result<File> {
    let descriptor = rustix::fs::openat(parent, name, directory_flags(), rustix::fs::Mode::empty())
        .map_err(io::Error::from)?;
    Ok(descriptor.into())
}

#[cfg(unix)]
fn open_or_create_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    match open_directory_child(parent, name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
            match rustix::fs::mkdirat(parent, name, rustix::fs::Mode::from_raw_mode(0o755)) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::EXIST => {}
                Err(error) => return Err(io::Error::from(error)),
            }
            open_directory_child(parent, name)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn remove_temp_secure(parent: &File, name: &str) -> io::Result<()> {
    match rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(unix)]
fn canonical_creation_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let absolute = secure_path(&absolute);
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir => current.push(Path::new("/")),
            Component::CurDir => {}
            Component::Normal(name) => {
                current.push(name);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!(
                                "secure path cannot contain a symlink: {}",
                                current.display()
                            ),
                        ));
                    }
                    Ok(metadata) if !metadata.is_dir() && current != absolute => {
                        return Err(io::Error::new(
                            io::ErrorKind::NotADirectory,
                            format!(
                                "secure path contains a non-directory: {}",
                                current.display()
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                    Err(error) => return Err(error),
                }
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "secure path contains a non-normal component",
                ));
            }
        }
    }
    Ok(absolute)
}

#[cfg(unix)]
fn open_secure_directory(path: &Path) -> io::Result<File> {
    let path = secure_path(path);
    let mut current = if path.is_absolute() {
        let descriptor =
            rustix::fs::open(Path::new("/"), directory_flags(), rustix::fs::Mode::empty())
                .map_err(io::Error::from)?;
        File::from(descriptor)
    } else {
        let descriptor =
            rustix::fs::open(Path::new("."), directory_flags(), rustix::fs::Mode::empty())
                .map_err(io::Error::from)?;
        File::from(descriptor)
    };
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                current = open_or_create_directory(&current, name)?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "secure directory path contains a non-normal component",
                ));
            }
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn open_existing_secure_directory(path: &Path) -> io::Result<File> {
    let path = secure_path(path);
    let mut current = if path.is_absolute() {
        let descriptor =
            rustix::fs::open(Path::new("/"), directory_flags(), rustix::fs::Mode::empty())
                .map_err(io::Error::from)?;
        File::from(descriptor)
    } else {
        let descriptor =
            rustix::fs::open(Path::new("."), directory_flags(), rustix::fs::Mode::empty())
                .map_err(io::Error::from)?;
        File::from(descriptor)
    };
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => {
                current = open_directory_child(&current, name)?;
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "secure directory path contains a non-normal component",
                ));
            }
        }
    }
    Ok(current)
}

#[derive(Clone, Copy)]
struct TreeTraversalLimits {
    max_entries: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
    max_path_bytes: u64,
    max_path_depth: usize,
    max_nodes: usize,
    max_directories: usize,
    max_directory_entries: usize,
}

impl TreeTraversalLimits {
    const fn production() -> Self {
        Self {
            max_entries: MAX_TREE_ENTRIES,
            max_file_bytes: MAX_TREE_FILE_BYTES,
            max_total_bytes: MAX_TREE_TOTAL_BYTES,
            max_path_bytes: MAX_TREE_PATH_BYTES,
            max_path_depth: MAX_TREE_PATH_DEPTH,
            max_nodes: MAX_TREE_NODES,
            max_directories: MAX_TREE_DIRECTORIES,
            max_directory_entries: MAX_TREE_DIRECTORY_ENTRIES,
        }
    }
}

struct TreeTraversalBudget {
    limits: TreeTraversalLimits,
    nodes: usize,
    directories: usize,
    directory_entries: usize,
    total_path_bytes: u64,
    total_file_bytes: u64,
}

impl TreeTraversalBudget {
    fn new(limits: TreeTraversalLimits) -> Self {
        Self {
            limits,
            nodes: 0,
            directories: 0,
            directory_entries: 0,
            total_path_bytes: 0,
            total_file_bytes: 0,
        }
    }

    fn visit_node(&mut self, relative: &str, depth: usize) -> Result<(), CasError> {
        if depth > self.limits.max_path_depth {
            return Err(CasError::InvalidManifest(format!(
                "tree path exceeds the {}-component depth limit: '{relative}'",
                self.limits.max_path_depth
            )));
        }
        self.nodes = self.nodes.checked_add(1).ok_or_else(|| {
            CasError::InvalidManifest("tree node count overflowed".into())
        })?;
        if self.nodes > self.limits.max_nodes {
            return Err(CasError::InvalidManifest(format!(
                "tree contains too many nodes (limit {})",
                self.limits.max_nodes
            )));
        }
        self.directory_entries = self.directory_entries.checked_add(1).ok_or_else(|| {
            CasError::InvalidManifest("tree directory-entry count overflowed".into())
        })?;
        if self.directory_entries > self.limits.max_directory_entries {
            return Err(CasError::InvalidManifest(format!(
                "tree contains too many directory entries (limit {})",
                self.limits.max_directory_entries
            )));
        }
        self.total_path_bytes = self
            .total_path_bytes
            .checked_add(relative.len() as u64)
            .ok_or_else(|| CasError::InvalidManifest("tree path byte count overflowed".into()))?;
        if self.total_path_bytes > self.limits.max_path_bytes {
            return Err(CasError::InvalidManifest(format!(
                "tree paths exceed the {}-byte limit",
                self.limits.max_path_bytes
            )));
        }
        Ok(())
    }

    fn visit_directory(&mut self) -> Result<(), CasError> {
        self.directories = self.directories.checked_add(1).ok_or_else(|| {
            CasError::InvalidManifest("tree directory count overflowed".into())
        })?;
        if self.directories > self.limits.max_directories {
            return Err(CasError::InvalidManifest(format!(
                "tree contains too many directories (limit {})",
                self.limits.max_directories
            )));
        }
        Ok(())
    }

    fn reserve_file(&mut self, current_files: usize, bytes: u64) -> Result<(), CasError> {
        if current_files >= self.limits.max_entries {
            return Err(CasError::InvalidManifest(format!(
                "tree contains too many entries (limit {})",
                self.limits.max_entries
            )));
        }
        if bytes > self.limits.max_file_bytes {
            return Err(CasError::InvalidManifest(format!(
                "tree file is {bytes} bytes, exceeding the {}-byte limit",
                self.limits.max_file_bytes
            )));
        }
        self.total_file_bytes = self
            .total_file_bytes
            .checked_add(bytes)
            .ok_or_else(|| CasError::InvalidManifest("tree file byte count overflowed".into()))?;
        if self.total_file_bytes > self.limits.max_total_bytes {
            return Err(CasError::InvalidManifest(format!(
                "tree files exceed the {}-byte limit",
                self.limits.max_total_bytes
            )));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn collect_directory_tree(
    directory: &File,
    relative_root: &Path,
    cas: &CasStore,
    entries: &mut Vec<TreeEntry>,
    budget: &mut TreeTraversalBudget,
) -> Result<(), CasError> {
    for entry in rustix::fs::Dir::read_from(directory).map_err(io::Error::from)? {
        let entry = entry.map_err(io::Error::from)?;
        let name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if name == OsStr::new(".") || name == OsStr::new("..") {
            continue;
        }
        let stat = rustix::fs::statat(directory, &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io::Error::from)?;
        let relative = relative_root.join(&name);
        let relative = relative.to_str().ok_or_else(|| {
            CasError::InvalidManifest(format!(
                "tree source path is not valid UTF-8: {}",
                relative.display()
            ))
        })?;
        canonical_manifest_path(relative)?;
        let depth = relative_root.components().count() + 1;
        budget.visit_node(relative, depth)?;
        match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
            rustix::fs::FileType::Directory => {
                budget.visit_directory()?;
                let child = open_directory_child(directory, &name).map_err(|error| {
                    CasError::UnsafePath(format!(
                        "tree source directory could not be opened without following links: {} ({error})",
                        relative
                    ))
                })?;
                collect_directory_tree(&child, Path::new(relative), cas, entries, budget)?;
            }
            rustix::fs::FileType::RegularFile => {
                let file = open_regular_tree_source(directory, &name, relative)?;
                let size = file.metadata()?.len();
                budget.reserve_file(entries.len(), size)?;
                let mode = stat.st_mode & 0o777;
                let mut file = file;
                let (digest, read_bytes) = cas.put_file_with_limit(
                    &mut file,
                    budget.limits.max_file_bytes,
                )?;
                if read_bytes != size {
                    return Err(CasError::InvalidManifest(format!(
                        "tree source changed while reading: {relative}"
                    )));
                }
                entries.push(TreeEntry {
                    path: relative.to_owned(),
                    digest,
                    class: if mode & 0o111 != 0 {
                        FileClass::Executable
                    } else {
                        FileClass::Runtime
                    },
                    mode: u32::from(mode),
                });
            }
            _ => {
                return Err(CasError::UnsafePath(format!(
                    "tree source contains unsupported entry: {relative}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn open_regular_tree_source(
    directory: &File,
    name: &OsStr,
    relative: &str,
) -> Result<File, CasError> {
    let descriptor = rustix::fs::openat(
        directory,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let file: File = descriptor.into();
    if !file.metadata()?.file_type().is_file() {
        return Err(CasError::UnsafePath(format!(
            "tree source entry is not a regular file: {relative}"
        )));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn collect_directory_tree_path(
    root: &Path,
    current: &Path,
    cas: &CasStore,
    entries: &mut Vec<TreeEntry>,
    budget: &mut TreeTraversalBudget,
) -> Result<(), CasError> {
    for entry in fs::read_dir(current)? {
        let name = entry?.file_name();
        let path = current.join(&name);
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(CasError::UnsafePath(format!(
                "tree source contains a symlink: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| CasError::InvalidManifest(error.to_string()))?
            .to_str()
            .ok_or_else(|| CasError::InvalidManifest("tree source path is not UTF-8".into()))?
            .replace('\\', "/");
        canonical_manifest_path(&relative)?;
        let depth = Path::new(&relative).components().count();
        budget.visit_node(&relative, depth)?;
        if metadata.is_dir() {
            budget.visit_directory()?;
            collect_directory_tree_path(root, &path, cas, entries, budget)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(CasError::UnsafePath(format!(
                "tree source contains unsupported entry: {}",
                path.display()
            )));
        }
        let mut file = open_readonly_nofollow(&path)?;
        let size = file.metadata()?.len();
        budget.reserve_file(entries.len(), size)?;
        let (digest, read_bytes) = cas.put_file_with_limit(
            &mut file,
            budget.limits.max_file_bytes,
        )?;
        if read_bytes != size {
            return Err(CasError::InvalidManifest(format!(
                "tree source changed while reading: {relative}"
            )));
        }
        entries.push(TreeEntry {
            path: relative,
            digest,
            class: FileClass::Runtime,
            mode: 0o644,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn secure_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(relative) = path.strip_prefix("/tmp") {
            return Path::new("/private/tmp").join(relative);
        }
        if let Ok(relative) = path.strip_prefix("/var") {
            return Path::new("/private/var").join(relative);
        }
    }
    path.to_path_buf()
}

#[cfg(unix)]
fn secure_destination_target(
    destination_root: &File,
    destination: &Path,
    relative: &str,
) -> Result<MaterializationTarget, CasError> {
    let components = Path::new(relative)
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_owned()),
            _ => Err(CasError::InvalidManifest(format!(
                "materialization path is not a normal relative path: '{relative}'"
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (name, parents) = components
        .split_last()
        .ok_or_else(|| CasError::InvalidManifest("materialization path is empty".into()))?;
    let mut parent = destination_root.try_clone()?;
    for component in parents {
        parent = open_or_create_directory(&parent, component).map_err(|error| {
            if matches!(
                error.raw_os_error(),
                Some(code) if code == libc::ELOOP || code == libc::ENOTDIR
            ) {
                CasError::UnsafePath(format!(
                    "materialization path contains a symlink: {}",
                    relative
                ))
            } else {
                CasError::Io(error)
            }
        })?;
    }
    Ok(MaterializationTarget {
        path: destination.join(relative),
        parent,
        name: name.clone(),
    })
}

#[cfg(unix)]
fn open_created_file(parent: &File, name: &OsStr) -> io::Result<File> {
    let descriptor = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(io::Error::from)?;
    Ok(descriptor.into())
}

#[cfg(unix)]
fn open_existing_object(
    parent: &File,
    name: &str,
    digest: &Digest,
) -> Result<Option<File>, CasError> {
    let descriptor = rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    );
    let file = match descriptor {
        Ok(descriptor) => File::from(descriptor),
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
        Err(error) if error == rustix::io::Errno::LOOP => {
            return Err(CasError::UnsafePath(format!(
                "CAS object is a symlink: {digest}"
            )))
        }
        Err(error) => return Err(CasError::Io(io::Error::from(error))),
    };
    if !file.metadata()?.file_type().is_file() {
        return Err(CasError::UnsafePath(format!(
            "CAS object is not a regular file: {digest}"
        )));
    }
    Ok(Some(file))
}

#[cfg(unix)]
fn digest_file_handle(file: &File, max_bytes: u64) -> Result<Digest, CasError> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    digest_reader(&mut reader, max_bytes).map(|(digest, _)| digest)
}

#[cfg(unix)]
fn copy_verified_file(
    source: &File,
    destination: &mut File,
    expected: &Digest,
    max_bytes: u64,
) -> Result<(), CasError> {
    let mut reader = source.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let (actual, _) = copy_reader_with_digest(&mut reader, destination, max_bytes)?;
    if actual != *expected {
        return Err(CasError::Corrupt {
            expected: expected.clone(),
            actual,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.open(path)
}

#[cfg(not(unix))]
fn sync_file_nofollow(path: &Path) -> io::Result<()> {
    open_readonly_nofollow(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Reader that rejects an object whose final digest does not match its key.
pub struct VerifiedReader {
    file: File,
    expected: Digest,
    hasher: blake3::Hasher,
    verified: bool,
}

impl Read for VerifiedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let count = self.file.read(buffer)?;
        if count > 0 {
            self.hasher.update(&buffer[..count]);
        } else if !self.verified {
            self.verified = true;
            let actual = Digest::from_hash(self.hasher.finalize());
            if actual != self.expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CAS integrity mismatch: expected {}, got {}",
                        self.expected, actual
                    ),
                ));
            }
        }
        Ok(count)
    }
}

#[cfg(not(unix))]
fn safe_destination_path(destination: &Path, relative: &str) -> Result<PathBuf, CasError> {
    if destination.exists() {
        if fs::symlink_metadata(destination)?.file_type().is_symlink() {
            return Err(CasError::InvalidManifest(
                "materialization destination cannot be a symlink".into(),
            ));
        }
    } else {
        fs::create_dir_all(destination)?;
    }
    let mut current = destination.to_path_buf();
    let components = Path::new(relative).components().collect::<Vec<_>>();
    let component_count = components.len();
    for (index, component) in components.into_iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(CasError::InvalidManifest(format!(
                "materialization path is not a normal relative path: '{relative}'"
            )));
        };
        current.push(name);
        if current.exists() && fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(CasError::InvalidManifest(format!(
                "materialization path contains a symlink: '{relative}'"
            )));
        }
        if index + 1 < component_count && !current.exists() {
            fs::create_dir_all(&current)?;
        }
    }
    if let Some(parent) = current.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(current)
}

#[cfg(unix)]
fn materialize_file(
    source: &File,
    parent: &File,
    name: &OsStr,
    expected: &Digest,
    mode: u32,
) -> Result<(MaterializationMethod, File), CasError> {
    let temp_name = format!(
        ".{}.materialize-{}",
        name.to_string_lossy(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let method = match try_clone(source, parent, OsStr::new(&temp_name)) {
        Ok(CloneAttempt::Cloned(method)) => method,
        Ok(CloneAttempt::Fallback(mut file)) => {
            if let Err(error) = copy_verified_file(source, &mut file, expected, MAX_TREE_FILE_BYTES)
            {
                drop(file);
                if let Err(cleanup_error) = remove_temp_secure(parent, &temp_name) {
                    return Err(cleanup_error.into());
                }
                return Err(error);
            }
            if let Err(error) = file.sync_all() {
                drop(file);
                if let Err(cleanup_error) = remove_temp_secure(parent, &temp_name) {
                    return Err(cleanup_error.into());
                }
                return Err(error.into());
            }
            MaterializationMethod::VerifiedCopy
        }
        Err(error) => return Err(error.into()),
    };
    let temp_file = match rustix::fs::openat(
        parent,
        &temp_name,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    ) {
        Ok(file) => file,
        Err(error) => {
            let _ = remove_temp_secure(parent, &temp_name);
            return Err(io::Error::from(error).into());
        }
    };
    let temp_file: File = temp_file.into();
    let actual = match digest_file_handle(&temp_file, MAX_TREE_FILE_BYTES) {
        Ok(actual) => actual,
        Err(error) => {
            let _ = remove_temp_secure(parent, &temp_name);
            return Err(error);
        }
    };
    if actual != *expected {
        let _ = remove_temp_secure(parent, &temp_name);
        return Err(CasError::Corrupt {
            expected: expected.clone(),
            actual,
        });
    }
    if let Err(error) = rustix::fs::fchmod(
        &temp_file,
        rustix::fs::Mode::from_raw_mode(mode as rustix::fs::RawMode),
    ) {
        let _ = remove_temp_secure(parent, &temp_name);
        return Err(io::Error::from(error).into());
    }
    if let Err(error) = temp_file.sync_all() {
        let _ = remove_temp_secure(parent, &temp_name);
        return Err(error.into());
    }

    match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if rustix::fs::FileType::from_raw_mode(stat.st_mode).is_symlink() => {
            let _ = remove_temp_secure(parent, &temp_name);
            return Err(CasError::InvalidManifest(format!(
                "materialization target is a symlink: {}",
                name.to_string_lossy()
            )));
        }
        Ok(_) => {
            if let Err(error) = rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty()) {
                let _ = remove_temp_secure(parent, &temp_name);
                return Err(io::Error::from(error).into());
            }
        }
        Err(error) if error == rustix::io::Errno::NOENT => {}
        Err(error) => {
            let _ = remove_temp_secure(parent, &temp_name);
            return Err(io::Error::from(error).into());
        }
    }
    rustix::fs::renameat(parent, &temp_name, parent, name).map_err(|error| {
        remove_temp_secure(parent, &temp_name)
            .err()
            .unwrap_or_else(|| io::Error::from(error))
    })?;
    parent.sync_all()?;
    Ok((method, temp_file))
}

#[cfg(not(unix))]
fn materialize_file(
    source: &Path,
    path: &Path,
    bytes: &[u8],
    expected: &Digest,
    mode: u32,
) -> Result<MaterializationMethod, CasError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(CasError::InvalidManifest(format!(
                "materialization target is a symlink: {}",
                path.display()
            )));
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| CasError::InvalidManifest("materialization target has no parent".into()))?;
    let temp = parent.join(format!(
        ".{}.materialize-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let method = match try_clone(source, &temp) {
        Ok(method) => method,
        Err(_) => {
            let _ = fs::remove_file(&temp);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = options.open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            MaterializationMethod::VerifiedCopy
        }
    };
    let actual = digest_file(&temp)?;
    if actual != *expected {
        let _ = fs::remove_file(&temp);
        return Err(CasError::Corrupt {
            expected: expected.clone(),
            actual,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    }
    sync_file_nofollow(&temp)?;
    fs::rename(&temp, path).or_else(|error| {
        if path.exists() && !fs::symlink_metadata(path)?.file_type().is_symlink() {
            fs::remove_file(path)?;
            fs::rename(&temp, path)
        } else {
            let _ = fs::remove_file(&temp);
            Err(error)
        }
    })?;
    sync_directory(parent)?;
    Ok(method)
}

#[cfg(not(unix))]
fn digest_file(path: &Path) -> Result<Digest, CasError> {
    let mut reader = open_readonly_nofollow(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(Digest::from_hash(hasher.finalize()));
        }
        hasher.update(&buffer[..count]);
    }
}

#[cfg(unix)]
enum CloneAttempt {
    Cloned(MaterializationMethod),
    Fallback(File),
}

#[cfg(unix)]
fn try_clone(source: &File, parent: &File, destination: &OsStr) -> io::Result<CloneAttempt> {
    #[cfg(target_os = "linux")]
    {
        let destination_file = open_created_file(parent, destination)?;
        match rustix::fs::ioctl_ficlone(&destination_file, source) {
            Ok(()) => Ok(CloneAttempt::Cloned(MaterializationMethod::Reflink)),
            Err(_) => {
                destination_file.set_len(0)?;
                let mut destination_file = destination_file;
                destination_file.seek(SeekFrom::Start(0))?;
                Ok(CloneAttempt::Fallback(destination_file))
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        match rustix::fs::fclonefileat(source, parent, destination, rustix::fs::CloneFlags::empty())
        {
            Ok(()) => Ok(CloneAttempt::Cloned(MaterializationMethod::ApfsClone)),
            Err(error) => match open_created_file(parent, destination) {
                Ok(file) => Ok(CloneAttempt::Fallback(file)),
                Err(_) => Err(io::Error::from(error)),
            },
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = source;
        Ok(CloneAttempt::Fallback(open_created_file(
            parent,
            destination,
        )?))
    }
}

#[cfg(not(unix))]
fn try_clone(source: &Path, destination: &Path) -> io::Result<MaterializationMethod> {
    let _ = (source, destination);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "copy-on-write cloning is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[derive(Default)]
    struct BudgetProbe {
        reserved: Mutex<Vec<u64>>,
        released: Mutex<Vec<u64>>,
    }

    impl BudgetCallback for BudgetProbe {
        fn reserve(&self, bytes: u64) -> Result<(), String> {
            self.reserved.lock().unwrap().push(bytes);
            Ok(())
        }

        fn release(&self, bytes: u64) {
            self.released.lock().unwrap().push(bytes);
        }
    }

    #[cfg(unix)]
    fn allocated_bytes(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;

        fs::metadata(path)
            .unwrap()
            .blocks()
            .checked_mul(512)
            .unwrap()
    }

    fn store() -> (TempDir, CasStore) {
        let temp = TempDir::new().unwrap();
        let store = CasStore::new(temp.path().join("cas")).unwrap();
        (temp, store)
    }

    #[test]
    fn round_trip_and_stream_verify() {
        let (_temp, store) = store();
        let digest = store.put(b"hello cas").unwrap();
        assert_eq!(store.get(&digest).unwrap(), b"hello cas");
        let mut reader = store.stream(&digest).unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"hello cas");
    }

    #[test]
    fn physical_accounting_serialization_preserves_unknown_and_known_invariants() {
        let unknown = PhysicalByteAccounting::unknown();
        assert_eq!(
            serde_json::to_value(unknown).unwrap(),
            serde_json::json!({"known": false})
        );
        let known = PhysicalByteAccounting::known(11, 23);
        assert_eq!(
            serde_json::to_value(known).unwrap(),
            serde_json::json!({
                "known": true,
                "shared_bytes": 11,
                "newly_allocated_bytes": 23
            })
        );
        assert!(
            serde_json::from_value::<PhysicalByteAccounting>(serde_json::json!({
                "known": true,
                "shared_bytes": 11
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<PhysicalByteAccounting>(serde_json::json!({
                "known": false,
                "shared_bytes": 11,
                "newly_allocated_bytes": 23
            }))
            .is_err()
        );
        for known in [true, false] {
            for field in ["shared_bytes", "newly_allocated_bytes"] {
                let mut value = serde_json::json!({
                    "known": known,
                    "shared_bytes": 11,
                    "newly_allocated_bytes": 23
                });
                if !known {
                    value["shared_bytes"] = serde_json::Value::Null;
                    value["newly_allocated_bytes"] = serde_json::Value::Null;
                } else {
                    value[field] = serde_json::Value::Null;
                }
                assert!(
                    serde_json::from_value::<PhysicalByteAccounting>(value).is_err(),
                    "explicit null must be rejected for {field} with known={known}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn first_cas_insert_is_new_and_duplicate_is_shared() {
        let (_temp, store) = store();
        let bytes = vec![0x5a; 8192];
        let (digest, first) = store.put_with_accounting(&bytes).unwrap();
        let allocated = allocated_bytes(&store.object_path(&digest));
        assert_eq!(first, PhysicalByteAccounting::known(0, allocated));

        let (duplicate_digest, duplicate) = store.put_with_accounting(&bytes).unwrap();
        assert_eq!(duplicate_digest, digest);
        assert_eq!(duplicate, PhysicalByteAccounting::known(allocated, 0));
    }

    #[cfg(unix)]
    #[test]
    fn zero_size_cas_insert_reports_exact_zero_allocation() {
        let (_temp, store) = store();
        let (digest, accounting) = store.put_with_accounting(b"").unwrap();
        let allocated = allocated_bytes(&store.object_path(&digest));
        assert_eq!(allocated, 0);
        assert_eq!(accounting, PhysicalByteAccounting::known(0, allocated));
    }

    #[cfg(not(unix))]
    #[test]
    fn unsupported_platform_reports_unknown_cas_accounting() {
        let (_temp, store) = store();
        let (_, accounting) = store.put_with_accounting(b"bytes").unwrap();
        assert!(!accounting.is_known());
        assert_eq!(accounting.shared_bytes(), None);
        assert_eq!(accounting.newly_allocated_bytes(), None);
    }

    #[test]
    fn corruption_is_rejected_with_normalized_reason() {
        let (_temp, store) = store();
        let digest = store.put(b"original").unwrap();
        let path = store.object_path(&digest);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 0x01;
        fs::write(path, bytes).unwrap();
        let error = store.get(&digest).unwrap_err();
        assert_eq!(error.miss_reason(), Some(MissReason::CorruptEntryRejected));
    }

    #[test]
    fn missing_object_has_normalized_reason() {
        let (_temp, store) = store();
        let digest = Digest::from_bytes(b"missing");
        let error = store.get(&digest).unwrap_err();
        assert_eq!(error.miss_reason(), Some(MissReason::KeyAbsent));
    }

    #[test]
    fn budget_reserves_only_new_objects_and_does_not_release_published_bytes() {
        let (_temp, store) = store();
        let budget = BudgetProbe::default();
        let bytes = b"budgeted object";

        store.put_with_budget(bytes, &budget).unwrap();
        store.put_with_budget(bytes, &budget).unwrap();

        assert_eq!(*budget.reserved.lock().unwrap(), vec![bytes.len() as u64]);
        assert!(budget.released.lock().unwrap().is_empty());
    }

    #[test]
    fn lazy_subset_materializes_exact_classes() {
        let (temp, store) = store();
        let executable = store.put(b"bin").unwrap();
        let runtime = store.put(b"lib").unwrap();
        let bundle = store.put(b"bundle").unwrap();
        let metadata = store.put(b"metadata").unwrap();
        let root = store
            .put_tree(&TreeManifest {
                entries: vec![
                    TreeEntry {
                        path: "bin/tool".into(),
                        digest: executable,
                        class: FileClass::Executable,
                        mode: 0o755,
                    },
                    TreeEntry {
                        path: "lib/libtool.so".into(),
                        digest: runtime,
                        class: FileClass::Runtime,
                        mode: 0o644,
                    },
                    TreeEntry {
                        path: "Tool.app/Contents/Info.plist".into(),
                        digest: bundle,
                        class: FileClass::Bundle,
                        mode: 0o644,
                    },
                    TreeEntry {
                        path: "manifest.json".into(),
                        digest: metadata,
                        class: FileClass::Metadata,
                        mode: 0o644,
                    },
                ],
            })
            .unwrap();
        let cases = [
            (SubsetSelector::ExecutablesOnly, ["bin/tool"].as_slice()),
            (
                SubsetSelector::RuntimeFiles,
                ["bin/tool", "lib/libtool.so"].as_slice(),
            ),
            (
                SubsetSelector::BundleOnly,
                ["Tool.app/Contents/Info.plist"].as_slice(),
            ),
            (SubsetSelector::MetadataOnly, ["manifest.json"].as_slice()),
        ];
        for (index, (selector, expected)) in cases.into_iter().enumerate() {
            let destination = temp.path().join(format!("materialized-{index}"));
            let paths = store
                .materialize_subset(&root, selector, &destination)
                .unwrap();
            assert_eq!(
                paths
                    .iter()
                    .map(|path| path.strip_prefix(&destination).unwrap().to_str().unwrap())
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn materialization_reports_allocation_by_copy_or_shared_method() {
        let (temp, store) = store();
        let digest = store.put(&vec![0x33; 8192]).unwrap();
        let root = store
            .put_tree(&TreeManifest {
                entries: vec![TreeEntry {
                    path: "bin/tool".into(),
                    digest,
                    class: FileClass::Executable,
                    mode: 0o755,
                }],
            })
            .unwrap();

        let report = store
            .materialize_subset_report(
                &root,
                SubsetSelector::ExecutablesOnly,
                &temp.path().join("materialized"),
            )
            .unwrap();
        let allocated = allocated_bytes(&report.paths[0]);
        let expected = if report
            .methods
            .contains_key(&MaterializationMethod::ApfsClone)
            || report.methods.contains_key(&MaterializationMethod::Reflink)
        {
            PhysicalByteAccounting::known(allocated, 0)
        } else {
            PhysicalByteAccounting::known(0, allocated)
        };
        assert_eq!(report.accounting, expected);
    }

    #[cfg(unix)]
    #[test]
    fn materialization_deduplicates_shared_accounting_for_repeated_digest() {
        let (temp, store) = store();
        let digest = store.put(&vec![0x44; 8192]).unwrap();
        let repeated_root = store
            .put_tree(&TreeManifest {
                entries: vec![
                    TreeEntry {
                        path: "first".into(),
                        digest: digest.clone(),
                        class: FileClass::Runtime,
                        mode: 0o644,
                    },
                    TreeEntry {
                        path: "second".into(),
                        digest,
                        class: FileClass::Runtime,
                        mode: 0o644,
                    },
                ],
            })
            .unwrap();

        let repeated = store
            .materialize_subset_report(
                &repeated_root,
                SubsetSelector::RuntimeFiles,
                &temp.path().join("repeated-destination"),
            )
            .unwrap();

        assert_eq!(repeated.paths.len(), 2);
        if repeated
            .methods
            .contains_key(&MaterializationMethod::ApfsClone)
            || repeated
                .methods
                .contains_key(&MaterializationMethod::Reflink)
        {
            assert_eq!(
                repeated.accounting,
                PhysicalByteAccounting::known(allocated_bytes(&repeated.paths[0]), 0)
            );
        } else {
            assert_eq!(
                repeated.accounting,
                PhysicalByteAccounting::known(
                    0,
                    allocated_bytes(&repeated.paths[0])
                        .checked_add(allocated_bytes(&repeated.paths[1]))
                        .unwrap()
                )
            );
        }
    }

    #[test]
    fn manifest_entry_order_does_not_change_root_digest() {
        let (_temp, store) = store();
        let first_digest = store.put(b"first").unwrap();
        let second_digest = store.put(b"second").unwrap();
        let first = TreeEntry {
            path: "bin/first".into(),
            digest: first_digest,
            class: FileClass::Executable,
            mode: 0o755,
        };
        let second = TreeEntry {
            path: "lib/second".into(),
            digest: second_digest,
            class: FileClass::Runtime,
            mode: 0o644,
        };
        let forward = store
            .put_tree(&TreeManifest {
                entries: vec![first.clone(), second.clone()],
            })
            .unwrap();
        let reverse = store
            .put_tree(&TreeManifest {
                entries: vec![second, first],
            })
            .unwrap();
        assert_eq!(forward, reverse);
    }

    #[test]
    fn manifest_rejects_escape_paths() {
        let (_temp, store) = store();
        let digest = store.put(b"x").unwrap();
        let error = store
            .put_tree(&TreeManifest {
                entries: vec![TreeEntry {
                    path: "../escape".into(),
                    digest,
                    class: FileClass::Metadata,
                    mode: 0o644,
                }],
            })
            .unwrap_err();
        assert!(matches!(error, CasError::InvalidManifest(_)));
    }

    #[test]
    fn manifest_rejects_filesystem_aliases_and_special_modes() {
        let (_temp, store) = store();
        let digest = store.put(b"x").unwrap();
        let aliases = [
            TreeManifest {
                entries: vec![
                    TreeEntry {
                        path: "a//b".into(),
                        digest: digest.clone(),
                        class: FileClass::Metadata,
                        mode: 0o644,
                    },
                    TreeEntry {
                        path: "a/b".into(),
                        digest: digest.clone(),
                        class: FileClass::Metadata,
                        mode: 0o644,
                    },
                ],
            },
            TreeManifest {
                entries: vec![
                    TreeEntry {
                        path: "A".into(),
                        digest: digest.clone(),
                        class: FileClass::Metadata,
                        mode: 0o644,
                    },
                    TreeEntry {
                        path: "a".into(),
                        digest,
                        class: FileClass::Metadata,
                        mode: 0o644,
                    },
                ],
            },
        ];
        for manifest in aliases {
            assert!(matches!(
                store.put_tree(&manifest),
                Err(CasError::InvalidManifest(_))
            ));
        }
    }

    #[test]
    fn manifest_rejects_special_permission_bits() {
        let (_temp, store) = store();
        let digest = store.put(b"x").unwrap();
        let error = store
            .put_tree(&TreeManifest {
                entries: vec![TreeEntry {
                    path: "setuid".into(),
                    digest,
                    class: FileClass::Executable,
                    mode: 0o4755,
                }],
            })
            .unwrap_err();
        assert!(matches!(error, CasError::InvalidManifest(_)));
    }

    #[test]
    fn manifest_rejects_file_descendants_and_control_paths() {
        let (_temp, store) = store();
        let digest = store.put(b"x").unwrap();
        let error = store
            .put_tree(&TreeManifest {
                entries: vec![
                    TreeEntry {
                        path: "file".into(),
                        digest: digest.clone(),
                        class: FileClass::Metadata,
                        mode: 0o644,
                    },
                    TreeEntry {
                        path: "file/child".into(),
                        digest: digest.clone(),
                        class: FileClass::Metadata,
                        mode: 0o644,
                    },
                ],
            })
            .unwrap_err();
        assert!(matches!(error, CasError::InvalidManifest(_)));

        let error = store
            .put_tree(&TreeManifest {
                entries: vec![TreeEntry {
                    path: "bad\nname".into(),
                    digest,
                    class: FileClass::Metadata,
                    mode: 0o644,
                }],
            })
            .unwrap_err();
        assert!(matches!(error, CasError::InvalidManifest(_)));
    }

    #[test]
    fn materialization_rejects_unknown_manifest_fields() {
        let (temp, store) = store();
        let digest = store.put(b"x").unwrap();
        let manifest = format!(
            r#"{{"entries":[{{"path":"file","digest":"{digest}","class":"metadata","extra":true}}]}}"#
        );
        let root = store.put(manifest.as_bytes()).unwrap();

        let error = store
            .materialize_subset(
                &root,
                SubsetSelector::MetadataOnly,
                &temp.path().join("unknown-field"),
            )
            .unwrap_err();
        assert!(matches!(error, CasError::InvalidManifest(_)));
    }

    #[test]
    fn oversized_manifest_is_rejected_before_json_allocation() {
        let (temp, store) = store();
        let oversized = vec![b'x'; (MAX_TREE_MANIFEST_BYTES + 1) as usize];
        let root = store.put(&oversized).unwrap();

        let error = store
            .materialize_subset(
                &root,
                SubsetSelector::MetadataOnly,
                &temp.path().join("oversized-manifest"),
            )
            .unwrap_err();
        assert!(matches!(error, CasError::InvalidManifest(_)));
    }

    #[cfg(unix)]
    #[test]
    fn cas_rejects_symlink_and_non_regular_objects() {
        use std::os::unix::fs::symlink;

        let (temp, store) = store();
        let symlink_digest = Digest::from_bytes(b"symlink object");
        let symlink_path = store.object_path(&symlink_digest);
        fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
        let outside = temp.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &symlink_path).unwrap();
        assert!(matches!(
            store.get(&symlink_digest),
            Err(CasError::UnsafePath(_))
        ));

        let directory_digest = Digest::from_bytes(b"directory object");
        let directory_path = store.object_path(&directory_digest);
        fs::create_dir_all(&directory_path).unwrap();
        assert!(matches!(
            store.get(&directory_digest),
            Err(CasError::UnsafePath(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn opened_tree_descriptor_rejects_fifo_before_reading() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let (temp, _store) = store();
        let root = temp.path().join("tree");
        fs::create_dir(&root).unwrap();
        let fifo = root.join("fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_name` is a valid, NUL-terminated path and the mode is
        // a valid permission mask. The call only creates the test fixture.
        let result = unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) };
        assert_eq!(result, 0, "mkfifo failed: {}", io::Error::last_os_error());

        let directory = open_existing_secure_directory(&root).unwrap();
        let error = open_regular_tree_source(&directory, OsStr::new("fifo"), "fifo")
            .expect_err("FIFO must be rejected at the opened descriptor boundary");
        assert!(matches!(error, CasError::UnsafePath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn materialization_does_not_follow_symlinked_destination_parent() {
        use std::os::unix::fs::symlink;

        let (temp, store) = store();
        let digest = store.put(b"safe content").unwrap();
        let root = store
            .put_tree(&TreeManifest {
                entries: vec![TreeEntry {
                    path: "nested/file".into(),
                    digest,
                    class: FileClass::Metadata,
                    mode: 0o644,
                }],
            })
            .unwrap();
        let destination = temp.path().join("destination");
        fs::create_dir_all(&destination).unwrap();
        let outside = temp.path().join("outside-destination");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, destination.join("nested")).unwrap();

        let error = store
            .materialize_subset(&root, SubsetSelector::MetadataOnly, &destination)
            .unwrap_err();
        assert!(
            matches!(error, CasError::UnsafePath(_)),
            "unexpected error: {error:?}"
        );
        assert!(!outside.join("file").exists());
    }
}
