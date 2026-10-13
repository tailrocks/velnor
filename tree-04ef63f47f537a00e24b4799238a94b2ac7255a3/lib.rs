//! Canonical Velnor Actions fleet — generator skeleton library.
//!
//! This crate is the headless seam for the canonical fleet repository. It exposes
//! the stable repository-class taxonomy and the required repository layout so the
//! CLI can prove the skeleton is well formed. Plans 005 and 006 EXTEND this API and
//! these roots; they must not replace them.

use std::path::Path;

/// One of the five normalized repository classes the fleet generator maps every
/// canonical repository onto exactly once.
///
/// The variants are declared in canonical order: code, tap, apt, infra, fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepositoryClass {
    /// Rust library and binary code repositories.
    Code,
    /// Homebrew tap repositories.
    Tap,
    /// Debian/apt package repositories.
    Apt,
    /// Shared infrastructure repository.
    Infra,
    /// Test fixture repository.
    Fixture,
}

impl RepositoryClass {
    /// Stable lowercase identifier used in declared data and CLI output.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            RepositoryClass::Code => "code",
            RepositoryClass::Tap => "tap",
            RepositoryClass::Apt => "apt",
            RepositoryClass::Infra => "infra",
            RepositoryClass::Fixture => "fixture",
        }
    }
}

/// The five repository classes in canonical order: code, tap, apt, infra, fixture.
pub const ALL_CLASSES: [RepositoryClass; 5] = [
    RepositoryClass::Code,
    RepositoryClass::Tap,
    RepositoryClass::Apt,
    RepositoryClass::Infra,
    RepositoryClass::Fixture,
];

/// The two required top-level roots every canonical checkout must expose:
/// reusable building blocks (`actions`) and normalized class templates
/// (`templates`).
pub const REQUIRED_LAYOUT: [&str; 2] = ["actions", "templates"];

/// Validate that every required layout root exists as a directory under `root`.
///
/// Returns `Ok(())` when each entry in [`REQUIRED_LAYOUT`] resolves to an existing
/// directory; otherwise returns an error naming the first missing root.
///
/// # Errors
///
/// Returns `Err` with a human-readable message when a required root is missing or
/// is not a directory.
pub fn validate_layout(root: &Path) -> Result<(), String> {
    for entry in REQUIRED_LAYOUT {
        let candidate = root.join(entry);
        if !candidate.is_dir() {
            return Err(format!(
                "missing required layout root: {}",
                candidate.display()
            ));
        }
    }
    Ok(())
}
