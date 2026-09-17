//! The docker-multiarch end-to-end contract: a repository that owns only a
//! Dockerfile plus a `[release] kind = "docker"` contract generates a
//! multi-arch publisher — admission, one native builder per platform, and a
//! single manifest assembly over the complete verified digest set. All
//! coordinates are fixture-local; nothing about the image is generated.

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
        "velnor-docker-multiarch-test-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&base).unwrap();
    base
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
            "github-hosted",
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

fn release_workflow(output: &Path) -> String {
    fs::read_to_string(output.join(".github/workflows/release.yml")).unwrap()
}

#[test]
fn docker_release_generates_the_multi_arch_publisher() {
    let workspace = tempfile();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docker-multiarch");
    let root = workspace.join("fixture");
    copy_tree(&source, &root);
    let output = generate(&root);
    let workflow = release_workflow(&output);

    // One native builder per declared platform, arm64 on the hosted label.
    assert!(workflow.contains("platform: linux/amd64"), "{workflow}");
    assert!(workflow.contains("platform: linux/arm64"), "{workflow}");
    assert!(workflow.contains("runner: ubuntu-24.04-arm"), "{workflow}");
    // Push-by-digest with caches and attestations; the consumer Dockerfile.
    assert!(
        workflow.contains("outputs: type=image,push-by-digest=true,name-canonical=true,push=true"),
        "{workflow}"
    );
    assert!(workflow.contains("provenance: true"), "{workflow}");
    assert!(workflow.contains("sbom: true"), "{workflow}");
    assert!(workflow.contains("file: Dockerfile"), "{workflow}");
    // Admission reconciles absent, resume, and conflict.
    assert!(workflow.contains("existing=false"), "{workflow}");
    assert!(
        workflow.contains("adopting explicitly supplied recovery index"),
        "{workflow}"
    );
    assert!(
        workflow.contains("refusing to adopt unknown bytes"),
        "{workflow}"
    );
    assert!(workflow.contains("existing-image-digest:"), "{workflow}");
    // Exactly one manifest assembly over the complete verified set.
    assert_eq!(
        workflow.matches("imagetools create").count(),
        1,
        "{workflow}"
    );
    assert!(workflow.contains("--archs amd64,arm64"), "{workflow}");
    assert!(
        workflow.contains("carries an unexpected platform set"),
        "{workflow}"
    );
    // The Dockerfile itself stays consumer-owned: the output carries no
    // image contents, only the declared reference to them.
    assert!(!workflow.contains("FROM scratch"), "{workflow}");
    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&output);
}
