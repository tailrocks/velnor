//! Sanitized row types plus write/read helpers over the operational store.
//!
//! Every key carries `instance_slug`; [`Store::record_job_transition`] is
//! the atomic current-state-plus-event seam. Events are append-only: no
//! update or delete helper exists for them.

use std::time::Duration;

use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use velnor_model::{
    transition_target, EventReason, ExitClass, InfrastructureCategory, InvalidJobSummaryField,
    JobConclusion, JobPhase, JobState, JobSummary as ModelJobSummary, NormalizedJob, RepositoryRef,
    Slug, Timestamp, TriggerEvent,
};

use super::error::{StoreError, StoreResult};
use super::retention::{
    storage_reservation_bytes_with_connection, PhysicalBudgetStatus, RetentionMaintenanceBudget,
    JOB_STORAGE_RESERVATION_BYTES,
};
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
    pub slot_name: Option<String>,
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

/// One normalized event row with its opaque host-local database id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub id: u64,
    pub row: EventRow,
}

/// One atomically read event window and its cursor validity watermarks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventWindow {
    pub first_retained_id: Option<u64>,
    pub high_water_id: Option<u64>,
    pub events: Vec<StoredEvent>,
}

/// Maximum number of unique lifecycle idempotency records retained per
/// instance. The quota is checked in the same SQLite transaction as a fresh
/// insert; replays are looked up first and remain valid after the cap is hit.
pub(crate) const MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE: usize = 4_096;
const LIFECYCLE_OPERATION_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

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

/// Durable instance desired/observed lifecycle projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleInstanceRow {
    pub instance_slug: String,
    pub desired_state: String,
    pub observed_state: String,
    pub resource_version: u64,
    pub desired_slots: Option<u32>,
}

/// Durable accepted lifecycle intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleOperationRow {
    pub instance_slug: String,
    pub idempotency_key: String,
    pub operation_id: String,
    pub kind: String,
    pub target: String,
    pub reason: String,
    pub desired_state: String,
    pub desired_slots: Option<u32>,
    pub resource_version: u64,
    pub phase: String,
    pub created_at: Timestamp,
}

/// Input for one atomic lifecycle intent write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleOperationRequest {
    pub instance_slug: String,
    pub kind: String,
    pub target: String,
    pub reason: String,
    pub idempotency_key: String,
    pub desired_state: String,
    pub desired_slots: Option<u32>,
    pub expected_version: Option<u64>,
    pub operation_id: String,
    pub created_at: Timestamp,
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
    pub slot_name: Option<String>,
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

    /// Read the durable lifecycle projection for one instance.
    pub fn lifecycle_instance(
        &self,
        instance_slug: &str,
    ) -> StoreResult<Option<LifecycleInstanceRow>> {
        let conn = self.lock_conn()?;
        conn.query_row(
            "SELECT instance_slug, desired_state, observed_state, resource_version, desired_slots
             FROM instances WHERE instance_slug = ?1",
            [instance_slug],
            |row| {
                let version = row.get::<_, i64>(3)?;
                Ok(LifecycleInstanceRow {
                    instance_slug: row.get(0)?,
                    desired_state: row.get(1)?,
                    observed_state: row.get(2)?,
                    resource_version: version
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, version))?,
                    desired_slots: row
                        .get::<_, Option<i64>>(4)?
                        .map(|slots| slots.try_into())
                        .transpose()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Atomically persist lifecycle intent, desired state, and idempotency.
    ///
    /// Returns the prior operation with `false` for a replay, or the newly
    /// committed operation with `true` for a fresh request.
    pub fn record_lifecycle_operation(
        &self,
        request: &LifecycleOperationRequest,
    ) -> StoreResult<(LifecycleOperationRow, bool)> {
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = lifecycle_operation_query(
            &transaction,
            &request.instance_slug,
            &request.idempotency_key,
        )? {
            if existing.kind != request.kind
                || existing.target != request.target
                || existing.reason != request.reason
                || existing.desired_state != request.desired_state
                || existing.desired_slots != request.desired_slots
            {
                return Err(StoreError::new(
                    ExitClass::Conflict,
                    "store.lifecycle.idempotency_conflict",
                ));
            }
            transaction.commit()?;
            return Ok((existing, false));
        }
        let mut operation_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM lifecycle_operations WHERE instance_slug = ?1",
            [&request.instance_slug],
            |row| row.get(0),
        )?;
        if operation_count >= MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE as i64 {
            // Nonterminal idempotency records remain durable until resolution.
            // Reclaim only terminal records older than the replay window, and
            // only for this instance, before enforcing the hard quota.
            let cutoff = rfc3339(Timestamp::now().minus(LIFECYCLE_OPERATION_RETENTION));
            transaction.execute(
                "DELETE FROM lifecycle_operations
                 WHERE instance_slug = ?1
                   AND created_at < ?2
                   AND phase COLLATE NOCASE IN ('completed', 'canceled', 'cancelled', 'rejected')",
                params![request.instance_slug, cutoff],
            )?;
            operation_count = transaction.query_row(
                "SELECT COUNT(*) FROM lifecycle_operations WHERE instance_slug = ?1",
                [&request.instance_slug],
                |row| row.get(0),
            )?;
            if operation_count >= MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE as i64 {
                return Err(
                    StoreError::new(ExitClass::Unavailable, "store.lifecycle.operation_quota")
                        .with_remediation(
                            "wait for pending operations to become terminal or for the terminal replay window to expire before submitting a new operation",
                        ),
                );
            }
        }
        let accepted_at = Timestamp::now();
        let current_version = transaction
            .query_row(
                "SELECT resource_version FROM instances WHERE instance_slug = ?1",
                [&request.instance_slug],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map_or(1_u64, |version| version.try_into().unwrap_or(0));
        if request
            .expected_version
            .is_some_and(|version| version != current_version)
        {
            return Err(StoreError::new(
                ExitClass::Conflict,
                "store.lifecycle.version_conflict",
            ));
        }
        let next_version = current_version.saturating_add(1);
        transaction.execute(
            "INSERT INTO instances (instance_slug, host, daemon_version, slots_configured, slots_busy,
                                    updated_at, desired_state, observed_state, resource_version, desired_slots)
             VALUES (?1, 'unknown', '', 0, 0, ?2, ?3, 'ready', ?4, ?5)
             ON CONFLICT(instance_slug) DO UPDATE SET
                 desired_state = excluded.desired_state,
                 resource_version = excluded.resource_version,
                 desired_slots = COALESCE(excluded.desired_slots, instances.desired_slots),
                 updated_at = excluded.updated_at",
            params![
                request.instance_slug,
                rfc3339(accepted_at),
                request.desired_state,
                next_version as i64,
                request.desired_slots.map(i64::from),
            ],
        )?;
        transaction.execute(
            "INSERT INTO lifecycle_operations
             (instance_slug, idempotency_key, operation_id, kind, target, reason,
              desired_state, desired_slots, resource_version, phase, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'accepted', ?10)",
            params![
                request.instance_slug,
                request.idempotency_key,
                request.operation_id,
                request.kind,
                request.target,
                request.reason,
                request.desired_state,
                request.desired_slots.map(i64::from),
                next_version as i64,
                rfc3339(accepted_at),
            ],
        )?;
        transaction.commit()?;
        Ok((
            LifecycleOperationRow {
                instance_slug: request.instance_slug.clone(),
                idempotency_key: request.idempotency_key.clone(),
                operation_id: request.operation_id.clone(),
                kind: request.kind.clone(),
                target: request.target.clone(),
                reason: request.reason.clone(),
                desired_state: request.desired_state.clone(),
                desired_slots: request.desired_slots,
                resource_version: next_version,
                phase: "accepted".to_owned(),
                created_at: accepted_at,
            },
            true,
        ))
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
        validate_job_row(row)?;
        let mut conn = self.lock_conn()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO jobs (instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
                               head_ref, head_sha, trigger_event, queued_at, acquired_at, slot_name, runner_name,
                               trust_scope, resource_policy, phase, conclusion, infrastructure_category, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
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
                slot_name = COALESCE(excluded.slot_name, jobs.slot_name),
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
                row.slot_name,
                row.runner_name,
                row.trust_scope,
                row.resource_policy,
                row.phase,
                row.conclusion,
                row.infrastructure_category,
                rfc3339(row.updated_at),
            ],
        )?;
        if !matches!(row.phase.as_str(), "completed" | "canceled" | "rejected") {
            ensure_job_storage_reservation(&transaction, &row.instance_slug, &row.job_uid)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Release one durable job claim after an abandoned in-flight job has
    /// been completed remotely. The identity-qualified delete is idempotent
    /// and deliberately bypasses the admission budget so cleanup can finish
    /// under disk pressure.
    pub fn release_job_storage_reservation(
        &self,
        instance_slug: &str,
        job_uid: &str,
    ) -> StoreResult<bool> {
        Slug::validate("instance_slug", instance_slug).map_err(|_| {
            StoreError::new(ExitClass::Conflict, "store.admission.reservation.identity")
                .with_remediation("release requires the daemon instance and job identity")
        })?;
        Slug::validate("job_uid", job_uid).map_err(|_| {
            StoreError::new(ExitClass::Conflict, "store.admission.reservation.identity")
                .with_remediation("release requires the daemon instance and job identity")
        })?;
        let mut conn = self.lock_conn()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let deleted = transaction.execute(
            "DELETE FROM job_storage_reservations
             WHERE instance_slug = ?1 AND job_uid = ?2",
            params![instance_slug, job_uid],
        )?;
        transaction.commit()?;
        Ok(deleted == 1)
    }

    /// Persist one sanitized [`ModelJobSummary`], upserting by its normalized
    /// `(instance_slug, job_uid)` identity.
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
        if summary.instance_slug() != instance_slug || summary.job_uid() != job_uid {
            return Err(
                StoreError::new(ExitClass::Conflict, "store.job.identity.mismatch")
                    .with_remediation(
                        "use one normalized instance and job identity for both records",
                    ),
            );
        }
        let mut conn = self.lock_conn()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if transition_token_exists(&transaction, instance_slug, job_uid, &transition.token)? {
            transaction.commit()?;
            return Ok(());
        }
        insert_summary(&transaction, summary)?;
        ensure_job_storage_reservation(&transaction, instance_slug, job_uid)?;
        record_job_transition_in_transaction(&transaction, instance_slug, job_uid, transition)?;
        transaction.commit()?;
        Ok(())
    }

    /// Atomically admit a new job only while the measured physical store is
    /// within its configured budget. The immediate transaction is opened
    /// before measurement, so competing SQLite writers cannot grow the store
    /// between the capacity check and this admission commit. A denied or
    /// unmeasurable status rolls the transaction back without row writes.
    pub fn persist_summary_and_transition_with_budget(
        &self,
        summary: &ModelJobSummary,
        instance_slug: &str,
        job_uid: &str,
        transition: &Transition,
        budget: &RetentionMaintenanceBudget,
    ) -> StoreResult<PhysicalBudgetStatus> {
        if summary.instance_slug() != instance_slug || summary.job_uid() != job_uid {
            return Err(
                StoreError::new(ExitClass::Conflict, "store.job.identity.mismatch")
                    .with_remediation(
                        "use one normalized instance and job identity for both records",
                    ),
            );
        }
        let mut conn = self.lock_conn()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let replay =
            transition_token_exists(&transaction, instance_slug, job_uid, &transition.token)?;
        let additional_reservation_bytes = if replay {
            0
        } else {
            admission_reservation_bytes(&transaction, instance_slug, job_uid)?
        };
        let status = self.physical_budget_status_with_connection_and_reservation(
            &transaction,
            budget,
            additional_reservation_bytes,
        )?;
        if !status.admits_job() {
            return Ok(status);
        }
        if !replay {
            insert_summary(&transaction, summary)?;
        }
        if additional_reservation_bytes != 0 {
            insert_job_storage_reservation(&transaction, instance_slug, job_uid)?;
        }
        record_job_transition_in_transaction(&transaction, instance_slug, job_uid, transition)?;
        transaction.commit()?;
        Ok(status)
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
                    head_ref, head_sha, trigger_event, queued_at, acquired_at, slot_name, runner_name,
                    trust_scope, resource_policy, phase, conclusion, infrastructure_category
             FROM jobs WHERE instance_slug = ?1 AND run_id = ?2 AND attempt = ?3",
        )?;
        let mut rows = statement.query(params![instance_slug, run_id, i64::from(attempt)])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let summary = decode_summary_row(row)?;
        if rows.next()?.is_some() {
            return Err(
                StoreError::new(ExitClass::Conflict, "store.job.summary.ambiguous")
                    .with_remediation("query the summary by its job identity"),
            );
        }
        Ok(Some(summary))
    }

    /// Fetch one persisted sanitized summary by its exact job identity.
    ///
    /// # Errors
    /// Envelope-classified read failures; a stored value that no longer
    /// satisfies the sanitized contract is `store.job.summary.decode`.
    pub fn fetch_summary_by_job_uid(
        &self,
        instance_slug: &str,
        job_uid: &str,
    ) -> StoreResult<Option<ModelJobSummary>> {
        let conn = self.lock_conn()?;
        let mut statement = conn.prepare_cached(
            "SELECT instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
                    head_ref, head_sha, trigger_event, queued_at, acquired_at, slot_name, runner_name,
                    trust_scope, resource_policy, phase, conclusion, infrastructure_category
             FROM jobs WHERE instance_slug = ?1 AND job_uid = ?2",
        )?;
        let mut rows = statement.query(params![instance_slug, job_uid])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(decode_summary_row(row)?))
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
    pub fn append_event(&self, row: &EventRow) -> StoreResult<u64> {
        let mut conn = self.lock_conn()?;
        let transaction =
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let id = Self::append_event_in_transaction(&transaction, row)?;
        transaction.commit()?;
        Ok(id)
    }

    /// Insert one validated event into an already-open transaction.
    fn append_event_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        row: &EventRow,
    ) -> StoreResult<u64> {
        Self::append_event_with_ancestry_in_transaction(transaction, row, None, None)
    }

    /// Insert an event with durable lifecycle ownership. Generic callers keep
    /// both ownership columns NULL; state-machine events pass the exact
    /// transition row ID and are the only events retention may remove with a
    /// deleted job.
    fn append_event_with_ancestry_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        row: &EventRow,
        transition_id: Option<i64>,
        reconciliation_id: Option<i64>,
    ) -> StoreResult<u64> {
        validate_event_row(row)?;
        validate_event_ancestry_in_transaction(
            transaction,
            row.instance_slug.as_str(),
            row.subject.as_str(),
            transition_id,
            reconciliation_id,
        )?;
        transaction.execute(
            "INSERT INTO events
                (instance_slug, event_kind, subject, correlation_id, occurred_at, detail,
                 transition_id, reconciliation_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row.instance_slug,
                row.event_kind,
                row.subject,
                row.correlation_id,
                rfc3339(row.occurred_at),
                row.detail,
                transition_id,
                reconciliation_id,
            ],
        )?;
        let id = u64::try_from(transaction.last_insert_rowid())
            .map_err(|_| StoreError::new(ExitClass::Operation, "store.event.id.range"))?;
        let id_i64 = i64::try_from(id)
            .map_err(|_| StoreError::new(ExitClass::Operation, "store.event.id.range"))?;
        transaction.execute(
            "INSERT INTO event_stream_state
                (instance_slug, first_retained_id, high_water_id, updated_at)
             VALUES (?1, ?2, ?2, ?3)
             ON CONFLICT (instance_slug) DO UPDATE SET
                first_retained_id = CASE
                    WHEN event_stream_state.first_retained_id > event_stream_state.high_water_id
                    THEN excluded.first_retained_id
                    ELSE event_stream_state.first_retained_id
                END,
                high_water_id = excluded.high_water_id,
                updated_at = excluded.updated_at",
            params![row.instance_slug, id_i64, rfc3339(Timestamp::now())],
        )?;
        Ok(id)
    }

    /// Read a bounded instance-scoped event window and its cursor watermarks
    /// under one SQLite read transaction.
    pub fn event_window(
        &self,
        instance_slug: &str,
        after_id: u64,
        resource_kind: Option<&str>,
        limit: u32,
    ) -> StoreResult<EventWindow> {
        if limit == 0 || limit > 4_096 {
            return Err(StoreError::new(ExitClass::Usage, "store.event.limit"));
        }
        let after_id = i64::try_from(after_id)
            .map_err(|_| StoreError::new(ExitClass::Usage, "store.event.cursor"))?;
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        let (first, high): (i64, i64) = transaction
            .query_row(
                "SELECT first_retained_id, high_water_id
                 FROM event_stream_state WHERE instance_slug = ?1",
                [instance_slug],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or((1, 0));
        let first_retained_id = u64::try_from(first)
            .map_err(|_| StoreError::new(ExitClass::Operation, "store.event.id.range"))?;
        let high_water_id = u64::try_from(high)
            .map_err(|_| StoreError::new(ExitClass::Operation, "store.event.id.range"))?;
        let mut events = Vec::new();
        if let Some(kind) = resource_kind {
            // Query the two indexed predicates independently. SQLite cannot
            // use both `(instance, kind, id)` and `(instance, subject, id)`
            // efficiently for one OR expression. `limit` rows from each
            // predicate are sufficient: any omitted row has at least `limit`
            // rows from the same predicate ahead of it.
            let mut kind_statement = transaction.prepare(
                "SELECT id, event_kind, subject, correlation_id, occurred_at, detail
                 FROM events
                 WHERE instance_slug = ?1 AND event_kind = ?3 COLLATE NOCASE AND id > ?2
                 ORDER BY id ASC LIMIT ?4",
            )?;
            let mut kind_rows =
                kind_statement.query(params![instance_slug, after_id, kind, limit])?;
            while let Some(row) = kind_rows.next()? {
                events.push(decode_stored_event(row, instance_slug)?);
            }
            drop(kind_rows);
            drop(kind_statement);

            let mut subject_statement = transaction.prepare(
                "SELECT id, event_kind, subject, correlation_id, occurred_at, detail
                 FROM events
                 WHERE instance_slug = ?1 AND subject = ?3 COLLATE NOCASE AND id > ?2
                 ORDER BY id ASC LIMIT ?4",
            )?;
            let mut subject_rows =
                subject_statement.query(params![instance_slug, after_id, kind, limit])?;
            while let Some(row) = subject_rows.next()? {
                events.push(decode_stored_event(row, instance_slug)?);
            }
            drop(subject_rows);
            drop(subject_statement);
            events.sort_unstable_by_key(|event| event.id);
            events.dedup_by_key(|event| event.id);
            events.truncate(limit as usize);
        } else {
            let mut statement = transaction.prepare(
                "SELECT id, event_kind, subject, correlation_id, occurred_at, detail
                 FROM events
                 WHERE instance_slug = ?1 AND id > ?2
                 ORDER BY id ASC LIMIT ?3",
            )?;
            let mut rows = statement.query(params![instance_slug, after_id, limit])?;
            while let Some(row) = rows.next()? {
                events.push(decode_stored_event(row, instance_slug)?);
            }
            drop(rows);
            drop(statement);
        }
        transaction.commit()?;
        Ok(EventWindow {
            first_retained_id: (high_water_id > 0).then_some(first_retained_id),
            high_water_id: (high_water_id > 0).then_some(high_water_id),
            events,
        })
    }

    /// Read a bounded instance-scoped event window by its opaque row id.
    pub fn events_after(
        &self,
        instance_slug: &str,
        after_id: u64,
        limit: u32,
    ) -> StoreResult<Vec<StoredEvent>> {
        Ok(self
            .event_window(instance_slug, after_id, None, limit)?
            .events)
    }

    /// Return the retained and high-water id bounds for one instance.
    pub fn event_bounds(&self, instance_slug: &str) -> StoreResult<(Option<u64>, Option<u64>)> {
        let mut conn = self.lock_conn()?;
        let transaction = conn.transaction()?;
        let bounds: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT first_retained_id, high_water_id
                 FROM event_stream_state WHERE instance_slug = ?1",
                [instance_slug],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        transaction.commit()?;
        bounds
            .map(|(first, high)| {
                Ok((
                    Some(u64::try_from(first).map_err(|_| {
                        StoreError::new(ExitClass::Operation, "store.event.id.range")
                    })?),
                    Some(u64::try_from(high).map_err(|_| {
                        StoreError::new(ExitClass::Operation, "store.event.id.range")
                    })?),
                ))
            })
            .unwrap_or(Ok((None, None)))
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
                    head_ref, head_sha, trigger_event, queued_at, acquired_at, slot_name, runner_name,
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

fn validate_event_ancestry_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    instance_slug: &str,
    subject: &str,
    transition_id: Option<i64>,
    reconciliation_id: Option<i64>,
) -> StoreResult<()> {
    if let Some(transition_id) = transition_id {
        let owned: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM job_transitions
                 WHERE id = ?1 AND instance_slug = ?2 AND job_uid = ?3
             )",
            params![transition_id, instance_slug, subject],
            |row| row.get(0),
        )?;
        if !owned {
            return Err(StoreError::new(ExitClass::Conflict, "store.event.ancestry")
                .with_remediation(
                "event transition ownership must match the event instance and subject job exactly",
            ));
        }
    }
    if let Some(reconciliation_id) = reconciliation_id {
        let owned: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM reconciliations
                 WHERE id = ?1 AND instance_slug = ?2
             )",
            params![reconciliation_id, instance_slug],
            |row| row.get(0),
        )?;
        if !owned {
            return Err(StoreError::new(ExitClass::Conflict, "store.event.ancestry")
                .with_remediation(
                    "event reconciliation ownership must match the event instance exactly",
                ));
        }
    }
    Ok(())
}

pub(crate) const MAX_EVENT_TEXT_BYTES: usize = 512;
pub(crate) const MAX_EVENT_DETAIL_BYTES: usize = 4 * 1024;
const EVENT_SECRET_KEYS: &[&str] = &[
    "authorization",
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "access_token",
    "client_secret",
    "credential",
    "credentials",
    "cookie",
    "private_key",
    "session",
];
const EVENT_SECRET_MARKERS: &[&str] = &[
    "bearer",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "gh_api_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "npm_",
    "sk-",
    "akia",
    "asia",
    "begin private key",
    "begin-private-key",
];

/// Validate the shared event contract before either memory or durable writes.
///
/// Identity fields use the model's closed slug contract. Details are bounded,
/// control-free, and reject credential-shaped content except for an exact
/// `[REDACTED]` value. Keeping this seam shared prevents the in-memory stream
/// from accepting data that the durable store would later reject.
pub(crate) fn validate_event_contract(
    instance_slug: &str,
    event_kind: &str,
    subject: &str,
    correlation_id: Option<&str>,
    detail: Option<&str>,
) -> StoreResult<()> {
    let required = [
        (instance_slug, "instance_slug"),
        (event_kind, "event_kind"),
        (subject, "subject"),
    ];
    if required
        .iter()
        .any(|(value, _)| value.trim().is_empty() || value.len() > MAX_EVENT_TEXT_BYTES)
        || correlation_id
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_EVENT_TEXT_BYTES)
        || detail.is_some_and(|value| {
            value.len() > MAX_EVENT_DETAIL_BYTES || value.chars().any(char::is_control)
        })
    {
        return Err(StoreError::new(ExitClass::Usage, "store.event.invalid"));
    }
    for (value, field) in required {
        if Slug::validate(field, value).is_err() {
            return Err(StoreError::new(ExitClass::Usage, "store.event.identity"));
        }
    }
    if let Some(value) = correlation_id {
        if Slug::validate("correlation_id", value).is_err() {
            return Err(StoreError::new(ExitClass::Usage, "store.event.identity"));
        }
    }
    if required
        .iter()
        .map(|(value, _)| *value)
        .chain(correlation_id)
        .chain(detail)
        .any(|value| {
            event_text_has_forbidden_character(value) || event_text_has_secret_marker(value)
        })
    {
        return Err(StoreError::new(
            ExitClass::Operation,
            "store.event.secret_marker",
        ));
    }
    Ok(())
}

fn validate_event_row(row: &EventRow) -> StoreResult<()> {
    validate_event_contract(
        &row.instance_slug,
        &row.event_kind,
        &row.subject,
        row.correlation_id.as_deref(),
        row.detail.as_deref(),
    )
}

fn decode_stored_event(
    row: &rusqlite::Row<'_>,
    instance_slug: &str,
) -> rusqlite::Result<StoredEvent> {
    let id = row.get::<_, i64>(0)?;
    let id = u64::try_from(id).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, id))?;
    let occurred_at_raw = row.get::<_, String>(4)?;
    let occurred_at = Timestamp::parse(&occurred_at_raw)
        .map_err(|_| rusqlite::Error::InvalidParameterName("event occurred_at".to_owned()))?;
    let event = EventRow {
        instance_slug: instance_slug.to_owned(),
        event_kind: row.get(1)?,
        subject: row.get(2)?,
        correlation_id: row.get(3)?,
        occurred_at,
        detail: row.get(5)?,
    };
    validate_event_row(&event).map_err(|_| {
        rusqlite::Error::InvalidParameterName("event violates sanitized contract".to_owned())
    })?;
    Ok(StoredEvent { id, row: event })
}

fn event_text_has_secret_marker(value: &str) -> bool {
    // Callers validate this limit first, but keep this helper safe at every
    // boundary so a future caller cannot make normalization allocate from an
    // unbounded diagnostic string.
    if value.len() > MAX_EVENT_DETAIL_BYTES {
        return true;
    }
    let normalized = normalize_escaped_text(value);
    if event_text_has_forbidden_character(&normalized) {
        return true;
    }
    let lowered = normalized.to_ascii_lowercase();
    if EVENT_SECRET_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        return true;
    }
    if EVENT_SECRET_KEYS
        .iter()
        .any(|key| secret_key_has_value(&lowered, key))
    {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(&normalized)
        .is_ok_and(|json| json_contains_secret(&json))
}

fn event_text_has_forbidden_character(value: &str) -> bool {
    value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{180e}'
                    | '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}'
                    | '\u{feff}'
                    | '\u{fff9}'..='\u{fffb}'
                    | '\u{e0001}'
                    | '\u{e0020}'..='\u{e007f}'
            )
    })
}

fn normalize_escaped_text(value: &str) -> String {
    let mut normalized = value.to_owned();
    // Each successful pass consumes at least one escape sequence, so this
    // reaches a fixed point without imposing a depth that an attacker can
    // exceed with repeatedly escaped input.
    loop {
        let next = decode_escaped_text_once(&normalized);
        if next == normalized {
            break;
        }
        normalized = next;
    }
    normalized
}

fn decode_escaped_text_once(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut normalized = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if let Some((decoded, consumed)) = decode_escape(bytes, index) {
                normalized.push(decoded);
                index += consumed;
                continue;
            }
        }
        if let Some(character) = value[index..].chars().next() {
            normalized.push(character);
            index += character.len_utf8();
        } else {
            break;
        }
    }
    normalized
}

fn decode_escape(bytes: &[u8], index: usize) -> Option<(char, usize)> {
    let marker = *bytes.get(index + 1)?;
    if marker == b'\\' {
        return Some(('\\', 2));
    }
    let (digits_start, digits) = match marker {
        b'x' => (index + 2, 2),
        b'u' => {
            if bytes.get(index + 2) == Some(&b'{') {
                let end = bytes[index + 3..].iter().position(|byte| *byte == b'}')? + index + 3;
                let digits = end.checked_sub(index + 3)?;
                if !(1..=6).contains(&digits) {
                    return None;
                }
                let codepoint = parse_hex(&bytes[index + 3..end])?;
                return char::from_u32(codepoint).map(|character| (character, end + 1 - index));
            }
            (index + 2, 4)
        }
        b'U' => (index + 2, 8),
        _ => return None,
    };
    let end = digits_start.checked_add(digits)?;
    let codepoint = parse_hex(bytes.get(digits_start..end)?)?;
    char::from_u32(codepoint).map(|character| (character, end - index))
}

fn parse_hex(bytes: &[u8]) -> Option<u32> {
    bytes.iter().try_fold(0_u32, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u32::from(byte - b'0'),
            b'a'..=b'f' => u32::from(byte - b'a' + 10),
            b'A'..=b'F' => u32::from(byte - b'A' + 10),
            _ => return None,
        };
        value.checked_mul(16)?.checked_add(digit)
    })
}

fn json_contains_secret(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(fields) => fields.iter().any(|(key, value)| {
            let sensitive = EVENT_SECRET_KEYS
                .iter()
                .any(|candidate| secret_keys_match(key, candidate));
            (sensitive && !json_value_is_exact_redaction(value)) || json_contains_secret(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(json_contains_secret),
        serde_json::Value::String(text) => event_text_has_secret_marker(text),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

fn json_value_is_exact_redaction(value: &serde_json::Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.eq_ignore_ascii_case("[REDACTED]"))
}

fn secret_key_has_value(text: &str, key: &str) -> bool {
    let text = normalize_secret_key_separators(text);
    let key = normalize_secret_key_separators(key);
    let mut offset = 0;
    while let Some(index) = text[offset..].find(key.as_str()) {
        let start = offset + index;
        let preceded_by_name_char = text[..start]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'));
        if !preceded_by_name_char {
            let mut suffix = text[start + key.len()..].trim_start();
            if suffix.starts_with('"') {
                suffix = suffix[1..].trim_start();
            }
            if suffix.starts_with('=') || suffix.starts_with(':') {
                suffix = suffix[1..].trim_start();
                if !is_exact_redaction(suffix) {
                    return true;
                }
            }
        }
        offset = start + key.len();
        if offset >= text.len() {
            break;
        }
    }
    false
}

fn secret_keys_match(actual: &str, expected: &str) -> bool {
    actual
        .chars()
        .filter(|character| !matches!(*character, '-' | '_'))
        .map(|character| character.to_ascii_lowercase())
        .eq(expected
            .chars()
            .filter(|character| !matches!(*character, '-' | '_'))
            .map(|character| character.to_ascii_lowercase()))
}

fn normalize_secret_key_separators(value: &str) -> String {
    value
        .chars()
        .filter(|character| !matches!(*character, '-' | '_'))
        .collect()
}

fn is_exact_redaction(value: &str) -> bool {
    let mut suffix = value.trim_start();
    let quote = suffix
        .as_bytes()
        .first()
        .copied()
        .filter(|byte| matches!(byte, b'"' | b'\''));
    if quote.is_some() {
        suffix = &suffix[1..];
    }
    let Some(rest) = suffix.strip_prefix("[redacted]") else {
        return false;
    };
    let mut rest = rest;
    if let Some(quote) = quote {
        if rest.as_bytes().first().copied() != Some(quote) {
            return false;
        }
        rest = &rest[1..];
    }
    let rest = rest.trim_start();
    rest.is_empty()
        || rest
            .as_bytes()
            .first()
            .is_some_and(|byte| matches!(byte, b',' | b'}' | b']'))
}

fn lifecycle_operation_query(
    transaction: &Transaction<'_>,
    instance_slug: &str,
    idempotency_key: &str,
) -> StoreResult<Option<LifecycleOperationRow>> {
    transaction
        .query_row(
            "SELECT instance_slug, idempotency_key, operation_id, kind, target, reason,
                    desired_state, desired_slots, resource_version, phase, created_at
             FROM lifecycle_operations
             WHERE instance_slug = ?1 AND idempotency_key = ?2",
            params![instance_slug, idempotency_key],
            |row| {
                let version = row.get::<_, i64>(8)?;
                let created_at = Timestamp::parse(&row.get::<_, String>(10)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(LifecycleOperationRow {
                    instance_slug: row.get(0)?,
                    idempotency_key: row.get(1)?,
                    operation_id: row.get(2)?,
                    kind: row.get(3)?,
                    target: row.get(4)?,
                    reason: row.get(5)?,
                    desired_state: row.get(6)?,
                    desired_slots: row
                        .get::<_, Option<i64>>(7)?
                        .map(|slots| slots.try_into())
                        .transpose()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    resource_version: version
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, version))?,
                    phase: row.get(9)?,
                    created_at,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn insert_summary(transaction: &Transaction<'_>, summary: &ModelJobSummary) -> StoreResult<()> {
    let Some(run_id) = summary.run_id() else {
        return Err(unidentified_summary());
    };
    let Some(attempt) = summary.attempt() else {
        return Err(unidentified_summary());
    };
    let run_id = i64::try_from(run_id).map_err(|_| summary_out_of_range("run_id"))?;
    let job_uid = summary.job_uid().to_owned();
    transaction.execute(
        "INSERT INTO jobs (instance_slug, job_uid, repository, workflow, job_name, run_id, attempt,
                           head_ref, head_sha, trigger_event, queued_at, acquired_at, slot_name, runner_name,
                           trust_scope, resource_policy, phase, conclusion, infrastructure_category, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT (instance_slug, job_uid)
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
            slot_name = COALESCE(excluded.slot_name, jobs.slot_name),
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
            summary.slot_name(),
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

fn validate_job_row(row: &JobRow) -> StoreResult<()> {
    let invalid = || {
        StoreError::new(ExitClass::Conflict, "store.job.summary.invalid")
            .with_remediation("persist summaries through the validated job-summary constructor")
    };
    for (field, value) in [
        ("instance_slug", row.instance_slug.as_str()),
        ("job_uid", row.job_uid.as_str()),
        ("workflow", row.workflow.as_str()),
        ("job_name", row.job_name.as_str()),
    ] {
        Slug::validate(field, value).map_err(|_| invalid())?;
    }
    let (owner, name) = row.repository.rsplit_once('/').ok_or_else(invalid)?;
    Slug::validate("repository.owner", owner).map_err(|_| invalid())?;
    Slug::validate("repository.name", name).map_err(|_| invalid())?;
    for (field, value) in [
        ("head_ref", row.head_ref.as_deref()),
        ("head_sha", row.head_sha.as_deref()),
        ("runner_name", row.runner_name.as_deref()),
        ("trust_scope", row.trust_scope.as_deref()),
        ("resource_policy", row.resource_policy.as_deref()),
        ("slot_name", row.slot_name.as_deref()),
        ("conclusion", row.conclusion.as_deref()),
        (
            "infrastructure_category",
            row.infrastructure_category.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            Slug::validate(field, value).map_err(|_| invalid())?;
        }
    }
    if let Some(value) = row.trigger_event.as_deref() {
        TriggerEvent::try_from(value).map_err(|_| invalid())?;
    }
    if !matches!(
        row.phase.as_str(),
        "queued" | "acquired" | "waiting" | "started" | "completed" | "canceled" | "rejected"
    ) {
        return Err(invalid());
    }
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
    let transaction_id: i64 = transaction.query_row(
        "SELECT id FROM job_transitions
         WHERE instance_slug = ?1 AND job_uid = ?2 AND transition_token = ?3",
        params![instance_slug, job_uid, transition.token],
        |row| row.get(0),
    )?;
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
    Store::append_event_with_ancestry_in_transaction(
        transaction,
        &EventRow {
            instance_slug: instance_slug.to_owned(),
            event_kind: format!("job.transition.{}", transition.reason.as_str()),
            subject: job_uid.to_owned(),
            correlation_id: Some(transition.correlation_id.as_str().to_owned()),
            occurred_at: transition.transition_time,
            detail: transition.message.clone(),
        },
        Some(transaction_id),
        None,
    )?;
    if target.is_terminal() {
        transaction.execute(
            "DELETE FROM job_storage_reservations
             WHERE instance_slug = ?1 AND job_uid = ?2",
            params![instance_slug, job_uid],
        )?;
    }
    Ok(true)
}

fn transition_token_exists(
    transaction: &Transaction<'_>,
    instance_slug: &str,
    job_uid: &str,
    token: &str,
) -> StoreResult<bool> {
    Ok(transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM job_transitions
             WHERE instance_slug = ?1 AND job_uid = ?2 AND transition_token = ?3
         )",
        params![instance_slug, job_uid, token],
        |row| row.get(0),
    )?)
}

fn admission_reservation_bytes(
    transaction: &Transaction<'_>,
    instance_slug: &str,
    job_uid: &str,
) -> StoreResult<u64> {
    // Validate the complete aggregate before deciding whether this job is a
    // replay. A damaged counter or orphaned row must never open a bypass.
    storage_reservation_bytes_with_connection(transaction)?;
    let existing_job: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM jobs WHERE instance_slug = ?1 AND job_uid = ?2
         )",
        params![instance_slug, job_uid],
        |row| row.get(0),
    )?;
    let existing_reservation: Option<i64> = transaction
        .query_row(
            "SELECT reserved_bytes FROM job_storage_reservations
             WHERE instance_slug = ?1 AND job_uid = ?2",
            params![instance_slug, job_uid],
            |row| row.get(0),
        )
        .optional()?;
    match (existing_job, existing_reservation) {
        (_, Some(_)) => Ok(0),
        (true, None) => Err(StoreError::new(
            ExitClass::Operation,
            "store.admission.reservation.missing",
        )
        .with_remediation(
            "reconcile the admitted job before retrying; its durable storage reservation is missing",
        )),
        (false, None) => Ok(JOB_STORAGE_RESERVATION_BYTES),
    }
}

fn ensure_job_storage_reservation(
    transaction: &Transaction<'_>,
    instance_slug: &str,
    job_uid: &str,
) -> StoreResult<()> {
    let existing: Option<i64> = transaction
        .query_row(
            "SELECT reserved_bytes FROM job_storage_reservations
             WHERE instance_slug = ?1 AND job_uid = ?2",
            params![instance_slug, job_uid],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(());
    }
    insert_job_storage_reservation(transaction, instance_slug, job_uid)
}

fn insert_job_storage_reservation(
    transaction: &Transaction<'_>,
    instance_slug: &str,
    job_uid: &str,
) -> StoreResult<()> {
    transaction.execute(
        "INSERT INTO job_storage_reservations
             (instance_slug, job_uid, reserved_bytes, reserved_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            instance_slug,
            job_uid,
            i64::try_from(JOB_STORAGE_RESERVATION_BYTES).map_err(|_| {
                StoreError::new(ExitClass::Operation, "store.admission.reservation.range")
                    .with_remediation("the fixed storage reservation exceeds SQLite range")
            })?,
            rfc3339(Timestamp::now()),
        ],
    )?;
    Ok(())
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
        slot_name: row.get(12)?,
        runner_name: row.get(13)?,
        trust_scope: row.get(14)?,
        resource_policy: row.get(15)?,
        phase: row.get(16)?,
        conclusion: row.get(17)?,
        infrastructure_category: row.get(18)?,
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
    let phase_raw: String = row.get(16)?;
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
        slot_name: row.get(12)?,
        runner_name: row.get(13)?,
        trust_scope: row.get(14)?,
        resource_policy: row.get(15)?,
        phase,
        conclusion: optional_enum("conclusion", row.get(17)?, |raw: &str| {
            JobConclusion::try_from(raw)
        })?,
        infrastructure_category: optional_enum(
            "infrastructure_category",
            row.get(18)?,
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

#[cfg(test)]
mod event_tests {
    use std::path::PathBuf;

    use super::*;

    struct TempDb {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "velnor-event-store-{}-{}",
                std::process::id(),
                Timestamp::now()
                    .as_offset_datetime()
                    .unix_timestamp_nanos()
                    .unsigned_abs()
            ));
            std::fs::create_dir_all(&dir).expect("temp directory");
            Self {
                path: dir.join("state.db"),
                dir,
            }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn event(instance: &str, subject: &str, kind: &str) -> EventRow {
        EventRow {
            instance_slug: instance.to_owned(),
            event_kind: kind.to_owned(),
            subject: subject.to_owned(),
            correlation_id: None,
            occurred_at: Timestamp::UNIX_EPOCH,
            detail: None,
        }
    }

    #[test]
    fn normalized_events_are_bounded_and_instance_scoped() {
        let temp = TempDb::new();
        let store = Store::open(&temp.path).expect("open store");
        let first = store
            .append_event(&event("a", "job-a", "job.started"))
            .expect("append a");
        let second = store
            .append_event(&event("b", "job-b", "job.started"))
            .expect("append b");
        store
            .append_event(&event("a", "job-a", "job.completed"))
            .expect("append a completion");

        assert_eq!(
            store.event_bounds("a").expect("bounds a"),
            (Some(first), Some(second + 1))
        );
        assert_eq!(
            store.event_bounds("b").expect("bounds b"),
            (Some(second), Some(second))
        );
        let rows = store.events_after("a", first, 1).expect("read a");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, second + 1);
        assert_eq!(rows[0].row.instance_slug, "a");
    }

    #[test]
    fn filtered_event_windows_merge_indexed_kind_and_subject_matches() {
        let temp = TempDb::new();
        let store = Store::open(&temp.path).expect("open store");
        store
            .append_event(&event("a", "unrelated", "other"))
            .expect("append unrelated");
        let kind_match = store
            .append_event(&event("a", "job-a", "Ready"))
            .expect("append kind match");
        let both_match = store
            .append_event(&event("a", "ready", "READY"))
            .expect("append dual match");
        store
            .append_event(&event("a", "job-b", "other"))
            .expect("append second unrelated");
        let fourth_match = store
            .append_event(&event("a", "job-c", "ready"))
            .expect("append fourth match");
        store
            .append_event(&event("b", "ready", "READY"))
            .expect("append other instance");

        let window = store
            .event_window("a", 0, Some("ready"), 3)
            .expect("read filtered events");
        assert_eq!(
            window
                .events
                .iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            vec![kind_match, both_match, fourth_match]
        );
        assert!(window
            .events
            .iter()
            .all(|event| event.row.instance_slug == "a"));
    }

    #[test]
    fn event_contract_rejects_secret_forms_and_controls() {
        let valid = event("a", "job-a", "job.started");
        assert!(validate_event_row(&valid).is_ok());

        for detail in [
            "token=[REDACTED]LEAK",
            "{\"token\":\"[REDACTED]LEAK\"}",
            "api-key=REAL_SECRET",
            "access-token: REAL_SECRET",
            "client-secret=REAL_SECRET",
            "private-key: REAL_SECRET",
            "{\"\\u0074oken\":\"secret\"}",
            r"tok\u0065n=REAL_SECRET",
            r"tok\x65n=REAL_SECRET",
            r"tok\U00000065n=REAL_SECRET",
            "Authorization: Bearer abc",
            "secret: value",
            "safe\nlog",
        ] {
            let mut candidate = valid.clone();
            candidate.detail = Some(detail.to_owned());
            assert!(validate_event_row(&candidate).is_err(), "detail={detail:?}");
        }

        let mut redacted = valid;
        redacted.detail = Some("{\"token\":\"[REDACTED]\"}".to_owned());
        assert!(validate_event_row(&redacted).is_ok());

        for detail in [
            r#"{"api-key":"secret"}"#,
            r#"{"access-token":"secret"}"#,
            r#"{"client-secret":"secret"}"#,
            r#"{"private-key":"secret"}"#,
        ] {
            let mut candidate = event("a", "job-a", "job.started");
            candidate.detail = Some(detail.to_owned());
            assert!(validate_event_row(&candidate).is_err(), "detail={detail:?}");
        }

        let mut escaped_redacted = event("a", "job-a", "job.started");
        escaped_redacted.detail = Some(r"tok\u0065n=[REDACTED]".to_owned());
        assert!(validate_event_row(&escaped_redacted).is_ok());
    }

    #[test]
    fn event_contract_enforces_shared_limits_and_identity() {
        let valid = event("a", "job-a", "job.started");

        let mut too_long = valid.clone();
        too_long.subject = "x".repeat(MAX_EVENT_TEXT_BYTES + 1);
        assert!(validate_event_row(&too_long).is_err());

        let mut too_long_detail = valid.clone();
        too_long_detail.detail = Some("x".repeat(MAX_EVENT_DETAIL_BYTES + 1));
        assert!(validate_event_row(&too_long_detail).is_err());

        let mut invalid_identity = valid;
        invalid_identity.subject = "job name".to_owned();
        assert!(validate_event_row(&invalid_identity).is_err());
    }

    #[test]
    fn event_ancestry_rejects_cross_instance_transition_identity() {
        let temp = TempDb::new();
        let store = Store::open(&temp.path).expect("open store");
        let mut connection = test_connection(&store);
        let transaction = connection.transaction().expect("transaction");
        transaction
            .execute(
                "INSERT INTO job_transitions
                 (instance_slug, job_uid, transition_token, correlation_id, reason,
                  transition_time)
                 VALUES ('instance-a', 'job-a', 'token-a', 'corr-a', 'job.started',
                         '1970-01-01T00:00:00Z')",
                [],
            )
            .expect("seed transition");
        let transition_id = transaction.last_insert_rowid();

        let error = Store::append_event_with_ancestry_in_transaction(
            &transaction,
            &event("instance-b", "job-b", "job.started"),
            Some(transition_id),
            None,
        )
        .expect_err("cross-instance event ancestry must fail closed");
        assert_eq!(error.envelope.reason, "store.event.ancestry");
        transaction.rollback().expect("rollback test transaction");
    }

    #[test]
    fn event_ancestry_rejects_same_instance_cross_job_transition_identity() {
        let temp = TempDb::new();
        let store = Store::open(&temp.path).expect("open store");
        let mut connection = test_connection(&store);
        let transaction = connection.transaction().expect("transaction");
        transaction
            .execute(
                "INSERT INTO job_transitions
                 (instance_slug, job_uid, transition_token, correlation_id, reason,
                  transition_time)
                 VALUES ('instance-a', 'job-a', 'token-a', 'corr-a', 'job.started',
                         '1970-01-01T00:00:00Z')",
                [],
            )
            .expect("seed transition");
        let transition_id = transaction.last_insert_rowid();

        let error = Store::append_event_with_ancestry_in_transaction(
            &transaction,
            &event("instance-a", "job-b", "job.started"),
            Some(transition_id),
            None,
        )
        .expect_err("same-instance cross-job ancestry must fail closed");
        assert_eq!(error.envelope.reason, "store.event.ancestry");
        transaction.rollback().expect("rollback test transaction");
    }

    #[test]
    fn event_contract_rejects_escaped_format_controls_and_credential_forms() {
        let mut row = event("a", "job-a", "job.started");
        row.detail = Some(r"Bearer\tghp_secret".to_owned());
        assert!(validate_event_row(&row).is_err());

        row.detail = Some(r"visible\u202ehidden".to_owned());
        assert!(validate_event_row(&row).is_err());

        row.detail = Some("x".repeat(MAX_EVENT_DETAIL_BYTES + 1));
        assert!(validate_event_row(&row).is_err());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn durable_operation_quota_is_per_instance_and_replays_at_capacity() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-lifecycle-quota-{}-{}",
            std::process::id(),
            Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
                .unsigned_abs()
        ));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let path = directory.join("state.db");
        let store = Store::open(&path).expect("open store");
        let created_at = rfc3339(Timestamp::UNIX_EPOCH);
        let mut connection = test_connection(&store);
        let transaction = connection.transaction().expect("transaction");
        for index in 0..MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE {
            let phase = if index + 1 == MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE {
                "pending"
            } else {
                "accepted"
            };
            transaction
                .execute(
                    "INSERT INTO lifecycle_operations
                     (instance_slug, idempotency_key, operation_id, kind, target, reason,
                      desired_state, desired_slots, resource_version, phase, created_at)
                     VALUES (?1, ?2, ?3, 'cordon', ?1, 'test', 'cordoned', NULL, 1,
                             ?4, ?5)",
                    params![
                        "primary",
                        format!("key-{index}"),
                        format!("op-{index}"),
                        phase,
                        created_at.as_str()
                    ],
                )
                .expect("seed operation");
        }
        transaction.commit().expect("commit seed");
        drop(connection);

        let request = LifecycleOperationRequest {
            instance_slug: "primary".to_owned(),
            kind: "cordon".to_owned(),
            target: "primary".to_owned(),
            reason: "test".to_owned(),
            idempotency_key: "key-0".to_owned(),
            desired_state: "cordoned".to_owned(),
            desired_slots: None,
            expected_version: None,
            operation_id: "new-op".to_owned(),
            created_at: Timestamp::UNIX_EPOCH,
        };
        let (_, replayed) = store
            .record_lifecycle_operation(&request)
            .expect("replay at quota");
        assert!(!replayed);

        let mut fresh = request.clone();
        fresh.idempotency_key = "new-primary".to_owned();
        assert_eq!(
            store
                .record_lifecycle_operation(&fresh)
                .expect_err("primary quota must reject fresh key")
                .envelope
                .reason,
            "store.lifecycle.operation_quota"
        );

        let connection = test_connection(&store);
        let retained_nonterminal: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM lifecycle_operations
                 WHERE instance_slug = 'primary'
                   AND phase IN ('accepted', 'pending')",
                [],
                |row| row.get(0),
            )
            .expect("count retained nonterminal operations");
        assert_eq!(
            retained_nonterminal,
            MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE as i64
        );
        drop(connection);

        fresh.instance_slug = "secondary".to_owned();
        fresh.target = "secondary".to_owned();
        fresh.idempotency_key = "new-secondary".to_owned();
        assert!(
            store
                .record_lifecycle_operation(&fresh)
                .expect("secondary has independent quota")
                .1
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn old_request_timestamp_does_not_reclaim_fresh_operation() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-lifecycle-request-time-{}-{}",
            std::process::id(),
            Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
                .unsigned_abs()
        ));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let path = directory.join("state.db");
        let store = Store::open(&path).expect("open store");
        let created_at = rfc3339(Timestamp::now());
        let mut connection = test_connection(&store);
        let transaction = connection.transaction().expect("transaction");
        for index in 0..(MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE - 1) {
            transaction
                .execute(
                    "INSERT INTO lifecycle_operations
                     (instance_slug, idempotency_key, operation_id, kind, target, reason,
                      desired_state, desired_slots, resource_version, phase, created_at)
                     VALUES ('primary', ?1, ?2, 'cordon', 'primary', 'test', 'cordoned', NULL, 1,
                             'accepted', ?3)",
                    params![
                        format!("key-{index}"),
                        format!("op-{index}"),
                        created_at.as_str()
                    ],
                )
                .expect("seed operation");
        }
        transaction.commit().expect("commit seed");
        drop(connection);

        let request = LifecycleOperationRequest {
            instance_slug: "primary".to_owned(),
            kind: "cordon".to_owned(),
            target: "primary".to_owned(),
            reason: "test".to_owned(),
            idempotency_key: "fresh-key".to_owned(),
            desired_state: "cordoned".to_owned(),
            desired_slots: None,
            expected_version: None,
            operation_id: "fresh-op".to_owned(),
            created_at: Timestamp::UNIX_EPOCH,
        };
        let (accepted, fresh) = store
            .record_lifecycle_operation(&request)
            .expect("fresh operation");
        assert!(fresh);
        assert_ne!(accepted.created_at, Timestamp::UNIX_EPOCH);

        let mut retry = request;
        retry.idempotency_key = "another-key".to_owned();
        retry.operation_id = "another-op".to_owned();
        assert_eq!(
            store
                .record_lifecycle_operation(&retry)
                .expect_err("fresh accepted operation must remain quota-protected")
                .envelope
                .reason,
            "store.lifecycle.operation_quota"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn stale_terminal_operations_reclaim_capacity() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-lifecycle-terminal-retention-{}-{}",
            std::process::id(),
            Timestamp::now()
                .as_offset_datetime()
                .unix_timestamp_nanos()
                .unsigned_abs()
        ));
        std::fs::create_dir_all(&directory).expect("temp directory");
        let path = directory.join("state.db");
        let store = Store::open(&path).expect("open store");
        let mut connection = test_connection(&store);
        let transaction = connection.transaction().expect("transaction");
        let terminal_phases = ["completed", "canceled", "cancelled", "rejected"];
        for index in 0..MAX_LIFECYCLE_OPERATIONS_PER_INSTANCE {
            transaction
                .execute(
                    "INSERT INTO lifecycle_operations
                     (instance_slug, idempotency_key, operation_id, kind, target, reason,
                      desired_state, desired_slots, resource_version, phase, created_at)
                     VALUES ('primary', ?1, ?2, 'cordon', 'primary', 'test', 'cordoned',
                             NULL, 1, ?3, ?4)",
                    params![
                        format!("terminal-key-{index}"),
                        format!("terminal-op-{index}"),
                        terminal_phases[index % terminal_phases.len()],
                        rfc3339(Timestamp::UNIX_EPOCH),
                    ],
                )
                .expect("seed terminal operation");
        }
        transaction.commit().expect("commit seed");
        drop(connection);

        let request = LifecycleOperationRequest {
            instance_slug: "primary".to_owned(),
            kind: "cordon".to_owned(),
            target: "primary".to_owned(),
            reason: "test".to_owned(),
            idempotency_key: "fresh-key".to_owned(),
            desired_state: "cordoned".to_owned(),
            desired_slots: None,
            expected_version: None,
            operation_id: "fresh-op".to_owned(),
            created_at: Timestamp::now(),
        };
        assert!(
            store
                .record_lifecycle_operation(&request)
                .expect("terminal rows reclaim capacity")
                .1
        );

        let connection = test_connection(&store);
        let retained: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM lifecycle_operations WHERE instance_slug = 'primary'",
                [],
                |row| row.get(0),
            )
            .expect("count retained operations");
        assert_eq!(retained, 1);
        drop(connection);
        let _ = std::fs::remove_dir_all(directory);
    }
}
