//! Canonical local control-plane endpoint selection.
//!
//! The client keeps the two Unix sockets distinct: read and stream operations
//! use `control.sock`, while mutations use `admin.sock`. No caller-provided
//! header or retry can change that routing decision.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::socket_root;

/// Canonical API version negotiated by the local client.
pub const API_VERSION: &str = "v1";

/// The socket selected for one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketKind {
    /// Read-only queries and observation streams.
    Control,
    /// Mutating lifecycle and reconciliation operations.
    Admin,
}

/// Validated `unix://<socket-root>/<instance>` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixEndpoint {
    root: PathBuf,
    instance: String,
}

impl UnixEndpoint {
    /// Parse the canonical directory URI for the active socket root.
    pub fn parse(uri: &str) -> Result<Self, EndpointError> {
        let path = uri
            .strip_prefix("unix://")
            .ok_or(EndpointError::InvalidScheme)?;
        if path.is_empty() || path.contains('?') || path.contains('#') {
            return Err(EndpointError::InvalidPath);
        }
        let path = Path::new(path);
        let root = socket_root::socket_root();
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| EndpointError::InvalidPath)?;
        if relative.components().count() != 1 {
            return Err(EndpointError::InvalidPath);
        }
        let instance = relative
            .as_os_str()
            .to_str()
            .ok_or(EndpointError::InvalidInstance)?;
        validate_instance(instance)?;
        Ok(Self {
            root: root.join(instance),
            instance: instance.to_owned(),
        })
    }

    /// Build an endpoint from a validated instance name.
    pub fn from_instance(instance: &str) -> Result<Self, EndpointError> {
        validate_instance(instance)?;
        Ok(Self {
            root: socket_root::socket_root().join(instance),
            instance: instance.to_owned(),
        })
    }

    /// Instance name carried by this endpoint.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Socket path for the operation class.
    #[must_use]
    pub fn socket_path(&self, kind: SocketKind) -> PathBuf {
        self.root.join(match kind {
            SocketKind::Control => "control.sock",
            SocketKind::Admin => "admin.sock",
        })
    }

    /// Canonical directory URI.
    #[must_use]
    pub fn uri(&self) -> String {
        format!("unix://{}", self.root.display())
    }
}

/// Endpoint validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    /// URI was not prefixed with `unix://`.
    InvalidScheme,
    /// URI path was not exactly `<socket-root>/<instance>`.
    InvalidPath,
    /// Instance name violated the local authorization grammar.
    InvalidInstance,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidScheme => "endpoint must use unix://",
            Self::InvalidPath => "endpoint must be unix://<socket-root>/<instance>",
            Self::InvalidInstance => "instance must match [a-z0-9][a-z0-9_-]{0,63}",
        })
    }
}

impl std::error::Error for EndpointError {}

fn validate_instance(instance: &str) -> Result<(), EndpointError> {
    if instance.is_empty() || instance.len() > 64 {
        return Err(EndpointError::InvalidInstance);
    }
    let bytes = instance.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return Err(EndpointError::InvalidInstance);
    }
    if !bytes.iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
    }) {
        return Err(EndpointError::InvalidInstance);
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    #[test]
    fn routes_read_and_mutation_to_different_sockets() {
        let endpoint = UnixEndpoint::from_instance("primary").expect("valid endpoint");
        let control = endpoint.socket_path(SocketKind::Control);
        let admin = endpoint.socket_path(SocketKind::Admin);
        assert_ne!(control, admin);
        assert_eq!(control.file_name(), Some(std::ffi::OsStr::new("control.sock")));
        assert_eq!(admin.file_name(), Some(std::ffi::OsStr::new("admin.sock")));
    }

    #[test]
    fn parser_rejects_invalid_schemes_and_paths() {
        for uri in ["tcp:///run/velnor/a", "unix://", "unix://?query=1"] {
            assert!(UnixEndpoint::parse(uri).is_err(), "accepted {uri}");
        }
    }
}
