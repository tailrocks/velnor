//! Offline executable fixtures for the owner bootstrap transport.
//!
//! The generated producer and acquisition shells run with fake API/tool
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
const CLOSURE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE_PIN: &str = "3333333333333333333333333333333333333333";
const WORKFLOW_ID: u64 = 700;
const RUN_ID: u64 = 900;
const RUN_ATTEMPT: u64 = 2;
const JOB_ID: u64 = 800;
const ARTIFACT_ID: u64 = 1000;
// Bootstrap API contract: reject an artifact before download when its
// advertised compressed size exceeds 256 MiB.
const MAX_CANDIDATE_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
const PLATFORM: &str = "Linux-X64";

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Generated {
    producer: String,
    acquire: String,
    publish_name: String,
    publish_path: String,
    producer_workflow: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureCase {
    ServiceDigestMismatch,
    WrongPlatform,
    WrongRepository,
    WrongRun,
    WrongWorkflow,
    WrongRunPath,
    WrongRunEvent,
    WrongRunHead,
    WrongArtifactRun,
    RunAttemptMismatch,
    FailedRun,
    ArtifactOutsideJobWindow,
    OversizedArtifact,
    ArtifactUpdatedBeforeCreated,
    WrongRevision,
    WrongClosure,
    WrongBinaryDigest,
    WrongManifestJobKey,
    WrongOrigin,
    MalformedWorkflow,
    HungApi,
    DuplicateMatchingJobs,
    StaleArtifact,
    DuplicateRun,
    DuplicateArtifact,
    MissingArtifact,
    PendingRunWithoutArtifact,
    ContinuationPage,
    JobsContinuationPage,
    ArtifactsContinuationPage,
    RedirectWithCredentials,
    RedirectHttp,
    SecondRedirect,
    PartialDownload,
    OversizedDownload,
    ForkHead,
    UnsupportedEvent,
    ArchiveExtraMember,
    ArchiveTraversal,
    ArchiveDuplicateMember,
    ArchiveSymlinkMember,
}

struct TransportFixture {
    root: PathBuf,
    generated: Generated,
    bin_dir: PathBuf,
    archive: PathBuf,
    scenario: PathBuf,
    contract: PathBuf,
    head_contract: PathBuf,
    binary: PathBuf,
    manifest: PathBuf,
    run_temp: PathBuf,
    sleep_marker: PathBuf,
    execution_sentinel: PathBuf,
    curl_log: PathBuf,
    gh_log: PathBuf,
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
        fs::write(
            root.join(".github-gen/visibility.toml"),
            "repository = \"tailrocks/velnor\"\nvisibility = \"public\"\n",
        )
        .expect("visibility config");

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
        let producer = step_run(&pr, "Build candidate generator product");
        let acquire = step_run(&policy, "Acquire candidate generator product");
        let publish_name = step_with(&pr, "Publish candidate generator product", "name");
        let publish_path = step_with(&pr, "Publish candidate generator product", "path");
        // Candidate bootstrap names are closure- and runner-qualified by the
        // producer. Keep the fixture independent of a rendered expression.
        let bin_dir = root.join("fake-bin");
        fs::create_dir_all(&bin_dir).expect("fake bin");
        write_fake_commands(&bin_dir);
        let archive = root.join("candidate.zip");
        let scenario = root.join("scenario.json");
        let contract = root.join("ci-pr-contract.yml");
        let head_contract = root.join("ci-pr-head.yml");
        fs::write(&contract, &pr).expect("contract");
        fs::write(&head_contract, &pr).expect("head contract");
        let binary = root.join("candidate.bin");
        let manifest = root.join("candidate-manifest.json");
        let run_temp = root.join("runner-temp");
        fs::create_dir_all(&run_temp).expect("runner temp");
        let sleep_marker = root.join("sleep-called");
        let execution_sentinel = root.join("candidate-executed");
        let curl_log = root.join("curl.log");
        let gh_log = root.join("gh.log");

        Self {
            root,
            generated: Generated {
                producer,
                acquire,
                publish_name,
                publish_path,
                producer_workflow: pr,
            },
            bin_dir,
            archive,
            scenario,
            contract,
            head_contract,
            binary,
            manifest,
            run_temp,
            sleep_marker,
            execution_sentinel,
            curl_log,
            gh_log,
        }
    }

    fn valid_artifact(&self) {
        self.valid_artifact_on_platform("Linux", "X64");
    }

    fn valid_artifact_on_platform(&self, runner_os: &str, runner_arch: &str) {
        let platform = format!("{runner_os}-{runner_arch}");
        let artifact_name = format!("velnor-workflow-candidate-{}-{platform}", &CLOSURE[..16]);
        fs::write(
            &self.binary,
            "#!/bin/sh\nprintf '%s\\n' 'executed' > \"$FIXTURE_EXECUTION_SENTINEL\"\nprintf '%s\\n' 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'\n",
        )
            .expect("candidate fixture");
        let mut permissions = fs::metadata(&self.binary)
            .expect("candidate metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&self.binary, permissions).expect("candidate permissions");
        let manifest_value = json!({
            "profile": "debug",
            "features": "tui",
            "platform": platform,
            "repository": REPOSITORY,
            "repository_id": "42",
            "head_repository": REPOSITORY,
            "head_repository_id": "42",
            "run_id": RUN_ID.to_string(),
            "job_id": "candidate-bootstrap",
            "revision": HEAD_SHA,
            "closure": CLOSURE,
            "build_revision": HEAD_SHA,
            "binary_sha256": file_sha256(&self.binary),
        });
        fs::write(
            &self.manifest,
            serde_json::to_vec(&manifest_value).expect("manifest JSON"),
        )
        .expect("manifest");
        zip_files(&self.archive, &self.binary, &self.manifest);
        let archive_sha = file_sha256(&self.archive);
        let mut scenario = valid_scenario(&artifact_name, &format!("sha256:{archive_sha}"));
        scenario["artifacts"][0]["size_in_bytes"] =
            json!(fs::metadata(&self.archive).expect("archive metadata").len());
        fs::write(
            &self.scenario,
            serde_json::to_vec(&scenario).expect("scenario JSON"),
        )
        .expect("scenario");
    }

    fn run_acquire(&self, failure: Option<FailureCase>) -> Output {
        self.run_acquire_on_platform(failure, "Linux", "X64")
    }

    fn run_acquire_on_platform(
        &self,
        failure: Option<FailureCase>,
        runner_os: &str,
        runner_arch: &str,
    ) -> Output {
        let mut scenario: JsonValue =
            serde_json::from_slice(&fs::read(&self.scenario).expect("scenario bytes"))
                .expect("scenario JSON");
        match failure {
            Some(FailureCase::ServiceDigestMismatch) => {
                scenario["artifacts"][0]["digest"] = json!(format!("sha256:{}", "c".repeat(64)));
            }
            Some(FailureCase::WrongPlatform) => self.mutate_manifest(|manifest| {
                manifest["platform"] = json!("Linux-ARM64");
            }),
            Some(FailureCase::WrongRepository) => self.mutate_manifest(|manifest| {
                manifest["repository"] = json!("someone-else/project");
            }),
            Some(FailureCase::WrongRun) => self.mutate_manifest(|manifest| {
                manifest["run_id"] = json!((RUN_ID + 1).to_string());
            }),
            Some(FailureCase::WrongWorkflow) => {
                scenario["runs"][0]["workflow_id"] = json!(WORKFLOW_ID + 1);
            }
            Some(FailureCase::WrongRunHead) => {
                scenario["runs"][0]["head_sha"] = json!(BASE_PIN);
            }
            Some(FailureCase::WrongRunPath) => {
                scenario["runs"][0]["path"] = json!(".github/workflows/other.yml");
            }
            Some(FailureCase::WrongRunEvent) => {
                scenario["runs"][0]["event"] = json!("push");
            }
            Some(FailureCase::WrongArtifactRun) => {
                scenario["artifacts"][0]["workflow_run"]["id"] = json!(RUN_ID + 1);
            }
            Some(FailureCase::RunAttemptMismatch) => {
                scenario["jobs"][0]["run_attempt"] = json!(RUN_ATTEMPT + 1);
            }
            Some(FailureCase::FailedRun) => {
                scenario["runs"][0]["conclusion"] = json!("failure");
            }
            Some(FailureCase::ArtifactOutsideJobWindow) => {
                scenario["artifacts"][0]["created_at"] = json!("2026-09-20T00:00:30Z");
            }
            Some(FailureCase::OversizedArtifact) => {
                scenario["artifacts"][0]["size_in_bytes"] = json!(MAX_CANDIDATE_ARTIFACT_BYTES + 1);
            }
            Some(FailureCase::ArtifactUpdatedBeforeCreated) => {
                scenario["artifacts"][0]["updated_at"] = json!("2026-09-20T00:01:00Z");
            }
            Some(FailureCase::WrongRevision) => self.mutate_manifest(|manifest| {
                manifest["revision"] = json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            }),
            Some(FailureCase::WrongClosure) => self.mutate_manifest(|manifest| {
                manifest["closure"] =
                    json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            }),
            Some(FailureCase::WrongBinaryDigest) => self.mutate_manifest(|manifest| {
                manifest["binary_sha256"] = json!("c".repeat(64));
            }),
            Some(FailureCase::WrongManifestJobKey) => self.mutate_manifest(|manifest| {
                manifest["job_id"] = json!("other-job");
            }),
            Some(
                FailureCase::WrongOrigin
                | FailureCase::MalformedWorkflow
                | FailureCase::HungApi
                | FailureCase::DuplicateMatchingJobs
                | FailureCase::SecondRedirect
                | FailureCase::PartialDownload,
            ) => {}
            Some(FailureCase::StaleArtifact) => {
                scenario["artifacts"][0]["expired"] = json!(true);
                scenario["artifacts"][0]["expires_at"] = json!("2000-01-01T00:00:00Z");
            }
            Some(FailureCase::DuplicateRun) => {
                let original_run = scenario["runs"][0].clone();
                let mut duplicate_run = original_run.clone();
                duplicate_run["id"] = json!(RUN_ID + 1);
                scenario["runs"] = json!([original_run, duplicate_run]);
            }
            Some(FailureCase::DuplicateArtifact) => {
                let original = scenario["artifacts"][0].clone();
                let mut duplicate = original.clone();
                duplicate["id"] = json!(ARTIFACT_ID + 1);
                scenario["artifacts"] = json!([original, duplicate]);
            }
            Some(FailureCase::MissingArtifact) => scenario["artifacts"] = json!([]),
            Some(FailureCase::PendingRunWithoutArtifact) => {
                scenario["runs"][0]["status"] = json!("in_progress");
                scenario["artifacts"] = json!([]);
            }
            Some(FailureCase::ContinuationPage) => {
                scenario["runs_total_count"] = json!(2);
            }
            Some(FailureCase::JobsContinuationPage) => {
                scenario["jobs_total_count"] = json!(2);
            }
            Some(FailureCase::ArtifactsContinuationPage) => {
                scenario["artifacts_total_count"] = json!(2);
            }
            Some(FailureCase::RedirectWithCredentials)
            | Some(FailureCase::RedirectHttp)
            | Some(FailureCase::OversizedDownload) => {}
            Some(
                FailureCase::ArchiveExtraMember
                | FailureCase::ArchiveTraversal
                | FailureCase::ArchiveDuplicateMember
                | FailureCase::ArchiveSymlinkMember,
            ) => {}
            Some(FailureCase::ForkHead | FailureCase::UnsupportedEvent) | None => {}
        }
        fs::write(
            &self.head_contract,
            fs::read(&self.contract).expect("contract bytes"),
        )
        .expect("head contract");
        // Repack the archive after a manifest mutation. The consumer verifies
        // the manifest against the downloaded bytes, so a fixture that merely
        // changes JSON outside the transported archive would be meaningless.
        let manifest_mutation = failure.is_some_and(|case| {
            matches!(
                case,
                FailureCase::WrongPlatform
                    | FailureCase::WrongRepository
                    | FailureCase::WrongRun
                    | FailureCase::WrongRevision
                    | FailureCase::WrongClosure
                    | FailureCase::WrongBinaryDigest
                    | FailureCase::WrongManifestJobKey
            )
        });
        if manifest_mutation {
            zip_files(&self.archive, &self.binary, &self.manifest);
            self.refresh_archive_metadata(&mut scenario);
        }
        if failure == Some(FailureCase::ArchiveExtraMember) {
            zip_files_with_extra_member(&self.archive, &self.binary, &self.manifest);
            self.refresh_archive_metadata(&mut scenario);
        }
        if failure == Some(FailureCase::ArchiveTraversal) {
            zip_files_with_traversal(&self.archive, &self.binary, &self.manifest);
            self.refresh_archive_metadata(&mut scenario);
        }
        if failure == Some(FailureCase::ArchiveDuplicateMember) {
            zip_files_with_duplicate_member(&self.archive, &self.binary, &self.manifest);
            self.refresh_archive_metadata(&mut scenario);
        }
        if failure == Some(FailureCase::ArchiveSymlinkMember) {
            zip_files_with_symlink_member(&self.archive, &self.binary, &self.manifest);
            self.refresh_archive_metadata(&mut scenario);
        }
        fs::write(
            &self.scenario,
            serde_json::to_vec(&scenario).expect("scenario JSON"),
        )
        .expect("scenario");
        let _ = fs::remove_file(&self.sleep_marker);
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
            .env("FIXTURE_CLOSURE", CLOSURE)
            .env("FIXTURE_EXECUTION_SENTINEL", &self.execution_sentinel)
            .env("RUNNER_TEMP", &self.run_temp)
            .env("GITHUB_REPOSITORY", REPOSITORY)
            .env("GITHUB_REPOSITORY_ID", "42")
            .env(
                "GITHUB_SERVER_URL",
                if failure == Some(FailureCase::WrongOrigin) {
                    "https://attacker.invalid"
                } else {
                    "https://github.com"
                },
            )
            .env("RUNNER_OS", runner_os)
            .env("RUNNER_ARCH", runner_arch)
            .env("GH_TOKEN", "fixture-token")
            .env(
                "GITHUB_API_URL",
                if failure == Some(FailureCase::WrongOrigin) {
                    "https://api.attacker.invalid"
                } else {
                    "https://api.github.com"
                },
            )
            .env("GH_HOST", "attacker.invalid")
            .env(
                "EVENT_NAME",
                if failure == Some(FailureCase::UnsupportedEvent) {
                    "workflow_dispatch"
                } else {
                    "pull_request_target"
                },
            )
            .env("MERGE_SHA", HEAD_SHA)
            .env("PR_HEAD_SHA", HEAD_SHA)
            .env("PR_HEAD_REPOSITORY_ID", "42")
            .env("HEAD_SHA", HEAD_SHA)
            .env("BASE_SHA", BASE_PIN)
            .env("GITHUB_RUN_ID", RUN_ID.to_string())
            .env("GITHUB_RUN_ATTEMPT", RUN_ATTEMPT.to_string())
            .env("PR_NUMBER", "7")
            .env(
                "PR_HEAD_REPOSITORY",
                if failure == Some(FailureCase::ForkHead) {
                    "attacker/fork"
                } else {
                    REPOSITORY
                },
            )
            .env("DEFAULT_BRANCH", "main")
            .env("BASE_PIN", BASE_PIN)
            .env("GITHUB_ENV", self.run_temp.join("github.env"));
        command.env("FIXTURE_SLEEP_MARKER", &self.sleep_marker);
        command.env("FIXTURE_CURL_LOG", &self.curl_log);
        command.env("FIXTURE_GH_LOG", &self.gh_log);
        command.env(
            "FIXTURE_HANG_API",
            if failure == Some(FailureCase::HungApi) {
                "1"
            } else {
                ""
            },
        );
        command.env(
            "FIXTURE_MALFORMED_WORKFLOW",
            if failure == Some(FailureCase::MalformedWorkflow) {
                "1"
            } else {
                ""
            },
        );
        command.env(
            "FIXTURE_DUPLICATE_MATCHING_JOBS",
            if failure == Some(FailureCase::DuplicateMatchingJobs) {
                "1"
            } else {
                ""
            },
        );
        command.env(
            "FIXTURE_REDIRECT_MODE",
            match failure {
                Some(FailureCase::RedirectWithCredentials) => "credentials",
                Some(FailureCase::RedirectHttp) => "http",
                Some(FailureCase::OversizedDownload) => "oversized",
                Some(FailureCase::SecondRedirect) => "second-redirect",
                Some(FailureCase::PartialDownload) => "partial",
                _ => "",
            },
        );
        command.output().expect("run generated acquire shell")
    }

    fn mutate_manifest(&self, mutate: impl FnOnce(&mut JsonValue)) {
        let mut manifest: JsonValue =
            serde_json::from_slice(&fs::read(&self.manifest).expect("manifest bytes"))
                .expect("manifest JSON");
        mutate(&mut manifest);
        fs::write(
            &self.manifest,
            serde_json::to_vec(&manifest).expect("manifest JSON bytes"),
        )
        .expect("mutated manifest");
    }

    fn refresh_archive_metadata(&self, scenario: &mut JsonValue) {
        scenario["artifacts"][0]["size_in_bytes"] =
            json!(fs::metadata(&self.archive).expect("archive metadata").len());
        scenario["artifacts"][0]["digest"] =
            json!(format!("sha256:{}", file_sha256(&self.archive)));
    }

    fn run_producer_build(&self) -> Output {
        self.run_producer_build_on_platform("Linux", "X64")
    }

    fn run_producer_build_on_platform(&self, runner_os: &str, runner_arch: &str) -> Output {
        self.run_producer_build_on_platform_with_check(runner_os, runner_arch, false)
    }

    fn run_producer_build_on_platform_with_check(
        &self,
        runner_os: &str,
        runner_arch: &str,
        check_ok: bool,
    ) -> Output {
        let _ = fs::remove_dir_all(&self.run_temp);
        fs::create_dir_all(&self.run_temp).expect("run temp");
        let script = self.generated.producer.clone();
        Command::new("bash")
            .args(["-euo", "pipefail", "-c", &script])
            .current_dir(&self.root)
            .env("PATH", prepend_path(&self.bin_dir))
            .env("RUNNER_TEMP", &self.run_temp)
            .env("CARGO_HOME", self.run_temp.join("cargo-home"))
            .env("GITHUB_REPOSITORY", REPOSITORY)
            .env("RUNNER_OS", runner_os)
            .env("RUNNER_ARCH", runner_arch)
            .env("CANDIDATE_PR_HEAD_SHA", HEAD_SHA)
            .env("CANDIDATE_REPOSITORY", REPOSITORY)
            .env("CANDIDATE_REPOSITORY_ID", "42")
            .env("CANDIDATE_HEAD_REPOSITORY", REPOSITORY)
            .env("CANDIDATE_HEAD_REPOSITORY_ID", "42")
            .env("CANDIDATE_RUN_ID", RUN_ID.to_string())
            .env("CANDIDATE_JOB_ID", "candidate-bootstrap")
            .env("BASE_PIN", BASE_PIN)
            .env("FIXTURE_CLOSURE", CLOSURE)
            .env("FIXTURE_CHECK_OK", if check_ok { "1" } else { "" })
            .env("FIXTURE_SCENARIO", &self.scenario)
            .env("GITHUB_SERVER_URL", "https://github.com")
            .env("GITHUB_API_URL", "https://api.github.com")
            .env("GITHUB_OUTPUT", self.run_temp.join("github.output"))
            .env_remove("GITHUB_TOKEN")
            .env_remove("ACTIONS_RUNTIME_TOKEN")
            .env_remove("ACTIONS_RUNTIME_URL")
            .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
            .env("GITHUB_TOKEN", "sensitive-github-token")
            .env("ACTIONS_RUNTIME_TOKEN", "sensitive-runtime-token")
            .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "sensitive-oidc-token")
            .env("SSH_AUTH_SOCK", "/tmp/sensitive-agent.sock")
            .env("HTTP_PROXY", "http://sensitive-proxy.invalid")
            .env("HTTPS_PROXY", "http://sensitive-proxy.invalid")
            .env("GIT_CONFIG_GLOBAL", "/tmp/sensitive-gitconfig")
            .env("RUSTFLAGS", "-C link-arg=sensitive")
            .output()
            .expect("run generated producer shell")
    }
}

impl Drop for TransportFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn generated_acquire_exports_the_downloaded_candidate_and_never_executes_it() {
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
    assert!(
        !fixture.execution_sentinel.exists(),
        "token-bearing acquisition executed the transported candidate"
    );
    let github_env = fs::read_to_string(fixture.run_temp.join("github.env"))
        .expect("acquire environment exports");
    let binary_path = github_env
        .lines()
        .find_map(|line| line.strip_prefix("VELNOR_WORKFLOW_CANDIDATE_BINARY="))
        .expect("candidate binary export")
        .to_owned();
    let manifest_path = github_env
        .lines()
        .find_map(|line| line.strip_prefix("VELNOR_WORKFLOW_CANDIDATE_MANIFEST="))
        .expect("candidate manifest export")
        .to_owned();
    assert!(github_env
        .lines()
        .any(|line| line == "HEAD_REPOSITORY=tailrocks/velnor"));
    assert!(github_env
        .lines()
        .any(|line| line == "HEAD_REPOSITORY_ID=42"));
    assert!(github_env
        .lines()
        .any(|line| line == "VELNOR_WORKFLOW_CANDIDATE_RUN_ID=900"));
    assert!(github_env
        .lines()
        .any(|line| line == "VELNOR_WORKFLOW_CANDIDATE_JOB_ID=candidate-bootstrap"));
    assert!(
        Path::new(&binary_path).is_file(),
        "downloaded binary exists"
    );
    assert!(
        Path::new(&manifest_path).is_file(),
        "downloaded manifest exists"
    );
    let manifest: JsonValue =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest bytes"))
            .expect("manifest JSON");
    assert_eq!(manifest["repository"], json!(REPOSITORY));
    assert_eq!(manifest["run_id"], json!(RUN_ID.to_string()));
    assert_eq!(manifest["revision"], json!(HEAD_SHA));
    assert_eq!(manifest["closure"], json!(CLOSURE));
    assert_eq!(
        file_sha256(Path::new(&binary_path)),
        manifest["binary_sha256"]
    );
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
    let curl_log = fs::read_to_string(&baseline.curl_log).expect("curl transport log");
    assert!(
        curl_log.contains("signed-plain") && !curl_log.contains("signed-authorized"),
        "the signed artifact request must not carry the GitHub token: {curl_log}"
    );
    let gh_log = fs::read_to_string(&baseline.gh_log).expect("GitHub API transport log");
    for line in gh_log.lines() {
        let args: Vec<String> = serde_json::from_str(line).expect("GitHub API argv JSON");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--hostname", "github.com"]),
            "every GitHub API call must bind the GitHub.com hostname: {args:?}"
        );
    }
    assert!(
        !gh_log.contains("attacker.invalid"),
        "GH_HOST must not override the explicit API hostname: {gh_log}"
    );

    // The fixture deliberately returns mutated response records even when the
    // request carries head/event filters. These cases prove the consumer
    // validates response fields instead of treating URL query parameters as
    // an integrity proof. ArchiveExtraMember is intentionally limited to the
    // post-extraction archive contract; traversal, duplicate, and symlink
    // members are exercised by dedicated central-directory fixtures below. The
    // validator checks member count before the allowlist: traversal therefore
    // reports an unexpected member, while duplicate reports an unexpected
    // member count.
    let cases = [
        FailureCase::ServiceDigestMismatch,
        FailureCase::WrongPlatform,
        FailureCase::WrongRepository,
        FailureCase::WrongRun,
        FailureCase::WrongWorkflow,
        FailureCase::WrongRunPath,
        FailureCase::WrongRunEvent,
        FailureCase::WrongRunHead,
        FailureCase::WrongArtifactRun,
        FailureCase::RunAttemptMismatch,
        FailureCase::FailedRun,
        FailureCase::ArtifactOutsideJobWindow,
        FailureCase::OversizedArtifact,
        FailureCase::ArtifactUpdatedBeforeCreated,
        FailureCase::WrongRevision,
        FailureCase::WrongClosure,
        FailureCase::WrongBinaryDigest,
        FailureCase::WrongManifestJobKey,
        FailureCase::DuplicateMatchingJobs,
        FailureCase::StaleArtifact,
        FailureCase::DuplicateRun,
        FailureCase::DuplicateArtifact,
        FailureCase::MissingArtifact,
        FailureCase::PendingRunWithoutArtifact,
        FailureCase::ContinuationPage,
        FailureCase::JobsContinuationPage,
        FailureCase::ArtifactsContinuationPage,
        FailureCase::RedirectWithCredentials,
        FailureCase::RedirectHttp,
        FailureCase::OversizedDownload,
        FailureCase::SecondRedirect,
        FailureCase::PartialDownload,
        FailureCase::MalformedWorkflow,
        FailureCase::HungApi,
        FailureCase::WrongOrigin,
        FailureCase::ForkHead,
        FailureCase::UnsupportedEvent,
        FailureCase::ArchiveExtraMember,
        FailureCase::ArchiveTraversal,
        FailureCase::ArchiveDuplicateMember,
        FailureCase::ArchiveSymlinkMember,
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
        let expected_sleep = matches!(
            case,
            FailureCase::WrongRunPath
                | FailureCase::WrongRunEvent
                | FailureCase::WrongRunHead
                | FailureCase::StaleArtifact
                | FailureCase::MissingArtifact
                | FailureCase::PendingRunWithoutArtifact
                | FailureCase::WrongWorkflow
        );
        assert_eq!(
            fixture.sleep_marker.is_file(),
            expected_sleep,
            "{case:?} polling marker mismatch; stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if matches!(case, FailureCase::WrongOrigin | FailureCase::HungApi) {
            assert!(
                !fixture.gh_log.exists(),
                "{case:?} must fail before invoking the GitHub API"
            );
            assert!(
                !fixture.curl_log.exists(),
                "{case:?} must fail before starting artifact transport"
            );
            if case == FailureCase::HungApi {
                assert!(
                    !fixture.root.join("timeout-child.pid").exists(),
                    "hung API timeout must reap its child process"
                );
            }
        }
        if matches!(case, FailureCase::MalformedWorkflow) {
            assert!(
                !fixture.curl_log.exists(),
                "malformed workflow metadata must fail before artifact transport"
            );
            let gh_log = fs::read_to_string(&fixture.gh_log).expect("workflow API call log");
            assert_eq!(
                gh_log.lines().count(),
                1,
                "malformed workflow must stop after metadata"
            );
        }
        if matches!(case, FailureCase::DuplicateMatchingJobs) {
            assert!(
                !fixture.curl_log.exists(),
                "duplicate matching jobs must fail before artifact transport"
            );
        }
        if case == FailureCase::ArchiveTraversal {
            let curl_log = fs::read_to_string(&fixture.curl_log)
                .expect("archive traversal curl transport log");
            assert!(
                curl_log.contains("signed-plain"),
                "archive traversal must reach artifact transport: {curl_log}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("candidate archive contains an unexpected member"),
                "archive traversal rejection changed: {stderr}"
            );
        }
        if case == FailureCase::ArchiveDuplicateMember {
            let curl_log = fs::read_to_string(&fixture.curl_log)
                .expect("archive duplicate-member curl transport log");
            assert!(
                curl_log.contains("signed-plain"),
                "archive duplicate-member fixture must reach artifact transport: {curl_log}"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("candidate archive has an unexpected member count"),
                "archive duplicate-member rejection changed: {stderr}"
            );
        }
        if matches!(case, FailureCase::PartialDownload) {
            assert!(
                !fixture
                    .run_temp
                    .join("velnor-workflow-candidate.zip.part")
                    .exists(),
                "partial artifact download must remove its .part file"
            );
            if fixture.run_temp.join("github.env").exists() {
                let env = fs::read_to_string(fixture.run_temp.join("github.env"))
                    .expect("partial download environment");
                assert!(
                    !env.contains("VELNOR_WORKFLOW_CANDIDATE_MANIFEST="),
                    "partial download must not export a candidate manifest"
                );
            }
        }
    }
}

#[test]
fn generated_producer_keeps_upload_surface_after_build() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
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
    let manifest: JsonValue = serde_json::from_slice(
        &fs::read(stage.join("candidate-manifest.json")).expect("producer manifest bytes"),
    )
    .expect("producer manifest JSON");
    assert_eq!(manifest["profile"], json!("debug"));
    assert_eq!(manifest["features"], json!("tui"));
    assert_eq!(manifest["platform"], json!(PLATFORM));
    assert_eq!(manifest["repository"], json!(REPOSITORY));
    assert_eq!(manifest["repository_id"], json!("42"));
    assert_eq!(manifest["head_repository"], json!(REPOSITORY));
    assert_eq!(manifest["head_repository_id"], json!("42"));
    assert_eq!(manifest["run_id"], json!(RUN_ID.to_string()));
    assert_eq!(manifest["job_id"], json!("candidate-bootstrap"));
    assert_eq!(manifest["revision"], json!(HEAD_SHA));
    assert_eq!(manifest["closure"], json!(CLOSURE));
    assert_eq!(manifest["build_revision"], json!(HEAD_SHA));
    assert_eq!(
        file_sha256(&stage.join("velnor-workflow")),
        manifest["binary_sha256"]
    );
    let output =
        fs::read_to_string(fixture.run_temp.join("github.output")).expect("producer output");
    assert!(
        output.contains(&format!(
            "name=velnor-workflow-candidate-{}-{PLATFORM}",
            &CLOSURE[..16]
        )),
        "producer must publish the closure-qualified artifact name: {output}"
    );
    let build_env = fs::read_to_string(fixture.root.join("candidate-build-env.txt"))
        .expect("hermetic cargo environment capture");
    let probe_env = fs::read_to_string(fixture.root.join("candidate-probe-env.txt"))
        .expect("hermetic candidate probe environment capture");
    for forbidden in [
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN=",
        "ACTIONS_RUNTIME_TOKEN=",
        "GITHUB_TOKEN=",
        "SSH_AUTH_SOCK=",
        "HTTP_PROXY=",
        "HTTPS_PROXY=",
        "RUSTFLAGS=",
    ] {
        assert!(
            !build_env.contains(forbidden) && !probe_env.contains(forbidden),
            "candidate build/probe leaked {forbidden}: build={build_env:?} probe={probe_env:?}"
        );
    }
    assert!(
        build_env.contains("GIT_CONFIG_GLOBAL=/dev/null")
            && probe_env.contains("GIT_CONFIG_GLOBAL=/dev/null"),
        "candidate build/probe must use the isolated global Git config: build={build_env:?} probe={probe_env:?}"
    );
    assert!(build_env.contains("CARGO_HOME=") && build_env.contains("PATH="));
    assert!(probe_env.contains("HOME=") && probe_env.contains("PATH="));
    for marker in ["hermetic_env()", "env -i", "GIT_CONFIG_NOSYSTEM=1"] {
        assert!(
            fixture.generated.producer.contains(marker),
            "producer contract is missing hermetic guard {marker:?}: {}",
            fixture.generated.producer
        );
    }
}

#[test]
fn generated_producer_binds_runner_platform_and_upload_contract() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
    assert_eq!(
        fixture.generated.publish_name,
        "${{ steps.candidate.outputs.name }}"
    );
    assert_eq!(
        fixture.generated.publish_path,
        "${{ runner.temp }}/velnor-workflow-candidate"
    );
    assert_eq!(
        step_with(
            &fixture.generated.producer_workflow,
            "Publish candidate generator product",
            "if-no-files-found",
        ),
        "error"
    );
    assert!(
        fixture
            .generated
            .producer_workflow
            .contains("          retention-days: 1\n"),
        "candidate artifact retention must stay bounded"
    );
    assert!(
        fixture
            .generated
            .producer
            .contains(r#"--arg platform "${RUNNER_OS}-${RUNNER_ARCH}""#),
        "producer must derive manifest platform from runner variables: {}",
        fixture.generated.producer
    );
    assert!(
        fixture
            .generated
            .producer
            .contains(r#"stage="$RUNNER_TEMP/velnor-workflow-candidate""#),
        "producer must stage only under runner.temp: {}",
        fixture.generated.producer
    );

    let output = fixture.run_producer_build_on_platform("Linux", "ARM64");
    assert!(
        output.status.success(),
        "alternate platform producer failed ({:?}):\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stage = fixture.run_temp.join("velnor-workflow-candidate");
    let manifest: JsonValue = serde_json::from_slice(
        &fs::read(stage.join("candidate-manifest.json")).expect("alternate manifest bytes"),
    )
    .expect("alternate manifest JSON");
    assert_eq!(manifest["platform"], json!("Linux-ARM64"));
    let output = fs::read_to_string(fixture.run_temp.join("github.output"))
        .expect("alternate producer output");
    assert!(
        output.contains(&format!(
            "name=velnor-workflow-candidate-{}-Linux-ARM64",
            &CLOSURE[..16]
        )),
        "producer must publish runner-qualified alternate artifact name: {output}"
    );
}

#[test]
fn generated_producer_same_closure_requires_a_trusted_check_before_skipping() {
    let matches = TransportFixture::new();
    matches.valid_artifact();
    let output = matches.run_producer_build_on_platform_with_check("Linux", "X64", true);
    assert!(
        output.status.success(),
        "same-closure trusted check should permit the producer skip:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !matches.run_temp.join("velnor-workflow-candidate").exists(),
        "same-closure producer must not stage a candidate after a passing check"
    );

    let differs = TransportFixture::new();
    differs.valid_artifact();
    let output = differs.run_producer_build();
    assert!(
        output.status.success(),
        "same-closure check failure should fall through to the candidate build:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        differs.run_temp.join("velnor-workflow-candidate").is_dir(),
        "a failed same-closure check must keep the candidate upload path live"
    );
}

#[test]
fn generated_acquire_binds_runner_platform_to_artifact_and_manifest() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact_on_platform("Linux", "ARM64");
    assert!(
        fixture
            .generated
            .acquire
            .contains(&MAX_CANDIDATE_ARTIFACT_BYTES.to_string()),
        "acquire contract must enforce the 256 MiB API artifact-size cap"
    );
    for marker in [
        "workflow_id",
        "path == \".github/workflows/ci-pr.yml\"",
        "total_count",
        "timeout --signal=TERM --kill-after=5s 30s gh --hostname github.com api",
        "job_status",
        "job_conclusion",
        "artifact_grace_deadline",
        "completed successfully but did not publish",
        "--max-redirs 0",
        "--max-time 120",
        "env -u GH_TOKEN -u GITHUB_TOKEN",
        "candidate artifact exceeded its advertised size before disk write",
    ] {
        assert!(
            fixture.generated.acquire.contains(marker),
            "acquire contract is missing transport guard {marker:?}"
        );
    }
    let output = fixture.run_acquire_on_platform(None, "Linux", "ARM64");
    assert!(
        output.status.success(),
        "alternate platform acquire failed ({:?}):\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let github_env = fs::read_to_string(fixture.run_temp.join("github.env"))
        .expect("alternate acquire environment exports");
    let manifest_path = github_env
        .lines()
        .find_map(|line| line.strip_prefix("VELNOR_WORKFLOW_CANDIDATE_MANIFEST="))
        .expect("alternate candidate manifest export");
    let manifest: JsonValue = serde_json::from_slice(
        &fs::read(manifest_path).expect("alternate downloaded manifest bytes"),
    )
    .expect("alternate downloaded manifest JSON");
    assert_eq!(manifest["platform"], json!("Linux-ARM64"));

    let mismatch = TransportFixture::new();
    mismatch.valid_artifact();
    let output = mismatch.run_acquire_on_platform(None, "Linux", "ARM64");
    assert!(
        !output.status.success(),
        "X64 artifact unexpectedly passed an ARM64 acquire rendezvous"
    );
}

#[test]
fn generated_producer_cleans_upload_surface_after_publish() {
    let fixture = TransportFixture::new();
    let document = parse_workflow(&fixture.generated.producer_workflow);
    let jobs = document["jobs"].as_mapping().expect("producer jobs map");
    let has_cleanup_after_publish = jobs.values().any(|job| {
        let Some(steps) = job.get("steps").and_then(YamlValue::as_sequence) else {
            return false;
        };
        let Some(publish_index) = steps
            .iter()
            .position(|step| step["name"].as_str() == Some("Publish candidate generator product"))
        else {
            return false;
        };
        let cleanup_index = steps.iter().position(|step| {
            let Some(step) = step.as_mapping() else {
                return false;
            };
            step.get("if")
                .and_then(YamlValue::as_str)
                == Some("always()")
                && step
                    .get("run")
                    .and_then(YamlValue::as_str)
                    .is_some_and(|run| {
                    run.contains("rm -rf") && run.contains("velnor-workflow-candidate")
                })
        });
        cleanup_index.is_some_and(|index| index > publish_index)
    });
    assert!(
        has_cleanup_after_publish,
        "candidate upload surface needs an always-run cleanup step: {}",
        fixture.generated.producer_workflow
    );
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
    for (_job_id, job) in jobs {
        let Some(steps) = job.get("steps").and_then(YamlValue::as_sequence) else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some(name) {
                return step["run"].as_str().expect("step run").to_owned();
            }
        }
    }
    panic!("workflow step {name:?} not found in jobs: {jobs:#?}");
}

fn step_with(workflow: &str, step_name: &str, key: &str) -> String {
    let document = parse_workflow(workflow);
    let jobs = document["jobs"].as_mapping().expect("jobs map");
    for (_job_id, job) in jobs {
        let Some(steps) = job.get("steps").and_then(YamlValue::as_sequence) else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some(step_name) {
                return step["with"][key].as_str().expect("step input").to_owned();
            }
        }
    }
    panic!("workflow step {step_name:?} input {key:?} not found in jobs: {jobs:#?}");
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

fn valid_scenario(artifact_name: &str, service_digest: &str) -> JsonValue {
    json!({
        "runs": [{
            "id": RUN_ID, "workflow_id": WORKFLOW_ID,
            "path": ".github/workflows/ci-pr.yml",
            "event": "pull_request", "head_sha": HEAD_SHA, "status": "completed",
            "conclusion": "success",
            "run_attempt": RUN_ATTEMPT,
            "created_at": "2026-09-20T00:00:00Z",
            "repository": {"id": 42, "full_name": REPOSITORY},
            "head_repository": {"id": 42, "full_name": REPOSITORY},
            "pull_requests": [{"number": 7, "base": {"sha": BASE_PIN}}]
        }],
        "jobs": [{
            "id": JOB_ID, "name": "Control / Candidate bootstrap", "run_id": RUN_ID,
            "head_sha": HEAD_SHA, "status": "completed", "conclusion": "success",
            "run_attempt": RUN_ATTEMPT,
            "started_at": "2026-09-20T00:01:00Z",
            "completed_at": "2026-09-20T00:10:00Z"
        }],
        "artifacts": [{
            "id": ARTIFACT_ID, "name": artifact_name, "expired": false,
            "size_in_bytes": 1024, "digest": service_digest,
            "expires_at": "2099-01-01T00:00:00Z",
            "created_at": "2026-09-20T00:02:00Z", "updated_at": "2026-09-20T00:03:00Z",
            "workflow_run": {"id": RUN_ID}
        }]
    })
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

fn zip_files_with_extra_member(path: &Path, binary: &Path, manifest: &Path) {
    let script = r#"import sys, zipfile
archive, binary, manifest = sys.argv[1:]
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
    out.write(binary, "velnor-workflow")
    out.write(manifest, "candidate-manifest.json")
    out.writestr("unexpected.txt", b"unexpected")
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
        .expect("extra-member zip fixture");
    assert!(status.success(), "extra-member zip fixture failed");
}

fn zip_files_with_traversal(path: &Path, binary: &Path, manifest: &Path) {
    let script = r#"import sys, zipfile
archive, binary, manifest = sys.argv[1:]
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
    out.write(binary, "../velnor-workflow")
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
        .expect("traversal zip fixture");
    assert!(status.success(), "traversal zip fixture failed");
}

fn zip_files_with_duplicate_member(path: &Path, binary: &Path, manifest: &Path) {
    let script = r#"import sys, zipfile
archive, binary, manifest = sys.argv[1:]
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
    out.write(binary, "velnor-workflow")
    out.write(manifest, "candidate-manifest.json")
    out.writestr("candidate-manifest.json", b"duplicate")
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
        .expect("duplicate-member zip fixture");
    assert!(status.success(), "duplicate-member zip fixture failed");
}

fn zip_files_with_symlink_member(path: &Path, binary: &Path, manifest: &Path) {
    let script = r#"import stat, sys, zipfile
archive, binary, manifest = sys.argv[1:]
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
    link = zipfile.ZipInfo("velnor-workflow")
    link.external_attr = (stat.S_IFLNK | 0o777) << 16
    out.writestr(link, b"candidate-target")
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
        .expect("symlink-member zip fixture");
    assert!(status.success(), "symlink-member zip fixture failed");
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
    write_executable(bin_dir.join("cargo").as_path(), CARGO_FIXTURE);
    write_executable(bin_dir.join("git").as_path(), GIT_FIXTURE);
    write_executable(bin_dir.join("velnor-workflow").as_path(), VELNOR_FIXTURE);
    write_executable(bin_dir.join("sha256sum").as_path(), SHA256SUM_FIXTURE);
    write_executable(bin_dir.join("sleep").as_path(), SLEEP_FIXTURE);
    write_executable(bin_dir.join("timeout").as_path(), TIMEOUT_FIXTURE);
}

const OWNER_CONFIG: &str = r#"schema = 2

[generator]
repository = "tailrocks/velnor"
revision = "3333333333333333333333333333333333333333"

[workflow]
providers = ["github-hosted"]
automatic_providers = ["github-hosted"]

"#;

const GH_FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, shutil, sys, zipfile
scenario = json.load(open(os.environ["FIXTURE_SCENARIO"]))
args = sys.argv[1:]
if os.environ.get("FIXTURE_GH_LOG"):
    with open(os.environ["FIXTURE_GH_LOG"], "a") as log:
        log.write(json.dumps(args) + "\n")
if args and args[0] == "run":
    destination = args[args.index("--dir") + 1]
    os.makedirs(destination, exist_ok=True)
    with zipfile.ZipFile(os.environ["FIXTURE_ARCHIVE"]) as archive:
        archive.extractall(destination)
    raise SystemExit(0)
url = next((arg for arg in args if "repos/" in arg), "")
if url.endswith("/actions/workflows/ci-pr.yml"):
    if os.environ.get("FIXTURE_MALFORMED_WORKFLOW") == "1":
        print(json.dumps({"path": ".github/workflows/ci-pr.yml", "id": "not-a-number"}))
    else:
        print(json.dumps({"path": ".github/workflows/ci-pr.yml", "id": 700}))
elif "/commits/" in url:
    sha = url.rsplit("/commits/", 1)[1].split("?", 1)[0]
    tree = scenario["api_base_tree"] if sha == os.environ["BASE_SHA"] else scenario["api_head_tree"]
    print(json.dumps({"sha": sha, "commit": {"tree": {"sha": tree}}}))
elif url.rstrip("/") == "repos/" + os.environ["GITHUB_REPOSITORY"]:
    print(json.dumps({"id": 42, "full_name": os.environ["GITHUB_REPOSITORY"]}))
elif "/actions/runs/" in url and "/jobs" not in url and "/artifacts" not in url:
    print(json.dumps(scenario["runs"][0]))
elif "/actions/jobs/" in url:
    print(json.dumps(scenario["jobs"][0]))
elif "/actions/artifacts/" in url:
    if url.endswith("/zip"):
        payload = open(os.environ["FIXTURE_ARCHIVE"], "rb").read()
        if "--output" in args or "-o" in args:
            flag = "--output" if "--output" in args else "-o"
            destination = args[args.index(flag) + 1]
            if destination == "-":
                sys.stdout.buffer.write(payload)
            else:
                open(destination, "wb").write(payload)
        else:
            sys.stdout.buffer.write(payload)
    else:
        print(json.dumps(scenario["artifacts"][0]))
elif "/runs/" in url and "/jobs" in url:
    jobs = list(scenario.get("jobs", []))
    if os.environ.get("FIXTURE_DUPLICATE_MATCHING_JOBS") == "1":
        duplicate = dict(jobs[0])
        duplicate["conclusion"] = "failure"
        jobs.append(duplicate)
    page = {"total_count": scenario.get("jobs_total_count", len(jobs)), "jobs": jobs}
    print(json.dumps([page] if "--slurp" in args else page))
elif "/runs/" in url and "/artifacts" in url:
    run_id = url.split("/runs/", 1)[1].split("/", 1)[0].split("?", 1)[0]
    # Return every artifact in the response. The consumer must validate the
    # workflow_run binding; the fixture must not filter a forged mismatch out
    # before the source sees it.
    artifacts = scenario["artifacts"]
    page = {"total_count": scenario.get("artifacts_total_count", len(artifacts)), "artifacts": artifacts}
    print(json.dumps([page] if "--slurp" in args else page))
elif "/runs?" in url:
    if "--slurp" in args:
        print(json.dumps([scenario["runs"]]))
    else:
        print(json.dumps({"total_count": scenario.get("runs_total_count", len(scenario["runs"])), "workflow_runs": scenario["runs"]}))
else:
    print("{}")
"#;

const CURL_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
url = next((arg for arg in args if arg.startswith("http")), "")
if os.environ.get("FIXTURE_CURL_LOG"):
    with open(os.environ["FIXTURE_CURL_LOG"], "a") as log:
        log.write(("signed-authorized\n" if url.startswith("https://signed.") and any("Authorization:" in arg for arg in args) else "signed-plain\n" if url.startswith("https://signed.") else "api\n"))
if "/actions/artifacts/" in url and url.endswith("/zip") and "--dump-header" in args:
    header_path = args[args.index("--dump-header") + 1]
    mode = os.environ.get("FIXTURE_REDIRECT_MODE", "")
    location = {
        "credentials": "https://token:secret@signed.invalid/artifact",
        "http": "http://signed.invalid/artifact",
    }.get(mode, "https://signed.invalid/artifact")
    with open(header_path, "w") as headers:
        headers.write("HTTP/1.1 302 Found\r\nLocation: " + location + "\r\n\r\n")
    if "--write-out" in args:
        sys.stdout.write("302")
    raise SystemExit(0)
elif url.startswith("https://signed."):
    payload = open(os.environ["FIXTURE_ARCHIVE"], "rb").read()
    if os.environ.get("FIXTURE_REDIRECT_MODE") == "oversized":
        payload += b"overflow"
    if os.environ.get("FIXTURE_REDIRECT_MODE") == "second-redirect":
        if os.environ.get("FIXTURE_CURL_LOG"):
            with open(os.environ["FIXTURE_CURL_LOG"], "a") as log:
                log.write("signed-redirect\n")
        raise SystemExit(47)
    if os.environ.get("FIXTURE_REDIRECT_MODE") == "partial":
        sys.stdout.buffer.write(payload[: max(1, len(payload) // 2)])
        raise SystemExit(28)
else:
    payload = b"offline archive fixture\\n"
if "--output" in args or "-o" in args:
    flag = "--output" if "--output" in args else "-o"
    destination = args[args.index(flag) + 1]
    if destination == "-":
        sys.stdout.buffer.write(payload)
    else:
        open(destination, "wb").write(payload)
else:
    sys.stdout.buffer.write(payload)
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
    if target.endswith("^{commit}"):
        print(os.environ.get("HEAD_SHA", os.environ.get("CANDIDATE_PR_HEAD_SHA")))
    elif target.startswith(os.environ["BASE_SHA"]):
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
  *--check*)
    if [ "${FIXTURE_CHECK_OK:-}" = "1" ]; then
      exit 0
    fi
    printf '%s\n' "tree differs" >&2
    exit 1
    ;;
  *--rev=3333333333333333333333333333333333333333*)
    if [ "${FIXTURE_CHECK_OK:-}" = "1" ]; then
      printf '%s\n' "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    else
      printf '%s\n' "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    fi
    ;;
  *--candidate*) printf '%s\n' "${FIXTURE_CLOSURE}" ;;
  *) printf '%s\n' "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" ;;
esac
"#;

const CARGO_FIXTURE: &str = r#"#!/bin/sh
set -eu
env | sort > candidate-build-env.txt
mkdir -p target/debug
printf '%s\n' '#!/bin/sh' > target/debug/velnor-workflow
printf '%s\n' 'case "$1" in' >> target/debug/velnor-workflow
printf '%s\n' '  --closure) env | sort > candidate-probe-env.txt; printf "%s\n" "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ;;' >> target/debug/velnor-workflow
printf '%s\n' '  --revision) env | sort > candidate-probe-env.txt; printf "%s\n" "0123456789abcdef0123456789abcdef01234567" ;;' >> target/debug/velnor-workflow
printf '%s\n' '  *) exit 0 ;;' >> target/debug/velnor-workflow
printf '%s\n' 'esac' >> target/debug/velnor-workflow
chmod 0755 target/debug/velnor-workflow
"#;

const SHA256SUM_FIXTURE: &str = r#"#!/usr/bin/env python3
import hashlib, sys
for path in sys.argv[1:]:
    with open(path, "rb") as stream:
        print(hashlib.sha256(stream.read()).hexdigest(), path)
"#;

// Offline polling must fail fast rather than wait fifteen minutes or spin on
// a never-changing fake API response.
const SLEEP_FIXTURE: &str = r#"#!/bin/sh
: > "$FIXTURE_SLEEP_MARKER"
exit 1
"#;

// The generated contract wraps every GitHub API request in a per-request
// timeout. Simulate a hung API without sleeping in the test process.
const TIMEOUT_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, signal, subprocess, sys
from pathlib import Path
args = sys.argv[1:]
while args and args[0].startswith("--"):
    args.pop(0)
if args:
    args.pop(0)  # duration
if os.environ.get("FIXTURE_HANG_API") == "1":
    marker = Path("timeout-child.pid")
    child = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        start_new_session=True,
    )
    marker.write_text(str(child.pid))
    try:
        child.wait(timeout=0.05)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid, signal.SIGTERM)
        child.wait(timeout=1)
    marker.unlink(missing_ok=True)
    raise SystemExit(124)
raise SystemExit(subprocess.run(args).returncode)
"#;
