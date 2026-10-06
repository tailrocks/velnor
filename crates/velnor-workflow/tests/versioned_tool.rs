//! The main-branch-driven versioned-tool publisher, end to end.
//!
//! The fixture declares one `versioned-tool` release row over neutral
//! `example/*` names. It pins that the row renders its own workflow file
//! with its own name, triggers, concurrency, and five-job version graph —
//! and that identity collisions and incomplete contracts fail closed with
//! errors naming the row.

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
        "velnor-versioned-tool-test-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&base).unwrap();
    base
}

/// The synthetic release fixture: one Rust crate. The versioned-tool lane
/// exists only when a config declares it, so the fixture pins that a
/// declared lane is added to — never guessed from — the scanned surface.
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

fn fixture_config() -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/versioned-tool/velnor-workflow.toml"),
    )
    .unwrap()
}

/// The fixture's declare rows without the document preamble, so a second
/// row appends to the same document instead of repeating its keys.
fn declare_rows_only(config: &str) -> &str {
    let start = config.find("[[declare]]").unwrap();
    &config[start..]
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
            "both",
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
            "--runners",
            "both",
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
fn versioned_tool_renders_end_to_end() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, &fixture_config());
    let generated = generate(&root);
    let files = generated.workflow_files();
    assert!(
        files.contains(&"example-tool.yml".to_owned()),
        "the declared lane must add its file: {files:?}"
    );
    assert!(
        !files.contains(&"release.yml".to_owned()),
        "no tag publisher is declared, so none renders: {files:?}"
    );
    let workflow = generated.workflow("example-tool.yml");
    assert!(
        workflow.contains("name: example-tool\n")
            && workflow.contains("  push:\n    branches: [main]\n")
            && workflow.contains("  pull_request:\n")
            && workflow.contains("      lanes:\n")
            && workflow.contains("cancel-in-progress: ${{ github.event_name == 'pull_request' }}"),
        "the row must render its own name, triggers, and concurrency: {workflow}"
    );
    for job in [
        "  validate-version:\n",
        "  version:\n",
        "  assert-version:\n",
        "  build:\n",
        "  publish:\n",
    ] {
        assert!(
            workflow.contains(job),
            "the five-job graph must render {job:?}: {workflow}"
        );
    }
    assert!(
        workflow.contains("run: mise run check-example-tool-version")
            && workflow.contains("run: mise run assert-example-tool-published")
            && workflow.contains("run: mise run build-example-tool"),
        "gate, assert, and build tasks must wire as named tasks: {workflow}"
    );
    assert!(
        workflow.contains("group: example-tap-publish")
            && workflow.contains("gh release create \"$tag\" dist/*")
            && !workflow.contains("verify-tag")
            && !workflow.contains("--clobber"),
        "the publish must mint the immutable release under its mutex: {workflow}"
    );
}

#[test]
fn second_versioned_tool_row_renders_its_own_publisher() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    let other = declare_rows_only(&fixture_config()).replace("example-tool", "other-tool");
    let rows = format!("{}\n{other}", fixture_config());
    write_config(&root, &rows);
    let generated = generate(&root);
    let first = generated.workflow("example-tool.yml");
    let second = generated.workflow("other-tool.yml");
    assert!(
        first.contains("name: example-tool\n")
            && second.contains("name: other-tool\n")
            && second.contains("tag=\"other-tool-v$VERSION\""),
        "each row must render its own identity:\n{first}\n{second}"
    );
}

#[test]
fn duplicate_versioned_tool_name_fails_closed() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    let other = declare_rows_only(&fixture_config())
        .replace("example-tool.yml", "other-tool.yml")
        .replace(
            "version_prefix = \"example-tool-v\"",
            "version_prefix = \"other-tool-v\"",
        )
        .replace(
            "kind = \"versioned-tool\"",
            "kind = \"versioned-tool\"\nname = \"example-tool\"",
        );
    write_config(&root, &format!("{}\n{other}", fixture_config()));
    let stderr = generate_failure(&root, &workspace.join("out"));
    assert!(
        stderr.contains("renders duplicate workflow name `example-tool`"),
        "the duplicate name must fail the run: {stderr}"
    );
}

#[test]
fn duplicate_versioned_tool_prefix_fails_closed() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    let other = declare_rows_only(&fixture_config()).replace("example-tool.yml", "other-tool.yml");
    write_config(&root, &format!("{}\n{other}", fixture_config()));
    let stderr = generate_failure(&root, &workspace.join("out"));
    assert!(
        stderr.contains("publishes tag prefix `example-tool-v`")
            && stderr.contains("already owned by file `example-tool.yml`"),
        "the duplicate prefix must fail the run: {stderr}"
    );
}

#[test]
fn incomplete_versioned_tool_contract_fails_closed() {
    let incomplete = fixture_config()
        .lines()
        .filter(|line| !line.starts_with("package = "))
        .collect::<Vec<_>>()
        .join("\n");
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, &incomplete);
    let stderr = generate_failure(&root, &workspace.join("out"));
    assert!(
        stderr.contains("declares an incomplete versioned-tool contract")
            && stderr.contains("example-tool.yml")
            && stderr.contains("`package`"),
        "the missing field must name the row: {stderr}"
    );
}

#[test]
fn declared_release_row_suppresses_the_default_row() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    // The `[release]` contract validates its targets against the pinned
    // toolchain, so the test copy pins the targets the contract builds for.
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\ntargets = [\"x86_64-unknown-linux-gnu\", \"aarch64-unknown-linux-gnu\"]\n",
    )
    .unwrap();
    let config = format!(
        "{}\n[release]\nenabled = true\nkind = \"rust-binary\"\npackage = \"app\"\nbinary = \"app\"\ntargets = [\"x86_64-unknown-linux-gnu\", \"aarch64-unknown-linux-gnu\"]\n\n[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n",
        fixture_config()
    );
    write_config(&root, &config);
    let generated = generate(&root);
    let files = generated.workflow_files();
    assert!(
        files.contains(&"release.yml".to_owned()) && files.contains(&"example-tool.yml".to_owned()),
        "both publishers must render: {files:?}"
    );
    let release = generated.workflow("release.yml");
    assert!(
        release.contains("name: Release\n"),
        "the canonical publisher keeps its bytes: {release}"
    );
}

/// Only the writer lane attests and uploads: the matrix fans out per lane
/// but the tarball name is per target, so an ungated upload would publish
/// the same name twice in `both` mode.
#[test]
fn versioned_tool_build_uploads_only_from_the_writer_lane() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, &fixture_config());
    let generated = generate(&root);
    let workflow = generated.workflow("example-tool.yml");
    assert_eq!(
        workflow.matches("if: ${{ matrix.config.writer }}").count(),
        2,
        "attest and upload must gate on the writer lane: {workflow}"
    );
    assert!(
        workflow.contains("- name: Attest tool artifact\n        if: ${{ matrix.config.writer }}")
            && workflow
                .contains("- name: Upload tool artifact\n        if: ${{ matrix.config.writer }}"),
        "the writer gates must sit on the attest and upload steps: {workflow}"
    );
    assert_eq!(
        workflow.matches("\"writer\":false").count(),
        1,
        "only the both-mode github arm must be a non-writer: {workflow}"
    );
}
