//! The scheduled-checks contract of the primitive registry.
//!
//! The fixture is a one-crate repository with four declared check profiles:
//! a required hosted smoke probe, an advisory hosted load probe that waits on
//! it and uploads a bundle, a required Velnor fleet probe, and an advisory
//! Apple-lane compat probe on its own weekly cadence. Generation renders two
//! scheduled workflow files — one per cadence — and every profile's platform,
//! tasks, dependencies, timeout, artifacts, thresholds, and status travel
//! from the config into the rendered jobs.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A generated tree, and the fixture it came from.
struct Generated {
    output: PathBuf,
}

impl Generated {
    fn workflow_files(&self) -> Vec<String> {
        let mut names = fs::read_dir(self.output.join(".github/workflows"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn workflow(&self, name: &str) -> String {
        fs::read_to_string(self.output.join(".github/workflows").join(name)).unwrap()
    }
}

fn copy_fixture(destination: &Path) -> PathBuf {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/check-profiles-workspace");
    copy_tree(&source, destination);
    destination.to_path_buf()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn generate(root: &Path) -> Generated {
    let output = root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ));
    let _ = fs::remove_dir_all(&output);
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--providers",
            "github-hosted,velnor",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        outcome.status.success(),
        "generation failed for {}:\n{}",
        root.display(),
        String::from_utf8_lossy(&outcome.stderr)
    );
    Generated { output }
}

fn job_block(workflow: &str, id: &str) -> Option<String> {
    let header = format!("  {id}:");
    let mut lines = Vec::new();
    let mut inside = false;
    for line in workflow.lines() {
        if line == header {
            inside = true;
            lines.push(line);
            continue;
        }
        if inside {
            if line.starts_with("  ") && !line.starts_with("   ") {
                break;
            }
            lines.push(line);
        }
    }
    if inside {
        Some(lines.join("\n"))
    } else {
        None
    }
}

fn tempfile() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "velnor-check-profiles-test-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&base).unwrap();
    base.join("workspace")
}

#[test]
fn each_cadence_renders_its_own_scheduled_file() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let generated = generate(&root);

    let files = generated.workflow_files();
    assert!(
        files.contains(&"scheduled-daily.yml".to_owned()),
        "{files:?}"
    );
    assert!(
        files.contains(&"scheduled-weekly.yml".to_owned()),
        "{files:?}"
    );

    let daily = generated.workflow("scheduled-daily.yml");
    assert!(daily.contains("- cron: \"23 2 * * *\""), "{daily}");
    assert!(daily.contains("name: scheduled-daily"), "{daily}");
    assert!(daily.contains("  smoke:\n"), "{daily}");
    assert!(daily.contains("  load:\n"), "{daily}");
    assert!(daily.contains("  fleet:\n"), "{daily}");
    assert!(!daily.contains("  compat:\n"), "{daily}");

    let weekly = generated.workflow("scheduled-weekly.yml");
    assert!(weekly.contains("- cron: \"41 4 * * 1\""), "{weekly}");
    assert!(weekly.contains("  compat:\n"), "{weekly}");
    assert!(!weekly.contains("  smoke:\n"), "{weekly}");
}

#[test]
fn required_and_advisory_profiles_split_status() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let generated = generate(&root);

    let daily = generated.workflow("scheduled-daily.yml");
    assert!(
        !job_block(&daily, "smoke")
            .unwrap()
            .contains("continue-on-error"),
        "{daily}"
    );
    assert!(
        job_block(&daily, "load")
            .unwrap()
            .contains("continue-on-error: true"),
        "{daily}"
    );
    assert!(
        !job_block(&daily, "fleet")
            .unwrap()
            .contains("continue-on-error"),
        "{daily}"
    );
    let weekly = generated.workflow("scheduled-weekly.yml");
    assert!(
        job_block(&weekly, "compat")
            .unwrap()
            .contains("continue-on-error: true"),
        "{weekly}"
    );
}

#[test]
fn profile_platforms_tasks_timeouts_and_dependencies_render() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let generated = generate(&root);

    let daily = generated.workflow("scheduled-daily.yml");
    let smoke = job_block(&daily, "smoke").unwrap();
    assert!(smoke.contains("runs-on: ubuntu-24.04"), "{smoke}");
    assert!(smoke.contains("timeout-minutes: 30"), "{smoke}");
    assert!(smoke.contains("mise run check-smoke"), "{smoke}");

    let load = job_block(&daily, "load").unwrap();
    assert!(load.contains("needs: [smoke]"), "{load}");
    assert!(load.contains("timeout-minutes: 90"), "{load}");
    assert!(load.contains("mise run check-load\n"), "{load}");
    assert!(load.contains("mise run check-load-report"), "{load}");
    assert!(load.contains("MAX_SECONDS: \"300\""), "{load}");
    assert!(load.contains("Upload load artifacts"), "{load}");
    assert!(load.contains("load-results/"), "{load}");

    let fleet = job_block(&daily, "fleet").unwrap();
    assert!(
        fleet.contains("runs-on: [self-hosted, example-lane]"),
        "{fleet}"
    );
    assert!(fleet.contains("timeout-minutes: 45"), "{fleet}");
    assert!(fleet.contains("mise run check-fleet"), "{fleet}");

    let weekly = generated.workflow("scheduled-weekly.yml");
    let compat = job_block(&weekly, "compat").unwrap();
    assert!(compat.contains("runs-on: macos-26"), "{compat}");
    assert!(compat.contains("timeout-minutes: 60"), "{compat}");
    assert!(compat.contains("install_args: ripgrep"), "{compat}");
    assert!(compat.contains("mise run check-compat"), "{compat}");
}

#[test]
fn an_unrendered_profile_stops_generation() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let config = root.join(".github-gen/velnor-workflow.toml");
    let before = fs::read_to_string(&config).unwrap();
    let after = before.replace(
        "profiles = [\"smoke\", \"load\", \"fleet\"]",
        "profiles = [\"smoke\", \"load\"]",
    );
    fs::write(&config, after).unwrap();
    let output = root.parent().unwrap().join("uncovered-out");
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--providers",
            "github-hosted,velnor",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        !outcome.status.success(),
        "an unrendered profile must fail closed, and did not"
    );
    assert!(
        String::from_utf8_lossy(&outcome.stderr).contains("fleet"),
        "{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
}

#[test]
fn evented_file_renders_push_pr_cron_and_pr_only_cancel() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let config = root.join(".github-gen/velnor-workflow.toml");
    let before = fs::read_to_string(&config).unwrap();
    let after = before.replace(
        "profiles = [\"smoke\", \"load\", \"fleet\"]",
        "profiles = [\"smoke\", \"load\", \"fleet\"]\nevents = [\"push\", \"pull_request\"]",
    );
    assert_ne!(before, after, "the daily row must gain file-level events");
    fs::write(&config, after).unwrap();
    let generated = generate(&root);

    let daily = generated.workflow("scheduled-daily.yml");
    assert!(daily.contains("  push:\n"), "{daily}");
    assert!(daily.contains("  pull_request:\n"), "{daily}");
    assert!(daily.contains("- cron: \"23 2 * * *\""), "{daily}");
    assert!(daily.contains("workflow_dispatch:"), "{daily}");
    let push = daily.find("  push:\n").unwrap();
    let pull = daily.find("  pull_request:\n").unwrap();
    let cron = daily.find("- cron:").unwrap();
    let dispatch = daily.find("workflow_dispatch:").unwrap();
    assert!(push < pull && pull < cron && cron < dispatch, "{daily}");
    assert!(
        daily.contains("cancel-in-progress: ${{ github.event_name == 'pull_request' }}"),
        "{daily}"
    );

    let weekly = generated.workflow("scheduled-weekly.yml");
    assert!(!weekly.contains("  push:\n"), "{weekly}");
    assert!(!weekly.contains("pull_request:"), "{weekly}");
    assert!(weekly.contains("cancel-in-progress: true"), "{weekly}");
}

#[test]
fn an_unknown_event_stops_generation() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let config = root.join(".github-gen/velnor-workflow.toml");
    let before = fs::read_to_string(&config).unwrap();
    let after = before.replace(
        "profiles = [\"smoke\", \"load\", \"fleet\"]",
        "profiles = [\"smoke\", \"load\", \"fleet\"]\nevents = [\"release\"]",
    );
    assert_ne!(before, after, "the daily row must gain an unknown event");
    fs::write(&config, after).unwrap();
    let output = root.parent().unwrap().join("unknown-event-out");
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--providers",
            "github-hosted,velnor",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        !outcome.status.success(),
        "an unknown event must fail closed, and did not"
    );
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(stderr.contains("scheduled-daily.yml"), "{stderr}");
    assert!(stderr.contains("release"), "{stderr}");
    assert!(stderr.contains("accepted events"), "{stderr}");
}

/// A shared-selection error names the declare row's file, not the primitive
/// every scheduled row shares: the coverage pre-check labels selection with
/// the file, exactly like the render path.
#[test]
fn mixed_schedules_name_the_file() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let config = root.join(".github-gen/velnor-workflow.toml");
    let before = fs::read_to_string(&config).unwrap();
    let after = before.replace(
        "id = \"fleet\"\nname = \"Fleet probe\"\nschedule = \"23 2 * * *\"",
        "id = \"fleet\"\nname = \"Fleet probe\"\nschedule = \"24 2 * * *\"",
    );
    assert_ne!(before, after, "the fleet profile must gain its own cadence");
    fs::write(&config, after).unwrap();
    let output = root.parent().unwrap().join("mixed-schedules-out");
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--providers",
            "github-hosted,velnor",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        !outcome.status.success(),
        "mixed schedules in one file must fail closed, and did not"
    );
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(stderr.contains("scheduled-daily.yml"), "{stderr}");
    assert!(stderr.contains("one cadence"), "{stderr}");
}
