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
use std::process::Command;

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
    write_config_with_provider(root, "github-hosted", &["ubuntu-24.04"], body);
}

fn write_config_with_provider(root: &Path, provider: &str, labels: &[&str], body: &str) {
    fs::create_dir_all(root.join(".github-gen")).unwrap();
    let provider_list = format!("[\"{provider}\"]");
    let selector_labels = labels
        .iter()
        .map(|label| format!("\"{label}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let selectors = format!("[workflow.selectors.{provider}]\nruns_on = [{selector_labels}]\n");
    let config = format!(
        "schema = 2\n\n[generator]\nrepository = \"example/fixture\"\n\n[workflow]\nproviders = {provider_list}\nautomatic_providers = {provider_list}\ndefault_dispatch_providers = {provider_list}\ndefault_branch = \"main\"\n\n{selectors}\n{body}",
    );
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config).unwrap();
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

fn workflow_job_block<'a>(workflow: &'a str, job: &str) -> &'a str {
    let header = format!("\n  {job}:\n");
    let start = workflow.find(&header).expect("workflow contains the job") + header.len();
    let tail = &workflow[start..];
    let mut end = tail.len();
    let mut search = 0;
    while let Some(found) = tail[search..].find("\n  ") {
        let candidate = search + found + 3;
        if tail[candidate..]
            .chars()
            .next()
            .is_some_and(|next| next != ' ')
        {
            end = search + found;
            break;
        }
        search = candidate;
    }
    &tail[..end]
}

#[test]
fn swiftpm_verifies_on_the_default_executor_while_xcode_needs_macos() {
    let root = unique_dir("swift-split");
    write_swift_fixture(&root);
    write_config(&root, "");
    let generated = generate(&root);
    let project = generated.project();
    let package = unit_block(&project, "swift-package-app");
    let xcode = unit_block(&project, "swift-xcodeproj-app");
    assert!(package.contains("platform = \"linux-x64\""), "{package}");
    assert!(xcode.contains("platform = \"macos-arm64\""), "{xcode}");
    let swift = generated.workflow("ci-unit-swift-github-hosted.yml");
    let default = workflow_job_block(&swift, "verify-github-hosted");
    assert!(default.contains("runs-on: ubuntu-24.04"), "{default}");
    assert!(
        default.contains("inputs.apple_executor != true"),
        "the default executor rejects Apple callers: {default}"
    );
    assert!(
        !default.contains("runs-on: macos-15"),
        "portable Swift stays on the configured default executor: {default}"
    );
    let apple = workflow_job_block(&swift, "verify-github-hosted-apple");
    assert!(apple.contains("runs-on: macos-15"), "{apple}");
    assert!(
        apple.contains("&& inputs.apple_executor"),
        "the Apple executor admits only Apple callers: {apple}"
    );
    assert!(
        !apple.contains("runs-on: ubuntu-24.04"),
        "Apple-bound Swift never reaches the default executor: {apple}"
    );
    assert!(swift.contains(
        "jobs.verify-github-hosted.outputs.result_identity || jobs.verify-github-hosted-apple.outputs.result_identity"
    ));
    let pr = generated.workflow("ci-pr.yml");
    let apple_caller = workflow_job_block(&pr, "github-hosted-swift-xcodeproj-app");
    assert!(
        apple_caller.contains("apple_executor: true"),
        "only the Apple caller selects the Apple executor: {apple_caller}"
    );
    let portable_caller = workflow_job_block(&pr, "github-hosted-swift-package-app");
    assert!(
        !portable_caller.contains("apple_executor"),
        "the portable caller keeps the default false input: {portable_caller}"
    );
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
    write_config_with_provider(&root, "velnor", &["self-hosted", "example-runner"], "");
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
        "\n\
         [[units]]\nid = \"rust-ffi\"\n\n\
         [[units.products]]\nname = \"xcframework\"\ntask = \"build-xcframework\"\n\n\
         [units.products.env]\nXCFRAMEWORK_PATH = \"target/xcframework/App.xcframework\"\n\n\
         [[units]]\nid = \"swift-package-app\"\nkind = \"swift\"\nroot = \"app\"\nplatform = \"macos-arm64\"\n\n\
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
    let kind = generated.workflow("ci-unit-swift-github-hosted.yml");
    assert!(
        kind.contains("runs-on: macos-15"),
        "the consumer's declared platform resolves to macOS: {kind}"
    );
    assert!(
        kind.contains("XCFRAMEWORK_PATH: \"target/xcframework/App.xcframework\""),
        "the product output reaches the consumer job env: {kind}"
    );
}

#[test]
fn prerequisite_on_an_undeclared_product_fails_closed() {
    let root = unique_dir("missing-product");
    write_ffi_fixture(&root);
    write_config(
        &root,
        "\n\
         [[units]]\nid = \"rust-ffi\"\n\n\
         [[units.products]]\nname = \"xcframework\"\ntask = \"build-xcframework\"\n\n\
         [[units]]\nid = \"swift-package-app\"\nkind = \"swift\"\nroot = \"app\"\nplatform = \"macos-arm64\"\n\n\
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
        "\n\
         [[units]]\nid = \"swift-package-app\"\nkind = \"swift\"\nroot = \"app\"\nplatform = \"macos-arm64\"\n\n\
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
        "\n\
         [[units]]\nid = \"rust-svc\"\n\n\
         [units.env]\nRUSTFLAGS = \"-C link-arg=-fuse-ld=mold -D warnings\"\nCUSTOM_FLAG = \"yes\"\n",
    );
    let generated = generate(&root);
    let kind = generated.workflow("ci-unit-rust-github-hosted.yml");
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
        "\n\
         [[units]]\nid = \"rust-svc\"\nmbx = false\n",
    );
    let generated = generate(&root);
    let project = generated.project();
    let unit = unit_block(&project, "rust-svc");
    assert!(
        unit.contains("cargo test") && !unit.contains("mbx test"),
        "an opted-out unit keeps plain Cargo: {unit}"
    );
    let kind = generated.workflow("ci-unit-rust-github-hosted.yml");
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
        "\n\
         [[units]]\nid = \"extra-docs\"\nkind = \"docs\"\nroot = \".\"\nmbx = true\n",
    );
    let error = generate_fail(&root);
    assert!(
        error.contains("extra-docs") && error.contains("Rust units only"),
        "the toggle is refused where it cannot apply: {error}"
    );
}

#[test]
fn unit_platform_override_moves_rust_to_macos() {
    let root = unique_dir("capability-override");
    write_single_rust_fixture(&root, "svc");
    write_config(
        &root,
        "\n\
         [[units]]\nid = \"rust-svc\"\nplatform = \"macos-arm64\"\n",
    );
    let generated = generate(&root);
    let kind = generated.workflow("ci-unit-rust-github-hosted.yml");
    assert!(
        kind.contains("runs-on: macos-15"),
        "the declared macOS platform resolves to its executor: {kind}"
    );
    assert!(
        !kind.contains("runs-on: ubuntu-24.04"),
        "no Linux job remains for the moved unit: {kind}"
    );
}
