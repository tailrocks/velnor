//! The tasks release publisher, end to end.
//!
//! The fixture declares a `kind = "tasks"` release over neutral `example/*`
//! names with two named-task jobs. It pins that the jobs render into
//! `release.yml` with tag-only publisher triggers and a separate read-only
//! selected-ref validator, sibling ordering, event lanes, and runner selectors —
//! and that unknown tasks, jobs on other publishers, and
//! artifact bindings fail closed with errors naming the row.

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

fn tempfile() -> PathBuf {
    // Test threads run concurrently and share a clock; a timestamp alone can
    // hand two tests the same directory and let one test's cleanup delete the
    // other's output mid-write. A process-local sequence makes the name
    // collision-free.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "velnor-release-tasks-test-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&base).unwrap();
    base
}

fn copy_release_fixture(destination: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic-release");
    copy_tree(&source, destination);
    destination.to_path_buf()
}

fn write_config(root: &Path, config: &str) {
    let directory = root.join(".github-gen");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("velnor-workflow.toml"), config).unwrap();
}

fn write_mise_tasks(root: &Path) {
    fs::write(
        root.join("mise.toml"),
        "[tasks.build-release]\nrun = \"echo build\"\n\n[tasks.verify-release]\nrun = \"echo verify\"\n\n[tasks.sign-release]\nrun = \"echo sign\"\n\n[tasks.review-release]\nrun = \"echo review\"\n",
    )
    .unwrap();
}

fn fixture_config() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/release-tasks/velnor-workflow.toml"),
    )
    .unwrap()
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

fn generate_failure(root: &Path, out: &Path) -> String {
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            out.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        !outcome.status.success(),
        "generation must fail closed for {}, and did not",
        root.display()
    );
    String::from_utf8_lossy(&outcome.stderr).into_owned()
}

#[test]
fn tasks_release_renders_end_to_end() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, &fixture_config());
    write_mise_tasks(&root);
    let generated = generate(&root);
    let files = generated.workflow_files();
    assert!(
        files.contains(&"release.yml".to_owned()),
        "the tasks publisher must render release.yml: {files:?}"
    );
    assert!(
        files.contains(&"release-validate.yml".to_owned()),
        "explicit validate jobs must render a separate read-only workflow: {files:?}"
    );
    let workflow = generated.workflow("release.yml");
    for expected in [
        "name: Release",
        "tags: [\"v[0-9]*\"]",
        "External trust requirement: protect release tags",
        "publisher task code comes from the tag",
        "group: release-${{ github.ref }}",
        "  \"build\":\n",
        "  \"sign\":\n",
        "needs: [\"build\"]",
        "run: mise run build-release",
        "run: mise run verify-release",
        "run: mise run sign-release",
        "runs-on: macos-15",
        "timeout-minutes: 45",
        "environment: example-signing",
        "id-token: write",
        "DEVELOPER_DIR:",
        "SIGNING_KEY_ID:",
        "Attest release artifacts",
        "actions/attest-build-provenance@",
        "dist/example-app.zip",
    ] {
        assert!(
            workflow.contains(expected),
            "release.yml must render {expected:?}: {workflow}"
        );
    }
    assert!(!workflow.contains("workflow_dispatch"), "{workflow}");
    assert!(!workflow.contains("manual-review"), "{workflow}");
    let sign = workflow.split("  \"sign\":\n").nth(1).unwrap_or_default();
    assert!(
        sign.contains("permissions:\n      attestations: write\n      contents: read\n      id-token: write"),
        "attestation jobs must retain checkout read access: {sign}"
    );
    let validator = generated.workflow("release-validate.yml");
    for expected in [
        "workflow_dispatch:",
        "permissions:\n  contents: read",
        "Selected-ref task code is untrusted",
        "  \"build\":\n",
        "  \"manual-review\":\n",
        "run: mise run review-release",
        "persist-credentials: false",
    ] {
        assert!(
            validator.contains(expected),
            "release-validate.yml must render {expected:?}: {validator}"
        );
    }
    for forbidden in [
        "tags:",
        "  \"sign\":",
        "permissions:\n      ",
        "contents: write",
        "attestations:",
        "id-token:",
        "environment:",
        "secrets.",
        "RELEASE_TOKEN",
        "SIGNING_KEY_ID",
        "velnor",
    ] {
        assert!(
            !validator.contains(forbidden),
            "selected-ref validator contains {forbidden:?}: {validator}"
        );
    }
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn tasks_release_regenerates_identical_bytes() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, &fixture_config());
    write_mise_tasks(&root);
    let first_run = generate(&root);
    let first = first_run.workflow("release.yml");
    let first_validation = first_run.workflow("release-validate.yml");
    let output = root.parent().unwrap().join("fixture-out-second");
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        outcome.status.success(),
        "regeneration failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    let second = fs::read_to_string(output.join(".github/workflows").join("release.yml")).unwrap();
    assert_eq!(first, second, "repeat generation must be byte-identical");
    let second_validation = fs::read_to_string(
        output
            .join(".github/workflows")
            .join("release-validate.yml"),
    )
    .unwrap();
    assert_eq!(
        first_validation, second_validation,
        "repeat validation generation must be byte-identical"
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn tasks_release_rejects_undeclared_mise_tasks() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, &fixture_config());
    fs::write(
        root.join("mise.toml"),
        "[tasks.build-release]\nrun = \"echo build\"\n",
    )
    .unwrap();
    let error = generate_failure(&root, &workspace.join("out"));
    assert!(
        error.contains("mise task `verify-release`, which mise.toml does not declare"),
        "the error must name the undeclared task: {error}"
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn tasks_release_rejects_declare_row_kind() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(
        &root,
        "schema = 2\n\n[generator]\nrepository = \"example/synthetic-release\"\n\n[workflow]\nproviders = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\ndefault_dispatch_providers = [\"github-hosted\"]\ndefault_branch = \"main\"\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n[declare.args]\nkind = \"tasks\"\n",
    );
    write_mise_tasks(&root);
    let error = generate_failure(&root, &workspace.join("out"));
    assert!(
        error.contains("declare rows carry no job tables"),
        "the error must point tasks at the config table: {error}"
    );
    let _ = fs::remove_dir_all(workspace);
}

#[test]
fn tasks_release_rejects_jobs_on_other_publishers() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(
        &root,
        "schema = 2\n\n[generator]\nrepository = \"example/synthetic-release\"\n\n[workflow]\nproviders = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\ndefault_dispatch_providers = [\"github-hosted\"]\ndefault_branch = \"main\"\n\n[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n[release]\nenabled = true\nkind = \"pages\"\nartifact_path = \"dist\"\n\n[[release.job]]\nid = \"build\"\ntasks = [\"build-release\"]\n",
    );
    write_mise_tasks(&root);
    let error = generate_failure(&root, &workspace.join("out"));
    assert!(
        error.contains("only for kind `tasks`"),
        "the error must name the kind restriction: {error}"
    );
    let _ = fs::remove_dir_all(workspace);
}
