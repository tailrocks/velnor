//! Executable fixtures for immutable package identity at the consumer boundary.

#![expect(
    clippy::unwrap_used,
    reason = "fixture setup failures should identify their operation"
)]
#![expect(
    clippy::expect_used,
    reason = "fixture setup failures should identify their operation"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_yaml::{Mapping, Value};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
const REPOSITORY: &str = "example/preview-source";
const TAP_REPOSITORY: &str = "example/preview-tap";
const CHANNEL: &str = "preview";
const RELEASE_TAG_PREFIX: &str = "fixture-assets";
const PAYLOAD: &str = "preview-package.tar.gz";
const SUPPORT: &str = "SHA256SUMS";
const LOCK_SHA: &str = "1111111111111111111111111111111111111111";

#[derive(Clone, Copy, Eq, PartialEq)]
enum TagMode {
    Immutable,
    Legacy,
}

impl TagMode {
    fn as_config(self) -> &'static str {
        match self {
            Self::Immutable => "immutable",
            Self::Legacy => "legacy",
        }
    }
}

struct Fixture {
    root: PathBuf,
    workflow: Value,
    mode: TagMode,
}

struct PublicationRun {
    output: Output,
    gh_log: String,
    git_log: String,
    workflow_output: String,
    release_state: String,
}

fn fresh_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "package-preview-004-{label}-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn config(mode: TagMode, refresh_rolling: bool) -> String {
    format!(
        r#"schema = 2

[generator]
repository = "{REPOSITORY}"

[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_branch = "main"

[workflow.selectors.github-hosted]
runs_on = ["ubuntu-24.04"]

[[declare]]
primitive = "package-release"
file = "preview.yml"

[declare.args]
build_tasks = ["build-release"]
verify_tasks = ["verify-release"]
package_dir = "package"
manifest_schema = "example.preview-manifest-v1"
source_repository = "{REPOSITORY}"
source_ref = "refs/heads/main"
payloads = ["{PAYLOAD}"]
supporting_assets = ["{SUPPORT}"]
channel = "{CHANNEL}"
release_tag = "{RELEASE_TAG_PREFIX}"
publication_lock_branch = "preview-publication-lock"
github_release_type = "prerelease"
publish_environment = "preview-publish"
consumer_repository = "{TAP_REPOSITORY}"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"
update_commit_message = "chore: update preview package"
consumer_tag_mode = "{mode}"
refresh_rolling_release = {refresh_rolling}

[declare.args.production_inputs]
product = ["src/**", "Cargo.toml", "Cargo.lock"]

[declare.args.production_dependencies]
runtime_resource = ["resources/**"]

[declare.args.non_production_inputs]
documentation = ["README.md", "docs/**"]
verification_fixtures = ["tests/**"]
"#,
        mode = mode.as_config(),
        refresh_rolling = refresh_rolling,
    )
}

fn old_legacy_config_without_task003_inputs() -> String {
    config(TagMode::Legacy, true)
        .lines()
        .filter(|line| {
            !line.starts_with("consumer_tag_mode =")
                && !line.starts_with("refresh_rolling_release =")
        })
        .take_while(|line| !line.starts_with("[declare.args.production_inputs]"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fixture_root_with_config(label: &str, config_text: &str) -> PathBuf {
    let root = fresh_root(label);
    fs::create_dir_all(root.join(".github-gen")).expect("create generator config directory");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"package-release-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write supported Cargo project manifest");
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    )
    .expect("pin supported Rust project toolchain");
    fs::create_dir_all(root.join("src")).expect("create supported Rust project source");
    fs::write(root.join("src/lib.rs"), "pub fn fixture() -> u8 { 1 }\n")
        .expect("write supported Rust project source");
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config_text)
        .expect("write generator config");
    fs::write(
        root.join(".github-gen/visibility.toml"),
        format!("repository = \"{REPOSITORY}\"\nvisibility = \"public\"\n"),
    )
    .expect("write fixture visibility");
    fs::write(
        root.join("mise.toml"),
        "[tasks.build-release]\nrun = \"true\"\n\n[tasks.verify-release]\nrun = \"./scripts/verify-package.sh\"\n",
    )
    .expect("write fixture task definitions");
    root
}

fn run_fixture_generator(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(["--plain", "--force", "--default-branch", "main"])
        .arg(root)
        .output()
        .expect("run generator")
}

fn generate_fixture_from_config(label: &str, config_text: &str, mode: TagMode) -> Fixture {
    let root = fixture_root_with_config(label, config_text);
    let generated = run_fixture_generator(&root);
    assert!(
        generated.status.success(),
        "generation failed:\n{}{}",
        String::from_utf8_lossy(&generated.stdout),
        String::from_utf8_lossy(&generated.stderr)
    );
    let rendered = fs::read_to_string(root.join(".github/workflows/preview.yml"))
        .expect("read generated package-release workflow");
    let workflow = serde_yaml::from_str(&rendered).expect("parse generated workflow YAML");
    Fixture {
        root,
        workflow,
        mode,
    }
}

fn generate_fixture(label: &str, mode: TagMode, refresh_rolling: bool) -> Fixture {
    generate_fixture_from_config(label, &config(mode, refresh_rolling), mode)
}

fn assert_generator_rejects(label: &str, config_text: &str, expected_message: &str) {
    let root = fixture_root_with_config(label, config_text);
    let output = run_fixture_generator(&root);
    assert!(
        !output.status.success(),
        "invalid package-release config unexpectedly generated: {label}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected_message),
        "config {label} failed for an unrelated reason: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn publish_steps(workflow: &Value) -> &[Value] {
    workflow["jobs"]["publish"]["steps"]
        .as_sequence()
        .expect("publish step list")
}

fn step_named<'a>(workflow: &'a Value, name: &str) -> &'a Value {
    publish_steps(workflow)
        .iter()
        .find(|step| step["name"].as_str() == Some(name))
        .expect("missing generated publish step")
}

fn step_run<'a>(workflow: &'a Value, name: &str) -> &'a str {
    step_named(workflow, name)["run"]
        .as_str()
        .expect("step has no run script")
}

fn env_map<'a>(workflow: &'a Value, name: &str) -> &'a Mapping {
    step_named(workflow, name)["env"]
        .as_mapping()
        .expect("step has no environment mapping")
}

fn mapping_string<'a>(mapping: &'a Mapping, name: &str) -> Option<&'a str> {
    mapping.get(name).and_then(Value::as_str)
}

fn value_field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.as_mapping()?.get(name)
}

fn write_executable(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().expect("executable has parent"))
        .expect("create executable parent");
    fs::write(path, body).expect("write executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path)
            .expect("executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("set executable permissions");
    }
}

fn run_bash(script: &str, cwd: &Path, envs: &[(String, String)]) -> Output {
    let mut command = Command::new("bash");
    command
        .env_clear()
        .arg("-e")
        .arg("-u")
        .arg("-o")
        .arg("pipefail")
        .arg("-c")
        .arg(script)
        .current_dir(cwd);
    command.envs(envs.iter().map(|(name, value)| (name, value)));
    command.output().expect("run emitted workflow script")
}

fn source_sha(letter: char) -> String {
    letter.to_string().repeat(40)
}

fn package_version(source_sha: &str) -> String {
    format!("1.2.3-{CHANNEL}.1+{}", &source_sha[..7])
}

fn file_sha256(path: &Path) -> String {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("hash fixture file");
    assert!(
        output.status.success(),
        "fixture hash command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture hash output is UTF-8")
        .split_whitespace()
        .next()
        .expect("fixture hash output contains digest")
        .to_owned()
}

fn write_verified_package(root: &Path, source_sha: &str, directory: &str) -> PathBuf {
    let package = root.join(directory);
    fs::create_dir_all(&package).expect("create package handoff fixture");
    fs::write(package.join(PAYLOAD), b"fixture package payload\n").expect("write package payload");
    let payload_sha = file_sha256(&package.join(PAYLOAD));
    fs::write(package.join(SUPPORT), format!("{payload_sha}  {PAYLOAD}\n"))
        .expect("write supporting asset");
    let support_sha = file_sha256(&package.join(SUPPORT));
    let version = package_version(source_sha);
    let manifest = format!(
        "{{\"schema\":\"example.preview-manifest-v1\",\"source_repository\":\"{REPOSITORY}\",\"source_ref\":\"refs/heads/main\",\"source_commit\":\"{source_sha}\",\"version\":\"{version}\",\"assets\":[{{\"name\":\"{PAYLOAD}\",\"sha256\":\"{payload_sha}\"}}],\"supporting_assets\":[{{\"name\":\"{SUPPORT}\",\"sha256\":\"{support_sha}\"}}]}}"
    );
    fs::write(
        package.join("release-manifest.json"),
        format!("{manifest}\n"),
    )
    .expect("write release manifest");
    fs::write(
        package.join("identity.json"),
        format!(
            "{{\"manifest\":{manifest},\"source_digest\":\"{source_sha}\",\"source_ref\":\"refs/heads/main\",\"source_repository\":\"{REPOSITORY}\"}}\n"
        ),
    )
    .expect("write identity metadata");
    package
}

fn updater_script(mode: TagMode) -> String {
    let required_identity = match mode {
        TagMode::Immutable => {
            r#"
: "${VELNOR_PACKAGE_ASSET_TAG:?missing immutable asset tag}"
: "${VELNOR_PACKAGE_VERSION:?missing package version}"
: "${VELNOR_PACKAGE_SOURCE_COMMIT:?missing source commit}"
: "${VELNOR_PACKAGE_SOURCE_REPOSITORY:?missing source repository}"
: "${VELNOR_PACKAGE_SOURCE_REF:?missing source ref}"
selected_tag="$VELNOR_PACKAGE_ASSET_TAG"
"#
        }
        // This models an existing updater that knows only the pre-immutable
        // channel, release-tag, and verified-directory inputs.
        TagMode::Legacy => {
            r#"
: "${VELNOR_PACKAGE_RELEASE_TAG:?missing legacy release tag}"
selected_tag="$VELNOR_PACKAGE_RELEASE_TAG"
"#
        }
    };
    let immutable_assertions = match mode {
        TagMode::Immutable => {
            r#"
[[ "$version" == "$VELNOR_PACKAGE_VERSION" ]]
[[ "$source_sha" == "$VELNOR_PACKAGE_SOURCE_COMMIT" ]]
[[ "$repository" == "$manifest_repository" ]]
[[ "$source_ref" == "$VELNOR_PACKAGE_SOURCE_REF" ]]
"#
        }
        TagMode::Legacy => "",
    };
    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
: "${{VELNOR_PACKAGE_CHANNEL:?missing channel}}"
: "${{VELNOR_VERIFIED_PACKAGE_DIR:?missing verified package directory}}"
{required_identity}
version="$(jq -er '.version' "$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json")"
source_sha="$(jq -er '.source_commit' "$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json")"
manifest_repository="$(jq -er '.source_repository' "$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json")"
source_ref="$(jq -er '.source_ref' "$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json")"
repository="${{VELNOR_PACKAGE_SOURCE_REPOSITORY:-{REPOSITORY}}}"
passed_source_ref="${{VELNOR_PACKAGE_SOURCE_REF:-unset}}"
{immutable_assertions}
printf 'channel=%s\nasset_tag=%s\nlegacy_tag=%s\nversion=%s\nsource_sha=%s\nrepository=%s\nsource_ref=%s\n' \
  "$VELNOR_PACKAGE_CHANNEL" "${{VELNOR_PACKAGE_ASSET_TAG:-unset}}" "${{VELNOR_PACKAGE_RELEASE_TAG:-unset}}" \
  "$version" "$source_sha" "$repository" "$passed_source_ref" > "$UPDATER_CAPTURE"
mkdir -p Formula
cat > Formula/preview-tool.rb <<EOF
class PreviewTool < Formula
  version "$version"
  url "https://github.com/$repository/releases/download/$selected_tag/{PAYLOAD}"
  resource "checksums" do
    url "https://github.com/$repository/releases/download/$selected_tag/{SUPPORT}"
  end
  # source: $source_sha
end
EOF
"#,
    )
}

fn real_git(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run local Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git fixture output is UTF-8")
        .trim()
        .to_owned()
}

fn commit_unknown_source_change(root: &Path) -> (String, String) {
    real_git(&["init", "--quiet", "--initial-branch=main"], root);
    real_git(&["config", "user.name", "Preview fixture"], root);
    real_git(
        &["config", "user.email", "preview-fixture@example.invalid"],
        root,
    );
    real_git(&["add", "--all"], root);
    real_git(
        &[
            "-c",
            "user.name=Preview fixture",
            "-c",
            "user.email=preview-fixture@example.invalid",
            "commit",
            "--quiet",
            "--message",
            "fixture baseline",
        ],
        root,
    );
    let before = real_git(&["rev-parse", "HEAD"], root);
    fs::write(
        root.join("unclassified-source-input.bin"),
        b"new source input\n",
    )
    .expect("write unclassified source change");
    real_git(&["add", "--all"], root);
    real_git(
        &[
            "-c",
            "user.name=Preview fixture",
            "-c",
            "user.email=preview-fixture@example.invalid",
            "commit",
            "--quiet",
            "--message",
            "fixture source change",
        ],
        root,
    );
    let head = real_git(&["rev-parse", "HEAD"], root);
    (before, head)
}

fn run_release_admission(root: &Path, before: &str, head: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "release-admission",
            "--repository",
            REPOSITORY,
            "--ref",
            "refs/heads/main",
            "--event",
            "push",
            "--before",
        ])
        .arg(before)
        .arg("--head")
        .arg(head)
        .current_dir(root)
        .output()
        .expect("run source-release admission for legacy fixture")
}

fn make_tap_remote(root: &Path, mode: TagMode) -> PathBuf {
    let seed = root.join("tap-seed");
    let remote = root.join("tap-origin.git");
    fs::create_dir_all(&seed).expect("create tap seed");
    real_git(&["init", "--quiet", "--initial-branch=main"], &seed);
    real_git(&["config", "user.name", "Preview fixture"], &seed);
    real_git(
        &["config", "user.email", "preview-fixture@example.invalid"],
        &seed,
    );
    fs::create_dir_all(seed.join("Formula")).expect("create Formula directory");
    fs::create_dir_all(seed.join("scripts")).expect("create scripts directory");
    fs::write(seed.join("README.md"), "tap fixture\n").expect("write tap readme");
    fs::write(
        seed.join("Formula/stable-tool.rb"),
        "class StableTool < Formula\n  version \"9.9.9\"\nend\n",
    )
    .expect("write stable formula sentinel");
    write_executable(
        &seed.join("scripts/package-update.sh"),
        &updater_script(mode),
    );
    real_git(&["add", "--all"], &seed);
    real_git(
        &[
            "-c",
            "user.name=Preview fixture",
            "-c",
            "user.email=preview-fixture@example.invalid",
            "commit",
            "--quiet",
            "--message",
            "tap base",
        ],
        &seed,
    );
    let seed_text = seed.to_string_lossy().into_owned();
    let remote_text = remote.to_string_lossy().into_owned();
    real_git(
        &["clone", "--quiet", "--bare", &seed_text, &remote_text],
        root,
    );
    remote
}

fn fake_pr_gh(root: &Path) -> PathBuf {
    let bin = root.join("fake-bin");
    write_executable(
        &bin.join("gh"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'gh' >> "$GH_LOG"
printf ' <%s>' "$@" >> "$GH_LOG"
printf '\n' >> "$GH_LOG"
case "${1:-} ${2:-}" in
  "pr list") exit 0 ;;
  "pr close") exit 0 ;;
  "pr create") printf 'https://example.invalid/pull/004\n'; exit 0 ;;
  *) echo "unexpected gh command: $*" >&2; exit 90 ;;
esac
"#,
    );
    bin
}

fn fake_failing_gh(root: &Path) -> PathBuf {
    let bin = root.join("failing-gh-bin");
    write_executable(
        &bin.join("gh"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'gh' >> "$GH_LOG"
printf ' <%s>' "$@" >> "$GH_LOG"
printf '\n' >> "$GH_LOG"
if [[ "$*" == *"contents/.package-release-publication-lock.json?ref="* ]]; then
  printf '{"type":"file","path":".package-release-publication-lock.json","sha":"%s"}\n' "$LOCK_SHA"
  exit 0
fi
if [[ "$*" == *"releases/tags/$RELEASE_TAG-$EXPECTED_SOURCE_COMMIT"* ]]; then
  cat "$RELEASE_STATE"
  exit 0
fi
echo 'fixture forces rolling refresh failure' >&2
exit 47
"#,
    );
    bin
}

fn workflow_environment(
    fixture: &Fixture,
    source_sha: &str,
    immutable_tag: &str,
    workspace: &Path,
    step_name: &str,
) -> BTreeMap<String, String> {
    let mut envs = BTreeMap::new();
    let publish = &fixture.workflow["jobs"]["publish"];
    for mapping in [
        publish["env"].as_mapping(),
        env_map(&fixture.workflow, step_name).into(),
    ] {
        let Some(mapping) = mapping else { continue };
        for (key, value) in mapping {
            let Some(value) = value.as_str() else {
                continue;
            };
            let resolved = if value == "${{ steps.publish.outputs.immutable_tag }}" {
                Some(immutable_tag.to_owned())
            } else if value == "${{ needs.attest.outputs.version }}"
                || value == "${{ steps.verify.outputs.version }}"
            {
                Some(package_version(source_sha))
            } else if value == "${{ needs.attest.outputs.source_commit }}"
                || value == "${{ needs.admission.outputs.head_sha }}"
                || value == "${{ steps.verify.outputs.source_commit }}"
            {
                Some(source_sha.to_owned())
            } else if let Some(suffix) = value.strip_prefix("${{ github.workspace }}")
                && (suffix.is_empty() || suffix.starts_with('/'))
            {
                Some(format!("{}{}", workspace.display(), suffix))
            } else if value == "${{ github.repository }}" {
                Some(REPOSITORY.to_owned())
            } else if value == "${{ secrets.TAP_TOKEN }}" || value == "${{ github.token }}" {
                Some("fixture-token".to_owned())
            } else if value.contains("${{") {
                None
            } else {
                Some(value.to_owned())
            };
            if let Some(resolved) = resolved {
                envs.insert(key.to_owned(), resolved);
            }
        }
    }
    envs
}

#[derive(Clone, Copy)]
struct ConsumerUpdateRequest<'a> {
    fixture: &'a Fixture,
    remote: &'a Path,
    run_root: &'a Path,
    publication_root: &'a Path,
    gh_bin: &'a Path,
    source_sha: &'a str,
    immutable_tag: &'a str,
    capture: &'a Path,
}

fn run_consumer_update(request: ConsumerUpdateRequest<'_>) -> String {
    let ConsumerUpdateRequest {
        fixture,
        remote,
        run_root,
        publication_root,
        gh_bin,
        source_sha,
        immutable_tag,
        capture,
    } = request;
    let consumer = run_root.join("consumer");
    fs::create_dir_all(run_root).expect("create consumer run root");
    let workspace = run_root.join("workspace");
    let package = run_published_verification(
        fixture,
        publication_root,
        &workspace,
        source_sha,
        immutable_tag,
        REPOSITORY,
        false,
    );
    let identity =
        run_consumer_identity_check(fixture, &workspace, &package, source_sha, immutable_tag);
    assert!(
        identity.status.success(),
        "consumer identity check rejected the package downloaded and verified for the updater:\n{}{}",
        String::from_utf8_lossy(&identity.stdout),
        String::from_utf8_lossy(&identity.stderr)
    );
    let remote_text = remote.to_string_lossy().into_owned();
    let consumer_text = consumer.to_string_lossy().into_owned();
    real_git(
        &["clone", "--quiet", &remote_text, &consumer_text],
        run_root,
    );
    let output_file = run_root.join("workflow-output");
    let gh_log = run_root.join("gh.log");
    fs::write(&output_file, "").expect("create workflow output file");
    fs::write(&gh_log, "").expect("create gh log");
    let mut envs = workflow_environment(
        fixture,
        source_sha,
        immutable_tag,
        &workspace,
        "Run updater and create or update consumer PR",
    );
    envs.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}",
            gh_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    envs.insert(
        "GITHUB_WORKSPACE".to_owned(),
        workspace.to_string_lossy().into_owned(),
    );
    envs.insert(
        "GITHUB_OUTPUT".to_owned(),
        output_file.to_string_lossy().into_owned(),
    );
    envs.insert("GITHUB_REPOSITORY".to_owned(), REPOSITORY.to_owned());
    envs.insert(
        "UPDATER_CAPTURE".to_owned(),
        capture.to_string_lossy().into_owned(),
    );
    envs.insert("GH_LOG".to_owned(), gh_log.to_string_lossy().into_owned());
    if fixture.mode == TagMode::Immutable {
        assert_eq!(
            envs.get("RELEASE_ASSET_TAG").map(String::as_str),
            Some(immutable_tag),
            "step environment must resolve the publisher's actual immutable tag"
        );
    }
    let script = step_run(
        &fixture.workflow,
        "Run updater and create or update consumer PR",
    );
    let output = run_bash(script, run_root, &envs.into_iter().collect::<Vec<_>>());
    assert!(
        output.status.success(),
        "generated updater step failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(output_file)
            .expect("read workflow output")
            .contains("pr_url=https://example.invalid/pull/004"),
        "the generated step should create a source-specific consumer PR"
    );
    assert!(package.join("release-manifest.json").is_file());
    fs::read_to_string(capture).expect("read observed updater environment")
}

fn run_consumer_identity_check(
    fixture: &Fixture,
    root: &Path,
    package: &Path,
    source_sha: &str,
    immutable_tag: &str,
) -> Output {
    fs::create_dir_all(root).expect("create identity-check root");
    assert!(package.join("identity.json").is_file());
    let mut envs = workflow_environment(
        fixture,
        source_sha,
        immutable_tag,
        root,
        "Verify immutable consumer package identity",
    );
    envs.insert("PATH".to_owned(), std::env::var("PATH").unwrap_or_default());
    envs.insert(
        "GITHUB_WORKSPACE".to_owned(),
        root.to_string_lossy().into_owned(),
    );
    envs.insert("GITHUB_REPOSITORY".to_owned(), REPOSITORY.to_owned());
    run_bash(
        step_run(
            &fixture.workflow,
            "Verify immutable consumer package identity",
        ),
        root,
        &envs.into_iter().collect::<Vec<_>>(),
    )
}

fn assert_published_verification_gates_consumer_pr(workflow: &Value) {
    let steps = publish_steps(workflow);
    let verification_names = [
        "Download and re-verify published release",
        "Run published package verification tasks",
        "Verify published release attestations",
        "Verify immutable consumer package identity",
        "Checkout consumer repository",
        "Run updater and create or update consumer PR",
    ];
    let indices = verification_names
        .iter()
        .map(|name| {
            steps
                .iter()
                .position(|step| step["name"].as_str() == Some(name))
                .expect("missing verification gate step")
        })
        .collect::<Vec<_>>();
    assert!(
        indices.windows(2).all(|pair| pair[0] < pair[1]),
        "published asset, package, and attestation checks must precede the consumer PR"
    );
    for index in indices.iter().copied() {
        let step = &steps[index];
        assert_ne!(
            value_field(step, "continue-on-error").and_then(Value::as_bool),
            Some(true),
            "failure in an earlier publication check must block consumer work: {}",
            step["name"]
        );
        if let Some(condition) = value_field(step, "if").and_then(Value::as_str) {
            assert!(
                !condition.contains("always()")
                    && !condition.contains("failure()")
                    && !condition.contains("cancelled()"),
                "consumer gate overrides failure-skipping behavior: {} ({condition})",
                step["name"]
            );
        }
    }
    let download = step_run(workflow, verification_names[0]);
    assert!(
        download.contains("gh release download") && download.contains("RELEASE_ASSET_TAG"),
        "published package bytes are not downloaded by the verification step: {download}"
    );
    let attestations = step_run(workflow, verification_names[2]);
    assert!(
        attestations.contains("gh attestation verify"),
        "published release attestation verification is missing: {attestations}"
    );
    let identity_step = step_run(workflow, verification_names[3]);
    assert!(
        identity_step.contains("VELNOR_PACKAGE_ASSET_TAG")
            && identity_step.contains("VELNOR_PACKAGE_SOURCE_COMMIT")
            && identity_step.contains("release-manifest.json"),
        "consumer identity gate must bind the source tag to the verified manifest: {identity_step}"
    );
    let identity_env = env_map(workflow, verification_names[3]);
    assert!(
        mapping_string(identity_env, "VELNOR_PACKAGE_ASSET_TAG")
            .is_some_and(|value| value.contains("steps.publish.outputs.immutable_tag")),
        "identity gate tag must come from immutable publisher output"
    );
    assert!(
        mapping_string(identity_env, "VELNOR_PACKAGE_SOURCE_COMMIT")
            .is_some_and(|value| value.contains("steps.verify.outputs.source_commit")),
        "identity gate source commit must come from package verification output"
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the coordinated fake tool protocols together as one verifier fixture."
)]
fn fake_publication_tools(root: &Path) -> PathBuf {
    let bin = root.join("publication-bin");
    write_executable(
        &bin.join("sha256sum"),
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--check" ]]; then
  shift
  [[ "${1:-}" == "--strict" ]] && shift
  exec shasum -a 256 -c "$@"
fi
if [[ "${1:-}" == "--" ]]; then shift; fi
exec shasum -a 256 -- "$@"
"#,
    );
    write_executable(
        &bin.join("git"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'git' >> "$GIT_LOG"
printf ' <%s>' "$@" >> "$GIT_LOG"
printf '\n' >> "$GIT_LOG"
checkout=""
previous=""
for arg in "$@"; do
  if [[ "$previous" == "-C" ]]; then checkout="$arg"; fi
  previous="$arg"
done
assert_verified_checkout() {
  [[ -n "$checkout" && "$checkout" == "$VELNOR_SOURCE_CHECKOUT_DIR" ]] || {
    echo "git source checkout path differs from emitted source checkout" >&2
    exit 94
  }
  [[ -d "$checkout" && -s "$checkout/.source-commit" ]] || {
    echo "git source checkout fixture lacks its source SHA marker" >&2
    exit 95
  }
  [[ -s "$checkout/.origin-url" ]] || {
    echo "git source checkout fixture lacks its origin marker" >&2
    exit 98
  }
  manifest="$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json"
  manifest_sha="$(jq -er '.source_commit' "$manifest")"
  checkout_sha="$(cat "$checkout/.source-commit")"
  [[ "$manifest_sha" == "$EXPECTED_SOURCE_COMMIT" && "$checkout_sha" == "$manifest_sha" ]] || {
    echo "git source checkout SHA does not match verified manifest" >&2
    exit 96
  }
  [[ "$RELEASE_ASSET_TAG" == "$RELEASE_TAG-$manifest_sha" ]] || {
    echo "git source checkout tag does not bind to verified manifest" >&2
    exit 97
  }
  origin_url="$(cat "$checkout/.origin-url")"
}
case " $* " in
  *" rev-parse HEAD "*) assert_verified_checkout; printf '%s\n' "$manifest_sha"; exit 0 ;;
  *" remote get-url origin "*) assert_verified_checkout; printf '%s\n' "$origin_url"; exit 0 ;;
  *" status --porcelain=v1 --untracked-files=all "*) assert_verified_checkout; exit 0 ;;
  *" ls-files --others "*) assert_verified_checkout; exit 0 ;;
  *" ls-remote "*) last=""; for arg in "$@"; do last="$arg"; done; printf '%s\t%s\n' "$TAG_SHA" "$last"; exit 0 ;;
  *) echo "unexpected fake git command: $*" >&2; exit 91 ;;
esac
"#,
    );
    write_executable(
        &bin.join("gh"),
        r#"#!/usr/bin/env bash
set -euo pipefail
printf 'gh' >> "$GH_LOG"
printf ' <%s>' "$@" >> "$GH_LOG"
printf '\n' >> "$GH_LOG"
if [[ "${1:-}" == "api" ]]; then
  if [[ " $* " == *" --method DELETE "* ]]; then exit 0; fi
  if [[ " $* " == *" --method PATCH "* ]]; then
    jq '.draft = false' "$RELEASE_STATE" > "$RELEASE_STATE.next"
    mv "$RELEASE_STATE.next" "$RELEASE_STATE"
    exit 0
  fi
  endpoint=""
  for arg in "$@"; do endpoint="$arg"; done
  case "$endpoint" in
    *"/contents/.package-release-publication-lock.json?ref="*)
      printf '{"type":"file","path":".package-release-publication-lock.json","sha":"%s"}\n' "$LOCK_SHA"
      exit 0
      ;;
    *"/releases/tags/"*)
      if [[ " $* " == *" -i "* ]]; then
        printf 'HTTP/1.1 200 OK\n\n'
        cat "$RELEASE_STATE"
      else
        cat "$RELEASE_STATE"
      fi
      exit 0
      ;;
  esac
fi
if [[ "${1:-}" == "attestation" && "${2:-}" == "verify" ]]; then
  asset_path="${3:-}"
  [[ -s "$asset_path" ]] || { echo "attestation subject is missing: $asset_path" >&2; exit 98; }
  manifest="$PACKAGE_DIR/release-manifest.json"
  manifest_sha="$(jq -er '.source_commit' "$manifest")"
  [[ "$manifest_sha" == "$EXPECTED_SOURCE_COMMIT" ]] || {
    echo "attestation package source differs from expected source" >&2
    exit 99
  }
  [[ "$RELEASE_ASSET_TAG" == "$RELEASE_TAG-$manifest_sha" ]] || {
    echo "attestation release tag differs from package source" >&2
    exit 100
  }
  digest="$(shasum -a 256 "$asset_path" | awk '{print $1}')"
  printf '%s %s %s\n' "$RELEASE_ASSET_TAG" "$(basename "$asset_path")" "$digest" >> "$ATTESTATION_LOG"
  exit 0
fi
if [[ "${1:-}" == "release" && "${2:-}" == "download" ]]; then
  requested_tag="${3:-}"
  state_tag="$(jq -er '.tag_name' "$RELEASE_STATE")"
  [[ "$requested_tag" == "$state_tag" ]] || {
    echo "download tag does not match release state" >&2
    exit 101
  }
  target=""
  while (($#)); do
    if [[ "$1" == "--dir" ]]; then target="$2"; shift 2; else shift; fi
  done
  [[ -n "$target" ]] || { echo 'missing --dir' >&2; exit 92; }
  mkdir -p "$target"
  while IFS= read -r name; do
    cp "$RELEASE_ASSET_DIR/$name" "$target/$name"
  done < <(jq -r '.assets[].name' "$RELEASE_STATE")
  exit 0
fi
if [[ "${1:-}" == "release" && "${2:-}" == "upload" ]]; then
  asset_path=""
  for arg in "$@"; do asset_path="$arg"; done
  asset_name="$(basename "$asset_path")"
  [[ -s "$asset_path" ]] || { echo "upload source is missing: $asset_path" >&2; exit 102; }
  if [[ "$ASSET_MODE" == "upload-mismatch" ]]; then
    printf 'corrupt bytes uploaded under declared name\n' > "$RELEASE_ASSET_DIR/$asset_name"
  else
    cp "$asset_path" "$RELEASE_ASSET_DIR/$asset_name"
  fi
  jq --arg name "$asset_name" '.assets += [{"name":$name}]' "$RELEASE_STATE" > "$RELEASE_STATE.next"
  mv "$RELEASE_STATE.next" "$RELEASE_STATE"
  exit 0
fi
echo "unexpected fake gh command: $*" >&2
exit 93
"#,
    );
    write_executable(
        &bin.join("mise"),
        r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 2 && "$1" == "run" && "$2" == "verify-release" ]] || {
  echo "unexpected fixture task invocation: $*" >&2
  exit 103
}
[[ -f mise.toml ]] && grep -Fq 'run = "./scripts/verify-package.sh"' mise.toml || {
  echo "fixture task definition does not match the executed verifier" >&2
  exit 104
}
echo "$*" >> "$MISE_LOG"
exec ./scripts/verify-package.sh
"#,
    );
    write_executable(
        &bin.join("find"),
        r#"#!/usr/bin/env bash
set -euo pipefail
directory="$1"
if [[ " $* " == *" -print -quit "* ]]; then
  for path in "$directory"/* "$directory"/.[!.]*; do
    [[ -e "$path" ]] || continue
    [[ -f "$path" ]] || { printf '%s\n' "$path"; exit 0; }
  done
  exit 0
fi
for path in "$directory"/*; do
  [[ -f "$path" ]] && basename "$path"
done | sort
"#,
    );
    bin
}

fn run_published_verification(
    fixture: &Fixture,
    publication_root: &Path,
    run_root: &Path,
    source_sha: &str,
    immutable_tag: &str,
    checkout_repository: &str,
    expect_origin_mismatch: bool,
) -> PathBuf {
    run_published_verification_controlled(PublishedVerificationRequest {
        fixture,
        publication_root,
        run_root,
        source_sha,
        immutable_tag,
        checkout_repository,
        checkout_sha: source_sha,
        expected_failure: expect_origin_mismatch
            .then_some("source checkout repository does not match the declared repository"),
    })
}

#[derive(Clone, Copy)]
struct PublishedVerificationRequest<'a> {
    fixture: &'a Fixture,
    publication_root: &'a Path,
    run_root: &'a Path,
    source_sha: &'a str,
    immutable_tag: &'a str,
    checkout_repository: &'a str,
    checkout_sha: &'a str,
    expected_failure: Option<&'a str>,
}

#[expect(
    clippy::too_many_lines,
    reason = "Exercise every emitted published-verification stage with one shared handoff."
)]
fn run_published_verification_controlled(request: PublishedVerificationRequest<'_>) -> PathBuf {
    let PublishedVerificationRequest {
        fixture,
        publication_root,
        run_root,
        source_sha,
        immutable_tag,
        checkout_repository,
        checkout_sha,
        expected_failure,
    } = request;
    fs::create_dir_all(run_root).expect("create published-verification workspace");
    let bin = fake_publication_tools(run_root);
    let gh_log = run_root.join("gh-verification.log");
    let git_log = run_root.join("git-verification.log");
    let mise_log = run_root.join("mise-verification.log");
    let attestation_log = run_root.join("attestation-verification.log");
    let output_file = run_root.join("verification-output");
    fs::write(&gh_log, "").expect("create verification GitHub log");
    fs::write(&git_log, "").expect("create verification Git log");
    fs::write(&mise_log, "").expect("create task-runner log");
    fs::write(&attestation_log, "").expect("create attestation log");
    fs::write(&output_file, "").expect("create verification output");
    let release_state = publication_root.join("release-state.json");
    let package_dir = publication_root.join("package");
    let released_assets = publication_root.join("released-assets");
    assert!(
        release_state.is_file(),
        "publisher release state is missing"
    );
    assert!(package_dir.is_dir(), "publisher package bytes are missing");
    assert!(
        released_assets.is_dir(),
        "published asset byte store is missing"
    );

    let mut envs = workflow_environment(
        fixture,
        source_sha,
        immutable_tag,
        run_root,
        "Download and re-verify published release",
    );
    let publish_env = fixture.workflow["jobs"]["publish"]["env"]
        .as_mapping()
        .expect("rendered publish job environment");
    let download_env = env_map(
        &fixture.workflow,
        "Download and re-verify published release",
    );
    let source_checkout_step = step_named(
        &fixture.workflow,
        "Checkout verified source for publication",
    );
    let source_checkout_with = value_field(source_checkout_step, "with")
        .and_then(Value::as_mapping)
        .expect("publication source checkout inputs");
    assert_eq!(
        mapping_string(source_checkout_with, "ref"),
        Some("${{ needs.attest.outputs.source_commit }}"),
        "publication checkout action must pin the verified build source commit"
    );
    let release_tag_prefix = emitted_release_tag_prefix(fixture);
    for (key, expected) in [
        (
            "EXPECTED_SOURCE_COMMIT",
            "${{ needs.attest.outputs.source_commit }}",
        ),
        ("EXPECTED_SOURCE_REPOSITORY", REPOSITORY),
        ("EXPECTED_SOURCE_REF", "refs/heads/main"),
        ("EXPECTED_MANIFEST_SCHEMA", "example.preview-manifest-v1"),
        ("VELNOR_PACKAGE_CHANNEL", CHANNEL),
        ("RELEASE_TAG", release_tag_prefix.as_str()),
    ] {
        assert_eq!(
            mapping_string(publish_env, key),
            Some(expected),
            "rendered publish environment must preserve the exact {key} source binding"
        );
    }
    assert_eq!(
        mapping_string(download_env, "RELEASE_ASSET_TAG"),
        Some("${{ steps.publish.outputs.immutable_tag }}"),
        "published verifier tag must come directly from the immutable publisher output"
    );
    assert_eq!(
        mapping_string(publish_env, "VELNOR_SOURCE_CHECKOUT_DIR"),
        Some("${{ github.workspace }}/source"),
        "generated source checkout path must bind to this workflow workspace"
    );
    envs.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    envs.insert("GH_TOKEN".to_owned(), "fixture-token".to_owned());
    envs.insert(
        "GITHUB_WORKSPACE".to_owned(),
        run_root.to_string_lossy().into_owned(),
    );
    envs.insert("GITHUB_REPOSITORY".to_owned(), REPOSITORY.to_owned());
    let release_tag_prefix = emitted_release_tag_prefix(fixture);
    for (name, expected) in [
        ("EXPECTED_SOURCE_COMMIT", source_sha),
        ("EXPECTED_SOURCE_REPOSITORY", REPOSITORY),
        ("EXPECTED_SOURCE_REF", "refs/heads/main"),
        ("EXPECTED_MANIFEST_SCHEMA", "example.preview-manifest-v1"),
        ("VELNOR_PACKAGE_CHANNEL", CHANNEL),
        ("RELEASE_TAG", release_tag_prefix.as_str()),
        ("RELEASE_ASSET_TAG", immutable_tag),
    ] {
        assert_eq!(
            envs.get(name).map(String::as_str),
            Some(expected),
            "published verifier {name} must resolve from its rendered workflow binding"
        );
    }
    assert_eq!(
        immutable_tag,
        format!("{release_tag_prefix}-{source_sha}"),
        "published verifier tag must bind its rendered release prefix to the admitted source"
    );
    let source_checkout = PathBuf::from(
        envs.get("VELNOR_SOURCE_CHECKOUT_DIR")
            .expect("rendered source checkout binding"),
    );
    assert_eq!(
        source_checkout,
        run_root.join("source"),
        "source checkout path must resolve from rendered github.workspace binding"
    );
    fs::create_dir_all(&source_checkout).expect("create exact source checkout fixture");
    fs::write(source_checkout.join(".source-commit"), checkout_sha)
        .expect("bind source checkout fixture to manifest commit");
    fs::write(
        source_checkout.join(".origin-url"),
        format!("https://github.com/{checkout_repository}.git"),
    )
    .expect("bind source checkout fixture to declared repository");
    fs::write(
        source_checkout.join("mise.toml"),
        "[tasks.verify-release]\nrun = \"./scripts/verify-package.sh\"\n",
    )
    .expect("write checked-out package verification task");
    write_executable(
        &source_checkout.join("scripts/verify-package.sh"),
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
dir="${{VELNOR_VERIFIED_PACKAGE_DIR:?missing verified package directory}}"
manifest="$dir/release-manifest.json"
identity="$dir/identity.json"
source_sha="$(jq -er '.source_commit' "$manifest")"
[[ "$source_sha" == "$EXPECTED_SOURCE_COMMIT" ]]
[[ "$(jq -er '.source_repository' "$manifest")" == "$EXPECTED_SOURCE_REPOSITORY" ]]
[[ "$(jq -er '.source_ref' "$manifest")" == "$EXPECTED_SOURCE_REF" ]]
[[ "$(jq -er '.schema' "$manifest")" == "$EXPECTED_MANIFEST_SCHEMA" ]]
[[ "$(jq -er '.version' "$manifest")" == "{expected_version}" ]]
[[ "$(jq -er '.source_digest' "$identity")" == "$source_sha" ]]
[[ "$(jq -er '.manifest.source_commit' "$identity")" == "$source_sha" ]]
for name in "{PAYLOAD}" "{SUPPORT}"; do
  if [[ "$name" == "{PAYLOAD}" ]]; then
    expected="$(jq -er --arg name "$name" '.assets[] | select(.name == $name) | .sha256' "$manifest")"
  else
    expected="$(jq -er --arg name "$name" '.supporting_assets[] | select(.name == $name) | .sha256' "$manifest")"
  fi
  actual="$(sha256sum -- "$dir/$name" | awk '{{print $1}}')"
  [[ "$actual" == "$expected" ]]
done
(cd "$dir" && sha256sum --check --strict SHA256SUMS) >/dev/null
"#,
            expected_version = package_version(source_sha)
        ),
    );
    envs.insert(
        "PACKAGE_DIR".to_owned(),
        package_dir.to_string_lossy().into_owned(),
    );
    envs.insert(
        "RELEASE_STATE".to_owned(),
        release_state.to_string_lossy().into_owned(),
    );
    envs.insert("ASSET_MODE".to_owned(), "exact".to_owned());
    envs.insert("TAG_SHA".to_owned(), source_sha.to_owned());
    envs.insert("LOCK_SHA".to_owned(), LOCK_SHA.to_owned());
    envs.insert(
        "RELEASE_ASSET_DIR".to_owned(),
        released_assets.to_string_lossy().into_owned(),
    );
    envs.insert("GH_LOG".to_owned(), gh_log.to_string_lossy().into_owned());
    envs.insert("GIT_LOG".to_owned(), git_log.to_string_lossy().into_owned());
    envs.insert(
        "MISE_LOG".to_owned(),
        mise_log.to_string_lossy().into_owned(),
    );
    envs.insert(
        "ATTESTATION_LOG".to_owned(),
        attestation_log.to_string_lossy().into_owned(),
    );
    envs.insert(
        "GITHUB_OUTPUT".to_owned(),
        output_file.to_string_lossy().into_owned(),
    );

    let output = run_bash(
        step_run(
            &fixture.workflow,
            "Download and re-verify published release",
        ),
        run_root,
        &envs.into_iter().collect::<Vec<_>>(),
    );
    if let Some(expected_failure) = expected_failure {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.status.success(),
            "emitted verifier accepted invalid source checkout binding"
        );
        assert!(
            combined.contains(expected_failure),
            "emitted verifier rejected the invalid binding for an unrelated reason; expected {expected_failure:?}, got {combined}"
        );
        return run_root.join("published-package");
    }
    assert!(
        output.status.success(),
        "generated published-package verifier failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let verification_log = fs::read_to_string(&gh_log).expect("read verifier GitHub log");
    assert!(
        verification_log.contains(&format!("release> <download> <{immutable_tag}")),
        "emitted verifier did not download the expected immutable tag: {verification_log}"
    );
    let git_commands = fs::read_to_string(&git_log).expect("read verifier Git log");
    assert!(
        git_commands.contains("rev-parse> <HEAD")
            && git_commands.contains("remote> <get-url> <origin"),
        "verifier did not bind source checkout commit and origin: {git_commands}"
    );
    let published = run_root.join("published-package");
    for name in ["release-manifest.json", "identity.json", PAYLOAD, SUPPORT] {
        assert!(
            published.join(name).is_file(),
            "verified handoff lacks {name}"
        );
    }

    let task_step = "Run published package verification tasks";
    let mut task_envs =
        workflow_environment(fixture, source_sha, immutable_tag, run_root, task_step);
    let expected_package_dir = published.to_string_lossy().into_owned();
    assert_eq!(
        task_envs.get("VELNOR_VERIFIED_PACKAGE_DIR"),
        Some(&expected_package_dir),
        "published verification task must receive the downloaded package handoff"
    );
    task_envs.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    task_envs.insert(
        "GITHUB_WORKSPACE".to_owned(),
        run_root.to_string_lossy().into_owned(),
    );
    task_envs.insert("GITHUB_REPOSITORY".to_owned(), REPOSITORY.to_owned());
    task_envs.insert(
        "MISE_LOG".to_owned(),
        mise_log.to_string_lossy().into_owned(),
    );
    let task_output = run_bash(
        step_run(&fixture.workflow, task_step),
        run_root,
        &task_envs.into_iter().collect::<Vec<_>>(),
    );
    assert!(
        task_output.status.success(),
        "emitted published verification task failed:\n{}{}",
        String::from_utf8_lossy(&task_output.stdout),
        String::from_utf8_lossy(&task_output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&mise_log).expect("read emitted task invocation"),
        "run verify-release\n",
        "generated published verification step did not execute its declared task"
    );

    let attestation_step = "Verify published release attestations";
    let mut attestation_envs = workflow_environment(
        fixture,
        source_sha,
        immutable_tag,
        run_root,
        attestation_step,
    );
    assert_eq!(
        attestation_envs.get("PACKAGE_DIR").map(String::as_str),
        Some("published-package"),
        "attestation step must inspect the downloaded package directory"
    );
    assert_eq!(
        attestation_envs
            .get("RELEASE_ASSET_TAG")
            .map(String::as_str),
        Some(immutable_tag),
        "attestation step must verify the actual immutable publisher tag"
    );
    attestation_envs.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    attestation_envs.insert(
        "GITHUB_WORKSPACE".to_owned(),
        run_root.to_string_lossy().into_owned(),
    );
    attestation_envs.insert("GITHUB_REPOSITORY".to_owned(), REPOSITORY.to_owned());
    attestation_envs.insert("GH_LOG".to_owned(), gh_log.to_string_lossy().into_owned());
    attestation_envs.insert(
        "ATTESTATION_LOG".to_owned(),
        attestation_log.to_string_lossy().into_owned(),
    );
    let attestation_output = run_bash(
        step_run(&fixture.workflow, attestation_step),
        run_root,
        &attestation_envs.into_iter().collect::<Vec<_>>(),
    );
    assert!(
        attestation_output.status.success(),
        "emitted published attestation step failed:\n{}{}",
        String::from_utf8_lossy(&attestation_output.stdout),
        String::from_utf8_lossy(&attestation_output.stderr)
    );
    let attested = fs::read_to_string(&attestation_log).expect("read attestation subject log");
    for name in [PAYLOAD, SUPPORT, "release-manifest.json", "identity.json"] {
        assert!(
            attested
                .lines()
                .any(|line| { line.starts_with(&format!("{immutable_tag} {name} ")) }),
            "emitted attestation step did not inspect {name} under {immutable_tag}: {attested}"
        );
    }
    published
}

fn release_json(release_tag_prefix: &str, source_sha: &str, names: &[&str]) -> String {
    let assets = names
        .iter()
        .map(|name| format!("{{\"name\":\"{name}\"}}"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"id\":404,\"tag_name\":\"{release_tag_prefix}-{source_sha}\",\"name\":\"Preview {}\",\"prerelease\":true,\"draft\":false,\"assets\":[{assets}]}}",
        package_version(source_sha)
    )
}

fn draft_release_json(release_tag_prefix: &str, source_sha: &str, names: &[&str]) -> String {
    release_json(release_tag_prefix, source_sha, names).replace("\"draft\":false", "\"draft\":true")
}

fn emitted_release_tag_prefix(fixture: &Fixture) -> String {
    let job_env = fixture.workflow["jobs"]["publish"]["env"]
        .as_mapping()
        .expect("publish job environment mapping");
    let release_tag = mapping_string(job_env, "RELEASE_TAG").expect("rendered release tag prefix");
    assert_eq!(
        release_tag, RELEASE_TAG_PREFIX,
        "generated release tag prefix must preserve the configured legacy alias"
    );
    release_tag.to_owned()
}

fn run_publication_step(
    fixture: &Fixture,
    root: &Path,
    source_sha: &str,
    asset_mode: &str,
    tag_sha: &str,
    release_body: &str,
) -> PublicationRun {
    let bin = fake_publication_tools(root);
    let publisher_source_sha = emitted_publisher_source_sha(fixture, source_sha);
    let package = write_verified_package(root, &publisher_source_sha, "package");
    let released_assets = root.join("released-assets");
    fs::create_dir_all(&released_assets).expect("create fake public release asset storage");
    for name in ["release-manifest.json", "identity.json", PAYLOAD, SUPPORT] {
        if release_body.contains(&format!("\"name\":\"{name}\"")) {
            fs::copy(package.join(name), released_assets.join(name))
                .expect("seed existing fake release asset bytes");
        }
    }
    if asset_mode == "digest-mismatch" {
        fs::write(released_assets.join(PAYLOAD), b"changed public byte\n")
            .expect("corrupt existing public asset fixture");
    }
    let gh_log = root.join("gh-publication.log");
    let git_log = root.join("git-publication.log");
    let github_env = root.join("github-env");
    let github_output = root.join("github-output");
    let release_state = root.join("release-state.json");
    fs::write(&gh_log, "").expect("create publication gh log");
    fs::write(&git_log, "").expect("create publication git log");
    fs::write(&github_env, "").expect("create GitHub env file");
    fs::write(&github_output, "").expect("create GitHub output file");
    fs::write(&release_state, release_body).expect("create fake release state");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let envs = vec![
        ("PATH".to_owned(), path),
        (
            "PACKAGE_DIR".to_owned(),
            package.to_string_lossy().into_owned(),
        ),
        ("EXPECTED_SOURCE_COMMIT".to_owned(), publisher_source_sha),
        ("GITHUB_REPOSITORY".to_owned(), REPOSITORY.to_owned()),
        (
            "GITHUB_ENV".to_owned(),
            github_env.to_string_lossy().into_owned(),
        ),
        (
            "GITHUB_OUTPUT".to_owned(),
            github_output.to_string_lossy().into_owned(),
        ),
        ("GITHUB_WORKFLOW".to_owned(), "fixture workflow".to_owned()),
        ("GITHUB_RUN_ID".to_owned(), "4004".to_owned()),
        ("GITHUB_RUN_ATTEMPT".to_owned(), "1".to_owned()),
        (
            "VELNOR_PUBLICATION_LOCK_BRANCH".to_owned(),
            "preview-publication-lock".to_owned(),
        ),
        (
            "VELNOR_PUBLICATION_LOCK_SHA".to_owned(),
            LOCK_SHA.to_owned(),
        ),
        (
            "VELNOR_PUBLICATION_LOCK_RELEASED".to_owned(),
            "0".to_owned(),
        ),
        ("VELNOR_PUBLICATION_LOCK_RETAIN".to_owned(), "0".to_owned()),
        (
            "RELEASE_TAG".to_owned(),
            emitted_release_tag_prefix(fixture),
        ),
        ("RELEASE_PRERELEASE".to_owned(), "true".to_owned()),
        ("RELEASE_TITLE_PREFIX".to_owned(), "Preview".to_owned()),
        ("GH_TOKEN".to_owned(), "fixture-token".to_owned()),
        ("TAG_SHA".to_owned(), tag_sha.to_owned()),
        ("LOCK_SHA".to_owned(), LOCK_SHA.to_owned()),
        ("GH_LOG".to_owned(), gh_log.to_string_lossy().into_owned()),
        ("GIT_LOG".to_owned(), git_log.to_string_lossy().into_owned()),
        ("ASSET_MODE".to_owned(), asset_mode.to_owned()),
        (
            "RELEASE_ASSET_DIR".to_owned(),
            released_assets.to_string_lossy().into_owned(),
        ),
        (
            "RELEASE_STATE".to_owned(),
            release_state.to_string_lossy().into_owned(),
        ),
    ];
    let script = step_run(&fixture.workflow, "Publish immutable source-bound release");
    let output = run_bash(script, &fixture.root, &envs);
    PublicationRun {
        output,
        gh_log: fs::read_to_string(&gh_log).expect("read fake GitHub API log"),
        git_log: fs::read_to_string(&git_log).expect("read fake Git log"),
        workflow_output: fs::read_to_string(&github_output)
            .expect("read immutable publication workflow output"),
        release_state: fs::read_to_string(release_state).expect("read final fake release state"),
    }
}

fn emitted_publisher_source_sha(fixture: &Fixture, event_source_sha: &str) -> String {
    let publish_job = &fixture.workflow["jobs"]["publish"];
    let job_env = publish_job["env"]
        .as_mapping()
        .expect("publish job environment mapping");
    let source_binding = mapping_string(job_env, "EXPECTED_SOURCE_COMMIT")
        .expect("publisher's admitted source SHA binding");
    assert!(
        source_binding.contains("needs.attest.outputs.source_commit"),
        "publisher source SHA must resolve from the verified package output: {source_binding}"
    );
    event_source_sha.to_owned()
}

fn immutable_tag_from(run: &PublicationRun, release_tag_prefix: &str, source_sha: &str) -> String {
    let expected = format!("{release_tag_prefix}-{source_sha}");
    let expected_output = format!("immutable_tag={expected}");
    assert!(
        run.output.status.success(),
        "immutable publication failed:\n{}{}",
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr)
    );
    assert!(
        run.workflow_output
            .lines()
            .any(|line| line == expected_output.as_str()),
        "publisher did not emit the source-bound tag: {}",
        run.workflow_output
    );
    assert!(
        run.git_log.contains(&format!("refs/tags/{expected}")),
        "publisher did not verify the exact immutable tag: {}",
        run.git_log
    );
    expected
}

fn publish_fixture_candidate(fixture: &Fixture, label: &str, source_sha: &str) -> String {
    let release_tag_prefix = emitted_release_tag_prefix(fixture);
    let root = fixture.root.join(label);
    fs::create_dir_all(&root).expect("create candidate publication run");
    let assets = ["release-manifest.json", "identity.json", PAYLOAD, SUPPORT];
    let run = run_publication_step(
        fixture,
        &root,
        source_sha,
        "exact",
        source_sha,
        &release_json(&release_tag_prefix, source_sha, &assets),
    );
    immutable_tag_from(&run, &release_tag_prefix, source_sha)
}

fn run_fake_git_source_probe(
    root: &Path,
    manifest_sha: &str,
    checkout_sha: &str,
    checkout_repository: &str,
    release_tag_prefix: &str,
    asset_tag_sha: &str,
) -> Output {
    fs::create_dir_all(root).expect("create fake checkout probe root");
    let bin = fake_publication_tools(root);
    let checkout = root.join("source");
    fs::create_dir_all(&checkout).expect("create fake source checkout");
    fs::write(checkout.join(".source-commit"), checkout_sha)
        .expect("write fake checkout commit marker");
    fs::write(
        checkout.join(".origin-url"),
        format!("https://github.com/{checkout_repository}.git"),
    )
    .expect("write fake checkout origin marker");
    let package = write_verified_package(root, manifest_sha, "verified-package");
    let git_log = root.join("git-probe.log");
    fs::write(&git_log, "").expect("create fake source probe log");
    Command::new(bin.join("git"))
        .args([
            "-C",
            checkout.to_str().expect("UTF-8 checkout path"),
            "rev-parse",
            "HEAD",
        ])
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("GIT_LOG", &git_log)
        .env("VELNOR_SOURCE_CHECKOUT_DIR", &checkout)
        .env("VELNOR_VERIFIED_PACKAGE_DIR", &package)
        .env("EXPECTED_SOURCE_COMMIT", manifest_sha)
        .env("EXPECTED_SOURCE_REPOSITORY", REPOSITORY)
        .env("RELEASE_TAG", release_tag_prefix)
        .env(
            "RELEASE_ASSET_TAG",
            format!("{release_tag_prefix}-{asset_tag_sha}"),
        )
        .output()
        .expect("run fake Git source identity probe")
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep this required end-to-end updater case and all its identity assertions together."
)]
fn immutable_tag_reaches_updater() {
    let fixture = generate_fixture("immutable-updater", TagMode::Immutable, false);
    assert_published_verification_gates_consumer_pr(&fixture.workflow);
    let env = env_map(
        &fixture.workflow,
        "Run updater and create or update consumer PR",
    );
    let emitted_release_tag =
        mapping_string(env, "RELEASE_ASSET_TAG").expect("immutable release output binding");
    assert!(
        emitted_release_tag.contains("steps.publish.outputs.immutable_tag"),
        "consumer release tag is not bound to publisher output: {emitted_release_tag}"
    );
    let updater_step = step_run(
        &fixture.workflow,
        "Run updater and create or update consumer PR",
    );
    let updater_env = env_map(
        &fixture.workflow,
        "Run updater and create or update consumer PR",
    );
    assert!(
        mapping_string(updater_env, "VELNOR_PACKAGE_ASSET_TAG")
            .is_some_and(|value| value.contains("steps.publish.outputs.immutable_tag"))
            || updater_step.contains("VELNOR_PACKAGE_ASSET_TAG=\"$RELEASE_ASSET_TAG\""),
        "immutable updater must receive VELNOR_PACKAGE_ASSET_TAG from RELEASE_ASSET_TAG"
    );
    for (name, expression) in [
        ("VELNOR_PACKAGE_VERSION", "steps.verify.outputs.version"),
        (
            "VELNOR_PACKAGE_SOURCE_COMMIT",
            "steps.verify.outputs.source_commit",
        ),
    ] {
        assert!(
            mapping_string(updater_env, name).is_some_and(|value| value.contains(expression)),
            "immutable updater {name} must come from verified package output"
        );
    }
    assert_eq!(
        mapping_string(updater_env, "VELNOR_PACKAGE_SOURCE_REPOSITORY"),
        Some(REPOSITORY),
        "immutable updater source repository must match the package declaration"
    );
    assert_eq!(
        mapping_string(updater_env, "VELNOR_PACKAGE_SOURCE_REF"),
        Some("refs/heads/main"),
        "immutable updater source ref must match the package declaration"
    );
    assert!(
        mapping_string(updater_env, "VELNOR_VERIFIED_PACKAGE_DIR").is_some_and(|value| value
            .contains("github.workspace")
            && value.ends_with("/published-package")),
        "immutable updater must receive the verified published package directory"
    );
    assert_eq!(
        mapping_string(updater_env, "VELNOR_PACKAGE_RELEASE_TAG"),
        None,
        "immutable updater must not receive legacy rolling-tag alias"
    );

    let sha = source_sha('a');
    let tag = publish_fixture_candidate(&fixture, "publisher-run", &sha);
    let publisher_root = fixture.root.join("publisher-run");
    let verifier_root = fixture.root.join("identity-check-run");
    let verified_package = run_published_verification(
        &fixture,
        &publisher_root,
        &verifier_root,
        &sha,
        &tag,
        REPOSITORY,
        false,
    );
    let identity_check =
        run_consumer_identity_check(&fixture, &verifier_root, &verified_package, &sha, &tag);
    assert!(
        identity_check.status.success(),
        "generated package identity gate rejected verified output:\n{}{}",
        String::from_utf8_lossy(&identity_check.stdout),
        String::from_utf8_lossy(&identity_check.stderr)
    );
    let remote = make_tap_remote(&fixture.root, TagMode::Immutable);
    let gh_bin = fake_pr_gh(&fixture.root);
    let capture = fixture.root.join("updater-env.log");
    let observed = run_consumer_update(ConsumerUpdateRequest {
        fixture: &fixture,
        remote: &remote,
        run_root: &fixture.root.join("consumer-run"),
        publication_root: &publisher_root,
        gh_bin: &gh_bin,
        source_sha: &sha,
        immutable_tag: &tag,
        capture: &capture,
    });
    assert!(
        observed.contains(&format!("channel={CHANNEL}\n")),
        "{observed}"
    );
    assert!(
        observed.contains(&format!("asset_tag={tag}\n")),
        "updater did not observe actual publisher output: {observed}"
    );
    assert!(
        observed.contains("legacy_tag=unset\n"),
        "immutable mode exposed rolling alias: {observed}"
    );
    assert!(
        observed.contains(&format!(
            "version={}\nsource_sha={sha}\nrepository={REPOSITORY}\nsource_ref=refs/heads/main\n",
            package_version(&sha)
        )),
        "source identity did not reach updater: {observed}"
    );
    let branch = format!("automation/package-release-{tag}");
    let branch_ref = format!("refs/heads/{branch}:Formula/preview-tool.rb");
    let formula = real_git(
        &["--git-dir", remote.to_str().unwrap(), "show", &branch_ref],
        &fixture.root,
    );
    assert!(
        formula.contains(&format!("/releases/download/{tag}/{PAYLOAD}")),
        "payload URL did not use actual immutable publisher output: {formula}"
    );
    assert!(
        formula.contains(&format!("/releases/download/{tag}/{SUPPORT}")),
        "resource URL did not use actual immutable publisher output: {formula}"
    );
    assert!(
        !formula.contains("/releases/download/preview/"),
        "rolling alias leaked into formula URLs: {formula}"
    );
    let stable = real_git(
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "show",
            "refs/heads/main:Formula/stable-tool.rb",
        ],
        &fixture.root,
    );
    assert!(
        stable.contains("version \"9.9.9\""),
        "stable formula changed: {stable}"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep optional-refresh failure and its succeeding immutable delivery in one required case."
)]
fn rolling_refresh_is_optional() {
    let immutable = generate_fixture("rolling-disabled", TagMode::Immutable, false);
    assert!(
        publish_steps(&immutable.workflow)
            .iter()
            .all(|step| step["name"].as_str() != Some("Refresh rolling preview release")),
        "immutable-only mode must not schedule a mutable release refresh"
    );
    let optional_refresh = generate_fixture("rolling-optional", TagMode::Immutable, true);
    let optional_release_tag_prefix = emitted_release_tag_prefix(&optional_refresh);
    assert_published_verification_gates_consumer_pr(&optional_refresh.workflow);
    let optional_step = step_named(
        &optional_refresh.workflow,
        "Refresh rolling preview release",
    );
    assert_eq!(
        value_field(optional_step, "continue-on-error").and_then(Value::as_bool),
        Some(true),
        "optional rolling refresh failure must not block immutable consumer delivery"
    );
    let optional_sha = source_sha('8');
    let optional_tag = publish_fixture_candidate(
        &optional_refresh,
        "optional-refresh-publisher-run",
        &optional_sha,
    );
    let refresh_root = optional_refresh.root.join("optional-refresh-failure");
    fs::create_dir_all(&refresh_root).expect("create optional rolling refresh run root");
    let refresh_workspace = refresh_root.join("workspace");
    let published_dir = refresh_workspace.join("published-package");
    fs::create_dir_all(&published_dir).expect("create rolling refresh package handoff");
    for entry in fs::read_dir(
        optional_refresh
            .root
            .join("optional-refresh-publisher-run/package"),
    )
    .expect("read optional immutable publisher package")
    {
        let entry = entry.expect("read immutable package entry");
        fs::copy(entry.path(), published_dir.join(entry.file_name()))
            .expect("copy immutable package into rolling refresh handoff");
    }
    fs::create_dir_all(refresh_workspace.join("source"))
        .expect("create rolling refresh verified source checkout");
    fs::write(
        refresh_workspace.join("source/.source-commit"),
        &optional_sha,
    )
    .expect("bind rolling refresh source checkout to immutable package");
    fs::write(
        refresh_workspace.join("source/.origin-url"),
        format!("https://github.com/{REPOSITORY}.git"),
    )
    .expect("bind rolling refresh source origin to immutable package");
    let publication_tools = fake_publication_tools(&refresh_root);
    let failing_gh = fake_failing_gh(&refresh_root);
    let refresh_log = refresh_root.join("gh-failure.log");
    fs::write(&refresh_log, "").expect("create forced rolling refresh log");
    let mut refresh_envs = workflow_environment(
        &optional_refresh,
        &optional_sha,
        &optional_tag,
        &refresh_workspace,
        "Refresh rolling preview release",
    );
    refresh_envs.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}:{}",
            failing_gh.display(),
            publication_tools.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    refresh_envs.insert(
        "GH_LOG".to_owned(),
        refresh_log.to_string_lossy().into_owned(),
    );
    refresh_envs.insert("GH_TOKEN".to_owned(), "fixture-token".to_owned());
    refresh_envs.insert("GITHUB_REPOSITORY".to_owned(), REPOSITORY.to_owned());
    refresh_envs.insert(
        "GITHUB_WORKSPACE".to_owned(),
        refresh_workspace.to_string_lossy().into_owned(),
    );
    refresh_envs.insert(
        "GITHUB_ENV".to_owned(),
        refresh_root
            .join("github-env")
            .to_string_lossy()
            .into_owned(),
    );
    refresh_envs.insert(
        "GIT_LOG".to_owned(),
        refresh_root.join("git.log").to_string_lossy().into_owned(),
    );
    refresh_envs.insert("TAG_SHA".to_owned(), optional_sha.clone());
    refresh_envs.insert("LOCK_SHA".to_owned(), LOCK_SHA.to_owned());
    refresh_envs.insert("EXPECTED_SOURCE_COMMIT".to_owned(), optional_sha.clone());
    refresh_envs.insert("RELEASE_ASSET_TAG".to_owned(), optional_tag.clone());
    refresh_envs.insert(
        "RELEASE_STATE".to_owned(),
        optional_refresh
            .root
            .join("optional-refresh-publisher-run/release-state.json")
            .to_string_lossy()
            .into_owned(),
    );
    refresh_envs.insert(
        "VELNOR_PUBLICATION_LOCK_SHA".to_owned(),
        LOCK_SHA.to_owned(),
    );
    refresh_envs.insert(
        "VELNOR_PUBLICATION_LOCK_RELEASED".to_owned(),
        "0".to_owned(),
    );
    refresh_envs.insert("VELNOR_PUBLICATION_LOCK_RETAIN".to_owned(), "0".to_owned());
    refresh_envs.insert(
        "VELNOR_VERIFIED_PACKAGE_DIR".to_owned(),
        published_dir.to_string_lossy().into_owned(),
    );
    let refresh_output = run_bash(
        step_run(
            &optional_refresh.workflow,
            "Refresh rolling preview release",
        ),
        &refresh_root,
        &refresh_envs.into_iter().collect::<Vec<_>>(),
    );
    assert!(
        !refresh_output.status.success(),
        "forced optional rolling refresh failure unexpectedly succeeded"
    );
    let refresh_calls = fs::read_to_string(&refresh_log).expect("read forced rolling refresh log");
    assert!(
        refresh_calls.contains(&format!("releases/tags/{optional_tag}"))
            && refresh_calls.contains(&format!("releases/tags/{optional_release_tag_prefix}")),
        "optional refresh failure did not reach rolling-release lookup after validating the immutable staged release and held publication lock; gh={refresh_calls}; git={}; stdout={}; stderr={}",
        fs::read_to_string(refresh_root.join("git.log")).unwrap_or_default(),
        String::from_utf8_lossy(&refresh_output.stdout),
        String::from_utf8_lossy(&refresh_output.stderr)
    );
    let optional_remote = make_tap_remote(&optional_refresh.root, TagMode::Immutable);
    let optional_pr_gh = fake_pr_gh(&optional_refresh.root);
    let optional_capture = optional_refresh.root.join("after-refresh-failure.log");
    let after_refresh_failure = run_consumer_update(ConsumerUpdateRequest {
        fixture: &optional_refresh,
        remote: &optional_remote,
        run_root: &optional_refresh.root.join("consumer-after-refresh-failure"),
        publication_root: &optional_refresh.root.join("optional-refresh-publisher-run"),
        gh_bin: &optional_pr_gh,
        source_sha: &optional_sha,
        immutable_tag: &optional_tag,
        capture: &optional_capture,
    });
    assert!(
        after_refresh_failure.contains(&format!("asset_tag={optional_tag}\n"))
            && after_refresh_failure.contains("legacy_tag=unset\n"),
        "immutable consumer update failed after the optional rolling refresh failure: {after_refresh_failure}"
    );
    let legacy = generate_fixture("rolling-enabled", TagMode::Legacy, true);
    let legacy_refresh = step_named(&legacy.workflow, "Refresh rolling preview release");
    assert!(
        step_run(&legacy.workflow, "Refresh rolling preview release").contains("rolling_tag")
            && value_field(legacy_refresh, "continue-on-error").and_then(Value::as_bool)
                != Some(true),
        "explicit compatibility mode must retain rolling refresh behavior"
    );
    let default_config = config(TagMode::Legacy, true)
        .lines()
        .filter(|line| {
            !line.starts_with("consumer_tag_mode =")
                && !line.starts_with("refresh_rolling_release =")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let default_legacy =
        generate_fixture_from_config("rolling-defaults", &default_config, TagMode::Legacy);
    assert!(
        !default_config.contains("consumer_tag_mode =")
            && !default_config.contains("refresh_rolling_release ="),
        "default fixture must omit every TASK-004 consumer mode field"
    );
    for required_task003_entry in [
        "[declare.args.production_inputs]\nproduct = [\"src/**\", \"Cargo.toml\", \"Cargo.lock\"]",
        "[declare.args.production_dependencies]\nruntime_resource = [\"resources/**\"]",
        "[declare.args.non_production_inputs]\ndocumentation = [\"README.md\", \"docs/**\"]\nverification_fixtures = [\"tests/**\"]",
    ] {
        assert!(
            default_config.contains(required_task003_entry),
            "legacy fixture must retain required TASK-003 input tables and values: {required_task003_entry}"
        );
    }
    let default_refresh = step_named(&default_legacy.workflow, "Refresh rolling preview release");
    assert_ne!(
        value_field(default_refresh, "continue-on-error").and_then(Value::as_bool),
        Some(true),
        "omitted config must retain blocking legacy rolling refresh"
    );
    assert!(
        step_run(&default_legacy.workflow, "Refresh rolling preview release")
            .contains("rolling_tag"),
        "omitted config must retain legacy rolling release behavior"
    );
    let default_updater_env = env_map(
        &default_legacy.workflow,
        "Run updater and create or update consumer PR",
    );
    assert_eq!(
        mapping_string(default_updater_env, "VELNOR_PACKAGE_RELEASE_TAG"),
        Some(RELEASE_TAG_PREFIX),
        "omitted config must supply the prior rolling alias to a legacy updater"
    );
    let default_sha = source_sha('7');
    let default_tag =
        publish_fixture_candidate(&default_legacy, "default-publisher-run", &default_sha);
    let default_remote = make_tap_remote(&default_legacy.root, TagMode::Legacy);
    let default_gh = fake_pr_gh(&default_legacy.root);
    let default_capture = default_legacy.root.join("default-legacy-updater.log");
    let default_observed = run_consumer_update(ConsumerUpdateRequest {
        fixture: &default_legacy,
        remote: &default_remote,
        run_root: &default_legacy.root.join("default-consumer-run"),
        publication_root: &default_legacy.root.join("default-publisher-run"),
        gh_bin: &default_gh,
        source_sha: &default_sha,
        immutable_tag: &default_tag,
        capture: &default_capture,
    });
    assert!(
        default_observed.contains(&format!("legacy_tag={RELEASE_TAG_PREFIX}\n")),
        "omitted config did not execute the legacy updater with its rolling alias: {default_observed}"
    );
    let default_branch = format!("automation/package-release-{default_tag}");
    let default_branch_ref = format!("refs/heads/{default_branch}:Formula/preview-tool.rb");
    let default_formula = real_git(
        &[
            "--git-dir",
            default_remote.to_str().expect("UTF-8 tap remote path"),
            "show",
            &default_branch_ref,
        ],
        &default_legacy.root,
    );
    assert!(
        default_formula.contains(&format!(
            "/releases/download/{RELEASE_TAG_PREFIX}/{PAYLOAD}"
        )),
        "omitted config changed default legacy formula URL: {default_formula}"
    );

    let legacy_without_inputs_config = old_legacy_config_without_task003_inputs();
    for absent_task003_table in [
        "[declare.args.production_inputs]",
        "[declare.args.production_dependencies]",
        "[declare.args.non_production_inputs]",
    ] {
        assert!(
            !legacy_without_inputs_config.contains(absent_task003_table),
            "legacy fixture unexpectedly declares {absent_task003_table}"
        );
    }
    let legacy_without_inputs = generate_fixture_from_config(
        "legacy-without-input-tables",
        &legacy_without_inputs_config,
        TagMode::Legacy,
    );
    let legacy_without_inputs_refresh = step_named(
        &legacy_without_inputs.workflow,
        "Refresh rolling preview release",
    );
    assert_ne!(
        value_field(legacy_without_inputs_refresh, "continue-on-error").and_then(Value::as_bool),
        Some(true),
        "legacy declaration without TASK-003 input tables must keep blocking refresh behavior"
    );
    let legacy_without_inputs_env = env_map(
        &legacy_without_inputs.workflow,
        "Run updater and create or update consumer PR",
    );
    assert_eq!(
        mapping_string(legacy_without_inputs_env, "VELNOR_PACKAGE_RELEASE_TAG"),
        Some(RELEASE_TAG_PREFIX),
        "legacy declaration without TASK-003 input tables must keep the rolling updater input"
    );
    let (before, head) = commit_unknown_source_change(&legacy_without_inputs.root);
    let admission = run_release_admission(&legacy_without_inputs.root, &before, &head);
    assert!(
        admission.status.success(),
        "legacy declaration without TASK-003 input tables failed admission:\n{}{}",
        String::from_utf8_lossy(&admission.stdout),
        String::from_utf8_lossy(&admission.stderr)
    );
    let admission: Value = serde_yaml::from_slice(&admission.stdout)
        .expect("parse legacy release-admission JSON output");
    assert_eq!(
        admission["disposition"].as_str(),
        Some("admit"),
        "unclassified source changes in a legacy declaration must be admitted"
    );
    assert!(
        admission["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("admitting conservatively")),
        "legacy missing-input fallback must explain its conservative admission: {admission}"
    );

    let invalid_mode = config(TagMode::Immutable, false).replace(
        "consumer_tag_mode = \"immutable\"",
        "consumer_tag_mode = \"unknown\"",
    );
    assert_generator_rejects("invalid-tag-mode", &invalid_mode, "consumer_tag_mode");
    let invalid_refresh_type = config(TagMode::Immutable, false).replace(
        "refresh_rolling_release = false",
        "refresh_rolling_release = \"false\"",
    );
    assert_generator_rejects(
        "invalid-refresh-type",
        &invalid_refresh_type,
        "refresh_rolling_release",
    );
    let invalid_legacy_refresh = config(TagMode::Legacy, false);
    assert_generator_rejects(
        "legacy-without-refresh",
        &invalid_legacy_refresh,
        "legacy consumer_tag_mode requires refresh_rolling_release = true",
    );
    let explicit_empty_inputs = config(TagMode::Legacy, true).replace(
        "[declare.args.production_inputs]\nproduct = [\"src/**\", \"Cargo.toml\", \"Cargo.lock\"]\n",
        "[declare.args.production_inputs]\n",
    );
    assert_generator_rejects(
        "explicit-empty-production-inputs",
        &explicit_empty_inputs,
        "production_inputs must declare at least one named path group",
    );

    let sha = source_sha('c');
    let tag = publish_fixture_candidate(&immutable, "publisher-run", &sha);
    let remote = make_tap_remote(&immutable.root, TagMode::Immutable);
    let gh_bin = fake_pr_gh(&immutable.root);
    let capture = immutable.root.join("rolling-disabled-updater.log");
    let observed = run_consumer_update(ConsumerUpdateRequest {
        fixture: &immutable,
        remote: &remote,
        run_root: &immutable.root.join("consumer-run"),
        publication_root: &immutable.root.join("publisher-run"),
        gh_bin: &gh_bin,
        source_sha: &sha,
        immutable_tag: &tag,
        capture: &capture,
    });
    assert!(
        observed.contains("legacy_tag=unset\n"),
        "immutable updater ran with a rolling alias: {observed}"
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the checkout, origin, tag, and publisher mismatch negatives in one required case."
)]
fn source_tag_mismatch_is_rejected() {
    let fixture = generate_fixture("tag-mismatch", TagMode::Immutable, false);
    let root = fixture.root.join("mismatch-run");
    fs::create_dir_all(&root).expect("create mismatch run root");
    let source = source_sha('a');
    let different = source_sha('b');
    let release_tag_prefix = emitted_release_tag_prefix(&fixture);
    let wrong_checkout = run_fake_git_source_probe(
        &fixture.root.join("wrong-checkout-probe"),
        &source,
        &different,
        REPOSITORY,
        &release_tag_prefix,
        &source,
    );
    assert!(
        !wrong_checkout.status.success()
            && String::from_utf8_lossy(&wrong_checkout.stderr)
                .contains("git source checkout SHA does not match verified manifest"),
        "fake Git accepted a checkout at the wrong SHA: {}",
        String::from_utf8_lossy(&wrong_checkout.stderr)
    );
    let wrong_checkout_tag = run_fake_git_source_probe(
        &fixture.root.join("wrong-checkout-tag-probe"),
        &source,
        &source,
        REPOSITORY,
        &release_tag_prefix,
        &different,
    );
    assert!(
        !wrong_checkout_tag.status.success()
            && String::from_utf8_lossy(&wrong_checkout_tag.stderr)
                .contains("git source checkout tag does not bind to verified manifest"),
        "fake Git accepted a tag bound to a different source SHA: {}",
        String::from_utf8_lossy(&wrong_checkout_tag.stderr)
    );
    let origin_mismatch_publisher = fixture.root.join("origin-mismatch-publisher");
    let origin_mismatch_tag =
        publish_fixture_candidate(&fixture, "origin-mismatch-publisher", &source);
    run_published_verification(
        &fixture,
        &origin_mismatch_publisher,
        &fixture.root.join("origin-mismatch-verifier"),
        &source,
        &origin_mismatch_tag,
        "example/other-preview-source",
        true,
    );
    run_published_verification_controlled(PublishedVerificationRequest {
        fixture: &fixture,
        publication_root: &origin_mismatch_publisher,
        run_root: &fixture.root.join("checkout-sha-mismatch-verifier"),
        source_sha: &source,
        immutable_tag: &origin_mismatch_tag,
        checkout_repository: REPOSITORY,
        checkout_sha: &different,
        expected_failure: Some("git source checkout SHA does not match verified manifest"),
    });
    let verified_package = run_published_verification(
        &fixture,
        &origin_mismatch_publisher,
        &fixture.root.join("tag-mismatch-good-verifier"),
        &source,
        &origin_mismatch_tag,
        REPOSITORY,
        false,
    );
    let wrong_consumer_tag = format!("{release_tag_prefix}-{different}");
    let mismatch_package_root = fixture.root.join("identity-mismatch-run");
    let identity_mismatch = run_consumer_identity_check(
        &fixture,
        &mismatch_package_root,
        &verified_package,
        &source,
        &wrong_consumer_tag,
    );
    assert!(
        !identity_mismatch.status.success(),
        "consumer identity gate accepted an asset tag for another source SHA"
    );
    assert!(
        String::from_utf8_lossy(&identity_mismatch.stderr)
            .contains("immutable package asset tag does not bind to its source commit"),
        "consumer identity mismatch was rejected for an unrelated reason: {}",
        String::from_utf8_lossy(&identity_mismatch.stderr)
    );
    let run = run_publication_step(
        &fixture,
        &root,
        &source,
        "exact",
        &different,
        &release_json(
            &release_tag_prefix,
            &source,
            &["release-manifest.json", "identity.json", PAYLOAD, SUPPORT],
        ),
    );
    assert!(
        !run.output.status.success(),
        "tag resolving to a different source SHA must fail"
    );
    assert!(
        run.git_log
            .contains(&format!("refs/tags/{release_tag_prefix}-{source}")),
        "publisher never queried the immutable source tag: {}",
        run.git_log
    );
    assert!(
        String::from_utf8_lossy(&run.output.stderr)
            .contains("immutable release tag resolves to an unexpected source commit"),
        "wrong source tag did not trigger the specific rejection: {}",
        String::from_utf8_lossy(&run.output.stderr)
    );
    assert!(
        !run.gh_log.contains("/releases/tags/"),
        "release lookup occurred before tag mismatch rejection: {}",
        run.gh_log
    );
    assert!(
        !run.gh_log.contains("release> <create>")
            && !run.gh_log.contains("release> <upload>")
            && !run.gh_log.contains("--method> <PATCH>"),
        "release mutation occurred after a source-tag mismatch: {}",
        run.gh_log
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep ordered partial-asset and byte-mismatch publication checks in one required case."
)]
fn partial_or_mismatched_assets_are_rejected() {
    let fixture = generate_fixture("asset-reject", TagMode::Immutable, false);
    let sha = source_sha('a');
    let release_tag_prefix = emitted_release_tag_prefix(&fixture);
    let complete = ["release-manifest.json", "identity.json", PAYLOAD, SUPPORT];

    let partial_root = fixture.root.join("partial-run");
    fs::create_dir_all(&partial_root).expect("create partial release run");
    let partial = run_publication_step(
        &fixture,
        &partial_root,
        &sha,
        "exact",
        &sha,
        &release_json(
            &release_tag_prefix,
            &sha,
            &["release-manifest.json", "identity.json", PAYLOAD],
        ),
    );
    assert!(
        !partial.output.status.success(),
        "public release missing a declared asset must fail"
    );
    assert!(
        String::from_utf8_lossy(&partial.output.stderr)
            .contains("immutable release is already public but incomplete"),
        "partial release was rejected for an unrelated reason: {}",
        String::from_utf8_lossy(&partial.output.stderr)
    );
    assert!(
        partial
            .gh_log
            .contains(&format!("/releases/tags/{release_tag_prefix}-{sha}")),
        "partial test never inspected the public release: {}",
        partial.gh_log
    );
    assert!(
        !partial.gh_log.contains("release> <upload>")
            && !partial.gh_log.contains("release> <create>")
            && !partial.gh_log.contains("--method> <PATCH>"),
        "partial public release was mutated: {}",
        partial.gh_log
    );

    let digest_root = fixture.root.join("digest-run");
    fs::create_dir_all(&digest_root).expect("create digest release run");
    let digest = run_publication_step(
        &fixture,
        &digest_root,
        &sha,
        "digest-mismatch",
        &sha,
        &release_json(&release_tag_prefix, &sha, &complete),
    );
    assert!(
        !digest.output.status.success(),
        "public release with changed payload bytes must fail"
    );
    assert!(
        String::from_utf8_lossy(&digest.output.stderr)
            .contains("immutable release asset bytes differ: preview-package.tar.gz"),
        "digest control was rejected for an unrelated reason: {}",
        String::from_utf8_lossy(&digest.output.stderr)
    );
    assert!(
        digest.gh_log.contains("release> <download>"),
        "digest negative control never exercised asset byte verification: {}",
        digest.gh_log
    );
    assert!(
        !digest.gh_log.contains("release> <upload>")
            && !digest.gh_log.contains("release> <create>")
            && !digest.gh_log.contains("--method> <PATCH>"),
        "mismatched public release bytes were mutated: {}",
        digest.gh_log
    );

    let draft_mismatch_root = fixture.root.join("draft-mismatch-run");
    fs::create_dir_all(&draft_mismatch_root).expect("create mismatched draft run");
    let draft_mismatch = run_publication_step(
        &fixture,
        &draft_mismatch_root,
        &sha,
        "digest-mismatch",
        &sha,
        &draft_release_json(&release_tag_prefix, &sha, &complete),
    );
    assert!(
        !draft_mismatch.output.status.success(),
        "draft with mismatched existing bytes must fail before publication"
    );
    assert!(
        String::from_utf8_lossy(&draft_mismatch.output.stderr)
            .contains("immutable release asset bytes differ: preview-package.tar.gz"),
        "draft digest mismatch was rejected for an unrelated reason: {}",
        String::from_utf8_lossy(&draft_mismatch.output.stderr)
    );
    assert!(
        draft_mismatch.gh_log.contains("release> <download>")
            && !draft_mismatch.gh_log.contains("release> <upload>")
            && !draft_mismatch.gh_log.contains("--method> <PATCH"),
        "bad draft bytes were uploaded or promoted: {}",
        draft_mismatch.gh_log
    );
    assert!(
        draft_mismatch.release_state.contains("\"draft\":true"),
        "failed draft validation changed release visibility: {}",
        draft_mismatch.release_state
    );

    let partial_draft_root = fixture.root.join("partial-draft-run");
    fs::create_dir_all(&partial_draft_root).expect("create partial draft run");
    let partial_draft = run_publication_step(
        &fixture,
        &partial_draft_root,
        &sha,
        "exact",
        &sha,
        &draft_release_json(
            &release_tag_prefix,
            &sha,
            &["release-manifest.json", "identity.json", PAYLOAD],
        ),
    );
    let partial_draft_tag = immutable_tag_from(&partial_draft, &release_tag_prefix, &sha);
    let partial_draft_state: Value = serde_yaml::from_str(&partial_draft.release_state)
        .expect("parse final partial-draft release state");
    assert!(
        value_field(&partial_draft_state, "draft").and_then(Value::as_bool) == Some(false),
        "verified partial draft did not publish: {}",
        partial_draft.release_state
    );
    let upload_position = partial_draft
        .gh_log
        .find("gh <release> <upload>")
        .expect("partial draft should upload its missing declared asset");
    let final_download_position = partial_draft
        .gh_log
        .rfind("gh <release> <download>")
        .expect("partial draft should verify all final public bytes");
    let promotion_position = partial_draft
        .gh_log
        .find("--method> <PATCH")
        .expect("verified draft should be promoted public");
    assert!(
        upload_position < final_download_position && final_download_position < promotion_position,
        "draft promotion preceded complete asset byte verification: {}",
        partial_draft.gh_log
    );
    assert_eq!(
        partial_draft.gh_log.matches("--method> <PATCH").count(),
        1,
        "partial draft must be promoted exactly once"
    );
    assert!(
        partial_draft
            .workflow_output
            .contains(&format!("immutable_tag={partial_draft_tag}")),
        "successful draft publication did not emit its immutable tag"
    );

    let corrupt_upload_root = fixture.root.join("corrupt-upload-draft-run");
    fs::create_dir_all(&corrupt_upload_root).expect("create corrupt upload draft run");
    let corrupt_upload = run_publication_step(
        &fixture,
        &corrupt_upload_root,
        &sha,
        "upload-mismatch",
        &sha,
        &draft_release_json(
            &release_tag_prefix,
            &sha,
            &["release-manifest.json", "identity.json", PAYLOAD],
        ),
    );
    assert!(
        !corrupt_upload.output.status.success(),
        "draft with corrupted uploaded bytes must fail before promotion"
    );
    assert!(
        String::from_utf8_lossy(&corrupt_upload.output.stderr)
            .contains("immutable release asset bytes differ: SHA256SUMS"),
        "corrupt uploaded bytes were rejected for an unrelated reason: {}",
        String::from_utf8_lossy(&corrupt_upload.output.stderr)
    );
    let upload_position = corrupt_upload
        .gh_log
        .find("gh <release> <upload>")
        .expect("corrupt-draft control must exercise the upload");
    let final_download_position = corrupt_upload
        .gh_log
        .rfind("gh <release> <download>")
        .expect("corrupt-draft control must download bytes from release storage");
    assert!(
        upload_position < final_download_position,
        "corrupt upload was not downloaded after upload: {}",
        corrupt_upload.gh_log
    );
    let corrupt_draft_state: Value =
        serde_yaml::from_str(&corrupt_upload.release_state).expect("parse corrupt draft state");
    assert!(
        !corrupt_upload.gh_log.contains("--method> <PATCH>")
            && value_field(&corrupt_draft_state, "draft").and_then(Value::as_bool) == Some(true),
        "draft with mismatched uploaded bytes was promoted: {}",
        corrupt_upload.gh_log
    );

    let extra_root = fixture.root.join("extra-asset-run");
    fs::create_dir_all(&extra_root).expect("create extra asset run");
    let extra = run_publication_step(
        &fixture,
        &extra_root,
        &sha,
        "exact",
        &sha,
        &release_json(
            &release_tag_prefix,
            &sha,
            &[
                "release-manifest.json",
                "identity.json",
                PAYLOAD,
                SUPPORT,
                "undeclared.bin",
            ],
        ),
    );
    assert!(
        !extra.output.status.success(),
        "public release with an undeclared asset must fail"
    );
    assert!(
        String::from_utf8_lossy(&extra.output.stderr)
            .contains("immutable release contains an undeclared asset"),
        "extra asset was rejected for an unrelated reason: {}",
        String::from_utf8_lossy(&extra.output.stderr)
    );
    assert!(
        !extra.gh_log.contains("release> <upload>")
            && !extra.gh_log.contains("release> <create>")
            && !extra.gh_log.contains("--method> <PATCH>"),
        "release with undeclared asset was mutated: {}",
        extra.gh_log
    );
}

fn legacy_updater_runs_with_old_environment(root: &Path, source_sha: &str) -> String {
    let package = write_verified_package(root, source_sha, "legacy-minimal-package");
    let tap = root.join("legacy-minimal-tap");
    fs::create_dir_all(tap.join("scripts")).expect("create minimal legacy tap");
    write_executable(
        &tap.join("scripts/package-update.sh"),
        &updater_script(TagMode::Legacy),
    );
    let capture = root.join("legacy-minimal-env.log");
    let path = std::env::var("PATH").unwrap_or_default();
    let envs = vec![
        ("PATH".to_owned(), path),
        ("VELNOR_PACKAGE_CHANNEL".to_owned(), CHANNEL.to_owned()),
        (
            "VELNOR_PACKAGE_RELEASE_TAG".to_owned(),
            RELEASE_TAG_PREFIX.to_owned(),
        ),
        (
            "VELNOR_VERIFIED_PACKAGE_DIR".to_owned(),
            package.to_string_lossy().into_owned(),
        ),
        (
            "UPDATER_CAPTURE".to_owned(),
            capture.to_string_lossy().into_owned(),
        ),
    ];
    let output = run_bash("./scripts/package-update.sh", &tap, &envs);
    assert!(
        output.status.success(),
        "pre-immutable updater inputs no longer work:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(capture).expect("read minimal legacy updater environment")
}

#[test]
fn legacy_consumer_mode_is_preserved() {
    let fixture = generate_fixture("legacy-mode", TagMode::Legacy, true);
    let sha = source_sha('d');
    let minimal_observed = legacy_updater_runs_with_old_environment(&fixture.root, &sha);
    assert!(
        minimal_observed.contains(&format!("channel={CHANNEL}\n")),
        "legacy updater lost channel input: {minimal_observed}"
    );
    assert!(
        minimal_observed.contains(&format!("legacy_tag={RELEASE_TAG_PREFIX}\n")),
        "legacy updater lost rolling release-tag input: {minimal_observed}"
    );
    assert!(
        minimal_observed.contains("asset_tag=unset\n"),
        "legacy updater unexpectedly depended on immutable tag input: {minimal_observed}"
    );

    let tag = publish_fixture_candidate(&fixture, "publisher-run", &sha);
    let remote = make_tap_remote(&fixture.root, TagMode::Legacy);
    let gh_bin = fake_pr_gh(&fixture.root);
    let capture = fixture.root.join("legacy-updater.log");
    let observed = run_consumer_update(ConsumerUpdateRequest {
        fixture: &fixture,
        remote: &remote,
        run_root: &fixture.root.join("legacy-run"),
        publication_root: &fixture.root.join("publisher-run"),
        gh_bin: &gh_bin,
        source_sha: &sha,
        immutable_tag: &tag,
        capture: &capture,
    });
    assert!(
        observed.contains(&format!("channel={CHANNEL}\n")),
        "legacy channel changed: {observed}"
    );
    assert!(
        observed.contains(&format!("legacy_tag={RELEASE_TAG_PREFIX}\n")),
        "legacy rolling tag disappeared: {observed}"
    );
    let branch = format!("automation/package-release-{tag}");
    let branch_ref = format!("refs/heads/{branch}:Formula/preview-tool.rb");
    let formula = real_git(
        &["--git-dir", remote.to_str().unwrap(), "show", &branch_ref],
        &fixture.root,
    );
    assert!(
        formula.contains(&format!(
            "/releases/download/{RELEASE_TAG_PREFIX}/{PAYLOAD}"
        )),
        "legacy alias URL changed: {formula}"
    );
}

#[test]
fn old_preview_urls_do_not_change() {
    let fixture = generate_fixture("older-preview", TagMode::Immutable, false);
    let old_sha = source_sha('e');
    let new_sha = source_sha('f');
    let old_tag = publish_fixture_candidate(&fixture, "old-publisher-run", &old_sha);
    let new_tag = publish_fixture_candidate(&fixture, "new-publisher-run", &new_sha);
    let remote = make_tap_remote(&fixture.root, TagMode::Immutable);
    let gh_bin = fake_pr_gh(&fixture.root);
    run_consumer_update(ConsumerUpdateRequest {
        fixture: &fixture,
        remote: &remote,
        run_root: &fixture.root.join("old-run"),
        publication_root: &fixture.root.join("old-publisher-run"),
        gh_bin: &gh_bin,
        source_sha: &old_sha,
        immutable_tag: &old_tag,
        capture: &fixture.root.join("old-updater.log"),
    });
    run_consumer_update(ConsumerUpdateRequest {
        fixture: &fixture,
        remote: &remote,
        run_root: &fixture.root.join("new-run"),
        publication_root: &fixture.root.join("new-publisher-run"),
        gh_bin: &gh_bin,
        source_sha: &new_sha,
        immutable_tag: &new_tag,
        capture: &fixture.root.join("new-updater.log"),
    });

    let old_branch = format!("automation/package-release-{old_tag}");
    let new_branch = format!("automation/package-release-{new_tag}");
    let old_ref = format!("refs/heads/{old_branch}:Formula/preview-tool.rb");
    let new_ref = format!("refs/heads/{new_branch}:Formula/preview-tool.rb");
    let old_formula = real_git(
        &["--git-dir", remote.to_str().unwrap(), "show", &old_ref],
        &fixture.root,
    );
    let new_formula = real_git(
        &["--git-dir", remote.to_str().unwrap(), "show", &new_ref],
        &fixture.root,
    );
    assert!(
        old_formula.contains(&format!("/releases/download/{old_tag}/{PAYLOAD}")),
        "old PR payload URL changed: {old_formula}"
    );
    assert!(
        old_formula.contains(&format!("/releases/download/{old_tag}/{SUPPORT}")),
        "old PR resource URL changed: {old_formula}"
    );
    assert!(
        new_formula.contains(&format!("/releases/download/{new_tag}/{PAYLOAD}")),
        "new PR payload URL did not use its immutable tag: {new_formula}"
    );
    assert!(
        new_formula.contains(&format!("/releases/download/{new_tag}/{SUPPORT}")),
        "new PR resource URL did not use its immutable tag: {new_formula}"
    );
    assert!(
        !old_formula.contains(&new_tag),
        "newer candidate rewrote old preview URLs: {old_formula}"
    );
    assert!(
        !new_formula.contains("/releases/download/preview/"),
        "new formula uses mutable alias: {new_formula}"
    );
}
