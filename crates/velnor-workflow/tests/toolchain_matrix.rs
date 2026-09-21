//! The Rust toolchain matrix: a kind whose units span more than one channel
//! renders one provision leg per channel, with per-unit `toolchain` inputs
//! selecting the leg. The MSRV leg provisions its channel explicitly and
//! verifies through plain `cargo` invocations — never `cargo +toolchain` —
//! while single-channel trees render exactly as before, with no matrix
//! machinery at all.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]

/// Shared harness: this binary reaches only the schema-2 helpers, so the
/// schema-1 and umask helpers it never calls read as dead within this
/// binary. They are live in the bins that drive those paths.
#[allow(dead_code)]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{check_ok, generate_ok, minimal_root, snapshot_tree, write_schema2_config};

fn output_for(root: &Path) -> PathBuf {
    root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ))
}

fn append_unit_row(root: &Path, row: &str) {
    let path = root.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&path).unwrap();
    config.push_str(row);
    fs::write(&path, config).unwrap();
}

fn read_tree_files(output: &Path) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    let mut stack = vec![output.to_owned()];
    while let Some(directory) = stack.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            } else if let Ok(text) = fs::read_to_string(&path) {
                files.push((path, text));
            }
        }
    }
    files
}

#[test]
fn msrv_leg_provisions_explicitly_with_plain_commands() {
    let root = minimal_root("toolchain-matrix");
    write_schema2_config(&root);
    // The MSRV leg: a declared Rust unit on an older channel, verifying
    // through the typed workspace check. The leg carries no test phase, so
    // compiler-sensitive UI tests never run there; it opts out of the
    // object transport so the old channel runs plain `cargo check`.
    append_unit_row(
        &root,
        "\n[[units]]\nid = \"rust-msrv\"\nkind = \"rust\"\ntoolchain = \"1.88.0\"\nworkspace_check = true\nmbx = false\n",
    );
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    generate_ok(&root, &output, false);

    let kind = fs::read_to_string(output.join(".github/workflows/ci-unit-rust.yml")).unwrap();
    assert!(
        kind.contains("      toolchain:\n        required: false\n        type: string"),
        "the kind reusable declares the toolchain input: {kind}"
    );
    assert!(
        kind.contains("if: ${{ inputs.toolchain == '1.91.1' }}"),
        "the pin leg gates on the pin channel: {kind}"
    );
    assert!(
        kind.contains("          rustup toolchain install\n"),
        "the pin leg keeps the bare file-driven install: {kind}"
    );
    assert!(
        kind.contains("if: ${{ inputs.toolchain == '1.88.0' }}"),
        "the MSRV leg gates on its declared channel: {kind}"
    );
    assert!(
        kind.contains("rustup toolchain install '1.88.0' --profile 'minimal'"),
        "the MSRV leg installs its channel explicitly: {kind}"
    );
    assert!(
        kind.contains("key: velnor-rustup-${{ runner.os }}-${{ runner.arch }}-1.88.0"),
        "the MSRV leg keys its cache on the channel: {kind}"
    );
    assert!(
        kind.contains("hashFiles('rust-toolchain.toml', 'rust-toolchain')"),
        "the pin leg keeps hashing the pin files: {kind}"
    );
    assert!(
        kind.contains("echo \"RUSTUP_TOOLCHAIN=1.88.0\" >> \"$GITHUB_ENV\""),
        "the MSRV leg retargets later cargo invocations: {kind}"
    );

    let callers = fs::read_to_string(output.join(".github/workflows/ci-pr.yml")).unwrap();
    assert!(
        callers.contains("toolchain: \"1.88.0\""),
        "the MSRV caller passes its channel: {callers}"
    );
    assert!(
        callers.contains("toolchain: \"1.91.1\""),
        "the primary caller passes the pin channel: {callers}"
    );

    let project = fs::read_to_string(output.join(".github/ci/project.toml")).unwrap();
    assert!(
        project.contains("cargo check --workspace --all-targets --locked"),
        "the MSRV unit verifies through the plain workspace check: {project}"
    );

    for (path, text) in read_tree_files(&output) {
        assert!(
            !text.contains("+1.88.0"),
            "no leg renders `cargo +toolchain` commands: {}",
            path.display()
        );
    }

    // Determinism: regeneration over the matrix tree is a byte-identical
    // no-op, and `--check` agrees.
    let before = snapshot_tree(&output);
    generate_ok(&root, &output, true);
    assert_eq!(
        snapshot_tree(&output),
        before,
        "matrix regeneration must be a no-op"
    );
    check_ok(&root, &output);
}

#[test]
fn single_channel_tree_stays_matrix_free() {
    let root = minimal_root("toolchain-single");
    write_schema2_config(&root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    generate_ok(&root, &output, false);

    for (path, text) in read_tree_files(&output) {
        assert!(
            !text.contains("inputs.toolchain"),
            "a single-channel tree provisions without leg inputs: {}",
            path.display()
        );
    }
    let kind = fs::read_to_string(output.join(".github/workflows/ci-unit-rust.yml")).unwrap();
    assert!(
        !kind.contains("toolchain:\n        required: false"),
        "a single-channel kind declares no toolchain input: {kind}"
    );
}
