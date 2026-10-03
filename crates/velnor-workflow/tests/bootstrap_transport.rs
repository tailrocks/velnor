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
const SAME_REPOSITORY_RUNS_JQ: &str = "{total_count: .total_count, page_count: (.workflow_runs | length), workflow_runs: [.workflow_runs[] | select(.head_repository.id == .repository.id)]}";
const ARTIFACT_ID: u64 = 12_345;
const RUN_ATTEMPT: u64 = 2;
const PRODUCER_REST_JOB_ID: u64 = 9_100;
const PRODUCER_REST_JOB_DISPLAY_NAME: &str =
    "Rust · velnor-workflow · github-hosted — rust-velnor-workflow / GitHub · hosted";
const PUBLISHER_JOB: &str = "verify-github-hosted";
const PUBLISH_START: &str = "2026-10-01T00:00:10Z";
const PUBLISH_END: &str = "2026-10-01T00:00:20Z";
// The PR unit workflow may compile its matching merge tree.
const CANDIDATE_BUILD_SHA: &str = "5555555555555555555555555555555555555555";
// A later default-branch push resolves back to the PR head above.
const MERGE_SHA: &str = "4444444444444444444444444444444444444444";
const CLOSURE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE_PIN: &str = "3333333333333333333333333333333333333333";
const RUN_ID: u64 = 900;
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
    WrongPlatform,
    WrongRepository,
    WrongRun,
    WrongManifestAttempt,
    WrongManifestPublisherJob,
    WrongManifestArtifactName,
    WrongRevision,
    WrongClosure,
    MalformedBuildRevision,
    WrongBinaryDigest,
    WrongArtifactName,
    ExpiredArtifact,
    ArtifactFromDifferentRun,
    MissingArtifact,
    PendingRunWithoutArtifact,
    NewerEligibleRunInProgress,
    EmptyJobLog,
    MissingJobLogMarker,
    MalformedJobLogMarker,
    DuplicateJobLogMarker,
    WrongJobLogRun,
    WrongJobLogAttempt,
    WrongJobLogPublisher,
    WrongJobLogArtifactName,
    WrongJobLogArtifactId,
    JobLogTooLarge,
    OldAttemptArtifactOnly,
    WrongProducerRestName,
    WrongProducerRestNameWithoutCheckRunId,
    DuplicatePublishers,
    RunsOverflow,
    RunsResponseTooLarge,
    ArtifactsOverflow,
    RunsAtPageCap,
    ArtifactsAtPageCap,
    ArtifactTooLarge,
    MissingServiceDigest,
    WrongServiceDigest,
    ModifiedArtifactMetadata,
    DuplicateArtifact,
    ApiNotRedirect,
    SignedUrlHttpFailure,
    RedirectWithCredentials,
    RedirectDowngrade,
    RedirectInvalidHost,
    RedirectNumericAddress,
    RedirectNonstandardPort,
    SignedUrlRedirect,
    PartialDownload,
    ZipTraversal,
    ZipDuplicate,
    ZipSymlink,
    ZipExtra,
    ZipCentralDirectoryTooLarge,
    ArchiveStreamTooLarge,
    ExtractedSizeTooLarge,
    ForkHead,
    UnsupportedEvent,
    PushNoAssociatedPullRequest,
    PushAmbiguousPullRequests,
    PushWrongMergeCommit,
    PushUnmergedPullRequest,
    PushWrongBaseBranch,
    PushWrongBaseRepository,
    PushForkHeadRepository,
    PushMalformedResponse,
    PushPullRequestPageOverflow,
}

struct TransportFixture {
    root: PathBuf,
    generated: Generated,
    bin_dir: PathBuf,
    archive: PathBuf,
    scenario: PathBuf,
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
        write_generator_fixture_tree(&root);

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

        let policy = read_workflow(&output, "ci-policy.yml");
        let producer_workflow = read_workflow(&output, "ci-unit-rust.yml");
        let producer = step_run(&producer_workflow, "Prepare candidate generator product")
            .expect("producer run step");
        let acquire =
            step_run(&policy, "Acquire candidate generator product").expect("acquire run step");
        let publish_name = step_with(
            &producer_workflow,
            "Publish candidate generator product",
            "name",
        )
        .expect("publish artifact name");
        let publish_path = step_with(
            &producer_workflow,
            "Publish candidate generator product",
            "path",
        )
        .expect("publish artifact path");
        // Candidate bootstrap names are closure- and runner-qualified by the
        // producer. Keep the fixture independent of a rendered expression.
        let bin_dir = root.join("fake-bin");
        fs::create_dir_all(&bin_dir).expect("fake bin");
        write_fake_commands(&bin_dir);
        let archive = root.join("candidate.zip");
        let scenario = root.join("scenario.json");
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
                producer_workflow,
            },
            bin_dir,
            archive,
            scenario,
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

    fn candidate_artifact_name(&self, runner_os: &str, runner_arch: &str) -> String {
        let template = self
            .generated
            .producer
            .lines()
            .find_map(|line| {
                let assignment = line.trim().strip_prefix("artifact_name=")?;
                assignment.strip_prefix('"')?.strip_suffix('"')
            })
            .expect("generated producer artifact name assignment");
        assert_eq!(
            template,
            "velnor-workflow-candidate-${head_closure:0:16}-${RUNNER_OS}-${RUNNER_ARCH}-r${GITHUB_RUN_ID}-a${GITHUB_RUN_ATTEMPT}-j${GITHUB_JOB}",
            "producer artifact name must bind closure, platform, run, attempt, and producer job"
        );
        template
            .replace("${head_closure:0:16}", &CLOSURE[..16])
            .replace("${RUNNER_OS}", runner_os)
            .replace("${RUNNER_ARCH}", runner_arch)
            .replace("${GITHUB_RUN_ID}", &RUN_ID.to_string())
            .replace("${GITHUB_RUN_ATTEMPT}", &RUN_ATTEMPT.to_string())
            .replace("${GITHUB_JOB}", PUBLISHER_JOB)
    }

    fn valid_artifact_on_platform(&self, runner_os: &str, runner_arch: &str) {
        let platform = format!("{runner_os}-{runner_arch}");
        let artifact_name = self.candidate_artifact_name(runner_os, runner_arch);
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
            "platform": platform,
            "repository": REPOSITORY,
            "run_id": RUN_ID.to_string(),
            "run_attempt": RUN_ATTEMPT,
            "publisher_job": PUBLISHER_JOB,
            "artifact_name": artifact_name,
            "revision": HEAD_SHA,
            "closure": CLOSURE,
            // A matching merge tree may supply the binary while revision
            // continues to identify the PR head.
            "build_revision": CANDIDATE_BUILD_SHA,
            "binary_sha256": file_sha256(&self.binary),
        });
        fs::write(
            &self.manifest,
            serde_json::to_vec(&manifest_value).expect("manifest JSON"),
        )
        .expect("manifest");
        zip_files(&self.archive, &self.binary, &self.manifest);
        let archive_bytes = fs::read(&self.archive).expect("candidate archive bytes");
        let scenario = valid_scenario(
            &artifact_name,
            archive_bytes.len() as u64,
            &hex_sha256(&archive_bytes),
        );
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
        self.run_acquire_at_server_url(failure, runner_os, runner_arch, "https://github.com")
    }

    fn run_acquire_at_server_url(
        &self,
        failure: Option<FailureCase>,
        runner_os: &str,
        runner_arch: &str,
        server_url: &str,
    ) -> Output {
        let event_name = if failure == Some(FailureCase::UnsupportedEvent) {
            "workflow_dispatch"
        } else {
            "pull_request_target"
        };
        self.run_acquire_with_event(failure, runner_os, runner_arch, server_url, event_name)
    }

    fn run_acquire_push(&self, failure: Option<FailureCase>) -> Output {
        self.run_acquire_with_event(failure, "Linux", "X64", "https://github.com", "push")
    }

    #[expect(
        clippy::too_many_lines,
        reason = "fixture scenario mutation and subprocess setup stay together for each case"
    )]
    fn run_acquire_with_event(
        &self,
        failure: Option<FailureCase>,
        runner_os: &str,
        runner_arch: &str,
        server_url: &str,
        event_name: &str,
    ) -> Output {
        let mut scenario: JsonValue =
            serde_json::from_slice(&fs::read(&self.scenario).expect("scenario bytes"))
                .expect("scenario JSON");
        match failure {
            Some(FailureCase::WrongPlatform) => self.mutate_manifest(|manifest| {
                manifest["platform"] = json!("Linux-ARM64");
            }),
            Some(FailureCase::WrongRepository) => self.mutate_manifest(|manifest| {
                manifest["repository"] = json!("someone-else/project");
            }),
            Some(FailureCase::WrongRun) => self.mutate_manifest(|manifest| {
                manifest["run_id"] = json!((RUN_ID + 1).to_string());
            }),
            Some(FailureCase::WrongManifestAttempt) => self.mutate_manifest(|manifest| {
                manifest["run_attempt"] = json!(RUN_ATTEMPT.to_string());
            }),
            Some(FailureCase::WrongManifestPublisherJob) => self.mutate_manifest(|manifest| {
                manifest["publisher_job"] = json!(PRODUCER_REST_JOB_ID);
            }),
            Some(FailureCase::WrongManifestArtifactName) => {
                self.mutate_manifest(|manifest| manifest["artifact_name"] = json!("wrong-name"));
            }
            Some(FailureCase::WrongRevision) => self.mutate_manifest(|manifest| {
                manifest["revision"] = json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            }),
            Some(FailureCase::WrongClosure) => self.mutate_manifest(|manifest| {
                manifest["closure"] =
                    json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            }),
            Some(FailureCase::MalformedBuildRevision) => {
                self.mutate_manifest(|manifest| manifest["build_revision"] = json!("invalid"));
            }
            Some(FailureCase::WrongBinaryDigest) => self.mutate_manifest(|manifest| {
                manifest["binary_sha256"] = json!("c".repeat(64));
            }),
            Some(FailureCase::WrongArtifactName) => {
                scenario["artifacts"][0]["name"] = json!("wrong-artifact-name");
            }
            Some(FailureCase::ExpiredArtifact) => {
                scenario["artifacts"][0]["expired"] = json!(true);
            }
            Some(FailureCase::ArtifactFromDifferentRun) => {
                scenario["artifacts"][0]["workflow_run"]["id"] = json!(RUN_ID + 1);
            }
            Some(FailureCase::MissingArtifact) => scenario["artifacts"] = json!([]),
            Some(FailureCase::PendingRunWithoutArtifact) => {
                let run_index = scenario["runs"]
                    .as_array()
                    .and_then(|runs| {
                        runs.iter()
                            .position(|run| run["id"].as_u64() == Some(RUN_ID))
                    })
                    .expect("same-repository workflow run");
                scenario["runs"][run_index]["status"] = json!("in_progress");
                scenario["artifacts"] = json!([]);
            }
            Some(FailureCase::NewerEligibleRunInProgress) => {
                scenario["runs"][0]["repository"]["id"] = json!(42);
                scenario["runs"][0]["repository"]["full_name"] = json!(REPOSITORY);
                scenario["runs"][0]["head_repository"]["id"] = json!(42);
                scenario["runs"][0]["head_repository"]["full_name"] = json!(REPOSITORY);
                scenario["runs"][0]["status"] = json!("in_progress");
                scenario["runs"][0]["run_attempt"] = json!(1);
                scenario["runs"][0]["created_at"] = json!("2026-10-02T00:00:00Z");
                scenario["runs"][1]["created_at"] = json!("2026-10-01T00:00:00Z");
                let older_artifact = scenario["artifacts"][0].clone();
                scenario["artifacts"] = json!([older_artifact]);
                scenario["jobs"]
                    .as_array_mut()
                    .expect("jobs array")
                    .push(json!({
                            "id": 9101,
                            "run_id": RUN_ID + 1,
                            "run_attempt": 1,
                            "name": PRODUCER_REST_JOB_DISPLAY_NAME,
                            "status": "in_progress",
                            "conclusion": JsonValue::Null,
                            "started_at": "2026-10-02T00:00:00Z",
                            "completed_at": JsonValue::Null,
                            "steps": [
                                {
                                    "name": "Prepare candidate generator product",
                                    "status": "completed",
                                    "conclusion": "success",
                                    "started_at": "2026-10-02T00:00:02Z",
                                    "completed_at": "2026-10-02T00:00:05Z"
                                },
                                {
                                    "name": "Publish candidate generator product",
                                    "status": "in_progress",
                                    "conclusion": JsonValue::Null,
                                    "started_at": "2026-10-02T00:00:10Z",
                                    "completed_at": JsonValue::Null
                                }
                            ]
                    }));
            }
            Some(FailureCase::EmptyJobLog) => {
                scenario["job_log_contents"] = json!("");
            }
            Some(FailureCase::MissingJobLogMarker) => {
                scenario["job_log_contents"] =
                    json!("ordinary producer output without an artifact marker\n");
            }
            Some(FailureCase::MalformedJobLogMarker) => {
                scenario["job_log_contents"] = json!("VELNOR_CANDIDATE_ARTIFACT invalid");
            }
            Some(FailureCase::DuplicateJobLogMarker) => {
                let marker = candidate_log_marker(
                    RUN_ID,
                    RUN_ATTEMPT,
                    PUBLISHER_JOB,
                    ARTIFACT_ID,
                    &self.candidate_artifact_name(runner_os, runner_arch),
                );
                scenario["job_log_contents"] = json!(format!("{marker}{marker}"));
            }
            Some(FailureCase::WrongJobLogRun) => {
                scenario["job_log_contents"] = json!(candidate_log_marker(
                    RUN_ID + 1,
                    RUN_ATTEMPT,
                    PUBLISHER_JOB,
                    ARTIFACT_ID,
                    &self.candidate_artifact_name(runner_os, runner_arch),
                ));
            }
            Some(FailureCase::WrongJobLogAttempt) => {
                scenario["job_log_contents"] = json!(candidate_log_marker(
                    RUN_ID,
                    RUN_ATTEMPT + 1,
                    PUBLISHER_JOB,
                    ARTIFACT_ID,
                    &self.candidate_artifact_name(runner_os, runner_arch),
                ));
            }
            Some(FailureCase::WrongJobLogPublisher) => {
                let artifact_name = self
                    .candidate_artifact_name(runner_os, runner_arch)
                    .replace("-jverify-github-hosted", "-jother-job");
                scenario["job_log_contents"] = json!(candidate_log_marker(
                    RUN_ID,
                    RUN_ATTEMPT,
                    "other-job",
                    ARTIFACT_ID,
                    &artifact_name,
                ));
            }
            Some(FailureCase::WrongJobLogArtifactName) => {
                scenario["job_log_contents"] = json!(candidate_log_marker(
                    RUN_ID,
                    RUN_ATTEMPT,
                    PUBLISHER_JOB,
                    ARTIFACT_ID,
                    "wrong-name",
                ));
            }
            Some(FailureCase::WrongJobLogArtifactId) => {
                scenario["job_log_contents"] = json!(candidate_log_marker(
                    RUN_ID,
                    RUN_ATTEMPT,
                    PUBLISHER_JOB,
                    ARTIFACT_ID + 1,
                    &self.candidate_artifact_name(runner_os, runner_arch),
                ));
            }
            Some(FailureCase::JobLogTooLarge) => {
                scenario["job_log_oversized_bytes"] = json!(16_777_217);
            }
            Some(FailureCase::OldAttemptArtifactOnly) => {
                scenario["artifacts"][0]["created_at"] = json!("2026-09-30T23:59:59Z");
            }
            Some(FailureCase::WrongProducerRestName) => {
                scenario["jobs"][0]["name"] = json!("untrusted producer display name");
            }
            Some(FailureCase::WrongProducerRestNameWithoutCheckRunId) => {
                scenario["jobs"][0]["name"] = json!("untrusted producer display name");
                scenario["jobs"][0]
                    .as_object_mut()
                    .expect("producer job object")
                    .remove("check_run_id");
            }
            Some(FailureCase::DuplicatePublishers) => {
                let mut duplicate = scenario["jobs"][0].clone();
                duplicate["id"] = json!(duplicate["id"].as_u64().expect("producer job id") + 1);
                scenario["jobs"]
                    .as_array_mut()
                    .expect("producer jobs array")
                    .push(duplicate);
            }
            Some(FailureCase::RunsOverflow) => scenario["runs_total_count"] = json!(1001),
            Some(FailureCase::RunsResponseTooLarge) => {
                scenario["runs_response_too_large"] = json!(true);
            }
            Some(FailureCase::ArtifactsOverflow) => {
                scenario["artifacts_total_count"] = json!(1001);
            }
            Some(FailureCase::RunsAtPageCap) => scenario["runs_total_count"] = json!(1000),
            Some(FailureCase::ArtifactsAtPageCap) => {
                scenario["artifacts_total_count"] = json!(1000);
            }
            Some(FailureCase::ArtifactTooLarge) => {
                scenario["artifacts"][0]["size_in_bytes"] = json!(268_435_457);
            }
            Some(FailureCase::MissingServiceDigest) => {
                scenario["artifacts"][0]["digest"] = JsonValue::Null;
            }
            Some(FailureCase::WrongServiceDigest) => {
                scenario["artifacts"][0]["digest"] = json!(format!("sha256:{}", "0".repeat(64)));
            }
            Some(FailureCase::ModifiedArtifactMetadata) => {
                scenario["mutate_exact_artifact_digest"] = json!(true);
            }
            Some(FailureCase::DuplicateArtifact) => {
                let duplicate = scenario["artifacts"][0].clone();
                scenario["artifacts"]
                    .as_array_mut()
                    .expect("artifact array")
                    .push(duplicate);
            }
            Some(FailureCase::ApiNotRedirect) => scenario["api_http_status"] = json!(200),
            Some(FailureCase::SignedUrlHttpFailure) => scenario["signed_http_status"] = json!(403),
            Some(FailureCase::RedirectWithCredentials) => {
                scenario["redirect_location"] =
                    json!("https://user:secret@objects.githubusercontent.com/artifacts/signed.zip");
            }
            Some(FailureCase::RedirectDowngrade) => {
                scenario["redirect_location"] =
                    json!("http://objects.githubusercontent.com/artifacts/signed.zip");
            }
            Some(FailureCase::RedirectInvalidHost) => {
                scenario["redirect_location"] = json!("https://.invalid/artifacts/signed.zip");
            }
            Some(FailureCase::RedirectNumericAddress) => {
                scenario["redirect_location"] = json!("https://0x7f.1/artifacts/signed.zip");
            }
            Some(FailureCase::RedirectNonstandardPort) => {
                scenario["redirect_location"] =
                    json!("https://objects.githubusercontent.com:8443/artifacts/signed.zip");
            }
            Some(FailureCase::SignedUrlRedirect) => scenario["signed_http_status"] = json!(302),
            Some(FailureCase::PartialDownload) => scenario["partial_download"] = json!(true),
            Some(FailureCase::ArchiveStreamTooLarge) => {
                scenario["download_padding_bytes"] = json!(16_384);
            }
            Some(FailureCase::PushNoAssociatedPullRequest) => {
                scenario["pull_requests"] = json!([]);
            }
            Some(FailureCase::PushAmbiguousPullRequests) => {
                let pull_request = scenario["pull_requests"][0].clone();
                scenario["pull_requests"] = json!([pull_request.clone(), pull_request]);
            }
            Some(FailureCase::PushWrongMergeCommit) => {
                scenario["pull_requests"][0]["merge_commit_sha"] = json!(HEAD_SHA);
            }
            Some(FailureCase::PushUnmergedPullRequest) => {
                scenario["pull_requests"][0]["merged_at"] = JsonValue::Null;
            }
            Some(FailureCase::PushWrongBaseBranch) => {
                scenario["pull_requests"][0]["base"]["ref"] = json!("feature");
            }
            Some(FailureCase::PushWrongBaseRepository) => {
                scenario["pull_requests"][0]["base"]["repo"]["full_name"] =
                    json!("attacker/velnor");
            }
            Some(FailureCase::PushForkHeadRepository) => {
                scenario["pull_requests"][0]["head"]["repo"]["full_name"] = json!("attacker/fork");
            }
            Some(FailureCase::PushMalformedResponse) => {
                scenario["malformed_push_response"] = json!(true);
            }
            Some(FailureCase::PushPullRequestPageOverflow) => {
                let pull_request = scenario["pull_requests"][0].clone();
                scenario["pull_requests"] = json!(vec![pull_request; 100]);
            }
            Some(FailureCase::ZipTraversal)
            | Some(FailureCase::ZipDuplicate)
            | Some(FailureCase::ZipSymlink)
            | Some(FailureCase::ZipExtra)
            | Some(FailureCase::ZipCentralDirectoryTooLarge)
            | Some(FailureCase::ExtractedSizeTooLarge)
            | Some(FailureCase::ForkHead)
            | Some(FailureCase::UnsupportedEvent)
            | None => {}
        }
        // Repack the archive after a manifest mutation. The consumer verifies
        // the manifest against the downloaded bytes, so a fixture that merely
        // changes JSON outside the transported archive would be meaningless.
        let manifest_mutation = failure.is_some_and(|case| {
            matches!(
                case,
                FailureCase::WrongPlatform
                    | FailureCase::WrongRepository
                    | FailureCase::WrongRun
                    | FailureCase::WrongManifestAttempt
                    | FailureCase::WrongManifestPublisherJob
                    | FailureCase::WrongManifestArtifactName
                    | FailureCase::WrongRevision
                    | FailureCase::WrongClosure
                    | FailureCase::MalformedBuildRevision
                    | FailureCase::WrongBinaryDigest
            )
        });
        if manifest_mutation {
            zip_files(&self.archive, &self.binary, &self.manifest);
        }
        let zip_mutation = match failure {
            Some(FailureCase::ZipTraversal) => Some("traversal"),
            Some(FailureCase::ZipDuplicate) => Some("duplicate"),
            Some(FailureCase::ZipSymlink) => Some("symlink"),
            Some(FailureCase::ZipExtra) => Some("extra"),
            Some(FailureCase::ZipCentralDirectoryTooLarge) => Some("central-directory"),
            _ => None,
        };
        if let Some(mode) = zip_mutation {
            zip_files_with_malformation(&self.archive, &self.binary, &self.manifest, mode);
        }
        if manifest_mutation || zip_mutation.is_some() {
            let archive_bytes = fs::read(&self.archive).expect("mutated candidate archive");
            scenario["artifacts"][0]["size_in_bytes"] = json!(archive_bytes.len() as u64);
            scenario["artifacts"][0]["digest"] =
                json!(format!("sha256:{}", hex_sha256(&archive_bytes)));
        }
        let expected_github_host = server_url
            .strip_prefix("https://")
            .unwrap_or_default()
            .split('/')
            .next()
            .unwrap_or_default();
        if expected_github_host != "github.com" {
            if let Some(jobs) = scenario["jobs"].as_array_mut() {
                for job in jobs {
                    if let Some(job) = job.as_object_mut() {
                        job.remove("check_run_id");
                    }
                }
            }
        }
        let api_base = api_base_for_host(expected_github_host);
        if let Some(artifacts) = scenario["artifacts"].as_array_mut() {
            for artifact in artifacts {
                if let Some(id) = artifact["id"].as_u64() {
                    artifact["archive_download_url"] = json!(format!(
                        "{api_base}/repos/{REPOSITORY}/actions/artifacts/{id}/zip"
                    ));
                }
            }
        }
        scenario["job_log_api_url"] = json!(format!(
            "{api_base}/repos/{REPOSITORY}/actions/jobs/{PRODUCER_REST_JOB_ID}/logs"
        ));
        fs::write(
            &self.scenario,
            serde_json::to_vec(&scenario).expect("scenario JSON"),
        )
        .expect("scenario");
        let _ = fs::remove_file(&self.sleep_marker);
        let _ = fs::remove_dir_all(&self.run_temp);
        fs::create_dir_all(&self.run_temp).expect("run temp");
        let mut command = Command::new("bash");
        let mut acquire_script =
            materialize_github_expressions(&self.generated.acquire, runner_os, runner_arch);
        if failure == Some(FailureCase::ArchiveStreamTooLarge) {
            assert!(
                fs::metadata(&self.archive).expect("archive metadata").len() < 8192,
                "stream-cap fixture archive must be below its advertised size limit"
            );
            acquire_script = acquire_script
                .replace("MAX_ARCHIVE_BYTES = 268435456", "MAX_ARCHIVE_BYTES = 8192")
                .replace("MAX_ARCHIVE_BYTES=268435456", "MAX_ARCHIVE_BYTES=8192");
        }
        if failure == Some(FailureCase::ExtractedSizeTooLarge) {
            acquire_script =
                acquire_script.replace("MAX_EXTRACTED_BYTES=268435456", "MAX_EXTRACTED_BYTES=64");
        }
        let expected_enterprise_token =
            if expected_github_host == "github.com" || expected_github_host.ends_with(".ghe.com") {
                ""
            } else {
                "fixture-token"
            };
        command
            .args(["-euo", "pipefail", "-c", &acquire_script])
            .current_dir(&self.root)
            .env("PATH", prepend_path(&self.bin_dir))
            .env("FIXTURE_SCENARIO", &self.scenario)
            .env("FIXTURE_ARCHIVE", &self.archive)
            .env("FIXTURE_CLOSURE", CLOSURE)
            .env("FIXTURE_MERGE_CLOSURE", CLOSURE)
            .env("FIXTURE_EXECUTION_SENTINEL", &self.execution_sentinel)
            .env("RUNNER_TEMP", &self.run_temp)
            .env("GITHUB_REPOSITORY", REPOSITORY)
            .env("GITHUB_SERVER_URL", server_url)
            .env("GITHUB_API_URL", api_base_for_host(expected_github_host))
            .env("RUNNER_OS", runner_os)
            .env("RUNNER_ARCH", runner_arch)
            .env("GH_TOKEN", "fixture-token")
            .env("GH_HOST", "attacker.invalid")
            .env("GH_CONFIG_DIR", self.root.join("attacker-gh-config"))
            .env("GH_ENTERPRISE_TOKEN", "attacker-enterprise-token")
            .env("GITHUB_ENTERPRISE_TOKEN", "attacker-enterprise-token")
            .env("GH_REPO", "attacker/project")
            .env("FIXTURE_EXPECT_GH_HOST", expected_github_host)
            .env("FIXTURE_EXPECT_RUNS_JQ", SAME_REPOSITORY_RUNS_JQ)
            .env("FIXTURE_EXPECT_ENTERPRISE_TOKEN", expected_enterprise_token)
            .env("EVENT_NAME", event_name)
            .env("MERGE_SHA", MERGE_SHA)
            .env(
                "PR_HEAD_SHA",
                if event_name == "push" {
                    BASE_PIN
                } else {
                    HEAD_SHA
                },
            )
            .env("BASE_SHA", BASE_PIN)
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

    fn mutate_scenario(&self, mutate: impl FnOnce(&mut JsonValue)) {
        let mut scenario: JsonValue =
            serde_json::from_slice(&fs::read(&self.scenario).expect("scenario bytes"))
                .expect("scenario JSON");
        mutate(&mut scenario);
        fs::write(
            &self.scenario,
            serde_json::to_vec(&scenario).expect("scenario JSON bytes"),
        )
        .expect("mutated scenario");
    }

    fn run_producer_build(&self) -> Output {
        self.run_producer_build_on_platform("Linux", "X64")
    }

    fn run_producer_build_on_platform(&self, runner_os: &str, runner_arch: &str) -> Output {
        self.run_producer_build_on_platform_with_base_closure_match(runner_os, runner_arch, false)
    }

    fn run_producer_build_on_platform_with_base_closure_match(
        &self,
        runner_os: &str,
        runner_arch: &str,
        base_closure_matches: bool,
    ) -> Output {
        let _ = fs::remove_dir_all(&self.run_temp);
        fs::create_dir_all(&self.run_temp).expect("run temp");
        let script =
            materialize_producer_expressions(&self.generated.producer, runner_os, runner_arch);
        Command::new("bash")
            .args(["-euo", "pipefail", "-c", &script])
            .current_dir(&self.root)
            .env("PATH", prepend_path(&self.bin_dir))
            .env("RUNNER_TEMP", &self.run_temp)
            .env("GITHUB_REPOSITORY", REPOSITORY)
            .env("RUNNER_OS", runner_os)
            .env("RUNNER_ARCH", runner_arch)
            .env("GITHUB_RUN_ID", RUN_ID.to_string())
            .env("GITHUB_RUN_ATTEMPT", RUN_ATTEMPT.to_string())
            .env("GITHUB_JOB", PUBLISHER_JOB)
            .env("CANDIDATE_MERGE_SHA", CANDIDATE_BUILD_SHA)
            .env("CANDIDATE_PR_HEAD_SHA", HEAD_SHA)
            .env("CANDIDATE_BASE_SHA", BASE_PIN)
            .env("HEAD_SHA", HEAD_SHA)
            .env("BASE_SHA", BASE_PIN)
            .env("BASE_PIN", BASE_PIN)
            .env("FIXTURE_CLOSURE", CLOSURE)
            .env(
                "FIXTURE_BASE_CLOSURE_MATCHES",
                if base_closure_matches { "1" } else { "" },
            )
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
    assert_ne!(
        PRODUCER_REST_JOB_DISPLAY_NAME, PUBLISHER_JOB,
        "fixture must exercise REST display-name to GITHUB_JOB-key mapping"
    );
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
    assert_eq!(manifest["run_attempt"], json!(RUN_ATTEMPT));
    assert_eq!(manifest["publisher_job"], json!(PUBLISHER_JOB));
    assert_eq!(
        manifest["artifact_name"],
        json!(fixture.candidate_artifact_name("Linux", "X64"))
    );
    assert_eq!(manifest["revision"], json!(HEAD_SHA));
    assert_eq!(manifest["closure"], json!(CLOSURE));
    let build_revision = manifest["build_revision"]
        .as_str()
        .expect("downloaded build revision");
    assert!(
        is_git_sha1(build_revision),
        "acquire requires a syntactically valid build revision: {build_revision}"
    );
    assert_eq!(
        file_sha256(Path::new(&binary_path)),
        manifest["binary_sha256"]
    );
    let gh_commands = logged_gh_commands(&fixture);
    let runs_query = gh_commands
        .iter()
        .find(|args| {
            args.first().is_some_and(|arg| arg == "api")
                && args
                    .iter()
                    .any(|arg| arg.contains("/actions/workflows/ci-pr.yml/runs?"))
        })
        .expect("candidate workflow runs query");
    let jq_filter = runs_query
        .windows(2)
        .find(|pair| pair[0] == "--jq")
        .map(|pair| pair[1].as_str())
        .expect("same-repository page filter passed to gh api");
    assert_eq!(
        jq_filter, SAME_REPOSITORY_RUNS_JQ,
        "candidate run query must filter fork runs: {runs_query:?}"
    );
    assert!(
        runs_query
            .iter()
            .any(|arg| arg.contains("sort=created&direction=desc")),
        "candidate runs must be queried in created-desc order: {runs_query:?}"
    );
    assert!(
        fixture.generated.acquire.contains("timeout=30"),
        "each GitHub API subprocess needs a bounded timeout"
    );
    assert!(
        gh_commands.iter().any(|args| args
            .iter()
            .any(|arg| { arg.contains("/actions/runs/900/attempts/2/jobs?") })),
        "acquire must enumerate jobs for the exact current attempt: {gh_commands:?}"
    );
    assert!(
        gh_commands.iter().any(|args| args
            .iter()
            .any(|arg| { arg.contains("/actions/runs/900/artifacts?") })),
        "acquire must paginate artifacts in the selected run: {gh_commands:?}"
    );
    assert!(
        gh_commands.iter().any(|args| args
            .iter()
            .any(|arg| { arg == &format!("repos/{REPOSITORY}/actions/artifacts/{ARTIFACT_ID}") })),
        "acquire must re-fetch the selected artifact by exact ID: {gh_commands:?}"
    );
    assert!(
        gh_commands
            .iter()
            .all(|args| args.first().is_some_and(|arg| arg == "api")),
        "artifact transport must use bounded REST calls, never gh run download: {gh_commands:?}"
    );

    let requests = logged_curl_requests(&fixture);
    assert_eq!(
        requests.len(),
        4,
        "transport has job-log API/signed requests and artifact API/signed requests: {requests:?}"
    );
    assert_eq!(requests[0]["stage"], json!("job_logs_api"));
    assert_eq!(requests[0]["authenticated"], json!(true));
    assert_eq!(
        requests[0]["url"],
        json!(format!(
            "https://api.github.com/repos/{REPOSITORY}/actions/jobs/{PRODUCER_REST_JOB_ID}/logs"
        ))
    );
    assert_eq!(requests[1]["stage"], json!("job_logs_signed"));
    assert_eq!(requests[1]["authenticated"], json!(false));
    assert_eq!(
        requests[1]["url"],
        json!("https://objects.githubusercontent.com/artifacts/job-logs.txt")
    );
    assert_eq!(requests[2]["stage"], json!("api"));
    assert_eq!(requests[2]["authenticated"], json!(true));
    assert_eq!(
        requests[2]["url"],
        json!(format!(
            "https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{ARTIFACT_ID}/zip"
        ))
    );
    assert_eq!(requests[3]["stage"], json!("signed"));
    assert_eq!(requests[3]["authenticated"], json!(false));
    assert_eq!(
        requests[3]["url"],
        json!("https://objects.githubusercontent.com/artifacts/signed.zip")
    );
    let signed_log_args = requests[1]["args"]
        .as_array()
        .expect("signed job log curl argv");
    assert!(
        !signed_log_args.iter().any(|arg| {
            arg.as_str().is_some_and(|value| {
                value == "--config"
                    || value.contains("Authorization")
                    || value == "fixture-token"
                    || value == "-L"
                    || value == "--location"
            })
        }),
        "signed job-log URL request must not carry the API bearer token or follow redirects: {signed_log_args:?}"
    );
    assert!(signed_log_args
        .windows(2)
        .any(|pair| pair == [json!("--max-filesize"), json!("16777216")]));
    let signed_args = requests[3]["args"]
        .as_array()
        .expect("signed artifact curl argv");
    assert!(
        !signed_args.iter().any(|arg| {
            arg.as_str().is_some_and(|value| {
                value == "--config"
                    || value.contains("Authorization")
                    || value == "fixture-token"
                    || value == "-L"
                    || value == "--location"
            })
        }),
        "signed artifact URL request must not carry the API bearer token or follow redirects: {signed_args:?}"
    );
    assert!(signed_args
        .windows(2)
        .any(|pair| pair == [json!("--max-time"), json!("120")]));
    assert!(signed_args
        .windows(2)
        .any(|pair| pair == [json!("--max-filesize"), json!("268435456")]));
}

#[test]
fn generated_acquire_paginates_run_and_artifact_lists() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
    fixture.mutate_scenario(|scenario| {
        let target_run = scenario["runs"][1].clone();
        let fork_decoy = scenario["runs"][0].clone();
        let mut runs = Vec::with_capacity(101);
        for index in 0..100 {
            let mut decoy = fork_decoy.clone();
            decoy["id"] = json!(1000 + index);
            runs.push(decoy);
        }
        runs.push(target_run);
        scenario["runs"] = json!(runs);

        let target_artifact = scenario["artifacts"][0].clone();
        let mut artifacts = Vec::with_capacity(101);
        for index in 0..100 {
            let mut decoy = target_artifact.clone();
            decoy["id"] = json!(20_000 + index);
            decoy["name"] = json!(format!("unrelated-{index}"));
            artifacts.push(decoy);
        }
        artifacts.push(target_artifact);
        scenario["artifacts"] = json!(artifacts);
    });

    let output = fixture.run_acquire(None);
    assert!(
        output.status.success(),
        "second-page candidate failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = logged_gh_commands(&fixture);
    assert!(
        commands.iter().any(|args| args.iter().any(|arg| {
            arg.contains("/actions/workflows/ci-pr.yml/runs?") && arg.ends_with("page=2")
        })),
        "same-repository target is on the second run page: {commands:?}"
    );
    assert!(
        commands.iter().any(|args| args.iter().any(|arg| {
            arg.contains("/actions/runs/900/artifacts?") && arg.ends_with("page=2")
        })),
        "candidate artifact is on the second artifact page: {commands:?}"
    );
}

#[test]
fn generated_acquire_uses_only_the_current_attempt_publish_interval() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
    let previous_attempt_name = fixture
        .candidate_artifact_name("Linux", "X64")
        .replace("-a2-", "-a1-");
    fixture.mutate_scenario(|scenario| {
        let mut previous_attempt_artifact = scenario["artifacts"][0].clone();
        previous_attempt_artifact["id"] = json!(ARTIFACT_ID - 1);
        previous_attempt_artifact["name"] = json!(previous_attempt_name);
        previous_attempt_artifact["created_at"] = json!("2026-09-30T23:59:59Z");
        scenario["artifacts"]
            .as_array_mut()
            .expect("artifact array")
            .insert(0, previous_attempt_artifact);
    });
    let output = fixture.run_acquire(None);
    assert!(
        output.status.success(),
        "current-attempt artifact should survive a prior-attempt decoy:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = logged_curl_requests(&fixture);
    assert_eq!(requests[0]["stage"], json!("job_logs_api"));
    assert_eq!(
        requests[2]["url"],
        json!(format!(
            "https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{ARTIFACT_ID}/zip"
        ))
    );
    let commands = logged_gh_commands(&fixture);
    assert!(commands.iter().any(|args| args
        .iter()
        .any(|arg| { arg.contains("/actions/runs/900/attempts/2/jobs?") })));
}

#[test]
fn generated_acquire_waits_for_newer_eligible_run_instead_of_using_older_candidate() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
    let output = fixture.run_acquire(Some(FailureCase::NewerEligibleRunInProgress));
    assert!(
        !output.status.success(),
        "newer in-progress run must keep acquisition waiting:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fixture.sleep_marker.is_file(),
        "pending newer run must be polled"
    );
    let commands = logged_gh_commands(&fixture);
    assert!(
        commands.iter().any(|args| args.iter().any(|arg| {
            arg.contains("/actions/workflows/ci-pr.yml/runs?")
                && arg.contains("sort=created&direction=desc")
        })),
        "fixture must query newest same-head runs first: {commands:?}"
    );
    assert!(
        commands.iter().all(|args| !args
            .iter()
            .any(|arg| { arg == &format!("repos/{REPOSITORY}/actions/artifacts/{ARTIFACT_ID}") })),
        "acquire must not select the older completed artifact: {commands:?}"
    );
    assert!(
        logged_curl_requests(&fixture).is_empty(),
        "newer pending run must prevent all downloads"
    );
    let github_env = fs::read_to_string(fixture.run_temp.join("github.env")).unwrap_or_default();
    assert!(
        !github_env.contains("VELNOR_WORKFLOW_CANDIDATE_BINARY=")
            && !github_env.contains("VELNOR_WORKFLOW_CANDIDATE_MANIFEST="),
        "pending newer run must not export older candidate paths: {github_env}"
    );
    assert_no_candidate_scratch(&fixture, FailureCase::NewerEligibleRunInProgress);
}

#[test]
fn generated_acquire_resolves_push_to_exactly_one_merged_same_repository_pull_request() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
    let output = fixture.run_acquire_push(None);
    assert!(
        output.status.success(),
        "valid squash-merge push acquire failed ({:?}):\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = logged_gh_commands(&fixture);
    assert!(
        commands.first().is_some_and(|args| {
            args.first().is_some_and(|arg| arg == "api")
                && args.iter().any(|arg| {
                    arg.starts_with(&format!("repos/{REPOSITORY}/commits/{MERGE_SHA}/pulls"))
                })
        }),
        "push resolution must query PRs associated with the event merge SHA: {commands:?}"
    );
    assert!(
        commands.iter().any(|args| {
            args.iter().any(|arg| {
                arg.contains(&format!(
                    "/actions/workflows/ci-pr.yml/runs?head_sha={HEAD_SHA}"
                ))
            })
        }),
        "the resolved PR head must select the candidate run: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .all(|args| args.first().is_some_and(|arg| arg == "api")),
        "a valid merged PR association must reach exact REST artifact transport: {commands:?}"
    );
    assert!(
        fixture.generated.acquire.contains(
            "select(.merge_commit_sha == $merge_sha and .merged_at != null and .base.ref == $default_branch and .base.repo.full_name == $repository and .head.repo.full_name == $repository)"
        ),
        "push selection must preserve merge, default-branch, and same-repository predicates"
    );
    let github_env = fs::read_to_string(fixture.run_temp.join("github.env"))
        .expect("push acquire environment exports");
    let manifest_path = github_env
        .lines()
        .find_map(|line| line.strip_prefix("VELNOR_WORKFLOW_CANDIDATE_MANIFEST="))
        .expect("push candidate manifest export");
    let manifest: JsonValue =
        serde_json::from_slice(&fs::read(manifest_path).expect("push downloaded manifest bytes"))
            .expect("push downloaded manifest JSON");
    assert_eq!(manifest["revision"], json!(HEAD_SHA));
    let build_revision = manifest["build_revision"]
        .as_str()
        .expect("push downloaded build revision");
    assert!(
        is_git_sha1(build_revision),
        "push acquire requires a syntactically valid build revision: {build_revision}"
    );

    for failure in [
        FailureCase::PushNoAssociatedPullRequest,
        FailureCase::PushAmbiguousPullRequests,
        FailureCase::PushWrongMergeCommit,
        FailureCase::PushUnmergedPullRequest,
        FailureCase::PushWrongBaseBranch,
        FailureCase::PushWrongBaseRepository,
        FailureCase::PushForkHeadRepository,
        FailureCase::PushMalformedResponse,
        FailureCase::PushPullRequestPageOverflow,
    ] {
        let fixture = TransportFixture::new();
        fixture.valid_artifact();
        let output = fixture.run_acquire_push(Some(failure));
        assert!(
            !output.status.success(),
            "{failure:?} unexpectedly passed push association resolution:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let commands = logged_gh_commands(&fixture);
        assert_eq!(
            commands.len(),
            1,
            "{failure:?} must stop after resolving the push association: {commands:?}"
        );
        assert!(
            commands[0].iter().any(|arg| {
                arg.starts_with(&format!("repos/{REPOSITORY}/commits/{MERGE_SHA}/pulls"))
            }),
            "{failure:?} did not query the push merge association: {commands:?}"
        );
    }
}

#[test]
fn generated_acquire_pins_ghes_server_and_replaces_ambient_enterprise_token() {
    let fixture = TransportFixture::new();
    let policy_yaml = read_workflow(&fixture.root.join("generated"), "ci-policy.yml");
    let policy = parse_workflow(&policy_yaml);
    assert!(
        policy["jobs"]["policy"]["steps"].is_sequence(),
        "generated ci-policy.yml must parse before the production acquisition regression"
    );
    let trusted_profile_arg =
        format!("'{{\"{PUBLISHER_JOB}\":\"{PRODUCER_REST_JOB_DISPLAY_NAME}\"}}'");
    assert!(
        fixture.generated.acquire.contains(&trusted_profile_arg),
        "parsed production acquire step must embed the Rust IR's trusted job-ID/display-name mapping: {}",
        fixture.generated.acquire
    );
    fixture.valid_artifact();
    let output = fixture.run_acquire_at_server_url(None, "Linux", "X64", "https://ghe.example");
    assert!(
        output.status.success(),
        "GHES acquire failed ({:?}):\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let commands = logged_gh_commands(&fixture);
    assert!(
        commands.len() >= 4,
        "REST selection must read run, artifacts, attempt jobs, and exact metadata: {commands:?}"
    );
    assert!(
        commands.iter().all(|args| {
            args.first().is_some_and(|arg| arg == "api")
                && args
                    .windows(2)
                    .any(|pair| pair == ["--hostname", "ghe.example"])
        }),
        "every GHES API call must use the validated host: {commands:?}"
    );
    let requests = logged_curl_requests(&fixture);
    assert!(requests[0]["url"]
        .as_str()
        .expect("GHES API URL")
        .starts_with(&format!(
            "https://ghe.example/api/v3/repos/{REPOSITORY}/actions/jobs/{PRODUCER_REST_JOB_ID}/logs"
        )));
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0]["stage"], json!("job_logs_api"));
    assert_eq!(requests[1]["stage"], json!("job_logs_signed"));
    assert_eq!(requests[2]["stage"], json!("api"));
    assert_eq!(requests[3]["stage"], json!("signed"));
    assert!(requests[2]["url"]
        .as_str()
        .expect("GHES artifact API URL")
        .starts_with(&format!(
            "https://ghe.example/api/v3/repos/{REPOSITORY}/actions/artifacts/{ARTIFACT_ID}/zip"
        )));
    let scenario: JsonValue =
        serde_json::from_slice(&fs::read(&fixture.scenario).expect("GHES fixture scenario"))
            .expect("GHES scenario JSON");
    assert!(scenario["jobs"][0].get("check_run_id").is_none());
}

#[test]
fn generated_acquire_rejects_unsupported_server_urls_before_github_access() {
    for server_url in [
        "https://github.com.evil.invalid/path",
        "https://ghe.example:8443",
    ] {
        let fixture = TransportFixture::new();
        fixture.valid_artifact();
        let output = fixture.run_acquire_at_server_url(None, "Linux", "X64", server_url);
        assert!(
            !output.status.success(),
            "unsupported server URL {server_url} must fail closed"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("GITHUB_SERVER_URL must be an HTTPS hostname without a port or path"),
            "invalid server URL should explain the supported host format: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            logged_gh_commands(&fixture).is_empty(),
            "invalid server URL {server_url} must fail before any GitHub request"
        );
    }
}

#[test]
fn generated_acquire_rejects_transport_and_identity_faults() {
    // Selection faults stop before signed bytes; archive, ZIP, and manifest
    // faults must stop before either candidate path is exported.
    let cases = [
        FailureCase::WrongPlatform,
        FailureCase::WrongRepository,
        FailureCase::WrongRun,
        FailureCase::WrongManifestAttempt,
        FailureCase::WrongManifestPublisherJob,
        FailureCase::WrongManifestArtifactName,
        FailureCase::WrongRevision,
        FailureCase::WrongClosure,
        FailureCase::MalformedBuildRevision,
        FailureCase::WrongBinaryDigest,
        FailureCase::WrongArtifactName,
        FailureCase::ExpiredArtifact,
        FailureCase::ArtifactFromDifferentRun,
        FailureCase::MissingArtifact,
        FailureCase::PendingRunWithoutArtifact,
        FailureCase::MissingJobLogMarker,
        FailureCase::EmptyJobLog,
        FailureCase::MalformedJobLogMarker,
        FailureCase::DuplicateJobLogMarker,
        FailureCase::WrongJobLogRun,
        FailureCase::WrongJobLogAttempt,
        FailureCase::WrongJobLogPublisher,
        FailureCase::WrongJobLogArtifactName,
        FailureCase::WrongJobLogArtifactId,
        FailureCase::JobLogTooLarge,
        FailureCase::OldAttemptArtifactOnly,
        FailureCase::WrongProducerRestName,
        FailureCase::WrongProducerRestNameWithoutCheckRunId,
        FailureCase::DuplicatePublishers,
        FailureCase::RunsOverflow,
        FailureCase::RunsResponseTooLarge,
        FailureCase::ArtifactsOverflow,
        FailureCase::RunsAtPageCap,
        FailureCase::ArtifactsAtPageCap,
        FailureCase::ArtifactTooLarge,
        FailureCase::MissingServiceDigest,
        FailureCase::WrongServiceDigest,
        FailureCase::ModifiedArtifactMetadata,
        FailureCase::DuplicateArtifact,
        FailureCase::ApiNotRedirect,
        FailureCase::SignedUrlHttpFailure,
        FailureCase::RedirectWithCredentials,
        FailureCase::RedirectDowngrade,
        FailureCase::RedirectInvalidHost,
        FailureCase::RedirectNumericAddress,
        FailureCase::RedirectNonstandardPort,
        FailureCase::SignedUrlRedirect,
        FailureCase::PartialDownload,
        FailureCase::ZipTraversal,
        FailureCase::ZipDuplicate,
        FailureCase::ZipSymlink,
        FailureCase::ZipExtra,
        FailureCase::ZipCentralDirectoryTooLarge,
        FailureCase::ArchiveStreamTooLarge,
        FailureCase::ExtractedSizeTooLarge,
        FailureCase::ForkHead,
        FailureCase::UnsupportedEvent,
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        match case {
            FailureCase::ZipTraversal | FailureCase::ZipDuplicate => assert!(
                stderr.contains("candidate ZIP must contain exactly one binary and one manifest"),
                "{case:?} did not fail on the archive member set: {stderr}"
            ),
            FailureCase::ZipSymlink => assert!(
                stderr.contains("candidate ZIP contains a non-regular entry"),
                "symlink member was not rejected by ZIP type validation: {stderr}"
            ),
            FailureCase::ZipExtra => assert!(
                stderr.contains("candidate ZIP must contain exactly two entries"),
                "extra entry was not rejected by count preflight: {stderr}"
            ),
            FailureCase::ZipCentralDirectoryTooLarge => assert!(
                stderr.contains("candidate ZIP directory exceeds the accepted bounds"),
                "oversized central directory was not rejected before ZIP parsing: {stderr}"
            ),
            FailureCase::RunsResponseTooLarge => assert!(
                stderr.contains("exceeded 16777216 bytes"),
                "oversized API response did not stop at the JSON cap: {stderr}"
            ),
            FailureCase::JobLogTooLarge => assert!(
                stderr.contains("producer job log exceeded the 16777216-byte limit"),
                "oversized job log was not stopped at its byte cap: {stderr}"
            ),
            FailureCase::ArchiveStreamTooLarge => assert!(
                stderr.contains("artifact archive exceeded"),
                "oversized stream was not stopped by the byte cap: {stderr}"
            ),
            FailureCase::ExtractedSizeTooLarge => assert!(
                stderr.contains("candidate ZIP extracted size exceeds"),
                "expanded size was not stopped by the extraction cap: {stderr}"
            ),
            FailureCase::EmptyJobLog => assert!(
                stderr.contains("producer job log is empty"),
                "empty plaintext job log was not rejected: {stderr}"
            ),
            FailureCase::MissingJobLogMarker => assert!(
                stderr.contains("found 0"),
                "nonempty job log without a marker was not rejected: {stderr}"
            ),
            FailureCase::WrongJobLogPublisher => assert!(
                stderr.contains("producer job log GITHUB_JOB key does not match its selected REST job profile"),
                "wrong but well-formed marker job key was not rejected against the REST publisher: {stderr}"
            ),
            FailureCase::WrongProducerRestName
            | FailureCase::WrongProducerRestNameWithoutCheckRunId => assert!(
                stderr.contains("does not identify exactly one trusted publisher profile"),
                "unrecognized REST producer display name was not rejected: {stderr}"
            ),
            _ => {}
        }
        assert_eq!(
            fixture.sleep_marker.is_file(),
            matches!(
                case,
                FailureCase::PendingRunWithoutArtifact
                    | FailureCase::MissingArtifact
                    | FailureCase::WrongArtifactName
            ),
            "{case:?} polling marker mismatch; stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let requests = logged_curl_requests(&fixture);
        let downloaded = requests
            .iter()
            .any(|request| request["stage"] == json!("signed"));
        assert_eq!(
            downloaded,
            matches!(
                case,
                FailureCase::WrongPlatform
                    | FailureCase::WrongRepository
                    | FailureCase::WrongRun
                    | FailureCase::WrongManifestAttempt
                    | FailureCase::WrongManifestPublisherJob
                    | FailureCase::WrongManifestArtifactName
                    | FailureCase::WrongRevision
                    | FailureCase::WrongClosure
                    | FailureCase::MalformedBuildRevision
                    | FailureCase::WrongBinaryDigest
                    | FailureCase::WrongServiceDigest
                    | FailureCase::SignedUrlHttpFailure
                    | FailureCase::SignedUrlRedirect
                    | FailureCase::PartialDownload
                    | FailureCase::ZipTraversal
                    | FailureCase::ZipDuplicate
                    | FailureCase::ZipSymlink
                    | FailureCase::ZipExtra
                    | FailureCase::ZipCentralDirectoryTooLarge
                    | FailureCase::ArchiveStreamTooLarge
                    | FailureCase::ExtractedSizeTooLarge
            ),
            "{case:?} unexpected signed archive request state: {requests:?}"
        );
        if downloaded {
            assert_eq!(
                requests.len(),
                4,
                "{case:?} must make two authenticated API calls and two signed fetches"
            );
            assert_eq!(requests[0]["stage"], json!("job_logs_api"));
            assert_eq!(requests[1]["stage"], json!("job_logs_signed"));
            assert_eq!(requests[2]["stage"], json!("api"));
            assert_eq!(requests[3]["stage"], json!("signed"));
        } else if matches!(
            case,
            FailureCase::ApiNotRedirect
                | FailureCase::RedirectWithCredentials
                | FailureCase::RedirectDowngrade
                | FailureCase::RedirectInvalidHost
                | FailureCase::RedirectNumericAddress
                | FailureCase::RedirectNonstandardPort
        ) {
            assert_eq!(
                requests.len(),
                3,
                "{case:?} must stop after the artifact API redirect"
            );
            assert_eq!(requests[0]["stage"], json!("job_logs_api"));
            assert_eq!(requests[1]["stage"], json!("job_logs_signed"));
            assert_eq!(requests[2]["stage"], json!("api"));
        } else if matches!(
            case,
            FailureCase::EmptyJobLog
                | FailureCase::MissingJobLogMarker
                | FailureCase::MalformedJobLogMarker
                | FailureCase::DuplicateJobLogMarker
                | FailureCase::WrongJobLogRun
                | FailureCase::WrongJobLogAttempt
                | FailureCase::WrongJobLogPublisher
                | FailureCase::WrongJobLogArtifactName
                | FailureCase::WrongJobLogArtifactId
                | FailureCase::JobLogTooLarge
                | FailureCase::PendingRunWithoutArtifact
                | FailureCase::WrongArtifactName
                | FailureCase::ExpiredArtifact
                | FailureCase::ArtifactFromDifferentRun
                | FailureCase::MissingArtifact
                | FailureCase::ArtifactsOverflow
                | FailureCase::ArtifactsAtPageCap
                | FailureCase::ArtifactTooLarge
                | FailureCase::MissingServiceDigest
                | FailureCase::ModifiedArtifactMetadata
                | FailureCase::DuplicateArtifact
                | FailureCase::OldAttemptArtifactOnly
        ) {
            assert_eq!(
                requests.len(),
                2,
                "{case:?} must reject after job-log verification and before artifact download"
            );
            assert_eq!(requests[0]["stage"], json!("job_logs_api"));
            assert_eq!(requests[1]["stage"], json!("job_logs_signed"));
        } else {
            assert!(
                requests.is_empty(),
                "{case:?} must fail before curl: {requests:?}"
            );
        }
        if matches!(case, FailureCase::ForkHead | FailureCase::UnsupportedEvent) {
            assert!(
                logged_gh_commands(&fixture).is_empty() && requests.is_empty(),
                "{case:?} must fail before querying GitHub: {:?}",
                logged_gh_commands(&fixture)
            );
        }
        if case == FailureCase::RunsResponseTooLarge {
            let commands = logged_gh_commands(&fixture);
            assert_eq!(
                commands.len(),
                1,
                "oversized runs response must stop before artifact/job REST calls: {commands:?}"
            );
            assert!(commands[0]
                .iter()
                .any(|arg| { arg.contains("/actions/workflows/ci-pr.yml/runs?") }));
        }
        let env = fs::read_to_string(fixture.run_temp.join("github.env")).unwrap_or_default();
        assert!(
            !env.contains("VELNOR_WORKFLOW_CANDIDATE_BINARY=")
                && !env.contains("VELNOR_WORKFLOW_CANDIDATE_MANIFEST="),
            "{case:?} must reject before exporting candidate paths: {env}"
        );
        assert_no_candidate_scratch(&fixture, case);
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
    assert_eq!(manifest["platform"], json!(PLATFORM));
    assert_eq!(manifest["repository"], json!(REPOSITORY));
    assert_eq!(manifest["run_id"], json!(RUN_ID.to_string()));
    assert_eq!(manifest["run_attempt"], json!(RUN_ATTEMPT));
    assert_eq!(manifest["publisher_job"], json!(PUBLISHER_JOB));
    assert_eq!(
        manifest["artifact_name"],
        json!(fixture.candidate_artifact_name("Linux", "X64"))
    );
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
        output
            .lines()
            .any(|line| line == format!("name={}", fixture.candidate_artifact_name("Linux", "X64"))),
        "producer must publish its exact run/attempt/job-qualified artifact name: {output}"
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
    for (label, environment) in [("build", &build_env), ("probe", &probe_env)] {
        let names: Vec<_> = environment
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect();
        assert!(
            names.iter().any(|name| *name == "PATH"),
            "candidate {label} must receive the explicit PATH: {environment:?}"
        );
        assert!(
            names
                .iter()
                .all(|name| matches!(*name, "PATH" | "PWD" | "SHLVL" | "_")),
            "candidate {label} inherited variables beyond PATH and shell defaults: {environment:?}"
        );
    }
    assert!(
        fixture
            .generated
            .producer
            .contains("env -i PATH=\"$PATH\" cargo build"),
        "producer build must clear its inherited environment: {}",
        fixture.generated.producer
    );
    assert!(
        !fixture.run_temp.join("velnor-workflow-head").exists(),
        "producer must remove the temporary head worktree after staging"
    );
}

#[test]
fn generated_producer_binds_runner_platform_and_upload_contract() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact();
    let identity_record = step_run(
        &fixture.generated.producer_workflow,
        "Record candidate artifact identity",
    )
    .expect("post-upload artifact marker step");
    assert!(
        fixture
            .generated
            .producer_workflow
            .contains(&format!("  {PUBLISHER_JOB}:\n")),
        "fixture GITHUB_JOB must equal the generated producer job key"
    );
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
        )
        .expect("publish missing-file policy"),
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
            .contains(r#"--arg publisher_job "$GITHUB_JOB""#),
        "producer manifest must record its portable GITHUB_JOB identity"
    );
    assert!(
        identity_record.contains("VELNOR_CANDIDATE_ARTIFACT")
            && identity_record.contains("$GITHUB_RUN_ID")
            && identity_record.contains("$GITHUB_RUN_ATTEMPT")
            && identity_record.contains("$GITHUB_JOB")
            && identity_record.contains("$CANDIDATE_ARTIFACT_ID")
            && identity_record.contains("$CANDIDATE_ARTIFACT_NAME"),
        "producer marker must bind run, attempt, publisher, uploaded artifact ID, and name"
    );
    assert!(
        fixture.generated.producer_workflow.contains(
            r#"CANDIDATE_ARTIFACT_ID: ${{ steps.candidate-upload.outputs.artifact-id }}"#
        ),
        "producer marker must use the upload action's artifact-id output"
    );
    let publish_position = fixture
        .generated
        .producer_workflow
        .find("- name: Publish candidate generator product")
        .expect("publish step position");
    let identity_position = fixture
        .generated
        .producer_workflow
        .find("- name: Record candidate artifact identity")
        .expect("identity marker position");
    assert!(
        identity_position > publish_position,
        "artifact identity marker must run after upload"
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
    assert_eq!(manifest["run_attempt"], json!(RUN_ATTEMPT));
    assert_eq!(manifest["publisher_job"], json!(PUBLISHER_JOB));
    assert_eq!(
        manifest["artifact_name"],
        json!(fixture.candidate_artifact_name("Linux", "ARM64"))
    );
    let output = fs::read_to_string(fixture.run_temp.join("github.output"))
        .expect("alternate producer output");
    assert!(
        output.lines().any(|line| {
            line == format!("name={}", fixture.candidate_artifact_name("Linux", "ARM64"))
        }),
        "producer must publish exact alternate-platform identity: {output}"
    );
}

#[test]
fn generated_producer_skips_when_candidate_matches_the_base_closure() {
    let matches = TransportFixture::new();
    matches.valid_artifact();
    let output =
        matches.run_producer_build_on_platform_with_base_closure_match("Linux", "X64", true);
    assert!(
        output.status.success(),
        "matching base closure should permit the producer skip:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !matches.run_temp.join("velnor-workflow-candidate").exists(),
        "matching base closure must not stage a candidate"
    );
    assert!(
        fs::read_to_string(matches.run_temp.join("github.output"))
            .expect("skip output")
            .contains("skip=true"),
        "matching base closure must mark candidate publishing skipped"
    );

    let differs = TransportFixture::new();
    differs.valid_artifact();
    let output = differs.run_producer_build();
    assert!(
        output.status.success(),
        "different candidate closure should fall through to the candidate build:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        differs.run_temp.join("velnor-workflow-candidate").is_dir(),
        "different candidate closure must keep the candidate upload path live"
    );
}

#[test]
fn generated_acquire_binds_runner_platform_to_artifact_and_manifest() {
    let fixture = TransportFixture::new();
    fixture.valid_artifact_on_platform("Linux", "ARM64");
    for marker in [
        ".head_repository.id == .repository.id",
        "attempts/{attempt}/jobs",
        "MAX_ARCHIVE_BYTES=268435456",
        "sha256:$downloaded_digest",
        "archive exceeded",
        ".revision == $revision",
        "candidate digest mismatch",
        "candidate manifest closure $manifest_closure is not the head's candidate $head_candidate",
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
fn generated_producer_traps_worktree_cleanup_and_publishes_staged_candidate() {
    let fixture = TransportFixture::new();
    let document = parse_workflow(&fixture.generated.producer_workflow);
    let jobs = document["jobs"].as_mapping().expect("producer jobs map");
    let prepare_precedes_publish =
        jobs.values().any(|job| {
            let Some(steps) = job.get("steps").and_then(YamlValue::as_sequence) else {
                return false;
            };
            let Some(prepare_index) = steps.iter().position(|step| {
                step["name"].as_str() == Some("Prepare candidate generator product")
            }) else {
                return false;
            };
            let Some(publish_index) = steps.iter().position(|step| {
                step["name"].as_str() == Some("Publish candidate generator product")
            }) else {
                return false;
            };
            prepare_index < publish_index
        });
    assert!(
        prepare_precedes_publish,
        "the owning reusable workflow must prepare before publishing: {}",
        fixture.generated.producer_workflow
    );
    let producer = &fixture.generated.producer;
    let worktree_add = producer
        .find("git worktree add --detach")
        .expect("candidate build adds its head worktree");
    let worktree_trap = producer
        .find("trap 'git worktree remove --force \"$worktree\"' EXIT")
        .expect("candidate build traps worktree cleanup");
    let staged_candidate = producer
        .find("stage=\"$RUNNER_TEMP/velnor-workflow-candidate\"")
        .expect("candidate stage uses runner temp");
    let worktree_remove = producer
        .rfind("git worktree remove --force \"$worktree\"")
        .expect("candidate build removes its head worktree");
    assert!(
        worktree_add < worktree_trap
            && worktree_trap < staged_candidate
            && staged_candidate < worktree_remove,
        "candidate worktree cleanup must be trapped before build and explicitly removed after staging: {producer}"
    );
    assert!(
        producer.contains(
            "cargo build --locked -p velnor-workflow --manifest-path \"$worktree/crates/velnor-workflow/Cargo.toml\""
        ),
        "candidate build must target the generator package in the head worktree: {producer}"
    );
    assert_eq!(
        step_with(
            &fixture.generated.producer_workflow,
            "Publish candidate generator product",
            "path"
        )
        .expect("publish path"),
        "${{ runner.temp }}/velnor-workflow-candidate",
        "the publish step consumes the staged candidate"
    );
}

fn read_workflow(output: &Path, name: &str) -> String {
    fs::read_to_string(output.join(".github/workflows").join(name)).expect("generated workflow")
}

fn parse_workflow(workflow: &str) -> YamlValue {
    serde_yaml::from_str(workflow).expect("workflow YAML")
}

fn step_run(workflow: &str, name: &str) -> Option<String> {
    let document = parse_workflow(workflow);
    let jobs = document["jobs"].as_mapping().expect("jobs map");
    for (_job_id, job) in jobs {
        let Some(steps) = job.get("steps").and_then(YamlValue::as_sequence) else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some(name) {
                return step["run"].as_str().map(str::to_owned);
            }
        }
    }
    None
}

fn step_with(workflow: &str, step_name: &str, key: &str) -> Option<String> {
    let document = parse_workflow(workflow);
    let jobs = document["jobs"].as_mapping().expect("jobs map");
    for (_job_id, job) in jobs {
        let Some(steps) = job.get("steps").and_then(YamlValue::as_sequence) else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some(step_name) {
                return step["with"][key].as_str().map(str::to_owned);
            }
        }
    }
    None
}

fn materialize_github_expressions(script: &str, runner_os: &str, runner_arch: &str) -> String {
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
        } else {
            match after[..end].trim() {
                "RUNNER_OS" => result.push_str(runner_os),
                "RUNNER_ARCH" => result.push_str(runner_arch),
                "runner.temp" => {
                    // This expression is inside a grep pattern for the raw
                    // workflow contract. Keep that protocol text literal.
                    result.push_str(r"\${{ runner.temp }}");
                }
                _ => result.push_str("fixture-expression"),
            }
        }
        offset = expression_end;
    }
    result.push_str(&script[offset..]);
    result
}

fn materialize_producer_expressions(script: &str, runner_os: &str, runner_arch: &str) -> String {
    script
        .replace("${RUNNER_OS}", runner_os)
        .replace("${RUNNER_ARCH}", runner_arch)
}

fn logged_gh_commands(fixture: &TransportFixture) -> Vec<Vec<String>> {
    fs::read_to_string(&fixture.gh_log)
        .map(|log| {
            log.lines()
                .map(|line| serde_json::from_str(line).expect("GitHub argv JSON"))
                .collect()
        })
        .unwrap_or_default()
}

fn logged_curl_requests(fixture: &TransportFixture) -> Vec<JsonValue> {
    fs::read_to_string(&fixture.curl_log)
        .map(|log| {
            log.lines()
                .map(|line| serde_json::from_str(line).expect("curl request JSON"))
                .collect()
        })
        .unwrap_or_default()
}

fn assert_no_candidate_scratch(fixture: &TransportFixture, case: FailureCase) {
    let leftovers: Vec<_> = fs::read_dir(&fixture.run_temp)
        .expect("runner temp after acquisition")
        .map(|entry| entry.expect("runner temp entry").file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| {
            name.starts_with("velnor-workflow-candidate")
                || name.starts_with("velnor-workflow-acquire")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "{case:?} left partial transport/extraction state behind: {leftovers:?}"
    );
    if case == FailureCase::ZipTraversal {
        assert!(
            !fixture.run_temp.join("escape").exists(),
            "traversal entry escaped the extraction directory"
        );
    }
}

fn api_base_for_host(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com".to_owned()
    } else if host.ends_with(".ghe.com") {
        format!("https://api.{host}")
    } else {
        format!("https://{host}/api/v3")
    }
}

fn prepend_path(bin_dir: &Path) -> String {
    format!("{}:{}", bin_dir.display(), env::var("PATH").expect("PATH"))
}

fn candidate_log_marker(
    run_id: u64,
    attempt: u64,
    publisher_job: &str,
    artifact_id: u64,
    artifact_name: &str,
) -> String {
    format!(
        "VELNOR_CANDIDATE_ARTIFACT\t{run_id}\t{attempt}\t{publisher_job}\t{artifact_id}\t{artifact_name}\n"
    )
}

fn valid_scenario(artifact_name: &str, artifact_size: u64, artifact_digest: &str) -> JsonValue {
    // The fork decoy shares the candidate head and comes first; only the
    // generated same-repository jq filter should let acquisition reach RUN_ID.
    json!({
        "runs": [{
            "id": RUN_ID + 1, "event": "pull_request", "head_sha": HEAD_SHA,
            "status": "completed", "created_at": "2026-10-02T00:00:00Z",
            "repository": {"id": 42, "full_name": REPOSITORY},
            "head_repository": {"id": 84, "full_name": "attacker/fork"}
        }, {
            "id": RUN_ID, "event": "pull_request", "head_sha": HEAD_SHA,
            "status": "completed", "run_attempt": RUN_ATTEMPT,
            "created_at": "2026-10-01T00:00:00Z",
            "repository": {"id": 42, "full_name": REPOSITORY},
            "head_repository": {"id": 42, "full_name": REPOSITORY}
        }],
        "pull_requests": [{
            "merge_commit_sha": MERGE_SHA, "merged_at": "2026-09-30T00:00:00Z",
            "base": {"ref": "main", "repo": {"full_name": REPOSITORY}},
            "head": {"sha": HEAD_SHA, "repo": {"full_name": REPOSITORY}}
        }],
        "jobs": [{
            "id": PRODUCER_REST_JOB_ID, "check_run_id": PRODUCER_REST_JOB_ID,
            "name": PRODUCER_REST_JOB_DISPLAY_NAME,
            "run_id": RUN_ID, "run_attempt": RUN_ATTEMPT,
            "status": "completed", "conclusion": "success",
            "started_at": "2026-10-01T00:00:00Z", "completed_at": "2026-10-01T00:01:00Z",
            "steps": [
                {"name": "Prepare candidate generator product", "status": "completed", "conclusion": "success",
                 "started_at": "2026-10-01T00:00:02Z", "completed_at": "2026-10-01T00:00:05Z"},
                {"name": "Publish candidate generator product", "status": "completed", "conclusion": "success",
                 "started_at": PUBLISH_START, "completed_at": PUBLISH_END},
                {"name": "Record candidate artifact identity", "status": "completed", "conclusion": "success",
                 "started_at": "2026-10-01T00:00:21Z", "completed_at": "2026-10-01T00:00:22Z"}
            ]
        }],
        "artifacts": [{
            "id": ARTIFACT_ID, "run_id": RUN_ID, "name": artifact_name, "expired": false,
            "size_in_bytes": artifact_size, "digest": format!("sha256:{artifact_digest}"),
            "archive_download_url": format!("https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{ARTIFACT_ID}/zip"),
            "created_at": "2026-10-01T00:00:15Z",
            "workflow_run": {"id": RUN_ID, "head_sha": HEAD_SHA}
        }, {
            "id": ARTIFACT_ID + 1, "run_id": RUN_ID + 1, "name": artifact_name, "expired": false,
            "size_in_bytes": artifact_size, "digest": format!("sha256:{artifact_digest}"),
            "archive_download_url": format!("https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{}/zip", ARTIFACT_ID + 1),
            "created_at": "2026-10-01T00:00:15Z",
            "workflow_run": {"id": RUN_ID + 1, "head_sha": HEAD_SHA}
        }],
        "redirect_location": "https://objects.githubusercontent.com/artifacts/signed.zip",
        "api_http_status": 302,
        "signed_http_status": 200,
        "job_log_contents": candidate_log_marker(
            RUN_ID,
            RUN_ATTEMPT,
            PUBLISHER_JOB,
            ARTIFACT_ID,
            artifact_name,
        ),
        "job_log_redirect_location": "https://objects.githubusercontent.com/artifacts/job-logs.txt"
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

fn zip_files_with_malformation(path: &Path, binary: &Path, manifest: &Path, mode: &str) {
    let script = r#"import os, stat, struct, sys, warnings, zipfile
archive, binary, manifest, mode = sys.argv[1:]
with warnings.catch_warnings():
    warnings.simplefilter("ignore", UserWarning)
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
        if mode == "symlink":
            link = zipfile.ZipInfo("velnor-workflow")
            link.create_system = 3
            link.external_attr = (stat.S_IFLNK | 0o777) << 16
            out.writestr(link, "target")
            out.write(manifest, "candidate-manifest.json")
        elif mode == "traversal":
            out.write(binary, "../escape")
            out.write(manifest, "candidate-manifest.json")
        elif mode == "duplicate":
            out.write(binary, "velnor-workflow")
            out.writestr("velnor-workflow", b"duplicate")
        elif mode == "central-directory":
            payload = b"x" * 32764
            extra = struct.pack("<HH", 0xCAFE, len(payload)) + payload
            for name, source in (("velnor-workflow", binary), ("candidate-manifest.json", manifest)):
                entry = zipfile.ZipInfo(name)
                entry.extra = extra
                with open(source, "rb") as contents:
                    out.writestr(entry, contents.read())
        else:
            out.write(binary, "velnor-workflow")
            out.write(manifest, "candidate-manifest.json")
        if mode == "extra":
            out.writestr("unexpected", "extra")
"#;
    let status = Command::new("python3")
        .args([
            "-c",
            script,
            path.to_str().expect("archive path"),
            binary.to_str().expect("binary path"),
            manifest.to_str().expect("manifest path"),
            mode,
        ])
        .status()
        .expect("malformed zip fixture");
    assert!(status.success(), "malformed zip fixture failed");
}

fn file_sha256(path: &Path) -> String {
    hex_sha256(&fs::read(path).expect("hash file"))
}

fn is_git_sha1(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
}

fn write_generator_fixture_tree(root: &Path) {
    fs::create_dir_all(root.join(".github-gen")).expect("fixture root");
    fs::create_dir_all(root.join("crates/velnor-workflow/src")).expect("fixture generator crate");
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/velnor-workflow\"]\nresolver = \"3\"\n",
    )
    .expect("workspace Cargo.toml");
    fs::write(
        root.join("crates/velnor-workflow/Cargo.toml"),
        "[package]\nname = \"velnor-workflow\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("generator crate Cargo.toml");
    fs::write(
        root.join("crates/velnor-workflow/src/lib.rs"),
        "pub fn fixture() {}\n",
    )
    .expect("fixture source");
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
import json, os, re, sys
from urllib.parse import parse_qs, urlsplit
scenario = json.load(open(os.environ["FIXTURE_SCENARIO"]))
args = sys.argv[1:]
expected_host = os.environ["FIXTURE_EXPECT_GH_HOST"]
expected_runs_jq = os.environ["FIXTURE_EXPECT_RUNS_JQ"]
for variable in ("GH_HOST", "GH_CONFIG_DIR", "GITHUB_ENTERPRISE_TOKEN", "GH_REPO"):
    if variable in os.environ:
        raise SystemExit(f"ambient {variable} override was not cleared")
if os.environ.get("GH_TOKEN") != "fixture-token":
    raise SystemExit("protected GH_TOKEN was not preserved")
expected_enterprise_token = os.environ["FIXTURE_EXPECT_ENTERPRISE_TOKEN"]
if os.environ.get("GH_ENTERPRISE_TOKEN", "") != expected_enterprise_token:
    raise SystemExit("inherited enterprise token was not cleared or correctly replaced")
if os.environ.get("FIXTURE_GH_LOG"):
    with open(os.environ["FIXTURE_GH_LOG"], "a") as log:
        log.write(json.dumps(args) + "\n")
if not args or args[0] != "api":
    raise SystemExit("unsupported gh command")
if "--hostname" not in args or args[args.index("--hostname") + 1] != expected_host:
    raise SystemExit("gh api was not pinned to the trusted server")
endpoint = next((arg for arg in args if arg.startswith("repos/")), "")
parsed = urlsplit(endpoint)
query = parse_qs(parsed.query)
page_number = int(query.get("page", ["1"])[0])
page_size = int(query.get("per_page", ["100"])[0])
start = (page_number - 1) * page_size
end = start + page_size
if "/actions/workflows/ci-pr.yml/runs" in parsed.path:
    all_runs = [
        run for run in scenario["runs"]
        if run["head_sha"] == query.get("head_sha", [""])[0]
        and run["event"] == query.get("event", [""])[0]
    ]
    all_runs.sort(key=lambda run: run["created_at"], reverse=True)
    raw_page = all_runs[start:end]
    if "--jq" not in args or args[args.index("--jq") + 1] != expected_runs_jq:
        raise SystemExit("runs query omitted the exact same-repository pagination filter")
    if scenario.get("runs_response_too_large"):
        sys.stdout.write(" " * (16 * 1024 * 1024 + 1))
        raise SystemExit(0)
    same_repository = [
        run for run in raw_page
        if run["head_repository"]["id"] == run["repository"]["id"]
    ]
    print(json.dumps({
        "total_count": scenario.get("runs_total_count", len(all_runs)),
        "page_count": len(raw_page),
        "workflow_runs": same_repository,
    }))
elif re.search(r"/actions/runs/\d+/artifacts$", parsed.path):
    match = re.search(r"/actions/runs/(\d+)/artifacts$", parsed.path)
    run_id = match.group(1)
    all_artifacts = [
        artifact for artifact in scenario["artifacts"]
        if str(artifact.get("run_id")) == run_id
    ]
    print(json.dumps({
        "total_count": scenario.get("artifacts_total_count", len(all_artifacts)),
        "artifacts": all_artifacts[start:end],
    }))
elif re.search(r"/actions/runs/\d+/attempts/\d+/jobs$", parsed.path):
    match = re.search(r"/actions/runs/(\d+)/attempts/(\d+)/jobs$", parsed.path)
    run_id, attempt = match.groups()
    all_jobs = [
        job for job in scenario["jobs"]
        if str(job.get("run_id")) == run_id and str(job.get("run_attempt")) == attempt
    ]
    print(json.dumps({"total_count": len(all_jobs), "jobs": all_jobs[start:end]}))
elif re.search(r"/actions/artifacts/\d+$", parsed.path):
    artifact_id = int(parsed.path.rsplit("/", 1)[1])
    artifact = next((item for item in scenario["artifacts"] if item["id"] == artifact_id), None)
    if artifact is None:
        print("{}")
    else:
        exact = dict(artifact)
        if scenario.get("mutate_exact_artifact_digest") and exact["id"] == scenario["artifacts"][0]["id"]:
            exact["digest"] = "sha256:" + "f" * 64
            exact["size_in_bytes"] += 1
        print(json.dumps(exact))
elif "/commits/" in parsed.path and parsed.path.endswith("/pulls"):
    if scenario.get("malformed_push_response"):
        print("{malformed response")
    else:
        print(json.dumps(scenario["pull_requests"]))
else:
    raise SystemExit("unexpected REST endpoint: " + endpoint)
"#;

const CURL_FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, sys
scenario = json.load(open(os.environ["FIXTURE_SCENARIO"]))
args = sys.argv[1:]
url = args[-1]
def record(stage, authenticated):
    with open(os.environ["FIXTURE_CURL_LOG"], "a") as log:
        log.write(json.dumps({"stage": stage, "authenticated": authenticated, "url": url, "args": args}) + "\n")
if "--disable" not in args or "--proto" not in args or args[args.index("--proto") + 1] != "=https":
    raise SystemExit("curl must disable ambient configuration and require HTTPS")
if "--connect-timeout" not in args or args[args.index("--connect-timeout") + 1] != "10":
    raise SystemExit("each curl request needs its own 10-second connection timeout")
if "--config" in args:
    if args[args.index("--config") + 1] != "-":
        raise SystemExit("authenticated API request must use the private inline config")
    config = sys.stdin.read()
    if "Authorization: Bearer fixture-token" not in config or "Accept: application/vnd.github+json" not in config:
        raise SystemExit("authenticated API request did not receive the expected bearer configuration")
    expected_job_logs_url = scenario["job_log_api_url"]
    is_job_logs = url == expected_job_logs_url
    if is_job_logs:
        record("job_logs_api", True)
        status = int(scenario.get("job_logs_api_http_status", 302))
        location = scenario.get("job_log_redirect_location", "https://objects.githubusercontent.com/artifacts/job-logs.txt")
    else:
        if "/actions/jobs/" in url:
            raise SystemExit("job logs API request did not use the exact REST producer job ID")
        artifact = next(
            item for item in scenario["artifacts"]
            if item.get("run_id") == 900
            and item.get("workflow_run", {}).get("id") == 900
            and item.get("created_at") == "2026-10-01T00:00:15Z"
            and item.get("name", "").startswith("velnor-workflow-candidate-")
        )
        if url != artifact["archive_download_url"]:
            raise SystemExit("API request did not use the selected artifact's exact URL")
        extraction_dirs = [
            os.path.join(os.environ["RUNNER_TEMP"], name)
            for name in os.listdir(os.environ["RUNNER_TEMP"])
            if name.startswith("velnor-workflow-candidate.")
        ]
        if len(extraction_dirs) != 1 or os.listdir(extraction_dirs[0]):
            raise SystemExit("candidate extraction directory must start empty before transport")
        record("api", True)
        status = int(scenario.get("api_http_status", 302))
        location = scenario.get("redirect_location", "https://objects.githubusercontent.com/artifacts/signed.zip")
    if "--dump-header" not in args or "--output" not in args or args[args.index("--output") + 1] != "/dev/null":
        raise SystemExit("API request must capture headers without following the redirect")
    if "--write-out" not in args or args[args.index("--write-out") + 1] != "%{http_code}":
        raise SystemExit("API request must expose the redirect status without following it")
    if "-L" in args or "--location" in args:
        raise SystemExit("API request must stop at the signed URL redirect")
    if "--max-time" not in args or args[args.index("--max-time") + 1] != "30":
        raise SystemExit("API request needs its own 30-second timeout")
    header_path = args[args.index("--dump-header") + 1]
    with open(header_path, "w") as headers:
        headers.write("HTTP/2 " + str(status) + " Response\r\n")
        if status == 302:
            headers.write("Location: " + location + "\r\n")
        headers.write("\r\n")
    sys.stdout.write(str(status))
    raise SystemExit(0)
if "-L" in args or "--location" in args:
    raise SystemExit("signed URL request must not follow redirects")
if "--max-time" not in args or args[args.index("--max-time") + 1] != "120":
    raise SystemExit("signed URL request needs its own 120-second timeout")
if "--max-filesize" not in args or args.index("--max-filesize") + 1 >= len(args):
    raise SystemExit("signed URL request needs a transfer size bound")
if "--write-out" not in args or args[args.index("--write-out") + 1] != "%{stderr}%{http_code}":
    raise SystemExit("signed URL request must report its status separately from archive bytes")
if url == scenario.get("job_log_redirect_location"):
    if args[args.index("--max-filesize") + 1] != "16777216":
        raise SystemExit("signed job log request must enforce the 16 MiB plaintext cap")
    if any(argument == "fixture-token" or "Authorization" in argument for argument in args):
        raise SystemExit("signed job log request must not carry the GitHub API credential")
    record("job_logs_signed", False)
    status = int(scenario.get("job_logs_signed_http_status", 200))
    if status == 200:
        body = scenario.get("job_log_contents", "").encode("utf-8")
        oversized_log_bytes = int(scenario.get("job_log_oversized_bytes", 0))
    else:
        body = b""
        oversized_log_bytes = 0
elif url == scenario.get("redirect_location"):
    expected_archive_limit = "8192" if scenario.get("download_padding_bytes") else "268435456"
    if args[args.index("--max-filesize") + 1] != expected_archive_limit:
        raise SystemExit("signed artifact request must enforce the configured archive byte cap")
    if any(argument == "fixture-token" or "Authorization" in argument for argument in args):
        raise SystemExit("signed artifact request must not carry the GitHub API credential")
    record("signed", False)
    status = int(scenario.get("signed_http_status", 200))
    if status == 200:
        body = open(os.environ["FIXTURE_ARCHIVE"], "rb").read()
        if scenario.get("partial_download"):
            body = body[:max(1, len(body) // 2)]
        else:
            body += b"x" * int(scenario.get("download_padding_bytes", 0))
    else:
        body = b""
    oversized_log_bytes = 0
else:
    raise SystemExit("signed URL request used an unexpected URL")
output_path = None
if "-o" in args:
    output_path = args[args.index("-o") + 1]
elif "--output" in args:
    output_path = args[args.index("--output") + 1]
if output_path and output_path != "/dev/null":
    with open(output_path, "wb") as output:
        output.write(body)
        stream = output
else:
    stream = sys.stdout.buffer
    stream.write(body)
if oversized_log_bytes:
    chunk = b"x" * 1024 * 1024
    remaining = oversized_log_bytes
    while remaining:
        next_chunk = chunk[:min(len(chunk), remaining)]
        stream.write(next_chunk)
        remaining -= len(next_chunk)
sys.stderr.write(str(status))
if url == scenario.get("redirect_location") and scenario.get("partial_download"):
    raise SystemExit(23)
if status >= 400:
    raise SystemExit(22)
raise SystemExit(0)
"#;

const GIT_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, shutil, sys
args = sys.argv[1:]
commands = {"init", "fetch", "show", "archive", "ls-tree", "cat-file", "rev-parse"}
commands.add("worktree")
command_index = next((index for index, value in enumerate(args) if value in commands), len(args))
command = args[command_index] if command_index < len(args) else ""
rest = args[command_index + 1:]
root = os.getcwd()
if command == "init":
    os.makedirs(args[-1], exist_ok=True)
elif command == "fetch":
    pass
elif command == "worktree":
    if rest[0] == "add":
        worktree = rest[-2]
        os.makedirs(os.path.join(worktree, "crates"), exist_ok=True)
        shutil.copyfile(os.path.join(root, "Cargo.toml"), os.path.join(worktree, "Cargo.toml"))
        shutil.copyfile(os.path.join(root, "Cargo.lock"), os.path.join(worktree, "Cargo.lock"))
        shutil.copyfile(os.path.join(root, "rust-toolchain.toml"), os.path.join(worktree, "rust-toolchain.toml"))
        shutil.copytree(os.path.join(root, "crates/velnor-workflow"), os.path.join(worktree, "crates/velnor-workflow"))
    elif rest[0] == "remove":
        worktree = rest[-1]
        build_env = os.path.join(worktree, "candidate-build-env.txt")
        if os.path.isfile(build_env):
            shutil.copyfile(build_env, os.path.join(root, "candidate-build-env.txt"))
        shutil.rmtree(worktree, ignore_errors=True)
elif command == "rev-parse":
    target = rest[-1]
    if target.endswith("^{commit}"):
        if target.startswith(os.environ["BASE_SHA"]):
            print(os.environ["BASE_SHA"])
        else:
            print(os.environ["HEAD_SHA"])
    else:
        print(os.environ["HEAD_SHA"])
elif command == "show":
    target = rest[-1]
    if target.endswith(":.github-gen/velnor-workflow.toml"):
        print("revision = \"" + os.environ["BASE_PIN"] + "\"")
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
    if [ "${FIXTURE_BASE_CLOSURE_MATCHES:-}" = "1" ]; then
      printf '%s\n' "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    else
      printf '%s\n' "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    fi
    ;;
  *--rev=5555555555555555555555555555555555555555*) printf '%s\n' "${FIXTURE_MERGE_CLOSURE:-cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc}" ;;
  *--candidate*) printf '%s\n' "${FIXTURE_CLOSURE}" ;;
  *) printf '%s\n' "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc" ;;
esac
"#;

const CARGO_FIXTURE: &str = r#"#!/bin/sh
set -eu
manifest=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--manifest-path" ]; then
    manifest="$2"
    shift 2
  else
    shift
  fi
done
build_root="$(dirname "$(dirname "$(dirname "$manifest")")")"
env | sort > "$build_root/candidate-build-env.txt"
mkdir -p "$build_root/target/debug"
printf '%s\n' '#!/bin/sh' > "$build_root/target/debug/velnor-workflow"
printf '%s\n' 'case "$1" in' >> "$build_root/target/debug/velnor-workflow"
printf '%s\n' '  --closure) env | sort > candidate-probe-env.txt; printf "%s\n" "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" ;;' >> "$build_root/target/debug/velnor-workflow"
printf '%s\n' '  --revision) env | sort > candidate-probe-env.txt; printf "%s\n" "0123456789abcdef0123456789abcdef01234567" ;;' >> "$build_root/target/debug/velnor-workflow"
printf '%s\n' '  *) exit 0 ;;' >> "$build_root/target/debug/velnor-workflow"
printf '%s\n' 'esac' >> "$build_root/target/debug/velnor-workflow"
chmod 0755 "$build_root/target/debug/velnor-workflow"
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
