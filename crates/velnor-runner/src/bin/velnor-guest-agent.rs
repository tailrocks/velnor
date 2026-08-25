//! Guest agent: vsock control plane inside the Firecracker guest.
//!
//! Production has no SSH server. The agent never receives runner-registration
//! keys, org admin tokens, or host signing material.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use velnor_model::{VsockMessage, PROTOCOL_VERSION};

fn main() {
    if let Err(error) = run() {
        eprintln!("velnor-guest-agent: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let socket = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| "usage: velnor-guest-agent <vsock-unix-path>".to_string())?;
    let listener = UnixListener::bind(&socket)
        .map_err(|error| format!("bind {}: {error}", socket.display()))?;
    let (mut stream, _) = listener
        .accept()
        .map_err(|error| format!("accept: {error}"))?;
    let ready = VsockMessage::GuestReady {
        isolation_id: std::env::var("VELNOR_ISOLATION_ID").unwrap_or_default(),
        generation: std::env::var("VELNOR_ISOLATION_GENERATION")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        docker_healthy: PathBuf::from("/var/run/docker.sock").exists(),
    };
    let bytes = ready.encode().map_err(|error| error.to_string())?;
    stream
        .write_all(&bytes)
        .map_err(|error| format!("write ready: {error}"))?;
    let mut buf = vec![0_u8; velnor_model::MAX_FRAME_BYTES];
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|error| format!("read: {error}"))?;
        if n == 0 {
            break;
        }
        match VsockMessage::decode(&buf[..n]) {
            Ok(VsockMessage::Cancel | VsockMessage::TeardownAck { .. }) => break,
            Ok(VsockMessage::DeliverPlan {
                isolation_id,
                generation,
                ..
            }) => {
                let ack = VsockMessage::TeardownAck {
                    isolation_id,
                    generation,
                };
                let bytes = ack.encode().map_err(|error| error.to_string())?;
                stream
                    .write_all(&bytes)
                    .map_err(|error| format!("write ack: {error}"))?;
            }
            Ok(_) => {}
            Err(error) => return Err(format!("protocol v{PROTOCOL_VERSION}: {error}")),
        }
    }
    Ok(())
}
