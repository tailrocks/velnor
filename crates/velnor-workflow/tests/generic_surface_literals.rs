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
    "package-release.v1",
];

fn source_files(directory: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(directory);
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let entries = std::fs::read_dir(&current)
            .unwrap_or_else(|error| panic!("read source directory {}: {error}", current.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn generic_modules_never_name_a_repository() {
    let mut offenders = Vec::new();
    for directory in ["primitives", "scan", "config"] {
        for path in source_files(directory) {
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            for literal in DENY_LIST {
                if source.contains(literal) {
                    offenders.push(format!("{} names `{literal}`", path.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "repository literals in generic modules:\n{}",
        offenders.join("\n")
    );
}
