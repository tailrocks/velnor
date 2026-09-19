//! Deterministic verification of the GitHub-first dual-lane evidence contract.
//!
//! The checker has three deliberately separate inputs: a reviewed workload
//! manifest, an independently collected GitHub snapshot, and result records.
//! Result records never define the scope, expected jobs, current revision, or
//! required checks.  `--live` replaces the snapshot facts with a fresh
//! read-only API collection before verification; G7 always requires it.

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const MANIFEST_SCHEMA_VERSION: u32 = 2;
const SNAPSHOT_SCHEMA_VERSION: u32 = 2;
const EVIDENCE_SCHEMA_VERSION: u32 = 2;
const CANONICAL_RELEASE_SCHEMA_VERSION: u32 = 1;
const REQUIRED_REPOSITORIES: usize = 32;
const SHA_LENGTH: usize = 40;
const DIGEST_LENGTH: usize = 64;
const LIVE_FRESHNESS_SECONDS: i64 = 15 * 60;
const REVIEWED_MANIFEST_ID: &str = "github-first-dual-lane-2026-09-19";
const REVIEWED_SOURCE_REPOSITORY: &str = "tailrocks/velnor";
const REVIEWED_SOURCE_REVISION: &str = "abe9ad82a2d4d01b706bbc6122ab6ccb150faad9";
// This is the digest of the reviewed `docs/ci/github-first-dual-lane/fleet.json`
// authority at the accepted preparation revision.  A future reviewed plan
// must update this checker constant in the same reviewed change; a caller may
// not select a new plan by changing JSON input alone.
const REVIEWED_SOURCE_DIGEST: &str =
    "sha256:b39b3bcb5d149db66a03483bd7a19482af2657df6f962872030d820a39f13514";
const EXPECTED_ORCHESTRATOR_MODEL: &str = "gpt-6-astra";
const EXPECTED_ORCHESTRATOR_EFFORT: &str = "low";
const EXPECTED_AGENT_MODEL: &str = "gpt-5.6-luna";
const EXPECTED_AGENT_EFFORT: &str = "max";

/// The fixed scope belongs to this task-specific checker.  It is not part of
/// the generic workflow generator and cannot be replaced with an arbitrary
/// list supplied by a caller.
pub(crate) const CANONICAL_REPOSITORIES: [&str; REQUIRED_REPOSITORIES] = [
    "tailrocks/velnor",
    "tailrocks/velnor-apt",
    "tailrocks/parallax",
    "tailrocks/tracing-request-level",
    "tailrocks/termrock",
    "tailrocks/termpane",
    "tailrocks/tablerock",
    "tailrocks/schemalane",
    "tailrocks/ruxel",
    "tailrocks/pg-bigdecimal",
    "tailrocks/parallax-telemetry-playground",
    "tailrocks/homebrew-tablerock",
    "tailrocks/homebrew-ruxel",
    "tailrocks/homebrew-parallax",
    "tailrocks/homebrew-holla",
    "tailrocks/holla-apt",
    "tailrocks/holla",
    "tailrocks/homebrew-velnor",
    "tailrocks/tailrocks-typescript-skills",
    "tailrocks/tailrocks-skill-authoring-skills",
    "tailrocks/tailrocks-rust-skills",
    "tailrocks/tailrocks-roadmap-skills",
    "tailrocks/tailrocks-pull-request-skills",
    "tailrocks/tailrocks-open-source-skills",
    "tailrocks/tailrocks-macos-skills",
    "tailrocks/tailrocks-code-quality-skills",
    "jackin-project/jackin",
    "jackin-project/jackin-agent-smith",
    "jackin-project/homebrew-tap",
    "jackin-project/jackin-the-architect",
    "jackin-project/jackin-sentinel",
    "jackin-project/jackin-role-action",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    G0,
    G1,
    G2,
    G3,
    G4,
    G5,
    G6,
    G7,
}

impl Stage {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "G0" => Ok(Self::G0),
            "G1" => Ok(Self::G1),
            "G2" => Ok(Self::G2),
            "G3" => Ok(Self::G3),
            "G4" => Ok(Self::G4),
            "G5" => Ok(Self::G5),
            "G6" => Ok(Self::G6),
            "G7" => Ok(Self::G7),
            other => bail!("stage must be one of G0, G1, G2, G3, G4, G5, G6, G7; got {other}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::G0 => "G0",
            Self::G1 => "G1",
            Self::G2 => "G2",
            Self::G3 => "G3",
            Self::G4 => "G4",
            Self::G5 => "G5",
            Self::G6 => "G6",
            Self::G7 => "G7",
        }
    }

    fn needs_execution(self) -> bool {
        self >= Self::G1
    }

    fn needs_release(self) -> bool {
        self >= Self::G2
    }

    fn needs_both_lanes(self) -> bool {
        matches!(self, Self::G4 | Self::G5 | Self::G6 | Self::G7)
    }

    fn needs_all_prs(self) -> bool {
        self.needs_execution()
    }

    fn needs_review(self) -> bool {
        self == Self::G7
    }
}

#[derive(Debug, Args)]
pub struct EvidenceCheckArgs {
    /// Gate to validate. It is never inferred from an evidence record.
    #[arg(long)]
    pub stage: String,
    /// Reviewed external exact-scope workload manifest.
    #[arg(long)]
    pub manifest: PathBuf,
    /// Independently captured snapshot. Live mode reconciles it to GitHub.
    #[arg(long)]
    pub snapshot: PathBuf,
    /// Evidence records for the snapshot.
    #[arg(long)]
    pub evidence: PathBuf,
    /// Independent producer-owned canonical application manifest for G2+.
    #[arg(long)]
    pub release_manifest: Option<PathBuf>,
    /// Reconcile current revisions, PRs, rulesets, runs, checks, and jobs
    /// through the read-only GitHub API. G7 always enables this mode.
    #[arg(long)]
    pub live: bool,
    /// Emit a stable JSON report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone)]
pub struct EvidenceCheckInput {
    pub stage: String,
    pub manifest: PathBuf,
    pub snapshot: PathBuf,
    pub evidence: PathBuf,
    pub release_manifest: Option<PathBuf>,
    pub live: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub schema_version: u32,
    pub stage: String,
    pub mode: &'static str,
    pub status: &'static str,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub code: String,
    pub repository: Option<String>,
    pub field: String,
    pub message: String,
}

impl Finding {
    fn new(
        code: impl Into<String>,
        repository: Option<&str>,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            repository: repository.map(str::to_owned),
            field: field.into(),
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reviewed source manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceIdentity {
    pub repository: String,
    pub revision: String,
    pub digest: String,
    pub reviewed_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestDocument {
    pub schema_version: u32,
    pub manifest_id: String,
    pub source: SourceIdentity,
    pub repositories: Vec<ManifestRepository>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestRepository {
    pub repository: String,
    pub repository_role: String,
    pub default_branch: String,
    pub expected_workload_ids: Vec<String>,
    pub required_check_contexts_and_apps: Vec<RequiredContext>,
    pub workload_platform_architecture: Vec<WorkloadPlatform>,
    pub expected_jobs: Vec<ExpectedJobSpec>,
    pub generated_plan_digest: String,
    pub workflow_path: String,
    pub workflow_revision: String,
    pub provider_eligibility: BTreeMap<String, Eligibility>,
    pub host_contracts: BTreeMap<String, HostContract>,
    pub release_applicability: Applicability,
    pub generator_revision: String,
    pub runtime_product_id: String,
    pub generator_artifact_digest: String,
    pub configuration_digest: String,
    pub generated_tree_digest: String,
    pub scan_state_digest: String,
    pub runtime_release_version: String,
    pub runtime_source_sha: String,
    pub job_image_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequiredContext {
    pub context: String,
    pub app_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkloadPlatform {
    pub workload_id: String,
    pub platform: String,
    pub architecture: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExpectedJobSpec {
    pub job_id: String,
    pub workload_id: String,
    pub provider: String,
    pub platform: String,
    pub architecture: String,
    pub required: bool,
    pub child_workflow: Option<ChildWorkflowSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildWorkflowSpec {
    pub repository: String,
    pub workflow_path: String,
    pub event: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostContract {
    pub runner_kind: String,
    pub required_labels: Vec<String>,
    pub forbidden_labels: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Applicability {
    Required,
    Applicable,
    #[default]
    NotApplicable,
    Excluded,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Eligibility {
    Eligible,
    #[default]
    NotApplicable,
    Excluded,
}

// ---------------------------------------------------------------------------
// Independently captured snapshot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotDocument {
    pub schema_version: u32,
    pub snapshot_id: String,
    pub manifest_id: String,
    pub observed_at_utc: String,
    pub source: SnapshotSource,
    pub repositories: Vec<SnapshotRepository>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotSource {
    pub collector: String,
    pub collector_revision: String,
    pub api_base: String,
    pub captured_at_utc: String,
    pub read_only: bool,
    pub page_count: u32,
    pub permission_scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct G0InventoryEvidence {
    pub source_url: String,
    pub repository_count: u32,
    pub open_pr_count: u32,
    pub workflow_repository_count: u32,
    pub ruleset_repository_count: u32,
    pub workload_matrix_digest: String,
    pub dependency_graph_digest: String,
    pub access_scopes: Vec<String>,
    pub access_gaps: Vec<String>,
    pub orchestrator_model: String,
    pub orchestrator_effort: String,
    pub agent_model: String,
    pub agent_effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotRepository {
    pub repository: String,
    pub repository_id: u64,
    pub default_branch: String,
    pub default_branch_sha: String,
    pub ruleset: RulesetObservation,
    pub workflows: Vec<WorkflowObservation>,
    pub main_executions: Vec<ExecutionObservation>,
    pub open_prs: Vec<SnapshotPullRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RulesetObservation {
    pub required_checks: Vec<RequiredContext>,
    pub source_url: String,
    pub pages_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowObservation {
    pub path: String,
    pub revision: String,
    pub source_sha: String,
    pub event: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotPullRequest {
    pub number: u64,
    pub state: String,
    pub draft: bool,
    pub author: String,
    pub author_association: String,
    pub head_repository: String,
    pub head_sha: String,
    pub base_sha: String,
    pub merge_sha: Option<String>,
    pub merge_group_sha: Option<String>,
    pub source_url: String,
    pub executions: Vec<ExecutionObservation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExecutionObservation {
    pub run_id: u64,
    pub run_attempt: u32,
    pub run_url: String,
    pub workflow_path: String,
    pub workflow_revision: String,
    pub event: String,
    pub trigger_source_sha: String,
    pub actual_checkout_sha: String,
    pub status: String,
    pub conclusion: String,
    pub provider: String,
    pub runner_name: String,
    pub host_id: String,
    pub runner_kind: String,
    pub runner_labels: Vec<String>,
    pub jobs: Vec<JobObservation>,
    pub required_checks: Vec<CheckObservation>,
    pub child_runs: Vec<ChildRunObservation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobObservation {
    pub job_id: String,
    pub job_name: String,
    pub workload_id: String,
    pub provider: String,
    pub platform: String,
    pub architecture: String,
    pub status: String,
    pub conclusion: String,
    pub event: String,
    pub runner_name: String,
    pub host_id: String,
    pub runner_kind: String,
    pub runner_labels: Vec<String>,
    pub source_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckObservation {
    pub context: String,
    pub app_id: String,
    pub status: String,
    pub conclusion: String,
    pub run_id: u64,
    pub job_id: String,
    pub source_url: String,
    pub event: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildRunObservation {
    pub parent_run_id: u64,
    pub run_id: u64,
    pub run_attempt: u32,
    pub repository: String,
    pub workflow_path: String,
    pub event: String,
    pub source_sha: String,
    pub provider: String,
    pub status: String,
    pub conclusion: String,
    pub source_url: String,
}

// ---------------------------------------------------------------------------
// Result records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceDocument {
    pub schema_version: u32,
    pub manifest_id: String,
    pub snapshot_id: String,
    pub stage: String,
    pub records: Vec<EvidenceRecord>,
    pub reviewer_attestation: Option<ReviewerAttestation>,
    pub g0_inventory: Option<G0InventoryEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewerAttestation {
    pub reviewer: String,
    pub report_digest: String,
    pub manifest_id: String,
    pub snapshot_id: String,
    pub attested_at_utc: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceRecord {
    pub repository: String,
    pub repository_role: String,
    pub default_branch: String,
    pub default_branch_sha: String,
    pub observed_at_utc: String,
    pub generator_revision: String,
    pub runtime_product_id: String,
    pub generator_artifact_digest: String,
    pub configuration_digest: String,
    pub generated_tree_digest: String,
    pub scan_state_digest: String,
    pub runtime_release_version: String,
    pub runtime_source_sha: String,
    pub job_image_digest: String,
    pub expected_workload_ids: Vec<String>,
    pub required_check_contexts_and_apps: Vec<RequiredContext>,
    pub workload_platform_architecture: Vec<WorkloadPlatform>,
    pub provider_eligibility: BTreeMap<String, Eligibility>,
    pub justified_exclusions: Vec<String>,
    pub pr_number: Option<u64>,
    pub pr_head_sha: Option<String>,
    pub pr_base_sha: Option<String>,
    pub tested_merge_sha: Option<String>,
    pub merge_group_sha: Option<String>,
    pub workflow_path: String,
    pub workflow_revision: String,
    pub event: String,
    pub run_id: u64,
    pub run_attempt: u32,
    pub run_url: String,
    pub trigger_source_sha: String,
    pub actual_checkout_sha: String,
    pub provider: String,
    pub runner_name: String,
    pub host_id: String,
    pub runner_kind: String,
    pub runner_labels: Vec<String>,
    pub run_status: String,
    pub run_conclusion: String,
    pub expected_jobs: Vec<ExpectedJobSpec>,
    pub actual_job_ids: Vec<String>,
    pub actual_job_conclusions: BTreeMap<String, String>,
    pub logs: Vec<String>,
    pub child_run_links: Vec<ChildRunLink>,
    pub required_checks: Vec<RequiredCheckEvidence>,
    pub release: Option<ReleaseEvidence>,
    pub install: Option<InstallEvidence>,
    pub owner: String,
    pub reviewer: String,
    pub gate_status: String,
    pub blocker: Option<String>,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChildRunLink {
    pub parent_run_id: u64,
    pub run_id: u64,
    pub run_attempt: u32,
    pub repository: String,
    pub workflow_path: String,
    pub event: String,
    pub source_sha: String,
    pub provider: String,
    pub status: String,
    pub conclusion: String,
    pub run_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequiredCheckEvidence {
    pub context: String,
    pub app_id: String,
    pub job_id: String,
    pub status: String,
    pub conclusion: String,
    pub run_id: u64,
    pub source_url: String,
    pub event: String,
}

// ---------------------------------------------------------------------------
// Canonical producer-owned release manifest and projections
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalReleaseDocument {
    pub schema_version: u32,
    pub manifest: CanonicalReleaseManifest,
    /// Digest of the canonical manifest bytes.  It is outside the manifest so
    /// the digest has no self-reference/hash-cycle.
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalReleaseManifest {
    pub schema: String,
    pub product_id: String,
    pub channel: String,
    pub version: String,
    pub source_repository: String,
    pub source_ref: String,
    pub source_commit: String,
    pub release_tag: String,
    pub release_id: String,
    pub producer: ReleaseProducer,
    pub artifacts: Vec<CanonicalArtifact>,
    pub components: Vec<CanonicalComponent>,
    pub targets: Vec<TargetInventory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseProducer {
    pub repository: String,
    pub workflow_path: String,
    pub run_id: u64,
    pub run_url: String,
    pub source_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalArtifact {
    pub name: String,
    pub target: String,
    pub kind: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CanonicalComponent {
    pub name: String,
    pub crate_name: String,
    pub version: String,
    pub binary: String,
    pub target: String,
    pub artifact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TargetInventory {
    pub target: String,
    pub platform: String,
    pub architecture: String,
    pub service: Applicability,
    pub binaries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseEvidence {
    pub applicability: Applicability,
    pub justification: Option<String>,
    pub execution: Option<ReleaseExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseExecution {
    pub channel: String,
    pub version: String,
    pub tag_target_sha: String,
    pub release_id: String,
    pub asset_digests: BTreeMap<String, String>,
    pub producer: ReleaseEvidenceProducer,
    pub apt: AptPublication,
    pub homebrew: HomebrewPublication,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseEvidenceProducer {
    pub repository: String,
    pub workflow_path: String,
    pub run_id: u64,
    pub run_url: String,
    pub source_commit: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AptPublication {
    pub repository: String,
    pub revision: String,
    pub suite: String,
    pub candidate: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HomebrewPublication {
    pub tap: String,
    pub revision: String,
    pub formula: String,
    pub version: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallEvidence {
    pub applicability: Applicability,
    pub justification: Option<String>,
    pub environment: Option<InstallEnvironment>,
    pub operations: Option<InstallOperations>,
    pub installed: Option<InstalledProduct>,
    pub service: Option<ServiceObservation>,
    pub functional_result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallEnvironment {
    pub os_image: String,
    pub platform: String,
    pub architecture: String,
    pub runner: String,
    pub workspace: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallOperations {
    pub clean_install: InstallOperation,
    pub same_channel_upgrade: InstallOperation,
    pub channel_switch: InstallOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallOperation {
    pub predecessor: Option<ProductIdentity>,
    pub observed_release_id: String,
    pub observed_version: String,
    pub result: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProductIdentity {
    pub product_id: String,
    pub channel: String,
    pub version: String,
    pub release_id: String,
    pub source_sha: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledProduct {
    pub product_id: String,
    pub channel: String,
    pub version: String,
    pub source_sha: String,
    pub manifest_sha256: String,
    pub target: String,
    pub binaries: Vec<InstalledBinary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstalledBinary {
    pub name: String,
    pub component: String,
    pub artifact: String,
    pub target: String,
    pub path: String,
    pub sha256: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServiceObservation {
    pub applicability: Applicability,
    pub result: String,
    pub justification: Option<String>,
}

// ---------------------------------------------------------------------------
// Entry points and strict parsing
// ---------------------------------------------------------------------------

pub async fn evidence_check(args: EvidenceCheckArgs) -> Result<()> {
    let stage = Stage::parse(&args.stage)?;
    let input = EvidenceCheckInput {
        stage: args.stage,
        manifest: args.manifest,
        snapshot: args.snapshot,
        evidence: args.evidence,
        release_manifest: args.release_manifest,
        live: args.live || stage == Stage::G7,
    };
    let report = if input.live {
        check_paths_live(&input).await?
    } else {
        check_paths(&input)?
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialize evidence report")?
        );
    } else if report.findings.is_empty() {
        println!("{} evidence gate passed ({})", report.stage, report.mode);
    } else {
        for finding in &report.findings {
            let repository = finding
                .repository
                .as_deref()
                .map(|value| format!(" [{value}]"))
                .unwrap_or_default();
            println!(
                "{}{} {}: {}",
                finding.code, repository, finding.field, finding.message
            );
        }
    }
    if report.status != "pass" {
        bail!(
            "{} evidence gate failed with {} finding(s)",
            report.stage,
            report.findings.len()
        );
    }
    Ok(())
}

/// Offline validation is useful for deterministic fixture tests and G0
/// preparation. It is explicitly marked offline and cannot pass G7.
pub fn check_paths(input: &EvidenceCheckInput) -> Result<CheckReport> {
    let stage = Stage::parse(&input.stage)?;
    let manifest: ManifestDocument = read_json(&input.manifest, "manifest")?;
    let snapshot: SnapshotDocument = read_json(&input.snapshot, "snapshot")?;
    let evidence: EvidenceDocument = read_json(&input.evidence, "evidence")?;
    let release = read_release_manifest(stage, input.release_manifest.as_deref())?;
    Ok(check_documents(
        stage,
        &manifest,
        &snapshot,
        &evidence,
        release.as_ref(),
        "offline",
    ))
}

/// Live mode uses the existing typed GitHub transport. The supplied snapshot
/// remains an audit artifact, but current facts are collected independently
/// and any mismatch fails the gate.
pub async fn check_paths_live(input: &EvidenceCheckInput) -> Result<CheckReport> {
    let stage = Stage::parse(&input.stage)?;
    if stage == Stage::G7 && !input.live {
        bail!("G7 requires --live");
    }
    let manifest: ManifestDocument = read_json(&input.manifest, "manifest")?;
    let supplied_snapshot: SnapshotDocument = read_json(&input.snapshot, "snapshot")?;
    let evidence: EvidenceDocument = read_json(&input.evidence, "evidence")?;
    let release = read_release_manifest(stage, input.release_manifest.as_deref())?;
    let token = env::var("GITHUB_TOKEN")
        .or_else(|_| env::var("GH_TOKEN"))
        .context("live evidence checking requires GITHUB_TOKEN or GH_TOKEN")?;
    if token.trim().is_empty() {
        bail!("live evidence checking requires a non-empty GitHub token");
    }
    let client = crate::fleet_policy_client::ReqwestFleetHttp::new(
        crate::fleet_policy_client::DEFAULT_GITHUB_API_URL,
        &token,
    )?;
    let live_snapshot = crate::evidence_live::collect_live_snapshot(
        &client,
        &manifest,
        supplied_snapshot.snapshot_id.clone(),
    )
    .await?;
    let mut report = check_documents(
        stage,
        &manifest,
        &live_snapshot,
        &evidence,
        release.as_ref(),
        "live",
    );
    compare_snapshot_facts(&supplied_snapshot, &live_snapshot, &mut report.findings);
    check_live_freshness(&live_snapshot, &mut report.findings);
    sort_findings(&mut report.findings);
    report.status = if report.findings.is_empty() {
        "pass"
    } else {
        "fail"
    };
    Ok(report)
}

fn read_release_manifest(
    stage: Stage,
    path: Option<&Path>,
) -> Result<Option<CanonicalReleaseDocument>> {
    if !stage.needs_release() {
        return Ok(None);
    }
    let path = path.context("G2+ requires --release-manifest")?;
    let release: CanonicalReleaseDocument = read_json(path, "release manifest")?;
    Ok(Some(release))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, label: &str) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {label} {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse strict {label} JSON {}", path.display()))
}

#[derive(Debug, Clone, Copy)]
struct RepositoryIndex<'a> {
    manifest: &'a ManifestRepository,
    snapshot: &'a SnapshotRepository,
}

fn check_documents(
    stage: Stage,
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    evidence: &EvidenceDocument,
    release: Option<&CanonicalReleaseDocument>,
    mode: &'static str,
) -> CheckReport {
    let mut findings = Vec::new();
    check_headers(stage, manifest, snapshot, evidence, &mut findings);
    check_manifest(manifest, &mut findings);
    check_snapshot(manifest, snapshot, &mut findings);
    if stage == Stage::G0 {
        check_g0_inventory(snapshot, evidence.g0_inventory.as_ref(), &mut findings);
    }

    let manifest_by_repo = index_manifest(manifest, &mut findings);
    let snapshot_by_repo = index_snapshot(snapshot, &mut findings);
    check_scope_coverage(&manifest_by_repo, &snapshot_by_repo, &mut findings);

    let mut records = BTreeMap::<RecordKey, &EvidenceRecord>::new();
    for record in &evidence.records {
        let key = RecordKey::from_record(record);
        if records.insert(key, record).is_some() {
            finding(
                &mut findings,
                "duplicate-evidence",
                &record.repository,
                "records",
                "one record per repository/provider/event/PR subject is required",
            );
        }
        match (
            manifest_by_repo.get(&record.repository),
            snapshot_by_repo.get(&record.repository),
        ) {
            (Some(manifest_repo), Some(snapshot_repo)) => check_record(
                stage,
                RepositoryIndex {
                    manifest: manifest_repo,
                    snapshot: snapshot_repo,
                },
                record,
                release,
                &mut findings,
            ),
            (None, _) => finding(
                &mut findings,
                "unknown-repository",
                &record.repository,
                "repository",
                "record repository is absent from the fixed external manifest",
            ),
            (_, None) => finding(
                &mut findings,
                "missing-snapshot-repository",
                &record.repository,
                "repository",
                "record repository is absent from the independently captured snapshot",
            ),
        }
    }

    if evidence.records.is_empty() {
        finding(
            &mut findings,
            "missing-evidence",
            "",
            "records",
            "evidence envelope has no records",
        );
    }
    check_record_coverage(
        stage,
        &manifest_by_repo,
        &snapshot_by_repo,
        &records,
        &mut findings,
    );
    check_lane_parity(stage, &records, &mut findings);
    if stage == Stage::G7 && mode != "live" {
        finding(
            &mut findings,
            "live-required",
            "",
            "mode",
            "G7 cannot pass from an offline fixture or caller-supplied snapshot",
        );
    }
    sort_findings(&mut findings);
    CheckReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        stage: stage.as_str().to_owned(),
        mode,
        status: if findings.is_empty() { "pass" } else { "fail" },
        findings,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RecordKey {
    repository: String,
    provider: String,
    event: String,
    pr_number: Option<u64>,
}

impl RecordKey {
    fn from_record(record: &EvidenceRecord) -> Self {
        Self {
            repository: record.repository.clone(),
            provider: record.provider.clone(),
            event: record.event.clone(),
            pr_number: record.pr_number,
        }
    }
}

fn check_headers(
    stage: Stage,
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    evidence: &EvidenceDocument,
    findings: &mut Vec<Finding>,
) {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        finding(
            findings,
            "schema-version",
            "",
            "manifest.schema_version",
            format!("expected {MANIFEST_SCHEMA_VERSION}"),
        );
    }
    if snapshot.schema_version != SNAPSHOT_SCHEMA_VERSION {
        finding(
            findings,
            "schema-version",
            "",
            "snapshot.schema_version",
            format!("expected {SNAPSHOT_SCHEMA_VERSION}"),
        );
    }
    if evidence.schema_version != EVIDENCE_SCHEMA_VERSION {
        finding(
            findings,
            "schema-version",
            "",
            "evidence.schema_version",
            format!("expected {EVIDENCE_SCHEMA_VERSION}"),
        );
    }
    if manifest.manifest_id.trim().is_empty() {
        finding(
            findings,
            "missing-manifest-id",
            "",
            "manifest.manifest_id",
            "manifest_id is required",
        );
    }
    if manifest.manifest_id != REVIEWED_MANIFEST_ID {
        finding(
            findings,
            "untrusted-manifest",
            "",
            "manifest.manifest_id",
            format!("manifest must bind reviewed authority {REVIEWED_MANIFEST_ID}"),
        );
    }
    if snapshot.snapshot_id.trim().is_empty() {
        finding(
            findings,
            "missing-snapshot-id",
            "",
            "snapshot.snapshot_id",
            "snapshot_id is required",
        );
    }
    if snapshot.manifest_id != manifest.manifest_id {
        finding(
            findings,
            "manifest-mismatch",
            "",
            "snapshot.manifest_id",
            "snapshot does not bind to the manifest identity",
        );
    }
    if evidence.manifest_id != manifest.manifest_id {
        finding(
            findings,
            "manifest-mismatch",
            "",
            "evidence.manifest_id",
            "evidence does not bind to the manifest identity",
        );
    }
    if evidence.snapshot_id != snapshot.snapshot_id {
        finding(
            findings,
            "snapshot-mismatch",
            "",
            "evidence.snapshot_id",
            "evidence does not bind to the authoritative snapshot identity",
        );
    }
    if evidence.stage != stage.as_str() {
        finding(
            findings,
            "stage-mismatch",
            "",
            "evidence.stage",
            format!(
                "evidence declares {}; checker was invoked for {}",
                evidence.stage,
                stage.as_str()
            ),
        );
    }
    if stage == Stage::G7 {
        let Some(attestation) = evidence.reviewer_attestation.as_ref() else {
            finding(
                findings,
                "missing-review-attestation",
                "",
                "evidence.reviewer_attestation",
                "G7 requires an external reviewer attestation bound to this manifest and snapshot",
            );
            return;
        };
        if attestation.reviewer.trim().is_empty()
            || !valid_digest(&attestation.report_digest)
            || attestation.manifest_id != manifest.manifest_id
            || attestation.snapshot_id != snapshot.snapshot_id
            || !valid_timestamp(&attestation.attested_at_utc)
        {
            finding(
                findings,
                "invalid-review-attestation",
                "",
                "evidence.reviewer_attestation",
                "reviewer attestation must bind exact identities and a content digest",
            );
        }
    }
    if !valid_timestamp(&snapshot.observed_at_utc) {
        finding(
            findings,
            "invalid-timestamp",
            "",
            "snapshot.observed_at_utc",
            "expected an RFC3339 UTC timestamp ending in Z",
        );
    }
}

fn check_manifest(manifest: &ManifestDocument, findings: &mut Vec<Finding>) {
    if manifest.source.repository != REVIEWED_SOURCE_REPOSITORY {
        finding(
            findings,
            "untrusted-manifest",
            "",
            "manifest.source.repository",
            format!("expected reviewed source {REVIEWED_SOURCE_REPOSITORY}"),
        );
    }
    if manifest.source.revision != REVIEWED_SOURCE_REVISION {
        finding(
            findings,
            "untrusted-manifest",
            "",
            "manifest.source.revision",
            "manifest source revision is not the accepted reviewed revision",
        );
    }
    if !digest_equal(&manifest.source.digest, REVIEWED_SOURCE_DIGEST) {
        finding(
            findings,
            "untrusted-manifest",
            "",
            "manifest.source.digest",
            "manifest source digest is not the accepted reviewed authority digest",
        );
    }
    validate_repository_name(
        &manifest.source.repository,
        "manifest.source.repository",
        findings,
    );
    validate_sha(
        "",
        "manifest.source.revision",
        &manifest.source.revision,
        findings,
    );
    validate_digest(
        "",
        "manifest.source.digest",
        &manifest.source.digest,
        findings,
    );
    if manifest.source.reviewed_by.trim().is_empty() {
        finding(
            findings,
            "missing-source-review",
            "",
            "manifest.source.reviewed_by",
            "reviewed source identity is required",
        );
    }

    let expected = canonical_scope();
    let actual = manifest
        .repositories
        .iter()
        .map(|repo| repo.repository.as_str())
        .collect::<BTreeSet<_>>();
    if manifest.repositories.len() != REQUIRED_REPOSITORIES {
        finding(
            findings,
            "manifest-count",
            "",
            "manifest.repositories",
            format!("expected exactly {REQUIRED_REPOSITORIES} unique rows"),
        );
    }
    if actual != expected {
        finding(
            findings,
            "manifest-scope",
            "",
            "manifest.repositories",
            "manifest must equal the fixed canonical 32-repository set exactly",
        );
    }
    let mut seen = BTreeSet::new();
    for repo in &manifest.repositories {
        if !seen.insert(repo.repository.clone()) {
            finding(
                findings,
                "manifest-duplicate",
                &repo.repository,
                "manifest.repositories",
                "repository appears more than once",
            );
        }
        check_manifest_repository(repo, findings);
    }
}

fn check_manifest_repository(repo: &ManifestRepository, findings: &mut Vec<Finding>) {
    validate_repository_name(
        &repo.repository,
        "manifest.repositories.repository",
        findings,
    );
    for (field, value) in [
        ("repository_role", repo.repository_role.as_str()),
        ("default_branch", repo.default_branch.as_str()),
        ("workflow_path", repo.workflow_path.as_str()),
        ("runtime_product_id", repo.runtime_product_id.as_str()),
        (
            "runtime_release_version",
            repo.runtime_release_version.as_str(),
        ),
    ] {
        if value.trim().is_empty() {
            finding(
                findings,
                "manifest-field",
                &repo.repository,
                field,
                "required source field is empty",
            );
        }
    }
    for (field, value) in [
        ("generator_revision", repo.generator_revision.as_str()),
        ("workflow_revision", repo.workflow_revision.as_str()),
        ("runtime_source_sha", repo.runtime_source_sha.as_str()),
    ] {
        validate_sha(&repo.repository, field, value, findings);
    }
    for (field, value) in [
        ("generated_plan_digest", repo.generated_plan_digest.as_str()),
        (
            "generator_artifact_digest",
            repo.generator_artifact_digest.as_str(),
        ),
        ("configuration_digest", repo.configuration_digest.as_str()),
        ("generated_tree_digest", repo.generated_tree_digest.as_str()),
        ("scan_state_digest", repo.scan_state_digest.as_str()),
        ("job_image_digest", repo.job_image_digest.as_str()),
    ] {
        validate_digest(&repo.repository, field, value, findings);
    }
    check_workloads(
        &repo.repository,
        &repo.expected_workload_ids,
        &repo.workload_platform_architecture,
        findings,
    );
    if repo.expected_jobs.is_empty() {
        finding(
            findings,
            "empty-workloads",
            &repo.repository,
            "expected_jobs",
            "reviewed generated plan must contain non-empty expected jobs",
        );
    }
    let mut job_ids = BTreeSet::new();
    for job in &repo.expected_jobs {
        check_expected_job(&repo.repository, job, &repo.provider_eligibility, findings);
        if !job_ids.insert(job.job_id.clone()) {
            finding(
                findings,
                "duplicate-job",
                &repo.repository,
                "expected_jobs.job_id",
                format!("duplicate reviewed job {}", job.job_id),
            );
        }
    }
    check_contexts(
        &repo.repository,
        &repo.required_check_contexts_and_apps,
        findings,
    );
    check_eligibility(&repo.repository, &repo.provider_eligibility, findings);
    for (provider, contract) in &repo.host_contracts {
        if !repo.provider_eligibility.contains_key(provider) {
            finding(
                findings,
                "host-contract",
                &repo.repository,
                "host_contracts",
                format!("host contract names undeclared provider {provider}"),
            );
        }
        if contract.runner_kind.trim().is_empty() {
            finding(
                findings,
                "host-contract",
                &repo.repository,
                "host_contracts.runner_kind",
                format!("provider {provider} has empty runner kind"),
            );
        }
    }
}

fn check_snapshot(
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    findings: &mut Vec<Finding>,
) {
    if snapshot.source.collector.trim().is_empty()
        || snapshot.source.collector_revision.trim().is_empty()
    {
        finding(
            findings,
            "snapshot-source",
            "",
            "snapshot.source",
            "collector identity is required",
        );
    }
    if !snapshot.source.api_base.starts_with("https://")
        || !snapshot.source.api_base.contains("github")
    {
        finding(
            findings,
            "snapshot-source",
            "",
            "snapshot.source.api_base",
            "snapshot must identify the HTTPS GitHub API endpoint",
        );
    }
    if !snapshot.source.read_only {
        finding(
            findings,
            "snapshot-source",
            "",
            "snapshot.source.read_only",
            "authoritative collection must be read-only",
        );
    }
    if snapshot.source.page_count == 0 {
        finding(
            findings,
            "snapshot-source",
            "",
            "snapshot.source.page_count",
            "collection must record at least one API page",
        );
    }
    if !snapshot.source.permission_scopes.is_empty()
        && snapshot
            .source
            .permission_scopes
            .iter()
            .any(|scope| scope == "contents:read" || scope == "metadata:read")
    {
        // A safe identity/scope record is useful. Credentials never belong in
        // this field; the collector only records scope names.
    } else if snapshot.source.permission_scopes.is_empty() {
        finding(
            findings,
            "snapshot-access",
            "",
            "snapshot.source.permission_scopes",
            "collector must record non-secret read scopes",
        );
    }
    let mut seen = BTreeSet::new();
    for repo in &snapshot.repositories {
        if !seen.insert(repo.repository.clone()) {
            finding(
                findings,
                "snapshot-duplicate",
                &repo.repository,
                "snapshot.repositories",
                "repository appears more than once",
            );
        }
        if manifest
            .repositories
            .iter()
            .all(|candidate| candidate.repository != repo.repository)
        {
            finding(
                findings,
                "snapshot-out-of-scope",
                &repo.repository,
                "snapshot.repositories.repository",
                "snapshot row is absent from the fixed manifest",
            );
        }
        if repo.repository_id == 0 {
            finding(
                findings,
                "snapshot-field",
                &repo.repository,
                "repository_id",
                "GitHub repository ID must be positive",
            );
        }
        validate_sha(
            &repo.repository,
            "default_branch_sha",
            &repo.default_branch_sha,
            findings,
        );
        if repo.default_branch.trim().is_empty() {
            finding(
                findings,
                "snapshot-field",
                &repo.repository,
                "default_branch",
                "default branch is required",
            );
        }
        check_contexts(&repo.repository, &repo.ruleset.required_checks, findings);
        if !nonempty_url(&repo.ruleset.source_url) {
            finding(
                findings,
                "snapshot-source",
                &repo.repository,
                "ruleset.source_url",
                "ruleset source URL is required",
            );
        }
        if !repo.ruleset.pages_complete {
            finding(
                findings,
                "snapshot-pagination",
                &repo.repository,
                "ruleset.pages_complete",
                "ruleset pagination was not complete",
            );
        }
        if repo.workflows.is_empty() {
            finding(
                findings,
                "missing-workflow-inventory",
                &repo.repository,
                "workflows",
                "snapshot must include current workflow/source inventory",
            );
        }
        for workflow in &repo.workflows {
            if workflow.path.trim().is_empty() || !valid_sha(&workflow.revision) {
                finding(
                    findings,
                    "workflow-identity",
                    &repo.repository,
                    "workflows",
                    "workflow path and immutable revision are required",
                );
            }
            validate_sha(
                &repo.repository,
                "workflows.source_sha",
                &workflow.source_sha,
                findings,
            );
            if !nonempty_url(&workflow.source_url) {
                finding(
                    findings,
                    "snapshot-source",
                    &repo.repository,
                    "workflows.source_url",
                    "workflow source URL is required",
                );
            }
        }
        check_pull_requests(repo, findings);
        for execution in &repo.main_executions {
            check_execution_observation(&repo.repository, execution, findings);
        }
    }
    let expected = canonical_scope();
    let actual = snapshot
        .repositories
        .iter()
        .map(|repo| repo.repository.as_str())
        .collect::<BTreeSet<_>>();
    if snapshot.repositories.len() != REQUIRED_REPOSITORIES {
        finding(
            findings,
            "snapshot-count",
            "",
            "snapshot.repositories",
            format!("expected exactly {REQUIRED_REPOSITORIES} unique rows"),
        );
    }
    if actual != expected {
        finding(
            findings,
            "snapshot-scope",
            "",
            "snapshot.repositories",
            "snapshot must equal the fixed canonical 32-repository set exactly",
        );
    }
}

fn canonical_scope() -> BTreeSet<&'static str> {
    CANONICAL_REPOSITORIES.iter().copied().collect()
}

fn check_g0_inventory(
    snapshot: &SnapshotDocument,
    inventory: Option<&G0InventoryEvidence>,
    findings: &mut Vec<Finding>,
) {
    let Some(inventory) = inventory else {
        finding(
            findings,
            "missing-g0-inventory",
            "",
            "evidence.g0_inventory",
            "G0 requires typed branch/PR/check/workflow/dependency/access/model inventory",
        );
        return;
    };
    if !nonempty_url(&inventory.source_url) {
        finding(
            findings,
            "g0-inventory-source",
            "",
            "evidence.g0_inventory.source_url",
            "G0 inventory needs an immutable HTTPS source identity",
        );
    }
    let open_pr_count = snapshot
        .repositories
        .iter()
        .map(|repo| repo.open_prs.len() as u32)
        .sum::<u32>();
    let workflow_repository_count = snapshot
        .repositories
        .iter()
        .filter(|repo| !repo.workflows.is_empty())
        .count() as u32;
    for (field, expected, actual) in [
        (
            "repository_count",
            REQUIRED_REPOSITORIES as u32,
            inventory.repository_count,
        ),
        ("open_pr_count", open_pr_count, inventory.open_pr_count),
        (
            "workflow_repository_count",
            workflow_repository_count,
            inventory.workflow_repository_count,
        ),
        (
            "ruleset_repository_count",
            snapshot.repositories.len() as u32,
            inventory.ruleset_repository_count,
        ),
    ] {
        if expected != actual {
            finding(
                findings,
                "g0-inventory-mismatch",
                "",
                format!("evidence.g0_inventory.{field}"),
                format!("declared {actual}, independently observed {expected}"),
            );
        }
    }
    validate_digest(
        "",
        "evidence.g0_inventory.workload_matrix_digest",
        &inventory.workload_matrix_digest,
        findings,
    );
    validate_digest(
        "",
        "evidence.g0_inventory.dependency_graph_digest",
        &inventory.dependency_graph_digest,
        findings,
    );
    if inventory.access_scopes.is_empty() {
        finding(
            findings,
            "g0-inventory-access",
            "",
            "evidence.g0_inventory.access_scopes",
            "G0 inventory must record observed non-secret access scopes",
        );
    }
    for (field, actual, expected) in [
        (
            "orchestrator_model",
            inventory.orchestrator_model.as_str(),
            EXPECTED_ORCHESTRATOR_MODEL,
        ),
        (
            "orchestrator_effort",
            inventory.orchestrator_effort.as_str(),
            EXPECTED_ORCHESTRATOR_EFFORT,
        ),
        (
            "agent_model",
            inventory.agent_model.as_str(),
            EXPECTED_AGENT_MODEL,
        ),
        (
            "agent_effort",
            inventory.agent_effort.as_str(),
            EXPECTED_AGENT_EFFORT,
        ),
    ] {
        if actual != expected {
            finding(
                findings,
                "g0-inventory-model",
                "",
                format!("evidence.g0_inventory.{field}"),
                format!("expected recorded runtime metadata {expected}"),
            );
        }
    }
}

fn check_pull_requests(repo: &SnapshotRepository, findings: &mut Vec<Finding>) {
    let mut seen = BTreeSet::new();
    for pr in &repo.open_prs {
        if !seen.insert(pr.number) {
            finding(
                findings,
                "snapshot-duplicate-pr",
                &repo.repository,
                "open_prs.number",
                format!("PR #{} appears more than once", pr.number),
            );
        }
        if pr.number == 0 || pr.state != "open" {
            finding(
                findings,
                "snapshot-pr-field",
                &repo.repository,
                "open_prs",
                "current PR inventory must contain positive open PR identities",
            );
        }
        validate_sha(
            &repo.repository,
            "open_prs.head_sha",
            &pr.head_sha,
            findings,
        );
        validate_sha(
            &repo.repository,
            "open_prs.base_sha",
            &pr.base_sha,
            findings,
        );
        if let Some(merge_sha) = &pr.merge_sha {
            validate_sha(&repo.repository, "open_prs.merge_sha", merge_sha, findings);
        }
        if !nonempty_url(&pr.source_url) {
            finding(
                findings,
                "snapshot-source",
                &repo.repository,
                "open_prs.source_url",
                "PR source URL is required",
            );
        }
        for execution in &pr.executions {
            check_execution_observation(&repo.repository, execution, findings);
        }
    }
}

fn check_execution_observation(
    repository: &str,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    if execution.run_id == 0 || execution.run_attempt == 0 {
        finding(
            findings,
            "run-identity",
            repository,
            "run_id/run_attempt",
            "run identity must be positive",
        );
    }
    if !nonempty_url(&execution.run_url)
        || execution.workflow_path.trim().is_empty()
        || !valid_sha(&execution.workflow_revision)
    {
        finding(
            findings,
            "run-identity",
            repository,
            "execution",
            "run URL, workflow path, and immutable workflow revision are required",
        );
    }
    validate_sha(
        repository,
        "execution.trigger_source_sha",
        &execution.trigger_source_sha,
        findings,
    );
    validate_sha(
        repository,
        "execution.actual_checkout_sha",
        &execution.actual_checkout_sha,
        findings,
    );
    if ![
        "push",
        "pull_request",
        "merge_group",
        "workflow_dispatch",
        "workflow_run",
    ]
    .contains(&execution.event.as_str())
    {
        finding(
            findings,
            "event",
            repository,
            "execution.event",
            "unsupported execution event",
        );
    }
    if execution.provider != "github" && execution.provider != "velnor" {
        finding(
            findings,
            "wrong-provider",
            repository,
            "execution.provider",
            "provider must be github or velnor",
        );
    }
    if execution.jobs.is_empty() {
        finding(
            findings,
            "empty-workloads",
            repository,
            "execution.jobs",
            "authoritative run must expose non-empty jobs",
        );
    }
    let mut ids = BTreeSet::new();
    for job in &execution.jobs {
        if !ids.insert(job.job_id.clone()) {
            finding(
                findings,
                "duplicate-job",
                repository,
                "execution.jobs.job_id",
                format!("duplicate actual job {}", job.job_id),
            );
        }
        if job.provider != execution.provider
            || job.event != execution.event
            || job.status != "completed"
            || job.conclusion != "success"
        {
            finding(
                findings,
                "job-conclusion",
                repository,
                "execution.jobs",
                format!(
                    "job {} is not a successful completed job for this provider",
                    job.job_id
                ),
            );
        }
        if !nonempty_url(&job.source_url) {
            finding(
                findings,
                "job-source",
                repository,
                "execution.jobs.source_url",
                "job source URL is required",
            );
        }
    }
    for check in &execution.required_checks {
        if check.run_id != execution.run_id
            || check.event != execution.event
            || check.status != "completed"
            || check.conclusion != "success"
            || !nonempty_url(&check.source_url)
        {
            finding(
                findings,
                "check-conclusion",
                repository,
                "execution.required_checks",
                format!(
                    "required check {} lacks successful run/job identity",
                    check.context
                ),
            );
        }
    }
    for child in &execution.child_runs {
        if child.parent_run_id != execution.run_id
            || child.run_id == 0
            || child.run_attempt == 0
            || child.status != "completed"
            || child.conclusion != "success"
            || !nonempty_url(&child.source_url)
        {
            finding(
                findings,
                "child-run-conclusion",
                repository,
                "execution.child_runs",
                "child run must be a successful completed run with identity",
            );
        }
    }
}

fn index_manifest<'a>(
    manifest: &'a ManifestDocument,
    findings: &mut Vec<Finding>,
) -> BTreeMap<String, &'a ManifestRepository> {
    let mut result = BTreeMap::new();
    for repo in &manifest.repositories {
        if result.insert(repo.repository.clone(), repo).is_some() {
            finding(
                findings,
                "manifest-duplicate",
                &repo.repository,
                "manifest.repositories",
                "duplicate prevents deterministic indexing",
            );
        }
    }
    result
}

fn index_snapshot<'a>(
    snapshot: &'a SnapshotDocument,
    findings: &mut Vec<Finding>,
) -> BTreeMap<String, &'a SnapshotRepository> {
    let mut result = BTreeMap::new();
    for repo in &snapshot.repositories {
        if result.insert(repo.repository.clone(), repo).is_some() {
            finding(
                findings,
                "snapshot-duplicate",
                &repo.repository,
                "snapshot.repositories",
                "duplicate prevents deterministic indexing",
            );
        }
    }
    result
}

fn check_scope_coverage(
    manifest: &BTreeMap<String, &ManifestRepository>,
    snapshot: &BTreeMap<String, &SnapshotRepository>,
    findings: &mut Vec<Finding>,
) {
    for name in manifest.keys() {
        if !snapshot.contains_key(name) {
            finding(
                findings,
                "missing-snapshot-repository",
                name,
                "snapshot.repositories",
                "manifest repository has no authoritative snapshot row",
            );
        }
    }
    for name in snapshot.keys() {
        if !manifest.contains_key(name) {
            finding(
                findings,
                "snapshot-out-of-scope",
                name,
                "snapshot.repositories",
                "snapshot row is absent from the fixed manifest",
            );
        }
    }
}

fn check_record_coverage(
    stage: Stage,
    manifest: &BTreeMap<String, &ManifestRepository>,
    snapshot: &BTreeMap<String, &SnapshotRepository>,
    records: &BTreeMap<RecordKey, &EvidenceRecord>,
    findings: &mut Vec<Finding>,
) {
    for (name, repo) in manifest {
        let snapshot_repo = snapshot.get(name).copied();
        let providers = required_providers(stage, repo);
        if stage == Stage::G0 {
            if !records.keys().any(|key| key.repository == *name) {
                finding(
                    findings,
                    "missing-repository",
                    name,
                    "records",
                    "G0 inventory has no record for this repository",
                );
            }
            continue;
        }
        for provider in providers {
            let has_main = records.keys().any(|key| {
                key.repository == *name
                    && key.provider == provider
                    && key.pr_number.is_none()
                    && key.event == "push"
            });
            if !has_main {
                finding(
                    findings,
                    "missing-main-evidence",
                    name,
                    "records",
                    format!("missing {provider} record for current default branch"),
                );
            }
            let Some(snapshot_repo) = snapshot_repo else {
                continue;
            };
            if stage.needs_all_prs() {
                for pr in &snapshot_repo.open_prs {
                    let has_pr = records.keys().any(|key| {
                        key.repository == *name
                            && key.provider == provider
                            && key.pr_number == Some(pr.number)
                            && key.event == "pull_request"
                    });
                    if !has_pr {
                        finding(
                            findings,
                            "missing-pr-evidence",
                            name,
                            "records",
                            format!("missing {provider} evidence for current PR #{}", pr.number),
                        );
                    }
                }
            }
        }
    }
}

fn check_lane_parity(
    stage: Stage,
    records: &BTreeMap<RecordKey, &EvidenceRecord>,
    findings: &mut Vec<Finding>,
) {
    if !stage.needs_both_lanes() {
        return;
    }
    for (key, velnor) in records {
        if key.provider != "velnor" {
            continue;
        }
        let github = records.iter().find_map(|(candidate_key, record)| {
            (candidate_key.repository == key.repository
                && candidate_key.event == key.event
                && candidate_key.pr_number == key.pr_number
                && candidate_key.provider == "github")
                .then_some(*record)
        });
        let Some(github) = github else {
            continue;
        };
        if velnor.trigger_source_sha != github.trigger_source_sha
            || velnor.actual_checkout_sha != github.actual_checkout_sha
            || velnor.expected_workload_ids != github.expected_workload_ids
            || velnor.workload_platform_architecture != github.workload_platform_architecture
            || velnor.expected_jobs != github.expected_jobs
        {
            finding(
                findings,
                "lane-parity",
                &key.repository,
                "provider/source/workloads",
                "GitHub and Velnor records do not prove the same source/workload contract",
            );
        }
        if stage == Stage::G6 || stage == Stage::G7 {
            let github_release = github
                .release
                .as_ref()
                .and_then(|release| release.execution.as_ref());
            let velnor_release = velnor
                .release
                .as_ref()
                .and_then(|release| release.execution.as_ref());
            let github_digest =
                github_release.map(|release| release.producer.manifest_sha256.as_str());
            let velnor_digest =
                velnor_release.map(|release| release.producer.manifest_sha256.as_str());
            if github_digest != velnor_digest {
                finding(
                    findings,
                    "single-publisher",
                    &key.repository,
                    "release.execution.producer.manifest_sha256",
                    "dual lanes must consume one producer-owned release manifest digest",
                );
            }
        }
    }
}

fn required_providers(stage: Stage, repo: &ManifestRepository) -> Vec<&str> {
    let github = matches!(
        repo.provider_eligibility.get("github"),
        Some(Eligibility::Eligible)
    );
    let velnor = matches!(
        repo.provider_eligibility.get("velnor"),
        Some(Eligibility::Eligible)
    );
    let mut result = Vec::new();
    if github && stage.needs_execution() {
        result.push("github");
    }
    if velnor && stage.needs_both_lanes() {
        result.push("velnor");
    }
    result
}

fn check_record(
    stage: Stage,
    index: RepositoryIndex<'_>,
    record: &EvidenceRecord,
    release: Option<&CanonicalReleaseDocument>,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let manifest = index.manifest;
    let snapshot = index.snapshot;
    if record.repository_role != manifest.repository_role
        || record.default_branch != manifest.default_branch
    {
        finding(
            findings,
            "manifest-mismatch",
            repo,
            "repository_role/default_branch",
            "record identity differs from reviewed manifest",
        );
    }
    if record.default_branch_sha != snapshot.default_branch_sha {
        finding(
            findings,
            "stale-sha",
            repo,
            "default_branch_sha",
            "record branch SHA differs from the authoritative snapshot",
        );
    }
    if !valid_timestamp(&record.observed_at_utc) {
        finding(
            findings,
            "invalid-timestamp",
            repo,
            "observed_at_utc",
            "record timestamp must be RFC3339 UTC",
        );
    }
    for (field, expected, actual) in [
        (
            "generator_revision",
            &manifest.generator_revision,
            &record.generator_revision,
        ),
        (
            "runtime_product_id",
            &manifest.runtime_product_id,
            &record.runtime_product_id,
        ),
        (
            "generator_artifact_digest",
            &manifest.generator_artifact_digest,
            &record.generator_artifact_digest,
        ),
        (
            "configuration_digest",
            &manifest.configuration_digest,
            &record.configuration_digest,
        ),
        (
            "generated_tree_digest",
            &manifest.generated_tree_digest,
            &record.generated_tree_digest,
        ),
        (
            "scan_state_digest",
            &manifest.scan_state_digest,
            &record.scan_state_digest,
        ),
        (
            "runtime_release_version",
            &manifest.runtime_release_version,
            &record.runtime_release_version,
        ),
        (
            "runtime_source_sha",
            &manifest.runtime_source_sha,
            &record.runtime_source_sha,
        ),
        (
            "job_image_digest",
            &manifest.job_image_digest,
            &record.job_image_digest,
        ),
    ] {
        let equal = if field.ends_with("digest") {
            digest_equal(expected, actual)
        } else {
            expected == actual
        };
        if !equal {
            finding(
                findings,
                "manifest-mismatch",
                repo,
                field,
                "record pin differs from reviewed source manifest",
            );
        }
    }
    if record.expected_workload_ids != manifest.expected_workload_ids
        || record.workload_platform_architecture != manifest.workload_platform_architecture
    {
        finding(
            findings,
            "workload-mismatch",
            repo,
            "expected_workload_ids/workload_platform_architecture",
            "record workload matrix differs from reviewed source plan",
        );
    }
    check_workloads(
        repo,
        &record.expected_workload_ids,
        &record.workload_platform_architecture,
        findings,
    );
    if stage == Stage::G0 {
        check_g0_record(record, findings);
        return;
    }
    if record.provider_eligibility != manifest.provider_eligibility {
        finding(
            findings,
            "eligibility-mismatch",
            repo,
            "provider_eligibility",
            "record provider policy differs from reviewed manifest",
        );
    }
    let Some(eligibility) = manifest.provider_eligibility.get(&record.provider) else {
        finding(
            findings,
            "wrong-provider",
            repo,
            "provider",
            "record provider has no reviewed eligibility",
        );
        return;
    };
    if *eligibility != Eligibility::Eligible {
        finding(
            findings,
            "wrong-provider",
            repo,
            "provider",
            "record uses a provider that is not eligible in the reviewed plan",
        );
    }
    if stage.needs_execution() {
        let expected_provider = required_providers(stage, manifest);
        if !expected_provider.contains(&record.provider.as_str()) {
            finding(
                findings,
                "wrong-provider",
                repo,
                "provider",
                format!(
                    "{} does not require this provider for the selected gate",
                    stage.as_str()
                ),
            );
        }
        check_execution_record(stage, index, record, findings);
    }
    if stage.needs_release() && record.pr_number.is_none() {
        check_release_install(index, record, release, findings);
    }
    if stage.needs_review() {
        if record.owner.trim().is_empty() || record.reviewer.trim().is_empty() {
            finding(
                findings,
                "missing-review",
                repo,
                "owner/reviewer",
                "G7 requires explicit owner and independent reviewer identities",
            );
        } else if record.owner == record.reviewer {
            finding(
                findings,
                "self-review",
                repo,
                "reviewer",
                "owner and reviewer must differ",
            );
        }
    }
    // gate_status is a report field, never an authorization input. A claimed
    // pass cannot hide any finding collected above.
    if record.gate_status != "pass" {
        finding(
            findings,
            "gate-status",
            repo,
            "gate_status",
            "record must declare pass after independently verified facts",
        );
    }
    if record.blocker.is_some() || record.next_action.is_some() {
        finding(
            findings,
            "unfinished-record",
            repo,
            "blocker/next_action",
            "a passing record cannot carry an unresolved blocker or next action",
        );
    }
}

fn check_g0_record(record: &EvidenceRecord, findings: &mut Vec<Finding>) {
    if record.provider != "inventory" || record.event != "inventory" {
        finding(
            findings,
            "g0-record-role",
            &record.repository,
            "provider/event",
            "G0 rows are inventory observations, never execution/provider claims",
        );
    }
    if record.run_id != 0 || record.run_attempt != 0 {
        finding(
            findings,
            "g0-record-run",
            &record.repository,
            "run_id/run_attempt",
            "G0 inventory rows cannot claim an execution",
        );
    }
    if record.gate_status != "inventory" {
        finding(
            findings,
            "g0-record-status",
            &record.repository,
            "gate_status",
            "G0 uses independently checked inventory status, not pass/self-attestation",
        );
    }
}

fn check_execution_record(
    stage: Stage,
    index: RepositoryIndex<'_>,
    record: &EvidenceRecord,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let snapshot_execution = find_execution(index.snapshot, record);
    let Some(execution) = snapshot_execution else {
        finding(
            findings,
            "missing-run",
            repo,
            "run_id",
            "run identity is absent from the authoritative PR/main execution graph",
        );
        return;
    };
    if execution.run_id != record.run_id || execution.run_attempt != record.run_attempt {
        finding(
            findings,
            "run-mismatch",
            repo,
            "run_id/run_attempt",
            "record run identity differs from the authoritative run",
        );
    }
    for (field, expected, actual) in [
        (
            "run_url",
            execution.run_url.as_str(),
            record.run_url.as_str(),
        ),
        (
            "workflow_path",
            execution.workflow_path.as_str(),
            record.workflow_path.as_str(),
        ),
        (
            "workflow_revision",
            execution.workflow_revision.as_str(),
            record.workflow_revision.as_str(),
        ),
        (
            "trigger_source_sha",
            execution.trigger_source_sha.as_str(),
            record.trigger_source_sha.as_str(),
        ),
        (
            "actual_checkout_sha",
            execution.actual_checkout_sha.as_str(),
            record.actual_checkout_sha.as_str(),
        ),
        ("event", execution.event.as_str(), record.event.as_str()),
        (
            "provider",
            execution.provider.as_str(),
            record.provider.as_str(),
        ),
        (
            "runner_name",
            execution.runner_name.as_str(),
            record.runner_name.as_str(),
        ),
        (
            "host_id",
            execution.host_id.as_str(),
            record.host_id.as_str(),
        ),
        (
            "runner_kind",
            execution.runner_kind.as_str(),
            record.runner_kind.as_str(),
        ),
        (
            "run_status",
            execution.status.as_str(),
            record.run_status.as_str(),
        ),
        (
            "run_conclusion",
            execution.conclusion.as_str(),
            record.run_conclusion.as_str(),
        ),
    ] {
        if expected != actual {
            finding(
                findings,
                "run-mismatch",
                repo,
                field,
                "record execution identity differs from authoritative run facts",
            );
        }
    }
    if execution.status != "completed" || execution.conclusion != "success" {
        finding(
            findings,
            "run-conclusion",
            repo,
            "run_status/run_conclusion",
            "required run must be completed with success conclusion",
        );
    }
    if execution.workflow_path != index.manifest.workflow_path
        || execution.workflow_revision != index.manifest.workflow_revision
    {
        finding(
            findings,
            "workflow-identity",
            repo,
            "workflow_path/workflow_revision",
            "executed workflow is not the reviewed source workflow revision",
        );
    }
    check_source_semantics(index.snapshot, record, execution, findings);
    check_host_binding(index.manifest, record, execution, findings);
    check_authoritative_jobs(index.manifest, record, execution, findings);
    check_authoritative_checks(index.snapshot, record, execution, findings);
    check_authoritative_children(index.manifest, record, execution, findings);
    if record.logs.is_empty() || record.logs.iter().any(|url| !nonempty_url(url)) {
        finding(
            findings,
            "missing-log",
            repo,
            "logs",
            "every executed record needs at least one HTTPS immutable log URL",
        );
    }
    let _ = stage;
}

fn find_execution<'a>(
    snapshot: &'a SnapshotRepository,
    record: &EvidenceRecord,
) -> Option<&'a ExecutionObservation> {
    if let Some(number) = record.pr_number {
        snapshot
            .open_prs
            .iter()
            .find(|pr| pr.number == number)
            .and_then(|pr| pr.executions.iter().find(|run| run.run_id == record.run_id))
    } else {
        snapshot
            .main_executions
            .iter()
            .find(|run| run.run_id == record.run_id)
    }
}

fn check_source_semantics(
    snapshot: &SnapshotRepository,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    if record.pr_number.is_none() {
        if execution.event != "push" {
            finding(
                findings,
                "event-source",
                repo,
                "event",
                "resulting-main evidence must use push; workflow_dispatch is diagnostic only",
            );
        }
        if execution.trigger_source_sha != snapshot.default_branch_sha
            || execution.actual_checkout_sha != snapshot.default_branch_sha
        {
            finding(
                findings,
                "stale-sha",
                repo,
                "trigger_source_sha/actual_checkout_sha",
                "main execution must check out the current default branch tip",
            );
        }
    } else {
        let number = record.pr_number.unwrap_or_default();
        let Some(pr) = snapshot.open_prs.iter().find(|pr| pr.number == number) else {
            finding(
                findings,
                "missing-pr",
                repo,
                "pr_number",
                "PR is absent from the current authoritative open-PR inventory",
            );
            return;
        };
        if execution.event != "pull_request" && execution.event != "merge_group" {
            finding(
                findings,
                "event-source",
                repo,
                "event",
                "PR evidence must use pull_request or merge_group",
            );
        }
        if record.pr_head_sha.as_deref() != Some(pr.head_sha.as_str())
            || record.pr_base_sha.as_deref() != Some(pr.base_sha.as_str())
        {
            finding(
                findings,
                "stale-sha",
                repo,
                "pr_head_sha/pr_base_sha",
                "record PR identity differs from current open PR",
            );
        }
        if execution.trigger_source_sha != pr.head_sha {
            finding(
                findings,
                "source-mismatch",
                repo,
                "trigger_source_sha",
                "PR execution must bind contributor head SHA",
            );
        }
        let Some(merge_sha) = pr.merge_sha.as_deref() else {
            finding(
                findings,
                "missing-merge-candidate",
                repo,
                "open_prs.merge_sha",
                "authoritative PR snapshot has no tested merge candidate",
            );
            return;
        };
        if record.tested_merge_sha.as_deref() != Some(merge_sha)
            || execution.actual_checkout_sha != merge_sha
        {
            finding(
                findings,
                "source-mismatch",
                repo,
                "tested_merge_sha/actual_checkout_sha",
                "PR execution must bind the current synthetic merge candidate",
            );
        }
    }
    if execution.event == "merge_group"
        && (record.merge_group_sha.as_deref() != Some(execution.trigger_source_sha.as_str())
            || execution.actual_checkout_sha != execution.trigger_source_sha)
    {
        finding(
            findings,
            "source-mismatch",
            repo,
            "merge_group_sha",
            "merge-group execution must use one immutable source SHA",
        );
    }
}

fn check_host_binding(
    manifest: &ManifestRepository,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let contract = manifest.host_contracts.get(&record.provider);
    if contract.is_none() {
        finding(
            findings,
            "host-contract",
            repo,
            "host_contracts",
            "provider has no reviewed host contract",
        );
    }
    if record.provider == "github" {
        if execution.runner_kind != "github-hosted"
            || execution.host_id == "github-hosted"
            || execution.host_id.trim().is_empty()
            || execution
                .runner_labels
                .iter()
                .any(|label| label == "self-hosted")
        {
            finding(
                findings,
                "host-trust",
                repo,
                "runner_kind/host_id/runner_labels",
                "GitHub evidence needs a concrete hosted runner binding, not a display boolean",
            );
        }
    } else if record.provider == "velnor"
        && (execution.runner_kind != "velnor-managed"
            || execution.host_id.trim().is_empty()
            || execution.host_id == "github-hosted"
            || execution
                .runner_labels
                .iter()
                .any(|label| label == "github-hosted"))
    {
        finding(
            findings,
            "host-trust",
            repo,
            "runner_kind/host_id/runner_labels",
            "Velnor evidence cannot use a GitHub-hosted identity or caller boolean",
        );
    }
    if let Some(contract) = contract
        && (contract.runner_kind != execution.runner_kind
            || contract
                .required_labels
                .iter()
                .any(|label| !execution.runner_labels.contains(label))
            || contract
                .forbidden_labels
                .iter()
                .any(|label| execution.runner_labels.contains(label)))
    {
        finding(
            findings,
            "host-trust",
            repo,
            "runner_kind/runner_labels",
            "actual runner binding violates reviewed provider host contract",
        );
    }
}

fn check_authoritative_jobs(
    manifest: &ManifestRepository,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let expected = manifest
        .expected_jobs
        .iter()
        .filter(|job| job.provider == record.provider && job.required)
        .collect::<Vec<_>>();
    if expected.is_empty() {
        finding(
            findings,
            "empty-workloads",
            repo,
            "manifest.expected_jobs",
            "provider has no non-empty reviewed expected job plan",
        );
        return;
    }
    let expected_ids = expected
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let actual_ids = execution
        .jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let actual_names = execution
        .jobs
        .iter()
        .map(|job| job.job_name.clone())
        .collect::<BTreeSet<_>>();
    let record_ids = record
        .actual_job_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if record_ids.len() != record.actual_job_ids.len() {
        finding(
            findings,
            "duplicate-job",
            repo,
            "actual_job_ids",
            "actual job identities must be unique",
        );
    }
    let record_expected = record
        .expected_jobs
        .iter()
        .filter(|job| job.provider == record.provider && job.required)
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let manifest_expected_all = manifest
        .expected_jobs
        .iter()
        .filter(|job| job.provider == record.provider && job.required)
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    if expected_ids != actual_names
        || record_ids != actual_ids
        || expected_ids != record_expected
        || record.expected_jobs.len()
            != manifest
                .expected_jobs
                .iter()
                .filter(|job| job.provider == record.provider)
                .count()
        || manifest_expected_all != expected_ids
    {
        finding(
            findings,
            "job-inventory-mismatch",
            repo,
            "expected_jobs/actual_job_ids",
            "actual jobs and record claims must equal the reviewed source plan exactly",
        );
    }
    for job in expected {
        let Some(actual) = execution
            .jobs
            .iter()
            .find(|candidate| candidate.job_name == job.job_id)
        else {
            finding(
                findings,
                "missing-job",
                repo,
                "execution.jobs",
                format!(
                    "reviewed job {} is missing from the authoritative run",
                    job.job_id
                ),
            );
            continue;
        };
        if actual.workload_id != job.workload_id
            || actual.platform != job.platform
            || actual.architecture != job.architecture
            || actual.provider != record.provider
            || actual.event != record.event
            || actual.status != "completed"
            || actual.conclusion != "success"
        {
            finding(
                findings,
                "job-mismatch",
                repo,
                "execution.jobs",
                format!(
                    "job {} does not satisfy the reviewed workload/provider/target contract",
                    job.job_id
                ),
            );
        }
        if record
            .actual_job_conclusions
            .get(&actual.job_id)
            .map(String::as_str)
            != Some("success")
        {
            finding(
                findings,
                "job-conclusion",
                repo,
                "actual_job_conclusions",
                format!(
                    "record lacks a successful conclusion for job {}",
                    job.job_id
                ),
            );
        }
    }
}

fn check_authoritative_checks(
    snapshot: &SnapshotRepository,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let required = snapshot
        .ruleset
        .required_checks
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual = execution
        .required_checks
        .iter()
        .map(|check| RequiredContext {
            context: check.context.clone(),
            app_id: check.app_id.clone(),
        })
        .collect::<BTreeSet<_>>();
    let claimed = record
        .required_checks
        .iter()
        .map(|check| RequiredContext {
            context: check.context.clone(),
            app_id: check.app_id.clone(),
        })
        .collect::<BTreeSet<_>>();
    if claimed.len() != record.required_checks.len() {
        finding(
            findings,
            "check-context-mismatch",
            repo,
            "required_checks",
            "required check claims must be unique",
        );
    }
    if required != actual || required != claimed {
        finding(
            findings,
            "check-context-mismatch",
            repo,
            "required_checks",
            "required context/app identities must come from current ruleset and run facts",
        );
    }
    if record.required_checks.len() != claimed.len() {
        finding(
            findings,
            "check-context-mismatch",
            repo,
            "required_checks",
            "required check claims must be unique",
        );
    }
    for check in &record.required_checks {
        if check.run_id != execution.run_id
            || check.event != execution.event
            || check.status != "completed"
            || check.conclusion != "success"
            || !nonempty_url(&check.source_url)
        {
            finding(
                findings,
                "check-conclusion",
                repo,
                "required_checks",
                format!(
                    "required check {} is absent/failed/skipped or lacks source identity",
                    check.context
                ),
            );
        }
        if !execution.jobs.iter().any(|job| job.job_id == check.job_id) {
            finding(
                findings,
                "check-job-association",
                repo,
                "required_checks.job_id",
                format!("required check {} names no actual job", check.context),
            );
        }
    }
}

fn check_authoritative_children(
    manifest: &ManifestRepository,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let expected = manifest
        .expected_jobs
        .iter()
        .filter(|job| job.provider == record.provider && job.required)
        .filter_map(|job| job.child_workflow.as_ref())
        .count();
    if expected != execution.child_runs.len() || expected != record.child_run_links.len() {
        finding(
            findings,
            "child-run-inventory",
            repo,
            "child_run_links",
            "child-run graph must equal the reviewed recursive workflow plan",
        );
    }
    let child_specs = manifest
        .expected_jobs
        .iter()
        .filter(|job| job.provider == record.provider && job.required)
        .filter_map(|job| job.child_workflow.as_ref())
        .collect::<Vec<_>>();
    for child in &execution.child_runs {
        if !child_specs.iter().any(|spec| {
            spec.repository == child.repository
                && spec.workflow_path == child.workflow_path
                && spec.event == child.event
        }) || (child.source_sha != record.trigger_source_sha
            && child.source_sha != record.actual_checkout_sha)
        {
            finding(
                findings,
                "child-run-mismatch",
                repo,
                "execution.child_runs",
                format!(
                    "child run {} is not in the reviewed recursive workflow plan",
                    child.run_id
                ),
            );
        }
    }
    for link in &record.child_run_links {
        let Some(actual) = execution
            .child_runs
            .iter()
            .find(|run| run.run_id == link.run_id)
        else {
            finding(
                findings,
                "missing-child-run",
                repo,
                "child_run_links.run_id",
                format!(
                    "child run {} is absent from authoritative graph",
                    link.run_id
                ),
            );
            continue;
        };
        if link.run_attempt != actual.run_attempt
            || link.parent_run_id != actual.parent_run_id
            || link.repository != actual.repository
            || link.workflow_path != actual.workflow_path
            || link.event != actual.event
            || link.source_sha != actual.source_sha
            || link.provider != actual.provider
            || link.status != actual.status
            || link.conclusion != actual.conclusion
            || link.run_url != actual.source_url
        {
            finding(
                findings,
                "child-run-mismatch",
                repo,
                "child_run_links",
                format!(
                    "child run {} claim differs from authoritative child graph",
                    link.run_id
                ),
            );
        }
    }
}

fn check_release_install(
    index: RepositoryIndex<'_>,
    record: &EvidenceRecord,
    release_document: Option<&CanonicalReleaseDocument>,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let Some(release) = record.release.as_ref() else {
        finding(
            findings,
            "missing-release-evidence",
            repo,
            "release",
            "G2+ requires a typed release object",
        );
        return;
    };
    let Some(install) = record.install.as_ref() else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install",
            "G2+ requires a typed install object",
        );
        return;
    };
    if index.manifest.release_applicability == Applicability::Required
        && release.applicability != Applicability::Required
    {
        finding(
            findings,
            "release-applicability",
            repo,
            "release.applicability",
            "record cannot downgrade a required release to not-applicable",
        );
    }
    if index.manifest.release_applicability == Applicability::Required
        && install.applicability != Applicability::Required
    {
        finding(
            findings,
            "install-applicability",
            repo,
            "install.applicability",
            "record cannot downgrade a required install to not-applicable",
        );
    }
    if release.applicability != Applicability::Required {
        if release
            .justification
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            finding(
                findings,
                "missing-justification",
                repo,
                "release.justification",
                "non-applicable release requires a reason",
            );
        }
        if index.manifest.release_applicability == Applicability::Required {
            return;
        }
    }
    if install.applicability != Applicability::Required {
        if install
            .justification
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            finding(
                findings,
                "missing-justification",
                repo,
                "install.justification",
                "non-applicable install requires a reason",
            );
        }
        if index.manifest.release_applicability == Applicability::Required {
            return;
        }
        return;
    }
    let Some(canonical) = release_document else {
        finding(
            findings,
            "missing-release-manifest",
            repo,
            "release_manifest",
            "required release requires the independent producer-owned canonical manifest",
        );
        return;
    };
    check_canonical_release(repo, record, release, install, canonical, findings);
}

fn check_canonical_release(
    repo: &str,
    record: &EvidenceRecord,
    release: &ReleaseEvidence,
    install: &InstallEvidence,
    document: &CanonicalReleaseDocument,
    findings: &mut Vec<Finding>,
) {
    let manifest = &document.manifest;
    if document.schema_version != CANONICAL_RELEASE_SCHEMA_VERSION {
        finding(
            findings,
            "release-schema",
            repo,
            "release_manifest.schema_version",
            format!("expected {CANONICAL_RELEASE_SCHEMA_VERSION}"),
        );
    }
    if manifest.schema != "velnor.application-manifest.v1" {
        finding(
            findings,
            "release-schema",
            repo,
            "release_manifest.manifest.schema",
            "only the canonical application manifest schema is accepted",
        );
    }
    let computed = digest_canonical_json(&manifest_as_value(manifest));
    if !digest_equal(&computed, &document.manifest_sha256) {
        finding(
            findings,
            "release-digest",
            repo,
            "release_manifest.manifest_sha256",
            "external digest does not match canonical manifest bytes",
        );
    }
    if manifest.product_id != record.runtime_product_id {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release_manifest.product_id",
            "producer manifest product identity differs from runtime pin",
        );
    }
    if manifest.channel != "preview" && manifest.channel != "stable" {
        finding(
            findings,
            "release-identity",
            repo,
            "release_manifest.manifest.channel",
            "canonical release channel must be preview or stable",
        );
    }
    validate_repository_name(
        &manifest.source_repository,
        "release_manifest.manifest.source_repository",
        findings,
    );
    validate_repository_name(
        &manifest.producer.repository,
        "release_manifest.manifest.producer.repository",
        findings,
    );
    if manifest.version.trim().is_empty()
        || manifest.release_tag.trim().is_empty()
        || manifest.release_id.trim().is_empty()
        || !manifest.source_ref.starts_with("refs/tags/")
        || manifest.source_ref != format!("refs/tags/{}", manifest.release_tag)
        || !valid_sha(&manifest.source_commit)
        || !nonempty_url(&manifest.producer.run_url)
        || manifest.producer.run_id == 0
    {
        finding(
            findings,
            "release-identity",
            repo,
            "release_manifest.manifest",
            "canonical release requires immutable source/tag/producer identity",
        );
    }
    if manifest.source_repository != manifest.producer.repository {
        finding(
            findings,
            "release-identity",
            repo,
            "release_manifest.manifest.source_repository",
            "producer repository must equal canonical source repository",
        );
    }
    if manifest.artifacts.is_empty()
        || manifest.components.is_empty()
        || manifest.targets.is_empty()
    {
        finding(
            findings,
            "release-inventory",
            repo,
            "release_manifest.manifest",
            "canonical release must include non-empty artifact/component/target inventories",
        );
    }
    let Some(execution) = release.execution.as_ref() else {
        finding(
            findings,
            "missing-release-evidence",
            repo,
            "release.execution",
            "required release needs publication identity",
        );
        return;
    };
    for (field, expected, actual) in [
        (
            "channel",
            manifest.channel.as_str(),
            execution.channel.as_str(),
        ),
        (
            "version",
            manifest.version.as_str(),
            execution.version.as_str(),
        ),
        (
            "release_id",
            manifest.release_id.as_str(),
            execution.release_id.as_str(),
        ),
        (
            "source_commit",
            manifest.source_commit.as_str(),
            execution.tag_target_sha.as_str(),
        ),
    ] {
        if expected != actual {
            finding(
                findings,
                "mismatched-artifact",
                repo,
                field,
                "release execution differs from canonical producer manifest",
            );
        }
    }
    if manifest.source_commit != record.actual_checkout_sha
        || manifest.producer.source_commit != manifest.source_commit
        || execution.producer.source_commit != manifest.source_commit
    {
        finding(
            findings,
            "source-tag-mismatch",
            repo,
            "source_commit",
            "release source/tag/producer identities are not the executed immutable source",
        );
    }
    if execution.producer.manifest_sha256 != document.manifest_sha256
        || execution.apt.manifest_sha256 != document.manifest_sha256
        || execution.homebrew.manifest_sha256 != document.manifest_sha256
    {
        finding(
            findings,
            "manifest-digest-mismatch",
            repo,
            "release.execution.manifest_sha256",
            "producer and distribution projections must bind the external canonical digest",
        );
    }
    if execution.producer.repository != manifest.producer.repository
        || execution.producer.workflow_path != manifest.producer.workflow_path
        || execution.producer.run_id != manifest.producer.run_id
        || execution.producer.run_url != manifest.producer.run_url
    {
        finding(
            findings,
            "producer-mismatch",
            repo,
            "release.execution.producer",
            "release evidence does not bind the canonical producer run",
        );
    }
    if execution.producer.run_id != record.run_id
        && !record
            .child_run_links
            .iter()
            .any(|link| link.run_id == execution.producer.run_id)
    {
        finding(
            findings,
            "producer-mismatch",
            repo,
            "release.execution.producer.run_id",
            "producer run is absent from the independently verified parent/child run graph",
        );
    }
    if execution.tag_target_sha != record.actual_checkout_sha
        || !valid_sha(&execution.tag_target_sha)
        || !nonempty_url(&execution.producer.run_url)
    {
        finding(
            findings,
            "mismatched-artifact",
            repo,
            "release.execution.tag_target_sha",
            "release tag target must be the executed immutable source",
        );
    }
    check_asset_inventory(repo, execution, manifest, findings);
    check_publication_projections(repo, execution, manifest, findings);
    check_target_inventory(repo, manifest, findings);
    check_install_evidence(repo, record, install, document, findings);
}

fn check_asset_inventory(
    repo: &str,
    execution: &ReleaseExecution,
    manifest: &CanonicalReleaseManifest,
    findings: &mut Vec<Finding>,
) {
    let expected = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.name.clone())
        .collect::<BTreeSet<_>>();
    let actual = execution
        .asset_digests
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if expected != actual || expected.is_empty() {
        finding(
            findings,
            "artifact-inventory-mismatch",
            repo,
            "release.execution.asset_digests",
            "published asset inventory must equal canonical producer inventory",
        );
    }
    for artifact in &manifest.artifacts {
        validate_digest(repo, "release.artifacts.sha256", &artifact.sha256, findings);
        if artifact.size == 0 || !valid_target(&artifact.target) {
            finding(
                findings,
                "artifact-target",
                repo,
                "release.artifacts",
                format!("artifact {} has invalid size or target", artifact.name),
            );
        }
        match execution.asset_digests.get(&artifact.name) {
            Some(value) if digest_equal(value, &artifact.sha256) => {}
            _ => finding(
                findings,
                "mismatched-artifact",
                repo,
                "release.execution.asset_digests",
                format!("asset {} is absent or digest-rebound", artifact.name),
            ),
        }
    }
}

fn check_publication_projections(
    repo: &str,
    execution: &ReleaseExecution,
    manifest: &CanonicalReleaseManifest,
    findings: &mut Vec<Finding>,
) {
    if execution.apt.repository.trim().is_empty()
        || execution.apt.suite.trim().is_empty()
        || execution.apt.candidate != manifest.version
        || !valid_sha(&execution.apt.revision)
    {
        finding(
            findings,
            "apt-projection-mismatch",
            repo,
            "release.execution.apt",
            "APT projection is not bound to producer repository/revision/version",
        );
    }
    if execution.homebrew.tap.trim().is_empty()
        || execution.homebrew.formula.trim().is_empty()
        || execution.homebrew.version != manifest.version
        || !valid_sha(&execution.homebrew.revision)
    {
        finding(
            findings,
            "homebrew-projection-mismatch",
            repo,
            "release.execution.homebrew",
            "Homebrew projection is not bound to producer repository/revision/version",
        );
    }
}

fn check_target_inventory(
    repo: &str,
    manifest: &CanonicalReleaseManifest,
    findings: &mut Vec<Finding>,
) {
    let targets = manifest
        .targets
        .iter()
        .map(|target| target.target.as_str())
        .collect::<BTreeSet<_>>();
    let artifact_targets = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.target.as_str())
        .collect::<BTreeSet<_>>();
    for target in &manifest.targets {
        if !valid_target(&target.target)
            || target.platform.trim().is_empty()
            || target.architecture.trim().is_empty()
            || target.binaries.is_empty()
        {
            finding(
                findings,
                "target-inventory",
                repo,
                "release_manifest.manifest.targets",
                "target inventory rows must identify platform, architecture, and binaries",
            );
        }
    }
    if !artifact_targets.is_subset(&targets) {
        finding(
            findings,
            "target-inventory",
            repo,
            "release_manifest.manifest.targets",
            "every artifact target must be in canonical target inventory",
        );
    }
    let mut component_binaries = BTreeSet::new();
    for component in &manifest.components {
        if !valid_target(&component.target)
            || !targets.contains(component.target.as_str())
            || !artifact_targets.contains(
                manifest
                    .artifacts
                    .iter()
                    .find(|artifact| artifact.name == component.artifact)
                    .map(|artifact| artifact.target.as_str())
                    .unwrap_or(""),
            )
            || !component_binaries.insert(component.binary.clone())
        {
            finding(
                findings,
                "component-inventory",
                repo,
                "release_manifest.manifest.components",
                format!(
                    "component {} is not bound to one canonical target/artifact",
                    component.name
                ),
            );
        }
    }
}

fn check_install_evidence(
    repo: &str,
    record: &EvidenceRecord,
    install: &InstallEvidence,
    document: &CanonicalReleaseDocument,
    findings: &mut Vec<Finding>,
) {
    let manifest = &document.manifest;
    let Some(environment) = install.environment.as_ref() else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.environment",
            "typed clean installation environment is required",
        );
        return;
    };
    if environment.os_image.trim().is_empty()
        || environment.platform.trim().is_empty()
        || environment.architecture.trim().is_empty()
        || environment.runner.trim().is_empty()
        || environment.workspace.trim().is_empty()
        || environment.path.trim().is_empty()
        || !valid_target(&format!(
            "{}-{}",
            environment.platform, environment.architecture
        ))
    {
        finding(
            findings,
            "install-environment",
            repo,
            "install.environment",
            "install environment must be typed, clean, and target a supported architecture",
        );
    }
    if install.functional_result.as_deref() != Some("success") {
        finding(
            findings,
            "install-result",
            repo,
            "install.functional_result",
            "functional installed-product result must be success",
        );
    }
    let Some(operations) = install.operations.as_ref() else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.operations",
            "clean install, same-channel upgrade, and channel-switch operations are required",
        );
        return;
    };
    for (name, operation) in [
        ("clean_install", &operations.clean_install),
        ("same_channel_upgrade", &operations.same_channel_upgrade),
        ("channel_switch", &operations.channel_switch),
    ] {
        if operation.result != "success"
            || operation.observed_release_id != manifest.release_id
            || operation.observed_version != manifest.version
        {
            finding(
                findings,
                "install-operation",
                repo,
                "install.operations",
                format!("{name} does not prove the canonical release was installed successfully"),
            );
        }
    }
    if let Some(predecessor) = operations.same_channel_upgrade.predecessor.as_ref() {
        if predecessor.product_id != manifest.product_id
            || predecessor.channel != manifest.channel
            || !version_less(&predecessor.version, &manifest.version)
            || predecessor.release_id == manifest.release_id
            || !valid_sha(&predecessor.source_sha)
            || !valid_digest(&predecessor.manifest_sha256)
        {
            finding(
                findings,
                "install-operation",
                repo,
                "install.operations.same_channel_upgrade.predecessor",
                "same-channel upgrade needs a distinct older predecessor identity",
            );
        }
    } else {
        finding(
            findings,
            "install-operation",
            repo,
            "install.operations.same_channel_upgrade.predecessor",
            "same-channel upgrade predecessor is required",
        );
    }
    if let Some(predecessor) = operations.channel_switch.predecessor.as_ref() {
        if predecessor.product_id != manifest.product_id
            || predecessor.channel == manifest.channel
            || predecessor.version == manifest.version
            || predecessor.release_id == manifest.release_id
            || !valid_sha(&predecessor.source_sha)
            || !valid_digest(&predecessor.manifest_sha256)
        {
            finding(
                findings,
                "install-operation",
                repo,
                "install.operations.channel_switch.predecessor",
                "channel switch needs a distinct predecessor channel/version identity",
            );
        }
    } else {
        finding(
            findings,
            "install-operation",
            repo,
            "install.operations.channel_switch.predecessor",
            "channel switch predecessor is required",
        );
    }
    let Some(installed) = install.installed.as_ref() else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.installed",
            "installed product identity is required",
        );
        return;
    };
    if installed.product_id != manifest.product_id
        || installed.channel != manifest.channel
        || installed.version != manifest.version
        || installed.source_sha != manifest.source_commit
        || !digest_equal(&installed.manifest_sha256, &document.manifest_sha256)
        || installed.target != format!("{}-{}", environment.platform, environment.architecture)
    {
        finding(
            findings,
            "installed-identity-mismatch",
            repo,
            "install.installed",
            "installed product identity does not bind canonical source/manifest/target",
        );
    }
    let expected = manifest
        .components
        .iter()
        .map(|component| component.binary.clone())
        .collect::<BTreeSet<_>>();
    let actual = installed
        .binaries
        .iter()
        .map(|binary| binary.name.clone())
        .collect::<BTreeSet<_>>();
    if expected != actual || expected.is_empty() {
        finding(
            findings,
            "binary-inventory-mismatch",
            repo,
            "install.installed.binaries",
            "installed binaries must equal canonical component inventory",
        );
    }
    for binary in &installed.binaries {
        let Some(component) = manifest
            .components
            .iter()
            .find(|item| item.binary == binary.name)
        else {
            continue;
        };
        let artifact_digest = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.name == component.artifact)
            .map(|artifact| artifact.sha256.as_str());
        if binary.component != component.name
            || binary.artifact != component.artifact
            || binary.target != component.target
            || !binary.path.starts_with('/')
            || binary.source != "release-asset"
            || binary.source.trim().is_empty()
            || !valid_digest(&binary.sha256)
            || artifact_digest
                .map(|digest| !digest_equal(digest, &binary.sha256))
                .unwrap_or(true)
        {
            finding(
                findings,
                "binary-provenance",
                repo,
                "install.installed.binaries",
                format!(
                    "binary {} lacks immutable artifact/component/target provenance",
                    binary.name
                ),
            );
        }
    }
    let Some(service) = install.service.as_ref() else {
        finding(
            findings,
            "missing-install-evidence",
            repo,
            "install.service",
            "service result/applicability is required",
        );
        return;
    };
    let target = manifest.targets.iter().find(|item| {
        item.target == installed.target
            || item.target == format!("{}-{}", environment.platform, environment.architecture)
    });
    if target.is_none() {
        finding(
            findings,
            "target-inventory",
            repo,
            "release_manifest.targets",
            "installed target is absent from canonical target inventory",
        );
    }
    let service_applicability = target.map(|item| &item.service);
    if service_applicability == Some(&Applicability::Required)
        && (service.applicability != Applicability::Required || service.result != "systemd-success")
    {
        finding(
            findings,
            "service-result",
            repo,
            "install.service",
            "authoritative target requires a real systemd success result",
        );
    }
    if service.applicability != Applicability::Required
        && service
            .justification
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        finding(
            findings,
            "service-result",
            repo,
            "install.service.justification",
            "not-applicable service requires an authoritative reason",
        );
    }
    let _ = record;
}

fn check_expected_job(
    repository: &str,
    job: &ExpectedJobSpec,
    eligibility: &BTreeMap<String, Eligibility>,
    findings: &mut Vec<Finding>,
) {
    if job.job_id.trim().is_empty()
        || job.workload_id.trim().is_empty()
        || job.provider.trim().is_empty()
        || job.platform.trim().is_empty()
        || job.architecture.trim().is_empty()
        || !job.required
    {
        finding(
            findings,
            "expected-job",
            repository,
            "expected_jobs",
            "expected job identity, target, provider, and required=true are mandatory",
        );
    }
    if eligibility.get(&job.provider) != Some(&Eligibility::Eligible) {
        finding(
            findings,
            "expected-job",
            repository,
            "expected_jobs.provider",
            format!(
                "job provider {} is not eligible in reviewed plan",
                job.provider
            ),
        );
    }
    if !valid_target(&format!("{}-{}", job.platform, job.architecture)) {
        finding(
            findings,
            "unsupported-target",
            repository,
            "expected_jobs.platform/architecture",
            format!(
                "unsupported workload target {}-{}",
                job.platform, job.architecture
            ),
        );
    }
    if let Some(child) = &job.child_workflow {
        validate_repository_name(
            &child.repository,
            "expected_jobs.child_workflow.repository",
            findings,
        );
        if child.workflow_path.trim().is_empty() || child.event != "workflow_run" {
            finding(
                findings,
                "child-workflow",
                repository,
                "expected_jobs.child_workflow",
                "child workflow must identify a path and workflow_run event",
            );
        }
    }
}

fn check_workloads(
    repository: &str,
    workload_ids: &[String],
    platforms: &[WorkloadPlatform],
    findings: &mut Vec<Finding>,
) {
    if workload_ids.is_empty() {
        finding(
            findings,
            "empty-workloads",
            repository,
            "expected_workload_ids",
            "workload inventory must be non-empty",
        );
    }
    let expected = workload_ids.iter().cloned().collect::<BTreeSet<_>>();
    if expected.len() != workload_ids.len() {
        finding(
            findings,
            "duplicate-workload",
            repository,
            "expected_workload_ids",
            "workload identities must be unique",
        );
    }
    let actual = platforms
        .iter()
        .map(|platform| platform.workload_id.clone())
        .collect::<BTreeSet<_>>();
    if expected != actual || actual.len() != platforms.len() {
        finding(
            findings,
            "platform-mismatch",
            repository,
            "workload_platform_architecture",
            "each workload must have exactly one platform/architecture row",
        );
    }
    for platform in platforms {
        if !valid_target(&format!("{}-{}", platform.platform, platform.architecture)) {
            finding(
                findings,
                "unsupported-target",
                repository,
                "workload_platform_architecture",
                format!(
                    "unsupported target {}-{}",
                    platform.platform, platform.architecture
                ),
            );
        }
    }
}

fn check_contexts(repository: &str, contexts: &[RequiredContext], findings: &mut Vec<Finding>) {
    if contexts.is_empty() {
        finding(
            findings,
            "empty-check-contract",
            repository,
            "required_check_contexts_and_apps",
            "at least one required context/app identity is required",
        );
    }
    let mut seen = BTreeSet::new();
    for context in contexts {
        if context.context.trim().is_empty() || context.app_id.trim().is_empty() {
            finding(
                findings,
                "check-context",
                repository,
                "required_check_contexts_and_apps",
                "context and app_id must be non-empty",
            );
        }
        if !seen.insert(context.clone()) {
            finding(
                findings,
                "check-context",
                repository,
                "required_check_contexts_and_apps",
                format!("duplicate required context {}", context.context),
            );
        }
    }
}

fn check_eligibility(
    repository: &str,
    eligibility: &BTreeMap<String, Eligibility>,
    findings: &mut Vec<Finding>,
) {
    for provider in ["github", "velnor"] {
        if !eligibility.contains_key(provider) {
            finding(
                findings,
                "manifest-provider",
                repository,
                "provider_eligibility",
                format!("missing {provider} eligibility"),
            );
        }
    }
}

fn compare_snapshot_facts(
    supplied: &SnapshotDocument,
    live: &SnapshotDocument,
    findings: &mut Vec<Finding>,
) {
    let supplied_by_repo = supplied
        .repositories
        .iter()
        .map(|repo| (repo.repository.as_str(), repo))
        .collect::<BTreeMap<_, _>>();
    let live_names = live
        .repositories
        .iter()
        .map(|repo| repo.repository.as_str())
        .collect::<BTreeSet<_>>();
    let supplied_names = supplied_by_repo.keys().copied().collect::<BTreeSet<_>>();
    if supplied_names != live_names {
        finding(
            findings,
            "live-mismatch",
            "",
            "snapshot.repositories",
            "supplied and freshly collected repository inventories differ",
        );
    }
    for live_repo in &live.repositories {
        let Some(supplied_repo) = supplied_by_repo.get(live_repo.repository.as_str()) else {
            finding(
                findings,
                "live-mismatch",
                &live_repo.repository,
                "snapshot.repositories",
                "live collector found a repository absent from supplied snapshot",
            );
            continue;
        };
        if supplied_repo.default_branch != live_repo.default_branch
            || supplied_repo.default_branch_sha != live_repo.default_branch_sha
            || supplied_repo.ruleset.required_checks != live_repo.ruleset.required_checks
            || supplied_repo.workflows.len() != live_repo.workflows.len()
            || supplied_repo.main_executions.len() != live_repo.main_executions.len()
            || supplied_repo.open_prs.len() != live_repo.open_prs.len()
        {
            finding(
                findings,
                "live-mismatch",
                &live_repo.repository,
                "default_branch/open_prs",
                "supplied snapshot differs from fresh GitHub revision/PR facts",
            );
        }
        let supplied_workflows = supplied_repo
            .workflows
            .iter()
            .map(|workflow| (&workflow.path, &workflow.revision, &workflow.source_sha))
            .collect::<BTreeSet<_>>();
        let live_workflows = live_repo
            .workflows
            .iter()
            .map(|workflow| (&workflow.path, &workflow.revision, &workflow.source_sha))
            .collect::<BTreeSet<_>>();
        if supplied_workflows != live_workflows {
            finding(
                findings,
                "live-mismatch",
                &live_repo.repository,
                "workflows",
                "supplied snapshot differs from fresh workflow/source inventory",
            );
        }
        if execution_ids(&supplied_repo.main_executions)
            != execution_ids(&live_repo.main_executions)
        {
            finding(
                findings,
                "live-mismatch",
                &live_repo.repository,
                "main_executions",
                "supplied snapshot differs from fresh main run inventory",
            );
        }
        let supplied_prs = supplied_repo
            .open_prs
            .iter()
            .map(|pr| (pr.number, pr))
            .collect::<BTreeMap<_, _>>();
        for live_pr in &live_repo.open_prs {
            match supplied_prs.get(&live_pr.number) {
                Some(supplied_pr)
                    if supplied_pr.head_sha == live_pr.head_sha
                        && supplied_pr.base_sha == live_pr.base_sha
                        && supplied_pr.merge_sha == live_pr.merge_sha
                        && execution_ids(&supplied_pr.executions)
                            == execution_ids(&live_pr.executions) => {}
                _ => finding(
                    findings,
                    "live-mismatch",
                    &live_repo.repository,
                    "open_prs",
                    format!(
                        "supplied snapshot differs for current PR #{}",
                        live_pr.number
                    ),
                ),
            }
        }
    }
}

fn execution_ids(executions: &[ExecutionObservation]) -> BTreeSet<(u64, u32, String, String)> {
    executions
        .iter()
        .map(|execution| {
            (
                execution.run_id,
                execution.run_attempt,
                execution.status.clone(),
                execution.conclusion.clone(),
            )
        })
        .collect()
}

fn check_live_freshness(snapshot: &SnapshotDocument, findings: &mut Vec<Finding>) {
    let Ok(captured) = OffsetDateTime::parse(&snapshot.source.captured_at_utc, &Rfc3339) else {
        finding(
            findings,
            "snapshot-freshness",
            "",
            "snapshot.source.captured_at_utc",
            "live collector timestamp is invalid",
        );
        return;
    };
    let age = OffsetDateTime::now_utc() - captured;
    if age.whole_seconds() < 0 || age.whole_seconds() > LIVE_FRESHNESS_SECONDS {
        finding(
            findings,
            "snapshot-freshness",
            "",
            "snapshot.source.captured_at_utc",
            "live snapshot is not fresh enough for final reconciliation",
        );
    }
}

fn manifest_as_value(manifest: &CanonicalReleaseManifest) -> Value {
    serde_json::to_value(manifest).unwrap_or(Value::Null)
}

pub(crate) fn digest_canonical_json(value: &Value) -> String {
    let canonical = canonical_json(value);
    let digest = Sha256::digest(canonical.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned()),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            let entries = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_json(values.get(key).unwrap_or(&Value::Null))
                    )
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", entries.join(","))
        }
    }
}

fn finding(
    findings: &mut Vec<Finding>,
    code: &str,
    repository: &str,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    findings.push(Finding::new(
        code,
        (!repository.is_empty()).then_some(repository),
        field,
        message,
    ));
}

fn validate_repository_name(repository: &str, field: &str, findings: &mut Vec<Finding>) {
    let parts = repository.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|part| part.trim().is_empty()) {
        finding(
            findings,
            "repository-name",
            repository,
            field,
            "repository must be owner/name",
        );
    }
}

fn validate_sha(repository: &str, field: &str, value: &str, findings: &mut Vec<Finding>) {
    if !valid_sha(value) {
        finding(
            findings,
            "invalid-sha",
            repository,
            field,
            "expected a 40-hex immutable Git SHA",
        );
    }
}

fn validate_digest(repository: &str, field: &str, value: &str, findings: &mut Vec<Finding>) {
    if !valid_digest(value) {
        finding(
            findings,
            "invalid-digest",
            repository,
            field,
            "expected sha256:<64 hex characters>",
        );
    }
}

fn valid_sha(value: &str) -> bool {
    value.len() == SHA_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_digest(value: &str) -> bool {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    value.len() == DIGEST_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_equal(left: &str, right: &str) -> bool {
    let left = left.strip_prefix("sha256:").unwrap_or(left);
    let right = right.strip_prefix("sha256:").unwrap_or(right);
    valid_digest(left) && valid_digest(right) && left.eq_ignore_ascii_case(right)
}

fn valid_target(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase().replace(['_', '.'], "-");
    matches!(
        normalized.as_str(),
        "linux-x64"
            | "linux-amd64"
            | "linux-arm64"
            | "linux-aarch64"
            | "macos-arm64"
            | "macos-aarch64"
            | "darwin-arm64"
            | "macos-x64"
            | "macos-amd64"
            | "darwin-x64"
    )
}

fn version_less(left: &str, right: &str) -> bool {
    let parse = |value: &str| {
        value
            .split('.')
            .map(|part| {
                part.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>()
    };
    let left = parse(left);
    let right = parse(right);
    left < right
}

fn valid_timestamp(value: &str) -> bool {
    value.ends_with('Z') && OffsetDateTime::parse(value, &Rfc3339).is_ok()
}

fn nonempty_url(value: &str) -> bool {
    value.starts_with("https://") && value.len() > "https://".len()
}

fn sort_findings(findings: &mut [Finding]) {
    findings.sort_by(|left, right| {
        (
            left.repository.as_deref().unwrap_or(""),
            left.code.as_str(),
            left.field.as_str(),
            left.message.as_str(),
        )
            .cmp(&(
                right.repository.as_deref().unwrap_or(""),
                right.code.as_str(),
                right.field.as_str(),
                right.message.as_str(),
            ))
    });
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "fixture assertions intentionally use direct construction"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sha(seed: char) -> String {
        std::iter::repeat_n(seed, SHA_LENGTH).collect()
    }

    fn digest(seed: char) -> String {
        format!(
            "sha256:{}",
            std::iter::repeat_n(seed, DIGEST_LENGTH).collect::<String>()
        )
    }

    fn minimal_execution() -> ExecutionObservation {
        ExecutionObservation {
            run_id: 1,
            run_attempt: 1,
            run_url: "https://github.com/o/r/actions/runs/1".to_owned(),
            workflow_path: ".github/workflows/ci.yml".to_owned(),
            workflow_revision: sha('a'),
            event: "push".to_owned(),
            trigger_source_sha: sha('b'),
            actual_checkout_sha: sha('b'),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            provider: "github".to_owned(),
            runner_name: "GitHub Actions 1".to_owned(),
            host_id: "runner-1".to_owned(),
            runner_kind: "github-hosted".to_owned(),
            runner_labels: vec!["ubuntu-24.04".to_owned()],
            ..Default::default()
        }
    }

    #[test]
    fn strict_schema_rejects_alias_and_unknown_fields() {
        let value = json!({
            "context": "ci",
            "name": "ci",
            "app_id": "123"
        });
        assert!(serde_json::from_value::<RequiredContext>(value).is_err());
    }

    #[test]
    fn fixed_scope_is_not_count_only() {
        let manifest = ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: "fixture".to_owned(),
            source: SourceIdentity {
                repository: "owner/source".to_owned(),
                revision: sha('a'),
                digest: digest('a'),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: Vec::new(),
        };
        let mut findings = Vec::new();
        check_manifest(&manifest, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "manifest-scope"));
    }

    #[test]
    fn stale_sha_fixture_is_rejected() {
        let mut execution = minimal_execution();
        execution.trigger_source_sha = sha('f');
        let snapshot = SnapshotRepository {
            repository: "owner/repo".to_owned(),
            repository_id: 1,
            default_branch: "main".to_owned(),
            default_branch_sha: sha('b'),
            ruleset: RulesetObservation {
                required_checks: Vec::new(),
                source_url: "https://github.com/owner/repo/settings/rules".to_owned(),
                pages_complete: true,
            },
            workflows: Vec::new(),
            main_executions: vec![execution.clone()],
            open_prs: Vec::new(),
        };
        let record = EvidenceRecord {
            repository: snapshot.repository.clone(),
            pr_number: None,
            run_id: execution.run_id,
            event: "push".to_owned(),
            trigger_source_sha: execution.trigger_source_sha.clone(),
            actual_checkout_sha: snapshot.default_branch_sha.clone(),
            ..Default::default()
        };
        let mut findings = Vec::new();
        check_source_semantics(&snapshot, &record, &execution, &mut findings);
        assert!(findings.iter().any(|finding| finding.code == "stale-sha"));
    }

    #[test]
    fn skipped_job_fixture_is_rejected() {
        let mut execution = minimal_execution();
        execution.jobs.push(JobObservation {
            job_id: "ci".to_owned(),
            provider: "github".to_owned(),
            status: "completed".to_owned(),
            conclusion: "skipped".to_owned(),
            ..Default::default()
        });
        let mut findings = Vec::new();
        check_execution_observation("owner/repo", &execution, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "job-conclusion"));
    }

    #[test]
    fn missing_repository_fixture_is_rejected() {
        let manifest_repo = ManifestRepository {
            repository: "owner/repo".to_owned(),
            ..Default::default()
        };
        let manifest = BTreeMap::from([("owner/repo".to_owned(), &manifest_repo)]);
        let snapshot = BTreeMap::new();
        let mut findings = Vec::new();
        check_scope_coverage(&manifest, &snapshot, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "missing-snapshot-repository"));
    }

    #[test]
    fn wrong_provider_fixture_is_rejected() {
        let mut execution = minimal_execution();
        execution.provider = "unknown".to_owned();
        let mut findings = Vec::new();
        check_execution_observation("owner/repo", &execution, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "wrong-provider"));
    }

    #[test]
    fn failed_child_fixture_is_rejected() {
        let mut execution = minimal_execution();
        execution.child_runs.push(ChildRunObservation {
            parent_run_id: 1,
            run_id: 9,
            run_attempt: 1,
            repository: "owner/repo".to_owned(),
            workflow_path: "child.yml".to_owned(),
            event: "workflow_run".to_owned(),
            source_sha: sha('b'),
            provider: "github".to_owned(),
            status: "completed".to_owned(),
            conclusion: "failure".to_owned(),
            source_url: "https://github.com/o/r/actions/runs/9".to_owned(),
        });
        let mut findings = Vec::new();
        check_execution_observation("owner/repo", &execution, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "child-run-conclusion"));
    }

    #[test]
    fn mismatched_artifact_fixture_is_rejected() {
        let manifest = CanonicalReleaseManifest {
            schema: "velnor.application-manifest.v1".to_owned(),
            product_id: "velnor".to_owned(),
            channel: "stable".to_owned(),
            version: "1.0.0".to_owned(),
            source_repository: "tailrocks/velnor".to_owned(),
            source_ref: "refs/tags/v1.0.0".to_owned(),
            source_commit: sha('a'),
            release_tag: "v1.0.0".to_owned(),
            release_id: "r1".to_owned(),
            producer: ReleaseProducer {
                repository: "tailrocks/velnor".to_owned(),
                workflow_path: "release.yml".to_owned(),
                run_id: 1,
                run_url: "https://github.com/tailrocks/velnor/actions/runs/1".to_owned(),
                source_commit: sha('a'),
            },
            artifacts: vec![CanonicalArtifact {
                name: "app".to_owned(),
                target: "linux-amd64".to_owned(),
                kind: "archive".to_owned(),
                sha256: digest('a'),
                size: 1,
            }],
            components: Vec::new(),
            targets: Vec::new(),
        };
        let execution = ReleaseExecution {
            channel: "stable".to_owned(),
            version: "1.0.0".to_owned(),
            tag_target_sha: sha('a'),
            release_id: "r1".to_owned(),
            asset_digests: BTreeMap::from([("app".to_owned(), digest('f'))]),
            producer: ReleaseEvidenceProducer {
                repository: "tailrocks/velnor".to_owned(),
                workflow_path: "release.yml".to_owned(),
                run_id: 1,
                run_url: "https://github.com/tailrocks/velnor/actions/runs/1".to_owned(),
                source_commit: sha('a'),
                manifest_sha256: digest('a'),
            },
            apt: AptPublication {
                repository: "tailrocks/velnor-apt".to_owned(),
                revision: sha('a'),
                suite: "stable".to_owned(),
                candidate: "1.0.0".to_owned(),
                manifest_sha256: digest('a'),
            },
            homebrew: HomebrewPublication {
                tap: "tailrocks/homebrew-velnor".to_owned(),
                revision: sha('a'),
                formula: "velnor".to_owned(),
                version: "1.0.0".to_owned(),
                manifest_sha256: digest('a'),
            },
        };
        let mut findings = Vec::new();
        check_asset_inventory("tailrocks/velnor", &execution, &manifest, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "mismatched-artifact"));
    }

    #[test]
    fn offline_g7_is_never_a_completion_claim() {
        let report = check_documents(
            Stage::G7,
            &ManifestDocument {
                schema_version: 2,
                manifest_id: "m".to_owned(),
                source: SourceIdentity {
                    repository: "owner/source".to_owned(),
                    revision: sha('a'),
                    digest: digest('a'),
                    reviewed_by: "reviewer".to_owned(),
                },
                repositories: Vec::new(),
            },
            &SnapshotDocument {
                schema_version: 2,
                snapshot_id: "s".to_owned(),
                manifest_id: "m".to_owned(),
                observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                source: SnapshotSource {
                    collector: "fixture".to_owned(),
                    collector_revision: "fixture".to_owned(),
                    api_base: "https://api.github.com".to_owned(),
                    captured_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                    read_only: true,
                    page_count: 1,
                    permission_scopes: vec!["metadata:read".to_owned()],
                },
                repositories: Vec::new(),
            },
            &EvidenceDocument {
                schema_version: 2,
                manifest_id: "m".to_owned(),
                snapshot_id: "s".to_owned(),
                stage: "G7".to_owned(),
                records: Vec::new(),
                reviewer_attestation: None,
                g0_inventory: None,
            },
            None,
            "offline",
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "live-required"));
    }
}
