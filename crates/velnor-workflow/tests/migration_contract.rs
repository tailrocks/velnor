//! Schema-2 consumer contract.
//!
//! The `swift-ffi-consumer` fixture is a neutral second consumer shaped
//! differently from the older Rust-only fixture: a Rust workspace under
//! `packages/` (not `crates/`), a standalone nested crate, a Swift package
//! under `clients/apple/` (not `native/`), no Dockerfile, no docs surface,
//! and a `[renovate]` declaration with explicit provider selectors. It pins
//! the behaviors the consumer contract requires: Swift units land on macOS
//! while Rust stays on Linux, a declared cross-kind `depends_on` selects the
//! Swift consumer when only FFI files change, Renovate uses its declared
//! Velnor and hosted selectors, generation is byte-stable, and no consumer
//! name leaks into output. The
//! deny probe declares the release, preview, docs, scheduled, and
//! maintenance surfaces on its own temp copy, so every rendered family is
//! scanned, not just the default file set.

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
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

fn temp_root(case: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "velnor-migration-contract-{}-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed),
        case
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
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

/// The genericity law's deny list, parsed out of its own source at runtime so
/// the probes stay in sync without this file spelling a consumer name. The
/// law's admitted rewrite (the generic id that embeds a retired label) is
/// parsed the same way and applied to the scanned text first, so this
/// contract matches the law exactly.
fn deny_list_probes() -> (Vec<String>, (String, String)) {
    const LAW: &str = include_str!("../src/s2/generic_surface_literals.rs");
    const LIST_MARKER: &str = "const DENY_LIST";
    const REWRITE_MARKER: &str = ".replace(";
    fn quoted(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = line;
        while let Some(open) = rest.find('"') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('"') else {
                break;
            };
            out.push(rest[..close].to_lowercase());
            rest = &rest[close + 1..];
        }
        out
    }
    let start = LAW.find(LIST_MARKER).unwrap_or(0);
    let mut probes = Vec::new();
    let mut in_list = false;
    for line in LAW[start..].lines() {
        if !in_list {
            if line.contains('[') {
                in_list = true;
            }
            continue;
        }
        if line.contains("];") {
            break;
        }
        probes.extend(quoted(line));
    }
    let mut rewrite = (String::new(), String::new());
    for line in LAW.lines() {
        if line.contains(REWRITE_MARKER) {
            let parts = quoted(line);
            if parts.len() >= 2 {
                rewrite = (parts[0].clone(), parts[1].clone());
            }
            break;
        }
    }
    (probes, rewrite)
}

fn fixture_root(case: &str) -> PathBuf {
    let root = temp_root(case);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/swift-ffi-consumer");
    copy_tree(&source, &root.join("repo"));
    root.join("repo")
}

fn generate(repo: &Path, case: &str) -> PathBuf {
    let output = repo.parent().unwrap().join(format!("{case}-out"));
    let _ = fs::remove_dir_all(&output);
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            output.to_str().unwrap(),
            repo.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        outcome.status.success(),
        "generation failed for {}:\n{}",
        repo.display(),
        String::from_utf8_lossy(&outcome.stderr)
    );
    output
}

fn git(repo: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .status()
        .expect("git present");
    assert!(status.success());
}

fn git_output(repo: &Path, arguments: &[&str]) -> String {
    let outcome = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()
        .expect("git present");
    assert!(outcome.status.success());
    String::from_utf8_lossy(&outcome.stdout).trim().to_owned()
}

#[cfg(unix)]
fn stale_remote_pin_fixture(case: &str) -> (PathBuf, String, PathBuf) {
    let repo = fixture_root(case);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "fixture@example.test"]);
    git(&repo, &["config", "user.name", "fixture"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base fixture"]);
    let pin = git_output(&repo, &["rev-parse", "HEAD"]);
    let config_path = repo.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&config_path).unwrap();
    fs::write(
        &config_path,
        config.replace(
            "repository = \"example/swift-ffi\"",
            &format!("repository = \"example/swift-ffi\"\nrevision = \"{pin}\""),
        ),
    )
    .unwrap();
    git(&repo, &["add", ".github-gen/velnor-workflow.toml"]);
    git(&repo, &["commit", "-qm", "declare remote generator pin"]);
    let output = generate(&repo, case);
    (repo, pin, output)
}

#[cfg(unix)]
fn write_cargo_shim(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let sentinel = directory.join("cargo-shim-fired");
    let shim = directory.join("cargo");
    fs::write(
        &shim,
        "#!/bin/sh\nprintf 'cargo invoked: %s\\n' \"$*\" > \"$CARGO_SHIM_SENTINEL\"\nexit 99\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    sentinel
}

#[cfg(unix)]
fn policy_check_command(repo: &Path, output: &Path, bin_directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"));
    command
        .args([
            "--plain",
            "--check",
            "--default-branch",
            "main",
            "--output",
            output.to_str().unwrap(),
            repo.to_str().unwrap(),
        ])
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", bin_directory.display()),
        )
        .env_remove("VELNOR_WORKFLOW_PINNED_BINARY")
        .env_remove("VELNOR_WORKFLOW_PINNED_BINARY_SHA256")
        .env_remove("VELNOR_WORKFLOW_PINNED_BINARY_REVISION")
        .env_remove("VELNOR_WORKFLOW_PINNED_BINARY_CLOSURE")
        .env_remove("VELNOR_WORKFLOW_CANDIDATE_BINARY")
        .env_remove("VELNOR_WORKFLOW_CANDIDATE_MANIFEST")
        .env_remove("CARGO_NET_OFFLINE");
    command
}

fn workflow(output: &Path, name: &str) -> String {
    fs::read_to_string(output.join(".github/workflows").join(name)).unwrap()
}

fn collect_files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                stack.push(entry);
            } else {
                let relative = entry.strip_prefix(root).unwrap().to_path_buf();
                files.push((relative, fs::read(&entry).unwrap()));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn swift_kind_renders_macos_while_rust_stays_linux() {
    let repo = fixture_root("swift-runners");
    let output = generate(&repo, "swift-runners");

    let swift = workflow(&output, "ci-unit-swift-github-hosted.yml");
    assert!(
        swift.contains("runs-on: macos-15"),
        "swift kind must land on macOS:\n{swift}"
    );
    assert!(
        !swift.contains("runs-on: ubuntu-24.04"),
        "swift kind must not land on Linux:\n{swift}"
    );

    let rust = workflow(&output, "ci-unit-rust-github-hosted.yml");
    assert!(
        rust.contains("runs-on: ubuntu-24.04"),
        "rust kind must stay on Linux:\n{rust}"
    );
    assert!(
        !rust.contains("runs-on: macos-15"),
        "rust kind must not move to macOS:\n{rust}"
    );
}

#[test]
fn renovate_writer_and_validator_use_declared_provider_selectors() {
    let repo = fixture_root("reno-lanes");
    let output = generate(&repo, "reno-lanes");

    let writer = workflow(&output, "renovate.yml");
    assert!(
        writer.contains("runs-on: [self-hosted, example-lane]"),
        "writer must use the repository's declared Velnor selector:\n{writer}"
    );
    assert!(
        !writer.contains("inputs.lanes"),
        "schema 2 must not render the retired lane selector:\n{writer}"
    );

    let validate = workflow(&output, "renovate-validate.yml");
    assert!(
        validate.contains("runs-on: ubuntu-24.04"),
        "validator runs on GitHub runners:\n{validate}"
    );
}

#[test]
fn repeat_generation_is_byte_identical() {
    let repo = fixture_root("byte-stable");
    let first = generate(&repo, "byte-stable-first");
    let second = generate(&repo, "byte-stable-second");

    let first_files = collect_files(&first);
    let second_files = collect_files(&second);
    assert_eq!(
        first_files.len(),
        second_files.len(),
        "regeneration changed the file set"
    );
    for (first_entry, second_entry) in first_files.iter().zip(second_files.iter()) {
        assert_eq!(
            first_entry.0, second_entry.0,
            "regeneration changed file paths"
        );
        assert_eq!(
            first_entry.1,
            second_entry.1,
            "regeneration changed bytes in {}",
            first_entry.0.display()
        );
    }
}

#[test]
fn ffi_change_selects_the_swift_consumer() {
    let repo = fixture_root("ffi-selects-swift");
    let output = generate(&repo, "ffi-selects-swift");
    let contract = output.join(".github/ci/project.toml");
    assert!(
        contract.is_file(),
        "generation must emit a runtime contract"
    );

    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "base",
        ],
    );
    let base = git_output(&repo, &["rev-parse", "HEAD"]);
    fs::write(
        repo.join("packages/engine-ffi/src/lib.rs"),
        "pub const ABI_VERSION: &str = \"2\";\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "ffi bump",
        ],
    );
    let head = git_output(&repo, &["rev-parse", "HEAD"]);

    let github_output = repo.parent().unwrap().join("github-output.txt");
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .current_dir(&repo)
        .env("BASE_SHA", &base)
        .env("HEAD_SHA", &head)
        .env("EVENT_NAME", "pull_request")
        .env("VELNOR_LANES", "github")
        .env("GITHUB_OUTPUT", &github_output)
        // The CI unit job exports a repo-relative VELNOR_SELECTION_FILE;
        // an inheriting child would try to write it under the fixture
        // instead of asserting on GITHUB_OUTPUT alone.
        .env_remove("VELNOR_SELECTION_FILE")
        .args(["plan", "--config", contract.to_str().unwrap()])
        .output()
        .expect("run velnor-workflow plan");
    assert!(
        outcome.status.success(),
        "plan failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    let plan_output = fs::read_to_string(&github_output).unwrap();
    let units = plan_output
        .lines()
        .find_map(|line| line.strip_prefix("units="))
        .unwrap_or_default();
    assert!(
        units.contains("rust-engine-ffi"),
        "changed FFI crate must be selected:\n{plan_output}"
    );
    assert!(
        units.contains("swift-package-clients-apple"),
        "Swift consumer must be selected by its FFI prerequisite:\n{plan_output}"
    );
    let matrix = plan_output
        .lines()
        .find_map(|line| line.strip_prefix("swift_matrix="))
        .unwrap_or_default();
    assert!(
        matrix.contains("swift-package-clients-apple"),
        "Swift matrix must be non-empty:\n{plan_output}"
    );
}

/// The default file set omits the release, preview, docs, and scheduled
/// surfaces, so the deny probe declares all of them on its own temp copy:
/// an unrendered surface is an unprobed surface. Every value stays neutral;
/// the law forbids consumer names in fixtures.
const EXTRA_SURFACES: &str = r#"
[[declare]]
primitive = "release"
file = "release.yml"

[declare.args]
kind = "docker"
image = "ghcr.io/example/app"
dockerfile = "Dockerfile"
context = "."
platforms = ["linux/amd64", "linux/arm64"]

[[declare]]
primitive = "preview"
file = "preview.yml"

[declare.args]
package = "app"
binary = "app"
targets = ["x86_64-unknown-linux-gnu"]

[docs]
enabled = true
reason = "Example site for the no-consumer-names probe"
site_url = "https://docs.example.com"
site_dir = "site"
build_commands = ["mise run docs:build"]
source_link_commands = ["mise run docs:check-source-links"]
site_link_commands = ["mise run docs:check-site-links"]
spell_commands = ["mise run docs:spell"]
verify_commands = ["mise run docs:verify-deployed"]

[[declare]]
primitive = "docs-site"
file = "docs.yml"

[[check_profile]]
id = "smoke"
name = "Smoke probe"
schedule = "23 2 * * *"
tasks = ["check-smoke"]
timeout_minutes = 30

[[declare]]
primitive = "scheduled-checks"
file = "scheduled-daily.yml"

[declare.args]
profiles = ["smoke"]

[[declare]]
primitive = "maintenance"
file = "maintenance.yml"
"#;

#[test]
fn generated_output_names_no_consumer() {
    let repo = fixture_root("no-consumer-names");
    let config_path = repo.join(".github-gen/velnor-workflow.toml");
    let mut config = fs::read_to_string(&config_path).unwrap();
    config.push_str(EXTRA_SURFACES);
    fs::write(&config_path, config).unwrap();
    // Check-profile tasks must exist in mise.toml; the fixture owns none, so
    // the probe declares the one task its smoke profile names.
    fs::write(
        repo.join("mise.toml"),
        "[tasks.check-smoke]\nrun = \"echo smoke\"\n",
    )
    .unwrap();
    let output = generate(&repo, "no-consumer-names");
    // Fail closed: every declared surface must have rendered, or the probe
    // below scans a silent subset.
    for file in [
        "release.yml",
        "preview.yml",
        "docs.yml",
        "scheduled-daily.yml",
        "maintenance.yml",
        "nightly.yml",
        "renovate.yml",
    ] {
        assert!(
            output.join(".github/workflows").join(file).is_file(),
            "the deny probe needs a rendered {file}"
        );
    }

    // The probe names come from the genericity law's own deny list, parsed at
    // runtime: this file must not spell a consumer name literally (see
    // `s2::generic_surface_literals`), and the probes stay in sync with the law.
    let (forbidden, (admitted, replacement)) = deny_list_probes();
    assert!(
        !forbidden.is_empty(),
        "the genericity deny list parsed to no probes"
    );
    for (relative, bytes) in collect_files(&output) {
        let text = String::from_utf8_lossy(&bytes)
            .to_lowercase()
            .replace(&admitted, &replacement);
        for name in &forbidden {
            assert!(
                !text.contains(name),
                "{} leaks consumer name {name}",
                relative.display()
            );
        }
    }
}

#[test]
fn clean_room_regeneration_keeps_ownership_proof_and_rejects_orphans() {
    let script = include_str!("../../../migrations/generic-workflow-generator/clean-room-regen.sh");
    assert!(
        script.contains("OWNERSHIP_STATE=\"$REPO_DIR/.github/ci/.github-actions-generator-state\""),
        "clean-room migration keeps the generated ownership ledger"
    );
    assert!(
        script.contains("grep -qx 'schema = 2' \"$OWNERSHIP_STATE\""),
        "an unknown ledger schema fails before regeneration"
    );
    assert!(
        !script.contains("rm -f \"$REPO_DIR/.github/ci/.github-actions-generator-state\""),
        "clean-room migration cannot erase hash ownership proof"
    );
    assert!(
        script.contains("Unowned obsolete workflow remains after regeneration"),
        "a missing ownership ledger cannot silently carry obsolete generic unit files forward"
    );
}

/// Offline policy execution must stay fail-closed even when the operator
/// explicitly opens `--pin-build`: a cargo shim proves no hidden source
/// build is attempted as a fallback.
#[cfg(unix)]
#[test]
fn offline_pin_build_does_not_invoke_cargo() {
    let (repo, pin, output) = stale_remote_pin_fixture("offline-pin-build");
    let bin_directory = repo.parent().unwrap().join("offline-pin-build-bin");
    fs::create_dir_all(&bin_directory).unwrap();
    let sentinel = write_cargo_shim(&bin_directory);
    let outcome = policy_check_command(&repo, &output, &bin_directory)
        .arg("--pin-build")
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_SHIM_SENTINEL", &sentinel)
        .output()
        .expect("run offline generator check");
    let detail = format!(
        "{}{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(!outcome.status.success(), "offline pin build must fail: {detail}");
    assert!(detail.contains(&pin), "the failure names the pin: {detail}");
    assert!(
        detail.contains("building one is forbidden here"),
        "the offline guard rejects the build fallback: {detail}"
    );
    assert!(
        !sentinel.exists(),
        "CARGO_NET_OFFLINE=true must stop before cargo executes"
    );
}

/// The candidate-manifest environment fallback is part of the generated
/// consumer contract. A manifest for another tree must fail before the
/// candidate executable can run.
#[cfg(unix)]
#[test]
fn candidate_manifest_environment_mismatch_fails_before_candidate_exec() {
    use std::os::unix::fs::PermissionsExt as _;

    let (repo, pin, output) = stale_remote_pin_fixture("candidate-manifest-env");
    let head = git_output(&repo, &["rev-parse", "HEAD"]);
    let bin_directory = repo.parent().unwrap().join("candidate-manifest-env-bin");
    fs::create_dir_all(&bin_directory).unwrap();
    let pinned = bin_directory.join("velnor-workflow");
    fs::write(
        &pinned,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --revision ]; then echo {pin}; exit 0; fi\nif [ \"$1\" = --closure ]; then echo cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc; exit 0; fi\nmkdir -p \"$3/drift\"\nprintf 'pin drift\\n' > \"$3/drift/file.txt\"\nexit 0\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&pinned, fs::Permissions::from_mode(0o755)).unwrap();

    let candidate = bin_directory.join("candidate");
    let sentinel = bin_directory.join("candidate-executed");
    fs::write(
        &candidate,
        "#!/bin/sh\nprintf 'candidate executed\\n' > \"$CANDIDATE_EXEC_SENTINEL\"\nexit 91\n",
    )
    .unwrap();
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
    let manifest = bin_directory.join("candidate-manifest.json");
    fs::write(
        &manifest,
        format!(
            "{{\"profile\":\"debug\",\"platform\":\"Linux-X64\",\"repository\":\"example/swift-ffi\",\"run_id\":\"1\",\"revision\":\"{head}\",\"build_revision\":\"{pin}\",\"closure\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"binary_sha256\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\"}}"
        ),
    )
    .unwrap();

    let outcome = policy_check_command(&repo, &output, &bin_directory)
        .env("VELNOR_WORKFLOW_CANDIDATE_BINARY", &candidate)
        .env("VELNOR_WORKFLOW_CANDIDATE_MANIFEST", &manifest)
        .env("CANDIDATE_EXEC_SENTINEL", &sentinel)
        .output()
        .expect("run generator check with env candidate manifest");
    let detail = format!(
        "{}{}",
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(!outcome.status.success(), "mismatched candidate must fail: {detail}");
    assert!(
        detail.contains("names closure"),
        "the environment manifest is loaded and its tree mismatch is explicit: {detail}"
    );
    assert!(
        !sentinel.exists(),
        "candidate bytes must not execute before the manifest closure matches"
    );
}
