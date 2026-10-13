//! Guest agent: AF_VSOCK control plane inside the Firecracker guest.
//!
//! Production has no SSH server and no UnixListener. The agent never receives
//! runner-registration keys, org admin tokens, or host signing material.

fn main() {
    if let Err(error) = run() {
        eprintln!("velnor-guest-agent: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    #[cfg(not(target_os = "linux"))]
    {
        Err("velnor-guest-agent requires Linux AF_VSOCK".into())
    }
    #[cfg(target_os = "linux")]
    {
        use velnor_runner::execution::{
            accept_af_vsock, bind_af_vsock, serve_guest_session, GuestSessionEnv, GUEST_AGENT_PORT,
        };
        let port = std::env::var("VELNOR_GUEST_VSOCK_PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(GUEST_AGENT_PORT);
        let listener = bind_af_vsock(port)?;
        let mut stream = accept_af_vsock(&listener)?;
        serve_guest_session(&mut stream, &GuestSessionEnv::from_guest_env(), |bytes| {
            velnor_runner::execution::run_guest_plan_bytes(bytes)
        })
    }
}
