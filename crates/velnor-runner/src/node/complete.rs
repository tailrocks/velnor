//! Persist completion intent and one durable send claim before any GitHub
//! `complete_job` send, then acknowledge the exact remote disposition so the
//! slot leaves Completing.
//!
//! The journal directory is a function argument, never thread-local: the
//! service binary uses a multi-thread Tokio runtime, so TLS set before an
//! `.await` is not visible on the worker that resumes `complete_job`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use velnor_control::journal::{payload_checksum, Event, Journal};
use velnor_model::{ActorPhase, Generation, JobId, SlotId};

use super::cleanup;
use crate::protocol::CompletionAcknowledgement;

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

/// Publish the outbox, apply `CompletionIntended`, claim the send durably, then
/// call `send`. If either durable step is rejected, `send` is never invoked.
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
    if commit_intent(journal, state_dir, job_id, generation, payload)?.is_none() {
        anyhow::bail!("completion intent rejected for job {}", job_id.0);
    }
    claim_completion_send(journal, job_id, generation)?;
    let result = send().map_err(Into::into)?;
    ack_remote(
        journal,
        state_dir,
        job_id,
        generation,
        CompletionAcknowledgement::Accepted,
    )?;
    Ok(result)
}

/// Async GitHub complete path used by `complete_run_service_job`.
///
/// # Errors
/// Journal rejection, outbox I/O, or `send`.
pub async fn guarded_complete_async<T, F, Fut>(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    payload: &[u8],
    send: F,
) -> anyhow::Result<T>
where
    F: FnOnce(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    guarded_complete_async_with_ack(
        journal,
        state_dir,
        job_id,
        generation,
        payload,
        |durable| async move {
            send(durable)
                .await
                .map(|result| (result, CompletionAcknowledgement::Accepted))
        },
    )
    .await
}

/// Async completion guard whose transport reports whether the remote service
/// accepted the payload or had already terminalized the job.
pub async fn guarded_complete_async_with_ack<T, F, Fut>(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    payload: &[u8],
    send: F,
) -> anyhow::Result<T>
where
    F: FnOnce(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(T, CompletionAcknowledgement)>>,
{
    let durable_payload = commit_intent(journal, state_dir, job_id, generation, payload)?
        .ok_or_else(|| anyhow::anyhow!("completion intent rejected for job {}", job_id.0))?;
    claim_completion_send(journal, job_id, generation)?;
    let (result, acknowledgement) = send(durable_payload).await?;
    ack_remote(journal, state_dir, job_id, generation, acknowledgement)?;
    Ok(result)
}

/// Replay a completion after the controller has proven the original worker
/// dead. A pre-send crash acquires the still-unclaimed row; a send-in-progress
/// crash reuses its existing claim. The replay never creates a second claim.
pub async fn replay_claimed_completion_async<T, F, Fut>(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    payload: Vec<u8>,
    send: F,
) -> anyhow::Result<T>
where
    F: FnOnce(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(T, CompletionAcknowledgement)>>,
{
    let state = journal.materialized_state()?;
    let row = state
        .outbox
        .iter()
        .find(|row| row.job_id == *job_id && row.generation == generation)
        .ok_or_else(|| anyhow::anyhow!("completion outbox row is missing for job {}", job_id.0))?;
    if !row.intended || row.remote_acked {
        anyhow::bail!("completion replay claim is not active for job {}", job_id.0);
    }
    let send_started = row.send_started;
    if payload_checksum(&payload) != row.payload_sha256 {
        anyhow::bail!("completion replay payload changed for job {}", job_id.0);
    }
    if !send_started {
        claim_completion_send(journal, job_id, generation)?;
    }

    // Re-read after any claim so a concurrent terminal acknowledgement or
    // ownership transition cannot turn this replay into a stale send.
    let state = journal.materialized_state()?;
    let row = state
        .outbox
        .iter()
        .find(|row| row.job_id == *job_id && row.generation == generation)
        .ok_or_else(|| anyhow::anyhow!("completion outbox row is missing for job {}", job_id.0))?;
    let owner_is_current = state
        .slots
        .iter()
        .any(|slot| slot.slot_id == row.slot_id && slot.generation == row.generation)
        && state.jobs.iter().any(|job| {
            job.job_id == row.job_id
                && job.slot_id == row.slot_id
                && job.generation == row.generation
                && job.phase == ActorPhase::Completing
        });
    if !row.intended || row.remote_acked || !row.send_started || !owner_is_current {
        anyhow::bail!(
            "completion replay claim is no longer current for job {}",
            job_id.0
        );
    }
    let (result, acknowledgement) = send(payload).await?;
    ack_remote(journal, state_dir, job_id, generation, acknowledgement)?;
    Ok(result)
}

/// Return the generation that already owns `job_id`. Never creates ownership.
///
/// # Errors
/// Job is missing from the journal.
pub fn ensure_owned(journal: &mut Journal, job_id: &JobId) -> anyhow::Result<Generation> {
    let state = journal.materialized_state()?;
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
    let state = journal.materialized_state()?;
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
    let state = journal.materialized_state().ok()?;
    if state.slots.is_empty() {
        return None;
    }
    if let Some(name) = config_dir.file_name().and_then(|name| name.to_str())
        && let Some(index) = name.strip_prefix("slot-")
        && let Some(slot) = state
            .slots
            .iter()
            .find(|slot| slot.slot_id.0.rsplit('-').next() == Some(index))
    {
        return Some(slot.slot_id.clone());
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
) -> anyhow::Result<Option<Vec<u8>>> {
    let checksum = payload_checksum(payload);
    let state = journal.materialized_state()?;
    if let Some(row) = state.outbox.iter().find(|row| {
        row.job_id == *job_id && row.generation == generation && row.intended && !row.remote_acked
    }) {
        if row.payload_sha256 != checksum {
            anyhow::bail!(
                "completion payload changed while job {} was pending",
                job_id.0
            );
        }
        let durable = cleanup::read_outbox(state_dir, &job_id.0, generation.0)?;
        if payload_checksum(&durable) != row.payload_sha256 {
            anyhow::bail!(
                "completion outbox checksum mismatch for pending job {}",
                job_id.0
            );
        }
        return Ok(Some(durable));
    }
    // Publish durable bytes first. A crash between the journal intent and the
    // old non-atomic write left `Completing` without a replayable payload.
    // This is local staging only; no remote side effect occurs before intent.
    // If another writer won no-replace publication, adopt only matching bytes.
    // This also repairs a journal-apply failure on its next retry.
    let durable = match cleanup::write_outbox(state_dir, &job_id.0, generation.0, payload) {
        Ok(_) => payload.to_vec(),
        Err(publish_error) => match cleanup::read_outbox(state_dir, &job_id.0, generation.0) {
            Ok(existing) if payload_checksum(&existing) == checksum => existing,
            Ok(_) => anyhow::bail!(
                "completion outbox payload drifted while adopting job {}",
                job_id.0
            ),
            Err(read_error) => {
                return Err(publish_error).context(format!(
                    "publish completion outbox and adopt existing payload: {read_error:#}"
                ));
            }
        },
    };
    let intended = journal.apply(Event::CompletionIntended {
        job_id: job_id.clone(),
        generation,
        payload_sha256: checksum,
    })?;
    if intended.rejected {
        cleanup::remove_outbox(state_dir, &job_id.0, generation.0)
            .context("remove rejected completion outbox")?;
        return Ok(None);
    }
    Ok(Some(durable))
}

/// Atomically claim the one permitted terminal send for this job generation.
/// A second caller, including a replay, fails closed before transport.
fn claim_completion_send(
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
) -> anyhow::Result<()> {
    let claimed = journal.apply(Event::CompletionSendStarted {
        job_id: job_id.clone(),
        generation,
    })?;
    if claimed.rejected {
        anyhow::bail!("completion send claim rejected for job {}", job_id.0);
    }
    Ok(())
}

/// GitHub accepted `complete_job`. Commit that before deleting the payload so
/// crash recovery can prove the remote terminal transition before cleanup.
fn ack_remote(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    acknowledgement: CompletionAcknowledgement,
) -> anyhow::Result<()> {
    let event = match acknowledgement {
        CompletionAcknowledgement::Accepted => Event::RemoteAcked {
            job_id: job_id.clone(),
            generation,
        },
        CompletionAcknowledgement::RemoteObservedTerminal => Event::RemoteObservedTerminal {
            job_id: job_id.clone(),
            generation,
        },
    };
    let outcome = journal.apply(event)?;
    if outcome.rejected {
        anyhow::bail!("remote ack rejected for job {}", job_id.0);
    }
    cleanup::remove_outbox(state_dir, &job_id.0, generation.0)
        .context("remove acknowledged completion outbox")?;
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
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: slot.clone(),
                generation: g,
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
        assert!(!cleanup::outbox_path(&dir, &job_id.0, generation.0).exists());
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
            |durable| async move {
                assert_eq!(durable, b"conclusion=success");
                Ok(())
            },
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

    #[tokio::test]
    async fn async_complete_job_remote_terminal_observation_acks_and_cleans_up() {
        let dir = tmp("observed-terminal-async");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-observed-terminal".into());
        let generation = prime_owned(&mut journal, &job_id);
        guarded_complete_async_with_ack(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"conclusion=success",
            |durable| async move {
                assert_eq!(durable, b"conclusion=success");
                Ok(((), CompletionAcknowledgement::RemoteObservedTerminal))
            },
        )
        .await
        .unwrap();
        assert!(journal.pending_outbox().unwrap().is_empty());
        assert!(journal.load_state().unwrap().jobs.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn replay_claimed_completion_reuses_only_existing_send_claim() {
        let dir = tmp("replay-claimed");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-replay-claimed".into());
        let generation = prime_owned(&mut journal, &job_id);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"exact-payload").unwrap();
        assert!(
            !journal
                .apply(Event::CompletionIntended {
                    job_id: job_id.clone(),
                    generation,
                    payload_sha256: payload_checksum(b"exact-payload"),
                })
                .unwrap()
                .rejected
        );
        assert!(
            !journal
                .apply(Event::CompletionSendStarted {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected
        );

        replay_claimed_completion_async(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"exact-payload".to_vec(),
            |payload| async move {
                assert_eq!(payload, b"exact-payload");
                Ok(((), CompletionAcknowledgement::Accepted))
            },
        )
        .await
        .unwrap();
        assert!(journal.pending_outbox().unwrap().is_empty());
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
        assert!(!cleanup::outbox_path(&dir, &job_id.0, Generation::INITIAL.0).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn replay_reuses_pending_payload_but_rejects_second_send_claim() {
        let dir = tmp("retry-exact");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-retry".into());
        let generation = prime_owned(&mut journal, &job_id);

        let first = guarded_complete_async(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"first-payload",
            |_payload| async { Err::<(), _>(anyhow::anyhow!("temporary send failure")) },
        )
        .await
        .unwrap_err();
        assert!(first.to_string().contains("temporary send failure"));
        assert_eq!(
            cleanup::read_outbox(&dir, &job_id.0, generation.0).unwrap(),
            b"first-payload"
        );

        let drift = guarded_complete_async(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"different-payload",
            |_payload| async { Ok::<(), anyhow::Error>(()) },
        )
        .await
        .unwrap_err();
        assert!(drift.to_string().contains("payload changed"));
        assert_eq!(
            cleanup::read_outbox(&dir, &job_id.0, generation.0).unwrap(),
            b"first-payload"
        );

        let replay = guarded_complete_async(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"first-payload",
            |_durable| async move { Ok::<(), anyhow::Error>(()) },
        )
        .await
        .unwrap_err();
        assert!(
            replay.to_string().contains("send claim rejected"),
            "{replay}"
        );
        assert_eq!(
            cleanup::read_outbox(&dir, &job_id.0, generation.0).unwrap(),
            b"first-payload"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn retry_adopts_exact_payload_published_before_intent_apply() {
        let dir = tmp("apply-failure-equivalent");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-apply-failure".into());
        let generation = prime_owned(&mut journal, &job_id);
        cleanup::write_outbox(&dir, &job_id.0, generation.0, b"exact-payload").unwrap();
        assert!(journal.pending_outbox().unwrap().is_empty());

        guarded_complete(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"exact-payload",
            || Ok::<(), anyhow::Error>(()),
        )
        .unwrap();
        assert!(!cleanup::outbox_path(&dir, &job_id.0, generation.0).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn concurrent_completion_claims_allow_one_terminal_send() {
        use std::sync::{Arc, Barrier};

        let dir = tmp("claim-race");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-race".into());
        let generation = prime_owned(&mut journal, &job_id);
        drop(journal);

        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let dir = dir.clone();
                let job_id = job_id.clone();
                std::thread::spawn(move || {
                    let mut journal = Journal::open(dir.join("journal.db")).unwrap();
                    let durable =
                        commit_intent(&mut journal, &dir, &job_id, generation, b"race-payload")
                            .unwrap()
                            .unwrap();
                    assert_eq!(durable, b"race-payload");
                    barrier.wait();
                    claim_completion_send(&mut journal, &job_id, generation).is_ok()
                })
            })
            .collect::<Vec<_>>();
        let claimed = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|claimed| *claimed)
            .count();
        assert_eq!(claimed, 1, "exactly one caller may claim the terminal send");

        let journal = Journal::open(dir.join("journal.db")).unwrap();
        let pending = journal.pending_outbox().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].send_started);
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
            Event::DesiredCapacity { ready: 1 },
            Event::PermitReserved {
                slot_id: slot.clone(),
                generation: g,
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
