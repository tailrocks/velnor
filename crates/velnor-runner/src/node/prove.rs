//! Observed Ready preconditions. Controller never stamps these true.

use std::path::{Path, PathBuf};
use std::process::Child;

use serde::{Deserialize, Serialize};

/// On-disk routing observation. Missing or invalid file is not valid routing.
pub const ROUTING_FILE: &str = "routing.json";
/// Host-local executor proof written after a successful preflight.
pub const EXECUTOR_OK: &str = "executor.ok";
/// Transitional host Docker socket. Presence is executor proof.
pub const HOST_DOCKER_SOCKET: &str = "/var/run/docker.sock";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingObservation {
    pub valid: bool,
    pub group_valid: bool,
}

impl RoutingObservation {
    #[must_use]
    pub const fn invalid() -> Self {
        Self {
            valid: false,
            group_valid: false,
        }
    }
}

/// Read `routing.json`. A missing or corrupt file is invalid, never assumed.
#[must_use]
pub fn observe_routing(state_dir: &Path) -> RoutingObservation {
    let Ok(bytes) = std::fs::read(state_dir.join(ROUTING_FILE)) else {
        return RoutingObservation::invalid();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|_| RoutingObservation::invalid())
}

/// Executor is proven by `executor.ok` or a live host Docker socket.
#[must_use]
pub fn observe_executor(state_dir: &Path) -> bool {
    state_dir.join(EXECUTOR_OK).is_file() || Path::new(HOST_DOCKER_SOCKET).exists()
}

/// Session is live when the slot child is running or its journal pid still exists.
#[must_use]
pub fn observe_session(child: Option<&mut Child>, pid: Option<u32>) -> bool {
    if let Some(child) = child {
        if child.try_wait().ok().flatten().is_none() {
            return true;
        }
    }
    pid.is_some_and(pid_is_alive)
}

/// SIGNAL 0 existence check. Does not deliver a signal.
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) only tests whether `pid` exists.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0
}

/// Persist routing observation for the controller to read.
///
/// # Errors
/// Directory or write failures.
pub fn write_routing(state_dir: &Path, valid: bool, group_valid: bool) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(ROUTING_FILE);
    let body = serde_json::to_vec(&RoutingObservation { valid, group_valid })?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Record that preflight proved the transitional executor.
///
/// # Errors
/// Directory or write failures.
pub fn write_executor_ok(state_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(EXECUTOR_OK);
    std::fs::write(&path, b"ok\n")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-prove-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn missing_routing_file_is_invalid() {
        let dir = tmp("missing");
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn written_routing_is_observed() {
        let dir = tmp("written");
        write_routing(&dir, true, true).unwrap();
        assert_eq!(
            observe_routing(&dir),
            RoutingObservation {
                valid: true,
                group_valid: true
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn executor_ok_file_is_proof() {
        let dir = tmp("exec");
        assert!(!dir.join(EXECUTOR_OK).exists());
        write_executor_ok(&dir).unwrap();
        assert!(observe_executor(&dir));
        std::fs::remove_dir_all(dir).ok();
    }
}
