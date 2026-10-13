use std::{collections::BTreeMap, fs};

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

/// Exercise the operational telemetry path without exposing its implementation
/// modules to integration crates.
#[must_use]
pub fn run_ops_telemetry_probe() -> Vec<serde_json::Value> {
    const INSTANCE: &str = "test-support-instance";
    const SECRET: &str = "ops-telemetry-probe-secret";

    let directory = TempDirGuard::new(std::env::temp_dir().join(format!(
        "velnor-ops-telemetry-probe-{}",
        uuid::Uuid::new_v4().simple()
    )));
    let state_path = directory.path.join("state.db");
    let telemetry_path = velnor_control::telemetry::path_for_instance(&state_path, INSTANCE);

    let raw_telemetry = {
        let sink = crate::ops::OpsSink::open(state_path, INSTANCE.to_owned())
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
            trust_scope: Some("trusted".to_owned()),
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

        fs::read_to_string(&telemetry_path).expect("read telemetry probe JSONL")
    };

    assert!(!raw_telemetry.contains(SECRET));
    let records = raw_telemetry
        .lines()
        .map(|line| serde_json::from_str(line).expect("parse telemetry probe JSONL"))
        .collect();
    records
}
