//! Offline executable fixtures for the owner bootstrap transport.
//!
//! The generated acquisition and execute shells run with fake API/tool
//! commands and harmless local archives. Docker, candidate bytes, and real
//! artifact endpoints are never invoked.

#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "fixture setup failures should abort loudly"
)]

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value as JsonValue};
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "tailrocks/velnor";
const HEAD_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const BASE_SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";
const HEAD_TREE_SHA: &str = "1111111111111111111111111111111111111111";
const BASE_TREE_SHA: &str = "2222222222222222222222222222222222222222";
const CLOSURE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE_REVISION: &str = "3333333333333333333333333333333333333333";
const WORKFLOW_ID: u64 = 700;
const RUN_ID: u64 = 900;
const RUN_ATTEMPT: u64 = 2;
const JOB_ID: u64 = 800;
const ARTIFACT_ID: u64 = 1000;
const PR_NUMBER: u64 = 7;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Generated {
    producer: String,
    acquire: String,
    execute: String,
    artifact_name: String,
    build_image_repository: String,
    build_image_digest: String,
    sandbox_image_repository: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureCase {
    ServiceDigestMismatch,
    ApiTreeMismatch,
    ObjectTreeMismatch,
    WrongRunHead,
    WrongArtifactRun,
    RunAttemptMismatch,
    StaleArtifact,
    DuplicateRun,
    DuplicateJob,
    DuplicateArtifact,
    MissingArtifact,
    FailedRun,
    WrongJob,
    HeadWorkflowSubstitution,
    ExtraUploader,
}

struct TransportFixture {
    root: PathBuf,
    generated: Generated,
    bin_dir: PathBuf,
    archive: PathBuf,
    source_archive: PathBuf,
    scenario: PathBuf,
    contract: PathBuf,
    head_contract: PathBuf,
    run_temp: PathBuf,
    docker_log: PathBuf,
}

impl TransportFixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = env::temp_dir().join(format!(
            "velnor-bootstrap-transport-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".github-gen")).expect("fixture root");
        fs::create_dir_all(root.join("src")).expect("fixture source");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"transport-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("Cargo.toml");
        fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n").expect("fixture source");
        fs::write(root.join("Cargo.lock"), "version = 3\n").expect("Cargo.lock");
        fs::write(
            root.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"stable\"\n",
        )
        .expect("toolchain");
        fs::write(root.join(".github-gen/velnor-workflow.toml"), OWNER_CONFIG)
            .expect("generator config");

        let output = root.join("generated");
        let generator = env::var_os("VELNOR_WORKFLOW_GENERATOR")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_velnor-workflow").into());
        let generation = Command::new(generator)
            .args([
                "--plain",
                "--default-branch",
                "main",
                "--output",
                output.to_str().expect("output path"),
                root.to_str().expect("root path"),
            ])
            .output()
            .expect("run generator");
        assert!(
            generation.status.success(),
            "generator failed:\n{}\n{}",
            String::from_utf8_lossy(&generation.stdout),
            String::from_utf8_lossy(&generation.stderr)
        );

        let pr = read_workflow(&output, "ci-pr.yml");
        let policy = read_workflow(&output, "ci-policy.yml");
        let producer = step_run(&pr, "Build candidate generator");
        let acquire = step_run(&policy, "Acquire candidate generator product");
        let execute = step_run(&policy, "Execute candidate in pinned sandbox");
        let artifact_name = step_with(&pr, "Upload candidate generator product", "name");
        let build_image_repository = step_env(
            &pr,
            "Build candidate generator",
            "CANDIDATE_BUILD_IMAGE_REPOSITORY",
        )
        .unwrap_or_else(|| "ghcr.io/tailrocks/velnor-bootstrap-builder".to_owned());
        let build_image_digest = step_env(
            &pr,
            "Build candidate generator",
            "CANDIDATE_BUILD_IMAGE_DIGEST",
        )
        .unwrap_or_default();
        let sandbox_image_repository = step_env(
            &policy,
            "Execute candidate in pinned sandbox",
            "SANDBOX_IMAGE_REPOSITORY",
        )
        .unwrap_or_else(|| "ghcr.io/tailrocks/velnor-bootstrap-sandbox".to_owned());

        let bin_dir = root.join("fake-bin");
        fs::create_dir_all(&bin_dir).expect("fake bin");
        write_fake_commands(&bin_dir);
        let archive = root.join("candidate.zip");
        let source_archive = root.join("source.tar");
        write_source_tar(&source_archive);
        let scenario = root.join("scenario.json");
        let contract = root.join("ci-pr-contract.yml");
        let head_contract = root.join("ci-pr-head.yml");
        fs::write(&contract, &pr).expect("contract");
        fs::write(&head_contract, &pr).expect("head contract");
        let run_temp = root.join("runner-temp");
        fs::create_dir_all(&run_temp).expect("runner temp");
        let docker_log = run_temp.join("docker.log");

        Self {
            root,
            generated: Generated {
                producer,
                acquire,
                execute,
                artifact_name,
                build_image_repository,
                build_image_digest,
                sandbox_image_repository,
            },
            bin_dir,
            archive,
            source_archive,
            scenario,
            contract,
            head_contract,
            run_temp,
            docker_log,
        }
    }

    fn valid_artifact(&self) {
        let binary = self.root.join("candidate.bin");
        let manifest = self.root.join("candidate-manifest.json");
        fs::write(&binary, format!("#!/bin/sh\nprintf '%s\\n' '{CLOSURE}'\n"))
            .expect("candidate fixture");
        let mut permissions = fs::metadata(&binary)
            .expect("candidate metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).expect("candidate permissions");
        let manifest_value = json!({
            "role": "producer",
            "workflow_path": ".github/workflows/ci-pr.yml",
            "job_name": "candidate_producer",
            "event": "pull_request",
            "repository": REPOSITORY,
            "head_sha": HEAD_SHA,
            "artifact_name": self.generated.artifact_name,
            "closure": CLOSURE,
            "profile": "debug",
            "platform": "linux/amd64",
            "features": "tui",
            "build_image_repository": self.generated.build_image_repository,
            "build_image_digest": self.generated.build_image_digest,
            "build_image_platform_digest": format!("sha256:{}", "b".repeat(64)),
            "binary_sha256": file_sha256(&binary),
        });
        fs::write(
            &manifest,
            serde_json::to_vec(&manifest_value).expect("manifest JSON"),
        )
        .expect("manifest");
        zip_files(&self.archive, &binary, &manifest);
        let archive_sha = file_sha256(&self.archive);
        let scenario = valid_scenario(
            &self.generated.artifact_name,
            &format!("sha256:{archive_sha}"),
            &self.generated.build_image_repository,
            &self.generated.build_image_digest,
        );
        fs::write(
            &self.scenario,
            serde_json::to_vec(&scenario).expect("scenario JSON"),
        )
        .expect("scenario");
    }

    fn run_acquire(&self, failure: Option<FailureCase>) -> Output {
        let base_contract = fs::read_to_string(&self.contract).expect("contract text");
        let mut head_contract = base_contract.clone();
        if failure == Some(FailureCase::HeadWorkflowSubstitution) {
            head_contract =
                base_contract.replace("candidate_producer", "candidate_producer_substituted");
        }
        if failure == Some(FailureCase::ExtraUploader) {
            head_contract = base_contract.replace(
                "        id: candidate_upload\n",
                "        id: candidate_upload\n      - name: Extra uploader\n        id: extra_upload\n        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a\n        with:\n          name: extra-upload\n          path: ${{ runner.temp }}/extra-upload\n",
            );
        }
        fs::write(&self.head_contract, head_contract).expect("head contract");

        let mut scenario: JsonValue =
            serde_json::from_slice(&fs::read(&self.scenario).expect("scenario bytes"))
                .expect("scenario JSON");
        match failure {
            Some(FailureCase::ServiceDigestMismatch) => {
                scenario["artifacts"][0]["digest"] = json!(format!("sha256:{}", "c".repeat(64)));
            }
            Some(FailureCase::ApiTreeMismatch) => {
                scenario["api_head_tree"] = json!(BASE_TREE_SHA);
            }
            Some(FailureCase::ObjectTreeMismatch) => {
                scenario["object_head_tree"] = json!(BASE_TREE_SHA);
            }
            Some(FailureCase::WrongRunHead) => scenario["runs"][0]["head_sha"] = json!(BASE_SHA),
            Some(FailureCase::WrongArtifactRun) => {
                scenario["artifacts"][0]["workflow_run"]["id"] = json!(RUN_ID + 1);
            }
            Some(FailureCase::RunAttemptMismatch) => scenario["jobs"][0]["run_attempt"] = json!(1),
            Some(FailureCase::StaleArtifact) => {
                scenario["artifacts"][0]["expires_at"] = json!("2000-01-01T00:00:00Z");
            }
            Some(FailureCase::DuplicateRun) => {
                let run = scenario["runs"][0].clone();
                scenario["runs"] = json!([run.clone(), run]);
            }
            Some(FailureCase::DuplicateJob) => {
                let mut duplicate = scenario["jobs"][0].clone();
                duplicate["id"] = json!(JOB_ID + 1);
                let original = scenario["jobs"][0].clone();
                scenario["jobs"] = json!([original, duplicate]);
            }
            Some(FailureCase::DuplicateArtifact) => {
                let mut duplicate = scenario["artifacts"][0].clone();
                duplicate["id"] = json!(ARTIFACT_ID + 1);
                let original = scenario["artifacts"][0].clone();
                scenario["artifacts"] = json!([original, duplicate]);
            }
            Some(FailureCase::MissingArtifact) => scenario["artifacts"] = json!([]),
            Some(FailureCase::FailedRun) => scenario["runs"][0]["conclusion"] = json!("failure"),
            Some(FailureCase::WrongJob) => scenario["jobs"][0]["name"] = json!("other_job"),
            Some(FailureCase::HeadWorkflowSubstitution | FailureCase::ExtraUploader) | None => {}
        }
        fs::write(
            &self.scenario,
            serde_json::to_vec(&scenario).expect("scenario JSON"),
        )
        .expect("scenario");
        let _ = fs::remove_dir_all(&self.run_temp);
        fs::create_dir_all(&self.run_temp).expect("run temp");
        let mut command = Command::new("bash");
        let acquire_script = materialize_github_expressions(&self.generated.acquire);
        command
            .args(["-euo", "pipefail", "-c", &acquire_script])
            .current_dir(&self.root)
            .env("PATH", prepend_path(&self.bin_dir))
            .env("FIXTURE_SCENARIO", &self.scenario)
            .env("FIXTURE_CONTRACT", &self.contract)
            .env("FIXTURE_HEAD_CONTRACT", &self.head_contract)
            .env("FIXTURE_ARCHIVE", &self.archive)
            .env("FIXTURE_SOURCE_ARCHIVE", &self.source_archive)
            .env("FIXTURE_HEAD_TREE", HEAD_TREE_SHA)
            .env("FIXTURE_BASE_TREE", BASE_TREE_SHA)
            .env("FIXTURE_CLOSURE", CLOSURE)
            .env("RUNNER_TEMP", &self.run_temp)
            .env("GITHUB_REPOSITORY", REPOSITORY)
            .env("GITHUB_REPOSITORY_ID", "42")
            .env("GITHUB_API_URL", "https://api.invalid")
            .env("GITHUB_SERVER_URL", "https://github.invalid")
            .env("RUNNER_OS", "Linux")
            .env("RUNNER_ARCH", "X64")
            .env("GH_TOKEN", "fixture-token")
            .env("HEAD_SHA", HEAD_SHA)
            .env("BASE_SHA", BASE_SHA)
            .env("PR_NUMBER", PR_NUMBER.to_string())
            .env("HEAD_REPOSITORY", REPOSITORY)
            .env("HEAD_REPOSITORY_ID", "42")
            .env("TARGET_REPOSITORY_ID", "42")
            .env("BASE_REVISION", BASE_REVISION)
            .env("GITHUB_RUN_ID", RUN_ID.to_string())
            .env("GITHUB_RUN_ATTEMPT", RUN_ATTEMPT.to_string())
            .env("GITHUB_ENV", self.run_temp.join("github.env"));
        command.output().expect("run generated acquire shell")
    }

    fn run_producer_build(&self) -> Output {
        let _ = fs::remove_dir_all(&self.run_temp);
        fs::create_dir_all(&self.run_temp).expect("run temp");
        let script = self.generated.producer.clone();
        // The b981 generator intentionally leaves the builder pin empty until the
        // published image is assigned. Supply only a fixture-local digest so the
        // executable producer shell reaches its Docker/lifetime assertions.
        let build_digest = if self.generated.build_image_digest.is_empty() {
            format!("sha256:{}", "d".repeat(64))
        } else {
            self.generated.build_image_digest.clone()
        };
        Command::new("bash")
            .args(["-euo", "pipefail", "-c", &script])
            .current_dir(&self.root)
            .env("PATH", prepend_path(&self.bin_dir))
            .env("RUNNER_TEMP", &self.run_temp)
            .env("GITHUB_WORKSPACE", &self.root)
            .env("GITHUB_REPOSITORY", REPOSITORY)
            .env("GITHUB_RUN_ID", RUN_ID.to_string())
            .env("GITHUB_RUN_ATTEMPT", RUN_ATTEMPT.to_string())
            .env("RUNNER_OS", "Linux")
            .env("RUNNER_ARCH", "X64")
            .env("CANDIDATE_ARTIFACT_NAME", &self.generated.artifact_name)
            .env("CANDIDATE_HEAD_SHA", HEAD_SHA)
            .env(
                "CANDIDATE_BUILD_IMAGE_REPOSITORY",
                &self.generated.build_image_repository,
            )
            .env("CANDIDATE_BUILD_IMAGE_DIGEST", &build_digest)
            .env("DOCKER_FIXTURE_MODE", "producer")
            .env_remove("GITHUB_TOKEN")
            .env_remove("ACTIONS_RUNTIME_TOKEN")
            .env_remove("ACTIONS_RUNTIME_URL")
            .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
            .output()
            .expect("run generated producer shell")
    }

    fn run_execute_with_hostile_output(&self) -> Output {
        let binary = self.root.join("execute-binary");
        fs::write(&binary, b"offline dummy candidate bytes\n").expect("execute binary");
        let handoff_dir = self.run_temp.join("candidate-handoff");
        fs::create_dir_all(&handoff_dir).expect("handoff dir");
        let source_sha = file_sha256(&self.source_archive);
        let handoff = json!({
            "role": "handoff", "workflow_path": ".github/workflows/ci-pr.yml",
            "workflow_id": WORKFLOW_ID, "run_id": RUN_ID, "run_attempt": RUN_ATTEMPT,
            "job_id": JOB_ID, "job_name": "candidate_producer", "event": "pull_request",
            "target_repository": REPOSITORY, "target_repository_id": 42,
            "head_repository": REPOSITORY, "head_repository_id": 42,
            "head_sha": HEAD_SHA, "base_sha": BASE_SHA, "base_revision": BASE_REVISION,
            "head_tree_sha": HEAD_TREE_SHA, "base_tree_sha": BASE_TREE_SHA,
            "pr_number": PR_NUMBER,
            "profile": "debug", "platform": "linux/amd64", "features": "tui",
            "build_image_repository": self.generated.build_image_repository,
            "build_image_digest": self.generated.build_image_digest,
            "build_image_platform_digest": format!("sha256:{}", "b".repeat(64)),
            "artifact_name": self.generated.artifact_name, "artifact_id": ARTIFACT_ID,
            "artifact_size": 128, "artifact_service_digest": format!("sha256:{}", "d".repeat(64)),
            "artifact_raw_zip_sha256": "e".repeat(64),
            "artifact_expires_at": "2099-01-01T00:00:00Z", "source_archive_sha256": source_sha,
            "candidate_closure": CLOSURE, "contract_sha256": "f".repeat(64)
        });
        let manifest = json!({
            "role": "producer", "workflow_path": ".github/workflows/ci-pr.yml",
            "job_name": "candidate_producer", "event": "pull_request", "repository": REPOSITORY,
            "head_sha": HEAD_SHA, "artifact_name": self.generated.artifact_name,
            "profile": "debug", "platform": "linux/amd64", "features": "tui",
            "build_image_repository": self.generated.build_image_repository,
            "build_image_digest": self.generated.build_image_digest,
            "build_image_platform_digest": format!("sha256:{}", "b".repeat(64)),
            "binary_sha256": file_sha256(&binary)
        });
        fs::write(
            handoff_dir.join("handoff.json"),
            serde_json::to_vec(&handoff).expect("handoff"),
        )
        .expect("handoff");
        fs::write(
            handoff_dir.join("candidate-manifest.json"),
            serde_json::to_vec(&manifest).expect("manifest"),
        )
        .expect("manifest");
        fs::copy(&binary, handoff_dir.join("velnor-workflow")).expect("binary");
        fs::copy(&self.source_archive, handoff_dir.join("source.tar")).expect("source archive");
        let script = materialize_github_expressions(&self.generated.execute);
        Command::new("bash")
            .args(["-euo", "pipefail", "-c", &script])
            .current_dir(&self.root)
            .env("PATH", prepend_path(&self.bin_dir))
            .env("HANDOFF", &handoff_dir)
            .env("RUNNER_TEMP", &self.run_temp)
            .env("RUNNER_OS", "Linux")
            .env("RUNNER_ARCH", "X64")
            .env("GITHUB_REPOSITORY", REPOSITORY)
            .env("GITHUB_RUN_ID", "901")
            .env("GITHUB_RUN_ATTEMPT", "1")
            .env("GITHUB_JOB", "candidate_execute")
            .env("PR_NUMBER", PR_NUMBER.to_string())
            .env(
                "SANDBOX_IMAGE_REPOSITORY",
                &self.generated.sandbox_image_repository,
            )
            .env("SANDBOX_IMAGE_DIGEST", format!("sha256:{}", "a".repeat(64)))
            .env("DEFAULT_BRANCH", "main")
            .env("DOCKER_FIXTURE_LOG", &self.docker_log)
            .output()
            .expect("run generated execute shell")
    }
}

impl Drop for TransportFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn generated_acquire_accepts_measured_zip_and_writes_trusted_handoff() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
    let output = fixture.run_acquire(None);
    assert!(
        output.status.success(),
        "generated acquire failed ({:?}):\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let handoff_path = fixture.run_temp.join("candidate-handoff/handoff.json");
    assert!(handoff_path.is_file(), "acquire must materialize handoff");
    let handoff: JsonValue =
        serde_json::from_slice(&fs::read(handoff_path).expect("handoff bytes"))
            .expect("handoff JSON");
    assert_eq!(
        handoff["artifact_raw_zip_sha256"],
        json!(file_sha256(&fixture.archive))
    );
    assert_eq!(handoff["run_attempt"], json!(RUN_ATTEMPT));
    assert_eq!(handoff["job_id"], json!(JOB_ID));
    assert_eq!(handoff["head_sha"], json!(HEAD_SHA));
}

#[test]
fn generated_acquire_rejects_transport_and_identity_faults() {
    let baseline = TransportFixture::new();
    baseline.valid_artifact();
    let baseline_output = baseline.run_acquire(None);
    assert!(
        baseline_output.status.success(),
        "negative fixtures require a passing generated acquire baseline ({:?}):\n{}\n{}",
        baseline_output.status.code(),
        String::from_utf8_lossy(&baseline_output.stdout),
        String::from_utf8_lossy(&baseline_output.stderr)
    );

    let cases = [
        FailureCase::ServiceDigestMismatch,
        FailureCase::ApiTreeMismatch,
        FailureCase::ObjectTreeMismatch,
        FailureCase::WrongRunHead,
        FailureCase::WrongArtifactRun,
        FailureCase::RunAttemptMismatch,
        FailureCase::StaleArtifact,
        FailureCase::DuplicateRun,
        FailureCase::DuplicateJob,
        FailureCase::DuplicateArtifact,
        FailureCase::MissingArtifact,
        FailureCase::FailedRun,
        FailureCase::WrongJob,
        FailureCase::HeadWorkflowSubstitution,
        FailureCase::ExtraUploader,
    ];
    for case in cases {
        let fixture = TransportFixture::new();
        fixture.valid_artifact();
        let output = fixture.run_acquire(Some(case));
        assert!(
            !output.status.success(),
            "{case:?} unexpectedly passed generated acquire:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn generated_producer_keeps_upload_surface_after_build() {
    let fixture = TransportFixture::new();
    let output = fixture.run_producer_build();
    assert!(
        output.status.success(),
        "generated producer failed ({:?}):\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stage = fixture.run_temp.join("velnor-workflow-candidate");
    assert!(stage.is_dir(), "producer build removed upload surface");
    assert!(stage.join("velnor-workflow").is_file());
    assert!(stage.join("candidate-manifest.json").is_file());
    assert!(stage.join("container.json").is_file());
    assert!(stage.join("after.json").is_file());
    assert!(stage.join("exit").is_file());
    let create: JsonValue = fs::read_to_string(&fixture.docker_log)
        .expect("producer docker fixture log")
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .find(|record: &JsonValue| record["op"] == "create")
        .expect("producer shell must reach docker create");
    let args = create["args"].as_array().expect("producer docker args");
    let envs: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--env")
        .map(|(index, _)| args[index + 1].as_str().unwrap_or_default())
        .collect();
    assert!(
        envs.contains(&"CARGO_TARGET_DIR=/target"),
        "producer must pin CARGO_TARGET_DIR despite builder image ENV: {envs:?}"
    );
}

#[test]
fn generated_execute_uses_fixed_uid_allowlist_and_rejects_extra_output() {
    let fixture = TransportFixture::new();
    let output = fixture.run_execute_with_hostile_output();
    assert!(
        !output.status.success(),
        "hostile output census unexpectedly passed"
    );
    assert!(
        fixture.docker_log.is_file(),
        "execute wrapper did not reach fake Docker: {:?} {:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let create: JsonValue = fs::read_to_string(&fixture.docker_log)
        .expect("docker fixture log")
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .find(|record: &JsonValue| record["op"] == "create")
        .expect("execute wrapper must reach docker create");
    let args = create["args"].as_array().expect("docker args");
    let user_index = args.iter().position(|arg| arg == "--user").expect("--user");
    assert_eq!(args[user_index + 1], json!("65532:65532"));
    let envs: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, arg)| *arg == "--env")
        .map(|(index, _)| args[index + 1].as_str().unwrap_or(""))
        .map(|entry| entry.split('=').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        envs,
        [
            "SOURCE_HEAD_SHA",
            "SOURCE_TREE_SHA",
            "SOURCE_REPOSITORY",
            "SOURCE_CLOSURE",
            "HOME",
            "PATH",
        ]
    );
    assert!(!envs.iter().any(|name| name == "HOSTNAME"));
}

fn read_workflow(output: &Path, name: &str) -> String {
    fs::read_to_string(output.join(".github/workflows").join(name)).expect("generated workflow")
}

fn parse_workflow(workflow: &str) -> YamlValue {
    serde_yaml::from_str(workflow).expect("workflow YAML")
}

fn step_run(workflow: &str, name: &str) -> String {
    let document = parse_workflow(workflow);
    let jobs = document["jobs"].as_mapping().expect("jobs map");
    for job in jobs.values() {
        let Some(steps) = job["steps"].as_sequence() else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some(name) {
                return step["run"].as_str().expect("step run").to_owned();
            }
        }
    }
    std::process::abort()
}

fn step_with(workflow: &str, step_name: &str, key: &str) -> String {
    let document = parse_workflow(workflow);
    let jobs = document["jobs"].as_mapping().expect("jobs map");
    for job in jobs.values() {
        let Some(steps) = job["steps"].as_sequence() else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some(step_name) {
                return step["with"][key].as_str().expect("step input").to_owned();
            }
        }
    }
    std::process::abort()
}

fn step_env(workflow: &str, step_name: &str, key: &str) -> Option<String> {
    let document = parse_workflow(workflow);
    let jobs = document["jobs"].as_mapping().expect("jobs map");
    for job in jobs.values() {
        let Some(steps) = job["steps"].as_sequence() else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some(step_name) {
                return step["env"][key].as_str().map(ToOwned::to_owned);
            }
        }
    }
    None
}

fn materialize_github_expressions(script: &str) -> String {
    let mut result = String::with_capacity(script.len());
    let mut offset = 0;
    while let Some(relative_start) = script[offset..].find("${{") {
        let start = offset + relative_start;
        result.push_str(&script[offset..start]);
        let after = &script[start + 3..];
        let end = after.find("}}").expect("closed GitHub expression");
        let expression_end = start + 3 + end + 2;
        if start > 0 && script.as_bytes()[start - 1] == b'\\' {
            // Backslash-escaped expressions are deliberate literal contract text.
            result.push_str(&script[start..expression_end]);
        } else if after[..end].trim() == "runner.temp" {
            // This expression is inside a grep pattern for the raw workflow
            // contract. Keep that protocol text literal while making the
            // generated shell executable under offline Bash.
            result.push_str(r"\${{ runner.temp }}");
        } else {
            result.push_str("fixture-expression");
        }
        offset = expression_end;
    }
    result.push_str(&script[offset..]);
    result
}

fn prepend_path(bin_dir: &Path) -> String {
    format!("{}:{}", bin_dir.display(), env::var("PATH").expect("PATH"))
}

fn valid_scenario(
    artifact_name: &str,
    service_digest: &str,
    build_image_repository: &str,
    build_image_digest: &str,
) -> JsonValue {
    json!({
        "runs": [{
            "id": RUN_ID, "path": ".github/workflows/ci-pr.yml", "workflow_id": WORKFLOW_ID,
            "event": "pull_request", "head_sha": HEAD_SHA, "status": "completed",
            "conclusion": "success", "run_attempt": RUN_ATTEMPT,
            "repository": {"id": 42, "full_name": REPOSITORY},
            "head_repository": {"id": 42, "full_name": REPOSITORY},
            "pull_requests": [{"number": PR_NUMBER, "base": {"sha": BASE_SHA}}]
        }],
        "api_head_tree": HEAD_TREE_SHA,
        "api_base_tree": BASE_TREE_SHA,
        "object_head_tree": HEAD_TREE_SHA,
        "object_base_tree": BASE_TREE_SHA,
        "jobs": [{
            "id": JOB_ID, "name": "candidate_producer", "run_id": RUN_ID,
            "head_sha": HEAD_SHA, "status": "completed", "conclusion": "success",
            "run_attempt": RUN_ATTEMPT
        }],
        "artifacts": [{
            "id": ARTIFACT_ID, "name": artifact_name, "expired": false,
            "size_in_bytes": 1024, "digest": service_digest,
            "expires_at": "2099-01-01T00:00:00Z",
            "created_at": "2026-09-20T00:00:00Z", "updated_at": "2026-09-20T00:00:00Z",
            "workflow_run": {"id": RUN_ID}
        }],
        "build_image_repository": build_image_repository,
        "build_image_digest": build_image_digest
    })
}

fn write_source_tar(path: &Path) {
    let source = path.with_extension("source");
    fs::create_dir_all(source.join(".github/workflows")).expect("source tree");
    fs::write(
        source.join(".github/workflows/ci-pr.yml"),
        "fixture workflow\n",
    )
    .expect("source file");
    let status = Command::new("tar")
        .args([
            "-cf",
            path.to_str().expect("tar path"),
            "-C",
            source.to_str().expect("source path"),
            ".github",
        ])
        .status()
        .expect("tar source fixture");
    assert!(status.success(), "tar source fixture failed");
    let _ = fs::remove_dir_all(source);
}

fn zip_files(path: &Path, binary: &Path, manifest: &Path) {
    let script = r#"import sys, zipfile
archive, binary, manifest = sys.argv[1:]
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
    out.write(binary, "velnor-workflow")
    out.write(manifest, "candidate-manifest.json")
"#;
    let status = Command::new("python3")
        .args([
            "-c",
            script,
            path.to_str().expect("archive path"),
            binary.to_str().expect("binary path"),
            manifest.to_str().expect("manifest path"),
        ])
        .status()
        .expect("zip fixture");
    assert!(status.success(), "zip fixture failed");
}

fn file_sha256(path: &Path) -> String {
    hex_sha256(&fs::read(path).expect("hash file"))
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(&mut output, "{byte:02x}").expect("write hash");
            output
        })
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("fake command");
    let mut permissions = fs::metadata(path).expect("fake metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fake permissions");
}

fn write_fake_commands(bin_dir: &Path) {
    write_executable(bin_dir.join("gh").as_path(), GH_FIXTURE);
    write_executable(bin_dir.join("curl").as_path(), CURL_FIXTURE);
    write_executable(bin_dir.join("git").as_path(), GIT_FIXTURE);
    write_executable(bin_dir.join("velnor-workflow").as_path(), VELNOR_FIXTURE);
    write_executable(bin_dir.join("uname").as_path(), UNAME_FIXTURE);
    write_executable(bin_dir.join("timeout").as_path(), TIMEOUT_FIXTURE);
    write_executable(bin_dir.join("sha256sum").as_path(), SHA256SUM_FIXTURE);
    write_executable(bin_dir.join("stat").as_path(), STAT_FIXTURE);
    write_executable(bin_dir.join("du").as_path(), DU_FIXTURE);
    write_executable(bin_dir.join("docker").as_path(), DOCKER_FIXTURE);
}

const OWNER_CONFIG: &str = r#"schema = 2

[generator]
repository = "tailrocks/velnor"
revision = "3333333333333333333333333333333333333333"

[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]

"#;

const GH_FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, shutil, sys, zipfile
scenario = json.load(open(os.environ["FIXTURE_SCENARIO"]))
args = sys.argv[1:]
if args and args[0] == "run":
    destination = args[args.index("--dir") + 1]
    os.makedirs(destination, exist_ok=True)
    with zipfile.ZipFile(os.environ["FIXTURE_ARCHIVE"]) as archive:
        archive.extractall(destination)
    raise SystemExit(0)
url = next((arg for arg in args if "repos/" in arg), "")
if url.endswith("/actions/workflows/ci-pr.yml"):
    print(json.dumps({"path": ".github/workflows/ci-pr.yml", "id": 700}))
elif "/commits/" in url:
    sha = url.rsplit("/commits/", 1)[1].split("?", 1)[0]
    tree = scenario["api_base_tree"] if sha == os.environ["BASE_SHA"] else scenario["api_head_tree"]
    print(json.dumps({"sha": sha, "commit": {"tree": {"sha": tree}}}))
elif url.rstrip("/") == "repos/" + os.environ["GITHUB_REPOSITORY"]:
    print(json.dumps({"id": 42, "full_name": os.environ["GITHUB_REPOSITORY"]}))
elif "/runs/" in url and "/jobs" in url:
    print(json.dumps([scenario["jobs"]]))
elif "/runs/" in url and "/artifacts" in url:
    print(json.dumps([scenario["artifacts"]]))
elif "/runs?" in url:
    if "--slurp" in args:
        print(json.dumps([scenario["runs"]]))
    else:
        print(json.dumps({"workflow_runs": scenario["runs"]}))
else:
    print("{}")
"#;

const CURL_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, shutil, sys
args = sys.argv[1:]
shutil.copyfile(os.environ["FIXTURE_ARCHIVE"], args[args.index("--output") + 1])
"#;

const GIT_FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
scenario = json.load(open(os.environ["FIXTURE_SCENARIO"]))
commands = {"init", "fetch", "show", "archive", "ls-tree", "cat-file", "rev-parse"}
command_index = next((index for index, value in enumerate(args) if value in commands), len(args))
command = args[command_index] if command_index < len(args) else ""
rest = args[command_index + 1:]
if command == "init":
    os.makedirs(args[-1], exist_ok=True)
elif command == "fetch":
    pass
elif command == "rev-parse":
    target = rest[-1]
    if target.startswith(os.environ["BASE_SHA"]):
        print(scenario["object_base_tree"])
    else:
        print(scenario["object_head_tree"])
elif command == "show":
    target = rest[-1]
    if target.endswith(":.github/workflows/ci-pr.yml"):
        path = os.environ["FIXTURE_HEAD_CONTRACT"] if target.startswith(os.environ["HEAD_SHA"] + ":") else os.environ["FIXTURE_CONTRACT"]
        sys.stdout.write(open(path).read())
    elif "--format=%T" in rest:
        sha = target
        print(scenario["object_base_tree"] if sha == os.environ["BASE_SHA"] else scenario["object_head_tree"])
elif command == "archive":
    sys.stdout.buffer.write(open(os.environ["FIXTURE_SOURCE_ARCHIVE"], "rb").read())
elif command == "ls-tree":
    print("100644 blob 4444444444444444444444444444444444444444\tCargo.toml")
elif command == "cat-file":
    raise SystemExit(0)
"#;

const VELNOR_FIXTURE: &str = r#"#!/bin/sh
case "$*" in
  *--candidate*) printf '%s\n' "${FIXTURE_CLOSURE}" ;;
  *--rev=3333333333333333333333333333333333333333*) printf '%s\n' "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" ;;
  *) printf '%s\n' "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" ;;
esac
"#;

const UNAME_FIXTURE: &str = r#"#!/bin/sh
if [ "$1" = "-m" ]; then printf 'x86_64\n'; else /usr/bin/uname "$@"; fi
"#;

const TIMEOUT_FIXTURE: &str = r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    --foreground|--kill-after=*) shift ;;
    *s) shift; break ;;
    *) break ;;
  esac
done
exec "$@"
"#;

const SHA256SUM_FIXTURE: &str = r#"#!/usr/bin/env python3
import hashlib, sys
for path in sys.argv[1:]:
    with open(path, "rb") as stream:
        print(hashlib.sha256(stream.read()).hexdigest(), path)
"#;

const STAT_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, sys
if sys.argv[1:3] == ["-c", "%s"]:
    print(os.path.getsize(sys.argv[3]))
else:
    raise SystemExit(1)
"#;

const DU_FIXTURE: &str = r"#!/usr/bin/env python3
import os, sys
path = sys.argv[-1]
total = 0
for root, _, files in os.walk(path):
    for name in files:
        total += os.path.getsize(os.path.join(root, name))
print(total, path)
";

const DOCKER_FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
config_dir = os.environ.get("DOCKER_CONFIG", "/tmp/velnor-docker-fixture")
os.makedirs(config_dir, exist_ok=True)
state_path = os.path.join(config_dir, "create.json")
log_path = os.path.join(os.path.dirname(config_dir), "docker.log")
def log(op, values):
    with open(log_path, "a") as stream:
        stream.write(json.dumps({"op": op, "args": values}) + "\n")
if not args:
    raise SystemExit(1)
if args[:1] == ["info"]:
    if "--format" in args:
        print("linux")
    raise SystemExit(0)
if args[:2] == ["buildx", "imagetools"] and "inspect" in args:
    if "--raw" in args:
        if args[-1].endswith("b" * 64):
            print(json.dumps({"config": {"digest": "sha256:" + "c" * 64}, "layers": []}))
        else:
            print(json.dumps({"manifests": [{"platform": {"os": "linux", "architecture": "amd64"}, "digest": "sha256:" + "b" * 64}]}))
    else:
        print(json.dumps({"architecture": "amd64", "os": "linux", "config": {"Env": ["PATH=/usr/bin:/bin"], "User": "", "Entrypoint": [], "Cmd": [], "WorkingDir": "/", "Volumes": None, "ExposedPorts": None, "Healthcheck": None, "Labels": {"org.velnor.sandbox": "true"}}}))
    raise SystemExit(0)
if args[:2] == ["image", "inspect"]:
    ref = next((value for value in args[2:] if not value.startswith("--")), "fixture")
    print(json.dumps([{"Id": "sha256:" + "c" * 64, "RepoDigests": [ref], "Os": "linux", "Architecture": "amd64", "Config": {"Env": ["PATH=/usr/bin:/bin"], "Entrypoint": [], "Cmd": [], "User": "", "Volumes": None, "ExposedPorts": None, "Healthcheck": None}}]))
    raise SystemExit(0)
if args[:1] == ["pull"]:
    raise SystemExit(0)
if args[:1] == ["create"]:
    with open(state_path, "w") as stream:
        json.dump(args, stream)
    log("create", args)
    print("execute-cid")
    raise SystemExit(0)
if args[:1] == ["inspect"]:
    create_args = json.load(open(state_path))
    def value_after(flag, default=""):
        for index, value in enumerate(create_args):
            if value == flag and index + 1 < len(create_args):
                return create_args[index + 1]
            if value.startswith(flag + "="):
                return value.split("=", 1)[1]
        return default
    def parse_size(value):
        units = {"k": 1024, "m": 1024 ** 2, "g": 1024 ** 3}
        suffix = value[-1:].lower()
        return int(float(value[:-1]) * units[suffix]) if suffix in units else int(value)
    envs = [create_args[index + 1] for index, value in enumerate(create_args[:-1]) if value == "--env"]
    user = value_after("--user", "65532:65532")
    workdir = value_after("--workdir", "/input")
    entrypoint = value_after("--entrypoint", "/candidate/velnor-workflow")
    mounts, tmpfs = [], {}
    for index, value in enumerate(create_args[:-1]):
        if value == "--mount":
            fields = dict(item.split("=", 1) for item in create_args[index + 1].split(",") if "=" in item)
            mounts.append({"Type": fields.get("type", "bind"), "Source": fields.get("src", ""), "Destination": fields.get("dst", ""), "RW": False, "Propagation": "rprivate"})
        if value == "--tmpfs":
            destination, options = create_args[index + 1].split(":", 1)
            tmpfs[destination] = options
            mounts.append({"Type": "tmpfs", "Destination": destination, "RW": True})
    pids = int(value_after("--pids-limit", "128"))
    memory = parse_size(value_after("--memory", "512m"))
    memory_swap = parse_size(value_after("--memory-swap", "512m"))
    cpus = float(value_after("--cpus", "1"))
    ulimits = []
    for index, value in enumerate(create_args[:-1]):
        if value == "--ulimit":
            name, limits = create_args[index + 1].split("=", 1)
            soft, hard = limits.split(":", 1) if ":" in limits else (limits, limits)
            ulimits.append({"Name": name, "Soft": int(soft), "Hard": int(hard)})
    document = [{"HostConfig": {"NetworkMode": "none", "ReadonlyRootfs": True, "Privileged": False, "CapDrop": ["ALL"], "CapAdd": [], "PidMode": "private", "IpcMode": "private", "UTSMode": "", "UsernsMode": "", "SecurityOpt": ["no-new-privileges=true"], "PidsLimit": pids, "Memory": memory, "MemorySwap": memory_swap, "NanoCpus": int(cpus * 1000000000), "LogConfig": {"Type": "none"}, "Binds": [], "Devices": [], "DeviceRequests": [], "Tmpfs": tmpfs, "Ulimits": ulimits}, "Config": {"User": user, "WorkingDir": workdir, "Entrypoint": [entrypoint], "Cmd": [], "Env": envs}, "Mounts": mounts, "State": {"Status": "exited", "ExitCode": 0, "OOMKilled": False, "Error": ""}}]
    print(json.dumps(document))
    raise SystemExit(0)
if args[:1] in (["start"], ["wait"], ["rm"], ["kill"]):
    if args[:1] == ["wait"]: print("0")
    raise SystemExit(0)
if args[:1] == ["cp"]:
    destination = args[-1]
    os.makedirs(destination, exist_ok=True)
    create_args = json.load(open(state_path))
    producer = any(value.startswith("velnor-candidate-build-") for value in create_args)
    if producer:
        with open(os.path.join(destination, "velnor-workflow"), "wb") as stream: stream.write(b"offline producer output\n")
    else:
        with open(os.path.join(destination, "extra.txt"), "w") as stream: stream.write("unexpected")
        os.symlink("extra.txt", os.path.join(destination, "output-link"))
    log("cp", args)
    raise SystemExit(0)
raise SystemExit(0)
"#;
