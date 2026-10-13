//! Atomic promotion contract: `velnor-workflow promote` stamps the pin,
//! regenerates the whole tree, and commits pin+metadata+tree in one commit —
//! and only when the rendering generator's source closure equals the pin it
//! stamps (render with X ⇒ stamp X).

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
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "velnor-promote-{name}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
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

/// A promotable consumer tree: the synthetic workspace with an old pin,
/// committed clean.
fn promotable_tree(root: &Path, old_pin: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic-workspace");
    let repo = root.join("consumer");
    copy_tree(&source, &repo);
    fs::write(
        repo.join(".markdownlint-cli2.yaml"),
        "config:\n  default: true\n",
    )
    .unwrap();
    let config = repo.join(".github-gen/velnor-workflow.toml");
    let mut content = fs::read_to_string(&config).unwrap();
    content = content.replace(
        "[generator]\n",
        &format!("[generator]\nrevision = \"{old_pin}\"\n"),
    );
    fs::write(&config, content).unwrap();
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["config", "user.email", "promote@test"]);
    git(&repo, &["config", "user.name", "promote"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "--message", "consumer base"]);
    repo
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf()
}

fn own_revision() -> String {
    let output = binary().arg("--revision").output().unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn own_closure() -> String {
    let output = binary().arg("--closure").output().unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn readiness(root: &Path, closure: &str, revision: &str) -> PathBuf {
    let path = root.join("publication-readiness.json");
    let products = ["Linux-X64", "Linux-ARM64", "macOS-ARM64"]
        .into_iter()
        .map(|platform| {
            serde_json::json!({
                "platform": platform,
                "digest": "d".repeat(64),
                "revoked": false,
                "expires_at": 4_102_444_800_u64,
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "schema": "velnor-workflow.publication-readiness.v1",
            "closure": closure,
            "activation_revision": revision,
            "product_revision": "b".repeat(40),
            "products": products,
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

const OLD_PIN: &str = "0000000000000000000000000000000000000000";

#[test]
fn promote_commits_pin_metadata_and_tree_atomically() {
    let root = temporary_root("atomic");
    let repo = promotable_tree(&root, OLD_PIN);
    let revision = own_revision();
    let closure = own_closure();
    let readiness = readiness(&root, &closure, &revision);

    let outcome = binary()
        .args([
            "promote",
            "--rev",
            &revision,
            "--publication-readiness",
            readiness.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            workspace_root().to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
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
    assert!(
        stdout.contains(&closure[..16]),
        "the report names the proven closure: {stdout}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml")).unwrap();
    assert!(
        pin.contains(&format!("revision = \"{revision}\"")),
        "the pin advances to the rendering generator: {pin}"
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "2",
        "exactly one promotion commit lands"
    );
    assert_eq!(
        git(&repo, &["log", "--format=%s", "-1"]).trim(),
        format!("chore(ci): bump D19 pin to {}", &revision[..8]),
        "the commit follows the pin-bump subject"
    );
    assert!(
        git(&repo, &["log", "--format=%B", "-1"]).contains("Signed-off-by:"),
        "the promotion commit is signed off"
    );
    let stat = git(&repo, &["show", "--stat", "--format=", "HEAD"]);
    for surface in [
        ".github-gen/velnor-workflow.toml",
        ".github-actions-generator-state",
        ".github/workflows/",
    ] {
        assert!(
            stat.contains(surface),
            "the atomic commit carries {surface}: {stat}"
        );
    }
    assert!(
        git(&repo, &["status", "--porcelain"]).is_empty(),
        "the promoted tree is clean"
    );

    // Promoting again is a no-op, not a second commit.
    let outcome = binary()
        .args([
            "promote",
            "--rev",
            &revision,
            "--publication-readiness",
            readiness.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            workspace_root().to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&outcome.stdout);
    assert!(outcome.status.success(), "re-promotion succeeds: {stdout}");
    assert!(
        stdout.contains("already matches the pin render"),
        "re-promotion reports the no-op: {stdout}"
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "2",
        "no second commit lands"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn promote_advances_a_committed_prior_render() {
    let root = temporary_root("update");
    let repo = promotable_tree(&root, OLD_PIN);
    // A committed prior render: every real promotion rewrites existing
    // generated bytes (the Update path), never just creates them.
    let prior = binary()
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--runners",
            "both",
            repo.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        prior.status.success(),
        "prior render succeeds: {}",
        String::from_utf8_lossy(&prior.stderr)
    );
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "--message", "prior render"]);
    assert!(
        !git(&repo, &["ls-files", ".github/workflows"]).is_empty(),
        "the prior render commits generated workflows"
    );
    let revision = own_revision();
    let readiness = readiness(&root, &own_closure(), &revision);

    let outcome = binary()
        .args([
            "promote",
            "--rev",
            &revision,
            "--publication-readiness",
            readiness.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            workspace_root().to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&outcome.stdout);
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        outcome.status.success(),
        "promotion over a prior render succeeds: {stdout}{stderr}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml")).unwrap();
    assert!(
        pin.contains(&format!("revision = \"{revision}\"")),
        "the pin advances to the rendering generator: {pin}"
    );
    let status = git(&repo, &["show", "--name-status", "--format=", "HEAD"]);
    assert!(
        status.lines().any(|line| line.starts_with("M\t")),
        "the promotion commit modifies previously rendered files (Update path): {status}"
    );
    assert!(
        git(&repo, &["status", "--porcelain"]).is_empty(),
        "the promoted tree is clean"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn promote_refuses_a_pin_its_source_cannot_render() {
    let root = temporary_root("binding");
    let repo = promotable_tree(&root, OLD_PIN);
    // A foreign generator history: committed closure inputs whose digest
    // cannot equal this binary's own stamped closure.
    let generator = root.join("generator");
    fs::create_dir_all(generator.join("crates/velnor-workflow/src")).unwrap();
    fs::write(generator.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(generator.join("Cargo.lock"), "# lock\n").unwrap();
    fs::write(
        generator.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn f() {}\n",
    )
    .unwrap();
    git(&generator, &["init", "--quiet", "-b", "main"]);
    git(&generator, &["config", "user.email", "promote@test"]);
    git(&generator, &["config", "user.name", "promote"]);
    git(&generator, &["add", "-A"]);
    git(&generator, &["commit", "--quiet", "--message", "foreign"]);
    let foreign = git(&generator, &["rev-parse", "HEAD"]);
    let readiness = readiness(&root, &"c".repeat(64), &foreign);

    let outcome = binary()
        .args([
            "promote",
            "--rev",
            &foreign,
            "--publication-readiness",
            readiness.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            generator.to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(!outcome.status.success(), "a foreign pin is refused");
    assert!(
        stderr.contains("renders with exactly the generator it stamps"),
        "the refusal names the render≡stamp binding: {stderr}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml")).unwrap();
    assert!(
        pin.contains(&format!("revision = \"{OLD_PIN}\"")),
        "the pin is untouched: {pin}"
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "no commit lands"
    );
    assert!(
        git(&repo, &["status", "--porcelain"]).is_empty(),
        "the tree is untouched"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn promote_refuses_invalid_publication_readiness_before_mutation() {
    let root = temporary_root("invalid-readiness");
    let repo = promotable_tree(&root, OLD_PIN);
    let revision = own_revision();
    let readiness = readiness(&root, &own_closure(), &revision);
    let mut manifest = fs::read_to_string(&readiness).unwrap();
    manifest = manifest.replacen("\"revoked\":false", "\"revoked\":true", 1);
    fs::write(&readiness, manifest).unwrap();

    let outcome = binary()
        .args([
            "promote",
            "--rev",
            &revision,
            "--publication-readiness",
            readiness.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            workspace_root().to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(!outcome.status.success(), "revoked product is refused");
    assert!(
        stderr.contains("publication readiness product") && stderr.contains("revoked"),
        "the refusal names the invalid readiness evidence: {stderr}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml")).unwrap();
    assert!(
        pin.contains(&format!("revision = \"{OLD_PIN}\"")),
        "the pin is untouched: {pin}"
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "no commit lands"
    );
    assert!(
        git(&repo, &["status", "--porcelain"]).is_empty(),
        "the tree is untouched"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn promote_refuses_missing_publication_readiness_before_mutation() {
    let root = temporary_root("missing-readiness");
    let repo = promotable_tree(&root, OLD_PIN);
    let revision = own_revision();
    let missing = root.join("missing-publication-readiness.json");

    let outcome = binary()
        .args([
            "promote",
            "--rev",
            &revision,
            "--publication-readiness",
            missing.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            workspace_root().to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(!outcome.status.success(), "missing readiness is refused");
    assert!(
        stderr.contains("read publication readiness manifest"),
        "the refusal names the missing evidence: {stderr}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml")).unwrap();
    assert!(
        pin.contains(&format!("revision = \"{OLD_PIN}\"")),
        "the pin is untouched: {pin}"
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "no commit lands"
    );
    assert!(
        git(&repo, &["status", "--porcelain"]).is_empty(),
        "the tree is untouched"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn promote_dry_run_verifies_without_writing() {
    let root = temporary_root("dry-run");
    let repo = promotable_tree(&root, OLD_PIN);
    let revision = own_revision();
    let readiness = readiness(&root, &own_closure(), &revision);

    let outcome = binary()
        .args([
            "promote",
            "--dry-run",
            "--rev",
            &revision,
            "--publication-readiness",
            readiness.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            workspace_root().to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&outcome.stdout);
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        outcome.status.success(),
        "dry run succeeds: {stdout}{stderr}"
    );
    assert!(
        stdout.contains("dry run"),
        "the report names the dry run: {stdout}"
    );
    assert!(
        stdout.contains(".github-gen/velnor-workflow.toml"),
        "the report lists the pin it would stamp: {stdout}"
    );
    let pin = fs::read_to_string(repo.join(".github-gen/velnor-workflow.toml")).unwrap();
    assert!(
        pin.contains(&format!("revision = \"{OLD_PIN}\"")),
        "the pin is unwritten: {pin}"
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "1",
        "no commit lands"
    );
    assert!(
        git(&repo, &["status", "--porcelain"]).is_empty(),
        "the tree is restored byte-clean"
    );
    assert!(
        !repo.join(".github").exists(),
        "rendered directories are pruned"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
#[cfg(unix)]
fn promote_refuses_a_symlinked_generation_config() {
    let root = temporary_root("symlinked-config");
    let repo = promotable_tree(&root, OLD_PIN);
    // The config moves outside the tree and a committed link takes its
    // place: the tree is clean, but rollback could not restore a stamp
    // written through the link.
    let outside = root.join("outside.toml");
    fs::rename(repo.join(".github-gen/velnor-workflow.toml"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join(".github-gen/velnor-workflow.toml")).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "--message", "symlink config"]);
    let revision = own_revision();
    let readiness = readiness(&root, &own_closure(), &revision);

    let outcome = binary()
        .args([
            "promote",
            "--rev",
            &revision,
            "--publication-readiness",
            readiness.to_str().unwrap(),
            "--repo",
            repo.to_str().unwrap(),
            "--generator-repo",
            workspace_root().to_str().unwrap(),
            "--default-branch",
            "main",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        !outcome.status.success(),
        "promotion through a symlinked config must fail"
    );
    assert!(
        stderr.contains("symlinked generation config"),
        "the refusal names the link: {stderr}"
    );
    let pin = fs::read_to_string(&outside).unwrap();
    assert!(
        pin.contains(&format!("revision = \"{OLD_PIN}\"")),
        "the external target is unstamped: {pin}"
    );
    assert!(
        fs::symlink_metadata(repo.join(".github-gen/velnor-workflow.toml"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link itself is untouched"
    );
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "2",
        "no commit lands"
    );
    assert!(
        git(&repo, &["status", "--porcelain"]).is_empty(),
        "the tree is untouched"
    );
    let _ = fs::remove_dir_all(&root);
}
