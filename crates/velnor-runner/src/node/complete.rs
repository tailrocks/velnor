//! Persist completion intent before any GitHub `complete_job` send, then
//! `RemoteAcked` after send succeeds so the slot leaves Completing.
//!
//! The journal directory is a function argument, never thread-local: the
//! service binary uses a multi-thread Tokio runtime, so TLS set before an
//! `.await` is not visible on the worker that resumes `complete_job`.

use std::path::{Path, PathBuf};

use velnor_control::journal::{payload_checksum, Event, Journal};
use velnor_model::{ActorPhase, Generation, JobId, SlotId};

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
    let result = send().map_err(Into::into)?;
    ack_remote(journal, job_id, generation)?;
    Ok(result)
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
    let result = send.await?;
    ack_remote(journal, job_id, generation)?;
    Ok(result)
}

/// Return the generation that already owns `job_id`. Never creates ownership.
///
/// # Errors
/// Job is missing from the journal.
pub fn ensure_owned(journal: &mut Journal, job_id: &JobId) -> anyhow::Result<Generation> {
    let state = journal.load_state()?;
    state
        .jobs
        .iter()
        .find(|job| job.job_id == *job_id)
        .map(|job| job.generation)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "job {} is not owned; completion cannot attach a permit at send time",
                job_id.0
            )
        })
}

/// Bind `job_id` to the slot that accepted it. `Assigned` is best-effort when
/// the slot is already Assigned; `JobOwned` must succeed.
///
/// # Errors
/// Missing slot or rejected `JobOwned`.
pub fn accept_job(
    journal: &mut Journal,
    job_id: &JobId,
    slot_id: &SlotId,
) -> anyhow::Result<Generation> {
    let state = journal.load_state()?;
    if let Some(job) = state.jobs.iter().find(|job| job.job_id == *job_id) {
        if job.slot_id != *slot_id {
            anyhow::bail!(
                "job {} is owned by slot {}, not {}",
                job_id.0,
                job.slot_id.0,
                slot_id.0
            );
        }
        return Ok(job.generation);
    }
    let slot = state
        .slots
        .iter()
        .find(|slot| slot.slot_id == *slot_id)
        .ok_or_else(|| anyhow::anyhow!("slot {} is missing from the journal", slot_id.0))?;
    let generation = slot.generation;
    let assigned = journal.apply(Event::Assigned {
        slot_id: slot_id.clone(),
        job_id: job_id.clone(),
        generation,
    })?;
    if assigned.rejected {
        anyhow::bail!(
            "Assigned rejected for {} on {} (slot must still be Ready)",
            job_id.0,
            slot_id.0
        );
    }
    let owned = journal.apply(Event::JobOwned {
        job_id: job_id.clone(),
        slot_id: slot_id.clone(),
        attempt: 1,
        generation,
        worker: format!("velnor-job@{}", job_id.0),
        accepted_unix: 0,
    })?;
    if owned.rejected {
        anyhow::bail!("JobOwned rejected for {} on {}", job_id.0, slot_id.0);
    }
    Ok(generation)
}

/// Slot this runner config belongs to. `None` when the journal has no slots.
#[must_use]
pub fn infer_slot_id(journal: &Journal, config_dir: &Path) -> Option<SlotId> {
    let state = journal.load_state().ok()?;
    if state.slots.is_empty() {
        return None;
    }
    if let Some(name) = config_dir.file_name().and_then(|name| name.to_str()) {
        if let Some(index) = name.strip_prefix("slot-") {
            if let Some(slot) = state
                .slots
                .iter()
                .find(|slot| slot.slot_id.0.rsplit('-').next() == Some(index))
            {
                return Some(slot.slot_id.clone());
            }
        }
    }
    if state.slots.len() == 1 {
        return Some(state.slots[0].slot_id.clone());
    }
    let running: Vec<&SlotId> = state
        .jobs
        .iter()
        .filter(|job| {
            matches!(
                job.phase,
                ActorPhase::Assigned | ActorPhase::Running | ActorPhase::Starting
            )
        })
        .map(|job| &job.slot_id)
        .collect();
    if running.len() == 1 {
        return Some(running[0].clone());
    }
    None
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

/// GitHub accepted `complete_job`. Commit that before the worker exits so
/// crash recovery does not replay Completing forever.
fn ack_remote(journal: &mut Journal, job_id: &JobId, generation: Generation) -> anyhow::Result<()> {
    let outcome = journal.apply(Event::RemoteAcked {
        job_id: job_id.clone(),
        generation,
    })?;
    if outcome.rejected {
        anyhow::bail!("remote ack rejected for job {}", job_id.0);
    }
    Ok(())
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
            Event::Assigned {
                slot_id: slot.clone(),
                job_id: job_id.clone(),
                generation: g,
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
                    accepted_unix: 0,
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
        assert!(
            journal.pending_outbox().unwrap().is_empty(),
            "successful complete_job must RemoteAcked so the outbox is not replayed"
        );
        let state = journal.load_state().unwrap();
        assert!(state.jobs.is_empty(), "{:?}", state.jobs);
        assert_eq!(state.slots[0].phase, ActorPhase::Ready);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn successful_complete_leaves_slot_ready_for_the_next_assign() {
        let dir = tmp("ack-ready");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let first = JobId("job-1".into());
        let generation = prime_owned(&mut journal, &first);
        guarded_complete(
            &mut journal,
            &dir,
            &first,
            generation,
            b"conclusion=success",
            || Ok::<(), anyhow::Error>(()),
        )
        .unwrap();
        let second = accept_job(
            &mut journal,
            &JobId("job-2".into()),
            &SlotId("scope-1".into()),
        );
        assert!(
            second.is_ok(),
            "Ready slot must accept the next job after RemoteAcked, got {second:?}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_send_does_not_ack_or_restore_the_slot() {
        let dir = tmp("ack-fail");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        let generation = prime_owned(&mut journal, &job_id);
        let error = guarded_complete(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"conclusion=success",
            || Err::<(), anyhow::Error>(anyhow::anyhow!("complete_job 503")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("complete_job 503"), "{error}");
        let pending = journal.pending_outbox().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].send_started);
        assert!(!pending[0].remote_acked);
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs[0].phase, ActorPhase::Completing);
        assert_eq!(state.slots[0].phase, ActorPhase::Assigned);
        let rejected = accept_job(
            &mut journal,
            &JobId("job-2".into()),
            &SlotId("scope-1".into()),
        )
        .unwrap_err();
        assert!(
            rejected.to_string().contains("Assigned rejected"),
            "{rejected}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn async_complete_job_success_applies_remote_acked() {
        let dir = tmp("ack-async");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-async".into());
        let generation = prime_owned(&mut journal, &job_id);
        guarded_complete_async(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"conclusion=success",
            async { Ok(()) },
        )
        .await
        .unwrap();
        assert!(journal.pending_outbox().unwrap().is_empty());
        let state = journal.load_state().unwrap();
        assert!(state.jobs.is_empty(), "{:?}", state.jobs);
        assert_eq!(state.slots[0].phase, ActorPhase::Ready);
        accept_job(
            &mut journal,
            &JobId("job-next".into()),
            &SlotId("scope-1".into()),
        )
        .expect("slot must leave Completing after async complete_job");
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

    #[test]
    fn ensure_owned_does_not_attach_to_the_first_permit() {
        let dir = tmp("no-attach");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let _ = prime_owned(&mut journal, &JobId("already".into()));
        let error = ensure_owned(&mut journal, &JobId("other".into())).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot attach a permit at send time"),
            "{error}"
        );
        let state = journal.load_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id.0, "already");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn accept_job_owns_a_ready_slot() {
        let dir = tmp("accept");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let slot = SlotId("scope-1".into());
        let g = Generation::INITIAL;
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
        let generation = accept_job(&mut journal, &JobId("guid-1".into()), &slot).unwrap();
        assert_eq!(generation, g);
        let state = journal.load_state().unwrap();
        assert_eq!(state.slots[0].phase, ActorPhase::Assigned);
        assert_eq!(state.jobs[0].job_id.0, "guid-1");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn accept_job_rejects_a_second_live_owner() {
        let dir = tmp("accept-second");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("already".into());
        let _ = prime_owned(&mut journal, &job_id);
        let error = accept_job(
            &mut journal,
            &JobId("other".into()),
            &SlotId("scope-1".into()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("Assigned rejected"), "{error}");
        assert_eq!(
            ensure_owned(&mut journal, &job_id).unwrap(),
            Generation::INITIAL
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
