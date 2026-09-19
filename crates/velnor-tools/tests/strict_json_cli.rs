use std::path::{Path, PathBuf};
use std::process::Command;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run_estate_fixture(fixture: &Path) -> Result<std::process::Output, String> {
    Command::new(env!("CARGO_BIN_EXE_velnor-tools"))
        .current_dir(repository_root())
        .args([
            "audit-ci",
            "--repo-path",
            ".",
            "--estate",
            fixture.to_str().unwrap_or("invalid-fixture-path"),
            "--offline",
        ])
        .output()
        .map_err(|error| format!("run audit-ci fixture: {error}"))
}

#[test]
fn actual_cli_rejects_root_and_nested_duplicate_fixtures() -> Result<(), String> {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/strict-json");
    for fixture in [
        "root-duplicate-equal.json",
        "root-duplicate-conflicting.json",
        "nested-duplicate-equal.json",
        "nested-duplicate-conflicting.json",
    ] {
        let output = run_estate_fixture(&fixture_dir.join(fixture))?;
        assert!(!output.status.success(), "accepted CLI fixture {fixture}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("duplicate JSON object key"),
            "fixture {fixture} did not fail at strict parser: {stderr}"
        );
    }
    Ok(())
}

#[test]
fn actual_cli_valid_control_reaches_existing_freshness_guard() -> Result<(), String> {
    let output = run_estate_fixture(&repository_root().join("config/estate-repositories.json"))?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("estate audit cannot skip delivered-default freshness checks"),
        "valid control did not pass parsing and scope validation: {stderr}"
    );
    assert!(!stderr.contains("duplicate JSON object key"), "{stderr}");
    Ok(())
}
