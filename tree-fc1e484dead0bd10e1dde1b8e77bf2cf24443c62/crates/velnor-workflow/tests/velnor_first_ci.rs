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
    // The dogfood repository flipped to schema 2 (R2m): transplant the
    // production Velnor placement from its provider selector into this
    // schema-1 surface. The crate must never spell the estate's labels
    // itself (see generic_surface_literals), so the values flow from the
    // repository's own config at test time.
    let approved = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github-gen/velnor-workflow.toml"),
    )
    .unwrap();
    let selector = "[workflow.selectors.velnor]";
    let mut in_selector = false;
    let mut transplanted = None;
    for line in approved.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_selector = trimmed == selector;
            continue;
        }
        if in_selector && trimmed.starts_with("runs_on =") {
            transplanted = Some(trimmed.replacen("runs_on =", "velnor_labels =", 1));
            break;
        }
    }
    let transplanted = transplanted.expect("the repository declares a velnor provider selector");
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
    lines.push(transplanted);
    lines.push("pull_request_on_velnor = true".to_owned());
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
        main.contains(&format!("rev: {}", velnor_workflow::SOURCE_REVISION)),
        "foreign Planning installs the generator's own revision when the tree declares no pin: {main}"
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

/// The validator on a consumer tree rendered in place: the pin is the
/// generator's own revision (no `[generator] revision` declared), so the
/// running binary is the pinned generator and the tree regenerates
/// byte-identically without a network. Ancestry rules do not apply to a
/// consumer; the semantic rules do.
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
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(["--plain", "--force", "--default-branch", "main"])
        .arg(&root)
        .output()
        .expect("run velnor-workflow in place");
    assert!(
        outcome.status.success(),
        "in-place generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    let unit = fs::read_to_string(root.join(".github/workflows/ci-unit-rust.yml")).unwrap();
    assert!(
        unit.contains("github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository"),
        "dual-lane Velnor units must emit the same-repo PR gate: {unit}"
    );
    let policy = fs::read_to_string(root.join(".github/workflows/ci-policy.yml")).unwrap();
    let pin = policy
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("VELNOR_WORKFLOW_POLICY_REVISION: ")
                .map(str::to_owned)
        })
        .expect("generated policy job must pin VELNOR_WORKFLOW_POLICY_REVISION");
    assert_eq!(pin, velnor_workflow::SOURCE_REVISION);
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "policy",
            "--workflow-root",
            root.to_str().unwrap(),
            "--base-revision",
            &pin,
        ])
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("run velnor-workflow policy");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(
        outcome.status.success(),
        "policy on rendered dual-lane tree:\n{combined}"
    );
    for rule in [
        "pin-declared",
        "pin-reachable",
        "pin-monotonic",
        "entrypoint-pin",
        "generated-tree",
        "pull-request-target",
        "entrypoint-privileges",
        "trusted-runners",
        "action-pins",
        "workflow-structure",
        "required-checks",
    ] {
        assert!(
            combined.contains(&format!("PASS {rule}")),
            "rule {rule} must pass on a rendered consumer tree:\n{combined}"
        );
    }
    assert!(
        combined.contains("not applicable: example/monorepo consumes the generator"),
        "{combined}"
    );
    assert!(!combined.contains("FAIL"), "{combined}");
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

#[cfg(unix)]
fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .status()
        .expect("git present");
    assert!(status.success());
}

#[cfg(unix)]
fn git_output(root: &Path, arguments: &[&str]) -> String {
    let outcome = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("git present");
    assert!(outcome.status.success());
    String::from_utf8_lossy(&outcome.stdout).trim().to_owned()
}

/// A git fixture whose `[generator] revision` names an older commit (the
/// stale-pin case): the running binary's render matches the generated
/// output, so `--check` reaches the D19 guard. Returns the fixture root,
/// the declared pin, and the generated output directory.
#[cfg(unix)]
fn stale_pin_check_fixture(name: &str) -> (PathBuf, String, PathBuf) {
    let root = unique_dir(name);
    write_rust_fixture(&root, 1);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "check@test"]);
    git(&root, &["config", "user.name", "check"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "fixture"]);
    let pin = git_output(&root, &["rev-parse", "HEAD"]);
    let config = root.join(".github-gen/velnor-workflow.toml");
    let body = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        body.replace(
            "[generator]\nrepository = \"example/monorepo\"\n",
            &format!("[generator]\nrepository = \"example/monorepo\"\nrevision = \"{pin}\"\n"),
        ),
    )
    .unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "declare the pin"]);
    let generated = generate(&root);
    (root, pin, generated.output)
}

/// A `cargo` that records any invocation in the returned sentinel file and
/// fails: the fail-closed tests prove the build escape hatch stays shut by
/// proving this shim never fires.
#[cfg(unix)]
fn write_cargo_shim(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let sentinel = directory.join("cargo-shim-fired");
    let shim = directory.join("cargo");
    fs::write(
        &shim,
        "#!/bin/sh\necho \"cargo invoked: $@\" > \"$CARGO_SHIM_SENTINEL\"\nexit 99\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    sentinel
}

#[cfg(unix)]
fn check_command(root: &Path, output: &Path, shim_dir: &Path, sentinel: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
    command
        .args([
            "--plain",
            "--check",
            "--default-branch",
            "main",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", shim_dir.to_str().unwrap()),
        )
        .env("CARGO_SHIM_SENTINEL", sentinel)
        .env_remove("VELNOR_WORKFLOW_PINNED_BINARY")
        .env_remove("VELNOR_WORKFLOW_CANDIDATE_MANIFEST")
        .env_remove("CARGO_NET_OFFLINE");
    command
}

/// `--check` without a provisioned pin fails closed with no cargo
/// invocation, naming `--pin-build`; with `--pin-build` the run proceeds
/// to the build (the shim fires), proving the flag opens exactly that gate.
#[cfg(unix)]
#[test]
fn check_without_a_provisioned_pin_fails_closed_without_invoking_cargo() {
    let (root, pin, output) = stale_pin_check_fixture("check-fail-closed");
    let shim_dir = unique_dir("check-fail-closed-shim");
    let sentinel = write_cargo_shim(&shim_dir);
    let closed = check_command(&root, &output, &shim_dir, &sentinel)
        .output()
        .expect("check");
    assert!(
        !closed.status.success(),
        "an unprovisioned pin fails the check"
    );
    let stderr = String::from_utf8_lossy(&closed.stderr);
    assert!(
        stderr.contains(&pin),
        "the failure names the declared pin: {stderr}"
    );
    assert!(
        stderr.contains("--pin-build"),
        "the failure names the escape hatch: {stderr}"
    );
    assert!(
        stderr.contains("building one is forbidden here"),
        "the failure is the fail-closed guard: {stderr}"
    );
    assert!(
        !sentinel.exists(),
        "the guard compiles nothing: the cargo shim never fired"
    );
    let _ = fs::remove_file(&sentinel);
    let opened = check_command(&root, &output, &shim_dir, &sentinel)
        .arg("--pin-build")
        .output()
        .expect("check with --pin-build");
    assert!(
        !opened.status.success(),
        "the shimmed build fails, proving the run reached it"
    );
    assert!(
        sentinel.exists(),
        "--pin-build opens exactly the build gate: the cargo shim fired"
    );
    let opened_stderr = String::from_utf8_lossy(&opened.stderr);
    assert!(
        opened_stderr.contains(&format!("build velnor-workflow at pin {pin}")),
        "the run proceeded to the build: {opened_stderr}"
    );
}

/// `CARGO_NET_OFFLINE=true` keeps the pin build forbidden even with
/// `--pin-build`: the offline guard outranks the consent flag.
#[cfg(unix)]
#[test]
fn pin_build_honors_cargo_net_offline() {
    let (root, pin, output) = stale_pin_check_fixture("check-offline");
    let shim_dir = unique_dir("check-offline-shim");
    let sentinel = write_cargo_shim(&shim_dir);
    let outcome = check_command(&root, &output, &shim_dir, &sentinel)
        .arg("--pin-build")
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("offline check with --pin-build");
    assert!(!outcome.status.success());
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("building one is forbidden here"),
        "offline stays fail-closed: {stderr}"
    );
    assert!(stderr.contains(&pin), "{stderr}");
    assert!(
        !sentinel.exists(),
        "offline never reaches the build: {stderr}"
    );
}

/// The `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` environment fallback binds the
/// env-slot candidate: a provisioned pin whose render differs reaches the
/// candidate exception, and a manifest naming another tree fails loudly.
#[cfg(unix)]
#[test]
fn candidate_manifest_env_fallback_binds_the_env_slot_candidate() {
    use std::os::unix::fs::PermissionsExt as _;
    let (root, pin, _output) = stale_pin_check_fixture("check-manifest-env");
    // A pin the fixture history cannot contain, so resolution takes the
    // revision fallback against the provisioned fake below.
    let foreign_pin = "0123456789abcdef0123456789abcdef01234567";
    let config = root.join(".github-gen/velnor-workflow.toml");
    let body = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        body.replace(
            &format!("revision = \"{pin}\"\n"),
            &format!("revision = \"{foreign_pin}\"\n"),
        ),
    )
    .unwrap();
    let generated = generate(&root);
    let output = generated.output;
    let head = git_output(&root, &["rev-parse", "HEAD"]);
    let shim_dir = unique_dir("check-manifest-env-shim");
    // The fake proves the foreign pin through the revision fallback, then
    // renders drift so the candidate exception is reached. It lives outside
    // the fixture root so the scan never sees it.
    let fake = shim_dir.join("fake-pin");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {foreign_pin}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc; exit 0; fi\nmkdir -p \"$3/drift\"\necho junk > \"$3/drift/file.txt\"\nexit 0\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = shim_dir.join("candidate-manifest.json");
    fs::write(
        &manifest,
        format!(
            "{{\"profile\":\"debug\",\"platform\":\"Linux-X64\",\"repository\":\"example/monorepo\",\"run_id\":\"1\",\"revision\":\"{head}\",\"closure\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"binary_sha256\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"}}"
        ),
    )
    .unwrap();
    let sentinel = write_cargo_shim(&shim_dir);
    let outcome = check_command(&root, &output, &shim_dir, &sentinel)
        .env("VELNOR_WORKFLOW_PINNED_BINARY", &fake)
        .env("VELNOR_WORKFLOW_CANDIDATE_MANIFEST", &manifest)
        .output()
        .expect("check with env manifest");
    assert!(!outcome.status.success());
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("names closure"),
        "the env manifest is honored and its mismatch fails loudly: {stderr}"
    );
    assert!(
        stderr.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "the failure names the manifest closure: {stderr}"
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
    // The tool list is a per-unit fact: the callee installs exactly the
    // caller's `mise_tools` input, split as shell words.
    assert!(
        unit.contains("MISE_TOOLS: ${{ inputs.mise_tools }}")
            && unit.contains("mise --yes install \"${tools[@]}\""),
        "Velnor lane must install the caller's unit-scoped tools: {unit}"
    );
    assert!(
        !unit.contains("mise --yes install\n"),
        "Velnor lane must not install the whole root manifest: {unit}"
    );
    let pr = generated.workflow("ci-pr.yml");
    assert!(
        pr.contains("      lane: velnor\n")
            && pr.contains("mise_tools: \"aqua:nextest-rs/nextest/cargo-nextest\""),
        "the Velnor caller passes the unit's lockfile tools: {pr}"
    );
}

#[test]
fn declared_mise_tools_propagate_to_non_rust_kind_reusables() {
    let root = unique_dir("mise-non-rust-kind");
    fs::write(
        root.join("mise.toml"),
        "[settings]\nlockfile = true\n\n[tools]\npython = \"3.13.0\"\n\"pipx:reuse\" = \"5.0.2\"\n\n[tasks.ci]\nrun = \"reuse lint\"\n",
    )
    .unwrap();
    fs::write(
        root.join("mise.lock"),
        "[[tools.python]]\nversion = \"3.13.0\"\n\n[[tools.\"pipx:reuse\"]]\nversion = \"5.0.2\"\n",
    )
    .unwrap();
    fs::write(root.join("REUSE.toml"), "version = 1\n").unwrap();
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 1\n\n[generator]\nrepository = \"example/docs-mise\"\n\n[workflow]\nrunners = \"github\"\nautomatic = \"github\"\ndefault_dispatch_runner = \"github\"\nautomatic_lanes = \"github\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n\n[[units]]\nid = \"reuse\"\nkind = \"docs\"\nroot = \".\"\nwatch = [\"REUSE.toml\", \"mise.toml\", \"mise.lock\"]\nci_tasks = [\"ci\"]\nmise_tools = [\"python\", \"pipx:reuse\"]\n",
    )
    .unwrap();
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-docs.yml");
    assert!(
        unit.contains("Set up Mise tools")
            && unit.contains("install_args: ${{ inputs.mise_tools }}"),
        "the docs kind reusable must bootstrap declared mise tools through inputs: {unit}"
    );
    let pr = generated.workflow("ci-pr.yml");
    assert!(
        pr.contains("mise_tools: \"python pipx:reuse\""),
        "the aggregate caller must pass declared mise tools for non-Rust units: {pr}"
    );
    let unit = generated.workflow("ci-unit-docs.yml");
    assert!(
        unit.contains("uses: tailrocks/velnor/.github/actions/report-velnor-ci-outcomes@"),
        "consumer kind reusables must reference the published report action: {unit}"
    );
}

#[test]
fn regen_gate_unit_provisions_the_pinned_policy_runtime_on_the_velnor_lane_only() {
    let root = unique_dir("regen-gate-policy-runtime");
    write_rust_fixture(&root, 2);
    let config = root.join(".github-gen/velnor-workflow.toml");
    let mut contents = fs::read_to_string(&config).unwrap();
    contents.push_str(
        "\n[[declare]]\nprimitive = \"regen-gate\"\nunits = [\"rust-crate00\"]\n\n[declare.args]\ncommand = \"cd -- 'crates/crate00' && cargo run --locked -- --plain --check ../..\"\n",
    );
    fs::write(&config, contents).unwrap();
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    let provision = "      - name: Provision pinned Velnor workflow policy runtime\n        if: ${{ inputs.policy_runtime }}\n";
    assert_eq!(
        unit.matches(provision).count(),
        1,
        "the Velnor lane job provisions the pinned policy binary behind the input gate exactly once: {unit}"
    );
    let (hosted, velnor) = unit
        .split_once("\n  verify-velnor:\n")
        .expect("both lane jobs render");
    assert!(
        !hosted.contains("Provision pinned Velnor workflow policy runtime"),
        "the hosted lane carries the pinned binary in the Planning runtime artifact: {hosted}"
    );
    assert!(
        !velnor.contains("cargo install") && !velnor.contains("cargo build"),
        "the Velnor lane never compiles the policy runtime: {velnor}"
    );
    assert!(
        velnor.contains("gh release download \"$tag\" --repo ")
            && velnor.contains("gh attestation verify \"$temporary/$asset\" --owner tailrocks --signer-workflow tailrocks/velnor/.github/workflows/ci-runtime-products.yml --source-ref refs/heads/main")
            && velnor.contains("gh attestation verify \"$temporary/manifest.json\"")
            && velnor.contains("if [[ \"$existing\" != \"$expected\" ]]; then")
            && velnor.contains("sha256sum \"$binary\"")
            && velnor.contains("\"$binary\" --closure")
            && velnor.contains("velnor-workflow-runtime-v1-")
            && velnor.contains("echo \"VELNOR_WORKFLOW_PINNED_BINARY=$binary\" >> \"$GITHUB_ENV\""),
        "the Velnor lane provisions the pinned product once into the host store and exports it for the D19 guard: {velnor}"
    );
    assert!(
        unit.contains("      policy_runtime:\n        required: false\n        type: boolean\n        default: false\n"),
        "the callee declares the flag: {unit}"
    );
    let pr = generated.workflow("ci-pr.yml");
    let velnor_caller = pr
        .split("\n  velnor-rust-crate00:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n  velnor-rust-crate01:\n").next())
        .expect("the Velnor caller of the regen-gate unit renders");
    assert!(
        velnor_caller.contains("      policy_runtime: true\n"),
        "only the regen-gate unit's Velnor caller passes the flag: {velnor_caller}"
    );
    assert_eq!(
        pr.matches("policy_runtime: true").count(),
        1,
        "no other caller (hosted lane, other units) passes the flag: {pr}"
    );
}

#[test]
fn kind_reusable_renders_each_unit_root_in_its_own_job() {
    let root = unique_dir("per-unit-capabilities");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let workflow = generated.workflow("ci-unit-rust.yml");
    assert!(
        !workflow.contains("inputs.unit == '"),
        "the collapsed verify steps must not be guarded by unit identity"
    );
    assert!(
        workflow.contains("CI_UNIT_ID: ${{ inputs.unit }}"),
        "the checks step binds CI_UNIT_ID from the caller's unit input"
    );
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
        github_job.contains("Restore unit cache")
            && github_job.contains("hashFiles(inputs.cache_key_files)"),
        "GitHub jobs must retain their per-job Cargo cache keyed on the caller's files: {github_job}"
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
    assert!(
        !unit.contains("inputs.unit == '"),
        "the collapsed steps are rendered once, not once per unit"
    );
    assert_eq!(unit.matches("- name: Run unit checks").count(), 2);
    let pr = generated.workflow("ci-pr.yml");
    for index in 0..8 {
        assert!(
            pr.contains(&format!("      unit: rust-crate{index:02}\n")),
            "each unit gets its own caller"
        );
    }
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
    assert!(
        workflow.contains("timeout-minutes: 45"),
        "collapsed verify uses the kind's max declared timeout"
    );
    assert!(!workflow.contains("inputs.unit == '"));
    // The github-only contract keeps rust-crate00 off the Velnor lane: it
    // gets a GitHub caller and no Velnor caller.
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("  github-rust-crate00:"));
    assert!(!pr.contains("  velnor-rust-crate00:"));
    assert!(pr.contains("  velnor-rust-crate01:"));
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
        "rust units must call the single kind reusable, not per-unit files: {calls:?}"
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
