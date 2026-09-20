//! Exact-path job ownership and completion I/O.
//!
//! Cleanup may delete only `{state}/owned/{id}.{gen}` and `{state}/jobs/{id}`.

#[cfg(unix)]
use std::ffi::{OsStr, OsString};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;
use serde::{Deserialize, Serialize};
#[cfg(all(unix, not(target_os = "linux")))]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;
use velnor_model::{Generation, SlotId};

#[cfg(unix)]
use rustix::fs::{AtFlags, FileType, Mode, OFlags};

/// Completion responses are small protocol records, not artifact storage.
/// Bound both durable writes and recovery reads so a hostile or corrupted
/// outbox cannot consume unbounded disk or heap.
pub const MAX_COMPLETION_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
const MAX_OWNED_PID_BYTES: usize = 512;
const MAX_LAUNCH_PROCESS_ARGS_BYTES: usize = 1024 * 1024;
const MAX_LAUNCH_PROCESS_INVENTORY_BYTES: usize = 64 * 1024 * 1024;

const QUARANTINE_ENTRY_NAME: &str = "outbox-entry";
const QUARANTINE_PREFIX: &str = ".outbox-remove-";
const OUTBOX_RECOVERY_LOCK_NAME: &str = ".outbox-recovery.lock";
const OWNED_INITIALIZATION_MARKER_NAME: &str = ".owned-initialized";
#[cfg(unix)]
const OWNED_PID_LOCK_NAME: &str = ".owned-pid.lock";

static NEXT_OUTBOX_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_OWNED_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct JobDirectoryIdentity {
    jobs_device: u64,
    jobs_inode: u64,
    job_device: u64,
    job_inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct OwnedPidRecord {
    pid: u32,
    #[serde(default)]
    process_identity: Option<String>,
    #[serde(default)]
    launch_token: Option<String>,
    #[serde(default)]
    job_directory: Option<JobDirectoryIdentity>,
    #[serde(default)]
    cleanup_started: bool,
    #[serde(default)]
    cleanup_complete: bool,
}

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
    #[cfg(unix)]
    {
        let state = open_state_directory(state_dir)?;
        open_owned_parent_at(&state)?;
        ensure_job_directory_at(&state, isolation_id)?;
    }
    #[cfg(not(unix))]
    {
        require_owned_parent(state_dir)?;
        ensure_job_directory(state_dir, isolation_id)?;
    }
    let owned = owned_path(state_dir, isolation_id, generation);
    Ok(owned)
}

/// Record the worker pid inside the ownership marker.
///
/// # Errors
/// Invalid isolation id or write failures.
#[cfg(test)]
pub fn write_owned_pid(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    pid: u32,
) -> anyhow::Result<()> {
    write_owned_pid_for_test(state_dir, isolation_id, generation, pid, None)
}

#[cfg(test)]
pub fn write_owned_pid_with_launch_token(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    pid: u32,
    launch_token: &str,
) -> anyhow::Result<()> {
    write_owned_pid_for_test(state_dir, isolation_id, generation, pid, Some(launch_token))
}

#[cfg(test)]
fn write_owned_pid_for_test(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    pid: u32,
    launch_token: Option<&str>,
) -> anyhow::Result<()> {
    assert_safe_id(isolation_id)?;
    let state = open_state_directory(state_dir)?;
    let parent = open_owned_parent_at(&state)?;
    let job_directory = ensure_job_directory_at(&state, isolation_id)?;
    let record = OwnedPidRecord {
        pid,
        process_identity: process_identity(pid)?.map(|identity| {
            persisted_process_identity(&identity, launch_token.unwrap_or_default())
        }),
        launch_token: launch_token.map(str::to_owned),
        job_directory,
        cleanup_started: false,
        cleanup_complete: false,
    };
    let name = owned_name(isolation_id, generation);
    let _lock = acquire_owned_pid_lock(&parent)?;
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    if let Some(existing) = read_owned_record_from_parent(&parent, &name)? {
        if existing == record {
            return Ok(());
        }
        anyhow::bail!(
            "ownership marker already exists for {isolation_id} generation {generation}; only its matching spawn intent may be replaced"
        );
    }
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    write_owned_record_exclusive(&parent, &name, &record)
}

/// Write a durable spawn intent before starting a worker process. The intent
/// occupies the generation marker, so a controller crash cannot make a live
/// but not-yet-published child look like free capacity.
#[cfg(test)]
pub fn write_owned_pid_intent(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
) -> anyhow::Result<String> {
    assert_safe_id(isolation_id)?;
    let state = open_state_directory(state_dir)?;
    let parent = open_owned_parent_at(&state)?;
    let job_directory = ensure_job_directory_at(&state, isolation_id)?;
    let (token, record) = new_owned_pid_intent(job_directory)?;
    let _lock = acquire_owned_pid_lock(&parent)?;
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    write_owned_record_exclusive(&parent, &owned_name(isolation_id, generation), &record)?;
    Ok(token)
}

/// Atomically replace an absent or proven-dead ownership marker with a new
/// durable launch intent. The directory lock spans liveness proof and marker
/// replacement, so another controller cannot publish a fresh intent between
/// those steps and have it mistaken for the dead marker.
///
/// `Ok(None)` means a validated live process already owns this generation.
/// An unpublished intent is recovered only after the host process inventory
/// proves that no process carries its exact launch token. Damaged markers or
/// unknown process liveness return an error so callers keep the generation
/// fenced.
#[cfg(test)]
pub fn replace_dead_owned_pid_with_intent(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
) -> anyhow::Result<Option<String>> {
    with_dead_owned_pid_intent(state_dir, isolation_id, generation, |token| {
        Ok(token.to_owned())
    })
}

/// Replace an absent or proven-dead marker with a durable launch intent and
/// run `publish` while still holding the ownership-directory lock. Callers
/// use this for the OS spawn itself: otherwise another controller can rotate
/// a freshly written PID-zero token during the gap between marker publication
/// and `Command::spawn`, causing both children to reject their tokens.
///
/// The callback must return only after the child process has been created.
/// The child may block while self-publishing its PID; the marker lock is
/// released as soon as the callback returns. If process creation reports an
/// error, the intent remains durable because the platform may have created a
/// child before surfacing the error.
pub fn with_dead_owned_pid_intent<T>(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    publish: impl FnOnce(&str) -> anyhow::Result<T>,
) -> anyhow::Result<Option<T>> {
    with_owned_pid_intent(state_dir, isolation_id, generation, false, publish)
}

/// Replace an existing, proven-dead ownership marker with a durable launch
/// intent and run `publish` while holding the ownership-directory lock.
/// Unlike `with_dead_owned_pid_intent`, an absent marker does not authorize a
/// launch. Recovery callers use this after restart, where marker loss cannot
/// be distinguished from a live owner whose evidence disappeared.
pub fn with_existing_dead_owned_pid_intent<T>(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    publish: impl FnOnce(&str) -> anyhow::Result<T>,
) -> anyhow::Result<Option<T>> {
    with_owned_pid_intent(state_dir, isolation_id, generation, true, publish)
}

fn with_owned_pid_intent<T>(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    require_existing_dead_marker: bool,
    publish: impl FnOnce(&str) -> anyhow::Result<T>,
) -> anyhow::Result<Option<T>> {
    assert_safe_id(isolation_id)?;
    let state = open_state_directory(state_dir)?;
    let parent = open_owned_parent_at(&state)?;
    let _lock = acquire_owned_pid_lock(&parent)?;
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    let name = owned_name(isolation_id, generation);
    let existing = read_owned_record_from_parent(&parent, &name)?;
    let liveness = match existing.as_ref() {
        Some(record) => owned_pid_record_liveness(Some(record))?,
        None => OwnedPidLiveness::Absent,
    };
    if require_existing_dead_marker {
        if liveness != OwnedPidLiveness::Dead {
            return Ok(None);
        }
    } else if matches!(
        liveness,
        OwnedPidLiveness::Live | OwnedPidLiveness::UnpublishedIntentLive
    ) {
        return Ok(None);
    }

    if existing.as_ref().is_some_and(|record| {
        record.cleanup_started || record.cleanup_complete || record.job_directory.is_none()
    }) {
        anyhow::bail!(
            "ownership marker does not prove an available job directory for {isolation_id}"
        );
    }
    let job_directory = match existing.as_ref() {
        Some(record) => {
            let current = open_existing_job_directory_identity(&state, isolation_id)?;
            ensure_owned_job_directory_matches(record, &current)?;
            current
        }
        None => ensure_job_directory_at(&state, isolation_id)?,
    };
    let (token, record) = new_owned_pid_intent(job_directory)?;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    match existing {
        Some(_) => replace_owned_intent(&parent, &name, &record)?,
        None => write_owned_record_exclusive(&parent, &name, &record)?,
    }
    publish(&token).map(Some)
}

fn new_owned_pid_intent(
    job_directory: Option<JobDirectoryIdentity>,
) -> anyhow::Result<(String, OwnedPidRecord)> {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let record = OwnedPidRecord {
        pid: 0,
        process_identity: None,
        launch_token: Some(token.clone()),
        job_directory,
        cleanup_started: false,
        cleanup_complete: false,
    };
    Ok((token, record))
}

/// Search the host process inventory for a worker still carrying one exact
/// launch token. An unreadable inventory is not proof that the child is gone.
#[cfg(all(unix, not(target_os = "linux")))]
fn process_inventory_command() -> std::process::Command {
    let mut command = std::process::Command::new("ps");
    command.args(["-ww", "-axo", "pid=,command="]);
    command
}

fn process_has_launch_token(launch_token: &str) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let entries = std::fs::read_dir("/proc")?;
        let mut scanned_bytes = 0usize;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().parse::<u32>().is_err() {
                continue;
            }
            let command_line = match std::fs::File::open(entry.path().join("cmdline")) {
                Ok(file) => {
                    let mut command_line = Vec::new();
                    file.take((MAX_LAUNCH_PROCESS_ARGS_BYTES + 1) as u64)
                        .read_to_end(&mut command_line)?;
                    if command_line.len() > MAX_LAUNCH_PROCESS_ARGS_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "process argv exceeds launch-token scan limit",
                        ));
                    }
                    scanned_bytes = scanned_bytes.saturating_add(command_line.len());
                    if scanned_bytes > MAX_LAUNCH_PROCESS_INVENTORY_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "process inventory exceeds launch-token scan limit",
                        ));
                    }
                    command_line
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if argv_contains_launch_token(&command_line, launch_token) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let mut child = process_inventory_command()
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let mut inventory = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("ps stdout was not piped"))?
            .take((MAX_LAUNCH_PROCESS_INVENTORY_BYTES + 1) as u64)
            .read_to_end(&mut inventory)?;
        if inventory.len() > MAX_LAUNCH_PROCESS_INVENTORY_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process inventory exceeds launch-token scan limit",
            ));
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("ps process inventory exited with {status}"),
            ));
        }
        Ok(argv_contains_launch_token(&inventory, launch_token))
    }
    #[cfg(not(unix))]
    {
        let _ = launch_token;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process inventory is unavailable on this platform",
        ))
    }
}

fn argv_contains_launch_token(arguments: &[u8], launch_token: &str) -> bool {
    arguments
        .split(|byte| byte.is_ascii_whitespace() || *byte == 0)
        .any(|argument| {
            argument == launch_token.as_bytes()
                || argument
                    .strip_prefix(b"--launch-token=")
                    .is_some_and(|value| value == launch_token.as_bytes())
        })
}

/// Publish the child PID before it can claim or execute the job. Only the
/// child holding the exact durable launch token may replace that intent.
pub fn write_owned_pid_for_intent(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
    pid: u32,
    launch_token: &str,
) -> anyhow::Result<()> {
    assert_safe_id(isolation_id)?;
    if pid != std::process::id() {
        anyhow::bail!("job worker may only publish its own process id");
    }
    let identity = process_identity(pid)?.ok_or_else(|| {
        anyhow::anyhow!("child process {pid} disappeared before publishing ownership")
    })?;
    #[cfg(all(unix, not(target_os = "linux")))]
    if !identity.contains(launch_token) {
        anyhow::bail!("child process {pid} command line does not contain its launch token");
    }
    let identity = persisted_process_identity(&identity, launch_token);
    let name = owned_name(isolation_id, generation);
    let state = open_state_directory(state_dir)?;
    let parent = open_owned_parent_at(&state)?;
    let _lock = acquire_owned_pid_lock(&parent)?;
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    let current = read_owned_record_from_parent(&parent, &name)?.ok_or_else(|| {
        anyhow::anyhow!("spawn intent disappeared before child {pid} published ownership")
    })?;
    if current.pid != 0 || current.launch_token.as_deref() != Some(launch_token) {
        anyhow::bail!(
            "spawn intent does not match child {pid} for {isolation_id} generation {generation}"
        );
    }
    if current.cleanup_started || current.cleanup_complete {
        anyhow::bail!("job directory cleanup started before child {pid} published ownership");
    }
    let current_job_directory = open_existing_job_directory_identity(&state, isolation_id)?;
    ensure_owned_job_directory_matches(&current, &current_job_directory)?;
    let record = OwnedPidRecord {
        pid,
        process_identity: Some(identity),
        launch_token: Some(launch_token.to_owned()),
        job_directory: current.job_directory,
        cleanup_started: current.cleanup_started,
        cleanup_complete: current.cleanup_complete,
    };
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    replace_owned_intent(&parent, &name, &record)
}

/// Read a published live pid from the ownership marker. A missing or
/// proven-dead marker is `Ok(None)`; an unpublished but live launch intent has
/// no PID to return and yields `WouldBlock`. Damaged or unreadable evidence
/// remains an error. Liveness is checked against the recorded process identity
/// under the marker lock, so callers never turn PID reuse into owner proof.
pub fn read_owned_pid(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
) -> io::Result<Option<u32>> {
    assert_safe_id(isolation_id)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let state = open_state_directory(state_dir)?;
    let parent = open_owned_parent_at(&state)?;
    let _lock =
        acquire_owned_pid_lock(&parent).map_err(|error| io::Error::other(error.to_string()))?;
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    let record = read_owned_record_from_parent(&parent, &owned_name(isolation_id, generation))?;
    let liveness = owned_pid_record_liveness(record.as_ref())?;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    match liveness {
        OwnedPidLiveness::Live => Ok(record.map(|record| record.pid)),
        OwnedPidLiveness::UnpublishedIntentLive => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "ownership marker has a live unpublished launch intent",
        )),
        OwnedPidLiveness::Absent | OwnedPidLiveness::Dead => Ok(None),
    }
}

/// Result of comparing one exact ownership marker with its recorded process.
/// `Absent` means no marker exists; `Dead` means the recorded process identity
/// is gone or its PID now belongs to a different process. Errors mean the
/// evidence is unknown and recovery must retain ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnedPidLiveness {
    Absent,
    Live,
    /// The marker is still PID zero, but an exact matching launch token is
    /// present in the process inventory. The child has spawned and may publish
    /// its PID shortly, so recovery must treat this as live ownership.
    UnpublishedIntentLive,
    Dead,
}

/// Prove whether the process recorded by one ownership marker is still live.
/// The marker lock spans identity and liveness checks. Linux pins the process
/// with pidfd so PID reuse cannot turn a dead owner into a live unrelated one.
pub fn owned_pid_liveness(
    state_dir: &Path,
    isolation_id: &str,
    generation: u64,
) -> io::Result<OwnedPidLiveness> {
    assert_safe_id(isolation_id)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let state = open_state_directory(state_dir)?;
    let parent = open_owned_parent_at(&state)?;
    let _lock =
        acquire_owned_pid_lock(&parent).map_err(|error| io::Error::other(error.to_string()))?;
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    let record = read_owned_record_from_parent(&parent, &owned_name(isolation_id, generation))?;
    let Some(record) = record else {
        return Ok(OwnedPidLiveness::Absent);
    };
    let liveness = owned_pid_record_liveness(Some(&record))?;
    verify_owned_parent(&state, &parent, &mut mount_id)?;
    Ok(liveness)
}

fn owned_pid_record_liveness(record: Option<&OwnedPidRecord>) -> io::Result<OwnedPidLiveness> {
    let Some(record) = record else {
        return Ok(OwnedPidLiveness::Absent);
    };
    if record.pid == 0 {
        let launch_token = record.launch_token.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "unresolved ownership intent has no launch token",
            )
        })?;
        return if process_has_launch_token(launch_token)? {
            Ok(OwnedPidLiveness::UnpublishedIntentLive)
        } else {
            Ok(OwnedPidLiveness::Dead)
        };
    }
    let Some(expected) = record.process_identity.as_deref() else {
        if !process_exists(record.pid)? {
            return Ok(OwnedPidLiveness::Dead);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pid {} has no recorded process identity", record.pid),
        ));
    };
    #[cfg(all(unix, not(target_os = "linux")))]
    if record.launch_token.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "legacy pid {} has no launch token to distinguish PID reuse",
                record.pid
            ),
        ));
    }
    let Some(current) = process_identity(record.pid)? else {
        return Ok(OwnedPidLiveness::Dead);
    };
    if !process_identity_matches(
        record.pid,
        expected,
        &current,
        record.launch_token.as_deref(),
    )? {
        return Ok(OwnedPidLiveness::Dead);
    }
    if process_is_live_with_identity(record.pid, expected, record.launch_token.as_deref())? {
        Ok(OwnedPidLiveness::Live)
    } else {
        Ok(OwnedPidLiveness::Dead)
    }
}

fn persisted_process_identity(identity: &str, launch_token: &str) -> String {
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // `ps` includes the whole command line, which can exceed the bounded
        // marker size for long checkout/state paths. Keep a compact digest of
        // start time plus argv and retain the token separately for explicit
        // child binding.
        let digest = Sha256::digest(identity.as_bytes());
        let hash = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        return format!("unix-ps-sha256:{hash}:{launch_token}");
    }
    #[cfg(not(all(unix, not(target_os = "linux"))))]
    {
        let _ = launch_token;
        identity.to_owned()
    }
}

fn process_identity_matches(
    pid: u32,
    expected: &str,
    current: &str,
    launch_token: Option<&str>,
) -> io::Result<bool> {
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        if let Some(encoded) = expected.strip_prefix("unix-ps-sha256:") {
            let Some((expected_hash, expected_token)) = encoded.split_once(':') else {
                return Ok(false);
            };
            let digest = Sha256::digest(current.as_bytes());
            let current_hash = digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let token_matches = match launch_token {
                Some(token) => expected_token == token && current.contains(token),
                None => expected_token.is_empty(),
            };
            return Ok(expected_hash == current_hash && token_matches);
        }
        return Ok(expected == current && launch_token.is_none_or(|token| current.contains(token)));
    }
    #[cfg(target_os = "linux")]
    {
        if expected != current {
            return Ok(false);
        }
        match launch_token {
            Some(token) => process_command_line_contains(pid, token),
            None => Ok(true),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, expected, current, launch_token);
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
fn process_command_line_contains(pid: u32, token: &str) -> io::Result<bool> {
    let command_line = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(command_line) => command_line,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(argv_contains_launch_token(&command_line, token))
}

fn owned_name(isolation_id: &str, generation: u64) -> String {
    format!("{isolation_id}.{generation}")
}

#[cfg(not(unix))]
fn ensure_owned_parent(state_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("create state directory {}", state_dir.display()))?;
    let state_metadata = std::fs::symlink_metadata(state_dir)
        .with_context(|| format!("inspect state directory {}", state_dir.display()))?;
    if state_metadata.file_type().is_symlink() || !state_metadata.is_dir() {
        anyhow::bail!(
            "state path is not a real directory: {}",
            state_dir.display()
        );
    }

    let parent = state_dir.join("owned");
    match std::fs::symlink_metadata(&parent) {
        Ok(metadata) => validate_owned_parent_metadata(&parent, &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match state_dir.join("journal.db").symlink_metadata() {
                Ok(_) => anyhow::bail!(
                    "ownership directory is missing from initialized state: {}",
                    parent.display()
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("inspect journal state in {}", state_dir.display())
                    });
                }
            }
            for entry in std::fs::read_dir(state_dir)
                .with_context(|| format!("inspect state directory {}", state_dir.display()))?
            {
                let entry = entry.context("read state directory entry")?;
                if entry.file_name() == std::ffi::OsStr::new("execution.toml") {
                    let metadata = std::fs::symlink_metadata(entry.path())
                        .with_context(|| "inspect execution configuration in fresh state")?;
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        anyhow::bail!(
                            "execution configuration in fresh state is not a regular file"
                        );
                    }
                    continue;
                }
                if entry.file_name() != "owned" {
                    anyhow::bail!(
                        "ownership directory is missing from non-empty state: {}",
                        parent.display()
                    );
                }
            }
            match std::fs::create_dir(&parent) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("create ownership directory {}", parent.display())
                    })
                }
            }
            let metadata = std::fs::symlink_metadata(&parent)
                .with_context(|| format!("inspect ownership directory {}", parent.display()))?;
            validate_owned_parent_metadata(&parent, &metadata)?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect ownership directory {}", parent.display()));
        }
    }
    Ok(())
}

fn validate_owned_parent_metadata(
    parent: &Path,
    metadata: &std::fs::Metadata,
) -> anyhow::Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "ownership parent is not a real directory: {}",
            parent.display()
        );
    }
    Ok(())
}

fn require_owned_parent(state_dir: &Path) -> anyhow::Result<()> {
    let parent = state_dir.join("owned");
    let metadata = std::fs::symlink_metadata(&parent)
        .with_context(|| format!("inspect ownership directory {}", parent.display()))?;
    validate_owned_parent_metadata(&parent, &metadata)
}

#[cfg(unix)]
fn open_state_directory(state_dir: &Path) -> io::Result<std::fs::File> {
    let normalized_state_dir = path_without_terminal_dot_components(state_dir);
    let fd = rustix::fs::openat(
        rustix::fs::CWD,
        &normalized_state_dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    if FileType::from_raw_mode(rustix::fs::fstat(&fd).map_err(io::Error::from)?.st_mode)
        != FileType::Directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("state path is not a directory: {}", state_dir.display()),
        ));
    }
    Ok(fd.into())
}

#[cfg(unix)]
fn path_without_terminal_dot_components(path: &Path) -> PathBuf {
    let bytes = path.as_os_str().as_bytes();
    let mut end = bytes.len();
    loop {
        while end > 1 && bytes[end - 1] == b'/' {
            end -= 1;
        }
        if end >= 2 && &bytes[end - 2..end] == b"/." {
            end -= 2;
            continue;
        }
        break;
    }
    if end == 0 && bytes.first() == Some(&b'/') {
        PathBuf::from("/")
    } else if end == 0 {
        PathBuf::from(".")
    } else {
        PathBuf::from(OsStr::from_bytes(&bytes[..end]))
    }
}

#[cfg(not(unix))]
fn open_state_directory(_state_dir: &Path) -> io::Result<std::fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "descriptor-relative state access is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn sync_state_directory_path(state_dir: &Path, state: &std::fs::File) -> anyhow::Result<()> {
    let mut sync_parent =
        |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
    sync_state_directory_path_with(state_dir, state, &mut sync_parent)
}

#[cfg(unix)]
fn sync_state_directory_path_with(
    state_dir: &Path,
    state: &std::fs::File,
    sync_parent: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> anyhow::Result<()> {
    let canonical_state_dir = std::fs::canonicalize(state_dir)
        .context("resolve state directory path before syncing its ancestors")?;
    let mut current_path = PathBuf::from("/");
    let mut current_directory: std::fs::File = rustix::fs::openat(
        rustix::fs::CWD,
        Path::new("/"),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .context("open filesystem root before syncing state path")?
    .into();
    let mut matched_state_directory = canonical_state_dir == Path::new("/");

    for component in canonical_state_dir.components() {
        let name = match component {
            std::path::Component::RootDir => continue,
            std::path::Component::Normal(name) => name,
            _ => anyhow::bail!("canonical state directory has an unexpected component"),
        };
        let child_path = current_path.join(name);
        let child: std::fs::File = rustix::fs::openat(
            &current_directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)
        .with_context(|| format!("open state path component {}", child_path.display()))?
        .into();
        verify_directory_entry_identity(&current_directory, name, &child)
            .with_context(|| format!("state path component changed: {}", child_path.display()))?;
        if child_path == canonical_state_dir {
            verify_state_directory_entry(&current_directory, name, state)
                .context("state directory name no longer points at pinned state")?;
            matched_state_directory = true;
        }

        sync_parent(&current_directory).with_context(|| {
            format!(
                "sync parent of state path component {}",
                child_path.display()
            )
        })?;
        verify_directory_entry_identity(&current_directory, name, &child).with_context(|| {
            format!(
                "state path component changed during sync: {}",
                child_path.display()
            )
        })?;
        if child_path == canonical_state_dir {
            verify_state_directory_entry(&current_directory, name, state)
                .context("state directory name changed during parent sync")?;
        }
        current_path = child_path;
        current_directory = child;
    }

    if !matched_state_directory {
        anyhow::bail!("could not identify the pinned state directory in its path");
    }
    let current = rustix::fs::fstat(&current_directory).map_err(io::Error::from)?;
    let pinned = rustix::fs::fstat(state).map_err(io::Error::from)?;
    if current.st_dev != pinned.st_dev || current.st_ino != pinned.st_ino {
        anyhow::bail!("state directory path no longer resolves to the pinned state");
    }
    Ok(())
}

#[cfg(unix)]
fn verify_state_directory_entry(
    parent: &std::fs::File,
    name: &OsStr,
    state: &std::fs::File,
) -> io::Result<()> {
    verify_directory_entry_identity(parent, name, state)
}

#[cfg(unix)]
fn verify_directory_entry_identity(
    parent: &std::fs::File,
    name: &OsStr,
    opened_directory: &std::fs::File,
) -> io::Result<()> {
    let entry =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    let opened = rustix::fs::fstat(opened_directory).map_err(io::Error::from)?;
    if FileType::from_raw_mode(entry.st_mode) != FileType::Directory
        || entry.st_dev != opened.st_dev
        || entry.st_ino != opened.st_ino
    {
        return Err(io::Error::other("state directory entry changed"));
    }
    Ok(())
}

#[cfg(unix)]
fn open_regular_file_at(
    parent: &std::fs::File,
    name: &str,
    flags: OFlags,
    mode: Mode,
) -> io::Result<std::fs::File> {
    let path = Path::new(name);
    // Reject stable special files before open. NONBLOCK prevents FIFO opens
    // from waiting for a peer, but some device drivers may still block during
    // open if a regular file is replaced after this preflight.
    match rustix::fs::statat(parent, path, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a regular file", path.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error == rustix::io::Errno::NOENT && flags.contains(OFlags::CREATE) => {}
        Err(error) => return Err(io::Error::from(error)),
    }

    let fd = rustix::fs::openat(
        parent,
        path,
        flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        mode,
    )
    .map_err(io::Error::from)?;
    if FileType::from_raw_mode(rustix::fs::fstat(&fd).map_err(io::Error::from)?.st_mode)
        != FileType::RegularFile
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(fd.into())
}

/// Open `jobs/` relative to the state directory without following a symlink.
/// Creation is limited to the parent itself; callers create one exact child
/// entry through the returned directory handle.
#[cfg(unix)]
fn sync_directory_fd(directory: &std::fs::File) -> io::Result<()> {
    rustix::fs::fsync(directory).map_err(io::Error::from)
}

#[cfg(unix)]
fn open_jobs_parent_at(state: &std::fs::File, create: bool) -> io::Result<Option<std::fs::File>> {
    let mut sync_directory = sync_directory_fd;
    open_jobs_parent_at_with_sync(state, create, &mut sync_directory)
}

#[cfg(unix)]
fn open_jobs_parent_at_with_sync(
    state: &std::fs::File,
    create: bool,
    sync_directory: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> io::Result<Option<std::fs::File>> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let jobs_path = Path::new("jobs");
    let fd = match rustix::fs::openat(state, jobs_path, directory_flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT && !create => return Ok(None),
        Err(error) if error == rustix::io::Errno::NOENT => {
            match rustix::fs::mkdirat(state, jobs_path, Mode::from_raw_mode(0o777)) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::EXIST => {}
                Err(error) => return Err(io::Error::from(error)),
            }
            rustix::fs::openat(state, jobs_path, directory_flags, Mode::empty())
                .map_err(io::Error::from)?
        }
        Err(error) => return Err(io::Error::from(error)),
    };
    if FileType::from_raw_mode(rustix::fs::fstat(&fd).map_err(io::Error::from)?.st_mode)
        != FileType::Directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "jobs parent is not a directory",
        ));
    }
    // Ensure a racing creator's `jobs/` entry is durable before callers can
    // publish ownership intent that depends on it.
    if create {
        sync_directory(state)?;
    }
    Ok(Some(fd.into()))
}

#[cfg(unix)]
fn ensure_job_directory_at(
    state: &std::fs::File,
    isolation_id: &str,
) -> anyhow::Result<Option<JobDirectoryIdentity>> {
    let mut mount_id = job_directory_mount_id;
    let mut sync_directory = sync_directory_fd;
    ensure_job_directory_at_with_sync(state, isolation_id, &mut mount_id, &mut sync_directory)
}

#[cfg(unix)]
fn ensure_job_directory_at_with(
    state: &std::fs::File,
    isolation_id: &str,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<Option<JobDirectoryIdentity>> {
    let mut sync_directory = sync_directory_fd;
    ensure_job_directory_at_with_sync(state, isolation_id, mount_id, &mut sync_directory)
}

#[cfg(unix)]
fn ensure_job_directory_at_with_sync(
    state: &std::fs::File,
    isolation_id: &str,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
    sync_directory: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> anyhow::Result<Option<JobDirectoryIdentity>> {
    let parent = open_jobs_parent_at_with_sync(state, true, sync_directory)?
        .ok_or_else(|| anyhow::anyhow!("jobs parent is missing"))?;
    // Reject an unsafe `jobs/` mount before creating or accepting a job path,
    // so spawn cannot begin in a directory cleanup will later refuse to walk.
    verify_jobs_parent(state, &parent, mount_id)?;
    let name = Path::new(isolation_id);
    match rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(0o777)) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::EXIST => {}
        Err(error) => return Err(io::Error::from(error).into()),
    }
    // Sync even when the entry already existed: another creator may have
    // exposed it before syncing its parent, and ownership must follow that
    // durable directory entry.
    sync_directory(&parent).context("sync jobs directory after ensuring job entry")?;
    let directory: std::fs::File = rustix::fs::openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?
    .into();
    if FileType::from_raw_mode(
        rustix::fs::fstat(&directory)
            .map_err(io::Error::from)?
            .st_mode,
    ) != FileType::Directory
    {
        anyhow::bail!("job path is not a directory: {isolation_id}");
    }
    // A job entry may itself be a same-device mount. Check it before the
    // caller can publish launch intent or spawn a worker into the tree.
    verify_job_ancestry(state, &parent, &directory, mount_id)?;
    verify_job_entry_identity(&parent, name.as_os_str(), &directory)?;
    let parent_stat = rustix::fs::fstat(&parent).map_err(io::Error::from)?;
    let job_stat = rustix::fs::fstat(&directory).map_err(io::Error::from)?;
    Ok(Some(JobDirectoryIdentity {
        jobs_device: parent_stat.st_dev as u64,
        jobs_inode: parent_stat.st_ino,
        job_device: job_stat.st_dev as u64,
        job_inode: job_stat.st_ino,
    }))
}

#[cfg(not(unix))]
fn ensure_job_directory(state_dir: &Path, isolation_id: &str) -> anyhow::Result<()> {
    let parent = state_dir.join("jobs");
    match std::fs::symlink_metadata(&parent) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("jobs parent is not a real directory: {}", parent.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(&parent)?,
        Err(error) => return Err(error.into()),
    }
    let job = parent.join(isolation_id);
    match std::fs::symlink_metadata(&job) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("job path is not a real directory: {}", job.display())
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => std::fs::create_dir(&job)?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_job_directory_at(
    _state: &std::fs::File,
    _isolation_id: &str,
) -> anyhow::Result<Option<JobDirectoryIdentity>> {
    anyhow::bail!("descriptor-relative job paths are unavailable on this platform")
}

fn ensure_owned_job_directory_matches(
    record: &OwnedPidRecord,
    current: &Option<JobDirectoryIdentity>,
) -> anyhow::Result<()> {
    if record.job_directory.is_none() || record.job_directory.as_ref() != current.as_ref() {
        anyhow::bail!("owned job directory identity changed before cleanup or replacement");
    }
    Ok(())
}

#[cfg(unix)]
fn open_existing_job_directory_identity(
    state: &std::fs::File,
    isolation_id: &str,
) -> anyhow::Result<Option<JobDirectoryIdentity>> {
    let Some(parent) = open_jobs_parent_at(state, false)? else {
        return Ok(None);
    };
    let mut mount_id = job_directory_mount_id;
    verify_jobs_parent(state, &parent, &mut mount_id)?;
    let directory = match open_job_directory_at(&parent, OsStr::new(isolation_id)) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verify_job_ancestry(state, &parent, &directory, &mut mount_id)?;
    verify_job_entry_identity(&parent, OsStr::new(isolation_id), &directory)?;
    let parent_stat = rustix::fs::fstat(&parent).map_err(io::Error::from)?;
    let job_stat = rustix::fs::fstat(&directory).map_err(io::Error::from)?;
    Ok(Some(JobDirectoryIdentity {
        jobs_device: parent_stat.st_dev as u64,
        jobs_inode: parent_stat.st_ino,
        job_device: job_stat.st_dev as u64,
        job_inode: job_stat.st_ino,
    }))
}

#[cfg(not(unix))]
fn open_existing_job_directory_identity(
    _state: &std::fs::File,
    _isolation_id: &str,
) -> anyhow::Result<Option<JobDirectoryIdentity>> {
    anyhow::bail!("descriptor-relative job paths are unavailable on this platform")
}

#[cfg(unix)]
struct JobRemovalChild {
    name: OsString,
    device: u64,
    inode: u64,
    is_directory: bool,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum JobMountIdentity {
    #[cfg(target_os = "linux")]
    Numeric(u64),
    #[cfg(not(target_os = "linux"))]
    MountPoint(Vec<u8>),
}

#[cfg(unix)]
struct JobRemovalFrame {
    name: OsString,
    device: u64,
    inode: u64,
    children: Vec<JobRemovalChild>,
}

/// Delete one exact job tree through pinned directory handles.
///
/// The caller must stop every writer with permission to mutate `jobs/` or the
/// target tree before cleanup begins. POSIX has no unlink-by-directory-fd or
/// atomic unlink-if-inode-matches operation, so a same-UID actor that can
/// rename entries concurrently with the final check remains outside this
/// function's trust boundary. The walk revalidates each opened entry and its
/// direct ancestry, rejects device and mount changes, and keeps descriptor
/// use constant with depth.
#[cfg(unix)]
fn remove_job_entry_with(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &OsStr,
    sync: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
    after_open: &mut dyn FnMut(&std::fs::File, &OsStr, &std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    verify_jobs_parent(state, parent, mount_id)?;
    let Some(stat) = stat_job_entry(parent, name, sync)? else {
        return Ok(());
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        let parent_device = rustix::fs::fstat(parent).map_err(io::Error::from)?.st_dev;
        if stat.st_dev != parent_device {
            return Err(io::Error::other(
                "refusing to remove a job entry across a filesystem boundary",
            ));
        }
        unlink_job_entry(parent, name)?;
        return sync_with_retry(parent, sync);
    }

    let root_directory = match open_job_directory_at(parent, name) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            sync_with_retry(parent, sync)?;
            return Err(io::Error::other("job directory moved during cleanup"));
        }
        Err(error) => return Err(error),
    };
    let opened_root = rustix::fs::fstat(&root_directory).map_err(io::Error::from)?;
    if opened_root.st_dev != stat.st_dev || opened_root.st_ino != stat.st_ino {
        return Err(io::Error::other("job directory changed during cleanup"));
    }
    after_open(parent, name, &root_directory)?;
    verify_job_mount_identity(parent, &root_directory, mount_id)?;
    verify_job_ancestry(state, parent, &root_directory, mount_id)?;
    verify_job_entry_identity(parent, name, &root_directory)?;

    let root_device = opened_root.st_dev as u64;
    let root_children = collect_job_children(&root_directory, root_device, sync)?;
    let mut stack = Vec::new();
    stack
        .try_reserve(1)
        .map_err(|error| io::Error::other(format!("grow job cleanup traversal stack: {error}")))?;
    stack.push(JobRemovalFrame {
        name: copy_job_component(name)?,
        device: opened_root.st_dev as u64,
        inode: opened_root.st_ino,
        children: root_children,
    });
    let mut current_directory = root_directory.try_clone()?;

    let removal = (|| -> io::Result<()> {
        loop {
            let child = stack
                .last_mut()
                .ok_or_else(|| io::Error::other("job cleanup stack is empty"))?
                .children
                .pop();

            if let Some(child) = child {
                let Some(stat) = stat_job_entry(&current_directory, &child.name, sync)? else {
                    continue;
                };
                let is_directory = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
                if is_directory != child.is_directory
                    || stat.st_dev as u64 != child.device
                    || stat.st_ino != child.inode
                {
                    return Err(io::Error::other("job entry changed during cleanup"));
                }
                if !is_directory {
                    unlink_job_entry(&current_directory, &child.name)?;
                    continue;
                }
                if child.device != root_device {
                    return Err(io::Error::other(
                        "refusing to remove job contents across a filesystem boundary",
                    ));
                }
                let child_directory = match open_job_directory_at(&current_directory, &child.name) {
                    Ok(directory) => directory,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        sync_with_retry(&current_directory, sync)?;
                        return Err(io::Error::other("job directory moved during cleanup"));
                    }
                    Err(error) => return Err(error),
                };
                let opened = rustix::fs::fstat(&child_directory).map_err(io::Error::from)?;
                if opened.st_dev as u64 != child.device || opened.st_ino != child.inode {
                    return Err(io::Error::other("job directory changed during cleanup"));
                }
                after_open(&current_directory, &child.name, &child_directory)?;
                verify_job_directory_parent(&child_directory, &current_directory)?;
                verify_job_mount_identity(&current_directory, &child_directory, mount_id)?;
                let children = collect_job_children(&child_directory, root_device, sync)?;
                stack.try_reserve(1).map_err(|error| {
                    io::Error::other(format!("grow job cleanup traversal: {error}"))
                })?;
                stack.push(JobRemovalFrame {
                    name: child.name,
                    device: opened.st_dev as u64,
                    inode: opened.st_ino,
                    children,
                });
                current_directory = child_directory;
                continue;
            }

            let frame = stack
                .last()
                .ok_or_else(|| io::Error::other("job cleanup stack is empty"))?;
            if stack.len() == 1 {
                verify_job_ancestry(state, parent, &root_directory, mount_id)?;
                sync(&current_directory)?;
                remove_job_directory_from_parent(parent, frame)?;
                sync_with_retry(parent, sync)?;
                return Ok(());
            }

            let containing_directory = open_parent_directory(&current_directory)?;
            let expected_parent = stack
                .get(stack.len() - 2)
                .ok_or_else(|| io::Error::other("job cleanup parent frame is missing"))?;
            verify_directory_identity(
                &containing_directory,
                expected_parent.device,
                expected_parent.inode,
            )?;
            verify_job_mount_identity(&containing_directory, &current_directory, mount_id)?;
            sync(&current_directory)?;
            remove_job_directory_from_parent(&containing_directory, frame)?;
            stack.pop();
            current_directory = containing_directory;
        }
    })();

    match removal {
        Ok(()) => Ok(()),
        Err(error) => match sync(&current_directory) {
            Ok(()) => Err(error),
            Err(sync_error) => Err(io::Error::other(format!(
                "job cleanup failed: {error}; syncing partial deletion also failed: {sync_error}"
            ))),
        },
    }
}

#[cfg(unix)]
fn collect_job_children(
    directory: &std::fs::File,
    root_device: u64,
    sync: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> io::Result<Vec<JobRemovalChild>> {
    let mut entries = rustix::fs::Dir::read_from(directory).map_err(io::Error::from)?;
    let mut children = Vec::new();
    while let Some(entry) = entries.read() {
        let entry = entry.map_err(io::Error::from)?;
        let entry_name = entry.file_name().to_bytes();
        if entry_name == b"." || entry_name == b".." {
            continue;
        }
        let name = copy_job_component(OsStr::from_bytes(entry_name))?;
        let stat = match rustix::fs::statat(directory, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) if error == rustix::io::Errno::NOENT => {
                sync_with_retry(directory, sync)?;
                continue;
            }
            Err(error) => return Err(io::Error::from(error)),
        };
        let is_directory = FileType::from_raw_mode(stat.st_mode) == FileType::Directory;
        if is_directory && stat.st_dev as u64 != root_device {
            return Err(io::Error::other(
                "refusing to remove job contents across a filesystem boundary",
            ));
        }
        children.try_reserve(1).map_err(|error| {
            io::Error::other(format!("grow job cleanup child snapshot: {error}"))
        })?;
        children.push(JobRemovalChild {
            name,
            device: stat.st_dev as u64,
            inode: stat.st_ino,
            is_directory,
        });
    }
    Ok(children)
}

#[cfg(unix)]
fn stat_job_entry(
    parent: &std::fs::File,
    name: &OsStr,
    sync: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> io::Result<Option<rustix::fs::Stat>> {
    match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(error) if error == rustix::io::Errno::NOENT => {
            sync_with_retry(parent, sync)?;
            Ok(None)
        }
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(unix)]
fn unlink_job_entry(parent: &std::fs::File, name: &OsStr) -> io::Result<()> {
    match rustix::fs::unlinkat(parent, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(unix)]
fn remove_job_directory_from_parent(
    parent: &std::fs::File,
    frame: &JobRemovalFrame,
) -> io::Result<()> {
    let current = match rustix::fs::statat(parent, &frame.name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(current) => current,
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(()),
        Err(error) => return Err(io::Error::from(error)),
    };
    if FileType::from_raw_mode(current.st_mode) != FileType::Directory
        || current.st_dev as u64 != frame.device
        || current.st_ino != frame.inode
    {
        return Err(io::Error::other("job directory changed during cleanup"));
    }
    match rustix::fs::unlinkat(parent, &frame.name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(unix)]
fn copy_job_component(name: &OsStr) -> io::Result<OsString> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(name.as_bytes().len())
        .map_err(|error| {
            io::Error::other(format!("allocate job cleanup path component: {error}"))
        })?;
    bytes.extend_from_slice(name.as_bytes());
    Ok(OsString::from_vec(bytes))
}

#[cfg(unix)]
fn open_job_directory_at(parent: &std::fs::File, name: &OsStr) -> io::Result<std::fs::File> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(Into::into)
    .map_err(io::Error::from)
}

#[cfg(unix)]
fn open_parent_directory(directory: &std::fs::File) -> io::Result<std::fs::File> {
    rustix::fs::openat(
        directory,
        OsStr::new(".."),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(Into::into)
    .map_err(io::Error::from)
}

#[cfg(unix)]
fn verify_directory_identity(directory: &std::fs::File, device: u64, inode: u64) -> io::Result<()> {
    let stat = rustix::fs::fstat(directory).map_err(io::Error::from)?;
    if stat.st_dev as u64 != device || stat.st_ino != inode {
        return Err(io::Error::other(
            "job directory ancestry changed during cleanup",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_job_directory_parent(
    directory: &std::fs::File,
    expected_parent: &std::fs::File,
) -> io::Result<()> {
    let parent = open_parent_directory(directory)?;
    let expected = rustix::fs::fstat(expected_parent).map_err(io::Error::from)?;
    verify_directory_identity(&parent, expected.st_dev as u64, expected.st_ino)
}

#[cfg(unix)]
fn verify_job_entry_identity(
    parent: &std::fs::File,
    name: &OsStr,
    opened: &std::fs::File,
) -> io::Result<()> {
    let entry =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
    let opened = rustix::fs::fstat(opened).map_err(io::Error::from)?;
    if FileType::from_raw_mode(entry.st_mode) != FileType::Directory
        || entry.st_dev != opened.st_dev
        || entry.st_ino != opened.st_ino
    {
        return Err(io::Error::other("job directory changed during cleanup"));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_job_ancestry(
    state: &std::fs::File,
    jobs: &std::fs::File,
    job_root: &std::fs::File,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> io::Result<()> {
    verify_jobs_parent(state, jobs, mount_id)?;
    verify_job_directory_parent(job_root, jobs)?;
    verify_job_mount_identity(jobs, job_root, mount_id)
}

#[cfg(unix)]
fn verify_jobs_parent(
    state: &std::fs::File,
    jobs: &std::fs::File,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> io::Result<()> {
    verify_job_directory_parent(jobs, state)?;
    verify_job_mount_identity(state, jobs, mount_id)
        .and_then(|()| verify_named_directory_identity(state, "jobs", jobs))
}

#[cfg(unix)]
fn verify_named_directory_identity(
    parent: &std::fs::File,
    name: &str,
    opened: &std::fs::File,
) -> io::Result<()> {
    let entry = rustix::fs::statat(parent, Path::new(name), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)?;
    let opened = rustix::fs::fstat(opened).map_err(io::Error::from)?;
    if FileType::from_raw_mode(entry.st_mode) != FileType::Directory
        || entry.st_dev != opened.st_dev
        || entry.st_ino != opened.st_ino
    {
        return Err(io::Error::other(format!(
            "{name} parent changed during operation"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_owned_parent(
    state: &std::fs::File,
    owned: &std::fs::File,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> io::Result<()> {
    verify_job_directory_parent(owned, state)?;
    verify_job_mount_identity(state, owned, mount_id)?;

    let entry = rustix::fs::statat(state, Path::new("owned"), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)?;
    let opened = rustix::fs::fstat(owned).map_err(io::Error::from)?;
    if FileType::from_raw_mode(entry.st_mode) != FileType::Directory
        || entry.st_dev != opened.st_dev
        || entry.st_ino != opened.st_ino
    {
        return Err(io::Error::other(
            "ownership parent changed during operation",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_outbox_parent(
    state: &std::fs::File,
    outbox: &std::fs::File,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> io::Result<()> {
    verify_job_directory_parent(outbox, state)?;
    verify_job_mount_identity(state, outbox, mount_id)?;

    let entry = rustix::fs::statat(state, Path::new("outbox"), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)?;
    let opened = rustix::fs::fstat(outbox).map_err(io::Error::from)?;
    if FileType::from_raw_mode(entry.st_mode) != FileType::Directory
        || entry.st_dev != opened.st_dev
        || entry.st_ino != opened.st_ino
    {
        return Err(io::Error::other(
            "completion outbox parent changed during operation",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn verify_outbox_quarantine(
    outbox: &std::fs::File,
    name: &str,
    quarantine: &std::fs::File,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> io::Result<()> {
    verify_job_directory_parent(quarantine, outbox)?;
    verify_job_mount_identity(outbox, quarantine, mount_id)?;
    verify_job_entry_identity(outbox, OsStr::new(name), quarantine)
        .map_err(|error| io::Error::other(format!("completion outbox quarantine changed: {error}")))
}

#[cfg(unix)]
fn verify_job_mount_identity(
    parent: &std::fs::File,
    directory: &std::fs::File,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> io::Result<()> {
    let parent_stat = rustix::fs::fstat(parent).map_err(io::Error::from)?;
    let directory_stat = rustix::fs::fstat(directory).map_err(io::Error::from)?;
    if parent_stat.st_dev != directory_stat.st_dev || mount_id(parent)? != mount_id(directory)? {
        return Err(io::Error::other(
            "refusing to remove job contents across a filesystem mount boundary",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn job_directory_mount_id(directory: &std::fs::File) -> io::Result<Option<JobMountIdentity>> {
    let stat = rustix::fs::statx(
        directory,
        OsStr::new(""),
        AtFlags::EMPTY_PATH,
        rustix::fs::StatxFlags::MNT_ID,
    )
    .map_err(io::Error::from)?;
    if stat.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem mount identity is unavailable",
        ));
    }
    Ok(Some(JobMountIdentity::Numeric(stat.stx_mnt_id)))
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd"
))]
fn job_directory_mount_id(directory: &std::fs::File) -> io::Result<Option<JobMountIdentity>> {
    // These systems expose the mount point through fstatfs, which separates
    // same-device mounts such as nullfs/bind-style mounts.
    let mut stat = unsafe { std::mem::zeroed::<libc::statfs>() };
    if unsafe { libc::fstatfs(directory.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mount_point = &stat.f_mntonname;
    let mount_point_len = mount_point
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(mount_point.len());
    if mount_point_len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem mount point identity is unavailable",
        ));
    }
    let mut identity = Vec::new();
    identity
        .try_reserve_exact(mount_point_len)
        .map_err(|error| io::Error::other(format!("allocate mount identity: {error}")))?;
    identity.extend(
        mount_point[..mount_point_len]
            .iter()
            .map(|byte| *byte as u8),
    );
    Ok(Some(JobMountIdentity::MountPoint(identity)))
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd"
    ))
))]
fn job_directory_mount_id(_directory: &std::fs::File) -> io::Result<Option<JobMountIdentity>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem mount identity is unavailable on this Unix platform",
    ))
}

#[cfg(all(test, unix))]
fn fake_job_mount_identity(identity: u64) -> JobMountIdentity {
    #[cfg(target_os = "linux")]
    {
        JobMountIdentity::Numeric(identity)
    }
    #[cfg(not(target_os = "linux"))]
    {
        JobMountIdentity::MountPoint(identity.to_ne_bytes().to_vec())
    }
}

#[cfg(unix)]
fn sync_with_retry(
    directory: &std::fs::File,
    sync: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    match sync(directory) {
        Ok(()) => Ok(()),
        Err(first_error) => match sync(directory) {
            Ok(()) => Err(first_error),
            Err(retry_error) => Err(io::Error::other(format!(
                "directory sync failed: {first_error}; retry failed: {retry_error}"
            ))),
        },
    }
}

#[cfg(unix)]
fn owned_generation_exists_other_than(
    parent: &std::fs::File,
    isolation_id: &str,
    excluded_name: &str,
) -> io::Result<bool> {
    let prefix = format!("{isolation_id}.");
    let entries = rustix::fs::Dir::read_from(parent).map_err(io::Error::from)?;
    for entry in entries {
        let entry = entry.map_err(io::Error::from)?;
        let name = entry.file_name().to_bytes();
        if name.starts_with(prefix.as_bytes()) && name != excluded_name.as_bytes() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn owned_entry_is_regular(parent: &std::fs::File, name: &str) -> io::Result<bool> {
    match rustix::fs::statat(parent, Path::new(name), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ownership marker is not a regular file",
        )),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(false),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(unix)]
fn owned_directory_identity_bytes(owned: &std::fs::File) -> io::Result<[u8; 16]> {
    let stat = rustix::fs::fstat(owned).map_err(io::Error::from)?;
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&(stat.st_dev as u64).to_le_bytes());
    bytes[8..].copy_from_slice(&stat.st_ino.to_le_bytes());
    Ok(bytes)
}

#[cfg(unix)]
fn verify_owned_initialization_marker(
    state: &std::fs::File,
    owned: &std::fs::File,
) -> io::Result<()> {
    let marker = open_regular_file_at(
        state,
        OWNED_INITIALIZATION_MARKER_NAME,
        OFlags::RDONLY,
        Mode::empty(),
    )?;
    let mut contents = Vec::new();
    marker.take(17).read_to_end(&mut contents)?;
    let expected = owned_directory_identity_bytes(owned)?;
    if contents.as_slice() != expected.as_slice() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ownership directory identity does not match initialization marker",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_owned_initialization_marker_with_sync(
    state: &std::fs::File,
    owned: &std::fs::File,
    sync_state: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> anyhow::Result<()> {
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(state, owned, &mut mount_id)
        .context("verify ownership directory before publishing identity")?;
    rustix::fs::fsync(owned)
        .map_err(io::Error::from)
        .context("sync ownership directory before initialization marker")?;
    let identity = owned_directory_identity_bytes(owned)?;
    let marker = match rustix::fs::openat(
        state,
        Path::new(OWNED_INITIALIZATION_MARKER_NAME),
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    ) {
        Ok(fd) => {
            let mut marker: std::fs::File = fd.into();
            marker.write_all(&identity)?;
            marker
        }
        Err(error) if error == rustix::io::Errno::EXIST => {
            verify_owned_initialization_marker(state, owned)
                .context("verify existing ownership initialization marker")?;
            open_regular_file_at(
                state,
                OWNED_INITIALIZATION_MARKER_NAME,
                OFlags::RDONLY,
                Mode::empty(),
            )?
        }
        Err(error) => return Err(io::Error::from(error).into()),
    };
    marker
        .sync_all()
        .context("sync ownership initialization marker")?;
    sync_state(state).context("sync state directory after ownership initialization")?;
    verify_owned_parent(state, owned, &mut mount_id)
        .context("ownership directory changed while publishing initialization marker")?;
    Ok(())
}

#[cfg(unix)]
fn ensure_fresh_owned_initialization_state(
    state: &std::fs::File,
    existing_owned: Option<&std::fs::File>,
) -> anyhow::Result<()> {
    let entries = rustix::fs::Dir::read_from(state).map_err(io::Error::from)?;
    for entry in entries {
        let entry = entry.map_err(io::Error::from)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        if existing_owned.is_some() && name == b"owned" {
            continue;
        }
        if name == b"execution.toml" {
            let stat = rustix::fs::statat(
                state,
                Path::new("execution.toml"),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(io::Error::from)?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                anyhow::bail!("execution configuration in fresh state is not a regular file");
            }
            continue;
        }
        anyhow::bail!(
            "ownership directory is missing from non-fresh state (entry {:?})",
            String::from_utf8_lossy(name)
        );
    }
    if let Some(owned) = existing_owned {
        let entries = rustix::fs::Dir::read_from(owned).map_err(io::Error::from)?;
        for entry in entries {
            let entry = entry.map_err(io::Error::from)?;
            let name = entry.file_name().to_bytes();
            if name != b"." && name != b".." {
                anyhow::bail!(
                    "ownership directory has artifacts without its initialization marker"
                );
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_owned_directory_at_with_sync(
    state: &std::fs::File,
    sync_state: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> anyhow::Result<()> {
    let mut mkdir_owned =
        |state: &std::fs::File, name: &Path, mode: Mode| rustix::fs::mkdirat(state, name, mode);
    ensure_owned_directory_at_with_sync_and_mkdir(state, sync_state, &mut mkdir_owned)
}

#[cfg(unix)]
fn ensure_owned_directory_at_with_sync_and_mkdir(
    state: &std::fs::File,
    sync_state: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
    mkdir_owned: &mut dyn FnMut(&std::fs::File, &Path, Mode) -> Result<(), rustix::io::Errno>,
) -> anyhow::Result<()> {
    match open_owned_parent_entry_at(state) {
        Ok(owned) => ensure_owned_initialization_for_parent_with_sync(state, &owned, sync_state),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match rustix::fs::statat(
                state,
                Path::new(OWNED_INITIALIZATION_MARKER_NAME),
                AtFlags::SYMLINK_NOFOLLOW,
            ) {
                Ok(_) => anyhow::bail!("ownership directory is missing from initialized state"),
                Err(error) if error == rustix::io::Errno::NOENT => {}
                Err(error) => return Err(io::Error::from(error).into()),
            }
            ensure_fresh_owned_initialization_state(state, None)?;
            match mkdir_owned(state, Path::new("owned"), Mode::from_raw_mode(0o777)) {
                Ok(()) => {
                    let owned = open_owned_parent_entry_at(state)?;
                    // The directory entry must be durable before the marker
                    // can prove that losing `owned/` is corruption.
                    sync_state(state)
                        .context("sync state directory after creating ownership directory")?;
                    ensure_owned_initialization_marker_with_sync(state, &owned, sync_state)
                }
                Err(error) if error == rustix::io::Errno::EXIST => {
                    // Another first-start path won mkdirat after our initial
                    // absence check. Reopen and validate its directory, then
                    // finish or verify the shared durable initialization.
                    let owned = open_owned_parent_entry_at(state)
                        .context("reopen ownership directory created by concurrent initializer")?;
                    ensure_owned_initialization_for_parent_with_sync(state, &owned, sync_state)
                }
                Err(error) => {
                    Err(io::Error::from(error)).context("create fresh ownership directory")
                }
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn ensure_owned_initialization_for_parent_with_sync(
    state: &std::fs::File,
    owned: &std::fs::File,
    sync_state: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> anyhow::Result<()> {
    match rustix::fs::statat(
        state,
        Path::new(OWNED_INITIALIZATION_MARKER_NAME),
        AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
            verify_owned_initialization_marker(state, owned)?;
            sync_state(state).context("sync state directory before ownership initialization")?;
            Ok(())
        }
        Ok(_) => anyhow::bail!("ownership initialization marker is not a regular file"),
        Err(error) if error == rustix::io::Errno::NOENT => {
            ensure_fresh_owned_initialization_state(state, Some(owned))?;
            sync_state(state).context("sync state directory before ownership marker")?;
            ensure_owned_initialization_marker_with_sync(state, owned, sync_state)
        }
        Err(error) => Err(io::Error::from(error).into()),
    }
}

#[cfg(unix)]
fn ensure_owned_directory_at(state: &std::fs::File) -> anyhow::Result<()> {
    let mut sync_state =
        |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
    ensure_owned_directory_at_with_sync(state, &mut sync_state)
}

/// Initialize the durable ownership directory before the controller reads
/// ownership records. A persistent root marker makes a later loss of `owned/`
/// fail closed even if no other state files remain.
pub(crate) fn initialize_owned_directory(state_dir: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create state directory {}", state_dir.display()))?;
        let state = open_state_directory(state_dir)?;
        sync_state_directory_path(state_dir, &state)
            .context("sync state directory name in its parent")?;
        ensure_owned_directory_at(&state)
    }
    #[cfg(not(unix))]
    ensure_owned_parent(state_dir)
}

#[cfg(unix)]
fn open_owned_parent_at(state: &std::fs::File) -> io::Result<std::fs::File> {
    let parent = open_owned_parent_entry_at(state)?;
    verify_owned_initialization_marker(state, &parent)?;
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(state, &parent, &mut mount_id)?;
    Ok(parent)
}

#[cfg(unix)]
fn open_owned_parent_entry_at(state: &std::fs::File) -> io::Result<std::fs::File> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent_fd = rustix::fs::openat(state, Path::new("owned"), directory_flags, Mode::empty())
        .map_err(io::Error::from)?;
    if FileType::from_raw_mode(
        rustix::fs::fstat(&parent_fd)
            .map_err(io::Error::from)?
            .st_mode,
    ) != FileType::Directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ownership parent is not a directory",
        ));
    }
    let parent: std::fs::File = parent_fd.into();
    let mut mount_id = job_directory_mount_id;
    verify_owned_parent(state, &parent, &mut mount_id)?;
    Ok(parent)
}

#[cfg(unix)]
fn open_owned_parent(state_dir: &Path) -> io::Result<std::fs::File> {
    let state = open_state_directory(state_dir)?;
    open_owned_parent_at(&state)
}

#[cfg(not(unix))]
fn open_owned_parent_at(_state: &std::fs::File) -> io::Result<std::fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no-follow ownership markers are unavailable on this platform",
    ))
}

#[cfg(not(unix))]
fn open_owned_parent(_state_dir: &Path) -> io::Result<std::fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no-follow ownership markers are unavailable on this platform",
    ))
}

#[cfg(unix)]
struct OwnedPidLock {
    _file: std::fs::File,
}

#[cfg(unix)]
fn acquire_owned_pid_lock(parent: &std::fs::File) -> anyhow::Result<OwnedPidLock> {
    let file = open_regular_file_at(
        parent,
        OWNED_PID_LOCK_NAME,
        OFlags::RDWR | OFlags::CREATE,
        Mode::from_raw_mode(0o600),
    )
    .context("open ownership marker lock")?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(io::Error::from)
        .context("lock ownership marker directory")?;
    Ok(OwnedPidLock { _file: file })
}

#[cfg(not(unix))]
fn acquire_owned_pid_lock(_parent: &std::fs::File) -> anyhow::Result<()> {
    anyhow::bail!("no-follow ownership marker locks are unavailable on this platform")
}

#[cfg(unix)]
fn read_owned_record_from_parent(
    parent: &std::fs::File,
    name: &str,
) -> io::Result<Option<OwnedPidRecord>> {
    let file = match open_regular_file_at(parent, name, OFlags::RDONLY, Mode::empty()) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let stat = rustix::fs::fstat(&file).map_err(io::Error::from)?;
    let size = usize::try_from(stat.st_size).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "negative ownership marker size")
    })?;
    if size > MAX_OWNED_PID_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("ownership marker exceeds {MAX_OWNED_PID_BYTES} bytes: {name}"),
        ));
    }
    let mut contents = String::new();
    file.take(MAX_OWNED_PID_BYTES as u64 + 1)
        .read_to_string(&mut contents)?;
    if contents.len() > MAX_OWNED_PID_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("ownership marker exceeds {MAX_OWNED_PID_BYTES} bytes: {name}"),
        ));
    }
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(not(unix))]
fn read_owned_record_from_parent(
    _parent: &std::fs::File,
    _name: &str,
) -> io::Result<Option<OwnedPidRecord>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no-follow ownership markers are unavailable on this platform",
    ))
}

#[cfg(unix)]
fn write_owned_record_exclusive(
    parent: &std::fs::File,
    name: &str,
    record: &OwnedPidRecord,
) -> anyhow::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags};

    let bytes = serde_json::to_vec(record).context("serialize ownership marker")?;
    if bytes.len() > MAX_OWNED_PID_BYTES {
        anyhow::bail!("ownership marker exceeds {MAX_OWNED_PID_BYTES} bytes");
    }
    let temporary = owned_temp_name(name);
    let fd = rustix::fs::openat(
        parent,
        Path::new(&temporary),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(io::Error::from)?;
    let mut file: std::fs::File = fd.into();
    let result = (|| -> anyhow::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        rustix::fs::linkat(
            parent,
            Path::new(&temporary),
            parent,
            Path::new(name),
            AtFlags::empty(),
        )
        .map_err(io::Error::from)?;
        rustix::fs::unlinkat(parent, Path::new(&temporary), AtFlags::empty())
            .map_err(io::Error::from)?;
        rustix::fs::fsync(parent).map_err(io::Error::from)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, Path::new(&temporary), AtFlags::empty());
    }
    result
}

#[cfg(not(unix))]
fn write_owned_record_exclusive(
    _parent: &std::fs::File,
    _name: &str,
    _record: &OwnedPidRecord,
) -> anyhow::Result<()> {
    anyhow::bail!("no-follow ownership markers are unavailable on this platform")
}

#[cfg(unix)]
fn replace_owned_intent(
    parent: &std::fs::File,
    name: &str,
    record: &OwnedPidRecord,
) -> anyhow::Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags};

    let bytes = serde_json::to_vec(record).context("serialize owned pid record")?;
    if bytes.len() > MAX_OWNED_PID_BYTES {
        anyhow::bail!("ownership marker exceeds {MAX_OWNED_PID_BYTES} bytes");
    }
    let temporary = owned_temp_name(name);
    let fd = rustix::fs::openat(
        parent,
        Path::new(&temporary),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(io::Error::from)?;
    let mut file: std::fs::File = fd.into();
    let result = (|| -> anyhow::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        rustix::fs::renameat(parent, Path::new(&temporary), parent, Path::new(name))
            .map_err(io::Error::from)?;
        rustix::fs::fsync(parent).map_err(io::Error::from)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, Path::new(&temporary), AtFlags::empty());
    }
    result
}

#[cfg(not(unix))]
fn replace_owned_intent(
    _parent: &std::fs::File,
    _name: &str,
    _record: &OwnedPidRecord,
) -> anyhow::Result<()> {
    anyhow::bail!("no-follow ownership markers are unavailable on this platform")
}

fn owned_temp_name(name: &str) -> String {
    format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple())
}

/// Read the stable identity for one live process ID. The value binds a host
/// PID to its boot/start record (and, on Unix targets with `ps`, its command
/// line) so durable permit rows can distinguish a dead owner from PID reuse.
pub fn process_identity(pid: u32) -> io::Result<Option<String>> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pid cannot be represented as pid_t",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
        let stat = match std::fs::read_to_string(&stat_path) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if process_exists(pid)? {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("cannot read process identity for live pid {pid}"),
                    ));
                }
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let close = stat.rfind(')').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "malformed /proc pid stat comm")
        })?;
        let fields: Vec<_> = stat[close + 1..].split_whitespace().collect();
        let start_ticks = fields.get(19).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "short /proc pid stat record")
        })?;
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
        Ok(Some(format!("linux:{}:{}", boot_id.trim(), start_ticks)))
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let output = std::process::Command::new("ps")
            .args([
                "-ww",
                "-p",
                &pid.to_string(),
                "-o",
                "lstart=",
                "-o",
                "command=",
            ])
            .output()?;
        if !output.status.success() || output.stdout.is_empty() {
            if process_exists(pid)? {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("cannot read process identity for live pid {pid}"),
                ));
            }
            return Ok(None);
        }
        let identity = String::from_utf8(output.stdout)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            .trim()
            .to_owned();
        if identity.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("empty process identity for pid {pid}"),
            ));
        }
        Ok(Some(format!("unix-ps:{identity}")))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process identity checks are unavailable on this platform",
        ))
    }
}

/// A slot process proven against its persisted slot identity. On Linux the
/// handle owns a pidfd, so later liveness checks and signals stay bound to the
/// process that passed validation even if the numeric PID is reused.
pub struct PinnedSlotProcess {
    #[cfg(target_os = "linux")]
    pidfd: std::fs::File,
    #[cfg(all(unix, not(target_os = "linux")))]
    pid: u32,
    #[cfg(all(unix, not(target_os = "linux")))]
    identity: String,
}

impl PinnedSlotProcess {
    /// Whether this exact process remains alive.
    pub fn is_alive(&self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            return pidfd_is_live(&self.pidfd);
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let Some(before) = process_identity(self.pid)? else {
                return Ok(false);
            };
            if before != self.identity || !process_exists(self.pid)? {
                return Ok(false);
            }
            Ok(process_identity(self.pid)?.as_deref() == Some(self.identity.as_str()))
        }
        #[cfg(not(unix))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "slot process liveness checks are unavailable on this platform",
            ))
        }
    }

    /// Signal this exact process. Linux sends through the pidfd held since
    /// identity validation; non-Linux Unix targets recheck the stable process
    /// snapshot immediately before the numeric-PID signal.
    pub fn signal(&self, signal: libc::c_int) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            if !pidfd_is_live(&self.pidfd)? {
                return Ok(false);
            }
            return pidfd_send_signal(&self.pidfd, signal);
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            loop {
                if !self.is_alive()? {
                    return Ok(false);
                }
                // SAFETY: non-Linux Unix has no pidfd API. The stable start
                // time and full command snapshot were just rechecked above.
                if unsafe { libc::kill(self.pid as libc::pid_t, signal) } == 0 {
                    return Ok(true);
                }
                let error = io::Error::last_os_error();
                match error.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(libc::ESRCH) => return Ok(false),
                    _ => return Err(error),
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = signal;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "slot process signaling is unavailable on this platform",
            ))
        }
    }
}

/// Open and validate one slot actor. `None` means the PID is gone or does not
/// carry the exact slot command identity. The returned handle keeps Linux
/// validation and all subsequent signals on the same pidfd.
pub fn pin_slot_process(
    pid: u32,
    state_dir: &Path,
    slot_id: &SlotId,
    generation: Generation,
) -> io::Result<Option<PinnedSlotProcess>> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "slot PID cannot be represented as pid_t",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
        if raw_fd < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error);
        }
        // SAFETY: successful pidfd_open returns an owned descriptor.
        let pidfd = unsafe { std::fs::File::from_raw_fd(raw_fd as i32) };
        let Some((scope, index)) = slot_id.0.rsplit_once('-') else {
            return Ok(None);
        };
        if scope.is_empty() {
            return Ok(None);
        }
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmdline) => cmdline,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !linux_slot_command_line_matches(&cmdline, state_dir, scope, index, generation) {
            return Ok(None);
        }
        if !pidfd_is_live(&pidfd)? {
            return Ok(None);
        }
        return Ok(Some(PinnedSlotProcess { pidfd }));
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let Some((scope, index)) = slot_id.0.rsplit_once('-') else {
            return Ok(None);
        };
        if scope.is_empty() {
            return Ok(None);
        }
        let Some(before_identity) = process_identity(pid)? else {
            return Ok(None);
        };
        let Some(before_command) = unix_slot_process_command_line(pid)? else {
            return Ok(None);
        };
        if !unix_slot_command_line_matches(&before_command, state_dir, scope, index, generation)
            || !process_exists(pid)?
        {
            return Ok(None);
        }
        let Some(after_identity) = process_identity(pid)? else {
            return Ok(None);
        };
        let Some(after_command) = unix_slot_process_command_line(pid)? else {
            return Ok(None);
        };
        if before_identity != after_identity || before_command != after_command {
            return Ok(None);
        }
        return Ok(Some(PinnedSlotProcess {
            pid,
            identity: before_identity,
        }));
    }

    #[cfg(not(unix))]
    {
        let _ = (state_dir, slot_id, generation);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "slot process identity checks are unavailable on this platform",
        ))
    }
}

#[cfg(target_os = "linux")]
fn linux_slot_command_line_matches(
    cmdline: &[u8],
    state_dir: &Path,
    scope: &str,
    index: &str,
    generation: Generation,
) -> bool {
    let args: Vec<&[u8]> = cmdline
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect();
    let Some(executable) = args
        .first()
        .and_then(|arg| arg.rsplit(|byte| *byte == b'/').next())
    else {
        return false;
    };
    let executable = String::from_utf8_lossy(executable);
    if !slot_service_executable_name(&executable) {
        return false;
    }
    let state_dir = state_dir.to_string_lossy();
    let generation = generation.0.to_string();
    let expected = [
        b"slot".as_slice(),
        b"--state-dir".as_slice(),
        state_dir.as_bytes(),
        b"--scope".as_slice(),
        scope.as_bytes(),
        b"--slot-index".as_slice(),
        index.as_bytes(),
        b"--generation".as_slice(),
        generation.as_bytes(),
    ];
    args.get(1..)
        .is_some_and(|args| args == expected.as_slice())
}

#[cfg(target_os = "linux")]
fn slot_service_executable_name(name: &str) -> bool {
    name == "velnor-runner"
        || name == "velnorctl"
        || name == "velnor-host"
        || name.starts_with("velnor-runner-")
        || name.starts_with("velnorctl-")
}

#[cfg(target_os = "linux")]
fn pidfd_is_live(pidfd: &std::fs::File) -> io::Result<bool> {
    let mut pollfd = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: poll reads the initialized descriptor record only.
        let result = unsafe { libc::poll(&mut pollfd, 1, 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            return Ok(true);
        }
        if pollfd.revents & libc::POLLIN != 0 {
            return Ok(false);
        }
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("pidfd poll returned unknown flags {:#x}", pollfd.revents),
        ));
    }
}

#[cfg(target_os = "linux")]
fn pidfd_send_signal(pidfd: &std::fs::File, signal: libc::c_int) -> io::Result<bool> {
    loop {
        // SAFETY: pidfd refers to the validated slot actor and remains open
        // across identity checks, liveness checks, and this signal syscall.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ESRCH) => return Ok(false),
            _ => return Err(error),
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_slot_process_command_line(pid: u32) -> io::Result<Option<String>> {
    let output = std::process::Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .output()?;
    if !output.status.success() || output.stdout.is_empty() {
        if process_exists(pid)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("cannot read command line for live pid {pid}"),
            ));
        }
        return Ok(None);
    }
    let command_line = String::from_utf8(output.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .trim()
        .to_owned();
    if command_line.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("empty command line for pid {pid}"),
        ));
    }
    Ok(Some(command_line))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_slot_command_line_matches(
    command_line: &str,
    state_dir: &Path,
    scope: &str,
    index: &str,
    generation: Generation,
) -> bool {
    let service_name_matches = ["velnor-runner", "velnorctl", "velnor-host"]
        .iter()
        .any(|name| command_line.contains(name));
    if !service_name_matches || !command_line.contains(" slot ") {
        return false;
    }
    command_line.contains(&state_dir.to_string_lossy().to_string())
        && command_line.contains(&format!("--scope {scope}"))
        && command_line.contains(&format!("--slot-index {index}"))
        && command_line.contains(&format!("--generation {}", generation.0))
}

/// Prove that a recorded process identity is still the process named by this
/// PID. Errors are unknown evidence and callers must retain the owner.
pub fn process_matches_identity(pid: u32, expected: &str) -> io::Result<bool> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Ok(false);
    }
    let Some(current) = process_identity(pid)? else {
        return Ok(false);
    };
    if !process_identity_matches(pid, expected, &current, None)? {
        return Ok(false);
    }
    process_is_live_with_identity(pid, expected, None)
}

#[cfg(unix)]
fn process_exists(pid: u32) -> io::Result<bool> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid does not fit pid_t"))?;
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Err(error),
        _ => Err(error),
    }
}

#[cfg(not(unix))]
fn process_exists(_pid: u32) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "process liveness checks are unavailable on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn process_is_live_with_identity(
    pid: u32,
    expected: &str,
    launch_token: Option<&str>,
) -> io::Result<bool> {
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
    if raw_fd < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(false);
        }
        return Err(error);
    }
    let pidfd = unsafe { std::fs::File::from_raw_fd(raw_fd as i32) };

    // The pidfd pins one process identity. Re-read identity and token after
    // opening it so a PID reused before the syscall cannot pass as the owner.
    let Some(current) = process_identity(pid)? else {
        return Ok(false);
    };
    if !process_identity_matches(pid, expected, &current, launch_token)? {
        return Ok(false);
    }

    let mut pollfd = libc::pollfd {
        fd: pidfd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut pollfd, 1, 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            return Ok(true);
        }
        if pollfd.revents & libc::POLLIN != 0 {
            return Ok(false);
        }
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("pidfd poll returned unknown flags {:#x}", pollfd.revents),
        ));
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_is_live_with_identity(
    pid: u32,
    expected: &str,
    launch_token: Option<&str>,
) -> io::Result<bool> {
    // Non-Linux targets lack pidfd. Bound the PID signal probe on both sides
    // with stable process-start and command-line identity checks; a changed
    // identity proves the recorded owner is gone, while any read/signal error
    // remains unknown.
    let Some(before) = process_identity(pid)? else {
        return Ok(false);
    };
    if !process_identity_matches(pid, expected, &before, launch_token)? {
        return Ok(false);
    }
    if !process_exists(pid)? {
        return Ok(false);
    }
    let Some(after) = process_identity(pid)? else {
        return Ok(false);
    };
    process_identity_matches(pid, expected, &after, launch_token)
}

#[cfg(not(unix))]
fn process_is_live_with_identity(
    _pid: u32,
    _expected: &str,
    _launch_token: Option<&str>,
) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bound process liveness checks are unavailable on this platform",
    ))
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
    {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("create state directory {}", state_dir.display()))?;
        let state = open_state_directory(state_dir)?;
        sync_state_directory_path(state_dir, &state)
            .context("sync state directory name in its parent")?;
        let parent = open_outbox_parent_at(&state, true)?
            .ok_or_else(|| anyhow::anyhow!("completion outbox parent is missing"))?;
        write_outbox_unix(&state, &parent, &name, payload)?;
    }
    #[cfg(not(unix))]
    {
        let parent = open_outbox_parent(state_dir, true)?
            .ok_or_else(|| anyhow::anyhow!("completion outbox parent is missing"))?;
        write_outbox_portable(&parent, &name, payload)?;
    }
    Ok(path)
}

/// Let controller policy inspect each outbox entry while its state-root and
/// outbox directory handles stay pinned, then remove only the rejected exact
/// entry through those same handles.
pub(crate) fn reconcile_outbox_entries(
    state_dir: &Path,
    mut keep: impl FnMut(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let state = match open_state_directory(state_dir) {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let Some(parent) = open_outbox_parent_at(&state, false)? else {
            return Ok(());
        };
        let mut mount_id = job_directory_mount_id;
        verify_outbox_parent(&state, &parent, &mut mount_id)?;
        let entries = rustix::fs::Dir::read_from(&parent).map_err(io::Error::from)?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io::Error::from)?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            names.push(
                std::str::from_utf8(bytes)
                    .context("completion outbox filename is not UTF-8")?
                    .to_owned(),
            );
        }

        for name in names {
            verify_outbox_parent(&state, &parent, &mut mount_id)?;
            let stat =
                match rustix::fs::statat(&parent, Path::new(&name), AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(stat) => stat,
                    Err(error) if error == rustix::io::Errno::NOENT => continue,
                    Err(error) => return Err(io::Error::from(error).into()),
                };
            if outbox_quarantine_pid(&name)?.is_some() {
                if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
                    anyhow::bail!("completion outbox quarantine is not a directory: {name}");
                }
                let _ = remove_stale_outbox_quarantine_at(&state, &parent, &name, &mut mount_id)?;
                continue;
            }
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                anyhow::bail!("completion outbox entry is not a regular file: {name}");
            }
            if keep(&name)? {
                continue;
            }
            let path = if name.starts_with("..") {
                let (job_id, generation) = temporary_outbox_job(&name)?;
                outbox_path_from_parts(&job_id, generation)
            } else {
                let (job_id, generation) = final_outbox_parts(&name)?;
                outbox_path_from_parts(&job_id, generation)
            };
            remove_outbox_entry_unix_with_mount_id(&state, &parent, &name, &path, &mut mount_id)?;
        }
        verify_outbox_parent(&state, &parent, &mut mount_id)?;
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        let Some(parent) = open_outbox_parent(state_dir, false)? else {
            return Ok(());
        };
        for entry in std::fs::read_dir(&parent)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("completion outbox filename is not UTF-8"))?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                anyhow::bail!("completion outbox entry is not a regular file: {name}");
            }
            if keep(&name)? {
                continue;
            }
            let (job_id, generation) = if name.starts_with("..") {
                temporary_outbox_job(&name)?
            } else {
                final_outbox_parts(&name)?
            };
            remove_outbox_portable(&parent, &name, &job_id, generation)?;
        }
        Ok(())
    }
}

fn final_outbox_parts(name: &str) -> anyhow::Result<(String, u64)> {
    let (job_id, generation) = name
        .rsplit_once('.')
        .ok_or_else(|| anyhow::anyhow!("completion outbox filename has no generation: {name}"))?;
    assert_safe_id(job_id)?;
    let generation = generation.parse::<u64>()?;
    Ok((job_id.to_owned(), generation))
}

fn temporary_outbox_job(name: &str) -> anyhow::Result<(String, u64)> {
    let stem = name
        .strip_prefix("..")
        .ok_or_else(|| anyhow::anyhow!("not a completion outbox temporary: {name}"))?;
    let (stem, nonce) = stem
        .rsplit_once(".tmp-")
        .ok_or_else(|| anyhow::anyhow!("malformed completion outbox temporary: {name}"))?;
    let (pid, serial) = nonce
        .split_once('-')
        .ok_or_else(|| anyhow::anyhow!("malformed completion outbox temporary: {name}"))?;
    let pid = pid.parse::<u32>()?;
    let _serial = serial.parse::<u64>()?;
    if pid == 0 {
        anyhow::bail!("malformed completion outbox temporary pid: {name}");
    }
    let (job_id, generation) = stem
        .rsplit_once('.')
        .ok_or_else(|| anyhow::anyhow!("completion outbox filename has no generation: {name}"))?;
    assert_safe_id(job_id)?;
    let generation = generation.parse::<u64>()?;
    Ok((job_id.to_owned(), generation))
}

/// Read a durable completion payload with exact-path and size validation.
///
/// # Errors
/// Invalid id, missing/non-regular/symlink path, filesystem failures, or an
/// oversized payload.
pub fn read_outbox(state_dir: &Path, job_id: &str, generation: u64) -> anyhow::Result<Vec<u8>> {
    assert_safe_id(job_id)?;
    let name = outbox_name(job_id, generation);
    #[cfg(unix)]
    {
        let state = open_state_directory(state_dir)?;
        let parent = open_outbox_parent_at(&state, false)?
            .ok_or_else(|| anyhow::anyhow!("completion outbox parent is missing"))?;
        return read_outbox_unix(&state, &parent, &name, job_id, generation);
    }
    #[cfg(not(unix))]
    {
        let parent = open_outbox_parent(state_dir, false)?
            .ok_or_else(|| anyhow::anyhow!("completion outbox parent is missing"))?;
        read_outbox_portable(&parent, &name, job_id, generation)
    }
}

/// Delete one acknowledged completion payload and durably publish the
/// directory update. Missing is already-clean; every other invalid path is a
/// hard error so cleanup cannot silently follow an attacker-controlled path.
///
/// # Errors
/// Invalid id, symlink/non-regular path, or filesystem failures.
pub fn remove_outbox(state_dir: &Path, job_id: &str, generation: u64) -> anyhow::Result<()> {
    assert_safe_id(job_id)?;
    let name = outbox_name(job_id, generation);
    #[cfg(unix)]
    {
        let state = match open_state_directory(state_dir) {
            Ok(state) => state,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let Some(parent) = open_outbox_parent_at(&state, false)? else {
            return Ok(());
        };
        return remove_outbox_unix(&state, &parent, &name, job_id, generation);
    }
    #[cfg(not(unix))]
    {
        let Some(parent) = open_outbox_parent(state_dir, false)? else {
            return Ok(());
        };
        remove_outbox_portable(&parent, &name, job_id, generation)
    }
}

fn outbox_name(job_id: &str, generation: u64) -> String {
    format!("{job_id}.{generation}")
}

fn temporary_outbox_name(name: &str) -> String {
    let serial = NEXT_OUTBOX_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    format!("..{name}.tmp-{}-{serial}", std::process::id())
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

#[cfg(unix)]
fn open_outbox_parent_at(state: &std::fs::File, create: bool) -> io::Result<Option<std::fs::File>> {
    let mut sync_parent =
        |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
    open_outbox_parent_at_with_sync(state, create, &mut sync_parent)
}

#[cfg(unix)]
fn open_outbox_parent_at_with_sync(
    state: &std::fs::File,
    create: bool,
    sync_parent: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> io::Result<Option<std::fs::File>> {
    let mut mount_id = job_directory_mount_id;
    open_outbox_parent_at_with_checks(state, create, sync_parent, &mut mount_id)
}

#[cfg(unix)]
fn open_outbox_parent_at_with_checks(
    state: &std::fs::File,
    create: bool,
    sync_parent: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> io::Result<Option<std::fs::File>> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let outbox_path = Path::new("outbox");
    let fd = match rustix::fs::openat(state, outbox_path, directory_flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT && !create => return Ok(None),
        Err(error) if error == rustix::io::Errno::NOENT => {
            match rustix::fs::mkdirat(state, outbox_path, Mode::from_raw_mode(0o777)) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::EXIST => {}
                Err(error) => return Err(io::Error::from(error)),
            }
            rustix::fs::openat(state, outbox_path, directory_flags, Mode::empty())
                .map_err(io::Error::from)?
        }
        Err(error) => return Err(io::Error::from(error)),
    };
    if FileType::from_raw_mode(rustix::fs::fstat(&fd).map_err(io::Error::from)?.st_mode)
        != FileType::Directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "completion outbox parent is not a directory",
        ));
    }
    let parent: std::fs::File = fd.into();
    verify_outbox_parent(state, &parent, mount_id)?;
    if create {
        // Persist the child-directory entry before any payload is published
        // and its own directory is synced.
        sync_parent(state)?;
        // The path may have moved while the parent was being synced. Keep the
        // returned descriptor usable only while it still names state/outbox.
        verify_outbox_parent(state, &parent, mount_id)?;
    }
    Ok(Some(parent))
}

#[cfg(not(unix))]
fn open_outbox_parent(state_dir: &Path, create: bool) -> anyhow::Result<Option<PathBuf>> {
    if create {
        return ensure_outbox_parent(state_dir).map(Some);
    }
    let parent = state_dir.join("outbox");
    match std::fs::symlink_metadata(&parent) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!(
                "completion outbox parent is not a real directory: {}",
                parent.display()
            )
        }
        Ok(_) => Ok(Some(parent)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
struct OutboxRecoveryLock {
    _file: std::fs::File,
}

#[cfg(unix)]
fn acquire_outbox_recovery_lock(
    state: &std::fs::File,
    nonblocking: bool,
) -> anyhow::Result<Option<OutboxRecoveryLock>> {
    let file = open_regular_file_at(
        state,
        OUTBOX_RECOVERY_LOCK_NAME,
        OFlags::RDWR | OFlags::CREATE,
        Mode::from_raw_mode(0o600),
    )
    .context("open completion outbox recovery lock")?;
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
fn write_outbox_unix(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut sync_parent =
        |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(std::io::Error::from);
    write_outbox_unix_with_sync(state, parent, name, payload, &mut sync_parent)
}

#[cfg(unix)]
fn write_outbox_unix_with_sync(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    payload: &[u8],
    sync_parent: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> anyhow::Result<()> {
    let mut mount_id = job_directory_mount_id;
    write_outbox_unix_with_checks(state, parent, name, payload, sync_parent, &mut mount_id)
}

#[cfg(unix)]
fn write_outbox_unix_with_checks(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    payload: &[u8],
    sync_parent: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<()> {
    verify_outbox_parent(state, parent, mount_id)
        .context("verify completion outbox parent before publishing payload")?;
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
        verify_outbox_parent(state, parent, mount_id)
            .context("completion outbox parent changed before payload publication")?;
        rustix::fs::linkat(
            parent,
            Path::new(&temporary),
            parent,
            Path::new(name),
            AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
        verify_outbox_parent(state, parent, mount_id)
            .context("completion outbox parent changed after payload publication")?;
        verify_outbox_entry_identity(parent, name, &temp_file)
            .context("published completion outbox changed before parent sync")?;
        rustix::fs::unlinkat(parent, Path::new(&temporary), AtFlags::empty())
            .map_err(std::io::Error::from)?;
        verify_outbox_parent(state, parent, mount_id)
            .context("completion outbox parent changed before parent sync")?;
        sync_parent(parent)?;
        verify_outbox_parent(state, parent, mount_id)
            .context("completion outbox parent changed during parent sync")?;
        verify_outbox_entry_identity(parent, name, &temp_file)
            .context("published completion outbox changed during parent sync")?;
        Ok(())
    })();
    if result.is_err() {
        if verify_outbox_parent(state, parent, mount_id).is_ok() {
            let _ = rustix::fs::unlinkat(parent, Path::new(&temporary), AtFlags::empty());
        }
    }
    result
}

/// Open a regular outbox entry without following symlinks. The no-follow
/// preflight rejects existing FIFOs and devices before opening them; the
/// nonblocking open also closes the FIFO replacement race. A device driver
/// can still define blocking `open` behavior if a device replaces the entry
/// after the preflight, so this is not a universal open deadline.
#[cfg(unix)]
fn open_regular_outbox_at(parent: &std::fs::File, name: &str) -> anyhow::Result<std::fs::File> {
    open_regular_file_at(parent, name, OFlags::RDONLY, Mode::empty())
        .map_err(anyhow::Error::from)
        .context("open regular completion outbox entry")
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
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut sync_parent =
        |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(std::io::Error::from);
    read_outbox_unix_with_sync(state, parent, name, job_id, generation, &mut sync_parent)
}

#[cfg(unix)]
fn read_outbox_unix_with_sync(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
    sync_parent: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
) -> anyhow::Result<Vec<u8>> {
    let mut mount_id = job_directory_mount_id;
    read_outbox_unix_with_checks(
        state,
        parent,
        name,
        job_id,
        generation,
        sync_parent,
        &mut mount_id,
    )
}

#[cfg(unix)]
fn read_outbox_unix_with_checks(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
    sync_parent: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<Vec<u8>> {
    let path = outbox_path_from_parts(job_id, generation);
    verify_outbox_parent(state, parent, mount_id)
        .context("verify completion outbox parent before opening payload")?;
    let file = open_regular_outbox_at(parent, name)
        .with_context(|| format!("read completion outbox {}", path.display()))?;
    verify_outbox_parent(state, parent, mount_id)
        .context("verify completion outbox parent before syncing payload")?;
    // Another writer may have linked this entry but failed its directory
    // sync. Do not let a caller persist journal intent until this exact file
    // entry has been made durable by the pinned outbox directory.
    sync_parent(parent).context("sync completion outbox parent before accepting payload")?;
    verify_outbox_parent(state, parent, mount_id)
        .context("completion outbox parent changed during parent sync")?;
    verify_outbox_entry_identity(parent, name, &file)
        .context("completion outbox changed before parent sync completed")?;
    let size = file.metadata()?.len();
    if size > MAX_COMPLETION_PAYLOAD_BYTES as u64 {
        anyhow::bail!(
            "completion outbox payload exceeds {} bytes: {}",
            MAX_COMPLETION_PAYLOAD_BYTES,
            path.display()
        );
    }
    let payload = read_bounded(file, size, &path)?;
    verify_outbox_parent(state, parent, mount_id)
        .context("completion outbox parent changed while reading payload")?;
    Ok(payload)
}

#[cfg(unix)]
fn verify_outbox_entry_identity(
    parent: &std::fs::File,
    name: &str,
    opened: &std::fs::File,
) -> io::Result<()> {
    let entry = rustix::fs::statat(parent, Path::new(name), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)?;
    let opened = rustix::fs::fstat(opened).map_err(io::Error::from)?;
    if FileType::from_raw_mode(entry.st_mode) != FileType::RegularFile
        || entry.st_dev != opened.st_dev
        || entry.st_ino != opened.st_ino
    {
        return Err(io::Error::other("completion outbox changed during read"));
    }
    Ok(())
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
fn outbox_entry_is_regular(parent: &std::fs::File, name: &str) -> anyhow::Result<bool> {
    match rustix::fs::statat(parent, Path::new(name), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => Ok(true),
        Ok(_) => anyhow::bail!("completion outbox is not a regular file"),
        Err(error) if error == rustix::io::Errno::NOENT => Ok(false),
        Err(error) => Err(io::Error::from(error).into()),
    }
}

#[cfg(unix)]
fn remove_outbox_unix(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    let mut mount_id = job_directory_mount_id;
    remove_outbox_unix_with_mount_id(state, parent, name, job_id, generation, &mut mount_id)
}

#[cfg(unix)]
fn remove_outbox_unix_with_mount_id(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    job_id: &str,
    generation: u64,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<()> {
    let path = outbox_path_from_parts(job_id, generation);
    remove_outbox_entry_unix_with_mount_id(state, parent, name, &path, mount_id)
}

#[cfg(unix)]
fn remove_outbox_entry_unix_with_mount_id(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    path: &Path,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<()> {
    verify_outbox_parent(state, parent, mount_id)
        .context("verify completion outbox parent before removal")?;
    if !outbox_entry_is_regular(parent, name)? {
        return Ok(());
    }
    let _recovery_lock = acquire_outbox_recovery_lock(state, false)?;
    verify_outbox_parent(state, parent, mount_id)
        .context("completion outbox parent changed while acquiring removal lock")?;
    if !outbox_entry_is_regular(parent, name)? {
        return Ok(());
    }
    let original = open_regular_outbox_at(parent, name)
        .with_context(|| format!("inspect completion outbox {}", path.display()))?;
    verify_outbox_entry_identity(parent, name, &original).with_context(|| {
        format!(
            "completion outbox changed before removal: {}",
            path.display()
        )
    })?;
    let (quarantine_name, quarantine) = create_outbox_quarantine_with_mount_id(parent, mount_id)?;
    verify_outbox_parent(state, parent, mount_id)?;
    verify_outbox_quarantine(parent, &quarantine_name, &quarantine, mount_id)?;
    verify_outbox_entry_identity(parent, name, &original)?;
    let moved = match rustix::fs::renameat(
        parent,
        Path::new(name),
        &quarantine,
        Path::new(QUARANTINE_ENTRY_NAME),
    ) {
        Ok(()) => true,
        Err(error) if error == rustix::io::Errno::NOENT => false,
        Err(error) => {
            drop(quarantine);
            remove_outbox_quarantine(state, parent, &quarantine_name, mount_id)?;
            return Err(std::io::Error::from(error).into());
        }
    };
    if !moved {
        drop(quarantine);
        remove_outbox_quarantine(state, parent, &quarantine_name, mount_id)?;
        return Ok(());
    }

    verify_outbox_parent(state, parent, mount_id)?;
    verify_outbox_quarantine(parent, &quarantine_name, &quarantine, mount_id)?;
    let payload = match open_regular_outbox_at(&quarantine, QUARANTINE_ENTRY_NAME) {
        Ok(payload) => payload,
        Err(error) => {
            restore_outbox_entry(state, parent, quarantine, &quarantine_name, name, mount_id)?;
            return Err(error)
                .with_context(|| format!("inspect completion outbox {}", path.display()));
        }
    };
    let original_stat = rustix::fs::fstat(&original).map_err(std::io::Error::from)?;
    let moved_stat = rustix::fs::fstat(&payload).map_err(std::io::Error::from)?;
    if original_stat.st_dev != moved_stat.st_dev || original_stat.st_ino != moved_stat.st_ino {
        drop(payload);
        let error = anyhow::anyhow!(
            "completion outbox changed during removal: {}",
            path.display()
        );
        if let Err(restore_error) =
            restore_outbox_entry(state, parent, quarantine, &quarantine_name, name, mount_id)
        {
            return Err(anyhow::anyhow!("{error}; restore failed: {restore_error}"));
        }
        return Err(error);
    }
    verify_outbox_parent(state, parent, mount_id)?;
    verify_outbox_quarantine(parent, &quarantine_name, &quarantine, mount_id)?;
    verify_outbox_entry_identity(&quarantine, QUARANTINE_ENTRY_NAME, &payload)?;
    drop(payload);
    let unlink_error = rustix::fs::unlinkat(
        &quarantine,
        Path::new(QUARANTINE_ENTRY_NAME),
        AtFlags::empty(),
    )
    .err()
    .map(std::io::Error::from);
    if let Some(error) = unlink_error {
        let error = anyhow::anyhow!("remove completion outbox {}: {error}", path.display());
        if let Err(restore_error) =
            restore_outbox_entry(state, parent, quarantine, &quarantine_name, name, mount_id)
        {
            return Err(anyhow::anyhow!("{error}; restore failed: {restore_error}"));
        }
        return Err(error);
    }
    verify_outbox_parent(state, parent, mount_id)?;
    verify_outbox_quarantine(parent, &quarantine_name, &quarantine, mount_id)?;
    rustix::fs::fsync(&quarantine).map_err(std::io::Error::from)?;
    drop(quarantine);
    remove_outbox_quarantine(state, parent, &quarantine_name, mount_id)?;
    rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
    verify_outbox_parent(state, parent, mount_id)?;
    Ok(())
}

#[cfg(all(unix, test))]
fn create_outbox_quarantine(parent: &std::fs::File) -> anyhow::Result<(String, std::fs::File)> {
    let mut mount_id = job_directory_mount_id;
    create_outbox_quarantine_with_mount_id(parent, &mut mount_id)
}

#[cfg(unix)]
fn create_outbox_quarantine_with_mount_id(
    parent: &std::fs::File,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<(String, std::fs::File)> {
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
                .map_err(|error| std::io::Error::from(error))?;
                let quarantine: std::fs::File = fd.into();
                verify_outbox_quarantine(parent, &name, &quarantine, mount_id)?;
                return Ok((name, quarantine));
            }
            Err(error) if error == rustix::io::Errno::EXIST => continue,
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    anyhow::bail!("could not allocate a unique completion outbox quarantine")
}

#[cfg(unix)]
fn remove_outbox_quarantine(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<()> {
    verify_outbox_parent(state, parent, mount_id)?;
    let quarantine: std::fs::File = rustix::fs::openat(
        parent,
        Path::new(name),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(std::io::Error::from)?
    .into();
    verify_outbox_quarantine(parent, name, &quarantine, mount_id)?;
    drop(quarantine);
    verify_outbox_parent(state, parent, mount_id)?;
    rustix::fs::unlinkat(parent, Path::new(name), AtFlags::REMOVEDIR)
        .map_err(std::io::Error::from)?;
    rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
    verify_outbox_parent(state, parent, mount_id)?;
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
    let state = match open_state_directory(state_dir) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    let Some(parent) = open_outbox_parent_at(&state, false)? else {
        return Ok(true);
    };
    let mut mount_id = job_directory_mount_id;
    remove_stale_outbox_quarantine_at(&state, &parent, name, &mut mount_id)
}

#[cfg(unix)]
fn remove_stale_outbox_quarantine_at(
    state: &std::fs::File,
    parent: &std::fs::File,
    name: &str,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<bool> {
    verify_outbox_parent(state, parent, mount_id)?;
    let initial_quarantine = match rustix::fs::openat(
        parent,
        Path::new(name),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(true),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    verify_outbox_quarantine(parent, name, &initial_quarantine, mount_id)?;
    drop(initial_quarantine);
    let Some(_recovery_lock) = acquire_outbox_recovery_lock(state, true)? else {
        return Ok(false);
    };
    verify_outbox_parent(state, parent, mount_id)?;
    let quarantine = match rustix::fs::openat(
        parent,
        Path::new(name),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => std::fs::File::from(fd),
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(true),
        Err(error) => return Err(std::io::Error::from(error).into()),
    };
    verify_outbox_quarantine(parent, name, &quarantine, mount_id)?;
    let entries = rustix::fs::Dir::read_from(&quarantine)
        .map_err(std::io::Error::from)
        .context("read stale completion outbox quarantine")?;
    let mut has_payload = false;
    for entry in entries {
        verify_outbox_parent(state, parent, mount_id)?;
        verify_outbox_quarantine(parent, name, &quarantine, mount_id)?;
        let entry = entry.map_err(std::io::Error::from)?;
        let entry_name = entry.file_name().to_bytes();
        if entry_name == b"." || entry_name == b".." {
            continue;
        }
        if entry_name != QUARANTINE_ENTRY_NAME.as_bytes() {
            anyhow::bail!("unexpected completion outbox quarantine entry in {}", name);
        }
        let payload = open_regular_outbox_at(&quarantine, QUARANTINE_ENTRY_NAME)
            .context("inspect stale completion outbox quarantine payload")?;
        verify_outbox_parent(state, parent, mount_id)?;
        verify_outbox_quarantine(parent, name, &quarantine, mount_id)?;
        verify_outbox_entry_identity(&quarantine, QUARANTINE_ENTRY_NAME, &payload)?;
        drop(payload);
        has_payload = true;
    }
    if has_payload {
        verify_outbox_parent(state, parent, mount_id)?;
        verify_outbox_quarantine(parent, name, &quarantine, mount_id)?;
        let payload = open_regular_outbox_at(&quarantine, QUARANTINE_ENTRY_NAME)
            .context("reopen stale completion outbox quarantine payload")?;
        verify_outbox_entry_identity(&quarantine, QUARANTINE_ENTRY_NAME, &payload)?;
        rustix::fs::unlinkat(
            &quarantine,
            Path::new(QUARANTINE_ENTRY_NAME),
            AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
    }
    verify_outbox_parent(state, parent, mount_id)?;
    verify_outbox_quarantine(parent, name, &quarantine, mount_id)?;
    rustix::fs::fsync(&quarantine).map_err(std::io::Error::from)?;
    drop(quarantine);
    remove_outbox_quarantine(state, parent, name, mount_id)?;
    rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
    verify_outbox_parent(state, parent, mount_id)?;
    Ok(true)
}

#[cfg(unix)]
fn restore_outbox_entry(
    state: &std::fs::File,
    parent: &std::fs::File,
    quarantine: std::fs::File,
    quarantine_name: &str,
    name: &str,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
) -> anyhow::Result<()> {
    verify_outbox_parent(state, parent, mount_id)?;
    verify_outbox_quarantine(parent, quarantine_name, &quarantine, mount_id)?;
    rustix::fs::renameat_with(
        &quarantine,
        Path::new(QUARANTINE_ENTRY_NAME),
        parent,
        Path::new(name),
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    verify_outbox_parent(state, parent, mount_id)?;
    drop(quarantine);
    remove_outbox_quarantine(state, parent, quarantine_name, mount_id)?;
    rustix::fs::fsync(parent).map_err(std::io::Error::from)?;
    verify_outbox_parent(state, parent, mount_id)?;
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

/// Delete the final ownership marker only after its job directory is durably removed.
///
/// # Errors
/// Invalid isolation id or filesystem failures.
pub fn remove_owned(state_dir: &Path, isolation_id: &str, generation: u64) -> anyhow::Result<()> {
    assert_safe_id(isolation_id)?;
    #[cfg(unix)]
    {
        let state = open_state_directory(state_dir)?;
        return remove_owned_at_root(&state, isolation_id, generation);
    }
    #[cfg(not(unix))]
    {
        let _ = (state_dir, isolation_id, generation);
        anyhow::bail!("no-follow ownership marker removal is unavailable on this platform");
    }
}

#[cfg(unix)]
fn remove_owned_at_root(
    state: &std::fs::File,
    isolation_id: &str,
    generation: u64,
) -> anyhow::Result<()> {
    let mut sync =
        |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
    let mut mount_id = job_directory_mount_id;
    let mut after_open = |_: &std::fs::File, _: &OsStr, _: &std::fs::File| Ok(());
    remove_owned_at_root_with(
        state,
        isolation_id,
        generation,
        &mut sync,
        &mut mount_id,
        &mut after_open,
    )
}

#[cfg(unix)]
fn remove_owned_at_root_with(
    state: &std::fs::File,
    isolation_id: &str,
    generation: u64,
    sync: &mut dyn FnMut(&std::fs::File) -> io::Result<()>,
    mount_id: &mut dyn FnMut(&std::fs::File) -> io::Result<Option<JobMountIdentity>>,
    after_open: &mut dyn FnMut(&std::fs::File, &OsStr, &std::fs::File) -> io::Result<()>,
) -> anyhow::Result<()> {
    let parent = open_owned_parent_at(state)?;
    verify_owned_parent(state, &parent, mount_id)?;
    let _lock = acquire_owned_pid_lock(&parent)?;
    verify_owned_parent(state, &parent, mount_id)?;
    let name = owned_name(isolation_id, generation);
    let marker_exists = owned_entry_is_regular(&parent, &name)?;
    let record = if marker_exists {
        Some(
            read_owned_record_from_parent(&parent, &name)?
                .ok_or_else(|| anyhow::anyhow!("ownership marker disappeared during cleanup"))?,
        )
    } else {
        None
    };
    let has_other_generation = owned_generation_exists_other_than(&parent, isolation_id, &name)?;

    if !has_other_generation {
        if let Some(mut record) = record {
            let evidence = record.job_directory.as_ref().ok_or_else(|| {
                anyhow::anyhow!("ownership marker has no durable job directory identity")
            })?;
            if !record.cleanup_complete {
                let jobs_parent = open_jobs_parent_at(state, false)?.ok_or_else(|| {
                    anyhow::anyhow!("jobs parent is missing while ownership cleanup intent exists")
                })?;
                verify_jobs_parent(state, &jobs_parent, mount_id)?;
                let jobs_stat = rustix::fs::fstat(&jobs_parent).map_err(io::Error::from)?;
                if jobs_stat.st_dev as u64 != evidence.jobs_device
                    || jobs_stat.st_ino != evidence.jobs_inode
                {
                    anyhow::bail!(
                        "jobs parent identity changed while ownership cleanup intent exists"
                    );
                }

                match stat_job_entry(&jobs_parent, OsStr::new(isolation_id), sync)? {
                    Some(job_stat) => {
                        if FileType::from_raw_mode(job_stat.st_mode) != FileType::Directory
                            || job_stat.st_dev as u64 != evidence.job_device
                            || job_stat.st_ino != evidence.job_inode
                        {
                            anyhow::bail!("job directory identity changed while ownership cleanup intent exists");
                        }
                        if !record.cleanup_started {
                            record.cleanup_started = true;
                            replace_owned_intent(&parent, &name, &record)?;
                        }
                        // The ownership marker remains durable cleanup intent
                        // until the tree and parent update are durably removed.
                        remove_job_entry_with(
                            state,
                            &jobs_parent,
                            OsStr::new(isolation_id),
                            sync,
                            mount_id,
                            after_open,
                        )?;
                        sync_with_retry(&jobs_parent, sync)?;
                    }
                    None if record.cleanup_started => {
                        // A previous pass may have removed the job entry and
                        // crashed before publishing completion. Syncing the
                        // pinned parent makes that deletion durable now.
                        sync_with_retry(&jobs_parent, sync)?;
                    }
                    None => {
                        anyhow::bail!("job directory is missing before durable cleanup intent");
                    }
                }

                record.cleanup_started = true;
                record.cleanup_complete = true;
                verify_owned_parent(state, &parent, mount_id)?;
                replace_owned_intent(&parent, &name, &record)?;
            } else {
                // Completion evidence permits marker removal after the jobs
                // tree was durably deleted, even if jobs/ is later moved.
                if let Some(jobs_parent) = open_jobs_parent_at(state, false)?
                    && stat_job_entry(&jobs_parent, OsStr::new(isolation_id), sync)?.is_some()
                {
                    anyhow::bail!("job directory reappeared after durable cleanup completion");
                }
            }
        } else if let Some(jobs_parent) = open_jobs_parent_at(state, false)? {
            // A claimed but not-yet-published job has no ownership marker.
            // Keep the historical exact-id cleanup behavior for that case.
            remove_job_entry_with(
                state,
                &jobs_parent,
                OsStr::new(isolation_id),
                sync,
                mount_id,
                after_open,
            )?;
            sync_with_retry(&jobs_parent, sync)?;
        }
    }

    verify_owned_parent(state, &parent, mount_id)?;
    match rustix::fs::unlinkat(&parent, Path::new(&name), AtFlags::empty()) {
        Ok(()) => sync_with_retry(&parent, sync)?,
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(io::Error::from(error).into()),
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::FileTypeExt;

    fn tmp_uninitialized(label: &str) -> PathBuf {
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

    fn tmp(label: &str) -> PathBuf {
        let path = tmp_uninitialized(label);
        initialize_owned_directory(&path).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn slot_identity_pin_rejects_an_unrelated_process() {
        let dir = tmp_uninitialized("slot-identity-unrelated");
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();

        assert!(pin_slot_process(
            child.id(),
            &dir,
            &SlotId("velnor-1".to_owned()),
            Generation::INITIAL,
        )
        .unwrap()
        .is_none());
        assert!(child.try_wait().unwrap().is_none());

        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pidfd_signal_reaches_the_pinned_process() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let raw_fd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as libc::pid_t, 0u32) };
        assert!(
            raw_fd >= 0,
            "pidfd_open failed: {}",
            io::Error::last_os_error()
        );
        // SAFETY: successful pidfd_open returns an owned descriptor.
        let pidfd = unsafe { std::fs::File::from_raw_fd(raw_fd as i32) };

        assert!(pidfd_send_signal(&pidfd, libc::SIGTERM).unwrap());
        assert!(!child.wait().unwrap().success());
        assert!(!pidfd_is_live(&pidfd).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn state_root_symlink_with_trailing_components_is_rejected() {
        let target = tmp_uninitialized("state-root-symlink-target");
        let link = tmp_uninitialized("state-root-symlink-link");
        std::fs::remove_dir(&link).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut trailing_separator = link.as_os_str().to_os_string();
        trailing_separator.push("/");
        let paths = [PathBuf::from(trailing_separator), link.join(".")];
        for path in paths {
            assert!(
                open_state_directory(&path).is_err(),
                "terminal path syntax must not hide a symlink: {}",
                path.display()
            );
            assert!(
                initialize_owned_directory(&path).is_err(),
                "startup must reject a state-root symlink: {}",
                path.display()
            );
        }
        assert!(open_state_directory(&target).is_ok());
        assert!(!target.join("owned").exists());
        assert!(!target.join(OWNED_INITIALIZATION_MARKER_NAME).exists());

        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir_all(target).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ownership_initialization_reopens_directory_after_concurrent_mkdir() {
        let dir = tmp_uninitialized("owned-init-mkdir-eexist");
        let state = open_state_directory(&dir).unwrap();
        let mut sync_state =
            |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
        let mut mkdir_racing_initializer = |directory: &std::fs::File, name: &Path, mode: Mode| {
            rustix::fs::mkdirat(directory, name, mode)?;
            Err(rustix::io::Errno::EXIST)
        };

        ensure_owned_directory_at_with_sync_and_mkdir(
            &state,
            &mut sync_state,
            &mut mkdir_racing_initializer,
        )
        .unwrap();

        assert!(dir.join("owned").is_dir());
        assert!(dir.join(OWNED_INITIALIZATION_MARKER_NAME).is_file());
        verify_owned_initialization_marker(&state, &open_owned_parent_entry_at(&state).unwrap())
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn job_directory_entries_are_synced_before_owner_intent_can_be_published() {
        for failed_sync in [None, Some(1), Some(2)] {
            let dir = tmp(&format!("job-directory-fsync-{failed_sync:?}"));
            let state = open_state_directory(&dir).unwrap();
            let state_stat = rustix::fs::fstat(&state).unwrap();
            let state_identity = (state_stat.st_dev as u64, state_stat.st_ino);
            let marker = owned_path(&dir, "job-fsync", 1);
            let mut syncs = Vec::new();
            let mut sync_directory = |directory: &std::fs::File| {
                assert!(
                    !marker.exists(),
                    "ownership intent must follow every directory fsync"
                );
                let stat = rustix::fs::fstat(directory).map_err(io::Error::from)?;
                syncs.push((stat.st_dev as u64, stat.st_ino));
                if failed_sync == Some(syncs.len()) {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "injected directory fsync failure",
                    ));
                }
                rustix::fs::fsync(directory).map_err(io::Error::from)
            };
            let mut mount_id = job_directory_mount_id;
            let ensured = ensure_job_directory_at_with_sync(
                &state,
                "job-fsync",
                &mut mount_id,
                &mut sync_directory,
            );

            match failed_sync {
                Some(_) => {
                    assert!(ensured.is_err());
                    assert!(
                        !marker.exists(),
                        "fsync failure must keep ownership intent unpublished"
                    );
                }
                None => {
                    assert!(ensured.unwrap().is_some());
                    let jobs = std::fs::File::open(dir.join("jobs")).unwrap();
                    let jobs_stat = rustix::fs::fstat(&jobs).unwrap();
                    assert_eq!(
                        syncs,
                        vec![state_identity, (jobs_stat.st_dev as u64, jobs_stat.st_ino)],
                        "sync state/jobs entries before publishing ownership"
                    );
                    write_owned_pid_intent(&dir, "job-fsync", 1).unwrap();
                    assert!(marker.is_file());
                }
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    fn create_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;

        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let result = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(result, 0, "create fifo: {}", io::Error::last_os_error());
    }

    #[cfg(target_os = "linux")]
    fn create_character_device(path: &Path) -> io::Result<()> {
        let parent = open_state_directory(path.parent().expect("device path has a parent"))?;
        rustix::fs::mknodat(
            &parent,
            path.file_name().expect("device path has a name"),
            FileType::CharacterDevice,
            Mode::from_raw_mode(0o600),
            rustix::fs::makedev(1, 3),
        )
        .map_err(io::Error::from)
    }

    #[cfg(unix)]
    fn run_fifo_worker(mode: &str, state_dir: &Path, quarantine_name: Option<&str>) {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "node::cleanup::tests::fifo_deadline_child_entrypoint",
                "--nocapture",
            ])
            .env("VELNOR_CLEANUP_FIFO_MODE", mode)
            .env("VELNOR_CLEANUP_FIFO_STATE_DIR", state_dir);
        if let Some(name) = quarantine_name {
            command.env("VELNOR_CLEANUP_FIFO_QUARANTINE", name);
        }
        let mut child = command.spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match child.try_wait().unwrap() {
                Some(status) => {
                    assert!(status.success(), "FIFO {mode} worker failed: {status}");
                    return;
                }
                None if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                None => {
                    child.kill().unwrap();
                    let _ = child.wait();
                    panic!("FIFO {mode} worker blocked past the two-second deadline");
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn fifo_deadline_child_entrypoint() {
        let Ok(mode) = std::env::var("VELNOR_CLEANUP_FIFO_MODE") else {
            return;
        };
        let state_dir = PathBuf::from(std::env::var_os("VELNOR_CLEANUP_FIFO_STATE_DIR").unwrap());
        let result = match mode.as_str() {
            "read" => read_outbox(&state_dir, "fifo-job", 1).map(|_| ()),
            "remove" => remove_outbox(&state_dir, "fifo-job", 1),
            "owned-lock" => read_owned_pid(&state_dir, "fifo-job", 1)
                .map(|_| ())
                .map_err(anyhow::Error::from),
            "outbox-lock" => remove_outbox(&state_dir, "fifo-job", 1),
            "stale" => remove_stale_outbox_quarantine(
                &state_dir,
                &std::env::var("VELNOR_CLEANUP_FIFO_QUARANTINE").unwrap(),
            )
            .map(|_| ()),
            _ => panic!("unknown FIFO worker mode: {mode}"),
        };
        assert!(
            result.is_err(),
            "FIFO {mode} operation must reject non-regular entries"
        );
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

    #[cfg(unix)]
    #[test]
    fn missing_jobs_parent_preserves_existing_cleanup_intent() {
        let dir = tmp("missing-jobs-cleanup-intent");
        claim_owned(&dir, "job-1", 1).unwrap();
        write_owned_pid_intent(&dir, "job-1", 1).unwrap();
        let marker = owned_path(&dir, "job-1", 1);
        let moved_jobs = dir.with_file_name(format!(
            "{}-moved-jobs",
            dir.file_name().unwrap().to_string_lossy()
        ));
        std::fs::rename(dir.join("jobs"), &moved_jobs).unwrap();

        assert!(remove_owned(&dir, "job-1", 1).is_err());
        assert!(marker.is_file(), "failed cleanup must retain its intent");
        assert!(moved_jobs.join("job-1").is_dir());
        assert!(!dir.join("jobs").exists());

        std::fs::remove_dir_all(moved_jobs).ok();
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_retries_after_tree_delete_before_completion_marker_update() {
        let dir = tmp("cleanup-completion-recovery");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let parent = open_owned_parent(&dir).unwrap();
        let name = owned_name("job-1", 1);
        let mut record = read_owned_record_from_parent(&parent, &name)
            .unwrap()
            .unwrap();
        record.cleanup_started = true;
        replace_owned_intent(&parent, &name, &record).unwrap();
        std::fs::remove_dir_all(job_dir(&dir, "job-1")).unwrap();

        assert!(owned_path(&dir, "job-1", 1).is_file());
        remove_owned(&dir, "job-1", 1).unwrap();
        assert!(!owned_path(&dir, "job-1", 1).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn claim_owned_does_not_publish_empty_marker() {
        let dir = tmp("claim");
        let owned = claim_owned(&dir, "job-1", 1).unwrap();
        assert!(!owned.exists());
        assert_eq!(read_owned_pid(&dir, "job-1", 1).unwrap(), None);
        assert!(job_dir(&dir, "job-1").is_dir());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn missing_owned_parent_is_not_reported_as_a_missing_pid_marker() {
        let dir = tmp_uninitialized("missing-owned-parent");
        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        initialize_owned_directory(&dir).unwrap();
        assert_eq!(read_owned_pid(&dir, "job-1", 1).unwrap(), None);
        // Reading creates the persistent directory lock inside `owned/`.
        let owned_parent = dir.join("owned");
        std::fs::remove_file(owned_parent.join(OWNED_PID_LOCK_NAME)).unwrap();
        std::fs::remove_dir(owned_parent).unwrap();
        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        assert!(claim_owned(&dir, "job-1", 1).is_err());
        assert!(!dir.join("owned").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn initialize_owned_directory_creates_it_for_fresh_state() {
        let dir = tmp_uninitialized("owned-fresh-init");
        initialize_owned_directory(&dir).unwrap();
        assert!(dir.join("owned").is_dir());
        assert!(dir.join(OWNED_INITIALIZATION_MARKER_NAME).is_file());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn initialize_owned_directory_allows_fresh_execution_configuration() {
        let dir = tmp_uninitialized("owned-fresh-config-init");
        std::fs::write(
            dir.join("execution.toml"),
            "[execution]\nbackend = \"docker\"\n",
        )
        .unwrap();

        initialize_owned_directory(&dir).unwrap();

        assert!(dir.join("owned").is_dir());
        assert!(dir.join(OWNED_INITIALIZATION_MARKER_NAME).is_file());
        assert!(dir.join("execution.toml").is_file());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_owned_directory_does_not_match_persistent_initialization_identity() {
        let dir = tmp("owned-replaced-root");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let original = dir.join("owned-original");
        let original_marker = original.join("job-1.1");
        std::fs::remove_file(dir.join("owned").join(OWNED_PID_LOCK_NAME)).unwrap();
        std::fs::rename(dir.join("owned"), &original).unwrap();
        std::fs::create_dir(dir.join("owned")).unwrap();

        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        assert!(claim_owned(&dir, "job-2", 1).is_err());
        assert!(initialize_owned_directory(&dir).is_err());
        assert!(
            original_marker.is_file(),
            "old ownership evidence is preserved"
        );
        assert!(
            std::fs::read_dir(dir.join("owned"))
                .unwrap()
                .next()
                .is_none(),
            "replacement owned/ remains empty"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn state_root_path_syncs_every_ancestor_entry() {
        let base = tmp("state-root-parent-sync");
        let state_path = base.join("new-parent").join("nested").join("state");
        std::fs::create_dir_all(&state_path).unwrap();
        let state = open_state_directory(&state_path).unwrap();
        let mut sync_count = 0;
        let expected_syncs = std::fs::canonicalize(&state_path)
            .unwrap()
            .components()
            .filter(|component| matches!(component, std::path::Component::Normal(_)))
            .count();

        sync_state_directory_path_with(&state_path, &state, &mut |parent| {
            let actual = rustix::fs::fstat(parent).map_err(io::Error::from)?;
            assert_eq!(actual.st_mode & libc::S_IFMT, libc::S_IFDIR);
            sync_count += 1;
            rustix::fs::fsync(parent).map_err(io::Error::from)
        })
        .unwrap();

        assert_eq!(sync_count, expected_syncs);
        std::fs::remove_dir_all(base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn owned_directory_is_synced_before_initialization_marker_is_published() {
        let dir = tmp_uninitialized("owned-init-sync-order");
        let state = open_state_directory(&dir).unwrap();
        let mut state_syncs = 0;
        ensure_owned_directory_at_with_sync(&state, &mut |directory| {
            let state_stat = rustix::fs::fstat(&state).map_err(io::Error::from)?;
            let synced_stat = rustix::fs::fstat(directory).map_err(io::Error::from)?;
            assert_eq!(state_stat.st_dev, synced_stat.st_dev);
            assert_eq!(state_stat.st_ino, synced_stat.st_ino);
            assert!(
                rustix::fs::statat(directory, Path::new("owned"), AtFlags::SYMLINK_NOFOLLOW,)
                    .is_ok()
            );
            let marker_exists = rustix::fs::statat(
                directory,
                Path::new(OWNED_INITIALIZATION_MARKER_NAME),
                AtFlags::SYMLINK_NOFOLLOW,
            )
            .is_ok();
            match state_syncs {
                0 => assert!(!marker_exists, "owned/ must sync before marker creation"),
                1 => assert!(marker_exists, "marker must sync after owned/"),
                _ => panic!("unexpected state-directory sync"),
            }
            state_syncs += 1;
            rustix::fs::fsync(directory).map_err(io::Error::from)
        })
        .unwrap();
        assert_eq!(state_syncs, 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn owned_directory_replacement_during_fresh_init_does_not_publish_marker() {
        let dir = tmp_uninitialized("owned-replaced-during-init");
        let state = open_state_directory(&dir).unwrap();
        let moved = dir.join("owned-original");
        let mut replaced = false;

        let result = ensure_owned_directory_at_with_sync(&state, &mut |directory| {
            if !replaced {
                std::fs::rename(dir.join("owned"), &moved)?;
                std::fs::create_dir(dir.join("owned"))?;
                replaced = true;
            }
            rustix::fs::fsync(directory).map_err(io::Error::from)
        });

        assert!(result.is_err());
        assert!(replaced);
        assert!(!dir.join(OWNED_INITIALIZATION_MARKER_NAME).exists());
        assert!(moved.is_dir());
        assert!(dir.join("owned").is_dir());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn missing_owned_after_initialization_fails_closed_when_root_is_otherwise_empty() {
        let dir = tmp("owned-lost-init");
        let owned = dir.join("owned");
        let marker = dir.join(OWNED_INITIALIZATION_MARKER_NAME);
        std::fs::remove_dir_all(&owned).unwrap();

        let remaining = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(
            remaining,
            vec![std::ffi::OsString::from(OWNED_INITIALIZATION_MARKER_NAME)]
        );
        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        assert!(claim_owned(&dir, "job-1", 1).is_err());
        assert!(write_owned_pid(&dir, "job-1", 1, std::process::id()).is_err());
        assert!(initialize_owned_directory(&dir).is_err());
        assert!(!owned.exists(), "failed startup must not recreate owned/");
        assert!(marker.is_file(), "durable initialization evidence remains");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn initialize_owned_directory_fails_closed_when_journal_exists() {
        let dir = tmp_uninitialized("owned-missing-journal");
        std::fs::write(dir.join("journal.db"), b"durable journal").unwrap();
        assert!(initialize_owned_directory(&dir).is_err());
        assert!(!dir.join("owned").exists());
        assert!(claim_owned(&dir, "job-1", 1).is_err());
        assert!(!dir.join("owned").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn write_owned_pid_publishes_exact_contents() {
        let dir = tmp("pid");
        let pid = std::process::id();
        write_owned_pid(&dir, "job-1", 1, pid).unwrap();
        let record: OwnedPidRecord =
            serde_json::from_slice(&std::fs::read(owned_path(&dir, "job-1", 1)).unwrap()).unwrap();
        assert_eq!(record.pid, pid);
        assert!(record.process_identity.is_some());
        assert_eq!(record.launch_token, None);
        #[cfg(target_os = "linux")]
        assert_eq!(
            owned_pid_liveness(&dir, "job-1", 1).unwrap(),
            OwnedPidLiveness::Live
        );
        #[cfg(target_os = "linux")]
        assert_eq!(read_owned_pid(&dir, "job-1", 1).unwrap(), Some(pid));
        #[cfg(all(unix, not(target_os = "linux")))]
        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    #[test]
    fn compact_process_identity_keeps_pid_start_and_launch_token_bound() {
        let token = "launch-123";
        let identity =
            "unix-ps:Mon Sep 21 01:02:03 2026 /opt/velnor-runner job --launch-token launch-123";
        let persisted = persisted_process_identity(identity, token);
        assert!(persisted.len() < MAX_OWNED_PID_BYTES);
        assert!(process_identity_matches(42, &persisted, identity, Some(token)).unwrap());
        assert!(!process_identity_matches(
            42,
            &persisted,
            "unix-ps:Mon Sep 21 01:02:04 2026 /opt/velnor-runner job --launch-token launch-123",
            Some(token)
        )
        .unwrap());
        assert!(
            !process_identity_matches(42, &persisted, identity, Some("replacement-token")).unwrap()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn child_atomically_replaces_only_its_matching_spawn_intent() {
        let dir = tmp("pid-intent");
        let token = write_owned_pid_intent(&dir, "job-1", 1).unwrap();
        assert_eq!(
            read_owned_pid(&dir, "job-1", 1).unwrap(),
            None,
            "no process carries the durable token before spawn"
        );
        write_owned_pid_for_intent(&dir, "job-1", 1, std::process::id(), &token).unwrap();
        let parent = open_owned_parent(&dir).unwrap();
        let record = read_owned_record_from_parent(&parent, &owned_name("job-1", 1))
            .unwrap()
            .unwrap();
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.launch_token.as_deref(), Some(token.as_str()));
        assert!(record.process_identity.is_some());
        assert!(write_owned_pid_for_intent(&dir, "job-1", 1, std::process::id(), &token).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn retry_replaces_only_absent_or_proven_dead_owned_marker() {
        let dir = tmp("pid-intent-replace-dead");
        let current_pid = std::process::id();
        let current_identity = process_identity(current_pid).unwrap().unwrap();
        write_owned_record_exclusive(
            &open_owned_parent(&dir).unwrap(),
            &owned_name("job-live", 1),
            &OwnedPidRecord {
                pid: current_pid,
                process_identity: Some(current_identity.clone()),
                launch_token: None,
                job_directory: None,
                cleanup_started: false,
                cleanup_complete: false,
            },
        )
        .unwrap();
        assert_eq!(
            replace_dead_owned_pid_with_intent(&dir, "job-live", 1).unwrap(),
            None,
            "a live generation keeps its marker"
        );
        assert_eq!(
            owned_pid_liveness(&dir, "job-live", 1).unwrap(),
            OwnedPidLiveness::Live
        );

        let job_directory = claim_owned(&dir, "job-dead", 2)
            .map(|_| open_state_directory(&dir).unwrap())
            .and_then(|state| ensure_job_directory_at(&state, "job-dead"))
            .unwrap();
        write_owned_record_exclusive(
            &open_owned_parent(&dir).unwrap(),
            &owned_name("job-dead", 2),
            &OwnedPidRecord {
                pid: current_pid,
                process_identity: Some("different-process-start-identity".into()),
                launch_token: None,
                job_directory,
                cleanup_started: false,
                cleanup_complete: false,
            },
        )
        .unwrap();
        let token = replace_dead_owned_pid_with_intent(&dir, "job-dead", 2)
            .unwrap()
            .expect("a process with another start identity is proven dead");
        let parent = open_owned_parent(&dir).unwrap();
        let record = read_owned_record_from_parent(&parent, &owned_name("job-dead", 2))
            .unwrap()
            .unwrap();
        assert_eq!(record.pid, 0);
        assert_eq!(record.launch_token.as_deref(), Some(token.as_str()));
        assert!(
            write_owned_pid_for_intent(&dir, "job-dead", 2, current_pid, "stale-token").is_err()
        );
        write_owned_pid_for_intent(&dir, "job-dead", 2, current_pid, &token).unwrap();

        let unresolved = write_owned_pid_intent(&dir, "job-unresolved", 3).unwrap();
        let retry = replace_dead_owned_pid_with_intent(&dir, "job-unresolved", 3)
            .unwrap()
            .expect("no process carries an unpublished intent's token");
        assert_ne!(unresolved, retry);
        let unresolved_record = read_owned_record_from_parent(
            &open_owned_parent(&dir).unwrap(),
            &owned_name("job-unresolved", 3),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            unresolved_record.launch_token.as_deref(),
            Some(retry.as_str())
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn launch_token_process_match_requires_a_full_argument() {
        assert!(argv_contains_launch_token(
            b"/opt/runner\0job\0--launch-token\0token-123\0",
            "token-123"
        ));
        assert!(argv_contains_launch_token(
            b"/opt/runner\0job\0--launch-token=token-123\0",
            "token-123"
        ));
        assert!(!argv_contains_launch_token(
            b"/opt/runner\0job\0--launch-token\0token-1234\0",
            "token-123"
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_process_inventory_command_uses_valid_bsd_output_format() {
        let output = process_inventory_command()
            .output()
            .expect("macOS ps must run for process inventory");
        assert!(
            output.status.success(),
            "macOS ps rejected the process inventory arguments: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_prepublication_child_keeps_its_intent_until_exit() {
        let dir = tmp("pid-intent-child-live");
        let token = write_owned_pid_intent(&dir, "job-1", 1).unwrap();
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("trap 'exit 0' TERM; while :; do sleep 1; done")
            .arg("worker")
            .arg(&token)
            .spawn()
            .unwrap();
        assert!(process_has_launch_token(&token).unwrap());
        assert_eq!(
            owned_pid_liveness(&dir, "job-1", 1).unwrap(),
            OwnedPidLiveness::UnpublishedIntentLive,
            "a process with this launch token owns the slot before PID publication"
        );
        assert_eq!(
            read_owned_pid(&dir, "job-1", 1).unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "a live PID-zero intent has no published PID to return"
        );
        assert_eq!(
            replace_dead_owned_pid_with_intent(&dir, "job-1", 1).unwrap(),
            None,
            "a child carrying the intent token must prevent duplicate spawn"
        );

        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            owned_pid_liveness(&dir, "job-1", 1).unwrap(),
            OwnedPidLiveness::Dead,
            "an intent becomes reclaimable only when its exact token disappears"
        );
        assert!(replace_dead_owned_pid_with_intent(&dir, "job-1", 1)
            .unwrap()
            .is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_child_token_cannot_replace_a_retried_spawn_intent() {
        let dir = tmp("pid-intent-retry");
        let stale_token = write_owned_pid_intent(&dir, "job-1", 1).unwrap();
        remove_owned(&dir, "job-1", 1).unwrap();
        let retry_token = write_owned_pid_intent(&dir, "job-1", 1).unwrap();
        assert_ne!(stale_token, retry_token);

        assert!(
            write_owned_pid_for_intent(&dir, "job-1", 1, std::process::id(), &stale_token).is_err()
        );
        let parent = open_owned_parent(&dir).unwrap();
        let record = read_owned_record_from_parent(&parent, &owned_name("job-1", 1))
            .unwrap()
            .unwrap();
        assert_eq!(record.pid, 0);
        assert_eq!(record.launch_token.as_deref(), Some(retry_token.as_str()));

        write_owned_pid_for_intent(&dir, "job-1", 1, std::process::id(), &retry_token).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn live_reused_pid_is_dead_when_start_identity_differs() {
        let dir = tmp("pid-reuse");
        claim_owned(&dir, "job-1", 1).unwrap();
        let current = std::process::id();
        let launch_token = "stale-launch-token";
        let job_directory =
            ensure_job_directory_at(&open_state_directory(&dir).unwrap(), "job-1").unwrap();
        let parent = open_owned_parent(&dir).unwrap();
        write_owned_record_exclusive(
            &parent,
            &owned_name("job-1", 1),
            &OwnedPidRecord {
                pid: current,
                process_identity: Some(persisted_process_identity(
                    "different-process-start-identity",
                    launch_token,
                )),
                launch_token: Some(launch_token.into()),
                job_directory,
                cleanup_started: false,
                cleanup_complete: false,
            },
        )
        .unwrap();
        assert_eq!(
            owned_pid_liveness(&dir, "job-1", 1).unwrap(),
            OwnedPidLiveness::Dead,
            "a live PID with another process start identity is not this owner"
        );
        assert_eq!(read_owned_pid(&dir, "job-1", 1).unwrap(), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn process_identity_adoption_pins_pid_and_rejects_reuse() {
        let pid = std::process::id();
        let identity = process_identity(pid).unwrap().unwrap();
        assert!(process_matches_identity(pid, &identity).unwrap());
        assert!(!process_matches_identity(pid, "different-process-start-identity").unwrap());
    }

    #[test]
    fn malformed_and_oversized_owned_pids_are_errors() {
        let dir = tmp("pid-bounded");
        std::fs::create_dir_all(dir.join("owned")).unwrap();
        std::fs::write(
            owned_path(&dir, "job-1", 1),
            vec![b'7'; MAX_OWNED_PID_BYTES + 1],
        )
        .unwrap();
        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        std::fs::write(owned_path(&dir, "job-1", 1), b"not-a-pid").unwrap();
        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        std::fs::write(owned_path(&dir, "job-1", 1), b"0").unwrap();
        assert!(read_owned_pid(&dir, "job-1", 1).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_non_file_owned_pids_are_errors() {
        let dir = tmp("pid-invalid-path");
        std::fs::create_dir_all(dir.join("owned")).unwrap();
        std::fs::write(dir.join("owned/target"), b"42").unwrap();
        std::os::unix::fs::symlink(dir.join("owned/target"), owned_path(&dir, "job-link", 1))
            .unwrap();
        assert!(read_owned_pid(&dir, "job-link", 1).is_err());

        std::fs::create_dir(owned_path(&dir, "job-directory", 1)).unwrap();
        assert!(read_owned_pid(&dir, "job-directory", 1).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn path_separator_id_is_rejected() {
        let dir = tmp("slash");
        assert!(claim_owned(&dir, "../etc", 1).is_err());
        assert!(claim_owned(&dir, ".job", 1).is_err());
        assert!(claim_owned(&dir, ".", 1).is_err());
        assert!(claim_owned(&dir, "..", 1).is_err());
        assert!(read_owned_pid(&dir, "../etc", 1).is_err());
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

    #[cfg(unix)]
    #[test]
    fn symlinked_jobs_parent_preserves_external_sentinel_and_owned_marker() {
        let dir = tmp("jobs-parent-symlink");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let marker = owned_path(&dir, "job-1", 1);
        let original_jobs = dir.join("jobs-original");
        std::fs::rename(dir.join("jobs"), &original_jobs).unwrap();

        let external = dir.join("external");
        let external_job = external.join("job-1");
        std::fs::create_dir_all(&external_job).unwrap();
        let sentinel = external_job.join("sentinel");
        std::fs::write(&sentinel, b"keep").unwrap();
        std::os::unix::fs::symlink(&external, dir.join("jobs")).unwrap();

        assert!(remove_owned(&dir, "job-1", 1).is_err());
        assert!(
            marker.is_file(),
            "invalid jobs parent must be rejected before marker unlink"
        );
        assert!(external_job.is_dir());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        assert!(original_jobs.join("job-1").is_dir());

        assert!(claim_owned(&dir, "job-create", 2).is_err());
        assert!(!owned_path(&dir, "job-create", 2).exists());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn replaced_jobs_parent_preserves_moved_tree_and_ownership_marker() {
        let dir = tmp("jobs-parent-replaced");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let marker = owned_path(&dir, "job-1", 1);
        let moved_jobs = dir.join("jobs-original");
        let moved_job = moved_jobs.join("job-1");
        std::fs::write(job_dir(&dir, "job-1").join("sentinel"), b"keep").unwrap();

        std::fs::rename(dir.join("jobs"), &moved_jobs).unwrap();
        std::fs::create_dir(dir.join("jobs")).unwrap();

        assert!(remove_owned(&dir, "job-1", 1).is_err());
        assert!(
            marker.is_file(),
            "replacement jobs/ must not release ownership"
        );
        assert_eq!(std::fs::read(moved_job.join("sentinel")).unwrap(), b"keep");
        assert!(job_dir(&dir, "job-1").parent().unwrap().is_dir());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn mounted_owned_parent_preserves_marker_and_job_tree() {
        let dir = tmp("owned-mounted-parent");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let marker = owned_path(&dir, "job-1", 1);
        let sentinel = job_dir(&dir, "job-1").join("sentinel");
        std::fs::write(&sentinel, b"keep").unwrap();

        let state = open_state_directory(&dir).unwrap();
        let owned = open_owned_parent_at(&state).unwrap();
        let owned_inode = rustix::fs::fstat(&owned).unwrap().st_ino;
        let mut mount_id = |directory: &std::fs::File| {
            let inode = rustix::fs::fstat(directory)
                .map_err(io::Error::from)?
                .st_ino;
            Ok(Some(fake_job_mount_identity(if inode == owned_inode {
                2
            } else {
                1
            })))
        };
        let mut sync =
            |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
        let mut after_open = |_: &std::fs::File, _: &OsStr, _: &std::fs::File| Ok(());

        assert!(remove_owned_at_root_with(
            &state,
            "job-1",
            1,
            &mut sync,
            &mut mount_id,
            &mut after_open,
        )
        .is_err());
        assert!(marker.is_file());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn claim_rejects_mounted_jobs_parent_before_creating_job_directory() {
        let dir = tmp("claim-mounted-jobs-parent");
        let marker = owned_path(&dir, "job-1", 1);
        std::fs::write(&marker, b"keep owner marker").unwrap();
        let jobs_path = dir.join("jobs");
        std::fs::create_dir(&jobs_path).unwrap();

        let state = open_state_directory(&dir).unwrap();
        let jobs = open_jobs_parent_at(&state, false).unwrap().unwrap();
        let jobs_inode = rustix::fs::fstat(&jobs).unwrap().st_ino;
        let mut fake_mount_id = |directory: &std::fs::File| {
            let inode = rustix::fs::fstat(directory)
                .map_err(io::Error::from)?
                .st_ino;
            Ok(Some(fake_job_mount_identity(if inode == jobs_inode {
                2
            } else {
                1
            })))
        };

        assert!(ensure_job_directory_at_with(&state, "job-1", &mut fake_mount_id).is_err());
        assert_eq!(std::fs::read(&marker).unwrap(), b"keep owner marker");
        assert!(!job_dir(&dir, "job-1").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn claim_rejects_mounted_job_root_before_worker_can_spawn() {
        let dir = tmp("claim-mounted-job-root");
        let marker = owned_path(&dir, "job-1", 1);
        std::fs::write(&marker, b"keep owner marker").unwrap();
        let job = job_dir(&dir, "job-1");
        std::fs::create_dir_all(&job).unwrap();
        let sentinel = job.join("sentinel");
        std::fs::write(&sentinel, b"keep mounted data").unwrap();

        let state = open_state_directory(&dir).unwrap();
        let jobs = open_jobs_parent_at(&state, false).unwrap().unwrap();
        let job_directory = open_job_directory_at(&jobs, OsStr::new("job-1")).unwrap();
        let job_inode = rustix::fs::fstat(&job_directory).unwrap().st_ino;
        let mut fake_mount_id = |directory: &std::fs::File| {
            let inode = rustix::fs::fstat(directory)
                .map_err(io::Error::from)?
                .st_ino;
            Ok(Some(fake_job_mount_identity(if inode == job_inode {
                2
            } else {
                1
            })))
        };

        assert!(ensure_job_directory_at_with(&state, "job-1", &mut fake_mount_id).is_err());
        assert_eq!(std::fs::read(&marker).unwrap(), b"keep owner marker");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep mounted data");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn remove_owned_uses_one_pinned_state_root_after_path_replacement() {
        let dir = tmp("state-root-pinned");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let state = open_state_directory(&dir).unwrap();
        let original = dir.with_extension("pinned-original");
        std::fs::rename(&dir, &original).unwrap();

        let external_marker = owned_path(&dir, "job-1", 1);
        let external_job = job_dir(&dir, "job-1");
        std::fs::create_dir_all(&external_job).unwrap();
        std::fs::create_dir_all(external_marker.parent().unwrap()).unwrap();
        std::fs::write(&external_marker, b"replacement ownership").unwrap();
        let sentinel = external_job.join("sentinel");
        std::fs::write(&sentinel, b"keep").unwrap();

        remove_owned_at_root(&state, "job-1", 1).unwrap();

        assert!(!original.join("owned/job-1.1").exists());
        assert!(!original.join("jobs/job-1").exists());
        assert_eq!(
            std::fs::read(&external_marker).unwrap(),
            b"replacement ownership"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        std::fs::remove_dir_all(original).ok();
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn job_cleanup_rejects_a_directory_moved_after_open() {
        let dir = tmp("job-move-race");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let marker = owned_path(&dir, "job-1", 1);
        let sentinel = job_dir(&dir, "job-1").join("sentinel");
        std::fs::write(&sentinel, b"keep outside state").unwrap();
        let external_parent = tmp_uninitialized("job-move-destination");
        let external_parent_fd = open_state_directory(&external_parent).unwrap();
        let moved_tree = external_parent.join("moved");
        let state = open_state_directory(&dir).unwrap();
        let mut sync =
            |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
        let mut mount_id = job_directory_mount_id;
        let mut moved = false;

        let result = remove_owned_at_root_with(
            &state,
            "job-1",
            1,
            &mut sync,
            &mut mount_id,
            &mut |parent, name, _opened| {
                if !moved {
                    rustix::fs::renameat(parent, name, &external_parent_fd, OsStr::new("moved"))
                        .map_err(io::Error::from)?;
                    moved = true;
                }
                Ok(())
            },
        );

        assert!(result.is_err(), "moved tree must fail ancestry validation");
        assert!(moved);
        assert!(
            marker.is_file(),
            "failed deletion keeps durable cleanup intent"
        );
        assert_eq!(
            std::fs::read(moved_tree.join("sentinel")).unwrap(),
            b"keep outside state"
        );
        std::fs::remove_dir_all(dir).ok();
        std::fs::remove_dir_all(external_parent).ok();
    }

    #[cfg(unix)]
    #[test]
    fn job_cleanup_rejects_nested_mount_before_mutating_it() {
        let dir = tmp("job-nested-mount");
        let root = job_dir(&dir, "job-1");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let sentinel = nested.join("sentinel");
        std::fs::write(&sentinel, b"keep mounted contents").unwrap();
        let state = open_state_directory(&dir).unwrap();
        let jobs = open_jobs_parent_at(&state, false).unwrap().unwrap();
        let nested_directory = open_job_directory_at(
            &open_job_directory_at(&jobs, OsStr::new("job-1")).unwrap(),
            OsStr::new("nested"),
        )
        .unwrap();
        let nested_inode = rustix::fs::fstat(&nested_directory).unwrap().st_ino;
        let mut sync =
            |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
        let mut fake_mount_id = |directory: &std::fs::File| {
            let inode = rustix::fs::fstat(directory)
                .map_err(io::Error::from)?
                .st_ino;
            Ok(Some(fake_job_mount_identity(if inode == nested_inode {
                2
            } else {
                1
            })))
        };
        let mut after_open = |_: &std::fs::File, _: &OsStr, _: &std::fs::File| Ok(());

        assert!(remove_job_entry_with(
            &state,
            &jobs,
            OsStr::new("job-1"),
            &mut sync,
            &mut fake_mount_id,
            &mut after_open,
        )
        .is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep mounted contents");
        assert!(nested.is_dir());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn non_directory_job_cleanup_rejects_mounted_jobs_parent_before_marker_removal() {
        let dir = tmp("job-root-file-mount");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let marker = owned_path(&dir, "job-1", 1);
        let job_entry = job_dir(&dir, "job-1");
        std::fs::remove_dir(&job_entry).unwrap();
        std::fs::write(&job_entry, b"keep mounted root entry").unwrap();

        let state = open_state_directory(&dir).unwrap();
        let jobs = open_jobs_parent_at(&state, false).unwrap().unwrap();
        let jobs_inode = rustix::fs::fstat(&jobs).unwrap().st_ino;
        let mut sync =
            |directory: &std::fs::File| rustix::fs::fsync(directory).map_err(io::Error::from);
        let mut fake_mount_id = |directory: &std::fs::File| {
            let inode = rustix::fs::fstat(directory)
                .map_err(io::Error::from)?
                .st_ino;
            Ok(Some(fake_job_mount_identity(if inode == jobs_inode {
                2
            } else {
                1
            })))
        };
        let mut after_open = |_: &std::fs::File, _: &OsStr, _: &std::fs::File| Ok(());

        assert!(remove_owned_at_root_with(
            &state,
            "job-1",
            1,
            &mut sync,
            &mut fake_mount_id,
            &mut after_open,
        )
        .is_err());
        assert!(marker.is_file(), "failed cleanup keeps ownership intent");
        assert_eq!(
            std::fs::read(&job_entry).unwrap(),
            b"keep mounted root entry"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ownership_marker_survives_partial_job_delete_and_recovers_on_retry() {
        let dir = tmp("job-partial-delete");
        write_owned_pid(&dir, "job-1", 1, std::process::id()).unwrap();
        let marker = owned_path(&dir, "job-1", 1);
        let nested = job_dir(&dir, "job-1").join("nested");
        std::fs::create_dir(&nested).unwrap();
        let secret = nested.join("secret");
        std::fs::write(&secret, b"delete during first attempt").unwrap();

        let state = open_state_directory(&dir).unwrap();
        let jobs = open_jobs_parent_at(&state, false).unwrap().unwrap();
        let job_root = open_job_directory_at(&jobs, OsStr::new("job-1")).unwrap();
        let nested_directory = open_job_directory_at(&job_root, OsStr::new("nested")).unwrap();
        let nested_inode = rustix::fs::fstat(&nested_directory).unwrap().st_ino;
        let mut fail_once = true;
        let mut sync = |directory: &std::fs::File| {
            let inode = rustix::fs::fstat(directory)
                .map_err(io::Error::from)?
                .st_ino;
            if inode == nested_inode && fail_once {
                fail_once = false;
                return Err(io::Error::other("injected nested-directory sync failure"));
            }
            rustix::fs::fsync(directory).map_err(io::Error::from)
        };
        let mut mount_id = job_directory_mount_id;
        let mut after_open = |_: &std::fs::File, _: &OsStr, _: &std::fs::File| Ok(());

        assert!(remove_owned_at_root_with(
            &state,
            "job-1",
            1,
            &mut sync,
            &mut mount_id,
            &mut after_open,
        )
        .is_err());
        assert!(
            marker.is_file(),
            "owner marker remains cleanup retry intent"
        );
        assert!(nested.is_dir());
        assert!(
            !secret.exists(),
            "first pass may have partially deleted entries"
        );

        remove_owned(&dir, "job-1", 1).unwrap();
        assert!(!marker.exists());
        assert!(!job_dir(&dir, "job-1").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn iterative_job_cleanup_uses_bounded_fds_for_deep_trees() {
        const CHILD: &str = "VELNOR_CLEANUP_DEEP_TREE_CHILD";
        const TREE_DEPTH: usize = 128;
        if std::env::var_os(CHILD).is_none() {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "node::cleanup::tests::iterative_job_cleanup_uses_bounded_fds_for_deep_trees",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .spawn()
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                match child.try_wait().unwrap() {
                    Some(status) => {
                        assert!(status.success(), "deep cleanup child failed: {status}");
                        return;
                    }
                    None if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    None => {
                        child.kill().unwrap();
                        let _ = child.wait();
                        panic!("deep cleanup exceeded the 30-second deadline");
                    }
                }
            }
        }

        let dir = tmp("job-deep-tree");
        let state = open_state_directory(&dir).unwrap();
        let jobs = open_jobs_parent_at(&state, true).unwrap().unwrap();
        rustix::fs::mkdirat(&jobs, OsStr::new("deep-job"), Mode::from_raw_mode(0o700)).unwrap();
        let mut current = open_job_directory_at(&jobs, OsStr::new("deep-job")).unwrap();
        for _ in 0..TREE_DEPTH {
            rustix::fs::mkdirat(&current, OsStr::new("d"), Mode::from_raw_mode(0o700)).unwrap();
            current = open_job_directory_at(&current, OsStr::new("d")).unwrap();
        }
        let sentinel_fd = rustix::fs::openat(
            &current,
            OsStr::new("sentinel"),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .unwrap();
        let mut sentinel_file: std::fs::File = sentinel_fd.into();
        sentinel_file.write_all(b"remove me").unwrap();
        sentinel_file.sync_all().unwrap();
        drop(sentinel_file);
        drop(current);

        let mut original = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut original) },
            0
        );
        let constrained_limit = original.rlim_max.min(32 as libc::rlim_t);
        assert!(
            constrained_limit >= 8,
            "descriptor limit too small for test"
        );
        assert!(
            TREE_DEPTH as libc::rlim_t > constrained_limit,
            "tree depth must exceed the descriptor limit"
        );
        let constrained = libc::rlimit {
            rlim_cur: constrained_limit,
            rlim_max: original.rlim_max,
        };
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &constrained) },
            0
        );

        let mut sync = |_directory: &std::fs::File| Ok(());
        let mut mount_id = job_directory_mount_id;
        let mut after_open = |_: &std::fs::File, _: &OsStr, _: &std::fs::File| Ok(());
        remove_job_entry_with(
            &state,
            &jobs,
            OsStr::new("deep-job"),
            &mut sync,
            &mut mount_id,
            &mut after_open,
        )
        .unwrap();
        assert!(!job_dir(&dir, "deep-job").exists());
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

    #[cfg(unix)]
    #[test]
    fn outbox_read_retries_parent_durability_before_adopting_published_entry() {
        let dir = tmp("outbox-read-parent-sync");
        let state = open_state_directory(&dir).unwrap();
        let parent = open_outbox_parent_at(&state, true).unwrap().unwrap();
        let parent_identity = rustix::fs::fstat(&parent).unwrap();
        let name = outbox_name("job-1", 1);
        let sync_error = || io::Error::other("injected outbox parent sync failure");

        assert!(write_outbox_unix_with_sync(
            &state,
            &parent,
            &name,
            b"payload",
            &mut |directory| {
                let synced = rustix::fs::fstat(directory).map_err(io::Error::from)?;
                assert_eq!(synced.st_dev, parent_identity.st_dev);
                assert_eq!(synced.st_ino, parent_identity.st_ino);
                Err(sync_error())
            },
        )
        .is_err());

        assert!(
            read_outbox_unix_with_sync(&state, &parent, &name, "job-1", 1, &mut |_| Err(
                sync_error()
            ),)
            .is_err()
        );

        assert_eq!(
            read_outbox_unix_with_sync(&state, &parent, &name, "job-1", 1, &mut |directory| {
                let synced = rustix::fs::fstat(directory).map_err(io::Error::from)?;
                assert_eq!(synced.st_dev, parent_identity.st_dev);
                assert_eq!(synced.st_ino, parent_identity.st_ino);
                rustix::fs::fsync(directory).map_err(io::Error::from)
            },)
            .unwrap(),
            b"payload"
        );

        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn moved_outbox_parent_is_rejected_before_read_or_remove() {
        let dir = tmp("outbox-moved-parent");
        write_outbox(&dir, "job-1", 1, b"external sentinel").unwrap();
        let state = open_state_directory(&dir).unwrap();
        let parent = open_outbox_parent_at(&state, false).unwrap().unwrap();
        let moved_parent = dir.with_file_name(format!(
            "{}-moved-outbox",
            dir.file_name().unwrap().to_string_lossy()
        ));
        std::fs::rename(dir.join("outbox"), &moved_parent).unwrap();

        let name = outbox_name("job-1", 1);
        let before_entries = std::fs::read_dir(&moved_parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(write_outbox_unix_with_sync(
            &state,
            &parent,
            &name,
            b"unreachable write",
            &mut |_| Ok(()),
        )
        .is_err());
        let mut sync_parent = |_directory: &std::fs::File| Ok(());
        assert!(
            read_outbox_unix_with_sync(&state, &parent, &name, "job-1", 1, &mut sync_parent,)
                .is_err()
        );
        assert!(remove_outbox_unix(&state, &parent, &name, "job-1", 1).is_err());
        assert_eq!(
            std::fs::read(moved_parent.join(&name)).unwrap(),
            b"external sentinel"
        );
        let after_entries = std::fs::read_dir(&moved_parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(before_entries, after_entries);

        std::fs::remove_dir_all(moved_parent).ok();
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn outbox_write_fails_if_parent_moves_during_publication_sync() {
        let dir = tmp("outbox-write-parent-moves-during-sync");
        let state = open_state_directory(&dir).unwrap();
        let parent = open_outbox_parent_at(&state, true).unwrap().unwrap();
        let moved_parent = dir.with_file_name(format!(
            "{}-moved-outbox",
            dir.file_name().unwrap().to_string_lossy()
        ));
        let name = outbox_name("job-1", 1);

        let result = write_outbox_unix_with_sync(
            &state,
            &parent,
            &name,
            b"detached payload",
            &mut |directory| {
                std::fs::rename(dir.join("outbox"), &moved_parent)?;
                rustix::fs::fsync(directory).map_err(io::Error::from)
            },
        );

        assert!(result.is_err());
        assert!(!dir.join("outbox").exists());
        assert_eq!(
            std::fs::read(moved_parent.join(&name)).unwrap(),
            b"detached payload"
        );
        assert!(read_outbox(&dir, "job-1", 1).is_err());
        std::fs::remove_dir_all(moved_parent).ok();
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn outbox_mount_identity_mismatch_rejects_read_and_remove() {
        let dir = tmp("outbox-mount-identity");
        write_outbox(&dir, "job-1", 1, b"mount sentinel").unwrap();
        let state = open_state_directory(&dir).unwrap();
        let parent = open_outbox_parent_at(&state, false).unwrap().unwrap();
        let state_identity = rustix::fs::fstat(&state).unwrap();
        let mut mount_id = |directory: &std::fs::File| {
            let stat = rustix::fs::fstat(directory).map_err(io::Error::from)?;
            let identity =
                if stat.st_dev == state_identity.st_dev && stat.st_ino == state_identity.st_ino {
                    1
                } else {
                    2
                };
            Ok(Some(fake_job_mount_identity(identity)))
        };
        let name = outbox_name("job-1", 1);
        assert!(read_outbox_unix_with_checks(
            &state,
            &parent,
            &name,
            "job-1",
            1,
            &mut |_| Ok(()),
            &mut mount_id,
        )
        .is_err());
        assert!(remove_outbox_unix_with_mount_id(
            &state,
            &parent,
            &name,
            "job-1",
            1,
            &mut mount_id,
        )
        .is_err());
        assert!(write_outbox_unix_with_checks(
            &state,
            &parent,
            &name,
            b"unreachable write",
            &mut |_| Ok(()),
            &mut mount_id,
        )
        .is_err());
        assert_eq!(
            std::fs::read(outbox_path(&dir, "job-1", 1)).unwrap(),
            b"mount sentinel"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn creating_outbox_syncs_its_pinned_state_parent() {
        let dir = tmp("outbox-parent-sync");
        let state = open_state_directory(&dir).unwrap();
        let state_identity = rustix::fs::fstat(&state).unwrap();
        let mut sync_count = 0;
        let parent = open_outbox_parent_at_with_sync(&state, true, &mut |directory| {
            let synced = rustix::fs::fstat(directory).unwrap();
            assert_eq!(synced.st_dev, state_identity.st_dev);
            assert_eq!(synced.st_ino, state_identity.st_ino);
            assert!(dir.join("outbox").is_dir());
            sync_count += 1;
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(sync_count, 1);
        let outbox_stat =
            rustix::fs::statat(&state, Path::new("outbox"), AtFlags::SYMLINK_NOFOLLOW).unwrap();
        assert_eq!(
            rustix::fs::fstat(&parent).unwrap().st_ino,
            outbox_stat.st_ino
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn missing_outbox_parent_is_a_non_creating_noop_for_remove_and_recovery() {
        let dir = tmp("outbox-missing-parent");
        let quarantine_name = format!("{QUARANTINE_PREFIX}999999-{}", "f".repeat(32));
        assert!(!dir.join("outbox").exists());

        remove_outbox(&dir, "job-1", 1).unwrap();
        assert!(remove_stale_outbox_quarantine(&dir, &quarantine_name).unwrap());
        assert!(read_outbox(&dir, "job-1", 1).is_err());

        assert!(!dir.join("outbox").exists());
        assert!(!dir.join(OUTBOX_RECOVERY_LOCK_NAME).exists());
        assert!(std::fs::read_dir(&dir).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            name != QUARANTINE_ENTRY_NAME && !name.to_string_lossy().starts_with(QUARANTINE_PREFIX)
        }));
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
    fn invalid_quarantine_restore_does_not_clobber_a_concurrent_valid_payload() {
        let dir = tmp("outbox-restore-no-clobber");
        let state = open_state_directory(&dir).unwrap();
        let parent = open_outbox_parent_at(&state, true).unwrap().unwrap();
        let (quarantine_name, quarantine) = create_outbox_quarantine(&parent).unwrap();
        create_fifo(
            &dir.join("outbox")
                .join(&quarantine_name)
                .join(QUARANTINE_ENTRY_NAME),
        );
        let target = dir.join("outbox/job-1.1");
        std::fs::write(&target, b"concurrent valid payload").unwrap();

        let mut mount_id = job_directory_mount_id;
        assert!(restore_outbox_entry(
            &state,
            &parent,
            quarantine,
            &quarantine_name,
            "job-1.1",
            &mut mount_id,
        )
        .is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"concurrent valid payload");
        assert!(dir
            .join("outbox")
            .join(&quarantine_name)
            .join(QUARANTINE_ENTRY_NAME)
            .exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn fifo_outbox_read_is_rejected_before_deadline() {
        let dir = tmp_uninitialized("outbox-fifo-read");
        std::fs::create_dir_all(dir.join("outbox")).unwrap();
        let path = outbox_path(&dir, "fifo-job", 1);
        create_fifo(&path);
        run_fifo_worker("read", &dir, None);
        assert!(path.exists(), "read must preserve the FIFO entry");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn fifo_outbox_remove_is_rejected_before_deadline() {
        let dir = tmp_uninitialized("outbox-fifo-remove");
        std::fs::create_dir_all(dir.join("outbox")).unwrap();
        let path = outbox_path(&dir, "fifo-job", 1);
        create_fifo(&path);
        run_fifo_worker("remove", &dir, None);
        assert!(path.exists(), "failed removal must restore the FIFO entry");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn fifo_stale_quarantine_recovery_is_rejected_before_deadline() {
        let dir = tmp_uninitialized("outbox-fifo-stale");
        let name = format!("{QUARANTINE_PREFIX}999999-{}", "e".repeat(32));
        let quarantine = dir.join("outbox").join(&name);
        std::fs::create_dir_all(&quarantine).unwrap();
        create_fifo(&quarantine.join(QUARANTINE_ENTRY_NAME));
        run_fifo_worker("stale", &dir, Some(&name));
        assert!(quarantine.join(QUARANTINE_ENTRY_NAME).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn fifo_ownership_lock_is_rejected_before_deadline() {
        let dir = tmp("owned-lock-fifo");
        let lock = dir.join("owned").join(OWNED_PID_LOCK_NAME);
        create_fifo(&lock);

        run_fifo_worker("owned-lock", &dir, None);
        assert!(
            lock.exists(),
            "failed ownership read must preserve the FIFO"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn fifo_outbox_recovery_lock_is_rejected_before_deadline() {
        let dir = tmp("outbox-lock-fifo");
        let target = write_outbox(&dir, "fifo-job", 1, b"payload").unwrap();
        let lock = dir.join(OUTBOX_RECOVERY_LOCK_NAME);
        create_fifo(&lock);

        run_fifo_worker("outbox-lock", &dir, None);
        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
        assert!(lock.exists(), "failed removal must preserve the FIFO lock");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn character_device_locks_are_rejected_before_open() {
        let dir = tmp("lock-device");
        let target = write_outbox(&dir, "fifo-job", 1, b"payload").unwrap();
        let owned_lock = dir.join("owned").join(OWNED_PID_LOCK_NAME);
        if let Err(error) = create_character_device(&owned_lock) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                eprintln!("skipping character-device lock regression: {error}");
                std::fs::remove_dir_all(dir).ok();
                return;
            }
            panic!("create ownership lock character device: {error}");
        }
        let outbox_lock = dir.join(OUTBOX_RECOVERY_LOCK_NAME);
        if let Err(error) = create_character_device(&outbox_lock) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                eprintln!("skipping character-device lock regression: {error}");
                std::fs::remove_dir_all(dir).ok();
                return;
            }
            panic!("create outbox lock character device: {error}");
        }

        run_fifo_worker("owned-lock", &dir, None);
        run_fifo_worker("outbox-lock", &dir, None);
        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
        assert!(std::fs::symlink_metadata(&owned_lock)
            .unwrap()
            .file_type()
            .is_char_device());
        assert!(std::fs::symlink_metadata(&outbox_lock)
            .unwrap()
            .file_type()
            .is_char_device());
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
    fn missing_outbox_recovery_is_a_noop_without_creating_artifacts() {
        let dir = tmp_uninitialized("outbox-missing-noop");

        reconcile_outbox_entries(&dir, |_| Ok(false)).unwrap();
        assert!(!dir.join("outbox").exists());
        assert!(!dir.join(OUTBOX_RECOVERY_LOCK_NAME).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn temporary_outbox_reconciliation_removes_only_exact_entry() {
        let dir = tmp("outbox-temp-cleanup");
        let final_path = write_outbox(&dir, "job-1", 1, b"published").unwrap();
        let name = temporary_outbox_name("job-1.1");
        let temporary = dir.join("outbox").join(&name);
        std::fs::write(&temporary, b"partial").unwrap();

        let mut saw_temporary = false;
        reconcile_outbox_entries(&dir, |entry| {
            if entry == name {
                saw_temporary = true;
                Ok(false)
            } else {
                Ok(true)
            }
        })
        .unwrap();

        assert!(saw_temporary);
        assert!(!temporary.exists());
        assert_eq!(std::fs::read(final_path).unwrap(), b"published");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn outbox_parent_replacement_after_scan_preserves_both_trees() {
        let dir = tmp("outbox-parent-replaced-during-scan");
        let payload = write_outbox(&dir, "job-1", 1, b"published").unwrap();
        let moved = dir.join("outbox-original");
        let replacement = dir.join("outbox");
        let sentinel = replacement.join("sentinel");
        let result = reconcile_outbox_entries(&dir, |_| {
            std::fs::rename(dir.join("outbox"), &moved)?;
            std::fs::create_dir(&replacement)?;
            std::fs::write(&sentinel, b"keep")?;
            Ok(false)
        });

        assert!(result.is_err());
        assert!(!payload.exists());
        assert_eq!(std::fs::read(moved.join("job-1.1")).unwrap(), b"published");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep");
        std::fs::remove_dir_all(dir).ok();
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
        let state = open_state_directory(&dir).unwrap();
        let lock = acquire_outbox_recovery_lock(&state, false).unwrap();
        assert!(lock.is_some());
        assert!(!remove_stale_outbox_quarantine(&dir, &name).unwrap());
        assert!(quarantine.exists());
        drop(lock);
        let mut removed = false;
        for _ in 0..16 {
            removed = remove_stale_outbox_quarantine(&dir, &name).unwrap();
            if removed {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(removed);
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
