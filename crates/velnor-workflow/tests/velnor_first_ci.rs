//! Velnor-first CI contract: PR jobs run, unique reusables stay under GitHub's
//! limit, and manual dispatch can select runner and scope without regen.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Generated {
    output: PathBuf,
}

impl Generated {
    fn workflow(&self, name: &str) -> String {
        fs::read_to_string(self.output.join(".github/workflows").join(name)).unwrap()
    }
}

fn unique_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "velnor-first-ci-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_rust_fixture(root: &Path, crates: usize) {
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    )
    .unwrap();
    let mut workspace = String::from("[workspace]\nmembers = [\n");
    for index in 0..crates {
        let name = format!("crate{index:02}");
        let _ = writeln!(workspace, "  \"crates/{name}\",");
        let dir = root.join("crates").join(&name).join("src");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            root.join("crates").join(&name).join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(dir.join("lib.rs"), "pub fn n() -> u8 { 1 }\n").unwrap();
    }
    workspace.push_str("]\n");
    fs::write(root.join("Cargo.toml"), workspace).unwrap();
    fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"velnor\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n",
    )
    .unwrap();
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
            "--runners",
            "velnor",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        outcome.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    Generated { output }
}

fn unique_reusable_calls(workflow: &str) -> BTreeSet<&str> {
    workflow
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("uses: ./.github/workflows/")
                .map(str::trim)
        })
        .collect()
}

use std::collections::BTreeSet;

#[test]
fn pull_request_plan_and_required_are_not_main_only() {
    let root = unique_dir("pr-gate");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("on:\n  pull_request:"));
    assert!(pr.contains("github.event_name == 'pull_request'"));
    assert!(
        !pr.contains("github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')"),
        "PR jobs must not be gated to the trusted main-only expression: {pr}"
    );
    assert!(pr.contains("  plan:"));
    assert!(pr.contains("  ci-required:"));
    assert!(pr.contains("runs-on: [self-hosted, example-runner]"));
}

#[test]
fn automatic_pr_does_not_schedule_github_hosted_unit_jobs() {
    let root = unique_dir("pr-velnor-only");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(unit.contains("runs-on: [self-hosted, example-runner]"));
    assert!(unit.contains("runs-on: ubuntu-24.04"));
    assert!(
        unit.contains("github.event.inputs.runner == 'github'"),
        "GitHub-hosted jobs exist only for manual dispatch"
    );
    let github_if = unit
        .lines()
        .find(|line| line.contains("github.event.inputs.runner == 'github'"))
        .unwrap();
    assert!(
        github_if.contains("workflow_dispatch"),
        "automatic PR must not enable the GitHub-hosted lane: {github_if}"
    );
    assert!(
        !github_if.contains("pull_request"),
        "automatic PR must not enable the GitHub-hosted lane: {github_if}"
    );
}

#[test]
fn dispatch_exposes_runner_and_scope_without_committed_flips() {
    let root = unique_dir("dispatch");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("workflow_dispatch:"), "{name}");
        assert!(workflow.contains("default: velnor"), "{name}");
        assert!(workflow.contains("- github"), "{name}");
        assert!(workflow.contains("- both"), "{name}");
        assert!(workflow.contains("options:\n          - affected"), "{name}");
        assert!(workflow.contains("- full"), "{name}");
        assert!(workflow.contains("CI_SCOPE_OVERRIDE: ${{ github.event.inputs.scope || '' }}"), "{name}");
    }
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("default: affected"));
    let main = generated.workflow("ci-main.yml");
    assert!(main.contains("default: full"));
}

#[test]
fn fifty_one_units_stay_under_github_unique_reusable_limit() {
    let root = unique_dir("unique-reusables");
    write_rust_fixture(&root, 51);
    let generated = generate(&root);
    let pr = generated.workflow("ci-pr.yml");
    let unique = unique_reusable_calls(&pr);
    assert!(
        unique.len() <= 50,
        "unique reusable workflows {} exceed GitHub's limit: {unique:?}",
        unique.len()
    );
    assert_eq!(unique, BTreeSet::from(["ci-unit-rust.yml"]));
    assert!(pr.contains("fromJSON(needs.plan.outputs.rust_matrix)"));
    assert!(!generated
        .output
        .join(".github/workflows/ci-rust-crate00.yml")
        .exists());
}

#[test]
fn unit_run_consumes_the_selection_artifact_not_a_hardcoded_id() {
    let root = unique_dir("selection");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(unit.contains("VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection"));
    assert!(unit.contains("--unit \"$CI_UNIT_ID\""));
    assert!(!unit.contains("--unit crate00"));
}
