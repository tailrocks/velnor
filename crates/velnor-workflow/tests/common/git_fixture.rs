//! Git setup shared by generator integration-test fixtures.

#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]

use std::path::Path;
use std::process::Command;

/// Initialize a temporary fixture as a self-contained repository with a
/// committed `main` HEAD.
pub fn initialize_git_fixture(root: &Path) {
    run_git(root, &["init", "--initial-branch=main"]);
    run_git(root, &["config", "user.name", "velnor-workflow tests"]);
    run_git(
        root,
        &[
            "config",
            "user.email",
            "velnor-workflow-tests@example.invalid",
        ],
    );
    commit_fixture(root);
}

/// Commit all fixture inputs before the generator observes them.
pub fn commit_fixture(root: &Path) {
    run_git(root, &["add", "--all"]);
    let status = Command::new("git")
        .current_dir(root)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .expect("inspect fixture index");
    if status.success() {
        return;
    }
    run_git(root, &["commit", "--message", "test fixture input"]);
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git command");
    assert!(
        output.status.success(),
        "git {} failed:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
