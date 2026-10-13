//! Layout skeleton tests.
//!
//! Prove the four skeleton guarantees: five unique classes, layout validation
//! success on the canonical repo root, missing-root failure, and the exact CLI
//! success text.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use velnor_actions_generator::{ALL_CLASSES, REQUIRED_LAYOUT, validate_layout};

/// Absolute path to the canonical repository root (two levels above the crate).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root resolves")
}

#[test]
fn five_unique_classes() {
    assert_eq!(ALL_CLASSES.len(), 5, "exactly five classes");
    let unique: HashSet<_> = ALL_CLASSES.iter().collect();
    assert_eq!(unique.len(), 5, "all five classes are distinct");
}

#[test]
fn validate_layout_succeeds_on_repo_root() {
    assert_eq!(REQUIRED_LAYOUT.len(), 2, "exactly two required roots");
    validate_layout(&repo_root()).expect("repo root exposes actions/ and templates/");
}

#[test]
fn validate_layout_fails_on_missing_root() {
    let missing = repo_root().join("this-directory-does-not-exist");
    assert!(
        validate_layout(&missing).is_err(),
        "a path without the required roots must fail validation"
    );
}

#[test]
fn cli_check_prints_exact_success_text() {
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-actions-generator"))
        .arg("check")
        .arg("--root")
        .arg(repo_root())
        .output()
        .expect("generator binary runs");
    assert!(output.status.success(), "check exits zero on a valid root");
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf8 stdout"),
        "skeleton valid: 5 classes, 2 roots\n"
    );
}
