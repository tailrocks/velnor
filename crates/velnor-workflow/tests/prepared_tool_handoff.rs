//! The prepared-tool handoff through the real generator binary: a declared
//! row binds governing lockfiles and renders consumer steps into the kind
//! reusable with caller inputs, while an undeclared repository generates no
//! prepared-tool bytes at all.

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

fn tempfile() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "velnor-prepared-tool-test-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&base).unwrap();
    base
}

fn copy_fixture(destination: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic-workspace");
    copy_tree(&source, destination);
    destination.to_path_buf()
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

fn write_config(root: &Path, config: &str) {
    let directory = root.join(".github-gen");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("velnor-workflow.toml"), config).unwrap();
}

fn generate(root: &Path) -> PathBuf {
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
            "--providers",
            "github-hosted,velnor",
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
    output
}

/// Every generated file's content under `.github`, concatenated.
fn generated_text(output: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![output.join(".github")];
    while let Some(current) = stack.pop() {
        let entries: Vec<_> = fs::read_dir(&current).unwrap().collect();
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                text.push_str(&fs::read_to_string(&path).unwrap());
                text.push('\n');
            }
        }
    }
    text
}

const PREPARED_TOOL_CONFIG: &str = "schema = 2\n\n[generator]\nrepository = \"example/synthetic\"\n\n[workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"example-lane\"]\n\n[[declare]]\nprimitive = \"prepared-tool\"\nunits = [\"rust-alpha\"]\n\n[declare.args.tools]\ntest-runner = [\"producer-job\"]\n\n[declare.args.recipes]\ntest-runner = [\"cargo build --locked\"]\n";

#[test]
fn declared_prepared_tool_renders_consumer_steps() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    write_config(&root, PREPARED_TOOL_CONFIG);
    let output = generate(&root);

    let kind = fs::read_to_string(output.join(".github/workflows/ci-unit-rust.yml")).unwrap();
    // The callee declares the input and renders the consumer block for the
    // workspace-bound need.
    assert!(
        kind.contains("prepared_tools:"),
        "the kind header declares the prepared-tools input"
    );
    assert!(
        kind.contains("Restore prepared tool test-runner"),
        "the kind reusable restores the declared tool"
    );
    assert!(
        kind.contains("prepared-tool-v1-test-runner-"),
        "the restore key names the prepared-tool namespace"
    );
    assert!(
        kind.contains("velnor-workflow prepared-tool-install"),
        "the install runs through the runtime verb"
    );
    // Only rust-alpha carries the need, so the block is gated and the
    // alpha caller passes its record.
    assert!(
        kind.contains("contains(format(',{0},', inputs.prepared_tools)"),
        "a partial member need renders behind a membership gate"
    );
    let callers = generated_text(&output);
    assert!(
        callers.contains("prepared_tools: \"test-runner:"),
        "a caller passes its need record"
    );
}

#[test]
fn undeclared_repository_generates_no_prepared_tool_bytes() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let output = generate(&root);

    let text = generated_text(&output);
    assert!(
        !text.contains("prepared-tool") && !text.contains("prepared_tool"),
        "an undeclared surface mentions no prepared tools"
    );
}
