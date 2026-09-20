//! Executable hostile transport fixtures for the generated bootstrap scripts.
//!
//! Each test extracts a Python heredoc or shell helper from the generated
//! workflow and executes that exact generated body against a harmless offline
//! archive. No candidate, Docker daemon, hosted endpoint, or upload service
//! is contacted.

#![cfg(unix)]
#![recursion_limit = "256"]
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

use serde_json::json;
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};

const HEAD_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const BASE_SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";
const HEAD_TREE_SHA: &str = "1111111111111111111111111111111111111111";
const BASE_TREE_SHA: &str = "2222222222222222222222222222222222222222";
const REPOSITORY: &str = "tailrocks/velnor";
const RUN_ID: u64 = 900;
const RUN_ATTEMPT: u64 = 2;
const WORKFLOW_ID: u64 = 700;
const PRODUCER_JOB_ID: u64 = 800;
const REPLACEMENT_JOB_ID: u64 = 801;
const ARTIFACT_ID: u64 = 1001;
const PR_NUMBER: u64 = 7;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct GeneratedScripts {
    root: PathBuf,
    acquire: String,
    execute: String,
}

impl GeneratedScripts {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = env::temp_dir().join(format!(
            "velnor-bootstrap-transport-hostile-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".github-gen")).expect("fixture root");
        fs::create_dir_all(root.join("src")).expect("fixture source");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"transport-hostile-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
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

        let policy = read_workflow(&output, "ci-policy.yml");
        Self {
            root,
            acquire: step_run(&policy, "Acquire candidate generator product"),
            execute: step_run(&policy, "Execute candidate in pinned sandbox"),
        }
    }
}

impl Drop for GeneratedScripts {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Clone, Copy, Debug)]
enum ZipFault {
    Symlink,
    HardlinkLikeSpecialMode,
    Traversal,
    Duplicate,
    ExcessMembers,
    OversizedDeclaredStream,
}

impl ZipFault {
    const ALL: [Self; 6] = [
        Self::Symlink,
        Self::HardlinkLikeSpecialMode,
        Self::Traversal,
        Self::Duplicate,
        Self::ExcessMembers,
        Self::OversizedDeclaredStream,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::HardlinkLikeSpecialMode => "hardlink-like-special-mode",
            Self::Traversal => "traversal",
            Self::Duplicate => "duplicate",
            Self::ExcessMembers => "excess-members",
            Self::OversizedDeclaredStream => "oversized-declared-stream",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TarFault {
    Symlink,
    Hardlink,
    Traversal,
    Duplicate,
    ExcessMembers,
    OversizedDeclaredStream,
}

impl TarFault {
    const ALL: [Self; 6] = [
        Self::Symlink,
        Self::Hardlink,
        Self::Traversal,
        Self::Duplicate,
        Self::ExcessMembers,
        Self::OversizedDeclaredStream,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::Hardlink => "hardlink",
            Self::Traversal => "traversal",
            Self::Duplicate => "duplicate",
            Self::ExcessMembers => "excess-members",
            Self::OversizedDeclaredStream => "oversized-declared-stream",
        }
    }
}

#[test]
fn generated_candidate_zip_validator_rejects_actual_hostile_members() {
    let generated = GeneratedScripts::new();
    let validator = extract_python(&generated.acquire, "python3 - \"$archive\" \"$candidate\"");
    for fault in ZipFault::ALL {
        let archive = generated
            .root
            .join(format!("candidate-{}.zip", fault.name()));
        let destination = generated.root.join(format!("candidate-{}", fault.name()));
        write_hostile_zip(&archive, fault);
        let output = run_python(
            &validator,
            &[
                archive.to_string_lossy().into_owned(),
                destination.to_string_lossy().into_owned(),
            ],
        );
        assert!(
            !output.status.success(),
            "ZIP fault {:?} passed the exact generated validator:\n{}\n{}",
            fault,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn generated_workflow_tar_scanner_rejects_actual_hostile_members() {
    let generated = GeneratedScripts::new();
    let scanner = extract_python(
        &generated.acquire,
        "python3 - \"$base_workflow_archive\" \"$head_workflow_archive\"",
    );
    for fault in TarFault::ALL {
        let archive = generated
            .root
            .join(format!("workflow-{}.tar", fault.name()));
        write_hostile_tar(&archive, fault);
        let output = run_python(&scanner, &scanner_args(&archive, &archive));
        assert!(
            !output.status.success(),
            "workflow TAR fault {:?} passed the exact generated scanner:\n{}\n{}",
            fault,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn generated_execute_source_census_rejects_actual_hostile_members() {
    let generated = GeneratedScripts::new();
    let census = extract_python(
        &generated.execute,
        "python3 - \"$HANDOFF/source.tar\" \"$input\"",
    );
    for fault in TarFault::ALL {
        let archive = generated.root.join(format!("source-{}.tar", fault.name()));
        let destination = generated.root.join(format!("source-{}", fault.name()));
        write_hostile_tar(&archive, fault);
        let output = run_python(
            &census,
            &[
                archive.to_string_lossy().into_owned(),
                destination.to_string_lossy().into_owned(),
            ],
        );
        assert!(
            !output.status.success(),
            "source TAR fault {:?} passed the exact generated execute census:\n{}\n{}",
            fault,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn generated_namespace_scanner_rejects_candidate_source_host_command() {
    let generated = GeneratedScripts::new();
    let scanner = extract_python(
        &generated.acquire,
        "python3 - \"$base_workflow_archive\" \"$head_workflow_archive\"",
    );
    let contract = generated.root.join("ci-pr.yml");
    let poisoned = generated.root.join("ci-pr-poisoned.yml");
    let pr = read_generated_pr(&generated.root);
    let needle = "          test -z \"${GITHUB_TOKEN:-}\"\n";
    assert!(
        pr.contains(needle),
        "generated producer token guard disappeared"
    );
    fs::write(
        &contract,
        pr.replace(
            needle,
            "          test -z \"${GITHUB_TOKEN:-}\"\n          bash ../candidate-source/evil.sh\n",
        ),
    )
    .expect("poisoned workflow");
    fs::copy(&contract, &poisoned).expect("head workflow");
    let base = generated.root.join("poisoned-base.tar");
    let head = generated.root.join("poisoned-head.tar");
    write_workflow_tar(&base, &contract);
    write_workflow_tar(&head, &poisoned);
    let output = run_python(&scanner, &scanner_args(&base, &head));
    assert!(
        !output.status.success(),
        "generated scanner admitted bash ../candidate-source/evil.sh:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generated_bounded_download_rejects_replaced_partial_path() {
    let generated = GeneratedScripts::new();
    let bounded = extract_shell_function(&generated.acquire, "bounded_curl_download");
    let bin = generated.root.join("fake-bin");
    fs::create_dir_all(&bin).expect("fake bin");
    write_executable(&bin.join("curl"), CURL_FIXTURE);
    write_executable(&bin.join("rm"), RACE_RM_FIXTURE);
    let destination = generated.root.join("download.bin");
    let outside = generated.root.join("outside-marker");
    let once = generated.root.join("race-once");
    fs::write(&outside, b"sentinel").expect("outside marker");
    let script = format!(
        "set -euo pipefail\n{bounded}\nbounded_curl_download https://api.invalid/archive {}\n",
        destination.display()
    );
    let output = Command::new("bash")
        .args(["-euo", "pipefail", "-c", &script])
        .env("PATH", format!("{}:{}", bin.display(), env!("PATH")))
        .env("RACE_TARGET", &outside)
        .env("RACE_ONCE", &once)
        .output()
        .expect("bounded helper");
    assert!(
        !output.status.success(),
        "generated bounded writer followed a replaced partial path:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&outside).expect("outside marker bytes"),
        b"sentinel"
    );
}

struct ProvenanceFixture {
    root: PathBuf,
    contract: PathBuf,
    workflow_tar: PathBuf,
    source_tar: PathBuf,
    archive: PathBuf,
    scenario: PathBuf,
    bin: PathBuf,
    runner_temp: PathBuf,
    artifact_name: String,
    digest: String,
    archive_size: u64,
    acquire: String,
}

impl ProvenanceFixture {
    fn new(generated: &GeneratedScripts) -> Self {
        let artifact_name = generated_artifact_name(&generated.root);
        let contract = generated.root.join("ci-pr.yml");
        fs::write(&contract, read_generated_pr(&generated.root)).expect("workflow contract");
        let workflow_tar = generated.root.join("workflow.tar");
        write_workflow_tar(&workflow_tar, &contract);
        let source_tar = generated.root.join("source.tar");
        write_workflow_tar(&source_tar, &contract);
        let archive = generated.root.join("candidate.zip");
        write_candidate_archive(&archive, &artifact_name);
        let digest = sha256_file(&archive);
        let bin = generated.root.join("fake-bin");
        fs::create_dir_all(&bin).expect("fake bin");
        write_executable(&bin.join("gh"), GH_PROVENANCE_FIXTURE);
        write_executable(&bin.join("curl"), CURL_PROVENANCE_FIXTURE);
        write_executable(&bin.join("git"), GIT_PROVENANCE_FIXTURE);
        write_executable(&bin.join("sha256sum"), SHA256SUM_PROVENANCE_FIXTURE);
        write_executable(&bin.join("stat"), STAT_PROVENANCE_FIXTURE);
        write_executable(&bin.join("du"), DU_PROVENANCE_FIXTURE);
        Self {
            root: generated.root.clone(),
            contract,
            workflow_tar,
            source_tar,
            archive_size: fs::metadata(&archive).expect("archive metadata").len(),
            archive,
            scenario: generated.root.join("scenario.json"),
            bin,
            runner_temp: generated.root.join("runner-temp"),
            artifact_name,
            digest,
            acquire: materialize_github_expressions(&generated.acquire),
        }
    }

    fn write_scenario(&self, replacement: bool) {
        let (artifact_id, created_at, updated_at) = if replacement {
            (
                ARTIFACT_ID + 1,
                "2026-09-20T00:08:00Z",
                "2026-09-20T00:08:30Z",
            )
        } else {
            (ARTIFACT_ID, "2026-09-20T00:02:30Z", "2026-09-20T00:03:00Z")
        };
        let fixture = json!({
            "runs": [{
                "id": RUN_ID, "path": ".github/workflows/ci-pr.yml", "workflow_id": WORKFLOW_ID,
                "event": "pull_request", "head_sha": HEAD_SHA, "status": "completed",
                "conclusion": "success", "run_attempt": RUN_ATTEMPT,
                "created_at": "2026-09-20T00:00:00Z",
                "repository": {"id": 42, "full_name": REPOSITORY},
                "head_repository": {"id": 42, "full_name": REPOSITORY},
                "pull_requests": [{"number": PR_NUMBER, "base": {"sha": BASE_SHA}}]
            }],
            "jobs": [
                {
                    "id": PRODUCER_JOB_ID, "name": "candidate_producer", "run_id": RUN_ID,
                    "head_sha": HEAD_SHA, "status": "completed", "conclusion": "success",
                    "run_attempt": RUN_ATTEMPT, "started_at": "2026-09-20T00:01:00Z",
                    "completed_at": "2026-09-20T00:10:00Z",
                    "steps": [{"id": "candidate_upload", "name": "Upload candidate generator product", "status": "completed", "conclusion": "success"}]
                },
                {
                    "id": REPLACEMENT_JOB_ID, "name": "ordinary_build", "run_id": RUN_ID,
                    "head_sha": HEAD_SHA, "status": "completed", "conclusion": "success",
                    "run_attempt": RUN_ATTEMPT, "started_at": "2026-09-20T00:02:00Z",
                    "completed_at": "2026-09-20T00:09:00Z", "steps": []
                }
            ],
            "artifacts": [{
                "id": artifact_id, "name": self.artifact_name, "expired": false,
                "size_in_bytes": self.archive_size, "digest": format!("sha256:{}", self.digest),
                "expires_at": "2099-01-01T00:00:00Z", "created_at": created_at,
                "updated_at": updated_at, "workflow_run": {"id": RUN_ID}
            }],
            "api_head_tree": HEAD_TREE_SHA, "api_base_tree": BASE_TREE_SHA,
            "api_head_entries": [{"path": "Cargo.toml", "mode": "100644", "type": "blob", "sha": "4444444444444444444444444444444444444444", "size": 1}],
            "api_base_entries": [{"path": "Cargo.toml", "mode": "100644", "type": "blob", "sha": "5555555555555555555555555555555555555555", "size": 1}],
            "object_head_tree": HEAD_TREE_SHA, "object_base_tree": BASE_TREE_SHA
        });
        fs::write(
            &self.scenario,
            serde_json::to_vec(&fixture).expect("scenario JSON"),
        )
        .expect("scenario bytes");
    }

    fn run_acquire(&self) -> Output {
        let _ = fs::remove_dir_all(&self.runner_temp);
        fs::create_dir_all(&self.runner_temp).expect("runner temp");
        Command::new("bash")
            .args(["-euo", "pipefail", "-c", &self.acquire])
            .current_dir(&self.root)
            .env("PATH", format!("{}:{}", self.bin.display(), env!("PATH")))
            .env("FIXTURE_SCENARIO", &self.scenario)
            .env("FIXTURE_ARCHIVE", &self.archive)
            .env("FIXTURE_ACTION_ARCHIVE", &self.workflow_tar)
            .env("FIXTURE_WORKFLOW_TAR", &self.workflow_tar)
            .env("FIXTURE_SOURCE_TAR", &self.source_tar)
            .env("FIXTURE_CONTRACT", &self.contract)
            .env("RUNNER_TEMP", &self.runner_temp)
            .env("VELNOR_TRANSPORT_SCRATCH", &self.runner_temp)
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
            .env("BASE_REVISION", "3333333333333333333333333333333333333333")
            .env("GITHUB_RUN_ID", RUN_ID.to_string())
            .env("GITHUB_RUN_ATTEMPT", RUN_ATTEMPT.to_string())
            .env("GITHUB_ENV", self.runner_temp.join("github.env"))
            .output()
            .expect("generated acquire shell")
    }
}

#[test]
fn generated_acquire_rejects_same_name_artifact_recreated_by_other_job() {
    let generated = GeneratedScripts::new();
    let fixture = ProvenanceFixture::new(&generated);
    fixture.write_scenario(false);
    let baseline = fixture.run_acquire();
    assert!(
        baseline.status.success(),
        "valid producer-bound artifact fixture failed before replacement check:\n{}\n{}",
        String::from_utf8_lossy(&baseline.stdout),
        String::from_utf8_lossy(&baseline.stderr)
    );
    // The documented artifact REST object has no uploader-job field. Model the
    // service's final same-name replacement with only documented metadata: a
    // new artifact ID, valid digest, and timestamps in the overlapping ordinary
    // job window. The producer and ordinary job records make the missing
    // binding explicit without inventing an API property.
    fixture.write_scenario(true);
    let output = fixture.run_acquire();
    assert!(
        !output.status.success(),
        "generated acquire accepted an artifact recreated by ordinary job {REPLACEMENT_JOB_ID} while producer job is {PRODUCER_JOB_ID}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn scanner_args(base: &Path, head: &Path) -> Vec<String> {
    vec![
        base.to_string_lossy().into_owned(),
        head.to_string_lossy().into_owned(),
        BASE_SHA.to_owned(),
        HEAD_SHA.to_owned(),
        BASE_TREE_SHA.to_owned(),
        HEAD_TREE_SHA.to_owned(),
        "a".repeat(64),
        "b".repeat(64),
    ]
}

fn generated_artifact_name(root: &Path) -> String {
    let workflow = parse_workflow(&read_generated_pr(root));
    let jobs = workflow["jobs"].as_mapping().expect("jobs map");
    for job in jobs.values() {
        let Some(steps) = job["steps"].as_sequence() else {
            continue;
        };
        for step in steps {
            if step["name"].as_str() == Some("Upload candidate generator product") {
                return step["with"]["name"]
                    .as_str()
                    .expect("artifact name")
                    .to_owned();
            }
        }
    }
    std::process::abort()
}

fn write_candidate_archive(path: &Path, artifact_name: &str) {
    let script = r#"import hashlib, json, sys, zipfile
archive, artifact_name = sys.argv[1:]
binary = b"offline candidate bytes\n"
manifest = {
    "schema": "velnor.bootstrap-producer-manifest.v1",
    "profile": "debug",
    "features": [],
    "platform": "linux-amd64",
    "repository": "tailrocks/velnor",
    "run_id": 900,
    "revision": "0123456789abcdef0123456789abcdef01234567",
    "closure": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "binary_sha256": hashlib.sha256(binary).hexdigest(),
}
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
    out.writestr("velnor-workflow", binary)
    out.writestr("candidate-manifest.json", json.dumps(manifest, separators=(",", ":")).encode())
"#;
    let status = Command::new("python3")
        .args([
            "-c",
            script,
            path.to_str().expect("candidate archive"),
            artifact_name,
        ])
        .status()
        .expect("candidate archive fixture");
    assert!(status.success(), "candidate archive fixture failed");
}

fn sha256_file(path: &Path) -> String {
    let mut digest = String::with_capacity(64);
    for byte in Sha256::digest(fs::read(path).expect("hash input")) {
        write!(&mut digest, "{byte:02x}").expect("digest formatting");
    }
    digest
}

fn run_python(script: &str, args: &[String]) -> Output {
    Command::new("python3")
        .arg("-c")
        .arg(script)
        .args(args)
        .output()
        .expect("generated Python fixture")
}

fn extract_python(script: &str, command: &str) -> String {
    let command_start = script.find(command).expect("generated Python command");
    let tail = &script[command_start..];
    let heredoc = tail.find("<<'PY'").expect("generated Python heredoc");
    let body = &tail[heredoc + 6..];
    let body_start = body.find('\n').expect("Python heredoc newline") + 1;
    let body = &body[body_start..];
    let mut end = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        if line.trim() == "PY" {
            end = Some(offset);
            break;
        }
        offset += line.len();
    }
    let body = &body[..end.expect("Python heredoc terminator")];
    dedent(body)
}

fn extract_shell_function(script: &str, name: &str) -> String {
    let start = script
        .find(&format!("{name}() {{"))
        .expect("generated shell function");
    let tail = &script[start..];
    let end = tail
        .find("\n          verify_action_archive()")
        .or_else(|| tail.find("\nverify_action_archive()"))
        .expect("generated shell function terminator");
    dedent(&tail[..end])
}

fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|line| line.get(indent..).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
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
            result.push_str(&script[start..expression_end]);
        } else if after[..end].trim() == "runner.temp" {
            result.push_str(r"\${{ runner.temp }}");
        } else {
            result.push_str("fixture-expression");
        }
        offset = expression_end;
    }
    result.push_str(&script[offset..]);
    result
}

fn read_generated_pr(root: &Path) -> String {
    read_workflow(&root.join("generated"), "ci-pr.yml")
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

fn write_hostile_zip(path: &Path, fault: ZipFault) {
    let script = r#"import stat, struct, sys, zipfile
archive, fault = sys.argv[1:]
with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as out:
    if fault == "symlink":
        info = zipfile.ZipInfo("velnor-workflow")
        info.create_system = 3
        info.external_attr = (stat.S_IFLNK | 0o777) << 16
        out.writestr(info, b"../candidate-source/evil.sh")
        out.writestr("candidate-manifest.json", b"{}")
    elif fault == "hardlink-like-special-mode":
        info = zipfile.ZipInfo("velnor-workflow")
        info.create_system = 3
        # ZIP has no hard-link record; reject a Unix non-regular mode equally.
        info.external_attr = (stat.S_IFIFO | 0o600) << 16
        out.writestr(info, b"hardlink-like")
        out.writestr("candidate-manifest.json", b"{}")
    elif fault == "traversal":
        out.writestr("../candidate-manifest.json", b"{}")
        out.writestr("velnor-workflow", b"fixture")
    elif fault == "duplicate":
        out.writestr("candidate-manifest.json", b"first")
        out.writestr("candidate-manifest.json", b"second")
        out.writestr("velnor-workflow", b"fixture")
    elif fault == "excess-members":
        for index in range(4097):
            out.writestr(f"member-{index}", b"")
    elif fault == "oversized-declared-stream":
        out.writestr("candidate-manifest.json", b"{}")
        out.writestr("velnor-workflow", b"fixture")
if fault == "oversized-declared-stream":
    data = bytearray(open(archive, "rb").read())
    central = data.find(b"PK\x01\x02")
    if central < 0:
        raise SystemExit("central directory missing")
    struct.pack_into("<I", data, central + 24, 536870913)
    open(archive, "wb").write(data)
"#;
    let status = Command::new("python3")
        .args(["-c", script, path.to_str().expect("ZIP path"), fault.name()])
        .status()
        .expect("hostile ZIP fixture");
    assert!(status.success(), "hostile ZIP fixture failed: {fault:?}");
}

fn write_hostile_tar(path: &Path, fault: TarFault) {
    let script = r#"import io, sys, tarfile
archive, fault = sys.argv[1:]
with tarfile.open(archive, "w:") as out:
    def file_entry(name, data=b"fixture"):
        info = tarfile.TarInfo(name)
        info.mode = 0o644
        info.size = len(data)
        out.addfile(info, io.BytesIO(data))
    if fault == "symlink":
        info = tarfile.TarInfo("candidate-source/evil.sh")
        info.type = tarfile.SYMTYPE
        info.linkname = "../../outside"
        info.mode = 0o777
        out.addfile(info)
    elif fault == "hardlink":
        info = tarfile.TarInfo("candidate-source/evil.sh")
        info.type = tarfile.LNKTYPE
        info.linkname = "candidate-source/real.sh"
        info.mode = 0o644
        out.addfile(info)
    elif fault == "traversal":
        file_entry("../candidate-source/evil.sh")
    elif fault == "duplicate":
        file_entry("candidate-source/real.sh", b"one")
        file_entry("candidate-source/real.sh", b"two")
    elif fault == "excess-members":
        for index in range(4097):
            info = tarfile.TarInfo(f"member-{index}")
            info.type = tarfile.DIRTYPE
            info.mode = 0o755
            out.addfile(info)
    elif fault == "oversized-declared-stream":
        info = tarfile.TarInfo("oversized")
        info.type = tarfile.DIRTYPE
        info.mode = 0o755
        info.size = 536870913
        out.addfile(info)
"#;
    let status = Command::new("python3")
        .args(["-c", script, path.to_str().expect("TAR path"), fault.name()])
        .status()
        .expect("hostile TAR fixture");
    assert!(status.success(), "hostile TAR fixture failed: {fault:?}");
}

fn write_workflow_tar(path: &Path, contract: &Path) {
    let script = r#"import io, sys, tarfile
archive, contract = sys.argv[1:]
with tarfile.open(archive, "w:") as out:
    members = [
        (".github/workflows/ci-pr.yml", open(contract, "rb").read()),
        (".github/actions/setup-velnor-workflow/action.yml", b"name: setup\nruns:\n  using: composite\n  steps: []\n"),
    ]
    members.extend(
        (name, b"name: fixture\non: workflow_call\njobs: {}\n")
        for name in (
            ".github/workflows/ci-unit-bun.yml",
            ".github/workflows/ci-unit-docker.yml",
            ".github/workflows/ci-unit-docs.yml",
            ".github/workflows/ci-unit-opentofu.yml",
            ".github/workflows/ci-unit-rust.yml",
        )
    )
    for name, contents in members:
        info = tarfile.TarInfo(name)
        info.mode = 0o644
        info.size = len(contents)
        out.addfile(info, io.BytesIO(contents))
"#;
    let status = Command::new("python3")
        .args([
            "-c",
            script,
            path.to_str().expect("workflow TAR"),
            contract.to_str().expect("contract"),
        ])
        .status()
        .expect("workflow TAR fixture");
    assert!(status.success(), "workflow TAR fixture failed");
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("fixture command");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fixture permissions");
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

const CURL_FIXTURE: &str = r#"#!/usr/bin/env python3
import sys
sys.stdout.buffer.write(b"offline curl payload")
"#;

const RACE_RM_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, subprocess, sys
subprocess.run(["/bin/rm", *sys.argv[1:]], check=True)
target = os.environ.get("RACE_TARGET")
once = os.environ.get("RACE_ONCE")
if target and once and not os.path.exists(once):
    try:
        fd = os.open(once, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        raise SystemExit(0)
    os.close(fd)
    for argument in sys.argv[1:]:
        if argument.endswith(".partial"):
            os.symlink(target, argument)
            break
"#;

const GH_PROVENANCE_FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, sys
scenario = json.load(open(os.environ["FIXTURE_SCENARIO"]))
args = sys.argv[1:]
url = next((arg for arg in args if "repos/" in arg), "")
if url.endswith("/actions/workflows/ci-pr.yml"):
    print(json.dumps({"path": ".github/workflows/ci-pr.yml", "id": 700}))
elif "/commits/" in url:
    sha = url.rsplit("/commits/", 1)[1].split("?", 1)[0]
    tree = scenario["api_base_tree"] if sha == os.environ["BASE_SHA"] else scenario["api_head_tree"]
    print(json.dumps({"sha": sha, "commit": {"tree": {"sha": tree}}}))
elif "/git/trees/" in url:
    tree = url.rsplit("/git/trees/", 1)[1].split("?", 1)[0]
    entries = scenario["api_base_entries"] if tree == scenario["api_base_tree"] else scenario["api_head_entries"]
    print(json.dumps({"sha": tree, "truncated": False, "tree": entries}))
elif url.rstrip("/") == "repos/" + os.environ["GITHUB_REPOSITORY"]:
    print(json.dumps({"id": 42, "full_name": os.environ["GITHUB_REPOSITORY"]}))
elif "/actions/runs/" in url and "/jobs" in url:
    print(json.dumps([scenario["jobs"]]))
elif "/actions/runs/" in url and "/artifacts" in url:
    print(json.dumps([scenario["artifacts"]]))
elif "/runs?" in url:
    print(json.dumps([scenario["runs"]]))
else:
    print("{}")
"#;

const CURL_PROVENANCE_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
url = next((arg for arg in args if arg.startswith("http")), "")
payload = open(os.environ["FIXTURE_ACTION_ARCHIVE"], "rb").read() if "/tarball/" in url else open(os.environ["FIXTURE_ARCHIVE"], "rb").read()
sys.stdout.buffer.write(payload)
"#;

const GIT_PROVENANCE_FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
commands = {"init", "fetch", "show", "archive", "ls-tree", "cat-file", "rev-parse"}
index = next((index for index, value in enumerate(args) if value in commands), len(args))
command = args[index] if index < len(args) else ""
rest = args[index + 1:]
scenario = json.load(open(os.environ["FIXTURE_SCENARIO"]))
if command == "init":
    os.makedirs(args[-1], exist_ok=True)
elif command == "fetch":
    pass
elif command == "show":
    target = rest[-1]
    if "--format=%T" in rest:
        print(scenario["object_base_tree"] if target == os.environ["BASE_SHA"] else scenario["object_head_tree"])
    elif target.endswith(":.github/workflows/ci-pr.yml"):
        sys.stdout.write(open(os.environ["FIXTURE_CONTRACT"]).read())
elif command == "ls-tree":
    print("100644 blob 4444444444444444444444444444444444444444\\tCargo.toml")
elif command == "archive":
    sys.stdout.buffer.write(open(os.environ["FIXTURE_WORKFLOW_TAR"], "rb").read())
"#;

const SHA256SUM_PROVENANCE_FIXTURE: &str = r#"#!/usr/bin/env python3
import hashlib, os, sys
if len(sys.argv) == 1:
    print(hashlib.sha256(sys.stdin.buffer.read()).hexdigest(), "-")
    raise SystemExit(0)
for name in sys.argv[1:]:
    fixed = {
        "action-checkout.tar.gz": "cebb825b471e77ce4dd7f1f37ccdfb3ae5b68e78f6f6d2f6e2521c3cfce27e72",
        "action-upload.tar.gz": "69baddda1bd8d80441109e489128f1b6944fddd92586ad1855275121beeae897",
        "action-download.tar.gz": "498e5a207a6e181257cfe7bb4d8e273e758ea729ad10f0eff617505006bd1d77",
    }
    value = next((digest for suffix, digest in fixed.items() if name.endswith(suffix)), None)
    if value is None and os.path.basename(name) == "candidate-closure":
        value = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    if value is None:
        value = hashlib.sha256(open(name, "rb").read()).hexdigest()
    print(value, name)
"#;

const STAT_PROVENANCE_FIXTURE: &str = r"#!/usr/bin/env python3
import os, sys
print(os.path.getsize(sys.argv[-1]))
";

const DU_PROVENANCE_FIXTURE: &str = r#"#!/usr/bin/env python3
import os, sys
print("0", sys.argv[-1])
"#;
