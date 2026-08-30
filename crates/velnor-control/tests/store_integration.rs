//! External-contract tests for the durable operational store.

use std::path::Path;
use std::time::Duration;

use velnor_control::store::{
    EventRow, InstanceRow, JobRow, OpenOptions, SlotIdentity, SlotTransition, Store, Transition,
    LATEST_SCHEMA_VERSION,
};
use velnor_model::{
    EventReason, ExitClass, Generation, SlotId, SlotKind, SlotPhase, Slug, Timestamp,
};

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
        slot_name: Some("slot-0".to_owned()),
        runner_name: Some("fixture-runner-0".to_owned()),
        trust_scope: Some("trusted".to_owned()),
        resource_policy: Some("standard".to_owned()),
        phase: "queued".to_owned(),
        conclusion: None,
        infrastructure_category: None,
        updated_at: Timestamp::now(),
    }
}

fn transition(token: &str, reason: EventReason, conclusion: Option<&str>) -> Transition {
    Transition {
        token: token.to_owned(),
        correlation_id: Slug::validate("correlation_id", &format!("corr-{token}"))
            .expect("valid slug"),
        reason,
        message: Some(format!("{} observed", reason.as_str())),
        transition_time: Timestamp::now(),
        conclusion: conclusion.map(str::to_owned),
        infrastructure_category: None,
    }
}

fn slot_identity(instance_slug: &str) -> SlotIdentity {
    SlotIdentity {
        instance_slug: instance_slug.to_owned(),
        slot_id: SlotId("fixture-1".to_owned()),
        host: "sentry".to_owned(),
        slot_index: 0,
        slot_kind: SlotKind::Stable,
    }
}

fn slot_transition(generation: u64, sequence: u64, target: SlotPhase) -> SlotTransition {
    let token = format!("slot-g{generation}-s{sequence}-{}", target.as_str());
    SlotTransition {
        correlation_id: Slug::validate("correlation_id", &format!("corr-{token}"))
            .expect("valid slot correlation"),
        token,
        generation: Generation(generation),
        sequence,
        target,
        job_name: None,
        message: Some(format!("entered {}", target.as_str())),
        transition_time: Timestamp::now(),
    }
}

/// Walk one job through the legal prefix up to `started`.
fn walk_to_started(store: &Store, instance_slug: &str, job_uid: &str) {
    for (token, reason) in [
        ("t-acquire", EventReason::JobAcquired),
        ("t-wait", EventReason::JobWaiting),
        ("t-start", EventReason::JobStarted),
    ] {
        assert!(store
            .record_job_transition(instance_slug, job_uid, &transition(token, reason, None))
            .unwrap());
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
            &transition("t-acquire", EventReason::JobAcquired, None)
        )
        .unwrap());

    let summary = &store.job_summaries("it").unwrap()[0];
    assert_eq!(summary.phase, "acquired");
    assert_eq!(summary.repository, "tailrocks/velnor-actions-fixture");
    assert_eq!(summary.run_id, Some(9));
    assert_eq!(summary.trust_scope.as_deref(), Some("trusted"));
    assert_eq!(store.transition_count("it", "hold-job").unwrap(), 1);
    assert_eq!(store.event_count("it", "hold-job").unwrap(), 1);

    // Reopen proves WAL persistence across process-style boundaries.
    drop(store);
    let reopened = Store::open(&temp.path).unwrap();
    assert_eq!(reopened.job_summaries("it").unwrap()[0].phase, "acquired");
}

#[test]
fn transition_replay_is_idempotent_noop() {
    let temp = TempDb::new("replay");
    let store = Store::open(&temp.path).unwrap();
    store.upsert_instance(&instance("rp")).unwrap();
    store.record_job(&job("rp", "j")).unwrap();

    walk_to_started(&store, "rp", "j");
    let apply = |conclusion: Option<&str>| {
        store.record_job_transition(
            "rp",
            "j",
            &transition("t-final", EventReason::JobCompleted, conclusion),
        )
    };
    assert!(apply(Some("success")).unwrap());
    assert_eq!(store.job_summaries("rp").unwrap()[0].phase, "completed");

    assert!(
        !apply(Some("success")).unwrap(),
        "replayed token is a no-op"
    );
    assert!(!apply(Some("failure")).unwrap(), "token wins over payload");

    let summary = &store.job_summaries("rp").unwrap()[0];
    assert_eq!(summary.phase, "completed");
    assert_eq!(summary.conclusion.as_deref(), Some("success"));
    assert_eq!(store.transition_count("rp", "j").unwrap(), 4);
    assert_eq!(store.event_count("rp", "j").unwrap(), 4);
}

#[test]
fn slot_transitions_are_typed_fenced_correlated_and_idempotent() {
    let temp = TempDb::new("slot-machine");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("slots");

    assert!(store
        .record_slot_transition(&identity, &slot_transition(1, 1, SlotPhase::Running))
        .unwrap());
    let mut collision = slot_transition(1, 1, SlotPhase::Running);
    collision.message = Some("different intent".to_owned());
    let collision = store
        .record_slot_transition(&identity, &collision)
        .expect_err("equal sequence cannot change transition intent");
    assert_eq!(
        collision.envelope.reason,
        "store.slot.transition.idempotency_conflict"
    );
    let illegal = store
        .record_slot_transition(&identity, &slot_transition(1, 2, SlotPhase::Recycling))
        .expect_err("running cannot skip teardown");
    assert_eq!(illegal.envelope.reason, "store.slot.transition.illegal");
    assert_eq!(store.event_count("slots", "fixture-1").unwrap(), 1);

    for (sequence, phase) in [
        (2, SlotPhase::Teardown),
        (3, SlotPhase::Recycling),
        (4, SlotPhase::Idle),
    ] {
        assert!(store
            .record_slot_transition(&identity, &slot_transition(1, sequence, phase))
            .unwrap());
    }
    assert!(!store
        .record_slot_transition(&identity, &slot_transition(1, 2, SlotPhase::Teardown))
        .unwrap());

    let current = store.slot("slots", &identity.slot_id).unwrap().unwrap();
    assert_eq!(current.phase, SlotPhase::Idle);
    assert_eq!(current.generation, Generation(1));
    assert_eq!(current.transition_sequence, 4);
    assert_eq!(store.event_count("slots", "fixture-1").unwrap(), 4);
    let events = store.events_after("slots", 0, 10).unwrap();
    assert_eq!(events.last().unwrap().row.event_kind, "slot.state_changed");
    assert_eq!(
        events.last().unwrap().row.correlation_id.as_deref(),
        Some("corr-slot-g1-s4-idle")
    );
    assert!(events
        .last()
        .unwrap()
        .row
        .detail
        .as_deref()
        .unwrap()
        .starts_with("phase=idle;"));

    assert!(store
        .record_slot_transition(&identity, &slot_transition(2, 1, SlotPhase::Teardown))
        .unwrap());
    let mut moved_identity = identity.clone();
    moved_identity.host = "other-host".to_owned();
    let moved = store
        .record_slot_transition(
            &moved_identity,
            &slot_transition(2, 2, SlotPhase::Recycling),
        )
        .expect_err("stable identity cannot move under the same slot id");
    assert_eq!(moved.envelope.reason, "store.slot.identity.mismatch");
    let stale = store
        .record_slot_transition(&identity, &slot_transition(1, 5, SlotPhase::Recycling))
        .expect_err("prior generation is fenced");
    assert_eq!(stale.envelope.reason, "store.slot.generation.stale");
    assert_eq!(store.event_count("slots", "fixture-1").unwrap(), 5);
}

#[test]
fn slot_event_failure_rolls_back_current_state() {
    let temp = TempDb::new("slot-atomic-rollback");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("slot-rollback");
    store
        .record_slot_transition(&identity, &slot_transition(1, 1, SlotPhase::Running))
        .unwrap();
    let connection = rusqlite_connection(&temp.path);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_slot_event
             BEFORE INSERT ON events
             WHEN NEW.event_kind = 'slot.state_changed'
             BEGIN SELECT RAISE(ABORT, 'injected slot event failure'); END;",
        )
        .unwrap();

    store
        .record_slot_transition(&identity, &slot_transition(1, 2, SlotPhase::Teardown))
        .expect_err("event failure must abort the transaction");
    let current = store
        .slot("slot-rollback", &identity.slot_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.phase, SlotPhase::Running);
    assert_eq!(current.transition_sequence, 1);
    assert_eq!(store.event_count("slot-rollback", "fixture-1").unwrap(), 1);
}

#[test]
fn generic_slot_event_cannot_bypass_atomic_transition() {
    let temp = TempDb::new("slot-generic-bypass");
    let store = Store::open(&temp.path).unwrap();
    let error = store
        .append_event(&EventRow {
            instance_slug: "slot-bypass".to_owned(),
            event_kind: EventReason::SlotStateChanged.as_str().to_owned(),
            subject: "fixture-1".to_owned(),
            correlation_id: Some("corr-bypass".to_owned()),
            occurred_at: Timestamp::now(),
            detail: Some("phase=recycling".to_owned()),
        })
        .expect_err("slot state events require the atomic typed API");
    assert_eq!(
        error.envelope.reason,
        "store.slot.event.requires_transition"
    );
    assert_eq!(store.event_count("slot-bypass", "fixture-1").unwrap(), 0);
    assert!(store
        .slot("slot-bypass", &SlotId("fixture-1".to_owned()))
        .unwrap()
        .is_none());
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
            &transition("t1", EventReason::JobAcquired, None),
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
