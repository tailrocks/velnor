#![cfg(feature = "test-support")]

#[test]
fn ops_telemetry_probe_is_ordered_secret_safe_and_reports_cache_misses() {
    const SECRET: &str = "ops-telemetry-probe-secret";

    let records = velnor_runner::test_support::run_ops_telemetry_probe();
    let events = records
        .iter()
        .map(|record| record["event"].as_str().expect("event string"))
        .collect::<Vec<_>>();

    assert_eq!(events, ["run_queued", "run_admitted", "cache_lookup"]);
    assert_eq!(records[2]["fields"]["miss_reason"], "key_absent");
    let serialized = serde_json::to_string(&records).expect("serialize probe records");
    assert!(!serialized.contains(SECRET));
}
