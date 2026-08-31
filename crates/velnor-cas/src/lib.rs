//! Small, durable content-addressed storage primitive.
//!
//! Objects are written to a two-level digest path with temp-file + fsync +
//! atomic rename. Reads always verify BLAKE3 before returning bytes. Tree
//! manifests make lazy subset materialization explicit without exposing a
//! mutable reference to stored content.

use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(unix))]
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{Seek, SeekFrom},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;
use velnor_action_model::{canonical_json_bytes, Digest, DigestError};

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

/// Aggregate evidence from one lazy materialization operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializationReport {
    pub paths: Vec<PathBuf>,
    pub methods: std::collections::BTreeMap<MaterializationMethod, usize>,
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
pub struct TreeManifest {
    pub entries: Vec<TreeEntry>,
}

impl TreeManifest {
    fn validate(&self) -> Result<(), CasError> {
        let mut paths = std::collections::BTreeSet::new();
        for entry in &self.entries {
            let identity = canonical_manifest_path(&entry.path)?;
            if entry.mode & !0o777 != 0 || !paths.insert(identity) {
                return Err(CasError::InvalidManifest(format!(
                    "entry path must be unique, portable, relative, and regular: '{}'",
                    entry.path
                )));
            }
        }
        Ok(())
    }
}

fn canonical_manifest_path(path: &str) -> Result<String, CasError> {
    let path_ref = Path::new(path);
    if path.is_empty() || path.contains('\\') || path.contains('\0') || path_ref.is_absolute() {
        return Err(CasError::InvalidManifest(format!(
            "entry path must be portable and relative: '{path}'"
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
        self.put_inner(bytes, None)
    }

    /// Store bytes while notifying the bounded-store policy hook.
    pub fn put_with_budget(
        &self,
        bytes: &[u8],
        budget: &dyn BudgetCallback,
    ) -> Result<Digest, CasError> {
        self.put_inner(bytes, Some(budget))
    }

    fn put_inner(
        &self,
        bytes: &[u8],
        budget: Option<&dyn BudgetCallback>,
    ) -> Result<Digest, CasError> {
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
    ) -> Result<Digest, CasError> {
        let digest = Digest::from_bytes(bytes);
        let bucket = self.open_bucket(&digest, true)?;
        let object_name = &digest.as_str()[2..];
        if open_existing_object(&bucket, object_name, &digest)?.is_some() {
            let _ = self.get(&digest)?;
            return Ok(digest);
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
        file.write_all(bytes)?;
        file.sync_all()?;
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
                let _ = rustix::fs::unlinkat(&bucket, &temp_name, rustix::fs::AtFlags::empty());
                let _ = self.get(&digest)?;
                return Ok(digest);
            }
            Err(error) => {
                let _ = rustix::fs::unlinkat(&bucket, &temp_name, rustix::fs::AtFlags::empty());
                return Err(io::Error::from(error).into());
            }
        }
        bucket.sync_all()?;
        if let Some(reservation) = &mut reservation {
            reservation.committed = true;
        }
        Ok(digest)
    }

    #[cfg(not(unix))]
    fn put_inner_path(
        &self,
        bytes: &[u8],
        budget: Option<&dyn BudgetCallback>,
    ) -> Result<Digest, CasError> {
        let digest = Digest::from_bytes(bytes);
        let path = self.checked_object_path(&digest)?;
        if path.exists() {
            let _ = self.get(&digest)?;
            return Ok(digest);
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
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::rename(&temp, &path) {
            Ok(()) => {}
            Err(_error) if path.exists() => {
                let _ = fs::remove_file(&temp);
                let _ = self.get(&digest)?;
                return Ok(digest);
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(error.into());
            }
        }
        sync_directory(parent)?;
        if let Some(reservation) = &mut reservation {
            reservation.committed = true;
        }
        Ok(digest)
    }

    /// Read and verify an object in full.
    pub fn get(&self, digest: &Digest) -> Result<Vec<u8>, CasError> {
        let mut file = self.open_object(digest)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let actual = Digest::from_bytes(&bytes);
        if actual != *digest {
            return Err(CasError::Corrupt {
                expected: digest.clone(),
                actual,
            });
        }
        Ok(bytes)
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
        manifest.validate()?;
        let mut canonical_manifest = manifest.clone();
        canonical_manifest
            .entries
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.put(&canonical_json_bytes(&canonical_manifest)?)
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
        let manifest: TreeManifest = serde_json::from_slice(&self.get(root_digest)?)?;
        manifest.validate()?;
        let mut materialized = Vec::new();
        let mut methods = std::collections::BTreeMap::new();
        for entry in manifest
            .entries
            .iter()
            .filter(|entry| selector.matches(entry.class))
        {
            let bytes = self.get(&entry.digest)?;
            #[cfg(unix)]
            let target = secure_destination_target(destination, &entry.path)?;
            #[cfg(unix)]
            let source = self.open_object(&entry.digest)?;
            #[cfg(unix)]
            let method = materialize_file(
                &source,
                &target.parent,
                &target.name,
                &bytes,
                &entry.digest,
                entry.mode,
            )?;
            #[cfg(not(unix))]
            let path = safe_destination_path(destination, &entry.path)?;
            #[cfg(not(unix))]
            let source = self.checked_object_path(&entry.digest)?;
            #[cfg(not(unix))]
            let method = materialize_file(&source, &path, &bytes, &entry.digest, entry.mode)?;
            *methods.entry(method).or_insert(0) += 1;
            #[cfg(unix)]
            materialized.push(target.path);
            #[cfg(not(unix))]
            materialized.push(path);
        }
        Ok(MaterializationReport {
            paths: materialized,
            methods,
        })
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
    eprintln!("parent {:?}, name {:?}", parent.metadata(), name);
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
fn canonical_creation_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if let Ok(metadata) = fs::symlink_metadata(&absolute)
        && metadata.file_type().is_symlink()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "secure path cannot be a symlink",
        ));
    }
    let mut unresolved = Vec::new();
    let mut existing = absolute.clone();
    while fs::symlink_metadata(&existing).is_err() {
        let name = existing.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "secure path has no existing ancestor",
            )
        })?;
        unresolved.push(name.to_owned());
        existing.pop();
    }
    let mut result = fs::canonicalize(existing)?;
    for component in unresolved.iter().rev() {
        result.push(component);
    }
    Ok(result)
}

#[cfg(unix)]
fn open_secure_directory(path: &Path) -> io::Result<File> {
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
    eprintln!("base {:?}", current.metadata());

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
fn secure_destination_target(
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
    let secure_destination = canonical_creation_path(destination)?;
    let mut parent = open_secure_directory(&secure_destination)?;
    for component in parents {
        parent = open_or_create_directory(&parent, component)?;
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
fn digest_file_handle(file: &File) -> Result<Digest, CasError> {
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
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
    bytes: &[u8],
    expected: &Digest,
    mode: u32,
) -> Result<MaterializationMethod, CasError> {
    let temp_name = format!(
        ".{}.materialize-{}",
        name.to_string_lossy(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let method = match try_clone(source, parent, OsStr::new(&temp_name)) {
        Ok(method) => method,
        Err(_) => {
            let _ = rustix::fs::unlinkat(parent, &temp_name, rustix::fs::AtFlags::empty());
            let mut file = open_created_file(parent, OsStr::new(&temp_name))?;
            file.write_all(bytes)?;
            file.sync_all()?;
            MaterializationMethod::VerifiedCopy
        }
    };
    let temp_file = rustix::fs::openat(
        parent,
        &temp_name,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let temp_file: File = temp_file.into();
    let actual = digest_file_handle(&temp_file)?;
    if actual != *expected {
        let _ = rustix::fs::unlinkat(parent, &temp_name, rustix::fs::AtFlags::empty());
        return Err(CasError::Corrupt {
            expected: expected.clone(),
            actual,
        });
    }
    rustix::fs::fchmod(
        &temp_file,
        rustix::fs::Mode::from_raw_mode(mode as rustix::fs::RawMode),
    )
    .map_err(io::Error::from)?;
    temp_file.sync_all()?;
    drop(temp_file);

    match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if rustix::fs::FileType::from_raw_mode(stat.st_mode).is_symlink() => {
            let _ = rustix::fs::unlinkat(parent, &temp_name, rustix::fs::AtFlags::empty());
            return Err(CasError::InvalidManifest(format!(
                "materialization target is a symlink: {}",
                name.to_string_lossy()
            )));
        }
        Ok(_) => {
            rustix::fs::unlinkat(parent, name, rustix::fs::AtFlags::empty())
                .map_err(io::Error::from)?;
        }
        Err(error) if error == rustix::io::Errno::NOENT => {}
        Err(error) => {
            let _ = rustix::fs::unlinkat(parent, &temp_name, rustix::fs::AtFlags::empty());
            return Err(io::Error::from(error).into());
        }
    }
    rustix::fs::renameat(parent, &temp_name, parent, name).map_err(|error| {
        let _ = rustix::fs::unlinkat(parent, &temp_name, rustix::fs::AtFlags::empty());
        io::Error::from(error)
    })?;
    parent.sync_all()?;
    Ok(method)
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
fn try_clone(
    source: &File,
    parent: &File,
    destination: &OsStr,
) -> io::Result<MaterializationMethod> {
    #[cfg(target_os = "linux")]
    {
        let destination_file = open_created_file(parent, destination)?;
        rustix::fs::ioctl_ficlone(&destination_file, source).map_err(io::Error::from)?;
        Ok(MaterializationMethod::Reflink)
    }
    #[cfg(target_os = "macos")]
    {
        rustix::fs::fclonefileat(source, parent, destination, rustix::fs::CloneFlags::empty())
            .map_err(io::Error::from)?;
        Ok(MaterializationMethod::ApfsClone)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (source, parent, destination);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "copy-on-write cloning is unavailable on this platform",
        ))
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
    use tempfile::TempDir;

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
}
