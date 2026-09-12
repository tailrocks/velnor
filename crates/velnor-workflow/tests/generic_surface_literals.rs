//! The law under test: the crate names families, units, and the generator's
//! own distribution paths — never a consumer repository. Every literal here
//! was deleted from the crate during the estate ports; if one reaches the
//! generic engine again, the scan + config split is broken.
//!
//! The one admitted boundary is `src/estate.rs`: it still carries the catalog
//! entries of consumers that have not adopted a repo-owned generation config
//! yet, and each of those entries is a port away from this file.

#![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

use std::path::{Path, PathBuf};

/// Repository slugs and estate identifiers this crate must never name again.
/// Each entry is one deleted profile, template family, unit id, or runner
/// placement; the list only ever grows.
const DENY_LIST: &[&str] = &[
    // Consumer repositories.
    "tailrocks/velnor-apt",
    "velnor-apt",
    "velnor-actions-fixture",
    "tailrocks/holla",
    "holla",
    "holla-apt",
    "jackin",
    "tailrocks/termrock",
    "tailrocks/parallax",
    "parallax",
    "ruxel",
    "ChainArgos",
    "chainargos",
    "tailrocks/homebrew-tap",
    "agent-brown",
    "java-monorepo",
    // Estate runner placement.
    "velnor-trusted",
    "velnor-target-mvp",
    // Estate unit ids and template families.
    "rust-velnor",
    "bun-velnor",
    "rust-production-topology",
    "package-release.v1",
];

/// The estate boundary: the only module allowed to name a repository.
const ADMITTED_FILES: &[&str] = &["src/estate.rs"];

/// Test fixtures are inputs a test authors on purpose; they never reach the
/// rendered surface of another repository.
const ADMITTED_DIRECTORIES: &[&str] = &["tests/fixtures"];

/// The generator's own distribution paths (`tailrocks/velnor/.github/...`) and
/// the regeneration marker are generator identity, not consumer knowledge. The
/// bare slug may appear exactly `BARE_GENERATOR_SLUG_OCCURRENCES` times: today
/// that is the regeneration marker constant alone.
const BARE_GENERATOR_SLUG_OCCURRENCES: usize = 1;

/// Everything the deny list applies to: the crate's Rust sources and the
/// workflow templates it renders, if any are ever checked back in.
fn scanned_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.join("src"), root.join("templates"), root.join("tests")];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("read source directory {}: {error}", current.display()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

/// Whether a path sits in a module allowed to name a repository. This test is
/// its own exception: the deny list has to spell out what it forbids.
fn admitted(root: &Path, path: &Path) -> bool {
    path.ends_with("tests/generic_surface_literals.rs")
        || ADMITTED_FILES.iter().any(|file| path.ends_with(file))
        || ADMITTED_DIRECTORIES
            .iter()
            .any(|directory| path.starts_with(root.join(directory)))
}

/// Occurrences of the generator slug that are part of a `.github/...` path are
/// the generator pointing at its own infrastructure.
fn is_generator_path(line: &str) -> bool {
    let mut rest = line;
    while let Some(start) = rest.find("tailrocks/velnor") {
        rest = &rest[start + "tailrocks/velnor".len()..];
        if rest.starts_with('/') || rest.starts_with(".github") {
            return true;
        }
    }
    false
}

#[test]
fn generic_modules_never_name_a_repository() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    let mut bare_slug_sites = Vec::new();
    for path in scanned_files(root) {
        if admitted(root, &path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        for (number, line) in source.lines().enumerate() {
            for literal in DENY_LIST {
                if line.contains(literal) {
                    offenders.push(format!(
                        "{}:{} names `{literal}`",
                        path.display(),
                        number + 1
                    ));
                }
            }
            if line.contains("tailrocks/velnor") && !is_generator_path(line) {
                bare_slug_sites.push(format!("{}:{}", path.display(), number + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "repository literals outside the estate modules:\n{}",
        offenders.join("\n")
    );
    assert_eq!(
        bare_slug_sites.len(),
        BARE_GENERATOR_SLUG_OCCURRENCES,
        "the bare generator slug must stay pinned to the regeneration marker: {bare_slug_sites:?}"
    );
}

/// The admitted set is not an excuse: the estate boundary has to be there, and
/// the directories it covers cannot silently disappear or move.
#[test]
fn the_admitted_estate_modules_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ADMITTED_FILES {
        assert!(
            root.join(file).is_file(),
            "admitted module {file} is gone; revisit the deny list"
        );
    }
    for directory in ADMITTED_DIRECTORIES {
        assert!(
            root.join(directory).is_dir(),
            "admitted directory {directory} is gone; revisit the deny list"
        );
    }
}
