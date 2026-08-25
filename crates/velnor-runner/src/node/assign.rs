//! Real GitHub assignments on disk. Never a synthetic `slot-N-worker` job.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::cleanup;

/// Directory of `{job_id}.json` assignment records.
pub const ASSIGNMENT_DIR: &str = "assignments";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub job_id: String,
    pub slot_id: String,
}

/// Persist one GitHub job bound to a Ready slot.
///
/// # Errors
/// Unsafe id or filesystem failures.
pub fn write(state_dir: &Path, assignment: &Assignment) -> anyhow::Result<std::path::PathBuf> {
    cleanup::assert_safe_id(&assignment.job_id)?;
    cleanup::assert_safe_id(&assignment.slot_id)?;
    let dir = state_dir.join(ASSIGNMENT_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", assignment.job_id));
    std::fs::write(&path, serde_json::to_vec_pretty(assignment)?)?;
    Ok(path)
}

/// Load assignment files. Invalid entries are skipped.
///
/// # Errors
/// Directory listing failures.
pub fn read_dir(state_dir: &Path) -> anyhow::Result<Vec<Assignment>> {
    let dir = state_dir.join(ASSIGNMENT_DIR);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut assignments = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(assignment) = serde_json::from_slice::<Assignment>(&bytes) else {
            continue;
        };
        if cleanup::assert_safe_id(&assignment.job_id).is_err()
            || cleanup::assert_safe_id(&assignment.slot_id).is_err()
        {
            continue;
        }
        assignments.push(assignment);
    }
    Ok(assignments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-assign-{label}-{}-{}",
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
    fn assignment_file_is_not_a_journal_owner() {
        let dir = tmp("file");
        write(
            &dir,
            &Assignment {
                job_id: "424242".into(),
                slot_id: "scope-1".into(),
            },
        )
        .unwrap();
        let got = read_dir(&dir).unwrap();
        assert_eq!(got[0].job_id, "424242");
        assert!(!dir.join("journal.db").exists());
        std::fs::remove_dir_all(dir).ok();
    }
}
