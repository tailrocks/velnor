//! External-contract tests for the durable operational store.

use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use velnor_control::store::{
    EventRow, InstanceRow, JobRow, OpenOptions, SlotIdentity, SlotTransition,
    SlotTransitionRequest, Store, Transition, LATEST_SCHEMA_VERSION, SLOT_TRANSITION_REQUEST_CAP,
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

fn next_slot_request(
    request_key: &str,
    generation: u64,
    target: SlotPhase,
    job_name: Option<&str>,
    message: Option<&str>,
) -> SlotTransitionRequest {
    SlotTransitionRequest {
        request_key: request_key.to_owned(),
        generation: Generation(generation),
        target,
        job_name: job_name.map(str::to_owned),
        message: message.map(str::to_owned),
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
fn slot_transition_request_migration_has_complete_schema() {
    let temp = TempDb::new("slot-transition-request-schema");
    let store = Store::open(&temp.path).expect("open migrates");
    assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
    let connection = rusqlite_connection(&temp.path);
    let columns: Vec<(String, String, bool, Option<String>, i64)> = connection
        .prepare(
            "SELECT name, type, \"notnull\", dflt_value, pk
             FROM pragma_table_info('slot_transition_requests')
             ORDER BY cid",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        columns,
        vec![
            ("instance_slug".to_owned(), "TEXT".to_owned(), true, None, 1),
            ("slot_id".to_owned(), "TEXT".to_owned(), true, None, 2),
            ("request_key".to_owned(), "TEXT".to_owned(), true, None, 3),
            ("generation".to_owned(), "INTEGER".to_owned(), true, None, 0),
            ("target".to_owned(), "TEXT".to_owned(), true, None, 0),
            ("job_name".to_owned(), "TEXT".to_owned(), false, None, 0),
            ("message".to_owned(), "TEXT".to_owned(), false, None, 0),
            (
                "allocated_sequence".to_owned(),
                "INTEGER".to_owned(),
                true,
                None,
                0,
            ),
            (
                "allocated_token".to_owned(),
                "TEXT".to_owned(),
                true,
                None,
                0,
            ),
            (
                "allocated_correlation_id".to_owned(),
                "TEXT".to_owned(),
                true,
                None,
                0,
            ),
            (
                "allocated_detail".to_owned(),
                "TEXT".to_owned(),
                true,
                None,
                0,
            ),
        ]
    );
    let primary_key: Vec<String> = connection
        .prepare(
            "SELECT name FROM pragma_table_info('slot_transition_requests')
             WHERE pk > 0 ORDER BY pk",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(primary_key, ["instance_slug", "slot_id", "request_key"]);
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
fn next_slot_transition_resumes_after_same_generation_worker_restart() {
    let temp = TempDb::new("slot-restart-sequence");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("slot-restart");

    // A prior worker left a durable same-generation sequence behind. The
    // replacement worker must allocate after it, even though its process
    // local cycle counter starts over.
    assert!(store
        .record_slot_transition(&identity, &slot_transition(1, 3, SlotPhase::Idle))
        .unwrap());
    drop(store);

    let restarted = Store::open(&temp.path).unwrap();
    for (sequence, target) in [
        (4, SlotPhase::Teardown),
        (5, SlotPhase::Recycling),
        (6, SlotPhase::Idle),
    ] {
        let request_key = format!("restart-{sequence}");
        let message = format!("restart sequence {sequence}");
        assert!(restarted
            .record_next_slot_transition(
                &identity,
                &next_slot_request(&request_key, 1, target, None, Some(&message)),
            )
            .unwrap());
        let current = restarted
            .slot("slot-restart", &identity.slot_id)
            .unwrap()
            .unwrap();
        assert_eq!(current.transition_sequence, sequence);
    }
    let events = restarted.events_after("slot-restart", 0, 10).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(
        events[1].row.correlation_id.as_deref(),
        Some("corr-slot-g1-s4-teardown")
    );
    assert_eq!(
        events[2].row.correlation_id.as_deref(),
        Some("corr-slot-g1-s5-recycling")
    );
    assert_eq!(
        events[3].row.correlation_id.as_deref(),
        Some("corr-slot-g1-s6-idle")
    );

    // A newer generation gets a fresh sequence root; the prior generation
    // remains fenced and cannot silently allocate against it.
    assert!(restarted
        .record_next_slot_transition(
            &identity,
            &next_slot_request("generation-2-teardown", 2, SlotPhase::Teardown, None, None),
        )
        .unwrap());
    let stale = restarted
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "stale-generation-replay",
                1,
                SlotPhase::Recycling,
                None,
                None,
            ),
        )
        .expect_err("a restarted stale generation must remain fenced");
    assert_eq!(stale.envelope.reason, "store.slot.generation.stale");
    assert_eq!(
        restarted
            .slot("slot-restart", &identity.slot_id)
            .unwrap()
            .unwrap()
            .transition_sequence,
        1
    );
}

#[test]
fn next_slot_transition_same_target_is_idempotent_retry() {
    let temp = TempDb::new("next-slot-idempotent-retry");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("next-slot-idempotent-retry");
    store
        .record_slot_transition(&identity, &slot_transition(1, 1, SlotPhase::Running))
        .unwrap();

    assert!(store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "cycle-one",
                1,
                SlotPhase::Teardown,
                None,
                Some("first transition"),
            ),
        )
        .unwrap());
    let committed = store
        .slot("next-slot-idempotent-retry", &identity.slot_id)
        .unwrap()
        .unwrap();
    let event_count = store
        .event_count("next-slot-idempotent-retry", "fixture-1")
        .unwrap();

    assert!(!store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "cycle-one",
                1,
                SlotPhase::Teardown,
                None,
                Some("first transition"),
            ),
        )
        .unwrap());
    let mismatch = store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "cycle-one",
                1,
                SlotPhase::Teardown,
                None,
                Some("different retry message"),
            ),
        )
        .expect_err("same request key cannot change its message");
    assert_eq!(
        mismatch.envelope.reason,
        "store.slot.transition.idempotency_conflict"
    );
    let retried = store
        .slot("next-slot-idempotent-retry", &identity.slot_id)
        .unwrap()
        .unwrap();
    assert_eq!(retried.transition_sequence, committed.transition_sequence);
    assert_eq!(
        store
            .event_count("next-slot-idempotent-retry", "fixture-1")
            .unwrap(),
        event_count
    );
}

#[test]
fn next_slot_transition_retained_retry_survives_newer_generation() {
    let temp = TempDb::new("next-slot-retained-stale-retry");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("next-slot-retained-stale-retry");
    let first = next_slot_request(
        "retained-request",
        1,
        SlotPhase::Teardown,
        None,
        Some("first generation"),
    );
    assert!(store
        .record_next_slot_transition(&identity, &first)
        .unwrap());
    assert!(store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "generation-two",
                2,
                SlotPhase::Teardown,
                None,
                Some("new owner"),
            ),
        )
        .unwrap());
    let before = store
        .slot("next-slot-retained-stale-retry", &identity.slot_id)
        .unwrap();
    let events_before = store
        .event_count("next-slot-retained-stale-retry", "fixture-1")
        .unwrap();

    assert!(!store
        .record_next_slot_transition(&identity, &first)
        .expect("retained exact replay is idempotent even after fencing"));
    assert_eq!(
        store
            .slot("next-slot-retained-stale-retry", &identity.slot_id)
            .unwrap(),
        before
    );
    assert_eq!(
        store
            .event_count("next-slot-retained-stale-retry", "fixture-1")
            .unwrap(),
        events_before
    );

    let stale_new_request = store
        .record_next_slot_transition(
            &identity,
            &next_slot_request("new-stale-request", 1, SlotPhase::Recycling, None, None),
        )
        .expect_err("a new stale request remains fenced");
    assert_eq!(
        stale_new_request.envelope.reason,
        "store.slot.generation.stale"
    );
}

#[test]
fn latest_slot_transition_request_requires_matching_slot_identity() {
    let temp = TempDb::new("latest-slot-request-identity");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("latest-slot-request-identity");
    assert!(store
        .record_next_slot_transition(
            &identity,
            &next_slot_request("identity-request", 1, SlotPhase::Teardown, None, None),
        )
        .unwrap());

    let mut wrong = identity.clone();
    wrong.slot_index = 1;
    let error = store
        .latest_slot_transition_request_key(&wrong, Generation(1))
        .expect_err("recovery must reject mismatched slot metadata");
    assert_eq!(error.envelope.reason, "store.slot.identity.mismatch");
}

#[test]
fn next_slot_transition_delayed_full_cycle_replay_is_idempotent() {
    let temp = TempDb::new("next-slot-delayed-replay");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("next-slot-delayed-replay");
    store
        .record_slot_transition(&identity, &slot_transition(1, 1, SlotPhase::Running))
        .unwrap();

    assert!(store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "delayed-cycle",
                1,
                SlotPhase::Teardown,
                Some("hold"),
                Some("tear down consumed slot"),
            ),
        )
        .unwrap());
    for (request_key, target, message) in [
        ("delayed-recycle", SlotPhase::Recycling, "recycle slot"),
        ("delayed-idle", SlotPhase::Idle, "return slot to idle"),
        ("delayed-running", SlotPhase::Running, "start next job"),
    ] {
        assert!(store
            .record_next_slot_transition(
                &identity,
                &next_slot_request(request_key, 1, target, Some("hold"), Some(message)),
            )
            .unwrap());
    }
    let before = store
        .slot("next-slot-delayed-replay", &identity.slot_id)
        .unwrap();
    let event_count = store
        .event_count("next-slot-delayed-replay", "fixture-1")
        .unwrap();

    assert!(!store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "delayed-cycle",
                1,
                SlotPhase::Teardown,
                Some("hold"),
                Some("tear down consumed slot"),
            ),
        )
        .unwrap());
    assert_eq!(
        store
            .slot("next-slot-delayed-replay", &identity.slot_id)
            .unwrap(),
        before
    );
    assert_eq!(
        store
            .event_count("next-slot-delayed-replay", "fixture-1")
            .unwrap(),
        event_count
    );

    let mismatch = store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "delayed-cycle",
                1,
                SlotPhase::Teardown,
                Some("different-job"),
                Some("tear down consumed slot"),
            ),
        )
        .expect_err("a reused request key cannot change exact intent");
    assert_eq!(
        mismatch.envelope.reason,
        "store.slot.transition.idempotency_conflict"
    );
}

#[test]
fn next_slot_transition_allocation_is_atomic_across_store_handles() {
    let temp = TempDb::new("slot-concurrent-allocation");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("slot-concurrent");
    assert!(store
        .record_slot_transition(&identity, &slot_transition(1, 1, SlotPhase::Configuring))
        .unwrap());
    drop(store);

    let left_store = Store::open(&temp.path).unwrap();
    let right_store = Store::open(&temp.path).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let left_barrier = Arc::clone(&barrier);
    let right_barrier = Arc::clone(&barrier);
    let left_identity = identity.clone();
    let right_identity = identity.clone();
    let left = thread::spawn(move || {
        left_barrier.wait();
        left_store.record_next_slot_transition(
            &left_identity,
            &next_slot_request("concurrent-idle", 1, SlotPhase::Idle, None, None),
        )
    });
    let right = thread::spawn(move || {
        right_barrier.wait();
        right_store.record_next_slot_transition(
            &right_identity,
            &next_slot_request("concurrent-parked", 1, SlotPhase::Parked, None, None),
        )
    });
    barrier.wait();

    assert!(left.join().unwrap().unwrap());
    assert!(right.join().unwrap().unwrap());

    let reopened = Store::open(&temp.path).unwrap();
    assert_eq!(
        reopened
            .slot("slot-concurrent", &identity.slot_id)
            .unwrap()
            .unwrap()
            .transition_sequence,
        3
    );
    let events = reopened.events_after("slot-concurrent", 0, 10).unwrap();
    let correlations: Vec<_> = events
        .iter()
        .filter_map(|event| event.row.correlation_id.as_deref())
        .collect();
    assert!(
        (correlations.contains(&"corr-slot-g1-s2-idle")
            && correlations.contains(&"corr-slot-g1-s3-parked"))
            || (correlations.contains(&"corr-slot-g1-s2-parked")
                && correlations.contains(&"corr-slot-g1-s3-idle"))
    );
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
fn next_slot_transition_event_failure_does_not_consume_sequence() {
    let temp = TempDb::new("next-slot-atomic-rollback");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("next-slot-rollback");
    store
        .record_slot_transition(&identity, &slot_transition(1, 1, SlotPhase::Running))
        .unwrap();
    let connection = rusqlite_connection(&temp.path);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_next_slot_event
             BEFORE INSERT ON events
             WHEN NEW.event_kind = 'slot.state_changed'
             BEGIN SELECT RAISE(ABORT, 'injected next slot event failure'); END;",
        )
        .unwrap();

    store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "rollback-attempt",
                1,
                SlotPhase::Teardown,
                None,
                Some("failed allocation attempt"),
            ),
        )
        .expect_err("event failure must roll back allocation and transition");
    let current = store
        .slot("next-slot-rollback", &identity.slot_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.phase, SlotPhase::Running);
    assert_eq!(current.transition_sequence, 1);
    assert_eq!(
        store
            .event_count("next-slot-rollback", "fixture-1")
            .unwrap(),
        1
    );

    connection
        .execute_batch("DROP TRIGGER reject_next_slot_event;")
        .unwrap();
    assert!(store
        .record_next_slot_transition(
            &identity,
            &next_slot_request(
                "rollback-attempt",
                1,
                SlotPhase::Teardown,
                None,
                Some("retry allocation"),
            ),
        )
        .unwrap());
    let current = store
        .slot("next-slot-rollback", &identity.slot_id)
        .unwrap()
        .unwrap();
    assert_eq!(current.phase, SlotPhase::Teardown);
    assert_eq!(current.transition_sequence, 2);
    assert_eq!(
        store
            .events_after("next-slot-rollback", 0, 10)
            .unwrap()
            .last()
            .and_then(|event| event.row.correlation_id.as_deref()),
        Some("corr-slot-g1-s2-teardown")
    );
    assert_eq!(
        store
            .event_count("next-slot-rollback", "fixture-1")
            .unwrap(),
        2
    );
}

#[test]
fn next_slot_transition_request_ledger_keeps_deterministic_per_slot_cap() {
    let temp = TempDb::new("next-slot-request-cap");
    let store = Store::open(&temp.path).unwrap();
    let identity = slot_identity("next-slot-request-cap");
    store
        .record_slot_transition(&identity, &slot_transition(1, 1, SlotPhase::Running))
        .unwrap();

    let phases = [
        SlotPhase::Teardown,
        SlotPhase::Recycling,
        SlotPhase::Idle,
        SlotPhase::Acquiring,
        SlotPhase::Running,
    ];
    for sequence in 2..=u64::from(SLOT_TRANSITION_REQUEST_CAP) + 2 {
        let target = phases[(sequence as usize - 2) % phases.len()];
        let request_key = format!("cap-{sequence}");
        assert!(store
            .record_next_slot_transition(
                &identity,
                &next_slot_request(&request_key, 1, target, None, None),
            )
            .unwrap());
    }

    let connection = rusqlite_connection(&temp.path);
    let retained: u32 = connection
        .query_row(
            "SELECT COUNT(*) FROM slot_transition_requests
             WHERE instance_slug = ?1 AND slot_id = ?2",
            [&identity.instance_slug, &identity.slot_id.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, SLOT_TRANSITION_REQUEST_CAP);
    let oldest: Option<String> = connection
        .query_row(
            "SELECT request_key FROM slot_transition_requests
             WHERE instance_slug = ?1 AND slot_id = ?2
             ORDER BY generation, allocated_sequence, request_key
             LIMIT 1",
            [&identity.instance_slug, &identity.slot_id.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(oldest.as_deref(), Some("cap-3"));
    let newest: Option<String> = connection
        .query_row(
            "SELECT request_key FROM slot_transition_requests
             WHERE instance_slug = ?1 AND slot_id = ?2
             ORDER BY generation DESC, allocated_sequence DESC, request_key DESC
             LIMIT 1",
            [&identity.instance_slug, &identity.slot_id.0],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(newest.as_deref(), Some("cap-66"));
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
