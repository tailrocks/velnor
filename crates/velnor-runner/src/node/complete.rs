//! Persist completion intent and one durable send claim before any GitHub
//! `complete_job` send, then acknowledge the exact remote disposition so the
//! slot leaves Completing.
//!
//! The journal directory is a function argument, never thread-local: the
//! service binary uses a multi-thread Tokio runtime, so TLS set before an
//! `.await` is not visible on the worker that resumes `complete_job`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use velnor_control::journal::{
    payload_checksum, AcquisitionLossProof, AcquisitionLossSource, Event, JobRecord, Journal,
    NativePermitLease,
};
use velnor_model::{Generation, JobId, JobPhase2, SlotId};

use super::cleanup;
use crate::protocol::{completion_failure_is_permanent, CompletionAcknowledgement};

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
    let result = match send() {
        Ok(result) => result,
        Err(error) => {
            return Err(record_attempt_failure(
                journal,
                job_id,
                generation,
                error.into(),
            ))
        }
    };
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
    let (result, acknowledgement) = match send(durable_payload).await {
        Ok(outcome) => outcome,
        Err(error) => return Err(record_attempt_failure(journal, job_id, generation, error)),
    };
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
                && job.phase == JobPhase2::Completing
        });
    if !row.intended || row.remote_acked || !row.send_started || !owner_is_current {
        anyhow::bail!(
            "completion replay claim is no longer current for job {}",
            job_id.0
        );
    }
    let (result, acknowledgement) = match send(payload).await {
        Ok(outcome) => outcome,
        Err(error) => return Err(record_attempt_failure(journal, job_id, generation, error)),
    };
    ack_remote(journal, state_dir, job_id, generation, acknowledgement)?;
    Ok(result)
}

/// Record the terminal result before the completion payload is built.
///
/// A crash between producing a result and publishing the outbox used to leave
/// no durable trace of what the job concluded, and recovery had to invent a
/// failure. With this committed first, a green job stays green across that
/// window.
///
/// # Errors
/// Journal write failure, or a rejection (stale generation, no owned job).
pub fn record_terminal_result(
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    conclusion: &str,
) -> anyhow::Result<()> {
    let outcome = journal.apply(Event::JobTerminalResult {
        job_id: job_id.clone(),
        generation,
        conclusion: conclusion.to_owned(),
    })?;
    if outcome.rejected {
        anyhow::bail!(
            "terminal result rejected for job {} generation {}",
            job_id.0,
            generation.0
        );
    }
    Ok(())
}

/// Charge one spent send attempt to the durable budget, then return the
/// transport error unchanged. The budget is what makes recovery terminate:
/// without it every controller cycle restarts from zero attempts forever.
fn record_attempt_failure(
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    error: anyhow::Error,
) -> anyhow::Error {
    // Category-driven: `permanent` is true only for the completion
    // boundary's `Terminal` verdict (or, for unclassified errors, the
    // historical status derivation, which fails open to retry). A
    // permanent refusal spends the whole recovery budget at once, so the
    // abandon path releases the slot now instead of after doomed retries.
    let permanent = completion_failure_is_permanent(&error);
    match journal.apply(Event::CompletionAttemptFailed {
        job_id: job_id.clone(),
        generation,
        permanent,
    }) {
        // A journal failure here must not mask the transport failure that
        // caused it; the row keeps its previous count and the next attempt
        // charges again.
        Err(journal_error) => eprintln!(
            "Warning: could not record failed completion attempt for job {}: {journal_error}",
            job_id.0
        ),
        Ok(outcome) if outcome.rejected => eprintln!(
            "Warning: failed completion attempt for job {} was rejected by the journal",
            job_id.0
        ),
        Ok(_) => {}
    }
    error
}

/// Give up on a completion whose durable budget is spent.
///
/// Returns `false` when the journal refuses because attempts and deadline are
/// both still within budget: the terminal state has to be provable from
/// durable state, never asserted by the caller.
///
/// No further send is attempted. A prior request may have reached the Run
/// Service before transport failed, so its remote outcome is unknown. The send
/// claim stays standing to prevent a second terminal send. If the remote did
/// not receive the payload, it may time the job out; the node releases the slot
/// instead of holding it indefinitely.
///
/// # Errors
/// Journal write failure, or removing the abandoned payload.
pub fn abandon_unresolvable_completion(
    journal: &mut Journal,
    state_dir: &Path,
    job_id: &JobId,
    generation: Generation,
    reason: &str,
) -> anyhow::Result<bool> {
    let outcome = journal.apply(Event::CompletionUnresolvable {
        job_id: job_id.clone(),
        generation,
        reason: reason.to_owned(),
    })?;
    if outcome.rejected {
        return Ok(false);
    }
    eprintln!(
        "Error: completion for job {} generation {} is unresolvable and was abandoned: {reason}. \
         The remote outcome is unknown; a prior send may have reached the Run Service. \
         No further send will be attempted. \
         The slot is released and the event is preserved in the journal.",
        job_id.0, generation.0
    );
    cleanup::remove_outbox(state_dir, &job_id.0, generation.0)
        .context("remove abandoned completion outbox")?;
    Ok(true)
}

/// Return the generation that already owns `job_id`. Never creates ownership or
/// treats a provisional acquisition intent as ownership.
///
/// # Errors
/// Job is missing from the journal.
pub fn ensure_owned(journal: &mut Journal, job_id: &JobId) -> anyhow::Result<Generation> {
    let state = journal.materialized_state()?;
    let job = state
        .jobs
        .iter()
        .find(|job| job.job_id == *job_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "job {} is not owned; completion cannot attach a permit at send time",
                job_id.0
            )
        })?;
    if job.provisional {
        anyhow::bail!(
            "job {} has only provisional acquisition evidence; completion cannot send",
            job_id.0
        );
    }
    Ok(job.generation)
}

/// Bind `job_id` to the slot that accepted it, from a slot that is still Ready.
/// `Assigned` is best-effort when the slot is already Assigned; `JobOwned` must
/// succeed.
///
/// Kept, rather than folded into the acquisition API, because its precondition
/// is the opposite one. `confirm_acquisition` promotes a row that already
/// exists and already occupies the slot; this creates ownership where there is
/// no row at all, for callers that acquire without an intent phase. They are
/// not two ways to do one thing, and collapsing them would mean either
/// inventing a provisional row nobody probes or letting `confirm_acquisition`
/// silently create ownership it was written to refuse.
///
/// The run-service acquire path no longer uses this: it intends, retargets and
/// confirms, so the window this whole module exists to close stays closed.
///
/// # Errors
/// Missing slot or rejected `JobOwned`.
/// What a probe of the run service concluded about a provisional row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionVerdict {
    /// `renewjob` succeeded, so this runner holds the lease: the row is ours
    /// and must be promoted rather than dropped.
    Owned,
    /// The run service reported the job gone or belonging to someone else.
    NotOurs,
    /// The probe could not reach a conclusion. Neither promoting nor dropping
    /// is safe, so the row is left alone for the next attempt.
    Indeterminate,
}

/// Record a native acquisition intent together with the exact permit lease
/// that was acquired before `acquirejob`.
///
/// A repeated write for the same request is idempotent only when its slot and
/// immutable permit identity still match. Missing legacy correlation or a
/// different lease is ambiguous and remains fenced.
pub fn intend_acquisition_with_permit(
    journal: &mut Journal,
    job_id: &JobId,
    slot_id: &SlotId,
    message_id: &str,
    runner_request_id: &str,
    run_service_url: &str,
    now: u64,
    permit_lease: NativePermitLease,
) -> anyhow::Result<Generation> {
    if runner_request_id.is_empty()
        || permit_lease.holder.is_empty()
        || permit_lease.ledger_path.is_empty()
        || permit_lease.generation == 0
    {
        anyhow::bail!("native acquisition intent has incomplete request or permit identity");
    }
    let state = journal.materialized_state()?;
    if let Some(job) = state
        .jobs
        .iter()
        .find(|job| job.runner_request_id == runner_request_id)
    {
        if job.slot_id != *slot_id
            || job.permit_lease.as_ref() != Some(&permit_lease)
            || !job.provisional
        {
            anyhow::bail!(
                "runner request {} already has a different or active journal owner",
                runner_request_id
            );
        }
        return Ok(job.generation);
    }
    if state.jobs.iter().any(|job| job.job_id == *job_id) {
        anyhow::bail!(
            "native acquisition intent {} conflicts with existing job row {}",
            runner_request_id,
            job_id.0
        );
    }
    let slot = state
        .slots
        .iter()
        .find(|slot| slot.slot_id == *slot_id)
        .ok_or_else(|| anyhow::anyhow!("slot {} is missing from the journal", slot_id.0))?;
    let generation = slot.generation;
    let outcome = journal.apply(Event::JobAcquisitionIntendedWithPermit {
        slot_id: slot_id.clone(),
        job_id: job_id.clone(),
        generation,
        message_id: message_id.to_string(),
        runner_request_id: runner_request_id.to_owned(),
        run_service_url: run_service_url.to_string(),
        intended_unix: now,
        permit_lease,
    })?;
    if outcome.rejected {
        anyhow::bail!(
            "native acquisition intent rejected for {} on {} (slot must still be Ready)",
            runner_request_id,
            slot_id.0
        );
    }
    Ok(generation)
}

/// Retarget an acquisition only while its immutable native permit association
/// still matches the provisional row.
pub fn resolve_acquisition_for_request_with_permit(
    journal: &mut Journal,
    provisional_job_id: &JobId,
    acquired_job_id: &JobId,
    plan_id: &str,
    generation: Generation,
    runner_request_id: &str,
    permit_lease: NativePermitLease,
) -> anyhow::Result<()> {
    let outcome = journal.apply(Event::JobAcquisitionResolved {
        provisional_job_id: provisional_job_id.clone(),
        acquired_job_id: acquired_job_id.clone(),
        plan_id: plan_id.to_string(),
        generation,
        runner_request_id: Some(runner_request_id.to_owned()),
        permit_lease: Some(permit_lease),
    })?;
    if outcome.rejected {
        anyhow::bail!(
            "acquisition retarget rejected for {} -> {} at generation {}",
            provisional_job_id.0,
            acquired_job_id.0,
            generation.0
        );
    }
    Ok(())
}

/// Rebuild a confirmed acquisition while preserving its exact native permit
/// lease. Existing rows must already carry the same lease; redelivery may not
/// attach a new or guessed permit to old journal state.
pub fn reintend_resolved_acquisition_with_permit(
    journal: &mut Journal,
    acquired_job_id: &JobId,
    slot_id: &SlotId,
    runner_request_id: &str,
    plan_id: &str,
    run_service_url: &str,
    now: u64,
    permit_lease: NativePermitLease,
) -> anyhow::Result<bool> {
    if permit_lease.holder.is_empty()
        || permit_lease.ledger_path.is_empty()
        || permit_lease.generation == 0
    {
        anyhow::bail!("native acquisition rebuild has incomplete permit identity");
    }
    reintend_resolved_acquisition_inner(
        journal,
        acquired_job_id,
        slot_id,
        runner_request_id,
        plan_id,
        run_service_url,
        now,
        permit_lease,
    )
}

fn reintend_resolved_acquisition_inner(
    journal: &mut Journal,
    acquired_job_id: &JobId,
    slot_id: &SlotId,
    runner_request_id: &str,
    plan_id: &str,
    run_service_url: &str,
    now: u64,
    permit_lease: NativePermitLease,
) -> anyhow::Result<bool> {
    let state = journal.materialized_state()?;
    match state.jobs.iter().find(|job| job.job_id == *acquired_job_id) {
        // An owned row can be adopted only when the original request mapping
        // is already durable. A legacy row with unknown correlation cannot be
        // guessed from the acquired job ID during broker redelivery.
        Some(job) if !job.provisional => {
            if job.runner_request_id == runner_request_id
                && job.permit_lease.as_ref() == Some(&permit_lease)
            {
                if job.slot_id != *slot_id {
                    anyhow::bail!(
                        "owned job {} is recorded on slot {} but acquisition redelivery names slot {}",
                        acquired_job_id.0,
                        job.slot_id.0,
                        slot_id.0
                    );
                }
                return Ok(false);
            }
            anyhow::bail!(
                "owned job {} cannot be correlated to runner request {} (stored correlation is {})",
                acquired_job_id.0,
                runner_request_id,
                if job.runner_request_id.is_empty() {
                    "unknown"
                } else {
                    job.runner_request_id.as_str()
                }
            );
        }
        Some(job) => {
            if job.runner_request_id.is_empty()
                || job.runner_request_id != runner_request_id
                || job.permit_lease.as_ref() != Some(&permit_lease)
            {
                anyhow::bail!(
                    "resolved provisional job {} cannot be adopted for runner request {} (stored correlation is {})",
                    acquired_job_id.0,
                    runner_request_id,
                    if job.runner_request_id.is_empty() {
                        "unknown"
                    } else {
                        job.runner_request_id.as_str()
                    }
                );
            }
            if job.slot_id != *slot_id {
                anyhow::bail!(
                    "resolved provisional job {} is recorded on slot {} but acquisition redelivery names slot {}",
                    acquired_job_id.0,
                    job.slot_id.0,
                    slot_id.0
                );
            }
            let needs_repair = job.plan_id.is_empty();
            resolve_acquisition_for_request_with_permit(
                journal,
                acquired_job_id,
                acquired_job_id,
                if job.plan_id.is_empty() {
                    plan_id
                } else {
                    &job.plan_id
                },
                job.generation,
                runner_request_id,
                permit_lease.clone(),
            )?;
            return Ok(needs_repair);
        }
        None => {}
    }
    // A planless request-keyed intent can still exist if this runner journal
    // received the intent before the acquire 200. Finish that row directly;
    // do not create a second job identity for the same permit holder.
    if let Some(job) = state
        .jobs
        .iter()
        .find(|job| job.runner_request_id == runner_request_id)
    {
        if !job.provisional || job.slot_id != *slot_id {
            anyhow::bail!(
                "runner request {} already maps to non-rebuildable job {} on slot {}",
                runner_request_id,
                job.job_id.0,
                job.slot_id.0
            );
        }
        if job.permit_lease.as_ref() != Some(&permit_lease) {
            anyhow::bail!(
                "runner request {} has no matching durable native permit lease",
                runner_request_id
            );
        }
        resolve_acquisition_for_request_with_permit(
            journal,
            &job.job_id,
            acquired_job_id,
            plan_id,
            job.generation,
            runner_request_id,
            permit_lease.clone(),
        )?;
        return Ok(true);
    }

    // The run service already confirmed this job. Persist one probeable row in
    // one journal transaction; a two-event intent+resolve sequence leaves a
    // planless crash window that startup could otherwise abandon.
    let slot = state
        .slots
        .iter()
        .find(|slot| slot.slot_id == *slot_id)
        .ok_or_else(|| anyhow::anyhow!("slot {} is missing from the journal", slot_id.0))?;
    let probe_deadline_unix =
        now.saturating_add(velnor_control::journal::ACQUISITION_RESOLUTION_SECONDS);
    let event = Event::JobAcquisitionRebuiltWithPermit {
        slot_id: slot_id.clone(),
        job_id: acquired_job_id.clone(),
        generation: slot.generation,
        runner_request_id: runner_request_id.to_owned(),
        plan_id: plan_id.to_owned(),
        run_service_url: run_service_url.to_owned(),
        probe_deadline_unix,
        permit_lease,
    };
    let outcome = journal.apply(event)?;
    if outcome.rejected {
        anyhow::bail!(
            "acquired job {} could not be durably rebuilt for request {} on slot {}",
            acquired_job_id.0,
            runner_request_id,
            slot_id.0
        );
    }
    Ok(true)
}

/// Promote a provisional row after GitHub returned the job.
pub fn confirm_acquisition(
    journal: &mut Journal,
    job_id: &JobId,
    slot_id: &SlotId,
    generation: Generation,
) -> anyhow::Result<()> {
    let owned = journal.apply(Event::JobOwned {
        job_id: job_id.clone(),
        slot_id: slot_id.clone(),
        attempt: 1,
        generation,
        worker: format!("wait-{}", slot_id.0),
        accepted_unix: 0,
    })?;
    if owned.rejected {
        anyhow::bail!("JobOwned rejected for {}", job_id.0);
    }
    Ok(())
}

/// Drop a provisional row after exact evidence proves its acquire did not
/// produce an owned job or was never sent.
pub fn abandon_acquisition(
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    proof: AcquisitionLossProof,
    reason: &str,
) -> anyhow::Result<()> {
    let lost = journal.apply(Event::JobAcquisitionLost {
        job_id: job_id.clone(),
        generation,
        reason: reason.to_string(),
        proof: Some(proof),
    })?;
    if lost.rejected {
        anyhow::bail!(
            "JobAcquisitionLost rejected for {} — exact proof is required for a provisional row",
            job_id.0
        );
    }
    Ok(())
}

/// Persist exact proof that a provisional acquisition was never sent or was
/// confirmed gone. Keep the row until its native permit lease is released.
pub fn record_acquisition_loss_proof(
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
    proof: AcquisitionLossProof,
) -> anyhow::Result<()> {
    let recorded = journal.apply(Event::JobAcquisitionLossProofRecorded {
        job_id: job_id.clone(),
        generation,
        proof,
    })?;
    if recorded.rejected {
        anyhow::bail!(
            "JobAcquisitionLossProofRecorded rejected for {} — exact provisional permit proof is required",
            job_id.0
        );
    }
    Ok(())
}

/// Outcome of resolving one provisional row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAcquisition {
    pub job_id: JobId,
    /// What the run service said, or `Indeterminate` when it was never asked.
    pub verdict: AcquisitionVerdict,
    /// The row was dropped after both the run service proved it was not ours
    /// and the exact native permit lease was durably released.
    pub abandoned: bool,
}

/// Resolve every provisional row left behind by a crash in the acquire window.
///
/// The oracle is `renewjob`, not the 409 from `acquirejob`: upstream's
/// `RunServiceError` carries only `source`, `statusCode` and `errorMessage`
/// (`src/Sdk/RSWebApi/RunServiceHttpClient.cs:88-120`), so a conflict cannot
/// distinguish "we acquired it and crashed" from "another runner has it".
/// Only the lease holder can renew, which is exactly the discrimination needed.
///
/// An indeterminate probe leaves the row untouched: promoting a job we may not
/// own would let two runners publish for it, and dropping one we do own would
/// strand it until the lease expired. A planless intent and an exhausted probe
/// budget are ambiguous for the same reason; neither is terminal evidence.
///
/// # The probe is bounded, and it has to be
///
/// `renewjob` extends the lease as a side effect. A row that keeps coming back
/// indeterminate would therefore renew, on every restart, the lease of a job
/// this node has repeatedly failed to make progress on — including the one
/// path where an indeterminate verdict still moved the lease: a 2xx whose body
/// fails to parse renews remotely and returns `Err` locally. So every
/// indeterminate probe charges the row's durable budget, and a row whose budget
/// is spent is abandoned *without being probed again*, which caps the renewals
/// one acquisition can ever cause at `MAX_ACQUISITION_PROBES`.
///
/// A row the acquire reply never named carries no plan id, so no `renewjob`
/// call can be built for it. It stays provisional and retains its permit
/// lease; a lost acquire reply is ambiguous and must never free capacity by
/// inference. Likewise, a spent probe budget ends renewal attempts, not the
/// durable ownership fence.
///
/// `release_exact_permit` must release the recorded holder from the recorded
/// ledger at the recorded immutable lease generation. A persisted
/// `AcquireJobNotSent` and `AcquireJobNotFound` proofs are replayed before any
/// network probe; release is idempotent, and the provisional row remains until
/// the terminal event lands.
///
/// # Errors
/// Journal read or write failure, missing permit evidence for a `NotOurs`
/// verdict, or failure to durably release the exact permit lease.
pub fn resolve_provisional_acquisitions<P, R>(
    journal: &mut Journal,
    now: u64,
    mut probe: P,
    mut release_exact_permit: R,
) -> anyhow::Result<Vec<ResolvedAcquisition>>
where
    P: FnMut(&JobRecord) -> AcquisitionVerdict,
    R: FnMut(&AcquisitionLossProof) -> anyhow::Result<()>,
{
    let state = journal.materialized_state()?;
    let pending: Vec<JobRecord> = state
        .jobs
        .iter()
        .filter(|job| job.provisional)
        .cloned()
        .collect();
    let mut resolved = Vec::new();
    for row in pending {
        let job_id = row.job_id.clone();
        let generation = row.generation;

        // The proof was persisted before release. Replay it idempotently
        // before considering planless-intent or probe-budget handling.
        if let Some(proof) = row.acquisition_loss_proof.clone() {
            release_exact_permit(&proof).with_context(|| {
                format!(
                    "replay exact native permit release for acquisition {} generation {}",
                    job_id.0, proof.permit_lease.generation
                )
            })?;
            abandon_acquisition(
                journal,
                &job_id,
                generation,
                proof,
                "replayed durable run-service acquisition-loss proof",
            )?;
            resolved.push(ResolvedAcquisition {
                job_id,
                verdict: AcquisitionVerdict::Indeterminate,
                abandoned: true,
            });
            continue;
        }

        // Nothing to renew: the acquire reply never named a plan. That does
        // not prove the remote rejected the request; retain the row and its
        // permit pointer so terminal evidence can resolve it later.
        if row.plan_id.is_empty() {
            resolved.push(ResolvedAcquisition {
                job_id,
                verdict: AcquisitionVerdict::Indeterminate,
                abandoned: false,
            });
            continue;
        }

        // Budget first, so a spent row costs no further lease renewal. The
        // exhausted budget is local retry evidence only; it cannot release an
        // uncertain remote assignment or its host permit.
        if row.probe_budget_exhausted(now) {
            resolved.push(ResolvedAcquisition {
                job_id,
                verdict: AcquisitionVerdict::Indeterminate,
                abandoned: false,
            });
            continue;
        }

        let verdict = probe(&row);
        let abandoned = match verdict {
            AcquisitionVerdict::Owned => {
                confirm_acquisition(journal, &job_id, &row.slot_id, generation)?;
                false
            }
            AcquisitionVerdict::NotOurs => {
                let permit_lease = row.permit_lease.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "run service proved acquisition {} is not ours, but its exact native permit lease is unknown; retaining the recovery row",
                        job_id.0
                    )
                })?;
                let proof = AcquisitionLossProof {
                    source: AcquisitionLossSource::RenewJobNotOurs,
                    permit_lease: permit_lease.clone(),
                };
                release_exact_permit(&proof).with_context(|| {
                    format!(
                        "release exact native permit {} generation {} before abandoning acquisition {}",
                        permit_lease.holder, permit_lease.generation, job_id.0
                    )
                })?;
                abandon_acquisition(
                    journal,
                    &job_id,
                    generation,
                    proof,
                    "run service reports the job is not held by this runner",
                )?;
                true
            }
            AcquisitionVerdict::Indeterminate => {
                charge_acquisition_probe(journal, &job_id, generation);
                false
            }
        };
        resolved.push(ResolvedAcquisition {
            job_id,
            verdict,
            abandoned,
        });
    }
    Ok(resolved)
}

/// Charge one spent probe to the row's durable budget.
///
/// A journal failure here must not abort recovery: the row keeps its previous
/// count and the next restart charges again. What it must never do is silently
/// leave the count still — that is the difference between a bounded row and one
/// that renews forever.
fn charge_acquisition_probe(journal: &mut Journal, job_id: &JobId, generation: Generation) {
    match journal.apply(Event::AcquisitionProbeFailed {
        job_id: job_id.clone(),
        generation,
    }) {
        Err(error) => eprintln!(
            "Warning: could not charge an acquisition probe for job {}: {error}",
            job_id.0
        ),
        Ok(outcome) if outcome.rejected => eprintln!(
            "Warning: acquisition probe charge for job {} was rejected by the journal",
            job_id.0
        ),
        Ok(_) => {}
    }
}

/// Mark an owned job Running once workflow execution begins.
///
/// The daemon calls this right after its store `JobStarted` transition, so
/// the journal phase tracks the execution the store already recorded. A
/// rejection means the owned row went missing or moved generation underneath
/// the caller.
///
/// # Errors
/// Journal write failure, or a rejection (no owned job at this generation).
pub fn record_job_started(
    journal: &mut Journal,
    job_id: &JobId,
    generation: Generation,
) -> anyhow::Result<()> {
    let outcome = journal.apply(Event::JobStarted {
        job_id: job_id.clone(),
        generation,
    })?;
    if outcome.rejected {
        anyhow::bail!(
            "job start rejected for job {} generation {}",
            job_id.0,
            generation.0
        );
    }
    Ok(())
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
        .filter(|job| matches!(job.phase, JobPhase2::Assigned | JobPhase2::Running))
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

/// Create the durable send claim for this job generation.
///
/// A new caller cannot create a second claim. After proving the previous
/// waiter dead, recovery may reissue transport under this existing claim; a
/// transport error leaves the remote outcome unknown.
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
    use velnor_control::journal::{Event, ACQUISITION_RESOLUTION_SECONDS, MAX_ACQUISITION_PROBES};
    use velnor_model::{Generation, SlotId, SlotPhase2};

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
        let runner_request_id = format!("request-{}", job_id.0);
        let provisional_job_id = JobId(runner_request_id.clone());
        let permit_lease = test_native_permit_lease(&runner_request_id);
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
            Event::JobAcquisitionIntendedWithPermit {
                slot_id: slot.clone(),
                job_id: provisional_job_id.clone(),
                generation: g,
                message_id: "message-1".into(),
                runner_request_id: runner_request_id.clone(),
                run_service_url: "https://run.example/run".into(),
                intended_unix: 1_000,
                permit_lease: permit_lease.clone(),
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        assert!(
            !journal
                .apply(Event::JobAcquisitionResolved {
                    provisional_job_id,
                    acquired_job_id: job_id.clone(),
                    plan_id: "plan-1".into(),
                    generation: g,
                    runner_request_id: Some(runner_request_id),
                    permit_lease: Some(permit_lease),
                })
                .unwrap()
                .rejected
        );
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

    /// Own a job through the acquisition path: intend, resolve, confirm.
    fn own_next_job(journal: &mut Journal, job_id: &JobId, slot_id: &SlotId) -> Generation {
        let runner_request_id = job_id.0.clone();
        let permit_lease = test_native_permit_lease(&runner_request_id);
        let generation = intend_acquisition_with_permit(
            journal,
            job_id,
            slot_id,
            "msg-1",
            &runner_request_id,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap();
        resolve_acquisition_for_request_with_permit(
            journal,
            job_id,
            job_id,
            "plan-1",
            generation,
            &runner_request_id,
            permit_lease,
        )
        .unwrap();
        confirm_acquisition(journal, job_id, slot_id, generation).unwrap();
        generation
    }

    fn prime_ready_slot(journal: &mut Journal) -> (SlotId, Generation) {
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
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        (slot, g)
    }

    /// Drive one acquisition to the point a crash is interesting: the intent is
    /// durable and the acquire reply has named the job, so the row carries the
    /// plan `renewjob` needs.
    fn intend_and_retarget(journal: &mut Journal, slot: &SlotId, message: &str, job: &JobId) {
        let permit_lease = test_native_permit_lease(message);
        intend_acquisition_with_permit(
            journal,
            &JobId(message.to_owned()),
            slot,
            message,
            message,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap();
        let generation = journal.materialized_state().unwrap().slots[0].generation;
        resolve_acquisition_for_request_with_permit(
            journal,
            &JobId(message.to_owned()),
            job,
            "plan-1",
            generation,
            message,
            permit_lease,
        )
        .unwrap();
    }

    const RUN_SERVICE_URL: &str = "https://run.example/run";
    const INTENDED_UNIX: u64 = 1_000;

    fn test_native_permit_lease(request_id: &str) -> NativePermitLease {
        NativePermitLease {
            holder: crate::permit_guard::native_permit_holder(
                "https://github.com/tailrocks/fixture",
                request_id,
            ),
            ledger_path: std::env::temp_dir()
                .join(format!("velnor-permit-ledger-{request_id}.db"))
                .to_string_lossy()
                .into_owned(),
            generation: 1,
        }
    }

    /// A crash between `acquirejob` returning 200 and the durable marker used
    /// to leave no local record at all: no renewal, no completion, and the job
    /// lost until its lease expired. The intent row survives that crash, and
    /// `renewjob` — which only the lease holder can call — resolves it.
    #[test]
    fn a_crash_in_the_acquire_window_recovers_the_job_it_owns() {
        let dir = tmp("acquire-window-owned");
        let job = JobId("guid-1".into());
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, _) = prime_ready_slot(&mut journal);
            intend_and_retarget(&mut journal, &slot, "msg-1", &job);
            // Process dies here, after the 200 and before the durable marker.
        }

        let mut journal = restart(&dir);
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1, "the intent survived the crash");
        assert!(state.jobs[0].provisional);
        assert_eq!(
            state.jobs[0].job_id, job,
            "the row is keyed by the acquired identity, not the broker message"
        );

        let mut probed = Vec::new();
        let resolved = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |row| {
                probed.push((row.plan_id.clone(), row.run_service_url.clone()));
                AcquisitionVerdict::Owned
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(
            probed,
            vec![("plan-1".to_owned(), RUN_SERVICE_URL.to_owned())],
            "the probe is handed everything renewjob needs"
        );
        assert_eq!(
            resolved,
            vec![ResolvedAcquisition {
                job_id: job.clone(),
                verdict: AcquisitionVerdict::Owned,
                abandoned: false,
            }]
        );
        let state = journal.materialized_state().unwrap();
        assert!(
            !state.jobs[0].provisional,
            "a successful renew proves the job is ours"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn native_acquisition_resolution_requires_exact_request_and_permit() {
        let dir = tmp("native-acquisition-exact-resolution");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot_id, generation) = prime_ready_slot(&mut journal);
        let runner_request_id = "native-request-1";
        let provisional_job_id = JobId(runner_request_id.to_owned());
        let acquired_job_id = JobId("run-service-job-1".to_owned());
        let permit_lease = test_native_permit_lease(runner_request_id);
        intend_acquisition_with_permit(
            &mut journal,
            &provisional_job_id,
            &slot_id,
            "broker-message-1",
            runner_request_id,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap();

        let wrong_request = resolve_acquisition_for_request_with_permit(
            &mut journal,
            &provisional_job_id,
            &acquired_job_id,
            "plan-1",
            generation,
            "different-request",
            permit_lease.clone(),
        )
        .unwrap_err();
        assert!(wrong_request
            .to_string()
            .contains("acquisition retarget rejected"));

        let mut wrong_permit = permit_lease.clone();
        wrong_permit.holder.push_str("-replacement");
        let mismatched_permit = resolve_acquisition_for_request_with_permit(
            &mut journal,
            &provisional_job_id,
            &acquired_job_id,
            "plan-1",
            generation,
            runner_request_id,
            wrong_permit,
        )
        .unwrap_err();
        assert!(mismatched_permit
            .to_string()
            .contains("acquisition retarget rejected"));

        let unresolved = journal.materialized_state().unwrap();
        assert_eq!(unresolved.jobs.len(), 1);
        assert_eq!(unresolved.jobs[0].job_id, provisional_job_id);
        assert!(unresolved.jobs[0].provisional);
        assert!(unresolved.jobs[0].plan_id.is_empty());
        assert_eq!(unresolved.jobs[0].runner_request_id, runner_request_id);
        assert_eq!(
            unresolved.jobs[0].permit_lease.as_ref(),
            Some(&permit_lease)
        );

        resolve_acquisition_for_request_with_permit(
            &mut journal,
            &provisional_job_id,
            &acquired_job_id,
            "plan-1",
            generation,
            runner_request_id,
            permit_lease.clone(),
        )
        .unwrap();
        let resolved = journal.materialized_state().unwrap();
        assert_eq!(resolved.jobs.len(), 1);
        assert_eq!(resolved.jobs[0].job_id, acquired_job_id);
        assert!(resolved.jobs[0].provisional);
        assert_eq!(resolved.jobs[0].plan_id, "plan-1");
        assert_eq!(resolved.jobs[0].runner_request_id, runner_request_id);
        assert_eq!(resolved.jobs[0].permit_lease.as_ref(), Some(&permit_lease));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn native_reintent_rebuild_preserves_exact_permit_lease() {
        let dir = tmp("native-acquisition-reintent-rebuild");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot_id, generation) = prime_ready_slot(&mut journal);
        let runner_request_id = "native-rebuild-request-1";
        let acquired_job_id = JobId("run-service-rebuilt-job-1".to_owned());
        let permit_lease = test_native_permit_lease(runner_request_id);

        assert!(
            reintend_resolved_acquisition_with_permit(
                &mut journal,
                &acquired_job_id,
                &slot_id,
                runner_request_id,
                "plan-rebuilt-1",
                RUN_SERVICE_URL,
                INTENDED_UNIX,
                permit_lease.clone(),
            )
            .unwrap(),
            "a confirmed acquisition absent from the journal must be rebuilt"
        );
        let rebuilt = journal.materialized_state().unwrap();
        assert_eq!(rebuilt.jobs.len(), 1);
        let row = &rebuilt.jobs[0];
        assert_eq!(row.job_id, acquired_job_id);
        assert_eq!(row.slot_id, slot_id);
        assert_eq!(row.generation, generation);
        assert!(row.provisional);
        assert_eq!(row.runner_request_id, runner_request_id);
        assert_eq!(row.plan_id, "plan-rebuilt-1");
        assert_eq!(row.run_service_url, RUN_SERVICE_URL);
        assert_eq!(row.permit_lease.as_ref(), Some(&permit_lease));
        assert_eq!(
            row.probe_deadline_unix,
            INTENDED_UNIX.saturating_add(ACQUISITION_RESOLUTION_SECONDS)
        );

        assert!(
            !reintend_resolved_acquisition_with_permit(
                &mut journal,
                &acquired_job_id,
                &slot_id,
                runner_request_id,
                "plan-rebuilt-1",
                RUN_SERVICE_URL,
                INTENDED_UNIX,
                permit_lease.clone(),
            )
            .unwrap(),
            "redelivery with the same request and exact lease must adopt the existing row"
        );

        let mut mismatched_lease = permit_lease.clone();
        mismatched_lease.holder.push_str("-replacement");
        let mismatch = reintend_resolved_acquisition_with_permit(
            &mut journal,
            &acquired_job_id,
            &slot_id,
            runner_request_id,
            "plan-rebuilt-1",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            mismatched_lease,
        )
        .unwrap_err();
        assert!(mismatch
            .to_string()
            .contains("resolved provisional job run-service-rebuilt-job-1 cannot be adopted"));

        let unchanged = journal.materialized_state().unwrap();
        assert_eq!(unchanged.jobs.len(), 1);
        assert_eq!(unchanged.jobs[0].job_id, acquired_job_id);
        assert_eq!(unchanged.jobs[0].runner_request_id, runner_request_id);
        assert_eq!(unchanged.jobs[0].permit_lease.as_ref(), Some(&permit_lease));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn native_reintent_rejects_wrong_slot_without_mutating_existing_rows() {
        let wrong_slot = SlotId("scope-2".to_owned());

        let owned_dir = tmp("native-reintent-owned-wrong-slot");
        let mut owned_journal = Journal::open(owned_dir.join("journal.db")).unwrap();
        let owned_job_id = JobId("guid-owned".to_owned());
        prime_owned(&mut owned_journal, &owned_job_id);
        let owned_request_id = "request-guid-owned";
        let owned_permit = test_native_permit_lease(owned_request_id);
        let before_owned = owned_journal.materialized_state().unwrap();

        let owned_error = reintend_resolved_acquisition_with_permit(
            &mut owned_journal,
            &owned_job_id,
            &wrong_slot,
            owned_request_id,
            "plan-1",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            owned_permit,
        )
        .unwrap_err();
        assert!(owned_error
            .to_string()
            .contains("acquisition redelivery names slot"));
        drop(owned_journal);
        let after_owned = Journal::open(owned_dir.join("journal.db"))
            .unwrap()
            .materialized_state()
            .unwrap();
        assert_eq!(after_owned, before_owned);
        std::fs::remove_dir_all(owned_dir).ok();

        let provisional_dir = tmp("native-reintent-provisional-wrong-slot");
        let mut provisional_journal = Journal::open(provisional_dir.join("journal.db")).unwrap();
        let (slot_id, generation) = prime_ready_slot(&mut provisional_journal);
        let request_id = "native-wrong-slot-request";
        let provisional_job_id = JobId(request_id.to_owned());
        let acquired_job_id = JobId("run-service-wrong-slot-job".to_owned());
        let permit_lease = test_native_permit_lease(request_id);
        intend_acquisition_with_permit(
            &mut provisional_journal,
            &provisional_job_id,
            &slot_id,
            "message-wrong-slot",
            request_id,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap();
        resolve_acquisition_for_request_with_permit(
            &mut provisional_journal,
            &provisional_job_id,
            &acquired_job_id,
            "plan-1",
            generation,
            request_id,
            permit_lease.clone(),
        )
        .unwrap();
        let before_provisional = provisional_journal.materialized_state().unwrap();

        let provisional_error = reintend_resolved_acquisition_with_permit(
            &mut provisional_journal,
            &acquired_job_id,
            &wrong_slot,
            request_id,
            "plan-1",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease,
        )
        .unwrap_err();
        assert!(provisional_error
            .to_string()
            .contains("acquisition redelivery names slot"));
        drop(provisional_journal);
        let after_provisional = Journal::open(provisional_dir.join("journal.db"))
            .unwrap()
            .materialized_state()
            .unwrap();
        assert_eq!(after_provisional, before_provisional);
        std::fs::remove_dir_all(provisional_dir).ok();
    }

    #[test]
    fn a_job_another_runner_holds_is_dropped_and_the_slot_freed() {
        let dir = tmp("acquire-window-not-ours");
        let job = JobId("guid-1".into());
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, _) = prime_ready_slot(&mut journal);
            intend_and_retarget(&mut journal, &slot, "msg-1", &job);
        }

        let mut journal = restart(&dir);
        let expected_lease = test_native_permit_lease("msg-1");
        let resolved = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| AcquisitionVerdict::NotOurs,
            |proof| {
                assert_eq!(&proof.permit_lease, &expected_lease);
                assert_eq!(proof.source, AcquisitionLossSource::RenewJobNotOurs);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            resolved,
            vec![ResolvedAcquisition {
                job_id: job,
                verdict: AcquisitionVerdict::NotOurs,
                abandoned: true,
            }]
        );
        assert!(
            journal.materialized_state().unwrap().jobs.is_empty(),
            "the slot is freed for the next job"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_permit_release_retains_the_pointer_and_retry_completes_cleanup() {
        let dir = tmp("acquire-release-failure-replay");
        let job = JobId("guid-1".into());
        let permit_lease = test_native_permit_lease("msg-1");
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, _) = prime_ready_slot(&mut journal);
            intend_and_retarget(&mut journal, &slot, "msg-1", &job);
        }

        let mut journal = restart(&dir);
        let release_failed = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| AcquisitionVerdict::NotOurs,
            |proof| {
                assert_eq!(&proof.permit_lease, &permit_lease);
                assert_eq!(proof.source, AcquisitionLossSource::RenewJobNotOurs);
                anyhow::bail!("injected permit ledger failure")
            },
        );
        assert!(release_failed.is_err());
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs[0].provisional);
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&permit_lease));

        let retried = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| AcquisitionVerdict::NotOurs,
            |proof| {
                assert_eq!(&proof.permit_lease, &permit_lease);
                assert_eq!(proof.source, AcquisitionLossSource::RenewJobNotOurs);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(retried.len(), 1);
        assert!(retried[0].abandoned);
        assert!(journal.materialized_state().unwrap().jobs.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn recorded_acquire_not_sent_proof_replays_after_release_failure_without_probe() {
        let dir = tmp("acquire-not-sent-release-replay");
        let job_id = JobId("request-1".into());
        let permit_lease = test_native_permit_lease("request-1");
        let proof = AcquisitionLossProof {
            source: AcquisitionLossSource::AcquireJobNotSent,
            permit_lease: permit_lease.clone(),
        };
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, generation) = prime_ready_slot(&mut journal);
            intend_acquisition_with_permit(
                &mut journal,
                &job_id,
                &slot,
                "request-1",
                "request-1",
                RUN_SERVICE_URL,
                INTENDED_UNIX,
                permit_lease.clone(),
            )
            .unwrap();
            record_acquisition_loss_proof(&mut journal, &job_id, generation, proof.clone())
                .unwrap();
        }

        let mut journal = restart(&dir);
        let mut probes = 0;
        let release_failed = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| {
                probes += 1;
                panic!("a durable not-sent proof must bypass renewjob")
            },
            |recorded| {
                assert_eq!(recorded, &proof);
                anyhow::bail!("injected eligible-release failure")
            },
        );
        assert!(release_failed.is_err());
        let state = journal.materialized_state().unwrap();
        assert_eq!(probes, 0);
        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs[0].provisional);
        assert_eq!(state.jobs[0].acquisition_loss_proof.as_ref(), Some(&proof));
        drop(journal);

        let mut journal = restart(&dir);
        let replayed = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| panic!("recorded not-sent proof must replay without probing"),
            |recorded| {
                assert_eq!(recorded, &proof);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            replayed,
            vec![ResolvedAcquisition {
                job_id,
                verdict: AcquisitionVerdict::Indeterminate,
                abandoned: true,
            }]
        );
        assert!(journal.materialized_state().unwrap().jobs.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_legacy_not_ours_verdict_without_permit_identity_stays_fenced() {
        let dir = tmp("acquire-legacy-no-permit");
        let job = JobId("guid-1".into());
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, generation) = prime_ready_slot(&mut journal);
            let provisional_job_id = JobId("msg-1".into());
            assert!(
                !journal
                    .apply(Event::JobAcquisitionIntended {
                        slot_id: slot,
                        job_id: provisional_job_id.clone(),
                        generation,
                        message_id: "msg-1".into(),
                        runner_request_id: Some("msg-1".into()),
                        run_service_url: RUN_SERVICE_URL.into(),
                        intended_unix: INTENDED_UNIX,
                    })
                    .unwrap()
                    .rejected
            );
            assert!(
                !journal
                    .apply(Event::JobAcquisitionResolved {
                        provisional_job_id,
                        acquired_job_id: job.clone(),
                        plan_id: "plan-1".into(),
                        generation,
                        runner_request_id: Some("msg-1".into()),
                        permit_lease: None,
                    })
                    .unwrap()
                    .rejected
            );
        }

        let mut journal = restart(&dir);
        let outcome = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| AcquisitionVerdict::NotOurs,
            |_| unreachable!("unknown legacy permit identity cannot be released"),
        );
        assert!(outcome.is_err());
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert!(state.jobs[0].provisional);
        assert!(state.jobs[0].permit_lease.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    /// The case that must change nothing. Promoting a job we may not own would
    /// let two runners publish for it; dropping one we do own would strand it
    /// until the lease expired. So an unreachable run service leaves the row
    /// exactly as it was, for the next attempt — one probe poorer.
    #[test]
    fn an_indeterminate_probe_leaves_the_row_untouched() {
        let dir = tmp("acquire-window-indeterminate");
        let job = JobId("guid-1".into());
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, _) = prime_ready_slot(&mut journal);
            intend_and_retarget(&mut journal, &slot, "msg-1", &job);
        }

        let mut journal = restart(&dir);
        let resolved = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| AcquisitionVerdict::Indeterminate,
            |_| unreachable!("indeterminate verdict must not release a permit"),
        )
        .unwrap();
        assert_eq!(
            resolved,
            vec![ResolvedAcquisition {
                job_id: job,
                verdict: AcquisitionVerdict::Indeterminate,
                abandoned: false,
            }]
        );
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert!(
            state.jobs[0].provisional,
            "still provisional, still ours to resolve later"
        );
        assert_eq!(
            state.jobs[0].probe_attempts, 1,
            "the probe that reached no verdict is still charged"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// `renewjob` extends the lease as a side effect, and the one indeterminate
    /// path that still moves it is a 2xx whose body fails to parse: renewed
    /// remotely, `Err` locally. Without a bound, every restart would renew a
    /// job this node never manages to run. Assert the renewals stop.
    #[test]
    fn the_probe_budget_bounds_renewals_but_keeps_uncertain_ownership_fenced() {
        let dir = tmp("acquire-probe-bounded");
        let job = JobId("guid-1".into());
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot, _) = prime_ready_slot(&mut journal);
        intend_and_retarget(&mut journal, &slot, "msg-1", &job);

        let mut renewals = 0u32;
        // Far more restarts than the budget allows.
        for _ in 0..(MAX_ACQUISITION_PROBES * 4) {
            resolve_provisional_acquisitions(
                &mut journal,
                INTENDED_UNIX,
                |_| {
                    renewals += 1;
                    AcquisitionVerdict::Indeterminate
                },
                |_| unreachable!("indeterminate verdict must not release a permit"),
            )
            .unwrap();
        }
        assert_eq!(
            renewals, MAX_ACQUISITION_PROBES,
            "the lease is renewed at most MAX_ACQUISITION_PROBES times, ever"
        );
        assert_eq!(
            journal.materialized_state().unwrap().jobs.len(),
            1,
            "a spent local probe budget does not prove the remote lease is gone"
        );
        assert!(journal.materialized_state().unwrap().jobs[0].provisional);
        std::fs::remove_dir_all(dir).ok();
    }

    /// The deadline spends the budget on its own, so a node that restarts only
    /// a few times still stops renewing once the run service would time the job
    /// out anyway.
    #[test]
    fn the_probe_is_bounded_by_its_deadline_even_with_attempts_left() {
        let dir = tmp("acquire-probe-deadline");
        let job = JobId("guid-1".into());
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot, _) = prime_ready_slot(&mut journal);
        intend_and_retarget(&mut journal, &slot, "msg-1", &job);

        let mut probes = 0u32;
        let resolved = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX + ACQUISITION_RESOLUTION_SECONDS,
            |_| {
                probes += 1;
                AcquisitionVerdict::Owned
            },
            |_| unreachable!("a skipped probe cannot release a permit"),
        )
        .unwrap();
        assert_eq!(probes, 0, "a row past its deadline is not probed at all");
        assert_eq!(resolved.len(), 1);
        assert!(!resolved[0].abandoned);
        assert_eq!(journal.materialized_state().unwrap().jobs.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A crash before the acquire reply was read leaves a row keyed by the
    /// broker message, with no plan. That does not prove the acquire failed:
    /// the reply may have been lost after remote assignment. Keep both the
    /// correlation and exact host lease until authoritative evidence appears.
    #[test]
    fn a_planless_row_keeps_its_exact_permit_without_a_probe() {
        let dir = tmp("acquire-window-unnamed");
        let permit_lease = test_native_permit_lease("msg-1");
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, _) = prime_ready_slot(&mut journal);
            intend_acquisition_with_permit(
                &mut journal,
                &JobId("msg-1".into()),
                &slot,
                "msg-1",
                "msg-1",
                RUN_SERVICE_URL,
                INTENDED_UNIX,
                permit_lease.clone(),
            )
            .unwrap();
        }

        let mut journal = restart(&dir);
        let mut probes = 0u32;
        let resolved = resolve_provisional_acquisitions(
            &mut journal,
            INTENDED_UNIX,
            |_| {
                probes += 1;
                AcquisitionVerdict::Owned
            },
            |_| unreachable!("a planless row cannot yield a release verdict"),
        )
        .unwrap();
        assert_eq!(probes, 0, "there is nothing to renew");
        assert_eq!(
            resolved,
            vec![ResolvedAcquisition {
                job_id: JobId("msg-1".into()),
                verdict: AcquisitionVerdict::Indeterminate,
                abandoned: false,
            }]
        );
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&permit_lease));
        std::fs::remove_dir_all(dir).ok();
    }

    /// Retrying the same broker message must not fork the row.
    #[test]
    fn repeating_the_intent_reuses_the_row() {
        let dir = tmp("acquire-intent-idempotent");
        let job = JobId("msg-1".into());
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot, _) = prime_ready_slot(&mut journal);
        let other_slot = SlotId("scope-2".into());
        assert!(
            !journal
                .apply(Event::DesiredCapacity { ready: 2 })
                .unwrap()
                .rejected
        );
        for event in [
            Event::PermitReserved {
                slot_id: other_slot.clone(),
                generation: Generation::INITIAL,
            },
            Event::ExecutorProven {
                slot_id: other_slot.clone(),
                generation: Generation::INITIAL,
            },
            Event::SessionLive {
                slot_id: other_slot.clone(),
                generation: Generation::INITIAL,
            },
            Event::RegistrationIntended {
                slot_id: other_slot.clone(),
                generation: Generation::INITIAL,
            },
            Event::Registered {
                slot_id: other_slot.clone(),
                generation: Generation::INITIAL,
            },
            Event::ReadyAttempt {
                slot_id: other_slot.clone(),
                generation: Generation::INITIAL,
            },
        ] {
            assert!(!journal.apply(event).unwrap().rejected);
        }
        let permit_lease = test_native_permit_lease(&job.0);
        let first = intend_acquisition_with_permit(
            &mut journal,
            &job,
            &slot,
            "msg-1",
            &job.0,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap();
        let second = intend_acquisition_with_permit(
            &mut journal,
            &job,
            &slot,
            "msg-1",
            &job.0,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap();
        assert_eq!(first, second);
        let stable_state = journal.materialized_state().unwrap();

        let other_request_id = "other-request";
        let distinct_request_with_same_lease = intend_acquisition_with_permit(
            &mut journal,
            &JobId(other_request_id.to_owned()),
            &other_slot,
            "other-message",
            other_request_id,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap_err();
        assert!(distinct_request_with_same_lease
            .to_string()
            .contains("native acquisition intent rejected"));

        let wrong_slot = intend_acquisition_with_permit(
            &mut journal,
            &job,
            &SlotId("scope-2".into()),
            "msg-1",
            &job.0,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap_err();
        assert!(wrong_slot
            .to_string()
            .contains("already has a different or active journal owner"));

        let mut wrong_lease = permit_lease.clone();
        wrong_lease.holder.push_str("-replacement");
        let mismatched_lease = intend_acquisition_with_permit(
            &mut journal,
            &job,
            &slot,
            "msg-1",
            &job.0,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            wrong_lease,
        )
        .unwrap_err();
        assert!(mismatched_lease
            .to_string()
            .contains("already has a different or active journal owner"));

        let mut wrong_ledger_path = permit_lease.clone();
        wrong_ledger_path.ledger_path.push_str(".replacement");
        let mismatched_ledger_path = intend_acquisition_with_permit(
            &mut journal,
            &job,
            &slot,
            "msg-1",
            &job.0,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            wrong_ledger_path,
        )
        .unwrap_err();
        assert!(mismatched_ledger_path
            .to_string()
            .contains("already has a different or active journal owner"));

        let mut wrong_lease_generation = permit_lease.clone();
        wrong_lease_generation.generation += 1;
        let mismatched_generation = intend_acquisition_with_permit(
            &mut journal,
            &job,
            &slot,
            "msg-1",
            &job.0,
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            wrong_lease_generation,
        )
        .unwrap_err();
        assert!(mismatched_generation
            .to_string()
            .contains("already has a different or active journal owner"));

        let state = journal.materialized_state().unwrap();
        assert_eq!(state, stable_state);
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, job);
        assert_eq!(state.jobs[0].slot_id, slot);
        assert_eq!(state.jobs[0].generation, first);
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&permit_lease));
        std::fs::remove_dir_all(dir).ok();
    }

    /// Simulate process death: every boundary below is a durable commit, so
    /// dropping the handle and reopening the file is exactly what a restart
    /// sees.
    fn restart(dir: &Path) -> Journal {
        Journal::open(dir.join("journal.db")).unwrap()
    }

    /// A planless request-keyed intent can survive a process restart. Redelivery
    /// resolves that exact row onto the acquired job without losing correlation.
    #[test]
    fn a_planless_known_request_row_is_resolved_on_redelivery() {
        let dir = tmp("planless-request-redelivery");
        let acquired_job = JobId("guid-1".into());
        let request_job = JobId("request-1".into());
        let permit_lease = test_native_permit_lease(&request_job.0);
        {
            let mut journal = Journal::open(dir.join("journal.db")).unwrap();
            let (slot, _) = prime_ready_slot(&mut journal);
            intend_acquisition_with_permit(
                &mut journal,
                &request_job,
                &slot,
                "broker-message-1",
                &request_job.0,
                RUN_SERVICE_URL,
                INTENDED_UNIX,
                permit_lease.clone(),
            )
            .unwrap();
        }

        let mut journal = restart(&dir);
        reintend_resolved_acquisition_with_permit(
            &mut journal,
            &acquired_job,
            &SlotId("scope-1".into()),
            "request-1",
            "plan-1",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap();

        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1, "the request row is resolved in place");
        assert_eq!(state.jobs[0].job_id, acquired_job);
        assert_eq!(state.jobs[0].plan_id, "plan-1");
        assert_eq!(state.jobs[0].run_service_url, RUN_SERVICE_URL);
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert!(state.jobs[0].provisional, "still not ownership");
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&permit_lease));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_missing_provisional_row_rebuilds_atomically_and_survives_restart() {
        let dir = tmp("rebuild-missing-provisional");
        let acquired_job = JobId("run-service-job-1".into());
        let permit_lease = test_native_permit_lease("request-1");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        prime_ready_slot(&mut journal);
        // The 200 already exists; a drain that arrived after acquisition must
        // not reject recording the known ownership.
        journal.set_admission_blocked(1).unwrap();

        assert!(reintend_resolved_acquisition_with_permit(
            &mut journal,
            &acquired_job,
            &SlotId("scope-1".into()),
            "request-1",
            "plan-1",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            permit_lease.clone(),
        )
        .unwrap());
        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, acquired_job);
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert_eq!(state.jobs[0].plan_id, "plan-1");
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&permit_lease));

        drop(journal);
        let recovered = restart(&dir);
        let state = recovered.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert_eq!(state.jobs[0].job_id, acquired_job);
        assert_eq!(state.jobs[0].runner_request_id, "request-1");
        assert_eq!(state.jobs[0].plan_id, "plan-1");
        assert_eq!(state.jobs[0].permit_lease.as_ref(), Some(&permit_lease));
        assert!(state.jobs[0].provisional);
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        std::fs::remove_dir_all(dir).ok();
    }

    /// A rebuild that arrives after ownership is a no-op: the row it would
    /// write already says more than the rebuild could.
    #[test]
    fn a_rebuild_never_touches_a_row_that_is_already_owned() {
        let dir = tmp("rebuild-after-ownership");
        let job = JobId("guid-1".into());
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let slot = SlotId("scope-1".into());
        let generation = prime_owned(&mut journal, &job);

        reintend_resolved_acquisition_with_permit(
            &mut journal,
            &job,
            &slot,
            "request-guid-1",
            "plan-1",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            test_native_permit_lease("request-guid-1"),
        )
        .unwrap();

        let state = journal.materialized_state().unwrap();
        assert_eq!(state.jobs.len(), 1);
        assert!(!state.jobs[0].provisional);
        assert_eq!(state.jobs[0].generation, generation);
        assert_eq!(state.jobs[0].runner_request_id, "request-guid-1");
        assert!(state.outbox.iter().all(|row| row.job_id != job));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn proofless_legacy_self_resolution_stays_provisional_and_rejects_redelivery() {
        let dir = tmp("rebuild-unknown-owned-correlation");
        let acquired_job = JobId("run-service-job-1".into());
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot_id, generation) = prime_ready_slot(&mut journal);
        assert!(
            !journal
                .apply(Event::JobAcquisitionIntended {
                    slot_id: slot_id.clone(),
                    job_id: acquired_job.clone(),
                    generation,
                    message_id: "request-1".into(),
                    runner_request_id: None,
                    run_service_url: RUN_SERVICE_URL.into(),
                    intended_unix: INTENDED_UNIX,
                })
                .unwrap()
                .rejected
        );
        let resolution = journal
            .apply(Event::JobAcquisitionResolved {
                provisional_job_id: acquired_job.clone(),
                acquired_job_id: acquired_job.clone(),
                plan_id: "plan-1".into(),
                generation,
                runner_request_id: None,
                permit_lease: None,
            })
            .unwrap();
        assert!(
            resolution.rejected,
            "a self-resolve without request proof is ambiguous"
        );
        let state = journal.materialized_state().unwrap();
        assert!(state.jobs[0].provisional);
        assert!(state.jobs[0].plan_id.is_empty());
        assert_eq!(state.jobs[0].runner_request_id, acquired_job.0);
        assert!(
            journal
                .apply(Event::JobOwned {
                    job_id: acquired_job.clone(),
                    slot_id: slot_id.clone(),
                    attempt: 1,
                    generation,
                    worker: "worker-1".into(),
                    accepted_unix: INTENDED_UNIX,
                })
                .unwrap()
                .rejected,
            "an unproven legacy acquisition cannot be promoted into ownership"
        );

        let error = reintend_resolved_acquisition_with_permit(
            &mut journal,
            &acquired_job,
            &slot_id,
            "request-1",
            "plan-1",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            test_native_permit_lease("request-1"),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("stored correlation is run-service-job-1"));
        assert_eq!(journal.materialized_state().unwrap().jobs.len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }

    fn pending_row(
        journal: &Journal,
        job_id: &JobId,
    ) -> Option<velnor_control::journal::OutboxRecord> {
        journal
            .materialized_state()
            .unwrap()
            .outbox
            .into_iter()
            .find(|row| row.job_id == *job_id)
    }

    fn spend_budget(journal: &mut Journal, job_id: &JobId, generation: Generation) {
        for _ in 0..velnor_control::journal::MAX_COMPLETION_ATTEMPTS {
            journal
                .apply(Event::CompletionAttemptFailed {
                    job_id: job_id.clone(),
                    generation,
                    permanent: false,
                })
                .unwrap();
        }
    }

    #[test]
    fn crash_before_journal_admission_leaves_no_completion_to_replay() {
        // Boundary: acquired remotely, dead before the durable marker.
        let dir = tmp("crash-before-admission");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        drop(journal.materialized_state().unwrap());
        assert!(
            ensure_owned(&mut journal, &job_id).is_err(),
            "an unadmitted job must never reach the send path"
        );
        drop(journal);

        let journal = restart(&dir);
        assert!(journal.pending_outbox().unwrap().is_empty());
        assert!(journal.materialized_state().unwrap().jobs.is_empty());
        assert!(!cleanup::outbox_path(&dir, &job_id.0, 1).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn provisional_acquisition_cannot_enter_completion_guard() {
        let dir = tmp("provisional-completion-guard");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot, _) = prime_ready_slot(&mut journal);
        let job_id = JobId("job-1".into());
        intend_and_retarget(&mut journal, &slot, "message-1", &job_id);

        let error = ensure_owned(&mut journal, &job_id).unwrap_err();
        assert!(error
            .to_string()
            .contains("provisional acquisition evidence"));
        assert!(journal.pending_outbox().unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn crash_after_terminal_result_keeps_the_real_conclusion() {
        // Boundary: terminal result durable, outbox payload not yet written.
        let dir = tmp("crash-after-terminal-result");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        let generation = prime_owned(&mut journal, &job_id);
        record_terminal_result(&mut journal, &job_id, generation, "succeeded").unwrap();
        drop(journal);

        let mut journal = restart(&dir);
        assert_eq!(
            journal
                .recorded_terminal_conclusion(&job_id, generation)
                .unwrap()
                .as_deref(),
            Some("succeeded"),
            "recovery must not turn a finished green job into a synthetic failure"
        );
        assert!(journal.pending_outbox().unwrap().is_empty());
        assert!(!cleanup::outbox_path(&dir, &job_id.0, generation.0).exists());

        // The completion is still drivable to a clean terminal send.
        guarded_complete(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"conclusion=success",
            || Ok::<(), anyhow::Error>(()),
        )
        .unwrap();
        assert!(journal.pending_outbox().unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn crash_after_the_send_claim_replays_under_the_same_claim() {
        // Boundary: claim durable, request never issued.
        let dir = tmp("crash-after-claim");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        let payload = b"conclusion=success".to_vec();
        let generation = prime_owned(&mut journal, &job_id);
        commit_intent(&mut journal, &dir, &job_id, generation, &payload).unwrap();
        claim_completion_send(&mut journal, &job_id, generation).unwrap();
        drop(journal);

        let mut journal = restart(&dir);
        let row = pending_row(&journal, &job_id).expect("claim survives the crash");
        assert!(row.send_started);
        assert!(!row.remote_acked);
        assert_eq!(row.attempts, 0);
        assert!(
            journal
                .apply(Event::CompletionSendStarted {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected,
            "a replay must never manufacture a second terminal send claim"
        );

        let mut sent = 0;
        replay_claimed_completion_async(
            &mut journal,
            &dir,
            &job_id,
            generation,
            payload,
            |_| async {
                sent += 1;
                Ok(((), CompletionAcknowledgement::Accepted))
            },
        )
        .await
        .unwrap();
        assert_eq!(sent, 1);
        assert!(journal.pending_outbox().unwrap().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn crash_after_the_request_terminalizes_without_a_duplicate_send() {
        // Boundary: the remote already has it; the acknowledgement was lost.
        let dir = tmp("crash-after-request");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        let payload = b"conclusion=success".to_vec();
        let generation = prime_owned(&mut journal, &job_id);
        commit_intent(&mut journal, &dir, &job_id, generation, &payload).unwrap();
        claim_completion_send(&mut journal, &job_id, generation).unwrap();
        drop(journal);

        let mut journal = restart(&dir);
        let mut sent = 0;
        replay_claimed_completion_async(
            &mut journal,
            &dir,
            &job_id,
            generation,
            payload,
            |_| async {
                sent += 1;
                // The run service reports the job as already terminal.
                Ok(((), CompletionAcknowledgement::RemoteObservedTerminal))
            },
        )
        .await
        .unwrap();
        assert_eq!(sent, 1, "recovery re-sends exactly once");
        assert!(journal.pending_outbox().unwrap().is_empty());
        let state = journal.materialized_state().unwrap();
        assert!(state.jobs.is_empty());
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_failed_send_charges_the_durable_budget_across_restarts() {
        let dir = tmp("charge-budget");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        let generation = prime_owned(&mut journal, &job_id);
        guarded_complete(
            &mut journal,
            &dir,
            &job_id,
            generation,
            b"conclusion=success",
            || Err::<(), anyhow::Error>(anyhow::anyhow!("complete_job 503")),
        )
        .unwrap_err();
        drop(journal);

        let journal = restart(&dir);
        assert_eq!(
            pending_row(&journal, &job_id).unwrap().attempts,
            1,
            "recovery must not restart the budget from zero on every cycle"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_unresolvable_completion_frees_its_slot_without_a_second_terminal_send() {
        let dir = tmp("unresolvable");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        let payload = b"conclusion=success".to_vec();
        let generation = prime_owned(&mut journal, &job_id);
        commit_intent(&mut journal, &dir, &job_id, generation, &payload).unwrap();
        claim_completion_send(&mut journal, &job_id, generation).unwrap();

        assert!(
            !abandon_unresolvable_completion(&mut journal, &dir, &job_id, generation, "impatient")
                .unwrap(),
            "the journal refuses a terminal state that durable state does not prove"
        );
        spend_budget(&mut journal, &job_id, generation);
        assert!(abandon_unresolvable_completion(
            &mut journal,
            &dir,
            &job_id,
            generation,
            "the durable completion send budget is spent",
        )
        .unwrap());
        drop(journal);

        let mut journal = restart(&dir);
        assert!(journal.pending_outbox().unwrap().is_empty());
        assert!(!cleanup::outbox_path(&dir, &job_id.0, generation.0).exists());
        assert_eq!(
            journal.materialized_state().unwrap().slots[0].phase,
            SlotPhase2::Ready,
        );
        // The abandonment is recorded, never disguised as a delivery.
        let abandoned = journal.unresolvable_completions().unwrap();
        assert_eq!(abandoned.len(), 1);
        assert_eq!(abandoned[0].job_id, job_id);
        assert!(
            journal
                .apply(Event::RemoteAcked {
                    job_id: job_id.clone(),
                    generation,
                })
                .unwrap()
                .rejected,
            "abandoning must never authorize a second terminal send"
        );
        // And the freed slot really does take the next job.
        own_next_job(
            &mut journal,
            &JobId("job-2".into()),
            &SlotId("scope-1".into()),
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_fenced_generation_can_never_publish_its_stale_completion() {
        let dir = tmp("fenced-replay");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("job-1".into());
        let payload = b"conclusion=success".to_vec();
        let generation = prime_owned(&mut journal, &job_id);
        commit_intent(&mut journal, &dir, &job_id, generation, &payload).unwrap();
        claim_completion_send(&mut journal, &job_id, generation).unwrap();
        journal
            .apply(Event::SlotStale {
                slot_id: SlotId("scope-1".into()),
                generation,
            })
            .unwrap();
        drop(journal);

        let mut journal = restart(&dir);
        let mut sent = 0;
        let error = replay_claimed_completion_async(
            &mut journal,
            &dir,
            &job_id,
            generation.next(),
            payload,
            |_| async {
                sent += 1;
                Ok(((), CompletionAcknowledgement::Accepted))
            },
        )
        .await
        .unwrap_err();
        assert_eq!(sent, 0, "{error}");
        std::fs::remove_dir_all(dir).ok();
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
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
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
        let second = own_next_job(
            &mut journal,
            &JobId("job-2".into()),
            &SlotId("scope-1".into()),
        );
        assert_eq!(
            second, generation,
            "Ready slot must accept the next job after RemoteAcked"
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
        assert_eq!(state.jobs[0].phase, JobPhase2::Completing);
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        let rejected = intend_acquisition_with_permit(
            &mut journal,
            &JobId("job-2".into()),
            &SlotId("scope-1".into()),
            "msg-2",
            "msg-2",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            test_native_permit_lease("msg-2"),
        )
        .unwrap_err();
        assert!(
            rejected.to_string().contains("acquisition intent rejected"),
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
        assert_eq!(state.slots[0].phase, SlotPhase2::Ready);
        // Slot must leave Completing after async complete_job.
        own_next_job(
            &mut journal,
            &JobId("job-next".into()),
            &SlotId("scope-1".into()),
        );
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
    fn acquisition_path_owns_a_ready_slot() {
        let dir = tmp("accept");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let (slot, g) = prime_ready_slot(&mut journal);
        let generation = own_next_job(&mut journal, &JobId("guid-1".into()), &slot);
        assert_eq!(generation, g);
        let state = journal.load_state().unwrap();
        assert_eq!(state.slots[0].phase, SlotPhase2::Assigned);
        assert_eq!(state.jobs[0].job_id.0, "guid-1");
        assert!(!state.jobs[0].provisional);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn acquisition_path_rejects_a_second_live_owner() {
        let dir = tmp("accept-second");
        let mut journal = Journal::open(dir.join("journal.db")).unwrap();
        let job_id = JobId("already".into());
        let _ = prime_owned(&mut journal, &job_id);
        let error = intend_acquisition_with_permit(
            &mut journal,
            &JobId("other".into()),
            &SlotId("scope-1".into()),
            "msg-other",
            "msg-other",
            RUN_SERVICE_URL,
            INTENDED_UNIX,
            test_native_permit_lease("msg-other"),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("acquisition intent rejected"),
            "{error}"
        );
        assert_eq!(
            ensure_owned(&mut journal, &job_id).unwrap(),
            Generation::INITIAL
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
