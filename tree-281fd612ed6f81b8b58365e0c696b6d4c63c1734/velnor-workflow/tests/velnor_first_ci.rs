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
use std::io::Write as _;
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

fn enable_approved_velnor_pull_requests(root: &Path) {
    let approved = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github-gen/velnor-workflow.toml"),
    )
    .unwrap();
    let path = root.join(".github-gen/velnor-workflow.toml");
    let current = fs::read_to_string(&path).unwrap();
    let mut lines = current
        .lines()
        .filter(|line| {
            ![
                "velnor_labels =",
                "velnor_runner_group =",
                "pull_request_on_velnor =",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for prefix in [
        "velnor_labels =",
        "velnor_runner_group =",
        "pull_request_on_velnor =",
    ] {
        if let Some(line) = approved.lines().find(|line| line.starts_with(prefix)) {
            lines.push(line.to_owned());
        }
    }
    fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

fn generator_output(root: &Path) -> std::process::Output {
    let output = root.parent().unwrap().join(format!(
        "{}-generator-output",
        root.file_name().unwrap().to_string_lossy()
    ));
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
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
        .expect("run velnor-workflow")
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
fn omitted_cli_runners_defaults_to_both() {
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
        text.contains("default: both") || text.to_lowercase().contains("[default: both]"),
        "CLI help must default runners to both: {text}"
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
fn pull_request_publish_required() {
    let root = unique_dir("pr-gate");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("on:\n  pull_request:"));
    assert!(!pr.contains("  merge_group:"));
    assert!(pr.contains("  plan:"));
    assert!(pr.contains("  ci-required:"));
    assert!(pr.contains("    name: ci-required"));
    assert!(pr.contains("  required:"));
    assert!(pr.contains("    name: \"Control / Required\""));
    assert!(!pr.contains("default: velnor"), "{pr}");
    assert!(pr.contains("default: both"), "{pr}");
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
        unit.contains("github.event_name == 'workflow_dispatch' && (github.event.inputs.runner == 'velnor' || github.event.inputs.runner == 'both'"),
        "Velnor jobs must stay dispatch-selectable: {unit}"
    );
    assert!(
        unit.contains("github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'schedule'"),
        "inferred automatic=both runs Velnor on trusted push/schedule: {unit}"
    );
    assert!(
        !unit.contains("default: velnor"),
        "generated YAML must not default to velnor: {unit}"
    );
    assert!(
        !unit.contains(
            "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository"
        ),
        "without pull_request_on_velnor, Velnor must not admit PRs: {unit}"
    );
}

#[test]
fn dispatch_defaults_to_github_and_omitted_runner_selects_github() {
    let root = unique_dir("dispatch");
    write_rust_fixture(&root, 2);
    // The stock fixture declares runners = "both" with no automatic lane, so
    // generation exercises the inferred-GitHub dispatch default below.
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("workflow_dispatch:"), "{name}");
        assert!(workflow.contains("default: both"), "{name}");
        assert!(!workflow.contains("default: velnor"), "{name}");
        assert!(workflow.contains("- github"), "{name}");
        assert!(workflow.contains("- both"), "{name}");
        assert!(
            workflow.contains("default: both"),
            "omitted dispatch runner must default to both: {name}"
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
fn dispatch_runner_default_is_configurable() {
    let root = unique_dir("dispatch-default");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"velnor\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\ndefault_dispatch_runner = \"velnor\"\n",
    )
    .unwrap();
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("default: velnor"), "{name}");
    }
}

#[test]
fn velnor_only_runners_limit_dispatch_runner_options() {
    let root = unique_dir("dispatch-velnor-only");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"velnor\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\ndefault_dispatch_runner = \"velnor\"\n",
    )
    .unwrap();
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml", "nightly.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("default: velnor"), "{name}");
        assert!(workflow.contains("options:\n          - velnor"), "{name}");
        assert!(!workflow.contains("          - github"), "{name}");
        assert!(!workflow.contains("          - both"), "{name}");
    }
}

#[test]
fn github_only_runners_limit_dispatch_runner_options() {
    let root = unique_dir("dispatch-github-only");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n",
    )
    .unwrap();
    let generated = generate(&root);
    let main = generated.workflow("ci-main.yml");
    assert!(main.contains("default: github"));
    assert!(main.contains("options:\n          - github"));
    assert!(!main.contains("options:\n          - velnor"));
    assert!(!main.contains("\n  velnor-"));
}

#[test]
fn pull_request_on_velnor_opt_in_admits_automatic_pr() {
    let root = unique_dir("pr-on-velnor");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"both\"\nautomatic = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\npull_request_on_velnor = true\n",
    )
    .unwrap();
    enable_approved_velnor_pull_requests(&root);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository"),
        "opt-in Velnor lane must admit pull_request: {unit}"
    );
    let project = fs::read_to_string(generated.output.join(".github/ci/project.toml")).unwrap();
    assert!(project.contains("runners = \"both\""), "{project}");
    assert!(
        !project.contains("automatic"),
        "packaged plan rejects unknown field automatic: {project}"
    );
    let main = generated.workflow("ci-main.yml");
    assert!(
        main.contains("rev: abc81a948d328583ebe757c2d0b62da5bee3ed74"),
        "foreign Planning installs the published pin: {main}"
    );
    assert!(
        !main.contains(
            "uses: tailrocks/velnor/.github/actions/setup-velnor-workflow@${{ github.sha }}"
        ),
        "GitHub forbids expressions in uses: {main}"
    );
    assert!(
        !main.contains("rev: ${{ github.sha }}"),
        "foreign Planning must not cargo-install a foreign github.sha: {main}"
    );
    assert!(
        !main.contains("runs-on: { group:"),
        "both-mode Planning must not require Velnor: {main}"
    );
}

#[test]
fn dual_lane_automatic_velnor_units_pass_policy() {
    let root = unique_dir("dual-lane-both-policy");
    write_rust_fixture(&root, 1);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nrunners = \"both\"\nautomatic = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\npull_request_on_velnor = true\n",
    )
    .unwrap();
    enable_approved_velnor_pull_requests(&root);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository"),
        "dual-lane Velnor units must emit the same-repo PR gate: {unit}"
    );
    fs::create_dir_all(generated.output.join(".github-gen")).unwrap();
    fs::copy(
        root.join(".github-gen/velnor-workflow.toml"),
        generated.output.join(".github-gen/velnor-workflow.toml"),
    )
    .unwrap();
    let pin = generated
        .workflow("ci-policy.yml")
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("VELNOR_WORKFLOW_POLICY_REVISION: ")
                .map(str::to_owned)
        })
        .expect("generated policy job must pin VELNOR_WORKFLOW_POLICY_REVISION");
    let run_policy = |message: &str| {
        let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .args([
                "policy",
                "--workflow-root",
                generated.output.to_str().unwrap(),
                "--approved-policy-revision",
                &pin,
            ])
            .output()
            .expect("run velnor-workflow policy");
        assert!(
            outcome.status.success(),
            "{message}:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&outcome.stdout),
            String::from_utf8_lossy(&outcome.stderr)
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&outcome.stdout),
            String::from_utf8_lossy(&outcome.stderr)
        );
        assert!(
            !combined.contains("self-hosted jobs require a default-branch trusted-event gate"),
            "{message} reported trusted-event findings:\n{combined}"
        );
    };
    run_policy("policy on rendered dual-lane tree");
    fs::remove_dir_all(generated.output.join(".github/ci")).unwrap();
    run_policy("policy on advisory sparse checkout without project.toml");
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
fn pull_request_on_velnor_rejects_an_arbitrary_runner_contract() {
    let root = unique_dir("pr-on-velnor-invalid-runner");
    write_rust_fixture(&root, 1);
    fs::OpenOptions::new()
        .append(true)
        .open(root.join(".github-gen/velnor-workflow.toml"))
        .unwrap()
        .write_all(b"pull_request_on_velnor = true\n")
        .unwrap();
    let outcome = generator_output(&root);
    assert!(!outcome.status.success());
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("approved Velnor runner contract"));
}

#[test]
fn pull_request_on_velnor_accepts_labels_only_approved_contract() {
    let root = unique_dir("pr-on-velnor-labels-only");
    write_rust_fixture(&root, 1);
    enable_approved_velnor_pull_requests(&root);
    let path = root.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&path).unwrap();
    let labels = config
        .lines()
        .find(|line| line.starts_with("velnor_labels ="))
        .expect("approved config must declare velnor_labels");
    let yaml_labels = labels
        .trim_start_matches("velnor_labels = [")
        .trim_end_matches(']')
        .replace('"', "");
    let filtered = config
        .lines()
        .filter(|line| !line.starts_with("velnor_runner_group ="))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, format!("{filtered}\n")).unwrap();
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    let expected = format!("runs-on: [{yaml_labels}]");
    assert!(
        unit.contains(&expected),
        "labels-only contract must emit a claimable sequence {expected}: {unit}"
    );
    assert!(
        !unit.contains("group:"),
        "labels-only contract must not emit an org runner group: {unit}"
    );
}

#[test]
fn pull_request_on_velnor_rejects_a_github_only_runner_mode() {
    let root = unique_dir("pr-on-velnor-invalid-mode");
    write_rust_fixture(&root, 1);
    let path = root.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&path)
        .unwrap()
        .replace("runners = \"both\"", "runners = \"github\"");
    fs::write(&path, format!("{config}pull_request_on_velnor = true\n")).unwrap();
    let outcome = generator_output(&root);
    assert!(!outcome.status.success());
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("Velnor runner lane"));
}

#[test]
fn velnor_lane_installs_declared_mise_tools() {
    let root = unique_dir("mise-velnor");
    write_rust_fixture(&root, 1);
    fs::write(
        root.join("mise.toml"),
        "[settings]\nlockfile = true\n\n[tools]\nnode = \"24.20.0\"\n\"aqua:nextest-rs/nextest/cargo-nextest\" = \"0.9.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("mise.lock"),
        "[[tools.\"aqua:nextest-rs/nextest/cargo-nextest\"]]\nversion = \"0.9.0\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".config")).unwrap();
    fs::write(
        root.join(".config/nextest.toml"),
        "[profile.ci]\nfail-fast = false\n",
    )
    .unwrap();
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("Install declared Mise tools"),
        "Velnor lane must install lockfile tools: {unit}"
    );
    assert!(
        unit.contains("mise --yes install aqua:nextest-rs/nextest/cargo-nextest"),
        "Velnor lane must install only unit-scoped tools: {unit}"
    );
    assert!(
        !unit.contains("mise --yes install\n"),
        "Velnor lane must not install the whole root manifest: {unit}"
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
            "{unit} must gate its collapsed verify steps"
        );
        assert!(
            workflow.contains("CI_UNIT_ID: ${{ inputs.unit }}"),
            "{unit} must bind CI_UNIT_ID from the caller's unit input"
        );
    }
    assert!(workflow.contains("unit:\n        required: true"));
    assert!(workflow.contains("  verify-github:"));
    assert!(workflow.contains("  verify-velnor:"));
    assert!(!workflow.contains("github-prepare-cargo-sources:"));
    assert!(workflow.contains("velnor-prepare-cargo-sources:"));
    assert!(!workflow.contains("needs: [github-prepare-cargo-sources]"));
    assert!(!workflow.contains("needs: [velnor-prepare-cargo-sources]"));
    assert!(
        !workflow.contains("unknown unit for cargo fetch"),
        "kind unit jobs must not repeat per-unit fetch bodies"
    );

    let github_job = workflow
        .split_once("  verify-github:\n")
        .and_then(|(_, body)| body.split_once("\n  verify-velnor:\n"))
        .map_or("", |(body, _)| body);
    assert!(
        github_job.contains("Restore \"Rust crate (crate00)\" cache"),
        "GitHub jobs must retain their per-job Cargo cache: {github_job}"
    );
    assert!(
        github_job.contains("Prepare Cargo sources") && github_job.contains("cargo fetch --locked"),
        "GitHub jobs must fetch Cargo sources in their own workspace: {github_job}"
    );
    assert!(
        github_job.contains("CARGO_NET_OFFLINE: \"true\""),
        "GitHub verification must remain offline after its local fetch: {github_job}"
    );

    let velnor_prep = workflow
        .split_once("  velnor-prepare-cargo-sources:\n")
        .and_then(|(_, body)| body.split_once("\n  verify-github:\n"))
        .map_or("", |(body, _)| body);
    assert!(
        velnor_prep.contains("github.event_name == 'workflow_dispatch'")
            && velnor_prep.contains("github.ref == 'refs/heads/main'"),
        "Velnor prep must use the Velnor lane event gate: {velnor_prep}"
    );
    assert!(!velnor_prep.contains("Restore Rust toolchain"));
    assert!(!velnor_prep.contains("Provision Rust toolchain"));
}

#[test]
fn kind_reusable_caller_is_one_call_per_kind() {
    let root = unique_dir("one-call");
    write_rust_fixture(&root, 8);
    let generated = generate(&root);
    let pr = generated.workflow("ci-pr.yml");
    assert!(
        pr.matches("uses: ./.github/workflows/ci-unit-rust.yml")
            .count()
            >= 16,
        "each selected (unit, lane) gets its own caller: {pr}"
    );
    assert!(pr.contains("  prepare-cargo:\n    name: \"Control / Prepare Cargo\""));
    assert!(pr.contains("  github-rust-crate00:\n    name: \"Rust · crate00\""));
    assert!(pr.contains("  velnor-rust-crate00:\n    name: \"Rust · crate00\""));
    assert!(pr.contains("      unit: rust-crate00"));
    assert!(pr.contains("      lane: github"));
    assert!(pr.contains("      lane: velnor"));
    assert!(pr.contains("      lane: control"));
    assert!(!pr.contains("strategy:"));
    assert!(!pr.contains("matrix.unit"));
    assert!(!pr.contains("name: ${{ matrix.label }}"));
    assert!(pr.contains("selected_units: ${{ needs.plan.outputs.units }}"));
    assert!(pr.contains("full_units: ${{ needs.plan.outputs.full_units }}"));
    assert!(pr.contains("base_sha: ${{ needs.plan.outputs.base_sha }}"));
    assert!(pr.contains("head_sha: ${{ needs.plan.outputs.head_sha }}"));
    assert!(!pr.contains("selection-artifact"));
}

#[test]
fn kind_reusable_materializes_selection_from_inputs_without_artifact() {
    let root = unique_dir("selection-materialize");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(unit.contains("name: Materialize Velnor CI selection"));
    assert!(unit.contains("SELECTION_UNITS: ${{ inputs.selected_units }}"));
    assert!(unit.contains("SELECTION_FULL_UNITS: ${{ inputs.full_units }}"));
    assert!(unit.contains("full_units:\n        required: true"));
    assert!(!unit.contains("Download Velnor CI selection"));
    assert!(!unit.contains("selection-artifact"));
    let pr = generated.workflow("ci-pr.yml");
    assert!(!pr.contains("Publish Velnor CI selection"));
    assert!(!pr.contains("velnor-ci-selection\n          path:"));
}

#[test]
fn kind_reusable_jobs_are_linear_in_units_not_a_matrix_product() {
    let root = unique_dir("linear-jobs");
    write_rust_fixture(&root, 8);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert_eq!(unit.matches("  verify-github:").count(), 1);
    assert_eq!(unit.matches("  verify-velnor:").count(), 1);
    for index in 0..8 {
        assert!(
            unit.contains(&format!("inputs.unit == 'rust-crate{index:02}'")),
            "each unit must gate its collapsed steps"
        );
    }
    let pr = generated.workflow("ci-pr.yml");
    assert_eq!(
        pr.matches("uses: ./.github/workflows/ci-unit-rust.yml")
            .count(),
        17
    );
}

#[test]
fn kind_reusable_consumes_caller_plan_shas() {
    let root = unique_dir("plan-shas");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(unit.contains("BASE_SHA: ${{ inputs.base_sha }}"));
    assert!(unit.contains("HEAD_SHA: ${{ inputs.head_sha }}"));
    assert!(!unit.contains("github.event.pull_request.base.sha"));
    assert!(!unit.contains("HEAD_SHA: ${{ github.sha }}"));
    assert!(unit.contains("base_sha:\n        required: true"));
    assert!(unit.contains("head_sha:\n        required: true"));
    assert!(unit.contains("selected_units:\n        required: true"));
    assert!(unit.contains("lane:\n        required: true"));
    assert!(unit.contains("      unit:\n        required: true"));
}

#[test]
fn kind_reusable_preserves_declared_contract_per_unit() {
    let root = unique_dir("per-unit-contracts");
    write_rust_fixture(&root, 2);
    let config_path = root.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&config_path).unwrap().replace(
        "runners = \"velnor\"",
        "runners = \"both\"\nautomatic = \"both\"",
    );
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
    assert!(workflow.contains("inputs.unit == 'rust-crate00'"));
    assert!(
        workflow.contains("timeout-minutes: 45"),
        "collapsed verify uses the shard's max declared timeout"
    );
    assert!(workflow.contains("inputs.unit == 'rust-crate01'"));
    assert!(!workflow.contains("inputs.unit == 'rust-crate00' && inputs.lane == 'velnor'"));
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
    assert!(
        calls.iter().all(|file| file.starts_with("ci-unit-rust")),
        "rust units must stay on kind shards, not per-unit files: {calls:?}"
    );
    assert!(calls.contains("ci-unit-rust.yml"));
    assert!(pr.contains("contains(format(',{0},', needs.plan.outputs.units), ',rust-crate"));
    assert!(!pr.contains("fromJSON(needs.plan.outputs.rust_matrix)"));
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
    assert!(unit.contains("CI_UNIT_ID: ${{ inputs.unit }}"));
    assert!(unit.contains("--unit \"$CI_UNIT_ID\""));
    assert!(!unit.contains("--unit crate00"));
}

#[test]
fn trust_gated_unit_appends_trusted_label_on_velnor_lane_only() {
    let root = unique_dir("trust-gated-unit");
    write_rust_fixture(&root, 2);
    let plain = generate(&root);
    let project = fs::read_to_string(plain.output.join(".github/ci/project.toml")).unwrap();
    let gated = project
        .lines()
        .filter_map(|line| line.strip_prefix("id = \""))
        .map(|line| line.trim_end_matches('"').to_owned())
        .next()
        .unwrap();
    let path = root.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&path).unwrap();
    let _ = writeln!(
        config,
        "velnor_trusted_label = \"example-trusted\"\nvelnor_trusted_runner_available = true\n\n[[units]]\nid = \"{gated}\"\nrequires_trusted = true"
    );
    fs::write(&path, config).unwrap();
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("runs-on: [self-hosted, example-runner, example-trusted]"),
        "gated Velnor job must append the trusted label: {unit}"
    );
    assert!(
        unit.contains("runs-on: [self-hosted, example-runner]"),
        "ungated Velnor jobs must keep the base labels: {unit}"
    );
    assert!(
        unit.contains("runs-on: ubuntu-24.04"),
        "GitHub lane must keep its hosted label: {unit}"
    );
    let project = fs::read_to_string(generated.output.join(".github/ci/project.toml")).unwrap();
    assert!(
        !project.contains("requires_trusted") && !project.contains("velnor_trusted_label"),
        "pinned Planning runtimes reject unknown fields: {project}"
    );
}

#[test]
fn trust_gated_unit_without_a_label_fails_generation() {
    let root = unique_dir("trust-gated-unlabeled");
    write_rust_fixture(&root, 1);
    let plain = generate(&root);
    let project = fs::read_to_string(plain.output.join(".github/ci/project.toml")).unwrap();
    let gated = project
        .lines()
        .filter_map(|line| line.strip_prefix("id = \""))
        .map(|line| line.trim_end_matches('"').to_owned())
        .next()
        .unwrap();
    let path = root.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&path).unwrap();
    let _ = writeln!(
        config,
        "\n[[units]]\nid = \"{gated}\"\nrequires_trusted = true"
    );
    fs::write(&path, config).unwrap();
    let error = generate_fail(&root, &[]);
    assert!(
        error.contains("velnor_trusted_label is not declared"),
        "unexpected error: {error}"
    );
}
