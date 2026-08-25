//! Real GitHub assignments on disk. Never a synthetic `slot-N-worker` job.

use std::path::Path;

use serde::{Deserialize, Serialize};
use velnor_control::journal::Journal;
use velnor_model::ActorPhase;

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

/// Bind each unowned queued GitHub job id to one Ready slot.
///
/// # Errors
/// Journal or filesystem failures.
pub fn bind_queued(state_dir: &Path, journal: &Journal, job_ids: &[String]) -> anyhow::Result<()> {
    let state = journal.load_state()?;
    let owned: std::collections::HashSet<&str> =
        state.jobs.iter().map(|job| job.job_id.0.as_str()).collect();
    let mut ready = state
        .slots
        .iter()
        .filter(|slot| slot.phase == ActorPhase::Ready);
    for job_id in job_ids {
        if owned.contains(job_id.as_str()) {
            continue;
        }
        let Some(slot) = ready.next() else {
            break;
        };
        write(
            state_dir,
            &Assignment {
                job_id: job_id.clone(),
                slot_id: slot.slot_id.0.clone(),
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use velnor_control::journal::Event;
    use velnor_model::{Generation, SlotId};

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
    fn bind_queued_uses_ready_slot_not_a_synthetic_worker_id() {
        let dir = tmp("bind");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let g = Generation::INITIAL;
        let slot = SlotId("scope-1".into());
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::Dependency {
                github_reachable: true,
            },
            Event::Routing {
                valid: true,
                group_valid: true,
            },
            Event::DesiredCapacity { ready: 1, surge: 0 },
            Event::PermitReserved {
                slot_id: slot.clone(),
                generation: g,
                surge: false,
            },
            Event::ExecutorProven {
                slot_id: slot.clone(),
                generation: g,
            },
            Event::SessionLive {
                slot_id: slot.clone(),
                generation: g,
            },
            Event::RegistrationIntended {
                slot_id: slot.clone(),
                generation: g,
            },
            Event::Registered {
                slot_id: slot.clone(),
                generation: g,
            },
            Event::ReadyAttempt {
                slot_id: slot.clone(),
                generation: g,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        bind_queued(&dir, &journal, &["gh-job-42".into()]).unwrap();
        let got = read_dir(&dir).unwrap();
        assert_eq!(
            got,
            vec![Assignment {
                job_id: "gh-job-42".into(),
                slot_id: "scope-1".into(),
            }]
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
