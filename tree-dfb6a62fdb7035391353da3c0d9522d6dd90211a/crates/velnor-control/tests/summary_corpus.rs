//! Secret-proof round-trip corpus for persisted job summaries.
//!
//! Modeled on `velnor-model`'s redaction corpus: summaries are built through
//! the sanitized constructor with adversarial-but-valid values, persisted,
//! and then proven absent from raw database page bytes, every stored text
//! column, and the decoded query DTO.

use rusqlite::Connection;
use velnor_control::store::{InstanceRow, Store, LATEST_SCHEMA_VERSION};
use velnor_model::{
    InfrastructureCategory, JobConclusion, JobPhase, JobSummary, NormalizedJob, RepositoryRef,
    Timestamp, TriggerEvent,
};

/// Values that must never appear anywhere in the store when summaries are
/// built exclusively through the sanitized constructor.
const SECRET_MARKERS: [&str; 7] = [
    "SECRET_MARKER_VALUE",
    "ghp_",
    "gho_",
    "sup3r-secret-value",
    "https://token@host/",
    "Authorization:",
    "Bearer ",
];

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
        let dir = std::env::temp_dir().join(format!("velnor-summary-corpus-{label}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("temp dir created");
        let path = dir.join("state.db");
        Self { dir, path }
    }

    fn truncate_wal_and_read_bytes(&self) -> Vec<u8> {
        let conn = Connection::open(&self.path).expect("raw connection for checkpoint");
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("wal truncated into the main database file");
        drop(conn);
        std::fs::read(&self.path).expect("database file readable")
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn at(rfc3339: &str) -> Timestamp {
    Timestamp::parse(rfc3339).expect("fixture timestamp")
}

/// Adversarial-but-valid: every value passes the slug charset while
/// exercising separators, dots, underscores, slashes, and long hex.
fn summary(run_id: u64, attempt: u32) -> JobSummary {
    JobSummary::from_normalized(NormalizedJob {
        instance_slug: "sentry/main".to_owned(),
        job_uid: format!("summary-run-{run_id}-attempt-{attempt}"),
        repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
        workflow: ".github/workflows/deep.path_v2/control-plane.yml".to_owned(),
        job_name: "hold_matrix.included_1".to_owned(),
        run_id: Some(run_id),
        attempt: Some(attempt),
        head_ref: Some("refs/pull/1337/head".to_owned()),
        head_sha: Some("0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e".to_owned()),
        trigger_event: Some(TriggerEvent::WorkflowDispatch),
        queued_at: Some(at("2026-08-24T12:30:45Z")),
        acquired_at: Some(at("2026-08-24T12:30:47Z")),
        runner_name: Some("sentry.slot-0.runner_a".to_owned()),
        trust_scope: Some("trusted".to_owned()),
        resource_policy: Some("standard.v2".to_owned()),
        phase: JobPhase::Running,
        conclusion: None,
        infrastructure_category: None,
    })
    .expect("adversarial-but-valid fixture passes validation")
}

fn completed(summary: &JobSummary) -> JobSummary {
    // Terminal state is rebuilt through the same validated constructor;
    // there is no other way to obtain a persistable DTO.
    let mut inputs = inputs_of(summary);
    inputs.phase = JobPhase::Completed;
    inputs.conclusion = Some(JobConclusion::Failure);
    inputs.infrastructure_category = Some(InfrastructureCategory::DockerEnvironment);
    JobSummary::from_normalized(inputs).expect("terminal variant revalidates")
}

fn inputs_of(summary: &JobSummary) -> NormalizedJob {
    NormalizedJob {
        instance_slug: summary.instance_slug().to_owned(),
        job_uid: summary.job_uid().to_owned(),
        repository: summary.repository().clone(),
        workflow: summary.workflow().to_owned(),
        job_name: summary.job_name().to_owned(),
        run_id: summary.run_id(),
        attempt: summary.attempt(),
        head_ref: summary.head_ref().map(str::to_owned),
        head_sha: summary.head_sha().map(str::to_owned),
        trigger_event: summary.trigger_event(),
        queued_at: summary.queued_at(),
        acquired_at: summary.acquired_at(),
        runner_name: summary.runner_name().map(str::to_owned),
        trust_scope: summary.trust_scope().map(str::to_owned),
        resource_policy: summary.resource_policy().map(str::to_owned),
        phase: summary.phase(),
        conclusion: summary.conclusion(),
        infrastructure_category: summary.infrastructure_category(),
    }
}

fn instance(slug: &str) -> InstanceRow {
    InstanceRow {
        instance_slug: slug.to_owned(),
        host: "sentry".to_owned(),
        daemon_version: "0.1.0".to_owned(),
        slots_configured: 4,
        slots_busy: 1,
        updated_at: Timestamp::now(),
    }
}

fn assert_no_markers(haystack: &str, context: &str) {
    for marker in SECRET_MARKERS {
        assert!(
            !haystack.contains(marker),
            "{context} leaked marker {marker:?}"
        );
    }
}

#[test]
fn schema_is_at_the_run_attempt_identity_version() {
    let temp = TempDb::new("version");
    let store = Store::open(&temp.path).expect("open");
    assert_eq!(store.schema_version().unwrap(), LATEST_SCHEMA_VERSION);
}

#[test]
fn persist_fetch_round_trip_and_idempotent_repersist() {
    let temp = TempDb::new("round-trip");
    let store = Store::open(&temp.path).unwrap();
    store.upsert_instance(&instance("sentry/main")).unwrap();

    let first = summary(42, 1);
    store.persist_summary(&first).unwrap();
    let fetched = store.fetch_summary("sentry/main", 42, 1).unwrap();
    assert_eq!(fetched.as_ref(), Some(&first), "decoded DTO is exact");

    // Same identity again: refreshes identity/metadata in place, never
    // duplicates. Lifecycle columns (phase/conclusion/category) are owned
    // exclusively by the state machine, so replaying a stale admission can
    // never regress or rewrite them.
    let terminal = completed(&first);
    store.persist_summary(&terminal).unwrap();
    let refetched = store.fetch_summary("sentry/main", 42, 1).unwrap();
    assert_eq!(
        refetched.as_ref(),
        Some(&first),
        "lifecycle columns survive replay"
    );
    assert_eq!(store.job_summaries("sentry/main").unwrap().len(), 1);

    // First-seen queue/acquisition times survive the refresh (COALESCE).
    assert_eq!(refetched.unwrap().queued_at(), first.queued_at());

    // A different attempt is a distinct identity.
    store.persist_summary(&summary(42, 2)).unwrap();
    assert_eq!(store.job_summaries("sentry/main").unwrap().len(), 2);
}

#[test]
fn unidentified_summary_fails_closed_and_writes_nothing() {
    let temp = TempDb::new("unidentified");
    let store = Store::open(&temp.path).unwrap();

    let mut no_run = inputs_of(&summary(7, 1));
    no_run.run_id = None;
    let error = store
        .persist_summary(&JobSummary::from_normalized(no_run).unwrap())
        .unwrap_err();
    assert_eq!(error.envelope.reason, "store.job.summary.unidentified");

    let mut no_attempt = inputs_of(&summary(7, 2));
    no_attempt.attempt = None;
    let error = store
        .persist_summary(&JobSummary::from_normalized(no_attempt).unwrap())
        .unwrap_err();
    assert_eq!(error.envelope.reason, "store.job.summary.unidentified");

    assert_eq!(store.job_summaries("sentry/main").unwrap().len(), 0);
}

#[test]
fn fetch_unknown_identity_returns_none() {
    let temp = TempDb::new("unknown");
    let store = Store::open(&temp.path).unwrap();
    store.persist_summary(&summary(42, 1)).unwrap();
    assert!(store.fetch_summary("sentry/main", 43, 1).unwrap().is_none());
    assert!(store
        .fetch_summary("other-instance", 42, 1)
        .unwrap()
        .is_none());
}

#[test]
fn database_pages_and_columns_contain_no_secret_markers() {
    let temp = TempDb::new("bytes");
    let store = Store::open(&temp.path).unwrap();
    store.persist_summary(&summary(42, 1)).unwrap();
    store.persist_summary(&summary(84, 3)).unwrap();

    // Raw page bytes: after truncating the WAL into the main file, the whole
    // database image must be free of every marker.
    let bytes = temp.truncate_wal_and_read_bytes();
    let rendered = String::from_utf8_lossy(&bytes);
    assert_no_markers(&rendered, "database page bytes");

    // Every stored text column of every row, read back verbatim.
    let conn = Connection::open(&temp.path).unwrap();
    let mut statement = conn
        .prepare(
            "SELECT instance_slug, job_uid, repository, workflow, job_name, head_ref, head_sha,
                    trigger_event, queued_at, acquired_at, runner_name, trust_scope,
                    resource_policy, phase, conclusion, infrastructure_category, updated_at
             FROM jobs",
        )
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    let mut inspected = 0;
    while let Some(row) = rows.next().unwrap() {
        for column in 0..17 {
            let value: Option<String> = row.get(column).unwrap();
            if let Some(value) = value {
                assert_no_markers(&value, "stored column");
            }
        }
        inspected += 1;
    }
    assert_eq!(inspected, 2, "both summaries were inspected");
}

#[test]
fn tampered_row_decodes_fail_closed_naming_reason() {
    let temp = TempDb::new("tamper");
    let store = Store::open(&temp.path).unwrap();
    store.persist_summary(&summary(42, 1)).unwrap();

    let conn = Connection::open(&temp.path).unwrap();
    conn.execute("UPDATE jobs SET phase = 'mysterious'", [])
        .unwrap();
    let error = store.fetch_summary("sentry/main", 42, 1).unwrap_err();
    assert_eq!(error.envelope.reason, "store.job.summary.decode");
    assert!(
        !error.to_string().contains("mysterious"),
        "decode failures must not echo stored values"
    );
}
