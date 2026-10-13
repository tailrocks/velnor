//! Guest-side vsock session. Production binds AF_VSOCK inside the microVM.

use std::io::{Read, Write};
use std::process::Command;

use velnor_model::{GuestJobPlan, JobConclusion, VsockCodecError, VsockMessage, PROTOCOL_VERSION};

use super::guest_runtime::{guest_capability_error, validate_guest_plan};

/// Guest identity announced on `GuestReady`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSessionEnv {
    isolation_id: String,
    generation: u64,
    docker_healthy: bool,
    job_credentials_absent: bool,
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

        let docker_healthy = Command::new("docker")
            .args(["info", "--format", "{{.ServerVersion}}"])
            .status()
            .map_err(|error| format!("guest Docker health probe failed: {error}"))?
            .success();
        if !docker_healthy {
            return Err("guest Docker health probe failed".into());
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
    mut run_plan: F,
) -> Result<(), String>
where
    S: Read + Write,
    F: FnMut(&[u8]) -> Result<i32, String>,
{
    if env.isolation_id.is_empty() || env.generation == 0 {
        return Err("guest readiness proof has incomplete isolation identity".into());
    }
    if !env.docker_healthy || !env.job_credentials_absent {
        return Err("guest readiness proof is missing Docker or credential proof".into());
    }
    VsockMessage::GuestReady {
        isolation_id: env.isolation_id.clone(),
        generation: env.generation,
        docker_healthy: env.docker_healthy,
        job_credentials_absent: env.job_credentials_absent,
    }
    .write_to(stream)
    .map_err(|error| format!("write ready: {error}"))?;
    loop {
        match VsockMessage::read_from(&mut *stream) {
            Ok(VsockMessage::Cancel) => break,
            Ok(VsockMessage::TeardownAck { .. }) => {
                return Err(guest_capability_error(
                    "guest.teardown_ack",
                    "host-to-guest",
                    "guest-to-host acknowledgement",
                ));
            }
            Ok(VsockMessage::DeliverPlan {
                job_id,
                isolation_id,
                generation,
                plan_bytes,
            }) => {
                require_identity("guest.plan.isolation_id", &isolation_id, &env.isolation_id)?;
                require_generation(generation, env.generation)?;
                let plan = GuestJobPlan::decode(&plan_bytes).map_err(|error| {
                    format!(
                        "{} ({error})",
                        guest_capability_error(
                            "guest.plan",
                            "malformed",
                            "valid serialized GuestJobPlan",
                        )
                    )
                })?;
                require_identity("guest.plan.job_id", &plan.job_id, &job_id)?;
                require_identity("guest.plan.isolation_id", &plan.isolation_id, &isolation_id)?;
                require_generation(plan.generation, generation)?;
                validate_guest_plan(&plan)?;
                if !plan.command_files.is_empty() {
                    return Err(guest_capability_error(
                        "guest.command_files",
                        "non-empty",
                        "empty until guest result-file transfer is implemented",
                    ));
                }
                let (conclusion, code) = if let Some(planned) = plan.planned_conclusion() {
                    planned
                } else {
                    let code = run_plan(&plan_bytes)?;
                    (
                        if code == 0 {
                            JobConclusion::Success
                        } else {
                            JobConclusion::Failure
                        },
                        code,
                    )
                };
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
                }
                .write_to(stream)
                .map_err(|error| format!("write ack: {error}"))?;
                return Ok(());
            }
            Ok(_) => {}
            Err(VsockCodecError::Io { .. }) => break,
            Err(error) => return Err(format!("protocol v{PROTOCOL_VERSION}: {error}")),
        }
    }
    Ok(())
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
    use velnor_model::GuestJobPlan;

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
                Ok(0)
            })
        });
        let ready = VsockMessage::read_from(&mut host).unwrap();
        assert_eq!(
            ready,
            VsockMessage::GuestReady {
                isolation_id: "job-1".into(),
                generation: 7,
                docker_healthy: true,
                job_credentials_absent: true,
            }
        );
        VsockMessage::DeliverPlan {
            job_id: "job-1".into(),
            isolation_id: "job-1".into(),
            generation: 7,
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
                Ok(0)
            })
        });
        assert!(matches!(
            VsockMessage::read_from(&mut host).unwrap(),
            VsockMessage::GuestReady { .. }
        ));
        VsockMessage::DeliverPlan {
            job_id: "job-malformed".into(),
            isolation_id: "job-malformed".into(),
            generation: 1,
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
                Ok(0)
            })
        });
        assert!(matches!(
            VsockMessage::read_from(&mut host).unwrap(),
            VsockMessage::GuestReady {
                isolation_id,
                generation: 4,
                docker_healthy: true,
                job_credentials_absent: true
            } if isolation_id == "job-expected"
        ));
        let mut plan = GuestJobPlan {
            isolation_id: "job-expected".into(),
            generation: 4,
            job_id: "job-expected".into(),
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
            cache: Vec::new(),
            artifacts: Vec::new(),
            annotations: Vec::new(),
            summary: String::new(),
            buildx: false,
            testcontainers: false,
        };
        plan.job_id = "job-replayed".into();
        VsockMessage::DeliverPlan {
            job_id: "job-expected".into(),
            isolation_id: "job-expected".into(),
            generation: 4,
            plan_bytes: plan.encode().unwrap(),
        }
        .write_to(&mut host)
        .unwrap();
        let error = thread.join().unwrap().unwrap_err();
        assert!(error.contains("guest.plan.job_id"), "{error}");
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }
}
