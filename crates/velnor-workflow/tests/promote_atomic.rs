//! End-to-end D19 promotion contract through the captured schema-2 dispatcher.

#![expect(
    clippy::unwrap_used,
    reason = "promotion fixture setup failures should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "promotion fixture setup failures should panic loudly"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "velnor-promote-{name}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create promotion temp root");
    root
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("git present");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create fixture directory");
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("read fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("inspect fixture entry").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture file");
        }
    }
}

fn promotable_tree(root: &Path, old_pin: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures-s2/promote-trust");
    let repo = root.join("consumer");
    copy_tree(&source, &repo);
    let config = repo.join(".github-gen/velnor-workflow.toml");
    let content = fs::read_to_string(&config).expect("read fixture config");
    fs::write(
        &config,
        content.replace(
            "[generator]\n",
            &format!("[generator]\nrevision = \"{old_pin}\"\n"),
        ),
    )
    .expect("add old D19 pin");
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["config", "user.email", "promote@test"]);
    git(&repo, &["config", "user.name", "promote"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "--message", "consumer base"]);
    repo
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn own_revision() -> String {
    let output = binary()
        .arg("--revision")
        .output()
        .expect("run revision query");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

const OLD_PIN: &str = "0000000000000000000000000000000000000000";

fn promote(repo: &Path, revision: &str, extra: &[&str]) -> Output {
    binary()
        .args(["promote", "--rev", revision, "--repo"])
        .arg(repo)
        .args(["--generator-repo"])
        .arg(workspace_root())
        .args(["--default-branch", "main"])
        .args(extra)
        .output()
        .expect("run promote")
}

#[test]
fn promote_commits_pin_trust_render_and_ownership_state_together() {
    let root = temporary_root("atomic");
    let repo = promotable_tree(&root, OLD_PIN);
    let revision = own_revision();

    let outcome = promote(&repo, "HEAD", &[]);
    let stdout = String::from_utf8_lossy(&outcome.stdout);
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        outcome.status.success(),
        "promotion succeeds: {stdout}{stderr}"
    );
    assert!(
        stdout.contains(&format!("promote: {} -> {}", &OLD_PIN[..8], &revision[..8])),
        "the report names the pin transition: {stdout}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml"))
        .expect("read promoted pin");
    assert!(pin.contains(&format!("revision = \"{revision}\"")), "{pin}");
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]), "2");
    assert_eq!(
        git(&repo, &["log", "--format=%s", "-1"]),
        format!("chore(ci): bump D19 pin to {}", &revision[..8])
    );
    assert!(
        git(&repo, &["log", "--format=%B", "-1"]).contains("Signed-off-by:"),
        "promotion commit is DCO signed off"
    );
    let stat = git(&repo, &["show", "--stat", "--format=", "HEAD"]);
    for surface in [
        ".github-gen/velnor-workflow.toml",
        ".github/ci/.github-actions-generator-state",
        ".github/workflows/",
    ] {
        assert!(
            stat.contains(surface),
            "atomic commit omits {surface}: {stat}"
        );
    }
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());

    let second = promote(&repo, &revision, &[]);
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        second.status.success(),
        "re-promotion succeeds: {second_stdout}"
    );
    assert!(second_stdout.contains("already matches the pin render"));
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]), "2");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn dry_run_restores_the_pin_without_creating_rendered_files() {
    let root = temporary_root("dry-run");
    let repo = promotable_tree(&root, OLD_PIN);
    let revision = own_revision();

    let outcome = promote(&repo, &revision, &["--dry-run"]);
    let stdout = String::from_utf8_lossy(&outcome.stdout);
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        outcome.status.success(),
        "dry run succeeds: {stdout}{stderr}"
    );
    assert!(stdout.contains("no commit: dry run"), "{stdout}");
    assert!(
        stdout.contains(".github-gen/velnor-workflow.toml"),
        "{stdout}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml"))
        .expect("read restored pin");
    assert!(pin.contains(&format!("revision = \"{OLD_PIN}\"")), "{pin}");
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]), "1");
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());
    assert!(!repo.join(".github").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn rejected_commit_restores_written_tree_and_unstages_promotion_paths() {
    let root = temporary_root("commit-failure");
    let repo = promotable_tree(&root, OLD_PIN);
    let hook = repo.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 19\n").expect("write rejecting hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("make hook executable");
    let revision = own_revision();

    let outcome = promote(&repo, &revision, &[]);
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        !outcome.status.success(),
        "commit rejection fails: {stderr}"
    );
    assert!(stderr.contains("git commit -s -m"), "{stderr}");
    assert!(stderr.contains("failed"), "{stderr}");
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml"))
        .expect("read restored pin");
    assert!(pin.contains(&format!("revision = \"{OLD_PIN}\"")), "{pin}");
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]), "1");
    assert!(git(&repo, &["status", "--porcelain"]).is_empty());
    assert!(!repo
        .join(".github/ci/.github-actions-generator-state")
        .exists());
    let _ = fs::remove_dir_all(root);
}
