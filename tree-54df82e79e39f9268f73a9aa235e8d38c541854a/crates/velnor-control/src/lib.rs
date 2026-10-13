//! Daemon-side control-plane application services.
//!
//! Transport adapters are isolated here by Plan 067; this crate depends only on
//! the shared model types and never on Clap or runner internals.

/// Marker service seam; Plan 067 fills in the versioned control API.
pub const CONTROL_SERVICE: &str = "velnor-control";

pub mod journal;
pub mod store;

#[cfg(test)]
mod tests {
    #[test]
    fn service_marker_is_stable() {
        assert_eq!(super::CONTROL_SERVICE, "velnor-control");
    }
}
