//! S2 pin-verification subprocess gates. Ordinary `--check` ignores D19 pins;
//! `--verify-pinned` requires a trusted renderer or an explicit local build,
//! which offline Cargo policy still blocks.

#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "fixture setup failures should panic")]
#![expect(clippy::expect_used, reason = "fixture setup failures should panic")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn unique_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "velnor-s2-pin-build-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_fixture(root: &Path) {
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join(".cargo")).unwrap();
    fs::write(root.join(".cargo/config.toml"), "[build]\n").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"crates/velnor-workflow\"]\n",
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
    let crate_dir = root.join("crates/velnor-workflow");
    fs::create_dir_all(crate_dir.join("src")).unwrap();
    fs::write(
        crate_dir.join("Cargo.toml"),
        "[package]\nname = \"velnor-workflow\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(crate_dir.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n\n[generator]\nrepository = \"tailrocks/velnor\"\n\n\
         [workflow]\nproviders = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\n\
         default_dispatch_providers = [\"github-hosted\"]\ndefault_branch = \"main\"\n\
         profile = \"rust-workspace-release\"\nrust_needs = \"dependency-closure\"\n\n\
         [workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n",
    )
    .unwrap();
}

fn git(root: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .status()
        .expect("git present");
    assert!(status.success(), "git {arguments:?} failed");
}

fn git_output(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("git present");
    assert!(output.status.success(), "git {arguments:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn generate(root: &Path) -> PathBuf {
    let output = root.parent().unwrap().join(format!(
        "{}-rendered",
        root.file_name().unwrap().to_string_lossy()
    ));
    let _ = fs::remove_dir_all(&output);
    let result = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--providers",
            "github-hosted",
            "--default-branch",
            "main",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .env_remove("VELNOR_WORKFLOW_PINNED_BINARY")
        .output()
        .expect("generate fixture");
    assert!(
        result.status.success(),
        "fixture generation failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    output
}

fn stale_pin_fixture(name: &str) -> (PathBuf, String, PathBuf) {
    let root = unique_dir(name);
    write_fixture(&root);
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "pin@test"]);
    git(&root, &["config", "user.name", "pin"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "fixture"]);
    let pin = git_output(&root, &["rev-parse", "HEAD"]);
    let config = root.join(".github-gen/velnor-workflow.toml");
    let body = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        body.replace(
            "repository = \"tailrocks/velnor\"\n",
            &format!("repository = \"tailrocks/velnor\"\nrevision = \"{pin}\"\n"),
        ),
    )
    .unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "declare generator pin"]);
    let output = generate(&root);
    (root, pin, output)
}

fn write_cargo_shim(directory: &Path) -> PathBuf {
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

fn workflow_command(root: &Path, output: &Path, shim_dir: &Path, sentinel: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
    command
        .args([
            "--plain",
            "--providers",
            "github-hosted",
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
        .env_remove("VELNOR_WORKFLOW_PINNED_CLOSURE")
        .env_remove("CARGO_NET_OFFLINE");
    command
}

#[test]
fn check_does_not_require_or_build_the_declared_pin() {
    let (root, _pin, output) = stale_pin_fixture("ordinary-check");
    let shim_dir = unique_dir("ordinary-check-shim");
    let sentinel = write_cargo_shim(&shim_dir);

    let mut check = workflow_command(&root, &output, &shim_dir, &sentinel);
    check.arg("--check");
    let outcome = check.output().expect("ordinary check");
    assert!(
        outcome.status.success(),
        "ordinary --check renders without a provisioned pin:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(!sentinel.exists(), "ordinary --check never builds the pin");
}

#[test]
fn verify_pinned_requires_pin_build_and_only_pin_build_invokes_cargo() {
    let (root, pin, output) = stale_pin_fixture("fail-closed");
    let shim_dir = unique_dir("fail-closed-shim");
    let sentinel = write_cargo_shim(&shim_dir);

    let mut verify = workflow_command(&root, &output, &shim_dir, &sentinel);
    verify.arg("--verify-pinned");
    let closed = verify.output().expect("verify pinned");
    assert!(!closed.status.success(), "an unprovisioned pin fails");
    let stderr = String::from_utf8_lossy(&closed.stderr);
    assert!(stderr.contains(&pin), "failure names the pin: {stderr}");
    assert!(
        stderr.contains("--pin-build with --verify-pinned"),
        "failure names the explicit build command: {stderr}"
    );
    assert!(
        stderr.contains("building is forbidden here"),
        "failure is the fail-closed guard: {stderr}"
    );
    assert!(!sentinel.exists(), "fail-closed guard invokes no Cargo");

    let _ = fs::remove_file(&sentinel);
    let mut build = workflow_command(&root, &output, &shim_dir, &sentinel);
    build.args(["--verify-pinned", "--pin-build"]);
    let opened = build.output().expect("verify pinned with pin-build");
    assert!(!opened.status.success(), "the shimmed build fails");
    assert!(sentinel.exists(), "explicit pin-build invokes Cargo");
    let stderr = String::from_utf8_lossy(&opened.stderr);
    assert!(
        stderr.contains(&format!("build velnor-workflow at pin {pin}")),
        "run reached the pinned build: {stderr}"
    );
}

#[test]
fn cargo_net_offline_overrides_pin_build() {
    let (root, pin, output) = stale_pin_fixture("offline");
    let shim_dir = unique_dir("offline-shim");
    let sentinel = write_cargo_shim(&shim_dir);
    let mut verify = workflow_command(&root, &output, &shim_dir, &sentinel);
    verify
        .args(["--verify-pinned", "--pin-build"])
        .env("CARGO_NET_OFFLINE", "true");
    let outcome = verify.output().expect("offline pinned verification");
    assert!(!outcome.status.success(), "offline pin build is refused");
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(stderr.contains(&pin), "failure names the pin: {stderr}");
    assert!(
        stderr.contains("building is forbidden here"),
        "offline guard fails closed: {stderr}"
    );
    assert!(!sentinel.exists(), "offline guard invokes no Cargo");
}
