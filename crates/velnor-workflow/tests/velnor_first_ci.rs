//! GitHub-default CI contract: automatic events run GitHub-hosted jobs,
//! omitted dispatch is github, Velnor is opt-in dispatch only, and the
//! required check is `CI / Required`.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]

use std::collections::BTreeSet;
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
        "github-default-ci-{name}-{}-{id}",
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
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n",
    )
    .unwrap();
}

fn generate_with(root: &Path, extra: &[&str]) -> Generated {
    let output = root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ));
    let _ = fs::remove_dir_all(&output);
    let mut args = vec![
        "--plain",
        "--default-branch",
        "main",
        "--output",
        output.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    args.push(root.to_str().unwrap());
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(&args)
        .output()
        .expect("run velnor-workflow");
    assert!(
        outcome.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    Generated { output }
}

fn generate(root: &Path) -> Generated {
    generate_with(root, &[])
}

fn generate_fail(root: &Path, extra: &[&str]) -> String {
    let output = root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ));
    let _ = fs::remove_dir_all(&output);
    let mut args = vec![
        "--plain",
        "--default-branch",
        "main",
        "--output",
        output.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    args.push(root.to_str().unwrap());
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(&args)
        .output()
        .expect("run velnor-workflow");
    assert!(
        !outcome.status.success(),
        "generation should have failed:\n{}",
        String::from_utf8_lossy(&outcome.stdout)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&outcome.stderr),
        String::from_utf8_lossy(&outcome.stdout)
    )
}

#[test]
fn omitted_cli_runners_defaults_to_github() {
    let help = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .arg("--help")
        .output()
        .expect("help");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        !text.contains("default: velnor"),
        "CLI help must not default to velnor: {text}"
    );
}

#[test]
fn adopt_is_rejected() {
    let root = unique_dir("adopt-rejected");
    write_rust_fixture(&root, 1);
    let error = generate_fail(&root, &["--adopt"]);
    assert!(
        error.contains("unexpected argument") || error.contains("--adopt"),
        "unexpected --adopt error: {error}"
    );
}

#[test]
fn pull_request_and_merge_group_publish_required() {
    let root = unique_dir("pr-gate");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("on:\n  pull_request:"));
    assert!(pr.contains("  merge_group:"));
    assert!(pr.contains("  plan:"));
    assert!(pr.contains("  ci-required:"));
    assert!(pr.contains("    name: ci-required"));
    assert!(pr.contains("  required:"));
    assert!(pr.contains("    name: Required"));
    assert!(!pr.contains("default: velnor"), "{pr}");
    assert!(pr.contains("default: github"), "{pr}");
    assert!(pr.contains("runs-on: ubuntu-24.04"));
}

#[test]
fn automatic_pr_schedules_github_hosted_unit_jobs() {
    let root = unique_dir("pr-github-default");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(unit.contains("runs-on: ubuntu-24.04"));
    assert!(
        unit.contains("github.event_name == 'pull_request'"),
        "automatic PR must enable the GitHub-hosted lane: {unit}"
    );
    assert!(
        unit.contains(
            "github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both')"
        ),
        "Velnor jobs must be dispatch-only: {unit}"
    );
    assert!(
        !unit.contains("default: velnor"),
        "generated YAML must not default to velnor: {unit}"
    );
}

#[test]
fn dispatch_defaults_to_github_and_omitted_runner_selects_github() {
    let root = unique_dir("dispatch");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("workflow_dispatch:"), "{name}");
        assert!(workflow.contains("default: github"), "{name}");
        assert!(!workflow.contains("default: velnor"), "{name}");
        assert!(workflow.contains("- github"), "{name}");
        assert!(workflow.contains("- both"), "{name}");
        assert!(
            workflow.contains("default: github"),
            "omitted dispatch runner must default to GitHub: {name}"
        );
        assert!(
            workflow.contains("options:\n          - affected"),
            "{name}"
        );
        assert!(workflow.contains("- full"), "{name}");
        assert!(
            workflow.contains("CI_SCOPE_OVERRIDE: ${{ github.event.inputs.scope || '' }}"),
            "{name}"
        );
        assert!(workflow.contains("base_sha:"), "{name}");
        assert!(workflow.contains("default: refs/heads/main"), "{name}");
    }
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("github.event.inputs.runner == 'github'")
            && unit.contains("github.event.inputs.runner == ''"),
        "omitted dispatch runner must select GitHub unit jobs: {unit}"
    );
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("default: affected"));
    assert!(pr.contains("github.event.inputs.base_sha"));
    assert!(pr.contains("github.event.before || 'refs/heads/main'"));
    let main = generated.workflow("ci-main.yml");
    assert!(main.contains("default: full"));
}

#[test]
fn pull_request_on_velnor_is_rejected() {
    let root = unique_dir("pr-on-velnor");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\npull_request_on_velnor = true\n",
    )
    .unwrap();
    let error = generate_fail(&root, &[]);
    assert!(
        error.contains("pull_request_on_velnor"),
        "untrusted Velnor PR opt-in must fail closed: {error}"
    );
}

#[test]
fn command_arrays_in_generation_config_are_rejected() {
    let root = unique_dir("command-arrays");
    write_rust_fixture(&root, 1);
    let mut config = fs::read_to_string(root.join(".github-gen/velnor-workflow.toml")).unwrap();
    config.push_str("\n[[units]]\nid = \"rust-crate00\"\npr_commands = [\"cargo test\"]\n");
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config).unwrap();
    let error = generate_fail(&root, &[]);
    assert!(
        error.contains("command arrays"),
        "command arrays must be rejected: {error}"
    );
}

#[test]
fn static_workflow_templates_path_is_rejected() {
    let root = unique_dir("templates-path");
    write_rust_fixture(&root, 1);
    let mut config = fs::read_to_string(root.join(".github-gen/velnor-workflow.toml")).unwrap();
    config.push_str("\ntemplates = \".github/ci/workflow-templates\"\n");
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config).unwrap();
    let error = generate_fail(&root, &[]);
    assert!(
        error.contains("templates"),
        "template paths must be rejected: {error}"
    );
}

#[test]
fn generate_twice_is_byte_identical_and_check_passes() {
    let root = unique_dir("generate-twice");
    write_rust_fixture(&root, 2);
    let first = generate_with(&root, &[]);
    let second_output = root.parent().unwrap().join(format!(
        "{}-out-2",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ));
    let _ = fs::remove_dir_all(&second_output);
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            second_output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("second generate");
    assert!(
        outcome.status.success(),
        "second generate failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    let first_pr = fs::read(first.output.join(".github/workflows/ci-pr.yml")).unwrap();
    let second_pr = fs::read(second_output.join(".github/workflows/ci-pr.yml")).unwrap();
    assert_eq!(first_pr, second_pr);
    let check = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--check",
            "--default-branch",
            "main",
            "--output",
            first.output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("check");
    assert!(
        check.status.success(),
        "check after generate failed:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
}

#[test]
fn delete_generated_outputs_and_regenerate_from_evidence() {
    let root = unique_dir("regen-from-evidence");
    write_rust_fixture(&root, 2);
    let first = generate(&root);
    let before = fs::read(first.output.join(".github/workflows/ci-pr.yml")).unwrap();
    fs::remove_dir_all(first.output.join(".github/workflows")).unwrap();
    let second = generate_with(&root, &[]);
    let after = fs::read(second.output.join(".github/workflows/ci-pr.yml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn velnor_lane_installs_declared_mise_tools() {
    let root = unique_dir("mise-velnor");
    write_rust_fixture(&root, 1);
    fs::write(root.join("mise.toml"), "[tools]\nnode = \"24.20.0\"\n").unwrap();
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("Install declared Mise tools"),
        "Velnor lane must install lockfile tools: {unit}"
    );
    assert!(
        unit.contains("mise --yes install"),
        "Velnor lane must run mise install: {unit}"
    );
}

#[test]
fn kind_reusable_renders_each_unit_root_in_its_own_job() {
    let root = unique_dir("per-unit-capabilities");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let workflow = generated.workflow("ci-unit-rust.yml");
    for unit in ["rust-crate00", "rust-crate01"] {
        assert!(
            workflow.contains(&format!("inputs.unit == '{unit}'")),
            "{unit} must select its own reusable job"
        );
        assert!(workflow.contains("CI_UNIT_ID: ${{ inputs.unit }}"));
    }
    assert!(workflow.contains("cd -- 'crates/crate00' && cargo fetch --locked"));
    assert!(workflow.contains("cd -- 'crates/crate01' && cargo fetch --locked"));
}

#[test]
fn kind_reusable_preserves_declared_contract_per_unit() {
    let root = unique_dir("per-unit-contracts");
    write_rust_fixture(&root, 2);
    let config_path = root.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str(
        r#"

[[declare]]
primitive = "rust-crate-pipeline"
units = ["rust-crate00"]

[declare.args]
jobs = ["github"]
timeout_minutes = 17
cache = "actions"

[[declare]]
primitive = "rust-crate-pipeline"
units = ["rust-crate01"]
"#,
    );
    fs::write(config_path, config).unwrap();

    let generated = generate(&root);
    let workflow = generated.workflow("ci-unit-rust.yml");
    assert!(workflow.contains("name: \"GitHub / rust-crate00\""));
    assert!(workflow.contains("timeout-minutes: 17"));
    assert!(workflow.contains("name: \"Velnor / rust-crate01\""));
    assert!(workflow.contains("name: \"GitHub / rust-crate01\""));
    assert!(!workflow.contains("name: \"Velnor / rust-crate00\""));
}

#[test]
fn unique_reusable_calls_stay_under_github_limit() {
    let root = unique_dir("reusable-limit");
    write_rust_fixture(&root, 12);
    let generated = generate(&root);
    let pr = generated.workflow("ci-pr.yml");
    let calls: BTreeSet<&str> = pr
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("uses: ./.github/workflows/")
                .map(str::trim)
        })
        .collect();
    assert!(
        calls.len() <= 50,
        "unique reusable calls {} exceed GitHub's limit: {calls:?}",
        calls.len()
    );
    assert!(calls.contains("ci-unit-rust.yml"));
}
