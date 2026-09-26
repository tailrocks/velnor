//! Pinned upstream revision for the scale-set protocol.
//!
//! Source of truth is `actions/scaleset`; every path, header, status code,
//! and retry semantic in this module traces to the revision below. Fixture
//! manifests record the same commit and verification fails closed on drift.

use velnor_model::SCALESET_UPSTREAM_COMMIT;

/// Audited `actions/scaleset` revision.
///
/// `e6daac70` ("Bump the actions group", #127): Go sources byte-identical to
/// `fb563005` (only CI workflow diffs after it), so the #113 listener API,
/// session client, token chain, and error taxonomy are exactly the audited
/// shapes. Chosen over `fb563005` because it is the newest audited ref.
pub const UPSTREAM_COMMIT: &str = SCALESET_UPSTREAM_COMMIT;

/// Upstream repository the pin refers to.
pub const UPSTREAM_REPO: &str = "https://github.com/actions/scaleset";

/// Assert a recorded fixture manifest was captured against this pin.
/// Fails closed: unknown or drifted commits are an error, never a warning.
pub fn require_pin(commit: &str) -> anyhow::Result<()> {
    if commit == UPSTREAM_COMMIT {
        Ok(())
    } else {
        anyhow::bail!(
            "scale-set fixture pin drift: manifest pins {commit}, code pins {UPSTREAM_COMMIT} ({UPSTREAM_REPO})"
        );
    }
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
    fn pin_is_full_audited_sha() {
        assert_eq!(UPSTREAM_COMMIT.len(), 40);
        assert_eq!(UPSTREAM_COMMIT, "e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5");
    }

    #[test]
    fn matching_pin_passes_drifted_fails_closed() {
        assert!(require_pin(UPSTREAM_COMMIT).is_ok());
        assert!(require_pin("fb56300503fd21caa788feeb85c63071d15155c6").is_err());
        assert!(require_pin("").is_err());
    }
}
