//! Shared versioned model types for the Velnor control plane, transport
//! contract, and operator CLI.
//!
//! Dependency law (Plan 064): every other new crate may depend on this one;
//! this crate depends on none of them and never on Clap or Axum.

/// Crate version reported by `velnorctl version` until Plan 065 owns the full
/// versioned resource envelope.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_version_is_reported() {
        assert!(!super::CRATE_VERSION.is_empty());
    }
}
