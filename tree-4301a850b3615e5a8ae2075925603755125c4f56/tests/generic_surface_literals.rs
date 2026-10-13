//! The law under test: the generic modules name families and units, never a
//! repository. These literals are the consumer repositories this phase ports;
//! if one reaches the registry, the scan + config split is broken.

#![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

use std::path::{Path, PathBuf};

const DENY_LIST: &[&str] = &[
    "tailrocks/",
    "velnor-apt",
    "velnor-actions-fixture",
    "velnor-trusted",
    "velnor-target-mvp",
    "velnor-runner",
    "rust-velnor",
    "bun-velnor",
    "rust-production-topology",
    "crates/velnor",
    "ChainArgos",
    "chainargos",
    "jackin",
    "holla",
    "package-release.v1",
];

/// The modules that carry the estate: the catalog and the runtime bootstrap
/// name their repositories because that is their job. Every other module under
/// `src/` is generic and must stay so.
const ADMITTED_FILES: &[&str] = &["src/lib.rs", "src/runtime.rs"];

/// The estate's reviewed workflow bodies. They name their own repositories by
/// design, so the tree is admitted as a whole rather than file by file.
const ADMITTED_DIRECTORIES: &[&str] = &["templates"];

/// Everything the deny list applies to: the crate's Rust sources and the
/// workflow templates it renders.
fn scanned_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.join("src"), root.join("templates")];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .unwrap_or_else(|error| panic!("read source directory {}: {error}", current.display()));
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

/// Whether a path sits in a module allowed to name a repository.
fn admitted(root: &Path, path: &Path) -> bool {
    ADMITTED_FILES.iter().any(|file| path.ends_with(file))
        || ADMITTED_DIRECTORIES
            .iter()
            .any(|directory| path.starts_with(root.join(directory)))
}

#[test]
fn generic_modules_never_name_a_repository() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    for path in scanned_files(root) {
        if admitted(root, &path) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        for literal in DENY_LIST {
            if source.contains(literal) {
                offenders.push(format!("{} names `{literal}`", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "repository literals outside the estate modules:\n{}",
        offenders.join("\n")
    );
}

/// The admitted set is not an excuse: the modules and the tree it names have to
/// be there, so a rename cannot silently widen what the deny list covers.
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
