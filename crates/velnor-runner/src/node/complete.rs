//! Persist completion intent before any GitHub `complete_job` send.
//!
//! The journal directory is a function argument, never thread-local: the
//! service binary uses a multi-thread Tokio runtime, so TLS set before an
//! `.await` is not visible on the worker that resumes `complete_job`.

use std::path::{Path, PathBuf};

use velnor_control::journal::{payload_checksum, Event, Journal};
use velnor_model::{Generation, JobId, SlotId};

use super::cleanup;

/// Walk from a slot config dir to the fleet journal directory.
#[must_use]
pub fn journal_dir_near(config_dir: &Path) -> PathBuf {
    let mut dir = config_dir.to_path_buf();
    for _ in 0..4 {
        if dir.join("journal.db").is_file() {
            return dir;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    config_dir.to_path_buf()
}

/// Apply `CompletionIntended`, write the outbox, then call `send`.
/// If intent is rejected, `send` is never invoked.
///
/// # Errors
/// Journal rejection, outbox I/O, or `send`.
pub fn guarded_complete<T, E>(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    payload: &[u8],
    send: impl FnOnce() -> Result<T, E>,
) -> anyhow::Result<T>
where
    E: Into<anyhow::Error>,
{
    if !commit_intent(journal, state_dir, job_id, generation, payload)? {
        anyhow::bail!("completion intent rejected for job {}", job_id.0);
    }
    let started = journal.apply(Event::CompletionSendStarted {
        job_id: job_id.clone(),
        generation,
    })?;
    if started.rejected {
        anyhow::bail!("completion send-started rejected for job {}", job_id.0);
    }
    send().map_err(Into::into)
}

/// Async GitHub complete path used by `complete_run_service_job`.
///
/// # Errors
/// Journal rejection, outbox I/O, or `send`.
pub async fn guarded_complete_async<T>(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    payload: &[u8],
    send: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    if !commit_intent(journal, state_dir, job_id, generation, payload)? {
        anyhow::bail!("completion intent rejected for job {}", job_id.0);
    }
    let started = journal.apply(Event::CompletionSendStarted {
        job_id: job_id.clone(),
        generation,
    })?;
    if started.rejected {
        anyhow::bail!("completion send-started rejected for job {}", job_id.0);
    }
    send.await
}

/// Own `job_id` on an existing permitted slot, then return that generation.
///
/// # Errors
/// No permit, or `JobOwned` rejected.
pub fn ensure_owned(journal: &mut Journal, job_id: &JobId) -> anyhow::Result<Generation> {
    let state = journal.load_state()?;
    if let Some(job) = state.jobs.iter().find(|job| job.job_id == *job_id) {
        return Ok(job.generation);
    }
    let slot = state
        .slots
        .iter()
        .find(|slot| slot.permit_held)
        .ok_or_else(|| anyhow::anyhow!("no capacity permit to own job {}", job_id.0))?;
    let generation = slot.generation;
    let slot_id = SlotId(slot.slot_id.0.clone());
    let owned = journal.apply(Event::JobOwned {
        job_id: job_id.clone(),
        slot_id,
        attempt: 1,
        generation,
        worker: format!("velnor-job@{}", job_id.0),
    })?;
    if owned.rejected {
        anyhow::bail!("JobOwned rejected for {}", job_id.0);
    }
    Ok(generation)
}

fn commit_intent(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    payload: &[u8],
) -> anyhow::Result<bool> {
    let checksum = payload_checksum(payload);
    let intended = journal.apply(Event::CompletionIntended {
        job_id: job_id.clone(),
        generation,
        payload_sha256: checksum,
    })?;
    if intended.rejected {
        return Ok(false);
    }
    cleanup::write_outbox(state_dir, &job_id.0, generation.0, payload)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use velnor_control::journal::Event;
    use velnor_model::{Generation, SlotId};

    fn tmp(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-complete-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn prime_owned(journal: &mut Journal, job_id: &JobId) -> Generation {
        let g = Generation::INITIAL;
        let slot = SlotId("scope-1".into());
        for event in [
            Event::ControlLive,
            Event::JournalWritable,
            Event::PermitReserved {
                slot_id: slot.clone(),
                generation: g,
                surge: false,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        assert!(
            !journal
                .apply(Event::JobOwned {
                    job_id: job_id.clone(),
                    slot_id: slot,
                    attempt: 1,
                    generation: g,
                    worker: "w".into(),
                })
                .unwrap()
                .rejected
        );
        g
    }

    #[test]
    fn send_runs_only_after_outbox_intent() {
        let dir = tmp("after");
        let db = dir.join("journal.db");
        let mut journal = Journal::open(&db).unwrap();
        let job_id = JobId("job-1".into());
        let generation = prime_owned(&mut journal, &job_id);
        let mut sent = false;
        guarded_complete(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"conclusion=success",
            || {
                let reader = Journal::open(&db).unwrap();
                let pending = reader.pending_outbox().unwrap();
                assert_eq!(pending.len(), 1);
                assert!(pending[0].intended);
                sent = true;
                Ok::<(), anyhow::Error>(())
            },
        )
        .unwrap();
        assert!(sent);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejected_intent_never_sends() {
        let dir = tmp("reject");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("missing".into());
        let mut sent = false;
        let error = guarded_complete(
            &mut journal,
            &dir,
            &job_id,
            Generation::INITIAL,
            b"nope",
            || {
                sent = true;
                Ok::<(), anyhow::Error>(())
            },
        )
        .unwrap_err();
        assert!(!sent, "send must not run without durable intent: {error}");
        assert!(journal.pending_outbox().unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn complete_on_another_thread_uses_the_passed_directory() {
        let dir = tmp("hop");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-hop".into());
        let generation = prime_owned(&mut journal, &job_id);
        drop(journal);
        let dir_for_thread = dir.clone();
        let sent = std::thread::spawn(move || {
            let mut journal = Journal::open(dir_for_thread.join("journal.db")).unwrap();
            let mut sent = false;
            guarded_complete(
                &mut journal,
                &dir_for_thread,
                &JobId("job-hop".into()),
                generation,
                b"ok",
                || {
                    sent = true;
                    Ok::<(), anyhow::Error>(())
                },
            )
            .unwrap();
            sent
        })
        .join()
        .expect("worker thread");
        assert!(sent);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn journal_dir_near_walks_up_to_fleet_journal() {
        let dir = tmp("near");
        Journal::open(dir.join("journal.db")).unwrap();
        let slot = dir.join("slots").join("slot-1");
        std::fs::create_dir_all(&slot).unwrap();
        assert_eq!(journal_dir_near(&slot), dir);
        std::fs::remove_dir_all(dir).ok();
    }
}
