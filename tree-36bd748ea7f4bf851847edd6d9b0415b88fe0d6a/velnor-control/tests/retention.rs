//! Bounded-retention contract tests (Plan 066 step 5).

use std::time::Duration;

use velnor_control::store::{
    EventRow, InstanceRow, JobRow, RetentionBudget, SlotRow, Store, StoreAccounting,
};
use velnor_model::Timestamp;

struct TempDb {
    dir: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let nanos = Timestamp::now()
            .as_offset_datetime()
            .unix_timestamp_nanos()
            .unsigned_abs();
        let dir = std::env::temp_dir().join(format!("velnor-retention-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("temp dir created");
        let path = dir.join("state.db");
        Self { dir, path }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn aged(seconds_back: u64) -> Timestamp {
    Timestamp::now().minus(Duration::from_secs(seconds_back))
}

fn terminal_job(slug: &str, uid: &str, seconds_back: u64) -> JobRow {
    // Deterministic unique identity per uid so the (instance, run, attempt)
    // partial-unique index never collides across helper calls.
    let run_id = 1_000_000
        + u64::from(uid.len() as u8) * 65536
        + uid
            .bytes()
            .rev()
            .enumerate()
            .map(|(i, b)| u64::from(b) << (i * 5))
            .sum::<u64>();
    JobRow {
        instance_slug: slug.to_owned(),
        job_uid: uid.to_owned(),
        repository: "tailrocks/velnor-actions-fixture".to_owned(),
        workflow: ".github/workflows/control-plane.yml".to_owned(),
        job_name: "hold".to_owned(),
        run_id: Some(i64::try_from(run_id).unwrap()),
        attempt: Some(1),
        head_ref: None,
        head_sha: None,
        trigger_event: Some("workflow_dispatch".to_owned()),
        queued_at: Some(aged(seconds_back)),
        acquired_at: Some(aged(seconds_back)),
        slot_name: Some("slot-0".to_owned()),
        runner_name: Some("fixture-runner-0".to_owned()),
        trust_scope: Some("trusted".to_owned()),
        resource_policy: None,
        phase: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
        infrastructure_category: None,
        updated_at: aged(seconds_back),
    }
}

fn active_job(slug: &str, uid: &str) -> JobRow {
    let mut row = terminal_job(slug, uid, 0);
    row.phase = "started".to_owned();
    row.updated_at = Timestamp::now();
    row
}

fn event(slug: &str, subject: &str, kind: &str, seconds_back: u64) -> EventRow {
    EventRow {
        instance_slug: slug.to_owned(),
        event_kind: kind.to_owned(),
        subject: subject.to_owned(),
        correlation_id: None,
        occurred_at: aged(seconds_back),
        detail: None,
    }
}

fn tiny_budget() -> RetentionBudget {
    RetentionBudget {
        max_event_age: Some(Duration::from_secs(60)),
        max_event_rows: 10_000,
        max_terminal_job_age: Some(Duration::from_secs(60)),
        max_terminal_job_rows: 10_000,
        // Disabled byte ceiling unless a test opts in explicitly.
        max_database_bytes: 0,
        batch_size: 2,
    }
}

fn job_exists(store: &Store, instance_slug: &str, job_uid: &str) -> bool {
    store
        .job_summaries(instance_slug)
        .unwrap()
        .iter()
        .any(|row| row.job_uid == job_uid)
}

fn terminal_job_count(store: &Store, instance_slug: &str, suffix: &str) -> usize {
    store
        .job_summaries(instance_slug)
        .unwrap()
        .iter()
        .filter(|row| row.job_uid.ends_with(suffix))
        .count()
}

fn total_jobs(store: &Store) -> u64 {
    store.accounting().unwrap().job_rows
}

fn total_events(store: &Store) -> u64 {
    store.accounting().unwrap().event_rows
}

#[test]
fn aged_terminal_rows_go_while_active_state_and_ancestry_survive() {
    let temp = TempDb::new("age-boundaries");
    let store = Store::open(&temp.path).unwrap();

    store
        .upsert_instance(&InstanceRow {
            instance_slug: "it".to_owned(),
            host: "sentry".to_owned(),
            daemon_version: "0.1.0".to_owned(),
            slots_configured: 1,
            slots_busy: 1,
            updated_at: Timestamp::now(),
        })
        .unwrap();
    store
        .upsert_slot(&SlotRow {
            instance_slug: "it".to_owned(),
            name: "slot-1".to_owned(),
            host: "sentry".to_owned(),
            slot_index: 1,
            slot_kind: "stable".to_owned(),
            phase: "busy".to_owned(),
            job_name: Some("active-job".to_owned()),
            updated_at: Timestamp::now(),
        })
        .unwrap();

    store
        .record_job(&terminal_job("it", "old-done", 3600))
        .unwrap();
    store.record_job(&active_job("it", "active-job")).unwrap();
    store
        .append_event(&event(
            "it",
            "old-done",
            "job.transition.job.completed",
            3600,
        ))
        .unwrap();
    store
        .append_event(&event(
            "it",
            "active-job",
            "job.transition.job.started",
            3600,
        ))
        .unwrap();
    store
        .append_event(&event("it", "active-job", "slot.state_changed", 0))
        .unwrap();

    let report = store.prune_history(&tiny_budget()).unwrap();

    assert_eq!(report.deleted_jobs, 1);
    assert!(report.deleted_events >= 1);
    // The active job survives with every one of its events, including the
    // ancient one — ancestry of live state is never removed.
    assert!(job_exists(&store, "it", "active-job"));
    assert_eq!(store.event_count("it", "active-job").unwrap(), 2);
    // Terminal job ancestry went with it.
    assert!(!job_exists(&store, "it", "old-done"));
    assert_eq!(store.event_count("it", "old-done").unwrap(), 0);

    // Current instance/slot state is untouchable.
    assert_eq!(total_jobs(&store), 1);
    assert_eq!(total_events(&store), 2);

    let accounting: StoreAccounting = store.accounting().unwrap();
    assert!(accounting.last_prune_at.is_some());
    assert_eq!(accounting.last_deleted_jobs, 1);
}

#[test]
fn row_caps_keep_the_newest_generation() {
    let temp = TempDb::new("row-caps");
    let store = Store::open(&temp.path).unwrap();
    // Fresh terminal jobs: the age rule stays inert so this test isolates
    // pure row-cap semantics.
    for index in 0..6 {
        let mut row = terminal_job("rc", &format!("done-{index}"), 3600);
        row.updated_at = Timestamp::now();
        store.record_job(&row).unwrap();
        let mut noise = event("rc", &format!("noise-{index}"), "gc.completed", 3600);
        noise.occurred_at = Timestamp::now();
        store.append_event(&noise).unwrap();
    }
    let mut budget = tiny_budget();
    budget.max_terminal_job_age = None;
    budget.max_event_age = None;
    budget.max_terminal_job_rows = 3;
    budget.max_event_rows = 3;
    store.prune_history(&budget).unwrap();

    assert_eq!(total_jobs(&store), 3);
    assert_eq!(total_events(&store), 3);
    // Newest survive: done-3..5 stay, done-0..2 go.
    assert_eq!(
        terminal_job_count(&store, "rc", "done-0")
            + terminal_job_count(&store, "rc", "done-1")
            + terminal_job_count(&store, "rc", "done-2"),
        0
    );
    assert_eq!(
        terminal_job_count(&store, "rc", "done-3")
            + terminal_job_count(&store, "rc", "done-4")
            + terminal_job_count(&store, "rc", "done-5"),
        3
    );

    // Idempotent: a second identical pass deletes nothing further.
    let again = store.prune_history(&budget).unwrap();
    assert_eq!(again.deleted_jobs, 0);
    assert_eq!(again.deleted_events, 0);
    assert_eq!(total_jobs(&store), 3);
}

#[test]
fn byte_ceiling_prunes_until_under_budget_or_exhausted() {
    let temp = TempDb::new("byte-ceiling");
    let store = Store::open(&temp.path).unwrap();
    for index in 0..40 {
        store
            .record_job(&terminal_job("bc", &format!("bulk-{index}"), 3600))
            .unwrap();
        store
            .append_event(&event(
                "bc",
                &format!("orphan-{index}"),
                "gc.completed",
                3600,
            ))
            .unwrap();
    }

    let mut budget = tiny_budget();
    budget.max_event_age = None;
    budget.max_terminal_job_age = None;
    budget.max_database_bytes = 1;
    let mut report = store.prune_history(&budget).unwrap();
    // A pass has a fixed transaction budget. A later pass converges the
    // remaining backlog without extending one writer lock indefinitely.
    let mut deleted_jobs = report.deleted_jobs;
    let mut deleted_events = report.deleted_events;
    while total_jobs(&store) > 0 || total_events(&store) > 0 {
        report = store.prune_history(&budget).unwrap();
        deleted_jobs += report.deleted_jobs;
        deleted_events += report.deleted_events;
    }
    // SQLite cannot shrink below one page, so this is deterministic exhaustion
    // behavior: every prunable terminal job is removed, then the byte ceiling
    // remains unsatisfied because no rows remain to delete.
    assert_eq!(deleted_jobs, 40);
    assert!(deleted_events >= 40);
    assert_eq!(total_jobs(&store), 0);
    assert!(report.database_bytes > budget.max_database_bytes);

    // Ceiling already satisfied: nothing is deleted.
    let before = store.accounting().unwrap().job_rows;
    let mut satisfied = tiny_budget();
    satisfied.max_database_bytes = u64::MAX;
    let idle = store.prune_history(&satisfied).unwrap();
    assert_eq!(idle.deleted_jobs, 0);
    assert_eq!(store.accounting().unwrap().job_rows, before);
}

#[test]
fn age_pruning_deletes_expired_prefix_before_fresh_rows() {
    let temp = TempDb::new("age-prefix");
    let store = Store::open(&temp.path).unwrap();
    store.record_job(&terminal_job("age", "fresh", 0)).unwrap();
    store.record_job(&terminal_job("age", "old", 3600)).unwrap();
    store
        .append_event(&event("age", "fresh-event", "gc.completed", 0))
        .unwrap();
    store
        .append_event(&event("age", "old-event", "gc.completed", 3600))
        .unwrap();

    let report = store.prune_history(&tiny_budget()).unwrap();

    assert_eq!(report.deleted_jobs, 1);
    assert_eq!(report.deleted_events, 1);
    assert!(!job_exists(&store, "age", "old"));
    assert!(job_exists(&store, "age", "fresh"));
    assert_eq!(store.event_count("age", "old-event").unwrap(), 0);
    assert_eq!(store.event_count("age", "fresh-event").unwrap(), 1);
}

#[test]
fn age_pruning_skips_protected_events_without_blocking_unprotected_rows() {
    let temp = TempDb::new("age-protected");
    let store = Store::open(&temp.path).unwrap();
    store
        .record_job(&active_job("protected", "active"))
        .unwrap();
    store
        .append_event(&event("protected", "active", "job.started", 3600))
        .unwrap();
    store
        .append_event(&event("protected", "expired", "gc.completed", 3600))
        .unwrap();
    store
        .append_event(&event("protected", "fresh", "gc.completed", 0))
        .unwrap();

    let report = store.prune_history(&tiny_budget()).unwrap();

    assert_eq!(report.deleted_events, 1);
    assert_eq!(store.event_count("protected", "active").unwrap(), 1);
    assert_eq!(store.event_count("protected", "expired").unwrap(), 0);
    assert_eq!(store.event_count("protected", "fresh").unwrap(), 1);
    assert!(job_exists(&store, "protected", "active"));
}

#[test]
fn open_reconciliation_protects_instance_events_until_closed() {
    let temp = TempDb::new("reconciliation-protection");
    let store = Store::open(&temp.path).unwrap();
    store
        .append_event(&event("reconcile", "host", "gc.completed", 3600))
        .unwrap();
    let reconciliation = store
        .start_reconciliation("reconcile", "runner-registration", "host", Timestamp::now())
        .unwrap();
    let budget = RetentionBudget {
        max_event_age: Some(Duration::from_secs(60)),
        max_event_rows: 0,
        max_terminal_job_age: None,
        max_terminal_job_rows: 0,
        max_database_bytes: 0,
        batch_size: 1,
    };

    assert_eq!(store.prune_history(&budget).unwrap().deleted_events, 0);
    assert_eq!(total_events(&store), 1);

    store
        .finish_reconciliation(
            "reconcile",
            reconciliation,
            "completed",
            Timestamp::now(),
            None,
        )
        .unwrap();
    assert_eq!(store.prune_history(&budget).unwrap().deleted_events, 1);
    assert_eq!(total_events(&store), 0);
}

#[test]
fn reopen_after_prune_stays_consistent_and_schema_current() {
    let temp = TempDb::new("reopen");
    let store = Store::open(&temp.path).unwrap();
    store.record_job(&terminal_job("ro", "gone", 3600)).unwrap();
    store.prune_history(&tiny_budget()).unwrap();
    drop(store);

    let reopened = Store::open(&temp.path).unwrap();
    assert_eq!(
        reopened.schema_version().unwrap(),
        velnor_control::store::LATEST_SCHEMA_VERSION
    );
    assert_eq!(total_jobs(&reopened), 0);
    let accounting = reopened.accounting().unwrap();
    assert!(accounting.last_prune_at.is_some());
}

#[test]
fn summary_replay_never_regresses_machine_phase() {
    use velnor_control::store::Transition;
    use velnor_model::{
        EventReason, JobSummary as ModelJobSummary, NormalizedJob, RepositoryRef, Slug, Timestamp,
        TriggerEvent,
    };
    let temp = TempDb::new("replay-phase");
    let store = Store::open(&temp.path).unwrap();

    let summary = |phase: velnor_model::JobPhase| {
        ModelJobSummary::from_normalized(NormalizedJob {
            instance_slug: "rp".to_owned(),
            job_uid: "summary-run-77-attempt-1".to_owned(),
            repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
            workflow: "control-plane".to_owned(),
            job_name: "hold".to_owned(),
            run_id: Some(77),
            attempt: Some(1),
            head_ref: None,
            head_sha: None,
            trigger_event: Some(TriggerEvent::WorkflowDispatch),
            queued_at: None,
            acquired_at: Some(Timestamp::now()),
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("fixture-runner-0".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            resource_policy: None,
            phase,
            conclusion: None,
            infrastructure_category: None,
        })
        .unwrap()
    };

    // Admission (queued), then the machine advances to acquired.
    store
        .persist_summary(&summary(velnor_model::JobPhase::Queued))
        .unwrap();
    let uid = "summary-run-77-attempt-1";
    let correlation = Slug::validate("correlation_id", "corr-t-acquired").unwrap();
    store
        .record_job_transition(
            "rp",
            uid,
            &Transition {
                token: "t-acquired".to_owned(),
                correlation_id: correlation,
                reason: EventReason::JobAcquired,
                message: None,
                transition_time: Timestamp::now(),
                conclusion: None,
                infrastructure_category: None,
            },
        )
        .unwrap();

    // Duplicate delivery replays the admission summary; the machine state
    // must survive untouched instead of regressing to queued.
    store
        .persist_summary(&summary(velnor_model::JobPhase::Queued))
        .unwrap();
    let stored = store.fetch_summary("rp", 77, 1).unwrap().unwrap();
    assert_eq!(stored.phase(), velnor_model::JobPhase::Running);
}
