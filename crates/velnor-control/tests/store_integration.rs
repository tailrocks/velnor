//! External-contract tests for the durable operational store.

use std::path::Path;
use std::time::Duration;

use velnor_control::store::{
    EventRow, InstanceRow, JobRow, OpenOptions, Store, Transition, LATEST_SCHEMA_VERSION,
};
use velnor_model::{ExitClass, Timestamp};

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
        let dir = std::env::temp_dir().join(format!("velnor-store-it-{label}-{nanos}"));
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

fn instance(slug: &str) -> InstanceRow {
    InstanceRow {
        instance_slug: slug.to_owned(),
        host: "sentry".to_owned(),
        daemon_version: "0.1.0".to_owned(),
        slots_configured: 2,
        slots_busy: 0,
        updated_at: Timestamp::now(),
    }
}

fn job(slug: &str, uid: &str) -> JobRow {
    JobRow {
        instance_slug: slug.to_owned(),
        job_uid: uid.to_owned(),
        repository: "tailrocks/velnor-actions-fixture".to_owned(),
        workflow: ".github/workflows/control-plane.yml".to_owned(),
        job_name: "hold".to_owned(),
        run_id: Some(9),
        attempt: Some(1),
        head_ref: Some("velnor-estate-standard".to_owned()),
        head_sha: Some("deadbeef".to_owned()),
        trigger_event: Some("workflow_dispatch".to_owned()),
        queued_at: Some(Timestamp::UNIX_EPOCH),
        acquired_at: None,
        runner_name: Some("fixture-runner-0".to_owned()),
        trust_scope: Some("trusted".to_owned()),
        resource_policy: Some("standard".to_owned()),
        phase: "queued".to_owned(),
        conclusion: None,
        infrastructure_category: None,
        updated_at: Timestamp::now(),
    }
}

fn transition(token: &str, reason: &str, phase: &str, conclusion: Option<&str>) -> Transition {
    Transition {
        token: token.to_owned(),
        correlation_id: format!("corr-{token}"),
        reason: reason.to_owned(),
        message: Some(format!("{reason} observed")),
        transition_time: Timestamp::now(),
        next_phase: phase.to_owned(),
        conclusion: conclusion.map(str::to_owned),
        infrastructure_category: None,
    }
}

#[test]
fn round_trip_summary_and_atomic_transition() {
    let temp = TempDb::new("round-trip");
    let store = Store::open(&temp.path).expect("open");
    assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);

    store.upsert_instance(&instance("it")).unwrap();
    store.record_job(&job("it", "hold-job")).unwrap();
    assert!(store
        .record_job_transition(
            "it",
            "hold-job",
            &transition("t-acquire", "job.acquired", "running", None)
        )
        .unwrap());

    let summary = &store.job_summaries("it").unwrap()[0];
    assert_eq!(summary.phase, "running");
    assert_eq!(summary.repository, "tailrocks/velnor-actions-fixture");
    assert_eq!(summary.run_id, Some(9));
    assert_eq!(summary.trust_scope.as_deref(), Some("trusted"));
    assert_eq!(store.transition_count("it", "hold-job").unwrap(), 1);
    assert_eq!(store.event_count("it", "hold-job").unwrap(), 1);

    // Reopen proves WAL persistence across process-style boundaries.
    drop(store);
    let reopened = Store::open(&temp.path).unwrap();
    assert_eq!(reopened.job_summaries("it").unwrap()[0].phase, "running");
}

#[test]
fn transition_replay_is_idempotent_noop() {
    let temp = TempDb::new("replay");
    let store = Store::open(&temp.path).unwrap();
    store.upsert_instance(&instance("rp")).unwrap();
    store.record_job(&job("rp", "j")).unwrap();

    let apply = |conclusion: Option<&str>| {
        store.record_job_transition(
            "rp",
            "j",
            &transition("t-final", "job.completed", "completed", conclusion),
        )
    };
    assert!(apply(Some("success")).unwrap());
    assert!(
        !apply(Some("success")).unwrap(),
        "replayed token is a no-op"
    );
    assert!(!apply(Some("failure")).unwrap(), "token wins over payload");

    let summary = &store.job_summaries("rp").unwrap()[0];
    assert_eq!(summary.phase, "completed");
    assert_eq!(summary.conclusion.as_deref(), Some("success"));
    assert_eq!(store.transition_count("rp", "j").unwrap(), 1);
    assert_eq!(store.event_count("rp", "j").unwrap(), 1);
}

#[test]
fn unknown_job_transition_fails_unavailable_and_writes_nothing() {
    let temp = TempDb::new("unknown-job");
    let store = Store::open(&temp.path).unwrap();
    store.upsert_instance(&instance("uj")).unwrap();
    let error = store
        .record_job_transition(
            "uj",
            "absent",
            &transition("t1", "job.acquired", "running", None),
        )
        .expect_err("unknown job rejected");
    assert_eq!(error.envelope.class, ExitClass::Unavailable.as_str());
    assert_eq!(error.envelope.reason, "store.job.missing");
    assert_eq!(store.event_count("uj", "absent").unwrap(), 0);
}

#[test]
fn missing_parent_directory_names_exact_path_and_never_creates_it() {
    let temp = TempDb::new("parent-missing");
    let nested = temp.dir.join("unprovisioned").join("state.db");
    let error = Store::open(&nested).expect_err("missing parent fails closed");
    assert_eq!(error.envelope.class, ExitClass::Unavailable.as_str());
    assert_eq!(error.envelope.reason, "store.parent.missing");
    let remediation = error.envelope.remediation.expect("names the path");
    assert!(
        remediation.contains(nested.parent().unwrap().to_string_lossy().as_ref()),
        "{remediation}"
    );
    assert!(!nested.parent().unwrap().exists());
}

#[test]
fn wal_mode_persists_on_reopened_file() {
    let temp = TempDb::new("wal");
    drop(Store::open(&temp.path).unwrap());
    let conn = rusqlite_connection(&temp.path);
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
}

fn rusqlite_connection(path: &Path) -> rusqlite::Connection {
    use rusqlite::Connection;
    Connection::open(path).expect("raw connection for pragma inspection")
}

#[test]
fn short_lock_wait_option_is_honored_on_contention_free_open() {
    // Contention-free open with a tiny wait still migrates and succeeds.
    let temp = TempDb::new("options");
    let store = Store::open_with(
        &temp.path,
        OpenOptions {
            migration_lock_wait: Duration::from_millis(50),
        },
    )
    .expect("tiny wait suffices without contention");
    assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
    store
        .append_event(&EventRow {
            instance_slug: "opt".to_owned(),
            event_kind: "daemon.ready".to_owned(),
            subject: "opt".to_owned(),
            correlation_id: None,
            occurred_at: Timestamp::now(),
            detail: None,
        })
        .unwrap();
    assert_eq!(store.event_count("opt", "opt").unwrap(), 1);
}
