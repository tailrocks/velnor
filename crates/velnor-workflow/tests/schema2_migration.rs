//! The schema-1 migration fixture must be accepted by the Rust S2 reader.

#![expect(
    clippy::unwrap_used,
    reason = "fixture setup and generator failures should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "fixture setup and generator failures should panic loudly"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

struct TempRepo(PathBuf);

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_repo() -> TempRepo {
    let root = std::env::temp_dir().join(format!(
        "velnor-schema2-migration-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".github-gen")).expect("create fixture config directory");
    fs::create_dir_all(root.join("src")).expect("create minimal Rust project");
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../migrations/generic-workflow-generator/tests/fixtures/schema2-roundtrip.toml"
        )),
    )
    .expect("write migrated schema-2 fixture");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"schema2-migration-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write minimal Cargo manifest");
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"stable\"\n",
    )
    .expect("write explicit Rust toolchain");
    fs::write(
        root.join("mise.toml"),
        "[tasks.build-release]\nrun = \"true\"\n\n[tasks.check-smoke]\nrun = \"true\"\n\n[tasks.check-macos]\nrun = \"true\"\n\n[tasks.check-velnor]\nrun = \"true\"\n",
    )
    .expect("declare fixture tasks");
    fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n")
        .expect("write minimal Rust source");
    TempRepo(root)
}

#[test]
fn converted_multiline_config_is_accepted_by_rust_s2_reader() {
    let repo = temp_repo();
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--dry-run",
            "--default-branch",
            "release/candidate",
        ])
        .arg(Path::new(&repo.0))
        .output()
        .expect("run the Rust schema-2 reader");

    assert!(
        outcome.status.success(),
        "Rust rejected the migrated schema-2 fixture:\n{}\n{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(
        !repo.0.join(".github/workflows/ci-main.yml").exists(),
        "dry-run must parse and render without writing the target repository"
    );
}
