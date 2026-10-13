//! Guest-side vsock session. Production binds AF_VSOCK inside the microVM.

use std::io::{Read, Write};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use velnor_model::{
    derive_execution_nonce, JobConclusion, VsockCodecError, VsockMessage, PROTOCOL_VERSION,
};

use super::artifacts::hex_sha256;
use super::guest_runtime::{decode_guest_plan, guest_capability_error, validate_guest_plan};

const GUEST_DOCKER_READY_TIMEOUT: Duration = Duration::from_secs(30);
const GUEST_DOCKER_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Immutable identity injected into the guest boot command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSessionEnv {
    isolation_id: String,
    generation: u64,
    docker_healthy: bool,
    job_credentials_absent: bool,
}

/// Guest-agent lifecycle across host connections.
///
/// The snapshot boundary is valid only while this state is waiting for the
/// next `GuestIdentity`. A restored VM therefore re-enters the handshake
/// instead of resuming inside a prior plan-delivery loop.
#[derive(Debug, Default)]
pub struct GuestAgentState {
    session_active: bool,
    last_session_challenge: Option<String>,
}

impl GuestSessionEnv {
    /// Read identity and prove guest Docker/credential state before readiness.
    ///
    /// # Errors
    /// Missing identity, unhealthy guest Docker, or present job credentials.
    pub fn from_guest_env() -> Result<Self, String> {
        let isolation_id = required_env("VELNOR_ISOLATION_ID")?;
        let generation = required_env("VELNOR_ISOLATION_GENERATION")?
            .parse::<u64>()
            .map_err(|error| format!("invalid VELNOR_ISOLATION_GENERATION: {error}"))?;
        if generation == 0 {
            return Err("VELNOR_ISOLATION_GENERATION must be greater than zero".into());
        }

        let job_credentials_absent = [
            "GITHUB_TOKEN",
            "RUNNER_TOKEN",
            "ACTIONS_RUNTIME_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ]
        .iter()
        .all(|name| std::env::var_os(name).is_none());
        if !job_credentials_absent {
            return Err("guest readiness refused: job credentials are present".into());
        }

        let docker_healthy = wait_for_guest_docker(
            GUEST_DOCKER_READY_TIMEOUT,
            GUEST_DOCKER_RETRY_INTERVAL,
            probe_guest_docker,
            std::thread::sleep,
        )?;
        if !docker_healthy {
            return Err(format!(
                "guest Docker health probe did not become ready before the {} second deadline",
                GUEST_DOCKER_READY_TIMEOUT.as_secs()
            ));
        }

        Ok(Self {
            isolation_id,
            generation,
            docker_healthy,
            job_credentials_absent,
        })
    }
}

/// Serve one host connection: ready, plan delivery, teardown.
///
/// # Errors
/// Protocol or plan-execution failures.
pub fn serve_guest_session<S, F>(
    stream: &mut S,
    env: &GuestSessionEnv,
    run_plan: F,
) -> Result<(), String>
where
    S: Read + Write,
    F: FnMut(&[u8]) -> Result<(i32, Vec<super::backend::ExecutionEvent>), String>,
{
    let mut state = GuestAgentState::default();
    serve_guest_session_with_state(stream, env, &mut state, run_plan)
}

/// Serve one host connection using persistent guest-agent lifecycle state.
///
/// # Errors
/// Protocol or plan-execution failures.
pub fn serve_guest_session_with_state<S, F>(
    stream: &mut S,
    env: &GuestSessionEnv,
    state: &mut GuestAgentState,
    mut run_plan: F,
) -> Result<(), String>
where
    S: Read + Write,
    F: FnMut(&[u8]) -> Result<(i32, Vec<super::backend::ExecutionEvent>), String>,
{
    if env.isolation_id.is_empty() || env.generation == 0 {
        return Err("guest readiness proof has incomplete isolation identity".into());
    }
    if !env.docker_healthy || !env.job_credentials_absent {
        return Err("guest readiness proof is missing Docker or credential proof".into());
    }
    if state.session_active {
        return Err(
            "guest protocol state is already active; session boundary transition required".into(),
        );
    }
    let VsockMessage::GuestIdentity {
        isolation_id: session_isolation_id,
        generation: session_generation,
        restored: _restored,
    } = VsockMessage::read_from(&mut *stream)
        .map_err(|error| format!("read session identity: {error}"))?
    else {
        return Err(guest_capability_error(
            "guest.identity",
            "unexpected first message",
            "GuestIdentity before GuestReady",
        ));
    };
    require_session_identity(&session_isolation_id, session_generation)?;
    // The command-line identity is immutable for the VM lifetime. This check
    // applies before GuestReady on both cold and restored sessions.
    require_identity(
        "guest.identity.isolation_id",
        &session_isolation_id,
        &env.isolation_id,
    )?;
    require_session_generation(session_generation, env.generation)?;
    let session_challenge = new_session_challenge(state.last_session_challenge.as_deref());
    state.session_active = true;
    state.last_session_challenge = Some(session_challenge.clone());
    VsockMessage::GuestReady {
        isolation_id: session_isolation_id.clone(),
        generation: session_generation,
        docker_healthy: env.docker_healthy,
        job_credentials_absent: env.job_credentials_absent,
        session_challenge: session_challenge.clone(),
    }
    .write_to(stream)
    .map_err(|error| format!("write ready: {error}"))?;
    let mut imported: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    loop {
        match VsockMessage::read_from(&mut *stream) {
            Ok(VsockMessage::Cancel) => {
                state.session_active = false;
                return Ok(());
            }
            Ok(VsockMessage::TeardownAck { .. }) => {
                return Err(guest_capability_error(
                    "guest.teardown_ack",
                    "host-to-guest",
                    "guest-to-host acknowledgement",
                ));
            }
            Ok(VsockMessage::PrepareSnapshot) => {
                VsockMessage::SnapshotReady
                    .write_to(stream)
                    .map_err(|error| format!("write snapshot readiness: {error}"))?;
                state.session_active = false;
                return Ok(());
            }
            Ok(VsockMessage::ImportBlob {
                digest_sha256,
                bytes,
            }) => {
                let blob = super::cache_transport::CacheBlob {
                    digest_sha256: digest_sha256.clone(),
                    bytes: bytes.clone(),
                };
                blob.import().map_err(|error| {
                    guest_capability_error(
                        "guest.import_blob",
                        &error.detail,
                        "bytes whose sha256 matches the declared digest",
                    )
                })?;
                imported.insert(digest_sha256, bytes);
                continue;
            }
            Ok(VsockMessage::DeliverPlan {
                job_id,
                isolation_id,
                generation,
                execution_nonce,
                plan_sha256,
                plan_bytes,
            }) => {
                if execution_nonce.is_empty() {
                    return Err(guest_capability_error(
                        "guest.execution_nonce",
                        "<empty>",
                        "a fresh non-empty nonce per execution",
                    ));
                }
                let expected_execution_nonce = derive_execution_nonce(
                    &session_challenge,
                    &job_id,
                    &isolation_id,
                    generation,
                    &plan_sha256,
                );
                if execution_nonce != expected_execution_nonce {
                    return Err(guest_capability_error(
                        "guest.execution_nonce",
                        &execution_nonce,
                        &format!("the nonce derived from the current session challenge and plan ({expected_execution_nonce})"),
                    ));
                }
                let actual_plan_sha256 = hex_sha256(&plan_bytes);
                if plan_sha256 != actual_plan_sha256 {
                    return Err(guest_capability_error(
                        "guest.plan_sha256",
                        &plan_sha256,
                        &format!("the SHA-256 digest of plan_bytes ({actual_plan_sha256})"),
                    ));
                }
                require_identity(
                    "guest.plan.isolation_id",
                    &isolation_id,
                    &session_isolation_id,
                )?;
                require_generation(generation, session_generation)?;
                let mut plan = decode_guest_plan(&plan_bytes)?;
                require_identity("guest.plan.job_id", &plan.job_id, &job_id)?;
                require_identity("guest.plan.isolation_id", &plan.isolation_id, &isolation_id)?;
                require_generation(plan.generation, generation)?;
                for cache in &mut plan.cache {
                    if cache.bytes.is_empty()
                        && let Some(bytes) = imported.get(&cache.digest)
                    {
                        cache.bytes = bytes.clone();
                    }
                }
                validate_guest_plan(&plan)?;
                let (conclusion, code, events) = if let Some(planned) = plan.planned_conclusion() {
                    let (conclusion, code) = planned;
                    let mut events = Vec::new();
                    super::guest_runtime::record_planned_result_bridge(&plan, &mut events);
                    (conclusion, code, events)
                } else {
                    let merged = plan
                        .encode()
                        .map_err(|error| format!("guest plan encode after import: {error}"))?;
                    let (code, events) = run_plan(&merged)?;
                    (
                        if code == 0 {
                            JobConclusion::Success
                        } else {
                            JobConclusion::Failure
                        },
                        code,
                        events,
                    )
                };
                write_result_bridge(stream, &events)?;
                VsockMessage::JobCompleted {
                    conclusion,
                    exit_code: code,
                }
                .write_to(stream)
                .map_err(|error| format!("write conclusion: {error}"))?;
                VsockMessage::TeardownAck {
                    job_id,
                    isolation_id,
                    generation,
                    execution_nonce,
                    plan_sha256,
                }
                .write_to(stream)
                .map_err(|error| format!("write ack: {error}"))?;
                state.session_active = false;
                return Ok(());
            }
            Ok(message) => {
                return Err(guest_capability_error(
                    "guest.protocol",
                    &format!("unexpected message {message:?}"),
                    "ImportBlob*, then Cancel, PrepareSnapshot, or exactly one DeliverPlan",
                ));
            }
            Err(VsockCodecError::Io { .. }) => {
                state.session_active = false;
                return Ok(());
            }
            Err(error) => return Err(format!("protocol v{PROTOCOL_VERSION}: {error}")),
        }
    }
}

fn write_result_bridge<S: Write>(
    stream: &mut S,
    events: &[super::backend::ExecutionEvent],
) -> Result<(), String> {
    use super::backend::ExecutionEvent;
    for event in events {
        let message = match event {
            ExecutionEvent::Log { stream, line } => VsockMessage::Stdio {
                stream: *stream,
                bytes: line.as_bytes().to_vec(),
            },
            ExecutionEvent::StepStarted { step_id } => VsockMessage::StepStarted {
                step_id: step_id.clone(),
            },
            ExecutionEvent::StepCompleted {
                step_id,
                exit_code,
                skipped,
            } => VsockMessage::StepCompleted {
                step_id: step_id.clone(),
                exit_code: *exit_code,
                skipped: *skipped,
            },
            ExecutionEvent::CommandFile { path, bytes } => VsockMessage::CommandFile {
                path: path.clone(),
                bytes: bytes.clone(),
            },
            ExecutionEvent::ResultExport {
                digest_sha256,
                bytes,
            } => VsockMessage::ResultExport {
                digest_sha256: digest_sha256.clone(),
                bytes: bytes.clone(),
            },
            ExecutionEvent::Output { .. }
            | ExecutionEvent::JobCompleted { .. }
            | ExecutionEvent::HostDockerInvoked(_)
            | ExecutionEvent::FirecrackerApi(_)
            | ExecutionEvent::GuestDocker(_) => continue,
        };
        message
            .write_to(stream)
            .map_err(|error| format!("write result bridge: {error}"))?;
    }
    Ok(())
}

fn wait_for_guest_docker<F, S>(
    timeout: Duration,
    retry_interval: Duration,
    mut probe: F,
    mut sleep: S,
) -> Result<bool, String>
where
    F: FnMut(Duration) -> Result<bool, String>,
    S: FnMut(Duration),
{
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        if probe(remaining)? {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        sleep(retry_interval.min(remaining));
    }
}

fn probe_guest_docker(timeout: Duration) -> Result<bool, String> {
    let mut command = Command::new("docker");
    command.args(["info", "--format", "{{.ServerVersion}}"]);
    run_command_with_deadline(&mut command, timeout)
}

fn run_command_with_deadline(command: &mut Command, timeout: Duration) -> Result<bool, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn guest Docker probe: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    kill_and_reap(&mut child);
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_millis(10).min(remaining));
            }
            Err(error) => {
                let message = format!("wait for guest Docker probe: {error}");
                kill_and_reap(&mut child);
                return Err(message);
            }
        }
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn new_session_challenge(previous: Option<&str>) -> String {
    loop {
        let challenge = uuid::Uuid::new_v4().to_string();
        if previous != Some(challenge.as_str()) {
            return challenge;
        }
    }
}

fn required_env(name: &str) -> Result<String, String> {
    let value =
        std::env::var(name).map_err(|_| format!("missing guest identity variable {name}"))?;
    if value.trim().is_empty() {
        return Err(format!("guest identity variable {name} is empty"));
    }
    Ok(value)
}

fn require_identity(field: &str, received: &str, expected: &str) -> Result<(), String> {
    if received == expected && !received.is_empty() {
        Ok(())
    } else {
        Err(guest_capability_error(
            field,
            if received.is_empty() {
                "<empty>"
            } else {
                received
            },
            "the expected session identity",
        ))
    }
}

fn require_session_identity(isolation_id: &str, generation: u64) -> Result<(), String> {
    if isolation_id.trim().is_empty() {
        return Err(guest_capability_error(
            "guest.identity.isolation_id",
            "<empty>",
            "a non-empty control-plane isolation identity",
        ));
    }
    if generation == 0 {
        return Err(guest_capability_error(
            "guest.identity.generation",
            "0",
            "a non-zero control-plane session generation",
        ));
    }
    Ok(())
}

fn require_generation(received: u64, expected: u64) -> Result<(), String> {
    if received == expected && received != 0 {
        Ok(())
    } else {
        Err(guest_capability_error(
            "guest.plan.generation",
            &received.to_string(),
            "the expected non-zero session generation",
        ))
    }
}

fn require_session_generation(received: u64, expected: u64) -> Result<(), String> {
    if received == expected && received != 0 {
        Ok(())
    } else {
        Err(guest_capability_error(
            "guest.identity.generation",
            &received.to_string(),
            "the expected non-zero boot/session generation",
        ))
    }
}

/// Bind `VMADDR_CID_ANY` on `port`. Firecracker delivers host connections here.
///
/// # Errors
/// Socket, bind, or listen failure.
#[cfg(target_os = "linux")]
pub fn bind_af_vsock(port: u32) -> Result<std::os::fd::OwnedFd, String> {
    use std::os::fd::{FromRawFd, OwnedFd};

    // SAFETY: `socket` returns a new file descriptor or -1.
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(format!(
            "AF_VSOCK socket: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_port = port;
    addr.svm_cid = libc::VMADDR_CID_ANY;
    let bind_rc = unsafe {
        libc::bind(
            fd,
            std::ptr::addr_of!(addr).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_vm>() as u32,
        )
    };
    if bind_rc != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("AF_VSOCK bind port {port}: {error}"));
    }
    let listen_rc = unsafe { libc::listen(fd, 1) };
    if listen_rc != 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(format!("AF_VSOCK listen: {error}"));
    }
    // SAFETY: `fd` is an open socket owned uniquely here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Accept one host connection on a bound AF_VSOCK listener.
///
/// # Errors
/// Accept failure.
#[cfg(target_os = "linux")]
pub fn accept_af_vsock(
    listener: &std::os::fd::OwnedFd,
) -> Result<std::os::unix::net::UnixStream, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::net::UnixStream;

    let fd = unsafe {
        libc::accept(
            listener.as_raw_fd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if fd < 0 {
        return Err(format!(
            "AF_VSOCK accept: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `accept` returned a new connected SOCK_STREAM fd.
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use velnor_model::{GuestJobPlan, VsockMessage};

    #[test]
    fn result_bridge_preserves_skipped_step_state() {
        let events = [
            crate::execution::ExecutionEvent::StepCompleted {
                step_id: "skipped".into(),
                exit_code: 0,
                skipped: true,
            },
            crate::execution::ExecutionEvent::StepCompleted {
                step_id: "executed".into(),
                exit_code: 0,
                skipped: false,
            },
        ];
        let mut bytes = Vec::new();
        write_result_bridge(&mut bytes, &events).unwrap();
        let mut cursor = std::io::Cursor::new(bytes);
        assert_eq!(
            VsockMessage::read_from(&mut cursor).unwrap(),
            VsockMessage::StepCompleted {
                step_id: "skipped".into(),
                exit_code: 0,
                skipped: true,
            }
        );
        assert_eq!(
            VsockMessage::read_from(&mut cursor).unwrap(),
            VsockMessage::StepCompleted {
                step_id: "executed".into(),
                exit_code: 0,
                skipped: false,
            }
        );
    }

    #[test]
    fn guest_docker_health_retries_until_ready_within_deadline() {
        let attempts = std::cell::Cell::new(0_u8);
        let ready = wait_for_guest_docker(
            Duration::from_secs(1),
            Duration::ZERO,
            |_| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                Ok(attempt >= 3)
            },
            |_| {},
        )
        .unwrap();
        assert!(ready);
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn guest_docker_probe_kills_hung_process_at_deadline() {
        let started = Instant::now();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        assert!(!run_command_with_deadline(&mut command, Duration::from_millis(50)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn guest_docker_probe_preserves_spawn_error() {
        let mut command = Command::new("/velnor/path/that/does/not/exist");
        let error = run_command_with_deadline(&mut command, Duration::from_millis(50)).unwrap_err();
        assert!(error.contains("spawn guest Docker probe"), "{error}");
    }

    #[test]
    fn cold_boot_identity_mismatch_fails_before_guest_ready() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let env = GuestSessionEnv {
            isolation_id: "boot-identity".into(),
            generation: 7,
            docker_healthy: true,
            job_credentials_absent: true,
        };
        let thread = std::thread::spawn(move || {
            serve_guest_session(&mut guest, &env, |_| Ok((0, Vec::new())))
        });
        VsockMessage::GuestIdentity {
            isolation_id: "wrong-identity".into(),
            generation: 7,
            restored: false,
        }
        .write_to(&mut host)
        .unwrap();
        let error = thread.join().unwrap().unwrap_err();
        assert!(error.contains("guest.identity.isolation_id"), "{error}");
        assert!(error.contains("expected session identity"), "{error}");
    }

    #[test]
    fn restore_closes_pre_snapshot_session_and_rebinds_fresh_session() {
        let env = GuestSessionEnv {
            isolation_id: "boot-identity".into(),
            generation: 7,
            docker_healthy: true,
            job_credentials_absent: true,
        };
        let (mut first_host, mut first_guest) = UnixStream::pair().unwrap();
        let (mut restored_host, mut restored_guest) = UnixStream::pair().unwrap();
        let first_env = env.clone();
        let restored_env = env.clone();
        let first = std::thread::spawn(move || {
            let mut state = GuestAgentState::default();
            serve_guest_session_with_state(&mut first_guest, &first_env, &mut state, |_| {
                Ok((0, Vec::new()))
            })?;
            serve_guest_session_with_state(&mut restored_guest, &restored_env, &mut state, |_| {
                Ok((0, Vec::new()))
            })
        });
        VsockMessage::GuestIdentity {
            isolation_id: "boot-identity".into(),
            generation: 7,
            restored: false,
        }
        .write_to(&mut first_host)
        .unwrap();
        let first_challenge = match VsockMessage::read_from(&mut first_host).unwrap() {
            VsockMessage::GuestReady {
                isolation_id,
                generation,
                session_challenge,
                ..
            } => {
                assert_eq!(isolation_id, "boot-identity");
                assert_eq!(generation, 7);
                session_challenge
            }
            other => panic!("expected first GuestReady, got {other:?}"),
        };
        VsockMessage::PrepareSnapshot
            .write_to(&mut first_host)
            .unwrap();
        assert_eq!(
            VsockMessage::read_from(&mut first_host).unwrap(),
            VsockMessage::SnapshotReady
        );
        VsockMessage::GuestIdentity {
            isolation_id: "boot-identity".into(),
            generation: 7,
            restored: true,
        }
        .write_to(&mut restored_host)
        .unwrap();
        match VsockMessage::read_from(&mut restored_host).unwrap() {
            VsockMessage::GuestReady {
                isolation_id,
                generation,
                session_challenge,
                ..
            } => {
                assert_eq!(isolation_id, "boot-identity");
                assert_eq!(generation, 7);
                assert_ne!(session_challenge, first_challenge);
            }
            other => panic!("expected restored GuestReady, got {other:?}"),
        }
        VsockMessage::Cancel.write_to(&mut restored_host).unwrap();
        first.join().unwrap().unwrap();
    }

    #[test]
    fn restored_session_rejects_nonce_from_previous_session() {
        let env = GuestSessionEnv {
            isolation_id: "boot-identity".into(),
            generation: 7,
            docker_healthy: true,
            job_credentials_absent: true,
        };
        let plan = GuestJobPlan {
            isolation_id: "boot-identity".into(),
            generation: 7,
            job_id: "boot-identity".into(),
            daemon_id: "test-daemon".into(),
            image: "velnor/job-ubuntu:26.04".into(),
            services: Vec::new(),
            steps: Vec::new(),
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: Vec::new(),
            outputs: Vec::new(),
            env: Vec::new(),
            workspace: "/__w".into(),
            context_data: Vec::new(),
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
        };
        let bytes = plan.encode().unwrap();
        let plan_sha256 = hex_sha256(&bytes);
        let (mut first_host, mut first_guest) = UnixStream::pair().unwrap();
        let (mut restored_host, mut restored_guest) = UnixStream::pair().unwrap();
        let first_env = env.clone();
        let restored_env = env;
        let thread = std::thread::spawn(move || {
            let mut state = GuestAgentState::default();
            serve_guest_session_with_state(&mut first_guest, &first_env, &mut state, |_| {
                Ok((0, Vec::new()))
            })?;
            serve_guest_session_with_state(&mut restored_guest, &restored_env, &mut state, |_| {
                Ok((0, Vec::new()))
            })
        });
        VsockMessage::GuestIdentity {
            isolation_id: "boot-identity".into(),
            generation: 7,
            restored: false,
        }
        .write_to(&mut first_host)
        .unwrap();
        let first_challenge = match VsockMessage::read_from(&mut first_host).unwrap() {
            VsockMessage::GuestReady {
                session_challenge, ..
            } => session_challenge,
            other => panic!("expected first GuestReady, got {other:?}"),
        };
        VsockMessage::PrepareSnapshot
            .write_to(&mut first_host)
            .unwrap();
        assert_eq!(
            VsockMessage::read_from(&mut first_host).unwrap(),
            VsockMessage::SnapshotReady
        );
        VsockMessage::GuestIdentity {
            isolation_id: "boot-identity".into(),
            generation: 7,
            restored: true,
        }
        .write_to(&mut restored_host)
        .unwrap();
        let second_challenge = match VsockMessage::read_from(&mut restored_host).unwrap() {
            VsockMessage::GuestReady {
                session_challenge, ..
            } => session_challenge,
            other => panic!("expected restored GuestReady, got {other:?}"),
        };
        assert_ne!(first_challenge, second_challenge);
        VsockMessage::DeliverPlan {
            job_id: "boot-identity".into(),
            isolation_id: "boot-identity".into(),
            generation: 7,
            execution_nonce: derive_execution_nonce(
                &first_challenge,
                "boot-identity",
                "boot-identity",
                7,
                &plan_sha256,
            ),
            plan_sha256,
            plan_bytes: bytes,
        }
        .write_to(&mut restored_host)
        .unwrap();
        let error = thread.join().unwrap().unwrap_err();
        assert!(error.contains("guest.execution_nonce"), "{error}");
        assert!(error.contains("current session challenge"), "{error}");
    }

    #[test]
    fn session_delivers_plan_and_acks_without_unix_listener() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let env = GuestSessionEnv {
            isolation_id: "job-1".into(),
            generation: 7,
            docker_healthy: true,
            job_credentials_absent: true,
        };
        let plan = GuestJobPlan {
            isolation_id: "job-1".into(),
            generation: 7,
            job_id: "job-1".into(),
            daemon_id: "test-daemon".into(),
            image: "velnor/job-ubuntu:26.04".into(),
            services: Vec::new(),
            steps: Vec::new(),
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: Vec::new(),
            outputs: Vec::new(),
            env: Vec::new(),
            workspace: "/__w".into(),
            context_data: Vec::new(),
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
        };
        let bytes = plan.encode().unwrap();
        let guest_env = env.clone();
        let plan_bytes = bytes.clone();
        let thread = std::thread::spawn(move || {
            serve_guest_session(&mut guest, &guest_env, |delivered| {
                assert_eq!(delivered, plan_bytes.as_slice());
                Ok((0, Vec::new()))
            })
        });
        VsockMessage::GuestIdentity {
            isolation_id: "job-1".into(),
            generation: 7,
            restored: false,
        }
        .write_to(&mut host)
        .unwrap();
        let ready = VsockMessage::read_from(&mut host).unwrap();
        let execution_nonce = match ready {
            VsockMessage::GuestReady {
                isolation_id,
                generation,
                docker_healthy,
                job_credentials_absent,
                session_challenge,
            } => {
                assert_eq!(isolation_id, "job-1");
                assert_eq!(generation, 7);
                assert!(docker_healthy);
                assert!(job_credentials_absent);
                session_challenge
            }
            other => panic!("expected GuestReady, got {other:?}"),
        };
        let plan_sha256 = hex_sha256(&bytes);
        let execution_nonce =
            derive_execution_nonce(&execution_nonce, "job-1", "job-1", 7, &plan_sha256);
        VsockMessage::DeliverPlan {
            job_id: "job-1".into(),
            isolation_id: "job-1".into(),
            generation: 7,
            execution_nonce,
            plan_sha256,
            plan_bytes: bytes,
        }
        .write_to(&mut host)
        .unwrap();
        assert!(matches!(
            VsockMessage::read_from(&mut host).unwrap(),
            VsockMessage::JobCompleted {
                conclusion: velnor_model::JobConclusion::Success,
                exit_code: 0
            }
        ));
        assert!(matches!(
            VsockMessage::read_from(&mut host).unwrap(),
            VsockMessage::TeardownAck { generation: 7, .. }
        ));
        thread.join().unwrap().unwrap();
    }

    #[test]
    fn malformed_plan_fails_closed_without_running_plan() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let env = GuestSessionEnv {
            isolation_id: "job-malformed".into(),
            generation: 1,
            docker_healthy: true,
            job_credentials_absent: true,
        };
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_by_guest = called.clone();
        let thread = std::thread::spawn(move || {
            serve_guest_session(&mut guest, &env, move |_| {
                called_by_guest.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok((0, Vec::new()))
            })
        });
        VsockMessage::GuestIdentity {
            isolation_id: "job-malformed".into(),
            generation: 1,
            restored: false,
        }
        .write_to(&mut host)
        .unwrap();
        let session_challenge = match VsockMessage::read_from(&mut host).unwrap() {
            VsockMessage::GuestReady {
                session_challenge, ..
            } => session_challenge,
            other => panic!("expected GuestReady, got {other:?}"),
        };
        let plan_sha256 = hex_sha256(b"not a GuestJobPlan");
        let execution_nonce = derive_execution_nonce(
            &session_challenge,
            "job-malformed",
            "job-malformed",
            1,
            &plan_sha256,
        );
        VsockMessage::DeliverPlan {
            job_id: "job-malformed".into(),
            isolation_id: "job-malformed".into(),
            generation: 1,
            execution_nonce,
            plan_sha256,
            plan_bytes: b"not a GuestJobPlan".to_vec(),
        }
        .write_to(&mut host)
        .unwrap();
        let error = thread.join().unwrap().unwrap_err();
        assert!(error.contains("guest.plan"), "{error}");
        assert!(error.contains("received 'malformed'"), "{error}");
        assert!(error.contains("manifest version"), "{error}");
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn mismatched_plan_identity_fails_before_execution() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        let env = GuestSessionEnv {
            isolation_id: "job-expected".into(),
            generation: 4,
            docker_healthy: true,
            job_credentials_absent: true,
        };
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called_by_guest = called.clone();
        let thread = std::thread::spawn(move || {
            serve_guest_session(&mut guest, &env, move |_| {
                called_by_guest.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok((0, Vec::new()))
            })
        });
        VsockMessage::GuestIdentity {
            isolation_id: "job-expected".into(),
            generation: 4,
            restored: false,
        }
        .write_to(&mut host)
        .unwrap();
        let session_challenge = match VsockMessage::read_from(&mut host).unwrap() {
            VsockMessage::GuestReady {
                isolation_id,
                generation: 4,
                docker_healthy: true,
                job_credentials_absent: true,
                session_challenge,
            } => {
                assert_eq!(isolation_id, "job-expected");
                session_challenge
            }
            other => panic!("expected GuestReady, got {other:?}"),
        };
        let mut plan = GuestJobPlan {
            isolation_id: "job-expected".into(),
            generation: 4,
            job_id: "job-expected".into(),
            daemon_id: "test-daemon".into(),
            image: "velnor/job-ubuntu:26.04".into(),
            services: Vec::new(),
            steps: Vec::new(),
            timeout_ms: 1000,
            cancel_requested: false,
            fail: false,
            cache_digest: None,
            command_files: Vec::new(),
            outputs: Vec::new(),
            env: Vec::new(),
            workspace: "/__w".into(),
            context_data: Vec::new(),
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
        };
        plan.job_id = "job-replayed".into();
        let plan_bytes = plan.encode().unwrap();
        let plan_sha256 = hex_sha256(&plan_bytes);
        let execution_nonce = derive_execution_nonce(
            &session_challenge,
            "job-expected",
            "job-expected",
            4,
            &plan_sha256,
        );
        VsockMessage::DeliverPlan {
            job_id: "job-expected".into(),
            isolation_id: "job-expected".into(),
            generation: 4,
            execution_nonce,
            plan_sha256,
            plan_bytes,
        }
        .write_to(&mut host)
        .unwrap();
        let error = thread.join().unwrap().unwrap_err();
        assert!(error.contains("guest.plan.job_id"), "{error}");
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
