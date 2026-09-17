//! Provider-schema CI contract: automatic events admit the configured
//! automatic providers, omitted dispatch selects the universe, local
//! providers carry the fork/bot trust conjunct, and the required check
//! is `CI / Required`.

#![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]
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
        "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nproviders = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\nautomatic_providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[workflow.selectors.github-self-hosted]\nruns_on = [\"example-scale-set\"]\n\n[workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"example-runner\"]\n",
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
fn omitted_cli_providers_keeps_config_universe() {
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
        text.contains("--providers <PROVIDERS>"),
        "CLI help must offer the provider universe override: {text}"
    );
    assert!(
        text.contains("Absent keeps the `[workflow] providers` config value"),
        "CLI help must keep the config universe when --providers is omitted: {text}"
    );
    assert!(
        !text.to_lowercase().contains("runner"),
        "CLI help must not name runner lanes: {text}"
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
    assert!(
        pr.contains("default: github-hosted,github-self-hosted,velnor"),
        "{pr}"
    );
    assert!(pr.contains("runs-on: ubuntu-24.04"));
}

#[test]
fn automatic_pr_admits_every_automatic_provider_with_trust_gates() {
    let root = unique_dir("pr-github-default");
    write_rust_fixture(&root, 2);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    // The stock fixture admits every provider automatically: the hosted job
    // routes to the ephemeral image with no trust conjunct, while each local
    // job carries the fork/bot conjunct on the same automatic events.
    assert!(unit.contains("runs-on: ubuntu-24.04"));
    let gate = |provider: &str| {
        unit.lines()
            .find(|line| {
                line.contains(&format!("inputs.provider == '{provider}'"))
                    && line.trim_start().starts_with("if:")
            })
            .unwrap_or_else(|| panic!("unit must gate provider {provider}: {unit}"))
            .to_owned()
    };
    let hosted = gate("github-hosted");
    assert!(
        !hosted.contains("head.repo.fork"),
        "hosted needs no trust conjunct: {hosted}"
    );
    for provider in ["github-self-hosted", "velnor"] {
        let gated = gate(provider);
        assert!(
            gated.contains("head.repo.fork") && gated.contains("user.type == 'Bot'"),
            "{provider} must carry the fork/bot conjunct: {gated}"
        );
        assert!(
            gated.contains(&format!("providers), ',{provider},'")),
            "{provider} must stay dispatch-selectable: {gated}"
        );
    }
}

#[test]
fn dispatch_defaults_to_the_universe_and_selects_by_provider() {
    let root = unique_dir("dispatch");
    write_rust_fixture(&root, 2);
    // The stock fixture declares the full universe, so generation exercises the
    // default dispatch provider set below.
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("workflow_dispatch:"), "{name}");
        assert!(
            workflow.contains("default: github-hosted,github-self-hosted,velnor"),
            "omitted dispatch providers must default to the universe: {name}"
        );
        assert!(
            workflow.contains("Comma-separated provider subset"),
            "{name}"
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
        unit.contains("contains(format(',{0},', github.event.inputs.providers)"),
        "unit callers must gate on the dispatch provider set: {unit}"
    );
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("default: affected"));
    assert!(pr.contains("github.event.inputs.base_sha"));
    assert!(pr.contains("github.event.before || 'refs/heads/main'"));
    let main = generated.workflow("ci-main.yml");
    assert!(main.contains("default: full"));
}

#[test]
fn dispatch_providers_default_is_configurable() {
    let root = unique_dir("dispatch-default");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nproviders = [\"velnor\"]\ndefault_dispatch_providers = [\"velnor\"]\n",
    )
    .unwrap();
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("default: velnor"), "{name}");
    }
}

#[test]
fn velnor_only_universe_limits_dispatch_provider_options() {
    let root = unique_dir("dispatch-velnor-only");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nproviders = [\"velnor\"]\ndefault_dispatch_providers = [\"velnor\"]\n",
    )
    .unwrap();
    let generated = generate(&root);
    for name in ["ci-pr.yml", "ci-main.yml", "nightly.yml"] {
        let workflow = generated.workflow(name);
        assert!(workflow.contains("default: velnor"), "{name}");
        assert!(
            workflow.contains("subset of [velnor]"),
            "dispatch providers describe only the declared universe: {name}"
        );
        assert!(!workflow.contains("github-hosted"), "{name}");
        assert!(!workflow.contains("github-self-hosted"), "{name}");
    }
}

#[test]
fn hosted_only_universe_limits_dispatch_provider_options() {
    let root = unique_dir("dispatch-github-only");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nproviders = [\"github-hosted\"]\ndefault_dispatch_providers = [\"github-hosted\"]\n",
    )
    .unwrap();
    let generated = generate(&root);
    let main = generated.workflow("ci-main.yml");
    assert!(main.contains("default: github-hosted"));
    assert!(main.contains("subset of [github-hosted]"));
    assert!(!main.contains(",velnor,"));
    assert!(!main.contains("github-self-hosted"));
}

#[test]
fn local_providers_admit_automatic_pr_by_default() {
    let root = unique_dir("pr-on-velnor");
    write_rust_fixture(&root, 2);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nproviders = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\nautomatic_providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\n",
    )
    .unwrap();
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'"),
        "admitted local providers must emit the same-repo non-fork non-bot PR gate: {unit}"
    );
    let project = fs::read_to_string(generated.output.join(".github/ci/project.toml")).unwrap();
    assert!(
        project.contains("\nproviders = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]"),
        "{project}"
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
fn all_provider_automatic_units_pass_policy() {
    let root = unique_dir("all-provider-policy");
    write_rust_fixture(&root, 1);
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\nproviders = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\nautomatic_providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\n",
    )
    .unwrap();
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
        unit.contains("github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'"),
        "admitted providers must emit the same-repo non-fork non-bot PR gate: {unit}"
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
        "policy on rendered all-provider tree:\n{combined}"
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
fn provider_sets_reject_unknown_ids() {
    let root = unique_dir("provider-unknown-id");
    write_rust_fixture(&root, 1);
    let path = root.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&path).unwrap().replace(
        "\nproviders = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]",
        "\nproviders = [\"github\"]",
    );
    fs::write(&path, config).unwrap();
    let error = generate_fail(&root, &[]);
    assert!(
        error.contains("[workflow] providers has unknown provider `github`; expected one of: github-hosted, github-self-hosted, velnor"),
        "legacy lane ids must fail explicitly: {error}"
    );
}

#[test]
fn declared_selectors_route_each_provider() {
    let root = unique_dir("selectors-route");
    write_rust_fixture(&root, 1);
    let generated = generate(&root);
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("runs-on: [self-hosted, example-runner]"),
        "multi-label selector must emit a claimable sequence: {unit}"
    );
    assert!(
        unit.contains("runs-on: ubuntu-24.04"),
        "hosted selector must route the hosted job: {unit}"
    );
    assert!(
        unit.contains("runs-on: example-scale-set"),
        "self-hosted selector must route the self-hosted job: {unit}"
    );
    assert!(
        !unit.contains("group:"),
        "label selectors must not emit an org runner group: {unit}"
    );
}

#[test]
fn automatic_providers_outside_universe_fails() {
    let root = unique_dir("automatic-outside-universe");
    write_rust_fixture(&root, 1);
    let path = root.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&path).unwrap().replace(
        "\nproviders = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]",
        "\nproviders = [\"github-hosted\"]",
    );
    fs::write(&path, config).unwrap();
    let error = generate_fail(&root, &[]);
    assert!(
        error.contains("outside [workflow] providers; dispatch and automatic selections narrow the repo universe, they never widen it"),
        "automatic selections must narrow the universe: {error}"
    );
}

#[test]
fn velnor_provider_installs_declared_mise_tools() {
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
        "Velnor provider must install lockfile tools: {unit}"
    );
    // The tool list is a per-unit fact: the callee installs exactly the
    // caller's `mise_tools` input, split as shell words.
    assert!(
        unit.contains("MISE_TOOLS: ${{ inputs.mise_tools }}")
            && unit.contains("mise --yes install \"${tools[@]}\""),
        "Velnor provider must install the caller's unit-scoped tools: {unit}"
    );
    assert!(
        !unit.contains("mise --yes install\n"),
        "Velnor provider must not install the whole root manifest: {unit}"
    );
    let pr = generated.workflow("ci-pr.yml");
    assert!(
        pr.contains("      provider: velnor\n")
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
        "schema = 2\n\n[generator]\nrepository = \"example/docs-mise\"\n\n[workflow]\nproviders = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\ndefault_dispatch_providers = [\"github-hosted\"]\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[[units]]\nid = \"reuse\"\nkind = \"docs\"\nroot = \".\"\nwatch = [\"REUSE.toml\", \"mise.toml\", \"mise.lock\"]\nci_tasks = [\"ci\"]\nmise_tools = [\"python\", \"pipx:reuse\"]\n",
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
fn regen_gate_unit_provisions_the_pinned_policy_runtime_on_local_providers_only() {
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
        2,
        "each local provider job provisions the pinned policy binary behind the input gate: {unit}"
    );
    let (hosted, local) = unit
        .split_once("\n  verify-github-self-hosted:\n")
        .expect("hosted and local provider jobs render");
    assert!(
        !hosted.contains("Provision pinned Velnor workflow policy runtime"),
        "the hosted provider carries the pinned binary in the Planning runtime artifact: {hosted}"
    );
    let (self_hosted, velnor) = local
        .split_once("\n  verify-velnor:\n")
        .expect("both local provider jobs render");
    for (provider, job) in [("github-self-hosted", self_hosted), ("velnor", velnor)] {
        assert!(
            !job.contains("cargo install") && !job.contains("cargo build"),
            "{provider} never compiles the policy runtime: {job}"
        );
        assert!(
            job.contains("gh release download \"$tag\" --repo ")
                && job.contains("gh attestation verify \"$temporary/$asset\" --owner tailrocks --signer-workflow tailrocks/velnor/.github/workflows/ci-runtime-products.yml --source-ref refs/heads/main")
                && job.contains("gh attestation verify \"$temporary/manifest.json\"")
                && job.contains("if [[ \"$existing\" != \"$expected\" ]]; then")
                && job.contains("sha256sum \"$binary\"")
                && job.contains("\"$binary\" --closure")
                && job.contains("velnor-workflow-runtime-v1-")
                && job.contains("echo \"VELNOR_WORKFLOW_PINNED_BINARY=$binary\" >> \"$GITHUB_ENV\""),
            "{provider} provisions the pinned product once into the host store and exports it for the D19 guard: {job}"
        );
    }
    assert!(
        unit.contains("      policy_runtime:\n        required: false\n        type: boolean\n        default: false\n"),
        "the callee declares the flag: {unit}"
    );
    let pr = generated.workflow("ci-pr.yml");
    for provider in ["github-self-hosted", "velnor"] {
        let caller = pr
            .split(&format!("\n  {provider}-rust-crate00:\n"))
            .nth(1)
            .expect("the regen-gate unit's local caller renders");
        assert!(
            caller.contains("      policy_runtime: true\n"),
            "the regen-gate unit's {provider} caller passes the flag: {caller}"
        );
    }
    assert_eq!(
        pr.matches("policy_runtime: true").count(),
        2,
        "no other caller (hosted provider, other units) passes the flag: {pr}"
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
    assert!(workflow.contains("  verify-github-hosted:"));
    assert!(workflow.contains("  verify-github-self-hosted:"));
    assert!(workflow.contains("  verify-velnor:"));
    assert!(workflow.contains("github-self-hosted-prepare-cargo-sources:"));
    assert!(workflow.contains("velnor-prepare-cargo-sources:"));
    assert!(!workflow.contains("needs: [github-self-hosted-prepare-cargo-sources]"));
    assert!(!workflow.contains("needs: [velnor-prepare-cargo-sources]"));
    assert!(
        !workflow.contains("unknown unit for cargo fetch"),
        "kind unit jobs must not repeat per-unit fetch bodies"
    );

    let hosted_job = workflow
        .split_once("  verify-github-hosted:\n")
        .and_then(|(_, body)| body.split_once("\n  verify-github-self-hosted:\n"))
        .map_or("", |(body, _)| body);
    assert!(
        hosted_job.contains("Restore unit cache")
            && hosted_job.contains("hashFiles(inputs.cache_key_files)"),
        "hosted jobs must retain their per-job Cargo cache keyed on the caller's files: {hosted_job}"
    );
    assert!(
        hosted_job.contains("Prepare Cargo sources") && hosted_job.contains("cargo fetch --locked"),
        "hosted jobs must fetch Cargo sources in their own workspace: {hosted_job}"
    );
    assert!(
        hosted_job.contains("CARGO_NET_OFFLINE: \"true\""),
        "hosted verification must remain offline after its local fetch: {hosted_job}"
    );

    let velnor_prep = workflow
        .split_once("  velnor-prepare-cargo-sources:\n")
        .and_then(|(_, body)| body.split_once("\n  verify-github-hosted:\n"))
        .map_or("", |(body, _)| body);
    assert!(
        velnor_prep.contains("github.event.inputs.providers), ',velnor,'")
            && velnor_prep.contains("head.repo.fork"),
        "Velnor prep must use the provider admission with the trust conjunct: {velnor_prep}"
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
            >= 24,
        "each selected (unit, provider) gets its own caller: {pr}"
    );
    assert!(pr.contains("  prepare-cargo:\n    name: \"Control / Prepare Cargo\""));
    assert!(pr.contains(
        "  github-hosted-rust-crate00:\n    name: \"Rust · crate00 · github-hosted — rust-crate00\""
    ));
    assert!(pr.contains(
        "  github-self-hosted-rust-crate00:\n    name: \"Rust · crate00 · github-self-hosted — rust-crate00\""
    ));
    assert!(
        pr.contains("  velnor-rust-crate00:\n    name: \"Rust · crate00 · velnor — rust-crate00\"")
    );
    assert!(pr.contains("      unit: rust-crate00"));
    assert!(pr.contains("      provider: github-hosted"));
    assert!(pr.contains("      provider: github-self-hosted"));
    assert!(pr.contains("      provider: velnor"));
    assert!(pr.contains("      provider: control"));
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
    assert_eq!(unit.matches("  verify-github-hosted:").count(), 1);
    assert_eq!(unit.matches("  verify-github-self-hosted:").count(), 1);
    assert_eq!(unit.matches("  verify-velnor:").count(), 1);
    assert!(
        !unit.contains("inputs.unit == '"),
        "the collapsed steps are rendered once, not once per unit"
    );
    assert_eq!(unit.matches("- name: Run unit checks").count(), 3);
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
        25
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
    assert!(unit.contains("provider:\n        required: true"));
    assert!(unit.contains("      unit:\n        required: true"));
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
jobs = ["github-hosted"]
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
    // The hosted-only contract keeps rust-crate00 off the local providers:
    // it gets a hosted caller and no local caller.
    let pr = generated.workflow("ci-pr.yml");
    assert!(pr.contains("  github-hosted-rust-crate00:"));
    assert!(!pr.contains("  github-self-hosted-rust-crate00:"));
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
    assert!(pr.contains("contains(needs.plan.outputs.units, '\\\"unit_id\\\":\\\"rust-crate"));
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
fn trusted_only_unit_gates_every_provider_to_trusted_events() {
    let root = unique_dir("trust-gated-unit");
    write_rust_fixture(&root, 2);
    let plain = generate(&root);
    let project = fs::read_to_string(plain.output.join(".github/ci/project.toml")).unwrap();
    let mut ids = project
        .lines()
        .filter_map(|line| line.strip_prefix("id = \""))
        .map(|line| line.trim_end_matches('"').to_owned());
    let gated = ids.next().unwrap();
    let ungated = ids.next().unwrap();
    let path = root.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&path).unwrap();
    let _ = writeln!(
        config,
        "\n[[units]]\nid = \"{gated}\"\ntrust = \"trusted-only\""
    );
    fs::write(&path, config).unwrap();
    let generated = generate(&root);
    // Trust is gating, not routing: selectors stay exactly as declared.
    let unit = generated.workflow("ci-unit-rust.yml");
    assert!(
        unit.contains("runs-on: [self-hosted, example-runner]"),
        "Velnor jobs must keep the declared selector: {unit}"
    );
    assert!(
        unit.contains("runs-on: ubuntu-24.04"),
        "hosted jobs must keep the declared selector: {unit}"
    );
    // Local providers split trusted-only members into their own callee job;
    // hosted members share one job whose gate strictens to the conjunct.
    assert!(unit.contains("  verify-github-self-hosted-trusted:"));
    assert!(unit.contains("  verify-velnor-trusted:"));
    // Caller gates stay per-unit: only the gated unit's hosted caller
    // carries the fork/bot conjunct.
    let pr = generated.workflow("ci-pr.yml");
    let caller_gate = |job: &str| {
        let lines: Vec<&str> = pr.lines().collect();
        let header = lines
            .iter()
            .position(|line| *line == format!("  {job}:"))
            .unwrap_or_else(|| panic!("caller {job} must render: {pr}"));
        lines[header..]
            .iter()
            .find(|line| line.trim_start().starts_with("if:"))
            .unwrap()
            .to_string()
    };
    assert!(
        caller_gate(&format!("github-hosted-{gated}")).contains("head.repo.fork"),
        "the gated unit's hosted caller must carry the trust conjunct: {pr}"
    );
    assert!(
        !caller_gate(&format!("github-hosted-{ungated}")).contains("head.repo.fork"),
        "the ungated unit's hosted caller must stay ungated: {pr}"
    );
    let project = fs::read_to_string(generated.output.join(".github/ci/project.toml")).unwrap();
    assert!(
        project.contains("trust = \"trusted-only\""),
        "the packaged plan records the typed trust requirement: {project}"
    );
}

#[test]
fn unit_trust_rejects_unknown_tiers() {
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
    let _ = writeln!(config, "\n[[units]]\nid = \"{gated}\"\ntrust = \"trusted\"");
    fs::write(&path, config).unwrap();
    let error = generate_fail(&root, &[]);
    assert!(
        error.contains("declares trust `trusted`; expected one of: untrusted-ok, trusted-only"),
        "unexpected error: {error}"
    );
}
