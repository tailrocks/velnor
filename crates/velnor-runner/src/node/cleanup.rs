//! Exact-path job ownership and completion I/O.
//!
//! Cleanup may delete only `{state}/owned/{id}.{gen}` and `{state}/jobs/{id}`.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags};

/// Completion responses are small protocol records, not artifact storage.
/// Bound both durable writes and recovery reads so a hostile or corrupted
/// outbox cannot consume unbounded disk or heap.
pub const MAX_COMPLETION_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const OUTBOX_LOCK_NAME: &str = ".velnor-outbox.lock";

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OutboxDirectoryIdentity {
    device: u64,
    inode: u64,
}

#[cfg(not(unix))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OutboxDirectoryIdentity;

#[cfg(unix)]
pub(crate) struct OutboxLock {
    _state_dir: std::fs::File,
    state_path: PathBuf,
    parent: std::fs::File,
}

#[cfg(not(unix))]
pub(crate) struct OutboxLock {
    parent: PathBuf,
    state_path: PathBuf,
    _lock: std::fs::File,
}

static NEXT_OUTBOX_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// Marker that a generation owns a job. Contents are the worker pid if known.
#[must_use]
pub fn owned_path(state_dir: &Path, isolation_id: &str, generation: u64) -> PathBuf {
    state_dir
        .join("owned")
        .join(format!("{isolation_id}.{generation}"))
}

/// Job-local directory for that isolation id. Never a glob.
#[must_use]
pub fn job_dir(state_dir: &Path, isolation_id: &str) -> PathBuf {
    state_dir.join("jobs").join(isolation_id)
}

/// Durable outbox payload path written before transport.
#[must_use]
pub fn outbox_path(state_dir: &Path, job_id: &str, generation: u64) -> PathBuf {
    state_dir
        .join("outbox")
        .join(format!("{job_id}.{generation}"))
}

#[must_use]
pub(crate) fn outbox_publication_owner_path(
    state_dir: &Path,
    job_id: &str,
    generation: u64,
) -> PathBuf {
    state_dir
        .join("owned")
        .join(format!(".outbox.{job_id}.{generation}"))
}

/// Reserve the ownership path and create the job directory.
///
/// # Errors
/// Invalid isolation id or filesystem failures.
pub fn claim_owned(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
) -> anyhow::Result<PathBuf> {
    assert_safe_id(isolation_id)?;
    let owned = owned_path(state_dir, isolation_id, generation);
    if let Some(parent) = owned.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(job_dir(state_dir, isolation_id))?;
    Ok(owned)
}

/// Record the worker pid inside the ownership marker.
///
/// # Errors
/// Invalid isolation id or write failures.
pub fn write_owned_pid(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    pid: u32,
) -> anyhow::Result<()> {
    assert_safe_id(isolation_id)?;
    let _lock = lock_outbox_shared(state_dir)?;
    write_owned_pid_locked(state_dir, isolation_id, generation, pid)
}

fn write_owned_pid_locked(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    pid: u32,
) -> anyhow::Result<()> {
    let owned = claim_owned(state_dir, isolation_id, generation)?;
    let parent = owned
        .parent()
        .ok_or_else(|| anyhow::anyhow!("ownership marker has no parent"))?;
    write_pid_marker_locked(parent, &format!("{isolation_id}.{generation}"), pid)
}

fn ensure_outbox_publication_owner_locked(
    state_dir: &Path,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    let parent = state_dir.join("owned");
    std::fs::create_dir_all(&parent)?;
    let path = outbox_publication_owner_path(state_dir, job_id, generation);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("outbox publication owner has invalid name"))?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "outbox publication owner must not be a symlink: {}",
                path.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!(
                "outbox publication owner must be a regular file: {}",
                path.display()
            )
        }
        Ok(_) => write_pid_marker_locked(&parent, name, std::process::id()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_pid_marker_locked(&parent, name, std::process::id())
        }
        Err(error) => Err(error.into()),
    }
}

fn write_pid_marker_locked(parent: &Path, name: &str, pid: u32) -> anyhow::Result<()> {
    assert_safe_basename(name)?;
    let temporary = parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| anyhow::anyhow!("system clock before Unix epoch: {error}"))?
            .as_nanos()
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(pid.to_string().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, parent.join(name))?;
        #[cfg(unix)]
        OpenOptions::new().read(true).open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Read a pid previously stored in the ownership marker.
#[must_use]
pub fn read_owned_pid(state_dir: &Path, isolation_id: &str, generation: u64) -> Option<u32> {
    assert_safe_id(isolation_id).ok()?;
    read_owned_pid_by_name(state_dir, &format!("{isolation_id}.{generation}"))
}

/// Read the PID that temporarily owns an outbox publication when the normal
/// job marker is absent. This marker closes the publication-to-intent crash
/// window without changing the normal ownership marker's meaning.
#[must_use]
pub(crate) fn read_outbox_publication_pid(
    state_dir: &Path,
    job_id: &str,
    generation: u64,
) -> Option<u32> {
    assert_safe_id(job_id).ok()?;
    read_owned_pid_by_name(state_dir, &format!(".outbox.{job_id}.{generation}"))
}

fn read_owned_pid_by_name(state_dir: &Path, name: &str) -> Option<u32> {
    #[cfg(unix)]
    let bytes = read_owned_pid_unix(state_dir, name).ok()?;
    #[cfg(not(unix))]
    let bytes = std::fs::read_to_string(state_dir.join("owned").join(name)).ok()?;
    bytes.trim().parse().ok()
}

#[cfg(unix)]
fn read_owned_pid_unix(state_dir: &Path, name: &str) -> anyhow::Result<String> {
    let owned_dir = rustix::fs::openat(
        rustix::fs::CWD,
        state_dir.join("owned"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let fd = rustix::fs::openat(
        &owned_dir,
        Path::new(&name),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let stat = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_size < 0
        || stat.st_size > 64
    {
        anyhow::bail!("ownership marker is not a bounded regular file: {name}");
    }
    let mut bytes = Vec::new();
    let file: std::fs::File = fd.into();
    file.take(65).read_to_end(&mut bytes)?;
    if bytes.len() > 64 {
        anyhow::bail!("ownership marker exceeds 64 bytes: {name}");
    }
    Ok(String::from_utf8(bytes)?)
}

/// Atomically publish the completion payload before transport or journal intent.
///
/// # Errors
/// Invalid id or filesystem failures.
pub fn write_outbox(
    state_dir: &Path,
    job_id: &str,
    generation: u64,
    payload: &[u8],
) -> anyhow::Result<PathBuf> {
    assert_safe_id(job_id)?;
    if payload.len() > MAX_COMPLETION_PAYLOAD_BYTES {
        anyhow::bail!(
            "completion outbox payload exceeds {} bytes",
            MAX_COMPLETION_PAYLOAD_BYTES
        );
    }
    // Observe ownership and publish while holding one shared namespace lock.
    // The completion caller normally already owns this marker; publication
    // must not race its cooperative marker removal.
    let name = outbox_name(job_id, generation);
    let path = outbox_path(state_dir, job_id, generation);
    let lock = lock_outbox_shared(state_dir)?;
    ensure_outbox_publication_owner_locked(state_dir, job_id, generation)?;
    #[cfg(unix)]
    {
        write_outbox_unix(&lock.parent, &name, payload)?;
    }
    #[cfg(not(unix))]
    {
        write_outbox_portable(&lock.parent, &name, payload)?;
    }
    Ok(path)
}

/// Read a durable completion payload with exact-path and size validation.
///
/// # Errors
/// Invalid id, missing/non-regular/symlink path, filesystem failures, or an
/// oversized payload.
pub fn read_outbox(state_dir: &Path, job_id: &str, generation: u64) -> anyhow::Result<Vec<u8>> {
    assert_safe_id(job_id)?;
    let lock = lock_outbox_shared(state_dir)?;
    let name = outbox_name(job_id, generation);
    #[cfg(unix)]
    return read_outbox_unix(&lock.parent, &name, job_id, generation);
    #[cfg(not(unix))]
    read_outbox_portable(&lock.parent, &name, job_id, generation)
}

/// Delete one acknowledged completion payload and durably publish the
/// directory update. Missing is already-clean; every other invalid path is a
/// hard error so cleanup cannot silently follow an attacker-controlled path.
///
/// # Errors
/// Invalid id, symlink/non-regular path, or filesystem failures.
pub fn remove_outbox(state_dir: &Path, job_id: &str, generation: u64) -> anyhow::Result<()> {
    assert_safe_id(job_id)?;
    let outbox = state_dir.join("outbox");
    let metadata = match std::fs::symlink_metadata(&outbox) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "completion outbox parent must not be a symlink: {}",
                outbox.display()
            )
        }
        Ok(metadata) if !metadata.is_dir() => {
            anyhow::bail!(
                "completion outbox parent must be a directory: {}",
                outbox.display()
            )
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    let expected_identity = outbox_directory_identity(&metadata);
    let lock = lock_outbox(state_dir)?;
    #[cfg(unix)]
    if outbox_parent_identity(&lock.parent)? != expected_identity {
        anyhow::bail!("completion outbox directory changed before removal");
    }
    let name = outbox_name(job_id, generation);
    remove_outbox_publication_owner_locked(&lock, job_id, generation)?;
    #[cfg(unix)]
    return remove_outbox_unix(&lock.parent, &name, job_id, generation);
    #[cfg(not(unix))]
    remove_outbox_portable(&lock.parent, &name, job_id, generation)
}

/// Remove one temporary completion outbox entry by its exact basename.
#[cfg(test)]
pub(crate) fn remove_temporary_outbox(state_dir: &Path, name: &str) -> anyhow::Result<()> {
    let lock = lock_outbox(state_dir)?;
    remove_outbox_entries_locked(&lock, &[name])
}

pub(crate) fn remove_outbox_entries_locked(
    lock: &OutboxLock,
    names: &[&str],
) -> anyhow::Result<()> {
    for name in names {
        assert_safe_basename(name)?;
    }
    if names.is_empty() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        let mut removed = false;
        for name in names {
            if let Some((job_id, generation)) = outbox_job_generation(name) {
                remove_outbox_publication_owner_locked(lock, job_id, generation)?;
            }
            removed |= remove_outbox_entry_unix(&lock.parent, name)?;
        }
        if removed {
            rustix::fs::fsync(&lock.parent).map_err(std::io::Error::from)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        for name in names {
            if let Some((job_id, generation)) = outbox_job_generation(name) {
                remove_outbox_publication_owner_locked(lock, job_id, generation)?;
            }
            remove_outbox_entry_portable(&lock.parent, name)?;
        }
        Ok(())
    }
}

fn remove_outbox_publication_owner_locked(
    lock: &OutboxLock,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    assert_safe_id(job_id)?;
    let path = outbox_publication_owner_path(&lock.state_path, job_id, generation);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "outbox publication owner must not be a symlink: {}",
                path.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!(
                "outbox publication owner must be a regular file: {}",
                path.display()
            )
        }
        Ok(_) => {
            std::fs::remove_file(&path)?;
            #[cfg(unix)]
            OpenOptions::new()
                .read(true)
                .open(lock.state_path.join("owned"))?
                .sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn assert_safe_basename(name: &str) -> anyhow::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || Path::new(name).components().count() != 1
    {
        anyhow::bail!("completion outbox temporary name must be a single path component");
    }
    Ok(())
}

fn outbox_name(job_id: &str, generation: u64) -> String {
    format!("{job_id}.{generation}")
}

fn outbox_job_generation(name: &str) -> Option<(&str, u64)> {
    let name = name.strip_prefix('.').unwrap_or(name);
    let name = if let Some((name, suffix)) = name.split_once(".tmp-") {
        if suffix.is_empty() {
            return None;
        }
        name
    } else {
        name
    };
    let (job_id, generation) = name.rsplit_once('.')?;
    assert_safe_id(job_id).ok()?;
    Some((job_id, generation.parse().ok()?))
}

#[cfg(unix)]
fn outbox_directory_identity(metadata: &std::fs::Metadata) -> OutboxDirectoryIdentity {
    OutboxDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn outbox_directory_identity(_metadata: &std::fs::Metadata) -> OutboxDirectoryIdentity {
    OutboxDirectoryIdentity
}

fn temporary_outbox_name(job_id: &str, generation: u64) -> String {
    let serial = NEXT_OUTBOX_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    format!(".{job_id}.{generation}.tmp-{}-{serial}", std::process::id())
}

#[cfg(not(unix))]
fn ensure_outbox_parent(state_dir: &Path) -> anyhow::Result<PathBuf> {
    let parent = state_dir.join("outbox");
    match std::fs::symlink_metadata(&parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "completion outbox parent must not be a symlink: {}",
                parent.display()
            )
        }
        Ok(metadata) if !metadata.is_dir() => {
            anyhow::bail!(
                "completion outbox parent must be a directory: {}",
                parent.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&parent)?;
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(&parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "completion outbox parent is not a real directory: {}",
            parent.display()
        );
    }
    Ok(parent)
}

pub(crate) fn lock_outbox(state_dir: &Path) -> anyhow::Result<OutboxLock> {
    lock_outbox_with(state_dir, LockMode::Exclusive)
}

pub(crate) fn lock_outbox_shared(state_dir: &Path) -> anyhow::Result<OutboxLock> {
    lock_outbox_with(state_dir, LockMode::Shared)
}

enum LockMode {
    Exclusive,
    Shared,
}

fn lock_outbox_with(state_dir: &Path, mode: LockMode) -> anyhow::Result<OutboxLock> {
    #[cfg(unix)]
    {
        let state = open_state_dir(state_dir)?;
        let operation = match mode {
            LockMode::Exclusive => FlockOperation::LockExclusive,
            LockMode::Shared => FlockOperation::LockShared,
        };
        rustix::fs::flock(&state, operation).map_err(std::io::Error::from)?;
        let parent = open_outbox_parent_from_state(&state)?;
        let parent = lock_outbox_parent_with_mode(parent, mode)?;
        Ok(OutboxLock {
            _state_dir: state,
            state_path: state_dir.to_owned(),
            parent,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        let parent = ensure_outbox_parent(state_dir)?;
        let lock_path = parent.join(OUTBOX_LOCK_NAME);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(lock_path)?;
        lock.lock()?;
        Ok(OutboxLock {
            parent,
            state_path: state_dir.to_owned(),
            _lock: lock,
        })
    }
}

#[cfg(unix)]
fn lock_outbox_parent_with_mode(
    parent: std::fs::File,
    mode: LockMode,
) -> anyhow::Result<std::fs::File> {
    let operation = match mode {
        LockMode::Exclusive => FlockOperation::LockExclusive,
        LockMode::Shared => FlockOperation::LockShared,
    };
    rustix::fs::flock(&parent, operation).map_err(std::io::Error::from)?;
    Ok(parent)
}

#[cfg(unix)]
fn open_state_dir(state_dir: &Path) -> anyhow::Result<std::fs::File> {
    let fd = rustix::fs::openat(
        rustix::fs::CWD,
        state_dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let stat = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        anyhow::bail!("state path is not a directory: {}", state_dir.display());
    }
    Ok(fd.into())
}

#[cfg(unix)]
fn outbox_parent_identity(parent: &std::fs::File) -> anyhow::Result<OutboxDirectoryIdentity> {
    let stat = rustix::fs::fstat(parent).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        anyhow::bail!("completion outbox parent is not a directory");
    }
    Ok(OutboxDirectoryIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino,
    })
}

#[cfg(unix)]
fn open_outbox_parent_from_state(state_dir: &std::fs::File) -> anyhow::Result<std::fs::File> {
    let name = Path::new("outbox");
    let fd = match rustix::fs::openat(
        state_dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => {
            match rustix::fs::mkdirat(state_dir, name, Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(std::io::Error::from(error).into()),
            }
            rustix::fs::openat(
                state_dir,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)?
        }
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let stat = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        anyhow::bail!("completion outbox parent is not a directory");
    }
    Ok(fd.into())
}

#[cfg(unix)]
fn write_outbox_unix(parent: &std::fs::File, name: &str, payload: &[u8]) -> anyhow::Result<()> {
    let temporary = temporary_outbox_name(name.trim_end_matches(|_| false), 0);
    let temporary = temporary.replace(".0.tmp-", ".tmp-");
    let temp_fd = rustix::fs::openat(
        parent,
        Path::new(&temporary),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(std::io::Error::from)?;
    let mut temp_file: std::fs::File = temp_fd.into();
    let result = (|| -> anyhow::Result<()> {
        temp_file.write_all(payload)?;
        temp_file.sync_all()?;
        rustix::fs::linkat(
            parent,
            Path::new(&temporary),
            parent,
            Path::new(name),
            AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
        rustix::fs::unlinkat(parent, Path::new(&temporary), AtFlags::empty())
            .map_err(std::io::Error::from)?;
        rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, Path::new(&temporary), AtFlags::empty());
    }
    result
}

#[cfg(not(unix))]
fn write_outbox_portable(parent: &Path, name: &str, payload: &[u8]) -> anyhow::Result<()> {
    let path = parent.join(name);
    let temporary = parent.join(temporary_outbox_name(name, 0));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(payload)?;
        file.sync_all()?;
        std::fs::hard_link(&temporary, &path)?;
        std::fs::remove_file(&temporary)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn read_outbox_unix(
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<Vec<u8>> {
    let path = outbox_path_from_parts(job_id, generation);
    let fd = rustix::fs::openat(
        parent,
        Path::new(name),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)
    .map_err(anyhow::Error::from)
    .map_err(|error| anyhow::anyhow!("read completion outbox {}: {error}", path.display()))?;
    let stat = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        anyhow::bail!(
            "completion outbox is not a regular file: {}",
            path.display()
        );
    }
    let size = u64::try_from(stat.st_size).map_err(|_| anyhow::anyhow!("negative outbox size"))?;
    if size > MAX_COMPLETION_PAYLOAD_BYTES as u64 {
        anyhow::bail!(
            "completion outbox payload exceeds {} bytes: {}",
            MAX_COMPLETION_PAYLOAD_BYTES,
            path.display()
        );
    }
    let file: std::fs::File = fd.into();
    read_bounded(file, size, &path)
}

#[cfg(not(unix))]
fn read_outbox_portable(
    parent: &Path,
    name: &str,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<Vec<u8>> {
    let path = parent.join(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "completion outbox must not be a symlink: {}",
                path.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!(
                "completion outbox is not a regular file: {}",
                path.display()
            )
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("completion outbox is missing: {}", path.display())
        }
        Err(error) => return Err(error.into()),
    };
    read_bounded(
        OpenOptions::new().read(true).open(&path)?,
        metadata.len(),
        &path,
    )
}

fn read_bounded(mut file: std::fs::File, size: u64, path: &Path) -> anyhow::Result<Vec<u8>> {
    if size > MAX_COMPLETION_PAYLOAD_BYTES as u64 {
        anyhow::bail!(
            "completion outbox payload exceeds {} bytes: {}",
            MAX_COMPLETION_PAYLOAD_BYTES,
            path.display()
        );
    }
    let mut payload = Vec::with_capacity(size as usize);
    Read::by_ref(&mut file)
        .take(MAX_COMPLETION_PAYLOAD_BYTES as u64 + 1)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_COMPLETION_PAYLOAD_BYTES {
        anyhow::bail!(
            "completion outbox payload exceeds {} bytes: {}",
            MAX_COMPLETION_PAYLOAD_BYTES,
            path.display()
        );
    }
    Ok(payload)
}

#[cfg(unix)]
fn remove_outbox_entry_unix(parent: &std::fs::File, name: &str) -> anyhow::Result<bool> {
    let stat = match rustix::fs::statat(parent, Path::new(name), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(false),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        anyhow::bail!("completion outbox entry is not a regular file: {name}");
    }
    match rustix::fs::unlinkat(parent, Path::new(name), AtFlags::empty()) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

#[cfg(not(unix))]
fn remove_outbox_entry_portable(parent: &Path, name: &str) -> anyhow::Result<()> {
    let path = parent.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "completion outbox temporary entry must not be a symlink: {}",
                path.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!(
                "completion outbox temporary entry is not a regular file: {}",
                path.display()
            )
        }
        Ok(_) => std::fs::remove_file(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(unix)]
fn remove_outbox_unix(
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    let path = outbox_path_from_parts(job_id, generation);
    let fd = match rustix::fs::openat(
        parent,
        Path::new(name),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(()),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let stat = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        anyhow::bail!(
            "completion outbox is not a regular file: {}",
            path.display()
        );
    }
    rustix::fs::unlinkat(parent, Path::new(name), AtFlags::empty())
        .map_err(std::io::Error::from)?;
    rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(not(unix))]
fn remove_outbox_portable(
    parent: &Path,
    name: &str,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    let path = parent.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!(
                "completion outbox must not be a symlink: {}",
                path.display()
            )
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!(
                "completion outbox is not a regular file: {}",
                path.display()
            )
        }
        Ok(_) => std::fs::remove_file(&path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let _ = (job_id, generation);
    Ok(())
}

fn outbox_path_from_parts(job_id: &str, generation: u64) -> PathBuf {
    PathBuf::from(format!("outbox/{}", outbox_name(job_id, generation)))
}

/// Delete only the ownership marker, then that job directory.
///
/// # Errors
/// Invalid isolation id or filesystem failures.
pub fn remove_owned(state_dir: &Path, isolation_id: &str, generation: u64) -> anyhow::Result<()> {
    assert_safe_id(isolation_id)?;
    let _lock = lock_outbox(state_dir)?;
    remove_outbox_publication_owner_locked(&_lock, isolation_id, generation)?;
    let owned = owned_path(state_dir, isolation_id, generation);
    let job = job_dir(state_dir, isolation_id);
    if owned.is_dir() {
        std::fs::remove_dir_all(&owned)?;
    } else if owned.is_file() {
        std::fs::remove_file(&owned)?;
    }
    if job.exists() && !owned_generation_exists(state_dir, isolation_id) {
        std::fs::remove_dir_all(&job)?;
    }
    Ok(())
}

/// Isolation / job / assignment ids must be a single path component.
///
/// # Errors
/// Empty, `..`, or separator characters.
pub(crate) fn assert_safe_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
    {
        anyhow::bail!("isolation id must be a single path component");
    }
    Ok(())
}

fn owned_generation_exists(state_dir: &Path, isolation_id: &str) -> bool {
    let prefix = format!("{isolation_id}.");
    std::fs::read_dir(state_dir.join("owned"))
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-cleanup-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn cleanup_removes_only_the_named_generation() {
        let dir = tmp("exact");
        claim_owned(&dir, "job-1", 1).unwrap();
        write_owned_pid(&dir, "job-2", 1, 22).unwrap();
        std::fs::write(job_dir(&dir, "job-1").join("work"), b"a").unwrap();
        std::fs::write(job_dir(&dir, "job-2").join("work"), b"b").unwrap();
        remove_owned(&dir, "job-1", 1).unwrap();
        assert!(!owned_path(&dir, "job-1", 1).exists());
        assert!(!job_dir(&dir, "job-1").exists());
        assert!(owned_path(&dir, "job-2", 1).exists());
        assert!(job_dir(&dir, "job-2").join("work").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn claim_owned_does_not_publish_empty_marker() {
        let dir = tmp("claim");
        let owned = claim_owned(&dir, "job-1", 1).unwrap();
        assert!(!owned.exists());
        assert!(job_dir(&dir, "job-1").is_dir());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn write_owned_pid_publishes_exact_contents() {
        let dir = tmp("pid");
        write_owned_pid(&dir, "job-1", 1, 42).unwrap();
        assert_eq!(
            std::fs::read_to_string(owned_path(&dir, "job-1", 1)).unwrap(),
            "42"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn path_separator_id_is_rejected() {
        let dir = tmp("slash");
        assert!(claim_owned(&dir, "../etc", 1).is_err());
        assert!(claim_owned(&dir, ".", 1).is_err());
        assert!(claim_owned(&dir, "..", 1).is_err());
        assert!(read_owned_pid(&dir, "../etc", 1).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn older_generation_does_not_remove_newer_job_directory() {
        let dir = tmp("generation");
        write_owned_pid(&dir, "job-1", 1, 11).unwrap();
        write_owned_pid(&dir, "job-1", 2, 22).unwrap();
        std::fs::write(job_dir(&dir, "job-1").join("work"), b"new").unwrap();
        remove_owned(&dir, "job-1", 1).unwrap();
        assert!(owned_path(&dir, "job-1", 2).exists());
        assert!(job_dir(&dir, "job-1").join("work").exists());
        remove_owned(&dir, "job-1", 2).unwrap();
        assert!(!job_dir(&dir, "job-1").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn outbox_read_and_remove_are_exact_and_bounded() {
        let dir = tmp("outbox");
        let path = write_outbox(&dir, "job-1", 1, b"payload").unwrap();
        assert_eq!(read_outbox(&dir, "job-1", 1).unwrap(), b"payload");
        remove_outbox(&dir, "job-1", 1).unwrap();
        assert!(!path.exists());
        assert!(read_outbox(&dir, "job-1", 1).is_err());
        assert!(
            write_outbox(&dir, "job-1", 1, &vec![0; MAX_COMPLETION_PAYLOAD_BYTES + 1]).is_err()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn read_bounded_rejects_oversized_declared_size_before_allocation() {
        let dir = tmp("outbox-declared-size");
        let path = dir.join("payload");
        std::fs::write(&path, b"payload").unwrap();
        let file = OpenOptions::new().read(true).open(&path).unwrap();
        let error = read_bounded(file, MAX_COMPLETION_PAYLOAD_BYTES as u64 + 1, &path).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "completion outbox payload exceeds {} bytes: {}",
                MAX_COMPLETION_PAYLOAD_BYTES,
                path.display()
            )
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn concurrent_outbox_writers_publish_one_immutable_payload() {
        let dir = tmp("outbox-concurrent");
        let left = dir.clone();
        let right = dir.clone();
        let first = std::thread::spawn(move || write_outbox(&left, "job-1", 1, b"left"));
        let second = std::thread::spawn(move || write_outbox(&right, "job-1", 1, b"right"));
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert_eq!(first.is_ok() as u8 + second.is_ok() as u8, 1);
        let payload = read_outbox(&dir, "job-1", 1).unwrap();
        assert!(payload == b"left" || payload == b"right");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn outbox_symlink_is_rejected_for_read_and_remove() {
        let dir = tmp("outbox-symlink");
        let target = dir.join("target");
        std::fs::write(&target, b"secret").unwrap();
        std::fs::create_dir_all(dir.join("outbox")).unwrap();
        std::os::unix::fs::symlink(&target, outbox_path(&dir, "job-1", 1)).unwrap();
        assert!(read_outbox(&dir, "job-1", 1).is_err());
        assert!(remove_outbox(&dir, "job-1", 1).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"secret");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn outbox_parent_symlink_is_rejected_for_all_operations() {
        let dir = tmp("outbox-parent-symlink");
        let target = dir.join("target");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("outbox")).unwrap();

        assert!(write_outbox(&dir, "job-1", 1, b"payload").is_err());
        assert!(read_outbox(&dir, "job-1", 1).is_err());
        assert!(remove_outbox(&dir, "job-1", 1).is_err());
        assert!(std::fs::read_dir(target).unwrap().next().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn temporary_outbox_removal_rejects_symlink_and_removes_exact_file() {
        let dir = tmp("outbox-temporary");
        let target = dir.join("target");
        std::fs::write(&target, b"secret").unwrap();
        let outbox = dir.join("outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        let temporary = ".job-1.1.tmp-test";
        std::os::unix::fs::symlink(&target, outbox.join(temporary)).unwrap();
        assert!(remove_temporary_outbox(&dir, temporary).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"secret");
        std::fs::remove_file(outbox.join(temporary)).unwrap();

        std::fs::write(outbox.join(temporary), b"temporary").unwrap();
        std::fs::write(outbox.join("sibling"), b"keep").unwrap();
        let publication_marker = outbox_publication_owner_path(&dir, "job-1", 1);
        std::fs::create_dir_all(publication_marker.parent().unwrap()).unwrap();
        std::fs::write(&publication_marker, b"123").unwrap();
        remove_temporary_outbox(&dir, temporary).unwrap();
        assert!(!outbox.join(temporary).exists());
        assert!(!publication_marker.exists());
        assert_eq!(std::fs::read(outbox.join("sibling")).unwrap(), b"keep");
        std::fs::remove_dir_all(dir).ok();
    }
}
