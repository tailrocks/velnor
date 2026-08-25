//! Guest-side vsock session. Production binds AF_VSOCK inside the microVM.

use std::io::{Read, Write};
use std::path::PathBuf;

use velnor_model::{VsockCodecError, VsockMessage, PROTOCOL_VERSION};

/// Guest identity announced on `GuestReady`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSessionEnv {
    pub isolation_id: String,
    pub generation: u64,
    pub docker_healthy: bool,
}

impl GuestSessionEnv {
    /// Read isolation labels from the guest environment.
    #[must_use]
    pub fn from_guest_env() -> Self {
        Self {
            isolation_id: std::env::var("VELNOR_ISOLATION_ID").unwrap_or_default(),
            generation: std::env::var("VELNOR_ISOLATION_GENERATION")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            docker_healthy: PathBuf::from("/var/run/docker.sock").exists(),
        }
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
    VsockMessage::GuestReady {
        isolation_id: env.isolation_id.clone(),
        generation: env.generation,
        docker_healthy: env.docker_healthy,
    }
    .write_to(stream)
    .map_err(|error| format!("write ready: {error}"))?;
    loop {
        match VsockMessage::read_from(&mut *stream) {
            Ok(VsockMessage::Cancel | VsockMessage::TeardownAck { .. }) => break,
            Ok(VsockMessage::DeliverPlan {
                isolation_id,
                generation,
                plan_bytes,
            }) => {
                let code = run_plan(&plan_bytes)?;
                VsockMessage::StepCompleted {
                    step_id: "job".into(),
                    exit_code: code,
                }
                .write_to(stream)
                .map_err(|error| format!("write step: {error}"))?;
                VsockMessage::TeardownAck {
                    isolation_id,
                    generation,
                }
                .write_to(stream)
                .map_err(|error| format!("write ack: {error}"))?;
            }
            Ok(_) => {}
            Err(VsockCodecError::Io { .. }) => break,
            Err(error) => return Err(format!("protocol v{PROTOCOL_VERSION}: {error}")),
        }
    }
    Ok(())
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
            }
        );
        VsockMessage::DeliverPlan {
            isolation_id: "job-1".into(),
            generation: 7,
            plan_bytes: bytes,
        }
        .write_to(&mut host)
        .unwrap();
        assert!(matches!(
            VsockMessage::read_from(&mut host).unwrap(),
            VsockMessage::StepCompleted { exit_code: 0, .. }
        ));
        assert!(matches!(
            VsockMessage::read_from(&mut host).unwrap(),
            VsockMessage::TeardownAck { generation: 7, .. }
        ));
        VsockMessage::Cancel.write_to(&mut host).unwrap();
        thread.join().unwrap().unwrap();
    }
}
