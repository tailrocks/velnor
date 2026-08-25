//! Exact-path job ownership and completion I/O.
//!
//! Cleanup may delete only `{state}/owned/{id}.{gen}` and `{state}/jobs/{id}`.

use std::path::{Path, PathBuf};

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

/// Persist the ownership marker and create the job directory.
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
    if !owned.exists() {
        std::fs::write(&owned, b"")?;
    }
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
    std::fs::write(owned, pid.to_string())?;
    Ok(())
}

/// Read a pid previously stored in the ownership marker.
#[must_use]
pub fn read_owned_pid(state_dir: &Path, isolation_id: &str, generation: u64) -> Option<u32> {
    let bytes = std::fs::read_to_string(owned_path(state_dir, isolation_id, generation)).ok()?;
    bytes.trim().parse().ok()
}

/// Write the completion payload after `CompletionIntended` is durable.
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
    let path = outbox_path(state_dir, job_id, generation);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, payload)?;
    Ok(path)
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
    if job.exists() {
        std::fs::remove_dir_all(&job)?;
    }
    Ok(())
}

fn assert_safe_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() || id.contains('/') || id.contains('\\') || id.contains("..") {
        anyhow::bail!("isolation id must be a single path component");
    }
    Ok(())
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
        claim_owned(&dir, "job-2", 1).unwrap();
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
    fn path_separator_id_is_rejected() {
        let dir = tmp("slash");
        assert!(claim_owned(&dir, "../etc", 1).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
