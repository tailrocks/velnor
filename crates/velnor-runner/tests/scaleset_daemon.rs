//! Scale-set daemon wiring: registration, sessions, lane, capacity,
//! shutdown, and crash recovery against a mocked Actions Service.
//!
//! No live calls. `wiremock` stands in for GitHub (admin plane, queue,
//! JIT); [`FakeDocker`] stands in for the Engine (stateful: create,
//! start, inspect, logs, stop, rm converge like the real daemon); the
//! stores, ledger, listener, and [`ScaleSetDaemon`] are the real
//! production code. Each test pins one wiring behavior: registration
//! adopt/create/reconcile, end-to-end job service, restart adoption
//! without re-provisioning, explicit failure of dead work, shared-`N`
//! capacity across lanes, key-file enforcement, and best-effort session
//! close.
//!
//! Multi-thread test flavor throughout: the production lane drives its
//! async JIT fetch from sync lane methods, which requires a
//! multi-threaded runtime (the daemon's `main` builds one).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
#![cfg(feature = "test-support")]

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velnor_control::permit_ledger::{PermitLane, PermitLedger, PermitState};
use velnor_runner::scaleset::worker::dind::DIND_DATA_ROOT;
use velnor_runner::scaleset::worker::ownership::{
    ROLE_RUNNER, ROLE_VOLUME_HOLDER, WORKER_ROLE_LABEL,
};
use velnor_runner::scaleset::worker::{
    HomogeneousProfile, OwnershipId, PinnedImage, ToolContentAttestation, ToolContentExpectation,
    ToolContentHook, WorkerIdentity, WorkerOutput, WorkerRunner, TOOL_CACHE_DIR, WORK_DIR,
};
use velnor_runner::scaleset::{
    runner_name, AcquireBatchStore, CapacityLedger, ClientSession, DaemonDefaults,
    DaemonWorkerLane, DemandState, DemandStore, IdlePolicy, LaneConfig, Listener, LoopConfig,
    MessageSessionClient, Metrics, OfferAdmission, Processor, ProcessorConfig, ProvisionImages,
    ProvisionIntentStore, RegistrationPlan, RetryPolicy, ScaleSetClient, ScaleSetDaemon,
    SessionStore, SharedLedger, SystemInfo, WorkerRegistry, MAX_ACQUIRE_BATCH,
};
use wiremock::{
    matchers::{body_string_contains, header, method, path, path_regex},
    Mock, MockServer, Request, ResponseTemplate,
};

const SCALE_SET_ID: i32 = 7;
const OWNER: &str = "octo-org";
const SESSION_ID: &str = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
const GROUP_ID: i32 = 3;
const GROUP_NAME: &str = "velnor";
const SET_NAME: &str = "velnor-set";
const REQUEST_ID: i64 = 4244;

// ---------------------------------------------------------------------------
// Harness: dirs, client, token chain
// ---------------------------------------------------------------------------

fn temp_root(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "velnor-scaleset-daemon-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn system_info() -> SystemInfo {
    SystemInfo {
        system: "velnor".into(),
        version: "0.1.0-test".into(),
        commit_sha: "test".into(),
        scale_set_id: SCALE_SET_ID,
        subsystem: "scaleset-listener".into(),
    }
}

fn test_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 0,
        wait_min: Duration::from_millis(1),
        wait_max: Duration::from_millis(1),
        timeout: Duration::from_secs(30),
    }
}

fn admin_jwt(exp: u64) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{exp}}}"));
    format!("test-header.{payload}.test-signature")
}

fn session_json(server: &MockServer, token: &str) -> serde_json::Value {
    serde_json::json!({
        "sessionId": SESSION_ID,
        "ownerName": OWNER,
        "messageQueueUrl": format!(
            "{}/tenant/_apis/runtime/runnerscalesets/7/messages/session-1",
            server.uri()
        ),
        "messageQueueAccessToken": token,
        "statistics": {
            "totalAvailableJobs": 1, "totalAcquiredJobs": 0, "totalAssignedJobs": 2,
            "totalRunningJobs": 1, "totalRegisteredRunners": 2,
            "totalBusyRunners": 1, "totalIdleRunners": 1
        }
    })
}

async fn mount_token_chain(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(
            "/api/v3/orgs/octo-org/actions/runners/registration-token",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "token": "reg-token",
            "expires_at": "2026-09-17T00:05:00Z"
        })))
        .mount(server)
        .await;
    let admin_url = format!("{}/tenant", server.uri());
    Mock::given(method("POST"))
        .and(path("/api/v3/actions/runner-registration"))
        .and(header("Authorization", "RemoteAuth reg-token"))
        .and(body_string_contains(OWNER))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "url": admin_url,
            "token": admin_jwt(2_000_000_000)
        })))
        .mount(server)
        .await;
}

async fn test_client(server: &MockServer) -> ScaleSetClient {
    mount_token_chain(server).await;
    ScaleSetClient::new_with_pat(
        &format!("{}/octo-org", server.uri()),
        "test-pat",
        system_info(),
        test_retry(),
    )
    .unwrap()
}

fn queue_path() -> String {
    format!("/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/messages/session-1")
}

fn sets_path() -> String {
    "/tenant/_apis/runtime/runnerscalesets".to_owned()
}

async fn mount_session_create(server: &MockServer, token: &str) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(session_json(server, token)))
        .mount(server)
        .await;
}

async fn mount_session_close(server: &MockServer, status: u16) {
    Mock::given(method("DELETE"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/sessions/{SESSION_ID}"
        )))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

// ---------------------------------------------------------------------------
// Registration mocks
// ---------------------------------------------------------------------------

fn group_json() -> serde_json::Value {
    serde_json::json!({
        "count": 1,
        "value": [{ "id": GROUP_ID, "name": GROUP_NAME, "size": 1, "isDefaultGroup": false }]
    })
}

fn set_json(id: i32, labels: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": SET_NAME,
        "runnerGroupId": GROUP_ID,
        "runnerGroupName": GROUP_NAME,
        "labels": labels.iter().map(|name| {
            serde_json::json!({ "type": "User", "name": name })
        }).collect::<Vec<_>>(),
        "RunnerSetting": { "disableUpdate": false },
        "createdOn": "2026-09-17T00:00:00Z",
    })
}

/// Group-by-name lookup (the trailing-slash-tolerant path form).
async fn mount_group_lookup(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path_regex(r"/_apis/runtime/runnergroups/?"))
        .respond_with(ResponseTemplate::new(200).set_body_json(group_json()))
        .mount(server)
        .await;
}

/// Get-or-create by name: first lookup misses, create lands, later lookups
/// hit. Models one daemon's view; each test mounts its own server.
async fn mount_set_get_or_create(server: &MockServer) {
    let looked_up = Arc::new(AtomicBool::new(false));
    let seen = looked_up.clone();
    Mock::given(method("GET"))
        .and(path(sets_path()))
        .respond_with(move |_: &Request| {
            if seen.swap(true, Ordering::SeqCst) {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1, "value": [set_json(SCALE_SET_ID, &["velnor", "linux"])]
                }))
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "count": 0, "value": [] }))
            }
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(sets_path()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(set_json(SCALE_SET_ID, &["velnor", "linux"])),
        )
        .mount(server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{sets}/{SCALE_SET_ID}", sets = sets_path())))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(set_json(SCALE_SET_ID, &["velnor", "linux"])),
        )
        .mount(server)
        .await;
    let _ = looked_up;
}

/// Adopt-by-id: the set exists with drifted labels; PATCH reconciles.
async fn mount_set_adopt_with_drift(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("{sets}/{SCALE_SET_ID}", sets = sets_path())))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(set_json(SCALE_SET_ID, &["velnor", "stale-label"])),
        )
        .mount(server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("{sets}/{SCALE_SET_ID}", sets = sets_path())))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(set_json(SCALE_SET_ID, &["velnor", "linux"])),
        )
        .mount(server)
        .await;
}

/// Requests that deleted the scale set itself (never legal for the daemon;
/// session DELETEs and ACK DELETEs live under different paths).
async fn set_delete_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| {
            request.method == wiremock::http::Method::DELETE
                && request.url.path() == format!("{sets}/{SCALE_SET_ID}", sets = sets_path())
        })
        .count()
}

// ---------------------------------------------------------------------------
// Poll scripting: one closure dispatches the scripted poll sequence and
// records every poll's cursor + advertisement for assertions.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PollStep {
    status: u16,
    body: Option<String>,
}

#[derive(Debug, Clone)]
struct PollSeen {
    last_message_id: Option<String>,
    capacity: Option<String>,
}

#[derive(Debug, Default)]
struct PollScript {
    steps: VecDeque<PollStep>,
    seen: Vec<PollSeen>,
}

type SharedScript = Arc<Mutex<PollScript>>;

fn nil_step() -> PollStep {
    PollStep {
        status: 202,
        body: None,
    }
}

fn message_step(message_id: i32, batched: &[serde_json::Value]) -> PollStep {
    let body = batched
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    PollStep {
        status: 200,
        body: Some(
            serde_json::json!({
                "messageId": message_id,
                "messageType": "RunnerScaleSetJobMessages",
                "body": format!("[{body}]"),
                "statistics": {
                    "totalAvailableJobs": 1, "totalAcquiredJobs": 0,
                    "totalAssignedJobs": 2, "totalRunningJobs": 1,
                    "totalRegisteredRunners": 2, "totalBusyRunners": 1,
                    "totalIdleRunners": 1
                }
            })
            .to_string(),
        ),
    }
}

fn offer_message(request_id: i64) -> serde_json::Value {
    serde_json::json!({
        "messageType": "JobAvailable",
        "runnerRequestId": request_id,
        "repositoryName": "velnor",
        "ownerName": OWNER,
        "jobId": format!("job-{request_id}"),
        "jobWorkflowRef": format!(
            "{OWNER}/velnor/.github/workflows/ci.yml@refs/heads/main"
        ),
        "jobDisplayName": format!("job-{request_id}"),
        "workflowRunId": 99,
        "eventName": "push",
        "requestLabels": ["velnor"],
        "queueTime": "2026-09-17T00:00:00Z",
        "scaleSetAssignTime": "",
        "runnerAssignTime": "",
        "finishTime": "",
        "acquireJobUrl": "",
    })
}

fn assigned_message(request_id: i64) -> serde_json::Value {
    let mut message = offer_message(request_id);
    message["messageType"] = serde_json::json!("JobAssigned");
    message
}

fn started_message(request_id: i64) -> serde_json::Value {
    let mut message = offer_message(request_id);
    message["messageType"] = serde_json::json!("JobStarted");
    message["runnerId"] = serde_json::json!(4242);
    message["runnerName"] = serde_json::json!(format!("velnor-{SCALE_SET_ID}-{request_id}"));
    message
}

fn completed_message(request_id: i64) -> serde_json::Value {
    let mut message = started_message(request_id);
    message["messageType"] = serde_json::json!("JobCompleted");
    message["result"] = serde_json::json!("Succeeded");
    message["finishTime"] = serde_json::json!("2026-09-17T00:01:00Z");
    message
}

async fn mount_poll_script(server: &MockServer, script: SharedScript) {
    Mock::given(method("GET"))
        .and(path(queue_path()))
        .respond_with(move |request: &Request| {
            let mut script = script.lock().unwrap();
            let last_message_id = request
                .url
                .query_pairs()
                .find(|(key, _)| key == "lastMessageId")
                .map(|(_, value)| value.into_owned());
            let capacity = request
                .headers
                .get("X-ScaleSetMaxCapacity")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            script.seen.push(PollSeen {
                last_message_id,
                capacity,
            });
            match script.steps.pop_front().unwrap_or_else(nil_step) {
                PollStep { status: 202, .. } => ResponseTemplate::new(202),
                PollStep { status, body } => {
                    let mut template = ResponseTemplate::new(status);
                    if let Some(body) = body {
                        template = template.set_body_string(body);
                    }
                    template
                }
            }
        })
        .mount(server)
        .await;
}

async fn mount_acks(server: &MockServer) {
    Mock::given(method("DELETE"))
        .and(path_regex(r"/messages/session-1/[0-9]+$"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

async fn mount_acquire(server: &MockServer, returned: &[i64]) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": returned.len(), "value": returned
        })))
        .mount(server)
        .await;
}

async fn mount_jit(server: &MockServer, blob: &str) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/generatejitconfig"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "runner": {
                "id": 4242,
                "name": format!("velnor-{SCALE_SET_ID}-{REQUEST_ID}"),
                "runnerScaleSetId": SCALE_SET_ID
            },
            "encodedJITConfig": blob
        })))
        .mount(server)
        .await;
}

// ---------------------------------------------------------------------------
// FakeDocker: a stateful Engine double. Creates/starts/inspects/logs/stops/
// removes converge like the real daemon; every argv is recorded for
// assertions (adopt-without-recreate, no stray host-socket binds, ...).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FakeMount {
    name: String,
    destination: String,
    driver: String,
    read_write: bool,
}

#[derive(Debug, Clone)]
struct FakeContainer {
    id: String,
    image: String,
    entrypoint: Option<Vec<String>>,
    command: Option<Vec<String>>,
    network_mode: String,
    /// Network ID captured when the container joined its network. A later
    /// same-name network replacement must not rewrite this attachment.
    network_attachment_id: Option<String>,
    status: String,
    labels: Vec<(String, String)>,
    mounts: Vec<FakeMount>,
    volumes_from: Vec<String>,
    connected_marker: bool,
    logs_text: String,
}

#[derive(Debug, Clone)]
struct FakeNetwork {
    id: String,
    labels: Vec<(String, String)>,
}

#[derive(Debug, Default)]
struct FakeEngine {
    containers: HashMap<String, FakeContainer>,
    networks: HashMap<String, FakeNetwork>,
    seen: Vec<Vec<String>>,
    /// `--env-file` bytes captured at `create` time, by container name.
    /// The blob's disk lifetime ends right after create, so
    /// capture-while-present is the only way to assert what the runner
    /// received.
    env_files: HashMap<String, String>,
    /// DinD readiness probe answers ready (else `docker exec` exits 1).
    probe_ready: bool,
    /// `rm` of these containers exits 1 (cleanup-failure injection).
    fail_rm: Vec<String>,
    /// Anonymous volumes created by holder `--mount type=volume` entries.
    volumes: HashSet<String>,
    next_id: u64,
    next_volume_id: u64,
    next_network_id: u64,
}

#[derive(Debug, Clone, Default)]
struct FakeDocker {
    engine: Arc<Mutex<FakeEngine>>,
}

impl FakeDocker {
    fn new() -> Self {
        let engine = FakeEngine {
            probe_ready: true,
            ..FakeEngine::default()
        };
        Self {
            engine: Arc::new(Mutex::new(engine)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeEngine> {
        self.engine.lock().unwrap()
    }

    /// Argv fragments matching `fragment` seen so far.
    fn seen_matching(&self, fragment: &str) -> Vec<Vec<String>> {
        self.lock()
            .seen
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg.contains(fragment)))
            .cloned()
            .collect()
    }

    fn creates(&self) -> Vec<Vec<String>> {
        self.lock()
            .seen
            .iter()
            .filter(|argv| argv.first().is_some_and(|head| head == "create"))
            .cloned()
            .collect()
    }

    fn commands_since(&self, offset: usize) -> Vec<Vec<String>> {
        self.lock().seen.iter().skip(offset).cloned().collect()
    }

    fn seen_len(&self) -> usize {
        self.lock().seen.len()
    }

    fn rm_commands(&self) -> Vec<Vec<String>> {
        self.lock()
            .seen
            .iter()
            .filter(|argv| argv.first().is_some_and(|head| head == "rm"))
            .cloned()
            .collect()
    }

    fn volume_commands(&self) -> Vec<Vec<String>> {
        self.lock()
            .seen
            .iter()
            .filter(|argv| argv.first().is_some_and(|head| head == "volume"))
            .cloned()
            .collect()
    }

    fn container_id(&self, container: &str) -> Option<String> {
        self.lock()
            .containers
            .get(container)
            .map(|entry| entry.id.clone())
    }

    fn mounts_for(&self, container: &str) -> Vec<FakeMount> {
        self.lock()
            .containers
            .get(container)
            .unwrap_or_else(|| panic!("container {container} must exist"))
            .mounts
            .clone()
    }

    fn volumes_from_for(&self, container: &str) -> Vec<String> {
        self.lock()
            .containers
            .get(container)
            .unwrap_or_else(|| panic!("container {container} must exist"))
            .volumes_from
            .clone()
    }

    fn anonymous_volume_count(&self) -> usize {
        self.lock().volumes.len()
    }

    fn stop_container(&self, container: &str) {
        if let Some(entry) = self.lock().containers.get_mut(container) {
            entry.status = "exited".to_owned();
        }
    }

    fn drop_container(&self, container: &str) {
        self.lock().containers.remove(container);
    }

    fn fail_rm(&self, container: &str) {
        self.lock().fail_rm.push(container.to_owned());
    }

    fn has_container(&self, container: &str) -> bool {
        self.lock().containers.contains_key(container)
    }

    /// `--env-file` bytes captured when `container` was created.
    fn env_file_for(&self, container: &str) -> Option<String> {
        self.lock().env_files.get(container).cloned()
    }

    /// Recreate a network under the same name with a new Engine identity.
    /// Existing container attachments retain the old ID, matching Docker's
    /// object identity semantics and making restart-attestation drift real.
    fn recreate_network(&self, name: &str) {
        let mut engine = self.lock();
        let labels = engine
            .networks
            .get(name)
            .unwrap_or_else(|| panic!("network {name} must exist before recreation"))
            .labels
            .clone();
        let id = allocate_network_id(&mut engine);
        engine
            .networks
            .insert(name.to_owned(), FakeNetwork { id, labels });
    }
}

fn allocate_network_id(engine: &mut FakeEngine) -> String {
    engine.next_network_id += 1;
    if engine.next_network_id == 1 {
        // Preserve the original fixture's first-network output.
        "fake-net-id".to_owned()
    } else {
        format!("fake-net-id-{:04}", engine.next_network_id)
    }
}

fn ok(stdout: &str) -> WorkerOutput {
    WorkerOutput {
        code: 0,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

fn missing(kind: &str, name: &str) -> WorkerOutput {
    WorkerOutput {
        code: 1,
        stdout: String::new(),
        stderr: format!("Error: No such {kind}: {name}"),
    }
}

fn target_name(args: &[String]) -> String {
    // Every lane call puts the target after a `--` separator.
    args.iter()
        .skip_while(|arg| arg.as_str() != "--")
        .nth(1)
        .cloned()
        .unwrap_or_default()
}

fn resolve_container_name(engine: &FakeEngine, target: &str) -> Option<String> {
    if engine.containers.contains_key(target) {
        return Some(target.to_owned());
    }
    engine
        .containers
        .iter()
        .find(|(_, entry)| entry.id == target)
        .map(|(name, _)| name.clone())
}

fn resolve_network_name(engine: &FakeEngine, target: &str) -> Option<String> {
    if engine.networks.contains_key(target) {
        return Some(target.to_owned());
    }
    engine
        .networks
        .iter()
        .find(|(_, network)| network.id == target)
        .map(|(name, _)| name.clone())
}

fn container_role(labels: &[(String, String)]) -> Option<&str> {
    labels
        .iter()
        .find(|(key, _)| key == WORKER_ROLE_LABEL)
        .map(|(_, value)| value.as_str())
}

fn anonymous_mount(engine: &mut FakeEngine, spec: &str) -> FakeMount {
    let fields: HashMap<_, _> = spec
        .split(',')
        .filter_map(|field| field.split_once('='))
        .collect();
    assert_eq!(fields.get("type").copied(), Some("volume"), "{spec}");
    assert!(
        fields.get("target").is_some(),
        "anonymous mount has no target: {spec}"
    );
    assert!(
        !fields.contains_key("source")
            && !fields.contains_key("src")
            && !fields.contains_key("volume"),
        "holder mount must be anonymous: {spec}"
    );
    let destination = fields.get("target").unwrap().to_string();
    engine.next_volume_id += 1;
    let name = format!("fake-anonymous-volume-{:04}", engine.next_volume_id);
    assert!(
        engine.volumes.insert(name.clone()),
        "duplicate fake volume {name}"
    );
    FakeMount {
        name,
        destination,
        driver: "local".to_owned(),
        read_write: true,
    }
}

fn bind_mount(spec: &str) -> FakeMount {
    let (source, destination) = spec
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("bind mount has no destination: {spec}"));
    FakeMount {
        name: source.to_owned(),
        destination: destination.to_owned(),
        driver: "local".to_owned(),
        read_write: true,
    }
}

fn mounts_json(mounts: &[FakeMount]) -> serde_json::Value {
    serde_json::json!(mounts
        .iter()
        .map(|mount| {
            serde_json::json!({
                "Type": "volume",
                "Name": mount.name,
                "Destination": mount.destination,
                "Driver": mount.driver,
                "RW": mount.read_write,
            })
        })
        .collect::<Vec<_>>())
}

impl WorkerRunner for FakeDocker {
    fn run(&mut self, program: &str, args: &[String]) -> anyhow::Result<WorkerOutput> {
        assert_eq!(program, "docker");
        let mut engine = self.lock();
        engine.seen.push(args.to_vec());
        let head = args.first().cloned().unwrap_or_default();
        match head.as_str() {
            "create" => {
                let name = args
                    .iter()
                    .skip_while(|arg| arg.as_str() != "--name")
                    .nth(1)
                    .cloned()
                    .unwrap_or_else(|| format!("unnamed-{}", engine.next_id));
                let mut labels = Vec::new();
                let mut rest: &[String] = args;
                while let Some(at) = rest.iter().position(|arg| arg == "--label") {
                    if let Some(pair) = rest.get(at + 1)
                        && let Some((key, value)) = pair.split_once('=')
                    {
                        labels.push((key.to_owned(), value.to_owned()));
                    }
                    rest = &rest[at + 1..];
                }
                engine.next_id += 1;
                let id = format!("fake-id-{:04}", engine.next_id);
                let separator = args
                    .iter()
                    .position(|arg| arg == "--")
                    .expect("container create has an image separator");
                let image = args
                    .get(separator + 1)
                    .cloned()
                    .expect("container create has an image");
                let command = match args.get(separator + 2..) {
                    Some(command) if !command.is_empty() => Some(command.to_vec()),
                    _ => None,
                };
                let network_mode = args
                    .iter()
                    .position(|arg| arg == "--network")
                    .and_then(|at| args.get(at + 1))
                    .cloned()
                    .unwrap_or_default();
                let network_attachment_id = engine
                    .networks
                    .get(&network_mode)
                    .map(|network| network.id.clone());
                let volumes_from = args
                    .iter()
                    .enumerate()
                    .filter_map(|(at, arg)| {
                        (arg == "--volumes-from")
                            .then(|| args.get(at + 1).cloned())
                            .flatten()
                    })
                    .collect::<Vec<_>>();
                let mut mounts = Vec::new();
                for source in &volumes_from {
                    let source_name = resolve_container_name(&engine, source)
                        .unwrap_or_else(|| panic!("--volumes-from target {source} is absent"));
                    let source_entry = engine
                        .containers
                        .get(&source_name)
                        .expect("resolved --volumes-from source");
                    assert_eq!(
                        container_role(&source_entry.labels),
                        Some(ROLE_VOLUME_HOLDER),
                        "worker containers inherit only the holder's volumes"
                    );
                    mounts.extend(source_entry.mounts.clone());
                }
                for (at, arg) in args.iter().enumerate() {
                    if arg == "--mount" {
                        let spec = args.get(at + 1).expect("--mount has a mount specification");
                        mounts.push(anonymous_mount(&mut engine, spec));
                    }
                    if arg == "--volume" {
                        let spec = args
                            .get(at + 1)
                            .expect("--volume has a volume specification");
                        mounts.push(bind_mount(spec));
                    }
                }
                if container_role(&labels) == Some(ROLE_VOLUME_HOLDER) {
                    assert_eq!(
                        mounts.len(),
                        3,
                        "holder must create exactly three anonymous mounts"
                    );
                    assert_eq!(
                        mounts
                            .iter()
                            .map(|mount| mount.destination.as_str())
                            .collect::<HashSet<_>>(),
                        HashSet::from([WORK_DIR, TOOL_CACHE_DIR, DIND_DATA_ROOT]),
                        "holder mount destinations"
                    );
                }
                // Capture `--env-file` bytes now: production deletes the
                // file right after create.
                if let Some(at) = args.iter().position(|arg| arg == "--env-file")
                    && let Some(path) = args.get(at + 1)
                    && let Ok(bytes) = std::fs::read_to_string(path)
                {
                    engine.env_files.insert(name.clone(), bytes);
                }
                engine.containers.insert(
                    name,
                    FakeContainer {
                        id,
                        image,
                        entrypoint: None,
                        command,
                        network_mode,
                        network_attachment_id,
                        status: "created".to_owned(),
                        labels,
                        mounts,
                        volumes_from,
                        connected_marker: false,
                        logs_text: "runner starting\n".to_owned(),
                    },
                );
                Ok(ok(&format!("fake-id-{:04}\n", engine.next_id)))
            }
            "start" => {
                let target = target_name(args);
                let name =
                    resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                match engine.containers.get_mut(&name) {
                    Some(entry)
                        if matches!(entry.status.as_str(), "created" | "exited" | "dead") =>
                    {
                        entry.status = "running".to_owned();
                        if container_role(&entry.labels) == Some(ROLE_RUNNER) {
                            entry.connected_marker = true;
                        }
                        Ok(ok(&format!("{}\n", entry.id)))
                    }
                    Some(entry) => Ok(WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: format!(
                            "Error: cannot start container {name} in state {}",
                            entry.status
                        ),
                    }),
                    None => Ok(missing("container", &name)),
                }
            }
            "exec" => {
                // Readiness probe: `exec <dind> docker -H unix://... version`.
                let target = args.get(1).cloned().unwrap_or_default();
                let name =
                    resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                if !engine.containers.contains_key(&name) {
                    return Ok(missing("container", &name));
                }
                if engine
                    .containers
                    .get(&name)
                    .is_some_and(|entry| entry.status != "running")
                {
                    return Ok(WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: format!("Error: container {name} is not running"),
                    });
                }
                if engine.probe_ready {
                    Ok(ok("28.5.2\n"))
                } else {
                    Ok(WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: "Cannot connect".to_owned(),
                    })
                }
            }
            "inspect" => {
                // `--format=` (running check) vs `--format <id|labels>`.
                if let Some(format) = args.iter().find_map(|arg| arg.strip_prefix("--format=")) {
                    let target = target_name(args);
                    let name =
                        resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                    return match engine.containers.get(&name) {
                        Some(entry) if format.contains(".State.Status") => {
                            Ok(ok(&format!("{}\n", entry.status)))
                        }
                        Some(entry) if format.contains(".State.Running") => {
                            Ok(ok(if entry.status == "running" {
                                "true\n"
                            } else {
                                "false\n"
                            }))
                        }
                        Some(_) => Ok(ok("")),
                        None => Ok(missing("container", &name)),
                    };
                }
                if args.iter().any(|arg| arg.contains("range")) {
                    let target = target_name(args);
                    let name =
                        resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                    return match engine.containers.get(&name) {
                        Some(entry) => {
                            let lines = entry
                                .labels
                                .iter()
                                .map(|(key, value)| format!("{key}={value}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            Ok(ok(&format!("{lines}\n")))
                        }
                        None => Ok(missing("container", &name)),
                    };
                }
                if let Some(format) = args
                    .iter()
                    .position(|arg| arg == "--format")
                    .and_then(|at| args.get(at + 1))
                {
                    let target = target_name(args);
                    let name =
                        resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                    return match engine.containers.get(&name) {
                        Some(entry) if format.contains(".Mounts") => {
                            let entrypoint = serde_json::to_string(&entry.entrypoint)
                                .expect("entrypoint serializes");
                            let command =
                                serde_json::to_string(&entry.command).expect("command serializes");
                            let labels = entry
                                .labels
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect::<HashMap<_, _>>();
                            Ok(ok(&format!(
                                "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                                serde_json::to_string(&entry.id).expect("id serializes"),
                                serde_json::to_string(&entry.image).expect("image serializes"),
                                serde_json::to_string(&labels).expect("labels serialize"),
                                entrypoint,
                                command,
                                serde_json::to_string(&entry.status).expect("status serializes"),
                                mounts_json(&entry.mounts),
                            )))
                        }
                        Some(entry) if format.contains(".HostConfig.NetworkMode") => {
                            let entrypoint = serde_json::to_string(&entry.entrypoint)
                                .expect("entrypoint serializes");
                            let command =
                                serde_json::to_string(&entry.command).expect("command serializes");
                            let labels = entry
                                .labels
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect::<HashMap<_, _>>();
                            if format.contains(".NetworkSettings.Networks") {
                                let mut networks = serde_json::Map::new();
                                if let Some(network_id) = &entry.network_attachment_id {
                                    networks.insert(
                                        entry.network_mode.clone(),
                                        serde_json::json!({"NetworkID": network_id}),
                                    );
                                }
                                Ok(ok(&format!(
                                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                                    serde_json::to_string(&entry.image).expect("image serializes"),
                                    serde_json::to_string(&labels).expect("labels serialize"),
                                    serde_json::to_string(&entry.network_mode)
                                        .expect("network mode serializes"),
                                    serde_json::to_string(&entry.volumes_from)
                                        .expect("volumes-from serializes"),
                                    serde_json::to_string(&networks).expect("networks serialize"),
                                    serde_json::to_string(&entry.status)
                                        .expect("status serializes")
                                )))
                            } else {
                                Ok(ok(&format!(
                                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                                    serde_json::to_string(&entry.image).expect("image serializes"),
                                    serde_json::to_string(&labels).expect("labels serialize"),
                                    serde_json::to_string(&entry.network_mode)
                                        .expect("network mode serializes"),
                                    serde_json::to_string(&entry.volumes_from)
                                        .expect("volumes-from serializes"),
                                    entrypoint,
                                    command,
                                    serde_json::to_string(&entry.status)
                                        .expect("status serializes")
                                )))
                            }
                        }
                        Some(entry)
                            if format.contains(".Id") && format.contains(".Config.Labels") =>
                        {
                            let labels = entry
                                .labels
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone()))
                                .collect::<HashMap<_, _>>();
                            Ok(ok(&format!(
                                "{}\t{}\n",
                                serde_json::to_string(&entry.id).expect("id serializes"),
                                serde_json::to_string(&labels).expect("labels serialize"),
                            )))
                        }
                        Some(entry) if format.contains(".Id") => Ok(ok(&format!("{}\n", entry.id))),
                        Some(entry) if format.contains(".Config.Image") => {
                            // Deliberately mirror only the production's
                            // non-secret projection. Never expose Config.Env:
                            // it may contain the one-shot JIT credential.
                            let entrypoint = serde_json::to_string(&entry.entrypoint)
                                .expect("entrypoint serializes");
                            let command =
                                serde_json::to_string(&entry.command).expect("command serializes");
                            Ok(ok(&format!(
                                "{}\t{}\t{}\t{}\n",
                                serde_json::to_string(&entry.image).expect("image serializes"),
                                entrypoint,
                                command,
                                serde_json::to_string(&entry.status).expect("status serializes")
                            )))
                        }
                        Some(entry) if format.contains(".State.Status") => {
                            Ok(ok(&format!("{}\n", entry.status)))
                        }
                        Some(_) => Ok(ok("")),
                        None => Ok(missing("container", &name)),
                    };
                }
                if args.len() == 3 {
                    // Bare `inspect -- <name>` (diagnostic capture).
                    let target = target_name(args);
                    let name =
                        resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                    return match engine.containers.get(&name) {
                        Some(entry) => Ok(ok(&format!(
                            "[{{\"Id\":\"{}\",\"Name\":\"{name}\"}}]",
                            entry.id
                        ))),
                        None => Ok(missing("container", &name)),
                    };
                }
                // `inspect --format {{.Id}} -- <name>` (adoption lookup).
                let target = target_name(args);
                let name =
                    resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                match engine.containers.get(&name) {
                    Some(entry) => Ok(ok(&format!("{}\n", entry.id))),
                    None => Ok(missing("container", &name)),
                }
            }
            "logs" => {
                let target = target_name(args);
                let name =
                    resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                match engine.containers.get(&name) {
                    Some(entry) => {
                        let mut text = entry.logs_text.clone();
                        if entry.connected_marker {
                            text.push_str("Connected to GitHub\n");
                        }
                        Ok(ok(&text))
                    }
                    None => Ok(missing("container", &name)),
                }
            }
            "stop" => {
                let target = target_name(args);
                let name =
                    resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                match engine.containers.get_mut(&name) {
                    Some(entry) => {
                        entry.status = "exited".to_owned();
                        Ok(ok(&format!("{}\n", entry.id)))
                    }
                    None => Ok(missing("container", &name)),
                }
            }
            "rm" => {
                let target = target_name(args);
                let name =
                    resolve_container_name(&engine, &target).unwrap_or_else(|| target.clone());
                if engine
                    .fail_rm
                    .iter()
                    .any(|failed| failed == &target || failed == &name)
                {
                    return Ok(WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: "Error: removal failed: device busy".to_owned(),
                    });
                }
                let force = args.iter().any(|arg| arg == "--force" || arg == "-f");
                if let Some(entry) = engine.containers.get(&name)
                    && matches!(entry.status.as_str(), "running" | "paused" | "restarting")
                    && !force
                {
                    return Ok(WorkerOutput {
                        code: 1,
                        stdout: String::new(),
                        stderr: format!(
                            "Error: cannot remove running container {name} without --force"
                        ),
                    });
                }
                let volumes = args.iter().any(|arg| arg == "--volumes");
                if volumes {
                    let entry = engine
                        .containers
                        .get(&name)
                        .unwrap_or_else(|| panic!("volume cleanup target {name} is absent"));
                    assert_eq!(
                        container_role(&entry.labels),
                        Some(ROLE_VOLUME_HOLDER),
                        "only the holder may receive --volumes"
                    );
                    if engine.containers.values().any(|container| {
                        container.volumes_from.iter().any(|source| source == &name)
                    }) {
                        return Ok(WorkerOutput {
                            code: 1,
                            stdout: String::new(),
                            stderr: "Error: volume is still in use by a worker container"
                                .to_owned(),
                        });
                    }
                    let mount_names = entry
                        .mounts
                        .iter()
                        .map(|mount| mount.name.clone())
                        .collect::<Vec<_>>();
                    for mount in mount_names {
                        engine.volumes.remove(&mount);
                    }
                }
                match engine.containers.remove(&name) {
                    Some(entry) => Ok(ok(&format!("{}\n", entry.id))),
                    None => Ok(missing("container", &name)),
                }
            }
            "network" => {
                let verb = args.get(1).cloned().unwrap_or_default();
                match verb.as_str() {
                    "inspect" => {
                        let target = target_name(args);
                        let name = resolve_network_name(&engine, &target)
                            .unwrap_or_else(|| target.clone());
                        if let Some(format) = args
                            .iter()
                            .position(|arg| arg == "--format")
                            .and_then(|at| args.get(at + 1))
                        {
                            return match engine.networks.get(&name) {
                                Some(network) if format.contains(".Driver") => {
                                    let labels = network
                                        .labels
                                        .iter()
                                        .map(|(key, value)| (key.clone(), value.clone()))
                                        .collect::<HashMap<_, _>>();
                                    Ok(ok(&format!(
                                        "{}\t{}\t{}\n",
                                        serde_json::to_string(&network.id)
                                            .expect("network id serializes"),
                                        serde_json::to_string("bridge").expect("driver serializes"),
                                        serde_json::to_string(&labels)
                                            .expect("network labels serialize")
                                    )))
                                }
                                Some(network) if format.contains(".Id") => {
                                    Ok(ok(&format!("{}\n", network.id)))
                                }
                                Some(_) => Ok(ok("")),
                                None => Ok(missing("network", &name)),
                            };
                        }
                        if args.iter().any(|arg| arg.contains("range")) {
                            return match engine.networks.get(&name) {
                                Some(network) => {
                                    let lines = network
                                        .labels
                                        .iter()
                                        .map(|(key, value)| format!("{key}={value}"))
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    Ok(ok(&format!("{lines}\n")))
                                }
                                None => Ok(missing("network", &name)),
                            };
                        }
                        match engine.networks.get(&name) {
                            Some(network) => Ok(ok(&format!("{}\n", network.id))),
                            None => Ok(missing("network", &name)),
                        }
                    }
                    "create" => {
                        let name = target_name(args);
                        let mut labels = Vec::new();
                        let mut rest: &[String] = args;
                        while let Some(at) = rest.iter().position(|arg| arg == "--label") {
                            if let Some(pair) = rest.get(at + 1)
                                && let Some((key, value)) = pair.split_once('=')
                            {
                                labels.push((key.to_owned(), value.to_owned()));
                            }
                            rest = &rest[at + 1..];
                        }
                        let id = allocate_network_id(&mut engine);
                        engine.networks.insert(
                            name,
                            FakeNetwork {
                                id: id.clone(),
                                labels,
                            },
                        );
                        Ok(ok(&format!("{id}\n")))
                    }
                    "rm" => {
                        let target = target_name(args);
                        let name = resolve_network_name(&engine, &target)
                            .unwrap_or_else(|| target.clone());
                        engine.networks.remove(&name);
                        Ok(ok(&format!("{name}\n")))
                    }
                    other => panic!("fake docker: unexpected network verb {other}"),
                }
            }
            "volume" => panic!("fake docker forbids name-based volume commands: {args:?}"),
            other => panic!("fake docker: unexpected verb {other} in {args:?}"),
        }
    }
}

fn assert_holder_contract(docker: &FakeDocker, identity: &WorkerIdentity) {
    let holder = identity.volume_holder_container();
    let dind = identity.dind_container();
    let runner = identity.runner_container();
    let creates = docker.creates();
    let holder_at = creates
        .iter()
        .position(|argv| argv.iter().any(|arg| arg == &holder))
        .expect("holder create recorded");
    let dind_at = creates
        .iter()
        .position(|argv| argv.iter().any(|arg| arg == &dind))
        .expect("DinD create recorded");
    let runner_at = creates
        .iter()
        .position(|argv| argv.iter().any(|arg| arg == &runner))
        .expect("runner create recorded");
    assert!(holder_at < dind_at && holder_at < runner_at);

    let holder_create = &creates[holder_at];
    let mount_specs = holder_create
        .iter()
        .enumerate()
        .filter_map(|(at, arg)| (arg == "--mount").then(|| holder_create.get(at + 1)))
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        mount_specs.len(),
        3,
        "holder create must declare three mounts"
    );
    assert!(mount_specs.iter().all(|spec| {
        spec.starts_with("type=volume,target=")
            && !spec.contains("source=")
            && !spec.contains("src=")
            && !spec.contains("volume=")
    }));

    for (child, child_at) in [(&dind, dind_at), (&runner, runner_at)] {
        let child_create = &creates[child_at];
        let volumes_from_at = child_create
            .iter()
            .position(|arg| arg == "--volumes-from")
            .expect("worker create uses --volumes-from");
        assert_eq!(child_create.get(volumes_from_at + 1), Some(&holder));
    }

    let holder_mounts = docker.mounts_for(&holder);
    assert_eq!(holder_mounts.len(), 3);
    assert!(holder_mounts.iter().all(|mount| {
        mount.driver == "local" && mount.read_write && mount.name.starts_with("fake-anonymous-")
    }));
    assert_eq!(
        holder_mounts
            .iter()
            .map(|mount| mount.destination.clone())
            .collect::<HashSet<_>>(),
        HashSet::from([
            WORK_DIR.to_owned(),
            TOOL_CACHE_DIR.to_owned(),
            DIND_DATA_ROOT.to_owned(),
        ])
    );
    for child in [&dind, &runner] {
        assert_eq!(docker.volumes_from_for(child), vec![holder.clone()]);
        let mounts = docker.mounts_for(child);
        for destination in [WORK_DIR, TOOL_CACHE_DIR, DIND_DATA_ROOT] {
            assert!(
                mounts.iter().any(|mount| mount.destination == destination),
                "{child} did not inherit {destination}"
            );
        }
    }
}

fn assert_holder_cleanup_uses_immutable_id(docker: &FakeDocker, identity: &WorkerIdentity) {
    let holder = identity.volume_holder_container();
    let holder_rms = docker
        .rm_commands()
        .into_iter()
        .filter(|argv| argv.iter().any(|arg| arg == "--volumes"))
        .collect::<Vec<_>>();
    assert!(!holder_rms.is_empty(), "holder cleanup was not attempted");
    assert!(holder_rms.iter().all(|argv| {
        let target = target_name(argv);
        argv.iter().any(|arg| arg == "--force")
            && target != holder
            && target.starts_with("fake-id-")
    }));
    assert!(
        docker.volume_commands().is_empty(),
        "worker cleanup must not use docker volume rm"
    );
}

#[test]
fn fake_docker_models_missing_inspects_and_lifecycle_gates() {
    let mut docker = FakeDocker::new();
    let call = |docker: &mut FakeDocker, args: &[&str]| {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        docker.run("docker", &args).unwrap()
    };
    let container = "fake-container";

    assert_eq!(
        call(
            &mut docker,
            &["create", "--name", container, "--", "runner:image"]
        )
        .code,
        0
    );
    let present = call(
        &mut docker,
        &["inspect", "--format", "{{.Id}}", "--", container],
    );
    assert_eq!(present.code, 0);
    assert_eq!(present.stdout, "fake-id-0001\n");

    let absent = call(
        &mut docker,
        &["inspect", "--format", "{{.Id}}", "--", "missing-container"],
    );
    assert_eq!(absent.code, 1);
    assert!(absent.stderr.contains("No such container"));

    let not_ready = call(&mut docker, &["exec", container, "docker", "version"]);
    assert_eq!(not_ready.code, 1);
    assert!(not_ready.stderr.contains("not running"));

    assert_eq!(
        call(&mut docker, &["start", "--", container]).code,
        0,
        "created containers can start"
    );
    assert_eq!(
        call(&mut docker, &["exec", container, "docker", "version"],).code,
        0,
        "readiness succeeds only while running"
    );
    let start_running = call(&mut docker, &["start", "--", container]);
    assert_eq!(start_running.code, 1);
    assert!(start_running.stderr.contains("cannot start"));

    assert_eq!(call(&mut docker, &["stop", "--", container]).code, 0);
    assert_eq!(
        call(&mut docker, &["start", "--", container]).code,
        0,
        "exited containers can restart"
    );
    docker.lock().containers.get_mut(container).unwrap().status = "dead".to_owned();
    assert_eq!(
        call(&mut docker, &["start", "--", container]).code,
        0,
        "dead containers can restart"
    );

    let non_force_rm = call(&mut docker, &["rm", "--", container]);
    assert_eq!(non_force_rm.code, 1);
    assert!(non_force_rm.stderr.contains("--force"));
    assert_eq!(
        call(&mut docker, &["rm", "--force", "--", container]).code,
        0,
        "force removal is allowed for running containers"
    );

    let mut network = call(
        &mut docker,
        &[
            "network",
            "inspect",
            "--format",
            "{{.Id}}",
            "--",
            "missing-network",
        ],
    );
    assert_eq!(network.code, 1);
    assert!(network.stderr.contains("No such network"));
    assert_eq!(
        call(&mut docker, &["network", "create", "--", "fake-network"]).code,
        0
    );
    network = call(
        &mut docker,
        &[
            "network",
            "inspect",
            "--format",
            "{{.Id}}",
            "--",
            "fake-network",
        ],
    );
    assert_eq!(network.code, 0);
    assert_eq!(network.stdout, "fake-net-id\n");
}

/// Tool-content hook double: attests every image (content proof is the
/// production hook's job, covered by d1b + the live canary).
#[derive(Debug, Clone, Copy, Default)]
struct ScriptHook;

impl ToolContentHook for ScriptHook {
    fn verify(
        &self,
        _runner: &mut dyn WorkerRunner,
        image: &PinnedImage,
        expected: &ToolContentExpectation,
    ) -> anyhow::Result<ToolContentAttestation> {
        Ok(ToolContentAttestation {
            reference: image.reference(),
            image_id: "sha256:fake".to_owned(),
            content_version: expected.content_version.clone(),
            source: expected.source.clone(),
        })
    }

    fn verify_platform(
        &self,
        _runner: &mut dyn WorkerRunner,
        _image: &PinnedImage,
        _expected: &velnor_runner::scaleset::worker::ImagePlatform,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn verify_attestation(
        &self,
        _runner: &mut dyn WorkerRunner,
        _image: &PinnedImage,
        _expected: &ToolContentExpectation,
        _attestation: &ToolContentAttestation,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn verify_signature(
        &self,
        _runner: &mut dyn WorkerRunner,
        _image: &PinnedImage,
        _expected: &ToolContentExpectation,
        _attestation: &ToolContentAttestation,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Daemon config writer
// ---------------------------------------------------------------------------

fn write_config(
    dir: &std::path::Path,
    server: &MockServer,
    pat_env: &str,
    extra: &str,
) -> std::path::PathBuf {
    let path = dir.join("scaleset.toml");
    let body = format!(
        "scope_url = \"{}/octo-org\"\n\
         owner = \"{OWNER}\"\n\
         group_name = \"{GROUP_NAME}\"\n\
         set_name = \"{SET_NAME}\"\n\
         labels = [\"velnor\", \"linux\"]\n\
         ready_attempts = 2\n\
         sweep_interval_secs = 3600\n\
         poll_timeout_secs = 5\n\
         nil_delay_secs = 0\n\
         [admission]\n\
         owner = [\"{OWNER}\"]\n\
         repository = [\"{OWNER}/velnor\"]\n\
         ref = [\"main\"]\n\
         source = [\"{OWNER}/velnor\"]\n\
         workflow = [\".github/workflows/ci.yml\"]\n\
         event = [\"push\"]\n\
         {extra}\n\
         [auth.pat]\n\
         token_env = \"{pat_env}\"\n",
        server.uri(),
    );
    std::fs::write(&path, body).unwrap();
    path
}

fn test_pat_env(test: &str) -> String {
    format!(
        "VELNOR_TEST_DAEMON_PAT_{}_{}_{}",
        test.to_uppercase(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn profile_images() -> ProvisionImages {
    let profile = HomogeneousProfile::host().unwrap();
    ProvisionImages {
        runner_digest: profile.runner().digest().to_owned(),
        dind_digest: profile.dind().digest().to_owned(),
    }
}

fn configure_ledger(path: &std::path::Path, max_jobs: u32) {
    let mut ledger = PermitLedger::open(path).unwrap();
    ledger.set_max_jobs(max_jobs).unwrap();
    ledger.begin_epoch().unwrap();
    ledger.reconcile(&[]).unwrap();
}

// ---------------------------------------------------------------------------
// Registration: adopt / create / reconcile / fail closed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_get_or_create_then_reconciles_labels() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    mount_group_lookup(&server).await;
    mount_set_get_or_create(&server).await;

    let plan = RegistrationPlan {
        group_id: None,
        group_name: Some(GROUP_NAME.to_owned()),
        set_id: None,
        set_name: Some(SET_NAME.to_owned()),
        labels: vec!["velnor".to_owned(), "linux".to_owned()],
    };
    let created = velnor_runner::scaleset::reconcile_registration(&client, &plan)
        .await
        .unwrap();
    assert_eq!(created.group_id, GROUP_ID);
    assert_eq!(created.group_name, GROUP_NAME);
    assert_eq!(created.set.id, SCALE_SET_ID);
    assert!(created.created);
    assert!(!created.labels_updated);

    // Second pass adopts the existing set; labels already match.
    let adopted = velnor_runner::scaleset::reconcile_registration(&client, &plan)
        .await
        .unwrap();
    assert!(!adopted.created);
    assert!(!adopted.labels_updated);
    assert_eq!(set_delete_calls(&server).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_adopt_by_id_patches_drifted_labels() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    mount_group_lookup(&server).await;
    mount_set_adopt_with_drift(&server).await;

    let plan = RegistrationPlan {
        group_id: Some(GROUP_ID),
        group_name: None,
        set_id: Some(SCALE_SET_ID),
        set_name: None,
        labels: vec!["velnor".to_owned(), "linux".to_owned()],
    };
    let reconciled = velnor_runner::scaleset::reconcile_registration(&client, &plan)
        .await
        .unwrap();
    assert!(!reconciled.created);
    assert!(
        reconciled.labels_updated,
        "drifted live labels must be patched back"
    );
    // The PATCH carried the desired label set.
    let patches: Vec<_> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.method == wiremock::http::Method::PATCH)
        .map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            body["labels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|label| label["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0], vec!["velnor".to_owned(), "linux".to_owned()]);
    assert_eq!(set_delete_calls(&server).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_adopt_by_id_fails_closed_on_group_drift() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    mount_group_lookup(&server).await;
    // Live set sits in group 3; the plan pins group 9: adopting must
    // refuse to move it, not silently re-home the set.
    Mock::given(method("GET"))
        .and(path(format!("{sets}/{SCALE_SET_ID}", sets = sets_path())))
        .respond_with(ResponseTemplate::new(200).set_body_json(set_json(SCALE_SET_ID, &["velnor"])))
        .mount(&server)
        .await;

    let plan = RegistrationPlan {
        group_id: Some(9),
        group_name: None,
        set_id: Some(SCALE_SET_ID),
        set_name: None,
        labels: vec!["velnor".to_owned()],
    };
    let error = velnor_runner::scaleset::reconcile_registration(&client, &plan)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("not the configured group"),
        "must name the drift: {error:#}"
    );
    assert_eq!(set_delete_calls(&server).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_create_race_adopts_instead_of_failing() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    mount_group_lookup(&server).await;
    // Lookup misses, create loses a same-name race (409), re-read adopts.
    let looked_up = Arc::new(AtomicBool::new(false));
    let seen = looked_up.clone();
    Mock::given(method("GET"))
        .and(path(sets_path()))
        .respond_with(move |_: &Request| {
            if seen.swap(true, Ordering::SeqCst) {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "count": 1, "value": [set_json(SCALE_SET_ID, &["velnor", "linux"])]
                }))
            } else {
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "count": 0, "value": [] }))
            }
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(sets_path()))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "typeName": "RunnerExistsError", "message": "already exists"
        })))
        .mount(&server)
        .await;

    let plan = RegistrationPlan {
        group_id: Some(GROUP_ID),
        group_name: None,
        set_id: None,
        set_name: Some(SET_NAME.to_owned()),
        labels: vec!["velnor".to_owned(), "linux".to_owned()],
    };
    let reconciled = velnor_runner::scaleset::reconcile_registration(&client, &plan)
        .await
        .unwrap();
    assert_eq!(reconciled.set.id, SCALE_SET_ID);
    assert!(
        !reconciled.created,
        "the race winner's set is adopted, not re-created"
    );
    assert_eq!(set_delete_calls(&server).await, 0);
}

// ---------------------------------------------------------------------------
// End to end: one job through the real daemon, lane, and ledger
// ---------------------------------------------------------------------------

fn daemon_defaults(dir: &std::path::Path) -> (DaemonDefaults, std::path::PathBuf) {
    let state_db = dir.join("state.db");
    let ledger = dir.join("permit-ledger.db");
    (
        DaemonDefaults {
            state_db: state_db.clone(),
            ledger_path: ledger.clone(),
            config_dir: dir.to_owned(),
        },
        ledger,
    )
}

async fn session_close_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| {
            request.method == wiremock::http::Method::DELETE
                && request.url.path().contains("/sessions/")
        })
        .count()
}

async fn jit_calls(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/generatejitconfig"))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_serves_one_job_end_to_end() {
    let server = MockServer::start().await;
    mount_token_chain(&server).await;
    mount_group_lookup(&server).await;
    mount_set_get_or_create(&server).await;
    mount_session_create(&server, "queue-token-1").await;
    mount_session_close(&server, 204).await;
    mount_acks(&server).await;
    mount_acquire(&server, &[REQUEST_ID]).await;
    mount_jit(&server, "jit-blob-e2e").await;

    let script: SharedScript = Arc::new(Mutex::new(PollScript {
        steps: VecDeque::from([
            message_step(20, &[offer_message(REQUEST_ID)]),
            message_step(21, &[assigned_message(REQUEST_ID)]),
            message_step(22, &[started_message(REQUEST_ID)]),
            message_step(23, &[completed_message(REQUEST_ID)]),
        ]),
        seen: Vec::new(),
    }));
    mount_poll_script(&server, script.clone()).await;

    let dir = temp_root("e2e");
    let pat_env = test_pat_env("e2e");
    unsafe { std::env::set_var(&pat_env, "test-pat") };
    let config = write_config(&dir, &server, &pat_env, "");
    let (defaults, ledger_path) = daemon_defaults(&dir);
    configure_ledger(&ledger_path, 4);
    let docker = FakeDocker::new();

    let mut daemon = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    let started = daemon.start().await.unwrap();
    assert_eq!(started.scale_set_id, SCALE_SET_ID);
    assert_eq!(started.session_id, SESSION_ID);

    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let task = tokio::spawn(async move { daemon.run(&flag).await });

    // Wait for the terminal state to converge, then stop the loop.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let demand = DemandStore::open(&defaults.state_db).unwrap();
        let terminal = demand
            .get(SCALE_SET_ID, REQUEST_ID)
            .unwrap()
            .is_some_and(|row| row.state == DemandState::Terminal);
        let ledger = SharedLedger::open(&ledger_path).unwrap();
        if terminal && ledger.occupied().unwrap() == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job did not converge in 30s"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    shutdown.store(true, Ordering::SeqCst);
    let report = task.await.unwrap().unwrap();

    assert_eq!(report.scale_set_id, SCALE_SET_ID);
    assert_eq!(report.metrics.acks, 4);
    assert_eq!(report.shutdown_report.recorded_total, 0);

    // Cursor advanced across all four messages; capacity advertised free
    // headroom from the shared ledger (4 with one held, then free again).
    let seen = script.lock().unwrap().seen.clone();
    assert!(seen.len() >= 4, "polls: {seen:?}");
    assert_eq!(seen[0].last_message_id, None);
    assert_eq!(seen[1].last_message_id.as_deref(), Some("20"));
    assert_eq!(seen[2].last_message_id.as_deref(), Some("21"));
    assert_eq!(seen[3].last_message_id.as_deref(), Some("22"));
    assert_eq!(seen[0].capacity.as_deref(), Some("4"));

    let cursors = SessionStore::open(&defaults.state_db).unwrap();
    assert_eq!(
        cursors.get(SCALE_SET_ID).unwrap().unwrap().last_message_id,
        23
    );

    // Exactly one worker pair plus its holder were created; the JIT blob reached the
    // runner via its one-shot `--env-file` (deleted right after create)
    // and never appeared in any argv; no host socket ever entered an argv.
    let creates = docker.creates();
    assert_eq!(
        creates.len(),
        3,
        "one holder + one dind + one runner: {creates:?}"
    );
    let runner = runner_container_for(REQUEST_ID);
    let runner_create = creates
        .iter()
        .find(|argv| argv.iter().any(|arg| arg == &runner))
        .expect("runner create recorded");
    assert!(
        runner_create.iter().any(|arg| arg == "--env-file"),
        "JIT travels via --env-file, never argv: {runner_create:?}"
    );
    let runner_image = HomogeneousProfile::host().unwrap().runner().reference();
    assert_eq!(
        runner_create
            .get(runner_create.len().saturating_sub(2))
            .map(String::as_str),
        Some(runner_image.as_str()),
        "the pinned runner image precedes the final command: {runner_create:?}"
    );
    assert_eq!(
        runner_create.last().map(String::as_str),
        Some("/home/runner/run.sh"),
        "the official runner launcher is the final command: {runner_create:?}"
    );
    assert_eq!(
        docker.env_file_for(&runner).as_deref(),
        Some("ACTIONS_RUNNER_INPUT_JITCONFIG=jit-blob-e2e\n"),
        "JIT blob reached the runner env"
    );
    assert!(
        docker.seen_matching("jit-blob-e2e").is_empty(),
        "JIT blob must never appear in an argv"
    );
    assert!(
        docker.seen_matching("/var/run/docker.sock").is_empty(),
        "host socket must never enter a job argv"
    );
    assert!(
        docker.seen_matching("tcp://").is_empty(),
        "no TCP surface in worker argv"
    );

    // Diagnostics were exported before deletion; objects are gone.
    let slug = slug_for(REQUEST_ID);
    let runner_log = dir
        .join("scaleset-workers")
        .join(&slug)
        .join("diagnostics")
        .join("runner.log");
    assert!(
        runner_log.is_file(),
        "diagnostics exported before deletion: {}",
        runner_log.display()
    );
    assert!(
        !docker.has_container(&runner_container_for(REQUEST_ID))
            && !docker.has_container(&dind_container_for(REQUEST_ID))
            && !docker.has_container(&identity_for(REQUEST_ID).volume_holder_container()),
        "owned cleanup removed the holder and both worker containers"
    );
    assert_eq!(docker.anonymous_volume_count(), 0);
    assert_holder_cleanup_uses_immutable_id(&docker, &identity_for(REQUEST_ID));

    // Single JIT fetch, clean session close, set never deleted.
    assert_eq!(jit_calls(&server).await, 1);
    assert_eq!(session_close_calls(&server).await, 1);
    assert_eq!(set_delete_calls(&server).await, 0);

    unsafe { std::env::remove_var(&pat_env) };
}

// ---------------------------------------------------------------------------
// Restart: adopt live work without re-provisioning, resume the cursor,
// preserve demand age, never delete the set.
// ---------------------------------------------------------------------------

async fn wait_for(
    db: &std::path::Path,
    ledger_path: &std::path::Path,
    what: &str,
    mut done: impl FnMut(&DemandStore, &SharedLedger) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let demand = DemandStore::open(db).unwrap();
        let ledger = SharedLedger::open(ledger_path).unwrap();
        if done(&demand, &ledger) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what} did not converge in 30s"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_adopts_live_worker_without_reprovision() {
    let server = MockServer::start().await;
    mount_token_chain(&server).await;
    mount_group_lookup(&server).await;
    mount_set_get_or_create(&server).await;
    mount_session_create(&server, "queue-token-1").await;
    mount_session_close(&server, 204).await;
    mount_acks(&server).await;
    mount_acquire(&server, &[REQUEST_ID]).await;
    mount_jit(&server, "jit-blob-restart").await;

    // Phase 1 serves the offer through provisioning, then idles: the job
    // is "running" (no completion yet) when the daemon stops.
    let script1: SharedScript = Arc::new(Mutex::new(PollScript {
        steps: VecDeque::from([message_step(20, &[offer_message(REQUEST_ID)])]),
        seen: Vec::new(),
    }));
    mount_poll_script(&server, script1.clone()).await;

    let dir = temp_root("restart");
    let pat_env = test_pat_env("restart");
    unsafe { std::env::set_var(&pat_env, "test-pat") };
    let config = write_config(&dir, &server, &pat_env, "");
    let (defaults, ledger_path) = daemon_defaults(&dir);
    configure_ledger(&ledger_path, 4);
    let docker = FakeDocker::new();

    let mut daemon = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    daemon.start().await.unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let task = tokio::spawn(async move { daemon.run(&flag).await });

    wait_for(
        &defaults.state_db,
        &ledger_path,
        "provision",
        |demand, _| {
            demand
                .get(SCALE_SET_ID, REQUEST_ID)
                .unwrap()
                .is_some_and(|row| row.state == DemandState::ProvisionIntent)
        },
    )
    .await;
    let creates_phase1 = docker.creates().len();
    assert_eq!(creates_phase1, 3);
    let identity = identity_for(REQUEST_ID);
    assert_holder_contract(&docker, &identity);
    let first_seen = DemandStore::open(&defaults.state_db)
        .unwrap()
        .get(SCALE_SET_ID, REQUEST_ID)
        .unwrap()
        .unwrap()
        .first_seen_at
        .clone();
    shutdown.store(true, Ordering::SeqCst);
    let report1 = task.await.unwrap().unwrap();
    assert_eq!(report1.shutdown_report.adopted_across_restart, 1);
    assert_eq!(report1.shutdown_report.recorded_total, 1);
    // Containers keep running; the permit stays held; demand is untouched.
    assert_eq!(
        SharedLedger::open(&ledger_path)
            .unwrap()
            .occupied()
            .unwrap(),
        1
    );
    assert_eq!(
        DemandStore::open(&defaults.state_db)
            .unwrap()
            .get(SCALE_SET_ID, REQUEST_ID)
            .unwrap()
            .unwrap()
            .state,
        DemandState::ProvisionIntent
    );

    // Phase 2: same DBs, same containers (a restarted daemon process).
    // Production bumps the epoch at startup; the lane's first poll
    // reconciles before advertising.
    PermitLedger::open(&ledger_path)
        .unwrap()
        .begin_epoch()
        .unwrap();
    let phase2_start = script1.lock().unwrap().seen.len();
    script1
        .lock()
        .unwrap()
        .steps
        .push_back(message_step(23, &[completed_message(REQUEST_ID)]));

    let docker_before_restart = docker.seen_len();
    let mut daemon2 = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    let started2 = daemon2.start().await.unwrap();
    assert_eq!(started2.adopt_report.adopted, 1);
    assert_eq!(started2.adopt_report.failed, 0);
    assert_eq!(
        docker.creates().len(),
        creates_phase1,
        "adoption must not re-create containers"
    );
    let restart_commands = docker.commands_since(docker_before_restart);
    let holder_attest = restart_commands
        .iter()
        .position(|argv| {
            argv.first().is_some_and(|head| head == "inspect")
                && argv.iter().any(|arg| arg.contains(".Mounts"))
        })
        .expect("restart attests holder");
    let network_attest = restart_commands
        .iter()
        .position(|argv| {
            argv.first().is_some_and(|head| head == "network")
                && argv.get(1).is_some_and(|verb| verb == "inspect")
        })
        .expect("restart attests network");
    assert!(
        holder_attest < network_attest,
        "restart must attest holder before network/worker pair: {restart_commands:?}"
    );
    let shutdown2 = Arc::new(AtomicBool::new(false));
    let flag2 = shutdown2.clone();
    let task2 = tokio::spawn(async move { daemon2.run(&flag2).await });

    wait_for(
        &defaults.state_db,
        &ledger_path,
        "completion",
        |demand, ledger| {
            demand
                .get(SCALE_SET_ID, REQUEST_ID)
                .unwrap()
                .is_some_and(|row| row.state == DemandState::Terminal)
                && ledger.occupied().unwrap() == 0
        },
    )
    .await;
    shutdown2.store(true, Ordering::SeqCst);
    let report2 = task2.await.unwrap().unwrap();
    assert_eq!(report2.shutdown_report.recorded_total, 0);

    // Cursor resumed (first phase-2 poll carried lastMessageId=20, not a
    // fresh 0); demand age survived the restart; still no re-provision.
    let seen = script1.lock().unwrap().seen.clone();
    assert!(
        seen.len() > phase2_start,
        "phase 2 must have polled at least once"
    );
    assert_eq!(seen[phase2_start].last_message_id.as_deref(), Some("20"));
    assert_eq!(
        DemandStore::open(&defaults.state_db)
            .unwrap()
            .get(SCALE_SET_ID, REQUEST_ID)
            .unwrap()
            .unwrap()
            .first_seen_at,
        first_seen,
        "redelivery retains original age across restarts"
    );
    assert_eq!(
        docker.creates().len(),
        creates_phase1,
        "completion must not re-provision either"
    );
    assert_holder_cleanup_uses_immutable_id(&docker, &identity);
    assert_eq!(jit_calls(&server).await, 1);
    // Three session DELETEs: phase-1 shutdown closes its session, phase-2
    // startup best-effort reaps the stored prior session id before creating
    // the new session (avoids a 409 session conflict on restart), and
    // phase-2 shutdown closes its own session.
    assert_eq!(
        session_close_calls(&server).await,
        3,
        "shutdown close per phase plus phase-2 startup reap of the prior session"
    );
    // The reap DELETE must land before phase 2 creates its replacement
    // session: it is startup cleanup, not a shutdown double close.
    let requests = server.received_requests().await.unwrap();
    let mut creates_seen = 0;
    let mut deletes_seen = 0;
    let mut reap_before_recreate = false;
    for request in &requests {
        let path = request.url.path().to_string();
        if request.method == wiremock::http::Method::POST && path.ends_with("/sessions") {
            creates_seen += 1;
        }
        if request.method == wiremock::http::Method::DELETE && path.contains("/sessions/") {
            deletes_seen += 1;
            if creates_seen == 1 && deletes_seen == 2 {
                reap_before_recreate = true;
            }
        }
    }
    assert_eq!(creates_seen, 2, "one session create per phase");
    assert!(
        reap_before_recreate,
        "phase-2 startup must reap the prior session before creating its own"
    );
    assert_eq!(set_delete_calls(&server).await, 0);

    unsafe { std::env::remove_var(&pat_env) };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_rejects_recreated_network_attachment() {
    let server = MockServer::start().await;
    mount_token_chain(&server).await;
    mount_group_lookup(&server).await;
    mount_set_get_or_create(&server).await;
    mount_session_create(&server, "queue-token-1").await;
    mount_session_close(&server, 204).await;
    mount_acks(&server).await;
    mount_acquire(&server, &[REQUEST_ID]).await;
    mount_jit(&server, "jit-blob-network-mismatch").await;

    let script: SharedScript = Arc::new(Mutex::new(PollScript {
        steps: VecDeque::from([message_step(20, &[offer_message(REQUEST_ID)])]),
        seen: Vec::new(),
    }));
    mount_poll_script(&server, script).await;

    let dir = temp_root("network-mismatch");
    let pat_env = test_pat_env("network-mismatch");
    unsafe { std::env::set_var(&pat_env, "test-pat") };
    let config = write_config(&dir, &server, &pat_env, "");
    let (defaults, ledger_path) = daemon_defaults(&dir);
    configure_ledger(&ledger_path, 4);
    let docker = FakeDocker::new();

    let mut daemon = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    daemon.start().await.unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let task = tokio::spawn(async move { daemon.run(&flag).await });

    wait_for(
        &defaults.state_db,
        &ledger_path,
        "provision before network replacement",
        |demand, _| {
            demand
                .get(SCALE_SET_ID, REQUEST_ID)
                .unwrap()
                .is_some_and(|row| row.state == DemandState::ProvisionIntent)
        },
    )
    .await;
    shutdown.store(true, Ordering::SeqCst);
    let report = task.await.unwrap().unwrap();
    assert_eq!(report.shutdown_report.adopted_across_restart, 1);
    assert_eq!(docker.creates().len(), 3);
    assert_holder_contract(&docker, &identity_for(REQUEST_ID));

    let network_name = identity_for(REQUEST_ID).network();
    docker.recreate_network(&network_name);
    PermitLedger::open(&ledger_path)
        .unwrap()
        .begin_epoch()
        .unwrap();

    let mut daemon2 = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    let error = daemon2.start().await.unwrap_err();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("network attachment mismatch"),
        "replacement must fail restart attestation: {rendered}"
    );
    assert_eq!(
        docker.creates().len(),
        3,
        "attestation mismatch must not re-create either container"
    );
    assert!(docker.has_container(&runner_container_for(REQUEST_ID)));
    assert!(docker.has_container(&dind_container_for(REQUEST_ID)));
    assert_eq!(jit_calls(&server).await, 1);
    assert_eq!(set_delete_calls(&server).await, 0);

    unsafe { std::env::remove_var(&pat_env) };
}

// ---------------------------------------------------------------------------
// Crash: dead work is failed explicitly — never lost, never false success.
// Worker A dies with its objects present (recorded `running`): adoption
// fails it with diagnostics + cleanup and releases the permit, while its
// demand row stays non-terminal (only GitHub completes demand). Worker B
// vanishes entirely: adoption cannot tick it, the message path exhausts
// the DinD restart budget, and the permit is retained `uncertain` —
// visible, releasable by no one, lost to no one.
// ---------------------------------------------------------------------------

const REQUEST_B: i64 = 4245;

// Derived names always come from the production constructors: the slug
// format (hash-suffixed) is owned by `OwnershipId`, never by tests.
fn identity_for(request_id: i64) -> WorkerIdentity {
    WorkerIdentity::new(OwnershipId::bind(
        SCALE_SET_ID,
        &runner_name(SCALE_SET_ID, request_id),
    ))
}

fn ownership_key_for(request_id: i64) -> String {
    identity_for(request_id).ownership().as_str()
}

fn slug_for(request_id: i64) -> String {
    identity_for(request_id).ownership().slug()
}

fn runner_container_for(request_id: i64) -> String {
    identity_for(request_id).runner_container()
}

fn dind_container_for(request_id: i64) -> String {
    identity_for(request_id).dind_container()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_with_dead_workers_fails_explicitly() {
    let server = MockServer::start().await;
    mount_token_chain(&server).await;
    mount_group_lookup(&server).await;
    mount_set_get_or_create(&server).await;
    mount_session_create(&server, "queue-token-1").await;
    mount_session_close(&server, 204).await;
    mount_acks(&server).await;
    // Both offers acquire (stats headroom covers both).
    Mock::given(method("POST"))
        .and(path(format!(
            "/tenant/_apis/runtime/runnerscalesets/{SCALE_SET_ID}/acquirejobs"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 2, "value": [REQUEST_ID, REQUEST_B]
        })))
        .mount(&server)
        .await;
    mount_jit(&server, "jit-blob-crash").await;

    // Phase 1: both workers provision; A runs (assigned + started), B idles
    // at DinD-ready. No completions: the crash lands mid-flight.
    let script: SharedScript = Arc::new(Mutex::new(PollScript {
        steps: VecDeque::from([
            message_step(20, &[offer_message(REQUEST_ID), offer_message(REQUEST_B)]),
            message_step(21, &[assigned_message(REQUEST_ID)]),
            message_step(22, &[started_message(REQUEST_ID)]),
        ]),
        seen: Vec::new(),
    }));
    mount_poll_script(&server, script.clone()).await;

    let dir = temp_root("crash");
    let pat_env = test_pat_env("crash");
    unsafe { std::env::set_var(&pat_env, "test-pat") };
    let config = write_config(&dir, &server, &pat_env, "");
    let (defaults, ledger_path) = daemon_defaults(&dir);
    configure_ledger(&ledger_path, 4);
    let docker = FakeDocker::new();

    let mut daemon = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    daemon.start().await.unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let task = tokio::spawn(async move { daemon.run(&flag).await });

    wait_for(
        &defaults.state_db,
        &ledger_path,
        "two provisions",
        |_, ledger| ledger.occupied().unwrap() == 2 && docker.creates().len() == 6,
    )
    .await;
    assert_holder_contract(&docker, &identity_for(REQUEST_ID));
    assert_holder_contract(&docker, &identity_for(REQUEST_B));
    // A reached `running` (assigned + started observations applied).
    wait_for(&defaults.state_db, &ledger_path, "job started", |_, _| {
        WorkerRegistry::open(&defaults.state_db)
            .unwrap()
            .get(&ownership_key_for(REQUEST_ID))
            .unwrap()
            .is_some_and(|row| row.worker_state == velnor_model::ScaleSetWorkerState::Running)
    })
    .await;
    shutdown.store(true, Ordering::SeqCst);
    let report1 = task.await.unwrap().unwrap();
    assert_eq!(report1.shutdown_report.adopted_across_restart, 2);

    // The crash: A's runner dies (objects stay for forensics), B vanishes.
    docker.stop_container(&runner_container_for(REQUEST_ID));
    docker.drop_container(&runner_container_for(REQUEST_B));
    docker.drop_container(&dind_container_for(REQUEST_B));

    // Phase 2: adoption triages both without completing either.
    PermitLedger::open(&ledger_path)
        .unwrap()
        .begin_epoch()
        .unwrap();
    let prior_holders = PermitLedger::open(&ledger_path).unwrap().holders().unwrap();
    let scaleset_alive: Vec<(&str, PermitState)> = prior_holders
        .iter()
        .filter(|holder| holder.lane == PermitLane::ScaleSet)
        .map(|holder| (holder.holder.as_str(), holder.state))
        .collect();
    assert_eq!(
        scaleset_alive.len(),
        2,
        "phase-1 scale-set permits are reattested"
    );
    let (reconcile, swept) = velnor_runner::scaleset::allocator::startup_reconcile(
        &ledger_path,
        &scaleset_alive,
        &[],
        &|_| false,
    )
    .unwrap();
    assert_eq!(reconcile.confirmed.len(), 2);
    assert!(swept.is_empty());
    let mut daemon2 = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    let started2 = daemon2.start().await.unwrap();
    assert_eq!(started2.adopt_report.failed, 1, "A failed explicitly");
    assert_eq!(started2.adopt_report.adopted, 1, "B stays tracked");
    // A's permit released after confirmed cleanup; B's still held.
    let ledger = SharedLedger::open(&ledger_path).unwrap();
    assert_eq!(ledger.occupied().unwrap(), 1);
    assert!(
        ledger
            .holder_state(&format!("scaleset/{SCALE_SET_ID}/{REQUEST_B}"))
            .unwrap()
            .is_some(),
        "B's permit is retained, never freed blind"
    );
    // Neither demand row completed: no GitHub observation, no success.
    let demand = DemandStore::open(&defaults.state_db).unwrap();
    for request in [REQUEST_ID, REQUEST_B] {
        assert_ne!(
            demand.get(SCALE_SET_ID, request).unwrap().unwrap().state,
            DemandState::Terminal,
            "demand {request} must not complete without a GitHub observation"
        );
    }
    // A's diagnostics survived its death.
    assert!(
        dir.join("scaleset-workers")
            .join(slug_for(REQUEST_ID))
            .join("diagnostics")
            .join("runner.log")
            .is_file(),
        "A's logs were exported before deletion"
    );

    // Phase 2 run: completions arrive. B's terminal path retains its
    // permit uncertain on the first try (its evidence vanished with its
    // containers) and vetoes the ACK; the redelivered replays below prove
    // the uncertain terminal state is stable, not flapping.
    let completion = message_step(
        23,
        &[completed_message(REQUEST_ID), completed_message(REQUEST_B)],
    );
    {
        let mut script = script.lock().unwrap();
        for _ in 0..3 {
            script.steps.push_back(completion.clone());
        }
    }
    let shutdown2 = Arc::new(AtomicBool::new(false));
    let flag2 = shutdown2.clone();
    let task2 = tokio::spawn(async move { daemon2.run(&flag2).await });
    wait_for(
        &defaults.state_db,
        &ledger_path,
        "crash convergence",
        |demand, ledger| {
            demand
                .get(SCALE_SET_ID, REQUEST_B)
                .unwrap()
                .is_some_and(|row| row.state == DemandState::Terminal)
                && ledger.occupied().unwrap() == 1
        },
    )
    .await;
    shutdown2.store(true, Ordering::SeqCst);
    task2.await.unwrap().unwrap();

    // A: terminal demand (GitHub said so) + released permit + released row.
    // B: terminal demand (GitHub said so) + UNCERTAIN permit + cleanup row.
    let demand = DemandStore::open(&defaults.state_db).unwrap();
    assert_eq!(
        demand.get(SCALE_SET_ID, REQUEST_ID).unwrap().unwrap().state,
        DemandState::Terminal
    );
    assert_eq!(
        demand.get(SCALE_SET_ID, REQUEST_B).unwrap().unwrap().state,
        DemandState::Terminal
    );
    let ledger = SharedLedger::open(&ledger_path).unwrap();
    assert_eq!(ledger.occupied().unwrap(), 1);
    assert_eq!(
        ledger
            .holder_state(&format!("scaleset/{SCALE_SET_ID}/{REQUEST_B}"))
            .unwrap(),
        Some(velnor_runner::scaleset::LedgerPermitState::Uncertain),
        "vanished B keeps a visible uncertain reservation"
    );
    let registry = WorkerRegistry::open(&defaults.state_db).unwrap();
    let live: Vec<_> = registry
        .list_live()
        .unwrap()
        .iter()
        .map(|row| row.runner_name.clone())
        .collect();
    assert_eq!(
        live,
        vec![format!("velnor-{SCALE_SET_ID}-{REQUEST_B}")],
        "only B still occupies"
    );
    assert_eq!(set_delete_calls(&server).await, 0);
    assert_holder_cleanup_uses_immutable_id(&docker, &identity_for(REQUEST_ID));
    assert_holder_cleanup_uses_immutable_id(&docker, &identity_for(REQUEST_B));

    unsafe { std::env::remove_var(&pat_env) };
}

// ---------------------------------------------------------------------------
// Cleanup failure: when owned cleanup cannot confirm (a `docker rm` fails
// with objects present), the permit is retained `uncertain` — visible in
// the ledger, releasable by no one, lost to no one. Demand still completes
// (GitHub is the job-truth oracle); the worker record parks at
// `owned_cleanup` for the redelivered retry.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cleanup_failure_retains_permit_uncertain() {
    let server = MockServer::start().await;
    mount_token_chain(&server).await;
    mount_group_lookup(&server).await;
    mount_set_get_or_create(&server).await;
    mount_session_create(&server, "queue-token-1").await;
    mount_session_close(&server, 204).await;
    mount_acks(&server).await;
    mount_acquire(&server, &[REQUEST_ID]).await;
    mount_jit(&server, "jit-blob-rmfail").await;

    // The completion redelivers (the failed cleanup vetoes its ACK):
    // every replay retries the terminal path until it confirms.
    let completion = message_step(21, &[completed_message(REQUEST_ID)]);
    let script: SharedScript = Arc::new(Mutex::new(PollScript {
        steps: VecDeque::from([
            message_step(20, &[offer_message(REQUEST_ID)]),
            completion.clone(),
            completion.clone(),
            completion,
        ]),
        seen: Vec::new(),
    }));
    mount_poll_script(&server, script.clone()).await;

    let dir = temp_root("rmfail");
    let pat_env = test_pat_env("rmfail");
    unsafe { std::env::set_var(&pat_env, "test-pat") };
    let config = write_config(&dir, &server, &pat_env, "");
    let (defaults, ledger_path) = daemon_defaults(&dir);
    configure_ledger(&ledger_path, 4);
    let docker = FakeDocker::new();
    docker.fail_rm(&runner_container_for(REQUEST_ID));

    let mut daemon = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    daemon.start().await.unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let task = tokio::spawn(async move { daemon.run(&flag).await });

    wait_for(
        &defaults.state_db,
        &ledger_path,
        "failed cleanup",
        |demand, ledger| {
            demand
                .get(SCALE_SET_ID, REQUEST_ID)
                .unwrap()
                .is_some_and(|row| row.state == DemandState::Terminal)
                && ledger
                    .holder_state(&format!("scaleset/{SCALE_SET_ID}/{REQUEST_ID}"))
                    .unwrap()
                    .is_some_and(|state| {
                        state == velnor_runner::scaleset::LedgerPermitState::Uncertain
                    })
        },
    )
    .await;
    // Let at least one redelivery retry the terminal path before stopping.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let rms = docker
            .seen_matching(&runner_container_for(REQUEST_ID))
            .iter()
            .filter(|argv| argv.first().is_some_and(|head| head == "rm"))
            .count();
        if rms >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cleanup retry never ran in 30s"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    shutdown.store(true, Ordering::SeqCst);
    task.await.unwrap().unwrap();

    // Demand completed (GitHub's verdict stands); the permit is a visible
    // uncertain reservation; the worker parks at `owned_cleanup`; the
    // runner container still exists (its `rm` failed) while DinD and the
    // network were reclaimed. The completion was never ACKed (cursor
    // parked at 20) and the `rm` was retried on redelivery.
    let cursors = SessionStore::open(&defaults.state_db).unwrap();
    assert_eq!(
        cursors.get(SCALE_SET_ID).unwrap().unwrap().last_message_id,
        20,
        "failed cleanup vetoes the ACK"
    );
    let runner_rms = docker
        .seen_matching(&runner_container_for(REQUEST_ID))
        .iter()
        .filter(|argv| argv.first().is_some_and(|head| head == "rm"))
        .count();
    assert!(
        runner_rms >= 2,
        "redelivery must retry the failed rm (saw {runner_rms})"
    );
    let ledger = SharedLedger::open(&ledger_path).unwrap();
    assert_eq!(ledger.occupied().unwrap(), 1);
    let registry = WorkerRegistry::open(&defaults.state_db).unwrap();
    let row = registry
        .get(&ownership_key_for(REQUEST_ID))
        .unwrap()
        .unwrap();
    assert_eq!(
        row.worker_state,
        velnor_model::ScaleSetWorkerState::OwnedCleanup
    );
    assert!(docker.has_container(&runner_container_for(REQUEST_ID)));
    assert!(!docker.has_container(&dind_container_for(REQUEST_ID)));
    assert!(docker.has_container(&identity_for(REQUEST_ID).volume_holder_container()));
    assert_eq!(docker.anonymous_volume_count(), 3);
    assert_holder_cleanup_uses_immutable_id(&docker, &identity_for(REQUEST_ID));
    assert_eq!(set_delete_calls(&server).await, 0);

    unsafe { std::env::remove_var(&pat_env) };
}

// ---------------------------------------------------------------------------
// Capacity: one shared N across lanes; advertisements derive from it.
// With N=1 and a native holder occupying the ledger, the scale-set poll
// advertises 0, grants nothing spendable, and never calls `acquirejobs`.
// After the native release the redelivered offer acquires under the
// unified `scaleset/<set>/<request>` holder and provisions.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn capacity_shares_one_ledger_across_lanes() {
    let server = MockServer::start().await;
    let client = test_client(&server).await;
    mount_session_create(&server, "queue-token-1").await;
    mount_acks(&server).await;
    mount_acquire(&server, &[REQUEST_ID]).await;
    mount_jit(&server, "jit-blob-capacity").await;

    let script: SharedScript = Arc::new(Mutex::new(PollScript {
        steps: VecDeque::from([
            message_step(20, &[offer_message(REQUEST_ID)]),
            message_step(21, &[offer_message(REQUEST_ID)]),
        ]),
        seen: Vec::new(),
    }));
    mount_poll_script(&server, script.clone()).await;

    let dir = temp_root("capacity");
    let db = dir.join("state.db");
    let ledger_path = dir.join("permit-ledger.db");
    configure_ledger(&ledger_path, 1);

    // A native acquisition occupies the only permit.
    let mut raw = PermitLedger::open(&ledger_path).unwrap();
    let generation = raw.generation().unwrap();
    assert_eq!(
        raw.acquire(
            "native/broker-9",
            PermitLane::Native,
            PermitState::Running,
            generation,
            Some(std::process::id()),
        )
        .unwrap(),
        velnor_control::permit_ledger::AcquireOutcome::Acquired
    );
    drop(raw);

    let session = MessageSessionClient::create(&client, SCALE_SET_ID, OWNER)
        .await
        .unwrap();
    let queue = ClientSession::new(session);
    let docker = FakeDocker::new();
    let lane = DaemonWorkerLane::open(
        client.clone(),
        LaneConfig {
            scale_set_id: SCALE_SET_ID,
            profile: HomogeneousProfile::host().unwrap(),
            state_root: dir.join("workers"),
            ready_attempts: 2,
            sweep_interval: Duration::from_secs(3600),
        },
        &db,
        &ledger_path,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    let metrics = Metrics::new();
    let processor = Processor::new(
        queue.clone(),
        SharedLedger::open(&ledger_path).unwrap(),
        lane,
        DemandStore::open_with_admission(
            &db,
            OfferAdmission::exact(
                OWNER,
                &format!("{OWNER}/velnor"),
                "main",
                &format!("{OWNER}/velnor"),
                ".github/workflows/ci.yml",
                "push",
            ),
        )
        .unwrap(),
        AcquireBatchStore::open(&db).unwrap(),
        ProvisionIntentStore::open(&db).unwrap(),
        metrics.clone(),
        ProcessorConfig {
            scale_set_id: SCALE_SET_ID,
            images: profile_images(),
            max_acquire_batch: MAX_ACQUIRE_BATCH,
        },
    );
    let mut listener = Listener::new(
        queue,
        processor,
        SessionStore::open(&db).unwrap(),
        metrics.clone(),
        LoopConfig {
            scale_set_id: SCALE_SET_ID,
            retry: test_retry(),
            idle: IdlePolicy {
                nil_delay: Duration::from_millis(1),
            },
        },
    );

    // Poll 1: full ledger advertises 0; the offer queues (granted) but no
    // `acquirejobs` call goes out; the message still ACKs (the grant is
    // durable, so the offer survives the ACK).
    let outcome = listener.run_once().await.unwrap();
    assert!(outcome.acquired.is_empty());
    assert!(outcome.provisioned.is_empty());
    let seen = script.lock().unwrap().seen.clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].capacity.as_deref(), Some("0"));
    drop(seen);
    assert_eq!(
        DemandStore::open(&db)
            .unwrap()
            .get(SCALE_SET_ID, REQUEST_ID)
            .unwrap()
            .unwrap()
            .state,
        DemandState::Granted
    );
    let acquires = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/acquirejobs"))
        .count();
    assert_eq!(acquires, 0, "a full ledger must not acquire");
    assert_eq!(metrics.snapshot().acks, 1);

    // The native holder releases; the redelivered offer acquires under the
    // unified holder and provisions exactly one pair.
    PermitLedger::open(&ledger_path)
        .unwrap()
        .release("native/broker-9")
        .unwrap();
    let outcome = listener.run_once().await.unwrap();
    assert_eq!(outcome.acquired, vec![REQUEST_ID]);
    assert_eq!(outcome.provisioned, vec![REQUEST_ID]);
    let seen = script.lock().unwrap().seen.clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].capacity.as_deref(), Some("1"));
    drop(seen);
    let ledger = SharedLedger::open(&ledger_path).unwrap();
    assert_eq!(ledger.occupied().unwrap(), 1);
    assert!(
        ledger
            .holder_state(&format!("scaleset/{SCALE_SET_ID}/{REQUEST_ID}"))
            .unwrap()
            .is_some(),
        "scale-set holder uses the unified slash namespace"
    );
    assert_eq!(docker.creates().len(), 3);
    assert_holder_contract(&docker, &identity_for(REQUEST_ID));
    assert_eq!(metrics.snapshot().acks, 2);
    assert_eq!(set_delete_calls(&server).await, 0);
}

// ---------------------------------------------------------------------------
// Key material: the daemon refuses world-readable key files and empty
// sources, and opens cleanly on a PAT env reference (no network in open).
// ---------------------------------------------------------------------------

fn write_key_file(
    dir: &std::path::Path,
    name: &str,
    contents: &str,
    mode: u32,
) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    path
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_file_permissions_fail_daemon_open() {
    let server = MockServer::start().await;
    let dir = temp_root("keyperm");
    let (defaults, _) = daemon_defaults(&dir);
    let docker = FakeDocker::new();

    // World-readable key file: refused, path named, content never shown.
    let wide = write_key_file(&dir, "wide.pem", "secret-pem-body", 0o644);
    let config = dir.join("wide.toml");
    std::fs::write(
        &config,
        format!(
            "scope_url = \"{}/octo-org\"\n\
             owner = \"{OWNER}\"\n\
             group_name = \"{GROUP_NAME}\"\n\
             set_name = \"{SET_NAME}\"\n\
             ready_attempts = 2\n\
             sweep_interval_secs = 3600\n\
             poll_timeout_secs = 5\n\
             nil_delay_secs = 0\n\
             [admission]\n\
             owner = [\"{OWNER}\"]\n\
             repository = [\"{OWNER}/velnor\"]\n\
             ref = [\"main\"]\n\
             source = [\"{OWNER}/velnor\"]\n\
             workflow = [\".github/workflows/ci.yml\"]\n\
             event = [\"push\"]\n\
             [auth.app]\n\
             client_id = \"Iv1.abc\"\n\
             installation_id = 42\n\
             private_key_file = \"{}\"\n",
            server.uri(),
            wide.display(),
        ),
    )
    .unwrap();
    let error = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .err()
    .unwrap();
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains(&wide.display().to_string()),
        "names the offending file: {rendered}"
    );
    assert!(
        !rendered.contains("secret-pem-body"),
        "never leaks key content: {rendered}"
    );

    // Tight file, garbage key: refused at PEM parse, still silent on bytes.
    let tight = write_key_file(&dir, "tight.pem", "not-a-key", 0o600);
    let config = dir.join("tight.toml");
    std::fs::write(
        &config,
        format!(
            "scope_url = \"{}/octo-org\"\n\
             owner = \"{OWNER}\"\n\
             group_name = \"{GROUP_NAME}\"\n\
             set_name = \"{SET_NAME}\"\n\
             ready_attempts = 2\n\
             sweep_interval_secs = 3600\n\
             poll_timeout_secs = 5\n\
             nil_delay_secs = 0\n\
             [admission]\n\
             owner = [\"{OWNER}\"]\n\
             repository = [\"{OWNER}/velnor\"]\n\
             ref = [\"main\"]\n\
             source = [\"{OWNER}/velnor\"]\n\
             workflow = [\".github/workflows/ci.yml\"]\n\
             event = [\"push\"]\n\
             [auth.app]\n\
             client_id = \"Iv1.abc\"\n\
             installation_id = 42\n\
             private_key_file = \"{}\"\n",
            server.uri(),
            tight.display(),
        ),
    )
    .unwrap();
    let error = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .err()
    .unwrap();
    assert!(
        !format!("{error:#}").contains("not-a-key"),
        "parse errors never echo the key: {error:#}"
    );

    // PAT by env reference opens cleanly (open performs no network I/O).
    let pat_env = test_pat_env("keyperm");
    unsafe { std::env::set_var(&pat_env, "test-pat") };
    let config = write_config(&dir, &server, &pat_env, "");
    let daemon = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    assert!(!daemon.started());
    unsafe { std::env::remove_var(&pat_env) };
}

// ---------------------------------------------------------------------------
// Shutdown: a failed session close is best-effort — the lane still reports
// its triage and run() still succeeds.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_close_failure_is_best_effort() {
    let server = MockServer::start().await;
    mount_token_chain(&server).await;
    mount_group_lookup(&server).await;
    mount_set_get_or_create(&server).await;
    mount_session_create(&server, "queue-token-1").await;
    mount_session_close(&server, 500).await;

    let script: SharedScript = Arc::new(Mutex::new(PollScript {
        steps: VecDeque::new(),
        seen: Vec::new(),
    }));
    mount_poll_script(&server, script.clone()).await;

    let dir = temp_root("close-fail");
    let pat_env = test_pat_env("closefail");
    unsafe { std::env::set_var(&pat_env, "test-pat") };
    let config = write_config(&dir, &server, &pat_env, "");
    let (defaults, ledger_path) = daemon_defaults(&dir);
    configure_ledger(&ledger_path, 4);
    let docker = FakeDocker::new();

    let mut daemon = ScaleSetDaemon::open(
        &config,
        &defaults,
        Box::new(docker.clone()),
        Box::new(ScriptHook),
    )
    .unwrap();
    daemon.start().await.unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let flag = shutdown.clone();
    let task = tokio::spawn(async move { daemon.run(&flag).await });

    // One nil poll proves the loop is alive, then stop it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if !script.lock().unwrap().seen.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "loop never polled in 30s"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    shutdown.store(true, Ordering::SeqCst);
    let report = task.await.unwrap().unwrap();
    assert_eq!(report.scale_set_id, SCALE_SET_ID);
    assert_eq!(report.shutdown_report.recorded_total, 0);
    // Upstream `Close` rides the shared retryable client (`retryMax=4`,
    // `DefaultRetryPolicy` retries 500): a persistent 500 costs 1 + 4
    // attempts, then the failure stays best-effort.
    assert_eq!(session_close_calls(&server).await, 5);
    assert_eq!(set_delete_calls(&server).await, 0);

    unsafe { std::env::remove_var(&pat_env) };
}
