//! Executable package-handoff and admitted-source identity checks.
//!
//! The producer is a child process invoked by the actual generated build step.
//! The test inspects files only after that process and the generated cleanup
//! boundary have exited.

#![expect(
    clippy::unwrap_used,
    reason = "fixture setup failures should identify the failed operation"
)]
#![expect(
    clippy::expect_used,
    reason = "fixture setup failures should identify the failed operation"
)]
#![expect(
    clippy::panic,
    reason = "fixture lookup failures should include the generated document"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "example/preview-source";
const SOURCE_REF: &str = "refs/heads/main";
const MANIFEST_SCHEMA: &str = "example.consumer-manifest-v1";
const PAYLOAD: &str = "preview-package.tar.gz";
const SUMS: &str = "SHA256SUMS";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    runner_temp: PathBuf,
    workflow: YamlValue,
    initial_sha: String,
}

impl Fixture {
    fn new(label: &str, package_dir: &str) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "package-handoff-{label}-{}-{sequence}",
            std::process::id()
        ));
        let root = parent.join("source");
        let runner_temp = parent.join("runner-temp");
        fs::create_dir_all(&root).expect("create fixture source checkout");
        fs::create_dir_all(&runner_temp).expect("create runner temporary root");

        write_fixture_inputs(&root, package_dir);
        let generated = generate(&root);
        assert!(
            generated.status.success(),
            "fixture generation failed:\n{}",
            output_text(&generated)
        );

        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.name", "Preview fixture"]);
        git(
            &root,
            &["config", "user.email", "preview-fixture@example.invalid"],
        );
        git(&root, &["add", "--all"]);
        git(&root, &["commit", "--quiet", "--message", "fixture source"]);
        let initial_sha = git(&root, &["rev-parse", "HEAD"]);
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/example/preview-source.git",
            ],
        );

        let workflow_path = root.join(".github/workflows/preview.yml");
        let workflow_text = fs::read_to_string(workflow_path).expect("read generated workflow");
        let workflow: YamlValue =
            serde_yaml::from_str(&workflow_text).expect("generated workflow is YAML");
        Self {
            root,
            runner_temp,
            workflow,
            initial_sha,
        }
    }

    fn commit_source_change(&self, contents: &str, message: &str) -> String {
        write(&self.root, "src/release.txt", contents);
        git(&self.root, &["add", "src/release.txt"]);
        git(&self.root, &["commit", "--quiet", "--message", message]);
        git(&self.root, &["rev-parse", "HEAD"])
    }

    fn build_job(&self) -> &YamlValue {
        &self.workflow["jobs"]["build"]
    }

    fn verify_job(&self) -> &YamlValue {
        &self.workflow["jobs"]["verify"]
    }

    fn attest_job(&self) -> &YamlValue {
        &self.workflow["jobs"]["attest"]
    }

    fn publish_job(&self) -> &YamlValue {
        &self.workflow["jobs"]["publish"]
    }

    fn fresh_runner(&self, label: &str, source_sha: &str) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "package-handoff-{label}-fresh-{}-{sequence}",
            std::process::id()
        ));
        let root = parent.join("workspace");
        let runner_temp = parent.join("runner-temp");
        fs::create_dir_all(&parent).expect("create fresh runner parent");
        fs::create_dir_all(&runner_temp).expect("create fresh runner temporary root");
        fs::create_dir_all(&root).expect("create fresh runner workspace");
        let source_checkout = root.join("source");
        let output = Command::new("git")
            .args(["clone", "--quiet", "--shared"])
            .arg(&self.root)
            .arg(&source_checkout)
            .output()
            .expect("clone source into fresh runner");
        assert!(
            output.status.success(),
            "clone admitted source into fresh runner failed:\n{}",
            output_text(&output)
        );
        git(
            &source_checkout,
            &["checkout", "--quiet", "--detach", source_sha],
        );
        git(
            &source_checkout,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/example/preview-source.git",
            ],
        );
        Self {
            root,
            runner_temp,
            workflow: self.workflow.clone(),
            initial_sha: self.initial_sha.clone(),
        }
    }

    fn fresh_workspace(&self, label: &str) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "package-handoff-{label}-workspace-{}-{sequence}",
            std::process::id()
        ));
        let root = parent.join("workspace");
        let runner_temp = parent.join("runner-temp");
        fs::create_dir_all(&root).expect("create source-free runner workspace");
        fs::create_dir_all(&runner_temp).expect("create source-free runner temporary root");
        Self {
            root,
            runner_temp,
            workflow: self.workflow.clone(),
            initial_sha: self.initial_sha.clone(),
        }
    }
}

struct EventContext {
    repository: String,
    event_ref: String,
    event_name: String,
    before_sha: String,
    head_sha: String,
    run_id: String,
    attempt: String,
}

impl EventContext {
    fn push(before_sha: &str, head_sha: &str, run_id: &str, attempt: &str) -> Self {
        Self {
            repository: REPOSITORY.to_owned(),
            event_ref: SOURCE_REF.to_owned(),
            event_name: "push".to_owned(),
            before_sha: before_sha.to_owned(),
            head_sha: head_sha.to_owned(),
            run_id: run_id.to_owned(),
            attempt: attempt.to_owned(),
        }
    }

    fn dispatch(head_sha: &str, run_id: &str, attempt: &str) -> Self {
        Self {
            repository: REPOSITORY.to_owned(),
            event_ref: SOURCE_REF.to_owned(),
            event_name: "workflow_dispatch".to_owned(),
            before_sha: head_sha.to_owned(),
            head_sha: head_sha.to_owned(),
            run_id: run_id.to_owned(),
            attempt: attempt.to_owned(),
        }
    }
}

struct StepResult {
    output: Output,
    github_output: PathBuf,
    github_env: PathBuf,
    github_path: PathBuf,
    environment: std::collections::BTreeMap<String, String>,
}

struct PackageRun {
    candidate: JsonValue,
    admission_outputs: std::collections::BTreeMap<String, String>,
    build_ran: bool,
    source_check: Option<StepResult>,
    build: Option<StepResult>,
    producer_followup: Option<StepResult>,
    verification: Option<StepResult>,
    checkout_sha: Option<String>,
    verification_fixture: Option<Fixture>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(parent) = self.root.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

fn write_fixture_inputs(root: &Path, package_dir: &str) {
    write(root, ".gitignore", "dist/\nlegacy-output/\n");
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"package-handoff-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        root,
        "rust-toolchain.toml",
        "[toolchain]\nchannel = \"1.98.1\"\n",
    );
    write(root, "src/lib.rs", "pub fn fixture() -> u8 { 1 }\n");
    write(root, "src/release.txt", "fixture source bytes\n");
    write(root, "README.md", "fixture source\n");
    write(
        root,
        ".github-gen/velnor-workflow.toml",
        &configuration(package_dir),
    );
    write(
        root,
        ".github-gen/visibility.toml",
        &format!("repository = \"{REPOSITORY}\"\nvisibility = \"public\"\n"),
    );
    write(
        root,
        "mise.toml",
        "[tasks.build-release]\nrun = \"true\"\n\n[tasks.verify-release]\nrun = \"true\"\n",
    );
}

fn configuration(package_dir: &str) -> String {
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
publication_lock_branch = "package-release-lock"
package_dir = "{package_dir}"
manifest_schema = "{MANIFEST_SCHEMA}"
source_repository = "{REPOSITORY}"
source_ref = "{SOURCE_REF}"
payloads = ["{PAYLOAD}"]
supporting_assets = ["{SUMS}"]
channel = "preview"
release_tag = "preview"
github_release_type = "prerelease"
publish_environment = "github-preview"
consumer_repository = "example/preview-tap"
consumer_branch = "main"
updater = "./scripts/package-update.sh"
updater_token_secret = "TAP_TOKEN"

[declare.args.production_inputs]
release_source = ["src/**"]

[declare.args.production_dependencies]
runtime_resource = ["share/**"]

[declare.args.non_production_inputs]
documentation = ["README.md"]
"#
    )
}

fn fake_mise_script() -> &'static str {
    r#"#!/bin/bash
set -euo pipefail
[[ "${1:-}" == run ]]
if [[ "${2:-}" == verify-release ]]; then
  printf '%s\n' "${EXPECTED_SOURCE_COMMIT:?}" > "$RUNNER_TEMP/followup-source-commit"
  jq --version > "$RUNNER_TEMP/followup-jq-version"
  exit 0
fi
[[ "${2:-}" == build-release ]]
printf '%s\n' "${PACKAGE_RELEASE_SCRATCH_DIR:?}" > "$RUNNER_TEMP/producer-env"
printf '%s\n' "${PACKAGE_DIR:?}" >> "$RUNNER_TEMP/producer-env"
printf '%s\n' "${VELNOR_VERIFIED_PACKAGE_DIR:?}" >> "$RUNNER_TEMP/producer-env"
printf 'called\n' >> "$RUNNER_TEMP/producer-called"
case "$PACKAGE_RELEASE_SCRATCH_DIR/" in
  "$RUNNER_TEMP"/*) ;;
  *) echo "scratch is outside runner temp" >&2; exit 91 ;;
esac
case "$PACKAGE_RELEASE_SCRATCH_DIR/" in
  "$GITHUB_WORKSPACE"/*) echo "scratch is inside checkout" >&2; exit 92 ;;
  *) ;;
esac
mkdir -p "$PACKAGE_RELEASE_SCRATCH_DIR"
printf 'disposable build intermediate\n' > "$PACKAGE_RELEASE_SCRATCH_DIR/intermediate"
mkdir -p "$VELNOR_VERIFIED_PACKAGE_DIR"
if [[ "${PACKAGE_TEST_FAIL_DURING_COPY:-}" == "1" ]]; then
  printf 'partial package copy\n' > "$VELNOR_VERIFIED_PACKAGE_DIR/preview-package.tar.gz"
  exit 77
fi
printf 'preview payload for %s\n' "$VELNOR_SOURCE_COMMIT" > "$VELNOR_VERIFIED_PACKAGE_DIR/preview-package.tar.gz"
payload_sha="$(sha256sum "$VELNOR_VERIFIED_PACKAGE_DIR/preview-package.tar.gz" | awk '{print $1}')"
printf '%s  %s\n' "$payload_sha" preview-package.tar.gz > "$VELNOR_VERIFIED_PACKAGE_DIR/SHA256SUMS"
sums_sha="$(sha256sum "$VELNOR_VERIFIED_PACKAGE_DIR/SHA256SUMS" | awk '{print $1}')"
version="0.1.0-${VELNOR_PACKAGE_CHANNEL}.1+${VELNOR_SOURCE_COMMIT:0:7}"
jq -n \
  --arg schema "$EXPECTED_MANIFEST_SCHEMA" \
  --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
  --arg source_ref "$EXPECTED_SOURCE_REF" \
  --arg source_commit "$VELNOR_SOURCE_COMMIT" \
  --arg version "$version" \
  --arg payload "preview-package.tar.gz" \
  --arg payload_sha "$payload_sha" \
  --arg supporting "SHA256SUMS" \
  --arg supporting_sha "$sums_sha" \
  '{schema:$schema,source_repository:$repository,source_ref:$source_ref,source_commit:$source_commit,version:$version,assets:[{name:$payload,sha256:$payload_sha}],supporting_assets:[{name:$supporting,sha256:$supporting_sha}]}' \
  > "$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json"
jq -n \
  --arg repository "$EXPECTED_SOURCE_REPOSITORY" \
  --arg source_ref "$EXPECTED_SOURCE_REF" \
  --arg source_digest "$VELNOR_SOURCE_COMMIT" \
  --slurpfile manifest "$VELNOR_VERIFIED_PACKAGE_DIR/release-manifest.json" \
  '{source_repository:$repository,source_ref:$source_ref,source_digest:$source_digest,manifest:$manifest[0]}' \
  > "$VELNOR_VERIFIED_PACKAGE_DIR/identity.json"
if [[ "${PACKAGE_TEST_STALE_HANDOFF_SIBLING:-}" == "1" ]]; then
  printf 'ignored stale sibling from producer\n' > "${VELNOR_VERIFIED_PACKAGE_DIR%/*}/stale-from-producer"
fi
if [[ -n "${PACKAGE_TEST_CANARY_PATH:-}" ]]; then
  printf 'EXPECTED_SOURCE_COMMIT=%s\n' '0000000000000000000000000000000000000000' >> "$GITHUB_ENV"
  printf '%s\n' "$PACKAGE_TEST_CANARY_PATH" >> "$GITHUB_PATH"
fi
"#
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("fixture file has a parent"))
        .expect("create fixture parent directory");
    fs::write(path, contents).expect("write fixture input");
}

fn generate(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(["--plain", "--force", "--default-branch", "main"])
        .arg(root)
        .output()
        .expect("run workflow generator")
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run fixture Git command");
    assert!(
        output.status.success(),
        "git {args:?} failed:\n{}",
        output_text(&output)
    );
    String::from_utf8(output.stdout)
        .expect("Git fixture output is UTF-8")
        .trim()
        .to_owned()
}

fn named_step<'a>(job: &'a YamlValue, name: &str) -> &'a YamlValue {
    job["steps"]
        .as_sequence()
        .expect("generated job has steps")
        .iter()
        .find(|step| step["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("generated job has no step {name:?}: {job}"))
}

fn step_position(job: &YamlValue, name: &str) -> usize {
    job["steps"]
        .as_sequence()
        .expect("generated job has steps")
        .iter()
        .position(|step| step["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("generated job has no step {name:?}: {job}"))
}

fn effective_env(
    job: &YamlValue,
    step: &YamlValue,
) -> std::collections::BTreeMap<String, YamlValue> {
    let mut values = std::collections::BTreeMap::new();
    for container in [job, step] {
        let Some(mapping) = container
            .as_mapping()
            .and_then(|fields| fields.get("env"))
            .and_then(YamlValue::as_mapping)
        else {
            continue;
        };
        for (key, value) in mapping {
            values.insert(key.as_str().to_owned(), value.clone());
        }
    }
    values
}

fn resolve_value(
    value: &str,
    fixture: &Fixture,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut resolved = value.to_owned();
    let mut replace = |expression: &str, value: &str| {
        let token = format!("${{{{ {expression} }}}}");
        resolved = resolved.replace(&token, value);
    };
    replace("github.repository", &event.repository);
    replace("github.ref", &event.event_ref);
    replace("github.event_name", &event.event_name);
    replace(
        "github.event.before || github.sha",
        if event.before_sha.is_empty() {
            &event.head_sha
        } else {
            &event.before_sha
        },
    );
    replace("github.sha", &event.head_sha);
    replace("github.run_id", &event.run_id);
    replace("github.run_attempt", &event.attempt);
    replace("github.workspace", fixture.root.to_str().unwrap());
    replace("runner.temp", fixture.runner_temp.to_str().unwrap());
    for (expression, key) in [
        ("steps.admit.outputs.disposition", "disposition"),
        ("steps.admit.outputs.head_sha", "head_sha"),
        ("steps.admit.outputs.head_tree", "head_tree"),
        ("needs.admission.outputs.disposition", "disposition"),
        ("needs.admission.outputs.head_sha", "head_sha"),
        ("needs.admission.outputs.head_tree", "head_tree"),
        ("needs.verify.outputs.version", "version"),
        ("needs.verify.outputs.source_commit", "source_commit"),
        ("needs.attest.outputs.version", "version"),
        ("needs.attest.outputs.source_commit", "source_commit"),
    ] {
        if let Some(output) = outputs.get(key) {
            replace(expression, output);
        }
    }
    assert!(
        !resolved.contains("${{"),
        "fixture does not resolve generated GitHub expression {value:?}"
    );
    resolved
}

fn resolved_step_env(
    fixture: &Fixture,
    job: &YamlValue,
    step: &YamlValue,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    effective_env(job, step)
        .into_iter()
        .map(|(key, value)| {
            let value = value
                .as_str()
                .unwrap_or_else(|| panic!("generated environment value for {key} is not text"));
            (key, resolve_value(value, fixture, event, outputs))
        })
        .collect()
}

fn output_values(path: &Path) -> std::collections::BTreeMap<String, String> {
    fs::read_to_string(path)
        .expect("read generated step outputs")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn admission_job_outputs(
    fixture: &Fixture,
    event: &EventContext,
    step_outputs: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let admission_job = &fixture.workflow["jobs"]["admission"];
    let declared = admission_job["outputs"]
        .as_mapping()
        .expect("admission job declares workflow outputs");
    let outputs = declared
        .iter()
        .map(|(key, value)| {
            let key = key.as_str();
            let expression = value
                .as_str()
                .unwrap_or_else(|| panic!("admission output {key} is not an expression"));
            (
                key.to_owned(),
                resolve_value(expression, fixture, event, step_outputs),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected_names = ["disposition", "head_sha", "head_tree"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        outputs.keys().cloned().collect::<BTreeSet<_>>(),
        expected_names,
        "admission exposes only its declared disposition and exact source identity"
    );
    outputs
}

fn execute_rendered_step(
    fixture: &Fixture,
    job_name: &str,
    step_name: &str,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
    fake_producer: bool,
) -> StepResult {
    execute_rendered_step_with_extra_environment(
        fixture,
        job_name,
        step_name,
        event,
        outputs,
        fake_producer,
        &[],
    )
}

fn execute_rendered_step_with_extra_environment(
    fixture: &Fixture,
    job_name: &str,
    step_name: &str,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
    fake_producer: bool,
    extra_environment: &[(&str, &str)],
) -> StepResult {
    execute_rendered_step_with_updates(
        fixture,
        job_name,
        step_name,
        event,
        outputs,
        fake_producer,
        extra_environment,
        &std::collections::BTreeMap::new(),
        &[],
    )
}

fn execute_rendered_step_after_previous(
    fixture: &Fixture,
    job_name: &str,
    step_name: &str,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
    fake_mise: bool,
    previous: &StepResult,
) -> StepResult {
    let (environment_updates, path_updates) = command_file_updates(previous);
    execute_rendered_step_with_updates(
        fixture,
        job_name,
        step_name,
        event,
        outputs,
        fake_mise,
        &[],
        &environment_updates,
        &path_updates,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "Pass all simulated runner inputs explicitly to preserve step isolation in the fixture."
)]
fn execute_rendered_step_with_updates(
    fixture: &Fixture,
    job_name: &str,
    step_name: &str,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
    fake_mise: bool,
    extra_environment: &[(&str, &str)],
    environment_updates: &std::collections::BTreeMap<String, String>,
    path_updates: &[PathBuf],
) -> StepResult {
    let job = &fixture.workflow["jobs"][job_name];
    let step = named_step(job, step_name);
    let script = step["run"]
        .as_str()
        .unwrap_or_else(|| panic!("generated step {step_name:?} has no run script"));
    let mut environment = resolved_step_env(fixture, job, step, event, outputs);
    for (key, value) in extra_environment {
        environment.insert((*key).to_owned(), (*value).to_owned());
    }
    environment.extend(environment_updates.clone());
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let output_path = fixture
        .runner_temp
        .join(format!("step-output-{}-{sequence}", event.run_id));
    let env_path = fixture
        .runner_temp
        .join(format!("step-env-{}-{sequence}", event.run_id));
    let path_path = fixture
        .runner_temp
        .join(format!("step-path-{}-{sequence}", event.run_id));
    fs::write(&output_path, "").expect("create generated step output file");
    fs::write(&env_path, "").expect("create generated step environment file");
    fs::write(&path_path, "").expect("create generated step path file");

    let fake_bin = fixture.runner_temp.join(format!("step-bin-{sequence}"));
    fs::create_dir_all(&fake_bin).expect("create generated step fake-bin directory");
    if fake_mise {
        let producer = fake_bin.join("mise");
        fs::write(&producer, fake_mise_script()).expect("write fake package producer");
        set_executable(&producer);
    }
    let find_shim = fake_bin.join("find");
    fs::write(&find_shim, find_compatibility_script()).expect("write find compatibility shim");
    set_executable(&find_shim);

    let target_binary = PathBuf::from(env!("CARGO_BIN_EXE_velnor-workflow"));
    let target_bin = target_binary
        .parent()
        .expect("generator executable has a parent directory");
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_parts = path_updates
        .iter()
        .map(|path| path.as_os_str().to_os_string())
        .collect::<Vec<_>>();
    path_parts.extend([
        fake_bin.into_os_string(),
        target_bin.as_os_str().to_os_string(),
    ]);
    path_parts.extend(std::env::split_paths(&current_path).map(PathBuf::into_os_string));
    let path = std::env::join_paths(path_parts).expect("join rendered-step PATH");
    environment.insert("PATH".to_owned(), path.to_string_lossy().into_owned());

    let mut command = Command::new("bash");
    command
        .args(["--noprofile", "--norc", "-euo", "pipefail", "-c", script])
        .current_dir(&fixture.root)
        .envs(environment.iter())
        .env("PATH", path)
        .env("GITHUB_WORKSPACE", &fixture.root)
        .env("GITHUB_REPOSITORY", &event.repository)
        .env("GITHUB_EVENT_NAME", &event.event_name)
        .env("GITHUB_REF", &event.event_ref)
        .env("GITHUB_SHA", &event.head_sha)
        .env("GITHUB_RUN_ID", &event.run_id)
        .env("GITHUB_RUN_ATTEMPT", &event.attempt)
        .env("RUNNER_TEMP", &fixture.runner_temp)
        .env("GITHUB_OUTPUT", &output_path)
        .env("GITHUB_ENV", &env_path)
        .env("GITHUB_PATH", &path_path);
    let output = command.output().expect("run actual emitted workflow step");
    StepResult {
        output,
        github_output: output_path,
        github_env: env_path,
        github_path: path_path,
        environment,
    }
}

fn command_file_updates(
    result: &StepResult,
) -> (std::collections::BTreeMap<String, String>, Vec<PathBuf>) {
    let environment = fs::read_to_string(&result.github_env)
        .expect("read generated step environment file")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let path = fs::read_to_string(&result.github_path)
        .expect("read generated step path file")
        .lines()
        .map(PathBuf::from)
        .collect();
    (environment, path)
}

fn find_compatibility_script() -> &'static str {
    r#"#!/bin/bash
set -euo pipefail
for argument in "$@"; do
  if [[ "$argument" == -printf ]]; then
    directory="$1"
    /usr/bin/find "$directory" -maxdepth 1 -type f -print | /usr/bin/sed 's#^.*/##'
    exit 0
  fi
done
exec /usr/bin/find "$@"
"#
}

fn execute_admission_classifier(
    fixture: &Fixture,
    event: &EventContext,
) -> (JsonValue, std::collections::BTreeMap<String, String>) {
    // A GitHub checkout action gives this job the exact event head and full
    // history. Recreate that runner state, then execute the generated
    // classifier script against the fixture repository and real Git diff.
    git(
        &fixture.root,
        &["checkout", "--quiet", "--detach", &event.head_sha],
    );
    let step = execute_rendered_step(
        fixture,
        "admission",
        "Classify complete source push diff",
        event,
        &std::collections::BTreeMap::new(),
        false,
    );
    assert!(
        step.output.status.success(),
        "generated complete-diff classifier failed:\n{}",
        output_text(&step.output)
    );
    let evidence_path = fixture.runner_temp.join("package-release-admission.json");
    let candidate: JsonValue = serde_json::from_slice(
        &fs::read(&evidence_path).expect("generated classifier wrote admission evidence"),
    )
    .expect("generated classifier evidence is JSON");
    assert_eq!(
        candidate["schema"], "homebrew-source-release-admission/v1",
        "the generated step ran the source admission classifier"
    );
    assert_eq!(candidate["repository"], event.repository);
    assert_eq!(candidate["source_ref"], event.event_ref);
    assert_eq!(candidate["configured_source_ref"], SOURCE_REF);
    let step_outputs = output_values(&step.github_output);
    assert_eq!(
        step_outputs.get("head_sha"),
        Some(&event.head_sha),
        "generated classifier exports the exact event head"
    );
    assert_eq!(
        candidate["head_sha"].as_str(),
        Some(event.head_sha.as_str()),
        "classifier evidence is bound to the event head"
    );
    assert_eq!(
        candidate["before_sha"].as_str(),
        Some(event.before_sha.as_str()),
        "classifier evidence is bound to the event before SHA"
    );
    assert_eq!(
        candidate["head_tree"].as_str(),
        Some(git(&fixture.root, &["rev-parse", "HEAD^{tree}"]).as_str()),
        "classifier reports the checked-out event head tree"
    );
    let outputs = admission_job_outputs(fixture, event, &step_outputs);
    assert_eq!(
        outputs.get("disposition").map(String::as_str),
        candidate["disposition"].as_str()
    );
    assert_eq!(outputs.get("head_sha"), Some(&event.head_sha));
    assert_eq!(
        outputs.get("head_tree").map(String::as_str),
        candidate["head_tree"].as_str()
    );
    (candidate, outputs)
}

fn build_gate_allows(
    fixture: &Fixture,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
) -> bool {
    let needs = &fixture.build_job()["needs"];
    let requires_admission = needs.as_str() == Some("admission")
        || needs
            .as_sequence()
            .is_some_and(|jobs| jobs.iter().any(|job| job.as_str() == Some("admission")));
    assert!(
        requires_admission,
        "generated build depends on the admission job before evaluating its gate"
    );
    let expression = fixture.build_job()["if"]
        .as_str()
        .expect("generated build job has an admission condition")
        .trim();
    let body = expression
        .strip_prefix("${{")
        .unwrap_or(expression)
        .strip_suffix("}}")
        .unwrap_or(expression)
        .trim();
    let mut allowed = true;
    for condition in body.split("&&") {
        let (left, right) = condition
            .split_once("==")
            .unwrap_or_else(|| panic!("unsupported generated build condition {condition:?}"));
        let left = left.trim();
        let right = right.trim().trim_matches(['\'', '"']);
        let actual = match left {
            "needs.admission.outputs.disposition" => outputs
                .get("disposition")
                .expect("admission output has disposition")
                .as_str(),
            "github.ref" => event.event_ref.as_str(),
            _ => panic!("unhandled build gate expression: {left}"),
        };
        allowed &= actual == right;
    }
    allowed
}

fn run_package_pipeline(
    fixture: &Fixture,
    event: &EventContext,
    dirty_before_tree_check: bool,
) -> PackageRun {
    run_package_pipeline_with_producer_environment(fixture, event, dirty_before_tree_check, &[])
}

fn run_package_pipeline_with_producer_environment(
    fixture: &Fixture,
    event: &EventContext,
    dirty_before_tree_check: bool,
    producer_environment: &[(&str, &str)],
) -> PackageRun {
    let (candidate, admission_outputs) = execute_admission_classifier(fixture, event);
    run_build_pipeline_with_producer_environment(
        fixture,
        event,
        candidate,
        admission_outputs,
        dirty_before_tree_check,
        producer_environment,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Assert the full build/verifier separation and exact candidate artifact flow together."
)]
fn assert_build_step_order(fixture: &Fixture) {
    let build = fixture.build_job();
    let verify = fixture.verify_job();
    let attest = fixture.attest_job();
    assert!(
        step_position(build, "Checkout source")
            < step_position(build, "Verify admitted source tree")
            && step_position(build, "Verify admitted source tree")
                < step_position(build, "Build package candidate")
            && step_position(build, "Build package candidate")
                < step_position(build, "Run repository package verification tasks")
            && step_position(build, "Run repository package verification tasks")
                < step_position(build, "Upload untrusted package candidate"),
        "build job produces and uploads data after source checks"
    );
    assert!(
        step_position(verify, "Checkout admitted source")
            < step_position(verify, "Verify admitted source tree")
            && step_position(verify, "Verify admitted source tree")
                < step_position(verify, "Require empty package candidate destination")
            && step_position(verify, "Require empty package candidate destination")
                < step_position(verify, "Download untrusted package candidate")
            && step_position(verify, "Download untrusted package candidate")
                < step_position(
                    verify,
                    "Verify manifest, identity, checksums, and exact file set"
                )
            && step_position(
                verify,
                "Verify manifest, identity, checksums, and exact file set"
            ) < step_position(verify, "Run handoff package verification tasks")
            && step_position(verify, "Run handoff package verification tasks")
                < step_position(verify, "Upload verified package handoff"),
        "fresh verifier checks the admitted source and candidate before handing off verified bytes"
    );

    let needs = verify["needs"]
        .as_sequence()
        .expect("fresh verifier declares producer dependencies")
        .iter()
        .filter_map(YamlValue::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(needs, BTreeSet::from(["admission", "build"]));
    assert_eq!(verify["runs-on"].as_str(), Some("ubuntu-24.04"));
    let attest_needs = attest["needs"]
        .as_sequence()
        .expect("signer dependencies are explicit")
        .iter()
        .filter_map(YamlValue::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(attest_needs, BTreeSet::from(["admission", "verify"]));
    assert_eq!(attest["runs-on"].as_str(), Some("ubuntu-24.04"));
    let build_permissions = build["permissions"]
        .as_mapping()
        .expect("producer permissions are explicit");
    assert_eq!(build_permissions.len(), 1);
    assert_eq!(
        build_permissions.get("contents"),
        Some(&YamlValue::String("read".to_owned())),
        "untrusted producer has only source checkout read access, without source-release or tap-write permissions"
    );
    let verify_permissions = verify["permissions"]
        .as_mapping()
        .expect("fresh verifier permissions are explicit");
    assert_eq!(verify_permissions.len(), 1);
    assert_eq!(
        verify_permissions.get("contents"),
        Some(&YamlValue::String("read".to_owned())),
        "fresh verifier has no write or OIDC credential"
    );
    let attest_permissions = attest["permissions"]
        .as_mapping()
        .expect("signer permissions are explicit");
    assert_eq!(attest_permissions.len(), 3);
    for (name, value) in [
        ("contents", "read"),
        ("id-token", "write"),
        ("attestations", "write"),
    ] {
        assert_eq!(
            attest_permissions.get(name),
            Some(&YamlValue::String(value.to_owned())),
            "signer permission {name} is minimal"
        );
    }
    let verify_steps = verify["steps"].as_sequence().expect("fresh verifier steps");
    let handoff_task = named_step(verify, "Run handoff package verification tasks");
    assert!(handoff_task["run"]
        .as_str()
        .is_some_and(|script| script.contains("mise run 'verify-release'")));
    assert_eq!(
        verify["permissions"]["contents"].as_str(),
        Some("read"),
        "source task executes with contents-read permission and no write credential"
    );
    assert!(
        verify_steps.iter().all(|step| {
            step["name"]
                .as_str()
                .is_some_and(|name| name != "Attest declared package assets")
                && step
                    .get("uses")
                    .and_then(YamlValue::as_str)
                    .is_none_or(|action| !action.contains("attest"))
        }),
        "credential-free verifier does not request OIDC attestations"
    );
    let attest_steps = attest["steps"].as_sequence().expect("signer steps");
    let signer_actions = attest_steps
        .iter()
        .filter_map(|step| step.get("uses").and_then(YamlValue::as_str))
        .collect::<Vec<_>>();
    assert_eq!(
        signer_actions,
        [
            "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
            "actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8",
            "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
        ],
        "OIDC signer invokes only the three pinned artifact and attestation actions"
    );
    assert!(
        attest_steps.iter().all(|step| {
            let name = step["name"].as_str().unwrap_or_default();
            let script = step
                .get("run")
                .and_then(YamlValue::as_str)
                .unwrap_or_default();
            !name.contains("Checkout")
                && !name.contains("Mise")
                && !script.contains("mise ")
                && !script.contains("cargo ")
                && !script.contains("velnor-workflow")
                && !script.contains("mise run")
        }),
        "OIDC signer has no checkout, source build, package verifier, or candidate task"
    );
    assert!(
        !attest.as_mapping().unwrap().contains_key("secrets")
            && !attest["env"].as_mapping().unwrap().contains_key("GH_TOKEN"),
        "OIDC signer has no publish secrets"
    );
    assert!(
        step_position(attest, "Require empty verified package destination")
            < step_position(attest, "Download verified package handoff")
            && step_position(attest, "Download verified package handoff")
                < step_position(attest, "Recheck source-bound package bytes")
            && step_position(attest, "Recheck source-bound package bytes")
                < step_position(attest, "Attest declared package assets")
            && step_position(attest, "Attest declared package assets")
                < step_position(attest, "Upload attested package handoff"),
        "signer rechecks verified immutable bytes before attesting and handing off exact files"
    );
    assert_eq!(
        fixture.publish_job()["needs"].as_str(),
        Some("attest"),
        "credentialed publisher waits for fresh verification and signer"
    );
    let publish = fixture.publish_job();
    assert_eq!(publish["runs-on"].as_str(), Some("ubuntu-24.04"));
    assert_eq!(publish["permissions"]["contents"].as_str(), Some("write"));
    assert!(publish["permissions"].get("id-token").is_none());
    assert!(publish["permissions"].get("pull-requests").is_none());
    let publish_text = serde_yaml::to_string(publish).expect("serialize release writer");
    assert!(!publish_text.contains("verify-release"));
    assert!(!publish_text.contains("TAP_TOKEN"));

    let verify_published = &fixture.workflow["jobs"]["verify_published"];
    let verify_published_needs = verify_published["needs"]
        .as_sequence()
        .expect("published verifier job dependencies")
        .iter()
        .filter_map(YamlValue::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        verify_published_needs,
        BTreeSet::from(["admission", "publish"])
    );
    assert_eq!(verify_published["runs-on"].as_str(), Some("ubuntu-24.04"));
    assert_eq!(
        verify_published["permissions"]["contents"].as_str(),
        Some("read")
    );
    assert_eq!(
        verify_published["permissions"]["attestations"].as_str(),
        Some("read")
    );
    let published_task = named_step(verify_published, "Run published package verification tasks");
    assert!(published_task["run"]
        .as_str()
        .is_some_and(|script| script.contains("mise run 'verify-release'")));
    assert_eq!(
        step_position(verify_published, "Run published package verification tasks"),
        verify_published["steps"].as_sequence().unwrap().len() - 1,
        "source-controlled post-publication verification is the final job step"
    );
    assert!(!verify_published["env"]
        .as_mapping()
        .unwrap()
        .contains_key("GH_TOKEN"));
    assert!(
        named_step(verify_published, "Download immutable published release")["env"]["GH_TOKEN"]
            .as_str()
            .is_some_and(|value| value == "${{ github.token }}"),
        "published read token is scoped to fixed release download"
    );

    let consumer = &fixture.workflow["jobs"]["consumer"];
    assert_eq!(consumer["needs"].as_str(), Some("verify_published"));
    assert_eq!(consumer["runs-on"].as_str(), Some("ubuntu-24.04"));
    assert_eq!(consumer["permissions"]["contents"].as_str(), Some("read"));
    assert_eq!(
        consumer["permissions"]["attestations"].as_str(),
        Some("read")
    );
    let consumer_steps = consumer["steps"].as_sequence().expect("consumer steps");
    let consumer_order = [
        "Download immutable release for consumer",
        "Re-verify immutable release for consumer",
        "Verify consumer release attestations",
        "Verify immutable consumer package identity",
        "Checkout consumer repository",
        "Run updater and create or update consumer PR",
    ];
    let consumer_indices = consumer_order
        .iter()
        .map(|name| step_position(consumer, name))
        .collect::<Vec<_>>();
    assert!(consumer_indices.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(consumer_steps.iter().all(|step| {
        step["name"].as_str().is_none_or(|name| {
            name == "Checkout consumer repository"
                || name == "Run updater and create or update consumer PR"
                || !serde_yaml::to_string(step)
                    .expect("serialize consumer step")
                    .contains("TAP_TOKEN")
        })
    }));
    assert_eq!(
        serde_yaml::to_string(consumer)
            .expect("serialize consumer job")
            .matches("secrets.TAP_TOKEN")
            .count(),
        3
    );
    let candidate_name =
        "${{ format('package-release-candidate-{0}', needs.admission.outputs.head_sha) }}";
    assert_eq!(
        named_step(build, "Upload untrusted package candidate")["with"]["name"].as_str(),
        Some(candidate_name),
        "candidate artifact identity is bound to admitted SHA"
    );
    assert_eq!(
        named_step(verify, "Download untrusted package candidate")["with"]["name"].as_str(),
        Some(candidate_name),
        "fresh verifier downloads the exact producer artifact"
    );
    assert_eq!(
        named_step(verify, "Upload verified package handoff")["with"]["name"].as_str(),
        Some("${{ format('package-release-{0}', steps.verify.outputs.source_commit) }}"),
        "verified artifact identity is bound to the manifest-verified source SHA"
    );
    assert_eq!(
        named_step(attest, "Download verified package handoff")["with"]["name"].as_str(),
        Some("${{ format('package-release-{0}', needs.verify.outputs.source_commit) }}"),
        "signer downloads only the exact fresh-verifier artifact"
    );
    assert_eq!(
        named_step(attest, "Upload attested package handoff")["with"]["name"].as_str(),
        Some("${{ format('package-release-attested-{0}', needs.verify.outputs.source_commit) }}"),
        "attested artifact identity is bound to verified source SHA"
    );
    assert_eq!(
        named_step(fixture.publish_job(), "Download verified package handoff")["with"]["name"]
            .as_str(),
        Some("${{ format('package-release-attested-{0}', needs.attest.outputs.source_commit) }}"),
        "publisher downloads only the exact attested artifact"
    );
}

fn run_build_pipeline(
    fixture: &Fixture,
    event: &EventContext,
    candidate: JsonValue,
    admission_outputs: std::collections::BTreeMap<String, String>,
    dirty_before_tree_check: bool,
) -> PackageRun {
    run_build_pipeline_with_producer_environment(
        fixture,
        event,
        candidate,
        admission_outputs,
        dirty_before_tree_check,
        &[],
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep producer output construction and environment-poison controls bound to one run."
)]
fn run_build_pipeline_with_producer_environment(
    fixture: &Fixture,
    event: &EventContext,
    candidate: JsonValue,
    admission_outputs: std::collections::BTreeMap<String, String>,
    dirty_before_tree_check: bool,
    producer_environment: &[(&str, &str)],
) -> PackageRun {
    if !build_gate_allows(fixture, event, &admission_outputs) {
        return PackageRun {
            candidate,
            admission_outputs,
            build_ran: false,
            source_check: None,
            build: None,
            producer_followup: None,
            verification: None,
            checkout_sha: None,
            verification_fixture: None,
        };
    }

    assert_build_step_order(fixture);
    let checkout = named_step(fixture.build_job(), "Checkout source");
    let checkout_ref = resolve_value(
        checkout["with"]["ref"]
            .as_str()
            .expect("build checkout ref"),
        fixture,
        event,
        &admission_outputs,
    );
    assert_eq!(
        checkout_ref,
        candidate["head_sha"]
            .as_str()
            .expect("verified event head SHA"),
        "build ref resolves from the exact already-admitted event SHA"
    );
    git(
        &fixture.root,
        &["checkout", "--quiet", "--detach", &checkout_ref],
    );
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), checkout_ref);

    if dirty_before_tree_check {
        write(
            &fixture.root,
            "src/release.txt",
            "dirty before source verification\n",
        );
    }

    let tree_check = execute_rendered_step(
        fixture,
        "build",
        "Verify admitted source tree",
        event,
        &admission_outputs,
        false,
    );
    if !tree_check.output.status.success() {
        return PackageRun {
            candidate,
            admission_outputs,
            build_ran: false,
            source_check: Some(tree_check),
            build: None,
            producer_followup: None,
            verification: None,
            checkout_sha: Some(checkout_ref),
            verification_fixture: None,
        };
    }

    let build = execute_rendered_step_with_extra_environment(
        fixture,
        "build",
        "Build package candidate",
        event,
        &admission_outputs,
        true,
        producer_environment,
    );
    if !build.output.status.success() {
        return PackageRun {
            candidate,
            admission_outputs,
            build_ran: true,
            source_check: Some(tree_check),
            build: Some(build),
            producer_followup: None,
            verification: None,
            checkout_sha: Some(checkout_ref),
            verification_fixture: None,
        };
    }

    let producer_followup = execute_rendered_step_after_previous(
        fixture,
        "build",
        "Run repository package verification tasks",
        event,
        &admission_outputs,
        true,
        &build,
    );
    if !producer_followup.output.status.success() {
        return PackageRun {
            candidate,
            admission_outputs,
            build_ran: true,
            source_check: Some(tree_check),
            build: Some(build),
            producer_followup: Some(producer_followup),
            verification: None,
            checkout_sha: Some(checkout_ref),
            verification_fixture: None,
        };
    }

    let verification_fixture = fixture.fresh_runner("verify", &checkout_ref);
    let verify_checkout = named_step(
        verification_fixture.verify_job(),
        "Checkout admitted source",
    );
    assert_eq!(
        verify_checkout["with"]["ref"].as_str(),
        Some("${{ needs.admission.outputs.head_sha }}"),
        "fresh verifier checkout is pinned to the admitted source SHA"
    );
    assert_eq!(
        git(
            &verification_fixture.root.join("source"),
            &["rev-parse", "HEAD"]
        ),
        checkout_ref,
        "fresh verifier runner is checked out at the same admitted SHA"
    );
    let verifier_source_check = execute_rendered_step(
        &verification_fixture,
        "verify",
        "Verify admitted source tree",
        event,
        &admission_outputs,
        false,
    );
    if !verifier_source_check.output.status.success() {
        return PackageRun {
            candidate,
            admission_outputs,
            build_ran: true,
            source_check: Some(tree_check),
            build: Some(build),
            producer_followup: Some(producer_followup),
            verification: None,
            checkout_sha: Some(checkout_ref),
            verification_fixture: Some(verification_fixture),
        };
    }
    let empty_destination = execute_rendered_step(
        &verification_fixture,
        "verify",
        "Require empty package candidate destination",
        event,
        &admission_outputs,
        false,
    );
    if !empty_destination.output.status.success() {
        return PackageRun {
            candidate,
            admission_outputs,
            build_ran: true,
            source_check: Some(tree_check),
            build: Some(build),
            producer_followup: Some(producer_followup),
            verification: Some(empty_destination),
            checkout_sha: Some(checkout_ref),
            verification_fixture: Some(verification_fixture),
        };
    }
    let producer_handoff = PathBuf::from(
        build
            .environment
            .get("VELNOR_VERIFIED_PACKAGE_DIR")
            .expect("producer job exposes package output directory"),
    );
    let verifier_handoff = verification_fixture.root.join("package");
    let candidate_upload = named_step(fixture.build_job(), "Upload untrusted package candidate");
    assert!(
        candidate_upload["with"]["path"].as_str().is_some(),
        "producer uploads an explicit fixed list of candidate files"
    );
    copy_package_assets(&producer_handoff, &verifier_handoff);
    let verification = execute_rendered_step(
        &verification_fixture,
        "verify",
        "Verify manifest, identity, checksums, and exact file set",
        event,
        &admission_outputs,
        false,
    );
    PackageRun {
        candidate,
        admission_outputs,
        build_ran: true,
        source_check: Some(tree_check),
        build: Some(build),
        producer_followup: Some(producer_followup),
        verification: Some(verification),
        checkout_sha: Some(checkout_ref),
        verification_fixture: Some(verification_fixture),
    }
}

fn copy_package_assets(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("create downloaded candidate directory");
    for name in ["release-manifest.json", "identity.json", PAYLOAD, SUMS] {
        fs::copy(source.join(name), destination.join(name))
            .unwrap_or_else(|error| panic!("copy candidate asset {name}: {error}"));
    }
}

fn reset_package_assets(source: &Path, destination: &Path) {
    if destination.exists() {
        fs::remove_dir_all(destination).expect("remove previous signer fixture package");
    }
    copy_package_assets(source, destination);
}

fn assert_signer_rejects(
    signer_fixture: &Fixture,
    event: &EventContext,
    outputs: &std::collections::BTreeMap<String, String>,
    verified_handoff: &Path,
    label: &str,
    expected_error: &str,
    mutate: impl FnOnce(&Path),
) {
    let package = signer_fixture.root.join("package");
    reset_package_assets(verified_handoff, &package);
    mutate(&package);
    let result = execute_rendered_step(
        signer_fixture,
        "attest",
        "Recheck source-bound package bytes",
        event,
        outputs,
        false,
    );
    assert_step_failure(&result, label);
    assert!(
        output_text(&result.output).contains(expected_error),
        "{label} failed for the expected reason; expected {expected_error:?}, got: {}",
        output_text(&result.output)
    );
}

fn read_json(path: &Path) -> JsonValue {
    serde_json::from_slice(&fs::read(path).expect("read JSON fixture"))
        .expect("fixture file is JSON")
}

fn verify_handoff_again(_fixture: &Fixture, event: &EventContext, run: &PackageRun) -> StepResult {
    let verification_fixture = run
        .verification_fixture
        .as_ref()
        .expect("fresh verifier runner exists");
    execute_rendered_step(
        verification_fixture,
        "verify",
        "Verify manifest, identity, checksums, and exact file set",
        event,
        &run.admission_outputs,
        false,
    )
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .expect("read fake producer permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake producer executable");
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn sha256(path: &Path) -> String {
    let bytes = fs::read(path).expect("read package bytes for digest assertion");
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        encoded.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn assert_complete_handoff(path: &Path, source_sha: &str) {
    assert!(
        path.is_dir(),
        "declared package handoff survives: {}",
        path.display()
    );
    let actual = fs::read_dir(path)
        .expect("read completed package handoff")
        .map(|entry| entry.expect("read package directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    let expected = [
        "SHA256SUMS",
        "identity.json",
        "release-manifest.json",
        "preview-package.tar.gz",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(
        actual, expected,
        "handoff contains only declared output files"
    );

    let manifest: JsonValue = serde_json::from_slice(
        &fs::read(path.join("release-manifest.json")).expect("read release manifest"),
    )
    .expect("release manifest is JSON");
    assert_eq!(manifest["schema"], MANIFEST_SCHEMA);
    assert_eq!(manifest["source_repository"], REPOSITORY);
    assert_eq!(manifest["source_ref"], SOURCE_REF);
    assert_eq!(manifest["source_commit"], source_sha);
    assert_eq!(manifest["assets"][0]["name"], PAYLOAD);
    assert_eq!(
        manifest["assets"][0]["sha256"],
        sha256(&path.join(PAYLOAD)),
        "manifest digest matches the producer payload bytes"
    );
    assert_eq!(manifest["supporting_assets"][0]["name"], SUMS);
    assert_eq!(
        manifest["supporting_assets"][0]["sha256"],
        sha256(&path.join(SUMS)),
        "manifest digest matches the checksum sidecar bytes"
    );
    let identity: JsonValue = serde_json::from_slice(
        &fs::read(path.join("identity.json")).expect("read package identity"),
    )
    .expect("package identity is JSON");
    assert_eq!(identity["source_repository"], REPOSITORY);
    assert_eq!(identity["source_ref"], SOURCE_REF);
    assert_eq!(identity["source_digest"], source_sha);
    assert_eq!(identity["manifest"], manifest);
}

fn handoff_path(run: &PackageRun) -> PathBuf {
    run.verification_fixture
        .as_ref()
        .expect("fresh verifier runner exists")
        .root
        .join("package")
}

fn producer_handoff_path(run: &PackageRun) -> PathBuf {
    PathBuf::from(
        run.build
            .as_ref()
            .expect("producer step ran")
            .environment
            .get("VELNOR_VERIFIED_PACKAGE_DIR")
            .expect("emitted producer job env defines its package output directory"),
    )
}

fn scratch_path(run: &PackageRun) -> PathBuf {
    let raw = run
        .build
        .as_ref()
        .expect("producer step ran")
        .environment
        .get("PACKAGE_RELEASE_SCRATCH_DIR")
        .expect("emitted job env defines disposable scratch");
    PathBuf::from(raw)
}

fn assert_step_success(result: &StepResult, label: &str) {
    assert!(
        result.output.status.success(),
        "generated {label} step failed:\n{}",
        output_text(&result.output)
    );
}

fn assert_step_failure(result: &StepResult, label: &str) {
    assert!(
        !result.output.status.success(),
        "generated {label} step unexpectedly succeeded:\n{}",
        output_text(&result.output)
    );
}

fn assert_package_pipeline_success(fixture: &Fixture, run: &PackageRun, expected_sha: &str) {
    assert_eq!(run.candidate["disposition"], "admit");
    assert!(
        run.build_ran,
        "admitted source passed the generated build gate"
    );
    assert_eq!(run.checkout_sha.as_deref(), Some(expected_sha));
    assert_step_success(
        run.source_check.as_ref().expect("source tree check ran"),
        "admitted source tree check",
    );
    assert_step_success(run.build.as_ref().unwrap(), "package producer");
    assert_step_success(
        run.producer_followup
            .as_ref()
            .expect("producer follow-up task ran"),
        "producer follow-up verification task",
    );
    assert_step_success(
        run.verification.as_ref().unwrap(),
        "manifest and identity verification",
    );
    let handoff = handoff_path(run);
    assert_complete_handoff(&handoff, expected_sha);
    let manifest: JsonValue =
        serde_json::from_slice(&fs::read(handoff.join("release-manifest.json")).unwrap()).unwrap();
    let verification_outputs = output_values(&run.verification.as_ref().unwrap().github_output);
    assert_eq!(
        verification_outputs
            .get("source_commit")
            .map(String::as_str),
        Some(expected_sha),
        "generated verifier exports the exact verified source SHA"
    );
    assert_eq!(
        verification_outputs.get("version").map(String::as_str),
        manifest["version"].as_str(),
        "generated verifier exports the manifest version"
    );
    let scratch = scratch_path(run);
    assert!(
        scratch.starts_with(&fixture.runner_temp),
        "scratch remains under runner.temp: {}",
        scratch.display()
    );
    assert!(
        !scratch.starts_with(&fixture.root),
        "scratch is outside the source checkout"
    );
    assert!(
        !scratch.exists(),
        "producer boundary cleaned its scratch directory"
    );
}

fn valid_package_fixture(
    label: &str,
    run_id: &str,
) -> (Fixture, String, EventContext, PackageRun, PathBuf) {
    let fixture = Fixture::new(label, "dist");
    let source_sha =
        fixture.commit_source_change(&format!("{label} source bytes\n"), "production source");
    let event = EventContext::push(&fixture.initial_sha, &source_sha, run_id, "1");
    let run = run_package_pipeline(&fixture, &event, false);
    assert_package_pipeline_success(&fixture, &run, &source_sha);
    assert!(run.candidate["changed_paths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|change| change["path"] == "src/release.txt" && change["status"] == "modified"));
    let handoff = handoff_path(&run);
    (fixture, source_sha, event, run, handoff)
}

fn producer_was_called(fixture: &Fixture) -> bool {
    fixture.runner_temp.join("producer-called").exists()
}

fn rewrite_json(path: &Path, value: &JsonValue) {
    fs::write(
        path,
        serde_json::to_vec(value).expect("serialize modified JSON"),
    )
    .expect("write adversarial package JSON");
}

fn sync_identity_manifest(handoff: &Path, manifest: &JsonValue) {
    let identity_path = handoff.join("identity.json");
    let mut identity: JsonValue =
        serde_json::from_slice(&fs::read(&identity_path).expect("read identity for manifest sync"))
            .expect("identity is JSON before manifest sync");
    identity["manifest"] = manifest.clone();
    rewrite_json(&identity_path, &identity);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "Keep the producer, fresh verifier, signer, and publisher handoff proof in one native case."
)]
fn handoff_survives_producer_exit() {
    let fixture = Fixture::new("producer-exit", "dist");
    let before = fixture.initial_sha.clone();
    let source_sha = fixture.commit_source_change("producer bytes\n", "production source");
    let event = EventContext::push(&before, &source_sha, "7001", "1");
    let canary_bin = fixture.runner_temp.join("producer-command-file-canary");
    fs::create_dir_all(&canary_bin).expect("create producer command-file PATH canary");
    let canary_jq = canary_bin.join("jq");
    fs::write(&canary_jq, "#!/bin/bash\nprintf 'CANARY_JQ\\n'\n")
        .expect("write producer command-file PATH canary");
    set_executable(&canary_jq);
    let canary_path = canary_bin.to_string_lossy().into_owned();
    let run = run_package_pipeline_with_producer_environment(
        &fixture,
        &event,
        false,
        &[("PACKAGE_TEST_CANARY_PATH", canary_path.as_str())],
    );
    assert_package_pipeline_success(&fixture, &run, &source_sha);
    let producer_followup = run.producer_followup.as_ref().unwrap();
    assert_eq!(
        producer_followup
            .environment
            .get("EXPECTED_SOURCE_COMMIT")
            .map(String::as_str),
        Some("0000000000000000000000000000000000000000"),
        "same-job follow-up receives the producer's real GITHUB_ENV update"
    );
    assert_eq!(
        fs::read_to_string(fixture.runner_temp.join("followup-source-commit"))
            .unwrap()
            .trim(),
        "0000000000000000000000000000000000000000"
    );
    assert_eq!(
        fs::read_to_string(fixture.runner_temp.join("followup-jq-version"))
            .unwrap()
            .trim(),
        "CANARY_JQ",
        "same-job follow-up PATH resolves the producer's GITHUB_PATH canary first"
    );
    let verification = run.verification.as_ref().unwrap();
    assert_eq!(
        verification
            .environment
            .get("EXPECTED_SOURCE_COMMIT")
            .map(String::as_str),
        Some(source_sha.as_str()),
        "fresh verifier re-pins source identity from admission"
    );
    assert!(
        !verification.environment["PATH"].contains(&canary_path),
        "fresh verifier PATH has no producer command-file entries"
    );
    assert_step_success(verification, "fresh isolated package verifier");
    let verified_handoff = handoff_path(&run);
    let signer_fixture = run
        .verification_fixture
        .as_ref()
        .unwrap()
        .fresh_workspace("attest");
    let signer_package = signer_fixture.root.join("package");
    copy_package_assets(&verified_handoff, &signer_package);
    let manifest: JsonValue =
        serde_json::from_slice(&fs::read(signer_package.join("release-manifest.json")).unwrap())
            .unwrap();
    let mut signer_outputs = run.admission_outputs.clone();
    signer_outputs.insert(
        "version".to_owned(),
        manifest["version"].as_str().unwrap().to_owned(),
    );
    signer_outputs.insert("source_commit".to_owned(), source_sha.clone());

    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer detects post-verification payload mutation",
        "attestation input digest mismatch",
        |package| {
            fs::write(package.join(PAYLOAD), b"changed after fresh verification\n").unwrap();
        },
    );
    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer rejects a different admitted source ref",
        "attestation manifest does not match admitted source identity",
        |package| {
            let manifest_path = package.join("release-manifest.json");
            let mut changed = read_json(&manifest_path);
            changed["source_ref"] = JsonValue::String("refs/heads/attacker".to_owned());
            rewrite_json(&manifest_path, &changed);
        },
    );
    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer rejects a different admitted source SHA",
        "attestation manifest does not match admitted source identity",
        |package| {
            let manifest_path = package.join("release-manifest.json");
            let mut changed = read_json(&manifest_path);
            let alternate_sha = if source_sha == "f".repeat(40) {
                "e".repeat(40)
            } else {
                "f".repeat(40)
            };
            changed["source_commit"] = JsonValue::String(alternate_sha);
            rewrite_json(&manifest_path, &changed);
        },
    );
    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer rejects a version bound to another source identity",
        "attestation input version does not bind the admitted SHA",
        |package| {
            let manifest_path = package.join("release-manifest.json");
            let mut changed = read_json(&manifest_path);
            let alternate = if source_sha.starts_with("aaaaaaa") {
                "bbbbbbb"
            } else {
                "aaaaaaa"
            };
            changed["version"] = JsonValue::String(format!("1.2.3-preview.7001+{alternate}"));
            rewrite_json(&manifest_path, &changed);
            sync_identity_manifest(package, &changed);
        },
    );
    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer rejects an extra undeclared file",
        "attestation input contains an undeclared or missing file",
        |package| fs::write(package.join("undeclared.txt"), b"extra\n").unwrap(),
    );
    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer rejects a missing declared file",
        "attestation input contains an undeclared or missing file",
        |package| fs::remove_file(package.join(PAYLOAD)).unwrap(),
    );
    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer rejects a mutated manifest schema",
        "attestation manifest does not match admitted source identity",
        |package| {
            let manifest_path = package.join("release-manifest.json");
            let mut changed = read_json(&manifest_path);
            changed["schema"] = JsonValue::String("unapproved.schema".to_owned());
            rewrite_json(&manifest_path, &changed);
        },
    );
    assert_signer_rejects(
        &signer_fixture,
        &event,
        &signer_outputs,
        &verified_handoff,
        "signer rejects a mutated identity record",
        "attestation identity does not match manifest and admitted source",
        |package| {
            let identity_path = package.join("identity.json");
            let mut changed = read_json(&identity_path);
            changed["source_repository"] = JsonValue::String("other/repository".to_owned());
            rewrite_json(&identity_path, &changed);
        },
    );

    reset_package_assets(&verified_handoff, &signer_package);
    let signer = execute_rendered_step(
        &signer_fixture,
        "attest",
        "Recheck source-bound package bytes",
        &event,
        &signer_outputs,
        false,
    );
    assert_step_success(&signer, "bytes-only attestation input validation");
    let signer_values = output_values(&signer.github_output);
    assert_eq!(signer_values.get("source_commit"), Some(&source_sha));
    assert_eq!(signer_values.get("version"), signer_outputs.get("version"));
    assert!(
        !signer.environment["PATH"].contains(&canary_path),
        "fresh OIDC signer PATH has no producer command-file entries"
    );
    assert!(
        producer_was_called(&fixture),
        "the emitted mise task ran as a child process"
    );
    assert!(
        fixture.root.is_dir(),
        "source checkout remains owned by its caller"
    );
}

#[test]
fn scratch_cleanup_preserves_output() {
    let fixture = Fixture::new("owned-cleanup", "dist");
    let before = fixture.initial_sha.clone();
    let source_sha = fixture.commit_source_change("scratch test bytes\n", "production source");
    let unrelated = fixture.runner_temp.join("unowned/keep.txt");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    fs::write(&unrelated, "caller-owned\n").unwrap();

    let first_event = EventContext::push(&before, &source_sha, "7100", "1");
    let first = run_package_pipeline(&fixture, &first_event, false);
    assert_package_pipeline_success(&fixture, &first, &source_sha);
    let first_scratch = scratch_path(&first);
    let first_handoff = producer_handoff_path(&first);
    assert!(
        fixture.runner_temp.is_dir(),
        "runner-temp parent was preserved"
    );
    assert_eq!(fs::read_to_string(&unrelated).unwrap(), "caller-owned\n");

    fs::remove_dir_all(&first_handoff).unwrap();
    let retry_event = EventContext::push(&before, &source_sha, "7100", "2");
    let retry = run_package_pipeline(&fixture, &retry_event, false);
    assert_package_pipeline_success(&fixture, &retry, &source_sha);
    let retry_scratch = scratch_path(&retry);
    assert_ne!(
        first_scratch, retry_scratch,
        "each attempt has a distinct scratch path"
    );
    assert_eq!(fs::read_to_string(&unrelated).unwrap(), "caller-owned\n");
    assert!(fixture.root.is_dir(), "checkout parent was preserved");

    fs::remove_dir_all(producer_handoff_path(&retry)).unwrap();
    let failure_event = EventContext::push(&before, &source_sha, "7100", "3");
    let failed = run_package_pipeline_with_producer_environment(
        &fixture,
        &failure_event,
        false,
        &[("PACKAGE_TEST_FAIL_DURING_COPY", "1")],
    );
    assert_eq!(failed.candidate["disposition"], "admit");
    assert!(
        failed.build_ran,
        "admitted source reached the package producer"
    );
    assert!(
        producer_was_called(&fixture),
        "producer ran before the simulated copy failure"
    );
    let producer = failed.build.as_ref().expect("producer step ran");
    assert_step_failure(producer, "producer interrupted during package copy");
    assert!(
        failed.verification.is_none(),
        "failed producer prevents the generated verifier from running"
    );
    let partial_handoff = producer_handoff_path(&failed);
    assert!(partial_handoff.starts_with(fixture.root.join("dist")));
    assert_eq!(
        fs::read_to_string(partial_handoff.join("preview-package.tar.gz")).unwrap(),
        "partial package copy\n",
        "failed copy leaves a truncated payload at the declared handoff path"
    );
    let partial_verifier = fixture.fresh_runner("partial-verifier", &failure_event.head_sha);
    let partial_verifier_handoff = partial_verifier.root.join("package");
    fs::create_dir(&partial_verifier_handoff).unwrap();
    fs::copy(
        partial_handoff.join(PAYLOAD),
        partial_verifier_handoff.join(PAYLOAD),
    )
    .unwrap();
    let partial_verification = execute_rendered_step(
        &partial_verifier,
        "verify",
        "Verify manifest, identity, checksums, and exact file set",
        &failure_event,
        &failed.admission_outputs,
        false,
    );
    assert_step_failure(&partial_verification, "partial handoff rejection");
    let failed_scratch = scratch_path(&failed);
    assert!(failed_scratch.starts_with(&fixture.runner_temp));
    assert!(!failed_scratch.starts_with(&fixture.root));
    assert!(
        !failed_scratch.exists(),
        "producer failure still cleans only its owned scratch directory"
    );
    assert!(fixture.runner_temp.is_dir());
    assert_eq!(fs::read_to_string(&unrelated).unwrap(), "caller-owned\n");
}

#[test]
fn queued_sha_is_accepted_after_main_moves() {
    let fixture = Fixture::new("queued-sha", "dist");
    let before = fixture.initial_sha.clone();
    let older = fixture.commit_source_change("older queued source\n", "older preview candidate");
    let older_event = EventContext::push(&before, &older, "7201", "1");
    let (older_candidate, older_admission_outputs) =
        execute_admission_classifier(&fixture, &older_event);
    assert_eq!(older_candidate["disposition"], "admit");
    assert_eq!(older_admission_outputs["head_sha"], older);
    assert_eq!(
        git(&fixture.root, &["rev-parse", "main"]),
        older,
        "the older event was admitted while it was main's tip"
    );

    git(&fixture.root, &["checkout", "--quiet", "main"]);
    let newer = fixture.commit_source_change("newer main source\n", "newer preview candidate");
    assert_ne!(older, newer);
    let ancestry = Command::new("git")
        .current_dir(&fixture.root)
        .args(["merge-base", "--is-ancestor", &older, &newer])
        .status()
        .expect("prove later main contains the older admitted source SHA");
    assert!(
        ancestry.success(),
        "newer candidate descends from older candidate"
    );

    let newer_event = EventContext::push(&older, &newer, "7200", "1");
    let newer_run = run_package_pipeline(&fixture, &newer_event, false);
    assert_package_pipeline_success(&fixture, &newer_run, &newer);
    assert_eq!(git(&fixture.root, &["rev-parse", "main"]), newer);

    fs::remove_dir_all(producer_handoff_path(&newer_run)).unwrap();
    let older_run = run_build_pipeline(
        &fixture,
        &older_event,
        older_candidate,
        older_admission_outputs,
        false,
    );
    assert_package_pipeline_success(&fixture, &older_run, &older);
    assert_eq!(
        older_run.candidate["head_sha"], older,
        "the producer boundary retained the queued earlier push SHA"
    );
    assert_eq!(
        git(&fixture.root, &["rev-parse", "main"]),
        newer,
        "main remains newer while the older admitted package is produced"
    );
    assert_eq!(
        git(&fixture.root, &["rev-parse", "HEAD"]),
        older,
        "the generated checkout ref used the admission job's exact SHA"
    );
    assert_complete_handoff(&handoff_path(&older_run), &older);

    git(
        &fixture.root,
        &["checkout", "--quiet", "-b", "unrelated-force-push", &before],
    );
    let unrelated =
        fixture.commit_source_change("unrelated source history\n", "unrelated force-push head");
    assert_eq!(
        git(&fixture.root, &["rev-parse", "main"]),
        newer,
        "the unrelated force-push head does not move main"
    );
    let non_ancestor = Command::new("git")
        .current_dir(&fixture.root)
        .args(["merge-base", "--is-ancestor", &newer, &unrelated])
        .status()
        .expect("check unrelated history is not descended from the former main head");
    assert!(
        !non_ancestor.success(),
        "force-push control has an unrelated before/head history"
    );

    let force_push_event = EventContext::push(&newer, &unrelated, "7202", "1");
    let stale_admission = fixture.runner_temp.join("package-release-admission.json");
    let _ = fs::remove_file(&stale_admission);
    let force_push_classifier = execute_rendered_step(
        &fixture,
        "admission",
        "Classify complete source push diff",
        &force_push_event,
        &std::collections::BTreeMap::new(),
        false,
    );
    assert_step_failure(
        &force_push_classifier,
        "non-ancestor force-push admission rejection",
    );
    assert!(
        output_values(&force_push_classifier.github_output).is_empty(),
        "unrelated before/head history exports no admission identity"
    );
    assert!(
        !stale_admission.exists() || fs::metadata(&stale_admission).unwrap().len() == 0,
        "unrelated before/head history writes no admission record"
    );
}

#[test]
fn unadmitted_dispatch_sha_is_rejected() {
    let fixture = Fixture::new("dispatch-rejected", "dist");
    let admitted_main = fixture.initial_sha.clone();
    git(
        &fixture.root,
        &["checkout", "--quiet", "-b", "untrusted-preview"],
    );
    let arbitrary_sha =
        fixture.commit_source_change("manual source bytes\n", "manual dispatch SHA");
    let event = EventContext::dispatch(&arbitrary_sha, "7300", "1");
    let run = run_package_pipeline(&fixture, &event, false);
    assert_eq!(run.candidate["disposition"], "skip");
    assert!(run.candidate["changed_paths"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(run.candidate["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("not a qualifying push")));
    assert!(
        !run.build_ran,
        "emitted job condition blocks an unadmitted dispatch"
    );
    assert!(run.build.is_none() && run.verification.is_none());
    assert!(
        !producer_was_called(&fixture),
        "dispatch never runs the producer task"
    );
    assert!(
        !fixture.root.join("dist").exists(),
        "dispatch creates no handoff"
    );
    assert_eq!(git(&fixture.root, &["rev-parse", "main"]), admitted_main);
    assert_eq!(git(&fixture.root, &["rev-parse", "HEAD"]), arbitrary_sha);

    let wrong_origin = Fixture::new("wrong-source-origin", "dist");
    let wrong_origin_sha =
        wrong_origin.commit_source_change("origin mismatch bytes\n", "production source");
    let mut wrong_origin_event =
        EventContext::push(&wrong_origin.initial_sha, &wrong_origin_sha, "7301", "1");
    wrong_origin_event.repository = "example/untrusted-source".to_owned();
    git(
        &wrong_origin.root,
        &["checkout", "--quiet", "--detach", &wrong_origin_sha],
    );
    let classifier = execute_rendered_step(
        &wrong_origin,
        "admission",
        "Classify complete source push diff",
        &wrong_origin_event,
        &std::collections::BTreeMap::new(),
        false,
    );
    assert_step_failure(&classifier, "source repository origin mismatch");
    assert!(
        output_text(&classifier.output).contains("does not match configured source"),
        "emitted classifier rejects the event repository against its configured origin"
    );
    assert!(
        output_values(&classifier.github_output).is_empty(),
        "origin mismatch exports no admission identity"
    );
    assert!(
        !producer_was_called(&wrong_origin),
        "source-origin mismatch never reaches the package producer"
    );
}

fn outside_sentinel() -> PathBuf {
    let outside = std::env::temp_dir().join(format!(
        "package-handoff-outside-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "outside caller data\n").unwrap();
    outside
}

fn assert_traversal_path_rejected(outside: &Path) {
    let invalid_parent = std::env::temp_dir().join(format!(
        "package-handoff-invalid-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&invalid_parent).unwrap();
    let invalid_root = invalid_parent.join("source");
    fs::create_dir_all(&invalid_root).unwrap();
    let traversal_sentinel = invalid_parent.join("outside/keep.txt");
    fs::create_dir_all(traversal_sentinel.parent().unwrap()).unwrap();
    fs::write(&traversal_sentinel, "escaped sibling caller data\n").unwrap();
    write_fixture_inputs(&invalid_root, "../outside");
    let invalid = generate(&invalid_root);
    assert!(
        !invalid.status.success(),
        "typed package path rejects traversal"
    );
    assert_eq!(
        fs::read_to_string(&traversal_sentinel).unwrap(),
        "escaped sibling caller data\n",
        "the sentinel sits at the actual sibling reached by ../outside"
    );
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "outside caller data\n"
    );
    let _ = fs::remove_dir_all(invalid_parent);
}

fn assert_stale_handoff_rejected() {
    let stale = Fixture::new("stale-output", "dist");
    let stale_sha = stale.commit_source_change("stale fixture source\n", "production source");
    let stale_file = stale.root.join("dist/stale-from-previous-attempt");
    fs::create_dir_all(stale_file.parent().unwrap()).unwrap();
    fs::write(&stale_file, "stale bytes\n").unwrap();
    let stale_event = EventContext::push(&stale.initial_sha, &stale_sha, "7400", "1");
    let stale_run = run_package_pipeline(&stale, &stale_event, false);
    assert_eq!(stale_run.candidate["disposition"], "admit");
    assert_step_failure(stale_run.build.as_ref().unwrap(), "stale handoff rejection");
    assert!(
        !producer_was_called(&stale),
        "producer does not consume stale output"
    );
    assert_eq!(fs::read_to_string(stale_file).unwrap(), "stale bytes\n");
}

fn assert_escaping_symlink_rejected(outside: &Path) {
    let symlinked = Fixture::new("escaping-symlink", "dist");
    let symlinked_sha =
        symlinked.commit_source_change("symlink fixture source\n", "production source");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside, symlinked.root.join("dist")).unwrap();
    let symlink_event = EventContext::push(&symlinked.initial_sha, &symlinked_sha, "7401", "1");
    let symlink_run = run_package_pipeline(&symlinked, &symlink_event, false);
    assert_eq!(symlink_run.candidate["disposition"], "admit");
    assert_step_failure(
        symlink_run.build.as_ref().unwrap(),
        "escaping output symlink rejection",
    );
    assert!(
        !producer_was_called(&symlinked),
        "escaping symlink blocks producer execution"
    );
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "outside caller data\n"
    );
}

fn assert_dirty_source_rejected() {
    let dirty = Fixture::new("dirty-source", "dist");
    let dirty_sha = dirty.commit_source_change("committed source\n", "production source");
    let dirty_event = EventContext::push(&dirty.initial_sha, &dirty_sha, "7402", "1");
    let dirty_run = run_package_pipeline(&dirty, &dirty_event, true);
    assert_eq!(dirty_run.candidate["disposition"], "admit");
    assert_step_failure(
        dirty_run
            .source_check
            .as_ref()
            .expect("generated source check ran before build"),
        "dirty source identity rejection",
    );
    assert_eq!(
        git(&dirty.root, &["rev-parse", "HEAD^{tree}"]),
        dirty_run.admission_outputs["head_tree"],
        "dirty file leaves the admitted Git tree unchanged"
    );
    assert!(git(
        &dirty.root,
        &["status", "--porcelain", "--untracked-files=all"]
    )
    .contains("src/release.txt"));
    assert!(!dirty_run.build_ran, "dirty source blocks package build");
    assert!(dirty_run.build.is_none());
    assert!(
        !producer_was_called(&dirty),
        "dirty checkout cannot invoke the producer"
    );
}

fn write_git_excludes(root: &Path, patterns: &[&str]) {
    fs::write(
        root.join(".git/info/exclude"),
        format!("{}\n", patterns.join("\n")),
    )
    .expect("write fixture Git excludes");
}

fn assert_ignored_handoff_is_accepted() {
    let accepted = Fixture::new("ignored-owned-handoff", "output/handoff");
    write_git_excludes(&accepted.root, &["output/"]);
    let accepted_sha =
        accepted.commit_source_change("ignored handoff source\n", "production source");
    let accepted_event = EventContext::push(&accepted.initial_sha, &accepted_sha, "7416", "1");
    let accepted_run = run_package_pipeline(&accepted, &accepted_event, false);
    assert_package_pipeline_success(&accepted, &accepted_run, &accepted_sha);
    assert_eq!(
        git(
            &accepted.root,
            &[
                "check-ignore",
                "--quiet",
                "--",
                "output/handoff/preview-package.tar.gz"
            ]
        ),
        "",
        "the producer payload is ignored under its declared PACKAGE_DIR"
    );
    assert_eq!(
        git(
            &accepted.root,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "ignored handoff files remain outside ordinary source status"
    );
}

fn assert_ignored_sibling_is_rejected_before_producer() {
    let before = Fixture::new("ignored-stale-sibling-before", "output/handoff");
    write_git_excludes(&before.root, &["output/"]);
    let before_sha = before.commit_source_change("ignored sibling source\n", "production source");
    let stale_path = before.root.join("output/stale-before-producer.bin");
    fs::create_dir_all(stale_path.parent().unwrap()).unwrap();
    fs::write(&stale_path, "ignored stale sibling\n").unwrap();
    let before_event = EventContext::push(&before.initial_sha, &before_sha, "7417", "1");
    assert_eq!(
        git(
            &before.root,
            &[
                "check-ignore",
                "--quiet",
                "--",
                "output/stale-before-producer.bin"
            ]
        ),
        "",
        "the stale sibling is actually ignored by Git"
    );
    assert_eq!(
        git(
            &before.root,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "ordinary status hides the ignored stale sibling"
    );
    let (candidate, admission_outputs) = execute_admission_classifier(&before, &before_event);
    assert_eq!(candidate["disposition"], "admit");
    assert!(build_gate_allows(
        &before,
        &before_event,
        &admission_outputs
    ));
    let producer = execute_rendered_step(
        &before,
        "build",
        "Build package candidate",
        &before_event,
        &admission_outputs,
        true,
    );
    assert_step_failure(&producer, "ignored stale sibling before producer rejection");
    let producer_output = output_text(&producer.output);
    assert!(
        producer_output.contains("output/stale-before-producer.bin"),
        "generated producer inventory reports the ignored same-parent sibling: {producer_output}"
    );
    assert!(
        !producer_was_called(&before),
        "ignored stale sibling blocks producer invocation"
    );
}

fn assert_ignored_sibling_is_rejected_after_producer() {
    let after = Fixture::new("ignored-stale-sibling-after", "output/handoff");
    write_git_excludes(&after.root, &["output/"]);
    let after_sha = after.commit_source_change("post-build sibling source\n", "production source");
    let after_event = EventContext::push(&after.initial_sha, &after_sha, "7418", "1");
    let (candidate, admission_outputs) = execute_admission_classifier(&after, &after_event);
    assert_eq!(candidate["disposition"], "admit");
    let source_check = execute_rendered_step(
        &after,
        "build",
        "Verify admitted source tree",
        &after_event,
        &admission_outputs,
        false,
    );
    assert_step_success(&source_check, "clean source before package producer");
    let producer = execute_rendered_step_with_extra_environment(
        &after,
        "build",
        "Build package candidate",
        &after_event,
        &admission_outputs,
        true,
        &[("PACKAGE_TEST_STALE_HANDOFF_SIBLING", "1")],
    );
    assert_step_failure(&producer, "ignored stale sibling after producer rejection");
    let producer_output = output_text(&producer.output);
    assert!(
        producer_output.contains("output/stale-from-producer"),
        "generated producer inventory reports its ignored post-build sibling: {producer_output}"
    );
    assert!(
        producer_was_called(&after),
        "post-build sibling is detected after the producer runs"
    );
    assert_eq!(
        git(
            &after.root,
            &[
                "check-ignore",
                "--quiet",
                "--",
                "output/stale-from-producer"
            ]
        ),
        "",
        "the post-build sibling is actually ignored by Git"
    );
    assert_eq!(
        git(
            &after.root,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        "",
        "ordinary status hides the post-build ignored sibling"
    );
}

fn assert_ignored_package_inventory_controls() {
    assert_ignored_handoff_is_accepted();
    assert_ignored_sibling_is_rejected_before_producer();
    assert_ignored_sibling_is_rejected_after_producer();
}

fn assert_handoff_ancestor_symlink_rejected() {
    let fixture = Fixture::new("handoff-ancestor-symlink", "handoff-parent/dist");
    let source_sha = fixture.commit_source_change("ancestor symlink source\n", "production source");
    let event = EventContext::push(&fixture.initial_sha, &source_sha, "7419", "1");
    let run = run_package_pipeline(&fixture, &event, false);
    assert_package_pipeline_success(&fixture, &run, &source_sha);
    let handoff = handoff_path(&run);
    let verification_fixture = run.verification_fixture.as_ref().unwrap();
    let alternate_package = verification_fixture.root.join("alternate-package");
    fs::rename(&handoff, &alternate_package).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&alternate_package, &handoff).unwrap();
    let resolved = fs::canonicalize(&handoff).expect("resolve in-workspace symlink package");
    let expected =
        fs::canonicalize(&alternate_package).expect("canonicalize alternate package target");
    let workspace = fs::canonicalize(&verification_fixture.root)
        .expect("canonicalize fresh verifier workspace");
    assert_eq!(resolved, expected);
    assert!(
        resolved.starts_with(workspace),
        "symlink target is another package path inside the workspace"
    );
    let alternate_manifest: JsonValue =
        serde_json::from_slice(&fs::read(resolved.join("release-manifest.json")).unwrap()).unwrap();
    assert_eq!(
        alternate_manifest["assets"][0]["sha256"],
        sha256(&resolved.join(PAYLOAD)),
        "alternate package payload has a matching manifest digest"
    );
    assert_eq!(
        alternate_manifest["supporting_assets"][0]["sha256"],
        sha256(&resolved.join(SUMS)),
        "alternate package sidecar has a matching manifest digest"
    );

    let verification = verify_handoff_again(&fixture, &event, &run);
    assert_step_failure(&verification, "fresh-runner package symlink rejection");
    assert!(
        output_text(&verification.output)
            .contains("verified package directory is missing or a symlink"),
        "verifier rejects a symlinked package root: {}",
        output_text(&verification.output)
    );
}

fn assert_checkout_identity_and_file_controls() {
    let wrong_identity = Fixture::new("wrong-source-identity", "dist");
    let wrong_sha = wrong_identity.commit_source_change("declared source\n", "production source");
    let wrong_event = EventContext::push(&wrong_identity.initial_sha, &wrong_sha, "7403", "1");
    let wrong_run = run_package_pipeline(&wrong_identity, &wrong_event, false);
    assert_package_pipeline_success(&wrong_identity, &wrong_run, &wrong_sha);
    let wrong_handoff = handoff_path(&wrong_run);

    let wrong_origin = Fixture::new("wrong-checkout-origin", "dist");
    let origin_sha =
        wrong_origin.commit_source_change("origin check source\n", "production source");
    let origin_event = EventContext::push(&wrong_origin.initial_sha, &origin_sha, "7408", "1");
    let origin_run = run_package_pipeline(&wrong_origin, &origin_event, false);
    assert_package_pipeline_success(&wrong_origin, &origin_run, &origin_sha);
    git(
        &origin_run
            .verification_fixture
            .as_ref()
            .unwrap()
            .root
            .join("source"),
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/example/attacker-source.git",
        ],
    );
    let checkout_origin_check = verify_handoff_again(&wrong_origin, &origin_event, &origin_run);
    assert_step_failure(&checkout_origin_check, "source checkout origin mismatch");
    assert!(output_text(&checkout_origin_check.output)
        .contains("source checkout repository does not match the declared repository"));

    write(
        &wrong_handoff,
        "unexpected.txt",
        "undeclared package output\n",
    );
    let extra_file_check = verify_handoff_again(&wrong_identity, &wrong_event, &wrong_run);
    assert_step_failure(&extra_file_check, "unexpected package output rejection");
    assert!(output_text(&extra_file_check.output)
        .contains("verified package directory contains an undeclared or missing file"));
    fs::remove_file(wrong_handoff.join("unexpected.txt")).unwrap();

    let mut identity: JsonValue =
        serde_json::from_slice(&fs::read(wrong_handoff.join("identity.json")).unwrap()).unwrap();
    identity["source_digest"] = JsonValue::String("1".repeat(40));
    rewrite_json(&wrong_handoff.join("identity.json"), &identity);
    let identity_check = verify_handoff_again(&wrong_identity, &wrong_event, &wrong_run);
    assert_step_failure(&identity_check, "source identity mismatch rejection");

    let missing = Fixture::new("missing-package-file", "dist");
    let missing_sha = missing.commit_source_change("missing asset fixture\n", "production source");
    let missing_event = EventContext::push(&missing.initial_sha, &missing_sha, "7406", "1");
    let missing_run = run_package_pipeline(&missing, &missing_event, false);
    assert_package_pipeline_success(&missing, &missing_run, &missing_sha);
    let missing_handoff = handoff_path(&missing_run);
    fs::remove_file(missing_handoff.join(PAYLOAD)).unwrap();
    let missing_check = verify_handoff_again(&missing, &missing_event, &missing_run);
    assert_step_failure(&missing_check, "missing declared payload rejection");
    assert!(output_text(&missing_check.output)
        .contains("verified package directory contains an undeclared or missing file"));
}

fn assert_manifest_source_controls() {
    let wrong_manifest_source = Fixture::new("wrong-manifest-source", "dist");
    let manifest_source_sha = wrong_manifest_source
        .commit_source_change("manifest source fixture\n", "production source");
    let manifest_source_event = EventContext::push(
        &wrong_manifest_source.initial_sha,
        &manifest_source_sha,
        "7407",
        "1",
    );
    let manifest_source_run =
        run_package_pipeline(&wrong_manifest_source, &manifest_source_event, false);
    assert_package_pipeline_success(
        &wrong_manifest_source,
        &manifest_source_run,
        &manifest_source_sha,
    );
    let manifest_source_handoff = handoff_path(&manifest_source_run);
    let mut manifest: JsonValue = serde_json::from_slice(
        &fs::read(manifest_source_handoff.join("release-manifest.json")).unwrap(),
    )
    .unwrap();
    let wrong_source_sha = "1".repeat(40);
    manifest["source_commit"] = JsonValue::String(wrong_source_sha.clone());
    let mut identity: JsonValue =
        serde_json::from_slice(&fs::read(manifest_source_handoff.join("identity.json")).unwrap())
            .unwrap();
    identity["source_digest"] = JsonValue::String(wrong_source_sha);
    identity["manifest"] = manifest.clone();
    rewrite_json(
        &manifest_source_handoff.join("release-manifest.json"),
        &manifest,
    );
    rewrite_json(&manifest_source_handoff.join("identity.json"), &identity);
    let wrong_source_check = verify_handoff_again(
        &wrong_manifest_source,
        &manifest_source_event,
        &manifest_source_run,
    );
    assert_step_failure(
        &wrong_source_check,
        "manifest source commit mismatch rejection",
    );
    assert!(output_text(&wrong_source_check.output)
        .contains("manifest source_commit is not the checked-out commit"));

    let (manifest_origin, _, origin_event, origin_run, origin_handoff) =
        valid_package_fixture("wrong-manifest-origin", "7410");
    let mut manifest: JsonValue =
        serde_json::from_slice(&fs::read(origin_handoff.join("release-manifest.json")).unwrap())
            .unwrap();
    manifest["source_repository"] = JsonValue::String("example/attacker-source".to_owned());
    rewrite_json(&origin_handoff.join("release-manifest.json"), &manifest);
    sync_identity_manifest(&origin_handoff, &manifest);
    let manifest_origin_check = verify_handoff_again(&manifest_origin, &origin_event, &origin_run);
    assert_step_failure(
        &manifest_origin_check,
        "manifest source repository mismatch",
    );

    let (manifest_ref, _, ref_event, ref_run, ref_handoff) =
        valid_package_fixture("wrong-manifest-ref", "7411");
    let mut manifest: JsonValue =
        serde_json::from_slice(&fs::read(ref_handoff.join("release-manifest.json")).unwrap())
            .unwrap();
    manifest["source_ref"] = JsonValue::String("refs/heads/untrusted-preview".to_owned());
    rewrite_json(&ref_handoff.join("release-manifest.json"), &manifest);
    sync_identity_manifest(&ref_handoff, &manifest);
    let manifest_ref_check = verify_handoff_again(&manifest_ref, &ref_event, &ref_run);
    assert_step_failure(&manifest_ref_check, "manifest source ref mismatch");
}

fn assert_manifest_tag_and_asset_controls() {
    let (wrong_tag, tag_sha, tag_event, tag_run, tag_handoff) =
        valid_package_fixture("wrong-version-tag", "7412");
    let wrong_suffix = if tag_sha.starts_with("0000000") {
        "1111111"
    } else {
        "0000000"
    };
    let mut manifest: JsonValue =
        serde_json::from_slice(&fs::read(tag_handoff.join("release-manifest.json")).unwrap())
            .unwrap();
    manifest["version"] = JsonValue::String(format!("0.1.0-preview.1+{wrong_suffix}"));
    rewrite_json(&tag_handoff.join("release-manifest.json"), &manifest);
    sync_identity_manifest(&tag_handoff, &manifest);
    let wrong_tag_check = verify_handoff_again(&wrong_tag, &tag_event, &tag_run);
    assert_step_failure(
        &wrong_tag_check,
        "source-bound package version tag mismatch",
    );
    assert!(
        output_text(&wrong_tag_check.output).contains("version does not bind its source commit")
    );
    assert_eq!(tag_run.candidate["head_sha"], tag_sha);

    let (wrong_assets, _, assets_event, assets_run, assets_handoff) =
        valid_package_fixture("wrong-declared-assets", "7413");
    let mut manifest: JsonValue =
        serde_json::from_slice(&fs::read(assets_handoff.join("release-manifest.json")).unwrap())
            .unwrap();
    manifest["assets"][0]["name"] = JsonValue::String("renamed-payload.tar.gz".to_owned());
    rewrite_json(&assets_handoff.join("release-manifest.json"), &manifest);
    sync_identity_manifest(&assets_handoff, &manifest);
    let wrong_assets_check = verify_handoff_again(&wrong_assets, &assets_event, &assets_run);
    assert_step_failure(&wrong_assets_check, "manifest declared assets mismatch");
    assert!(output_text(&wrong_assets_check.output)
        .contains("manifest asset names do not equal the declared payloads"));
}

fn assert_payload_digest_controls() {
    let (changed_payload, _, payload_event, payload_run, payload_handoff) =
        valid_package_fixture("changed-payload-bytes", "7414");
    fs::write(
        payload_handoff.join(PAYLOAD),
        b"payload bytes changed after manifest creation\n",
    )
    .unwrap();
    let changed_payload_check =
        verify_handoff_again(&changed_payload, &payload_event, &payload_run);
    assert_step_failure(
        &changed_payload_check,
        "payload bytes changed after digesting",
    );
    assert!(output_text(&changed_payload_check.output).contains("payload checksum mismatch"));

    let bad_digest = Fixture::new("bad-package-digest", "dist");
    let digest_sha =
        bad_digest.commit_source_change("digest fixture source\n", "production source");
    let digest_event = EventContext::push(&bad_digest.initial_sha, &digest_sha, "7404", "1");
    let digest_run = run_package_pipeline(&bad_digest, &digest_event, false);
    assert_package_pipeline_success(&bad_digest, &digest_run, &digest_sha);
    let digest_handoff = handoff_path(&digest_run);
    let mut manifest: JsonValue =
        serde_json::from_slice(&fs::read(digest_handoff.join("release-manifest.json")).unwrap())
            .unwrap();
    manifest["assets"][0]["sha256"] = JsonValue::String("0".repeat(64));
    let mut identity: JsonValue =
        serde_json::from_slice(&fs::read(digest_handoff.join("identity.json")).unwrap()).unwrap();
    identity["manifest"] = manifest.clone();
    rewrite_json(&digest_handoff.join("release-manifest.json"), &manifest);
    rewrite_json(&digest_handoff.join("identity.json"), &identity);
    let digest_check = verify_handoff_again(&bad_digest, &digest_event, &digest_run);
    assert_step_failure(&digest_check, "tampered manifest digest rejection");
    assert!(output_text(&digest_check.output).contains("payload checksum mismatch"));
}

fn assert_checksum_sidecar_rejected() {
    let bad_sums = Fixture::new("bad-checksum-sidecar", "dist");
    let sums_sha = bad_sums.commit_source_change("checksum fixture source\n", "production source");
    let sums_event = EventContext::push(&bad_sums.initial_sha, &sums_sha, "7405", "1");
    let sums_run = run_package_pipeline(&bad_sums, &sums_event, false);
    assert_package_pipeline_success(&bad_sums, &sums_run, &sums_sha);
    let sums_handoff = handoff_path(&sums_run);
    fs::write(
        sums_handoff.join(SUMS),
        format!("{}  {PAYLOAD}\n", "0".repeat(64)),
    )
    .unwrap();
    let mut manifest: JsonValue =
        serde_json::from_slice(&fs::read(sums_handoff.join("release-manifest.json")).unwrap())
            .unwrap();
    manifest["supporting_assets"][0]["sha256"] =
        JsonValue::String(sha256(&sums_handoff.join(SUMS)));
    let mut identity: JsonValue =
        serde_json::from_slice(&fs::read(sums_handoff.join("identity.json")).unwrap()).unwrap();
    identity["manifest"] = manifest.clone();
    rewrite_json(&sums_handoff.join("release-manifest.json"), &manifest);
    rewrite_json(&sums_handoff.join("identity.json"), &identity);
    let sums_check = verify_handoff_again(&bad_sums, &sums_event, &sums_run);
    assert_step_failure(&sums_check, "tampered checksum sidecar rejection");
    assert!(output_text(&sums_check.output).contains("SHA256SUMS does not verify"));
}

#[test]
fn unsafe_paths_and_stale_outputs_fail() {
    let outside = outside_sentinel();
    assert_traversal_path_rejected(&outside);
    assert_stale_handoff_rejected();
    assert_escaping_symlink_rejected(&outside);
    assert_dirty_source_rejected();
    assert_ignored_package_inventory_controls();
    assert_handoff_ancestor_symlink_rejected();
    assert_checkout_identity_and_file_controls();
    assert_manifest_source_controls();
    assert_manifest_tag_and_asset_controls();
    assert_payload_digest_controls();
    assert_checksum_sidecar_rejected();
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "outside caller data\n"
    );
    let _ = fs::remove_dir_all(outside);
}

#[test]
fn valid_legacy_output_path_still_works() {
    let fixture = Fixture::new("legacy-relative-output", "dist/legacy-output");
    let source_sha = fixture.commit_source_change("legacy output source\n", "production source");
    let event = EventContext::push(&fixture.initial_sha, &source_sha, "7500", "1");
    let run = run_package_pipeline(&fixture, &event, false);
    assert_package_pipeline_success(&fixture, &run, &source_sha);

    let build = run.build.as_ref().unwrap();
    assert_eq!(
        build.environment.get("PACKAGE_DIR").map(String::as_str),
        Some("dist/legacy-output"),
        "legacy producers retain the relative package path from generated YAML"
    );
    let expected_handoff = fixture.root.join("dist/legacy-output");
    assert_eq!(producer_handoff_path(&run), expected_handoff);
    assert_eq!(
        handoff_path(&run),
        run.verification_fixture
            .as_ref()
            .unwrap()
            .root
            .join("package")
    );

    let upload = named_step(fixture.build_job(), "Upload untrusted package candidate");
    let upload_path = resolve_value(
        upload["with"]["path"]
            .as_str()
            .expect("artifact path declaration"),
        &fixture,
        &event,
        &run.admission_outputs,
    );
    assert!(
        !upload_path.contains("${{"),
        "all artifact expressions resolved"
    );
    let actual_upload_paths = upload_path
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected_upload_paths = ["release-manifest.json", "identity.json", PAYLOAD, SUMS]
        .into_iter()
        .map(|name| {
            fixture
                .root
                .join("dist/legacy-output")
                .join(name)
                .display()
                .to_string()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_upload_paths, expected_upload_paths,
        "legacy handoff upload contains exactly the manifest, identity, payload and checksum paths"
    );
}
