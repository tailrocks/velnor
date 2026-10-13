//! Operational store sink wiring the daemon to the durable lifecycle store.
//!
//! Plan 066 steps 2–4. The sink is installed once per process (`daemon` or a
//! standalone `run`) and every method after [`install_global`] is
//! failure-absorbing: a store write that fails mid-job logs a forensic error
//! and marks control state degraded, but never changes the GitHub-facing
//! outcome of a running workflow step. Exactly one write class is
//! *required*: the sanitized admission row must persist before a job is
//! accepted; when it cannot, the caller fail-closes that job explicitly as
//! infrastructure rejection instead of silently executing unrecorded work.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use velnor_control::store::{
    EventRow, InstanceRow, RetentionBudget, RetentionLease, Store, StoreError, Transition,
    DEFAULT_STATE_DB_PATH,
};
#[cfg(test)]
use velnor_model::ExitClass;
use velnor_model::{
    EventReason, JobPhase, JobSummary as ModelJobSummary, NormalizedJob, RepositoryRef, Slug,
    Timestamp, TriggerEvent,
};

/// Environment override for the operational database location; tests and
/// fixture validation runs point this at a temporary file.
pub const STATE_DB_ENV: &str = "VELNOR_STATE_DB";

/// How often the daemon-side retention pass may run.
const PRUNE_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Keep transient/pre-commit failures retryable without allowing a durable
/// post-commit/reporting failure to spin the retention path.
const PRUNE_RETRY_INITIAL: Duration = Duration::from_secs(15);
const PRUNE_RETRY_MAX: Duration = PRUNE_INTERVAL;
/// The lease is longer than the bounded store pass, but remains finite so an
/// abandoned process cannot suppress maintenance indefinitely.
const PRUNE_LEASE_DURATION: Duration = Duration::from_secs(30 * 60);

static OPS: OnceLock<Arc<OpsSink>> = OnceLock::new();

/// Resolved operational database path: `VELNOR_STATE_DB` wins so fixture
/// validation and tests never touch the deployed store.
#[must_use]
pub fn state_db_path() -> PathBuf {
    std::env::var_os(STATE_DB_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DB_PATH))
}

/// The process-wide sink, when one was installed.
#[must_use]
pub fn global() -> Option<&'static Arc<OpsSink>> {
    OPS.get()
}

fn install_global(sink: Arc<OpsSink>) {
    let _ = OPS.set(sink);
}

/// Open the operational store as a mandatory readiness gate.
pub fn init(instance_slug: String) -> Result<(), velnor_control::store::StoreError> {
    let sink = OpsSink::open(state_db_path(), instance_slug)?;
    install_global(Arc::new(sink));
    Ok(())
}

/// Host text projected onto the slug charset for instance identity; empty
/// hosts fall back to a stable placeholder instead of failing validation.
#[must_use]
pub fn sanitize_slug_for_instance(raw: &str) -> String {
    let slug = sanitize_slug(raw);
    if slug == "unknown" {
        "velnor-instance".to_owned()
    } else {
        slug
    }
}

/// Everything needed to persist one sanitized admission row.
#[derive(Debug, Clone)]
pub struct JobAdmission {
    pub instance_slug: String,
    pub job_uid: String,
    pub repository_full_name: String,
    pub workflow: String,
    pub job_name: String,
    pub run_id: Option<u64>,
    pub attempt: Option<u32>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub trigger_event: Option<String>,
    pub queued_at_rfc3339: Option<String>,
    pub slot_name: Option<String>,
    pub runner_name: Option<String>,
    pub trust_scope: Option<String>,
    pub resource_policy: Option<String>,
    /// Secret/mask values collected from the raw job message; applied to
    /// every textual projection so no secret value can enter the store even
    /// when a workflow embeds one in its name or ref.
    pub masks: Vec<String>,
}

impl JobAdmission {
    /// Stable store identity for a summary row, sourced from the normalized
    /// GitHub job identity rather than the run/attempt pair.
    #[must_use]
    pub fn job_uid(&self) -> Option<String> {
        (!self.job_uid.trim().is_empty()).then(|| self.job_uid.clone())
    }

    fn model_summary(&self) -> Result<ModelJobSummary, String> {
        let repository_full = self.project(&self.repository_full_name);
        let (owner, name) = repository_full
            .rsplit_once('/')
            .ok_or_else(|| "repository must be owner/name".to_owned())?;
        ModelJobSummary::from_normalized(NormalizedJob {
            instance_slug: self.instance_slug.clone(),
            job_uid: self.job_uid().unwrap_or_default(),
            repository: RepositoryRef::new(owner, name),
            workflow: self.project(&self.workflow),
            job_name: self.project(&self.job_name),
            run_id: self.run_id,
            attempt: self.attempt,
            head_ref: self.head_ref.as_deref().map(|v| self.project(v)),
            head_sha: self.head_sha.as_deref().map(|v| self.project(v)),
            trigger_event: self
                .trigger_event
                .as_deref()
                .and_then(|raw| TriggerEvent::try_from(raw).ok()),
            queued_at: self
                .queued_at_rfc3339
                .as_deref()
                .and_then(|raw| Timestamp::parse(raw).ok()),
            acquired_at: Some(Timestamp::now()),
            slot_name: self.slot_name.as_deref().map(|v| self.project(v)),
            runner_name: self.runner_name.as_deref().map(|v| self.project(v)),
            trust_scope: self.trust_scope.as_deref().map(|v| self.project(v)),
            resource_policy: self
                .resource_policy
                .as_deref()
                .map(|value| self.project(value)),
            phase: JobPhase::Queued,
            conclusion: None,
            infrastructure_category: None,
        })
        .map_err(|error| error.to_string())
    }

    /// Mask, then project onto the slug charset. Validation still runs
    /// afterwards and fails closed naming only the field.
    fn project(&self, raw: &str) -> String {
        let masked = crate::runner::mask_all(raw, &self.masks);
        sanitize_slug(&masked)
    }
}

/// Project arbitrary workflow-provided text onto the slug charset so
/// validation can never reject an otherwise-runnable job while no raw value
/// ever reaches the database unchanged.
fn sanitize_slug(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = true;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '/') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 512 {
            break;
        }
    }
    let trimmed = out.trim_end_matches('-').to_owned();
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed
    }
}

/// Admission token held by exactly one retention worker.
///
/// Dropping the token releases admission even when a blocking task is
/// cancelled before it starts or unwinds after a panic.
pub(crate) struct PruneAdmission {
    in_flight: Arc<AtomicBool>,
    now_unix: u64,
}

/// Finalizes a runtime lease on every exit path, including panic unwinding or
/// cancellation after acquisition. The SQL release is one owner+generation
/// qualified zero-wait statement, so finalization cannot wait on a competing
/// writer for an unbounded interval.
struct RetentionLeaseGuard<'a> {
    store: &'a Store,
    lease: Option<RetentionLease>,
    telemetry: Option<&'a OpsSink>,
}

impl<'a> RetentionLeaseGuard<'a> {
    #[cfg(test)]
    fn new(store: &'a Store, lease: RetentionLease) -> Self {
        Self {
            store,
            lease: Some(lease),
            telemetry: None,
        }
    }

    fn with_sink(sink: &'a OpsSink, lease: RetentionLease) -> Self {
        Self {
            store: &sink.store,
            lease: Some(lease),
            telemetry: Some(sink),
        }
    }

    fn lease(&self) -> &RetentionLease {
        self.lease
            .as_ref()
            .expect("retention lease guard must own a lease")
    }

    fn release(&mut self) -> Result<bool, StoreError> {
        let Some(lease) = self.lease.take() else {
            return Ok(false);
        };
        // The store finalizer treats false matches and SQLite errors as
        // finalization failures and retries only bounded transient contention.
        self.store
            .release_retention_lease_final(&lease)
            .map(|()| true)
    }
}

impl Drop for RetentionLeaseGuard<'_> {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            if self.store.release_retention_lease_final(&lease).is_err() {
                if let Some(sink) = self.telemetry {
                    sink.report_lease_finalization_failure();
                } else {
                    eprintln!(
                        "{}",
                        forensic_failure_line(
                            "store.prune-lease-release",
                            "bounded finalization attempt failed",
                        )
                    );
                }
            }
        }
    }
}

impl Drop for PruneAdmission {
    fn drop(&mut self) {
        self.in_flight.store(false, Ordering::Release);
    }
}

/// Per-process handle over the shared operational database.
pub struct OpsSink {
    store: Store,
    instance_slug: String,
    retention_owner: String,
    // Keep admitted masks for this sink lifetime: a later operational event
    // may repeat an earlier secret, so eviction would re-enable persistence.
    masks: Mutex<Vec<String>>,
    degraded: AtomicBool,
    last_prune_unix: AtomicU64,
    next_prune_attempt_unix: AtomicU64,
    prune_retry_delay_secs: AtomicU64,
    prune_in_flight: Arc<AtomicBool>,
    budget: RetentionBudget,
    #[cfg(test)]
    injected_write_failure: Mutex<Option<(ExitClass, &'static str)>>,
    #[cfg(test)]
    forensic_failures: Mutex<Vec<String>>,
    #[cfg(test)]
    injected_prune_failure: AtomicBool,
    #[cfg(test)]
    injected_prune_store_failure: Mutex<Option<StoreError>>,
}

impl OpsSink {
    /// Open (and migrate) the store at `path`.
    ///
    /// # Errors
    /// Any open or migration failure with its envelope classification.
    pub fn open(
        path: PathBuf,
        instance_slug: String,
    ) -> Result<Self, velnor_control::store::StoreError> {
        let store = Store::open(&path)?;
        Ok(Self {
            store,
            instance_slug,
            retention_owner: format!(
                "velnor-retention-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ),
            masks: Mutex::new(Vec::new()),
            degraded: AtomicBool::new(false),
            last_prune_unix: AtomicU64::new(0),
            next_prune_attempt_unix: AtomicU64::new(0),
            prune_retry_delay_secs: AtomicU64::new(0),
            prune_in_flight: Arc::new(AtomicBool::new(false)),
            budget: RetentionBudget::default(),
            #[cfg(test)]
            injected_write_failure: Mutex::new(None),
            #[cfg(test)]
            forensic_failures: Mutex::new(Vec::new()),
            #[cfg(test)]
            injected_prune_failure: AtomicBool::new(false),
            #[cfg(test)]
            injected_prune_store_failure: Mutex::new(None),
        })
    }

    /// Validated slug naming this daemon instance in the shared store.
    #[must_use]
    pub fn instance_slug(&self) -> &str {
        &self.instance_slug
    }

    /// Whether any mid-job write has failed since startup.
    #[must_use]
    pub fn degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    /// REQUIRED write before accepting a job: persists the sanitized
    /// admission row plus its `job.acquired` transition.
    ///
    /// Returns `false` — and never panics — when validation or persistence
    /// fails; the caller must then reject the job explicitly rather than
    /// execute unrecorded work.
    pub fn record_admission(&self, admission: &JobAdmission) -> bool {
        let Some(job_uid) = admission.job_uid() else {
            // The admission row is the required durable record before a job
            // can execute. Without both identity fields it has no stable key,
            // so accepting the job would create unrecorded work.
            return self.required_failure("store.admission.identity", "github.job_id is required");
        };
        let summary = match admission.model_summary() {
            Ok(summary) => summary,
            Err(error) => {
                return self.required_failure("store.admission.validate", &error);
            }
        };
        if summary.instance_slug() != self.instance_slug {
            return self.required_failure(
                "store.admission.instance",
                "admission instance does not match the installed operational-store sink",
            );
        }
        if !self.remember_masks(&admission.masks) {
            return self.required_failure(
                "store.masks",
                "event mask registry is unavailable; admission rejected",
            );
        }
        let token = format!("t-acquired-{job_uid}");
        let correlation_id = match Slug::validate("correlation_id", &format!("corr-{token}")) {
            Ok(value) => value,
            Err(error) => {
                return self.required_failure("store.admission.transition", &error.to_string());
            }
        };
        let transition = Transition {
            token,
            correlation_id,
            reason: EventReason::JobAcquired,
            message: Some("admission persisted".to_owned()),
            transition_time: Timestamp::now(),
            conclusion: None,
            infrastructure_category: None,
        };
        if let Err(error) = self.before_store_write() {
            return self.required_failure("store.admission.persist", &error.to_string());
        }
        if let Err(error) = self.store.persist_summary_and_transition(
            &summary,
            &self.instance_slug,
            &job_uid,
            &transition,
        ) {
            return self.required_failure("store.admission.persist", &error.to_string());
        }
        true
    }

    /// Best-effort normalized event; failures degrade, never propagate.
    pub fn emit(&self, reason: EventReason, subject: &str, detail: Option<String>) {
        let Some(masks) = self.event_masks() else {
            return;
        };
        let subject = sanitize_event_subject(subject, &masks);
        let detail = detail.map(|value| sanitize_event_detail(&value, &masks));
        let correlation = Slug::validate("correlation_id", &subject).ok();
        if let Err(error) = self.before_store_write() {
            self.absorb("store.event", &error.to_string());
            return;
        }
        if let Err(error) = self.store.append_event(&EventRow {
            instance_slug: self.instance_slug.clone(),
            event_kind: reason.as_str().to_owned(),
            subject,
            correlation_id: correlation.map(|slug| slug.as_str().to_owned()),
            occurred_at: Timestamp::now(),
            detail,
        }) {
            self.absorb("store.event", &error.to_string());
        }
    }

    /// Best-effort idempotent job transition; replaying `(job, token)` is a
    /// no-op at the store, so retry storms cannot duplicate terminal events.
    pub fn transition(
        &self,
        job_uid: &str,
        token: &str,
        reason: EventReason,
        message: Option<String>,
        conclusion: Option<String>,
        infrastructure_category: Option<String>,
    ) -> bool {
        let Some(masks) = self.event_masks() else {
            return false;
        };
        let Ok(correlation) = Slug::validate("correlation_id", &format!("corr-{token}")) else {
            return false;
        };
        let transition = Transition {
            token: token.to_owned(),
            correlation_id: correlation,
            reason,
            message: message.map(|value| sanitize_event_detail(&value, &masks)),
            transition_time: Timestamp::now(),
            conclusion,
            infrastructure_category,
        };
        if let Err(error) = self.before_store_write() {
            self.absorb("store.transition", &error.to_string());
            return false;
        }
        if let Err(error) =
            self.store
                .record_job_transition(&self.instance_slug, job_uid, &transition)
        {
            self.absorb("store.transition", &error.to_string());
            return false;
        }
        true
    }

    pub(crate) fn try_admit_prune(&self) -> Option<PruneAdmission> {
        let now_unix = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp()
            .unsigned_abs();
        self.try_admit_prune_at(now_unix)
    }

    /// Defer the first retention pass until the normal interval after the
    /// daemon has announced readiness. Readiness persistence/configuration is
    /// still settling at that boundary, so an initial `last_prune_unix=0`
    /// must not make retention immediately compete with supervision startup.
    pub(crate) fn defer_initial_prune(&self, now_unix: u64) {
        let _ =
            self.last_prune_unix
                .compare_exchange(0, now_unix, Ordering::AcqRel, Ordering::Acquire);
    }

    #[cfg(test)]
    fn prune_if_due_at(&self, now_unix: u64) {
        if let Some(admission) = self.try_admit_prune_at(now_unix) {
            self.prune_once(admission.now_unix, Some(now_unix));
        }
    }

    fn try_admit_prune_at(&self, now_unix: u64) -> Option<PruneAdmission> {
        let last = self.last_prune_unix.load(Ordering::Relaxed);
        let next_attempt = self.next_prune_attempt_unix.load(Ordering::Acquire);
        let due = if next_attempt > 0 {
            now_unix >= next_attempt
        } else {
            now_unix.saturating_sub(last) >= PRUNE_INTERVAL.as_secs()
        };
        if !due {
            return None;
        }
        if self
            .prune_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        Some(PruneAdmission {
            in_flight: Arc::clone(&self.prune_in_flight),
            now_unix,
        })
    }

    pub(crate) fn run_admitted_prune(&self, admission: PruneAdmission) {
        self.prune_once(admission.now_unix, None);
    }

    fn prune_once(&self, _scheduled_now_unix: u64, completion_override: Option<u64>) {
        let completion_now = || {
            completion_override.unwrap_or_else(|| {
                Timestamp::now()
                    .as_offset_datetime()
                    .unix_timestamp()
                    .unsigned_abs()
            })
        };
        let lease = match self
            .store
            .try_acquire_retention_lease(&self.retention_owner, PRUNE_LEASE_DURATION)
        {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                // Live ownership is expected cross-process contention, not
                // degraded control state. It remains retryable and visible.
                self.schedule_prune_retry(completion_now());
                self.record_forensic_failure(
                    "store.prune-lease-busy",
                    "retention lease is held by another daemon",
                );
                tracing::debug!(target: "velnor::ops", "retention lease is held by another daemon");
                return;
            }
            Err(error) => {
                self.schedule_prune_retry(completion_now());
                if error.envelope.reason == "store.locked" {
                    self.record_forensic_failure("store.prune-lease-busy", &error.envelope.reason);
                } else {
                    self.absorb("store.prune-lease", &error.envelope.reason);
                }
                return;
            }
        };
        let mut lease_guard = RetentionLeaseGuard::with_sink(self, lease);

        let mut committed = false;
        let mut maintenance_retry = false;
        let completion;
        let prune_result = {
            #[cfg(test)]
            if let Some(error) = self.injected_prune_store_failure.lock().unwrap().take() {
                Err(velnor_control::store::retention::PruneFailure::PreCommit(
                    error,
                ))
            } else {
                self.store
                    .prune_history_outcome_with_lease(&self.budget, lease_guard.lease())
            }
            #[cfg(not(test))]
            {
                self.store
                    .prune_history_outcome_with_lease(&self.budget, lease_guard.lease())
            }
        };
        match prune_result {
            Ok(report) => {
                committed = true;
                maintenance_retry = report.maintenance_deferred;
                completion = completion_now();
                if maintenance_retry {
                    self.record_forensic_failure(
                        "store.retention-maintenance-deferred",
                        report
                            .maintenance_reason
                            .as_deref()
                            .unwrap_or("physical retention maintenance deferred"),
                    );
                }
                self.publish_prune_report(report);
            }
            Err(failure) if failure.is_post_commit() => {
                committed = true;
                completion = completion_now();
                self.clear_prune_retry();
                self.absorb("store.prune-post-commit", &failure.error().envelope.reason);
            }
            Err(failure) if failure.is_lease_lost() => {
                if failure.is_post_commit() {
                    committed = true;
                    completion = completion_now();
                    self.clear_prune_retry();
                    self.absorb("store.prune-lease-lost", &failure.error().envelope.reason);
                } else {
                    completion = completion_now();
                    self.schedule_prune_retry(completion);
                    self.record_forensic_failure(
                        "store.prune-lease-lost",
                        &failure.error().envelope.reason,
                    );
                }
            }
            Err(failure) => {
                completion = completion_now();
                self.schedule_prune_retry(completion);
                self.absorb("store.prune", &failure.error().envelope.reason);
            }
        }
        #[cfg(test)]
        if committed && self.injected_prune_failure.swap(false, Ordering::Relaxed) {
            committed = true;
            self.clear_prune_retry();
            self.absorb(
                "store.accounting",
                "test-injected post-commit accounting failure",
            );
        }

        if committed {
            // Completion is recorded before release. A release/reporting
            // failure must not cause already durable deletions to run again.
            self.last_prune_unix.store(completion, Ordering::Release);
            if maintenance_retry {
                // Deletion is already durable; retry only the bounded
                // retention/maintenance cycle on its normal backoff. The
                // explicit status prevents a physical failure from becoming
                // silent, while never treating committed deletion as rolled
                // back work.
                self.schedule_prune_retry(completion);
            } else {
                self.clear_prune_retry();
            }
        }
        match lease_guard.release() {
            Ok(true) => {}
            Ok(false) | Err(_) => self.report_lease_finalization_failure(),
        }
    }

    fn publish_prune_report(&self, report: velnor_control::store::PruneReport) {
        if report.maintenance_deferred {
            println!(
                "forensics.ops event=retention-maintenance-deferred reason={}",
                report
                    .maintenance_reason
                    .as_deref()
                    .unwrap_or("unspecified")
            );
        }
        // Publish the coherent post-prune report so retention stays
        // observable from logs without a second full accounting scan.
        println!(
            "forensics.ops event=retention deleted_jobs={} deleted_events={} deleted_transitions={} db_bytes={} wal_bytes={} total_bytes={} free_bytes={} reserve_bytes={} reserve_violation={} physical_budget_status={:?} checkpoint_attempted={} checkpoint_busy_frames={} checkpoint_log_frames={} checkpointed_frames={} oldest_retained_at={}",
            report.deleted_jobs,
            report.deleted_events,
            report.deleted_transitions,
            report.database_bytes,
            report.wal_bytes,
            report.total_bytes,
            report.free_bytes.unwrap_or(0),
            report.reserve_bytes,
            report.reserve_violation,
            report.physical_budget_status,
            report.checkpoint.attempted,
            report.checkpoint.busy_frames,
            report.checkpoint.log_frames,
            report.checkpoint.checkpointed_frames,
            report.oldest_retained_at.as_deref().unwrap_or("none"),
        );
    }

    pub(crate) fn record_prune_worker_failure(&self, now_unix: u64, detail: &str) {
        self.schedule_prune_retry(now_unix);
        self.absorb("store.prune-worker", detail);
    }

    pub(crate) fn prune_wait_duration(&self) -> Duration {
        let now_unix = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp()
            .unsigned_abs();
        self.prune_wait_duration_at(now_unix)
    }

    fn prune_wait_duration_at(&self, now_unix: u64) -> Duration {
        let next_attempt = self.next_prune_attempt_unix.load(Ordering::Acquire);
        let due_at = if next_attempt > 0 {
            next_attempt
        } else {
            self.last_prune_unix
                .load(Ordering::Relaxed)
                .saturating_add(PRUNE_INTERVAL.as_secs())
        };
        Duration::from_secs(due_at.saturating_sub(now_unix))
    }

    fn schedule_prune_retry(&self, now_unix: u64) {
        let previous = self.prune_retry_delay_secs.load(Ordering::Relaxed);
        let initial = PRUNE_RETRY_INITIAL.as_secs();
        let maximum = PRUNE_RETRY_MAX.as_secs();
        let delay = if previous == 0 {
            initial
        } else {
            previous.saturating_mul(2).clamp(initial, maximum)
        };
        self.prune_retry_delay_secs
            .store(delay.min(maximum), Ordering::Relaxed);
        self.next_prune_attempt_unix.store(
            now_unix.saturating_add(delay.min(maximum)),
            Ordering::Release,
        );
    }

    fn clear_prune_retry(&self) {
        self.prune_retry_delay_secs.store(0, Ordering::Relaxed);
        self.next_prune_attempt_unix.store(0, Ordering::Release);
    }

    /// Read-only accounting snapshot for diagnostics.
    ///
    /// # Errors
    /// Propagates store read failures.
    #[allow(dead_code)]
    pub fn accounting(
        &self,
    ) -> Result<velnor_control::store::StoreAccounting, velnor_control::store::StoreError> {
        self.store.accounting()
    }

    /// Refresh the current instance row (identity, version, slot counts).
    ///
    /// # Errors
    /// Propagates persistence failures; callers treat this as best-effort.
    pub fn upsert_instance(
        &self,
        host: &str,
        daemon_version: &str,
        slots_configured: u32,
    ) -> velnor_control::store::StoreResult<()> {
        self.store.upsert_instance(&InstanceRow {
            instance_slug: self.instance_slug.clone(),
            host: sanitize_slug(host),
            daemon_version: sanitize_slug(daemon_version),
            slots_configured,
            slots_busy: 0,
            updated_at: Timestamp::now(),
        })
    }

    fn required_failure(&self, code: &str, detail: &str) -> bool {
        let detail = sanitize_forensic_detail(detail);
        eprintln!("REQUIRED operational-store write failed ({code}): {detail}");
        self.degraded.store(true, Ordering::Relaxed);
        self.record_forensic_failure(code, &detail);
        eprintln!("{}", forensic_failure_line(code, &detail));
        false
    }

    fn before_store_write(&self) -> Result<(), StoreError> {
        #[cfg(test)]
        if let Ok(mut injected) = self.injected_write_failure.lock() {
            if let Some((class, reason)) = injected.take() {
                return Err(
                    StoreError::new(class, reason).with_remediation("test-injected write failure")
                );
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn fail_next_store_write(&self, class: ExitClass, reason: &'static str) {
        self.injected_write_failure
            .lock()
            .unwrap()
            .replace((class, reason));
    }

    #[cfg(test)]
    fn fail_next_prune_accounting(&self) {
        self.injected_prune_failure.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn fail_next_prune_store(&self, class: ExitClass, reason: &'static str) {
        self.injected_prune_store_failure
            .lock()
            .unwrap()
            .replace(StoreError::new(class, reason));
    }

    #[cfg(test)]
    fn forensic_failures(&self) -> Vec<String> {
        self.forensic_failures.lock().unwrap().clone()
    }

    fn record_forensic_failure(&self, code: &str, detail: &str) {
        #[cfg(not(test))]
        let _ = (code, detail);
        #[cfg(test)]
        self.forensic_failures
            .lock()
            .unwrap()
            .push(forensic_failure_line(code, detail));
    }

    fn remember_masks(&self, values: &[String]) -> bool {
        let Ok(mut masks) = self.masks.lock() else {
            self.absorb("store.masks", "mask registry lock poisoned");
            return false;
        };
        for value in values.iter().filter(|value| !value.is_empty()) {
            if !masks.iter().any(|known| known == value) {
                masks.push(value.clone());
            }
        }
        true
    }

    fn event_masks(&self) -> Option<Vec<String>> {
        match self.masks.lock() {
            Ok(values) => Some(values.clone()),
            Err(_) => {
                self.absorb("store.masks", "mask registry lock poisoned");
                None
            }
        }
    }

    fn absorb(&self, code: &str, detail: &str) {
        self.degraded.store(true, Ordering::Relaxed);
        self.record_forensic_failure(code, detail);
        let detail = sanitize_forensic_detail(detail);
        tracing::error!(target: "velnor::ops", code, error = %detail, "operational store failure");
        eprintln!("{}", forensic_failure_line(code, &detail));
    }

    fn report_lease_finalization_failure(&self) {
        self.degraded.store(true, Ordering::Relaxed);
        const DETAIL: &str = "bounded finalization attempt failed";
        self.record_forensic_failure("store.prune-lease-release", DETAIL);
        eprintln!(
            "{}",
            forensic_failure_line("store.prune-lease-release", DETAIL)
        );
    }
}

fn forensic_failure_line(code: &str, detail: &str) -> String {
    format!(
        "forensics.ops event=store-write-failed code={} error={}",
        sanitize_forensic_code(code),
        sanitize_forensic_detail(detail)
    )
}

fn sanitize_forensic_code(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len().min(128));
    for character in raw.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
            output.push(character);
        }
        if output.len() >= 128 {
            break;
        }
    }
    if output.is_empty() {
        "unknown".to_owned()
    } else {
        output
    }
}

fn sanitize_forensic_detail(raw: &str) -> String {
    const MAX_FORENSIC_DETAIL_BYTES: usize = 512;
    let lower = raw.to_ascii_lowercase();
    if [
        "authorization:",
        "bearer ",
        "password",
        "secret",
        "token=",
        "token:",
        "fingerprint=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "redacted-sensitive-diagnostic".to_owned();
    }
    let mut output = String::with_capacity(raw.len().min(MAX_FORENSIC_DETAIL_BYTES));
    for character in raw.chars() {
        let character = if character.is_control() || character.is_whitespace() {
            ' '
        } else {
            character
        };
        if output.len() + character.len_utf8() > MAX_FORENSIC_DETAIL_BYTES {
            break;
        }
        output.push(character);
    }
    if output.is_empty() {
        "unknown".to_owned()
    } else {
        output
    }
}

fn sanitize_event_subject(raw: &str, masks: &[String]) -> String {
    sanitize_slug(&crate::runner::mask_all(raw, masks))
}

fn sanitize_event_detail(raw: &str, masks: &[String]) -> String {
    const MAX_EVENT_DETAIL_BYTES: usize = 4096;
    let masked = crate::runner::mask_all(raw, masks);
    let mut detail = String::with_capacity(masked.len().min(MAX_EVENT_DETAIL_BYTES));
    for character in masked.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if detail.len() + character.len_utf8() > MAX_EVENT_DETAIL_BYTES {
            break;
        }
        detail.push(character);
    }
    if detail.is_empty() {
        "unknown".to_owned()
    } else {
        detail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_sink(label: &str) -> (std::path::PathBuf, Arc<OpsSink>) {
        let nanos = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp_nanos()
            .unsigned_abs();
        let dir = std::env::temp_dir().join(format!("velnor-ops-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let sink =
            Arc::new(OpsSink::open(dir.join("state.db"), "test-instance".to_owned()).unwrap());
        (dir, sink)
    }

    fn admission(run_id: u64, secret: Option<&str>) -> JobAdmission {
        JobAdmission {
            instance_slug: "test-instance".to_owned(),
            job_uid: format!("job-{run_id}"),
            repository_full_name: "tailrocks/velnor-actions-fixture".to_owned(),
            workflow: "control plane".to_owned(),
            job_name: secret.map_or("hold".to_owned(), str::to_owned),
            run_id: Some(run_id),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("fixture-runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            masks: secret.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    #[test]
    fn admission_persists_sanitized_row_and_acquired_transition() {
        let (_dir, sink) = temp_sink("admission");
        assert!(sink.record_admission(&admission(101, None)));
        let uid = admission(101, None).job_uid().unwrap();
        let stored = sink.store.fetch_summary("test-instance", 101, 1).unwrap();
        assert!(stored.is_some());
        assert_eq!(
            sink.store.transition_count("test-instance", &uid).unwrap(),
            1
        );
        // Workflow names carry spaces in GitHub; the stored spelling is the
        // sanitized projection.
        let summary = stored.unwrap();
        assert_eq!(summary.workflow(), "control-plane");
    }

    #[test]
    fn admission_without_complete_identity_fails_closed() {
        for missing in ["run_id", "attempt"] {
            let (_dir, sink) = temp_sink(&format!("missing-{missing}"));
            let mut adm = admission(111, None);
            match missing {
                "run_id" => adm.run_id = None,
                "attempt" => adm.attempt = None,
                _ => unreachable!("test only covers required identity fields"),
            }

            assert!(
                !sink.record_admission(&adm),
                "missing {missing} was accepted"
            );
            assert!(sink.degraded());
            assert!(sink
                .store
                .job_summaries("test-instance")
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn admission_fails_closed_when_acquired_transition_is_rejected() {
        let (_dir, sink) = temp_sink("admission-transition-failure");
        let mut adm = admission(121, None);
        // Force the summary and transition to address different store
        // instances. The summary write succeeds, but the required transition
        // must be rejected rather than allowing execution to proceed.
        adm.instance_slug = "other-instance".to_owned();

        assert!(!sink.record_admission(&adm));
        assert!(sink.degraded());
    }

    #[test]
    fn injected_admission_write_failure_rejects_without_partial_summary() {
        let (_dir, sink) = temp_sink("injected-admission-failure");
        sink.fail_next_store_write(ExitClass::Operation, "store.test.disk-full");

        assert!(!sink.record_admission(&admission(122, None)));
        assert!(sink.degraded());
        assert!(sink
            .forensic_failures()
            .iter()
            .any(|entry| entry.contains("store.admission.persist")
                && entry.contains("store.test.disk-full")));
        assert!(sink
            .store
            .job_summaries("test-instance")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn injected_event_write_failure_degrades_without_changing_job_state() {
        let (_dir, sink) = temp_sink("injected-event-failure");
        let adm = admission(123, None);
        assert!(sink.record_admission(&adm));
        sink.fail_next_store_write(ExitClass::Operation, "store.test.disk-full");

        sink.emit(
            EventReason::ReadinessDegraded,
            "readiness-failure-123",
            Some("injected failure".to_owned()),
        );

        assert!(sink.degraded());
        assert!(sink
            .forensic_failures()
            .iter()
            .any(|entry| entry.contains("store.event") && entry.contains("store.test.disk-full")));
        assert_eq!(
            sink.store
                .event_count("test-instance", "readiness-failure-123")
                .unwrap(),
            0
        );
        assert_eq!(
            sink.store
                .fetch_summary("test-instance", 123, 1)
                .unwrap()
                .unwrap()
                .phase(),
            JobPhase::Running
        );
    }

    #[test]
    fn injected_transition_write_failure_degrades_without_changing_job_state() {
        let (_dir, sink) = temp_sink("injected-transition-failure");
        let adm = admission(124, None);
        assert!(sink.record_admission(&adm));
        let uid = adm.job_uid().unwrap();
        sink.fail_next_store_write(ExitClass::Timeout, "store.test.locked");

        assert!(!sink.transition(
            &uid,
            "t-started-injected",
            EventReason::JobStarted,
            None,
            None,
            None,
        ));

        assert!(sink.degraded());
        assert!(
            sink.forensic_failures()
                .iter()
                .any(|entry| entry.contains("store.transition")
                    && entry.contains("store.test.locked"))
        );
        assert_eq!(
            sink.store.transition_count("test-instance", &uid).unwrap(),
            1
        );
        assert_eq!(
            sink.store
                .fetch_summary("test-instance", 124, 1)
                .unwrap()
                .unwrap()
                .phase(),
            JobPhase::Running
        );
    }

    #[test]
    fn actual_sqlite_lock_during_event_write_degrades_without_job_mutation() {
        let (dir, sink) = temp_sink("actual-locked-event");
        let adm = admission(125, None);
        assert!(sink.record_admission(&adm));

        let locker = rusqlite::Connection::open(dir.join("state.db")).unwrap();
        locker
            .busy_timeout(std::time::Duration::from_millis(1))
            .unwrap();
        locker.execute_batch("BEGIN IMMEDIATE").unwrap();

        sink.emit(
            EventReason::ReadinessDegraded,
            "actual-lock-125",
            Some("locked write".to_owned()),
        );

        assert!(sink.degraded());
        assert!(sink
            .forensic_failures()
            .iter()
            .any(|entry| entry.contains("store.event") && entry.contains("store.locked")));
        assert_eq!(
            sink.store
                .fetch_summary("test-instance", 125, 1)
                .unwrap()
                .unwrap()
                .phase(),
            JobPhase::Running
        );
        locker.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn concurrent_event_writes_consume_one_injected_failure_deterministically() {
        let (_dir, sink) = temp_sink("concurrent-injected-failure");
        sink.fail_next_store_write(ExitClass::Operation, "store.test.disk-full");

        let workers: Vec<_> = (0..8)
            .map(|sequence| {
                let sink = Arc::clone(&sink);
                std::thread::spawn(move || {
                    sink.emit(
                        EventReason::ReadinessDegraded,
                        &format!("concurrent-{sequence}"),
                        Some("concurrent write".to_owned()),
                    );
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }

        assert!(sink.degraded());
        assert_eq!(sink.forensic_failures().len(), 1);
        assert_eq!(sink.store.accounting().unwrap().event_rows, 7);
    }

    #[test]
    fn atomic_admission_rolls_back_summary_when_transition_fails() {
        let (_dir, sink) = temp_sink("atomic-admission");
        let summary = admission(131, None).model_summary().unwrap();
        let token = "t-acquired-wrong-job".to_owned();
        let transition = Transition {
            token,
            correlation_id: Slug::validate("correlation_id", "corr-t-acquired-wrong-job").unwrap(),
            reason: EventReason::JobAcquired,
            message: Some("admission persisted".to_owned()),
            transition_time: Timestamp::now(),
            conclusion: None,
            infrastructure_category: None,
        };

        assert!(
            sink.store
                .persist_summary_and_transition(
                    &summary,
                    "test-instance",
                    "wrong-job-uid",
                    &transition,
                )
                .is_err()
        );
        assert!(sink
            .store
            .job_summaries("test-instance")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn secrets_never_reach_database_pages() {
        let (dir, sink) = temp_sink("secret-safety");
        let marker_secret = "super-secret-marker-value-42";
        // A workflow whose display name embeds a secret variable value still
        // admits — but every projection is masked first.
        assert!(sink.record_admission(&admission(202, Some(marker_secret))));
        let all = sink.store.job_summaries("test-instance").unwrap();
        assert_eq!(all.len(), 1);
        assert!(!all[0].job_name.contains(marker_secret));
        assert_eq!(all[0].job_name, "unknown");
        assert_eq!(
            sink.store
                .event_count("test-instance", marker_secret)
                .unwrap(),
            0
        );
        sink.emit(
            EventReason::ReadinessDegraded,
            marker_secret,
            Some(format!("failure detail: {marker_secret}")),
        );
        assert_eq!(
            sink.store
                .event_count("test-instance", marker_secret)
                .unwrap(),
            0
        );
        assert_eq!(
            sink.store.event_count("test-instance", "unknown").unwrap(),
            1
        );
        drop(sink);
        for filename in ["state.db", "state.db-wal", "state.db-shm"] {
            let path = dir.join(filename);
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            assert!(
                !bytes
                    .windows(marker_secret.len())
                    .any(|window| window == marker_secret.as_bytes()),
                "secret marker reached {filename}"
            );
        }
    }

    #[test]
    fn transitions_replay_idempotently_and_terminal_events_do_not_duplicate() {
        let (_dir, sink) = temp_sink("replay");
        let adm = admission(303, None);
        assert!(sink.record_admission(&adm));
        let uid = adm.job_uid().unwrap();
        let waiting = format!("t-waiting-{uid}");
        sink.transition(&uid, &waiting, EventReason::JobWaiting, None, None, None);
        let started = format!("t-started-{uid}");
        sink.transition(&uid, &started, EventReason::JobStarted, None, None, None);
        let complete = format!("t-completed-{uid}");
        for _ in 0..3 {
            sink.transition(
                &uid,
                &complete,
                EventReason::JobCompleted,
                Some("done".to_owned()),
                Some("success".to_owned()),
                None,
            );
        }
        assert_eq!(
            sink.store.transition_count("test-instance", &uid).unwrap(),
            4
        );
        assert_eq!(sink.store.event_count("test-instance", &uid).unwrap(), 4);
    }

    #[test]
    fn prune_if_due_is_gated_and_safe_under_concurrency() {
        let (_dir, sink) = temp_sink("prune-due");
        let now = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp()
            .unsigned_abs();
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let sink = Arc::clone(&sink);
                std::thread::spawn(move || {
                    if let Some(admission) = sink.try_admit_prune_at(now) {
                        sink.run_admitted_prune(admission);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(!sink.degraded());
        assert_eq!(sink.last_prune_unix.load(Ordering::Relaxed), now);
        assert_eq!(sink.next_prune_attempt_unix.load(Ordering::Acquire), 0);
    }

    #[test]
    fn prune_admission_rejects_overlap_and_releases_after_long_pass() {
        let (_dir, sink) = temp_sink("prune-admission");
        let first = sink.try_admit_prune_at(10_000).unwrap();

        assert!(sink.try_admit_prune_at(10_000).is_none());
        assert!(sink.prune_in_flight.load(Ordering::Acquire));

        drop(first);

        assert!(!sink.prune_in_flight.load(Ordering::Acquire));
        assert!(sink.try_admit_prune_at(10_900).is_some());
    }

    #[test]
    fn initial_prune_is_deferred_until_the_normal_interval() {
        let (_dir, sink) = temp_sink("prune-initial-deferred");
        let now = 30_000;

        sink.defer_initial_prune(now);

        assert!(sink.try_admit_prune_at(now).is_none());
        assert!(sink
            .try_admit_prune_at(now + PRUNE_INTERVAL.as_secs())
            .is_some());
    }

    #[test]
    fn prune_wait_uses_retry_deadline_instead_of_fixed_polling() {
        let (_dir, sink) = temp_sink("prune-deadline");
        let first = sink.try_admit_prune_at(20_000).unwrap();
        drop(first);
        sink.last_prune_unix.store(20_000, Ordering::Release);

        assert_eq!(sink.prune_wait_duration_at(20_000), PRUNE_INTERVAL);

        sink.schedule_prune_retry(20_000);
        assert_eq!(sink.prune_wait_duration_at(20_000), PRUNE_RETRY_INITIAL);
        assert_eq!(sink.prune_wait_duration_at(20_014), Duration::from_secs(1));
    }

    #[test]
    fn prune_if_due_does_not_retry_post_commit_reporting_failure() {
        let (_dir, sink) = temp_sink("prune-retry");
        sink.fail_next_prune_accounting();
        let now = 10_000;

        sink.prune_if_due_at(now);

        assert_eq!(sink.last_prune_unix.load(Ordering::Relaxed), now);
        assert_eq!(sink.next_prune_attempt_unix.load(Ordering::Acquire), 0);
        assert!(sink.degraded());
        assert!(sink
            .forensic_failures()
            .iter()
            .any(|entry| entry.contains("store.accounting")));

        let probe = Store::open(sink.store.path()).unwrap();
        let lease = probe
            .try_acquire_retention_lease("probe-owner-1", PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        probe.release_retention_lease(&lease).unwrap();
    }

    #[test]
    fn prune_if_due_retry_backoff_is_bounded() {
        let (_dir, sink) = temp_sink("prune-retry-bound");
        let mut now = 20_000;

        for _ in 0..16 {
            sink.fail_next_prune_store(ExitClass::Operation, "test.injected.pre-commit");
            sink.prune_if_due_at(now);
            let retry_at = sink.next_prune_attempt_unix.load(Ordering::Acquire);
            assert!(retry_at.saturating_sub(now) <= PRUNE_RETRY_MAX.as_secs());
            now = retry_at;
        }

        assert_eq!(
            sink.prune_retry_delay_secs.load(Ordering::Relaxed),
            PRUNE_RETRY_MAX.as_secs()
        );
    }

    #[test]
    fn runtime_prune_lease_is_released_after_success_and_has_unique_owner() {
        let (dir, first) = temp_sink("runtime-lease-success");
        let second =
            Arc::new(OpsSink::open(dir.join("state.db"), "second-instance".to_owned()).unwrap());

        assert_ne!(first.retention_owner, second.retention_owner);
        first.prune_if_due_at(30_000);

        let probe = Store::open(first.store.path()).unwrap();
        let lease = probe
            .try_acquire_retention_lease("probe-owner-1", PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        probe.release_retention_lease(&lease).unwrap();
        assert!(!second.degraded());
    }

    #[test]
    fn runtime_prune_lease_blocks_competing_sink_with_bounded_retry() {
        let (dir, first) = temp_sink("runtime-lease-competing");
        let second =
            Arc::new(OpsSink::open(dir.join("state.db"), "second-instance".to_owned()).unwrap());
        let lease = first
            .store
            .try_acquire_retention_lease(&first.retention_owner, PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();

        second.prune_if_due_at(40_000);

        assert!(!second.degraded());
        assert_eq!(
            second.next_prune_attempt_unix.load(Ordering::Acquire),
            40_000 + PRUNE_RETRY_INITIAL.as_secs()
        );
        first.store.release_retention_lease(&lease).unwrap();
    }

    #[test]
    fn runtime_prune_releases_lease_after_pre_commit_error() {
        let (_dir, sink) = temp_sink("runtime-lease-pre-commit");
        sink.fail_next_prune_store(ExitClass::Operation, "test.injected.pre-commit");

        sink.prune_if_due_at(50_000);

        assert_eq!(
            sink.next_prune_attempt_unix.load(Ordering::Acquire),
            50_000 + 15
        );
        let probe = Store::open(sink.store.path()).unwrap();
        let lease = probe
            .try_acquire_retention_lease("probe-owner-1", PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        probe.release_retention_lease(&lease).unwrap();
    }

    #[test]
    fn runtime_lease_guard_releases_on_panic_and_drop() {
        let (dir, sink) = temp_sink("runtime-lease-guard");
        let lease = sink
            .store
            .try_acquire_retention_lease(&sink.retention_owner, PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = RetentionLeaseGuard::new(&sink.store, lease);
            panic!("test cancellation/panic");
        }));
        assert!(panic_result.is_err());

        let probe = Store::open(dir.join("state.db")).unwrap();
        let replacement = probe
            .try_acquire_retention_lease("probe-owner-1", PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        probe.release_retention_lease(&replacement).unwrap();

        let lease = sink
            .store
            .try_acquire_retention_lease(&sink.retention_owner, PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        drop(RetentionLeaseGuard::new(&sink.store, lease));
        let replacement = probe
            .try_acquire_retention_lease("probe-owner-2", PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        probe.release_retention_lease(&replacement).unwrap();
    }

    #[test]
    fn lease_finalization_failure_emits_bounded_sanitized_telemetry() {
        let (dir, sink) = temp_sink("runtime-lease-finalization-telemetry");
        let lease = sink
            .store
            .try_acquire_retention_lease(&sink.retention_owner, PRUNE_LEASE_DURATION)
            .unwrap()
            .unwrap();
        let guard = RetentionLeaseGuard::with_sink(&sink, lease);
        std::fs::remove_dir_all(&dir).unwrap();

        drop(guard);

        let lines = sink.forensic_failures();
        assert!(lines.iter().any(|line| {
            line.contains("store.prune-lease-release")
                && line.contains("bounded finalization attempt failed")
                && !line.contains('\n')
        }));
    }

    #[test]
    fn worker_failure_retry_uses_completion_time_and_sanitizes_detail() {
        let (_dir, sink) = temp_sink("runtime-worker-failure");
        let completion = 60_000;
        sink.record_prune_worker_failure(completion, "worker\nfailed\ttoken=secret");

        assert_eq!(
            sink.next_prune_attempt_unix.load(Ordering::Acquire),
            completion + PRUNE_RETRY_INITIAL.as_secs()
        );
        let lines = sink.forensic_failures();
        assert!(lines.iter().any(|line| {
            line.contains("store.prune-worker")
                && line.contains("redacted-sensitive-diagnostic")
                && !line.contains('\n')
        }));
    }

    #[test]
    fn forensic_failure_line_rejects_control_and_sensitive_fields() {
        let line = forensic_failure_line("store.test\ncode", "first\nsecond\ttoken=do-not-log");
        assert!(!line.contains('\n'));
        assert!(line.contains("redacted-sensitive-diagnostic"));
        assert!(!line.contains("do-not-log"));
    }

    #[test]
    fn sanitize_slug_projects_charset_and_caps_length() {
        assert_eq!(sanitize_slug("Deploy Prod"), "Deploy-Prod");
        // Slug-charset characters pass through verbatim, including repeats.
        assert_eq!(sanitize_slug("a//b__c"), "a//b__c");
        assert_eq!(sanitize_slug("!!!"), "unknown");
        let long = sanitize_slug(&"x".repeat(600));
        assert!(long.len() <= 512);
    }
}
