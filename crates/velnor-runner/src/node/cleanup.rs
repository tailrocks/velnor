//! Exact-path job ownership and completion I/O.
//!
//! Cleanup may delete only `{state}/owned/{id}.{gen}` and `{state}/jobs/{id}`.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// Completion responses are small protocol records, not artifact storage.
/// Bound both durable writes and recovery reads so a hostile or corrupted
/// outbox cannot consume unbounded disk or heap.
pub const MAX_COMPLETION_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_OWNED_PID_BYTES: usize = 64;

const QUARANTINE_ENTRY_NAME: &str = "outbox-entry";
const QUARANTINE_PREFIX: &str = ".outbox-remove-";
const OUTBOX_RECOVERY_LOCK_NAME: &str = ".outbox-recovery.lock";

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
    let owned = claim_owned(state_dir, isolation_id, generation)?;
    let parent = owned
        .parent()
        .ok_or_else(|| anyhow::anyhow!("ownership marker has no parent"))?;
    let temporary = parent.join(format!(
        ".{isolation_id}.{generation}.tmp-{}-{}",
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
        std::fs::rename(&temporary, &owned)?;
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
    let path = owned_path(state_dir, isolation_id, generation);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).ok()?;
    let mut contents = String::new();
    file.take(MAX_OWNED_PID_BYTES as u64 + 1)
        .read_to_string(&mut contents)
        .ok()?;
    if contents.len() > MAX_OWNED_PID_BYTES {
        return None;
    }
    contents.trim().parse().ok()
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
    let name = outbox_name(job_id, generation);
    let path = outbox_path(state_dir, job_id, generation);
    #[cfg(unix)]
    write_outbox_unix(&open_outbox_parent(state_dir)?, &name, payload)?;
    #[cfg(not(unix))]
    write_outbox_portable(&ensure_outbox_parent(state_dir)?, &name, payload)?;
    Ok(path)
}

/// Read a durable completion payload with exact-path and size validation.
///
/// # Errors
/// Invalid id, missing/non-regular/symlink path, filesystem failures, or an
/// oversized payload.
pub fn read_outbox(state_dir: &Path, job_id: &str, generation: u64) -> anyhow::Result<Vec<u8>> {
    assert_safe_id(job_id)?;
    let parent = open_outbox_parent(state_dir)?;
    let name = outbox_name(job_id, generation);
    #[cfg(unix)]
    return read_outbox_unix(&parent, &name, job_id, generation);
    #[cfg(not(unix))]
    read_outbox_portable(&parent, &name, job_id, generation)
}

/// Delete one acknowledged completion payload and durably publish the
/// directory update. Missing is already-clean; every other invalid path is a
/// hard error so cleanup cannot silently follow an attacker-controlled path.
///
/// # Errors
/// Invalid id, symlink/non-regular path, or filesystem failures.
pub fn remove_outbox(state_dir: &Path, job_id: &str, generation: u64) -> anyhow::Result<()> {
    assert_safe_id(job_id)?;
    let parent = match open_outbox_parent(state_dir) {
        Ok(parent) => parent,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let name = outbox_name(job_id, generation);
    #[cfg(unix)]
    return remove_outbox_unix(state_dir, &parent, &name, job_id, generation);
    #[cfg(not(unix))]
    remove_outbox_portable(&parent, &name, job_id, generation)
}

fn outbox_name(job_id: &str, generation: u64) -> String {
    format!("{job_id}.{generation}")
}

fn temporary_outbox_name(name: &str) -> String {
    let serial = NEXT_OUTBOX_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    format!("..{name}.tmp-{}-{serial}", std::process::id())
}

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

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(unix)]
fn open_outbox_parent(state_dir: &Path) -> anyhow::Result<std::fs::File> {
    let parent = ensure_outbox_parent(state_dir)?;
    let fd = rustix::fs::openat(
        rustix::fs::CWD,
        &parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let stat = rustix::fs::fstat(&fd).map_err(std::io::Error::from)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        anyhow::bail!(
            "completion outbox parent is not a directory: {}",
            parent.display()
        );
    }
    Ok(fd.into())
}

#[cfg(not(unix))]
fn open_outbox_parent(state_dir: &Path) -> anyhow::Result<PathBuf> {
    ensure_outbox_parent(state_dir)
}

#[cfg(unix)]
struct OutboxRecoveryLock {
    _file: std::fs::File,
}

#[cfg(unix)]
fn acquire_outbox_recovery_lock(
    state_dir: &Path,
    nonblocking: bool,
) -> anyhow::Result<Option<OutboxRecoveryLock>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600);
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(state_dir.join(OUTBOX_RECOVERY_LOCK_NAME))
        .context("open completion outbox recovery lock")?;
    if !file.metadata()?.is_file() {
        anyhow::bail!("completion outbox recovery lock is not a regular file");
    }
    let operation = if nonblocking {
        rustix::fs::FlockOperation::NonBlockingLockExclusive
    } else {
        rustix::fs::FlockOperation::LockExclusive
    };
    match rustix::fs::flock(&file, operation) {
        Ok(()) => Ok(Some(OutboxRecoveryLock { _file: file })),
        Err(rustix::io::Errno::WOULDBLOCK) if nonblocking => Ok(None),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

#[cfg(unix)]
fn write_outbox_unix(parent: &std::fs::File, name: &str, payload: &[u8]) -> anyhow::Result<()> {
    let temporary = temporary_outbox_name(name);
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
    let temporary = parent.join(temporary_outbox_name(name));
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
    let capacity = usize::try_from(size).context("completion outbox size does not fit usize")?;
    let mut payload = Vec::with_capacity(capacity);
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
fn remove_outbox_unix(
    state_dir: &Path,
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    let path = outbox_path_from_parts(job_id, generation);
    let _recovery_lock = acquire_outbox_recovery_lock(state_dir, false)?;
    let (quarantine_name, quarantine) = create_outbox_quarantine(parent)?;
    let moved = match rustix::fs::renameat(
        parent,
        Path::new(name),
        &quarantine,
        Path::new(QUARANTINE_ENTRY_NAME),
    ) {
        Ok(()) => true,
        Err(error) if error == rustix::io::Errno::NOENT => false,
        Err(error) => {
            remove_outbox_quarantine(parent, &quarantine_name)?;
            return Err(std::io::Error::from(error).into());
        }
    };
    if !moved {
        drop(quarantine);
        remove_outbox_quarantine(parent, &quarantine_name)?;
        return Ok(());
    }

    let fd = match rustix::fs::openat(
        &quarantine,
        Path::new(QUARANTINE_ENTRY_NAME),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(error) => {
            restore_outbox_entry(parent, quarantine, &quarantine_name, name)?;
            return Err(std::io::Error::from(error).into());
        }
    };
    let stat = match rustix::fs::fstat(&fd).map_err(std::io::Error::from) {
        Ok(stat) => stat,
        Err(error) => {
            drop(fd);
            restore_outbox_entry(parent, quarantine, &quarantine_name, name)?;
            return Err(error.into());
        }
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        drop(fd);
        restore_outbox_entry(parent, quarantine, &quarantine_name, name)?;
        anyhow::bail!(
            "completion outbox is not a regular file: {}",
            path.display()
        );
    }
    drop(fd);
    let unlink_error = rustix::fs::unlinkat(
        &quarantine,
        Path::new(QUARANTINE_ENTRY_NAME),
        AtFlags::empty(),
    )
    .err()
    .map(std::io::Error::from);
    if let Some(error) = unlink_error {
        let error = anyhow::anyhow!("remove completion outbox {}: {error}", path.display());
        if let Err(restore_error) = restore_outbox_entry(parent, quarantine, &quarantine_name, name)
        {
            return Err(anyhow::anyhow!("{error}; restore failed: {restore_error}"));
        }
        return Err(error);
    }
    rustix::fs::fsync(&quarantine).map_err(std::io::Error::from)?;
    drop(quarantine);
    remove_outbox_quarantine(parent, &quarantine_name)?;
    rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(unix)]
fn create_outbox_quarantine(parent: &std::fs::File) -> anyhow::Result<(String, std::fs::File)> {
    for _ in 0..8 {
        let name = format!(
            ".outbox-remove-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        );
        match rustix::fs::mkdirat(parent, &name, Mode::from_raw_mode(0o700)) {
            Ok(()) => {
                let fd = rustix::fs::openat(
                    parent,
                    &name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|error| {
                    let _ = rustix::fs::unlinkat(parent, Path::new(&name), AtFlags::REMOVEDIR);
                    std::io::Error::from(error)
                })?;
                return Ok((name, fd.into()));
            }
            Err(error) if error == rustix::io::Errno::EXIST => continue,
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    anyhow::bail!("could not allocate a unique completion outbox quarantine")
}

#[cfg(unix)]
fn remove_outbox_quarantine(parent: &std::fs::File, name: &str) -> anyhow::Result<()> {
    rustix::fs::unlinkat(parent, Path::new(name), AtFlags::REMOVEDIR)
        .map_err(std::io::Error::from)?;
    Ok(())
}

/// Parse the owner pid from a quarantine directory name.
#[cfg(unix)]
pub(crate) fn outbox_quarantine_pid(name: &str) -> anyhow::Result<Option<u32>> {
    let Some(rest) = name.strip_prefix(QUARANTINE_PREFIX) else {
        return Ok(None);
    };
    let (pid, nonce) = rest
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("malformed completion outbox quarantine: {name}"))?;
    if nonce.len() != 32 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("malformed completion outbox quarantine: {name}");
    }
    let pid = pid.parse::<u32>().map_err(|error| {
        anyhow::anyhow!("malformed completion outbox quarantine {name}: {error}")
    })?;
    if pid == 0 {
        anyhow::bail!("malformed completion outbox quarantine pid: {name}");
    }
    Ok(Some(pid))
}

/// Remove a crash-left quarantine directory using only directory fds.
///
/// The caller must handle a `false` result as an active removal operation.
/// Unexpected children and non-regular payloads fail closed.
#[cfg(unix)]
pub(crate) fn remove_stale_outbox_quarantine(state_dir: &Path, name: &str) -> anyhow::Result<bool> {
    if outbox_quarantine_pid(name)?.is_none() {
        anyhow::bail!("not a completion outbox quarantine: {name}");
    }
    let parent = match open_outbox_parent(state_dir) {
        Ok(parent) => parent,
        Err(error) if is_not_found(&error) => return Ok(true),
        Err(error) => return Err(error),
    };
    let Some(_recovery_lock) = acquire_outbox_recovery_lock(state_dir, true)? else {
        return Ok(false);
    };
    let quarantine = match rustix::fs::openat(
        &parent,
        Path::new(name),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(true),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    let entries = rustix::fs::Dir::read_from(&quarantine)
        .map_err(std::io::Error::from)
        .context("read stale completion outbox quarantine")?;
    let mut has_payload = false;
    for entry in entries {
        let entry = entry.map_err(std::io::Error::from)?;
        let entry_name = entry.file_name().to_bytes();
        if entry_name == b"." || entry_name == b".." {
            continue;
        }
        if entry_name != QUARANTINE_ENTRY_NAME.as_bytes() {
            anyhow::bail!("unexpected completion outbox quarantine entry in {}", name);
        }
        let payload = rustix::fs::openat(
            &quarantine,
            Path::new(QUARANTINE_ENTRY_NAME),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let stat = rustix::fs::fstat(&payload).map_err(std::io::Error::from)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            anyhow::bail!("stale completion outbox quarantine payload is not a regular file");
        }
        drop(payload);
        has_payload = true;
    }
    if has_payload {
        rustix::fs::unlinkat(
            &quarantine,
            Path::new(QUARANTINE_ENTRY_NAME),
            AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
    }
    rustix::fs::fsync(&quarantine).map_err(std::io::Error::from)?;
    drop(quarantine);
    remove_outbox_quarantine(&parent, name)?;
    rustix::fs::fsync(&parent).map_err(std::io::Error::from)?;
    Ok(true)
}

#[cfg(unix)]
fn restore_outbox_entry(
    parent: &std::fs::File,
    quarantine: std::fs::File,
    quarantine_name: &str,
    name: &str,
) -> anyhow::Result<()> {
    rustix::fs::renameat(
        &quarantine,
        Path::new(QUARANTINE_ENTRY_NAME),
        parent,
        Path::new(name),
    )
    .map_err(std::io::Error::from)?;
    drop(quarantine);
    remove_outbox_quarantine(parent, quarantine_name)?;
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
/// Empty, dot-prefixed, traversal, or separator characters.
pub(crate) fn assert_safe_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty()
        || id.starts_with('.')
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
    fn oversized_owned_pid_is_rejected_without_unbounded_read() {
        let dir = tmp("pid-bounded");
        std::fs::create_dir_all(dir.join("owned")).unwrap();
        std::fs::write(
            owned_path(&dir, "job-1", 1),
            vec![b'7'; MAX_OWNED_PID_BYTES + 1],
        )
        .unwrap();
        assert_eq!(read_owned_pid(&dir, "job-1", 1), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn path_separator_id_is_rejected() {
        let dir = tmp("slash");
        assert!(claim_owned(&dir, "../etc", 1).is_err());
        assert!(claim_owned(&dir, ".job", 1).is_err());
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
    fn oversized_outbox_read_is_rejected() {
        let dir = tmp("outbox-read-bounded");
        std::fs::create_dir_all(dir.join("outbox")).unwrap();
        let path = outbox_path(&dir, "job-1", 1);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len((MAX_COMPLETION_PAYLOAD_BYTES + 1) as u64)
            .unwrap();
        assert!(read_outbox(&dir, "job-1", 1)
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
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
        assert!(std::fs::symlink_metadata(outbox_path(&dir, "job-1", 1)).is_ok());
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

    #[test]
    fn temporary_outbox_name_preserves_full_outbox_name() {
        let name = temporary_outbox_name("job.0.tmp-user.7");
        let stem = name
            .strip_prefix("..")
            .unwrap()
            .rsplit_once(".tmp-")
            .unwrap()
            .0;
        assert_eq!(stem, "job.0.tmp-user.7");
    }

    #[cfg(unix)]
    #[test]
    fn stale_outbox_quarantine_is_removed_by_exact_directory_handle() {
        let dir = tmp("outbox-quarantine");
        let name = format!("{QUARANTINE_PREFIX}999999-{}", "a".repeat(32));
        let quarantine = dir.join("outbox").join(&name);
        std::fs::create_dir_all(&quarantine).unwrap();
        std::fs::write(quarantine.join(QUARANTINE_ENTRY_NAME), b"payload").unwrap();

        assert_eq!(outbox_quarantine_pid(&name).unwrap(), Some(999_999));
        remove_stale_outbox_quarantine(&dir, &name).unwrap();
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn empty_stale_outbox_quarantine_is_removed() {
        let dir = tmp("outbox-quarantine-empty");
        let name = format!("{QUARANTINE_PREFIX}999999-{}", "d".repeat(32));
        let quarantine = dir.join("outbox").join(&name);
        std::fs::create_dir_all(&quarantine).unwrap();

        assert!(remove_stale_outbox_quarantine(&dir, &name).unwrap());
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn stale_outbox_quarantine_defers_while_removal_lock_is_held() {
        let dir = tmp("outbox-quarantine-lock");
        let name = format!("{QUARANTINE_PREFIX}999999-{}", "b".repeat(32));
        let quarantine = dir.join("outbox").join(&name);
        std::fs::create_dir_all(&quarantine).unwrap();
        std::fs::write(quarantine.join(QUARANTINE_ENTRY_NAME), b"payload").unwrap();
        let lock = acquire_outbox_recovery_lock(&dir, false).unwrap();
        assert!(lock.is_some());
        assert!(!remove_stale_outbox_quarantine(&dir, &name).unwrap());
        assert!(quarantine.exists());
        drop(lock);
        assert!(remove_stale_outbox_quarantine(&dir, &name).unwrap());
        assert!(!quarantine.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn stale_outbox_quarantine_preflights_before_mutating() {
        let dir = tmp("outbox-quarantine-preflight");
        let name = format!("{QUARANTINE_PREFIX}999999-{}", "c".repeat(32));
        let quarantine = dir.join("outbox").join(&name);
        std::fs::create_dir_all(&quarantine).unwrap();
        std::fs::write(quarantine.join(QUARANTINE_ENTRY_NAME), b"payload").unwrap();
        std::fs::write(quarantine.join("unexpected"), b"must survive").unwrap();

        assert!(remove_stale_outbox_quarantine(&dir, &name).is_err());
        assert!(quarantine.join(QUARANTINE_ENTRY_NAME).exists());
        assert!(quarantine.join("unexpected").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
