#![cfg(feature = "test-support")]

use std::path::Path;

use rusqlite::Connection;

fn arm_actual_store_failure(path: &Path) {
    // Store admission writes summary, reservation, and transition rows in one
    // SQLite transaction. Abort after the transition insert to prove rollback
    // of rows written earlier in that same transaction.
    let connection = Connection::open(path).expect("open Store failure injection connection");
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .expect("configure Store failure injection timeout");
    connection
        .execute_batch(
            "CREATE TRIGGER velnor_telemetry_proof_store_failure
             AFTER INSERT ON job_transitions
             BEGIN
                 SELECT RAISE(ABORT, 'test-injected durable store write failure: store.test.disk-full');
             END;",
        )
        .expect("install Store transaction failure injection");
}

#[test]
fn ops_telemetry_probe_is_ordered_secret_safe_and_reports_cache_misses() {
    const SECRET: &str = "ops-telemetry-probe-secret";

    let (raw_telemetry, durable_store) = velnor_runner::test_support::run_ops_telemetry_probe();
    assert!(!raw_telemetry.contains(SECRET));
    assert!(!durable_store
        .windows(SECRET.len())
        .any(|window| window == SECRET.as_bytes()));
    let records = raw_telemetry
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse persisted telemetry JSONL"))
        .collect::<Vec<serde_json::Value>>();
    let events = records
        .iter()
        .map(|record| record["event"].as_str().expect("event string"))
        .collect::<Vec<_>>();

    assert_eq!(events, ["run_queued", "run_admitted", "cache_lookup"]);
    assert_eq!(records[0]["trust_domain"], "unknown");
    assert_eq!(records[2]["fields"]["miss_reason"], "key_absent");
    let serialized = serde_json::to_string(&records).expect("serialize probe records");
    assert!(!serialized.contains(SECRET));
}

#[test]
fn telemetry_sink_open_failure_falls_back_to_ring() {
    let (stats, records) = velnor_runner::test_support::run_ops_telemetry_open_failure_probe();

    assert_eq!(stats.emitted(), 1);
    assert_eq!(stats.file_failures(), 1);
    assert_eq!(stats.file_writes(), 0);
    assert_eq!(stats.file_bytes(), 0);
    assert!(!stats.file_enabled());
    assert_eq!(records[0]["event"], "run_queued");
}

#[test]
#[cfg(unix)]
fn telemetry_sink_post_open_failure_keeps_ring_records_and_is_secret_safe() {
    const SECRET: &str = "ops-telemetry-sink-secret";

    let (stats, records) = velnor_runner::test_support::run_ops_telemetry_sink_failure_probe();

    assert_eq!(stats.emitted(), 3);
    assert_eq!(stats.file_failures(), 1);
    assert_eq!(stats.file_writes(), 0);
    assert_eq!(stats.file_bytes(), 0);
    assert!(!stats.file_enabled());
    assert_eq!(
        records
            .iter()
            .map(|record| record["event"].as_str().expect("event string"))
            .collect::<Vec<_>>(),
        ["run_queued", "run_admitted", "cache_lookup"]
    );
    assert_eq!(records[2]["fields"]["miss_reason"], "key_absent");
    assert!(records
        .iter()
        .all(|record| record["trust_domain"] == "unknown"));
    let serialized = serde_json::to_string(&records).expect("serialize ring records");
    assert!(!serialized.contains(SECRET));
}

#[test]
fn store_sink_failure_rejects_admission_without_partial_state_or_secret_leak() {
    const SECRET: &str = "ops-telemetry-store-secret";

    let (accepted, durable_rows, forensic_failures) =
        velnor_runner::test_support::run_ops_store_failure_probe(arm_actual_store_failure);

    assert!(!accepted);
    assert_eq!(durable_rows, (0, 0, 0));
    assert!(forensic_failures.iter().any(|entry| {
        entry.contains("store.admission.persist")
            && entry.contains("store.test.disk-full")
            && entry.contains("durable store write failure")
    }));
    let serialized = serde_json::to_string(&forensic_failures).expect("serialize failures");
    assert!(!serialized.contains(SECRET));
}

#[test]
fn deterministic_telemetry_ndjson_matches_golden() {
    const GOLDEN: &str = include_str!("fixtures/telemetry.golden.ndjson");
    const SECRET: &str = "ops-telemetry-golden-secret";

    let actual = velnor_runner::test_support::render_deterministic_telemetry_fixture();
    assert_eq!(actual, GOLDEN);
    assert!(actual.contains("\"trust_domain\":\"unknown\""));
    assert!(!actual.contains(SECRET));

    let events = actual
        .lines()
        .map(|line| {
            let envelope: velnor_model::TelemetryEnvelope =
                serde_json::from_str(line).expect("golden line is a telemetry envelope");
            serde_json::to_value(envelope).expect("serialize parsed golden envelope")["event"]
                .as_str()
                .expect("event string")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(events, ["run_queued", "run_admitted", "cache_lookup"]);
}
