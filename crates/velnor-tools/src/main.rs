mod audit_ci;
mod lane_compare;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use clap::{ArgAction, Args, Parser, Subcommand};
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const DEFAULT_FIXTURE_REPO: &str = "donbeave/velnor-actions-fixture";
const DEFAULT_FIXTURE_REF: &str = "main";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/actions/runner/releases/latest";
const LATEST_RELEASE_REDIRECT_URL: &str = "https://github.com/actions/runner/releases/latest";
const GITHUB_HTTP_TIMEOUT_SECONDS: u64 = 20;

#[derive(Debug, Parser)]
#[command(name = "velnor-tools")]
#[command(about = "Rust repository automation for Velnor")]
struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Check that the documented actions/runner reference matches upstream.
    CheckRunnerReference,
    /// Audit the public fixture repository feature surface.
    FixtureAudit(FixtureAuditArgs),
    /// Run non-mutating fixture readiness checks.
    FixtureReadiness(FixtureReadinessArgs),
    /// Write non-mutating fixture readiness report.
    FixtureReport(FixtureReportArgs),
    /// Inspect latest or selected public fixture workflow run status.
    FixtureStatus(FixtureStatusArgs),
    /// Validate and print fixture smoke execution plan without mutating GitHub or Docker.
    FixtureSmokePlan(FixtureSmokePlanArgs),
    /// Validate and print target smoke execution plan without mutating GitHub or Docker.
    TargetSmokePlan(TargetSmokePlanArgs),
    /// Validate and print live host doctor plan without mutating Docker or GitHub.
    LiveHostDoctorPlan(LiveHostDoctorPlanArgs),
    /// Audit current target repository workflow surface.
    TargetAudit(TargetAuditArgs),
    /// Run full local target verification gate.
    TargetVerify(TargetVerifyArgs),
    /// Commit repository changes using the Rust automation helper.
    Commit(CommitArgs),
    /// Dispatch a GitHub Actions workflow and print the new run id.
    WorkflowDispatch(WorkflowDispatchArgs),
    /// Write Velnor live evidence for a GitHub Actions run.
    WriteLiveEvidence(WriteLiveEvidenceArgs),
    /// Run the full fixture smoke sequence against a Velnor JIT daemon.
    FixtureSmoke(FixtureSmokeArgs),
    /// Run the full target smoke sequence against a Velnor JIT daemon.
    TargetSmoke(TargetSmokeArgs),
    /// Check fixture lane pattern: matrix.config has both lanes, runs-on uses matrix variable, steps are lane-agnostic.
    CheckFixtureLanes(CheckFixtureLanesArgs),
    /// Audit a repository or estate against the Velnor CI contract.
    AuditCi(audit_ci::AuditCiArgs),
    /// Compare GitHub and Velnor lanes (promoted alias of lane-compare).
    Compare(lane_compare::LaneCompareArgs),
    /// Diff the GitHub-hosted and Velnor lanes of one run via the GitHub API (equal-or-better gate).
    LaneCompare(lane_compare::LaneCompareArgs),
}

#[derive(Debug, Args)]
struct FixtureAuditArgs {
    /// GitHub repository slug to audit.
    #[arg(long, default_value = DEFAULT_FIXTURE_REPO)]
    repo: String,
    /// Git ref to audit.
    #[arg(long, default_value = DEFAULT_FIXTURE_REF)]
    git_ref: String,
    /// Audit a local fixture checkout/root instead of GitHub.
    #[arg(long)]
    fixture_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct FixtureReadinessArgs {
    /// Inspect fixture workflow status.
    #[arg(
        long,
        env = "VELNOR_FIXTURE_READINESS_CHECK_STATUS",
        default_value_t = true,
        action = ArgAction::Set
    )]
    check_status: bool,
    /// Audit fixture feature surface.
    #[arg(
        long,
        env = "VELNOR_FIXTURE_READINESS_CHECK_AUDIT",
        default_value_t = true,
        action = ArgAction::Set
    )]
    check_audit: bool,
    /// Run target verifier and Rust test suite.
    #[arg(
        long,
        env = "VELNOR_FIXTURE_READINESS_RUN_LOCAL_TESTS",
        default_value_t = false,
        action = ArgAction::Set
    )]
    run_local_tests: bool,
    /// Fixture status command.
    #[arg(
        long,
        env = "VELNOR_FIXTURE_STATUS_SCRIPT",
        default_value = "scripts/fixture_status.sh"
    )]
    fixture_status_script: String,
    /// Fixture audit command.
    #[arg(
        long,
        env = "VELNOR_FIXTURE_AUDIT_SCRIPT",
        default_value = "cargo run -q -p velnor-tools -- fixture-audit"
    )]
    fixture_audit_script: String,
    /// Live host doctor command.
    #[arg(
        long,
        env = "VELNOR_LIVE_HOST_DOCTOR_SCRIPT",
        default_value = "scripts/live_host_doctor.sh"
    )]
    live_host_doctor_script: String,
}

#[derive(Debug, Args)]
struct FixtureReportArgs {
    /// Directory for the default report path.
    #[arg(long, env = "VELNOR_FIXTURE_REPORT_DIR")]
    report_dir: Option<PathBuf>,
    /// Exact report path to write.
    #[arg(long, env = "VELNOR_FIXTURE_REPORT_PATH")]
    report_path: Option<PathBuf>,
    /// Fixture status command.
    #[arg(
        long,
        env = "VELNOR_FIXTURE_STATUS_SCRIPT",
        default_value = "scripts/fixture_status.sh"
    )]
    fixture_status_script: String,
    /// Fixture audit command.
    #[arg(
        long,
        env = "VELNOR_FIXTURE_AUDIT_SCRIPT",
        default_value = "cargo run -q -p velnor-tools -- fixture-audit"
    )]
    fixture_audit_script: String,
    /// Live host doctor command.
    #[arg(
        long,
        env = "VELNOR_LIVE_HOST_DOCTOR_SCRIPT",
        default_value = "scripts/live_host_doctor.sh"
    )]
    live_host_doctor_script: String,
}

#[derive(Debug, Args)]
struct FixtureStatusArgs {
    /// GitHub repository slug to inspect.
    #[arg(long, env = "VELNOR_FIXTURE_REPO", default_value = DEFAULT_FIXTURE_REPO)]
    repo: String,
    /// Workflow file name to inspect.
    #[arg(long, env = "VELNOR_FIXTURE_WORKFLOW", default_value = "compat.yml")]
    workflow: String,
    /// GitHub Actions run id. Defaults to latest run for the workflow.
    #[arg(long, env = "VELNOR_FIXTURE_RUN_ID")]
    run_id: Option<u64>,
}

#[derive(Debug, Args)]
struct FixtureSmokePlanArgs {
    /// GitHub repository slug for the fixture.
    #[arg(long, env = "VELNOR_FIXTURE_REPO", default_value = DEFAULT_FIXTURE_REPO)]
    repo: String,
    /// GitHub repository URL for runner setup.
    #[arg(long, env = "VELNOR_FIXTURE_URL")]
    url: Option<String>,
    /// Runner name.
    #[arg(long, env = "VELNOR_RUNNER_NAME", default_value = "velnor-target-mvp")]
    runner_name: String,
    /// Runner label.
    #[arg(long, env = "VELNOR_RUNNER_LABEL", default_value = "velnor-target-mvp")]
    runner_label: String,
    /// Velnor work directory.
    #[arg(long, env = "VELNOR_WORK_DIR")]
    work_dir: Option<PathBuf>,
    /// Docker-daemon-visible work directory.
    #[arg(long, env = "VELNOR_DOCKER_HOST_WORK_DIR")]
    docker_host_work_dir: Option<PathBuf>,
    /// Require Docker socket for fixture jobs.
    #[arg(long, env = "VELNOR_REQUIRE_DOCKER_SOCKET", default_value_t = true, action = ArgAction::Set)]
    require_docker_socket: bool,
    /// Idle timeout seconds.
    #[arg(long, env = "VELNOR_IDLE_TIMEOUT_SECONDS", default_value_t = 900)]
    idle_timeout_seconds: u64,
    /// Allow other online matching runners.
    #[arg(long, env = "VELNOR_ALLOW_OTHER_MATCHING_RUNNERS", default_value_t = false, action = ArgAction::Set)]
    allow_other_matching_runners: bool,
    /// Fixture workflow file.
    #[arg(long, env = "VELNOR_FIXTURE_WORKFLOW", default_value = "compat.yml")]
    workflow: String,
    /// Whether to dispatch a fresh fixture workflow. Defaults from run id.
    #[arg(long, env = "VELNOR_FIXTURE_DISPATCH", action = ArgAction::Set)]
    dispatch: Option<bool>,
    /// Fixture workflow ref.
    #[arg(long, env = "VELNOR_FIXTURE_REF", default_value = "")]
    fixture_ref: String,
    /// Fixture workflow inputs as comma-separated key=value pairs.
    #[arg(long, env = "VELNOR_FIXTURE_INPUTS", default_value = "")]
    fixture_inputs: String,
    /// Existing fixture run id.
    #[arg(long, env = "VELNOR_FIXTURE_RUN_ID")]
    run_id: Option<u64>,
    /// Number of fixture jobs/slots.
    #[arg(long, env = "VELNOR_FIXTURE_JOB_COUNT", default_value_t = 2)]
    job_count: u64,
    /// Cleanup fixture runner on exit.
    #[arg(long, env = "VELNOR_FIXTURE_CLEANUP_RUNNER", default_value_t = true, action = ArgAction::Set)]
    cleanup_runner: bool,
    /// Job-message dump directory. Empty disables dumps.
    #[arg(long, env = "VELNOR_DUMP_JOB_MESSAGES")]
    dump_job_messages: Option<String>,
}

#[derive(Debug, Args)]
struct TargetSmokePlanArgs {
    /// GitHub repository slug for the target.
    #[arg(long, env = "VELNOR_TARGET_REPO")]
    repo: String,
    /// GitHub repository URL for runner setup.
    #[arg(long, env = "VELNOR_TARGET_URL")]
    url: Option<String>,
    /// Runner name.
    #[arg(long, env = "VELNOR_RUNNER_NAME", default_value = "velnor-target-mvp")]
    runner_name: String,
    /// Velnor work directory.
    #[arg(long, env = "VELNOR_WORK_DIR")]
    work_dir: Option<PathBuf>,
    /// Docker-daemon-visible work directory.
    #[arg(long, env = "VELNOR_DOCKER_HOST_WORK_DIR")]
    docker_host_work_dir: Option<PathBuf>,
    /// Require Docker socket for target jobs.
    #[arg(long, env = "VELNOR_REQUIRE_DOCKER_SOCKET", default_value_t = true, action = ArgAction::Set)]
    require_docker_socket: bool,
    /// Idle timeout seconds.
    #[arg(long, env = "VELNOR_IDLE_TIMEOUT_SECONDS", default_value_t = 900)]
    idle_timeout_seconds: u64,
    /// Allow other online matching runners.
    #[arg(long, env = "VELNOR_ALLOW_OTHER_MATCHING_RUNNERS", default_value_t = false, action = ArgAction::Set)]
    allow_other_matching_runners: bool,
    /// Cleanup target runner on exit.
    #[arg(long, env = "VELNOR_TARGET_CLEANUP_RUNNER", default_value_t = false, action = ArgAction::Set)]
    cleanup_runner: bool,
    /// Job-message dump directory. Empty disables dumps.
    #[arg(long, env = "VELNOR_DUMP_JOB_MESSAGES")]
    dump_job_messages: Option<String>,
    /// Number of target jobs/slots.
    #[arg(long, env = "VELNOR_TARGET_JOB_COUNT", default_value_t = 1)]
    job_count: u64,
    /// Target workflow file. Empty means consume existing queued run.
    #[arg(long, env = "VELNOR_TARGET_WORKFLOW", default_value = "")]
    workflow: String,
    /// Target workflow ref.
    #[arg(long, env = "VELNOR_TARGET_REF", default_value = "")]
    target_ref: String,
    /// Target workflow inputs as comma-separated key=value pairs.
    #[arg(long, env = "VELNOR_TARGET_INPUTS", default_value = "")]
    target_inputs: String,
    /// Existing target run id.
    #[arg(long, env = "VELNOR_TARGET_RUN_ID")]
    run_id: Option<u64>,
    /// Watch target run after Velnor exits.
    #[arg(long, env = "VELNOR_TARGET_WATCH_RUN", default_value_t = false, action = ArgAction::Set)]
    watch_run: bool,
    /// Human target label for logs/evidence.
    #[arg(long, env = "VELNOR_TARGET_LABEL", default_value = "target")]
    target_label: String,
    /// Claim ARM64 target label.
    #[arg(long, env = "VELNOR_TARGET_MVP_ARM_LABEL", default_value_t = false, action = ArgAction::Set)]
    target_mvp_arm_label: bool,
    /// Manual confirmation for real target repositories.
    #[arg(long, env = "VELNOR_REAL_TARGET_MANUAL_CONFIRM", default_value_t = false, action = ArgAction::Set)]
    real_target_manual_confirm: bool,
}

#[derive(Debug, Args)]
struct LiveHostDoctorPlanArgs {
    /// Velnor work directory.
    #[arg(long, env = "VELNOR_WORK_DIR")]
    work_dir: Option<PathBuf>,
    /// Docker-daemon-visible work directory.
    #[arg(long, env = "VELNOR_DOCKER_HOST_WORK_DIR")]
    docker_host_work_dir: Option<PathBuf>,
    /// Require Docker socket for target jobs.
    #[arg(long, env = "VELNOR_REQUIRE_DOCKER_SOCKET", default_value_t = true, action = ArgAction::Set)]
    require_docker_socket: bool,
    /// Check target MVP runner config after preflight.
    #[arg(long, env = "VELNOR_CHECK_TARGET_MVP_CONFIG", default_value_t = false, action = ArgAction::Set)]
    check_target_mvp_config: bool,
    /// Run target verifier before preflight.
    #[arg(long, env = "VELNOR_RUN_TARGET_VERIFY", default_value_t = false, action = ArgAction::Set)]
    run_target_verify: bool,
    /// Claim ARM64 target label.
    #[arg(long, env = "VELNOR_TARGET_MVP_ARM_LABEL", default_value_t = false, action = ArgAction::Set)]
    target_mvp_arm_label: bool,
    /// Docker host setting to report.
    #[arg(long, env = "DOCKER_HOST")]
    docker_host: Option<String>,
    /// Host OS label to report. Defaults to Rust compile-time OS.
    #[arg(long, env = "VELNOR_LIVE_HOST_OS")]
    host_os: Option<String>,
    /// Host architecture to validate. Defaults to Rust compile-time arch.
    #[arg(long, env = "VELNOR_LIVE_HOST_ARCH")]
    host_arch: Option<String>,
}

#[derive(Debug, Args)]
struct TargetAuditArgs {
    /// Check current Phase 0 target workflow surface.
    #[arg(long, conflicts_with = "self_test")]
    check_target_mvp: bool,
    /// Run target audit self-tests, then validate supplied target roots exist.
    #[arg(long, conflicts_with = "check_target_mvp")]
    self_test: bool,
    /// Target repository roots to audit.
    #[arg(required = true)]
    roots: Vec<PathBuf>,
}

#[derive(Debug, Args)]
struct TargetVerifyArgs {
    /// Jackin checkout root.
    #[arg(long, env = "VELNOR_JACKIN_ROOT", default_value = "/tmp/velnor-jackin")]
    jackin_root: PathBuf,
    /// ChainArgos checkout root.
    #[arg(
        long,
        env = "VELNOR_CHAINARGOS_ROOT",
        default_value = "/tmp/velnor-chainargos"
    )]
    chainargos_root: PathBuf,
    /// Skip target checkout freshness checks.
    #[arg(long, env = "VELNOR_SKIP_TARGET_FRESHNESS_CHECK", default_value_t = false, action = ArgAction::Set)]
    skip_target_freshness_check: bool,
}

#[derive(Debug, Args)]
struct CommitArgs {
    /// Commit message.
    #[arg(short, long)]
    message: String,
    /// Paths to stage. Defaults to all changes.
    #[arg(default_value = ".")]
    paths: Vec<String>,
    /// Push after committing.
    #[arg(long)]
    push: bool,
}

#[derive(Debug, Args)]
struct WorkflowDispatchArgs {
    /// GitHub repository slug.
    #[arg(long)]
    repo: String,
    /// Workflow file name.
    #[arg(long)]
    workflow: String,
    /// Git ref (branch, tag, or SHA). Empty uses default branch.
    #[arg(long, default_value = "")]
    ref_: String,
    /// Workflow inputs as comma-separated key=value pairs.
    #[arg(long, default_value = "")]
    inputs: String,
    /// Maximum poll attempts after dispatch.
    #[arg(long, default_value_t = 30)]
    max_attempts: u32,
    /// Seconds between poll attempts.
    #[arg(long, default_value_t = 2)]
    poll_interval_seconds: u64,
}

#[derive(Debug, Args)]
struct WriteLiveEvidenceArgs {
    /// Evidence phase label (e.g. after-velnor, completed, failed-before-completion).
    phase: String,
    /// GitHub repository slug.
    #[arg(long, env = "VELNOR_FIXTURE_REPO")]
    repo: String,
    /// GitHub Actions run id.
    #[arg(long, env = "VELNOR_FIXTURE_RUN_ID")]
    run_id: u64,
    /// Runner name.
    #[arg(long, env = "VELNOR_RUNNER_NAME", default_value = "velnor-target-mvp")]
    runner_name: String,
    /// Velnor work directory.
    #[arg(long, env = "VELNOR_WORK_DIR")]
    work_dir: Option<PathBuf>,
    /// Docker-daemon-visible work directory.
    #[arg(long, env = "VELNOR_DOCKER_HOST_WORK_DIR", default_value = "")]
    docker_host_work_dir: String,
    /// Require Docker socket.
    #[arg(long, env = "VELNOR_REQUIRE_DOCKER_SOCKET", default_value_t = true, action = ArgAction::Set)]
    require_docker_socket: bool,
    /// Job message dump directory.
    #[arg(long, env = "VELNOR_DUMP_JOB_MESSAGES", default_value = "")]
    dump_job_messages: String,
    /// Evidence title (e.g. Fixture, Target).
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_TITLE", default_value = "Target")]
    title: String,
    /// Workflow used for the run.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_WORKFLOW", default_value = "")]
    workflow: String,
    /// Ref used for the run.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_REF", default_value = "")]
    ref_: String,
    /// Inputs used for the run.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_INPUTS", default_value = "")]
    inputs: String,
    /// Number of jobs requested.
    #[arg(long, env = "VELNOR_FIXTURE_JOB_COUNT", default_value_t = 1)]
    job_count: u64,
    /// Evidence output directory.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_DIR")]
    evidence_dir: Option<PathBuf>,
    /// Maximum log lines to include.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_LOG_LINES", default_value_t = 80)]
    log_lines: u64,
    /// Maximum local storage entries to list.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES", default_value_t = 80)]
    local_entries: u64,
    /// Extra metadata lines (each printed as a list item).
    #[arg(long)]
    extra_metadata: Vec<String>,
}

#[derive(Debug, Args)]
struct FixtureSmokeArgs {
    #[command(flatten)]
    plan: FixtureSmokePlanArgs,
    /// Evidence output directory.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_DIR")]
    evidence_dir: Option<PathBuf>,
    /// Maximum log lines in evidence.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_LOG_LINES", default_value_t = 80)]
    log_lines: u64,
    /// Maximum local storage entries in evidence.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES", default_value_t = 80)]
    local_entries: u64,
}

#[derive(Debug, Args)]
struct TargetSmokeArgs {
    #[command(flatten)]
    plan: TargetSmokePlanArgs,
    /// Evidence output directory.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_DIR")]
    evidence_dir: Option<PathBuf>,
    /// Maximum log lines in evidence.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_LOG_LINES", default_value_t = 80)]
    log_lines: u64,
    /// Maximum local storage entries in evidence.
    #[arg(long, env = "VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES", default_value_t = 80)]
    local_entries: u64,
}

#[derive(Debug, Args)]
struct CheckFixtureLanesArgs {
    /// GitHub repository slug to verify.
    #[arg(long, default_value = DEFAULT_FIXTURE_REPO)]
    repo: String,
    /// Git ref to verify.
    #[arg(long, default_value = DEFAULT_FIXTURE_REF)]
    git_ref: String,
    /// Local fixture root instead of GitHub.
    #[arg(long)]
    fixture_root: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = repo_root()?;
    match cli.command {
        CommandKind::CheckRunnerReference => check_runner_reference(&root).await,
        CommandKind::FixtureAudit(args) => fixture_audit(args).await,
        CommandKind::FixtureReadiness(args) => fixture_readiness(args),
        CommandKind::FixtureReport(args) => fixture_report(&root, args),
        CommandKind::FixtureStatus(args) => fixture_status(args),
        CommandKind::FixtureSmokePlan(args) => fixture_smoke_plan(&root, args),
        CommandKind::TargetSmokePlan(args) => target_smoke_plan(&root, args),
        CommandKind::LiveHostDoctorPlan(args) => live_host_doctor_plan(&root, args),
        CommandKind::TargetAudit(args) => target_audit(args),
        CommandKind::TargetVerify(args) => target_verify(&root, args).await,
        CommandKind::Commit(args) => commit(&root, args),
        CommandKind::WorkflowDispatch(args) => workflow_dispatch(args),
        CommandKind::WriteLiveEvidence(args) => write_live_evidence_cmd(&root, args),
        CommandKind::FixtureSmoke(args) => fixture_smoke(&root, args),
        CommandKind::TargetSmoke(args) => target_smoke(&root, args),
        CommandKind::CheckFixtureLanes(args) => check_fixture_lanes(args).await,
        CommandKind::AuditCi(args) => audit_ci::audit_ci(args),
        CommandKind::Compare(args) => lane_compare::lane_compare(&root, args),
        CommandKind::LaneCompare(args) => lane_compare::lane_compare(&root, args),
    }
}

fn repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("resolve git repository root")?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

async fn check_runner_reference(root: &Path) -> Result<()> {
    let reference = pinned_reference(root)?;
    let protocol_version = pinned_protocol_version(root)?;
    let latest = latest_runner_release().await?;

    if protocol_version != reference.trim_start_matches('v') {
        bail!(
            "actions/runner protocol version drift: docs pin {reference}, protocol advertises {protocol_version}"
        );
    }
    if reference != latest {
        bail!(
            "actions/runner reference drift: pinned {reference}, latest {latest}\nRefresh docs/reference/latest-runner-v2-refresh-2026-06-01.md and re-audit V2 source anchors."
        );
    }

    println!("actions/runner reference current: {reference}");
    Ok(())
}

fn pinned_reference(root: &Path) -> Result<String> {
    let path = root.join("docs/reference/latest-runner-v2-refresh-2026-06-01.md");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let regex = Regex::new(r"latest release checked:\s*`(v[0-9]+\.[0-9]+\.[0-9]+)`")?;
    let captures = regex.captures(&text).ok_or_else(|| {
        anyhow::anyhow!("could not find pinned runner version in {}", path.display())
    })?;
    Ok(captures[1].to_string())
}

fn pinned_protocol_version(root: &Path) -> Result<String> {
    let path = root.join("crates/velnor-runner/src/protocol.rs");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let regex = Regex::new(r#"RUNNER_VERSION:\s*&str\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)""#)?;
    let captures = regex
        .captures(&text)
        .ok_or_else(|| anyhow::anyhow!("could not find RUNNER_VERSION in {}", path.display()))?;
    let version = captures[1].to_string();
    let expected_agent =
        format!(r#"RUNNER_USER_AGENT: &str = "actions-runner/{version} (velnor)""#);
    if !text.contains(&expected_agent) {
        bail!(
            "RUNNER_USER_AGENT in {} does not match RUNNER_VERSION {version}",
            path.display()
        );
    }
    Ok(version)
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

async fn latest_runner_release() -> Result<String> {
    let client = github_http_client()?;
    let response = client
        .get(LATEST_RELEASE_URL)
        .headers(github_headers("velnor-runner-reference-check")?)
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => {
            let release = response
                .json::<GitHubRelease>()
                .await
                .context("parse latest actions/runner release response")?;
            if !release.tag_name.starts_with('v') {
                bail!("GitHub latest release response did not include a tag_name");
            }
            Ok(release.tag_name)
        }
        Ok(response) => {
            latest_release_from_redirect_or_git(format!("API {}", response.status())).await
        }
        Err(error) => latest_release_from_redirect_or_git(format!("API {error}")).await,
    }
}

async fn latest_release_from_redirect_or_git(reason: String) -> Result<String> {
    let client = github_http_client()?;
    let response = client
        .get(LATEST_RELEASE_REDIRECT_URL)
        .header(
            USER_AGENT,
            HeaderValue::from_static("velnor-runner-reference-check"),
        )
        .send()
        .await
        .with_context(|| format!("GitHub release redirect lookup failed: {reason}"));
    let response = match response {
        Ok(response) => response,
        Err(error) => return latest_release_from_git_tags(format!("{error:#}")),
    };
    let final_url = response.url().to_string();
    let regex = Regex::new(r"/releases/tag/(v[0-9]+\.[0-9]+\.[0-9]+)$")?;
    let captures = regex.captures(&final_url).ok_or_else(|| {
        anyhow::anyhow!("GitHub release lookup failed: {reason}, redirect URL {final_url}")
    })?;
    Ok(captures[1].to_string())
}

fn latest_release_from_git_tags(reason: String) -> Result<String> {
    let output = Command::new("git")
        .args([
            "ls-remote",
            "--tags",
            "--refs",
            "https://github.com/actions/runner.git",
            "refs/tags/v*",
        ])
        .output()
        .with_context(|| {
            format!("GitHub release lookup failed and git tag fallback failed: {reason}")
        })?;
    if !output.status.success() {
        bail!(
            "GitHub release lookup failed and git tag fallback failed: {reason}; git stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    latest_semver_tag_from_ls_remote(&String::from_utf8_lossy(&output.stdout))
        .with_context(|| format!("GitHub release lookup failed: {reason}"))
}

fn latest_semver_tag_from_ls_remote(output: &str) -> Result<String> {
    let regex = Regex::new(r"refs/tags/v([0-9]+)\.([0-9]+)\.([0-9]+)$")?;
    let mut latest: Option<(u64, u64, u64, String)> = None;
    for line in output.lines() {
        let Some(captures) = regex.captures(line) else {
            continue;
        };
        let major = captures[1].parse::<u64>()?;
        let minor = captures[2].parse::<u64>()?;
        let patch = captures[3].parse::<u64>()?;
        let tag = format!("v{major}.{minor}.{patch}");
        if latest
            .as_ref()
            .is_none_or(|(old_major, old_minor, old_patch, _)| {
                (major, minor, patch) > (*old_major, *old_minor, *old_patch)
            })
        {
            latest = Some((major, minor, patch, tag));
        }
    }
    latest
        .map(|(_, _, _, tag)| tag)
        .ok_or_else(|| anyhow::anyhow!("no vX.Y.Z actions/runner tags found"))
}

async fn fixture_audit(args: FixtureAuditArgs) -> Result<()> {
    let mut failures = Vec::new();
    for (path, snippets) in fixture_required_snippets() {
        let content = match read_fixture_file(&args, path).await {
            Ok(content) => content,
            Err(error) => {
                failures.push(format!("{path}: missing or unreadable: {error:#}"));
                continue;
            }
        };

        for (label, snippet) in snippets {
            if !content.contains(snippet) {
                failures.push(format!("{path}: missing {label}: {snippet}"));
            }
        }
    }

    if !failures.is_empty() {
        eprintln!("fixture audit failed:");
        for failure in failures {
            eprintln!("  - {failure}");
        }
        std::process::exit(1);
    }

    let source = args
        .fixture_root
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("{}@{}", args.repo, args.git_ref));
    println!("fixture audit passed for {source}");
    println!("checked {} files", fixture_required_snippets().len());
    Ok(())
}

async fn read_fixture_file(args: &FixtureAuditArgs, path: &str) -> Result<String> {
    if let Some(root) = &args.fixture_root {
        let path = root.join(path);
        return fs::read_to_string(&path).with_context(|| format!("read {}", path.display()));
    }
    fetch_github_file(&args.repo, &args.git_ref, path).await
}

#[derive(Debug, Deserialize)]
struct GitHubContentFile {
    #[serde(rename = "type")]
    kind: String,
    content: String,
}

async fn fetch_github_file(repo: &str, git_ref: &str, path: &str) -> Result<String> {
    // Use `gh` CLI to bypass reqwest TLS-fingerprint throttling from GitHub's infrastructure.
    let output = tokio::task::spawn_blocking({
        let repo = repo.to_string();
        let git_ref = git_ref.to_string();
        let path = path.to_string();
        move || {
            Command::new("gh")
                .args([
                    "api",
                    &format!("repos/{repo}/contents/{path}?ref={git_ref}"),
                ])
                .output()
        }
    })
    .await
    .context("spawn_blocking gh api")?
    .with_context(|| format!("fetch {repo}@{git_ref}:{path}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("fetch {repo}@{git_ref}:{path}: {stderr}");
    }
    let payload: GitHubContentFile = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parse GitHub content response for {path}"))?;
    if payload.kind != "file" {
        bail!("{path} is not a file in {repo}@{git_ref}");
    }
    let content = payload.content.replace('\n', "");
    let bytes = general_purpose::STANDARD
        .decode(content)
        .with_context(|| format!("decode GitHub file content for {path}"))?;
    String::from_utf8(bytes).with_context(|| format!("decode UTF-8 content for {path}"))
}

fn github_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(GITHUB_HTTP_TIMEOUT_SECONDS))
        .build()
        .context("build GitHub HTTP client")
}

fn github_headers(user_agent: &'static str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(user_agent));
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static("2022-11-28"),
    );
    if let Ok(token) = env::var("GITHUB_TOKEN") {
        if !token.trim().is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))
                    .context("build GitHub authorization header")?,
            );
        }
    }
    Ok(headers)
}

fn fixture_required_snippets() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            ".github/workflows/compat.yml",
            vec![
                ("matrix config lanes", "matrix.config"),
                ("inline lane ternary", "inputs.lanes == 'both'"),
                ("github lane entry", r#""lane":"GitHub""#),
                ("velnor lane entry", r#""lane":"Velnor""#),
                ("pinned GitHub runner", "ubuntu-26.04"),
                ("matrix runner", "matrix.config.runner"),
                ("bash run defaults", "shell: bash"),
                ("path filtering", "dorny/paths-filter@v4"),
                ("mise tool installer", "jdx/mise-action@v4"),
                ("mold linker", "rui314/setup-mold@v1"),
                ("sccache action", "mozilla-actions/sccache-action@9e7fa8a"),
                ("sccache local env", "SCCACHE_GHA_ENABLED: \"false\""),
                ("cargo cache", "actions/cache@55cc834"),
                ("cargo cache restore-keys", "restore-keys:"),
                ("cache off job", "cache-off:"),
                ("cache sccache job", "cache-sccache:"),
                ("cache kache job", "cache-kache:"),
                ("Postgres services job", "services-postgres:"),
                ("services declaration", "services:"),
                ("Postgres health check", "pg_isready"),
                ("provenance attestation job", "attestation:"),
                (
                    "pinned provenance attestation",
                    "actions/attest-build-provenance@0f67c3f4856b2e3261c31976d6725780e5e4c373",
                ),
                ("attestation permission", "attestations: write"),
                ("attestation verification", "gh attestation verify"),
                ("artifact upload", "actions/upload-artifact@v7"),
                ("artifact download", "actions/download-artifact@v8"),
                ("command env file", "GITHUB_ENV"),
                ("command output file", "GITHUB_OUTPUT"),
                ("command path file", "GITHUB_PATH"),
                ("command state file", "GITHUB_STATE"),
                ("step summary file", "GITHUB_STEP_SUMMARY"),
                ("matrix packages", "matrix:"),
                ("lane in step summary", "matrix.config.lane }}"),
                ("fmt check", "just fmt-check"),
                ("clippy", "just clippy"),
                ("nextest", "just nextest"),
                ("compare needs", "needs: [compat]"),
                (
                    "fixture output composite",
                    "./.github/actions/check-fixture-output",
                ),
                ("aggregate composite", "./.github/actions/aggregate-needs"),
            ],
        ),
        (
            ".github/workflows/attestation-negative.yml",
            vec![
                ("missing permission case", "missing-permission:"),
                ("no subject match case", "no-match:"),
                ("unapproved input case", "unapproved-input:"),
                ("unapproved registry input", "push-to-registry: false"),
            ],
        ),
        (
            ".github/workflows/docker.yml",
            vec![
                ("matrix config lanes", "matrix.config"),
                ("github lane entry", r#""lane":"GitHub""#),
                ("velnor lane entry", r#""lane":"Velnor""#),
                ("matrix runner", "matrix.config.runner"),
                (
                    "GitHub runtime export",
                    "crazy-max/ghaction-github-runtime@v4",
                ),
                ("Docker login", "docker/login-action@v4"),
                ("Docker metadata", "docker/metadata-action@v6"),
                ("Buildx setup", "docker/setup-buildx-action@v4"),
                ("Docker build action", "docker/build-push-action@v7"),
                ("Docker bake action", "docker/bake-action@v7"),
                ("non-push build", "push: false"),
                ("loaded image", "load: true"),
                (
                    "GHA build cache from with lane",
                    "cache-from: type=gha,scope=fixture-build-${{ matrix.config.lane }}",
                ),
                (
                    "GHA build cache to with lane",
                    "cache-to: type=gha,scope=fixture-build-${{ matrix.config.lane }}",
                ),
                (
                    "GHA bake cache scope with lane",
                    "scope=fixture-bake-${{ matrix.config.lane }}",
                ),
                ("mode max cache", "mode=max"),
                (
                    "container execution with lane",
                    "docker run --rm velnor-actions-fixture:${{ matrix.config.lane }}",
                ),
                ("buildx cache stats", "docker buildx du"),
                ("docker compare job", "docker-compare:"),
                ("aggregate composite", "./.github/actions/aggregate-needs"),
            ],
        ),
        (
            ".github/workflows/pages.yml",
            vec![
                ("matrix config lanes", "matrix.config"),
                ("github lane entry", r#""lane":"GitHub""#),
                ("velnor lane entry", r#""lane":"Velnor""#),
                ("matrix runner", "matrix.config.runner"),
                ("pages write permission", "pages: write"),
                ("OIDC token permission", "id-token: write"),
                ("upload pages artifact", "actions/upload-pages-artifact@v5"),
                (
                    "pages artifact name with lane",
                    "name: github-pages-${{ matrix.config.lane }}",
                ),
                ("deploy pages", "actions/deploy-pages@v5"),
                ("pages environment", "name: github-pages"),
                (
                    "check deployed docs composite",
                    "./.github/actions/check-deployed-docs",
                ),
            ],
        ),
        (
            ".github/workflows/renovate.yml",
            vec![
                ("matrix config lanes", "matrix.config"),
                ("github lane entry", r#""lane":"GitHub""#),
                ("velnor lane entry", r#""lane":"Velnor""#),
                ("matrix runner", "matrix.config.runner"),
                ("renovate action", "renovatebot/github-action@693b9ef"),
                ("renovate token env", "GH_RENOVATE_TOKEN:"),
            ],
        ),
        (
            ".github/actions/aggregate-needs/action.yml",
            vec![("aggregate composite metadata", "runs:")],
        ),
        (
            ".github/actions/check-fixture-output/action.yml",
            vec![("fixture output composite metadata", "runs:")],
        ),
        (
            ".github/actions/check-deployed-docs/action.yml",
            vec![("check-deployed-docs composite metadata", "runs:")],
        ),
        (
            ".github/actions/aggregate-needs/action.yml",
            vec![
                ("aggregate composite metadata", "runs:"),
                ("needs-json bulk mode", "needs-json"),
                ("toJSON aggregation", "toJSON"),
            ],
        ),
        (
            "mise.toml",
            vec![
                ("rust toolchain via mise", "rust = "),
                ("cargo-nextest via mise", "cargo:cargo-nextest"),
            ],
        ),
        (
            "docker-bake.hcl",
            vec![
                ("bake group", "group \"default\""),
                ("bake fixture target", "target \"fixture\""),
            ],
        ),
        (
            ".github/workflows/compat.yml",
            vec![
                // Patterns added in gap-analysis pass
                ("concurrency block", "concurrency:"),
                ("cancel-in-progress expression", "cancel-in-progress:"),
                ("merge_group trigger", "merge_group:"),
                ("schedule trigger", "schedule:"),
                ("multiple path filter outputs", "tools:"),
                ("complex if with contains", "contains("),
                ("heredoc output pattern", "<<EOF"),
                ("env injection in step", "RUSTUP_TOOLCHAIN:"),
                ("MSRV check step", "cargo check -p"),
                ("per-job permissions", "permissions:"),
                ("toJSON aggregate", "toJSON(needs)"),
                ("required aggregate job", "compat-required:"),
                ("workflow_dispatch packages input", "packages:"),
                ("merge-multiple artifact download", "merge-multiple:"),
            ],
        ),
        (
            ".github/workflows/fixture-rust-check.yml",
            vec![
                ("workflow_call trigger", "workflow_call:"),
                ("workflow_call inputs", "inputs:"),
                ("workflow_call secrets", "secrets:"),
                ("typed input package", "type: string"),
            ],
        ),
        (
            ".github/workflows/reuse-caller.yml",
            vec![
                (
                    "reusable workflow uses",
                    "uses: ./.github/workflows/fixture-rust-check.yml",
                ),
                ("secrets inherit", "secrets: inherit"),
                ("toJSON aggregate in caller", "toJSON(needs)"),
                ("concurrency in caller", "concurrency:"),
            ],
        ),
        (
            ".github/workflows/schedule.yml",
            vec![
                ("schedule cron trigger", "cron:"),
                ("merge_group trigger", "merge_group:"),
                ("concurrency with event", "cancel-in-progress:"),
                ("toJSON aggregate in schedule", "toJSON(needs)"),
            ],
        ),
        (
            ".github/workflows/multi-arch.yml",
            vec![
                ("platform matrix", r#""platform":"#),
                ("multi-arch build push", "platforms:"),
                ("push-by-digest", "push-by-digest"),
                ("digest artifact upload", "digest-"),
                ("merge-multiple download", "merge-multiple: true"),
                (
                    "aggregate-needs in multi-arch",
                    "./.github/actions/aggregate-needs",
                ),
            ],
        ),
    ]
}

fn fixture_readiness(args: FixtureReadinessArgs) -> Result<()> {
    println!("==> Checking fixture proof readiness");
    println!("This script does not create JIT runner configs or dispatch workflows.");

    if args.check_status {
        println!("==> Inspecting fixture workflow status");
        run_readiness_section(&args.fixture_status_script)?;
    }

    if args.check_audit {
        println!("==> Auditing fixture feature surface");
        run_readiness_section(&args.fixture_audit_script)?;
    }

    if args.run_local_tests {
        println!("==> Running local target verifier");
        run_readiness_section("scripts/target_verify.sh")?;

        println!("==> Running Rust test suite");
        run_readiness_section("cargo nextest run --workspace --locked")?;
    }

    println!("==> Checking live host readiness");
    run_readiness_section(&args.live_host_doctor_script)?;

    println!(
        "Fixture readiness passed. It is safe to attempt scripts/fixture_smoke.sh on this host."
    );
    Ok(())
}

fn run_readiness_section(command: &str) -> Result<()> {
    let status = Command::new("bash")
        .args(["-lc", command])
        .status()
        .with_context(|| format!("run readiness section {command}"))?;
    if status.success() {
        return Ok(());
    }
    eprintln!("fixture readiness failed; run scripts/fixture_report.sh for a non-mutating markdown handoff report");
    std::process::exit(status.code().unwrap_or(1));
}

fn fixture_report(root: &Path, args: FixtureReportArgs) -> Result<()> {
    let report_path = fixture_report_path(root, &args)?;
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let mut report = String::new();
    report.push_str("# Velnor Fixture Readiness Report\n\n");
    report.push_str("This report does not create JIT runner configs or dispatch workflows.\n\n");
    report.push_str(&format!("- generated_at_utc: {}\n", utc_timestamp()?));
    report.push_str(&format!(
        "- velnor_commit: {}\n",
        git_value(root, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "<unavailable>".to_string())
    ));
    report.push_str(&format!(
        "- velnor_branch: {}\n",
        git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap_or_else(|_| "<unavailable>".to_string())
    ));
    report.push_str(&format!(
        "- velnor_dirty_files: {}\n",
        git_dirty_file_count(root)
    ));
    report.push_str(&format!(
        "- fixture_repo: {}\n",
        env::var("VELNOR_FIXTURE_REPO").unwrap_or_else(|_| DEFAULT_FIXTURE_REPO.to_string())
    ));
    report.push_str(&format!(
        "- fixture_workflow: {}\n",
        env::var("VELNOR_FIXTURE_WORKFLOW").unwrap_or_else(|_| "compat.yml".to_string())
    ));
    report.push_str(&format!(
        "- velnor_work_dir: {}\n",
        env::var("VELNOR_WORK_DIR")
            .unwrap_or_else(|_| root.join(".velnor-work").display().to_string())
    ));
    report.push_str(&format!(
        "- docker_host: {}\n",
        env::var("DOCKER_HOST").unwrap_or_else(|_| "<local>".to_string())
    ));
    report.push_str(&format!(
        "- docker_host_work_dir: {}\n",
        env::var("VELNOR_DOCKER_HOST_WORK_DIR")
            .unwrap_or_else(|_| "<same as velnor_work_dir>".to_string())
    ));
    report.push_str(&format!(
        "- require_docker_socket: {}\n",
        env::var("VELNOR_REQUIRE_DOCKER_SOCKET").unwrap_or_else(|_| "true".to_string())
    ));
    report.push_str(&format!(
        "- run_target_verify: {}\n",
        env::var("VELNOR_RUN_TARGET_VERIFY").unwrap_or_else(|_| "false".to_string())
    ));
    report.push_str(&format!(
        "- check_target_mvp_config: {}\n\n",
        env::var("VELNOR_CHECK_TARGET_MVP_CONFIG").unwrap_or_else(|_| "false".to_string())
    ));

    let sections = [
        (
            "Fixture Workflow Status",
            args.fixture_status_script.as_str(),
        ),
        ("Fixture Feature Audit", args.fixture_audit_script.as_str()),
        ("Live Host Readiness", args.live_host_doctor_script.as_str()),
    ];
    let mut overall = 0;
    for (title, command) in sections {
        let section = run_report_command(command)?;
        if section.status != 0 {
            overall = 1;
        }
        report.push_str(&format!("## {title}\n\n"));
        report.push_str(&format!("- status: {}\n\n", section.status));
        report.push_str("```text\n");
        report.push_str(&section.output);
        if !section.output.ends_with('\n') {
            report.push('\n');
        }
        report.push_str("```\n\n");
    }

    report.push_str("## Next Action\n\n");
    if overall == 0 {
        report
            .push_str("Fixture readiness passed. Run `scripts/fixture_smoke.sh` on this host.\n\n");
        report.push_str("```sh\nscripts/fixture_smoke.sh\n```\n\n");
        report.push_str("After the smoke run, inspect the run and evidence:\n\n");
        report.push_str("```sh\nscripts/fixture_status.sh\nls -1 .velnor-live-evidence\n```\n\n");
    } else {
        report.push_str("Fixture readiness has blockers. Fix the failing section above before running `scripts/fixture_smoke.sh`.\n\n");
        report.push_str("Docker readiness guidance:\n\n");
        report.push_str("- For target-repository proof, use a Linux host where `/var/run/docker.sock` exists and the Docker daemon can see `velnor_work_dir`.\n");
        report.push_str("- For fixture-only checks without Docker-in-job coverage, `VELNOR_REQUIRE_DOCKER_SOCKET=false` may be used deliberately.\n");
        report.push_str("- For remote Docker daemons, set `VELNOR_DOCKER_HOST_WORK_DIR` only when the daemon host sees the same work directory at a different path.\n\n");
        report.push_str("Re-run the non-mutating checks after fixing host readiness:\n\n");
        report
            .push_str("```sh\nscripts/live_host_doctor.sh\nscripts/fixture_readiness.sh\n```\n\n");
        report.push_str("Do not create Velnor JIT runner configs or dispatch real target repository workflows from this report alone.\n\n");
    }

    fs::write(&report_path, report).with_context(|| format!("write {}", report_path.display()))?;
    if overall == 0 {
        println!("Fixture report passed: {}", report_path.display());
        Ok(())
    } else {
        eprintln!("Fixture report found blockers: {}", report_path.display());
        std::process::exit(overall);
    }
}

fn fixture_report_path(root: &Path, args: &FixtureReportArgs) -> Result<PathBuf> {
    if let Some(path) = &args.report_path {
        return Ok(path.clone());
    }
    let dir = args
        .report_dir
        .clone()
        .unwrap_or_else(|| root.join(".velnor-live-evidence"));
    Ok(dir.join("fixture-readiness-report.md"))
}

struct ReportSection {
    status: i32,
    output: String,
}

fn run_report_command(command: &str) -> Result<ReportSection> {
    let output = Command::new("bash")
        .args(["-lc", command])
        .output()
        .with_context(|| format!("run report section {command}"))?;
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(ReportSection {
        status: output.status.code().unwrap_or(1),
        output: combined,
    })
}

fn utc_timestamp() -> Result<String> {
    let now = time::OffsetDateTime::now_utc();
    now.format(&time::format_description::well_known::Rfc3339)
        .context("format UTC timestamp")
}

fn git_value(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_dirty_file_count(root: &Path) -> String {
    match git_value(root, &["status", "--short"]) {
        Ok(output) => output.lines().count().to_string(),
        Err(_) => "unknown".to_string(),
    }
}

fn fixture_status(args: FixtureStatusArgs) -> Result<()> {
    validate_repo_slug("repo", &args.repo)?;
    validate_workflow_file("workflow", &args.workflow)?;
    ensure_command("gh")?;

    let run_id = match args.run_id {
        Some(run_id) => run_id,
        None => latest_fixture_run_id(&args.repo, &args.workflow)?,
    };
    let run = github_run_view(&args.repo, run_id)?;
    println!("url\t{}", required_string(&run, "url")?);
    println!(
        "run\t{}\t{}",
        required_string(&run, "status")?,
        optional_string(&run, "conclusion").unwrap_or_default()
    );
    let jobs = run
        .get("jobs")
        .and_then(|value| value.as_array())
        .ok_or_else(|| anyhow::anyhow!("gh run view response missing jobs array"))?;
    for job in jobs {
        println!(
            "job\t{}\t{}\t{}",
            required_string(job, "name")?,
            required_string(job, "status")?,
            optional_string(job, "conclusion").unwrap_or_default()
        );
    }
    Ok(())
}

fn latest_fixture_run_id(repo: &str, workflow: &str) -> Result<u64> {
    let output = run_command_output(
        Command::new("gh").args([
            "run",
            "list",
            "--repo",
            repo,
            "--workflow",
            workflow,
            "--limit",
            "1",
            "--json",
            "databaseId",
        ]),
        "list fixture workflow runs",
    )?;
    let runs: serde_json::Value =
        serde_json::from_str(&output).context("parse gh run list output")?;
    let Some(run_id) = runs
        .as_array()
        .and_then(|runs| runs.first())
        .and_then(|run| run.get("databaseId"))
        .and_then(|database_id| database_id.as_u64())
    else {
        bail!("No fixture workflow runs found for {workflow} in {repo}.");
    };
    Ok(run_id)
}

fn github_run_view(repo: &str, run_id: u64) -> Result<serde_json::Value> {
    let output = run_command_output(
        Command::new("gh").args([
            "run",
            "view",
            &run_id.to_string(),
            "--repo",
            repo,
            "--json",
            "status,conclusion,jobs,url",
        ]),
        "view fixture workflow run",
    )?;
    serde_json::from_str(&output).context("parse gh run view output")
}

fn validate_repo_slug(name: &str, value: &str) -> Result<()> {
    let regex = Regex::new(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")?;
    if regex.is_match(value) {
        Ok(())
    } else {
        bail!("{name} must be a GitHub repository slug in owner/name form")
    }
}

fn validate_workflow_file(name: &str, value: &str) -> Result<()> {
    validate_nonempty(name, value)?;
    validate_optional_workflow_file(name, value)
}

fn validate_optional_workflow_file(name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    let valid = !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && (value.ends_with(".yml") || value.ends_with(".yaml"));
    if valid {
        Ok(())
    } else {
        bail!("{name} must be a workflow file name ending in .yml or .yaml")
    }
}

fn ensure_command(name: &str) -> Result<()> {
    let output = Command::new(name)
        .arg("--version")
        .output()
        .with_context(|| format!("run {name} --version"))?;
    if output.status.success() {
        Ok(())
    } else {
        bail!("{name} --version failed")
    }
}

fn run_command_output(command: &mut Command, label: &str) -> Result<String> {
    let output = command.output().with_context(|| label.to_string())?;
    if !output.status.success() {
        bail!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn required_string<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow::anyhow!("gh response missing string field {key}"))
}

fn optional_string<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(|value| value.as_str())
}

fn fixture_smoke_plan(root: &Path, args: FixtureSmokePlanArgs) -> Result<()> {
    validate_repo_slug("repo", &args.repo)?;
    validate_nonempty("runner-name", &args.runner_name)?;
    validate_nonempty("runner-label", &args.runner_label)?;
    validate_workflow_file("workflow", &args.workflow)?;
    validate_positive_u64("idle-timeout-seconds", args.idle_timeout_seconds)?;
    validate_positive_u64("job-count", args.job_count)?;
    validate_workflow_dispatch_inputs(&args.fixture_inputs)?;

    let dispatch = fixture_smoke_dispatch(args.dispatch, args.run_id)?;

    let fixture_url = args
        .url
        .unwrap_or_else(|| format!("https://github.com/{}", args.repo));
    let work_dir = args.work_dir.unwrap_or_else(|| root.join(".velnor-work"));
    let dump_job_messages = args
        .dump_job_messages
        .unwrap_or_else(|| root.join(".velnor-job-dumps/fixture").display().to_string());

    println!("fixture_repo\t{}", args.repo);
    println!("fixture_url\t{fixture_url}");
    println!("runner_name\t{}", args.runner_name);
    println!("runner_label\t{}", args.runner_label);
    println!("workflow\t{}", args.workflow);
    println!("dispatch\t{dispatch}");
    println!(
        "run_id\t{}",
        args.run_id
            .map(|run_id| run_id.to_string())
            .unwrap_or_default()
    );
    println!("fixture_ref\t{}", args.fixture_ref);
    println!("fixture_inputs\t{}", args.fixture_inputs);
    println!("job_count\t{}", args.job_count);
    println!("cleanup_runner\t{}", args.cleanup_runner);
    println!(
        "allow_other_matching_runners\t{}",
        args.allow_other_matching_runners
    );
    println!("require_docker_socket\t{}", args.require_docker_socket);
    println!("idle_timeout_seconds\t{}", args.idle_timeout_seconds);
    println!("work_dir\t{}", work_dir.display());
    println!("dump_job_messages\t{dump_job_messages}");
    if let Some(path) = &args.docker_host_work_dir {
        println!("docker_host_work_dir\t{}", path.display());
    } else {
        println!("docker_host_work_dir\t");
    }
    println!(
        "daemon_args\t{}",
        fixture_smoke_daemon_args(
            &fixture_url,
            &args.runner_name,
            &args.runner_label,
            args.job_count,
            args.idle_timeout_seconds,
            &work_dir,
            args.docker_host_work_dir.as_deref(),
            args.require_docker_socket,
            &dump_job_messages,
        )
        .join(" ")
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn fixture_smoke_daemon_args(
    fixture_url: &str,
    runner_name: &str,
    runner_label: &str,
    job_count: u64,
    idle_timeout_seconds: u64,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
    require_docker_socket: bool,
    dump_job_messages: &str,
) -> Vec<String> {
    let mut args = vec![
        "--url".to_string(),
        fixture_url.to_string(),
        "--pat".to_string(),
        "$GITHUB_TOKEN".to_string(),
        "--name".to_string(),
        runner_name.to_string(),
        "--labels".to_string(),
        runner_label.to_string(),
        "--replace".to_string(),
        "--slots".to_string(),
        job_count.to_string(),
        "--once".to_string(),
        "--idle-timeout-seconds".to_string(),
        idle_timeout_seconds.to_string(),
        "--work-dir".to_string(),
        work_dir.display().to_string(),
    ];
    if !dump_job_messages.is_empty() {
        args.push("--dump-job-message".to_string());
        args.push(dump_job_messages.to_string());
    }
    if let Some(path) = docker_host_work_dir {
        args.push("--docker-host-work-dir".to_string());
        args.push(path.display().to_string());
    }
    if require_docker_socket {
        args.push("--require-docker-socket".to_string());
    }
    args
}

fn fixture_smoke_dispatch(dispatch: Option<bool>, run_id: Option<u64>) -> Result<bool> {
    let dispatch = dispatch.unwrap_or(run_id.is_none());
    if !dispatch && run_id.is_none() {
        bail!("VELNOR_FIXTURE_RUN_ID is required when VELNOR_FIXTURE_DISPATCH=false.");
    }
    Ok(dispatch)
}

fn target_smoke_plan(root: &Path, args: TargetSmokePlanArgs) -> Result<()> {
    validate_repo_slug("repo", &args.repo)?;
    validate_nonempty("runner-name", &args.runner_name)?;
    validate_optional_workflow_file("workflow", &args.workflow)?;
    validate_positive_u64("idle-timeout-seconds", args.idle_timeout_seconds)?;
    validate_positive_u64("job-count", args.job_count)?;
    validate_workflow_dispatch_inputs(&args.target_inputs)?;
    validate_real_target_manual_confirmation_bool(&args.repo, args.real_target_manual_confirm)?;

    let target_url = args
        .url
        .unwrap_or_else(|| format!("https://github.com/{}", args.repo));
    let work_dir = args.work_dir.unwrap_or_else(|| root.join(".velnor-work"));
    let dump_job_messages = args
        .dump_job_messages
        .unwrap_or_else(|| root.join(".velnor-job-dumps/target").display().to_string());
    let scheduling_labels = target_smoke_scheduling_labels(args.target_mvp_arm_label);

    println!("target_repo\t{}", args.repo);
    println!("target_url\t{target_url}");
    println!("runner_name\t{}", args.runner_name);
    println!("workflow\t{}", args.workflow);
    println!(
        "run_id\t{}",
        args.run_id
            .map(|run_id| run_id.to_string())
            .unwrap_or_default()
    );
    println!("target_ref\t{}", args.target_ref);
    println!("target_inputs\t{}", args.target_inputs);
    println!("job_count\t{}", args.job_count);
    println!("cleanup_runner\t{}", args.cleanup_runner);
    println!("watch_run\t{}", args.watch_run);
    println!("target_label\t{}", args.target_label);
    println!("target_mvp_arm_label\t{}", args.target_mvp_arm_label);
    println!(
        "real_target_manual_confirm\t{}",
        args.real_target_manual_confirm
    );
    println!(
        "allow_other_matching_runners\t{}",
        args.allow_other_matching_runners
    );
    println!("require_docker_socket\t{}", args.require_docker_socket);
    println!("idle_timeout_seconds\t{}", args.idle_timeout_seconds);
    println!("work_dir\t{}", work_dir.display());
    println!("dump_job_messages\t{dump_job_messages}");
    if let Some(path) = &args.docker_host_work_dir {
        println!("docker_host_work_dir\t{}", path.display());
    } else {
        println!("docker_host_work_dir\t");
    }
    println!("scheduling_labels\t{}", scheduling_labels.join(","));
    println!(
        "daemon_args\t{}",
        target_smoke_daemon_args(
            &target_url,
            &args.runner_name,
            args.target_mvp_arm_label,
            args.job_count,
            args.idle_timeout_seconds,
            &work_dir,
            args.docker_host_work_dir.as_deref(),
            args.require_docker_socket,
            &dump_job_messages,
        )
        .join(" ")
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn target_smoke_daemon_args(
    target_url: &str,
    runner_name: &str,
    target_mvp_arm_label: bool,
    job_count: u64,
    idle_timeout_seconds: u64,
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
    require_docker_socket: bool,
    dump_job_messages: &str,
) -> Vec<String> {
    let mut args = vec![
        "--url".to_string(),
        target_url.to_string(),
        "--pat".to_string(),
        "$GITHUB_TOKEN".to_string(),
        "--name".to_string(),
        runner_name.to_string(),
        "--target-mvp-labels".to_string(),
        "--replace".to_string(),
        "--slots".to_string(),
        job_count.to_string(),
        "--once".to_string(),
        "--idle-timeout-seconds".to_string(),
        idle_timeout_seconds.to_string(),
        "--work-dir".to_string(),
        work_dir.display().to_string(),
    ];
    if target_mvp_arm_label {
        args.push("--target-mvp-arm-label".to_string());
    }
    if !dump_job_messages.is_empty() {
        args.push("--dump-job-message".to_string());
        args.push(dump_job_messages.to_string());
    }
    if let Some(path) = docker_host_work_dir {
        args.push("--docker-host-work-dir".to_string());
        args.push(path.display().to_string());
    }
    if require_docker_socket {
        args.push("--require-docker-socket".to_string());
    }
    args
}

fn target_smoke_scheduling_labels(target_mvp_arm_label: bool) -> Vec<&'static str> {
    let mut labels = vec!["hetzner-sentry-ci", "ubuntu-latest", "ubuntu-24.04"];
    if target_mvp_arm_label {
        labels.push("ubuntu-24.04-arm");
    }
    labels
}

fn live_host_doctor_plan(root: &Path, args: LiveHostDoctorPlanArgs) -> Result<()> {
    let host_os = args
        .host_os
        .unwrap_or_else(|| std::env::consts::OS.to_string());
    let host_arch = args
        .host_arch
        .unwrap_or_else(|| std::env::consts::ARCH.to_string());
    validate_target_mvp_arm_label_host(args.target_mvp_arm_label, &host_arch)?;

    let work_dir = args.work_dir.unwrap_or_else(|| root.join(".velnor-work"));
    let preflight_args = live_host_doctor_preflight_args(
        &work_dir,
        args.docker_host_work_dir.as_deref(),
        args.require_docker_socket,
    );

    println!("host_os\t{host_os}");
    println!("host_arch\t{host_arch}");
    println!("required_tools\tgit,docker,cargo");
    println!("require_docker_socket\t{}", args.require_docker_socket);
    println!("check_target_mvp_config\t{}", args.check_target_mvp_config);
    println!("run_target_verify\t{}", args.run_target_verify);
    println!("target_mvp_arm_label\t{}", args.target_mvp_arm_label);
    println!("work_dir\t{}", work_dir.display());
    if let Some(path) = &args.docker_host_work_dir {
        println!("docker_host_work_dir\t{}", path.display());
    } else {
        println!("docker_host_work_dir\t");
    }
    if let Some(docker_host) = &args.docker_host {
        println!("docker_host\t{docker_host}");
        println!("remote_docker_hint\t{}", is_remote_docker_host(docker_host));
    } else {
        println!("docker_host\t");
        println!("remote_docker_hint\tfalse");
    }
    println!("preflight_args\t{}", preflight_args.join(" "));
    println!(
        "status_args\t{}",
        live_host_doctor_status_args(args.check_target_mvp_config).join(" ")
    );
    Ok(())
}

fn live_host_doctor_preflight_args(
    work_dir: &Path,
    docker_host_work_dir: Option<&Path>,
    require_docker_socket: bool,
) -> Vec<String> {
    let mut args = vec!["--work-dir".to_string(), work_dir.display().to_string()];
    if let Some(path) = docker_host_work_dir {
        args.push("--docker-host-work-dir".to_string());
        args.push(path.display().to_string());
    }
    if require_docker_socket {
        args.push("--require-docker-socket".to_string());
    }
    args
}

fn live_host_doctor_status_args(check_target_mvp_config: bool) -> Vec<String> {
    if check_target_mvp_config {
        vec!["--check-target-mvp".to_string()]
    } else {
        Vec::new()
    }
}

fn is_remote_docker_host(docker_host: &str) -> bool {
    docker_host.starts_with("tcp://") || docker_host.starts_with("ssh://")
}

fn validate_target_mvp_arm_label_host(enabled: bool, host_arch: &str) -> Result<()> {
    if !enabled || matches!(host_arch, "aarch64" | "arm64") {
        return Ok(());
    }
    bail!("unsupported ARM runner label on host architecture '{host_arch}'; only set VELNOR_TARGET_MVP_ARM_LABEL=true when Docker can provide ARM64 Linux job containers")
}

fn validate_nonempty(name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{name} must not be empty")
    }
    Ok(())
}

fn validate_positive_u64(name: &str, value: u64) -> Result<()> {
    if value == 0 {
        bail!("{name} must be a positive integer")
    }
    Ok(())
}

fn validate_positive_int_string(name: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.chars().all(|ch| ch.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|value| value > 0);
    if valid {
        Ok(())
    } else {
        bail!("{name} must be a positive integer")
    }
}

#[allow(dead_code)]
fn validate_optional_positive_int_string(name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Ok(());
    }
    validate_positive_int_string(name, value)
}

fn validate_workflow_dispatch_inputs(inputs: &str) -> Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let regex = Regex::new(r"^[A-Za-z_][A-Za-z0-9_-]*$")?;
    for input in inputs.split(',') {
        let Some((key, _)) = input.split_once('=') else {
            bail!("workflow dispatch inputs must be comma-separated key=value pairs: '{inputs}'");
        };
        if key.is_empty() || !regex.is_match(key) {
            bail!("workflow dispatch input key must match [A-Za-z_][A-Za-z0-9_-]*: '{input}'");
        }
    }
    Ok(())
}

fn validate_live_evidence_controls(log_lines: &str, local_entries: &str) -> Result<()> {
    validate_positive_int_string("VELNOR_LIVE_EVIDENCE_LOG_LINES", log_lines)?;
    validate_positive_int_string("VELNOR_LIVE_EVIDENCE_LOCAL_ENTRIES", local_entries)
}

#[allow(dead_code)]
fn validate_real_target_manual_confirmation(repo: &str, confirm: &str) -> Result<()> {
    match repo {
        "ChainArgos/java-monorepo" | "jackin-project/jackin" => {
            validate_bool_string("VELNOR_REAL_TARGET_MANUAL_CONFIRM", confirm)?;
            if confirm != "true" {
                bail!("{repo} is a real target repository. Set VELNOR_REAL_TARGET_MANUAL_CONFIRM=true only when the user/operator is intentionally running manual target validation.");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_real_target_manual_confirmation_bool(repo: &str, confirm: bool) -> Result<()> {
    match repo {
        "ChainArgos/java-monorepo" | "jackin-project/jackin" if !confirm => {
            bail!("{repo} is a real target repository. Set VELNOR_REAL_TARGET_MANUAL_CONFIRM=true only when the user/operator is intentionally running manual target validation.");
        }
        _ => Ok(()),
    }
}

#[allow(dead_code)]
fn validate_bool_string(name: &str, value: &str) -> Result<()> {
    match value {
        "true" | "false" => Ok(()),
        _ => bail!("{name} must be 'true' or 'false'."),
    }
}

fn other_matching_online_runners(
    runner_rows: &str,
    expected_name: &str,
    labels: &[&str],
) -> Vec<String> {
    let mut matches = Vec::new();
    for line in runner_rows.lines() {
        let mut parts = line.split('\t');
        let Some(name) = parts.next() else {
            continue;
        };
        let Some(status) = parts.next() else {
            continue;
        };
        let Some(label_csv) = parts.next() else {
            continue;
        };
        if status != "online"
            || name == expected_name
            || name.starts_with(&format!("{expected_name}-slot-"))
        {
            continue;
        }
        for label in labels {
            if label_csv.split(',').any(|candidate| candidate == *label) {
                matches.push(format!("{name} ({label})"));
            }
        }
    }
    matches
}

fn job_execution_model(job_count: u64, label: &str) -> String {
    format!(
        "==> {label} job execution model\nProduction Velnor should run as one daemon with multiple internal GitHub runner slots.\nEach assigned job gets its own isolated Docker container and can run concurrently with other assigned jobs.\nThis smoke script exercises {job_count} job(s) through one bounded daemon: daemon --once with one internal runner slot per requested job."
    )
}

#[derive(Debug)]
struct TargetWorkflow {
    path: String,
    yaml: serde_yaml::Value,
}

#[derive(Debug)]
struct TargetAction {
    path: String,
    kind: String,
    yaml: serde_yaml::Value,
}

#[derive(Debug, Default)]
struct TargetSurface {
    workflow_files: BTreeMap<String, usize>,
    action_files: BTreeSet<(String, String)>,
    uses: BTreeMap<String, usize>,
    reusable_workflows: BTreeMap<(String, String, String), usize>,
    workflow_triggers: BTreeMap<(String, String), usize>,
    job_runs_on: BTreeMap<(String, String, String), usize>,
    workflow_env: BTreeMap<(String, String), usize>,
    unsupported: Vec<String>,
}

fn target_audit(args: TargetAuditArgs) -> Result<()> {
    if !args.check_target_mvp && !args.self_test {
        bail!("pass --check-target-mvp or --self-test");
    }
    if args.self_test {
        target_audit_self_test()?;
        validate_target_roots(&args.roots)?;
        println!(
            "target audit self-test passed for {} roots",
            args.roots.len()
        );
        return Ok(());
    }

    let surface = collect_target_surface(&args.roots)?;
    assert_counts(
        "workflow files",
        &expected_workflow_files(),
        &surface.workflow_files,
    )?;
    assert_set(
        "local action files",
        &expected_action_files(),
        &surface.action_files,
    )?;
    assert_counts("uses", &expected_target_uses(), &surface.uses)?;
    assert_counts(
        "reusable workflows",
        &expected_reusable_workflows(),
        &surface.reusable_workflows,
    )?;
    assert_counts(
        "workflow triggers",
        &expected_workflow_triggers(),
        &surface.workflow_triggers,
    )?;
    assert_counts("job runs-on", &expected_job_runs_on(), &surface.job_runs_on)?;
    assert_counts(
        "workflow env",
        &expected_workflow_env(),
        &surface.workflow_env,
    )?;
    if !surface.unsupported.is_empty() {
        bail!(
            "unsupported target workflow surface:\n{}",
            surface.unsupported.join("\n")
        );
    }

    println!("target audit passed");
    println!("workflow files: {}", surface.workflow_files.len());
    println!("action families: {}", surface.uses.len());
    Ok(())
}

fn validate_target_roots(roots: &[PathBuf]) -> Result<()> {
    for root in roots {
        if !root.join(".github").is_dir() {
            bail!("{} missing .github directory", root.display());
        }
    }
    Ok(())
}

fn collect_target_surface(roots: &[PathBuf]) -> Result<TargetSurface> {
    validate_target_roots(roots)?;
    let mut surface = TargetSurface::default();
    for root in roots {
        for workflow in read_target_workflows(root)? {
            increment(&mut surface.workflow_files, workflow.path.clone());
            collect_workflow(&mut surface, &workflow)?;
        }
        for action in read_target_actions(root)? {
            surface
                .action_files
                .insert((action.path.clone(), action.kind.clone()));
            collect_action(&mut surface, &action);
        }
    }
    Ok(surface)
}

fn read_target_workflows(root: &Path) -> Result<Vec<TargetWorkflow>> {
    let workflows = root.join(".github/workflows");
    let mut result = Vec::new();
    if !workflows.is_dir() {
        return Ok(result);
    }
    for path in collect_yaml_files(&workflows)? {
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let yaml = serde_yaml::from_str::<serde_yaml::Value>(&text)
            .with_context(|| format!("parse {}", path.display()))?;
        result.push(TargetWorkflow {
            path: relative_path(root, &path)?,
            yaml,
        });
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

fn read_target_actions(root: &Path) -> Result<Vec<TargetAction>> {
    let actions = root.join(".github/actions");
    let mut result = Vec::new();
    if !actions.is_dir() {
        return Ok(result);
    }
    for path in collect_yaml_files(&actions)? {
        if path
            .file_stem()
            .and_then(|name| name.to_str())
            .is_none_or(|name| name != "action")
        {
            continue;
        }
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let yaml = serde_yaml::from_str::<serde_yaml::Value>(&text)
            .with_context(|| format!("parse {}", path.display()))?;
        let kind = object_get(&yaml, "runs")
            .and_then(|runs| object_get(runs, "using"))
            .and_then(|using| using.as_str())
            .unwrap_or("unknown")
            .to_string();
        result.push(TargetAction {
            path: relative_path(root, &path)?,
            kind,
            yaml,
        });
    }
    result.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(result)
}

fn collect_yaml_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    collect_yaml_files_inner(root, &mut result)?;
    result.sort();
    Ok(result)
}

fn collect_yaml_files_inner(path: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(path).with_context(|| format!("read dir {}", path.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            collect_yaml_files_inner(&path, result)?;
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            result.push(path);
        }
    }
    Ok(())
}

fn collect_workflow(surface: &mut TargetSurface, workflow: &TargetWorkflow) -> Result<()> {
    let Some(root) = workflow.yaml.as_mapping() else {
        surface
            .unsupported
            .push(format!("{}: workflow root is not mapping", workflow.path));
        return Ok(());
    };
    if let Some(on) = mapping_get(root, "on") {
        increment(
            &mut surface.workflow_triggers,
            (workflow.path.clone(), list_keys(on).join(",")),
        );
    }
    if let Some(env) = mapping_get(root, "env") {
        increment(
            &mut surface.workflow_env,
            (workflow.path.clone(), compact_value(env)),
        );
    }
    let Some(jobs) = mapping_get(root, "jobs").and_then(|value| value.as_mapping()) else {
        return Ok(());
    };
    for (job_name, job_value) in jobs {
        let Some(job) = job_value.as_mapping() else {
            continue;
        };
        if let Some(runs_on) = mapping_get(job, "runs-on") {
            increment(
                &mut surface.job_runs_on,
                (
                    workflow.path.clone(),
                    job_name.to_string(),
                    compact_value(runs_on),
                ),
            );
        }
        if let Some(uses) = mapping_get(job, "uses").and_then(|value| value.as_str()) {
            increment(
                &mut surface.reusable_workflows,
                (
                    workflow.path.clone(),
                    job_name.to_string(),
                    normalize_uses(uses).to_string(),
                ),
            );
        }
        if let Some(steps) = mapping_get(job, "steps").and_then(|value| value.as_sequence()) {
            for step in steps {
                collect_step(surface, &workflow.path, step);
            }
        }
    }
    Ok(())
}

fn collect_action(surface: &mut TargetSurface, action: &TargetAction) {
    let Some(steps) = object_get(&action.yaml, "runs")
        .and_then(|runs| object_get(runs, "steps"))
        .and_then(|steps| steps.as_sequence())
    else {
        return;
    };
    for step in steps {
        collect_step(surface, &action.path, step);
    }
}

fn collect_step(surface: &mut TargetSurface, workflow_path: &str, step: &serde_yaml::Value) {
    let Some(step) = step.as_mapping() else {
        return;
    };
    let Some(uses) = mapping_get(step, "uses").and_then(|value| value.as_str()) else {
        return;
    };
    if uses.starts_with("docker://") {
        surface
            .unsupported
            .push(format!("{workflow_path}: docker action {uses}"));
        return;
    }
    let family = normalize_uses(uses).to_string();
    increment(&mut surface.uses, family.clone());

    if family == "actions/checkout" {
        if let Some(with) = mapping_get(step, "with").and_then(|value| value.as_mapping()) {
            for input in with.keys() {
                let supported = matches!(
                    input.as_str(),
                    "repository"
                        | "ref"
                        | "token"
                        | "path"
                        | "fetch-depth"
                        | "submodules"
                        | "persist-credentials"
                        | "lfs"
                );
                if !supported {
                    surface.unsupported.push(format!(
                        "{workflow_path}: unsupported actions/checkout input {input}"
                    ));
                }
            }
        }
    }
}

fn normalize_uses(uses: &str) -> &str {
    let Some((family, _)) = uses.split_once('@') else {
        return uses;
    };
    family
}

fn list_keys(value: &serde_yaml::Value) -> Vec<String> {
    match value {
        serde_yaml::Value::Mapping(mapping) => {
            let mut keys = mapping.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            keys
        }
        serde_yaml::Value::Sequence(sequence) => {
            let mut keys = sequence
                .iter()
                .filter_map(scalar_to_string)
                .collect::<Vec<_>>();
            keys.sort();
            keys
        }
        serde_yaml::Value::String(value) => vec![value.clone()],
        _ => vec![compact_value(value)],
    }
}

fn compact_value(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Null => "null".to_string(),
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::String(value) => value.clone(),
        serde_yaml::Value::Sequence(sequence) => {
            let values = sequence.iter().map(compact_value).collect::<Vec<_>>();
            format!("[{}]", values.join(", "))
        }
        serde_yaml::Value::Mapping(mapping) => {
            let mut entries = mapping
                .iter()
                .map(|(key, value)| (key.clone(), compact_value(value)))
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let entries = entries
                .into_iter()
                .map(|(key, value)| format!("{key}: {value}"))
                .collect::<Vec<_>>();
            format!("{{{}}}", entries.join(", "))
        }
        serde_yaml::Value::Tagged(tagged) => compact_value(tagged.value()),
    }
}

fn object_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()
        .and_then(|mapping| mapping_get(mapping, key))
}

fn mapping_get<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    mapping.get(key)
}

fn scalar_to_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => Some(value.clone()),
        serde_yaml::Value::Bool(value) => Some(value.to_string()),
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(root)
        .with_context(|| format!("strip {} from {}", root.display(), path.display()))?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn increment<K: Ord>(counts: &mut BTreeMap<K, usize>, key: K) {
    *counts.entry(key).or_insert(0) += 1;
}

fn assert_counts<K>(
    label: &str,
    expected: &BTreeMap<K, usize>,
    actual: &BTreeMap<K, usize>,
) -> Result<()>
where
    K: Ord + std::fmt::Debug,
{
    if expected == actual {
        return Ok(());
    }
    let mut failures = Vec::new();
    for key in expected
        .keys()
        .chain(actual.keys())
        .collect::<BTreeSet<_>>()
    {
        let expected_count = expected.get(key).copied().unwrap_or_default();
        let actual_count = actual.get(key).copied().unwrap_or_default();
        if expected_count != actual_count {
            failures.push(format!(
                "{key:?}: expected {expected_count}, got {actual_count}"
            ));
        }
    }
    bail!("{label} drift:\n{}", failures.join("\n"))
}

fn assert_set<K>(label: &str, expected: &BTreeSet<K>, actual: &BTreeSet<K>) -> Result<()>
where
    K: Ord + std::fmt::Debug,
{
    if expected == actual {
        return Ok(());
    }
    let missing = expected.difference(actual).collect::<Vec<_>>();
    let extra = actual.difference(expected).collect::<Vec<_>>();
    bail!("{label} drift: missing {missing:?}, extra {extra:?}")
}

fn expected_target_uses() -> BTreeMap<String, usize> {
    map_counts([
        ("./.github/actions/aggregate-needs", 3),
        ("./.github/actions/check-deployed-docs", 2),
        ("actions/cache", 13),
        ("actions/checkout", 46),
        ("actions/deploy-pages", 1),
        ("actions/download-artifact", 3),
        ("actions/setup-python", 1),
        ("actions/upload-artifact", 6),
        ("actions/upload-pages-artifact", 1),
        ("baptiste0928/cargo-install", 1),
        ("crazy-max/ghaction-github-runtime", 2),
        ("docker/bake-action", 1),
        ("docker/build-push-action", 1),
        ("docker/login-action", 5),
        ("docker/metadata-action", 1),
        ("docker/setup-buildx-action", 5),
        ("dorny/paths-filter", 5),
        ("dtolnay/rust-toolchain", 1),
        ("extractions/setup-just", 4),
        ("jdx/mise-action", 13),
        ("mozilla-actions/sccache-action", 7),
        ("renovatebot/github-action", 2),
        ("rui314/setup-mold", 5),
        ("Swatinem/rust-cache", 1),
    ])
}

fn expected_workflow_files() -> BTreeMap<String, usize> {
    map_counts([
        (".github/workflows/ansible.yml", 1),
        (".github/workflows/ci.yml", 1),
        (".github/workflows/construct.yml", 1),
        (".github/workflows/docs.yml", 1),
        (".github/workflows/kestra-build-image.yml", 1),
        (".github/workflows/kestra-build-publish.yml", 1),
        (".github/workflows/preview.yml", 1),
        (".github/workflows/release.yml", 1),
        (".github/workflows/renovate-validate.yml", 1),
        (".github/workflows/renovate.yml", 2),
        (".github/workflows/rust-docker-build.yml", 1),
        (".github/workflows/rust-docker.yml", 1),
        (".github/workflows/rust.yml", 1),
    ])
}

fn expected_action_files() -> BTreeSet<(String, String)> {
    [
        (
            ".github/actions/aggregate-needs/action.yml".to_string(),
            "composite".to_string(),
        ),
        (
            ".github/actions/check-deployed-docs/action.yml".to_string(),
            "composite".to_string(),
        ),
    ]
    .into_iter()
    .collect()
}

fn expected_reusable_workflows() -> BTreeMap<(String, String, String), usize> {
    map_tuple3_counts([
        (
            ".github/workflows/kestra-build-publish.yml",
            "docker-jvm-base",
            "./.github/workflows/kestra-build-image.yml",
            1,
        ),
        (
            ".github/workflows/kestra-build-publish.yml",
            "docker-kestra-backup",
            "./.github/workflows/kestra-build-image.yml",
            1,
        ),
        (
            ".github/workflows/kestra-build-publish.yml",
            "docker-kestra-playwright",
            "./.github/workflows/kestra-build-image.yml",
            1,
        ),
    ])
}

fn expected_workflow_triggers() -> BTreeMap<(String, String), usize> {
    map_tuple2_counts([
        (
            ".github/workflows/ansible.yml",
            "pull_request,push,workflow_dispatch",
            1,
        ),
        (
            ".github/workflows/ci.yml",
            "pull_request,push,workflow_dispatch",
            1,
        ),
        (
            ".github/workflows/construct.yml",
            "pull_request,push,workflow_dispatch",
            1,
        ),
        (
            ".github/workflows/docs.yml",
            "pull_request,push,schedule,workflow_dispatch",
            1,
        ),
        (
            ".github/workflows/kestra-build-image.yml",
            "workflow_call",
            1,
        ),
        (
            ".github/workflows/kestra-build-publish.yml",
            "push,workflow_dispatch",
            1,
        ),
        (
            ".github/workflows/preview.yml",
            "workflow_dispatch,workflow_run",
            1,
        ),
        (".github/workflows/release.yml", "push,workflow_dispatch", 1),
        (
            ".github/workflows/renovate-validate.yml",
            "pull_request,push,workflow_dispatch",
            1,
        ),
        (
            ".github/workflows/renovate.yml",
            "merge_group,push,schedule,workflow_dispatch",
            2,
        ),
        (
            ".github/workflows/rust-docker-build.yml",
            "workflow_call",
            1,
        ),
        (
            ".github/workflows/rust-docker.yml",
            "pull_request,push,workflow_dispatch",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "pull_request,push,workflow_dispatch",
            1,
        ),
    ])
}

fn expected_job_runs_on() -> BTreeMap<(String, String, String), usize> {
    map_tuple3_counts([
        (
            ".github/workflows/ansible.yml",
            "syntax-check",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/ci.yml",
            "build-validator",
            "ubuntu-latest",
            1,
        ),
        (".github/workflows/ci.yml", "changes", "ubuntu-latest", 1),
        (".github/workflows/ci.yml", "check", "ubuntu-latest", 1),
        (
            ".github/workflows/ci.yml",
            "ci-required",
            "ubuntu-latest",
            1,
        ),
        (".github/workflows/ci.yml", "msrv", "ubuntu-latest", 1),
        (
            ".github/workflows/construct.yml",
            "build",
            "${{ matrix.runner }}",
            1,
        ),
        (
            ".github/workflows/construct.yml",
            "changes",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/construct.yml",
            "construct-required",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/construct.yml",
            "publish-manifest",
            "ubuntu-24.04",
            1,
        ),
        (
            ".github/workflows/construct.yml",
            "publish-manifest-rehearsal",
            "ubuntu-24.04",
            1,
        ),
        (".github/workflows/docs.yml", "changes", "ubuntu-latest", 1),
        (
            ".github/workflows/docs.yml",
            "check-deployed",
            "ubuntu-latest",
            1,
        ),
        (".github/workflows/docs.yml", "deploy", "ubuntu-latest", 1),
        (
            ".github/workflows/docs.yml",
            "docs-link-check",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/docs.yml",
            "docs-required",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/docs.yml",
            "repo-link-check",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/kestra-build-image.yml",
            "build",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/preview.yml",
            "build-jackin-capsule",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/preview.yml",
            "build-preview",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/preview.yml",
            "publish-preview",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/preview.yml",
            "source-changed",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/release.yml",
            "build",
            "${{ matrix.os }}",
            1,
        ),
        (
            ".github/workflows/release.yml",
            "build-jackin-capsule",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/release.yml",
            "check-version",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/release.yml",
            "homebrew",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/release.yml",
            "release",
            "ubuntu-latest",
            1,
        ),
        (".github/workflows/release.yml", "test", "ubuntu-latest", 1),
        (
            ".github/workflows/renovate-validate.yml",
            "validate",
            "ubuntu-latest",
            1,
        ),
        (
            ".github/workflows/renovate.yml",
            "renovate",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/renovate.yml",
            "renovate",
            "ubuntu-24.04",
            1,
        ),
        (
            ".github/workflows/rust-docker-build.yml",
            "build",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust-docker.yml",
            "changes",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust-docker.yml",
            "docker-bake",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust-docker.yml",
            "docker-required",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "changes",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "check",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "rust-required",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-bitcoin-processor",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-blockchain-explorer",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-coingecko-pricing",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-eth-grpc-server",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-eth-processor",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-legacy-grpc-server",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-tron-grpc-server",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "test-tron-processor",
            "hetzner-sentry-ci",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "warm-sccache",
            "hetzner-sentry-ci",
            1,
        ),
    ])
}

fn expected_workflow_env() -> BTreeMap<(String, String), usize> {
    map_tuple2_counts([
        (
            ".github/workflows/construct.yml",
            "{DIGEST_DIR: /tmp/jackin-construct-digests, REGISTRY_IMAGE: projectjackin/construct}",
            1,
        ),
        (
            ".github/workflows/docs.yml",
            "{DOCS_SITE_URL: https://jackin.tailrocks.com, JACKIN_REPO_BLOB_URL: https://github.com/jackin-project/jackin/blob/main, JACKIN_REPO_EDIT_URL: https://github.com/jackin-project/jackin/edit/main}",
            1,
        ),
        (
            ".github/workflows/rust.yml",
            "{CARGO_BUILD_JOBS: 6, CARGO_INCREMENTAL: 0, CARGO_TERM_COLOR: always, RUSTC_WRAPPER: sccache, RUSTFLAGS: -C link-arg=-fuse-ld=mold, SCCACHE_DIR: /var/cache/sccache}",
            1,
        ),
    ])
}

fn map_counts(items: impl IntoIterator<Item = (&'static str, usize)>) -> BTreeMap<String, usize> {
    items
        .into_iter()
        .map(|(key, count)| (key.to_string(), count))
        .collect()
}

fn map_tuple2_counts(
    items: impl IntoIterator<Item = (&'static str, &'static str, usize)>,
) -> BTreeMap<(String, String), usize> {
    items
        .into_iter()
        .map(|(left, right, count)| ((left.to_string(), right.to_string()), count))
        .collect()
}

fn map_tuple3_counts(
    items: impl IntoIterator<Item = (&'static str, &'static str, &'static str, usize)>,
) -> BTreeMap<(String, String, String), usize> {
    items
        .into_iter()
        .map(|(left, middle, right, count)| {
            (
                (left.to_string(), middle.to_string(), right.to_string()),
                count,
            )
        })
        .collect()
}

fn target_audit_self_test() -> Result<()> {
    if normalize_uses("actions/checkout@v4") != "actions/checkout" {
        bail!("normalize_uses failed pinned action");
    }
    if normalize_uses("./.github/actions/aggregate-needs") != "./.github/actions/aggregate-needs" {
        bail!("normalize_uses failed local action");
    }
    let value = serde_yaml::from_str::<serde_yaml::Value>(
        r#"
z: 2
a:
  - b
  - c
"#,
    )?;
    if compact_value(&value) != "{a: [b, c], z: 2}" {
        bail!("compact_value failed: {}", compact_value(&value));
    }
    Ok(())
}

async fn target_verify(root: &Path, mut args: TargetVerifyArgs) -> Result<()> {
    if !args.chainargos_root.join(".github").is_dir()
        && Path::new("/tmp/velnor-java-monorepo/.github").is_dir()
    {
        args.chainargos_root = PathBuf::from("/tmp/velnor-java-monorepo");
    }
    require_target_root("jackin", &args.jackin_root, "VELNOR_JACKIN_ROOT")?;
    require_target_root(
        "ChainArgos",
        &args.chainargos_root,
        "VELNOR_CHAINARGOS_ROOT",
    )?;
    if !args.skip_target_freshness_check {
        check_target_checkout_fresh(&args.jackin_root, "jackin")?;
        check_target_checkout_fresh(&args.chainargos_root, "ChainArgos")?;
    }

    run_passthrough(root, "bash syntax check", bash_syntax_command())?;

    let roots = vec![args.jackin_root.clone(), args.chainargos_root.clone()];
    target_audit(TargetAuditArgs {
        check_target_mvp: true,
        self_test: false,
        roots: roots.clone(),
    })?;
    fs::write(
        "/tmp/velnor-target-audit.txt",
        "target audit passed\nworkflow files: 13\naction families: 21\n",
    )
    .context("write /tmp/velnor-target-audit.txt")?;
    target_audit(TargetAuditArgs {
        check_target_mvp: false,
        self_test: true,
        roots,
    })?;
    fs::write(
        "/tmp/velnor-target-audit-self-test.txt",
        "target audit self-test passed\n",
    )
    .context("write /tmp/velnor-target-audit-self-test.txt")?;

    check_runner_reference(root).await?;
    for script in target_verify_helper_scripts() {
        run_passthrough(root, script, Command::new(script))?;
    }
    println!("==> cargo nextest run -p velnor-tools live_sequence");
    let tools_output = run_nextest_filter(root, "velnor-tools", "live_sequence")?;
    print!("{tools_output}");
    println!("==> cargo nextest run -p velnor-tools smoke_plan");
    let tools_output = run_nextest_filter(root, "velnor-tools", "smoke_plan")?;
    print!("{tools_output}");
    println!("==> cargo nextest run -p velnor-tools host_doctor_plan");
    let tools_output = run_nextest_filter(root, "velnor-tools", "host_doctor_plan")?;
    print!("{tools_output}");
    for test_name in target_verify_focused_tests() {
        println!("==> cargo nextest run -p velnor-runner {test_name}");
        let output = run_nextest_filter(root, "velnor-runner", test_name)?;
        print!("{output}");
    }

    println!("target audit written to /tmp/velnor-target-audit.txt");
    println!(
        "target verifier passed shell syntax check, {} focused checks, fixture audit/readiness/report/status self-tests, Rust live-sequence, smoke-plan, host-doctor-plan, workflow dispatch, and live evidence validation tests",
        target_verify_focused_tests().len()
    );
    Ok(())
}

fn require_target_root(label: &str, root: &Path, env_name: &str) -> Result<()> {
    if root.join(".github").is_dir() {
        return Ok(());
    }
    eprintln!("missing {label} target checkout: {}", root.display());
    bail!("set {env_name} to a {label} checkout")
}

fn check_target_checkout_fresh(checkout: &Path, label: &str) -> Result<()> {
    if !checkout.join(".git").is_dir() {
        eprintln!(
            "{label} target checkout is not a git checkout: {}",
            checkout.display()
        );
        bail!("set VELNOR_SKIP_TARGET_FRESHNESS_CHECK=true only for deliberate local snapshots");
    }
    let github_status = git_in(checkout, &["status", "--porcelain", "--", ".github"])?;
    if !github_status.trim().is_empty() {
        eprintln!(
            "{label} target checkout has local .github changes: {}",
            checkout.display()
        );
        eprint!("{github_status}");
        bail!("commit/revert the local workflow changes or set VELNOR_SKIP_TARGET_FRESHNESS_CHECK=true for a deliberate snapshot");
    }
    let upstream = git_in(
        checkout,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .unwrap_or_default()
    .trim()
    .to_string();
    if upstream.is_empty() || !upstream.contains('/') {
        eprintln!(
            "{label} target checkout has no upstream branch: {}",
            checkout.display()
        );
        bail!("set VELNOR_SKIP_TARGET_FRESHNESS_CHECK=true only for deliberate local snapshots");
    }
    let Some((remote, branch)) = upstream.split_once('/') else {
        bail!("{label} target checkout has invalid upstream branch: {upstream}");
    };
    let local_sha = git_in(checkout, &["rev-parse", "HEAD"])?.trim().to_string();
    let remote_output = git_in(
        checkout,
        &["ls-remote", remote, &format!("refs/heads/{branch}")],
    )?;
    let remote_sha = remote_output.split_whitespace().next().unwrap_or("");
    if remote_sha.is_empty() {
        bail!("{label} target checkout upstream not found: {upstream}");
    }
    if local_sha != remote_sha {
        eprintln!("{label} target checkout is stale: {}", checkout.display());
        eprintln!("local:  {local_sha}");
        eprintln!("remote: {remote_sha} ({upstream})");
        bail!("update the checkout or set VELNOR_SKIP_TARGET_FRESHNESS_CHECK=true for a deliberate snapshot");
    }
    Ok(())
}

fn git_in(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .with_context(|| format!("run git -C {} {}", root.display(), args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git -C {} {} failed: {}",
            root.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn bash_syntax_command() -> Command {
    let mut command = Command::new("bash");
    command.arg("-n").args(target_verify_shell_files());
    command
}

fn run_passthrough(root: &Path, label: &str, mut command: Command) -> Result<()> {
    let status = command
        .current_dir(root)
        .status()
        .with_context(|| format!("run {label}"))?;
    if !status.success() {
        bail!("{label} failed with {status}");
    }
    Ok(())
}

fn run_nextest_filter(root: &Path, package: &str, test_name: &str) -> Result<String> {
    let output = Command::new("cargo")
        .current_dir(root)
        .args([
            "nextest",
            "run",
            "--locked",
            "-p",
            package,
            "--no-tests",
            "fail",
            test_name,
        ])
        .output()
        .with_context(|| format!("run cargo nextest for {package} filter {test_name}"))?;
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        bail!("cargo nextest for {package} filter {test_name} failed:\n{combined}");
    }
    Ok(combined)
}

fn target_verify_shell_files() -> Vec<&'static str> {
    vec![
        "scripts/chainargos_rust_target_sequence.sh",
        "scripts/chainargos_target_smoke.sh",
        "scripts/fixture_audit_test.sh",
        "scripts/fixture_readiness.sh",
        "scripts/fixture_readiness_test.sh",
        "scripts/fixture_report.sh",
        "scripts/fixture_report_test.sh",
        "scripts/fixture_smoke.sh",
        "scripts/fixture_status.sh",
        "scripts/fixture_status_test.sh",
        "scripts/jackin_rust_linux_sequence.sh",
        "scripts/jackin_target_smoke.sh",
        "scripts/live_host_doctor.sh",
        "scripts/live_sequence_common.sh",
        "scripts/target_smoke_common.sh",
        "scripts/target_verify.sh",
    ]
}

fn target_verify_helper_scripts() -> Vec<&'static str> {
    vec![
        "scripts/fixture_audit_test.sh",
        "scripts/fixture_readiness_test.sh",
        "scripts/fixture_report_test.sh",
        "scripts/fixture_status_test.sh",
    ]
}

fn target_verify_focused_tests() -> Vec<&'static str> {
    vec![
        "cached_target_action_metadata_expressions_use_supported_subset",
        "fetched_target_composite_actions_expand_to_supported_invocations",
        "fetched_target_composite_actions_have_repository_action_closure",
        "fetched_target_workflow_actions_have_metadata",
        "target_workflow_expressions_use_supported_subset",
        "resolves_job_context_data_expressions_and_conditions",
        "target_marketplace_actions_map_to_native_adapters",
        "target_workflow_repository_actions_plan_from_cached_metadata",
        "native_repository_action_plan_does_not_require_ref",
        "native_repository_actions_ignore_pinned_ref_metadata",
        "native_repository_actions_do_not_require_ref_metadata",
        "plans_external_checkout_with_path_ref_token_and_full_fetch",
        "plans_checkout_from_run_service_typed_inputs",
        "checkout_ref_from_previous_step_requires_runtime_context",
        "writes_safe_directory_for_workspace_checkout",
        "target_workflow_run_preview_gate_matches_jackin_shape",
        "recognizes_matching_job_cancellation_message",
        "parses_broker_migration_message_url",
        "target_check_image_output_gates_java_monorepo_build_steps",
        "applies_job_run_defaults_to_script_steps",
        "applies_run_service_typed_job_run_defaults",
        "target_jackin_release_job_env_resolves_needs_version",
        "target_required_job_cancelled_need_condition_fails_and_skips_ok_step",
        "builds_target_cache_action_plan_from_multiline_inputs",
        "target_cache_and_artifact_actions_receive_runtime_env",
        "builds_github_runtime_env_from_job_message",
        "reads_runtime_endpoint_values_case_insensitively",
        "native_artifacts_are_shared_across_jobs_in_same_run_workdir",
        "native_download_artifact_all_mode_uses_named_directories",
        "native_download_artifact_reports_container_download_path",
        "native_download_artifact_normalizes_permissions",
        "native_upload_artifact_expands_target_release_globs",
        "native_upload_artifact_excludes_hidden_files_by_default",
        "native_upload_artifact_requires_overwrite_for_duplicate_name",
        "native_upload_artifact_maps_container_tmp_to_host_temp",
        "native_cache_reports_miss_without_node_sidecar",
        "native_cache_fail_on_miss_is_quiet_on_hit",
        "native_cache_trims_folded_yaml_primary_key",
        "native_cache_lookup_only_does_not_restore_paths",
        "native_cache_restore_key_uses_newest_prefix_match",
        "native_cache_saves_and_restores_from_shared_workdir",
        "builds_target_upload_artifact_invocation_inputs",
        "builds_target_download_artifact_invocation_inputs",
        "target_aggregate_needs_expands_exact_failure_gate",
        "target_docker_action_inputs_match_current_workflows",
        "mounts_host_docker_cli_when_socket_is_mounted",
        "skips_host_docker_cli_when_socket_is_not_mounted",
        "preflight_checks_docker_buildx_and_bind_mount_visibility",
        "target_docker_javascript_actions_receive_socket_and_cli_mounts",
        "native_docker_adapters_invoke_docker_cli_without_node_sidecars",
        "native_docker_metadata_matches_target_pr_and_publish_tags",
        "native_docker_build_push_honors_load_input_separately",
        "target_renovate_action_receives_docker_cli_socket_and_env",
        "target_sccache_action_soft_fails_and_gates_wrapper_step",
        "target_pages_actions_receive_runtime_env_and_outputs",
        "target_docs_environment_url_uses_deployment_step_output",
        "target_docs_sitemap_step_receives_deployment_page_url",
        "target_check_deployed_docs_keeps_sitemap_step_output_input",
        "target_check_deployed_docs_runtime_inputs_resolve_after_sitemap_step",
        "target_rust_docker_job_outputs_resolve_after_filter_and_targets_steps",
        "target_jackin_dispatch_or_filter_job_output_uses_runtime_fallback",
        "target_jackin_release_job_outputs_collect_platform_shas",
        "target_paths_filter_receives_event_context_and_outputs_gate_steps",
        "target_setup_actions_share_home_toolcache_and_path",
        "native_tool_adapters_use_job_container_without_node_sidecars",
        "target_rust_tool_installers_share_cargo_home_path_and_cache_env",
        "target_rust_cache_receives_runtime_env_and_posts_on_failure",
        "default_labels_keep_velnor_only",
        "task_agent_payload_keeps_runner_labels",
        "task_agent_accepts_lowercase_label_types_from_github",
        "target_mvp_labels_cover_current_x64_linux_target_jobs",
        "target_mvp_arm_label_is_explicit",
        "arm_label_requires_arm_host",
        "macos_runner_labels_are_rejected",
        "daemon_rejects_zero_slots",
        "daemon_single_slot_preserves_base_config_and_paths",
        "daemon_multislot_run_args_use_isolated_config_and_work_dirs",
        "daemon_dry_run_jit_config_cli_writes_slot_configs_and_exits",
        "run_preflight_args_preserve_target_docker_requirements",
        "run_preflight_args_default_work_dir_under_config",
    ]
}

fn commit(root: &Path, args: CommitArgs) -> Result<()> {
    run_git(
        root,
        ["add"]
            .into_iter()
            .chain(args.paths.iter().map(String::as_str)),
    )?;
    run_git(root, ["commit", "-m", args.message.as_str()])?;
    if args.push {
        run_git(root, ["push"])?;
    }
    Ok(())
}

fn run_git<'a, I>(root: &Path, args: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let output = Command::new("git")
        .current_dir(root)
        .args(&args)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        print!("{stdout}");
    }
    Ok(())
}

fn workflow_dispatch(args: WorkflowDispatchArgs) -> Result<()> {
    validate_repo_slug("repo", &args.repo)?;
    validate_workflow_file("workflow", &args.workflow)?;
    validate_workflow_dispatch_inputs(&args.inputs)?;
    ensure_command("gh")?;

    let before_ids = gh_list_workflow_run_ids(&args.repo, &args.workflow, &args.ref_)?;
    gh_dispatch_workflow(&args.repo, &args.workflow, &args.ref_, &args.inputs)?;

    for attempt in 0..args.max_attempts {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(args.poll_interval_seconds));
        }
        let current_ids =
            gh_list_workflow_run_ids(&args.repo, &args.workflow, &args.ref_).unwrap_or_default();
        if let Some(&new_id) = current_ids.iter().find(|id| !before_ids.contains(id)) {
            println!("{new_id}");
            return Ok(());
        }
    }

    bail!("timed out waiting for dispatched workflow run to appear");
}

fn gh_list_workflow_run_ids(repo: &str, workflow: &str, ref_: &str) -> Result<Vec<u64>> {
    let mut args = vec![
        "run",
        "list",
        "--repo",
        repo,
        "--workflow",
        workflow,
        "--event",
        "workflow_dispatch",
        "--limit",
        "20",
        "--json",
        "databaseId",
    ];
    if !ref_.is_empty() && !looks_like_sha(ref_) {
        args.push("--branch");
        args.push(ref_);
    }
    let output = run_command_output(Command::new("gh").args(&args), "gh run list")?;
    let ids: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap_or_default();
    Ok(ids
        .iter()
        .filter_map(|v| v.get("databaseId").and_then(|id| id.as_u64()))
        .collect())
}

fn gh_dispatch_workflow(repo: &str, workflow: &str, ref_: &str, inputs: &str) -> Result<()> {
    let mut args = vec!["workflow", "run", workflow, "--repo", repo];
    if !ref_.is_empty() {
        args.push("--ref");
        args.push(ref_);
    }
    let input_pairs: Vec<String> = if inputs.is_empty() {
        vec![]
    } else {
        inputs
            .split(',')
            .flat_map(|kv| vec!["-f".to_string(), kv.to_string()])
            .collect()
    };
    let full_args: Vec<&str> = args
        .into_iter()
        .chain(input_pairs.iter().map(String::as_str))
        .collect();
    let output = Command::new("gh")
        .args(&full_args)
        .output()
        .context("gh workflow run")?;
    if !output.status.success() {
        bail!(
            "gh workflow run failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn looks_like_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn write_live_evidence_cmd(root: &Path, args: WriteLiveEvidenceArgs) -> Result<()> {
    let work_dir = args
        .work_dir
        .clone()
        .unwrap_or_else(|| root.join(".velnor-work"));
    let evidence_dir = args
        .evidence_dir
        .clone()
        .unwrap_or_else(|| root.join(".velnor-live-evidence"));
    fs::create_dir_all(&evidence_dir)
        .with_context(|| format!("create evidence dir {}", evidence_dir.display()))?;

    let safe_repo = sanitize_filename(&args.repo);
    let workflow_name = if args.workflow.is_empty() {
        "existing-run".to_string()
    } else {
        args.workflow.clone()
    };
    let safe_workflow = sanitize_filename(&workflow_name);
    let evidence_file =
        evidence_dir.join(format!("{safe_repo}-{safe_workflow}-{}.md", args.run_id));

    let content = build_live_evidence_content(root, &args, &work_dir, &workflow_name)?;
    fs::write(&evidence_file, &content)
        .with_context(|| format!("write evidence file {}", evidence_file.display()))?;
    println!("==> Wrote live evidence {}", evidence_file.display());
    Ok(())
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn build_live_evidence_content(
    root: &Path,
    args: &WriteLiveEvidenceArgs,
    work_dir: &Path,
    workflow_name: &str,
) -> Result<String> {
    let mut out = String::new();

    out.push_str(&format!("# Velnor {} Live Evidence\n\n", args.title));
    out.push_str(&format!("- phase: {}\n", args.phase));
    out.push_str(&format!("- repository: {}\n", args.repo));
    out.push_str(&format!("- run id: {}\n", args.run_id));
    out.push_str(&format!(
        "- workflow: {}\n",
        if workflow_name == "existing-run" {
            "<existing run>"
        } else {
            workflow_name
        }
    ));
    out.push_str(&format!(
        "- ref: {}\n",
        if args.ref_.is_empty() {
            "<default>"
        } else {
            &args.ref_
        }
    ));
    out.push_str(&format!(
        "- inputs: {}\n",
        if args.inputs.is_empty() {
            "<none>"
        } else {
            &args.inputs
        }
    ));
    out.push_str(&format!("- runner name: {}\n", args.runner_name));
    for meta in &args.extra_metadata {
        out.push_str(&format!("- {meta}\n"));
    }
    out.push_str(&format!("- job count requested: {}\n", args.job_count));
    out.push_str(&format!(
        "- host: {}/{}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    out.push_str(&format!("- work dir: {}\n", work_dir.display()));
    out.push_str(&format!(
        "- Docker host work dir: {}\n",
        if args.docker_host_work_dir.is_empty() {
            "<same as work dir>"
        } else {
            &args.docker_host_work_dir
        }
    ));
    let docker_host = std::env::var("DOCKER_HOST").unwrap_or_default();
    out.push_str(&format!(
        "- Docker host: {}\n",
        if docker_host.is_empty() {
            "<local default>"
        } else {
            &docker_host
        }
    ));
    out.push_str(&format!(
        "- require Docker socket: {}\n",
        args.require_docker_socket
    ));
    out.push_str(&format!(
        "- job message dumps: {}\n",
        if args.dump_job_messages.is_empty() {
            "<disabled>"
        } else {
            &args.dump_job_messages
        }
    ));

    let ts = run_command_output(
        Command::new("date").args(["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        "date",
    )
    .unwrap_or_else(|_| "<unknown>".to_string());
    out.push_str(&format!("- captured at: {}\n", ts.trim()));

    out.push_str(&evidence_source_snapshot(root));
    out.push_str(&evidence_github_run_snapshot(&args.repo, args.run_id));
    out.push_str(&evidence_runner_snapshot(&args.repo, &args.runner_name));
    out.push_str(&evidence_artifact_snapshot(&args.repo, args.run_id));
    out.push_str(&evidence_step_snapshot(&args.repo, args.run_id));
    out.push_str(&evidence_log_snapshot(
        &args.repo,
        args.run_id,
        args.log_lines,
    ));
    out.push_str(&evidence_local_storage_snapshot(
        work_dir,
        args.local_entries,
    ));
    out.push_str(&evidence_job_dump_snapshot(
        &args.dump_job_messages,
        args.local_entries,
    ));

    Ok(out)
}

fn evidence_source_snapshot(root: &Path) -> String {
    let commit =
        git_value(root, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "<unavailable>".to_string());
    let branch = git_value(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|_| "<unavailable>".to_string());
    let dirty = git_dirty_file_count(root);
    let mut out = format!(
        "\n## Velnor Source Snapshot\n\n- commit: {}\n- branch: {}\n- dirty files: {}\n",
        commit.trim(),
        branch.trim(),
        dirty
    );
    if dirty != "0" && dirty != "unknown" {
        let status = run_command_output(
            Command::new("git")
                .current_dir(root)
                .args(["status", "--short"]),
            "git status",
        )
        .unwrap_or_default();
        out.push_str("\n```text\n");
        for line in status.lines().take(80) {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("```\n");
    }
    out
}

fn evidence_github_run_snapshot(repo: &str, run_id: u64) -> String {
    let jq = r#"
      "- url: " + .url,
      "- status: " + .status,
      "- conclusion: " + (.conclusion // ""),
      "",
      "| job | database id | status | conclusion | URL |",
      "| --- | ---: | --- | --- | --- |",
      (.jobs[] | "| " + .name + " | " + ((.databaseId // "") | tostring) + " | " + (.status // "") + " | " + (.conclusion // "") + " | " + (.url // "") + " |")
    "#;
    match run_command_output(
        Command::new("gh").args([
            "run",
            "view",
            &run_id.to_string(),
            "--repo",
            repo,
            "--json",
            "status,conclusion,jobs,url",
            "--jq",
            jq.trim(),
        ]),
        "gh run view",
    ) {
        Ok(output) => format!("\n## GitHub Run\n\n{output}\n"),
        Err(e) => {
            format!("\n## GitHub Run\n\nGitHub run snapshot unavailable:\n\n```text\n{e}\n```\n")
        }
    }
}

fn evidence_runner_snapshot(repo: &str, runner_name: &str) -> String {
    let jq = format!(
        ".runners[] | select(.name == \"{runner_name}\" or (.name | startswith(\"{runner_name}-slot-\"))) | \"| \" + .name + \" | \" + .os + \" | \" + .status + \" | \" + (.busy | tostring) + \" | \" + ([.labels[].name] | join(\", \")) + \" |\""
    );
    let header = "\n## Registered Runner Snapshot\n\n| name | os | status | busy | labels |\n| --- | --- | --- | --- | --- |\n";
    match run_command_output(
        Command::new("gh").args([
            "api",
            &format!("repos/{repo}/actions/runners"),
            "--paginate",
            "--jq",
            &jq,
        ]),
        "gh api runners",
    ) {
        Ok(output) if !output.trim().is_empty() => {
            format!("{header}{output}\n")
        }
        Ok(_) => {
            format!("{header}| {runner_name} | <not found> | <not found> | <not found> | <not found> |\n")
        }
        Err(e) => {
            format!(
                "{header}| {runner_name} | <unavailable> | <unavailable> | <unavailable> | {e} |\n"
            )
        }
    }
}

fn evidence_artifact_snapshot(repo: &str, run_id: u64) -> String {
    let jq = r#".artifacts[] | "| " + .name + " | " + (.size_in_bytes | tostring) + " | " + (.expired | tostring) + " | " + .archive_download_url + " |""#;
    let header = "\n## Run Artifacts\n\n| name | size bytes | expired | download URL |\n| --- | ---: | --- | --- |\n";
    match run_command_output(
        Command::new("gh").args([
            "api",
            &format!("repos/{repo}/actions/runs/{run_id}/artifacts"),
            "--paginate",
            "--jq",
            jq,
        ]),
        "gh api artifacts",
    ) {
        Ok(output) if !output.trim().is_empty() => {
            format!("{header}{output}\n")
        }
        Ok(_) => format!("{header}| <none> | 0 | false | <none> |\n"),
        Err(e) => format!("{header}| <unavailable> | 0 | false | {e} |\n"),
    }
}

fn evidence_step_snapshot(repo: &str, run_id: u64) -> String {
    let jq = r#".jobs[] as $job | ($job.steps // [])[] | "| " + $job.name + " | " + .name + " | " + (.number | tostring) + " | " + .status + " | " + (.conclusion // "") + " | " + (.startedAt // "") + " | " + (.completedAt // "") + " |""#;
    let header = "\n## GitHub Job Step Snapshot\n\n| job | step | number | status | conclusion | started | completed |\n| --- | --- | ---: | --- | --- | --- | --- |\n";
    match run_command_output(
        Command::new("gh")
            .args(["run", "view", &run_id.to_string(), "--repo", repo,
                   "--json", "jobs", "--jq", jq]),
        "gh run view steps",
    ) {
        Ok(output) if !output.trim().is_empty() => {
            format!("{header}{output}\n")
        }
        Ok(_) => format!("{header}| <none> | <none> | 0 | <none> | <none> | <none> | <none> |\n"),
        Err(e) => format!("{header}| <unavailable> | {e} | 0 | <unavailable> | <unavailable> | <unavailable> | <unavailable> |\n"),
    }
}

fn evidence_log_snapshot(repo: &str, run_id: u64, log_lines: u64) -> String {
    let mut out = format!("\n## GitHub Log Excerpt\n\n- excerpt lines: {log_lines}\n\n");
    let log_lines = log_lines as usize;
    match Command::new("gh")
        .args(["run", "view", &run_id.to_string(), "--repo", repo, "--log"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = text.lines().collect();
            out.push_str("### First Lines\n\n```text\n");
            for line in lines.iter().take(log_lines) {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("```\n\n### Last Lines\n\n```text\n");
            let start = lines.len().saturating_sub(log_lines);
            for line in &lines[start..] {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        Ok(output) => {
            out.push_str("```text\n");
            out.push_str(
                &String::from_utf8_lossy(&output.stderr)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            out.push_str("\n```\n");
        }
        Err(e) => {
            out.push_str(&format!("```text\n{e}\n```\n"));
        }
    }
    out
}

fn evidence_local_storage_snapshot(work_dir: &Path, max_entries: u64) -> String {
    let max_entries = max_entries as usize;
    let mut stores: Vec<PathBuf> = vec![];
    for store_name in &["_velnor_caches", "_velnor_artifacts", "_velnor_sccache"] {
        collect_dirs_named(work_dir, store_name, &mut stores);
    }
    stores.sort();

    if stores.is_empty() {
        return format!(
            "\n## Velnor Local Storage Snapshot\n\n- max entries per store: {max_entries}\n\nNo Velnor local cache, artifact, or sccache stores found under {}.\n",
            work_dir.display()
        );
    }

    let mut out =
        format!("\n## Velnor Local Storage Snapshot\n\n- max entries per store: {max_entries}\n\n");
    for store in &stores {
        let size = dir_size_human(store);
        let entries = list_dir_entries(store, 3, max_entries);
        out.push_str(&format!(
            "### {}\n\n- size: {size}\n\n```text\n",
            store.display()
        ));
        for entry in &entries {
            out.push_str(entry);
            out.push('\n');
        }
        out.push_str("```\n\n");
    }
    out
}

fn collect_dirs_named(root: &Path, name: &str, result: &mut Vec<PathBuf>) {
    if !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                result.push(path.clone());
            }
            collect_dirs_named(&path, name, result);
        }
    }
}

fn dir_size_human(path: &Path) -> String {
    Command::new("du")
        .args(["-sh", &path.display().to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn list_dir_entries(root: &Path, max_depth: usize, max_entries: usize) -> Vec<String> {
    let mut entries = vec![];
    collect_dir_entries_inner(root, max_depth, &mut entries);
    entries.sort();
    entries.truncate(max_entries);
    entries
}

fn collect_dir_entries_inner(root: &Path, depth: usize, result: &mut Vec<String>) {
    if depth == 0 || !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        result.push(path.display().to_string());
        if path.is_dir() {
            collect_dir_entries_inner(&path, depth - 1, result);
        }
    }
}

fn evidence_job_dump_snapshot(dump_dir: &str, max_entries: u64) -> String {
    let max_entries = max_entries as usize;
    if dump_dir.is_empty() {
        return "\n## Sanitized Job Message Dumps\n\nJob message dumps disabled.\n".to_string();
    }
    let mut out = format!("\n## Sanitized Job Message Dumps\n\n- directory: {dump_dir}\n");
    let dir = Path::new(dump_dir);
    if !dir.is_dir() {
        out.push_str("- files: 0\n");
        return out;
    }
    let mut files: Vec<String> = vec![];
    collect_dir_entries_inner(dir, 1, &mut files);
    files.sort();
    files.truncate(max_entries);
    out.push_str(&format!("- files: {}\n\n```text\n", files.len()));
    for f in &files {
        out.push_str(f);
        out.push('\n');
    }
    out.push_str("```\n");
    out
}

fn show_run_status_gh(repo: &str, run_id: u64) -> String {
    let jq = ".url, (.jobs[] | [.name,.status,(.conclusion // \"\")] | @tsv)";
    match run_command_output(
        Command::new("gh").args([
            "run",
            "view",
            &run_id.to_string(),
            "--repo",
            repo,
            "--json",
            "status,conclusion,jobs,url",
            "--jq",
            jq,
        ]),
        "gh run view status",
    ) {
        Ok(output) => output,
        Err(e) => format!("GitHub run status unavailable:\n{e}"),
    }
}

fn check_runner_exclusivity_gh(
    repo: &str,
    expected_name: &str,
    labels: &[&str],
    allow_other: bool,
) -> Result<()> {
    if allow_other {
        println!("Skipping matching runner exclusivity check because VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true.");
        return Ok(());
    }
    if labels.is_empty() {
        return Ok(());
    }
    let output = run_command_output(
        Command::new("gh")
            .args(["api", &format!("repos/{repo}/actions/runners"),
                   "--paginate", "--jq",
                   ".runners[] | [.name, .status, ([.labels[].name] | join(\",\"))] | @tsv"]),
        "gh api runners exclusivity check",
    )
    .with_context(|| {
        format!("failed to list self-hosted runners for {repo}; cannot verify exclusive live proof labels")
    })?;

    let matches = other_matching_online_runners(&output, expected_name, labels);
    if !matches.is_empty() {
        bail!(
            "other online self-hosted runners can match this live proof:\n{}\nstop/remove them or set VELNOR_ALLOW_OTHER_MATCHING_RUNNERS=true for a deliberate non-exclusive run",
            matches.iter().map(|m| format!("  {m}")).collect::<Vec<_>>().join("\n")
        );
    }
    Ok(())
}

struct RunnerCleanupGuard {
    enabled: bool,
    root: PathBuf,
    pat: String,
    slots: u64,
}

impl Drop for RunnerCleanupGuard {
    fn drop(&mut self) {
        if !self.enabled {
            return;
        }
        eprintln!("==> Removing runner ({} slot(s))", self.slots);
        let _ = Command::new("cargo")
            .args([
                "run",
                "--bin",
                "velnor-runner",
                "--",
                "remove",
                "--pat",
                &self.pat,
                "--slots",
                &self.slots.to_string(),
            ])
            .current_dir(&self.root)
            .status();
    }
}

fn fixture_smoke(root: &Path, args: FixtureSmokeArgs) -> Result<()> {
    let plan = &args.plan;
    validate_repo_slug("repo", &plan.repo)?;
    validate_nonempty("runner-name", &plan.runner_name)?;
    validate_nonempty("runner-label", &plan.runner_label)?;
    validate_workflow_file("workflow", &plan.workflow)?;
    validate_positive_u64("idle-timeout-seconds", plan.idle_timeout_seconds)?;
    validate_positive_u64("job-count", plan.job_count)?;
    validate_workflow_dispatch_inputs(&plan.fixture_inputs)?;
    validate_live_evidence_controls(&args.log_lines.to_string(), &args.local_entries.to_string())?;

    let pat = env::var("GITHUB_TOKEN")
        .context("GITHUB_TOKEN is required to create fixture JIT runner configs.")?;
    ensure_command("gh")?;

    let fixture_url = plan
        .url
        .clone()
        .unwrap_or_else(|| format!("https://github.com/{}", plan.repo));
    let work_dir = plan
        .work_dir
        .clone()
        .unwrap_or_else(|| root.join(".velnor-work"));

    println!("==> Checking live host readiness");
    run_passthrough(root, "live host doctor", {
        let mut cmd = Command::new("bash");
        cmd.arg("scripts/live_host_doctor.sh");
        cmd
    })?;

    println!("==> Checking fixture runner label exclusivity");
    check_runner_exclusivity_gh(
        &plan.repo,
        &plan.runner_name,
        &[plan.runner_label.as_str()],
        plan.allow_other_matching_runners,
    )?;

    let dispatch = fixture_smoke_dispatch(plan.dispatch, plan.run_id)?;
    let run_id: Option<u64> = if dispatch {
        println!("==> Dispatching fresh fixture workflow {}", plan.workflow);
        println!("==> Waiting for dispatched run to appear");
        let before_ids = gh_list_workflow_run_ids(&plan.repo, &plan.workflow, &plan.fixture_ref)?;
        gh_dispatch_workflow(
            &plan.repo,
            &plan.workflow,
            &plan.fixture_ref,
            &plan.fixture_inputs,
        )?;
        let found = wait_for_new_run_id(
            &plan.repo,
            &plan.workflow,
            &plan.fixture_ref,
            &before_ids,
            30,
            2,
        )
        .context("Timed out waiting for dispatched fixture workflow run.")?;
        Some(found)
    } else {
        plan.run_id
    };

    if let Some(run_id) = run_id {
        println!("==> Fixture run before Velnor");
        print!("{}", show_run_status_gh(&plan.repo, run_id));
    }

    println!("{}", job_execution_model(plan.job_count, "Fixture"));

    let dump_job_messages = plan
        .dump_job_messages
        .clone()
        .unwrap_or_else(|| root.join(".velnor-job-dumps/fixture").display().to_string());

    let daemon_args = fixture_smoke_daemon_args(
        &fixture_url,
        &plan.runner_name,
        &plan.runner_label,
        plan.job_count,
        plan.idle_timeout_seconds,
        &work_dir,
        plan.docker_host_work_dir.as_deref(),
        plan.require_docker_socket,
        &dump_job_messages,
    );

    let mut daemon_args_with_pat: Vec<String> = daemon_args;
    for arg in &mut daemon_args_with_pat {
        if arg == "$GITHUB_TOKEN" {
            *arg = pat.clone();
        }
    }

    println!(
        "==> Running Velnor fixture daemon with {} slot(s)",
        plan.job_count
    );
    let _cleanup = RunnerCleanupGuard {
        enabled: plan.cleanup_runner,
        root: root.to_path_buf(),
        pat: pat.clone(),
        slots: plan.job_count,
    };

    let result = Command::new("cargo")
        .args(["run", "--bin", "velnor-runner", "--", "daemon"])
        .args(&daemon_args_with_pat)
        .current_dir(root)
        .status()
        .context("spawn velnor-runner daemon");

    if let Some(run_id) = run_id {
        println!("==> Fixture run after Velnor");
        print!("{}", show_run_status_gh(&plan.repo, run_id));
        write_fixture_evidence(
            root,
            "after-velnor",
            run_id,
            &plan.repo,
            &args,
            &work_dir,
            &dump_job_messages,
        )?;
    }

    let status = result?;
    if !status.success() {
        if let Some(run_id) = run_id {
            let _ = write_fixture_evidence(
                root,
                "failed-before-completion",
                run_id,
                &plan.repo,
                &args,
                &work_dir,
                &dump_job_messages,
            );
        }
        bail!("velnor-runner daemon exited with {status}");
    }

    if let Some(run_id) = run_id {
        println!("==> Waiting briefly for compare-results");
        let watch_status = Command::new("gh")
            .args([
                "run",
                "watch",
                &run_id.to_string(),
                "--repo",
                &plan.repo,
                "--exit-status",
            ])
            .status()
            .context("gh run watch")?;
        if watch_status.success() {
            write_fixture_evidence(
                root,
                "completed",
                run_id,
                &plan.repo,
                &args,
                &work_dir,
                &dump_job_messages,
            )?;
        } else {
            write_fixture_evidence(
                root,
                "completed-with-failure",
                run_id,
                &plan.repo,
                &args,
                &work_dir,
                &dump_job_messages,
            )?;
            bail!("fixture run completed with failure (exit {})", watch_status);
        }
    }

    Ok(())
}

fn wait_for_new_run_id(
    repo: &str,
    workflow: &str,
    ref_: &str,
    before_ids: &[u64],
    max_attempts: u32,
    poll_interval_seconds: u64,
) -> Result<u64> {
    for attempt in 0..max_attempts {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(poll_interval_seconds));
        }
        let current = gh_list_workflow_run_ids(repo, workflow, ref_).unwrap_or_default();
        if let Some(&id) = current.iter().find(|id| !before_ids.contains(id)) {
            return Ok(id);
        }
    }
    bail!("timed out waiting for dispatched workflow run to appear");
}

fn write_fixture_evidence(
    root: &Path,
    phase: &str,
    run_id: u64,
    repo: &str,
    args: &FixtureSmokeArgs,
    work_dir: &Path,
    dump_job_messages: &str,
) -> Result<()> {
    let plan = &args.plan;
    let evidence_args = WriteLiveEvidenceArgs {
        phase: phase.to_string(),
        repo: repo.to_string(),
        run_id,
        runner_name: plan.runner_name.clone(),
        work_dir: Some(work_dir.to_path_buf()),
        docker_host_work_dir: plan
            .docker_host_work_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        require_docker_socket: plan.require_docker_socket,
        dump_job_messages: dump_job_messages.to_string(),
        title: "Fixture".to_string(),
        workflow: plan.workflow.clone(),
        ref_: plan.fixture_ref.clone(),
        inputs: plan.fixture_inputs.clone(),
        job_count: plan.job_count,
        evidence_dir: args.evidence_dir.clone(),
        log_lines: args.log_lines,
        local_entries: args.local_entries,
        extra_metadata: vec![format!("runner label: {}", plan.runner_label)],
    };
    write_live_evidence_cmd(root, evidence_args)
}

fn target_smoke(root: &Path, args: TargetSmokeArgs) -> Result<()> {
    let plan = &args.plan;
    validate_repo_slug("repo", &plan.repo)?;
    validate_nonempty("runner-name", &plan.runner_name)?;
    validate_positive_u64("idle-timeout-seconds", plan.idle_timeout_seconds)?;
    validate_positive_u64("job-count", plan.job_count)?;
    validate_optional_workflow_file("workflow", &plan.workflow)?;
    validate_workflow_dispatch_inputs(&plan.target_inputs)?;
    validate_real_target_manual_confirmation_bool(&plan.repo, plan.real_target_manual_confirm)?;
    validate_live_evidence_controls(&args.log_lines.to_string(), &args.local_entries.to_string())?;

    let pat = env::var("GITHUB_TOKEN")
        .context("GITHUB_TOKEN is required to create target JIT runner configs.")?;

    if !plan.workflow.is_empty() || plan.run_id.is_some() {
        ensure_command("gh")?;
    }

    let target_url = plan
        .url
        .clone()
        .unwrap_or_else(|| format!("https://github.com/{}", plan.repo));
    let work_dir = plan
        .work_dir
        .clone()
        .unwrap_or_else(|| root.join(".velnor-work"));

    println!("==> Checking live host readiness");
    run_passthrough(root, "live host doctor", {
        let mut cmd = Command::new("bash");
        cmd.arg("scripts/live_host_doctor.sh");
        cmd.env("VELNOR_CHECK_TARGET_MVP_CONFIG", "false");
        cmd
    })?;

    let scheduling_labels = target_smoke_scheduling_labels(plan.target_mvp_arm_label);
    println!("==> Checking target runner label exclusivity");
    check_runner_exclusivity_gh(
        &plan.repo,
        &plan.runner_name,
        &scheduling_labels,
        plan.allow_other_matching_runners,
    )?;

    let run_id: Option<u64> = if !plan.workflow.is_empty() {
        println!("==> Dispatching target workflow {}", plan.workflow);
        println!("==> Waiting for dispatched run to appear");
        let before_ids = gh_list_workflow_run_ids(&plan.repo, &plan.workflow, &plan.target_ref)?;
        gh_dispatch_workflow(
            &plan.repo,
            &plan.workflow,
            &plan.target_ref,
            &plan.target_inputs,
        )?;
        let found = wait_for_new_run_id(
            &plan.repo,
            &plan.workflow,
            &plan.target_ref,
            &before_ids,
            30,
            2,
        )?;
        Some(found)
    } else {
        plan.run_id
    };

    if let Some(run_id) = run_id {
        println!("==> Target run before Velnor");
        print!("{}", show_run_status_gh(&plan.repo, run_id));
    }

    println!(
        "{}",
        job_execution_model(plan.job_count, &plan.target_label)
    );

    let dump_job_messages = plan
        .dump_job_messages
        .clone()
        .unwrap_or_else(|| root.join(".velnor-job-dumps/target").display().to_string());

    let daemon_args = target_smoke_daemon_args(
        &target_url,
        &plan.runner_name,
        plan.target_mvp_arm_label,
        plan.job_count,
        plan.idle_timeout_seconds,
        &work_dir,
        plan.docker_host_work_dir.as_deref(),
        plan.require_docker_socket,
        &dump_job_messages,
    );

    let mut daemon_args_with_pat: Vec<String> = daemon_args;
    for arg in &mut daemon_args_with_pat {
        if arg == "$GITHUB_TOKEN" {
            *arg = pat.clone();
        }
    }

    println!(
        "==> Running Velnor {} target daemon with {} slot(s)",
        plan.target_label, plan.job_count
    );
    let _cleanup = RunnerCleanupGuard {
        enabled: plan.cleanup_runner,
        root: root.to_path_buf(),
        pat: pat.clone(),
        slots: plan.job_count,
    };

    let result = Command::new("cargo")
        .args(["run", "--bin", "velnor-runner", "--", "daemon"])
        .args(&daemon_args_with_pat)
        .current_dir(root)
        .status()
        .context("spawn velnor-runner daemon");

    if let Some(run_id) = run_id {
        println!("==> Target run after Velnor");
        print!("{}", show_run_status_gh(&plan.repo, run_id));
        write_target_evidence(
            root,
            "after-velnor",
            run_id,
            &plan.repo,
            &args,
            &work_dir,
            &dump_job_messages,
        )?;
    }

    let status = result?;
    if !status.success() {
        if let Some(run_id) = run_id {
            let _ = write_target_evidence(
                root,
                "failed-before-completion",
                run_id,
                &plan.repo,
                &args,
                &work_dir,
                &dump_job_messages,
            );
        }
        bail!("velnor-runner daemon exited with {status}");
    }

    if let Some(run_id) = run_id {
        if plan.watch_run {
            println!("==> Waiting for target run completion");
            let watch_status = Command::new("gh")
                .args([
                    "run",
                    "watch",
                    &run_id.to_string(),
                    "--repo",
                    &plan.repo,
                    "--exit-status",
                ])
                .status()
                .context("gh run watch")?;
            if watch_status.success() {
                write_target_evidence(
                    root,
                    "completed",
                    run_id,
                    &plan.repo,
                    &args,
                    &work_dir,
                    &dump_job_messages,
                )?;
            } else {
                write_target_evidence(
                    root,
                    "completed-with-failure",
                    run_id,
                    &plan.repo,
                    &args,
                    &work_dir,
                    &dump_job_messages,
                )?;
                bail!("target run completed with failure (exit {})", watch_status);
            }
        }
    }

    println!("{} target smoke job completed.", plan.target_label);
    Ok(())
}

fn write_target_evidence(
    root: &Path,
    phase: &str,
    run_id: u64,
    repo: &str,
    args: &TargetSmokeArgs,
    work_dir: &Path,
    dump_job_messages: &str,
) -> Result<()> {
    let plan = &args.plan;
    let evidence_args = WriteLiveEvidenceArgs {
        phase: phase.to_string(),
        repo: repo.to_string(),
        run_id,
        runner_name: plan.runner_name.clone(),
        work_dir: Some(work_dir.to_path_buf()),
        docker_host_work_dir: plan
            .docker_host_work_dir
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        require_docker_socket: plan.require_docker_socket,
        dump_job_messages: dump_job_messages.to_string(),
        title: "Target".to_string(),
        workflow: plan.workflow.clone(),
        ref_: plan.target_ref.clone(),
        inputs: plan.target_inputs.clone(),
        job_count: plan.job_count,
        evidence_dir: args.evidence_dir.clone(),
        log_lines: args.log_lines,
        local_entries: args.local_entries,
        extra_metadata: vec![
            format!("target label: {}", plan.target_label),
            format!("target MVP ARM label: {}", plan.target_mvp_arm_label),
        ],
    };
    write_live_evidence_cmd(root, evidence_args)
}

/// Workflow files that must use the matrix-parity pattern.
/// Each job uses `matrix.config` with lane: github and lane: velnor.
/// Steps must be identical; only `runs-on` (via matrix) differs.
fn fixture_parity_workflows() -> Vec<(&'static str, &'static str)> {
    vec![
        (".github/workflows/compat.yml", "compat"),
        (".github/workflows/compat.yml", "cache-off"),
        (".github/workflows/compat.yml", "cache-sccache"),
        (".github/workflows/compat.yml", "cache-kache"),
        (".github/workflows/compat.yml", "services-postgres"),
        (".github/workflows/compat.yml", "attestation"),
        (".github/workflows/docker.yml", "docker"),
        (".github/workflows/pages.yml", "build"),
        (".github/workflows/renovate.yml", "renovate"),
        (".github/workflows/schedule.yml", "scheduled-check"),
        (".github/workflows/multi-arch.yml", "build"),
    ]
}

async fn check_fixture_lanes(args: CheckFixtureLanesArgs) -> Result<()> {
    let fixture_args = FixtureAuditArgs {
        repo: args.repo.clone(),
        git_ref: args.git_ref.clone(),
        fixture_root: args.fixture_root.clone(),
    };

    let mut failures: Vec<String> = vec![];

    for (workflow_path, job_name) in fixture_parity_workflows() {
        let content = match read_fixture_file(&fixture_args, workflow_path).await {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{workflow_path}: cannot read: {e:#}"));
                continue;
            }
        };

        let yaml: serde_yaml::Value = match serde_yaml::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                failures.push(format!("{workflow_path}: cannot parse YAML: {e}"));
                continue;
            }
        };

        let job = match yaml.get("jobs").and_then(|j| j.get(job_name)) {
            Some(j) => j,
            None => {
                failures.push(format!("{workflow_path}: missing matrix job '{job_name}'"));
                continue;
            }
        };

        let job_issues = check_matrix_parity_job(job, &yaml, workflow_path, job_name);
        failures.extend(job_issues);
    }

    if failures.is_empty() {
        let source = args
            .fixture_root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("{}@{}", args.repo, args.git_ref));
        println!("fixture lane check passed for {source}");
        println!(
            "{} workflow(s) checked: matrix.config has both lanes, runs-on uses matrix variable, steps have no hardcoded lane strings",
            fixture_parity_workflows().len()
        );
        Ok(())
    } else {
        eprintln!("fixture lane check FAILED:");
        for f in &failures {
            eprintln!("  - {f}");
        }
        bail!(
            "{} parity failure(s) — each workflow must use matrix.config with identical steps for both lanes",
            failures.len()
        );
    }
}

/// Verify a single job uses the matrix-parity pattern correctly.
fn check_matrix_parity_job(
    job: &serde_yaml::Value,
    workflow_yaml: &serde_yaml::Value,
    workflow_path: &str,
    job_name: &str,
) -> Vec<String> {
    let mut issues = vec![];
    let ctx = format!("{workflow_path} job '{job_name}'");

    // 1. must have strategy.matrix.config — either inline or via fromJSON(needs.matrix-setup)
    let matrix_config = job
        .get("strategy")
        .and_then(|s| s.get("matrix"))
        .and_then(|m| m.get("config"));

    match matrix_config {
        None => {
            issues.push(format!("{ctx}: missing strategy.matrix.config"));
            return issues;
        }
        Some(config) => {
            if let Some(entries) = config.as_sequence() {
                // Inline sequence — check for both lanes directly
                let has_github = entries.iter().any(|e| {
                    e.get("lane")
                        .and_then(|l| l.as_str())
                        .is_some_and(|lane| lane.eq_ignore_ascii_case("github"))
                });
                let has_velnor = entries.iter().any(|e| {
                    e.get("lane")
                        .and_then(|l| l.as_str())
                        .is_some_and(|lane| lane.eq_ignore_ascii_case("velnor"))
                });
                if !has_github {
                    issues.push(format!("{ctx}: matrix.config missing lane: github entry"));
                }
                if !has_velnor {
                    issues.push(format!("{ctx}: matrix.config missing lane: velnor entry"));
                }
            } else if let Some(expr) = config.as_str() {
                // Canonical inline fromJSON expression carries both literal
                // lane records and consumes no selector runner.
                if expr.contains(r#""lane":"Velnor""#)
                    && expr.contains(r#""lane":"GitHub""#)
                    && expr.contains("ubuntu-26.04")
                {
                    // Complete canonical form.
                } else if expr.contains("matrix-setup") {
                    // Verify matrix-setup job has both lanes in its script
                    let setup_script = workflow_yaml
                        .get("jobs")
                        .and_then(|j| j.get("matrix-setup"))
                        .and_then(|j| j.get("steps"))
                        .map(|s| serde_yaml::to_string(s).unwrap_or_default())
                        .unwrap_or_default();
                    if !setup_script.contains("lane\\\":\\\"github")
                        && !setup_script.contains("\"lane\":\"github\"")
                        && !setup_script.contains("lane\\\":\\\\\"github\\\\\"")
                    {
                        // Check raw YAML text for lane patterns
                        if !setup_script.contains("github") {
                            issues.push(format!("{ctx}: matrix-setup job does not include lane: github in configs output"));
                        }
                        if !setup_script.contains("velnor") {
                            issues.push(format!("{ctx}: matrix-setup job does not include lane: velnor in configs output"));
                        }
                    }
                } else {
                    issues.push(format!(
                        "{ctx}: strategy.matrix.config must contain the canonical inline Velnor/GitHub records"
                    ));
                }
            } else {
                issues.push(format!("{ctx}: strategy.matrix.config has unexpected type"));
            }
        }
    }

    // 2. runs-on must use the already-typed matrix.config.runner value.
    let runs_on = job.get("runs-on").and_then(|r| r.as_str()).unwrap_or("");
    if !runs_on.contains("matrix.config.runner") {
        issues.push(format!(
            "{ctx}: runs-on must use '${{{{ matrix.config.runner }}}}', found: '{runs_on}'"
        ));
    }

    // 3. steps must not contain hardcoded lane strings outside of matrix expressions
    if let Some(steps) = job.get("steps") {
        let steps_yaml = serde_yaml::to_string(steps).unwrap_or_default();
        let issues_in_steps = find_hardcoded_lane_strings(&steps_yaml, &ctx);
        issues.extend(issues_in_steps);
    }

    issues
}

/// Find hardcoded "github" or "velnor" lane strings in serialized steps YAML.
/// Ignores legitimate uses: GitHub Actions context (${{ github.actor }}, etc.),
/// matrix.config.lane references, and the action name "crazy-max/ghaction-github-runtime".
fn find_hardcoded_lane_strings(steps_yaml: &str, ctx: &str) -> Vec<String> {
    let mut issues = vec![];

    // Strip known-good patterns before checking
    let cleaned = steps_yaml
        // GitHub Actions expression context: ${{ github.something }}
        .replace("${{ github.", "${{ GH_CTX.")
        // GITHUB_* env var names (GITHUB_ENV, GITHUB_OUTPUT, GITHUB_PATH, etc.)
        .replace("GITHUB_", "GH_VAR_")
        // crazy-max action name contains "github" legitimately
        .replace("ghaction-github-runtime", "ghaction-GH_RUNTIME")
        // ghcr.io registry URL
        .replace("ghcr.io", "GH_CR_IO")
        // .github/ directory paths in action references and scripts
        .replace(".github/", ".GH_DIR/")
        // github-pages is the GitHub Pages service name, not a lane label
        .replace("github-pages", "GH_PAGES")
        // matrix.config.lane and matrix.config.runner are the intended references
        .replace("matrix.config.lane", "MATRIX_LANE")
        .replace("matrix.config.runner", "MATRIX_RUNNER");

    // After stripping allowed patterns, "github" or "velnor" appearing as
    // standalone lane identifiers (surrounded by non-alphanumeric chars) is a violation
    for lane in &["github", "velnor"] {
        let mut pos = 0;
        while let Some(found) = cleaned[pos..].find(lane) {
            let abs = pos + found;
            let before = cleaned[..abs]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
            let after = cleaned[abs + lane.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
            if before && after {
                // Get surrounding context for the error message
                let start = abs.saturating_sub(30);
                let end = (abs + lane.len() + 30).min(cleaned.len());
                issues.push(format!(
                    "{ctx}: hardcoded lane string '{}' in steps (use ${{{{ matrix.config.lane }}}} instead): ...{}...",
                    lane,
                    &cleaned[start..end]
                ));
                break; // one report per lane per workflow is enough
            }
            pos = abs + 1;
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_semver_tag_from_ls_remote_ignores_non_release_tags() {
        let output = "\
abc\trefs/tags/v2.333.1
def\trefs/tags/v2.334.0
ghi\trefs/tags/v2.334.0-beta
jkl\trefs/tags/v3.0.0
mno\trefs/tags/not-a-runner-release
";

        let tag = latest_semver_tag_from_ls_remote(output).unwrap();

        assert_eq!(tag, "v3.0.0");
    }

    #[test]
    fn target_audit_normalizes_uses_and_compacts_values() {
        target_audit_self_test().unwrap();
    }

    #[test]
    fn live_sequence_validators_match_shell_contract() {
        assert!(validate_bool_string("TEST_BOOL", "true").is_ok());
        assert!(validate_bool_string("TEST_BOOL", "false").is_ok());
        assert!(validate_bool_string("TEST_BOOL", "True").is_err());
        assert!(validate_bool_string("TEST_BOOL", "").is_err());
        assert!(validate_bool_string("TEST_BOOL", "yes").is_err());

        assert!(validate_positive_int_string("TEST_COUNT", "1").is_ok());
        assert!(validate_positive_int_string("TEST_COUNT", "42").is_ok());
        assert!(validate_positive_int_string("TEST_COUNT", "0").is_err());
        assert!(validate_positive_int_string("TEST_COUNT", "-1").is_err());
        assert!(validate_positive_int_string("TEST_COUNT", "two").is_err());
        assert!(validate_optional_positive_int_string("TEST_COUNT", "").is_ok());
        assert!(validate_optional_positive_int_string("TEST_COUNT", "123").is_ok());
        assert!(validate_optional_positive_int_string("TEST_COUNT", "abc").is_err());

        assert!(validate_nonempty("TEST_VALUE", "value").is_ok());
        assert!(validate_nonempty("TEST_VALUE", "").is_err());
        assert!(validate_repo_slug("TEST_REPO", "owner/repo-name").is_ok());
        assert!(validate_repo_slug("TEST_REPO", "org.name/repo.name").is_ok());
        assert!(validate_repo_slug("TEST_REPO", "repo").is_err());
        assert!(validate_repo_slug("TEST_REPO", "owner/repo/extra").is_err());
        assert!(validate_optional_workflow_file("TEST_WORKFLOW", "").is_ok());
        assert!(validate_optional_workflow_file("TEST_WORKFLOW", "ci.yml").is_ok());
        assert!(validate_optional_workflow_file("TEST_WORKFLOW", "release.yaml").is_ok());
        assert!(
            validate_optional_workflow_file("TEST_WORKFLOW", ".github/workflows/ci.yml").is_err()
        );
        assert!(validate_optional_workflow_file("TEST_WORKFLOW", "ci.txt").is_err());
        assert!(validate_workflow_file("TEST_WORKFLOW", "compat.yml").is_ok());
        assert!(validate_workflow_file("TEST_WORKFLOW", "").is_err());
        assert!(validate_workflow_file("TEST_WORKFLOW", "compat.txt").is_err());
    }

    #[test]
    fn live_sequence_evidence_and_manual_target_rules_match_shell_contract() {
        assert!(validate_live_evidence_controls("80", "80").is_ok());
        assert!(validate_live_evidence_controls("12", "5").is_ok());
        assert!(validate_live_evidence_controls("0", "80").is_err());
        assert!(validate_live_evidence_controls("80", "abc").is_err());

        assert!(validate_real_target_manual_confirmation(
            "donbeave/velnor-actions-fixture",
            "false"
        )
        .is_ok());
        assert!(
            validate_real_target_manual_confirmation("ChainArgos/java-monorepo", "false").is_err()
        );
        assert!(validate_real_target_manual_confirmation("jackin-project/jackin", "true").is_ok());
        assert!(validate_real_target_manual_confirmation("jackin-project/jackin", "yes").is_err());
    }

    #[test]
    fn live_sequence_runner_exclusivity_and_job_model_match_shell_contract() {
        let runner_rows = "\
velnor-target-mvp\tonline\tself-hosted,velnor-target-mvp
other-runner\tonline\tself-hosted,velnor-target-mvp
offline-runner\toffline\tself-hosted,velnor-target-mvp
";
        assert_eq!(
            other_matching_online_runners(runner_rows, "velnor-target-mvp", &["velnor-target-mvp"]),
            vec!["other-runner (velnor-target-mvp)"]
        );
        assert!(other_matching_online_runners(
            runner_rows,
            "velnor-target-mvp",
            &["hetzner-sentry-ci"]
        )
        .is_empty());

        let model = job_execution_model(3, "Target");
        assert!(model.contains("one daemon with multiple internal GitHub runner slots"));
        assert!(model.contains("can run concurrently"));
        assert!(model.contains("3 job(s) through one bounded daemon"));
        assert!(model.contains("daemon --once"));
    }

    #[test]
    fn smoke_plan_fixture_dispatch_defaults_match_shell_contract() {
        assert!(fixture_smoke_dispatch(None, None).unwrap());
        assert!(!fixture_smoke_dispatch(None, Some(123)).unwrap());
        assert!(fixture_smoke_dispatch(Some(true), Some(123)).unwrap());
        assert!(fixture_smoke_dispatch(Some(false), None).is_err());
    }

    #[test]
    fn smoke_plan_fixture_daemon_args_match_shell_contract() {
        let args = fixture_smoke_daemon_args(
            "https://github.com/donbeave/velnor-actions-fixture",
            "velnor-target-mvp",
            "velnor-target-mvp",
            2,
            900,
            Path::new("/work"),
            None,
            false,
            "/dumps",
        );

        assert!(args.windows(2).any(|pair| pair == ["--slots", "2"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--labels", "velnor-target-mvp"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--dump-job-message", "/dumps"]));
        assert!(!args.contains(&"--require-docker-socket".to_string()));
        assert!(!args.contains(&"--docker-host-work-dir".to_string()));
    }

    #[test]
    fn smoke_plan_fixture_validates_dispatch_inputs() {
        assert!(validate_workflow_dispatch_inputs("key=value,other=value").is_ok());
        assert!(validate_workflow_dispatch_inputs("").is_ok());
        assert!(validate_workflow_dispatch_inputs("=bad").is_err());
        assert!(validate_workflow_dispatch_inputs("bad").is_err());
        assert!(validate_workflow_dispatch_inputs("1bad=value").is_err());
    }

    #[test]
    fn smoke_plan_target_labels_match_shell_contract() {
        assert_eq!(
            target_smoke_scheduling_labels(false),
            vec!["hetzner-sentry-ci", "ubuntu-latest", "ubuntu-24.04"]
        );
        assert_eq!(
            target_smoke_scheduling_labels(true),
            vec![
                "hetzner-sentry-ci",
                "ubuntu-latest",
                "ubuntu-24.04",
                "ubuntu-24.04-arm"
            ]
        );
    }

    #[test]
    fn smoke_plan_target_daemon_args_match_shell_contract() {
        let args = target_smoke_daemon_args(
            "https://github.com/owner/repo",
            "velnor-target-mvp",
            true,
            3,
            120,
            Path::new("/work"),
            Some(Path::new("/docker-work")),
            true,
            "/dumps",
        );

        assert!(args
            .windows(2)
            .any(|pair| pair == ["--url", "https://github.com/owner/repo"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--name", "velnor-target-mvp"]));
        assert!(args.contains(&"--target-mvp-labels".to_string()));
        assert!(args.contains(&"--target-mvp-arm-label".to_string()));
        assert!(args.windows(2).any(|pair| pair == ["--slots", "3"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--idle-timeout-seconds", "120"]));
        assert!(args.windows(2).any(|pair| pair == ["--work-dir", "/work"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--docker-host-work-dir", "/docker-work"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--dump-job-message", "/dumps"]));
        assert!(args.contains(&"--require-docker-socket".to_string()));
    }

    #[test]
    fn smoke_plan_target_daemon_args_omit_optional_flags_when_disabled() {
        let args = target_smoke_daemon_args(
            "https://github.com/owner/repo",
            "velnor-target-mvp",
            false,
            1,
            900,
            Path::new("/work"),
            None,
            false,
            "",
        );

        assert!(!args.contains(&"--target-mvp-arm-label".to_string()));
        assert!(!args.contains(&"--docker-host-work-dir".to_string()));
        assert!(!args.contains(&"--dump-job-message".to_string()));
        assert!(!args.contains(&"--require-docker-socket".to_string()));
    }

    #[test]
    fn smoke_plan_target_requires_manual_confirmation_for_real_targets() {
        assert!(validate_real_target_manual_confirmation_bool(
            "donbeave/velnor-actions-fixture",
            false
        )
        .is_ok());
        assert!(
            validate_real_target_manual_confirmation_bool("ChainArgos/java-monorepo", false)
                .is_err()
        );
        assert!(
            validate_real_target_manual_confirmation_bool("jackin-project/jackin", false).is_err()
        );
        assert!(
            validate_real_target_manual_confirmation_bool("jackin-project/jackin", true).is_ok()
        );
    }

    #[test]
    fn host_doctor_plan_preflight_args_match_shell_contract() {
        let args = live_host_doctor_preflight_args(
            Path::new("/work"),
            Some(Path::new("/docker-work")),
            true,
        );

        assert_eq!(
            args,
            vec![
                "--work-dir",
                "/work",
                "--docker-host-work-dir",
                "/docker-work",
                "--require-docker-socket"
            ]
        );

        let args = live_host_doctor_preflight_args(Path::new("/work"), None, false);
        assert_eq!(args, vec!["--work-dir", "/work"]);
    }

    #[test]
    fn host_doctor_plan_status_args_match_shell_contract() {
        assert_eq!(
            live_host_doctor_status_args(true),
            vec!["--check-target-mvp"]
        );
        assert!(live_host_doctor_status_args(false).is_empty());
    }

    #[test]
    fn host_doctor_plan_detects_remote_docker_hosts() {
        assert!(is_remote_docker_host("tcp://127.0.0.1:2375"));
        assert!(is_remote_docker_host("ssh://docker.example"));
        assert!(!is_remote_docker_host("unix:///var/run/docker.sock"));
        assert!(!is_remote_docker_host(""));
    }

    #[test]
    fn host_doctor_plan_rejects_arm_label_on_non_arm_arch() {
        assert!(validate_target_mvp_arm_label_host(false, "x86_64").is_ok());
        assert!(validate_target_mvp_arm_label_host(true, "arm64").is_ok());
        assert!(validate_target_mvp_arm_label_host(true, "aarch64").is_ok());
        assert!(validate_target_mvp_arm_label_host(true, "x86_64").is_err());
    }

    #[test]
    fn workflow_dispatch_sha_detection_matches_shell_contract() {
        assert!(looks_like_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(!looks_like_sha("main"));
        assert!(!looks_like_sha("v1.0.0"));
        assert!(!looks_like_sha(""));
        assert!(!looks_like_sha("0123456789abcdef0123456789abcdef0123456")); // 39 chars
    }

    #[test]
    fn workflow_dispatch_input_pairs_are_valid() {
        assert!(validate_workflow_dispatch_inputs("package=a,push=false").is_ok());
        assert!(validate_workflow_dispatch_inputs("").is_ok());
        assert!(validate_workflow_dispatch_inputs("bad").is_err());
        assert!(validate_workflow_dispatch_inputs(",key=val").is_err());
    }

    #[test]
    fn sanitize_filename_replaces_non_alphanum() {
        assert_eq!(sanitize_filename("owner/repo"), "owner_repo");
        assert_eq!(sanitize_filename("ci.yml"), "ci.yml");
        assert_eq!(sanitize_filename("a b"), "a_b");
        assert_eq!(sanitize_filename("a-b_c.d"), "a-b_c.d");
    }

    #[test]
    fn write_live_evidence_cmd_creates_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let tmp = std::env::temp_dir().join("velnor-evidence-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let dump_dir = tmp.join("dumps");
        std::fs::create_dir_all(&dump_dir).unwrap();
        std::fs::write(dump_dir.join("job-123.json"), b"{}").unwrap();

        let args = WriteLiveEvidenceArgs {
            phase: "test-phase".to_string(),
            repo: "owner/repo".to_string(),
            run_id: 123,
            runner_name: "velnor-test".to_string(),
            work_dir: Some(tmp.join("work")),
            docker_host_work_dir: String::new(),
            require_docker_socket: true,
            dump_job_messages: dump_dir.display().to_string(),
            title: "Test".to_string(),
            workflow: "ci.yml".to_string(),
            ref_: "main".to_string(),
            inputs: "package=a".to_string(),
            job_count: 1,
            evidence_dir: Some(tmp.join("evidence")),
            log_lines: 80,
            local_entries: 80,
            extra_metadata: vec!["runner label: velnor-test".to_string()],
        };

        write_live_evidence_cmd(root, args).unwrap();

        let evidence_file = tmp.join("evidence").join("owner_repo-ci.yml-123.md");
        assert!(evidence_file.exists(), "evidence file should exist");
        let content = std::fs::read_to_string(&evidence_file).unwrap();
        assert!(content.contains("# Velnor Test Live Evidence"));
        assert!(content.contains("- phase: test-phase"));
        assert!(content.contains("- repository: owner/repo"));
        assert!(content.contains("- run id: 123"));
        assert!(content.contains("Sanitized Job Message Dumps"));
        assert!(content.contains("Velnor Source Snapshot"));
        assert!(content.contains("job-123.json"));
        assert!(content.contains("runner label: velnor-test"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
