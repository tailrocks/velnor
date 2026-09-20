#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "the CLI regression harness should fail at the exact fixture operation"
)]

use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn temporary_fixture_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("velnor-estate-authority-{nonce}"))
}

fn run_audit(root: &Path, fixture: &Value) -> Output {
    let directory = temporary_fixture_dir();
    fs::create_dir_all(&directory).expect("create fixture directory");
    let path = directory.join("caller-estate.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(fixture).expect("serialize caller fixture"),
    )
    .expect("write caller fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-tools"))
        .args([
            "audit-ci",
            "--repo-path",
            root.to_str().expect("repository root is UTF-8"),
            "--estate",
            path.to_str().expect("fixture path is UTF-8"),
            "--offline",
        ])
        .output()
        .expect("run audit-ci");
    fs::remove_dir_all(directory).expect("remove fixture directory");
    output
}

fn read_canonical_manifest(root: &Path) -> Value {
    serde_json::from_str(
        &fs::read_to_string(root.join("config/estate-repositories.json"))
            .expect("read canonical estate manifest"),
    )
    .expect("parse canonical estate manifest")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn caller_concern_downgrade_is_rejected_before_freshness_or_audit() {
    let root = repository_root();
    let mut fixture = read_canonical_manifest(&root);
    let object = fixture.as_object_mut().expect("manifest object");
    let defaults = object
        .get_mut("defaults")
        .and_then(Value::as_object_mut)
        .expect("defaults object");
    for concern in defaults.values_mut() {
        if matches!(
            concern.get("classification").and_then(Value::as_str),
            Some("required" | "applicable")
        ) {
            concern["classification"] = json!("non-applicable");
            concern["implementations"] = json!([]);
        }
    }
    let repositories = object
        .get_mut("repositories")
        .and_then(Value::as_array_mut)
        .expect("repository array");
    for repository in repositories {
        let concerns = repository
            .get_mut("concerns")
            .and_then(Value::as_object_mut)
            .expect("repository concern object");
        for concern in concerns.values_mut() {
            if matches!(
                concern.get("classification").and_then(Value::as_str),
                Some("required" | "applicable")
            ) {
                concern["classification"] = json!("non-applicable");
                concern["implementations"] = json!([]);
            }
        }
    }

    let output = run_audit(&root, &fixture);
    let error = stderr(&output);
    assert!(
        !output.status.success(),
        "downgraded caller unexpectedly passed"
    );
    assert!(error.contains("caller-plan-not-authority"), "{error}");
    assert!(error.contains("classification downgrade"), "{error}");
    assert!(
        !error.contains("freshness"),
        "caller must fail before freshness: {error}"
    );
}

#[test]
fn caller_duplicate_and_unknown_metadata_are_rejected_by_real_command() {
    let root = repository_root();
    let canonical = read_canonical_manifest(&root);

    let mut duplicate = canonical.clone();
    let repositories = duplicate
        .get_mut("repositories")
        .and_then(Value::as_array_mut)
        .expect("repository array");
    repositories.push(repositories[0].clone());
    let output = run_audit(&root, &duplicate);
    let error = stderr(&output);
    assert!(
        !output.status.success(),
        "duplicate caller unexpectedly passed"
    );
    assert!(error.contains("duplicate=true"), "{error}");

    let mut unknown = canonical;
    unknown["hostile"] = json!("caller metadata");
    let output = run_audit(&root, &unknown);
    let error = stderr(&output);
    assert!(
        !output.status.success(),
        "unknown caller metadata unexpectedly passed"
    );
    assert!(error.contains("unknown field"), "{error}");
}

#[test]
fn caller_default_downgrade_is_rejected_even_when_repository_rows_match() {
    let root = repository_root();
    let mut fixture = read_canonical_manifest(&root);
    let defaults = fixture
        .get_mut("defaults")
        .and_then(Value::as_object_mut)
        .expect("defaults object");
    let lane_selection = defaults
        .get_mut("lane-selection")
        .expect("lane-selection default");
    lane_selection["classification"] = json!("non-applicable");
    lane_selection["implementations"] = json!([]);

    let output = run_audit(&root, &fixture);
    let error = stderr(&output);
    assert!(
        !output.status.success(),
        "default downgrade unexpectedly passed"
    );
    assert!(error.contains("default concern lane-selection"), "{error}");
    assert!(error.contains("classification downgrade"), "{error}");
}

#[test]
fn caller_identity_substitution_is_rejected_before_live_default_lookup() {
    let root = repository_root();
    let mut fixture = read_canonical_manifest(&root);
    let repositories = fixture
        .get_mut("repositories")
        .and_then(Value::as_array_mut)
        .expect("repository array");
    repositories[0]["name"] = json!("tailrocks/not-in-scope");

    let output = run_audit(&root, &fixture);
    let error = stderr(&output);
    assert!(
        !output.status.success(),
        "identity substitution unexpectedly passed"
    );
    assert!(error.contains("missing"), "{error}");
    assert!(error.contains("tailrocks/not-in-scope"), "{error}");
    assert!(
        !error.contains("resolve remote default"),
        "identity must fail before live lookup: {error}"
    );
}
