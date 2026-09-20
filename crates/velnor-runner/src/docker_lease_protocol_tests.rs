#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "protocol tests assert through panics"
)]

use super::*;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const TEST_JOB_ID: &str = "velnor-job-protocol-test";
const TEST_DAEMON_ID: &str = "velnor-daemon-protocol-test";
const TEST_NETWORK: &str = "velnor-net-protocol-test";
const TEST_CONTAINER_ID: &str = "buildkit-daemon-protocol-test";
const TEST_EXEC_ID: &str = "buildkit-workers-protocol-test";
const TEST_FAILED_EXEC_ID: &str = "buildkit-workers-failed-test";
const TEST_CONFIG: &[u8] = b"[registry.\"docker.io\"]\n  mirrors = [\"mirror.gcr.io\"]";
const TEST_TIMEOUT: Duration = Duration::from_secs(3);
static NEXT_FAKE_ENGINE_ID: AtomicUsize = AtomicUsize::new(0);

/// Local-only Engine endpoint. Tests pass this socket path directly to the
/// proxy; no test creates or connects to a Docker daemon socket.
struct FakeEngine {
    listener: UnixListener,
    socket_path: PathBuf,
    directory: PathBuf,
}

impl FakeEngine {
    fn bind() -> Self {
        let directory = PathBuf::from("/tmp").join(format!(
            "vlp-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FAKE_ENGINE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let socket_path = directory.join("e.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        Self {
            listener,
            socket_path,
            directory,
        }
    }

    fn reply_once(&self, response: Vec<u8>) -> JoinHandle<Vec<u8>> {
        self.reply_once_after(response, Duration::ZERO)
    }

    fn reply_once_after(&self, response: Vec<u8>, delay: Duration) -> JoinHandle<Vec<u8>> {
        let listener = self.listener.try_clone().unwrap();
        std::thread::spawn(move || {
            let (mut engine, _) = listener.accept().unwrap();
            engine.set_read_timeout(Some(TEST_TIMEOUT)).unwrap();
            let request = read_http_request(&mut engine).unwrap();
            let request = request.bytes;
            std::thread::sleep(delay);
            engine.write_all(&response).unwrap();
            request
        })
    }

    fn assert_no_connection(&self) {
        self.listener.set_nonblocking(true).unwrap();
        let result = self.listener.accept();
        self.listener.set_nonblocking(false).unwrap();
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("checking fake Engine listener: {error}"),
            Ok((stream, _)) => {
                drop(stream);
                panic!("denied request reached the fake Engine")
            }
        }
    }
}

impl Drop for FakeEngine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn fixture(phase: crate::buildkit::BuilderBootstrapPhase) -> (FakeEngine, Arc<DockerLeasePolicy>) {
    let engine = FakeEngine::bind();
    let policy = Arc::new(DockerLeasePolicy::new(TEST_JOB_ID).unwrap());
    policy.enable_test_buildkit_engine();
    policy.set_job_network(TEST_NETWORK).unwrap();

    let builder = format!(
        "{}{}",
        crate::buildkit::PERSISTENT_BUILDER_PREFIX,
        "a".repeat(64)
    );
    let state_volume = crate::buildkit::daemon_state_volume(&builder);
    let admission = PersistentBuildKitAdmission {
        builder: builder.clone(),
        state_volume: state_volume.clone(),
        owner_token: "owner-token-protocol-test".to_owned(),
        container_id: Some(TEST_CONTAINER_ID.to_owned()),
        network_reconciled: false,
        approved_config: TEST_CONFIG.to_vec(),
        bootstrap_phase: phase,
        readiness_attempts: 0,
        archive_in_flight: false,
    };
    let mut resources = policy.resources.lock().unwrap();
    resources
        .admitted_buildkit_daemons
        .insert(crate::buildkit::daemon_container_name(&builder), admission);
    resources.admitted_buildkit_volumes.insert(state_volume);
    drop(resources);

    (engine, policy)
}

fn proxy_request(
    engine_path: PathBuf,
    policy: Arc<DockerLeasePolicy>,
    request: Vec<u8>,
) -> (Vec<u8>, anyhow::Result<()>) {
    proxy_request_with_test_storage(engine_path, policy, request, None)
}

fn proxy_request_with_test_storage(
    engine_path: PathBuf,
    policy: Arc<DockerLeasePolicy>,
    request: Vec<u8>,
    test_storage: Option<crate::storage::StorageLayout>,
) -> (Vec<u8>, anyhow::Result<()>) {
    let (mut guest, proxy) = UnixStream::pair().unwrap();
    guest.set_read_timeout(Some(TEST_TIMEOUT)).unwrap();
    let worker = std::thread::spawn(move || {
        let _storage = test_storage.map(crate::buildkit::use_test_storage_layout);
        handle_client_with(
            proxy,
            &engine_path,
            TEST_JOB_ID,
            TEST_DAEMON_ID,
            LeaseConnSet::new(Arc::new(AtomicBool::new(false))),
            policy,
        )
    });
    guest.write_all(&request).unwrap();
    if request_is_upgrade(&request) {
        guest.shutdown(std::net::Shutdown::Write).unwrap();
    }
    let mut response = Vec::new();
    guest.read_to_end(&mut response).unwrap();
    (response, worker.join().unwrap())
}

fn http_request(method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut request = format!(
        "{method} {target} HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n",
        body.len()
    )
    .into_bytes();
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("connection"))
    {
        request.extend_from_slice(b"Connection: close\r\n");
    }
    for (name, value) in headers {
        request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(body);
    request
}

fn http_response(status: u16, reason: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn response_status(response: &[u8], status: u16) {
    let response = String::from_utf8_lossy(response);
    assert!(
        response.starts_with(&format!("HTTP/1.1 {status} ")),
        "expected HTTP {status}, got {response:?}"
    );
}

fn assert_denied(response: &[u8], result: anyhow::Result<()>, status: u16) {
    response_status(response, status);
    let error = result.unwrap_err();
    assert_eq!(
        error.downcast_ref::<LeaseDeny>().map(|deny| deny.status),
        Some(status),
        "unexpected proxy error: {error:#}"
    );
}

fn current_admission(policy: &DockerLeasePolicy) -> PersistentBuildKitAdmission {
    policy
        .persistent_buildkit_container_admission(TEST_CONTAINER_ID)
        .unwrap()
        .unwrap()
}

fn buildkit_archive(config: &[u8]) -> Vec<u8> {
    fn set_octal(field: &mut [u8], value: usize) {
        let digits = format!("{value:o}");
        assert!(digits.len() < field.len());
        field.fill(b'0');
        let end = field.len() - 1;
        let start = end - digits.len();
        field[start..end].copy_from_slice(digits.as_bytes());
        field[end] = 0;
    }

    fn append_entry(archive: &mut Vec<u8>, path: &str, kind: u8, mode: usize, content: &[u8]) {
        let mut header = [0_u8; 512];
        assert!(path.len() < 100);
        header[..path.len()].copy_from_slice(path.as_bytes());
        set_octal(&mut header[100..108], mode);
        set_octal(&mut header[108..116], 0);
        set_octal(&mut header[116..124], 0);
        set_octal(&mut header[124..136], content.len());
        set_octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        set_octal(&mut header[329..337], 0);
        set_octal(&mut header[337..345], 0);
        let checksum = header.iter().map(|byte| usize::from(*byte)).sum::<usize>();
        let checksum_field = format!("{checksum:06o}\0 ");
        assert_eq!(checksum_field.len(), 8);
        header[148..156].copy_from_slice(checksum_field.as_bytes());
        archive.extend_from_slice(&header);
        archive.extend_from_slice(content);
        let padded_len = content.len().div_ceil(512) * 512;
        archive.resize(archive.len() + padded_len - content.len(), 0);
    }

    let mut archive = Vec::new();
    append_entry(&mut archive, "buildkit/", b'5', 0o755, b"");
    append_entry(&mut archive, "buildkit/buildkitd.toml", b'0', 0o644, config);
    archive.resize(archive.len() + 1024, 0);
    archive
}

fn archive_request(body: &[u8]) -> Vec<u8> {
    archive_request_for(TEST_CONTAINER_ID, body)
}

fn archive_request_for(container_id: &str, body: &[u8]) -> Vec<u8> {
    http_request(
        "PUT",
        &format!("/v1.43/containers/{container_id}/archive?path=%2Fetc&noOverwriteDirNonDir=true"),
        &[],
        body,
    )
}

fn exec_create_request(command: &[&str]) -> Vec<u8> {
    let value = serde_json::json!({
        "AttachStdin": true,
        "AttachStdout": true,
        "AttachStderr": true,
        "Cmd": command,
        "DetachKeys": "",
        "Env": null,
        "Privileged": false,
        "Tty": false,
        "User": "",
        "WorkingDir": ""
    });
    http_request(
        "POST",
        &format!("/v1.43/containers/{TEST_CONTAINER_ID}/exec"),
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(&value).unwrap(),
    )
}

fn exec_attach_request(exec_id: &str) -> Vec<u8> {
    http_request(
        "POST",
        &format!("/v1.43/exec/{exec_id}/start"),
        &[
            ("Content-Type", "application/json"),
            ("Connection", "Upgrade"),
            ("Upgrade", "tcp"),
        ],
        br#"{"Detach":false,"Tty":false}"#,
    )
}

#[test]
fn buildkit_archive_accepts_normalized_buildx_tar_without_content_type_and_tracks_status() {
    let (engine, policy) = fixture(crate::buildkit::BuilderBootstrapPhase::Created);

    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        archive_request(b""),
    );
    assert_denied(&response, result, 403);
    engine.assert_no_connection();
    let admission = current_admission(&policy);
    assert_eq!(
        admission.bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Created
    );
    assert!(!admission.archive_in_flight);

    let archive = buildkit_archive(TEST_CONFIG);
    assert_eq!(ustar_string(&archive[..100]).unwrap(), "buildkit/");
    assert_eq!(
        ustar_string(&archive[512..612]).unwrap(),
        "buildkit/buildkitd.toml"
    );
    let failed_request = engine.reply_once(http_response(500, "Engine Error", b"archive failed"));
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        archive_request(&archive),
    );
    result.unwrap();
    response_status(&response, 500);
    let forwarded = failed_request.join().unwrap();
    assert!(String::from_utf8_lossy(&forwarded).contains(&format!(
        "PUT /v1.43/containers/{TEST_CONTAINER_ID}/archive?path=%2Fetc&noOverwriteDirNonDir=true HTTP/1.1"
    )));
    assert!(!docker_request_headers(&forwarded)
        .unwrap()
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type")));
    assert_eq!(docker_request_body(&forwarded).unwrap(), archive);
    let admission = current_admission(&policy);
    assert_eq!(
        admission.bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Created
    );
    assert!(!admission.archive_in_flight);

    let truncated_request = engine.reply_once(
        b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nshort".to_vec(),
    );
    let (_, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        archive_request(&archive),
    );
    assert!(
        result.is_err(),
        "truncated Engine response must fail closed"
    );
    assert!(truncated_request.join().unwrap().starts_with(b"PUT "));
    let admission = current_admission(&policy);
    assert_eq!(
        admission.bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Created
    );
    assert!(
        !admission.archive_in_flight,
        "truncated Engine response must release the archive reservation"
    );

    let successful_request = engine.reply_once(http_response(200, "OK", b""));
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        archive_request(&archive),
    );
    result.unwrap();
    response_status(&response, 200);
    let forwarded = successful_request.join().unwrap();
    assert_eq!(docker_request_body(&forwarded).unwrap(), archive);
    assert!(!archive.is_empty());
    let admission = current_admission(&policy);
    assert_eq!(
        admission.bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Archived
    );
    assert!(!admission.archive_in_flight);

    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        archive_request(&archive),
    );
    assert_denied(&response, result, 403);
    engine.assert_no_connection();
    assert_eq!(
        current_admission(&policy).bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Archived
    );
}

#[test]
fn successful_buildkit_archive_phase_survives_policy_restart() {
    let engine = FakeEngine::bind();
    let layout = crate::storage::StorageLayout::from_prefix(&engine.directory.join("storage"));
    let run_root = layout.run_root.clone();
    let worker_layout = layout.clone();
    let _storage = crate::buildkit::use_test_storage_layout(layout);
    let _containers =
        crate::buildkit::use_test_running_container_names(std::collections::BTreeSet::new());
    let builder = crate::buildkit::persistent_builder_name(
        "durable-archive",
        "trusted",
        crate::buildkit::TRUST_TIER_BRANCH,
        Some("o/r"),
    );
    let required = [TEST_JOB_ID.to_string()];
    crate::buildkit::claim_builder_bounded(
        &run_root,
        &builder,
        "slot-durable-archive",
        TEST_JOB_ID,
        "trusted",
        crate::buildkit::TRUST_TIER_BRANCH,
        Some("o/r"),
        &required,
        true,
    )
    .unwrap();

    let container_id = "a".repeat(64);
    let config_digest = sha256_hex(TEST_CONFIG);
    let owner_token = crate::buildkit::seed_test_admitted_builder_bootstrap_node(
        &builder,
        &container_id,
        crate::buildkit::BuilderBootstrapPhase::Created,
        &config_digest,
    )
    .unwrap();
    let state_volume = crate::buildkit::daemon_state_volume(&builder);
    let policy = Arc::new(DockerLeasePolicy::new(TEST_JOB_ID).unwrap());
    policy.enable_test_buildkit_engine();
    policy.enable_test_bootstrap_persistence();
    policy.set_job_network(TEST_NETWORK).unwrap();
    policy
        .resources
        .lock()
        .unwrap()
        .admitted_buildkit_daemons
        .insert(
            crate::buildkit::daemon_container_name(&builder),
            PersistentBuildKitAdmission {
                builder: builder.clone(),
                state_volume: state_volume.clone(),
                owner_token,
                container_id: Some(container_id.clone()),
                network_reconciled: false,
                approved_config: TEST_CONFIG.to_vec(),
                bootstrap_phase: crate::buildkit::BuilderBootstrapPhase::Created,
                readiness_attempts: 0,
                archive_in_flight: false,
            },
        );
    policy
        .resources
        .lock()
        .unwrap()
        .admitted_buildkit_volumes
        .insert(state_volume);

    let archive = buildkit_archive(TEST_CONFIG);
    let engine_request = engine.reply_once(http_response(200, "OK", b""));
    let (response, result) = proxy_request_with_test_storage(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        archive_request_for(&container_id, &archive),
        Some(worker_layout),
    );
    result.unwrap();
    response_status(&response, 200);
    assert!(engine_request.join().unwrap().starts_with(b"PUT "));
    assert_eq!(
        crate::buildkit::registered_owner_bootstrap_state(&builder).unwrap(),
        Some((
            container_id.clone(),
            crate::buildkit::BuilderBootstrapPhase::Archived,
            Some(config_digest),
        )),
        "the owner registry must commit Archived before returning archive success"
    );

    let restarted_policy = DockerLeasePolicy::new(TEST_JOB_ID).unwrap();
    restarted_policy
        .admit_persistent_buildkit_builder(&builder, TEST_CONFIG)
        .unwrap();
    assert_eq!(
        restarted_policy
            .persistent_buildkit_container_admission(&container_id)
            .unwrap()
            .unwrap()
            .bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Archived,
        "a new policy instance must load the committed archive phase"
    );
}

#[test]
fn persistent_buildkit_start_rejects_query_and_body_then_accepts_exact_empty_route() {
    let (engine, policy) = fixture(crate::buildkit::BuilderBootstrapPhase::Archived);

    for request in [
        http_request(
            "POST",
            &format!("/v1.43/containers/{TEST_CONTAINER_ID}/start?t=1"),
            &[],
            b"",
        ),
        http_request(
            "POST",
            &format!("/v1.43/containers/{TEST_CONTAINER_ID}/start"),
            &[],
            b"unexpected",
        ),
    ] {
        let (response, result) =
            proxy_request(engine.socket_path.clone(), Arc::clone(&policy), request);
        assert_denied(&response, result, 403);
        engine.assert_no_connection();
        assert_eq!(
            current_admission(&policy).bootstrap_phase,
            crate::buildkit::BuilderBootstrapPhase::Archived
        );
    }

    let engine_request = engine.reply_once(http_response(204, "No Content", b""));
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        http_request(
            "POST",
            &format!("/v1.43/containers/{TEST_CONTAINER_ID}/start"),
            &[],
            b"",
        ),
    );
    result.unwrap();
    response_status(&response, 204);
    let forwarded = engine_request.join().unwrap();
    assert_eq!(docker_request_line(&forwarded).unwrap().0, "POST");
    assert_eq!(docker_request_body(&forwarded).unwrap(), b"");
    assert_eq!(
        current_admission(&policy).bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Started
    );
}

#[test]
fn buildx_exec_create_attach_inspect_is_id_bound_sequenced_and_one_shot() {
    let (engine, policy) = fixture(crate::buildkit::BuilderBootstrapPhase::Started);

    let failed_create = engine.reply_once(http_response(
        500,
        "Engine Error",
        format!(r#"{{"Id":"{TEST_FAILED_EXEC_ID}"}}"#).as_bytes(),
    ));
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        exec_create_request(&["buildctl", "debug", "workers"]),
    );
    result.unwrap();
    response_status(&response, 500);
    let forwarded = failed_create.join().unwrap();
    assert_eq!(
        docker_request_body(&forwarded).unwrap(),
        docker_request_body(&exec_create_request(&["buildctl", "debug", "workers"])).unwrap()
    );
    assert!(policy
        .persistent_buildkit_exec(TEST_FAILED_EXEC_ID)
        .unwrap()
        .is_none());
    assert_eq!(current_admission(&policy).readiness_attempts, 1);

    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        exec_attach_request(TEST_FAILED_EXEC_ID),
    );
    assert_denied(&response, result, 404);
    engine.assert_no_connection();

    let create_response = http_response(
        201,
        "Created",
        br#"{"Id":"buildkit-workers-protocol-test"}"#,
    );
    let successful_create = engine.reply_once(create_response);
    let create = exec_create_request(&["buildctl", "debug", "workers"]);
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        create.clone(),
    );
    result.unwrap();
    response_status(&response, 201);
    let forwarded = successful_create.join().unwrap();
    assert_eq!(
        docker_request_body(&forwarded).unwrap(),
        docker_request_body(&create).unwrap()
    );
    assert_eq!(
        policy.persistent_buildkit_exec(TEST_EXEC_ID).unwrap(),
        Some(PersistentBuildKitExec {
            container_id: TEST_CONTAINER_ID.to_owned(),
            command: PersistentBuildKitExecCommand::Workers,
            phase: PersistentBuildKitExecPhase::Created,
        })
    );
    assert_eq!(current_admission(&policy).readiness_attempts, 2);

    let inspect = http_request("GET", &format!("/v1.43/exec/{TEST_EXEC_ID}/json"), &[], b"");
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        inspect.clone(),
    );
    assert_denied(&response, result, 403);
    engine.assert_no_connection();

    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        http_request(
            "POST",
            &format!("/v1.43/exec/{TEST_EXEC_ID}/start"),
            &[("Connection", "Upgrade"), ("Upgrade", "tcp")],
            br#"{"Detach":false,"Tty":true}"#,
        ),
    );
    assert_denied(&response, result, 403);
    engine.assert_no_connection();

    let attach = exec_attach_request(TEST_EXEC_ID);
    let attach_engine_request = engine.reply_once_after(
        b"HTTP/1.1 101 UPGRADED\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\nworkers-stream\n"
            .to_vec(),
        Duration::from_millis(25),
    );
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        attach.clone(),
    );
    result.unwrap();
    assert!(response.starts_with(b"HTTP/1.1 101 UPGRADED\r\n"));
    assert!(response.ends_with(b"workers-stream\n"));
    let forwarded = attach_engine_request.join().unwrap();
    let (method, target) = docker_request_line(&forwarded).unwrap();
    let expected_target = format!("/v1.43/exec/{TEST_EXEC_ID}/start");
    assert_eq!((method, target), ("POST", expected_target.as_str()));
    assert_eq!(
        docker_request_body(&forwarded).unwrap(),
        br#"{"Detach":false,"Tty":false}"#
    );
    let attach_headers = docker_request_headers(&forwarded).unwrap();
    assert_eq!(
        attach_headers
            .iter()
            .filter(|(name, value)| name.eq_ignore_ascii_case("upgrade")
                && value.eq_ignore_ascii_case("tcp"))
            .count(),
        1
    );
    assert!(attach_headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("connection")
            && value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    }));
    assert_eq!(
        policy
            .persistent_buildkit_exec(TEST_EXEC_ID)
            .unwrap()
            .unwrap()
            .phase,
        PersistentBuildKitExecPhase::Streamed
    );

    let (response, result) = proxy_request(engine.socket_path.clone(), Arc::clone(&policy), attach);
    assert_denied(&response, result, 403);
    engine.assert_no_connection();

    let inspect_response = http_response(
        200,
        "OK",
        format!(
            r#"{{"ID":"{TEST_EXEC_ID}","ContainerID":"{TEST_CONTAINER_ID}","Running":false,"ExitCode":0}}"#
        )
        .as_bytes(),
    );
    let inspect_engine_request = engine.reply_once(inspect_response);
    let (response, result) = proxy_request(
        engine.socket_path.clone(),
        Arc::clone(&policy),
        inspect.clone(),
    );
    result.unwrap();
    response_status(&response, 200);
    assert_eq!(inspect_engine_request.join().unwrap(), inspect);
    let admission = current_admission(&policy);
    assert_eq!(
        admission.bootstrap_phase,
        crate::buildkit::BuilderBootstrapPhase::Ready
    );
    assert!(policy
        .persistent_buildkit_exec(TEST_EXEC_ID)
        .unwrap()
        .is_none());

    let (response, result) = proxy_request(engine.socket_path.clone(), policy, inspect);
    assert_denied(&response, result, 404);
    engine.assert_no_connection();
}
