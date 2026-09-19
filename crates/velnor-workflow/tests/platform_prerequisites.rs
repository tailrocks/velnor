//! Slice A: platform requirements, prerequisites, and environment.
//!
//! Units declare what they need (OS, architecture, SDK capabilities) instead
//! of naming providers; Apple work reaches a macOS executor while ordinary
//! `SwiftPM` support stays portable; prerequisite edges compile into selection
//! and prepare steps; and build flags plus the object-transport toggle travel
//! through generic config.

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
use std::process::{Command, Output};

struct Generated {
    output: PathBuf,
}

impl Generated {
    fn workflow(&self, name: &str) -> String {
        fs::read_to_string(self.output.join(".github/workflows").join(name)).unwrap()
    }

    fn project(&self) -> String {
        fs::read_to_string(self.output.join(".github/ci/project.toml")).unwrap()
    }
}

fn unique_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "platform-prereq-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
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
        "generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    Generated { output }
}

fn generate_fail(root: &Path) -> String {
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
        !outcome.status.success(),
        "generation should have failed:\n{}",
        String::from_utf8_lossy(&outcome.stdout)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&outcome.stderr),
        String::from_utf8_lossy(&outcome.stdout)
    )
}

fn write_config(root: &Path, body: &str) {
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        format!("schema = 1\n\n[generator]\nrepository = \"example/fixture\"\n\n{body}"),
    )
    .unwrap();
}

#[cfg(unix)]
fn write_s2_config(root: &Path, body: &str) {
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        format!("schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n{body}"),
    )
    .unwrap();
}

#[cfg(unix)]
fn generate_s2(root: &Path) -> Generated {
    let output = root.parent().unwrap().join(format!(
        "{}-s2-out",
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
        "schema-2 generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    Generated { output }
}

#[cfg(unix)]
fn workflow_step_script(workflow: &str, name: &str) -> String {
    let marker = format!("      - name: {name}\n");
    let start = workflow
        .find(marker.as_str())
        .expect("workflow step is absent");
    let step = &workflow[start..];
    let run = step
        .find("        run: |\n")
        .expect("workflow step has no run body");
    let body = &step[run + "        run: |\n".len()..];
    let mut script = String::new();
    for line in body.lines() {
        if line.starts_with("      - name:") || line.starts_with("    - name:") {
            break;
        }
        if let Some(line) = line.strip_prefix("          ") {
            script.push_str(line);
            script.push('\n');
        } else if line.trim().is_empty() {
            script.push('\n');
        } else {
            break;
        }
    }
    script
}

#[cfg(unix)]
fn run_workflow_step(
    script: &str,
    workspace: &Path,
    runner_temp: &Path,
    unit: &str,
    head_sha: &str,
) -> Output {
    let script_path = runner_temp.join(format!("{unit}.sh"));
    fs::write(&script_path, script).unwrap();
    Command::new("bash")
        .arg(&script_path)
        .env("GITHUB_WORKSPACE", workspace)
        .env("RUNNER_TEMP", runner_temp)
        .env("CI_UNIT_ID", unit)
        .env("HEAD_SHA", head_sha)
        .output()
        .unwrap()
}

#[cfg(unix)]
fn sha256(path: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .unwrap();
    assert!(output.status.success(), "sha256 failed: {output:?}");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned()
}

#[cfg(unix)]
fn write_artifact_manifest(root: &Path, archive: &Path, source_sha: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("manifest.json"),
        format!(
            "{{\"schema\":1,\"producer\":\"rust-ffi\",\"product\":\"xcframework\",\"source_sha\":\"{source_sha}\",\"path\":\"target/xcframework/App.xcframework\",\"kind\":\"xcframework\",\"archive_sha256\":\"{}\"}}\n",
            sha256(archive)
        ),
    )
    .unwrap();
}

fn write_swift_fixture(root: &Path) {
    fs::create_dir_all(root.join("app/Sources/App")).unwrap();
    fs::write(
        root.join("app/Package.swift"),
        "// swift-tools-version: 5.9\n",
    )
    .unwrap();
    let project = root.join("App.xcodeproj");
    let schemes = project.join("xcshareddata/xcschemes");
    fs::create_dir_all(&schemes).unwrap();
    fs::write(project.join("project.pbxproj"), "// empty project\n").unwrap();
    fs::write(
        schemes.join("App.xcscheme"),
        "<Scheme><BuildAction/><TestAction/></Scheme>\n",
    )
    .unwrap();
}

fn write_ffi_fixture(root: &Path) {
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/base\", \"crates/ffi\"]\n",
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
    for name in ["base", "ffi"] {
        fs::create_dir_all(root.join("crates").join(name).join("src")).unwrap();
        fs::write(
            root.join("crates").join(name).join("src/lib.rs"),
            "pub fn n() -> u8 { 1 }\n",
        )
        .unwrap();
    }
    fs::write(
        root.join("crates/base/Cargo.toml"),
        "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        root.join("crates/ffi/Cargo.toml"),
        "[package]\nname = \"ffi\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"staticlib\", \"rlib\"]\n\n[dependencies]\nbase = { path = \"../base\" }\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(
        root.join("app/Package.swift"),
        "// swift-tools-version: 5.9\n",
    )
    .unwrap();
}

fn write_single_rust_fixture(root: &Path, name: &str) {
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!("[workspace]\nmembers = [\"crates/{name}\"]\n"),
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
    fs::create_dir_all(root.join("crates").join(name).join("src")).unwrap();
    fs::write(
        root.join("crates").join(name).join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    fs::write(
        root.join("crates").join(name).join("src/lib.rs"),
        "pub fn n() -> u8 { 1 }\n",
    )
    .unwrap();
}

/// The `[[unit]]` block for `id` inside an emitted `project.toml`.
fn unit_block<'a>(project: &'a str, id: &str) -> &'a str {
    let marker = format!("id = \"{id}\"");
    let start = project
        .find(marker.as_str())
        .expect("project.toml has the unit");
    let rest = &project[start..];
    match rest.find("\n[[unit]]") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

#[test]
fn swiftpm_verifies_on_the_default_executor_while_xcode_needs_macos() {
    let root = unique_dir("swift-split");
    write_swift_fixture(&root);
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n",
    );
    let generated = generate(&root);
    let swift = generated.workflow("ci-unit-swift.yml");
    let (default, apple) = swift
        .split_once("verify-github-apple:")
        .expect("the split kind renders both partitions");
    assert!(
        default.contains("runs-on: ubuntu-24.04"),
        "the default partition stays on Linux: {default}"
    );
    assert!(
        default.contains("inputs.apple_executor != true"),
        "the default partition admits only portable callers: {default}"
    );
    assert!(
        apple.contains("runs-on: macos-15"),
        "the Apple partition reaches macOS: {apple}"
    );
    assert!(
        apple.contains("&& inputs.apple_executor"),
        "the Apple partition admits only Apple callers: {apple}"
    );
    let pr = generated.workflow("ci-pr.yml");
    assert_eq!(
        pr.matches("apple_executor: true").count(),
        1,
        "only the Xcode caller passes the Apple flag: {pr}"
    );
}

#[test]
fn velnor_only_rejects_apple_placement() {
    let root = unique_dir("apple-fail-closed");
    write_swift_fixture(&root);
    write_config(
        &root,
        "[workflow]\nrunners = \"velnor\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n",
    );
    let error = generate_fail(&root);
    assert!(
        error.contains("swift-xcodeproj-app") && error.contains("no matching executor"),
        "placement names the unit and the missing executor: {error}"
    );
}

#[test]
fn ffi_prerequisite_selects_the_consumer_and_prepares_the_product() {
    let root = unique_dir("ffi-prereq");
    write_ffi_fixture(&root);
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"rust-ffi\"\n\n\
         [[units.products]]\nname = \"xcframework\"\ntask = \"build-xcframework\"\n\n\
         [units.products.env]\nXCFRAMEWORK_PATH = \"target/xcframework/App.xcframework\"\n\n\
         [[units]]\nid = \"swift-package-app\"\ncapabilities = [\"xcframework\"]\n\n\
         [[units.prerequisites]]\nproducer = \"rust-ffi\"\nproduct = \"xcframework\"\n\n\
         [units.prerequisites.env]\nTARGETS = \"ios\"\n",
    );
    let generated = generate(&root);
    let project = generated.project();
    assert!(
        project.contains("rust-ffi:ffi"),
        "the scan reports FFI evidence: {project}"
    );
    let ffi = unit_block(&project, "rust-ffi");
    assert!(
        ffi.contains("depends_on = [\"rust-base\"]"),
        "the scan derives the path dependency: {ffi}"
    );
    let swift = unit_block(&project, "swift-package-app");
    assert!(
        swift.contains("depends_on = [\"rust-ffi\"]"),
        "the edge compiles into the selection graph: {swift}"
    );
    assert!(
        swift.contains("TARGETS='ios' mise run build-xcframework"),
        "the consumer prepares the product with its task inputs: {swift}"
    );
    let kind = generated.workflow("ci-unit-swift.yml");
    assert!(
        kind.contains("runs-on: macos-15"),
        "the xcframework capability resolves to macOS: {kind}"
    );
    assert!(
        kind.contains("XCFRAMEWORK_PATH: \"target/xcframework/App.xcframework\""),
        "the product output reaches the consumer job env: {kind}"
    );
}

#[test]
fn artifact_prerequisite_builds_once_and_crosses_hosted_jobs() {
    let root = unique_dir("ffi-artifact");
    write_ffi_fixture(&root);
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"rust-ffi\"\nos = \"macos\"\n\n\
         [[units.products]]\nname = \"xcframework\"\ntask = \"build-xcframework\"\nartifact_path = \"target/xcframework/App.xcframework\"\nartifact_kind = \"xcframework\"\n\n\
         [units.products.env]\nXCFRAMEWORK_PATH = \"target/xcframework/App.xcframework\"\n\n\
         [[units]]\nid = \"swift-package-app\"\ncapabilities = [\"xcframework\"]\n\n\
         [[units.prerequisites]]\nproducer = \"rust-ffi\"\nproduct = \"xcframework\"\n\n\
         [units.prerequisites.env]\nTARGETS = \"ios\"\n",
    );
    let generated = generate(&root);
    let project = generated.project();
    let producer = unit_block(&project, "rust-ffi");
    let consumer = unit_block(&project, "swift-package-app");
    assert!(
        producer.contains("TARGETS='ios' mise run build-xcframework"),
        "the producer owns the artifact task and its inputs: {producer}"
    );
    assert!(
        !consumer.contains("mise run build-xcframework"),
        "the consumer must not silently rebuild a cross-job artifact: {consumer}"
    );
    let rust = generated.workflow("ci-unit-rust.yml");
    assert!(
        rust.contains("Publish hosted product artifacts")
            && rust.contains("archive_sha256")
            && rust.contains("source_sha"),
        "the producer publishes an identity-bound manifest: {rust}"
    );
    let swift = generated.workflow("ci-unit-swift.yml");
    assert!(
        swift.contains("Download hosted product artifacts")
            && swift.contains("Materialize hosted product artifacts")
            && swift.contains("duplicate artifact manifests"),
        "the consumer verifies and materializes the producer artifact: {swift}"
    );
    let aggregate = generated.workflow("ci-pr.yml");
    assert!(
        aggregate.contains("needs: [plan, github-rust-ffi]"),
        "the consumer caller waits for the hosted producer job: {aggregate}"
    );
}

#[cfg(unix)]
#[expect(
    clippy::too_many_lines,
    reason = "the hostile archive harness exercises producer and consumer bytes"
)]
#[test]
fn artifact_shell_materializes_real_bytes_and_rejects_hostile_symlinks() {
    let root = unique_dir("artifact-shell");
    write_ffi_fixture(&root);
    write_config(
        &root,
        r#"[workflow]
runners = "github"
github_runner = "ubuntu-24.04"

[[units]]
id = "rust-ffi"
os = "macos"

[[units.products]]
name = "xcframework"
task = "build-xcframework"
artifact_path = "target/xcframework/App.xcframework"
artifact_kind = "xcframework"

[[units]]
id = "swift-package-app"
capabilities = ["xcframework"]

[[units.prerequisites]]
producer = "rust-ffi"
product = "xcframework"
"#,
    );
    let source_product = root.join("target/xcframework/App.xcframework");
    fs::create_dir_all(&source_product).unwrap();
    fs::write(source_product.join("Info.plist"), "fixture\n").unwrap();
    fs::create_dir_all(source_product.join("Versions/A")).unwrap();
    fs::write(source_product.join("Versions/A/marker"), "linked\n").unwrap();
    std::os::unix::fs::symlink("A", source_product.join("Versions/Current")).unwrap();
    let framework = source_product.join("Foo.framework");
    fs::create_dir_all(framework.join("Versions/A")).unwrap();
    fs::write(framework.join("Versions/A/Foo"), "framework\n").unwrap();
    std::os::unix::fs::symlink("A", framework.join("Versions/Current")).unwrap();
    std::os::unix::fs::symlink("Versions/Current/Foo", framework.join("Foo")).unwrap();
    let generated = generate(&root);
    let producer_script = workflow_step_script(
        &generated.workflow("ci-unit-rust.yml"),
        "Stage hosted product artifacts",
    );
    let consumer_script = workflow_step_script(
        &generated.workflow("ci-unit-swift.yml"),
        "Materialize hosted product artifacts",
    );
    assert!(producer_script.contains("product_dir=\"$stage/$product_name\""));
    assert!(consumer_script.contains("destination=\"$GITHUB_WORKSPACE/$path\""));
    let runner_temp = unique_dir("artifact-shell-temp");
    let head_sha = "0123456789012345678901234567890123456789";
    let producer = run_workflow_step(&producer_script, &root, &runner_temp, "rust-ffi", head_sha);
    assert!(
        producer.status.success(),
        "producer failed:\n{}\n{}",
        String::from_utf8_lossy(&producer.stdout),
        String::from_utf8_lossy(&producer.stderr)
    );
    let incoming = runner_temp.join("velnor-product-inputs/download");
    let staged = runner_temp.join("velnor-products/xcframework");
    fs::create_dir_all(&incoming).unwrap();
    fs::copy(staged.join("archive.tar"), incoming.join("archive.tar")).unwrap();
    fs::copy(staged.join("manifest.json"), incoming.join("manifest.json")).unwrap();
    let consumer_root = unique_dir("artifact-shell-consumer");
    fs::create_dir_all(consumer_root.join("target/xcframework")).unwrap();
    let consumer = run_workflow_step(
        &consumer_script,
        &consumer_root,
        &runner_temp,
        "swift-package-app",
        head_sha,
    );
    assert!(
        consumer.status.success(),
        "consumer failed:\n{}\n{}",
        String::from_utf8_lossy(&consumer.stdout),
        String::from_utf8_lossy(&consumer.stderr)
    );
    assert_eq!(
        fs::read_to_string(consumer_root.join("target/xcframework/App.xcframework/Info.plist"))
            .unwrap(),
        "fixture\n"
    );
    assert_eq!(
        fs::read_to_string(
            consumer_root.join("target/xcframework/App.xcframework/Versions/Current/marker")
        )
        .unwrap(),
        "linked\n"
    );
    assert_eq!(
        fs::read_to_string(
            consumer_root.join("target/xcframework/App.xcframework/Foo.framework/Foo")
        )
        .unwrap(),
        "framework\n"
    );

    let hostile_source = unique_dir("artifact-hostile-source");
    fs::create_dir_all(hostile_source.join("target/xcframework")).unwrap();
    let outside = unique_dir("artifact-hostile-outside");
    std::os::unix::fs::symlink(
        &outside,
        hostile_source.join("target/xcframework/App.xcframework"),
    )
    .unwrap();
    let hostile_archive = unique_dir("artifact-hostile-archive").join("archive.tar");
    let tar = Command::new("tar")
        .args([
            "-C",
            hostile_source.to_str().unwrap(),
            "-cf",
            hostile_archive.to_str().unwrap(),
            "--",
            "target/xcframework/App.xcframework",
        ])
        .output()
        .unwrap();
    assert!(tar.status.success(), "hostile tar failed: {tar:?}");
    let hostile_temp = unique_dir("artifact-hostile-temp");
    let hostile_incoming = hostile_temp.join("velnor-product-inputs/download");
    fs::create_dir_all(&hostile_incoming).unwrap();
    fs::copy(&hostile_archive, hostile_incoming.join("archive.tar")).unwrap();
    write_artifact_manifest(&hostile_incoming, &hostile_archive, head_sha);
    let hostile_consumer = unique_dir("artifact-hostile-consumer");
    fs::create_dir_all(hostile_consumer.join("target/xcframework")).unwrap();
    let hostile_run = run_workflow_step(
        &consumer_script,
        &hostile_consumer,
        &hostile_temp,
        "swift-package-app",
        head_sha,
    );
    assert!(
        !hostile_run.status.success(),
        "root symlink archive was accepted"
    );
    assert!(
        String::from_utf8_lossy(&hostile_run.stderr).contains("symlink")
            || String::from_utf8_lossy(&hostile_run.stdout).contains("symlink"),
        "hostile archive error should name the symlink: {hostile_run:?}"
    );

    let hardlink_source = unique_dir("artifact-hardlink-source");
    let hardlink_product = hardlink_source.join("target/xcframework/App.xcframework");
    fs::create_dir_all(&hardlink_product).unwrap();
    fs::write(hardlink_product.join("one"), "same\n").unwrap();
    fs::hard_link(hardlink_product.join("one"), hardlink_product.join("two")).unwrap();
    let hardlink_archive = unique_dir("artifact-hardlink-archive").join("archive.tar");
    let tar = Command::new("tar")
        .args([
            "-C",
            hardlink_source.to_str().unwrap(),
            "-cf",
            hardlink_archive.to_str().unwrap(),
            "--",
            "target/xcframework/App.xcframework",
        ])
        .output()
        .unwrap();
    assert!(tar.status.success(), "hardlink tar failed: {tar:?}");
    let hardlink_temp = unique_dir("artifact-hardlink-temp");
    let hardlink_incoming = hardlink_temp.join("velnor-product-inputs/download");
    fs::create_dir_all(&hardlink_incoming).unwrap();
    fs::copy(&hardlink_archive, hardlink_incoming.join("archive.tar")).unwrap();
    write_artifact_manifest(&hardlink_incoming, &hardlink_archive, head_sha);
    let hardlink_consumer = unique_dir("artifact-hardlink-consumer");
    fs::create_dir_all(hardlink_consumer.join("target/xcframework")).unwrap();
    let hardlink_run = run_workflow_step(
        &consumer_script,
        &hardlink_consumer,
        &hardlink_temp,
        "swift-package-app",
        head_sha,
    );
    assert!(
        !hardlink_run.status.success(),
        "hardlink archive was accepted"
    );
    assert!(
        String::from_utf8_lossy(&hardlink_run.stderr).contains("hardlink")
            || String::from_utf8_lossy(&hardlink_run.stdout).contains("hardlink"),
        "hardlink rejection should name the member type: {hardlink_run:?}"
    );

    let duplicate_source = unique_dir("artifact-duplicate-source");
    let duplicate_product = duplicate_source.join("target/xcframework/App.xcframework");
    fs::create_dir_all(&duplicate_product).unwrap();
    fs::write(duplicate_product.join("file"), "duplicate\n").unwrap();
    let duplicate_archive = unique_dir("artifact-duplicate-archive").join("archive.tar");
    let tar = Command::new("tar")
        .args([
            "-C",
            duplicate_source.to_str().unwrap(),
            "-cf",
            duplicate_archive.to_str().unwrap(),
            "--",
            "target/xcframework/App.xcframework/file",
        ])
        .output()
        .unwrap();
    assert!(tar.status.success(), "duplicate tar failed: {tar:?}");
    let tar = Command::new("tar")
        .args([
            "-C",
            duplicate_source.to_str().unwrap(),
            "-rf",
            duplicate_archive.to_str().unwrap(),
            "--",
            "target/xcframework/App.xcframework/./file",
        ])
        .output()
        .unwrap();
    assert!(
        tar.status.success(),
        "canonical duplicate append failed: {tar:?}"
    );
    let duplicate_temp = unique_dir("artifact-duplicate-temp");
    let duplicate_incoming = duplicate_temp.join("velnor-product-inputs/download");
    fs::create_dir_all(&duplicate_incoming).unwrap();
    fs::copy(&duplicate_archive, duplicate_incoming.join("archive.tar")).unwrap();
    write_artifact_manifest(&duplicate_incoming, &duplicate_archive, head_sha);
    let duplicate_consumer = unique_dir("artifact-duplicate-consumer");
    fs::create_dir_all(duplicate_consumer.join("target/xcframework")).unwrap();
    let duplicate_run = run_workflow_step(
        &consumer_script,
        &duplicate_consumer,
        &duplicate_temp,
        "swift-package-app",
        head_sha,
    );
    assert!(
        !duplicate_run.status.success(),
        "canonical duplicate archive was accepted"
    );
    assert!(
        String::from_utf8_lossy(&duplicate_run.stderr).contains("non-canonical")
            || String::from_utf8_lossy(&duplicate_run.stdout).contains("non-canonical"),
        "canonical duplicate rejection should name the path form: {duplicate_run:?}"
    );

    let cycle_source = unique_dir("artifact-symlink-cycle-source");
    let cycle_product = cycle_source.join("target/xcframework/App.xcframework");
    fs::create_dir_all(&cycle_product).unwrap();
    std::os::unix::fs::symlink("B", cycle_product.join("A")).unwrap();
    std::os::unix::fs::symlink("A", cycle_product.join("B")).unwrap();
    let cycle_archive = unique_dir("artifact-symlink-cycle-archive").join("archive.tar");
    let tar = Command::new("tar")
        .args([
            "-C",
            cycle_source.to_str().unwrap(),
            "-cf",
            cycle_archive.to_str().unwrap(),
            "--",
            "target/xcframework/App.xcframework",
        ])
        .output()
        .unwrap();
    assert!(tar.status.success(), "symlink cycle tar failed: {tar:?}");
    let cycle_temp = unique_dir("artifact-symlink-cycle-temp");
    let cycle_incoming = cycle_temp.join("velnor-product-inputs/download");
    fs::create_dir_all(&cycle_incoming).unwrap();
    fs::copy(&cycle_archive, cycle_incoming.join("archive.tar")).unwrap();
    write_artifact_manifest(&cycle_incoming, &cycle_archive, head_sha);
    let cycle_consumer = unique_dir("artifact-symlink-cycle-consumer");
    fs::create_dir_all(cycle_consumer.join("target/xcframework")).unwrap();
    let cycle_run = run_workflow_step(
        &consumer_script,
        &cycle_consumer,
        &cycle_temp,
        "swift-package-app",
        head_sha,
    );
    assert!(
        !cycle_run.status.success(),
        "symlink cycle archive was accepted"
    );
    assert!(
        String::from_utf8_lossy(&cycle_run.stderr).contains("cycle")
            || String::from_utf8_lossy(&cycle_run.stdout).contains("cycle"),
        "symlink cycle rejection should name the cycle: {cycle_run:?}"
    );
}

#[cfg(unix)]
#[test]
fn schema_two_artifact_renderer_materializes_a_real_file() {
    let root = unique_dir("artifact-s2-shell");
    write_ffi_fixture(&root);
    write_s2_config(
        &root,
        r#"[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[[units]]
id = "rust-base"

[[units.products]]
name = "base-artifact"
task = "build-base"
artifact_path = "target/base.bin"
artifact_kind = "file"

[[units]]
id = "rust-ffi"

[[units.prerequisites]]
producer = "rust-base"
product = "base-artifact"
"#,
    );
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/base.bin"), "s2 fixture\n").unwrap();
    let generated = generate_s2(&root);
    let workflow = generated.workflow("ci-unit-rust.yml");
    let producer_script = workflow_step_script(&workflow, "Stage hosted product artifacts");
    let consumer_script = workflow_step_script(&workflow, "Materialize hosted product artifacts");
    let runner_temp = unique_dir("artifact-s2-shell-temp");
    let head_sha = "0123456789012345678901234567890123456789";
    let producer = run_workflow_step(&producer_script, &root, &runner_temp, "rust-base", head_sha);
    assert!(
        producer.status.success(),
        "schema-2 producer failed:\n{}\n{}",
        String::from_utf8_lossy(&producer.stdout),
        String::from_utf8_lossy(&producer.stderr)
    );
    let staged = runner_temp.join("velnor-products/base-artifact");
    let incoming = runner_temp.join("velnor-product-inputs/download");
    fs::create_dir_all(&incoming).unwrap();
    fs::copy(staged.join("archive.tar"), incoming.join("archive.tar")).unwrap();
    fs::copy(staged.join("manifest.json"), incoming.join("manifest.json")).unwrap();
    let consumer_root = unique_dir("artifact-s2-shell-consumer");
    fs::create_dir_all(consumer_root.join("target")).unwrap();
    let consumer = run_workflow_step(
        &consumer_script,
        &consumer_root,
        &runner_temp,
        "rust-ffi",
        head_sha,
    );
    assert!(
        consumer.status.success(),
        "schema-2 consumer failed:\n{}\n{}",
        String::from_utf8_lossy(&consumer.stdout),
        String::from_utf8_lossy(&consumer.stderr)
    );
    assert_eq!(
        fs::read_to_string(consumer_root.join("target/base.bin")).unwrap(),
        "s2 fixture\n"
    );
}

#[cfg(unix)]
#[test]
fn schema_two_artifact_renderer_accepts_framework_symlink_chain() {
    let root = unique_dir("artifact-s2-framework-chain");
    write_ffi_fixture(&root);
    write_s2_config(
        &root,
        r#"[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[[units]]
id = "rust-ffi"
platform = "macos-arm64"

[[units.products]]
name = "xcframework"
task = "build-xcframework"
artifact_path = "target/xcframework/App.xcframework"
artifact_kind = "xcframework"

[[units]]
id = "rust-base"
platform = "macos-arm64"

[[units.prerequisites]]
producer = "rust-ffi"
product = "xcframework"
"#,
    );
    let source_product = root.join("target/xcframework/App.xcframework");
    let framework = source_product.join("Foo.framework");
    fs::create_dir_all(framework.join("Versions/A")).unwrap();
    fs::write(framework.join("Versions/A/Foo"), "s2 framework\n").unwrap();
    std::os::unix::fs::symlink("A", framework.join("Versions/Current")).unwrap();
    std::os::unix::fs::symlink("Versions/Current/Foo", framework.join("Foo")).unwrap();

    let generated = generate_s2(&root);
    let workflow = generated.workflow("ci-unit-rust.yml");
    let producer_script = workflow_step_script(&workflow, "Stage hosted product artifacts");
    let consumer_script = workflow_step_script(&workflow, "Materialize hosted product artifacts");
    let runner_temp = unique_dir("artifact-s2-framework-chain-temp");
    let head_sha = "0123456789012345678901234567890123456789";
    let producer = run_workflow_step(&producer_script, &root, &runner_temp, "rust-ffi", head_sha);
    assert!(
        producer.status.success(),
        "schema-2 framework producer failed:\n{}\n{}",
        String::from_utf8_lossy(&producer.stdout),
        String::from_utf8_lossy(&producer.stderr)
    );
    let staged = runner_temp.join("velnor-products/xcframework");
    let incoming = runner_temp.join("velnor-product-inputs/download");
    fs::create_dir_all(&incoming).unwrap();
    fs::copy(staged.join("archive.tar"), incoming.join("archive.tar")).unwrap();
    fs::copy(staged.join("manifest.json"), incoming.join("manifest.json")).unwrap();
    let consumer_root = unique_dir("artifact-s2-framework-chain-consumer");
    fs::create_dir_all(consumer_root.join("target/xcframework")).unwrap();
    let consumer = run_workflow_step(
        &consumer_script,
        &consumer_root,
        &runner_temp,
        "rust-base",
        head_sha,
    );
    assert!(
        consumer.status.success(),
        "schema-2 framework consumer failed:\n{}\n{}",
        String::from_utf8_lossy(&consumer.stdout),
        String::from_utf8_lossy(&consumer.stderr)
    );
    assert_eq!(
        fs::read_to_string(
            consumer_root.join("target/xcframework/App.xcframework/Foo.framework/Foo")
        )
        .unwrap(),
        "s2 framework\n"
    );
}

#[test]
fn artifact_contract_requires_both_typed_fields_and_a_task() {
    let root = unique_dir("artifact-contract");
    write_ffi_fixture(&root);
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"rust-ffi\"\n\n\
         [[units.products]]\nname = \"headers\"\nartifact_path = \"../outside\"\n",
    );
    let error = generate_fail(&root);
    assert!(
        error.contains("must declare both `artifact_path` and `artifact_kind`"),
        "partial artifact metadata fails before rendering: {error}"
    );
}

#[test]
fn artifact_contract_rejects_local_lanes_and_prerequisite_cycles() {
    let dual_lane = unique_dir("artifact-dual-lane");
    write_ffi_fixture(&dual_lane);
    write_config(
        &dual_lane,
        r#"[workflow]
runners = "both"
github_runner = "ubuntu-24.04"
velnor_labels = ["self-hosted", "example-runner"]

[[units]]
id = "rust-ffi"

[[units.products]]
name = "xcframework"
task = "build-xcframework"
artifact_path = "target/xcframework/App.xcframework"
artifact_kind = "xcframework"

[[units]]
id = "swift-package-app"
capabilities = ["xcframework"]

[[units.prerequisites]]
producer = "rust-ffi"
product = "xcframework"
"#,
    );
    let error = generate_fail(&dual_lane);
    assert!(
        error.contains("GitHub-only") && error.contains("no artifact transport"),
        "dual-lane artifact transfer must fail closed: {error}"
    );

    let cycle = unique_dir("artifact-cycle");
    write_ffi_fixture(&cycle);
    write_config(
        &cycle,
        r#"[workflow]
runners = "github"
github_runner = "ubuntu-24.04"

[[units]]
id = "rust-base"

[[units.products]]
name = "base-artifact"
task = "build-base"
artifact_path = "target/base.bin"
artifact_kind = "file"

[[units.prerequisites]]
producer = "rust-ffi"
product = "ffi-artifact"

[[units]]
id = "rust-ffi"

[[units.products]]
name = "ffi-artifact"
task = "build-ffi"
artifact_path = "target/ffi.bin"
artifact_kind = "file"

[[units.prerequisites]]
producer = "rust-base"
product = "base-artifact"
"#,
    );
    let error = generate_fail(&cycle);
    assert!(
        error.contains("prerequisite graph contains a cycle")
            && error.contains("rust-base")
            && error.contains("rust-ffi"),
        "prerequisite cycles must fail closed: {error}"
    );
}

#[test]
fn prerequisite_on_an_undeclared_product_fails_closed() {
    let root = unique_dir("missing-product");
    write_ffi_fixture(&root);
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"rust-ffi\"\n\n\
         [[units.products]]\nname = \"xcframework\"\ntask = \"build-xcframework\"\n\n\
         [[units]]\nid = \"swift-package-app\"\n\n\
         [[units.prerequisites]]\nproducer = \"rust-ffi\"\nproduct = \"headers\"\n",
    );
    let error = generate_fail(&root);
    assert!(
        error.contains("swift-package-app")
            && error.contains("headers")
            && error.contains("does not produce it"),
        "the error names the consumer, the product, and the producer: {error}"
    );
}

#[test]
fn prerequisite_on_an_unknown_producer_fails_closed() {
    let root = unique_dir("missing-producer");
    write_ffi_fixture(&root);
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"swift-package-app\"\n\n\
         [[units.prerequisites]]\nproducer = \"rust-ghost\"\nproduct = \"xcframework\"\n",
    );
    let error = generate_fail(&root);
    assert!(
        error.contains("rust-ghost") && error.contains("does not declare"),
        "the error names the unknown producer: {error}"
    );
}

#[test]
fn declared_build_flags_reach_unit_jobs() {
    let root = unique_dir("build-flags");
    write_single_rust_fixture(&root, "svc");
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"rust-svc\"\n\n\
         [units.env]\nRUSTFLAGS = \"-C link-arg=-fuse-ld=mold -D warnings\"\nCUSTOM_FLAG = \"yes\"\n",
    );
    let generated = generate(&root);
    let kind = generated.workflow("ci-unit-rust.yml");
    assert!(
        kind.contains("RUSTFLAGS: \"-C link-arg=-fuse-ld=mold -D warnings\""),
        "declared flags render into the kind env: {kind}"
    );
    assert!(
        kind.contains("CUSTOM_FLAG: \"yes\""),
        "custom env renders alongside, quoted against boolean typing: {kind}"
    );
}

#[test]
fn mbx_opt_out_keeps_plain_cargo() {
    let root = unique_dir("mbx-opt-out");
    write_single_rust_fixture(&root, "svc");
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"rust-svc\"\nmbx = false\n",
    );
    let generated = generate(&root);
    let project = generated.project();
    let unit = unit_block(&project, "rust-svc");
    assert!(
        unit.contains("cargo test") && !unit.contains("mbx test"),
        "an opted-out unit keeps plain Cargo: {unit}"
    );
    let kind = generated.workflow("ci-unit-rust.yml");
    assert!(
        !kind.contains("Set up Mr. Boxington"),
        "an opted-out unit provisions no object transport: {kind}"
    );
}

#[test]
fn mbx_on_a_non_rust_unit_fails_closed() {
    let root = unique_dir("mbx-mismatch");
    write_single_rust_fixture(&root, "svc");
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"extra-docs\"\nkind = \"docs\"\nroot = \".\"\nmbx = true\n",
    );
    let error = generate_fail(&root);
    assert!(
        error.contains("extra-docs") && error.contains("Rust units only"),
        "the toggle is refused where it cannot apply: {error}"
    );
}

#[test]
fn capability_override_moves_a_unit_to_macos() {
    let root = unique_dir("capability-override");
    write_single_rust_fixture(&root, "svc");
    write_config(
        &root,
        "[workflow]\nrunners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n\n\
         [[units]]\nid = \"rust-svc\"\ncapabilities = [\"xcframework\"]\n",
    );
    let generated = generate(&root);
    let kind = generated.workflow("ci-unit-rust.yml");
    assert!(
        kind.contains("runs-on: macos-15"),
        "an Apple capability resolves to the macOS executor: {kind}"
    );
    assert!(
        !kind.contains("runs-on: ubuntu-24.04"),
        "no Linux job remains for the moved unit: {kind}"
    );
}
