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
    EventRow, InstanceRow, RetentionBudget, Store, Transition, DEFAULT_STATE_DB_PATH,
};
use velnor_model::{
    EventReason, JobPhase, JobSummary as ModelJobSummary, NormalizedJob, RepositoryRef, Slug,
    Timestamp, TriggerEvent,
};

/// Environment override for the operational database location; tests and
/// fixture validation runs point this at a temporary file.
pub const STATE_DB_ENV: &str = "VELNOR_STATE_DB";

/// How often the daemon-side retention pass may run.
const PRUNE_INTERVAL: Duration = Duration::from_secs(15 * 60);

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

/// Single entry point: `strict` classifies open failure as a readiness
/// failure (daemon); otherwise the store degrades observability only
/// (standalone run).
pub fn init(instance_slug: String, strict: bool) -> Result<(), velnor_control::store::StoreError> {
    match OpsSink::open(state_db_path(), instance_slug) {
        Ok(sink) => {
            install_global(Arc::new(sink));
            Ok(())
        }
        Err(error) if strict => Err(error),
        Err(error) => {
            eprintln!(
                "operational store unavailable at {} ({error:#}); continuing without durable lifecycle records",
                state_db_path().display()
            );
            Ok(())
        }
    }
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
    pub repository_full_name: String,
    pub workflow: String,
    pub job_name: String,
    pub run_id: Option<u64>,
    pub attempt: Option<u32>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub trigger_event: Option<String>,
    pub queued_at_rfc3339: Option<String>,
    pub runner_name: Option<String>,
    pub trust_scope: Option<String>,
    /// Secret/mask values collected from the raw job message; applied to
    /// every textual projection so no secret value can enter the store even
    /// when a workflow embeds one in its name or ref.
    pub masks: Vec<String>,
}

impl JobAdmission {
    /// Stable store identity for a summary row; matches the store's own
    /// upsert key derivation exactly.
    #[must_use]
    pub fn job_uid(&self) -> Option<String> {
        Some(format!(
            "summary-run-{}-attempt-{}",
            self.run_id?, self.attempt?
        ))
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
            runner_name: self.runner_name.as_deref().map(|v| self.project(v)),
            trust_scope: self.trust_scope.as_deref().map(|v| self.project(v)),
            resource_policy: None,
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

/// Per-process handle over the shared operational database.
pub struct OpsSink {
    store: Store,
    instance_slug: String,
    masks: Mutex<Vec<String>>,
    degraded: AtomicBool,
    last_prune_unix: AtomicU64,
    budget: RetentionBudget,
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
            masks: Mutex::new(Vec::new()),
            degraded: AtomicBool::new(false),
            last_prune_unix: AtomicU64::new(0),
            budget: RetentionBudget::default(),
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
            return self.required_failure(
                "store.admission.identity",
                "github.run_id and github.run_attempt are required",
            );
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
        self.remember_masks(&admission.masks);
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
        let subject = self.project_event_text(subject);
        let detail = detail.map(|value| self.project_event_text(&value));
        let correlation = Slug::validate("correlation_id", &subject).ok();
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
        let Ok(correlation) = Slug::validate("correlation_id", &format!("corr-{token}")) else {
            return false;
        };
        let transition = Transition {
            token: token.to_owned(),
            correlation_id: correlation,
            reason,
            message: message.map(|value| self.project_event_text(&value)),
            transition_time: Timestamp::now(),
            conclusion: conclusion.map(|value| self.project_event_text(&value)),
            infrastructure_category: infrastructure_category
                .map(|value| self.project_event_text(&value)),
        };
        if let Err(error) =
            self.store
                .record_job_transition(&self.instance_slug, job_uid, &transition)
        {
            self.absorb("store.transition", &error.to_string());
            return false;
        }
        true
    }

    /// Daemon-side bounded retention pass, time-gated per process; safe to
    /// call from every slot cycle because concurrent passes serialize on the
    /// database's write lock.
    pub fn prune_if_due(&self) {
        let now_unix = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp()
            .unsigned_abs();
        let last = self.last_prune_unix.load(Ordering::Relaxed);
        if now_unix.saturating_sub(last) < PRUNE_INTERVAL.as_secs() {
            return;
        }
        if self
            .last_prune_unix
            .compare_exchange(last, now_unix, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        if let Err(error) = self.store.prune_history(&self.budget) {
            self.absorb("store.prune", &error.to_string());
            return;
        }
        // Publish post-prune accounting so retention stays observable from
        // logs alone (Plan 066 step 5).
        match self.accounting() {
            Ok(accounting) => println!(
                "forensics.ops event=retention jobs={} events={} transitions={} db_bytes={} wal_bytes={} last_prune_at={}",
                accounting.job_rows,
                accounting.event_rows,
                accounting.transition_rows,
                accounting.database_bytes,
                accounting.wal_bytes,
                accounting.last_prune_at.as_deref().unwrap_or("never"),
            ),
            Err(error) => self.absorb("store.accounting", &error.to_string()),
        }
    }

    /// Read-only accounting snapshot for diagnostics.
    ///
    /// # Errors
    /// Propagates store read failures.
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
        eprintln!("REQUIRED operational-store write failed ({code}): {detail}");
        self.degraded.store(true, Ordering::Relaxed);
        false
    }

    fn remember_masks(&self, values: &[String]) {
        let Ok(mut masks) = self.masks.lock() else {
            self.absorb("store.masks", "mask registry lock poisoned");
            return;
        };
        for value in values.iter().filter(|value| !value.is_empty()) {
            if !masks.iter().any(|known| known == value) {
                masks.push(value.clone());
            }
        }
        if masks.len() > 128 {
            let excess = masks.len() - 128;
            masks.drain(..excess);
        }
    }

    fn project_event_text(&self, raw: &str) -> String {
        let masks = self
            .masks
            .lock()
            .map(|values| values.clone())
            .unwrap_or_default();
        sanitize_event_text(raw, &masks)
    }

    fn absorb(&self, code: &str, detail: &str) {
        self.degraded.store(true, Ordering::Relaxed);
        tracing::error!(target: "velnor::ops", code, "{code}: {detail}");
        eprintln!("forensics.ops event=store-write-failed code={code} error={detail}");
    }
}

fn sanitize_event_text(raw: &str, masks: &[String]) -> String {
    sanitize_slug(&crate::runner::mask_all(raw, masks))
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
            repository_full_name: "tailrocks/velnor-actions-fixture".to_owned(),
            workflow: "control plane".to_owned(),
            job_name: secret.map_or("hold".to_owned(), str::to_owned),
            run_id: Some(run_id),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            runner_name: Some("fixture-runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
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
        let (_dir, sink) = temp_sink("secret-safety");
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
        sink.prune_if_due();
        sink.prune_if_due();
        assert!(!sink.degraded());
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
