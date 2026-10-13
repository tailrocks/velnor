//! Client-side transport contract for the Velnor control plane.
//!
//! Boundary law (Plan 064): this crate meets the daemon only through versioned
//! model DTOs. It never depends on `velnor-control`, Axum, daemon internals,
//! runner internals, or Clap.

/// Marker transport seam; later plans own the versioned client implementation.
pub const TRANSPORT_CONTRACT: &str = "velnor-client/v1";

#[cfg(test)]
mod tests {
    #[test]
    fn transport_marker_is_versioned() {
        assert!(super::TRANSPORT_CONTRACT.ends_with("/v1"));
    }
}
