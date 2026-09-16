//! Client-side transport contract for the Velnor control plane.
//!
//! Boundary law (Plan 064): this crate meets the daemon only through versioned
//! model DTOs. It never depends on `velnor-control`, Axum, daemon internals,
//! runner internals, or Clap.

#![forbid(unsafe_code)]

/// Marker transport seam; later plans own the versioned client implementation.
pub const TRANSPORT_CONTRACT: &str = "velnor-client/v1";

pub mod http;
pub mod socket_root;
pub mod unix;

pub use http::{
    ClientError, Info, LogItem, MutationResponse, ResourcePage, ResourceQuery, UnixControlClient,
    WatchItem,
};
pub use socket_root::{
    ensure_socket_root, is_package_socket_mode, socket_root, socket_root_for_storage_root,
};
pub use unix::{EndpointError, SocketKind, UnixEndpoint, API_VERSION};

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
    #[test]
    fn transport_marker_is_versioned() {
        assert!(super::TRANSPORT_CONTRACT.ends_with("/v1"));
    }
}
