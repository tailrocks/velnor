//! Schema-2 native Apple graph: a Rust BoltFFI producer, its SwiftPM
//! consumer, and an XcodeGen app discovered from one fixture, with the
//! product edge, macOS placement, and Xcode probe rendered end to end.

use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

const PRODUCER: &str = "rust-bridge-core-ffi";
const CONSUMER: &str = "swift-package-clients-apple";
const APP: &str = "swift-xcodegen-clients-apple-project-yml-bridgeapp";

fn scratch_dir(name: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "velnor-swift-native-{name}-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Copy the fixture to `scratch/src` and return `(scratch, src)`. Generated
/// output must live beside the scan root, never inside it: a second scan
/// would otherwise observe the first generation's files.
fn fixture_root(name: &str) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let scratch = scratch_dir(name);
    let src = scratch.join("src");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures-s2/swift-native"),
        &src,
    )?;
    Ok((scratch, src))
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "status={}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn generate(root: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            &out.to_string_lossy(),
        ])
        .arg(root)
        .output()?;
    if !output.status.success() {
        return Err(format!("generate failed: {}", output_text(&output)).into());
    }
    Ok(())
}

fn unit_block(project: &str, id: &str) -> Result<String, Box<dyn Error>> {
    let marker = format!("id = \"{id}\"");
    let start = project
        .find(&marker)
        .ok_or_else(|| format!("project.toml has no unit {id}"))?;
    let rest = &project[start..];
    let end = rest
        .find("\n[[unit]]")
        .map(|offset| start + offset)
        .unwrap_or(project.len());
    Ok(project[start..end].to_owned())
}

#[test]
fn native_graph_renders_product_edge_probe_and_macos_placement() -> Result<(), Box<dyn Error>> {
    let (scratch, root) = fixture_root("graph")?;
    let out = scratch.join("out");
    generate(&root, &out)?;
    let project = fs::read_to_string(out.join(".github/ci/project.toml"))?;

    for id in [PRODUCER, CONSUMER, APP] {
        assert!(
            project.contains(&format!("id = \"{id}\"")),
            "project.toml names {id}:\n{project}"
        );
    }

    let producer = unit_block(&project, PRODUCER)?;
    assert!(
        producer.contains("platform = \"macos-arm64\""),
        "the BoltFFI producer is forced onto macOS:\n{producer}"
    );
    assert!(
        producer.contains("boltffi -v --cargo-arg=--locked pack apple"),
        "the producer runs the locked pack recipe:\n{producer}"
    );

    let consumer = unit_block(&project, CONSUMER)?;
    assert!(
        consumer.contains("platform = \"macos-arm64\""),
        "the Swift consumer runs on macOS:\n{consumer}"
    );
    assert!(
        consumer.contains(&format!("depends_on = [\"{PRODUCER}\"]")),
        "the consumer selects the producer:\n{consumer}"
    );
    let rebuild = consumer
        .find("VELNOR_PRODUCT_RUST_BRIDGE_CORE_FFI__XCFRAMEWORK_BRIDGECORE_READY")
        .ok_or("the consumer guards on the XCFramework product")?;
    let build = consumer
        .find("swift build")
        .ok_or("the consumer builds with SwiftPM")?;
    assert!(
        rebuild < build,
        "the producer materializes before `swift build` consumes it:\n{consumer}"
    );

    let app = unit_block(&project, APP)?;
    assert!(
        app.contains("xcodebuild"),
        "the XcodeGen app unit builds via xcodebuild:\n{app}"
    );

    assert!(
        project.contains("BridgeCoreFFI")
            && project.contains("target/xcframework/BridgeCore.xcframework"),
        "the join diagnostic names the module and artifact:\n{project}"
    );

    let swift = fs::read_to_string(out.join(".github/workflows/ci-unit-swift.yml"))?;
    assert!(
        swift.contains("runs-on: macos-26"),
        "the Swift job runs on macOS:\n{swift}"
    );
    assert!(
        swift.contains("- name: Select Xcode 26.6"),
        "the Swift job probes the pinned Xcode:\n{swift}"
    );
    assert!(
        swift.contains("DEVELOPER_DIR="),
        "the probe exports DEVELOPER_DIR:\n{swift}"
    );

    let rust = fs::read_to_string(out.join(".github/workflows/ci-unit-rust.yml"))?;
    assert!(
        rust.contains("runs-on: macos-26"),
        "the producer job runs on macOS:\n{rust}"
    );

    fs::remove_dir_all(&scratch)?;
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = Command::new("git").current_dir(root).args(args).output()?;
    if !output.status.success() {
        return Err(format!("git {args:?} failed: {}", output_text(&output)).into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn plan_unit_ids(root: &Path, base: &str, head: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let github_output = root.join("github-output.txt");
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .current_dir(root)
        .env("EVENT_NAME", "pull_request")
        .env("BASE_SHA", base)
        .env("HEAD_SHA", head)
        .env("GITHUB_OUTPUT", &github_output)
        .args(["plan", "--config", ".github/ci/project.toml"])
        .output()?;
    if !output.status.success() {
        return Err(format!("plan failed: {}", output_text(&output)).into());
    }
    let text = fs::read_to_string(&github_output)?;
    for line in text.lines() {
        if let Some(json) = line.strip_prefix("units=") {
            let mut ids = Vec::new();
            for chunk in json.split("\"unit_id\"") {
                let Some(rest) = chunk.split(':').nth(1) else {
                    continue;
                };
                let id: String = rest
                    .chars()
                    .skip_while(|char| *char != '"')
                    .skip(1)
                    .take_while(|char| *char != '"')
                    .collect();
                if !id.is_empty() {
                    ids.push(id);
                }
            }
            return Ok(ids);
        }
    }
    Err(format!("plan emitted no units output:\n{text}").into())
}

#[test]
fn ffi_source_change_selects_producer_and_swift_consumer() -> Result<(), Box<dyn Error>> {
    let (scratch, root) = fixture_root("plan")?;
    git(&root, &["init", "-q"])?;
    git(&root, &["config", "user.email", "test@example.invalid"])?;
    git(&root, &["config", "user.name", "Velnor test"])?;
    git(&root, &["add", "."])?;
    git(&root, &["commit", "-qm", "base"])?;
    let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .current_dir(&root)
        .args(["--plain", "--default-branch", "main", "."])
        .output()?;
    if !output.status.success() {
        return Err(format!("in-place generate failed: {}", output_text(&output)).into());
    }
    let base = git(&root, &["rev-parse", "HEAD"])?;
    fs::write(
        root.join("libs/bridge-ffi/src/lib.rs"),
        "pub struct BridgeCore;\n\n#[boltffi::export]\nimpl BridgeCore {\n    pub fn version() -> u32 {\n        2\n    }\n}\n",
    )?;
    git(&root, &["commit", "-qam", "bump ffi"])?;
    let head = git(&root, &["rev-parse", "HEAD"])?;

    let ids = plan_unit_ids(&root, &base, &head)?;
    for id in [PRODUCER, CONSUMER] {
        assert!(
            ids.iter().any(|selected| selected == id),
            "an FFI edit selects {id}: {ids:?}"
        );
    }

    let bindings =
        root.join("libs/bridge-ffi/dist/apple/Sources/BoltFFI/BridgeCoreFfiBoltFFI.swift");
    let mut edited = fs::read_to_string(&bindings)?;
    edited.push_str("// hand edit to committed bindings\n");
    fs::write(&bindings, edited)?;
    git(&root, &["commit", "-qam", "touch bindings"])?;
    let drift = git(&root, &["rev-parse", "HEAD"])?;

    let ids = plan_unit_ids(&root, &head, &drift)?;
    assert!(
        ids.iter().any(|selected| selected == PRODUCER),
        "a committed-bindings edit selects the producer for drift verification: {ids:?}"
    );

    fs::remove_dir_all(&scratch)?;
    Ok(())
}

fn snapshot_files(out: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>, Box<dyn Error>> {
    let mut files = Vec::new();
    let mut stack = vec![out.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()?;
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                stack.push(entry);
            } else {
                let bytes = fs::read(&entry)?;
                files.push((entry.strip_prefix(out)?.to_path_buf(), bytes));
            }
        }
    }
    files.sort();
    Ok(files)
}

#[test]
fn repeat_generation_is_byte_identical() -> Result<(), Box<dyn Error>> {
    let (scratch, root) = fixture_root("stable")?;
    let first = scratch.join("first");
    let second = scratch.join("second");
    generate(&root, &first)?;
    generate(&root, &second)?;
    let before = snapshot_files(&first)?;
    let after = snapshot_files(&second)?;
    let before_names: Vec<&PathBuf> = before.iter().map(|(name, _)| name).collect();
    let after_names: Vec<&PathBuf> = after.iter().map(|(name, _)| name).collect();
    assert_eq!(
        before_names, after_names,
        "regeneration emits the same file set"
    );
    for ((name, old), (_, new)) in before.iter().zip(after.iter()) {
        assert_eq!(
            old,
            new,
            "regeneration is byte-identical for {}",
            name.display()
        );
    }
    fs::remove_dir_all(&scratch)?;
    Ok(())
}
