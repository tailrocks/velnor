//! Sanitized row types plus write/read helpers over the operational store.
//!
//! Every key carries `instance_slug`; [`Store::record_job_transition`] is
//! the atomic current-state-plus-event seam. Events are append-only: no
//! update or delete helper exists for them.

use rusqlite::{params, Transaction};
use velnor_model::{
    transition_target, EventReason, ExitClass, InfrastructureCategory, InvalidJobSummaryField,
    JobConclusion, JobPhase, JobState, JobSummary as ModelJobSummary, NormalizedJob, RepositoryRef,
    Slug, Timestamp, TriggerEvent,
};

use super::error::{StoreError, StoreResult};
use super::rfc3339;
use super::Store;

/// Current instance process state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRow {
    pub instance_slug: String,
    pub host: String,
    pub daemon_version: String,
    pub slots_configured: u32,
    pub slots_busy: u32,
    pub updated_at: Timestamp,
}

/// One execution slot owned by an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRow {
    pub instance_slug: String,
    pub name: String,
    pub host: String,
    pub slot_index: u32,
    /// Stable-slot versus ephemeral-runner class (`SlotKind` spelling).
    pub slot_kind: String,
    pub phase: String,
    pub job_name: Option<String>,
    pub updated_at: Timestamp,
}

/// One runner registration GitHub reports; never a credential field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRegistrationRow {
    pub instance_slug: String,
    pub runner_id: i64,
    pub name: String,
    pub ephemeral: bool,
    pub online: bool,
    /// JSON object of sanitized labels.
    pub labels_json: String,
    pub registered_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Sanitized job summary; derived only from already-normalized fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub instance_slug: String,
    /// Daemon-canonical unique job identity within the instance.
    pub job_uid: String,
    /// `owner/name`.
    pub repository: String,
    pub workflow: String,
    pub job_name: String,
    pub run_id: Option<i64>,
    pub attempt: Option<i64>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub trigger_event: Option<String>,
    pub queued_at: Option<Timestamp>,
    pub acquired_at: Option<Timestamp>,
    pub runner_name: Option<String>,
    pub trust_scope: Option<String>,
    pub resource_policy: Option<String>,
    pub phase: String,
    pub conclusion: Option<String>,
    pub infrastructure_category: Option<String>,
    pub updated_at: Timestamp,
}

/// One idempotent, machine-validated state transition for a stored job.
///
/// The target phase is never supplied by the caller: [`Store::record_job_transition`]
/// derives it from the enforced Plan 066 transition table
/// (`queued → acquired → waiting → started → terminal`), so an impossible
/// edge cannot even be expressed, let alone persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// Unique retry token; replaying a `(instance, job, token)` triple is a
    /// no-op instead of a duplicate.
    pub token: String,
    /// Validated non-empty correlation slug carried on the row and its event.
    pub correlation_id: Slug,
    /// Retained taxonomy reason driving this transition.
    pub reason: EventReason,
    pub message: Option<String>,
    pub transition_time: Timestamp,
    /// Terminal payload data; ignored by the state machine itself.
    pub conclusion: Option<String>,
    pub infrastructure_category: Option<String>,
}

/// Append-only normalized event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub instance_slug: String,
    pub event_kind: String,
    pub subject: String,
    pub correlation_id: Option<String>,
    pub occurred_at: Timestamp,
    pub detail: Option<String>,
}

/// One reconciliation pass record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationRow {
    pub id: i64,
    pub instance_slug: String,
    pub kind: String,
    pub subject: String,
    pub status: String,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub detail: Option<String>,
}

/// Read-back projection of one stored job summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSummary {
    pub instance_slug: String,
    pub job_uid: String,
    pub repository: String,
    pub workflow: String,
    pub job_name: String,
    pub run_id: Option<i64>,
    pub attempt: Option<i64>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub trigger_event: Option<String>,
    pub queued_at: Option<String>,
    pub acquired_at: Option<String>,
    pub runner_name: Option<String>,
    pub trust_scope: Option<String>,
    pub resource_policy: Option<String>,
    pub phase: String,
    pub conclusion: Option<String>,
    pub infrastructure_category: Option<String>,
}

impl Store {
    /// Insert or refresh current instance state.
    ///
    /// # Errors
    /// Envelope-classified persistence failures.
    pub fn upsert_instance(&self, row: &InstanceRow) -> StoreResult<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO instances (instance_slug, host, daemon_version, slots_configured, slots_busy, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (instance_slug) DO UPDATE SET
                host = excluded.host,
                daemon_version = excluded.daemon_version,
                slots_configured = excluded.slots_configured,
                slots_busy = excluded.slots_busy,
                updated_at = excluded.updated_at",
            params![
                row.instance_slug,
                row.host,
                row.daemon_version,
                row.slots_configured,
                row.slots_busy,
                rfc3339(row.updated_at),
            ],
        )?;
        Ok(())
    }

    /// Insert or refresh current slot state.
    ///
    /// # Errors
    /// Envelope-classified persistence failures.
    pub fn upsert_slot(&self, row: &SlotRow) -> StoreResult<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO slots (instance_slug, name, host, slot_index, slot_kind, phase, job_name, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (instance_slug, name) DO UPDATE SET
                host = excluded.host,
                slot_index = excluded.slot_index,
                slot_kind = excluded.slot_kind,
                phase = excluded.phase,
                job_name = excluded.job_name,
                updated_at = excluded.updated_at",
            params![
                row.instance_slug,
                row.name,
                row.host,
                row.slot_index,
                row.slot_kind,
                row.phase,
                row.job_name,
                rfc3339(row.updated_at),
            ],
        )?;
        Ok(())
    }

    /// Insert or refresh one runner registration.
    ///
    /// # Errors
    /// Envelope-classified persistence failures.
    pub fn upsert_runner_registration(&self, row: &RunnerRegistrationRow) -> StoreResult<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO runner_registrations (instance_slug, runner_id, name, ephemeral, online, labels_json, registered_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (instance_slug, runner_id) DO UPDATE SET
                name = excluded.name,
                ephemeral = excluded.ephemeral,
                online = excluded.online,
                labels_json = excluded.labels_json,
                updated_at = excluded.updated_at",
            params![
                row.instance_slug,
                row.runner_id,
                row.name,
                i64::from(row.ephemeral),
                i64::from(row.online),
                row.labels_json,
                rfc3339(row.registered_at),
                rfc3339(row.updated_at),
            ],
        )?;
        Ok(())
    }

    /// Insert or refresh a sanitized job summary keyed by
    /// `(instance_slug, job_uid)`.
    ///
    /// # Errors
    /// Envelope-classified persistence failures.
    pub fn record_job(&self, row: &JobRow) -> StoreResult<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO jobs (instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
                               head_ref, head_sha, trigger_event, queued_at, acquired_at, runner_name,
                               trust_scope, resource_policy, phase, conclusion, infrastructure_category, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
             ON CONFLICT (instance_slug, job_uid) DO UPDATE SET
                repository = excluded.repository,
                workflow = excluded.workflow,
                job_name = excluded.job_name,
                run_id = excluded.run_id,
                attempt = excluded.attempt,
                head_ref = excluded.head_ref,
                head_sha = excluded.head_sha,
                trigger_event = excluded.trigger_event,
                queued_at = COALESCE(jobs.queued_at, excluded.queued_at),
                acquired_at = COALESCE(jobs.acquired_at, excluded.acquired_at),
                runner_name = excluded.runner_name,
                trust_scope = excluded.trust_scope,
                resource_policy = excluded.resource_policy,
                phase = excluded.phase,
                conclusion = excluded.conclusion,
                infrastructure_category = excluded.infrastructure_category,
                updated_at = excluded.updated_at",
            params![
                row.instance_slug,
                row.job_uid,
                row.repository,
                row.workflow,
                row.job_name,
                row.run_id,
                row.attempt,
                row.head_ref,
                row.head_sha,
                row.trigger_event,
                row.queued_at.map(rfc3339),
                row.acquired_at.map(rfc3339),
                row.runner_name,
                row.trust_scope,
                row.resource_policy,
                row.phase,
                row.conclusion,
                row.infrastructure_category,
                rfc3339(row.updated_at),
            ],
        )?;
        Ok(())
    }

    /// Persist one sanitized [`ModelJobSummary`], upserting by
    /// `(instance_slug, run_id, attempt)` so replaying the same identity
    /// refreshes the row instead of duplicating it.
    ///
    /// The upsert deliberately never touches `phase`, `conclusion`, or
    /// `infrastructure_category`: those columns belong exclusively to the
    /// enforced state machine, so a duplicate delivery replaying the
    /// admission summary can never regress a job that already advanced.
    ///
    /// The runner keeps its private in-flight record (`in-flight-job.json`,
    /// which holds the run-service URL and billing owner) until
    /// reconciliation has switched over to this summary path; that file stays
    /// runner-local and neither of those fields exists on the sanitized DTO,
    /// so they can never enter this table.
    ///
    /// # Errors
    /// `store.job.summary.unidentified` when the summary lacks a run ID or
    /// attempt (the idempotency key would be undefined); other persistence
    /// failures are envelope-classified.
    pub fn persist_summary(&self, summary: &ModelJobSummary) -> StoreResult<()> {
        let mut conn = self.lock_conn()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        insert_summary(&transaction, summary)?;
        transaction.commit()?;
        Ok(())
    }

    /// Atomically persist an admission summary and its required acquired
    /// transition. A failure rolls back both writes, so callers never accept
    /// work with a partial operational record.
    pub fn persist_summary_and_transition(
        &self,
        summary: &ModelJobSummary,
        instance_slug: &str,
        job_uid: &str,
        transition: &Transition,
    ) -> StoreResult<()> {
        let mut conn = self.lock_conn()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        insert_summary(&transaction, summary)?;
        record_job_transition_in_transaction(&transaction, instance_slug, job_uid, transition)?;
        transaction.commit()?;
        Ok(())
    }

    /// Fetch one persisted sanitized summary by its identity triple.
    ///
    /// # Errors
    /// Envelope-classified read failures; a stored value that no longer
    /// satisfies the sanitized contract is `store.job.summary.decode`.
    pub fn fetch_summary(
        &self,
        instance_slug: &str,
        run_id: u64,
        attempt: u32,
    ) -> StoreResult<Option<ModelJobSummary>> {
        let run_id = i64::try_from(run_id).map_err(|_| summary_out_of_range("run_id"))?;
        let conn = self.lock_conn()?;
        let mut statement = conn.prepare_cached(
            "SELECT instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
                    head_ref, head_sha, trigger_event, queued_at, acquired_at, runner_name,
                    trust_scope, resource_policy, phase, conclusion, infrastructure_category
             FROM jobs WHERE instance_slug = ?1 AND run_id = ?2 AND attempt = ?3",
        )?;
        let mut rows = statement.query(params![instance_slug, run_id, i64::from(attempt)])?;
        match rows.next()? {
            Some(row) => decode_summary_row(row).map(Some),
            None => Ok(None),
        }
    }

    /// Atomically apply one transition under the enforced job state machine:
    /// update the job's current-state row and append its event in a single
    /// transaction.
    ///
    /// Validation order is deliberate:
    /// 1. the job must exist (`store.job.missing`, `UNAVAILABLE`);
    /// 2. an already-applied token is an idempotent no-op success
    ///    (`Ok(false)`) even past a terminal state — retry-safe;
    /// 3. otherwise `(current phase, reason)` must be a legal edge of the
    ///    Plan 066 table; an impossible transition fails with `CONFLICT`
    ///    naming from/to and writes nothing;
    /// 4. the correlation id must be a validated non-empty slug
    ///    (`store.job.transition.correlation`) or nothing is written.
    ///
    /// Returns `Ok(true)` when applied; `Ok(false)` when the same transition
    /// token was already applied (idempotent replay).
    ///
    /// # Errors
    /// Unknown jobs are `UNAVAILABLE`; impossible transitions are `CONFLICT`
    /// naming the from/to states; invalid correlation ids fail closed; all
    /// leave both rows untouched via rollback.
    pub fn record_job_transition(
        &self,
        instance_slug: &str,
        job_uid: &str,
        transition: &Transition,
    ) -> StoreResult<bool> {
        let mut conn = self.lock_conn()?;
        // Immediate, not deferred: a deferred transaction that reads first
        // and writes later must upgrade its lock mid-flight, and SQLite
        // refuses such upgrades with an immediate SQLITE_BUSY that no busy
        // timeout can absorb when two daemons race the same upgrade. Taking
        // the write intent up front makes the bounded busy timeout govern
        // the whole wait instead.
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let applied =
            record_job_transition_in_transaction(&transaction, instance_slug, job_uid, transition)?;
        transaction.commit()?;
        Ok(applied)
    }

    /// Append one normalized event. Events are never updated or deleted.
    ///
    /// # Errors
    /// Envelope-classified persistence failures.
    pub fn append_event(&self, row: &EventRow) -> StoreResult<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO events (instance_slug, event_kind, subject, correlation_id, occurred_at, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.instance_slug,
                row.event_kind,
                row.subject,
                row.correlation_id,
                rfc3339(row.occurred_at),
                row.detail,
            ],
        )?;
        Ok(())
    }

    /// Record the start of a reconciliation pass and return its row id.
    ///
    /// # Errors
    /// Envelope-classified persistence failures.
    pub fn start_reconciliation(
        &self,
        instance_slug: &str,
        kind: &str,
        subject: &str,
        started_at: Timestamp,
    ) -> StoreResult<i64> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT INTO reconciliations (instance_slug, kind, subject, status, started_at)
             VALUES (?1, ?2, ?3, 'running', ?4)",
            params![instance_slug, kind, subject, rfc3339(started_at)],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Finish a reconciliation pass.
    ///
    /// # Errors
    /// Unknown rows are `UNAVAILABLE`; other failures envelope-classified.
    pub fn finish_reconciliation(
        &self,
        instance_slug: &str,
        reconciliation_id: i64,
        status: &str,
        finished_at: Timestamp,
        detail: Option<&str>,
    ) -> StoreResult<()> {
        let conn = self.lock_conn()?;
        let updated = conn.execute(
            "UPDATE reconciliations SET status = ?1, finished_at = ?2, detail = ?3
             WHERE instance_slug = ?4 AND id = ?5",
            params![
                status,
                rfc3339(finished_at),
                detail,
                instance_slug,
                reconciliation_id
            ],
        )?;
        if updated == 0 {
            return Err(
                StoreError::new(ExitClass::Unavailable, "store.reconciliation.missing")
                    .with_remediation("the reconciliation row was pruned or never started"),
            );
        }
        Ok(())
    }

    /// Stored summaries for one instance, newest first.
    ///
    /// # Errors
    /// Envelope-classified read failures.
    pub fn job_summaries(&self, instance_slug: &str) -> StoreResult<Vec<JobSummary>> {
        let conn = self.lock_conn()?;
        let mut statement = conn.prepare_cached(
            "SELECT instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
                    head_ref, head_sha, trigger_event, queued_at, acquired_at, runner_name,
                    trust_scope, resource_policy, phase, conclusion, infrastructure_category
             FROM jobs WHERE instance_slug = ?1 ORDER BY id DESC",
        )?;
        let rows = statement.query_map([instance_slug], map_summary)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Number of transitions recorded for one job.
    ///
    /// # Errors
    /// Envelope-classified read failures.
    pub fn transition_count(&self, instance_slug: &str, job_uid: &str) -> StoreResult<u32> {
        let conn = self.lock_conn()?;
        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM job_transitions WHERE instance_slug = ?1 AND job_uid = ?2",
            [instance_slug, job_uid],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Number of events appended for one subject.
    ///
    /// # Errors
    /// Envelope-classified read failures.
    pub fn event_count(&self, instance_slug: &str, subject: &str) -> StoreResult<u32> {
        let conn = self.lock_conn()?;
        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE instance_slug = ?1 AND subject = ?2",
            [instance_slug, subject],
            |row| row.get(0),
        )?;
        Ok(count)
    }
}

fn insert_summary(transaction: &Transaction<'_>, summary: &ModelJobSummary) -> StoreResult<()> {
    let Some(run_id) = summary.run_id() else {
        return Err(unidentified_summary());
    };
    let Some(attempt) = summary.attempt() else {
        return Err(unidentified_summary());
    };
    let run_id = i64::try_from(run_id).map_err(|_| summary_out_of_range("run_id"))?;
    let job_uid = format!("summary-run-{run_id}-attempt-{attempt}");
    transaction.execute(
        "INSERT INTO jobs (instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
                           head_ref, head_sha, trigger_event, queued_at, acquired_at, runner_name,
                           trust_scope, resource_policy, phase, conclusion, infrastructure_category, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT (instance_slug, run_id, attempt)
            WHERE run_id IS NOT NULL AND attempt IS NOT NULL
          DO UPDATE SET
            job_uid = excluded.job_uid,
            repository = excluded.repository,
            workflow = excluded.workflow,
            job_name = excluded.job_name,
            head_ref = excluded.head_ref,
            head_sha = excluded.head_sha,
            trigger_event = excluded.trigger_event,
            queued_at = COALESCE(jobs.queued_at, excluded.queued_at),
            acquired_at = COALESCE(jobs.acquired_at, excluded.acquired_at),
            runner_name = excluded.runner_name,
            trust_scope = excluded.trust_scope,
            resource_policy = excluded.resource_policy,
            updated_at = excluded.updated_at",
        params![
            summary.instance_slug(),
            job_uid,
            summary.repository().full_name(),
            summary.workflow(),
            summary.job_name(),
            run_id,
            i64::from(attempt),
            summary.head_ref(),
            summary.head_sha(),
            summary.trigger_event().map(TriggerEvent::as_str),
            summary.queued_at().map(rfc3339),
            summary.acquired_at().map(rfc3339),
            summary.runner_name(),
            summary.trust_scope(),
            summary.resource_policy(),
            summary.phase().as_str(),
            summary.conclusion().map(JobConclusion::as_str),
            summary
                .infrastructure_category()
                .map(InfrastructureCategory::as_str),
            rfc3339(Timestamp::now()),
        ],
    )?;
    Ok(())
}

fn record_job_transition_in_transaction(
    transaction: &Transaction<'_>,
    instance_slug: &str,
    job_uid: &str,
    transition: &Transition,
) -> StoreResult<bool> {
    let current_phase: String = transaction
        .query_row(
            "SELECT phase FROM jobs WHERE instance_slug = ?1 AND job_uid = ?2",
            params![instance_slug, job_uid],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => {
                StoreError::new(ExitClass::Unavailable, "store.job.missing")
                    .with_remediation(format!("job {job_uid} is not recorded for this instance"))
            }
            other => StoreError::from(other),
        })?;
    let inserted = transaction.execute(
        "INSERT OR IGNORE INTO job_transitions (instance_slug, job_uid, transition_token,
                                                correlation_id, reason, message, transition_time)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            instance_slug,
            job_uid,
            transition.token,
            transition.correlation_id.as_str(),
            transition.reason.as_str(),
            transition.message,
            rfc3339(transition.transition_time),
        ],
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    let from = JobState::try_from(current_phase.as_str()).map_err(|_| {
        StoreError::new(ExitClass::Operation, "store.job.state.unknown")
            .with_remediation("stored phase is not part of the closed job state taxonomy")
    })?;
    let Some(target) = transition_target(from, transition.reason) else {
        return Err(illegal_transition_error(from, transition.reason));
    };
    let updated = transaction.execute(
        "UPDATE jobs SET phase = ?1, conclusion = ?2, infrastructure_category = ?3, updated_at = ?4
         WHERE instance_slug = ?5 AND job_uid = ?6",
        params![
            target.as_str(),
            transition.conclusion,
            transition.infrastructure_category,
            rfc3339(transition.transition_time),
            instance_slug,
            job_uid,
        ],
    )?;
    if updated == 0 {
        return Err(StoreError::new(ExitClass::Unavailable, "store.job.missing")
            .with_remediation(format!("job {job_uid} is not recorded for this instance")));
    }
    transaction.execute(
        "INSERT INTO events (instance_slug, event_kind, subject, correlation_id, occurred_at, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            instance_slug,
            format!("job.transition.{}", transition.reason.as_str()),
            job_uid,
            transition.correlation_id.as_str(),
            rfc3339(transition.transition_time),
            transition.message,
        ],
    )?;
    Ok(true)
}

fn map_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobSummary> {
    Ok(JobSummary {
        instance_slug: row.get(0)?,
        job_uid: row.get(1)?,
        repository: row.get(2)?,
        workflow: row.get(3)?,
        job_name: row.get(4)?,
        run_id: row.get(5)?,
        attempt: row.get(6)?,
        head_ref: row.get(7)?,
        head_sha: row.get(8)?,
        trigger_event: row.get(9)?,
        queued_at: row.get(10)?,
        acquired_at: row.get(11)?,
        runner_name: row.get(12)?,
        trust_scope: row.get(13)?,
        resource_policy: row.get(14)?,
        phase: row.get(15)?,
        conclusion: row.get(16)?,
        infrastructure_category: row.get(17)?,
    })
}

/// Re-validate one stored job summary back into the sanitized model DTO.
///
/// Every stored value passes through [`JobSummary::from_normalized`] again,
/// so a hand-edited database cannot smuggle an unvalidated string into the
/// query path. The error remediation names the field, never the value.
fn decode_summary_row(row: &rusqlite::Row<'_>) -> StoreResult<ModelJobSummary> {
    let repository_full: String = row.get(2)?;
    let (owner, name) = repository_full
        .rsplit_once('/')
        .ok_or_else(|| summary_decode("repository"))?;
    let phase_raw: String = row.get(15)?;
    // The column stores two coordinated vocabularies: the summary phase on
    // insert and the store-side machine state after transitions. Map the
    // machine spellings back onto the closed JobPhase taxonomy; anything
    // else fails closed.
    let phase = match JobPhase::try_from(phase_raw.as_str()) {
        Ok(phase) => phase,
        Err(_) => {
            let machine =
                JobState::try_from(phase_raw.as_str()).map_err(|_| summary_decode("phase"))?;
            match machine {
                JobState::Queued => JobPhase::Queued,
                JobState::Acquired | JobState::Waiting | JobState::Started => JobPhase::Running,
                JobState::Completed => JobPhase::Completed,
                JobState::Canceled => JobPhase::Canceled,
                JobState::Rejected => JobPhase::Rejected,
            }
        }
    };
    let run_id: i64 = row.get(5)?;
    let attempt: i64 = row.get(6)?;
    ModelJobSummary::from_normalized(NormalizedJob {
        instance_slug: row.get(0)?,
        job_uid: row.get(1)?,
        repository: RepositoryRef::new(owner, name),
        workflow: row.get(3)?,
        job_name: row.get(4)?,
        run_id: Some(u64::try_from(run_id).map_err(|_| summary_decode("run_id"))?),
        attempt: Some(u32::try_from(attempt).map_err(|_| summary_decode("attempt"))?),
        head_ref: row.get(7)?,
        head_sha: row.get(8)?,
        trigger_event: optional_enum("trigger_event", row.get(9)?, |raw: &str| {
            TriggerEvent::try_from(raw)
        })?,
        queued_at: optional_timestamp("queued_at", row.get(10)?)?,
        acquired_at: optional_timestamp("acquired_at", row.get(11)?)?,
        runner_name: row.get(12)?,
        trust_scope: row.get(13)?,
        resource_policy: row.get(14)?,
        phase,
        conclusion: optional_enum("conclusion", row.get(16)?, |raw: &str| {
            JobConclusion::try_from(raw)
        })?,
        infrastructure_category: optional_enum(
            "infrastructure_category",
            row.get(17)?,
            |raw: &str| InfrastructureCategory::try_from(raw),
        )?,
    })
    .map_err(|_| summary_decode("summary"))
}

fn optional_enum<T>(
    field: &'static str,
    raw: Option<String>,
    parse: impl Fn(&str) -> Result<T, InvalidJobSummaryField>,
) -> StoreResult<Option<T>> {
    raw.map(|value| parse(&value).map_err(|_| summary_decode(field)))
        .transpose()
}

fn optional_timestamp(field: &'static str, raw: Option<String>) -> StoreResult<Option<Timestamp>> {
    raw.map(|value| Timestamp::parse(&value).map_err(|_| summary_decode(field)))
        .transpose()
}

fn unidentified_summary() -> StoreError {
    StoreError::new(ExitClass::Operation, "store.job.summary.unidentified").with_remediation(
        "persist only summaries carrying both run_id and attempt so the upsert key exists",
    )
}

/// Impossible transition under the Plan 066 table: names from/to and the
/// rejected reason without ambiguity.
fn illegal_transition_error(from: JobState, reason: EventReason) -> StoreError {
    let to = reason
        .job_target()
        .map_or_else(|| "<n/a>".to_owned(), |target| target.as_str().to_owned());
    let why = if reason.is_job_transition() {
        format!(
            "job state '{}' has no edge to '{}' via reason '{}'",
            from.as_str(),
            to,
            reason.as_str()
        )
    } else {
        format!("reason '{}' is not a job transition", reason.as_str())
    };
    StoreError::new(ExitClass::Conflict, "store.job.transition.illegal").with_remediation(format!(
        "{why}; legal path is queued→acquired→waiting→started→terminal"
    ))
}

fn summary_out_of_range(field: &'static str) -> StoreError {
    StoreError::new(ExitClass::Operation, "store.job.summary.range")
        .with_remediation(format!("{field} exceeds the database integer range"))
}

fn summary_decode(field: &'static str) -> StoreError {
    StoreError::new(ExitClass::Operation, "store.job.summary.decode").with_remediation(format!(
        "stored {field} no longer satisfies the sanitized contract; rewrite the row through persist_summary"
    ))
}

/// Direct connection access used by migration tests only.
#[cfg(test)]
pub(crate) fn test_connection(store: &Store) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
    store.lock_conn().expect("store lock poisoned")
}
