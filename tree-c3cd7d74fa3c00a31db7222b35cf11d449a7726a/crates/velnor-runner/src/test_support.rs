use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::{
    ffi::CString, fs::OpenOptions, os::unix::ffi::OsStrExt, sync::mpsc, thread, time::Duration,
};

use velnor_model::{TelemetryEvent, TelemetrySinkStats};

#[cfg(test)]
use std::{ffi::OsString, sync::LazyLock};

#[cfg(test)]
static GITHUB_HTTP_TRANSPORT_ENV_LOCK: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

#[cfg(test)]
pub(crate) struct GithubHttpTransportEnvGuard {
    previous: Option<OsString>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub(crate) async fn github_http_transport_env() -> GithubHttpTransportEnvGuard {
    let lock = GITHUB_HTTP_TRANSPORT_ENV_LOCK.lock().await;
    let previous = std::env::var_os(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV);
    GithubHttpTransportEnvGuard {
        previous,
        _lock: lock,
    }
}

#[cfg(test)]
impl GithubHttpTransportEnvGuard {
    pub(crate) fn set_native(&self) {
        std::env::set_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV, "native");
    }
}

#[cfg(test)]
impl Drop for GithubHttpTransportEnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV, value),
            None => std::env::remove_var(crate::protocol::GITHUB_HTTP_TRANSPORT_ENV),
        }
    }
}

struct TempDirGuard {
    path: std::path::PathBuf,
}

impl TempDirGuard {
    fn new(path: std::path::PathBuf) -> Self {
        fs::create_dir_all(&path).expect("create telemetry probe directory");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn probe_directory(label: &str) -> TempDirGuard {
    TempDirGuard::new(
        std::env::temp_dir().join(format!("velnor-{label}-{}", uuid::Uuid::new_v4().simple())),
    )
}

fn sqlite_wal_path(path: &Path) -> PathBuf {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    wal.into()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn probe_admission(
    instance: &str,
    job_uid: &str,
    workflow: &str,
    job_name: &str,
    trust_scope: &str,
    run_id: u64,
    masks: Vec<String>,
) -> crate::ops::JobAdmission {
    crate::ops::JobAdmission {
        instance_slug: instance.to_owned(),
        job_uid: job_uid.to_owned(),
        repository_full_name: "tailrocks/velnor-actions-fixture".to_owned(),
        workflow: workflow.to_owned(),
        job_name: job_name.to_owned(),
        run_id: Some(run_id),
        attempt: Some(1),
        head_ref: Some("refs/heads/main".to_owned()),
        head_sha: Some("deadbeef".to_owned()),
        trigger_event: Some("workflow_dispatch".to_owned()),
        queued_at_rfc3339: None,
        slot_name: Some("slot-0".to_owned()),
        runner_name: Some("probe-runner".to_owned()),
        trust_scope: Some(trust_scope.to_owned()),
        resource_policy: Some("standard".to_owned()),
        masks,
    }
}

/// Exercise the operational telemetry path without exposing its implementation
/// modules to integration crates.
#[must_use]
pub fn run_ops_telemetry_probe() -> (String, Vec<u8>) {
    const INSTANCE: &str = "test-support-instance";
    const SECRET: &str = "ops-telemetry-probe-secret";

    let directory = probe_directory("ops-telemetry-probe");
    let state_path = directory.path.join("state.db");
    let telemetry_path = velnor_control::telemetry::path_for_instance(&state_path, INSTANCE);

    let (raw_telemetry, durable_store) = {
        let sink = crate::ops::OpsSink::open(state_path.clone(), INSTANCE.to_owned())
            .expect("open telemetry probe operational sink");
        let admission = crate::ops::JobAdmission {
            instance_slug: INSTANCE.to_owned(),
            job_uid: "probe-job-1".to_owned(),
            repository_full_name: "tailrocks/velnor-actions-fixture".to_owned(),
            workflow: "telemetry probe".to_owned(),
            job_name: SECRET.to_owned(),
            run_id: Some(1),
            attempt: Some(1),
            head_ref: Some("refs/heads/main".to_owned()),
            head_sha: Some("deadbeef".to_owned()),
            trigger_event: Some("workflow_dispatch".to_owned()),
            queued_at_rfc3339: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("probe-runner".to_owned()),
            trust_scope: Some(SECRET.to_owned()),
            resource_policy: Some("standard".to_owned()),
            masks: vec![SECRET.to_owned()],
        };

        let queued_fields = BTreeMap::from([
            ("queued_for_ms".to_owned(), serde_json::json!(0_u64)),
            ("queue_time_present".to_owned(), serde_json::json!(false)),
        ]);
        assert!(sink
            .emit_telemetry_for_admission(
                &admission,
                velnor_model::TelemetryEvent::RunQueued,
                queued_fields,
            )
            .is_some());
        assert!(sink.record_admission(&admission));

        let cache_fields = BTreeMap::from([
            ("hit".to_owned(), serde_json::json!(false)),
            ("lookup_ms".to_owned(), serde_json::json!(0_u64)),
            ("miss_reason".to_owned(), serde_json::json!("key_absent")),
            ("store".to_owned(), serde_json::json!("probe")),
        ]);
        assert!(sink
            .emit_telemetry_for_admission(
                &admission,
                velnor_model::TelemetryEvent::CacheLookup,
                cache_fields,
            )
            .is_some());

        let raw_telemetry =
            fs::read_to_string(&telemetry_path).expect("read telemetry probe JSONL");
        let mut durable_store = fs::read(&state_path).expect("read SQLite database");
        let wal = fs::read(sqlite_wal_path(&state_path))
            .expect("successful store writes leave a SQLite WAL to inspect");
        assert!(
            !wal.is_empty(),
            "successful store write produced an empty WAL"
        );
        durable_store.extend(wal);
        (raw_telemetry, durable_store)
    };

    assert!(!raw_telemetry.contains(SECRET));
    assert!(!contains_bytes(&durable_store, SECRET.as_bytes()));
    (raw_telemetry, durable_store)
}

/// Exercise telemetry-file constructor fallback through the operational sink.
///
/// The path is unavailable while `OpsSink::open` constructs its telemetry
/// sink, so the resulting file failure is an open failure, not an emission
/// failure.
#[must_use]
pub fn run_ops_telemetry_open_failure_probe() -> (TelemetrySinkStats, Vec<serde_json::Value>) {
    const INSTANCE: &str = "test-support-instance";

    let directory = probe_directory("ops-telemetry-open-failure");
    let state_path = directory.path.join("state.db");
    let telemetry_path = velnor_control::telemetry::path_for_instance(&state_path, INSTANCE);
    fs::create_dir(&telemetry_path).expect("reserve telemetry path as a directory");

    let sink = crate::ops::OpsSink::open(state_path, INSTANCE.to_owned())
        .expect("open sink with degraded telemetry file");
    let admission = probe_admission(
        INSTANCE,
        "probe-job-telemetry-open-failure",
        "telemetry open failure",
        "probe-job",
        "trusted",
        2,
        Vec::new(),
    );

    let queued = sink
        .emit_telemetry_for_admission(
            &admission,
            TelemetryEvent::RunQueued,
            BTreeMap::from([
                ("queue_time_present".to_owned(), serde_json::json!(false)),
                ("queued_for_ms".to_owned(), serde_json::json!(0_u64)),
            ]),
        )
        .expect("queue event is valid");
    assert!(!queued.file_written());
    let (stats, envelopes) = sink.telemetry_probe_snapshot();
    let records = envelopes
        .into_iter()
        .map(|envelope| serde_json::to_value(envelope).expect("serialize ring envelope"))
        .collect();
    (stats, records)
}

/// Exercise a post-open telemetry emission failure through the operational
/// sink. The telemetry writer is opened successfully on a FIFO, then its
/// reader closes before the first emission reaches the write/flush path.
#[must_use]
#[cfg(unix)]
pub fn run_ops_telemetry_sink_failure_probe() -> (TelemetrySinkStats, Vec<serde_json::Value>) {
    const INSTANCE: &str = "test-support-instance";
    const SECRET: &str = "ops-telemetry-sink-secret";

    let directory = probe_directory("ops-telemetry-sink-failure");
    let state_path = directory.path.join("state.db");
    let telemetry_path = velnor_control::telemetry::path_for_instance(&state_path, INSTANCE);
    let telemetry_fifo_path = CString::new(telemetry_path.as_os_str().as_bytes())
        .expect("telemetry FIFO path has no NUL bytes");
    // SAFETY: `telemetry_fifo_path` is a live, NUL-terminated pathname and
    // the mode has no pointer or lifetime requirements.
    let fifo_result = unsafe { libc::mkfifo(telemetry_fifo_path.as_ptr(), 0o600) };
    assert_eq!(
        fifo_result,
        0,
        "create telemetry FIFO: {}",
        std::io::Error::last_os_error()
    );

    let (reader_ready, reader_ready_rx) = mpsc::sync_channel(1);
    let reader_path = telemetry_path.clone();
    let reader = thread::spawn(move || {
        let _reader = OpenOptions::new()
            .read(true)
            .open(reader_path)
            .expect("open telemetry FIFO reader");
        reader_ready.send(()).expect("signal telemetry FIFO reader");
    });
    let sink = crate::ops::OpsSink::open(state_path, INSTANCE.to_owned())
        .expect("open sink with writable telemetry file");
    reader_ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("telemetry FIFO reader opens with sink writer");
    reader.join().expect("telemetry FIFO reader exits cleanly");
    let initial_stats = sink.telemetry_probe_snapshot().0;
    assert!(initial_stats.file_enabled());
    assert_eq!(initial_stats.file_failures(), 0);

    let admission = probe_admission(
        INSTANCE,
        "probe-job-sink-failure",
        "telemetry sink failure",
        SECRET,
        SECRET,
        2,
        vec![SECRET.to_owned()],
    );
    let queued = sink
        .emit_telemetry_for_admission(
            &admission,
            TelemetryEvent::RunQueued,
            BTreeMap::from([
                ("queue_time_present".to_owned(), serde_json::json!(false)),
                ("queued_for_ms".to_owned(), serde_json::json!(0_u64)),
            ]),
        )
        .expect("queue event is valid");
    assert!(!queued.file_written());

    assert!(sink.record_admission(&admission));
    let cache = sink
        .emit_telemetry_for_admission(
            &admission,
            TelemetryEvent::CacheLookup,
            BTreeMap::from([
                ("hit".to_owned(), serde_json::json!(false)),
                ("lookup_ms".to_owned(), serde_json::json!(0_u64)),
                ("miss_reason".to_owned(), serde_json::json!("key_absent")),
                ("store".to_owned(), serde_json::json!("probe")),
            ]),
        )
        .expect("cache event is valid");
    assert!(!cache.file_written());

    let (stats, envelopes) = sink.telemetry_probe_snapshot();
    let records = envelopes
        .into_iter()
        .map(|envelope| serde_json::to_value(envelope).expect("serialize ring envelope"))
        .collect();
    (stats, records)
}

/// Exercise an injected operational-store write failure without exposing the
/// store sink or its failure hooks to integration crates. The callback arms a
/// failure in SQLite itself before the real Store operation begins.
#[must_use]
pub fn run_ops_store_failure_probe(
    arm_failure: impl FnOnce(&Path),
) -> (bool, (u64, u64, u64), Vec<String>) {
    const INSTANCE: &str = "test-support-instance";
    const SECRET: &str = "ops-telemetry-store-secret";

    let directory = probe_directory("ops-store-failure");
    let state_path = directory.path.join("state.db");
    let sink = crate::ops::OpsSink::open(state_path.clone(), INSTANCE.to_owned())
        .expect("open operational store sink");
    let admission = probe_admission(
        INSTANCE,
        "probe-job-store-failure",
        "telemetry store failure",
        SECRET,
        "trusted",
        3,
        vec![SECRET.to_owned()],
    );

    arm_failure(&state_path);
    let accepted = sink.record_admission(&admission);
    let forensic_failures = sink.forensic_failures();
    drop(sink);

    let reopened = velnor_control::store::Store::open(&state_path)
        .expect("reopen store after failed transaction");
    let accounting = reopened
        .accounting()
        .expect("read reopened store accounting");
    (
        accepted,
        (
            accounting.job_rows,
            accounting.transition_rows,
            accounting.event_rows,
        ),
        forensic_failures,
    )
}

/// Render the current telemetry wire contract through `OpsSink` with fixed
/// wall-clock output for a deterministic golden fixture.
#[must_use]
pub fn render_deterministic_telemetry_fixture() -> String {
    const SECRET: &str = "ops-telemetry-golden-secret";

    let directory = probe_directory("telemetry-golden");
    let state_path = directory.path.join("state.db");
    let telemetry_path =
        velnor_control::telemetry::path_for_instance(&state_path, "test-support-instance");
    let sink = crate::ops::OpsSink::open(state_path, "test-support-instance".to_owned())
        .expect("open golden operational sink");
    let admission = probe_admission(
        "test-support-instance",
        "probe-job-golden",
        "telemetry golden",
        SECRET,
        SECRET,
        1,
        vec![SECRET.to_owned()],
    );

    assert!(sink
        .emit_telemetry_for_admission(
            &admission,
            TelemetryEvent::RunQueued,
            BTreeMap::from([
                ("queue_time_present".to_owned(), serde_json::json!(false)),
                ("queued_for_ms".to_owned(), serde_json::json!(0_u64)),
            ]),
        )
        .is_some_and(|emission| emission.file_written()));
    assert!(sink.record_admission(&admission));
    assert!(sink
        .emit_telemetry_for_admission(
            &admission,
            TelemetryEvent::CacheLookup,
            BTreeMap::from([
                ("hit".to_owned(), serde_json::json!(false)),
                ("lookup_ms".to_owned(), serde_json::json!(0_u64)),
                ("miss_reason".to_owned(), serde_json::json!("key_absent")),
                ("store".to_owned(), serde_json::json!("probe")),
            ]),
        )
        .is_some_and(|emission| emission.file_written()));
    drop(sink);

    let raw = fs::read_to_string(telemetry_path).expect("read OpsSink telemetry fixture");
    raw.lines()
        .map(|line| {
            let marker = "\"ts_wall\":\"";
            let value_start =
                line.find(marker).expect("OpsSink line has wall timestamp") + marker.len();
            let value_end = value_start
                + line[value_start..]
                    .find('"')
                    .expect("OpsSink wall timestamp is quoted");
            let mut normalized = line.to_owned();
            normalized.replace_range(value_start..value_end, "2026-08-24T12:30:45Z");
            normalized
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}
