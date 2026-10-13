//! Canonical Velnor Actions fleet — generator skeleton library.
//!
//! This crate is the headless seam for the canonical fleet repository. It exposes
//! the stable repository-class taxonomy and the required repository layout so the
//! CLI can prove the skeleton is well formed. Plans 005 and 006 EXTEND this API and
//! these roots; they must not replace them.

use std::path::{Path, PathBuf};

pub mod audit;
pub mod model;
pub mod render;

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

/// Generate every deterministic output from the declared fleet data under `root`.
///
/// Always renders the five consumer class templates to
/// `templates/<class>/ci.yml`. When `fleet/block-sha` is bound to a 40-hex commit
/// SHA, also renders the five owner-local callable workflows to
/// `.github/workflows/ci-<class>.yml`, pinning their internal composite closure to
/// that SHA. Returns the written paths in a stable order.
///
/// # Errors
///
/// Returns `Err` on any data-contract violation, a malformed block SHA, or an I/O
/// failure while writing.
pub fn generate(root: &Path) -> Result<Vec<PathBuf>, String> {
    let manifest = model::FleetManifest::load(root)?;
    let mut written = Vec::new();

    for class in ALL_CLASSES {
        let body = render::consumer_template(class);
        let dir = root.join("templates").join(class.code());
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let path = dir.join("ci.yml");
        write_if_changed(&path, &body)?;
        written.push(path);
    }

    let block_sha_path = root.join("fleet").join("block-sha");
    if block_sha_path.exists() {
        let raw = std::fs::read_to_string(&block_sha_path)
            .map_err(|e| format!("reading {}: {e}", block_sha_path.display()))?;
        let block_sha = raw.trim();
        if !model::is_sha40(block_sha) {
            return Err(format!(
                "fleet/block-sha {block_sha:?} is not a 40 lowercase hex commit SHA"
            ));
        }
        let dir = root.join(".github").join("workflows");
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        for class in ALL_CLASSES {
            let body = render::callable_workflow(manifest.class(class), block_sha);
            let path = dir.join(render::callable_file_name(class));
            write_if_changed(&path, &body)?;
            written.push(path);
        }
    }

    Ok(written)
}

/// Materialize one consumer repository's `ci.yml` from its class template,
/// replacing the shared release placeholders with `release_sha` and `calver`, and
/// write only `<output>/.github/workflows/ci.yml`.
///
/// # Errors
///
/// Returns `Err` if the repository is not a fleet member, the release identity is
/// invalid, a placeholder survives, or writing fails.
pub fn render_consumer_to_dir(
    root: &Path,
    repository: &str,
    release_sha: &str,
    calver: &str,
    output: &Path,
) -> Result<PathBuf, String> {
    let manifest = model::FleetManifest::load(root)?;
    let repo = manifest
        .repositories()
        .iter()
        .find(|r| r.slug == repository)
        .ok_or_else(|| format!("{repository:?} is not a fleet member"))?;
    let template = render::consumer_template(repo.class);
    let body = render::render_consumer(&template, release_sha, calver)?;
    let dir = output.join(".github").join("workflows");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join("ci.yml");
    std::fs::write(&path, &body).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(path)
}

fn write_if_changed(path: &Path, body: &str) -> Result<(), String> {
    if let Ok(existing) = std::fs::read_to_string(path)
        && existing == body
    {
        return Ok(());
    }
    std::fs::write(path, body).map_err(|e| format!("writing {}: {e}", path.display()))
}
