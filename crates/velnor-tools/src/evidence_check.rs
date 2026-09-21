//! Deterministic verification of the GitHub-first dual-lane evidence contract.
//!
//! The checker has three deliberately separate inputs: a reviewed workload
//! manifest, an independently collected GitHub snapshot, and result records.
//! Result records never define the scope, expected jobs, current revision, or
//! required checks.  Offline input files are validation fixtures only.  The
//! `--live` gate remains closed until a producer-owned authenticated collector
//! (or independently verified attestation) is wired to the current API and
//! closing-head reconciliation.  A caller-supplied JSON/CAS capture cannot
//! acquire authority by passing this checker.

use crate::g0_contract::*;
use crate::g0_workflow::{derive_workflow_plan, DerivedWorkflowPlan};
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clap::Args;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use url::Url;

const MANIFEST_SCHEMA_VERSION: u32 = 2;
const SNAPSHOT_SCHEMA_VERSION: u32 = 2;
const EVIDENCE_SCHEMA_VERSION: u32 = 2;
const CANONICAL_RELEASE_SCHEMA_VERSION: u32 = 1;
const REQUIRED_REPOSITORIES: usize = 32;
const SHA_LENGTH: usize = 40;
const DIGEST_LENGTH: usize = 64;
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
const G0_ALLOWED_SCOPES: [&str; 8] = [
    "actions:read",
    "administration:read",
    "checks:read",
    "contents:read",
    "metadata:read",
    "pull_requests:read",
    "statuses:read",
    "workflows:read",
];

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
    /// Snapshot input for offline validation. A future trusted live collector
    /// will supply and reconcile its own current snapshot.
    #[arg(long)]
    pub snapshot: PathBuf,
    /// Evidence records for the snapshot.
    #[arg(long)]
    pub evidence: PathBuf,
    /// Independent producer-owned canonical application manifest for G2+.
    #[arg(long)]
    pub release_manifest: Option<PathBuf>,
    /// Request the trusted current live collector. Until that authenticated
    /// collector is wired, this mode fails closed; caller files never become
    /// live authority merely by passing this flag. G7 always enables it.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckMode {
    Offline,
    TrustedLive,
}

impl CheckMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::TrustedLive => "live",
        }
    }
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

/// Immutable coverage subject for one evidence record.  Inventory is the
/// only valid role for G0; execution stages must identify either the current
/// default branch, a pull-request candidate, or a merge-group candidate.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EvidenceRole {
    #[default]
    Inventory,
    DefaultBranch,
    PullRequest,
    MergeGroup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewerAttestation {
    pub reviewer: String,
    pub report_digest: String,
    pub manifest_id: String,
    pub snapshot_id: String,
    pub attested_at_utc: String,
    pub artifact: ReviewArtifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewArtifact {
    pub source_repository: String,
    pub source_revision: String,
    pub source_tree_digest: String,
    pub source_diff_digest: String,
    pub run_manifest_digest: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvidenceRecord {
    pub repository: String,
    pub repository_role: String,
    pub evidence_role: EvidenceRole,
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
/// preparation. It is explicitly validation-only and cannot pass any gate.
pub fn check_paths(input: &EvidenceCheckInput) -> Result<CheckReport> {
    let stage = Stage::parse(&input.stage)?;
    let manifest: ManifestDocument = read_json(&input.manifest, "manifest")?;
    let snapshot: SnapshotDocument = read_json(&input.snapshot, "snapshot")?;
    let evidence: EvidenceDocument = read_json(&input.evidence, "evidence")?;
    let release = read_release_manifest(stage, input.release_manifest.as_deref())?;
    let mut report = check_documents(
        stage,
        &manifest,
        &snapshot,
        &evidence,
        release.as_ref(),
        CheckMode::Offline,
    );
    finding(
        &mut report.findings,
        "offline-validation-only",
        "",
        "mode",
        "offline evidence files are validation-only and cannot authorize a gate",
    );
    sort_findings(&mut report.findings);
    report.status = "fail";
    Ok(report)
}

/// Live mode is intentionally closed until its authority boundary is wired.
///
/// A typed `EvidenceDocument.g0_inventory` plus a fresh timestamp and
/// self-consistent local CAS proves only caller-controlled internal
/// consistency.  It does not prove that GitHub produced the capture, that the
/// collector authenticated to the requested account, or that the closing API
/// state was reconciled.  Refusing the input here prevents `--live` from
/// upgrading caller-authored offline evidence into gate evidence.
pub async fn check_paths_live(input: &EvidenceCheckInput) -> Result<CheckReport> {
    let stage = Stage::parse(&input.stage)?;
    let capture = crate::live_authority::current_collector()
        .collect_closing(crate::live_authority::ClosingCaptureRequest::from_input(
            input,
        ))
        .await?;
    let (manifest, snapshot, mut evidence, release, raw_store) = capture.into_parts();
    if let Some(inventory) = evidence.g0_inventory.as_mut() {
        raw_store
            .verify_g0(inventory)
            .context("verify producer raw-store binding")?;
        for raw in &mut inventory.collector_snapshot.raw_objects {
            let bytes = raw_store
                .read_g0_raw(raw)
                .with_context(|| format!("reopen producer raw object {}", raw.raw_id))?;
            if bytes.len() as u64 != raw.byte_length || digest_bytes(&bytes) != raw.sha256 {
                bail!(
                    "producer raw object {} failed reopened-byte digest/length binding",
                    raw.raw_id
                );
            }
            raw.bytes_base64 = BASE64.encode(bytes);
        }
    }
    Ok(check_documents(
        stage,
        &manifest,
        &snapshot,
        &evidence,
        release.as_ref(),
        CheckMode::TrustedLive,
    ))
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
    reject_duplicate_json_keys(&bytes).map_err(|error| {
        anyhow::anyhow!("reject duplicate keys in strict {label} JSON: {error}")
    })?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse strict {label} JSON {}", path.display()))
}

fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<(), String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    DuplicateKeyGuard::deserialize(&mut deserializer).map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())
}

struct DuplicateKeyGuard;

impl<'de> Deserialize<'de> for DuplicateKeyGuard {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateKeyVisitor)
    }
}

struct DuplicateKeyVisitor;

impl<'de> Visitor<'de> for DuplicateKeyVisitor {
    type Value = DuplicateKeyGuard;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON value without duplicate object keys")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object key {key}"
                )));
            }
            map.next_value::<DuplicateKeyGuard>()?;
        }
        Ok(DuplicateKeyGuard)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<DuplicateKeyGuard>()?.is_some() {}
        Ok(DuplicateKeyGuard)
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }

    fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }

    fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }

    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }

    fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }

    fn visit_string<E>(self, _: String) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(DuplicateKeyGuard)
    }
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
    mode: CheckMode,
) -> CheckReport {
    let mut findings = Vec::new();
    check_headers(stage, manifest, snapshot, evidence, &mut findings);
    check_manifest(manifest, &mut findings);
    check_snapshot(manifest, snapshot, &mut findings);
    if stage == Stage::G0 {
        check_g0_inventory(
            manifest,
            snapshot,
            evidence.g0_inventory.as_ref(),
            &mut findings,
        );
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
                evidence.g0_inventory.as_ref(),
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
    if stage == Stage::G7 && mode != CheckMode::TrustedLive {
        finding(
            &mut findings,
            "live-required",
            "",
            "mode",
            "G7 cannot pass from an offline fixture or caller-supplied snapshot",
        );
    }
    if stage.needs_execution() && mode != CheckMode::TrustedLive {
        finding(
            &mut findings,
            "authoritative-collector-required",
            "",
            "mode/coverage",
            "execution stages remain blocked until the collector independently derives complete PR, main, run, check, job, and child coverage",
        );
    }
    sort_findings(&mut findings);
    CheckReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        stage: stage.as_str().to_owned(),
        mode: mode.as_str(),
        status: if findings.is_empty() { "pass" } else { "fail" },
        findings,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RecordKey {
    repository: String,
    provider: String,
    evidence_role: EvidenceRole,
    event: String,
    pr_number: Option<u64>,
    pr_head_sha: Option<String>,
    pr_base_sha: Option<String>,
    tested_merge_sha: Option<String>,
    merge_group_sha: Option<String>,
    trigger_source_sha: String,
    run_id: u64,
    run_attempt: u32,
}

impl RecordKey {
    fn from_record(record: &EvidenceRecord) -> Self {
        Self {
            repository: record.repository.clone(),
            provider: record.provider.clone(),
            evidence_role: record.evidence_role,
            event: record.event.clone(),
            pr_number: record.pr_number,
            pr_head_sha: record.pr_head_sha.clone(),
            pr_base_sha: record.pr_base_sha.clone(),
            tested_merge_sha: record.tested_merge_sha.clone(),
            merge_group_sha: record.merge_group_sha.clone(),
            trigger_source_sha: record.trigger_source_sha.clone(),
            run_id: record.run_id,
            run_attempt: record.run_attempt,
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
        check_review_attestation(manifest, snapshot, attestation, findings);
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

fn check_review_attestation(
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    attestation: &ReviewerAttestation,
    findings: &mut Vec<Finding>,
) {
    let artifact = &attestation.artifact;
    if attestation.reviewer.trim().is_empty()
        || !valid_digest(&attestation.report_digest)
        || attestation.manifest_id != manifest.manifest_id
        || attestation.snapshot_id != snapshot.snapshot_id
        || !valid_timestamp(&attestation.attested_at_utc)
        || artifact.source_repository != REVIEWED_SOURCE_REPOSITORY
        || !valid_sha(&artifact.source_revision)
        || !valid_digest(&artifact.source_tree_digest)
        || !valid_digest(&artifact.source_diff_digest)
        || !valid_digest(&artifact.run_manifest_digest)
        || !nonempty_url(&artifact.source_url)
    {
        finding(
            findings,
            "invalid-review-attestation",
            "",
            "evidence.reviewer_attestation",
            "reviewer attestation must bind an external source, tree, diff, run manifest, URL, and exact evidence identities",
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
    let workload_targets = repo
        .workload_platform_architecture
        .iter()
        .map(|platform| {
            (
                platform.workload_id.as_str(),
                (platform.platform.as_str(), platform.architecture.as_str()),
            )
        })
        .collect::<BTreeMap<_, _>>();
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
        match workload_targets.get(job.workload_id.as_str()) {
            None => finding(
                findings,
                "expected-job-workload",
                &repo.repository,
                "expected_jobs.workload_id",
                format!(
                    "expected job {} names workload {} absent from the reviewed workload map",
                    job.job_id, job.workload_id
                ),
            ),
            Some((platform, architecture))
                if (*platform, *architecture)
                    != (job.platform.as_str(), job.architecture.as_str()) =>
            {
                finding(
                    findings,
                    "expected-job-target",
                    &repo.repository,
                    "expected_jobs.platform/architecture",
                    format!(
                        "expected job {} target {}-{} differs from workload {} target {}-{}",
                        job.job_id,
                        job.platform,
                        job.architecture,
                        job.workload_id,
                        platform,
                        architecture
                    ),
                )
            }
            Some(_) => {}
        }
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
    manifest: &ManifestDocument,
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
    let collector = &inventory.collector_snapshot;
    if collector.schema_version != EVIDENCE_SCHEMA_VERSION
        || collector.snapshot_id.trim().is_empty()
        || collector.snapshot_id != snapshot.snapshot_id
        || collector.manifest_id != snapshot.manifest_id
        || collector.phase != "G0"
        || !valid_timestamp(&collector.observed_at_utc)
        || !valid_timestamp(&collector.completed_at_utc)
    {
        finding(
            findings,
            "g0-collector-identity",
            "",
            "evidence.g0_inventory.collector_snapshot",
            "typed collector snapshot must bind schema, manifest, snapshot, phase, and UTC timestamps",
        );
    }
    let expected_value = serde_json::to_value(collector).unwrap_or(Value::Null);
    let expected_bytes = canonical_json(&expected_value).into_bytes();
    let decoded_snapshot = BASE64.decode(&inventory.collector_snapshot_bytes_base64);
    let expected_digest = decoded_snapshot
        .as_ref()
        .map(|bytes| digest_bytes(bytes))
        .unwrap_or_default();
    let snapshot_bytes_valid = decoded_snapshot.as_ref().is_ok_and(|bytes| {
        reject_duplicate_json_keys(bytes).is_ok()
            && bytes == &expected_bytes
            && serde_json::from_slice::<G0CollectorSnapshot>(bytes)
                .ok()
                .and_then(|parsed| serde_json::to_value(parsed).ok())
                .is_some_and(|parsed| canonical_json(&parsed) == canonical_json(&expected_value))
    });
    if !snapshot_bytes_valid {
        finding(
            findings,
            "g0-snapshot-bytes",
            "",
            "evidence.g0_inventory.collector_snapshot_bytes_base64",
            "collector snapshot bytes must be canonical JSON for the typed snapshot and parse back to the same object",
        );
    }
    if !digest_equal(&inventory.collector_snapshot_sha256, &expected_digest) {
        finding(
            findings,
            "g0-collector-digest",
            "",
            "evidence.g0_inventory.collector_snapshot_sha256",
            "collector snapshot digest must match canonical typed snapshot bytes",
        );
    }
    if !g0_storage_ref(
        &inventory.collector_snapshot_storage_ref,
        &inventory.collector_snapshot_sha256,
    ) {
        finding(
            findings,
            "g0-collector-storage",
            "",
            "evidence.g0_inventory.collector_snapshot_storage_ref",
            "collector snapshot bytes require an immutable content-addressed artifact reference",
        );
    }
    check_g0_collector_identity(collector, findings);
    check_g0_request_provenance(collector, findings);
    let raw_ids = collector
        .raw_objects
        .iter()
        .map(|raw| raw.raw_id.clone())
        .collect::<BTreeSet<_>>();
    check_g0_artifact_reference(
        "evidence.g0_inventory.collector_snapshot.workload_artifact",
        &collector.workload_artifact,
        &raw_ids,
        &collector.raw_objects,
        findings,
    );
    if !g0_typed_raw_binding(
        &collector.workload_artifact.raw_object_refs,
        &collector.raw_objects,
        "workload.artifact",
        &collector.workload_artifact,
    ) {
        finding(
            findings,
            "g0-workload-artifact",
            "",
            "collector_snapshot.workload_artifact.raw_object_refs",
            "workload artifact metadata must be parsed from a collector-owned workload.artifact raw object",
        );
    }
    check_g0_repositories(
        manifest,
        snapshot,
        collector,
        &raw_ids,
        &collector.requests,
        &collector.raw_objects,
        findings,
    );
    check_g0_reconciliation(manifest, snapshot, collector, &raw_ids, findings);
    check_g0_dependency_graph(manifest, collector, &raw_ids, findings);
    check_g0_model_session(collector, &raw_ids, &collector.raw_objects, findings);
    check_g0_access(
        collector,
        &raw_ids,
        &collector.requests,
        &collector.raw_objects,
        findings,
    );
}

fn check_g0_collector_identity(collector: &G0CollectorSnapshot, findings: &mut Vec<Finding>) {
    if collector.collector.name.trim().is_empty()
        || !valid_sha(&collector.collector.revision)
        || collector.collector.mode != "read_only"
        || !g0_api_base(&collector.collector.api_base)
        || collector.collector.api_versions.is_empty()
    {
        finding(
            findings,
            "g0-collector-source",
            "",
            "evidence.g0_inventory.collector_snapshot.collector",
            "collector must identify an immutable read-only GitHub API source",
        );
    }
    if collector.auth.provider != "github"
        || collector.auth.viewer_id.trim().is_empty()
        || collector.auth.viewer_login.trim().is_empty()
        || collector.auth.safe_scopes.is_empty()
        || collector
            .auth
            .safe_scopes
            .iter()
            .any(|scope| !G0_ALLOWED_SCOPES.contains(&scope.as_str()))
        || !collector.auth.secret_excluded
    {
        finding(
            findings,
            "g0-auth-source",
            "",
            "evidence.g0_inventory.collector_snapshot.auth",
            "collector must record safe GitHub viewer identity/scopes and exclude secrets",
        );
    }
    if collector.rate_limit.api.trim().is_empty()
        || collector.rate_limit.limit == 0
        || collector.rate_limit.remaining > collector.rate_limit.limit
        || collector.rate_limit.used > collector.rate_limit.limit
        || !valid_timestamp(&collector.rate_limit.reset_at_utc)
        || !valid_timestamp(&collector.rate_limit.observed_at_utc)
    {
        finding(
            findings,
            "g0-rate-limit",
            "",
            "evidence.g0_inventory.collector_snapshot.rate_limit",
            "collector must record a valid non-secret API rate-limit observation",
        );
    }
}

fn check_g0_request_provenance(collector: &G0CollectorSnapshot, findings: &mut Vec<Finding>) {
    let request_ids = collector
        .requests
        .iter()
        .map(|request| request.request_id.clone())
        .collect::<BTreeSet<_>>();
    let mut raw_ids = BTreeSet::new();
    if collector.requests.is_empty() || collector.raw_objects.is_empty() {
        finding(
            findings,
            "g0-provenance-missing",
            "",
            "evidence.g0_inventory.collector_snapshot.requests/raw_objects",
            "typed G0 proof requires request records and immutable raw-object references",
        );
    }
    if request_ids.len() != collector.requests.len() {
        finding(
            findings,
            "g0-request-duplicate",
            "",
            "evidence.g0_inventory.collector_snapshot.requests",
            "request identities must be unique",
        );
    }
    for request in &collector.requests {
        let query = BASE64.decode(&request.query_base64);
        let variables = BASE64.decode(&request.variables_base64);
        let response_contract = collector
            .raw_objects
            .iter()
            .find(|raw| raw.raw_id == request.response_raw_ref)
            .map(|raw| g0_endpoint_contract(&raw.object_kind, &request.endpoint_or_operation));
        if let Some(Err(reason)) = response_contract.as_ref() {
            finding(
                findings,
                "g0-endpoint-contract",
                "",
                "evidence.g0_inventory.collector_snapshot.raw_objects",
                format!("evidence request endpoint contract rejected: {reason}"),
            );
        }
        let query_contract = match (query.as_ref(), response_contract.as_ref()) {
            (Ok(bytes), Some(Ok(contract))) => {
                g0_response_query_contract(request, bytes, *contract)
            }
            _ => false,
        };
        if request.request_id.trim().is_empty()
            || request.method.trim().is_empty()
            || request.endpoint_or_operation.trim().is_empty()
            || query.is_err()
            || variables.is_err()
            || query
                .as_ref()
                .is_ok_and(|bytes| digest_bytes(bytes) != request.query_sha256)
            || variables
                .as_ref()
                .is_ok_and(|bytes| digest_bytes(bytes) != request.variables_sha256)
            || !valid_digest(&request.query_sha256)
            || !valid_digest(&request.variables_sha256)
            || request.auth_identity_ref != "collector.auth"
            || !valid_timestamp(&request.started_at_utc)
            || !valid_timestamp(&request.completed_at_utc)
            || request.http_status < 200
            || request.http_status >= 300
            || request.api_request_id.trim().is_empty()
            || request.rate_limit_ref != "collector.rate_limit"
            || request.page.number == 0
            || request.page.per_page == 0
            || request.page.per_page > 100
            || request.page.items_returned > request.page.per_page
            || request.response_raw_ref.trim().is_empty()
            || request.error_raw_ref.is_some()
            || !request.complete
            || !matches!(
                request.state,
                G0RequestState::Complete | G0RequestState::EmptyComplete
            )
            || (request.state == G0RequestState::EmptyComplete && request.page.items_returned != 0)
            || (request.page.has_next_page
                && request.page.link_next.is_none()
                && request.page.cursor_out.is_none())
            || (request.page.has_next_page
                && request.page.link_next.as_ref().is_some_and(|link| {
                    !g0_endpoint_path_query(&request.endpoint_or_operation)
                        .ok()
                        .is_some_and(|(path, _)| {
                            g0_api_url(link).is_some_and(|url| url.path() == path)
                        })
                }))
            || (!request.page.has_next_page
                && (request.page.link_next.is_some() || request.page.cursor_out.is_some()))
            || !g0_request_semantics(
                request,
                query.as_ref().ok().map(Vec::as_slice),
                variables.as_ref().ok().map(Vec::as_slice),
            )
            || !query_contract
        {
            finding(
                findings,
                "g0-request-incomplete",
                "",
                "evidence.g0_inventory.collector_snapshot.requests",
                "every request must be a complete successful page with query, viewer, rate-limit, and raw-response provenance",
            );
        }
        if !query_contract {
            finding(
                findings,
                "g0-request-query",
                "",
                "evidence.g0_inventory.collector_snapshot.requests.query_base64",
                "request query must exactly bind endpoint pagination and provider filter semantics",
            );
        }
    }
    let mut pages = BTreeMap::<(G0ApiKind, String, String, String), Vec<&G0RequestRecord>>::new();
    for request in &collector.requests {
        let query = BASE64.decode(&request.query_base64).ok();
        let variables = BASE64.decode(&request.variables_base64).ok();
        let Some(stream_key) =
            g0_request_stream_key(request, query.as_deref(), variables.as_deref())
        else {
            finding(
                findings,
                "g0-pagination",
                "",
                "evidence.g0_inventory.collector_snapshot.requests",
                "request pagination identity must be canonical and parseable",
            );
            continue;
        };
        let page_set = pages.entry(stream_key).or_default();
        page_set.push(request);
    }
    for page_set in pages.values_mut() {
        page_set.sort_by_key(|request| request.page.number);
        if page_set
            .first()
            .is_none_or(|request| request.page.number != 1)
        {
            finding(
                findings,
                "g0-pagination",
                "",
                "evidence.g0_inventory.collector_snapshot.requests.page.number",
                "a paginated request stream must capture its first page",
            );
        }
        for window in page_set.windows(2) {
            let current = window[0];
            let next = window[1];
            if current.page.number == next.page.number {
                finding(
                    findings,
                    "g0-pagination",
                    "",
                    "evidence.g0_inventory.collector_snapshot.requests.page",
                    "a paginated request stream cannot repeat a page number",
                );
            }
            if current.page.number.saturating_add(1) != next.page.number
                || !current.page.has_next_page
                || !g0_next_link_matches(current, next)
                || (current.page.cursor_out.is_some()
                    && current.page.cursor_out != next.page.cursor_in)
            {
                finding(
                    findings,
                    "g0-pagination",
                    "",
                    "evidence.g0_inventory.collector_snapshot.requests.page",
                    "captured pages must be contiguous and linked by the authoritative next URL/cursor",
                );
            }
        }
        if let Some(last) = page_set.last()
            && last.page.has_next_page
        {
            finding(
                findings,
                "g0-pagination",
                "",
                "evidence.g0_inventory.collector_snapshot.requests.page",
                "a page declaring another page must have a captured subsequent page",
            );
        }
    }
    check_g0_paginated_response_streams(collector, &pages, findings);
    for raw in &collector.raw_objects {
        let decoded = BASE64.decode(&raw.bytes_base64);
        if !raw_ids.insert(raw.raw_id.clone())
            || raw.request_id.trim().is_empty()
            || (!g0_local_raw_kind(&raw.object_kind) && !request_ids.contains(&raw.request_id))
            || raw.object_kind.trim().is_empty()
            || raw.canonicalization.trim().is_empty()
            || !valid_digest(&raw.sha256)
            || raw.byte_length == 0
            || decoded.is_err()
            || decoded
                .as_ref()
                .is_ok_and(|bytes| bytes.len() as u64 != raw.byte_length)
            || decoded
                .as_ref()
                .is_ok_and(|bytes| digest_bytes(bytes) != raw.sha256)
            || !g0_storage_ref(&raw.storage_ref, &raw.sha256)
            || !valid_digest(&raw.original_sha256)
            || raw.original_byte_length == 0
            || !g0_storage_ref(&raw.original_storage_ref, &raw.original_sha256)
            || raw.media_type.trim().is_empty()
            || raw.storage_ref.trim().is_empty()
            || raw.original_storage_ref.trim().is_empty()
        {
            finding(
                findings,
                "g0-raw-object",
                "",
                "evidence.g0_inventory.collector_snapshot.raw_objects",
                "raw object references must be unique, request-bound, hashed, and non-empty",
            );
        }
    }
    for raw in &collector.raw_objects {
        let provider_response_bound = collector.requests.iter().any(|request| {
            request.request_id == raw.request_id && request.response_raw_ref == raw.raw_id
        });
        if g0_local_raw_kind(&raw.object_kind) {
            if request_ids.contains(&raw.request_id) || !raw.request_id.starts_with("local-") {
                finding(
                    findings,
                    "g0-local-raw-request",
                    "",
                    "evidence.g0_inventory.collector_snapshot.raw_objects.request_id",
                    "local typed objects require an explicit local provenance namespace and cannot reuse a provider request identity",
                );
            }
        } else if !provider_response_bound {
            finding(
                findings,
                "g0-raw-request-binding",
                "",
                "evidence.g0_inventory.collector_snapshot.raw_objects",
                "every provider raw object must be the exact response object of its recorded request",
            );
        }
    }
    for request in &collector.requests {
        for raw_id in std::iter::once(&request.response_raw_ref).chain(request.error_raw_ref.iter())
        {
            if let Some(raw) = collector
                .raw_objects
                .iter()
                .find(|raw| &raw.raw_id == raw_id)
                && raw.request_id != request.request_id
            {
                finding(
                    findings,
                    "g0-raw-request-binding",
                    "",
                    "evidence.g0_inventory.collector_snapshot.requests.raw_refs",
                    "raw response/error object must belong to the request that produced it",
                );
            }
        }
    }
    for request in &collector.requests {
        check_g0_raw_ref(
            "evidence.g0_inventory.collector_snapshot.requests.response_raw_ref",
            &request.response_raw_ref,
            &raw_ids,
            findings,
        );
        if let Some(raw_id) = &request.error_raw_ref {
            check_g0_raw_ref(
                "evidence.g0_inventory.collector_snapshot.requests.error_raw_ref",
                raw_id,
                &raw_ids,
                findings,
            );
        }
    }
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.repository.raw_object_refs",
        &collector
            .repositories
            .iter()
            .flat_map(|repo| repo.raw_object_refs.iter().cloned())
            .collect::<Vec<_>>(),
        &raw_ids,
        findings,
    );
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.dependency_graph.raw_object_refs",
        &collector.dependency_graph.raw_object_refs,
        &raw_ids,
        findings,
    );
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.model_session.raw_object_refs",
        &collector.model_session.raw_object_refs,
        &raw_ids,
        findings,
    );
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.workload_artifact.raw_object_refs",
        &collector.workload_artifact.raw_object_refs,
        &raw_ids,
        findings,
    );
}

fn g0_local_raw_kind(object_kind: &str) -> bool {
    matches!(
        object_kind,
        "model.session" | "workload.source" | "workload.artifact"
    )
}

fn g0_page_members(value: &Value, response_schema: G0RawResponseSchema) -> Option<(u64, &[Value])> {
    let envelope_key = match response_schema {
        G0RawResponseSchema::PullRequestsPage => "pulls",
        G0RawResponseSchema::RulesetsPage => "rulesets",
        G0RawResponseSchema::WorkflowsPage => "workflows",
        G0RawResponseSchema::WorkflowRunsPage => "workflow_runs",
        G0RawResponseSchema::CheckRunsPage => "check_runs",
        G0RawResponseSchema::CheckSuitesPage => "check_suites",
        G0RawResponseSchema::CheckSuiteRunsPage => "check_runs",
        G0RawResponseSchema::JobsPage => "jobs",
        G0RawResponseSchema::ArtifactsPage => "artifacts",
        G0RawResponseSchema::Singular | G0RawResponseSchema::Binary => return None,
    };
    let total_count = value.get("total_count")?.as_u64()?;
    let members = value.get(envelope_key)?.as_array()?;
    Some((total_count, members.as_slice()))
}

fn check_g0_paginated_response_streams(
    collector: &G0CollectorSnapshot,
    pages: &BTreeMap<(G0ApiKind, String, String, String), Vec<&G0RequestRecord>>,
    findings: &mut Vec<Finding>,
) {
    for page_set in pages.values() {
        let mut expected_total = None;
        let mut observed_items = 0u64;
        let mut member_ids = BTreeSet::new();
        let mut list_stream = false;
        for request in page_set {
            let Some(raw) = collector.raw_objects.iter().find(|raw| {
                raw.raw_id == request.response_raw_ref && raw.request_id == request.request_id
            }) else {
                finding(
                    findings,
                    "g0-raw-reference",
                    "",
                    "evidence.g0_inventory.collector_snapshot.requests.response_raw_ref",
                    "every paginated request must resolve its captured raw response before stream validation",
                );
                continue;
            };
            let contract =
                match g0_endpoint_contract(&raw.object_kind, &request.endpoint_or_operation) {
                    Ok(contract) => contract,
                    Err(reason) => {
                        finding(
                            findings,
                            "g0-endpoint-contract",
                            "",
                            "evidence.g0_inventory.collector_snapshot.raw_objects",
                            format!("paginated evidence endpoint contract rejected: {reason}"),
                        );
                        continue;
                    }
                };
            let response_schema = contract.response_schema;
            let is_list = matches!(
                response_schema,
                G0RawResponseSchema::PullRequestsPage
                    | G0RawResponseSchema::RulesetsPage
                    | G0RawResponseSchema::WorkflowsPage
                    | G0RawResponseSchema::WorkflowRunsPage
                    | G0RawResponseSchema::CheckRunsPage
                    | G0RawResponseSchema::CheckSuitesPage
                    | G0RawResponseSchema::CheckSuiteRunsPage
                    | G0RawResponseSchema::JobsPage
                    | G0RawResponseSchema::ArtifactsPage
            );
            if !is_list {
                continue;
            }
            list_stream = true;
            let body_valid = BASE64
                .decode(&raw.bytes_base64)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|value| {
                    let (total, members) = g0_page_members(&value, response_schema)?;
                    Some((total, members.to_vec()))
                });
            let Some((total, members)) = body_valid else {
                finding(
                    findings,
                    "g0-response-envelope",
                    "",
                    "evidence.g0_inventory.collector_snapshot.raw_objects",
                    "paginated REST responses must use their exact typed envelope",
                );
                continue;
            };
            if expected_total.is_some_and(|expected| expected != total) {
                finding(
                    findings,
                    "g0-pagination-total",
                    "",
                    "evidence.g0_inventory.collector_snapshot.requests.page",
                    "every page in one response stream must report the same total_count",
                );
            } else {
                expected_total = Some(total);
            }
            if members.len() as u32 != request.page.items_returned {
                finding(
                    findings,
                    "g0-pagination-count",
                    "",
                    "evidence.g0_inventory.collector_snapshot.requests.page.items_returned",
                    "page.items_returned must equal the exact envelope member count",
                );
            }
            observed_items = observed_items.saturating_add(members.len() as u64);
            for member in members {
                let Some(id) = g0_json_u64(&member, &["id"]) else {
                    finding(
                        findings,
                        "g0-pagination-member",
                        "",
                        "evidence.g0_inventory.collector_snapshot.raw_objects",
                        "every paginated member must expose a numeric immutable id",
                    );
                    continue;
                };
                if !member_ids.insert(id) {
                    finding(
                        findings,
                        "g0-pagination-duplicate",
                        "",
                        "evidence.g0_inventory.collector_snapshot.raw_objects",
                        "a paginated response stream cannot repeat a member id",
                    );
                }
            }
        }
        if list_stream
            && let Some(total) = expected_total
            && page_set
                .last()
                .is_some_and(|request| !request.page.has_next_page)
            && observed_items != total
        {
            finding(
                findings,
                "g0-pagination-total",
                "",
                "evidence.g0_inventory.collector_snapshot.requests.page",
                "the final captured page must account for total_count items",
            );
        }
    }
}

fn check_g0_raw_ref(
    field: &str,
    raw_id: &str,
    raw_ids: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    if raw_id.trim().is_empty() || !raw_ids.contains(raw_id) {
        finding(
            findings,
            "g0-raw-reference",
            "",
            field,
            format!("raw object reference {raw_id} is absent from collector raw_objects"),
        );
    }
}

fn check_g0_raw_refs(
    field: &str,
    refs: &[String],
    raw_ids: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    if refs.is_empty() {
        finding(
            findings,
            "g0-raw-reference",
            "",
            field,
            "typed authoritative fields require at least one raw object reference",
        );
    }
    for raw_id in refs {
        check_g0_raw_ref(field, raw_id, raw_ids, findings);
    }
}

fn check_g0_artifact_reference(
    field: &str,
    artifact: &G0ArtifactReference,
    raw_ids: &BTreeSet<String>,
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    if artifact.name.trim().is_empty()
        || artifact.schema.trim().is_empty()
        || !nonempty_url(&artifact.source_url)
        || !valid_digest(&artifact.sha256)
        || !g0_storage_ref(&artifact.storage_ref, &artifact.sha256)
        || !valid_sha(&artifact.source_revision)
        || !valid_digest(&artifact.source_digest)
        || !valid_timestamp(&artifact.observed_at_utc)
    {
        finding(
            findings,
            "g0-artifact-reference",
            "",
            field,
            "immutable artifact references require name, schema, HTTPS source, digest, and observation time",
        );
    }
    check_g0_raw_refs(
        &format!("{field}.raw_object_refs"),
        &artifact.raw_object_refs,
        raw_ids,
        findings,
    );
    if !artifact.raw_object_refs.iter().any(|raw_id| {
        raw_objects
            .iter()
            .any(|raw| raw.raw_id == *raw_id && raw.sha256 == artifact.sha256)
    }) {
        finding(
            findings,
            "g0-artifact-digest",
            "",
            field,
            "artifact digest must match bytes from one of its referenced raw objects",
        );
    }
}

/// Local collector objects are still evidence, not typed self-attestation.
/// The collector mapper accepts these only after parsing the exact raw bytes;
/// keep the checker boundary equally strict so a caller cannot replace a
/// malformed/raw object with a populated `model_session` or artifact field.
fn g0_raw_object_matches_typed<T: Serialize>(
    raw: &G0RawObjectRef,
    expected_kind: &str,
    expected: &T,
) -> bool {
    if raw.object_kind != expected_kind {
        return false;
    }
    let Ok(bytes) = BASE64.decode(&raw.bytes_base64) else {
        return false;
    };
    if reject_duplicate_json_keys(&bytes).is_err() {
        return false;
    }
    let Ok(actual) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    serde_json::to_value(expected).is_ok_and(|expected| actual == expected)
}

fn g0_typed_raw_binding<T: Serialize>(
    refs: &[String],
    raw_objects: &[G0RawObjectRef],
    expected_kind: &str,
    expected: &T,
) -> bool {
    refs.iter().any(|raw_id| {
        raw_objects.iter().any(|raw| {
            raw.raw_id == *raw_id && g0_raw_object_matches_typed(raw, expected_kind, expected)
        })
    })
}

fn g0_valid_applicability(value: &str) -> bool {
    matches!(
        value,
        "required" | "applicable" | "not-applicable" | "excluded"
    )
}

fn g0_valid_event(value: &str) -> bool {
    matches!(
        value,
        "push" | "pull_request" | "merge_group" | "workflow_dispatch" | "workflow_run"
    )
}

fn g0_valid_app_id(value: &str) -> bool {
    value.parse::<u64>().is_ok_and(|id| id > 0)
}

fn g0_response_query_contract(
    request: &G0RequestRecord,
    query: &[u8],
    contract: G0EndpointContract,
) -> bool {
    let Some(payload_pairs) = g0_request_query_pairs_ordered(query) else {
        return false;
    };
    let Some(ordered) = g0_effective_request_query(request, &payload_pairs) else {
        return false;
    };
    let response_schema = contract.response_schema;
    let endpoint_query_valid = g0_endpoint_inline_query_contract(request, contract);
    if matches!(
        response_schema,
        G0RawResponseSchema::Singular | G0RawResponseSchema::Binary
    ) {
        let singular_page = payload_pairs.is_empty()
            || (payload_pairs.len() == 1
                && payload_pairs[0].0 == "per_page"
                && payload_pairs[0].1 == request.page.per_page.to_string());
        return endpoint_query_valid
            && singular_page
            && request.page.number == 1
            && !request.page.has_next_page;
    }

    let keys = ordered
        .iter()
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>();
    let expected_keys = g0_paginated_query_keys(contract.kind);
    if keys != expected_keys {
        return false;
    }
    let value = |key: &str| {
        ordered
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    };
    let Some(per_page) = value("per_page").and_then(|value| value.parse::<u32>().ok()) else {
        return false;
    };
    let Some(page) = value("page").and_then(|value| value.parse::<u32>().ok()) else {
        return false;
    };
    endpoint_query_valid
        && per_page == request.page.per_page
        && per_page > 0
        && per_page <= 100
        && page == request.page.number
        && (contract.coverage != G0CoveragePurpose::CheckInventory
            || value("filter") == Some("all"))
}

fn g0_endpoint_inline_query_contract(
    request: &G0RequestRecord,
    contract: G0EndpointContract,
) -> bool {
    g0_endpoint_path_query(&request.endpoint_or_operation)
        .ok()
        .is_some_and(|(_, query)| g0_endpoint_inline_query_shape_from_query(query, contract.kind))
}

fn g0_effective_request_query(
    request: &G0RequestRecord,
    payload_pairs: &[(String, String)],
) -> Option<Vec<(String, String)>> {
    let (_, endpoint_pairs) = g0_endpoint_path_query(&request.endpoint_or_operation).ok()?;
    let mut seen = BTreeSet::new();
    let mut effective = Vec::with_capacity(endpoint_pairs.len() + payload_pairs.len());
    for (key, value) in endpoint_pairs
        .into_iter()
        .chain(payload_pairs.iter().cloned())
    {
        if !seen.insert(key.clone()) {
            return None;
        }
        effective.push((key, value));
    }
    effective.sort();
    Some(effective)
}

fn g0_endpoint_inline_query_shape(endpoint: &str, kind: G0EndpointKind) -> bool {
    g0_endpoint_path_query(endpoint)
        .ok()
        .is_some_and(|(_, query)| g0_endpoint_inline_query_shape_from_query(query, kind))
}

fn g0_endpoint_inline_query_shape_from_query(
    query: Vec<(String, String)>,
    kind: G0EndpointKind,
) -> bool {
    match kind {
        G0EndpointKind::WorkflowSource | G0EndpointKind::WorkflowDependencySource => {
            query.len() == 1 && query[0].0 == "ref" && valid_sha(&query[0].1)
        }
        G0EndpointKind::WorkflowDependency => {
            query.as_slice() == [("recursive".to_owned(), "1".to_owned())]
        }
        kind if !g0_paginated_query_keys(kind).is_empty() => {
            if query.is_empty() {
                return true;
            }
            let expected_keys = g0_paginated_query_keys(kind);
            let mut ordered = query;
            ordered.sort();
            let keys = ordered
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>();
            if keys != expected_keys {
                return false;
            }
            ordered.iter().all(|(key, value)| match key.as_str() {
                "page" => value.parse::<u32>().is_ok_and(|page| page > 0),
                "per_page" => value
                    .parse::<u32>()
                    .is_ok_and(|per_page| (1..=100).contains(&per_page)),
                "filter" => value == "all",
                _ => !value.is_empty(),
            })
        }
        _ => query.is_empty(),
    }
}

fn g0_paginated_query_keys(kind: G0EndpointKind) -> &'static [&'static str] {
    match kind {
        G0EndpointKind::PullRequestsPage => &["page", "per_page", "state"],
        G0EndpointKind::RulesetsPage => &["includes_parents", "page", "per_page"],
        G0EndpointKind::WorkflowsPage | G0EndpointKind::WorkflowRunsPage => &["page", "per_page"],
        G0EndpointKind::CheckRunsPage => &["filter", "page", "per_page"],
        G0EndpointKind::CheckSuitesPage => &["page", "per_page"],
        G0EndpointKind::CheckSuiteRunsPage => &["filter", "page", "per_page"],
        G0EndpointKind::WorkflowJobsPage => &["filter", "page", "per_page"],
        G0EndpointKind::WorkflowAttemptJobsPage | G0EndpointKind::ArtifactsPage => {
            &["page", "per_page"]
        }
        G0EndpointKind::Repository
        | G0EndpointKind::Viewer
        | G0EndpointKind::DefaultBranchCommit
        | G0EndpointKind::PullRequest
        | G0EndpointKind::Ruleset
        | G0EndpointKind::WorkflowSource
        | G0EndpointKind::WorkflowDependency
        | G0EndpointKind::WorkflowDependencySource
        | G0EndpointKind::WorkflowAttempt
        | G0EndpointKind::CheckRun
        | G0EndpointKind::CheckSuite
        | G0EndpointKind::WorkflowRun
        | G0EndpointKind::ArtifactArchive
        | G0EndpointKind::App => &[],
    }
}

fn g0_request_semantics(
    request: &G0RequestRecord,
    query: Option<&[u8]>,
    variables: Option<&[u8]>,
) -> bool {
    let endpoint = request.endpoint_or_operation.as_str();
    let Some(query) = query else {
        return false;
    };
    let Some(variables) = variables else {
        return false;
    };
    let Ok((endpoint_path, endpoint_query)) = g0_endpoint_path_query(endpoint) else {
        return false;
    };
    if endpoint_path.contains("..") {
        return false;
    }
    match request.api {
        G0ApiKind::Rest => {
            if request.method != "GET"
                || !(endpoint_path == "/user"
                    || endpoint_path == "/rate_limit"
                    || endpoint_path.starts_with("/repos/")
                    || endpoint_path.starts_with("/apps/")
                    || endpoint_path.starts_with("/orgs/"))
            {
                return false;
            }
            if let Some(repository_path) = endpoint_path.strip_prefix("/repos/") {
                let parts = repository_path.split('/').collect::<Vec<_>>();
                if parts.len() < 2
                    || parts[0].trim().is_empty()
                    || parts[1].trim().is_empty()
                    || !canonical_scope().contains(&format!("{}/{}", parts[0], parts[1]).as_str())
                {
                    return false;
                }
            } else if let Some(organization_path) = endpoint_path.strip_prefix("/orgs/") {
                let parts = organization_path.split('/').collect::<Vec<_>>();
                let known_owner = canonical_scope().iter().any(|repository| {
                    repository.split('/').next() == Some(parts.first().copied().unwrap_or_default())
                });
                if parts.len() < 2 || parts[0].trim().is_empty() || !known_owner {
                    return false;
                }
            }
            let Some(pairs) = g0_rest_query_pairs(query) else {
                return false;
            };
            if endpoint_query.iter().any(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "after"
                        | "branch"
                        | "event"
                        | "filter"
                        | "first"
                        | "head_sha"
                        | "includes_parents"
                        | "page"
                        | "per_page"
                        | "recursive"
                        | "ref"
                        | "state"
                        | "status"
                )
            }) || pairs.iter().any(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "after"
                        | "branch"
                        | "event"
                        | "filter"
                        | "first"
                        | "head_sha"
                        | "includes_parents"
                        | "page"
                        | "per_page"
                        | "recursive"
                        | "ref"
                        | "state"
                        | "status"
                )
            }) {
                return false;
            }
            variables == b"{}" || variables.is_empty()
        }
        G0ApiKind::Graphql => {
            if request.method != "POST"
                || !matches!(endpoint, "/graphql" | "graphql")
                || variables.is_empty()
                || reject_duplicate_json_keys(variables).is_err()
                || serde_json::from_slice::<Value>(variables)
                    .ok()
                    .is_none_or(|value| !value.is_object())
            {
                return false;
            }
            let Ok(query_text) = std::str::from_utf8(query) else {
                return false;
            };
            let lower = query_text.to_ascii_lowercase();
            !lower.contains("mutation")
                && !lower.contains("subscription")
                && (lower.contains("query") || query_text.trim_start().starts_with('{'))
        }
    }
}

fn g0_rest_query_pairs(query: &[u8]) -> Option<Vec<(String, String)>> {
    let mut pairs = g0_request_query_pairs_ordered(query)?;
    pairs.sort();
    Some(pairs)
}

/// Parse the producer's canonical REST query payload.  The acquisition
/// collector serializes its sorted map as `key\0value\0` pairs; URL query
/// strings are parsed separately by `g0_endpoint_query_pairs_ordered`.
fn g0_request_query_pairs_ordered(query: &[u8]) -> Option<Vec<(String, String)>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    if query.last() != Some(&0) {
        return None;
    }
    let fields = query.split(|byte| *byte == 0).collect::<Vec<_>>();
    if fields.len() < 3 || fields.len() % 2 == 0 {
        return None;
    }
    let mut keys = BTreeSet::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut pair_start = 0;
    while pair_start + 1 < fields.len() {
        let key = std::str::from_utf8(fields[pair_start]).ok()?;
        let value = std::str::from_utf8(fields[pair_start + 1]).ok()?;
        if key.is_empty()
            || value.contains('#')
            || key.contains('%')
            || value.contains('%')
            || !keys.insert(key.to_owned())
            || pairs
                .last()
                .is_some_and(|(previous, _)| previous.as_str() >= key)
        {
            return None;
        }
        pairs.push((key.to_owned(), value.to_owned()));
        pair_start += 2;
    }
    Some(pairs)
}

fn g0_endpoint_query_pairs_ordered(query: &[u8]) -> Option<Vec<(String, String)>> {
    let query = std::str::from_utf8(query).ok()?;
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut keys = BTreeSet::new();
    let mut pairs = Vec::new();
    for part in query.split('&') {
        let (key, value) = part.split_once('=')?;
        if key.is_empty()
            || value.contains('#')
            || key.contains('%')
            || value.contains('%')
            || !keys.insert(key.to_owned())
        {
            return None;
        }
        pairs.push((key.to_owned(), value.to_owned()));
    }
    Some(pairs)
}

fn g0_request_stream_key(
    request: &G0RequestRecord,
    query: Option<&[u8]>,
    variables: Option<&[u8]>,
) -> Option<(G0ApiKind, String, String, String)> {
    let query = query?;
    let variables = variables?;
    match request.api {
        G0ApiKind::Rest => {
            let payload_pairs = g0_request_query_pairs_ordered(query)?;
            let stable_query = g0_effective_request_query(request, &payload_pairs)?
                .into_iter()
                .filter(|(key, _)| key != "page" && key != "after")
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&");
            Some((
                G0ApiKind::Rest,
                g0_endpoint_path_query(&request.endpoint_or_operation)
                    .ok()?
                    .0,
                digest_bytes(stable_query.as_bytes()),
                digest_bytes(b"{}"),
            ))
        }
        G0ApiKind::Graphql => {
            reject_duplicate_json_keys(variables).ok()?;
            let mut value = serde_json::from_slice::<Value>(variables).ok()?;
            g0_remove_pagination_variables(&mut value);
            let normalized = canonical_json(&value);
            Some((
                G0ApiKind::Graphql,
                request.endpoint_or_operation.clone(),
                digest_bytes(query),
                digest_bytes(normalized.as_bytes()),
            ))
        }
    }
}

fn g0_remove_pagination_variables(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|key, _| {
                !matches!(
                    key.as_str(),
                    "after" | "before" | "cursor" | "endCursor" | "page"
                )
            });
            for child in object.values_mut() {
                g0_remove_pagination_variables(child);
            }
        }
        Value::Array(array) => {
            for child in array {
                g0_remove_pagination_variables(child);
            }
        }
        _ => {}
    }
}

fn g0_api_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.github.com")
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url)
}

fn g0_next_link_matches(current: &G0RequestRecord, next: &G0RequestRecord) -> bool {
    if let Some(link) = current.page.link_next.as_deref() {
        let Some(url) = g0_api_url(link) else {
            return false;
        };
        let Some((next_path, _)) = g0_endpoint_path_query(&next.endpoint_or_operation).ok() else {
            return false;
        };
        if url.path() != next_path {
            return false;
        }
        let Some(next_payload) = BASE64
            .decode(&next.query_base64)
            .ok()
            .and_then(|bytes| g0_request_query_pairs_ordered(&bytes))
        else {
            return false;
        };
        let Some(next_query) = g0_effective_request_query(next, &next_payload) else {
            return false;
        };
        let link_query = g0_endpoint_query_pairs_ordered(
            url.query().unwrap_or_default().as_bytes(),
        )
        .map(|mut pairs| {
            pairs.sort();
            pairs
        });
        link_query == Some(next_query)
    } else {
        current.page.cursor_out.is_some() && current.page.cursor_out == next.page.cursor_in
    }
}

fn g0_storage_ref(value: &str, digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return false;
    };
    value.strip_prefix("sha256://") == Some(hex)
}

fn digest_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hex}")
}

fn g0_repo_source_url(repository: &str, value: &str) -> bool {
    nonempty_url(value) && value.starts_with(&format!("https://github.com/{repository}/"))
}

fn run_url_matches_repository(value: &str, repository: &str, run_id: u64) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == format!("/{repository}/actions/runs/{run_id}")
}

fn check_run_url_matches_provider(
    value: &str,
    repository: &str,
    provider: &G0CheckProvider,
    check_run_id: u64,
) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let expected_path = match provider {
        G0CheckProvider::GithubActions {
            workflow_run_id, ..
        } => {
            format!("/{repository}/actions/runs/{workflow_run_id}/job/{check_run_id}")
        }
        G0CheckProvider::ExternalApp => format!("/{repository}/runs/{check_run_id}"),
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == expected_path
}

fn job_url_matches_repository(
    value: &str,
    repository: &str,
    workflow_run_id: u64,
    job_id: u64,
) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == format!("/{repository}/actions/runs/{workflow_run_id}/job/{job_id}")
}

fn g0_context_pairs(contexts: &[RequiredContext]) -> BTreeSet<(String, String)> {
    contexts
        .iter()
        .map(|context| (context.context.clone(), context.app_id.clone()))
        .collect()
}

fn g0_policy_pairs(rulesets: &[G0RulesetInventory]) -> BTreeSet<(String, String)> {
    rulesets
        .iter()
        .flat_map(|ruleset| {
            ruleset
                .required_checks
                .iter()
                .map(|check| (check.context.clone(), check.app_id.clone()))
        })
        .collect()
}

fn check_g0_workflow_source(
    field: &str,
    source: &G0WorkflowSource,
    repository: &str,
    default_branch_sha: &str,
    dependencies: &[G0WorkflowDependency],
    collector: &G0CollectorSnapshot,
    findings: &mut Vec<Finding>,
) -> Option<DerivedWorkflowPlan> {
    let raw_ids = collector
        .raw_objects
        .iter()
        .map(|raw| raw.raw_id.clone())
        .collect::<BTreeSet<_>>();
    let decoded = BASE64.decode(&source.bytes_base64);
    let valid = !source.repository.trim().is_empty()
        && source.repository == repository
        && source.path.starts_with(".github/workflows/")
        && !source.path.contains("..")
        && valid_sha(&source.revision)
        && valid_sha(&source.source_sha)
        && source.source_sha == default_branch_sha
        && workflow_source_url_matches(source)
        && matches!(source.media_type.as_str(), "text/yaml" | "application/yaml")
        && source.canonicalization == "raw-utf8"
        && valid_digest(&source.sha256)
        && g0_storage_ref(&source.storage_ref, &source.sha256)
        && decoded.as_ref().is_ok_and(|bytes| {
            bytes.len() as u64 == source.byte_length && digest_bytes(bytes) == source.sha256
        })
        && !source.raw_object_refs.is_empty()
        && source
            .raw_object_refs
            .iter()
            .all(|raw_id| raw_ids.contains(raw_id));
    let raw_binding = source_has_raw_binding(
        source,
        &collector.requests,
        &collector.raw_objects,
        "workflow.source",
    );
    if !valid || !raw_binding {
        finding(
            findings,
            "g0-workflow-source",
            repository,
            field,
            "workflow source must bind repository/path, immutable revision/commit, URL, raw UTF-8 bytes, and recomputed digest",
        );
        return None;
    }
    let source_key = format!("{}/{}", source.repository, source.path);
    match derive_workflow_plan(source, dependencies) {
        Ok(plan) => Some(plan),
        Err(error) => {
            finding(
                findings,
                "g0-workflow-derivation",
                repository,
                field,
                format!("cannot derive source-bound workflow plan for {source_key}: {error}"),
            );
            None
        }
    }
}

fn workflow_source_url_matches(source: &G0WorkflowSource) -> bool {
    let Ok(url) = Url::parse(&source.source_url) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    url.path()
        == format!(
            "/{}/blob/{}/{}",
            source.repository, source.source_sha, source.path
        )
}

fn g0_api_base(value: &str) -> bool {
    g0_api_url(value).is_some_and(|url| matches!(url.path(), "" | "/") && url.query().is_none())
}

fn check_g0_dependency_source(
    field: &str,
    source: &G0WorkflowSource,
    raw_ids: &BTreeSet<String>,
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    let decoded = BASE64.decode(&source.bytes_base64);
    if source.repository.trim().is_empty()
        || source.path.trim().is_empty()
        || source.path.contains("..")
        || !valid_sha(&source.revision)
        || !valid_sha(&source.source_sha)
        || !workflow_source_url_matches(source)
        || !matches!(source.media_type.as_str(), "text/yaml" | "application/yaml")
        || source.canonicalization != "raw-utf8"
        || !valid_digest(&source.sha256)
        || !g0_storage_ref(&source.storage_ref, &source.sha256)
        || decoded.is_err()
        || decoded.as_ref().is_ok_and(|bytes| {
            bytes.len() as u64 != source.byte_length || digest_bytes(bytes) != source.sha256
        })
        || source.raw_object_refs.is_empty()
        || source
            .raw_object_refs
            .iter()
            .any(|raw_id| !raw_ids.contains(raw_id))
        || !source_has_raw_binding(source, requests, raw_objects, "workflow.dependency.source")
    {
        finding(
            findings,
            "g0-workflow-dependency-source",
            &source.repository,
            field,
            "workflow dependency source must bind immutable repository bytes and raw references",
        );
    }
}

fn source_has_raw_binding(
    source: &G0WorkflowSource,
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
    expected_kind: &str,
) -> bool {
    let Ok(bytes) = BASE64.decode(&source.bytes_base64) else {
        return false;
    };
    source.raw_object_refs.iter().any(|raw_id| {
        raw_objects.iter().any(|raw| {
            raw.raw_id == *raw_id
                && raw.object_kind == expected_kind
                && raw.sha256 == source.sha256
                && BASE64
                    .decode(&raw.bytes_base64)
                    .ok()
                    .is_some_and(|raw_bytes| raw_bytes == bytes)
                && requests.iter().any(|request| {
                    request.request_id == raw.request_id
                        && request.response_raw_ref == raw.raw_id
                        && g0_source_request_matches(request, source)
                })
        })
    })
}

fn g0_source_request_matches(request: &G0RequestRecord, source: &G0WorkflowSource) -> bool {
    let Ok((path, query)) = g0_endpoint_path_query(&request.endpoint_or_operation) else {
        return false;
    };
    path == format!("/repos/{}/contents/{}", source.repository, source.path)
        && query.as_slice() == [("ref".to_owned(), source.source_sha.clone())]
        && request.api == G0ApiKind::Rest
        && request.method == "GET"
        && request.http_status == 200
        && request.complete
        && request.state == G0RequestState::Complete
        && BASE64
            .decode(&request.query_base64)
            .ok()
            .is_some_and(|query| query.is_empty() || query.as_slice() == b"per_page=1")
}

fn check_g0_derived_plan(
    repository: &str,
    manifest: &ManifestRepository,
    workflow: &G0WorkflowInventory,
    plan: &DerivedWorkflowPlan,
    findings: &mut Vec<Finding>,
) {
    let dependencies = workflow
        .reusable_workflows
        .iter()
        .chain(workflow.actions.iter())
        .chain(workflow.scanners.iter())
        .collect::<Vec<_>>();
    let expected_workloads = manifest
        .expected_workload_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if expected_workloads.is_empty() || manifest.expected_jobs.is_empty() {
        finding(
            findings,
            "g0-workflow-plan",
            repository,
            "manifest.expected_workload_ids/expected_jobs",
            "source-derived G0 plan requires non-empty expected workloads and jobs",
        );
        return;
    }
    let actual_jobs = plan
        .jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    let expected_jobs = manifest
        .expected_jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<BTreeSet<_>>();
    if actual_jobs != expected_jobs || expected_workloads != actual_jobs {
        finding(
            findings,
            "g0-workflow-plan",
            repository,
            "workflow.source/jobs",
            "source-derived workflow job identities must exactly equal reviewed expected workloads/jobs",
        );
    }
    if plan.events != workflow.events.iter().cloned().collect::<BTreeSet<_>>() {
        finding(
            findings,
            "g0-workflow-events",
            repository,
            "workflows.events",
            "workflow trigger inventory must equal events parsed from immutable source bytes",
        );
    }
    for expected in &manifest.expected_jobs {
        let actual = plan
            .jobs
            .iter()
            .filter(|job| job.job_id == expected.job_id)
            .collect::<Vec<_>>();
        if actual.is_empty() {
            continue;
        }
        if actual.iter().any(|job| {
            job.provider != expected.provider
                || job.platform != expected.platform
                || job.architecture != expected.architecture
        }) {
            finding(
                findings,
                "g0-workflow-job-target",
                repository,
                "manifest.expected_jobs",
                format!(
                    "source-derived job {} target differs from reviewed provider/platform/architecture",
                    expected.job_id
                ),
            );
        }
    }
    let derived_children = plan
        .child_edges
        .iter()
        .map(|edge| {
            (
                edge.root_workload_id.clone(),
                edge.repository.clone(),
                edge.workflow_path.clone(),
                edge.event.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut child_edge_ids = BTreeSet::new();
    for edge in &plan.child_edges {
        let edge_id = (
            edge.root_workload_id.clone(),
            edge.workload_id.clone(),
            edge.repository.clone(),
            edge.workflow_path.clone(),
            edge.event.clone(),
            edge.relation.clone(),
            edge.source_sha.clone(),
            edge.parent_repository.clone(),
            edge.parent_workflow_path.clone(),
            edge.parent_source_sha.clone(),
        );
        if !child_edge_ids.insert(edge_id) {
            finding(
                findings,
                "g0-workflow-child",
                repository,
                "workflow.source/child_edges",
                "source-derived child identities must be unique",
            );
        }
        let Some(expected) = manifest
            .expected_jobs
            .iter()
            .find(|job| job.job_id == edge.root_workload_id)
        else {
            finding(
                findings,
                "g0-workflow-child",
                repository,
                "workflow.source/child_edges",
                format!(
                    "source-derived child edge has no reviewed root workload {}",
                    edge.root_workload_id
                ),
            );
            continue;
        };
        let parent_source_matches = (edge.parent_repository == repository
            && edge.parent_workflow_path == workflow.source.path
            && edge.parent_source_sha == workflow.source.source_sha)
            || dependencies.iter().any(|dependency| {
                dependency.source.repository == edge.parent_repository
                    && dependency.source.path == edge.parent_workflow_path
                    && dependency.source.source_sha == edge.parent_source_sha
            });
        let matches = if edge.workload_id == edge.root_workload_id {
            expected.child_workflow.as_ref().is_some_and(|child| {
                child.repository == edge.repository
                    && child.workflow_path == edge.workflow_path
                    && child.event == edge.event
            })
        } else {
            expected.child_workflow.is_some() && parent_source_matches
        };
        if !matches {
            finding(
                findings,
                "g0-workflow-child",
                repository,
                "manifest.expected_jobs.child_workflow",
                format!(
                    "reviewed child obligation for {} does not match source-derived {} edge",
                    edge.root_workload_id, edge.relation
                ),
            );
        }
    }
    for expected in manifest
        .expected_jobs
        .iter()
        .filter(|job| job.child_workflow.is_some())
    {
        let Some(child) = expected.child_workflow.as_ref() else {
            continue;
        };
        if !derived_children.contains(&(
            expected.job_id.clone(),
            child.repository.clone(),
            child.workflow_path.clone(),
            child.event.clone(),
        )) {
            finding(
                findings,
                "g0-workflow-child",
                repository,
                "workflow.source/child_edges",
                format!(
                    "reviewed child obligation for {} is absent from immutable source",
                    expected.job_id
                ),
            );
        }
    }
}

fn check_g0_repositories(
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    collector: &G0CollectorSnapshot,
    raw_ids: &BTreeSet<String>,
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    let expected = canonical_scope();
    let actual = collector
        .repositories
        .iter()
        .map(|repo| repo.repository.as_str())
        .collect::<BTreeSet<_>>();
    if collector.repositories.len() != REQUIRED_REPOSITORIES || actual != expected {
        finding(
            findings,
            "g0-scope",
            "",
            "evidence.g0_inventory.collector_snapshot.repositories",
            "typed collector repository inventory must equal the fixed canonical 32-repository set exactly",
        );
    }

    let mut seen_ids = BTreeSet::new();
    for repo in &collector.repositories {
        if !expected.contains(repo.repository.as_str()) {
            finding(
                findings,
                "g0-scope",
                &repo.repository,
                "repositories.repository",
                "collector row is outside the fixed canonical scope",
            );
        }
        if !seen_ids.insert(repo.repository_id) || repo.repository_id == 0 {
            finding(
                findings,
                "g0-repository-id",
                &repo.repository,
                "repositories.repository_id",
                "repository IDs must be positive and unique",
            );
        }
        validate_repository_name(&repo.repository, "repositories.repository", findings);
        if repo.default_branch.trim().is_empty() {
            finding(
                findings,
                "g0-default-branch",
                &repo.repository,
                "repositories.default_branch",
                "observed default branch is required",
            );
        }
        validate_sha(
            &repo.repository,
            "repositories.default_branch_sha",
            &repo.default_branch_sha,
            findings,
        );
        if let Some(manifest_repo) = manifest
            .repositories
            .iter()
            .find(|candidate| candidate.repository == repo.repository)
            && repo.default_branch != manifest_repo.default_branch
        {
            finding(
                findings,
                "g0-default-branch-mismatch",
                &repo.repository,
                "repositories.default_branch",
                "observed default branch differs from the reviewed manifest",
            );
        }
        if let Some(snapshot_repo) = snapshot
            .repositories
            .iter()
            .find(|candidate| candidate.repository == repo.repository)
        {
            if repo.repository_id != snapshot_repo.repository_id
                || repo.default_branch != snapshot_repo.default_branch
                || repo.default_branch_sha != snapshot_repo.default_branch_sha
            {
                finding(
                    findings,
                    "g0-snapshot-mismatch",
                    &repo.repository,
                    "repositories.repository_id/default_branch/default_branch_sha",
                    "typed collector default branch facts differ from the independently captured snapshot",
                );
            }
            if g0_policy_pairs(&repo.rulesets)
                != g0_context_pairs(&snapshot_repo.ruleset.required_checks)
            {
                finding(
                    findings,
                    "g0-ruleset-snapshot-mismatch",
                    &repo.repository,
                    "repositories.rulesets.required_checks",
                    "collector ruleset contexts/apps differ from the independent snapshot",
                );
            }
            let collector_workflows = repo
                .workflows
                .iter()
                .map(|workflow| {
                    (
                        workflow.source.path.clone(),
                        workflow.source.revision.clone(),
                        workflow.source.source_sha.clone(),
                    )
                })
                .collect::<BTreeSet<_>>();
            let snapshot_workflows = snapshot_repo
                .workflows
                .iter()
                .map(|workflow| {
                    (
                        workflow.path.clone(),
                        workflow.revision.clone(),
                        workflow.source_sha.clone(),
                    )
                })
                .collect::<BTreeSet<_>>();
            if collector_workflows != snapshot_workflows {
                finding(
                    findings,
                    "g0-workflow-snapshot-mismatch",
                    &repo.repository,
                    "repositories.workflows",
                    "collector workflow/source inventory differs from the independent snapshot",
                );
            }
            check_g0_main_checks_against_snapshot(
                &repo.repository,
                &repo.main_checks,
                snapshot_repo,
                findings,
            );
        } else {
            finding(
                findings,
                "g0-snapshot-mismatch",
                &repo.repository,
                "repositories",
                "typed collector repository is absent from the independent snapshot",
            );
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.repositories.raw_object_refs",
            &repo.raw_object_refs,
            raw_ids,
            findings,
        );

        if repo.rulesets.is_empty() {
            finding(
                findings,
                "g0-ruleset-inventory",
                &repo.repository,
                "repositories.rulesets",
                "complete ruleset inventory is required",
            );
        }
        let mut ruleset_ids = BTreeSet::new();
        let mut required_policy_ids = BTreeSet::new();
        for ruleset in &repo.rulesets {
            if ruleset.ruleset_id == 0
                || ruleset.name.trim().is_empty()
                || !ruleset.complete
                || !g0_repo_source_url(&repo.repository, &ruleset.source_url)
                || !ruleset_ids.insert(ruleset.ruleset_id)
            {
                finding(
                    findings,
                    "g0-ruleset-inventory",
                    &repo.repository,
                    "repositories.rulesets",
                    "ruleset rows require immutable identity, repository-bound source URL, and complete pagination",
                );
            }
            check_g0_raw_refs(
                "evidence.g0_inventory.collector_snapshot.rulesets.raw_object_refs",
                &ruleset.raw_object_refs,
                raw_ids,
                findings,
            );
            for required in &ruleset.required_checks {
                if required.context.trim().is_empty()
                    || required.app_id.trim().is_empty()
                    || required.ruleset_id != ruleset.ruleset_id
                    || !g0_valid_app_id(&required.app_id)
                    || !required_policy_ids
                        .insert((required.context.clone(), required.app_id.clone()))
                {
                    finding(
                        findings,
                        "g0-required-check-policy",
                        &repo.repository,
                        "repositories.rulesets.required_checks",
                        "required check policy must bind context/app and owning ruleset IDs",
                    );
                }
                check_g0_raw_refs(
                    "evidence.g0_inventory.collector_snapshot.required_check.raw_object_refs",
                    &required.raw_object_refs,
                    raw_ids,
                    findings,
                );
            }
        }
        check_g0_artifact_observations(
            &repo.repository,
            &repo.artifacts,
            G0ArtifactContext {
                default_branch_sha: &repo.default_branch_sha,
                open_prs: &repo.open_prs,
                main_checks: &repo.main_checks,
                requests: &collector.requests,
                raw_ids,
                raw_objects: &collector.raw_objects,
            },
            findings,
        );
        if repo.workflows.is_empty() {
            finding(
                findings,
                "g0-workflow-inventory",
                &repo.repository,
                "repositories.workflows",
                "complete workflow, reusable-action, scanner, and generated-state inventory is required",
            );
        }
        let mut workflow_ids = BTreeSet::new();
        for workflow in &repo.workflows {
            let dependencies = workflow
                .reusable_workflows
                .iter()
                .chain(workflow.actions.iter())
                .chain(workflow.scanners.iter())
                .cloned()
                .collect::<Vec<_>>();
            let source_plan = check_g0_workflow_source(
                "evidence.g0_inventory.collector_snapshot.workflow.source",
                &workflow.source,
                &repo.repository,
                &repo.default_branch_sha,
                &dependencies,
                collector,
                findings,
            );
            if workflow.source.path.trim().is_empty()
                || !valid_sha(&workflow.source.revision)
                || !valid_sha(&workflow.source.source_sha)
                || workflow.source.source_sha != repo.default_branch_sha
                || workflow.events.is_empty()
                || !workflow_ids.insert((
                    workflow.source.path.clone(),
                    workflow.source.revision.clone(),
                    workflow.source.source_sha.clone(),
                ))
            {
                finding(
                    findings,
                    "g0-workflow-inventory",
                    &repo.repository,
                    "repositories.workflows",
                    "workflow rows require path, immutable revision/source, and trigger events",
                );
            }
            if workflow.events.iter().any(|event| !g0_valid_event(event)) {
                finding(
                    findings,
                    "g0-workflow-event",
                    &repo.repository,
                    "repositories.workflows.events",
                    "workflow inventory contains an unsupported trigger event",
                );
            }
            check_g0_artifact_reference(
                "evidence.g0_inventory.collector_snapshot.workflow.generated_state",
                &workflow.generated_state,
                raw_ids,
                &collector.raw_objects,
                findings,
            );
            check_g0_raw_refs(
                "evidence.g0_inventory.collector_snapshot.workflow.raw_object_refs",
                &workflow.raw_object_refs,
                raw_ids,
                findings,
            );
            for dependency in workflow
                .reusable_workflows
                .iter()
                .chain(workflow.actions.iter())
                .chain(workflow.scanners.iter())
            {
                check_g0_dependency_source(
                    "evidence.g0_inventory.collector_snapshot.workflow_dependency.source",
                    &dependency.source,
                    raw_ids,
                    &collector.requests,
                    &collector.raw_objects,
                    findings,
                );
                if dependency.kind.trim().is_empty()
                    || dependency.source.path.trim().is_empty()
                    || !valid_sha(&dependency.source.revision)
                {
                    finding(
                        findings,
                        "g0-workflow-dependency",
                        &repo.repository,
                        "repositories.workflows.dependencies",
                        "workflow dependencies require kind, path, and immutable revision",
                    );
                }
                validate_repository_name(
                    &dependency.source.repository,
                    "workflow dependency.repository",
                    findings,
                );
                check_g0_raw_refs(
                    "evidence.g0_inventory.collector_snapshot.workflow_dependency.raw_object_refs",
                    &dependency.source.raw_object_refs,
                    raw_ids,
                    findings,
                );
            }
            if let Some(manifest_repo) = manifest
                .repositories
                .iter()
                .find(|candidate| candidate.repository == repo.repository)
                && workflow.source.path == manifest_repo.workflow_path
                && workflow.source.revision == manifest_repo.workflow_revision
                && let Some(plan) = source_plan.as_ref()
            {
                check_g0_derived_plan(&repo.repository, manifest_repo, workflow, plan, findings);
                check_g0_source_jobs(
                    &repo.repository,
                    manifest_repo,
                    workflow,
                    plan,
                    raw_ids,
                    findings,
                );
            }
        }
        if let Some(manifest_repo) = manifest
            .repositories
            .iter()
            .find(|candidate| candidate.repository == repo.repository)
            && !repo.workflows.iter().any(|workflow| {
                workflow.source.path == manifest_repo.workflow_path
                    && workflow.source.revision == manifest_repo.workflow_revision
                    && workflow.source.source_sha == repo.default_branch_sha
            })
        {
            finding(
                findings,
                "g0-workflow-mismatch",
                &repo.repository,
                "repositories.workflows",
                "collector workflow inventory must contain the reviewed workflow path and revision on the observed default branch",
            );
        }
        if repo.main_checks.is_empty() {
            finding(
                findings,
                "g0-check-inventory",
                &repo.repository,
                "repositories.main_checks",
                "current default-branch check/run inventory is required",
            );
        }
        let manifest_contexts = manifest
            .repositories
            .iter()
            .find(|candidate| candidate.repository == repo.repository)
            .map(|candidate| g0_context_pairs(&candidate.required_check_contexts_and_apps))
            .unwrap_or_default();
        let observed_contexts = g0_policy_pairs(&repo.rulesets);
        if observed_contexts != manifest_contexts {
            finding(
                findings,
                "g0-required-check-policy",
                &repo.repository,
                "repositories.rulesets.required_checks",
                "collector ruleset context/app inventory must exactly match the reviewed manifest",
            );
        }
        check_g0_check_producers(
            &repo.repository,
            &repo.main_checks,
            G0CheckExpectation {
                contexts: &manifest_contexts,
                source_sha: &repo.default_branch_sha,
                checkout_sha: &repo.default_branch_sha,
                event: "push",
                raw_ids,
            },
            findings,
        );
        check_g0_check_raw_evidence(
            &repo.repository,
            &repo.main_checks,
            requests,
            raw_objects,
            findings,
        );

        let snapshot_prs = snapshot
            .repositories
            .iter()
            .find(|candidate| candidate.repository == repo.repository)
            .map(|candidate| {
                candidate
                    .open_prs
                    .iter()
                    .map(|pr| pr.number)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let collector_prs = repo
            .open_prs
            .iter()
            .map(|pr| pr.number)
            .collect::<BTreeSet<_>>();
        if collector_prs != snapshot_prs || collector_prs.len() != repo.open_prs.len() {
            finding(
                findings,
                "g0-pr-coverage",
                &repo.repository,
                "repositories.open_prs",
                "typed collector must cover every current open PR exactly once",
            );
        }
        for pr in &repo.open_prs {
            check_g0_pull_request(
                &repo.repository,
                pr,
                &manifest_contexts,
                snapshot,
                &repo.workflows,
                raw_ids,
                requests,
                raw_objects,
                findings,
            );
            check_g0_check_raw_evidence(
                &repo.repository,
                &pr.required_check_producers,
                requests,
                raw_objects,
                findings,
            );
        }
    }
}

struct G0ArtifactContext<'a> {
    default_branch_sha: &'a str,
    open_prs: &'a [G0PullRequestInventory],
    main_checks: &'a [G0CheckProducer],
    requests: &'a [G0RequestRecord],
    raw_ids: &'a BTreeSet<String>,
    raw_objects: &'a [G0RawObjectRef],
}

fn check_g0_artifact_observations(
    repository: &str,
    artifacts: &[G0ArtifactObservation],
    context: G0ArtifactContext<'_>,
    findings: &mut Vec<Finding>,
) {
    if artifacts.is_empty() {
        finding(
            findings,
            "g0-artifact-inventory",
            repository,
            "repositories.artifacts",
            "complete source-bound artifact inventory is required",
        );
        return;
    }

    let mut artifact_ids = BTreeSet::new();
    let mut artifact_names = BTreeSet::new();
    let mut known_run_bindings = BTreeMap::<u64, BTreeSet<(u32, String)>>::new();
    for check in context.main_checks {
        if let G0CheckProvider::GithubActions {
            workflow_run_id,
            run_attempt,
            ..
        } = &check.provider
        {
            known_run_bindings
                .entry(*workflow_run_id)
                .or_default()
                .insert((*run_attempt, check.source_sha.clone()));
        }
    }
    let mut known_source_shas = BTreeSet::from([context.default_branch_sha.to_owned()]);
    for pr in context.open_prs {
        known_source_shas.insert(pr.head_sha.clone());
        known_source_shas.insert(pr.base_sha.clone());
        known_source_shas.insert(pr.tested_merge_sha.clone());
        if let Some(merge_group_sha) = &pr.merge_group_sha {
            known_source_shas.insert(merge_group_sha.clone());
        }
        for check in &pr.required_check_producers {
            if let G0CheckProvider::GithubActions {
                workflow_run_id,
                run_attempt,
                ..
            } = &check.provider
            {
                known_run_bindings
                    .entry(*workflow_run_id)
                    .or_default()
                    .insert((*run_attempt, check.source_sha.clone()));
            }
        }
    }

    for artifact in artifacts {
        if artifact.artifact_id == 0
            || !artifact_ids.insert(artifact.artifact_id)
            || artifact.name.trim().is_empty()
            || !artifact_names.insert(artifact.name.clone())
            || artifact.run_id == 0
            || artifact.run_attempt == 0
            || !valid_sha(&artifact.run_head_sha)
            || !known_source_shas.contains(&artifact.run_head_sha)
            || !known_run_bindings
                .get(&artifact.run_id)
                .is_some_and(|bindings| {
                    bindings.contains(&(artifact.run_attempt, artifact.run_head_sha.clone()))
                })
            || !valid_digest(&artifact.digest)
            || artifact.expired
            || !g0_artifact_source_url(repository, artifact.artifact_id, &artifact.source_url)
        {
            finding(
                findings,
                "g0-artifact-identity",
                repository,
                "repositories.artifacts",
                "artifact rows require unique ID/name, source-bound run identity, unexpired digest, and canonical API URL",
            );
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.artifact.raw_object_refs",
            &artifact.raw_object_refs,
            context.raw_ids,
            findings,
        );
        let Some(raw) = artifact.raw_object_refs.iter().find_map(|raw_id| {
            context
                .raw_objects
                .iter()
                .find(|raw| raw.raw_id == *raw_id && raw.object_kind == "workflow_artifacts")
        }) else {
            finding(
                findings,
                "g0-artifact-digest",
                repository,
                "repositories.artifacts",
                "artifact archive digest must be present and its row must bind to a workflow_artifacts API object",
            );
            continue;
        };
        if !g0_artifact_row_matches_raw(repository, artifact, raw) {
            finding(
                findings,
                "g0-artifact-row",
                repository,
                "repositories.artifacts",
                "artifact identity must match one row in the independently captured artifact page",
            );
        }
        let Some(request) = context
            .requests
            .iter()
            .find(|request| request.request_id == raw.request_id)
        else {
            finding(
                findings,
                "g0-artifact-request",
                repository,
                "repositories.artifacts.raw_object_refs",
                "artifact raw object must bind to a captured artifact request",
            );
            continue;
        };
        let expected_endpoint = format!(
            "/repos/{repository}/actions/runs/{}/artifacts",
            artifact.run_id
        );
        if request.endpoint_or_operation != expected_endpoint
            || request.method != "GET"
            || request.http_status != 200
            || !request.complete
            || !matches!(
                request.state,
                G0RequestState::Complete | G0RequestState::EmptyComplete
            )
        {
            finding(
                findings,
                "g0-artifact-request",
                repository,
                "repositories.artifacts.raw_object_refs",
                "artifact raw object must bind to the successful API request for its exact run",
            );
        }
        if raw.object_kind != "workflow_artifacts" {
            finding(
                findings,
                "g0-artifact-raw-kind",
                repository,
                "repositories.artifacts.raw_object_refs",
                "artifact provenance must reference a workflow_artifacts API object",
            );
        }
    }
}

fn check_g0_source_jobs(
    repository: &str,
    manifest: &ManifestRepository,
    workflow: &G0WorkflowInventory,
    plan: &DerivedWorkflowPlan,
    raw_ids: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    if workflow.source_jobs.is_empty() {
        finding(
            findings,
            "g0-source-job-inventory",
            repository,
            "repositories.workflows.source_jobs",
            "immutable workflow source must emit a non-empty typed job inventory",
        );
        return;
    }

    let mut seen = BTreeSet::new();
    let source_jobs = workflow
        .source_jobs
        .iter()
        .map(|job| {
            if !seen.insert(job.job_id.clone())
                || job.job_id.trim().is_empty()
                || job.workload_id.trim().is_empty()
                || job.provider.trim().is_empty()
                || job.platform.trim().is_empty()
                || job.architecture.trim().is_empty()
            {
                finding(
                    findings,
                    "g0-source-job-identity",
                    repository,
                    "repositories.workflows.source_jobs",
                    "source jobs require unique non-empty logical and target identities",
                );
            }
            check_g0_raw_refs(
                "evidence.g0_inventory.collector_snapshot.workflow.source_jobs.raw_object_refs",
                &job.raw_object_refs,
                raw_ids,
                findings,
            );
            if !job
                .raw_object_refs
                .iter()
                .any(|raw_id| workflow.source.raw_object_refs.contains(raw_id))
            {
                finding(
                    findings,
                    "g0-source-job-source",
                    repository,
                    "repositories.workflows.source_jobs.raw_object_refs",
                    "source job provenance must include the immutable workflow source object",
                );
            }
            (
                job.job_id.clone(),
                job.workload_id.clone(),
                job.provider.clone(),
                job.platform.clone(),
                job.architecture.clone(),
                job.required,
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_jobs = manifest
        .expected_jobs
        .iter()
        .map(|job| {
            (
                job.job_id.clone(),
                job.workload_id.clone(),
                job.provider.clone(),
                job.platform.clone(),
                job.architecture.clone(),
                job.required,
            )
        })
        .collect::<BTreeSet<_>>();
    let derived_job_ids = plan
        .jobs
        .iter()
        .map(|job| job.job_id.as_str())
        .collect::<BTreeSet<_>>();
    if derived_job_ids.len() != plan.jobs.len() {
        finding(
            findings,
            "g0-source-job-matrix",
            repository,
            "repositories.workflows.source_jobs",
            "finite matrix expands a logical job into multiple instances, but the reviewed source-job contract has no concrete matrix identity; collector must publish every instance before this gate can pass",
        );
    }
    if source_jobs != expected_jobs {
        finding(
            findings,
            "g0-source-job-plan",
            repository,
            "repositories.workflows.source_jobs",
            "source-derived job inventory must exactly match the reviewed expected job plan",
        );
    }

    let derived_jobs = plan
        .jobs
        .iter()
        .map(|job| {
            (
                job.job_id.clone(),
                manifest
                    .expected_jobs
                    .iter()
                    .find(|expected| expected.job_id == job.job_id)
                    .map(|expected| expected.workload_id.clone())
                    .unwrap_or_default(),
                job.provider.clone(),
                job.platform.clone(),
                job.architecture.clone(),
                manifest
                    .expected_jobs
                    .iter()
                    .find(|expected| expected.job_id == job.job_id)
                    .is_some_and(|expected| expected.required),
            )
        })
        .collect::<BTreeSet<_>>();
    if derived_jobs != source_jobs {
        finding(
            findings,
            "g0-source-job-derivation",
            repository,
            "repositories.workflows.source_jobs",
            "typed source jobs must equal the jobs derived from immutable workflow bytes",
        );
    }
}

fn g0_artifact_source_url(repository: &str, artifact_id: u64, value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("api.github.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == format!("/repos/{repository}/actions/artifacts/{artifact_id}/zip")
}

fn g0_artifact_row_matches_raw(
    repository: &str,
    artifact: &G0ArtifactObservation,
    raw: &G0RawObjectRef,
) -> bool {
    let expected_endpoint = format!(
        "/repos/{repository}/actions/runs/{}/artifacts",
        artifact.run_id
    );
    let Ok(contract) = g0_endpoint_contract("workflow_artifacts", &expected_endpoint) else {
        return false;
    };
    let schema = contract.response_schema;
    let Ok(bytes) = BASE64.decode(&raw.bytes_base64) else {
        return false;
    };
    if raw.byte_length != bytes.len() as u64 || digest_bytes(&bytes) != raw.sha256 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    let Some((_, members)) = g0_page_members(&value, schema) else {
        return false;
    };
    members.iter().any(|member| {
        g0_json_u64(member, &["id"]) == Some(artifact.artifact_id)
            && g0_json_string(member, &["name"]) == Some(artifact.name.as_str())
            && member.get("expired").and_then(Value::as_bool) == Some(artifact.expired)
            && g0_json_string(member, &["digest"]) == Some(artifact.digest.as_str())
            && g0_json_u64(member, &["workflow_run", "id"]) == Some(artifact.run_id)
            && g0_json_string(member, &["workflow_run", "head_sha"])
                == Some(artifact.run_head_sha.as_str())
            && g0_json_string(member, &["archive_download_url"])
                == Some(artifact.source_url.as_str())
    })
}

struct G0CheckExpectation<'a> {
    contexts: &'a BTreeSet<(String, String)>,
    source_sha: &'a str,
    checkout_sha: &'a str,
    event: &'a str,
    raw_ids: &'a BTreeSet<String>,
}

fn g0_check_provider_valid(check: &G0CheckProducer, repository: &str) -> bool {
    if check.app_slug.trim().is_empty() {
        return false;
    }
    match &check.provider {
        G0CheckProvider::ExternalApp => {
            check.app_slug != "github-actions"
                && check_run_url_matches_provider(
                    &check.html_url,
                    repository,
                    &check.provider,
                    check.check_run_id,
                )
        }
        G0CheckProvider::GithubActions {
            workflow_run_id,
            run_attempt,
            job_id,
            job_run_id,
            job_run_attempt,
            job_check_run_id,
            job_source_sha,
            job_html_url,
            actual_checkout_sha,
        } => {
            check.app_slug == "github-actions"
                && *workflow_run_id != 0
                && *run_attempt != 0
                && *job_id != 0
                && *job_run_id == *workflow_run_id
                && *job_run_attempt == *run_attempt
                && *job_check_run_id == check.check_run_id
                && valid_sha(job_source_sha)
                && *job_source_sha == check.source_sha
                && valid_sha(actual_checkout_sha)
                && job_url_matches_repository(job_html_url, repository, *workflow_run_id, *job_id)
                && check_run_url_matches_provider(
                    &check.html_url,
                    repository,
                    &check.provider,
                    check.check_run_id,
                )
        }
    }
}

fn g0_actions_checkout_sha(check: &G0CheckProducer) -> Option<&str> {
    match &check.provider {
        G0CheckProvider::GithubActions {
            actual_checkout_sha,
            ..
        } => Some(actual_checkout_sha),
        G0CheckProvider::ExternalApp => None,
    }
}

fn g0_check_job_key(check: &G0CheckProducer) -> Option<(u64, u32, u64)> {
    match &check.provider {
        G0CheckProvider::GithubActions {
            workflow_run_id,
            run_attempt,
            job_id,
            ..
        } => Some((*workflow_run_id, *run_attempt, *job_id)),
        G0CheckProvider::ExternalApp => None,
    }
}

fn g0_json_u64(value: &Value, path: &[&str]) -> Option<u64> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_u64)
}

fn g0_json_string<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_str)
}

fn g0_check_run_identity_valid(value: &Value, check: &G0CheckProducer, app_id: u64) -> bool {
    g0_json_u64(value, &["id"]) == Some(check.check_run_id)
        && g0_json_string(value, &["name"]) == Some(check.context.as_str())
        && g0_json_string(value, &["head_sha"]) == Some(check.source_sha.as_str())
        && g0_json_string(value, &["status"]) == Some(check.status.as_str())
        && g0_json_string(value, &["conclusion"]) == Some(check.conclusion.as_str())
        && g0_json_string(value, &["html_url"]) == Some(check.html_url.as_str())
        && g0_json_u64(value, &["check_suite", "id"]) == Some(check.check_suite_id)
        && g0_json_u64(value, &["app", "id"]) == Some(app_id)
        && g0_json_string(value, &["app", "slug"]) == Some(check.app_slug.as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum G0RawResponseSchema {
    Singular,
    PullRequestsPage,
    RulesetsPage,
    WorkflowsPage,
    WorkflowRunsPage,
    CheckRunsPage,
    CheckSuitesPage,
    CheckSuiteRunsPage,
    JobsPage,
    ArtifactsPage,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum G0EndpointKind {
    Viewer,
    Repository,
    DefaultBranchCommit,
    PullRequestsPage,
    PullRequest,
    RulesetsPage,
    Ruleset,
    WorkflowsPage,
    WorkflowSource,
    WorkflowDependency,
    WorkflowDependencySource,
    WorkflowRunsPage,
    CheckRunsPage,
    CheckRun,
    CheckSuitesPage,
    CheckSuite,
    CheckSuiteRunsPage,
    WorkflowJobsPage,
    WorkflowAttempt,
    WorkflowAttemptJobsPage,
    ArtifactsPage,
    ArtifactArchive,
    WorkflowRun,
    App,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum G0CoveragePurpose {
    Authentication,
    RepositorySnapshot,
    DefaultBranchSnapshot,
    PullRequestInventory,
    RulesetInventory,
    WorkflowInventory,
    WorkflowSource,
    WorkflowDependency,
    WorkflowDependencySource,
    WorkflowRunInventory,
    CheckInventory,
    CheckSuiteInventory,
    JobInventory,
    ArtifactCensus,
    ArtifactArchive,
    WorkflowRunIdentity,
    ProviderIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct G0EndpointContract {
    kind: G0EndpointKind,
    coverage: G0CoveragePurpose,
    response_schema: G0RawResponseSchema,
}

/// Parse every evidence-bearing REST response into a closed endpoint and
/// coverage purpose. Unknown object/path combinations are errors; callers
/// must report them rather than silently treating them as an untyped stream.
fn g0_endpoint_contract(object_kind: &str, endpoint: &str) -> Result<G0EndpointContract, String> {
    let (path, _) = g0_endpoint_path_query(endpoint)?;
    let parts = path
        .strip_prefix('/')
        .ok_or_else(|| "endpoint path must begin with /".to_owned())?
        .split('/')
        .collect::<Vec<_>>();
    let repository_path =
        parts.len() >= 3 && parts[0] == "repos" && !parts[1].is_empty() && !parts[2].is_empty();
    let contract = match object_kind {
        "auth.viewer" if path == "/user" => G0EndpointContract {
            kind: G0EndpointKind::Viewer,
            coverage: G0CoveragePurpose::Authentication,
            response_schema: G0RawResponseSchema::Singular,
        },
        "repository" if repository_path && parts.len() == 3 => G0EndpointContract {
            kind: G0EndpointKind::Repository,
            coverage: G0CoveragePurpose::RepositorySnapshot,
            response_schema: G0RawResponseSchema::Singular,
        },
        "default_branch.commit"
            if repository_path
                && parts.len() == 5
                && parts[3] == "commits"
                && !parts[4].is_empty()
                && !parts[4].contains('/') =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::DefaultBranchCommit,
                coverage: G0CoveragePurpose::DefaultBranchSnapshot,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "pull_requests" if repository_path && parts.len() == 4 && parts[3] == "pulls" => {
            G0EndpointContract {
                kind: G0EndpointKind::PullRequestsPage,
                coverage: G0CoveragePurpose::PullRequestInventory,
                response_schema: G0RawResponseSchema::PullRequestsPage,
            }
        }
        "pull_request"
            if repository_path
                && parts.len() == 5
                && parts[3] == "pulls"
                && parts[4].parse::<u64>().is_ok_and(|id| id > 0) =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::PullRequest,
                coverage: G0CoveragePurpose::PullRequestInventory,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "rulesets" if repository_path && parts.len() == 4 && parts[3] == "rulesets" => {
            G0EndpointContract {
                kind: G0EndpointKind::RulesetsPage,
                coverage: G0CoveragePurpose::RulesetInventory,
                response_schema: G0RawResponseSchema::RulesetsPage,
            }
        }
        "ruleset"
            if repository_path
                && parts.len() == 5
                && parts[3] == "rulesets"
                && parts[4].parse::<u64>().is_ok_and(|id| id > 0) =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::Ruleset,
                coverage: G0CoveragePurpose::RulesetInventory,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "workflows"
            if repository_path
                && parts.len() == 5
                && parts[3] == "actions"
                && parts[4] == "workflows" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::WorkflowsPage,
                coverage: G0CoveragePurpose::WorkflowInventory,
                response_schema: G0RawResponseSchema::WorkflowsPage,
            }
        }
        "workflow.source" if repository_path && g0_is_contents_path(&parts) => G0EndpointContract {
            kind: G0EndpointKind::WorkflowSource,
            coverage: G0CoveragePurpose::WorkflowSource,
            response_schema: G0RawResponseSchema::Singular,
        },
        "workflow.dependency" if repository_path && g0_is_tree_path(&parts) => G0EndpointContract {
            kind: G0EndpointKind::WorkflowDependency,
            coverage: G0CoveragePurpose::WorkflowDependency,
            response_schema: G0RawResponseSchema::Singular,
        },
        "workflow.dependency.source" if repository_path && g0_is_contents_path(&parts) => {
            G0EndpointContract {
                kind: G0EndpointKind::WorkflowDependencySource,
                coverage: G0CoveragePurpose::WorkflowDependencySource,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "workflow_runs"
            if repository_path
                && parts.len() == 5
                && parts[3] == "actions"
                && parts[4] == "runs" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::WorkflowRunsPage,
                coverage: G0CoveragePurpose::WorkflowRunInventory,
                response_schema: G0RawResponseSchema::WorkflowRunsPage,
            }
        }
        "check_runs"
            if repository_path
                && parts.len() == 6
                && parts[3] == "commits"
                && valid_sha(parts[4])
                && parts[5] == "check-runs" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::CheckRunsPage,
                coverage: G0CoveragePurpose::CheckInventory,
                response_schema: G0RawResponseSchema::CheckRunsPage,
            }
        }
        "check_run"
            if repository_path
                && parts.len() == 5
                && parts[3] == "check-runs"
                && parts[4].parse::<u64>().is_ok_and(|id| id > 0) =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::CheckRun,
                coverage: G0CoveragePurpose::CheckInventory,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "check_suites"
            if repository_path
                && parts.len() == 6
                && parts[3] == "commits"
                && valid_sha(parts[4])
                && parts[5] == "check-suites" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::CheckSuitesPage,
                coverage: G0CoveragePurpose::CheckSuiteInventory,
                response_schema: G0RawResponseSchema::CheckSuitesPage,
            }
        }
        "check_suite"
            if repository_path
                && parts.len() == 5
                && parts[3] == "check-suites"
                && parts[4].parse::<u64>().is_ok_and(|id| id > 0) =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::CheckSuite,
                coverage: G0CoveragePurpose::CheckSuiteInventory,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "check_suite_runs"
            if repository_path
                && parts.len() == 6
                && parts[3] == "check-suites"
                && parts[4].parse::<u64>().is_ok_and(|id| id > 0)
                && parts[5] == "check-runs" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::CheckSuiteRunsPage,
                coverage: G0CoveragePurpose::CheckInventory,
                response_schema: G0RawResponseSchema::CheckSuiteRunsPage,
            }
        }
        "workflow_jobs"
            if repository_path
                && parts.len() == 7
                && parts[3] == "actions"
                && parts[4] == "runs"
                && parts[5].parse::<u64>().is_ok_and(|id| id > 0)
                && parts[6] == "jobs" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::WorkflowJobsPage,
                coverage: G0CoveragePurpose::JobInventory,
                response_schema: G0RawResponseSchema::JobsPage,
            }
        }
        "workflow_attempt"
            if repository_path
                && parts.len() == 8
                && parts[3] == "actions"
                && parts[4] == "runs"
                && parts[5].parse::<u64>().is_ok_and(|id| id > 0)
                && parts[6] == "attempts"
                && parts[7].parse::<u32>().is_ok_and(|attempt| attempt > 0) =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::WorkflowAttempt,
                coverage: G0CoveragePurpose::WorkflowRunIdentity,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "workflow_attempt_jobs"
            if repository_path
                && parts.len() == 9
                && parts[3] == "actions"
                && parts[4] == "runs"
                && parts[5].parse::<u64>().is_ok_and(|id| id > 0)
                && parts[6] == "attempts"
                && parts[7].parse::<u32>().is_ok_and(|attempt| attempt > 0)
                && parts[8] == "jobs" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::WorkflowAttemptJobsPage,
                coverage: G0CoveragePurpose::JobInventory,
                response_schema: G0RawResponseSchema::JobsPage,
            }
        }
        "workflow_artifacts"
            if repository_path
                && parts.len() == 7
                && parts[3] == "actions"
                && parts[4] == "runs"
                && parts[5].parse::<u64>().is_ok_and(|id| id > 0)
                && parts[6] == "artifacts" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::ArtifactsPage,
                coverage: G0CoveragePurpose::ArtifactCensus,
                response_schema: G0RawResponseSchema::ArtifactsPage,
            }
        }
        "workflow_artifacts"
            if repository_path
                && parts.len() == 7
                && parts[3] == "actions"
                && parts[4] == "artifacts"
                && parts[5].parse::<u64>().is_ok_and(|id| id > 0)
                && parts[6] == "zip" =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::ArtifactArchive,
                coverage: G0CoveragePurpose::ArtifactArchive,
                response_schema: G0RawResponseSchema::Binary,
            }
        }
        "workflow_run"
            if repository_path
                && parts.len() == 6
                && parts[3] == "actions"
                && parts[4] == "runs"
                && parts[5].parse::<u64>().is_ok_and(|id| id > 0) =>
        {
            G0EndpointContract {
                kind: G0EndpointKind::WorkflowRun,
                coverage: G0CoveragePurpose::WorkflowRunIdentity,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        "app" if parts.len() == 2 && parts[0] == "apps" && !parts[1].is_empty() => {
            G0EndpointContract {
                kind: G0EndpointKind::App,
                coverage: G0CoveragePurpose::ProviderIdentity,
                response_schema: G0RawResponseSchema::Singular,
            }
        }
        _ => {
            return Err(format!(
                "unknown evidence endpoint/object kind: {object_kind} {endpoint}"
            ));
        }
    };
    if !g0_endpoint_inline_query_shape(endpoint, contract.kind) {
        return Err(format!(
            "endpoint query does not match closed evidence contract: {object_kind} {endpoint}"
        ));
    }
    Ok(contract)
}

/// Return the canonical GitHub API path and the query embedded in a recorded
/// endpoint.  The acquisition layer records absolute API URLs, while offline
/// fixtures use the same path in relative form; both forms still have one
/// exact origin and one exact path grammar.  No arbitrary URL is accepted.
fn g0_endpoint_path_query(endpoint: &str) -> Result<(String, Vec<(String, String)>), String> {
    if endpoint.starts_with("https://") {
        let url = g0_api_url(endpoint).ok_or_else(|| {
            "endpoint URL must use the exact https://api.github.com origin".to_owned()
        })?;
        let query = g0_endpoint_query_pairs_ordered(url.query().unwrap_or_default().as_bytes())
            .ok_or_else(|| "endpoint URL query is not canonical".to_owned())?;
        return Ok((url.path().to_owned(), query));
    }
    if endpoint.contains("://") {
        return Err("relative endpoint must not contain a URL origin".to_owned());
    }
    let (path, query) = endpoint.split_once('?').unwrap_or((endpoint, ""));
    if !path.starts_with('/') || path.contains("..") || path.is_empty() {
        return Err("endpoint path is not canonical".to_owned());
    }
    let query = g0_endpoint_query_pairs_ordered(query.as_bytes())
        .ok_or_else(|| "endpoint query is not canonical".to_owned())?;
    Ok((path.to_owned(), query))
}

fn g0_is_contents_path(parts: &[&str]) -> bool {
    parts.len() >= 6
        && parts[3] == "contents"
        && parts[4..]
            .iter()
            .all(|part| !part.is_empty() && *part != "..")
}

fn g0_is_tree_path(parts: &[&str]) -> bool {
    parts.len() == 6 && parts[3] == "git" && parts[4] == "trees" && valid_sha(parts[5])
}

fn g0_capture_raw_json(
    raw_refs: &[String],
    object_kind: &str,
    member_id: u64,
    endpoints: &[String],
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
) -> Option<Value> {
    let candidates = raw_objects
        .iter()
        .filter(|raw| {
            raw.object_kind == object_kind && raw_refs.iter().any(|raw_id| raw_id == &raw.raw_id)
        })
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return None;
    }
    let raw = candidates[0];
    let request = requests.iter().find(|request| {
        request.request_id == raw.request_id && request.response_raw_ref == raw.raw_id
    })?;
    if request.api != G0ApiKind::Rest
        || request.method != "GET"
        || request.http_status != 200
        || !request.complete
        || !matches!(request.state, G0RequestState::Complete)
        || !endpoints
            .iter()
            .any(|endpoint| endpoint == &request.endpoint_or_operation)
    {
        return None;
    }
    let response_contract =
        g0_endpoint_contract(object_kind, &request.endpoint_or_operation).ok()?;
    let response_schema = response_contract.response_schema;
    let query = BASE64.decode(&request.query_base64).ok()?;
    if !g0_response_query_contract(request, &query, response_contract) {
        return None;
    }
    let bytes = BASE64.decode(&raw.bytes_base64).ok()?;
    if raw.byte_length != bytes.len() as u64
        || digest_bytes(&bytes) != raw.sha256
        || !g0_storage_ref(&raw.storage_ref, &raw.sha256)
    {
        return None;
    }
    let value = serde_json::from_slice::<Value>(&bytes).ok()?;
    g0_select_raw_member(value, response_schema, member_id, request)
}

/// Select one object from the exact GitHub page envelope retained by the
/// collector.  Check-runs and jobs are paginated envelopes in the REST API;
/// treating the envelope itself as a singular object silently loses every
/// relationship except whichever fields a caller invents at the top level.
/// Singular endpoints remain accepted only when their returned object ID is
/// the requested ID.  A page declaring its final page must account for every
/// item in `total_count`; duplicate IDs are rejected.
fn g0_select_raw_member(
    value: Value,
    response_schema: G0RawResponseSchema,
    member_id: u64,
    request: &G0RequestRecord,
) -> Option<Value> {
    let envelope_key = match response_schema {
        G0RawResponseSchema::PullRequestsPage => "pulls",
        G0RawResponseSchema::RulesetsPage => "rulesets",
        G0RawResponseSchema::WorkflowsPage => "workflows",
        G0RawResponseSchema::WorkflowRunsPage => "workflow_runs",
        G0RawResponseSchema::CheckRunsPage => "check_runs",
        G0RawResponseSchema::CheckSuitesPage => "check_suites",
        G0RawResponseSchema::CheckSuiteRunsPage => "check_runs",
        G0RawResponseSchema::JobsPage => "jobs",
        G0RawResponseSchema::ArtifactsPage => "artifacts",
        G0RawResponseSchema::Singular | G0RawResponseSchema::Binary => {
            return (g0_json_u64(&value, &["id"]) == Some(member_id)
                && request.page.items_returned == 1
                && !request.page.has_next_page
                && value.get("check_runs").is_none()
                && value.get("check_suites").is_none()
                && value.get("jobs").is_none()
                && value.get("pulls").is_none()
                && value.get("rulesets").is_none()
                && value.get("workflows").is_none()
                && value.get("workflow_runs").is_none())
            .then_some(value);
        }
    };
    {
        let total_count = value.get("total_count")?.as_u64()?;
        let members = value.get(envelope_key)?.as_array()?;
        if members.len() as u32 != request.page.items_returned
            || (!request.page.has_next_page && total_count != members.len() as u64)
            || (request.page.has_next_page && total_count < members.len() as u64)
        {
            return None;
        }
        let matches = members
            .iter()
            .filter(|member| g0_json_u64(member, &["id"]) == Some(member_id))
            .collect::<Vec<_>>();
        (matches.len() == 1).then(|| matches[0].clone())
    }
}

fn g0_api_path_is(value: &Value, path: &str) -> bool {
    value
        .as_str()
        .and_then(g0_api_url)
        .is_some_and(|url| url.query().is_none() && url.path() == path)
}

fn g0_check_raw_evidence_valid(
    repository: &str,
    check: &G0CheckProducer,
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
) -> bool {
    let check_run_endpoints = [
        format!("/repos/{repository}/check-runs/{}", check.check_run_id),
        format!(
            "/repos/{repository}/commits/{}/check-runs",
            check.source_sha
        ),
    ];
    let suite_endpoints = [
        format!("/repos/{repository}/check-suites/{}", check.check_suite_id),
        format!(
            "/repos/{repository}/commits/{}/check-suites",
            check.source_sha
        ),
    ];
    let app_endpoint = format!("/apps/{}", check.app_slug);
    let app_id = check.app_id.parse::<u64>().ok();
    let check_run = g0_capture_raw_json(
        &check.raw_object_refs,
        "check_run",
        check.check_run_id,
        std::slice::from_ref(&check_run_endpoints[0]),
        requests,
        raw_objects,
    )
    .or_else(|| {
        g0_capture_raw_json(
            &check.raw_object_refs,
            "check_runs",
            check.check_run_id,
            std::slice::from_ref(&check_run_endpoints[1]),
            requests,
            raw_objects,
        )
    });
    let check_suite = g0_capture_raw_json(
        &check.raw_object_refs,
        "check_suite",
        check.check_suite_id,
        std::slice::from_ref(&suite_endpoints[0]),
        requests,
        raw_objects,
    )
    .or_else(|| {
        g0_capture_raw_json(
            &check.raw_object_refs,
            "check_suites",
            check.check_suite_id,
            std::slice::from_ref(&suite_endpoints[1]),
            requests,
            raw_objects,
        )
    });
    let app = g0_capture_raw_json(
        &check.raw_object_refs,
        "app",
        app_id.unwrap_or_default(),
        &[app_endpoint],
        requests,
        raw_objects,
    );
    let common = app_id.is_some_and(|app_id| {
        check_run
            .as_ref()
            .is_some_and(|value| g0_check_run_identity_valid(value, check, app_id))
            && check_suite.as_ref().is_some_and(|value| {
                g0_json_u64(value, &["id"]) == Some(check.check_suite_id)
                    && g0_json_string(value, &["head_sha"]) == Some(check.source_sha.as_str())
                    && g0_json_string(value, &["status"]) == Some(check.status.as_str())
                    && g0_json_string(value, &["conclusion"]) == Some(check.conclusion.as_str())
                    && g0_json_u64(value, &["app", "id"]) == Some(app_id)
                    && g0_json_string(value, &["app", "slug"]) == Some(check.app_slug.as_str())
            })
            && app.as_ref().is_some_and(|value| {
                g0_json_u64(value, &["id"]) == Some(app_id)
                    && g0_json_string(value, &["slug"]) == Some(check.app_slug.as_str())
            })
    });
    if !common {
        return false;
    }
    let has_actions_raw_objects = check.raw_object_refs.iter().any(|raw_id| {
        raw_objects.iter().any(|raw| {
            &raw.raw_id == raw_id
                && matches!(
                    raw.object_kind.as_str(),
                    "workflow_run" | "workflow_jobs" | "workflow_attempt_jobs"
                )
        })
    });
    match &check.provider {
        G0CheckProvider::ExternalApp => !has_actions_raw_objects,
        G0CheckProvider::GithubActions {
            workflow_run_id,
            run_attempt,
            job_id,
            job_run_id,
            job_run_attempt,
            job_check_run_id,
            job_source_sha,
            job_html_url,
            ..
        } => {
            let run = g0_capture_raw_json(
                &check.raw_object_refs,
                "workflow_run",
                *workflow_run_id,
                &[format!(
                    "/repos/{repository}/actions/runs/{workflow_run_id}"
                )],
                requests,
                raw_objects,
            );
            let job = g0_capture_raw_json(
                &check.raw_object_refs,
                "workflow_attempt_jobs",
                *job_id,
                &[format!(
                    "/repos/{repository}/actions/runs/{workflow_run_id}/attempts/{run_attempt}/jobs"
                )],
                requests,
                raw_objects,
            );
            run.as_ref().is_some_and(|value| {
                g0_json_u64(value, &["id"]) == Some(*workflow_run_id)
                    && g0_json_u64(value, &["run_attempt"]) == Some(u64::from(*run_attempt))
                    && g0_json_string(value, &["head_sha"]) == Some(job_source_sha.as_str())
                    && g0_json_string(value, &["event"]) == Some(check.event.as_str())
                    && g0_json_string(value, &["status"]) == Some(check.status.as_str())
                    && g0_json_string(value, &["conclusion"]) == Some(check.conclusion.as_str())
            }) && job.as_ref().is_some_and(|value| {
                g0_json_u64(value, &["id"]) == Some(*job_id)
                    && g0_json_u64(value, &["run_id"]) == Some(*job_run_id)
                    && g0_json_u64(value, &["run_attempt"]) == Some(u64::from(*job_run_attempt))
                    && g0_json_string(value, &["head_sha"]) == Some(job_source_sha.as_str())
                    && g0_json_string(value, &["html_url"]) == Some(job_html_url.as_str())
                    && g0_api_path_is(
                        &value["check_run_url"],
                        &format!("/repos/{repository}/check-runs/{job_check_run_id}"),
                    )
            })
        }
    }
}

fn check_g0_check_raw_evidence(
    repository: &str,
    checks: &[G0CheckProducer],
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    for check in checks {
        if !g0_check_raw_evidence_valid(repository, check, requests, raw_objects) {
            finding(
                findings,
                "g0-check-raw-evidence",
                repository,
                "check_producers.raw_object_refs",
                "check producer must bind independently captured check-run, check-suite, App, source, status, conclusion, html_url, and provider-specific run/job objects",
            );
        }
    }
}

fn check_g0_check_producers(
    repository: &str,
    checks: &[G0CheckProducer],
    expectation: G0CheckExpectation<'_>,
    findings: &mut Vec<Finding>,
) {
    let mut seen = BTreeSet::new();
    let mut check_run_ids = BTreeSet::new();
    let mut producer_jobs = BTreeSet::new();
    for check in checks {
        let key = (check.context.clone(), check.app_id.clone());
        let job_key = g0_check_job_key(check);
        if !seen.insert(key.clone())
            || !check_run_ids.insert(check.check_run_id)
            || job_key.is_some_and(|key| !producer_jobs.insert(key))
        {
            finding(
                findings,
                "g0-check-duplicate",
                repository,
                "check_producers.context/app_id",
                "check producer context/app identity must be unique per source",
            );
        }
        if check.context.trim().is_empty()
            || check.app_id.trim().is_empty()
            || !g0_valid_app_id(&check.app_id)
            || check.api != G0ApiKind::Rest
            || check.check_suite_id == 0
            || check.check_run_id == 0
            || !valid_sha(&check.source_sha)
            || check.source_sha != expectation.source_sha
            || g0_actions_checkout_sha(check).is_some_and(|checkout_sha| {
                !valid_sha(checkout_sha) || checkout_sha != expectation.checkout_sha
            })
            || check.event != expectation.event
            || check.status != "completed"
            || check.conclusion != "success"
            || !g0_check_provider_valid(check, repository)
        {
            finding(
                findings,
                "g0-check-producer",
                repository,
                "check_producers",
            "check producer must bind captured App/provider, provider-specific identities, current source SHA, event, successful status/conclusion, and provider html_url",
        );
        }
        if !expectation.contexts.contains(&key) {
            finding(
                findings,
                "g0-check-policy",
                repository,
                "check_producers.context/app_id",
                "observed check producer is not in the reviewed required context/app policy",
            );
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.check_producer.raw_object_refs",
            &check.raw_object_refs,
            expectation.raw_ids,
            findings,
        );
    }
    for required in expectation.contexts {
        if !checks
            .iter()
            .any(|check| check.context == required.0 && check.app_id == required.1)
        {
            finding(
                findings,
                "g0-check-coverage",
                repository,
                "check_producers",
                format!(
                    "missing required check producer {}/{}",
                    required.0, required.1
                ),
            );
        }
    }
}

fn check_g0_main_checks_against_snapshot(
    repository: &str,
    checks: &[G0CheckProducer],
    snapshot: &SnapshotRepository,
    findings: &mut Vec<Finding>,
) {
    for check in checks {
        let matched = match &check.provider {
            G0CheckProvider::GithubActions {
                workflow_run_id,
                run_attempt,
                job_id,
                actual_checkout_sha,
                ..
            } => snapshot.main_executions.iter().any(|execution| {
                execution.run_id == *workflow_run_id
                    && execution.run_attempt == *run_attempt
                    && execution.trigger_source_sha == check.source_sha
                    && execution.actual_checkout_sha == *actual_checkout_sha
                    && execution.event == check.event
                    && execution.required_checks.iter().any(|observed| {
                        observed.context == check.context
                            && observed.app_id == check.app_id
                            && observed.run_id == *workflow_run_id
                            && observed.job_id == job_id.to_string()
                            && observed.event == check.event
                            && observed.status == check.status
                            && observed.conclusion == check.conclusion
                            && observed.source_url == check.html_url
                    })
            }),
            G0CheckProvider::ExternalApp => snapshot.main_executions.iter().any(|execution| {
                execution.trigger_source_sha == check.source_sha
                    && execution.event == check.event
                    && execution.required_checks.iter().any(|observed| {
                        observed.context == check.context
                            && observed.app_id == check.app_id
                            && observed.event == check.event
                            && observed.status == check.status
                            && observed.conclusion == check.conclusion
                            && observed.source_url == check.html_url
                    })
            }),
        };
        if !matched {
            finding(
                findings,
                "g0-check-snapshot-mismatch",
                repository,
                "repositories.main_checks",
                "collector check producer does not match an independently observed main check tuple",
            );
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "PR validation keeps snapshot, source, request, and raw bindings explicit"
)]
fn check_g0_pull_request(
    repository: &str,
    pr: &G0PullRequestInventory,
    expected_contexts: &BTreeSet<(String, String)>,
    snapshot: &SnapshotDocument,
    workflows: &[G0WorkflowInventory],
    raw_ids: &BTreeSet<String>,
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    if pr.number == 0
        || pr.state != "open"
        || pr.author.trim().is_empty()
        || pr.author_association.trim().is_empty()
        || !g0_valid_applicability(&pr.applicability)
        || pr.head_repository.trim().is_empty()
        || !valid_sha(&pr.head_sha)
        || !valid_sha(&pr.base_sha)
        || !valid_sha(&pr.tested_merge_sha)
        || !g0_repo_source_url(repository, &pr.source_url)
    {
        finding(
            findings,
            "g0-pr-identity",
            repository,
            "open_prs",
            "PR inventory requires open identity, author/trust fields, head/base/tested-merge SHAs, and applicability",
        );
    }
    validate_repository_name(&pr.head_repository, "open_prs.head_repository", findings);
    if let Some(merge_group) = &pr.merge_group_sha {
        validate_sha(
            repository,
            "open_prs.merge_group_sha",
            merge_group,
            findings,
        );
    }
    if !matches!(pr.trust.state.as_str(), "trusted" | "untrusted")
        || pr.trust.reason.trim().is_empty()
    {
        finding(
            findings,
            "g0-pr-trust",
            repository,
            "open_prs.trust",
            "PR trust must be an explicit observed trusted/untrusted state with reason",
        );
    }
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.open_pr.raw_object_refs",
        &pr.raw_object_refs,
        raw_ids,
        findings,
    );
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.open_pr.trust.raw_object_refs",
        &pr.trust.raw_object_refs,
        raw_ids,
        findings,
    );
    let mut binding_run_ids = BTreeSet::new();
    let requires_workflow_binding = pr
        .required_check_producers
        .iter()
        .any(|check| matches!(&check.provider, G0CheckProvider::GithubActions { .. }));
    if requires_workflow_binding && pr.workflow_bindings.is_empty() {
        finding(
            findings,
            "g0-pr-workflow-binding",
            repository,
            "open_prs.workflow_bindings",
            "PR check evidence requires independently observed workflow event/run bindings",
        );
    }
    for binding in &pr.workflow_bindings {
        if binding.workflow_path.trim().is_empty()
            || !valid_sha(&binding.workflow_revision)
            || binding.event != "pull_request"
            || !valid_sha(&binding.source_sha)
            || binding.source_sha != pr.head_sha
            || !valid_sha(&binding.actual_checkout_sha)
            || binding.actual_checkout_sha != pr.tested_merge_sha
            || binding.run_ids.is_empty()
        {
            finding(
                findings,
                "g0-pr-workflow-binding",
                repository,
                "open_prs.workflow_bindings",
                "workflow binding must identify immutable workflow/source/event and run IDs",
            );
        }
        for run_id in &binding.run_ids {
            if *run_id == 0 || !binding_run_ids.insert(*run_id) {
                finding(
                    findings,
                    "g0-pr-workflow-binding",
                    repository,
                    "open_prs.workflow_bindings.run_ids",
                    "workflow run IDs must be positive and unique per PR",
                );
            }
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.workflow_binding.raw_object_refs",
            &binding.raw_object_refs,
            raw_ids,
            findings,
        );
        let source_bound = workflows.iter().any(|workflow| {
            workflow.source.path == binding.workflow_path
                && workflow.source.revision == binding.workflow_revision
                && source_has_raw_binding(
                    &workflow.source,
                    requests,
                    raw_objects,
                    "workflow.source",
                )
        });
        if !source_bound {
            finding(
                findings,
                "g0-pr-workflow-binding",
                repository,
                "open_prs.workflow_bindings.workflow_path/workflow_revision",
                "PR workflow binding must resolve to an immutable captured workflow source",
            );
        }
    }
    if pr.required_check_producers.is_empty() {
        finding(
            findings,
            "g0-pr-check-inventory",
            repository,
            "open_prs.required_check_producers",
            "every current PR requires a complete required-check producer inventory",
        );
    }
    let mut seen_checks = BTreeSet::new();
    let mut check_run_ids = BTreeSet::new();
    let mut producer_jobs = BTreeSet::new();
    for check in &pr.required_check_producers {
        let key = (check.context.clone(), check.app_id.clone());
        let job_key = g0_check_job_key(check);
        if !seen_checks.insert(key.clone())
            || !check_run_ids.insert(check.check_run_id)
            || job_key.is_some_and(|key| !producer_jobs.insert(key))
            || !expected_contexts.contains(&key)
        {
            finding(
                findings,
                "g0-pr-check-policy",
                repository,
                "open_prs.required_check_producers",
                "PR check producer must uniquely match the reviewed required context/app policy",
            );
        }
        if check.check_suite_id == 0
            || check.check_run_id == 0
            || check.api != G0ApiKind::Rest
            || !g0_valid_app_id(&check.app_id)
            || !valid_sha(&check.source_sha)
            || check.source_sha != pr.head_sha
            || g0_actions_checkout_sha(check).is_some_and(|checkout_sha| {
                !valid_sha(checkout_sha) || checkout_sha != pr.tested_merge_sha
            })
            || check.event != "pull_request"
            || check.status != "completed"
            || check.conclusion != "success"
            || !g0_check_provider_valid(check, repository)
            || !match &check.provider {
                G0CheckProvider::GithubActions {
                    workflow_run_id, ..
                } => pr.workflow_bindings.iter().any(|binding| {
                    binding.run_ids.contains(workflow_run_id)
                        && binding.event == check.event
                        && binding.source_sha == check.source_sha
                        && g0_actions_checkout_sha(check)
                            .is_some_and(|checkout_sha| binding.actual_checkout_sha == checkout_sha)
                }),
                G0CheckProvider::ExternalApp => true,
            }
        {
            finding(
                findings,
                "g0-pr-check-producer",
                repository,
                "open_prs.required_check_producers",
            "PR check producer must bind successful API status, source/event, provider-specific identities, html_url, and App identity",
        );
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.pr_check.raw_object_refs",
            &check.raw_object_refs,
            raw_ids,
            findings,
        );
    }
    for required in expected_contexts {
        if !pr
            .required_check_producers
            .iter()
            .any(|check| check.context == required.0 && check.app_id == required.1)
        {
            finding(
                findings,
                "g0-pr-check-coverage",
                repository,
                "open_prs.required_check_producers",
                format!(
                    "PR is missing required check producer {}/{}",
                    required.0, required.1
                ),
            );
        }
    }
    for snapshot_repo in &snapshot.repositories {
        if snapshot_repo.repository != repository {
            continue;
        }
        if let Some(snapshot_pr) = snapshot_repo
            .open_prs
            .iter()
            .find(|candidate| candidate.number == pr.number)
        {
            let snapshot_run_ids = snapshot_pr
                .executions
                .iter()
                .map(|execution| execution.run_id)
                .collect::<BTreeSet<_>>();
            if snapshot_run_ids != binding_run_ids {
                finding(
                    findings,
                    "g0-pr-workflow-binding",
                    repository,
                    "open_prs.workflow_bindings.run_ids",
                    "PR workflow bindings must cover the complete independently observed execution set",
                );
            }
            for binding in &pr.workflow_bindings {
                for run_id in &binding.run_ids {
                    let matched = snapshot_pr.executions.iter().any(|execution| {
                        execution.run_id == *run_id
                            && execution.workflow_path == binding.workflow_path
                            && execution.workflow_revision == binding.workflow_revision
                            && execution.event == binding.event
                            && execution.trigger_source_sha == binding.source_sha
                            && execution.actual_checkout_sha == binding.actual_checkout_sha
                    });
                    if !matched {
                        finding(
                            findings,
                            "g0-pr-workflow-snapshot-mismatch",
                            repository,
                            "open_prs.workflow_bindings",
                            "PR workflow binding does not match an independently observed execution identity",
                        );
                    }
                }
            }
            if snapshot_pr.state != pr.state
                || snapshot_pr.draft != pr.draft
                || snapshot_pr.author != pr.author
                || snapshot_pr.author_association != pr.author_association
                || snapshot_pr.head_repository != pr.head_repository
                || snapshot_pr.source_url != pr.source_url
                || snapshot_pr.head_sha != pr.head_sha
                || snapshot_pr.base_sha != pr.base_sha
                || snapshot_pr.merge_sha.as_deref() != Some(pr.tested_merge_sha.as_str())
                || snapshot_pr.merge_group_sha != pr.merge_group_sha
            {
                finding(
                    findings,
                    "g0-pr-snapshot-mismatch",
                    repository,
                    "open_prs.identity",
                    "typed PR identity differs from the independently captured snapshot",
                );
            }
            for check in &pr.required_check_producers {
                let matched = match &check.provider {
                    G0CheckProvider::GithubActions {
                        workflow_run_id,
                        run_attempt,
                        job_id,
                        actual_checkout_sha,
                        ..
                    } => snapshot_pr.executions.iter().any(|execution| {
                        execution.run_id == *workflow_run_id
                            && execution.run_attempt == *run_attempt
                            && execution.event == check.event
                            && execution.trigger_source_sha == check.source_sha
                            && execution.actual_checkout_sha == *actual_checkout_sha
                            && execution.required_checks.iter().any(|observed| {
                                observed.context == check.context
                                    && observed.app_id == check.app_id
                                    && observed.job_id == job_id.to_string()
                                    && observed.run_id == *workflow_run_id
                                    && observed.status == check.status
                                    && observed.conclusion == check.conclusion
                                    && observed.source_url == check.html_url
                                    && observed.event == check.event
                            })
                    }),
                    G0CheckProvider::ExternalApp => {
                        snapshot_pr.executions.iter().any(|execution| {
                            execution.trigger_source_sha == check.source_sha
                                && execution.event == check.event
                                && execution.required_checks.iter().any(|observed| {
                                    observed.context == check.context
                                        && observed.app_id == check.app_id
                                        && observed.status == check.status
                                        && observed.conclusion == check.conclusion
                                        && observed.source_url == check.html_url
                                        && observed.event == check.event
                                })
                        })
                    }
                };
                if !matched {
                    finding(
                        findings,
                        "g0-pr-check-snapshot-mismatch",
                        repository,
                        "open_prs.required_check_producers",
                        "PR check producer does not match an independently observed provider check tuple",
                    );
                }
            }
        }
    }
}

fn check_g0_reconciliation(
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    collector: &G0CollectorSnapshot,
    raw_ids: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    if collector.reconciliation.pre_state.len() != REQUIRED_REPOSITORIES
        || collector.reconciliation.post_state.len() != REQUIRED_REPOSITORIES
        || !collector.reconciliation.changed_refs.is_empty()
        || !collector.reconciliation.invalidated.is_empty()
    {
        finding(
            findings,
            "g0-reconciliation",
            "",
            "collector_snapshot.reconciliation",
            "G0 requires complete unchanged pre/post repository revision reconciliation",
        );
    }
    let before = g0_revision_map(
        &collector.reconciliation.pre_state,
        raw_ids,
        findings,
        "pre_state",
    );
    let after = g0_revision_map(
        &collector.reconciliation.post_state,
        raw_ids,
        findings,
        "post_state",
    );
    let expected = canonical_scope();
    let before_scope = before.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let after_scope = after.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if before_scope != expected || after_scope != expected {
        finding(
            findings,
            "g0-reconciliation",
            "",
            "collector_snapshot.reconciliation.pre_state/post_state",
            "reconciliation must cover the fixed canonical 32 repositories exactly",
        );
    }
    let current = g0_current_revision_map(manifest, snapshot, collector, findings);
    if before != after || before != current {
        finding(
            findings,
            "g0-reconciliation",
            "",
            "collector_snapshot.reconciliation.pre_state/post_state",
            "reconciliation must match the current collector repository/PR identities and independent snapshot",
        );
    }
}

fn g0_current_revision_map(
    manifest: &ManifestDocument,
    snapshot: &SnapshotDocument,
    collector: &G0CollectorSnapshot,
    findings: &mut Vec<Finding>,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for repo in &collector.repositories {
        let prs = repo
            .open_prs
            .iter()
            .map(|pr| {
                (
                    pr.number,
                    (
                        pr.head_sha.clone(),
                        pr.base_sha.clone(),
                        pr.tested_merge_sha.clone(),
                        pr.merge_group_sha.clone(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let value =
            serde_json::to_string(&(repo.default_branch_sha.clone(), prs)).unwrap_or_default();
        result.insert(repo.repository.clone(), value);
    }
    for manifest_repo in &manifest.repositories {
        if let Some(repo) = collector
            .repositories
            .iter()
            .find(|repo| repo.repository == manifest_repo.repository)
            && repo.default_branch != manifest_repo.default_branch
        {
            finding(
                findings,
                "g0-reconciliation",
                &manifest_repo.repository,
                "collector_snapshot.repositories.default_branch",
                "reconciliation branch identity differs from the reviewed manifest",
            );
        }
    }
    for snapshot_repo in &snapshot.repositories {
        if let Some(repo) = collector
            .repositories
            .iter()
            .find(|repo| repo.repository == snapshot_repo.repository)
            && repo.default_branch_sha != snapshot_repo.default_branch_sha
        {
            finding(
                findings,
                "g0-reconciliation",
                &snapshot_repo.repository,
                "collector_snapshot.repositories.default_branch_sha",
                "reconciliation branch SHA differs from the independent snapshot",
            );
        }
    }
    result
}

fn g0_source_child_workloads(
    manifest: &ManifestDocument,
    collector: &G0CollectorSnapshot,
) -> BTreeSet<(String, String)> {
    let mut result = BTreeSet::new();
    for repository in &collector.repositories {
        let Some(manifest_repo) = manifest
            .repositories
            .iter()
            .find(|candidate| candidate.repository == repository.repository)
        else {
            continue;
        };
        for workflow in &repository.workflows {
            if workflow.source.path != manifest_repo.workflow_path
                || workflow.source.revision != manifest_repo.workflow_revision
            {
                continue;
            }
            let dependencies = workflow
                .reusable_workflows
                .iter()
                .chain(workflow.actions.iter())
                .chain(workflow.scanners.iter())
                .cloned()
                .collect::<Vec<_>>();
            if let Ok(plan) = derive_workflow_plan(&workflow.source, &dependencies) {
                for edge in plan.child_edges {
                    result.insert((repository.repository.clone(), edge.root_workload_id));
                }
            }
        }
    }
    result
}

fn g0_revision_map(
    revisions: &[G0RepositoryRevision],
    raw_ids: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
    field: &str,
) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for revision in revisions {
        validate_repository_name(&revision.repository, field, findings);
        validate_sha(
            &revision.repository,
            field,
            &revision.default_branch_sha,
            findings,
        );
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.reconciled_repository.raw_object_refs",
            &revision.raw_object_refs,
            raw_ids,
            findings,
        );
        let mut prs = BTreeMap::new();
        for pr in &revision.prs {
            if pr.number == 0
                || !valid_sha(&pr.head_sha)
                || !valid_sha(&pr.base_sha)
                || !valid_sha(&pr.tested_merge_sha)
            {
                finding(
                    findings,
                    "g0-reconciliation",
                    &revision.repository,
                    field,
                    "reconciled PR rows require positive number and immutable head/base/tested-merge SHAs",
                );
            }
            if let Some(merge_group) = &pr.merge_group_sha {
                validate_sha(
                    &revision.repository,
                    "reconciled_pr.merge_group_sha",
                    merge_group,
                    findings,
                );
            }
            if prs
                .insert(
                    pr.number,
                    (
                        pr.head_sha.clone(),
                        pr.base_sha.clone(),
                        pr.tested_merge_sha.clone(),
                        pr.merge_group_sha.clone(),
                    ),
                )
                .is_some()
            {
                finding(
                    findings,
                    "g0-reconciliation",
                    &revision.repository,
                    field,
                    "reconciled PR rows must be unique",
                );
            }
            check_g0_raw_refs(
                "evidence.g0_inventory.collector_snapshot.reconciled_pr.raw_object_refs",
                &pr.raw_object_refs,
                raw_ids,
                findings,
            );
        }
        if result
            .insert(
                revision.repository.clone(),
                serde_json::to_string(&(revision.default_branch_sha.clone(), prs))
                    .unwrap_or_default(),
            )
            .is_some()
        {
            finding(
                findings,
                "g0-reconciliation",
                &revision.repository,
                field,
                "reconciled repositories must be unique",
            );
        }
    }
    result
}

fn check_g0_dependency_graph(
    manifest: &ManifestDocument,
    collector: &G0CollectorSnapshot,
    raw_ids: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    let graph = &collector.dependency_graph;
    if graph.nodes.is_empty() || graph.edges.is_empty() {
        finding(
            findings,
            "g0-dependency-graph",
            "",
            "collector_snapshot.dependency_graph",
            "G0 requires a non-empty source-bound dependency graph with edges",
        );
    }
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.dependency_graph.raw_object_refs",
        &graph.raw_object_refs,
        raw_ids,
        findings,
    );
    if !g0_has_workflow_source_raw(&graph.raw_object_refs, collector) {
        finding(
            findings,
            "g0-dependency-source",
            "",
            "collector_snapshot.dependency_graph.raw_object_refs",
            "dependency graph must bind immutable workflow/dependency source bytes, not only generic raw objects",
        );
    }
    let mut node_ids = BTreeSet::new();
    let mut adjacency = BTreeMap::<String, Vec<String>>::new();
    let mut node_edges = BTreeSet::new();
    let mut edge_ids = BTreeSet::new();
    let derived_child_workloads = g0_source_child_workloads(manifest, collector);
    let allowed_node_kinds = BTreeSet::from([
        "artifact", "check", "child", "package", "release", "source", "workload", "workflow",
    ]);
    let allowed_edge_kinds = BTreeSet::from([
        "consumes",
        "dependency",
        "produces",
        "requires",
        "workload-to-check",
        "workload-to-child",
        "workload-to-package",
        "workload-to-release",
    ]);
    for node in &graph.nodes {
        if node.id.trim().is_empty()
            || !node_ids.insert(node.id.clone())
            || node.kind.trim().is_empty()
            || !allowed_node_kinds.contains(node.kind.as_str())
            || node.workload_id.trim().is_empty()
            || !g0_valid_applicability(&node.applicability)
            || !valid_sha(&node.source_sha)
            || node.source_ref.trim().is_empty()
        {
            finding(
                findings,
                "g0-dependency-node",
                "",
                "collector_snapshot.dependency_graph.nodes",
            "graph nodes require unique typed identity, source revision/ref, repository, workload, and applicability",
            );
        }
        validate_repository_name(
            &node.repository,
            "dependency_graph.node.repository",
            findings,
        );
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.dependency_graph.node.raw_object_refs",
            &node.raw_object_refs,
            raw_ids,
            findings,
        );
        if !g0_graph_node_raw_binding(node, collector) {
            finding(
                findings,
                "g0-dependency-source",
                &node.repository,
                "dependency_graph.nodes.raw_object_refs",
                "every dependency node must bind the raw source appropriate to its typed kind",
            );
        }
    }
    let node_by_id = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    for repo in &manifest.repositories {
        for workload in &repo.expected_workload_ids {
            let required = repo
                .expected_jobs
                .iter()
                .any(|job| job.workload_id == *workload && job.required);
            if !graph.nodes.iter().any(|node| {
                node.repository == repo.repository
                    && node.workload_id == *workload
                    && node.kind == "workload"
                    && (!required || node.applicability == "required")
                    && (g0_graph_source_matches_manifest(node.source_sha.as_str(), repo)
                        || collector.repositories.iter().any(|observed| {
                            observed.repository == repo.repository
                                && observed.default_branch_sha == node.source_sha
                        }))
            }) {
                finding(
                    findings,
                    "g0-dependency-node",
                    &repo.repository,
                    "dependency_graph.nodes",
                    format!("missing graph node for reviewed workload {workload}"),
                );
            }
        }
    }
    for edge in &graph.edges {
        let edge_id = (
            edge.from.clone(),
            edge.to.clone(),
            edge.kind.clone(),
            edge.required,
        );
        let from_node = node_by_id.get(edge.from.as_str());
        let to_node = node_by_id.get(edge.to.as_str());
        if edge.from.trim().is_empty()
            || edge.to.trim().is_empty()
            || edge.from == edge.to
            || edge.kind.trim().is_empty()
            || !allowed_edge_kinds.contains(edge.kind.as_str())
            || !node_ids.contains(&edge.from)
            || !node_ids.contains(&edge.to)
            || !valid_sha(&edge.source_sha)
            || edge.source_ref.trim().is_empty()
            || !valid_sha(&edge.target_source_sha)
            || edge.target_source_ref.trim().is_empty()
            || !edge_ids.insert(edge_id)
            || from_node.is_some_and(|node| {
                node.source_sha != edge.source_sha || node.source_ref != edge.source_ref
            })
            || to_node.is_some_and(|node| {
                node.source_sha != edge.target_source_sha
                    || node.source_ref != edge.target_source_ref
            })
            || !g0_graph_edge_shape_is_bound(edge, from_node, to_node)
        {
            finding(
                findings,
                "g0-dependency-edge",
                "",
                "collector_snapshot.dependency_graph.edges",
                "graph edges require unique distinct existing nodes with independently bound source and target revisions/refs",
            );
        }
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
        node_edges.insert(edge.from.clone());
        node_edges.insert(edge.to.clone());
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.dependency_graph.edge.raw_object_refs",
            &edge.raw_object_refs,
            raw_ids,
            findings,
        );
        if !g0_graph_edge_raw_binding(edge, from_node, to_node, collector) {
            finding(
                findings,
                "g0-dependency-source",
                "",
                "dependency_graph.edges.raw_object_refs",
                "every dependency edge must bind source bytes from the immutable workflow graph",
            );
        }
    }
    for node in graph.nodes.iter().filter(|node| node.kind == "workload") {
        if !node_edges.contains(&node.id) {
            finding(
                findings,
                "g0-dependency-edge",
                &node.repository,
                "dependency_graph.edges",
                format!("workload node {} has no dependency edge", node.id),
            );
        }
    }
    for repo in &manifest.repositories {
        for job in repo.expected_jobs.iter().filter(|job| job.required) {
            let Some(workload) = graph.nodes.iter().find(|node| {
                node.repository == repo.repository
                    && node.workload_id == job.workload_id
                    && node.kind == "workload"
            }) else {
                continue;
            };
            let outgoing = graph
                .edges
                .iter()
                .filter(|edge| edge.from == workload.id && edge.required)
                .collect::<Vec<_>>();
            if !outgoing.iter().any(|edge| {
                edge.kind == "workload-to-check"
                    && node_by_id
                        .get(edge.to.as_str())
                        .is_some_and(|node| node.kind == "check")
            }) {
                finding(
                    findings,
                    "g0-dependency-obligation",
                    &repo.repository,
                    "dependency_graph.edges",
                    format!(
                        "required workload {} lacks a required-check edge",
                        job.workload_id
                    ),
                );
            }
            if (job.child_workflow.is_some()
                || derived_child_workloads
                    .contains(&(repo.repository.clone(), job.workload_id.clone())))
                && !outgoing.iter().any(|edge| {
                    edge.kind == "workload-to-child"
                        && node_by_id
                            .get(edge.to.as_str())
                            .is_some_and(|node| node.kind == "child")
                })
            {
                finding(
                    findings,
                    "g0-dependency-obligation",
                    &repo.repository,
                    "dependency_graph.edges",
                    format!("workload {} lacks its required child edge", job.workload_id),
                );
            }
            if matches!(
                repo.release_applicability,
                Applicability::Required | Applicability::Applicable
            ) && (!outgoing.iter().any(|edge| {
                edge.kind == "workload-to-release"
                    && node_by_id
                        .get(edge.to.as_str())
                        .is_some_and(|node| node.kind == "release")
            }) || !outgoing.iter().any(|edge| {
                edge.kind == "workload-to-package"
                    && node_by_id
                        .get(edge.to.as_str())
                        .is_some_and(|node| node.kind == "package")
            })) {
                finding(
                    findings,
                    "g0-dependency-obligation",
                    &repo.repository,
                    "dependency_graph.edges",
                    format!(
                        "workload {} lacks release/package inventory edges",
                        job.workload_id
                    ),
                );
            }
        }
    }
    if g0_graph_has_cycle(&node_ids, &adjacency) {
        finding(
            findings,
            "g0-dependency-cycle",
            "",
            "collector_snapshot.dependency_graph.edges",
            "dependency graph contains an illegal cycle",
        );
    }
}

fn g0_has_workflow_source_raw(refs: &[String], collector: &G0CollectorSnapshot) -> bool {
    refs.iter().any(|raw_id| {
        collector.raw_objects.iter().any(|raw| {
            raw.raw_id == *raw_id
                && matches!(
                    raw.object_kind.as_str(),
                    "workflow.source" | "workflow.dependency.source"
                )
                && g0_raw_response_bound(raw, collector)
                && g0_raw_source_identity_known(raw, collector)
        })
    })
}

fn g0_graph_node_raw_binding(node: &G0GraphNode, collector: &G0CollectorSnapshot) -> bool {
    // A graph node is a source-derived relationship, not a free-standing
    // repository/API observation.  Ruleset, artifact-page, and other
    // same-repository responses are validated at their typed inventory
    // owners; accepting them here would let a caller clone bytes under a
    // different endpoint and self-attest a graph node without source
    // identity.  Every graph node therefore needs the exact workflow source
    // response (or recursively captured dependency source) for its repo/SHA.
    let kinds: &[&str] = &["workflow.source", "workflow.dependency.source"];
    node.raw_object_refs.iter().any(|raw_id| {
        collector.raw_objects.iter().any(|raw| {
            raw.raw_id == *raw_id
                && kinds.contains(&raw.object_kind.as_str())
                && g0_raw_response_bound(raw, collector)
                && g0_raw_source_identity_matches(
                    raw,
                    &node.repository,
                    &node.source_sha,
                    collector,
                )
        })
    })
}

fn g0_graph_edge_raw_binding(
    edge: &G0GraphEdge,
    from: Option<&&G0GraphNode>,
    to: Option<&&G0GraphNode>,
    collector: &G0CollectorSnapshot,
) -> bool {
    let (Some(from), Some(to)) = (from, to) else {
        return false;
    };
    let binds = |node: &G0GraphNode| {
        node.raw_object_refs.iter().any(|raw_id| {
            edge.raw_object_refs.contains(raw_id)
                && g0_graph_node_raw_binding(
                    &G0GraphNode {
                        raw_object_refs: vec![raw_id.clone()],
                        ..node.clone()
                    },
                    collector,
                )
        })
    };
    binds(from)
        && ((from.repository == to.repository && from.source_sha == to.source_sha) || binds(to))
}

fn g0_raw_response_bound(raw: &G0RawObjectRef, collector: &G0CollectorSnapshot) -> bool {
    collector.requests.iter().any(|request| {
        request.request_id == raw.request_id && request.response_raw_ref == raw.raw_id
    })
}

fn g0_raw_source_identity_known(raw: &G0RawObjectRef, collector: &G0CollectorSnapshot) -> bool {
    collector.repositories.iter().any(|repo| {
        repo.workflows.iter().any(|workflow| {
            (workflow.source.raw_object_refs.contains(&raw.raw_id)
                && source_has_raw_binding(
                    &workflow.source,
                    &collector.requests,
                    &collector.raw_objects,
                    "workflow.source",
                ))
                || workflow
                    .reusable_workflows
                    .iter()
                    .chain(workflow.actions.iter())
                    .chain(workflow.scanners.iter())
                    .any(|dependency| {
                        dependency.source.raw_object_refs.contains(&raw.raw_id)
                            && source_has_raw_binding(
                                &dependency.source,
                                &collector.requests,
                                &collector.raw_objects,
                                "workflow.dependency.source",
                            )
                    })
        })
    })
}

fn g0_raw_source_identity_matches(
    raw: &G0RawObjectRef,
    repository: &str,
    source_sha: &str,
    collector: &G0CollectorSnapshot,
) -> bool {
    collector.repositories.iter().any(|repo| {
        repo.workflows.iter().any(|workflow| {
            (workflow.source.repository == repository
                && workflow.source.source_sha == source_sha
                && workflow.source.raw_object_refs.contains(&raw.raw_id)
                && source_has_raw_binding(
                    &workflow.source,
                    &collector.requests,
                    &collector.raw_objects,
                    "workflow.source",
                ))
                || workflow
                    .reusable_workflows
                    .iter()
                    .chain(workflow.actions.iter())
                    .chain(workflow.scanners.iter())
                    .any(|dependency| {
                        dependency.source.repository == repository
                            && dependency.source.source_sha == source_sha
                            && dependency.source.raw_object_refs.contains(&raw.raw_id)
                            && source_has_raw_binding(
                                &dependency.source,
                                &collector.requests,
                                &collector.raw_objects,
                                "workflow.dependency.source",
                            )
                    })
        })
    })
}

fn g0_graph_edge_shape_is_bound(
    edge: &G0GraphEdge,
    from: Option<&&G0GraphNode>,
    to: Option<&&G0GraphNode>,
) -> bool {
    let (Some(from), Some(to)) = (from, to) else {
        return false;
    };
    match edge.kind.as_str() {
        "workload-to-check" => {
            from.kind == "workload"
                && to.kind == "check"
                && from.repository == to.repository
                && from.workload_id == to.workload_id
        }
        "workload-to-child" => {
            from.kind == "workload"
                && to.kind == "child"
                && from.repository == to.repository
                && from.workload_id == to.workload_id
        }
        "workload-to-package" => {
            from.kind == "workload"
                && to.kind == "package"
                && from.repository == to.repository
                && from.workload_id == to.workload_id
        }
        "workload-to-release" => {
            from.kind == "workload"
                && to.kind == "release"
                && from.repository == to.repository
                && from.workload_id == to.workload_id
        }
        _ => true,
    }
}

fn g0_graph_has_cycle(nodes: &BTreeSet<String>, adjacency: &BTreeMap<String, Vec<String>>) -> bool {
    fn visit(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> bool {
        if visiting.contains(node) {
            return true;
        }
        if !visited.insert(node.to_owned()) {
            return false;
        }
        visiting.insert(node.to_owned());
        if adjacency.get(node).is_some_and(|children| {
            children
                .iter()
                .any(|child| visit(child, adjacency, visiting, visited))
        }) {
            return true;
        }
        visiting.remove(node);
        false
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    nodes
        .iter()
        .any(|node| visit(node, adjacency, &mut visiting, &mut visited))
}

fn g0_graph_source_matches_manifest(source_sha: &str, repo: &ManifestRepository) -> bool {
    [
        repo.generator_revision.as_str(),
        repo.workflow_revision.as_str(),
        repo.runtime_source_sha.as_str(),
    ]
    .contains(&source_sha)
}

fn check_g0_model_session(
    collector: &G0CollectorSnapshot,
    raw_ids: &BTreeSet<String>,
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    let session = &collector.model_session;
    if session.session_id.trim().is_empty()
        || !session.effective
        || session.orchestrator_model != EXPECTED_ORCHESTRATOR_MODEL
        || session.orchestrator_effort != EXPECTED_ORCHESTRATOR_EFFORT
        || session.agents.is_empty()
    {
        finding(
            findings,
            "g0-model-session",
            "",
            "collector_snapshot.model_session",
            "effective runtime model session must record the required orchestrator and agent settings",
        );
    }
    check_g0_raw_refs(
        "evidence.g0_inventory.collector_snapshot.model_session.raw_object_refs",
        &session.raw_object_refs,
        raw_ids,
        findings,
    );
    if !g0_typed_raw_binding(
        &session.raw_object_refs,
        raw_objects,
        "model.session",
        session,
    ) {
        finding(
            findings,
            "g0-model-session",
            "",
            "collector_snapshot.model_session.raw_object_refs",
            "effective runtime model settings must be parsed from a collector-owned model.session raw object",
        );
    }
    let mut ids = BTreeSet::new();
    for agent in &session.agents {
        if agent.agent_id.trim().is_empty()
            || !ids.insert(agent.agent_id.clone())
            || !agent.effective
            || agent.model != EXPECTED_AGENT_MODEL
            || agent.effort != EXPECTED_AGENT_EFFORT
        {
            finding(
                findings,
                "g0-model-session",
                "",
                "collector_snapshot.model_session.agents",
                "every agent must record unique effective Luna/max runtime settings",
            );
        }
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.agent_model.raw_object_refs",
            &agent.raw_object_refs,
            raw_ids,
            findings,
        );
        if !agent.raw_object_refs.iter().any(|raw_id| {
            session
                .raw_object_refs
                .iter()
                .any(|session_raw_id| raw_id == session_raw_id)
        }) {
            finding(
                findings,
                "g0-model-session",
                "",
                "collector_snapshot.model_session.agents.raw_object_refs",
                "each effective agent setting must remain bound to the captured model.session object",
            );
        }
    }
}

fn check_g0_access(
    collector: &G0CollectorSnapshot,
    raw_ids: &BTreeSet<String>,
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
    findings: &mut Vec<Finding>,
) {
    let expected = canonical_scope();
    let actual = collector
        .access
        .iter()
        .map(|access| access.repository.as_str())
        .collect::<BTreeSet<_>>();
    if collector.access.len() != REQUIRED_REPOSITORIES || actual != expected {
        finding(
            findings,
            "g0-access",
            "",
            "collector_snapshot.access",
            "access observations must cover the fixed canonical 32 repositories exactly",
        );
    }
    let mut seen = BTreeSet::new();
    for access in &collector.access {
        if !seen.insert(access.repository.clone())
            || !expected.contains(access.repository.as_str())
            || access.state != "complete"
            || access.scopes.is_empty()
            || !access.gaps.is_empty()
        {
            finding(
                findings,
                "g0-access",
                &access.repository,
                "collector_snapshot.access",
                "access must be complete, scoped, gap-free, and in the fixed repository set",
            );
        }
        validate_repository_name(
            &access.repository,
            "collector_snapshot.access.repository",
            findings,
        );
        check_g0_raw_refs(
            "evidence.g0_inventory.collector_snapshot.access.raw_object_refs",
            &access.raw_object_refs,
            raw_ids,
            findings,
        );
        if !g0_access_raw_binding(
            &access.repository,
            &access.raw_object_refs,
            requests,
            raw_objects,
        ) {
            finding(
                findings,
                "g0-access",
                &access.repository,
                "collector_snapshot.access.raw_object_refs",
                "access must bind the repository's successful /repos/{owner}/{repo} API response",
            );
        }
    }
}

fn g0_access_raw_binding(
    repository: &str,
    refs: &[String],
    requests: &[G0RequestRecord],
    raw_objects: &[G0RawObjectRef],
) -> bool {
    let endpoint = format!("/repos/{repository}");
    refs.iter().any(|raw_id| {
        raw_objects.iter().any(|raw| {
            raw.raw_id == *raw_id
                && raw.object_kind == "repository"
                && requests.iter().any(|request| {
                    request.request_id == raw.request_id
                        && request.response_raw_ref == raw.raw_id
                        && request.api == G0ApiKind::Rest
                        && request.method == "GET"
                        && request.endpoint_or_operation == endpoint
                        && request.http_status == 200
                        && request.complete
                        && request.state == G0RequestState::Complete
                        && BASE64
                            .decode(&request.query_base64)
                            .is_ok_and(|query| query.is_empty())
                })
        })
    })
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
        if child.parent_run_id == 0
            || child.parent_run_id == child.run_id
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
            let has_main = snapshot_repo.is_some_and(|snapshot_repo| {
                records.values().any(|record| {
                    record.repository == *name
                        && record.provider == provider
                        && record.evidence_role == EvidenceRole::DefaultBranch
                        && record.event == "push"
                        && record.pr_number.is_none()
                        && record.default_branch_sha == snapshot_repo.default_branch_sha
                        && record.trigger_source_sha == snapshot_repo.default_branch_sha
                        && record.actual_checkout_sha == snapshot_repo.default_branch_sha
                })
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
                    let has_pr = records.values().any(|record| {
                        record.repository == *name
                            && record.provider == provider
                            && record_matches_pr_subject(record, pr)
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

fn record_matches_pr_subject(record: &EvidenceRecord, pr: &SnapshotPullRequest) -> bool {
    if record.pr_number != Some(pr.number)
        || record.pr_head_sha.as_deref() != Some(pr.head_sha.as_str())
        || record.pr_base_sha.as_deref() != Some(pr.base_sha.as_str())
    {
        return false;
    }
    match record.evidence_role {
        EvidenceRole::PullRequest => {
            record.event == "pull_request"
                && pr.merge_sha.is_some()
                && record.tested_merge_sha.as_deref() == pr.merge_sha.as_deref()
                && record.merge_group_sha.is_none()
        }
        EvidenceRole::MergeGroup => {
            record.event == "merge_group"
                && pr.merge_group_sha.is_some()
                && record.merge_group_sha.as_deref() == pr.merge_group_sha.as_deref()
                && record.tested_merge_sha.is_none()
        }
        EvidenceRole::Inventory | EvidenceRole::DefaultBranch => false,
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
        let github = records
            .values()
            .find(|record| record.provider == "github" && same_qualifying_subject(velnor, record));
        let Some(github) = github else {
            finding(
                findings,
                "lane-association",
                &key.repository,
                "evidence_role/pr/source/run",
                "G6/G7 requires GitHub and Velnor records for the same immutable candidate or resulting-main subject",
            );
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

fn same_qualifying_subject(left: &EvidenceRecord, right: &EvidenceRecord) -> bool {
    left.repository == right.repository
        && left.evidence_role == right.evidence_role
        && left.event == right.event
        && left.pr_number == right.pr_number
        && left.pr_head_sha == right.pr_head_sha
        && left.pr_base_sha == right.pr_base_sha
        && left.tested_merge_sha == right.tested_merge_sha
        && left.merge_group_sha == right.merge_group_sha
        && left.trigger_source_sha == right.trigger_source_sha
        && left.actual_checkout_sha == right.actual_checkout_sha
        && left.run_id > 0
        && right.run_id > 0
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
    g0_inventory: Option<&G0InventoryEvidence>,
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
    check_eligibility(repo, &record.provider_eligibility, findings);
    check_record_completion(stage, record, findings);
    check_record_subject(stage, record, findings);
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
        check_execution_record(stage, index, record, g0_inventory, findings);
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
}

fn check_record_subject(stage: Stage, record: &EvidenceRecord, findings: &mut Vec<Finding>) {
    let repo = &record.repository;
    for (field, value) in [
        ("pr_head_sha", record.pr_head_sha.as_deref()),
        ("pr_base_sha", record.pr_base_sha.as_deref()),
        ("tested_merge_sha", record.tested_merge_sha.as_deref()),
        ("merge_group_sha", record.merge_group_sha.as_deref()),
    ] {
        if let Some(value) = value {
            validate_sha(repo, field, value, findings);
        }
    }
    if stage == Stage::G0 {
        if record.evidence_role != EvidenceRole::Inventory {
            finding(
                findings,
                "record-role",
                repo,
                "evidence_role",
                "G0 records must use the inventory role",
            );
        }
        return;
    }
    match record.evidence_role {
        EvidenceRole::Inventory => finding(
            findings,
            "record-role",
            repo,
            "evidence_role",
            "execution records cannot use the inventory role",
        ),
        EvidenceRole::DefaultBranch => {
            if record.event != "push"
                || record.pr_number.is_some()
                || record.pr_head_sha.is_some()
                || record.pr_base_sha.is_some()
                || record.tested_merge_sha.is_some()
                || record.merge_group_sha.is_some()
            {
                finding(
                    findings,
                    "record-subject",
                    repo,
                    "evidence_role/event/PR identity",
                    "default-branch evidence must be a push with no PR or merge-group identity",
                );
            }
        }
        EvidenceRole::PullRequest => {
            if record.event != "pull_request"
                || record.pr_number.is_none()
                || record.pr_head_sha.is_none()
                || record.pr_base_sha.is_none()
                || record.tested_merge_sha.is_none()
                || record.merge_group_sha.is_some()
            {
                finding(
                    findings,
                    "record-subject",
                    repo,
                    "evidence_role/event/PR identity",
                    "pull-request evidence must bind PR head/base and a tested merge SHA",
                );
            }
        }
        EvidenceRole::MergeGroup => {
            if record.event != "merge_group"
                || record.pr_number.is_none()
                || record.pr_head_sha.is_none()
                || record.pr_base_sha.is_none()
                || record.merge_group_sha.is_none()
                || record.tested_merge_sha.is_some()
            {
                finding(
                    findings,
                    "record-subject",
                    repo,
                    "evidence_role/event/merge_group_sha",
                    "merge-group evidence must bind PR identity and a merge-group SHA",
                );
            }
        }
    }
}

fn check_record_completion(stage: Stage, record: &EvidenceRecord, findings: &mut Vec<Finding>) {
    // These fields are report metadata, never authorization input. Validate
    // them before the G0 inventory branch so that its early return cannot
    // hide an unresolved blocker or next action.
    if stage != Stage::G0 && record.gate_status != "pass" {
        finding(
            findings,
            "gate-status",
            &record.repository,
            "gate_status",
            "execution record must declare pass only after independently verified facts",
        );
    }
    if record.blocker.is_some() || record.next_action.is_some() {
        finding(
            findings,
            "unfinished-record",
            &record.repository,
            "blocker/next_action",
            "a record with an unresolved blocker or next action cannot pass",
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
    g0_inventory: Option<&G0InventoryEvidence>,
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
    check_authoritative_children_from_source(
        index.manifest,
        g0_inventory,
        record,
        execution,
        findings,
    );
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
            .and_then(|pr| {
                pr.executions.iter().find(|run| {
                    run.run_id == record.run_id && run.run_attempt == record.run_attempt
                })
            })
    } else {
        snapshot
            .main_executions
            .iter()
            .find(|run| run.run_id == record.run_id && run.run_attempt == record.run_attempt)
    }
}

fn check_source_semantics(
    snapshot: &SnapshotRepository,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    match record.evidence_role {
        EvidenceRole::DefaultBranch => {
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
        }
        EvidenceRole::PullRequest | EvidenceRole::MergeGroup => {
            let Some(number) = record.pr_number else {
                finding(
                    findings,
                    "missing-pr",
                    repo,
                    "pr_number",
                    "PR execution role requires a positive PR identity",
                );
                return;
            };
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
            match record.evidence_role {
                EvidenceRole::PullRequest => {
                    if execution.event != "pull_request" {
                        finding(
                            findings,
                            "event-source",
                            repo,
                            "event",
                            "pull-request evidence must use pull_request",
                        );
                    }
                    if execution.trigger_source_sha != pr.head_sha {
                        finding(
                            findings,
                            "source-mismatch",
                            repo,
                            "trigger_source_sha",
                            "pull-request execution must bind contributor head SHA",
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
                            "pull-request execution must bind the current synthetic merge candidate",
                        );
                    }
                }
                EvidenceRole::MergeGroup => {
                    if execution.event != "merge_group" {
                        finding(
                            findings,
                            "event-source",
                            repo,
                            "event",
                            "merge-group evidence must use merge_group",
                        );
                    }
                    let Some(merge_group_sha) = pr.merge_group_sha.as_deref() else {
                        finding(
                            findings,
                            "missing-merge-group",
                            repo,
                            "open_prs.merge_group_sha",
                            "authoritative PR snapshot has no merge-group candidate",
                        );
                        return;
                    };
                    if record.merge_group_sha.as_deref() != Some(merge_group_sha)
                        || execution.trigger_source_sha != merge_group_sha
                        || execution.actual_checkout_sha != merge_group_sha
                    {
                        finding(
                            findings,
                            "source-mismatch",
                            repo,
                            "merge_group_sha/actual_checkout_sha",
                            "merge-group execution must bind the current immutable merge-group SHA",
                        );
                    }
                }
                EvidenceRole::Inventory | EvidenceRole::DefaultBranch => finding(
                    findings,
                    "record-role",
                    repo,
                    "evidence_role",
                    "PR source validation received a non-PR evidence role",
                ),
            }
        }
        EvidenceRole::Inventory => {
            finding(
                findings,
                "record-role",
                repo,
                "evidence_role",
                "inventory evidence cannot prove an execution source",
            );
        }
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

fn check_authoritative_children_from_source(
    manifest: &ManifestRepository,
    inventory: Option<&G0InventoryEvidence>,
    record: &EvidenceRecord,
    execution: &ExecutionObservation,
    findings: &mut Vec<Finding>,
) {
    let repo = &record.repository;
    let Some(inventory) = inventory else {
        finding(
            findings,
            "authoritative-child-source-missing",
            repo,
            "evidence.g0_inventory",
            "child expectations require the independently captured immutable workflow source",
        );
        return;
    };
    let Some(repository) = inventory
        .collector_snapshot
        .repositories
        .iter()
        .find(|candidate| candidate.repository == record.repository)
    else {
        finding(
            findings,
            "authoritative-child-source-missing",
            repo,
            "evidence.g0_inventory.collector_snapshot.repositories",
            "child expectations require a source inventory row for the executed repository",
        );
        return;
    };
    let Some(workflow) = repository.workflows.iter().find(|workflow| {
        workflow.source.path == manifest.workflow_path
            && workflow.source.revision == manifest.workflow_revision
            && workflow.source.source_sha == repository.default_branch_sha
    }) else {
        finding(
            findings,
            "authoritative-child-source-missing",
            repo,
            "evidence.g0_inventory.collector_snapshot.repositories.workflows",
            "child expectations require the executed workflow source at the observed default-branch SHA",
        );
        return;
    };
    let dependencies = workflow
        .reusable_workflows
        .iter()
        .chain(workflow.actions.iter())
        .chain(workflow.scanners.iter())
        .cloned()
        .collect::<Vec<_>>();
    let plan = match derive_workflow_plan(&workflow.source, &dependencies) {
        Ok(plan) => plan,
        Err(error) => {
            finding(
                findings,
                "authoritative-child-source-invalid",
                repo,
                "evidence.g0_inventory.collector_snapshot.repositories.workflows",
                format!("cannot derive child obligations from immutable workflow source: {error}"),
            );
            return;
        }
    };
    let expected = plan
        .child_edges
        .iter()
        .filter(|edge| {
            manifest.expected_jobs.iter().any(|job| {
                job.job_id == edge.root_workload_id
                    && job.provider == record.provider
                    && job.required
            })
        })
        .collect::<Vec<_>>();
    let mut matched_edges = BTreeSet::new();
    let mut observed_child_ids = BTreeSet::new();
    if expected.len() != execution.child_runs.len()
        || expected.len() != record.child_run_links.len()
    {
        finding(
            findings,
            "child-run-inventory",
            repo,
            "execution.child_runs",
            "child-run graph must exactly cover source-derived recursive workflow obligations",
        );
    }
    for child in &execution.child_runs {
        if !observed_child_ids.insert(child.run_id) {
            finding(
                findings,
                "duplicate-child-run",
                repo,
                "execution.child_runs.run_id",
                format!("duplicate child run {}", child.run_id),
            );
        }
        let matching_edges = expected
            .iter()
            .enumerate()
            .filter(|(_, edge)| {
                edge.repository == child.repository
                    && edge.workflow_path == child.workflow_path
                    && edge.event == child.event
                    && edge.source_sha == child.source_sha
            })
            .collect::<Vec<_>>();
        let parent_matches = matching_edges.iter().any(|(_, edge)| {
            if edge.parent_repository == repository.repository
                && edge.parent_workflow_path == execution.workflow_path
                && (edge.parent_source_sha == execution.workflow_revision
                    || edge.parent_source_sha == execution.trigger_source_sha
                    || edge.parent_source_sha == execution.actual_checkout_sha)
            {
                child.parent_run_id == execution.run_id
            } else {
                execution.child_runs.iter().any(|parent| {
                    parent.run_id == child.parent_run_id
                        && parent.repository == edge.parent_repository
                        && parent.workflow_path == edge.parent_workflow_path
                        && parent.source_sha == edge.parent_source_sha
                })
            }
        });
        let manifest_target_matches = matching_edges.first().is_some_and(|(_, edge)| {
            if edge.workload_id != edge.root_workload_id {
                return true;
            }
            manifest.expected_jobs.iter().any(|job| {
                job.job_id == edge.root_workload_id
                    && job.provider == record.provider
                    && job.required
                    && job.child_workflow.as_ref().is_some_and(|child| {
                        child.repository == edge.repository
                            && child.workflow_path == edge.workflow_path
                            && child.event == edge.event
                    })
            })
        });
        let edge_is_unique = matching_edges.len() == 1
            && parent_matches
            && manifest_target_matches
            && matching_edges
                .first()
                .is_some_and(|(index, _)| matched_edges.insert(*index));
        if !edge_is_unique
            || !parent_matches
            || !manifest_target_matches
            || child.provider != record.provider
            || child.parent_run_id == child.run_id
            || child.run_id == 0
            || child.run_attempt == 0
            || child.status != "completed"
            || child.conclusion != "success"
            || !valid_sha(&child.source_sha)
            || !run_url_matches_repository(&child.source_url, &child.repository, child.run_id)
        {
            finding(
                findings,
                "child-run-mismatch",
                repo,
                "execution.child_runs",
                format!(
                    "child run {} lacks exact parent, source, event, status, or URL binding",
                    child.run_id
                ),
            );
        }
    }
    if matched_edges.len() != expected.len() {
        finding(
            findings,
            "child-run-inventory",
            repo,
            "execution.child_runs",
            "each source-derived child edge must map to exactly one distinct observed run",
        );
    }
    let mut child_run_ids = BTreeSet::new();
    for link in &record.child_run_links {
        if !child_run_ids.insert(link.run_id) {
            finding(
                findings,
                "duplicate-child-run",
                repo,
                "child_run_links.run_id",
                format!("duplicate child run {}", link.run_id),
            );
        }
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
    let actual_child_ids = execution
        .child_runs
        .iter()
        .map(|child| child.run_id)
        .collect::<BTreeSet<_>>();
    if child_run_ids != actual_child_ids {
        finding(
            findings,
            "child-run-inventory",
            repo,
            "child_run_links.run_id",
            "child links must form a complete bijection with authoritative child runs",
        );
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
        if child.workflow_path.trim().is_empty()
            || !matches!(child.event.as_str(), "workflow_call" | "workflow_run")
        {
            finding(
                findings,
                "child-workflow",
                repository,
                "expected_jobs.child_workflow",
                "child workflow must identify a path and workflow_call/workflow_run event",
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
    let expected = BTreeSet::from(["github", "velnor"]);
    let actual = eligibility
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if actual != expected {
        finding(
            findings,
            "provider-keyset",
            repository,
            "provider_eligibility",
            "provider eligibility must contain exactly github and velnor keys",
        );
    }
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
    let Some(value) = value.strip_prefix("sha256:") else {
        return false;
    };
    value.len() == DIGEST_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn digest_equal(left: &str, right: &str) -> bool {
    valid_digest(left) && valid_digest(right) && left == right
}

fn valid_target(value: &str) -> bool {
    matches!(
        value,
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
    use std::io::{Cursor, Read};

    fn real_api_fixture_entry(path: &str) -> Vec<u8> {
        let encoded =
            include_str!("testdata/g0/real-api-fixture-corpus-20260920-080318/corpus.zip.base64")
                .trim();
        let archive_bytes = BASE64.decode(encoded).expect("real fixture archive base64");
        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes))
            .expect("real fixture archive is a zip");
        let mut entry = archive.by_name(path).expect("real fixture entry exists");
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .expect("read real fixture entry");
        bytes
    }

    fn canonical_rest_query(query: &str) -> Vec<u8> {
        if query.is_empty() {
            return Vec::new();
        }
        let mut pairs = query
            .split('&')
            .map(|part| part.split_once('=').expect("fixture query key/value"))
            .collect::<Vec<_>>();
        pairs.sort_by(|left, right| left.0.cmp(right.0));
        let mut bytes = Vec::new();
        for (key, value) in pairs {
            bytes.extend_from_slice(key.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    fn captured_page_request(endpoint: &str, query: &str, items: u32) -> G0RequestRecord {
        let query_bytes = canonical_rest_query(query);
        G0RequestRecord {
            request_id: format!("fixture:{endpoint}"),
            api: G0ApiKind::Rest,
            method: "GET".to_owned(),
            endpoint_or_operation: endpoint.to_owned(),
            query_base64: BASE64.encode(&query_bytes),
            variables_base64: BASE64.encode(b"{}"),
            query_sha256: digest_bytes(&query_bytes),
            variables_sha256: digest_bytes(b"{}"),
            auth_identity_ref: "collector.auth".to_owned(),
            started_at_utc: "2026-09-20T07:53:35Z".to_owned(),
            completed_at_utc: "2026-09-20T07:53:36Z".to_owned(),
            http_status: 200,
            api_request_id: "fixture-api-request".to_owned(),
            rate_limit_ref: "collector.rate_limit".to_owned(),
            page: G0Page {
                number: 1,
                per_page: 100,
                link_next: None,
                cursor_in: None,
                cursor_out: None,
                has_next_page: false,
                items_returned: items,
            },
            response_raw_ref: "fixture-raw".to_owned(),
            error_raw_ref: None,
            state: G0RequestState::Complete,
            complete: true,
            truncation_reason: None,
        }
    }

    fn captured_raw_reference(
        raw_id: &str,
        request_id: &str,
        object_kind: &str,
        bytes: &[u8],
    ) -> G0RawObjectRef {
        let digest = digest_bytes(bytes);
        let storage_ref = format!(
            "sha256://{}",
            digest
                .strip_prefix("sha256:")
                .expect("fixture digest prefix")
        );
        G0RawObjectRef {
            raw_id: raw_id.to_owned(),
            request_id: request_id.to_owned(),
            object_kind: object_kind.to_owned(),
            canonicalization: "raw-json".to_owned(),
            sha256: digest.clone(),
            byte_length: bytes.len() as u64,
            bytes_base64: BASE64.encode(bytes),
            media_type: "application/json".to_owned(),
            storage_ref: storage_ref.clone(),
            original_sha256: digest,
            original_byte_length: bytes.len() as u64,
            original_storage_ref: storage_ref,
        }
    }

    fn sha(seed: char) -> String {
        std::iter::repeat_n(seed, SHA_LENGTH).collect()
    }

    fn digest(seed: char) -> String {
        format!(
            "sha256:{}",
            std::iter::repeat_n(seed, DIGEST_LENGTH).collect::<String>()
        )
    }

    fn minimal_g0_collector() -> G0CollectorSnapshot {
        G0CollectorSnapshot {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            snapshot_id: "snapshot".to_owned(),
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            phase: "G0".to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            collector: G0CollectorIdentity {
                name: "fixture-collector".to_owned(),
                revision: sha('a'),
                mode: "read_only".to_owned(),
                api_base: "https://api.github.com".to_owned(),
                api_versions: vec!["2022-11-28".to_owned()],
            },
            auth: G0AuthIdentity {
                provider: "github".to_owned(),
                viewer_id: "viewer-id".to_owned(),
                viewer_login: "viewer".to_owned(),
                safe_scopes: vec!["metadata:read".to_owned()],
                secret_excluded: true,
            },
            rate_limit: G0RateLimitObservation {
                api: "core".to_owned(),
                limit: 5_000,
                remaining: 4_999,
                used: 1,
                reset_at_utc: "2026-09-20T01:00:00Z".to_owned(),
                observed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            },
            requests: Vec::new(),
            raw_objects: Vec::new(),
            repositories: Vec::new(),
            reconciliation: G0RevisionReconciliation {
                pre_state: Vec::new(),
                post_state: Vec::new(),
                changed_refs: Vec::new(),
                invalidated: Vec::new(),
            },
            dependency_graph: G0DependencyGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
                raw_object_refs: Vec::new(),
            },
            model_session: G0ModelSession {
                session_id: "session".to_owned(),
                effective: false,
                orchestrator_model: EXPECTED_ORCHESTRATOR_MODEL.to_owned(),
                orchestrator_effort: EXPECTED_ORCHESTRATOR_EFFORT.to_owned(),
                agents: Vec::new(),
                raw_object_refs: Vec::new(),
            },
            access: Vec::new(),
            workload_artifact: G0ArtifactReference {
                name: "workload-matrix".to_owned(),
                schema: "velnor.workload-matrix.v1".to_owned(),
                source_url: "https://github.com/tailrocks/velnor/blob/reviewed/matrix.json"
                    .to_owned(),
                sha256: digest_bytes(b"{}"),
                storage_ref: format!(
                    "sha256://{}",
                    digest_bytes(b"{}")
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                source_revision: sha('a'),
                source_digest: digest('c'),
                observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                raw_object_refs: Vec::new(),
            },
        }
    }

    fn typed_inventory(collector: G0CollectorSnapshot) -> G0InventoryEvidence {
        let value = serde_json::to_value(&collector).unwrap();
        let bytes = canonical_json(&value).into_bytes();
        G0InventoryEvidence {
            collector_snapshot: collector,
            collector_snapshot_bytes_base64: BASE64.encode(&bytes),
            collector_snapshot_sha256: digest_bytes(&bytes),
            collector_snapshot_storage_ref: format!(
                "sha256://{}",
                digest_bytes(&bytes)
                    .strip_prefix("sha256:")
                    .expect("digest has sha256 prefix")
            ),
        }
    }

    fn push_provider_raw_fixture(
        requests: &mut Vec<G0RequestRecord>,
        raw_objects: &mut Vec<G0RawObjectRef>,
        repository: &str,
        prefix: &str,
        check: &G0CheckProducer,
    ) -> Vec<String> {
        let app_id = check.app_id.parse::<u64>().unwrap();
        let mut raw_refs = Vec::new();
        let mut push = |kind: &str, endpoint: String, body: Value| {
            let raw_id = if kind == "app" {
                format!("raw-app-{}", check.app_slug)
            } else {
                format!("raw-{prefix}-{kind}")
            };
            if raw_objects.iter().any(|raw| raw.raw_id == raw_id) {
                raw_refs.push(raw_id);
                return;
            }
            let request_id = if kind == "app" {
                format!("request-app-{}", check.app_slug)
            } else {
                format!("request-{prefix}-{kind}")
            };
            let query = if kind == "workflow_attempt_jobs" {
                "per_page=100&page=1"
            } else {
                ""
            };
            let query_bytes = canonical_rest_query(query);
            let body = if kind == "workflow_attempt_jobs" {
                json!({"total_count": 1, "jobs": [body]})
            } else {
                body
            };
            let bytes = canonical_json(&body).into_bytes();
            let digest = digest_bytes(&bytes);
            let storage_ref = format!("sha256://{}", digest.strip_prefix("sha256:").unwrap());
            raw_objects.push(G0RawObjectRef {
                raw_id: raw_id.clone(),
                request_id: request_id.clone(),
                object_kind: kind.to_owned(),
                canonicalization: "jcs".to_owned(),
                sha256: digest.clone(),
                byte_length: bytes.len() as u64,
                bytes_base64: BASE64.encode(&bytes),
                media_type: "application/json".to_owned(),
                storage_ref: storage_ref.clone(),
                original_sha256: digest.clone(),
                original_byte_length: bytes.len() as u64,
                original_storage_ref: storage_ref,
            });
            requests.push(G0RequestRecord {
                request_id,
                api: G0ApiKind::Rest,
                method: "GET".to_owned(),
                endpoint_or_operation: endpoint,
                query_base64: BASE64.encode(&query_bytes),
                variables_base64: BASE64.encode(b"{}"),
                query_sha256: digest_bytes(&query_bytes),
                variables_sha256: digest_bytes(b"{}"),
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: 200,
                api_request_id: format!("api-{prefix}-{kind}"),
                rate_limit_ref: "collector.rate_limit".to_owned(),
                page: G0Page {
                    number: 1,
                    per_page: 100,
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: false,
                    items_returned: 1,
                },
                response_raw_ref: raw_id.clone(),
                error_raw_ref: None,
                state: G0RequestState::Complete,
                complete: true,
                truncation_reason: None,
            });
            raw_refs.push(raw_id);
        };

        push(
            "check_run",
            format!("/repos/{repository}/check-runs/{}", check.check_run_id),
            json!({
                "id": check.check_run_id,
                "name": check.context,
                "head_sha": check.source_sha,
                "status": check.status,
                "conclusion": check.conclusion,
                "html_url": check.html_url,
                "check_suite": {"id": check.check_suite_id},
                "app": {"id": app_id, "slug": check.app_slug}
            }),
        );
        push(
            "check_suite",
            format!("/repos/{repository}/check-suites/{}", check.check_suite_id),
            json!({
                "id": check.check_suite_id,
                "head_sha": check.source_sha,
                "status": check.status,
                "conclusion": check.conclusion,
                "app": {"id": app_id, "slug": check.app_slug}
            }),
        );
        push(
            "app",
            format!("/apps/{}", check.app_slug),
            json!({"id": app_id, "slug": check.app_slug}),
        );
        if let G0CheckProvider::GithubActions {
            workflow_run_id,
            run_attempt,
            job_id,
            job_run_id,
            job_run_attempt,
            job_check_run_id,
            job_source_sha,
            job_html_url,
            ..
        } = &check.provider
        {
            push(
                "workflow_run",
                format!("/repos/{repository}/actions/runs/{workflow_run_id}"),
                json!({
                    "id": workflow_run_id,
                    "run_attempt": run_attempt,
                    "head_sha": job_source_sha,
                    "event": check.event,
                    "status": check.status,
                    "conclusion": check.conclusion
                }),
            );
            push(
                "workflow_attempt_jobs",
                format!(
                    "/repos/{repository}/actions/runs/{workflow_run_id}/attempts/{run_attempt}/jobs"
                ),
                json!({
                    "id": job_id,
                    "run_id": job_run_id,
                    "run_attempt": job_run_attempt,
                    "head_sha": job_source_sha,
                    "html_url": job_html_url,
                    "check_run_url": format!(
                        "https://api.github.com/repos/{repository}/check-runs/{job_check_run_id}"
                    )
                }),
            );
        }
        raw_refs
    }

    /// Complete typed collector fixture.  It is intentionally generated from
    /// the checker-owned canonical scope, with every repository, current PR,
    /// workflow, required check, raw object, reconciliation row, graph edge,
    /// model observation, and access observation populated.  It is a positive
    /// contract fixture, not a claim about the live fleet.
    fn complete_g0_fixture() -> (ManifestDocument, SnapshotDocument, G0InventoryEvidence) {
        let mut requests = Vec::with_capacity(REQUIRED_REPOSITORIES);
        let mut raw_objects = Vec::with_capacity(REQUIRED_REPOSITORIES * 2);
        let mut manifest_repositories = Vec::with_capacity(REQUIRED_REPOSITORIES);
        let mut snapshot_repositories = Vec::with_capacity(REQUIRED_REPOSITORIES);
        let mut collector_repositories = Vec::with_capacity(REQUIRED_REPOSITORIES);
        let mut pre_state = Vec::with_capacity(REQUIRED_REPOSITORIES);
        let mut post_state = Vec::with_capacity(REQUIRED_REPOSITORIES);
        let mut access = Vec::with_capacity(REQUIRED_REPOSITORIES);
        let source_sha = sha('a');
        let head_sha = sha('b');
        let merge_sha = sha('c');
        let workflow_path = ".github/workflows/ci.yml";
        let workflow_url_suffix = "blob/main/.github/workflows/ci.yml";
        let workflow_bytes =
            b"on: [push, pull_request]\njobs:\n  scan:\n    runs-on: ubuntu-24.04\n    steps: []\n";

        for (index, repository) in CANONICAL_REPOSITORIES.iter().enumerate() {
            let repository = (*repository).to_owned();
            let repository_id = index as u64 + 1;
            let main_run_id = 10_000 + repository_id;
            let main_check_suite_id = 20_000 + repository_id;
            let main_check_run_id = 30_000 + repository_id;
            let main_job_id = 40_000 + repository_id;
            let pr_run_id = 50_000 + repository_id;
            let pr_check_suite_id = 60_000 + repository_id;
            let pr_check_run_id = 70_000 + repository_id;
            let pr_job_id = 80_000 + repository_id;
            let artifact_id = 90_000 + repository_id;
            let raw_id = format!("raw-repository-{repository_id}");
            let request_id = format!("request-repository-{repository_id}");
            let artifact_raw_id = format!("raw-artifact-{repository_id}");
            let artifact_request_id = format!("request-artifact-{repository_id}");
            let workflow_request_id = format!("request-workflow-{repository_id}");
            let raw_bytes =
                format!("{{\"repository\":\"{repository}\",\"id\":{repository_id}}}").into_bytes();
            let raw_digest = digest_bytes(&raw_bytes);
            raw_objects.push(G0RawObjectRef {
                raw_id: raw_id.clone(),
                request_id: request_id.clone(),
                object_kind: "repository".to_owned(),
                canonicalization: "jcs".to_owned(),
                sha256: raw_digest.clone(),
                byte_length: raw_bytes.len() as u64,
                bytes_base64: BASE64.encode(&raw_bytes),
                media_type: "application/json".to_owned(),
                storage_ref: format!(
                    "sha256://{}",
                    raw_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                original_sha256: raw_digest.clone(),
                original_byte_length: raw_bytes.len() as u64,
                original_storage_ref: format!(
                    "sha256://{}",
                    raw_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
            });
            let artifact_name = format!("scan-artifact-{repository_id}");
            let artifact_archive_digest = digest('d');
            let artifact_source_url = format!(
                "https://api.github.com/repos/{repository}/actions/artifacts/{artifact_id}/zip"
            );
            let artifact_bytes = format!(
                "{{\"total_count\":1,\"artifacts\":[{{\"id\":{artifact_id},\"name\":\"{artifact_name}\",\"digest\":\"{artifact_archive_digest}\",\"expired\":false,\"archive_download_url\":\"{artifact_source_url}\",\"workflow_run\":{{\"id\":{main_run_id},\"head_sha\":\"{source_sha}\"}}}}]}}"
            )
            .into_bytes();
            let artifact_page_digest = digest_bytes(&artifact_bytes);
            raw_objects.push(G0RawObjectRef {
                raw_id: artifact_raw_id.clone(),
                request_id: artifact_request_id.clone(),
                object_kind: "workflow_artifacts".to_owned(),
                canonicalization: "raw-json".to_owned(),
                sha256: artifact_page_digest.clone(),
                byte_length: artifact_bytes.len() as u64,
                bytes_base64: BASE64.encode(&artifact_bytes),
                media_type: "application/json".to_owned(),
                storage_ref: format!(
                    "sha256://{}",
                    artifact_page_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                original_sha256: artifact_page_digest.clone(),
                original_byte_length: artifact_bytes.len() as u64,
                original_storage_ref: format!(
                    "sha256://{}",
                    artifact_page_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
            });
            let workflow_raw_id = format!("raw-workflow-{repository_id}");
            let workflow_raw_digest = digest_bytes(workflow_bytes);
            raw_objects.push(G0RawObjectRef {
                raw_id: workflow_raw_id.clone(),
                request_id: workflow_request_id.clone(),
                object_kind: "workflow.source".to_owned(),
                canonicalization: "raw-utf8".to_owned(),
                sha256: workflow_raw_digest.clone(),
                byte_length: workflow_bytes.len() as u64,
                bytes_base64: BASE64.encode(workflow_bytes),
                media_type: "text/yaml".to_owned(),
                storage_ref: format!(
                    "sha256://{}",
                    workflow_raw_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                original_sha256: workflow_raw_digest.clone(),
                original_byte_length: workflow_bytes.len() as u64,
                original_storage_ref: format!(
                    "sha256://{}",
                    workflow_raw_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
            });
            requests.push(G0RequestRecord {
                request_id,
                api: G0ApiKind::Rest,
                method: "GET".to_owned(),
                endpoint_or_operation: format!("/repos/{repository}"),
                query_base64: BASE64.encode(b""),
                variables_base64: BASE64.encode(b"{}"),
                query_sha256: digest_bytes(b""),
                variables_sha256: digest_bytes(b"{}"),
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: 200,
                api_request_id: format!("api-request-{repository_id}"),
                rate_limit_ref: "collector.rate_limit".to_owned(),
                page: G0Page {
                    number: 1,
                    per_page: 100,
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: false,
                    items_returned: 1,
                },
                response_raw_ref: raw_id.clone(),
                error_raw_ref: None,
                state: G0RequestState::Complete,
                complete: true,
                truncation_reason: None,
            });
            requests.push(G0RequestRecord {
                request_id: artifact_request_id,
                api: G0ApiKind::Rest,
                method: "GET".to_owned(),
                endpoint_or_operation: format!(
                    "/repos/{repository}/actions/runs/{main_run_id}/artifacts"
                ),
                query_base64: BASE64.encode(&canonical_rest_query("per_page=100&page=1")),
                variables_base64: BASE64.encode(b"{}"),
                query_sha256: digest_bytes(&canonical_rest_query("per_page=100&page=1")),
                variables_sha256: digest_bytes(b"{}"),
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: 200,
                api_request_id: format!("api-artifact-request-{repository_id}"),
                rate_limit_ref: "collector.rate_limit".to_owned(),
                page: G0Page {
                    number: 1,
                    per_page: 100,
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: false,
                    items_returned: 1,
                },
                response_raw_ref: artifact_raw_id.clone(),
                error_raw_ref: None,
                state: G0RequestState::Complete,
                complete: true,
                truncation_reason: None,
            });
            requests.push(G0RequestRecord {
                request_id: workflow_request_id,
                api: G0ApiKind::Rest,
                method: "GET".to_owned(),
                endpoint_or_operation: format!(
                    "/repos/{repository}/contents/{workflow_path}?ref={source_sha}"
                ),
                query_base64: BASE64.encode(b""),
                variables_base64: BASE64.encode(b"{}"),
                query_sha256: digest_bytes(b""),
                variables_sha256: digest_bytes(b"{}"),
                auth_identity_ref: "collector.auth".to_owned(),
                started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
                http_status: 200,
                api_request_id: format!("api-workflow-request-{repository_id}"),
                rate_limit_ref: "collector.rate_limit".to_owned(),
                page: G0Page {
                    number: 1,
                    per_page: 100,
                    link_next: None,
                    cursor_in: None,
                    cursor_out: None,
                    has_next_page: false,
                    items_returned: 1,
                },
                response_raw_ref: workflow_raw_id.clone(),
                error_raw_ref: None,
                state: G0RequestState::Complete,
                complete: true,
                truncation_reason: None,
            });
            let run_url = format!("https://github.com/{repository}/actions/runs/{main_run_id}");
            let pr_run_url = format!("https://github.com/{repository}/actions/runs/{pr_run_id}");
            let repository_url = format!("https://github.com/{repository}");
            let rules_url = format!("{repository_url}/settings/rules");
            let workflow_url = format!("{repository_url}/{workflow_url_suffix}");
            // Keep one captured external-App shape in the PR fixture while
            // exercising the captured GitHub Actions shape for main and the
            // remaining PRs.
            let main_provider = G0CheckProvider::GithubActions {
                workflow_run_id: main_run_id,
                run_attempt: 1,
                job_id: main_job_id,
                job_run_id: main_run_id,
                job_run_attempt: 1,
                job_check_run_id: main_check_run_id,
                job_source_sha: source_sha.clone(),
                job_html_url: format!(
                    "{repository_url}/actions/runs/{main_run_id}/job/{main_job_id}"
                ),
                actual_checkout_sha: source_sha.clone(),
            };
            let pr_provider = if index < 2 {
                G0CheckProvider::ExternalApp
            } else {
                G0CheckProvider::GithubActions {
                    workflow_run_id: pr_run_id,
                    run_attempt: 1,
                    job_id: pr_job_id,
                    job_run_id: pr_run_id,
                    job_run_attempt: 1,
                    job_check_run_id: pr_check_run_id,
                    job_source_sha: head_sha.clone(),
                    job_html_url: format!(
                        "{repository_url}/actions/runs/{pr_run_id}/job/{pr_job_id}"
                    ),
                    actual_checkout_sha: merge_sha.clone(),
                }
            };
            let main_app_slug = "github-actions";
            let pr_app_slug = match index {
                0 => "dco-2",
                1 => "sonarcloud",
                _ => "github-actions",
            };
            let main_check_url =
                format!("{repository_url}/actions/runs/{main_run_id}/job/{main_check_run_id}");
            let pr_check_url = if index < 2 {
                format!("{repository_url}/runs/{pr_check_run_id}")
            } else {
                format!("{repository_url}/actions/runs/{pr_run_id}/job/{pr_check_run_id}")
            };
            let main_job_id_string = main_job_id.to_string();
            let pr_job_id_string = pr_job_id.to_string();
            let required_context = RequiredContext {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
            };
            let workload_platform = WorkloadPlatform {
                workload_id: "scan".to_owned(),
                platform: "linux".to_owned(),
                architecture: "amd64".to_owned(),
            };
            manifest_repositories.push(ManifestRepository {
                repository: repository.clone(),
                repository_role: "library".to_owned(),
                default_branch: "main".to_owned(),
                expected_workload_ids: vec!["scan".to_owned()],
                required_check_contexts_and_apps: vec![required_context.clone()],
                workload_platform_architecture: vec![workload_platform],
                expected_jobs: vec![ExpectedJobSpec {
                    job_id: "scan".to_owned(),
                    workload_id: "scan".to_owned(),
                    provider: "github".to_owned(),
                    platform: "linux".to_owned(),
                    architecture: "amd64".to_owned(),
                    required: true,
                    child_workflow: None,
                }],
                generated_plan_digest: digest('a'),
                workflow_path: workflow_path.to_owned(),
                workflow_revision: source_sha.clone(),
                provider_eligibility: BTreeMap::from([
                    ("github".to_owned(), Eligibility::Eligible),
                    ("velnor".to_owned(), Eligibility::NotApplicable),
                ]),
                host_contracts: BTreeMap::new(),
                release_applicability: Applicability::NotApplicable,
                generator_revision: source_sha.clone(),
                runtime_product_id: "velnor".to_owned(),
                generator_artifact_digest: digest('a'),
                configuration_digest: digest('a'),
                generated_tree_digest: digest('a'),
                scan_state_digest: digest('a'),
                runtime_release_version: "1.0.0".to_owned(),
                runtime_source_sha: source_sha.clone(),
                job_image_digest: digest('a'),
            });

            let main_job = JobObservation {
                job_id: main_job_id_string.clone(),
                job_name: "scan".to_owned(),
                workload_id: "scan".to_owned(),
                provider: "github".to_owned(),
                platform: "linux".to_owned(),
                architecture: "amd64".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                event: "push".to_owned(),
                runner_name: "GitHub Actions 1".to_owned(),
                host_id: format!("github-runner-{repository_id}"),
                runner_kind: "github-hosted".to_owned(),
                runner_labels: vec!["ubuntu-24.04".to_owned()],
                source_url: run_url.clone(),
            };
            let main_check = CheckObservation {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                run_id: main_run_id,
                job_id: main_job_id_string.clone(),
                source_url: main_check_url.clone(),
                event: "push".to_owned(),
            };
            let main_execution = ExecutionObservation {
                run_id: main_run_id,
                run_attempt: 1,
                run_url: run_url.clone(),
                workflow_path: workflow_path.to_owned(),
                workflow_revision: source_sha.clone(),
                event: "push".to_owned(),
                trigger_source_sha: source_sha.clone(),
                actual_checkout_sha: source_sha.clone(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                provider: "github".to_owned(),
                runner_name: "GitHub Actions 1".to_owned(),
                host_id: format!("github-runner-{repository_id}"),
                runner_kind: "github-hosted".to_owned(),
                runner_labels: vec!["ubuntu-24.04".to_owned()],
                jobs: vec![main_job],
                required_checks: vec![main_check],
                child_runs: Vec::new(),
            };
            let pr_job = JobObservation {
                job_id: pr_job_id_string.clone(),
                job_name: "scan".to_owned(),
                workload_id: "scan".to_owned(),
                provider: "github".to_owned(),
                platform: "linux".to_owned(),
                architecture: "amd64".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                event: "pull_request".to_owned(),
                runner_name: "GitHub Actions 1".to_owned(),
                host_id: format!("github-pr-runner-{repository_id}"),
                runner_kind: "github-hosted".to_owned(),
                runner_labels: vec!["ubuntu-24.04".to_owned()],
                source_url: pr_run_url.clone(),
            };
            let pr_check = CheckObservation {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                run_id: pr_run_id,
                job_id: pr_job_id_string.clone(),
                source_url: pr_check_url.clone(),
                event: "pull_request".to_owned(),
            };
            let pr_execution = ExecutionObservation {
                run_id: pr_run_id,
                run_attempt: 1,
                run_url: pr_run_url.clone(),
                workflow_path: workflow_path.to_owned(),
                workflow_revision: source_sha.clone(),
                event: "pull_request".to_owned(),
                trigger_source_sha: head_sha.clone(),
                actual_checkout_sha: merge_sha.clone(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                provider: "github".to_owned(),
                runner_name: "GitHub Actions 1".to_owned(),
                host_id: format!("github-pr-runner-{repository_id}"),
                runner_kind: "github-hosted".to_owned(),
                runner_labels: vec!["ubuntu-24.04".to_owned()],
                jobs: vec![pr_job],
                required_checks: vec![pr_check],
                child_runs: Vec::new(),
            };
            let pull_request = SnapshotPullRequest {
                number: 1,
                state: "open".to_owned(),
                draft: false,
                author: "author".to_owned(),
                author_association: "CONTRIBUTOR".to_owned(),
                head_repository: repository.clone(),
                head_sha: head_sha.clone(),
                base_sha: source_sha.clone(),
                merge_sha: Some(merge_sha.clone()),
                merge_group_sha: None,
                source_url: format!("{repository_url}/pull/1"),
                executions: vec![pr_execution],
            };
            snapshot_repositories.push(SnapshotRepository {
                repository: repository.clone(),
                repository_id,
                default_branch: "main".to_owned(),
                default_branch_sha: source_sha.clone(),
                ruleset: RulesetObservation {
                    required_checks: vec![required_context.clone()],
                    source_url: rules_url,
                    pages_complete: true,
                },
                workflows: vec![WorkflowObservation {
                    path: workflow_path.to_owned(),
                    revision: source_sha.clone(),
                    source_sha: source_sha.clone(),
                    event: "push".to_owned(),
                    source_url: workflow_url.clone(),
                }],
                main_executions: vec![main_execution],
                open_prs: vec![pull_request],
            });

            let generated_state = G0ArtifactReference {
                name: format!("generated-state-{repository_id}"),
                schema: "velnor.generated-state.v1".to_owned(),
                source_url: workflow_url,
                sha256: raw_digest.clone(),
                storage_ref: format!(
                    "sha256://{}",
                    raw_digest
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                source_revision: source_sha.clone(),
                source_digest: digest('c'),
                observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                raw_object_refs: vec![raw_id.clone()],
            };
            let workflow_source = G0WorkflowSource {
                repository: repository.clone(),
                path: workflow_path.to_owned(),
                revision: source_sha.clone(),
                source_sha: source_sha.clone(),
                source_url: format!(
                    "https://github.com/{repository}/blob/{source_sha}/{workflow_path}"
                ),
                media_type: "text/yaml".to_owned(),
                canonicalization: "raw-utf8".to_owned(),
                sha256: digest_bytes(workflow_bytes),
                storage_ref: format!(
                    "sha256://{}",
                    digest_bytes(workflow_bytes)
                        .strip_prefix("sha256:")
                        .expect("digest has prefix")
                ),
                byte_length: workflow_bytes.len() as u64,
                bytes_base64: BASE64.encode(workflow_bytes),
                raw_object_refs: vec![workflow_raw_id.clone()],
            };
            let mut main_check_producer = G0CheckProducer {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
                app_slug: main_app_slug.to_owned(),
                provider: main_provider,
                api: G0ApiKind::Rest,
                check_suite_id: main_check_suite_id,
                check_run_id: main_check_run_id,
                source_sha: source_sha.clone(),
                event: "push".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                html_url: main_check_url,
                raw_object_refs: vec![raw_id.clone()],
            };
            let mut pr_check_producer = G0CheckProducer {
                context: "ci".to_owned(),
                app_id: "123".to_owned(),
                app_slug: pr_app_slug.to_owned(),
                provider: pr_provider,
                api: G0ApiKind::Rest,
                check_suite_id: pr_check_suite_id,
                check_run_id: pr_check_run_id,
                source_sha: head_sha.clone(),
                event: "pull_request".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                html_url: pr_check_url,
                raw_object_refs: vec![raw_id.clone()],
            };
            main_check_producer
                .raw_object_refs
                .extend(push_provider_raw_fixture(
                    &mut requests,
                    &mut raw_objects,
                    &repository,
                    &format!("{repository_id}-main"),
                    &main_check_producer,
                ));
            pr_check_producer
                .raw_object_refs
                .extend(push_provider_raw_fixture(
                    &mut requests,
                    &mut raw_objects,
                    &repository,
                    &format!("{repository_id}-pr"),
                    &pr_check_producer,
                ));
            collector_repositories.push(G0RepositoryInventory {
                repository: repository.clone(),
                repository_id,
                default_branch: "main".to_owned(),
                default_branch_sha: source_sha.clone(),
                rulesets: vec![G0RulesetInventory {
                    ruleset_id: repository_id,
                    name: "required-ci".to_owned(),
                    source_url: format!("https://github.com/{repository}/settings/rules"),
                    complete: true,
                    required_checks: vec![G0RequiredCheckPolicy {
                        context: "ci".to_owned(),
                        app_id: "123".to_owned(),
                        ruleset_id: repository_id,
                        raw_object_refs: vec![raw_id.clone()],
                    }],
                    raw_object_refs: vec![raw_id.clone()],
                }],
                workflows: vec![G0WorkflowInventory {
                    source: workflow_source,
                    events: vec!["push".to_owned(), "pull_request".to_owned()],
                    source_jobs: vec![G0SourceJob {
                        job_id: "scan".to_owned(),
                        workload_id: "scan".to_owned(),
                        provider: "github".to_owned(),
                        platform: "linux".to_owned(),
                        architecture: "amd64".to_owned(),
                        required: true,
                        raw_object_refs: vec![workflow_raw_id.clone()],
                    }],
                    reusable_workflows: Vec::new(),
                    actions: Vec::new(),
                    scanners: Vec::new(),
                    generated_state,
                    raw_object_refs: vec![raw_id.clone()],
                }],
                artifacts: vec![G0ArtifactObservation {
                    artifact_id,
                    run_id: main_run_id,
                    run_attempt: 1,
                    run_head_sha: source_sha.clone(),
                    name: artifact_name,
                    digest: artifact_archive_digest,
                    expired: false,
                    source_url: artifact_source_url,
                    raw_object_refs: vec![artifact_raw_id],
                }],
                open_prs: vec![G0PullRequestInventory {
                    number: 1,
                    state: "open".to_owned(),
                    draft: false,
                    author: "author".to_owned(),
                    author_association: "CONTRIBUTOR".to_owned(),
                    head_repository: repository.clone(),
                    head_sha: head_sha.clone(),
                    base_sha: source_sha.clone(),
                    tested_merge_sha: merge_sha.clone(),
                    merge_group_sha: None,
                    trust: G0TrustObservation {
                        state: "trusted".to_owned(),
                        reason: "fixture trust observation".to_owned(),
                        raw_object_refs: vec![raw_id.clone()],
                    },
                    applicability: "required".to_owned(),
                    source_url: format!("https://github.com/{repository}/pull/1"),
                    workflow_bindings: vec![G0WorkflowBinding {
                        workflow_path: workflow_path.to_owned(),
                        workflow_revision: source_sha.clone(),
                        event: "pull_request".to_owned(),
                        source_sha: head_sha.clone(),
                        actual_checkout_sha: merge_sha.clone(),
                        run_ids: vec![pr_run_id],
                        raw_object_refs: vec![raw_id.clone()],
                    }],
                    required_check_producers: vec![pr_check_producer],
                    raw_object_refs: vec![raw_id.clone()],
                }],
                main_checks: vec![main_check_producer],
                raw_object_refs: vec![raw_id.clone()],
            });
            let revision = G0PullRequestRevision {
                number: 1,
                head_sha: head_sha.clone(),
                base_sha: source_sha.clone(),
                tested_merge_sha: merge_sha.clone(),
                merge_group_sha: None,
                raw_object_refs: vec![raw_id.clone()],
            };
            pre_state.push(G0RepositoryRevision {
                repository: repository.clone(),
                default_branch_sha: source_sha.clone(),
                prs: vec![revision.clone()],
                raw_object_refs: vec![raw_id.clone()],
            });
            post_state.push(G0RepositoryRevision {
                repository,
                default_branch_sha: source_sha.clone(),
                prs: vec![revision],
                raw_object_refs: vec![raw_id.clone()],
            });
            access.push(G0AccessObservation {
                repository: (*CANONICAL_REPOSITORIES.get(index).unwrap()).to_owned(),
                state: "complete".to_owned(),
                scopes: vec!["metadata:read".to_owned()],
                gaps: Vec::new(),
                raw_object_refs: vec![format!("raw-repository-{repository_id}")],
            });
        }
        let nodes = CANONICAL_REPOSITORIES
            .iter()
            .enumerate()
            .flat_map(|(index, repository)| {
                let workflow_raw_id = format!("raw-workflow-{}", index + 1);
                [
                    G0GraphNode {
                        id: format!("workload:{repository}:scan"),
                        kind: "workload".to_owned(),
                        repository: (*repository).to_owned(),
                        workload_id: "scan".to_owned(),
                        applicability: "required".to_owned(),
                        source_sha: source_sha.clone(),
                        source_ref: "refs/heads/main".to_owned(),
                        raw_object_refs: vec![workflow_raw_id.clone()],
                    },
                    G0GraphNode {
                        id: format!("check:{repository}:ci"),
                        kind: "check".to_owned(),
                        repository: (*repository).to_owned(),
                        workload_id: "scan".to_owned(),
                        applicability: "required".to_owned(),
                        source_sha: source_sha.clone(),
                        source_ref: "refs/heads/main".to_owned(),
                        raw_object_refs: vec![workflow_raw_id],
                    },
                ]
            })
            .collect::<Vec<_>>();
        let edges = CANONICAL_REPOSITORIES
            .iter()
            .enumerate()
            .map(|(index, repository)| G0GraphEdge {
                from: format!("workload:{repository}:scan"),
                to: format!("check:{repository}:ci"),
                kind: "workload-to-check".to_owned(),
                required: true,
                source_sha: source_sha.clone(),
                source_ref: "refs/heads/main".to_owned(),
                target_source_sha: source_sha.clone(),
                target_source_ref: "refs/heads/main".to_owned(),
                raw_object_refs: vec![format!("raw-workflow-{}", index + 1)],
            })
            .collect::<Vec<_>>();
        let model_session = G0ModelSession {
            session_id: "session".to_owned(),
            effective: true,
            orchestrator_model: EXPECTED_ORCHESTRATOR_MODEL.to_owned(),
            orchestrator_effort: EXPECTED_ORCHESTRATOR_EFFORT.to_owned(),
            agents: vec![G0AgentModel {
                agent_id: "agent".to_owned(),
                model: EXPECTED_AGENT_MODEL.to_owned(),
                effort: EXPECTED_AGENT_EFFORT.to_owned(),
                effective: true,
                raw_object_refs: vec!["raw-model-session".to_owned()],
            }],
            raw_object_refs: vec!["raw-model-session".to_owned()],
        };
        let model_session_bytes =
            serde_json::to_vec(&model_session).expect("serialize model session fixture");
        raw_objects.push(captured_raw_reference(
            "raw-model-session",
            "local-model-session",
            "model.session",
            &model_session_bytes,
        ));
        let workload_source_bytes = b"reviewed workload source fixture";
        let workload_source_raw = captured_raw_reference(
            "raw-workload-source",
            "local-workload-source",
            "workload.source",
            workload_source_bytes,
        );
        let workload_artifact = G0ArtifactReference {
            name: "workload-matrix".to_owned(),
            schema: "velnor.workload-matrix.v1".to_owned(),
            source_url: "https://github.com/tailrocks/velnor/blob/reviewed/matrix.json".to_owned(),
            sha256: workload_source_raw.sha256.clone(),
            storage_ref: workload_source_raw.storage_ref.clone(),
            source_revision: source_sha.clone(),
            source_digest: workload_source_raw.sha256.clone(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            raw_object_refs: vec![
                workload_source_raw.raw_id.clone(),
                "raw-workload-artifact".to_owned(),
            ],
        };
        raw_objects.push(workload_source_raw);
        let workload_artifact_bytes =
            serde_json::to_vec(&workload_artifact).expect("serialize workload artifact fixture");
        raw_objects.push(captured_raw_reference(
            "raw-workload-artifact",
            "local-workload-artifact",
            "workload.artifact",
            &workload_artifact_bytes,
        ));
        let collector = G0CollectorSnapshot {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            snapshot_id: "snapshot".to_owned(),
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            phase: "G0".to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            collector: G0CollectorIdentity {
                name: "fixture-collector".to_owned(),
                revision: source_sha.clone(),
                mode: "read_only".to_owned(),
                api_base: "https://api.github.com".to_owned(),
                api_versions: vec!["2022-11-28".to_owned()],
            },
            auth: G0AuthIdentity {
                provider: "github".to_owned(),
                viewer_id: "viewer-id".to_owned(),
                viewer_login: "viewer".to_owned(),
                safe_scopes: G0_ALLOWED_SCOPES
                    .iter()
                    .map(|scope| (*scope).to_owned())
                    .collect(),
                secret_excluded: true,
            },
            rate_limit: G0RateLimitObservation {
                api: "core".to_owned(),
                limit: 5_000,
                remaining: 4_999,
                used: 1,
                reset_at_utc: "2026-09-20T01:00:00Z".to_owned(),
                observed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            },
            requests,
            raw_objects,
            repositories: collector_repositories,
            reconciliation: G0RevisionReconciliation {
                pre_state,
                post_state,
                changed_refs: Vec::new(),
                invalidated: Vec::new(),
            },
            dependency_graph: G0DependencyGraph {
                nodes,
                edges,
                raw_object_refs: vec!["raw-workflow-1".to_owned()],
            },
            model_session,
            access,
            workload_artifact,
        };
        let manifest = ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            source: SourceIdentity {
                repository: REVIEWED_SOURCE_REPOSITORY.to_owned(),
                revision: REVIEWED_SOURCE_REVISION.to_owned(),
                digest: REVIEWED_SOURCE_DIGEST.to_owned(),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: manifest_repositories,
        };
        let snapshot = SnapshotDocument {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            snapshot_id: "snapshot".to_owned(),
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            source: SnapshotSource {
                collector: "fixture".to_owned(),
                collector_revision: sha('a'),
                api_base: "https://api.github.com".to_owned(),
                captured_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                read_only: true,
                page_count: 1,
                permission_scopes: vec!["metadata:read".to_owned()],
            },
            repositories: snapshot_repositories,
        };
        (manifest, snapshot, typed_inventory(collector))
    }

    fn g0_codes(findings: &[Finding]) -> BTreeSet<String> {
        findings
            .iter()
            .map(|finding| finding.code.clone())
            .collect()
    }

    fn refresh_typed_inventory_bytes(inventory: &mut G0InventoryEvidence) {
        let value = serde_json::to_value(&inventory.collector_snapshot).unwrap();
        let bytes = canonical_json(&value).into_bytes();
        inventory.collector_snapshot_bytes_base64 = BASE64.encode(&bytes);
        inventory.collector_snapshot_sha256 = digest_bytes(&bytes);
        inventory.collector_snapshot_storage_ref = format!(
            "sha256://{}",
            inventory
                .collector_snapshot_sha256
                .strip_prefix("sha256:")
                .expect("digest has sha256 prefix")
        );
    }

    #[test]
    fn complete_g0_collector_fixture_is_positive_and_mutations_fail() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(findings.is_empty(), "unexpected findings: {findings:?}");

        let mut unknown_paginated = inventory.clone();
        unknown_paginated
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.object_kind == "workflow_artifacts")
            .expect("complete fixture artifact page")
            .object_kind = "repository".to_owned();
        refresh_typed_inventory_bytes(&mut unknown_paginated);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&unknown_paginated),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-endpoint-contract"));

        let mut unknown_collection_path = inventory.clone();
        let artifact_request_id = unknown_collection_path
            .collector_snapshot
            .raw_objects
            .iter()
            .find(|raw| raw.object_kind == "workflow_artifacts")
            .expect("complete fixture artifact page")
            .request_id
            .clone();
        unknown_collection_path
            .collector_snapshot
            .requests
            .iter_mut()
            .find(|request| request.request_id == artifact_request_id)
            .expect("artifact page request")
            .endpoint_or_operation
            .push_str("-unknown");
        unknown_collection_path
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.request_id == artifact_request_id)
            .expect("artifact page raw object")
            .object_kind = "repository".to_owned();
        refresh_typed_inventory_bytes(&mut unknown_collection_path);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&unknown_collection_path),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-endpoint-contract"));

        let mut conflated_artifact_digest = inventory.clone();
        let page_raw_id = conflated_artifact_digest.collector_snapshot.repositories[0].artifacts[0]
            .raw_object_refs[0]
            .clone();
        let page_digest = conflated_artifact_digest
            .collector_snapshot
            .raw_objects
            .iter()
            .find(|raw| raw.raw_id == page_raw_id)
            .expect("artifact page raw object")
            .sha256
            .clone();
        conflated_artifact_digest.collector_snapshot.repositories[0].artifacts[0].digest =
            page_digest;
        refresh_typed_inventory_bytes(&mut conflated_artifact_digest);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&conflated_artifact_digest),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-artifact-row"));

        let mut missing_artifacts = inventory.clone();
        missing_artifacts.collector_snapshot.repositories[0]
            .artifacts
            .clear();
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&missing_artifacts),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-artifact-inventory"));

        let mut missing_original = inventory.clone();
        missing_original.collector_snapshot.raw_objects[0].original_storage_ref =
            "sha256://not-the-digest".to_owned();
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&missing_original), &mut findings);
        assert!(g0_codes(&findings).contains("g0-raw-object"));

        let mut wrong_artifact = inventory.clone();
        wrong_artifact.collector_snapshot.repositories[0].artifacts[0].source_url =
            "https://api.github.com/repos/other/repository/actions/artifacts/90001/zip".to_owned();
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_artifact), &mut findings);
        assert!(g0_codes(&findings).contains("g0-artifact-identity"));

        let mut wrong_check_url = complete_g0_fixture().2;
        let check = &mut wrong_check_url.collector_snapshot.repositories[0].main_checks[0];
        let workflow_run_id = match &check.provider {
            G0CheckProvider::GithubActions {
                workflow_run_id, ..
            } => *workflow_run_id,
            G0CheckProvider::ExternalApp => panic!("fixture main check must be Actions"),
        };
        check.html_url = format!(
            "https://github.com/{}/actions/runs/{}/job/{}",
            CANONICAL_REPOSITORIES[0],
            workflow_run_id,
            check.check_run_id + 1
        );
        refresh_typed_inventory_bytes(&mut wrong_check_url);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&wrong_check_url), &mut findings);
        assert!(g0_codes(&findings).contains("g0-check-producer"));

        macro_rules! assert_check_url_rejected {
            ($candidate:expr) => {{
                let mut candidate = $candidate;
                refresh_typed_inventory_bytes(&mut candidate);
                findings.clear();
                check_g0_inventory(&manifest, &snapshot, Some(&candidate), &mut findings);
                assert!(
                    g0_codes(&findings).contains("g0-check-producer")
                        || g0_codes(&findings).contains("g0-pr-check-producer"),
                    "mutated provider URL unexpectedly passed: {findings:?}"
                );
            }};
        }

        let mut wrong_actions_run = inventory.clone();
        let action_check = &mut wrong_actions_run.collector_snapshot.repositories[1].main_checks[0];
        let action_run_id = match &action_check.provider {
            G0CheckProvider::GithubActions {
                workflow_run_id, ..
            } => *workflow_run_id,
            G0CheckProvider::ExternalApp => panic!("fixture check must be Actions"),
        };
        action_check.html_url = format!(
            "https://github.com/{}/actions/runs/{}/job/{}",
            CANONICAL_REPOSITORIES[1],
            action_run_id + 1,
            action_check.check_run_id
        );
        assert_check_url_rejected!(wrong_actions_run);

        let mut wrong_actions_check = inventory.clone();
        let action_check =
            &mut wrong_actions_check.collector_snapshot.repositories[1].main_checks[0];
        let action_run_id = match &action_check.provider {
            G0CheckProvider::GithubActions {
                workflow_run_id, ..
            } => *workflow_run_id,
            G0CheckProvider::ExternalApp => panic!("fixture check must be Actions"),
        };
        action_check.html_url = format!(
            "https://github.com/{}/actions/runs/{}/job/{}",
            CANONICAL_REPOSITORIES[1],
            action_run_id,
            action_check.check_run_id + 1
        );
        assert_check_url_rejected!(wrong_actions_check);

        let mut wrong_actions_job = inventory.clone();
        let action_check = &mut wrong_actions_job.collector_snapshot.repositories[1].main_checks[0];
        if let G0CheckProvider::GithubActions {
            workflow_run_id,
            job_id,
            job_html_url,
            ..
        } = &mut action_check.provider
        {
            *job_html_url = format!(
                "https://github.com/{}/actions/runs/{}/job/{}",
                CANONICAL_REPOSITORIES[1],
                *workflow_run_id,
                *job_id + 1
            );
        } else {
            panic!("fixture check must be Actions");
        }
        assert_check_url_rejected!(wrong_actions_job);

        let mut foreign_check_host = inventory.clone();
        let action_check =
            &mut foreign_check_host.collector_snapshot.repositories[1].main_checks[0];
        action_check.html_url =
            action_check
                .html_url
                .replacen("https://github.com", "https://evil.example", 1);
        assert_check_url_rejected!(foreign_check_host);

        let mut check_query = inventory.clone();
        let action_check = &mut check_query.collector_snapshot.repositories[1].main_checks[0];
        action_check.html_url.push_str("?source=details");
        assert_check_url_rejected!(check_query);

        let mut details_url = inventory.clone();
        let external_check = &mut details_url.collector_snapshot.repositories[0].main_checks[0];
        external_check.html_url = "https://cncf.github.io/dco2".to_owned();
        assert_check_url_rejected!(details_url);

        let mut external_missing_app = inventory.clone();
        external_missing_app.collector_snapshot.repositories[0].open_prs[0]
            .required_check_producers[0]
            .app_slug
            .clear();
        assert_check_url_rejected!(external_missing_app);

        let mut external_wrong_app = inventory.clone();
        external_wrong_app.collector_snapshot.repositories[0].open_prs[0]
            .required_check_producers[0]
            .app_slug = "github-actions".to_owned();
        assert_check_url_rejected!(external_wrong_app);

        let mut external_wrong_source = inventory.clone();
        external_wrong_source.collector_snapshot.repositories[0].open_prs[0]
            .required_check_producers[0]
            .source_sha = sha('d');
        assert_check_url_rejected!(external_wrong_source);

        let mut external_wrong_api = inventory.clone();
        external_wrong_api.collector_snapshot.repositories[0].open_prs[0]
            .required_check_producers[0]
            .api = G0ApiKind::Graphql;
        assert_check_url_rejected!(external_wrong_api);

        let mut wrong_artifact_attempt = inventory.clone();
        wrong_artifact_attempt.collector_snapshot.repositories[0].artifacts[0].run_attempt = 2;
        refresh_typed_inventory_bytes(&mut wrong_artifact_attempt);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&wrong_artifact_attempt),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-artifact-identity"));

        let mut wrong_artifact_request = inventory.clone();
        let artifact_raw_id = wrong_artifact_request.collector_snapshot.repositories[0].artifacts
            [0]
        .raw_object_refs[0]
            .clone();
        wrong_artifact_request
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == artifact_raw_id)
            .expect("artifact raw object")
            .request_id = "request-repository-1".to_owned();
        refresh_typed_inventory_bytes(&mut wrong_artifact_request);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&wrong_artifact_request),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-artifact-request"));

        let mut missing_source_job = inventory.clone();
        missing_source_job.collector_snapshot.repositories[0].workflows[0]
            .source_jobs
            .clear();
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&missing_source_job),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-source-job-inventory"));

        let mut mismatched_source_job = inventory.clone();
        mismatched_source_job.collector_snapshot.repositories[0].workflows[0].source_jobs[0]
            .platform = "windows".to_owned();
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&mismatched_source_job),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-source-job-plan"));

        let mut raw_tampered = inventory.clone();
        raw_tampered.collector_snapshot.raw_objects[0].bytes_base64 = BASE64.encode(b"tampered");
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&raw_tampered), &mut findings);
        assert!(g0_codes(&findings).contains("g0-raw-object"));

        let mut pr_event_tampered = inventory.clone();
        pr_event_tampered.collector_snapshot.repositories[0].open_prs[0].workflow_bindings[0]
            .event = "workflow_dispatch".to_owned();
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&pr_event_tampered),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-pr-workflow-binding"));

        let mut graph_tampered = inventory.clone();
        graph_tampered
            .collector_snapshot
            .dependency_graph
            .edges
            .clear();
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&graph_tampered), &mut findings);
        assert!(g0_codes(&findings).contains("g0-dependency-graph"));

        // A same-repository response with the same bytes is not a source
        // binding. Clone the workflow body under a valid ruleset endpoint;
        // repository-only graph provenance must reject this typed path.
        let mut same_repository_endpoint_clone = complete_g0_fixture().2;
        let source_raw = same_repository_endpoint_clone
            .collector_snapshot
            .raw_objects
            .iter()
            .find(|raw| raw.raw_id == "raw-workflow-1")
            .expect("complete fixture workflow source raw")
            .clone();
        let source_request = same_repository_endpoint_clone
            .collector_snapshot
            .requests
            .iter()
            .find(|request| request.request_id == source_raw.request_id)
            .expect("complete fixture workflow source request")
            .clone();
        let clone_raw_id = "raw-same-repository-ruleset-clone".to_owned();
        let clone_request_id = "request-same-repository-ruleset-clone".to_owned();
        let mut clone_raw = source_raw;
        clone_raw.raw_id = clone_raw_id.clone();
        clone_raw.request_id = clone_request_id.clone();
        clone_raw.object_kind = "ruleset".to_owned();
        let mut clone_request = source_request;
        clone_request.request_id = clone_request_id;
        clone_request.response_raw_ref = clone_raw_id.clone();
        clone_request.endpoint_or_operation = "/repos/tailrocks/velnor/rulesets/1".to_owned();
        clone_request.api_request_id = "api-same-repository-ruleset-clone".to_owned();
        same_repository_endpoint_clone
            .collector_snapshot
            .raw_objects
            .push(clone_raw);
        same_repository_endpoint_clone
            .collector_snapshot
            .requests
            .push(clone_request);
        same_repository_endpoint_clone
            .collector_snapshot
            .dependency_graph
            .nodes
            .iter_mut()
            .find(|node| node.kind == "check" && node.repository == CANONICAL_REPOSITORIES[0])
            .expect("complete fixture check graph node")
            .raw_object_refs = vec![clone_raw_id];
        refresh_typed_inventory_bytes(&mut same_repository_endpoint_clone);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&same_repository_endpoint_clone),
            &mut findings,
        );
        assert!(
            g0_codes(&findings).contains("g0-dependency-source"),
            "same-repository endpoint clone unexpectedly bound graph source: {findings:?}"
        );

        let mut graph_target_tampered = complete_g0_fixture().2;
        graph_target_tampered
            .collector_snapshot
            .dependency_graph
            .edges[0]
            .target_source_ref = "refs/heads/stale".to_owned();
        refresh_typed_inventory_bytes(&mut graph_target_tampered);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&graph_target_tampered),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-dependency-edge"));

        let mut foreign_graph_target = complete_g0_fixture().2;
        let target = foreign_graph_target
            .collector_snapshot
            .dependency_graph
            .nodes
            .iter_mut()
            .find(|node| node.kind == "check")
            .expect("complete fixture has a check node");
        target.repository = "foreign/repository".to_owned();
        refresh_typed_inventory_bytes(&mut foreign_graph_target);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&foreign_graph_target),
            &mut findings,
        );
        assert!(g0_codes(&findings).contains("g0-dependency-edge"));

        let mut scope_tampered = inventory;
        scope_tampered.collector_snapshot.repositories.pop();
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&scope_tampered), &mut findings);
        assert!(g0_codes(&findings).contains("g0-scope"));

        let mut omitted_job = complete_g0_fixture().2;
        let source = &mut omitted_job.collector_snapshot.repositories[0].workflows[0].source;
        let bytes = b"on: [push]\njobs: {}\n";
        source.bytes_base64 = BASE64.encode(bytes);
        source.byte_length = bytes.len() as u64;
        source.sha256 = digest_bytes(bytes);
        refresh_typed_inventory_bytes(&mut omitted_job);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&omitted_job), &mut findings);
        assert!(
            g0_codes(&findings).contains("g0-workflow-source")
                || g0_codes(&findings).contains("g0-workflow-derivation")
        );

        let mut omitted_child_source = complete_g0_fixture().2;
        let source =
            &mut omitted_child_source.collector_snapshot.repositories[0].workflows[0].source;
        let bytes = b"on: [push]\njobs:\n  scan:\n    uses: ./.github/workflows/missing.yml@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        source.bytes_base64 = BASE64.encode(bytes);
        source.byte_length = bytes.len() as u64;
        source.sha256 = digest_bytes(bytes);
        refresh_typed_inventory_bytes(&mut omitted_child_source);
        findings.clear();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&omitted_child_source),
            &mut findings,
        );
        assert!(
            g0_codes(&findings).contains("g0-workflow-source")
                || g0_codes(&findings).contains("g0-workflow-derivation")
        );

        let mut evil_api = complete_g0_fixture().2;
        evil_api.collector_snapshot.collector.api_base =
            "https://api.github.com.evil.example".to_owned();
        refresh_typed_inventory_bytes(&mut evil_api);
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&evil_api), &mut findings);
        assert!(g0_codes(&findings).contains("g0-collector-source"));
    }

    #[test]
    fn g0_source_join_bindings_are_typed_and_repository_specific() {
        let (manifest, snapshot, inventory) = complete_g0_fixture();
        let mut baseline_findings = Vec::new();
        check_g0_inventory(
            &manifest,
            &snapshot,
            Some(&inventory),
            &mut baseline_findings,
        );
        assert!(
            baseline_findings.is_empty(),
            "unexpected findings: {baseline_findings:?}"
        );

        let assert_rejected = |mut candidate: G0InventoryEvidence, code: &str| {
            refresh_typed_inventory_bytes(&mut candidate);
            let mut findings = Vec::new();
            check_g0_inventory(&manifest, &snapshot, Some(&candidate), &mut findings);
            assert!(
                g0_codes(&findings).contains(code),
                "expected {code}, got {findings:?}"
            );
        };

        let mut wrong_model_kind = inventory.clone();
        wrong_model_kind
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == "raw-model-session")
            .expect("model session raw object")
            .object_kind = "repository".to_owned();
        assert_rejected(wrong_model_kind, "g0-model-session");

        let mut wrong_artifact_kind = inventory.clone();
        wrong_artifact_kind
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == "raw-workload-artifact")
            .expect("workload artifact raw object")
            .object_kind = "repository".to_owned();
        assert_rejected(wrong_artifact_kind, "g0-workload-artifact");

        let mut cross_repository_access = inventory.clone();
        cross_repository_access.collector_snapshot.access[0].raw_object_refs =
            vec!["raw-repository-2".to_owned()];
        assert_rejected(cross_repository_access, "g0-access");

        let mut generic_graph_raw = inventory.clone();
        generic_graph_raw.collector_snapshot.dependency_graph.nodes[0].raw_object_refs =
            vec!["raw-repository-1".to_owned()];
        assert_rejected(generic_graph_raw, "g0-dependency-source");

        let mut generic_workflow_raw = inventory;
        generic_workflow_raw
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == "raw-workflow-1")
            .expect("workflow source raw object")
            .object_kind = "repository".to_owned();
        assert_rejected(generic_workflow_raw, "g0-workflow-source");

        let (_, _, mut missing_pr_source_binding) = complete_g0_fixture();
        missing_pr_source_binding.collector_snapshot.repositories[0].open_prs[0]
            .workflow_bindings[0]
            .workflow_revision = sha('f');
        assert_rejected(missing_pr_source_binding, "g0-pr-workflow-binding");
    }

    #[test]
    fn complete_g0_fixture_round_trips_through_public_check_paths() {
        let (mut manifest, mut snapshot, mut inventory) = complete_g0_fixture();
        // Add one independently captured provider chain to the serialized
        // collector.  The 32-repository fixture remains synthetic, but this
        // path proves the public JSON entrypoint consumes real raw selection
        // rather than only checking generated fixture rows.
        let real_source_sha = "df9fb272c025f76cc8711560209afcdfd6cc4e00";
        let real_check_endpoint =
            format!("/repos/tailrocks/velnor/commits/{real_source_sha}/check-runs");
        let real_check_bytes = real_api_fixture_entry(
            "raw/velnor/commit-df9fb272c025f76cc8711560209afcdfd6cc4e00/check-runs/page-0001.body",
        );
        let real_suite_bytes = BASE64
            .decode(
                include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/suites/velnor-96108219979.body.base64"
                )
                .trim(),
            )
            .expect("captured DCO suite base64");
        let real_app_bytes = BASE64
            .decode(
                include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/apps/dco-2.body.base64"
                )
                .trim(),
            )
            .expect("captured DCO App base64");
        let real_run_bytes = real_api_fixture_entry("raw/velnor/run-35493166478/run.body");
        let real_attempt_bytes =
            real_api_fixture_entry("raw/velnor/run-35493166478/attempt-1/attempt.body");
        let real_jobs_bytes =
            real_api_fixture_entry("raw/velnor/run-35493166478/attempt-1/jobs/page-0001.body");
        let real_artifacts_bytes =
            real_api_fixture_entry("raw/velnor/run-35493166478/artifacts/page-0001.body");
        let mut real_check_request =
            captured_page_request(&real_check_endpoint, "per_page=100&filter=all&page=1", 70);
        real_check_request.request_id = "real-public-checks-request".to_owned();
        real_check_request.response_raw_ref = "real-public-checks-raw".to_owned();
        let mut real_suite_request =
            captured_page_request("/repos/tailrocks/velnor/check-suites/96108219979", "", 1);
        real_suite_request.request_id = "real-public-suite-request".to_owned();
        real_suite_request.response_raw_ref = "real-public-suite-raw".to_owned();
        let mut real_app_request = captured_page_request("/apps/dco-2", "", 1);
        real_app_request.request_id = "real-public-app-request".to_owned();
        real_app_request.response_raw_ref = "real-public-app-raw".to_owned();
        let capture = |raw_id: &str,
                       request_id: &str,
                       object_kind: &str,
                       endpoint: &str,
                       query: &str,
                       items: u32,
                       bytes: &[u8]| {
            let mut request = captured_page_request(endpoint, query, items);
            request.request_id = request_id.to_owned();
            request.response_raw_ref = raw_id.to_owned();
            let raw = captured_raw_reference(raw_id, request_id, object_kind, bytes);
            (request, raw)
        };
        let (real_run_request, real_run_raw) = capture(
            "real-public-run-raw",
            "real-public-run-request",
            "workflow_run",
            "/repos/tailrocks/velnor/actions/runs/35493166478",
            "",
            1,
            &real_run_bytes,
        );
        let (real_attempt_request, real_attempt_raw) = capture(
            "real-public-attempt-raw",
            "real-public-attempt-request",
            "workflow_attempt",
            "/repos/tailrocks/velnor/actions/runs/35493166478/attempts/1",
            "",
            1,
            &real_attempt_bytes,
        );
        let (real_jobs_request, real_jobs_raw) = capture(
            "real-public-jobs-raw",
            "real-public-jobs-request",
            "workflow_attempt_jobs",
            "/repos/tailrocks/velnor/actions/runs/35493166478/attempts/1/jobs",
            "per_page=100&page=1",
            68,
            &real_jobs_bytes,
        );
        let (real_artifacts_request, real_artifacts_raw) = capture(
            "real-public-artifacts-raw",
            "real-public-artifacts-request",
            "workflow_artifacts",
            "/repos/tailrocks/velnor/actions/runs/35493166478/artifacts",
            "per_page=100&page=1",
            2,
            &real_artifacts_bytes,
        );
        inventory.collector_snapshot.requests.extend([
            real_check_request.clone(),
            real_suite_request.clone(),
            real_app_request.clone(),
            real_run_request.clone(),
            real_attempt_request.clone(),
            real_jobs_request.clone(),
            real_artifacts_request.clone(),
        ]);
        inventory.collector_snapshot.raw_objects.extend([
            captured_raw_reference(
                "real-public-checks-raw",
                "real-public-checks-request",
                "check_runs",
                &real_check_bytes,
            ),
            captured_raw_reference(
                "real-public-suite-raw",
                "real-public-suite-request",
                "check_suite",
                &real_suite_bytes,
            ),
            captured_raw_reference(
                "real-public-app-raw",
                "real-public-app-request",
                "app",
                &real_app_bytes,
            ),
            real_run_raw,
            real_attempt_raw,
            real_jobs_raw,
            real_artifacts_raw,
        ]);
        let real_dco_check = G0CheckProducer {
            context: "DCO".to_owned(),
            app_id: "974774".to_owned(),
            app_slug: "dco-2".to_owned(),
            provider: G0CheckProvider::ExternalApp,
            api: G0ApiKind::Rest,
            check_suite_id: 96_108_219_979,
            check_run_id: 106_031_458_188,
            source_sha: real_source_sha.to_owned(),
            event: "pull_request".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            html_url: "https://github.com/tailrocks/velnor/runs/106031458188".to_owned(),
            raw_object_refs: vec![
                "real-public-checks-raw".to_owned(),
                "real-public-suite-raw".to_owned(),
                "real-public-app-raw".to_owned(),
            ],
        };
        // Bind the captured chain into the typed PR producer consumed by the
        // public checker.  A free-standing raw-object tuple is insufficient:
        // semantic validation must reach the producer's suite/App identities.
        manifest.repositories[0].required_check_contexts_and_apps = vec![RequiredContext {
            context: "DCO".to_owned(),
            app_id: "974774".to_owned(),
        }];
        snapshot.repositories[0].ruleset.required_checks = manifest.repositories[0]
            .required_check_contexts_and_apps
            .clone();
        snapshot.repositories[0].open_prs[0].head_sha = real_source_sha.to_owned();
        snapshot.repositories[0].open_prs[0].executions[0].trigger_source_sha =
            real_source_sha.to_owned();
        snapshot.repositories[0].open_prs[0].executions[0].required_checks =
            vec![CheckObservation {
                context: "DCO".to_owned(),
                app_id: "974774".to_owned(),
                status: "completed".to_owned(),
                conclusion: "success".to_owned(),
                run_id: 50_000 + 1,
                job_id: String::new(),
                source_url: real_dco_check.html_url.clone(),
                event: "pull_request".to_owned(),
            }];
        inventory.collector_snapshot.repositories[0].rulesets[0].required_checks =
            vec![G0RequiredCheckPolicy {
                context: "DCO".to_owned(),
                app_id: "974774".to_owned(),
                ruleset_id: inventory.collector_snapshot.repositories[0].rulesets[0].ruleset_id,
                raw_object_refs: vec!["raw-repository-1".to_owned()],
            }];
        inventory.collector_snapshot.repositories[0].open_prs[0].head_sha =
            real_source_sha.to_owned();
        inventory.collector_snapshot.repositories[0].open_prs[0].workflow_bindings[0].source_sha =
            real_source_sha.to_owned();
        inventory.collector_snapshot.repositories[0].open_prs[0].required_check_producers =
            vec![real_dco_check.clone()];
        let serialized_collector =
            serde_json::to_vec(&inventory.collector_snapshot).expect("serialize actual chain");
        let round_tripped_collector: G0CollectorSnapshot =
            serde_json::from_slice(&serialized_collector).expect("deserialize actual chain");
        let selected_check = g0_capture_raw_json(
            &["real-public-checks-raw".to_owned()],
            "check_runs",
            106_031_458_188,
            std::slice::from_ref(&real_check_endpoint),
            &round_tripped_collector.requests,
            &round_tripped_collector.raw_objects,
        )
        .expect("public serialized path selects captured DCO check");
        assert_eq!(g0_json_u64(&selected_check, &["id"]), Some(106_031_458_188));
        assert!(g0_capture_raw_json(
            &["real-public-suite-raw".to_owned()],
            "check_suite",
            96_108_219_979,
            &["/repos/tailrocks/velnor/check-suites/96108219979".to_owned()],
            &round_tripped_collector.requests,
            &round_tripped_collector.raw_objects,
        )
        .is_some());
        assert!(g0_capture_raw_json(
            &["real-public-app-raw".to_owned()],
            "app",
            974_774,
            &["/apps/dco-2".to_owned()],
            &round_tripped_collector.requests,
            &round_tripped_collector.raw_objects,
        )
        .is_some());
        assert!(g0_capture_raw_json(
            &["real-public-run-raw".to_owned()],
            "workflow_run",
            35_493_166_478,
            &["/repos/tailrocks/velnor/actions/runs/35493166478".to_owned()],
            &round_tripped_collector.requests,
            &round_tripped_collector.raw_objects,
        )
        .is_some());
        assert!(g0_capture_raw_json(
            &["real-public-attempt-raw".to_owned()],
            "workflow_attempt",
            35_493_166_478,
            &["/repos/tailrocks/velnor/actions/runs/35493166478/attempts/1".to_owned()],
            &round_tripped_collector.requests,
            &round_tripped_collector.raw_objects,
        )
        .is_some());
        let real_jobs: Value =
            serde_json::from_slice(&real_jobs_bytes).expect("captured Actions jobs page JSON");
        let qualified_job_id = real_jobs["jobs"][0]["id"]
            .as_u64()
            .expect("captured qualified job ID");
        let selected_job = g0_capture_raw_json(
            &["real-public-jobs-raw".to_owned()],
            "workflow_attempt_jobs",
            qualified_job_id,
            &["/repos/tailrocks/velnor/actions/runs/35493166478/attempts/1/jobs".to_owned()],
            &round_tripped_collector.requests,
            &round_tripped_collector.raw_objects,
        )
        .expect("public serialized path selects qualified job from captured page");
        assert_eq!(
            g0_json_u64(&selected_job, &["run_id"]),
            Some(35_493_166_478)
        );
        assert_eq!(g0_json_u64(&selected_job, &["run_attempt"]), Some(1));
        assert!(
            g0_json_string(&selected_job, &["html_url"]).is_some_and(|url| {
                url.starts_with("https://github.com/tailrocks/velnor/actions/runs/35493166478/job/")
            })
        );
        let real_artifacts: Value = serde_json::from_slice(&real_artifacts_bytes)
            .expect("captured Actions artifacts page JSON");
        let qualified_artifact_id = real_artifacts["artifacts"][0]["id"]
            .as_u64()
            .expect("captured qualified artifact ID");
        assert!(g0_capture_raw_json(
            &["real-public-artifacts-raw".to_owned()],
            "workflow_artifacts",
            qualified_artifact_id,
            &["/repos/tailrocks/velnor/actions/runs/35493166478/artifacts".to_owned()],
            &round_tripped_collector.requests,
            &round_tripped_collector.raw_objects,
        )
        .is_some());
        inventory = typed_inventory(round_tripped_collector);
        let baseline_inventory = inventory.clone();
        let records = manifest
            .repositories
            .iter()
            .map(|repo| {
                let snapshot_repo = snapshot
                    .repositories
                    .iter()
                    .find(|candidate| candidate.repository == repo.repository)
                    .expect("complete fixture has every snapshot repository");
                EvidenceRecord {
                    repository: repo.repository.clone(),
                    repository_role: repo.repository_role.clone(),
                    evidence_role: EvidenceRole::Inventory,
                    default_branch: repo.default_branch.clone(),
                    default_branch_sha: snapshot_repo.default_branch_sha.clone(),
                    observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                    generator_revision: repo.generator_revision.clone(),
                    runtime_product_id: repo.runtime_product_id.clone(),
                    generator_artifact_digest: repo.generator_artifact_digest.clone(),
                    configuration_digest: repo.configuration_digest.clone(),
                    generated_tree_digest: repo.generated_tree_digest.clone(),
                    scan_state_digest: repo.scan_state_digest.clone(),
                    runtime_release_version: repo.runtime_release_version.clone(),
                    runtime_source_sha: repo.runtime_source_sha.clone(),
                    job_image_digest: repo.job_image_digest.clone(),
                    expected_workload_ids: repo.expected_workload_ids.clone(),
                    required_check_contexts_and_apps: repo.required_check_contexts_and_apps.clone(),
                    workload_platform_architecture: repo.workload_platform_architecture.clone(),
                    provider_eligibility: repo.provider_eligibility.clone(),
                    workflow_path: repo.workflow_path.clone(),
                    workflow_revision: repo.workflow_revision.clone(),
                    provider: "inventory".to_owned(),
                    event: "inventory".to_owned(),
                    gate_status: "inventory".to_owned(),
                    ..EvidenceRecord::default()
                }
            })
            .collect();
        let evidence = EvidenceDocument {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            manifest_id: manifest.manifest_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            stage: "G0".to_owned(),
            records,
            reviewer_attestation: None,
            g0_inventory: Some(inventory),
        };
        let directory = std::fs::canonicalize(std::env::temp_dir())
            .expect("canonical temp directory")
            .join(format!("velnor-g0-public-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create fixture directory");
        let manifest_path = directory.join("manifest.json");
        let snapshot_path = directory.join("snapshot.json");
        let evidence_path = directory.join("evidence.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("serialize manifest"),
        )
        .expect("write manifest");
        std::fs::write(
            &snapshot_path,
            serde_json::to_vec(&snapshot).expect("serialize snapshot"),
        )
        .expect("write snapshot");
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&evidence).expect("serialize evidence"),
        )
        .expect("write evidence");
        let report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses serialized fixture");
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "offline-validation-only"));
        assert!(!report.findings.iter().any(|finding| {
            matches!(
                finding.code.as_str(),
                "g0-endpoint-contract" | "g0-request-query" | "g0-raw-object"
            )
        }));
        assert!(!report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-check-raw-evidence"));
        assert_eq!(report.status, "fail");
        assert_eq!(report.mode, "offline");

        let mut unknown_actual = baseline_inventory.clone();
        unknown_actual
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == "real-public-jobs-raw")
            .expect("captured jobs raw object")
            .object_kind = "unknown.provider.response".to_owned();
        refresh_typed_inventory_bytes(&mut unknown_actual);
        let mut unknown_evidence = evidence.clone();
        unknown_evidence.g0_inventory = Some(unknown_actual);
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&unknown_evidence).expect("serialize unknown-kind mutation"),
        )
        .expect("write unknown-kind mutation");
        let unknown_report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses unknown-kind mutation");
        assert!(unknown_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-endpoint-contract"));

        let mut missing_actual = baseline_inventory.clone();
        missing_actual
            .collector_snapshot
            .raw_objects
            .retain(|raw| raw.raw_id != "real-public-attempt-raw");
        refresh_typed_inventory_bytes(&mut missing_actual);
        let mut missing_evidence = evidence.clone();
        missing_evidence.g0_inventory = Some(missing_actual);
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&missing_evidence).expect("serialize missing-raw mutation"),
        )
        .expect("write missing-raw mutation");
        let missing_report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses missing-raw mutation");
        assert!(missing_report.findings.iter().any(|finding| {
            matches!(
                finding.code.as_str(),
                "g0-request-incomplete" | "g0-raw-reference"
            )
        }));

        // A raw response copied under a different request identity must not
        // self-attest its source merely because its bytes and digest remain
        // unchanged.  Exercise the serialized public entrypoint so this
        // cannot regress to a helper-only provenance check.
        let mut cross_request_inventory = baseline_inventory.clone();
        cross_request_inventory
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == "raw-workflow-1")
            .expect("workflow source raw object")
            .request_id = "request-repository-2".to_owned();
        refresh_typed_inventory_bytes(&mut cross_request_inventory);
        let mut cross_request_evidence = evidence.clone();
        cross_request_evidence.g0_inventory = Some(cross_request_inventory);
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&cross_request_evidence).expect("serialize cross-request mutation"),
        )
        .expect("write cross-request mutation");
        let cross_request_report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses cross-request mutation");
        assert!(cross_request_report.findings.iter().any(|finding| {
            matches!(
                finding.code.as_str(),
                "g0-raw-request-binding" | "g0-workflow-source"
            )
        }));

        // A same-byte source response from another repository is not a valid
        // graph source.  Raw digest equality is insufficient without source
        // identity and request ancestry.
        let mut cross_repository_graph = baseline_inventory.clone();
        cross_repository_graph
            .collector_snapshot
            .dependency_graph
            .nodes[0]
            .raw_object_refs = vec!["raw-workflow-2".to_owned()];
        refresh_typed_inventory_bytes(&mut cross_repository_graph);
        let mut cross_repository_evidence = evidence.clone();
        cross_repository_evidence.g0_inventory = Some(cross_repository_graph);
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&cross_repository_evidence)
                .expect("serialize cross-repository graph mutation"),
        )
        .expect("write cross-repository graph mutation");
        let cross_repository_report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses cross-repository graph mutation");
        assert!(cross_repository_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-dependency-source"));

        // Rehashing a captured suite body must not make a wrong App/member
        // identity valid.  This mutation traverses the same serialized
        // public path and should fail semantic producer binding, not merely
        // byte-integrity validation.
        let mut tampered_inventory = baseline_inventory.clone();
        let suite_index = tampered_inventory
            .collector_snapshot
            .raw_objects
            .iter()
            .position(|raw| raw.raw_id == "real-public-suite-raw")
            .expect("serialized DCO suite raw object");
        let suite_bytes = BASE64
            .decode(&tampered_inventory.collector_snapshot.raw_objects[suite_index].bytes_base64)
            .expect("serialized DCO suite bytes");
        let mut tampered_suite: Value =
            serde_json::from_slice(&suite_bytes).expect("serialized DCO suite JSON");
        tampered_suite["app"]["id"] = json!(12_526);
        tampered_suite["app"]["slug"] = json!("sonarqubecloud");
        let tampered_suite_bytes = canonical_json(&tampered_suite).into_bytes();
        tampered_inventory.collector_snapshot.raw_objects[suite_index] = captured_raw_reference(
            "real-public-suite-raw",
            "real-public-suite-request",
            "check_suite",
            &tampered_suite_bytes,
        );
        refresh_typed_inventory_bytes(&mut tampered_inventory);
        let mut tampered_evidence = evidence.clone();
        tampered_evidence.g0_inventory = Some(tampered_inventory);
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&tampered_evidence).expect("serialize semantic mutation"),
        )
        .expect("write semantic mutation");
        let tampered_report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses semantic mutation");
        assert!(tampered_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-check-raw-evidence"));

        let mut graph_inventory = baseline_inventory.clone();
        graph_inventory.collector_snapshot.dependency_graph.edges[0].target_source_ref =
            "refs/heads/foreign".to_owned();
        refresh_typed_inventory_bytes(&mut graph_inventory);
        let mut graph_evidence = evidence.clone();
        graph_evidence.g0_inventory = Some(graph_inventory);
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&graph_evidence).expect("serialize graph mutation"),
        )
        .expect("write graph mutation");
        let graph_report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses graph mutation");
        assert!(graph_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-dependency-edge"));

        // Recompute the outer caller-visible envelope digest after changing one
        // raw response, but retain the captured raw digest.
        let mut mutated_inventory = baseline_inventory;
        let raw = &mut mutated_inventory.collector_snapshot.raw_objects[0];
        raw.bytes_base64 = BASE64.encode(b"tampered");
        raw.byte_length = 8;
        let value = serde_json::to_value(&mutated_inventory.collector_snapshot)
            .expect("serialize mutated snapshot");
        let bytes = canonical_json(&value).into_bytes();
        mutated_inventory.collector_snapshot_bytes_base64 = BASE64.encode(&bytes);
        mutated_inventory.collector_snapshot_sha256 = digest_bytes(&bytes);
        let mut mutated_evidence = evidence;
        mutated_evidence.g0_inventory = Some(mutated_inventory);
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&mutated_evidence).expect("serialize mutated evidence"),
        )
        .expect("write mutated evidence");
        let mutated_report = check_paths(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path,
            snapshot: snapshot_path,
            evidence: evidence_path,
            release_manifest: None,
            live: false,
        })
        .expect("public checker parses mutated fixture");
        assert!(mutated_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-raw-object"));
        assert!(mutated_report
            .findings
            .iter()
            .any(|finding| finding.code == "g0-collector-storage"));
        std::fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[tokio::test]
    async fn public_evidence_command_rejects_recursive_child_matrix() {
        let (mut manifest, snapshot, mut inventory) = complete_g0_fixture();
        let repository = manifest.repositories[0].repository.clone();
        manifest.repositories[0].expected_jobs[0].child_workflow = Some(ChildWorkflowSpec {
            repository: repository.clone(),
            workflow_path: ".github/workflows/reusable.yml".to_owned(),
            event: "workflow_call".to_owned(),
        });

        let workflow = &mut inventory.collector_snapshot.repositories[0].workflows[0];
        let root_yaml = format!(
            "on: [push]\njobs:\n  scan:\n    uses: ./.github/workflows/reusable.yml@{}\n",
            sha('a')
        );
        workflow.source.bytes_base64 = BASE64.encode(root_yaml.as_bytes());
        workflow.source.byte_length = root_yaml.len() as u64;
        workflow.source.sha256 = digest_bytes(root_yaml.as_bytes());
        workflow.source.storage_ref = format!(
            "sha256://{}",
            workflow
                .source
                .sha256
                .strip_prefix("sha256:")
                .expect("root workflow digest has prefix")
        );
        let root_raw_id = workflow.source.raw_object_refs[0].clone();
        let root_raw = inventory
            .collector_snapshot
            .raw_objects
            .iter_mut()
            .find(|raw| raw.raw_id == root_raw_id)
            .expect("complete fixture has workflow raw object");
        root_raw.bytes_base64 = BASE64.encode(root_yaml.as_bytes());
        root_raw.byte_length = root_yaml.len() as u64;
        root_raw.sha256 = digest_bytes(root_yaml.as_bytes());
        root_raw.storage_ref = format!(
            "sha256://{}",
            root_raw
                .sha256
                .strip_prefix("sha256:")
                .expect("root raw digest has prefix")
        );
        root_raw.original_sha256 = digest_bytes(root_yaml.as_bytes());
        root_raw.original_byte_length = root_yaml.len() as u64;
        root_raw.original_storage_ref = format!(
            "sha256://{}",
            root_raw
                .original_sha256
                .strip_prefix("sha256:")
                .expect("root original digest has prefix")
        );

        let child_yaml = b"on:\n  workflow_call: {}\njobs:\n  nested:\n    strategy:\n      matrix:\n        os: [ubuntu-24.04, ubuntu-22.04]\n    runs-on: ${{ matrix.os }}\n    steps: []\n";
        let child_digest = digest_bytes(child_yaml);
        let child_raw_id = "raw-child-workflow-1".to_owned();
        let child_request_id = "request-child-workflow-1".to_owned();
        inventory
            .collector_snapshot
            .raw_objects
            .push(G0RawObjectRef {
                raw_id: child_raw_id.clone(),
                request_id: child_request_id.clone(),
                object_kind: "workflow.dependency.source".to_owned(),
                canonicalization: "raw-utf8".to_owned(),
                sha256: child_digest.clone(),
                byte_length: child_yaml.len() as u64,
                bytes_base64: BASE64.encode(child_yaml),
                media_type: "text/yaml".to_owned(),
                storage_ref: format!(
                    "sha256://{}",
                    child_digest
                        .strip_prefix("sha256:")
                        .expect("child digest has prefix")
                ),
                original_sha256: child_digest.clone(),
                original_byte_length: child_yaml.len() as u64,
                original_storage_ref: format!(
                    "sha256://{}",
                    child_digest
                        .strip_prefix("sha256:")
                        .expect("child digest has prefix")
                ),
            });
        let mut child_request = inventory.collector_snapshot.requests[0].clone();
        child_request.request_id = child_request_id;
        child_request.endpoint_or_operation =
            format!("/repos/{repository}/contents/.github/workflows/reusable.yml");
        child_request.api_request_id = "api-child-workflow-1".to_owned();
        child_request.response_raw_ref = child_raw_id.clone();
        inventory.collector_snapshot.requests.push(child_request);

        workflow.reusable_workflows = vec![G0WorkflowDependency {
            kind: "reusable_workflow".to_owned(),
            source: G0WorkflowSource {
                repository: repository.clone(),
                path: ".github/workflows/reusable.yml".to_owned(),
                revision: sha('a'),
                source_sha: sha('a'),
                source_url: format!(
                    "https://github.com/{repository}/blob/{}/.github/workflows/reusable.yml",
                    sha('a')
                ),
                media_type: "text/yaml".to_owned(),
                canonicalization: "raw-utf8".to_owned(),
                sha256: child_digest.clone(),
                storage_ref: format!(
                    "sha256://{}",
                    child_digest
                        .strip_prefix("sha256:")
                        .expect("child digest has prefix")
                ),
                byte_length: child_yaml.len() as u64,
                bytes_base64: BASE64.encode(child_yaml),
                raw_object_refs: vec![child_raw_id],
            },
        }];
        refresh_typed_inventory_bytes(&mut inventory);

        let directory = std::fs::canonicalize(std::env::temp_dir())
            .expect("canonical temp directory")
            .join(format!("velnor-g0-child-matrix-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create fixture directory");
        let manifest_path = directory.join("manifest.json");
        let snapshot_path = directory.join("snapshot.json");
        let evidence_path = directory.join("evidence.json");
        let evidence = EvidenceDocument {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            manifest_id: manifest.manifest_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            stage: "G0".to_owned(),
            records: Vec::new(),
            reviewer_attestation: None,
            g0_inventory: Some(inventory),
        };
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("serialize manifest"),
        )
        .expect("write manifest");
        std::fs::write(
            &snapshot_path,
            serde_json::to_vec(&snapshot).expect("serialize snapshot"),
        )
        .expect("write snapshot");
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&evidence).expect("serialize evidence"),
        )
        .expect("write evidence");

        let input = EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path.clone(),
            snapshot: snapshot_path.clone(),
            evidence: evidence_path.clone(),
            release_manifest: None,
            live: false,
        };
        let report = check_paths(&input).expect("public checker parses child matrix fixture");
        assert!(
            report.findings.iter().any(|finding| {
                finding.code == "g0-workflow-derivation"
                    && finding.message.contains("concrete matrix identity")
            }),
            "unexpected child-matrix findings: {:?}",
            report.findings
        );
        let command_error = evidence_check(EvidenceCheckArgs {
            stage: input.stage,
            manifest: input.manifest,
            snapshot: input.snapshot,
            evidence: input.evidence,
            release_manifest: None,
            live: false,
            json: true,
        })
        .await
        .expect_err("public evidence command must reject child matrix collapse");
        assert!(command_error.to_string().contains("evidence gate failed"));
        std::fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[test]
    fn nested_child_runs_require_source_derived_parent_edges() {
        let (mut manifest, _snapshot, mut inventory) = complete_g0_fixture();
        let manifest_repo = &mut manifest.repositories[0];
        let repository = manifest_repo.repository.clone();
        manifest_repo.expected_jobs[0].child_workflow = Some(ChildWorkflowSpec {
            repository: repository.clone(),
            workflow_path: ".github/workflows/reusable.yml".to_owned(),
            event: "workflow_call".to_owned(),
        });

        let workflow = &mut inventory.collector_snapshot.repositories[0].workflows[0];
        let root_source = &mut workflow.source;
        let root_yaml = b"on: [push]\njobs:\n  scan:\n    uses: ./.github/workflows/reusable.yml@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        root_source.bytes_base64 = BASE64.encode(root_yaml);
        root_source.byte_length = root_yaml.len() as u64;
        root_source.sha256 = digest_bytes(root_yaml);
        root_source.source_url = format!(
            "https://github.com/{repository}/blob/{}/{}",
            root_source.source_sha, root_source.path
        );

        let mut reusable = root_source.clone();
        reusable.path = ".github/workflows/reusable.yml".to_owned();
        reusable.revision = sha('b');
        reusable.source_sha = sha('b');
        reusable.source_url = format!(
            "https://github.com/{repository}/blob/{}/{}",
            reusable.source_sha, reusable.path
        );
        let reusable_yaml = b"on: {workflow_call: {}}\njobs:\n  nested:\n    uses: ./.github/workflows/deep.yml@cccccccccccccccccccccccccccccccccccccccc\n";
        reusable.bytes_base64 = BASE64.encode(reusable_yaml);
        reusable.byte_length = reusable_yaml.len() as u64;
        reusable.sha256 = digest_bytes(reusable_yaml);

        let mut deep = reusable.clone();
        deep.path = ".github/workflows/deep.yml".to_owned();
        deep.revision = sha('c');
        deep.source_sha = sha('c');
        deep.source_url = format!(
            "https://github.com/{repository}/blob/{}/{}",
            deep.source_sha, deep.path
        );
        let deep_yaml =
            b"on: {workflow_call: {}}\njobs:\n  deep:\n    runs-on: ubuntu-24.04\n    steps: []\n";
        deep.bytes_base64 = BASE64.encode(deep_yaml);
        deep.byte_length = deep_yaml.len() as u64;
        deep.sha256 = digest_bytes(deep_yaml);
        workflow.reusable_workflows = vec![
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: reusable,
            },
            G0WorkflowDependency {
                kind: "reusable_workflow".to_owned(),
                source: deep,
            },
        ];

        let mut child_execution = ExecutionObservation {
            run_id: 101,
            run_attempt: 1,
            run_url: format!("https://github.com/{repository}/actions/runs/101"),
            workflow_path: manifest_repo.workflow_path.clone(),
            workflow_revision: sha('a'),
            event: "push".to_owned(),
            trigger_source_sha: sha('a'),
            actual_checkout_sha: sha('a'),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            provider: "github".to_owned(),
            ..Default::default()
        };
        let child_a = ChildRunObservation {
            parent_run_id: child_execution.run_id,
            run_id: 102,
            run_attempt: 1,
            repository: repository.clone(),
            workflow_path: ".github/workflows/reusable.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('b'),
            provider: "github".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            source_url: format!("https://github.com/{repository}/actions/runs/102"),
        };
        let child_b = ChildRunObservation {
            parent_run_id: child_a.run_id,
            run_id: 103,
            run_attempt: 1,
            repository: repository.clone(),
            workflow_path: ".github/workflows/deep.yml".to_owned(),
            event: "workflow_call".to_owned(),
            source_sha: sha('c'),
            provider: "github".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            source_url: format!("https://github.com/{repository}/actions/runs/103"),
        };
        child_execution.child_runs = vec![child_a.clone(), child_b.clone()];
        let links = [child_a, child_b]
            .into_iter()
            .map(|child| ChildRunLink {
                parent_run_id: child.parent_run_id,
                run_id: child.run_id,
                run_attempt: child.run_attempt,
                repository: child.repository,
                workflow_path: child.workflow_path,
                event: child.event,
                source_sha: child.source_sha,
                provider: child.provider,
                status: child.status,
                conclusion: child.conclusion,
                run_url: child.source_url,
            })
            .collect();
        let record = EvidenceRecord {
            repository: repository.clone(),
            provider: "github".to_owned(),
            child_run_links: links,
            ..Default::default()
        };
        let mut findings = Vec::new();
        check_authoritative_children_from_source(
            manifest_repo,
            Some(&inventory),
            &record,
            &child_execution,
            &mut findings,
        );
        assert!(
            findings.is_empty(),
            "valid nested graph rejected: {findings:?}"
        );
        let mut observation_findings = Vec::new();
        check_execution_observation(&repository, &child_execution, &mut observation_findings);
        assert!(!observation_findings
            .iter()
            .any(|finding| finding.code == "child-run-conclusion"));

        let mut duplicate_links = record.clone();
        duplicate_links.child_run_links[1] = duplicate_links.child_run_links[0].clone();
        findings.clear();
        check_authoritative_children_from_source(
            manifest_repo,
            Some(&inventory),
            &duplicate_links,
            &child_execution,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "duplicate-child-run"));

        let mut wrong_parent = child_execution;
        wrong_parent.child_runs[1].parent_run_id = wrong_parent.run_id;
        findings.clear();
        check_authoritative_children_from_source(
            manifest_repo,
            Some(&inventory),
            &record,
            &wrong_parent,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "child-run-mismatch"));
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

    fn minimal_pr() -> SnapshotPullRequest {
        SnapshotPullRequest {
            number: 7,
            state: "open".to_owned(),
            draft: false,
            author: "author".to_owned(),
            author_association: "CONTRIBUTOR".to_owned(),
            head_repository: "owner/repo".to_owned(),
            head_sha: sha('a'),
            base_sha: sha('b'),
            merge_sha: Some(sha('c')),
            merge_group_sha: None,
            source_url: "https://github.com/owner/repo/pull/7".to_owned(),
            executions: Vec::new(),
        }
    }

    fn minimal_snapshot_for_coverage(pr: SnapshotPullRequest) -> SnapshotRepository {
        SnapshotRepository {
            repository: "owner/repo".to_owned(),
            repository_id: 1,
            default_branch: "main".to_owned(),
            default_branch_sha: sha('m'),
            ruleset: RulesetObservation {
                required_checks: Vec::new(),
                source_url: "https://github.com/owner/repo/settings/rules".to_owned(),
                pages_complete: true,
            },
            workflows: Vec::new(),
            main_executions: Vec::new(),
            open_prs: vec![pr],
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
    fn immutable_pr_subject_positive_and_head_mutation_negative() {
        let pr = minimal_pr();
        let record = EvidenceRecord {
            repository: "owner/repo".to_owned(),
            evidence_role: EvidenceRole::PullRequest,
            provider: "github".to_owned(),
            event: "pull_request".to_owned(),
            pr_number: Some(pr.number),
            pr_head_sha: Some(pr.head_sha.clone()),
            pr_base_sha: Some(pr.base_sha.clone()),
            tested_merge_sha: pr.merge_sha.clone(),
            ..Default::default()
        };
        assert!(record_matches_pr_subject(&record, &pr));

        let mut stale = record;
        stale.pr_head_sha = Some(sha('z'));
        assert!(!record_matches_pr_subject(&stale, &pr));
    }

    #[test]
    fn coverage_requires_default_branch_and_each_pr_subject() {
        let manifest_repo = ManifestRepository {
            repository: "owner/repo".to_owned(),
            provider_eligibility: BTreeMap::from([("github".to_owned(), Eligibility::Eligible)]),
            ..Default::default()
        };
        let snapshot_repo = minimal_snapshot_for_coverage(minimal_pr());
        let main = EvidenceRecord {
            repository: "owner/repo".to_owned(),
            evidence_role: EvidenceRole::DefaultBranch,
            provider: "github".to_owned(),
            event: "push".to_owned(),
            default_branch_sha: sha('m'),
            trigger_source_sha: sha('m'),
            actual_checkout_sha: sha('m'),
            ..Default::default()
        };
        let pr = minimal_pr();
        let pull_request = EvidenceRecord {
            repository: "owner/repo".to_owned(),
            evidence_role: EvidenceRole::PullRequest,
            provider: "github".to_owned(),
            event: "pull_request".to_owned(),
            pr_number: Some(pr.number),
            pr_head_sha: Some(pr.head_sha.clone()),
            pr_base_sha: Some(pr.base_sha.clone()),
            tested_merge_sha: pr.merge_sha.clone(),
            ..Default::default()
        };
        let manifest = BTreeMap::from([("owner/repo".to_owned(), &manifest_repo)]);
        let snapshot = BTreeMap::from([("owner/repo".to_owned(), &snapshot_repo)]);
        let mut records = BTreeMap::new();
        records.insert(RecordKey::from_record(&main), &main);
        records.insert(RecordKey::from_record(&pull_request), &pull_request);
        let mut findings = Vec::new();
        check_record_coverage(Stage::G1, &manifest, &snapshot, &records, &mut findings);
        assert!(!findings
            .iter()
            .any(|finding| finding.code == "missing-main-evidence"
                || finding.code == "missing-pr-evidence"));

        let mut records_without_pr = BTreeMap::new();
        records_without_pr.insert(RecordKey::from_record(&main), &main);
        findings.clear();
        check_record_coverage(
            Stage::G1,
            &manifest,
            &snapshot,
            &records_without_pr,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "missing-pr-evidence"));
    }

    #[test]
    fn lane_pairing_requires_same_immutable_subject() {
        let mut github = EvidenceRecord {
            repository: "owner/repo".to_owned(),
            evidence_role: EvidenceRole::PullRequest,
            provider: "github".to_owned(),
            event: "pull_request".to_owned(),
            pr_number: Some(7),
            pr_head_sha: Some(sha('a')),
            pr_base_sha: Some(sha('b')),
            tested_merge_sha: Some(sha('c')),
            trigger_source_sha: sha('a'),
            actual_checkout_sha: sha('c'),
            run_id: 1,
            ..Default::default()
        };
        let mut velnor = github.clone();
        velnor.provider = "velnor".to_owned();
        assert!(same_qualifying_subject(&github, &velnor));
        github.actual_checkout_sha = sha('d');
        assert!(!same_qualifying_subject(&github, &velnor));
    }

    #[test]
    fn run_attempt_is_part_of_authoritative_lookup() {
        let mut execution = minimal_execution();
        execution.run_attempt = 2;
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
            main_executions: vec![execution],
            open_prs: Vec::new(),
        };
        let record = EvidenceRecord {
            repository: "owner/repo".to_owned(),
            evidence_role: EvidenceRole::DefaultBranch,
            run_id: 1,
            run_attempt: 1,
            ..Default::default()
        };
        assert!(find_execution(&snapshot, &record).is_none());
    }

    #[test]
    fn g7_attestation_requires_external_artifact_bindings() {
        let manifest = ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            source: SourceIdentity {
                repository: REVIEWED_SOURCE_REPOSITORY.to_owned(),
                revision: REVIEWED_SOURCE_REVISION.to_owned(),
                digest: REVIEWED_SOURCE_DIGEST.to_owned(),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: Vec::new(),
        };
        let snapshot = SnapshotDocument {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            snapshot_id: "snapshot".to_owned(),
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            source: SnapshotSource {
                collector: "fixture".to_owned(),
                collector_revision: sha('a'),
                api_base: "https://api.github.com".to_owned(),
                captured_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                read_only: true,
                page_count: 1,
                permission_scopes: vec!["metadata:read".to_owned()],
            },
            repositories: Vec::new(),
        };
        let attestation = ReviewerAttestation {
            reviewer: "independent-reviewer".to_owned(),
            report_digest: digest('a'),
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            snapshot_id: "snapshot".to_owned(),
            attested_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            artifact: ReviewArtifact {
                source_repository: REVIEWED_SOURCE_REPOSITORY.to_owned(),
                source_revision: REVIEWED_SOURCE_REVISION.to_owned(),
                source_tree_digest: digest('b'),
                source_diff_digest: digest('c'),
                run_manifest_digest: digest('d'),
                source_url: "https://github.com/tailrocks/velnor-review".to_owned(),
            },
        };
        let mut findings = Vec::new();
        check_review_attestation(&manifest, &snapshot, &attestation, &mut findings);
        assert!(findings.is_empty());

        let mut unbound = attestation;
        unbound.artifact.run_manifest_digest = "unbound".to_owned();
        check_review_attestation(&manifest, &snapshot, &unbound, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "invalid-review-attestation"));
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
    fn fixed_scope_positive_control_has_exact_membership() {
        let repositories = CANONICAL_REPOSITORIES
            .iter()
            .map(|repository| ManifestRepository {
                repository: (*repository).to_owned(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let manifest = ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            source: SourceIdentity {
                repository: REVIEWED_SOURCE_REPOSITORY.to_owned(),
                revision: REVIEWED_SOURCE_REVISION.to_owned(),
                digest: REVIEWED_SOURCE_DIGEST.to_owned(),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories,
        };
        let mut findings = Vec::new();
        check_manifest(&manifest, &mut findings);
        assert_eq!(manifest.repositories.len(), REQUIRED_REPOSITORIES);
        assert!(!findings.iter().any(|finding| matches!(
            finding.code.as_str(),
            "manifest-count" | "manifest-scope" | "manifest-duplicate"
        )));
    }

    #[test]
    fn fixed_scope_rejects_same_count_substitution() {
        let mut repositories = CANONICAL_REPOSITORIES
            .iter()
            .map(|repository| ManifestRepository {
                repository: (*repository).to_owned(),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        repositories[0].repository = "unreviewed/substitute".to_owned();
        let manifest = ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            source: SourceIdentity {
                repository: REVIEWED_SOURCE_REPOSITORY.to_owned(),
                revision: REVIEWED_SOURCE_REVISION.to_owned(),
                digest: REVIEWED_SOURCE_DIGEST.to_owned(),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories,
        };
        let mut findings = Vec::new();
        check_manifest(&manifest, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "manifest-scope"));
    }

    #[test]
    fn exact_provider_keyset_positive_and_third_provider_negative() {
        let valid = BTreeMap::from([
            ("github".to_owned(), Eligibility::Eligible),
            ("velnor".to_owned(), Eligibility::Eligible),
        ]);
        let mut findings = Vec::new();
        check_eligibility("owner/repo", &valid, &mut findings);
        assert!(!findings
            .iter()
            .any(|finding| finding.code == "provider-keyset"));

        let mut extra = valid;
        extra.insert("third_party".to_owned(), Eligibility::Eligible);
        findings.clear();
        check_eligibility("owner/repo", &extra, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "provider-keyset"));
    }

    #[test]
    fn g0_completion_rejects_blocker_before_inventory_return() {
        let manifest = ManifestRepository::default();
        let snapshot = SnapshotRepository {
            repository: "owner/repo".to_owned(),
            repository_id: 0,
            default_branch: String::new(),
            default_branch_sha: String::new(),
            ruleset: RulesetObservation {
                required_checks: Vec::new(),
                source_url: String::new(),
                pages_complete: false,
            },
            workflows: Vec::new(),
            main_executions: Vec::new(),
            open_prs: Vec::new(),
        };
        let mut record = EvidenceRecord {
            repository: "owner/repo".to_owned(),
            blocker: Some("collector incomplete".to_owned()),
            ..Default::default()
        };
        record.provider_eligibility = BTreeMap::from([
            ("github".to_owned(), Eligibility::Eligible),
            ("velnor".to_owned(), Eligibility::Eligible),
        ]);
        let mut findings = Vec::new();
        check_record(
            Stage::G0,
            RepositoryIndex {
                manifest: &manifest,
                snapshot: &snapshot,
            },
            &record,
            None,
            None,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "unfinished-record"));
    }

    #[test]
    fn legacy_scalar_inventory_shape_is_rejected() {
        let legacy = json!({
            "source_url": "https://github.com/tailrocks/velnor/blob/reviewed",
            "repository_count": REQUIRED_REPOSITORIES,
            "open_pr_count": 0,
            "workflow_repository_count": 0,
            "ruleset_repository_count": 0,
            "workload_matrix_digest": digest('a'),
            "dependency_graph_digest": digest('a'),
            "access_scopes": ["metadata:read"],
            "access_gaps": [],
            "orchestrator_model": EXPECTED_ORCHESTRATOR_MODEL,
            "orchestrator_effort": EXPECTED_ORCHESTRATOR_EFFORT,
            "agent_model": EXPECTED_AGENT_MODEL,
            "agent_effort": EXPECTED_AGENT_EFFORT
        });
        assert!(serde_json::from_value::<G0InventoryEvidence>(legacy).is_err());
    }

    #[test]
    fn strict_json_rejects_duplicate_object_keys() {
        assert!(reject_duplicate_json_keys(br#"{"a":1,"a":2}"#).is_err());
        assert!(reject_duplicate_json_keys(br#"{"a":{"b":1},"c":[{"d":2}]}"#).is_ok());
    }

    #[test]
    fn g0_request_contract_rejects_unknown_provenance_field() {
        let mut request = json!({
            "request_id": "request-1",
            "api": "rest",
            "method": "GET",
            "endpoint_or_operation": "/repos/tailrocks/velnor",
            "query_base64": BASE64.encode(&canonical_rest_query("page=1")),
            "variables_base64": BASE64.encode(b"{}"),
            "query_sha256": digest_bytes(&canonical_rest_query("page=1")),
            "variables_sha256": digest_bytes(b"{}"),
            "auth_identity_ref": "collector.auth",
            "started_at_utc": "2026-09-20T00:00:00Z",
            "completed_at_utc": "2026-09-20T00:00:01Z",
            "http_status": 200,
            "api_request_id": "request-id",
            "rate_limit_ref": "collector.rate_limit",
            "page": {
                "number": 1,
                "per_page": 100,
                "link_next": null,
                "cursor_in": null,
                "cursor_out": null,
                "has_next_page": false,
                "items_returned": 1
            },
            "response_raw_ref": "raw-1",
            "error_raw_ref": null,
            "state": "complete",
            "complete": true,
            "truncation_reason": null
        });
        assert!(serde_json::from_value::<G0RequestRecord>(request.clone()).is_ok());
        request["caller_claimed_success"] = json!(true);
        assert!(serde_json::from_value::<G0RequestRecord>(request).is_err());
    }

    #[test]
    fn g0_provider_variant_rejects_unknown_fields() {
        let (_, _, inventory) = complete_g0_fixture();
        let mut value = serde_json::to_value(&inventory).unwrap();
        value["collector_snapshot"]["repositories"][1]["main_checks"][0]["provider"]
            ["github_actions"]["details_url"] = json!("https://example.invalid");
        assert!(serde_json::from_value::<G0InventoryEvidence>(value).is_err());
    }

    #[test]
    fn actual_captured_check_runs_page_exposes_envelope_parser_gap() {
        let body_base64 = include_str!(
            "testdata/g0/raw-capture-20260920-065449/checks/jackin-project_jackin-agent-smith/pr-head-206-b9db5b149cc46baba9c49549432307c29e3972b0/check-runs/page-0001.body.base64"
        )
        .trim();
        let bytes = BASE64.decode(body_base64).expect("captured body base64");
        assert_eq!(bytes.len(), 29_175);
        assert_eq!(
            digest_bytes(&bytes),
            "sha256:bcc6670efbb51a1457eda1da19e473921b8b0ef51feba016e761b3ccf3d67210"
        );
        let raw_id = "captured-check-runs-page-0001".to_owned();
        let request_id = "checks-fresh-jackin-project_jackin-agent-smith-check-runs-pr-head-206-b9db5b149cc46baba9c49549432307c29e3972b0-1".to_owned();
        let raw = G0RawObjectRef {
            raw_id: raw_id.clone(),
            request_id: request_id.clone(),
            object_kind: "check_runs".to_owned(),
            canonicalization: "raw-json".to_owned(),
            sha256: "sha256:bcc6670efbb51a1457eda1da19e473921b8b0ef51feba016e761b3ccf3d67210"
                .to_owned(),
            byte_length: bytes.len() as u64,
            bytes_base64: body_base64.to_owned(),
            media_type: "application/json".to_owned(),
            storage_ref:
                "sha256://bcc6670efbb51a1457eda1da19e473921b8b0ef51feba016e761b3ccf3d67210"
                    .to_owned(),
            original_sha256:
                "sha256:bcc6670efbb51a1457eda1da19e473921b8b0ef51feba016e761b3ccf3d67210".to_owned(),
            original_byte_length: bytes.len() as u64,
            original_storage_ref:
                "sha256://bcc6670efbb51a1457eda1da19e473921b8b0ef51feba016e761b3ccf3d67210"
                    .to_owned(),
        };
        let request = G0RequestRecord {
            request_id,
            api: G0ApiKind::Rest,
            method: "GET".to_owned(),
            endpoint_or_operation: "/repos/jackin-project/jackin-agent-smith/commits/b9db5b149cc46baba9c49549432307c29e3972b0/check-runs".to_owned(),
            query_base64: BASE64.encode(&canonical_rest_query("per_page=100&filter=all&page=1")),
            variables_base64: BASE64.encode(b"{}"),
            query_sha256: digest_bytes(&canonical_rest_query("per_page=100&filter=all&page=1")),
            variables_sha256: digest_bytes(b"{}"),
            auth_identity_ref: "collector.auth".to_owned(),
            started_at_utc: "2026-09-20T07:02:33Z".to_owned(),
            completed_at_utc: "2026-09-20T07:02:34Z".to_owned(),
            http_status: 200,
            api_request_id: "api-request-check-runs-page-0001".to_owned(),
            rate_limit_ref: "collector.rate_limit".to_owned(),
            page: G0Page {
                number: 1,
                per_page: 100,
                link_next: None,
                cursor_in: None,
                cursor_out: None,
                has_next_page: false,
                items_returned: 7,
            },
            response_raw_ref: raw_id,
            error_raw_ref: None,
            state: G0RequestState::Complete,
            complete: true,
            truncation_reason: None,
        };
        let check = G0CheckProducer {
            context: "DCO".to_owned(),
            app_id: "974774".to_owned(),
            app_slug: "dco-2".to_owned(),
            provider: G0CheckProvider::ExternalApp,
            api: G0ApiKind::Rest,
            check_suite_id: 95096323548,
            check_run_id: 104858522157,
            source_sha: "b9db5b149cc46baba9c49549432307c29e3972b0".to_owned(),
            event: "pull_request".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            html_url: "https://github.com/jackin-project/jackin-agent-smith/runs/104858522157"
                .to_owned(),
            raw_object_refs: vec!["captured-check-runs-page-0001".to_owned()],
        };
        let selected_check = g0_capture_raw_json(
            &check.raw_object_refs,
            "check_runs",
            check.check_run_id,
            &["/repos/jackin-project/jackin-agent-smith/commits/b9db5b149cc46baba9c49549432307c29e3972b0/check-runs".to_owned()],
            std::slice::from_ref(&request),
            std::slice::from_ref(&raw),
        )
        .expect("DCO member must be selected from the complete check-runs page");
        assert_eq!(
            g0_json_u64(&selected_check, &["id"]),
            Some(check.check_run_id)
        );
        assert_eq!(
            g0_json_string(&selected_check, &["app", "slug"]),
            Some(check.app_slug.as_str())
        );

        // This body is only the independently captured check-runs page.  The
        // full provider proof remains closed until separately captured suite
        // and App objects are linked by the collector.
        assert!(!g0_check_raw_evidence_valid(
            "jackin-project/jackin-agent-smith",
            &check,
            &[request],
            &[raw]
        ));
    }

    #[test]
    fn paginated_selector_rejects_bare_objects_and_requires_check_suites_envelope() {
        let endpoint = format!("/repos/tailrocks/velnor/commits/{}/check-suites", sha('a'));
        let request = captured_page_request(&endpoint, "page=1", 1);
        let bare = json!({"id": 42});
        assert!(
            g0_select_raw_member(bare, G0RawResponseSchema::CheckSuitesPage, 42, &request)
                .is_none()
        );

        let envelope = json!({
            "total_count": 1,
            "check_suites": [{"id": 42}]
        });
        let selected =
            g0_select_raw_member(envelope, G0RawResponseSchema::CheckSuitesPage, 42, &request)
                .expect("check-suites member must come from its exact envelope");
        assert_eq!(g0_json_u64(&selected, &["id"]), Some(42));

        let wrong_envelope = json!({
            "total_count": 1,
            "check_runs": [{"id": 42}]
        });
        assert!(g0_select_raw_member(
            wrong_envelope,
            G0RawResponseSchema::CheckSuitesPage,
            42,
            &request
        )
        .is_none());
    }

    #[test]
    fn singular_response_rejects_pagination_query() {
        let endpoint = "/apps/dco-2";
        let body = br#"{"id":974774,"slug":"dco-2"}"#;
        let raw = captured_raw_reference("app-raw", "app-request", "app", body);
        let mut request = captured_page_request(endpoint, "page=1", 1);
        request.request_id = "app-request".to_owned();
        request.response_raw_ref = "app-raw".to_owned();
        assert!(g0_capture_raw_json(
            &["app-raw".to_owned()],
            "app",
            974774,
            &[endpoint.to_owned()],
            std::slice::from_ref(&request),
            std::slice::from_ref(&raw),
        )
        .is_none());
        request.query_base64 = BASE64.encode(b"");
        request.query_sha256 = digest_bytes(b"");
        assert!(g0_capture_raw_json(
            &["app-raw".to_owned()],
            "app",
            974774,
            &[endpoint.to_owned()],
            std::slice::from_ref(&request),
            std::slice::from_ref(&raw),
        )
        .is_some());
    }

    #[test]
    fn endpoint_contract_is_closed_and_coverage_specific() {
        let source = sha('a');
        let check_endpoint = format!("/repos/tailrocks/velnor/commits/{source}/check-runs");
        let cases = [
            (
                "repository",
                "/repos/tailrocks/velnor",
                G0EndpointKind::Repository,
                G0CoveragePurpose::RepositorySnapshot,
            ),
            (
                "check_runs",
                check_endpoint.as_str(),
                G0EndpointKind::CheckRunsPage,
                G0CoveragePurpose::CheckInventory,
            ),
            (
                "check_suite",
                "/repos/tailrocks/velnor/check-suites/1",
                G0EndpointKind::CheckSuite,
                G0CoveragePurpose::CheckSuiteInventory,
            ),
            (
                "workflow_attempt_jobs",
                "/repos/tailrocks/velnor/actions/runs/1/attempts/1/jobs",
                G0EndpointKind::WorkflowAttemptJobsPage,
                G0CoveragePurpose::JobInventory,
            ),
            (
                "workflow_artifacts",
                "/repos/tailrocks/velnor/actions/runs/1/artifacts",
                G0EndpointKind::ArtifactsPage,
                G0CoveragePurpose::ArtifactCensus,
            ),
            (
                "workflow_run",
                "/repos/tailrocks/velnor/actions/runs/1",
                G0EndpointKind::WorkflowRun,
                G0CoveragePurpose::WorkflowRunIdentity,
            ),
            (
                "app",
                "/apps/dco-2",
                G0EndpointKind::App,
                G0CoveragePurpose::ProviderIdentity,
            ),
        ];
        for (object_kind, endpoint, expected_kind, expected_coverage) in cases {
            let contract = g0_endpoint_contract(object_kind, endpoint)
                .expect("known evidence endpoint must have one closed contract");
            assert_eq!(contract.kind, expected_kind, "{object_kind} {endpoint}");
            assert_eq!(
                contract.coverage, expected_coverage,
                "{object_kind} {endpoint}"
            );
        }

        // Every producer-emitted raw kind has one explicit path and coverage
        // purpose.  This is intentionally exhaustive: adding a producer kind
        // without adding its closed endpoint contract must fail this test.
        let emitted_cases = vec![
            (
                "auth.viewer",
                "/user".to_owned(),
                G0EndpointKind::Viewer,
                G0CoveragePurpose::Authentication,
            ),
            (
                "default_branch.commit",
                format!("/repos/tailrocks/velnor/commits/{source}"),
                G0EndpointKind::DefaultBranchCommit,
                G0CoveragePurpose::DefaultBranchSnapshot,
            ),
            (
                "pull_requests",
                "/repos/tailrocks/velnor/pulls".to_owned(),
                G0EndpointKind::PullRequestsPage,
                G0CoveragePurpose::PullRequestInventory,
            ),
            (
                "pull_request",
                "/repos/tailrocks/velnor/pulls/1".to_owned(),
                G0EndpointKind::PullRequest,
                G0CoveragePurpose::PullRequestInventory,
            ),
            (
                "rulesets",
                "/repos/tailrocks/velnor/rulesets".to_owned(),
                G0EndpointKind::RulesetsPage,
                G0CoveragePurpose::RulesetInventory,
            ),
            (
                "ruleset",
                "/repos/tailrocks/velnor/rulesets/1".to_owned(),
                G0EndpointKind::Ruleset,
                G0CoveragePurpose::RulesetInventory,
            ),
            (
                "workflows",
                "/repos/tailrocks/velnor/actions/workflows".to_owned(),
                G0EndpointKind::WorkflowsPage,
                G0CoveragePurpose::WorkflowInventory,
            ),
            (
                "workflow.source",
                format!("/repos/tailrocks/velnor/contents/.github/workflows/ci.yml?ref={source}"),
                G0EndpointKind::WorkflowSource,
                G0CoveragePurpose::WorkflowSource,
            ),
            (
                "workflow.dependency",
                format!("/repos/tailrocks/velnor/git/trees/{source}?recursive=1"),
                G0EndpointKind::WorkflowDependency,
                G0CoveragePurpose::WorkflowDependency,
            ),
            (
                "workflow.dependency.source",
                format!(
                    "/repos/tailrocks/velnor/contents/.github/workflows/reusable.yml?ref={source}"
                ),
                G0EndpointKind::WorkflowDependencySource,
                G0CoveragePurpose::WorkflowDependencySource,
            ),
            (
                "workflow_runs",
                "/repos/tailrocks/velnor/actions/runs".to_owned(),
                G0EndpointKind::WorkflowRunsPage,
                G0CoveragePurpose::WorkflowRunInventory,
            ),
            (
                "check_run",
                "/repos/tailrocks/velnor/check-runs/1".to_owned(),
                G0EndpointKind::CheckRun,
                G0CoveragePurpose::CheckInventory,
            ),
            (
                "check_suites",
                format!("/repos/tailrocks/velnor/commits/{source}/check-suites"),
                G0EndpointKind::CheckSuitesPage,
                G0CoveragePurpose::CheckSuiteInventory,
            ),
            (
                "check_suite_runs",
                "/repos/tailrocks/velnor/check-suites/1/check-runs".to_owned(),
                G0EndpointKind::CheckSuiteRunsPage,
                G0CoveragePurpose::CheckInventory,
            ),
            (
                "workflow_jobs",
                "/repos/tailrocks/velnor/actions/runs/1/jobs".to_owned(),
                G0EndpointKind::WorkflowJobsPage,
                G0CoveragePurpose::JobInventory,
            ),
            (
                "workflow_attempt",
                "/repos/tailrocks/velnor/actions/runs/1/attempts/1".to_owned(),
                G0EndpointKind::WorkflowAttempt,
                G0CoveragePurpose::WorkflowRunIdentity,
            ),
            (
                "workflow_artifacts",
                "/repos/tailrocks/velnor/actions/artifacts/1/zip".to_owned(),
                G0EndpointKind::ArtifactArchive,
                G0CoveragePurpose::ArtifactArchive,
            ),
        ];
        for (object_kind, endpoint, expected_kind, expected_coverage) in emitted_cases {
            let contract = g0_endpoint_contract(object_kind, &endpoint)
                .expect("every producer-emitted kind must have a closed contract");
            assert_eq!(contract.kind, expected_kind, "{object_kind} {endpoint}");
            assert_eq!(
                contract.coverage, expected_coverage,
                "{object_kind} {endpoint}"
            );
        }

        let mut all_request =
            captured_page_request(&check_endpoint, "per_page=100&filter=all&page=1", 1);
        let check_contract =
            g0_endpoint_contract("check_runs", &check_endpoint).expect("check inventory contract");
        assert!(g0_response_query_contract(
            &all_request,
            &canonical_rest_query("per_page=100&filter=all&page=1"),
            check_contract,
        ));
        let latest_query = canonical_rest_query("per_page=100&filter=latest&page=1");
        all_request.query_base64 = BASE64.encode(&latest_query);
        all_request.query_sha256 = digest_bytes(&latest_query);
        assert!(!g0_response_query_contract(
            &all_request,
            &latest_query,
            check_contract,
        ));

        for (object_kind, endpoint) in [
            ("repository", "/repos/tailrocks/velnor/issues"),
            (
                "repository",
                "/repos/tailrocks/velnor/actions/runs/1/artifacts-unknown",
            ),
            (
                "check_runs",
                "/repos/tailrocks/velnor/commits/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/issues",
            ),
            ("job", "/repos/tailrocks/velnor/actions/runs/1/jobs"),
            (
                "workflow_artifacts",
                "/repos/tailrocks/velnor/actions/runs/1/artifacts?filter=latest",
            ),
            ("unknown", "/repos/tailrocks/velnor"),
        ] {
            assert!(
                g0_endpoint_contract(object_kind, endpoint).is_err(),
                "arbitrary evidence path was accepted: {object_kind} {endpoint}"
            );
        }
    }

    #[test]
    fn producer_rest_query_wire_round_trips_without_url_query_alias() {
        let endpoint = format!("/repos/tailrocks/velnor/commits/{}/check-runs", sha('a'));
        let request = captured_page_request(&endpoint, "per_page=100&filter=all&page=1", 1);
        let query = BASE64
            .decode(&request.query_base64)
            .expect("canonical producer query base64");
        assert_eq!(
            query,
            canonical_rest_query("per_page=100&filter=all&page=1")
        );
        assert!(g0_request_semantics(&request, Some(&query), Some(b"{}")));

        let mut url_query_payload = request.clone();
        url_query_payload.query_base64 = BASE64.encode(b"per_page=100&filter=all&page=1");
        url_query_payload.query_sha256 = digest_bytes(b"per_page=100&filter=all&page=1");
        let mut findings = Vec::new();
        let collector = {
            let mut collector = minimal_g0_collector();
            let bytes = br#"{"total_count":1,"check_runs":[{"id":1}]}"#;
            collector.requests.push(url_query_payload);
            collector.raw_objects.push(captured_raw_reference(
                "fixture-raw",
                &collector.requests[0].request_id,
                "check_runs",
                bytes,
            ));
            collector
        };
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-request-query"));
    }

    #[test]
    fn paginated_stream_rejects_omitted_duplicate_and_cross_scope_pages() {
        let mut collector = minimal_g0_collector();
        let endpoint = format!("/repos/tailrocks/velnor/commits/{}/check-runs", sha('a'));
        let mut first = captured_page_request(&endpoint, "per_page=100&filter=all&page=1", 1);
        first.request_id = "checks-page-1".to_owned();
        first.response_raw_ref = "checks-raw-1".to_owned();
        first.page.has_next_page = true;
        first.page.link_next = Some(format!(
            "https://api.github.com{endpoint}?per_page=100&filter=all&page=2"
        ));
        let mut second = captured_page_request(&endpoint, "per_page=100&filter=all&page=2", 1);
        second.request_id = "checks-page-2".to_owned();
        second.response_raw_ref = "checks-raw-2".to_owned();
        second.page.number = 2;
        let first_bytes = br#"{"total_count":2,"check_runs":[{"id":1}]}"#;
        let second_bytes = br#"{"total_count":2,"check_runs":[{"id":2}]}"#;
        collector.requests.extend([first.clone(), second.clone()]);
        collector.raw_objects.extend([
            captured_raw_reference("checks-raw-1", "checks-page-1", "check_runs", first_bytes),
            captured_raw_reference("checks-raw-2", "checks-page-2", "check_runs", second_bytes),
        ]);
        let mut findings = Vec::new();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(!findings.iter().any(|finding| {
            matches!(
                finding.code.as_str(),
                "g0-response-envelope"
                    | "g0-pagination-count"
                    | "g0-pagination-total"
                    | "g0-pagination-duplicate"
            )
        }));

        // The collector may represent page two as an absolute API URL while
        // leaving its producer query payload empty. Normalize only the
        // endpoint URL query; do not treat it as an ampersand payload alias.
        collector.requests[1].endpoint_or_operation =
            format!("https://api.github.com{endpoint}?per_page=100&filter=all&page=2");
        collector.requests[1].query_base64 = BASE64.encode(b"");
        collector.requests[1].query_sha256 = digest_bytes(b"");
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(
            !findings.iter().any(|finding| {
                matches!(
                    finding.code.as_str(),
                    "g0-request-query" | "g0-pagination" | "g0-request-incomplete"
                )
            }),
            "absolute page-2 pagination findings: {findings:?}"
        );

        let wrong_page_size = canonical_rest_query("per_page=50&filter=all&page=1");
        collector.requests[0].query_base64 = BASE64.encode(&wrong_page_size);
        collector.requests[0].query_sha256 = digest_bytes(&wrong_page_size);
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-request-query"));

        let noncanonical_order = b"filter\0all\0per_page\0100\0page\01\0";
        collector.requests[0].query_base64 = BASE64.encode(noncanonical_order);
        collector.requests[0].query_sha256 = digest_bytes(noncanonical_order);
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-request-query"));

        let duplicate_query = b"filter\0all\0filter\0all\0page\01\0per_page\0100\0";
        collector.requests[0].query_base64 = BASE64.encode(duplicate_query);
        collector.requests[0].query_sha256 = digest_bytes(duplicate_query);
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-request-incomplete"));

        let valid_query = canonical_rest_query("per_page=100&filter=all&page=1");
        collector.requests[0].query_base64 = BASE64.encode(&valid_query);
        collector.requests[0].query_sha256 = digest_bytes(&valid_query);

        let duplicate_bytes = br#"{"total_count":2,"check_runs":[{"id":1}]}"#;
        collector.raw_objects[1] = captured_raw_reference(
            "checks-raw-2",
            "checks-page-2",
            "check_runs",
            duplicate_bytes,
        );
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-pagination-duplicate"));

        collector.raw_objects[1] =
            captured_raw_reference("checks-raw-2", "checks-page-2", "check_runs", second_bytes);
        collector.requests[0].page.link_next = Some(
            "https://api.github.com/repos/other/repo/commits/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/check-runs?per_page=100&filter=all&page=2"
                .to_owned(),
        );
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-pagination"));

        collector.requests[0].page.link_next = Some(format!(
            "https://api.github.com{endpoint}?per_page=100&filter=all&page=2"
        ));
        collector.requests.pop();
        collector.raw_objects.pop();
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-pagination"));
    }

    #[test]
    fn paginated_request_missing_raw_response_fails_closed() {
        let mut collector = minimal_g0_collector();
        let endpoint = format!("/repos/tailrocks/velnor/commits/{}/check-runs", sha('a'));
        let mut request = captured_page_request(&endpoint, "per_page=100&filter=all&page=1", 1);
        request.request_id = "missing-raw-request".to_owned();
        request.response_raw_ref = "missing-raw-response".to_owned();
        collector.requests.push(request);

        let mut findings = Vec::new();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "g0-request-incomplete"),
            "missing raw response must invalidate request: {findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code == "g0-raw-reference"),
            "missing raw response must be reported by reference validation: {findings:?}"
        );
    }

    #[test]
    fn actual_captured_jobs_page_binds_job_to_attempt_and_check_url() {
        let body_base64 = include_str!(
            "testdata/g0/raw-capture-20260920-065449/runs/tailrocks_holla-apt/run-35079189599/attempt-1/jobs/page-0001.body.base64"
        )
        .trim();
        let bytes = BASE64
            .decode(body_base64)
            .expect("captured jobs body base64");
        assert_eq!(bytes.len(), 8_017);
        assert_eq!(
            digest_bytes(&bytes),
            "sha256:e6f19864a4ef1a7c45f7f467a0409d20b1ede477e4f913722fc4b54bf083d2ad"
        );
        let raw_id = "captured-jobs-page-0001".to_owned();
        let request_id = "jobs-tailrocks_holla-apt-run-35079189599-attempt-1-jobs-1".to_owned();
        let raw = G0RawObjectRef {
            raw_id: raw_id.clone(),
            request_id: request_id.clone(),
            object_kind: "workflow_attempt_jobs".to_owned(),
            canonicalization: "raw-json".to_owned(),
            sha256: "sha256:e6f19864a4ef1a7c45f7f467a0409d20b1ede477e4f913722fc4b54bf083d2ad"
                .to_owned(),
            byte_length: bytes.len() as u64,
            bytes_base64: body_base64.to_owned(),
            media_type: "application/json".to_owned(),
            storage_ref:
                "sha256://e6f19864a4ef1a7c45f7f467a0409d20b1ede477e4f913722fc4b54bf083d2ad"
                    .to_owned(),
            original_sha256:
                "sha256:e6f19864a4ef1a7c45f7f467a0409d20b1ede477e4f913722fc4b54bf083d2ad".to_owned(),
            original_byte_length: bytes.len() as u64,
            original_storage_ref:
                "sha256://e6f19864a4ef1a7c45f7f467a0409d20b1ede477e4f913722fc4b54bf083d2ad"
                    .to_owned(),
        };
        let request = G0RequestRecord {
            request_id,
            api: G0ApiKind::Rest,
            method: "GET".to_owned(),
            endpoint_or_operation:
                "/repos/tailrocks/holla-apt/actions/runs/35079189599/attempts/1/jobs".to_owned(),
            query_base64: BASE64.encode(&canonical_rest_query("per_page=100&page=1")),
            variables_base64: BASE64.encode(b"{}"),
            query_sha256: digest_bytes(&canonical_rest_query("per_page=100&page=1")),
            variables_sha256: digest_bytes(b"{}"),
            auth_identity_ref: "collector.auth".to_owned(),
            started_at_utc: "2026-09-20T07:36:18Z".to_owned(),
            completed_at_utc: "2026-09-20T07:36:19Z".to_owned(),
            http_status: 200,
            api_request_id: "api-request-jobs-page-0001".to_owned(),
            rate_limit_ref: "collector.rate_limit".to_owned(),
            page: G0Page {
                number: 1,
                per_page: 100,
                link_next: None,
                cursor_in: None,
                cursor_out: None,
                has_next_page: false,
                items_returned: 6,
            },
            response_raw_ref: raw_id,
            error_raw_ref: None,
            state: G0RequestState::Complete,
            complete: true,
            truncation_reason: None,
        };
        let selected_job = g0_capture_raw_json(
            &["captured-jobs-page-0001".to_owned()],
            "workflow_attempt_jobs",
            104741135689,
            &["/repos/tailrocks/holla-apt/actions/runs/35079189599/attempts/1/jobs".to_owned()],
            &[request],
            &[raw],
        )
        .expect("job must be selected from the complete attempt jobs page");
        assert_eq!(g0_json_u64(&selected_job, &["id"]), Some(104741135689));
        assert_eq!(g0_json_u64(&selected_job, &["run_id"]), Some(35079189599));
        assert_eq!(g0_json_u64(&selected_job, &["run_attempt"]), Some(1));
        assert_eq!(
            g0_json_string(&selected_job, &["check_run_url"]),
            Some("https://api.github.com/repos/tailrocks/holla-apt/check-runs/104741135689")
        );
    }

    #[test]
    fn frozen_real_api_corpus_preserves_run_graph_and_provider_relationships() {
        let run_bytes = real_api_fixture_entry("raw/velnor/run-35493166478/run.body");
        let attempt_bytes =
            real_api_fixture_entry("raw/velnor/run-35493166478/attempt-1/attempt.body");
        let jobs_bytes =
            real_api_fixture_entry("raw/velnor/run-35493166478/attempt-1/jobs/page-0001.body");
        let artifacts_bytes =
            real_api_fixture_entry("raw/velnor/run-35493166478/artifacts/page-0001.body");
        let checks_bytes = real_api_fixture_entry(
            "raw/velnor/commit-df9fb272c025f76cc8711560209afcdfd6cc4e00/check-runs/page-0001.body",
        );
        let sonar_bytes = real_api_fixture_entry(
            "raw/homebrew-tap/commit-c501e90d014c207234ed94ea41f7a1c9b6ea0c7c/check-runs/page-0001.body",
        );
        assert_eq!(
            digest_bytes(&run_bytes),
            "sha256:40bf9508b2a024d6813c498b225caf255f02390d27d00dcf390bbee19294aa10"
        );
        assert_eq!(
            digest_bytes(&attempt_bytes),
            "sha256:9837c6d5b687cfb1979b7bd9f547e1eec69536bdfb08bcf435dcd6444f820405"
        );
        assert_eq!(
            digest_bytes(&jobs_bytes),
            "sha256:b9937bad52729f233fed821f81ed5d2e8526eefdc049f462dea5d20aefcfdec9"
        );
        assert_eq!(
            digest_bytes(&artifacts_bytes),
            "sha256:7abeb5ab6ba79e4d085d70bacb67bd1357adb7fb0ab9811933bdf2a6f0a5c171"
        );
        assert_eq!(
            digest_bytes(&checks_bytes),
            "sha256:2f30395551e7bfad2f0271dfeaa09bd4768da3905dfa1a42785aa12d6388bf57"
        );
        assert_eq!(
            digest_bytes(&sonar_bytes),
            "sha256:c241e54220d3a3c04d5761e100be507b9c2dfec15772414b898a90957b7551b5"
        );

        let run: Value = serde_json::from_slice(&run_bytes).expect("run body JSON");
        let attempt: Value = serde_json::from_slice(&attempt_bytes).expect("attempt body JSON");
        assert_eq!(g0_json_u64(&run, &["id"]), Some(35_493_166_478));
        assert_eq!(g0_json_u64(&run, &["run_attempt"]), Some(1));
        assert_eq!(
            g0_json_string(&run, &["head_sha"]),
            Some("df9fb272c025f76cc8711560209afcdfd6cc4e00")
        );
        assert_eq!(g0_json_string(&run, &["event"]), Some("pull_request"));
        assert_eq!(g0_json_u64(&attempt, &["id"]), Some(35_493_166_478));
        assert_eq!(g0_json_u64(&attempt, &["run_attempt"]), Some(1));
        assert_eq!(
            g0_json_string(&attempt, &["head_sha"]),
            Some("df9fb272c025f76cc8711560209afcdfd6cc4e00")
        );

        let jobs: Value = serde_json::from_slice(&jobs_bytes).expect("jobs page JSON");
        let jobs_array = jobs["jobs"].as_array().expect("jobs envelope");
        assert_eq!(jobs["total_count"].as_u64(), Some(68));
        assert_eq!(jobs_array.len(), 68);
        let job_ids = jobs_array
            .iter()
            .map(|job| g0_json_u64(job, &["id"]).expect("job id"))
            .collect::<BTreeSet<_>>();
        assert_eq!(job_ids.len(), 68);
        assert!(jobs_array.iter().all(|job| {
            g0_json_u64(job, &["run_id"]) == Some(35_493_166_478)
                && g0_json_u64(job, &["run_attempt"]) == Some(1)
                && g0_json_string(job, &["html_url"]).is_some_and(|url| {
                    url.starts_with(
                        "https://github.com/tailrocks/velnor/actions/runs/35493166478/job/",
                    )
                })
        }));
        let job_request = captured_page_request(
            "/repos/tailrocks/velnor/actions/runs/35493166478/attempts/1/jobs",
            "per_page=100&page=1",
            68,
        );
        let selected_job = g0_select_raw_member(
            jobs,
            G0RawResponseSchema::JobsPage,
            job_ids.iter().next().copied().expect("first job id"),
            &job_request,
        )
        .expect("first job must be selected from the complete page");
        assert_eq!(
            g0_json_u64(&selected_job, &["run_id"]),
            Some(35_493_166_478)
        );

        let artifacts: Value =
            serde_json::from_slice(&artifacts_bytes).expect("artifacts page JSON");
        let artifacts_array = artifacts["artifacts"]
            .as_array()
            .expect("artifacts envelope");
        assert_eq!(artifacts["total_count"].as_u64(), Some(2));
        assert_eq!(artifacts_array.len(), 2);
        assert!(artifacts_array.iter().all(|artifact| {
            g0_json_u64(artifact, &["workflow_run", "id"]) == Some(35_493_166_478)
                && artifact
                    .get("workflow_run")
                    .and_then(|run| run.get("run_attempt"))
                    .is_none()
                && artifact.get("workflow_run_id").is_none()
        }));
        let first_artifact = artifacts_array.first().expect("captured artifact row");
        let artifact_observation = G0ArtifactObservation {
            artifact_id: g0_json_u64(first_artifact, &["id"]).expect("artifact id"),
            run_id: g0_json_u64(first_artifact, &["workflow_run", "id"]).expect("artifact run id"),
            run_attempt: 1,
            run_head_sha: g0_json_string(first_artifact, &["workflow_run", "head_sha"])
                .expect("artifact head SHA")
                .to_owned(),
            name: g0_json_string(first_artifact, &["name"])
                .expect("artifact name")
                .to_owned(),
            digest: g0_json_string(first_artifact, &["digest"])
                .expect("archive digest")
                .to_owned(),
            expired: first_artifact["expired"]
                .as_bool()
                .expect("artifact expiry"),
            source_url: g0_json_string(first_artifact, &["archive_download_url"])
                .expect("artifact URL")
                .to_owned(),
            raw_object_refs: vec!["real-artifacts-raw".to_owned()],
        };
        let artifact_raw = captured_raw_reference(
            "real-artifacts-raw",
            "real-artifacts-request",
            "workflow_artifacts",
            &artifacts_bytes,
        );
        assert!(g0_artifact_row_matches_raw(
            "tailrocks/velnor",
            &artifact_observation,
            &artifact_raw
        ));
        let mut conflated_artifact = artifact_observation.clone();
        conflated_artifact.digest = artifact_raw.sha256.clone();
        assert!(!g0_artifact_row_matches_raw(
            "tailrocks/velnor",
            &conflated_artifact,
            &artifact_raw
        ));

        let checks: Value = serde_json::from_slice(&checks_bytes).expect("checks page JSON");
        let checks_array = checks["check_runs"].as_array().expect("checks envelope");
        assert_eq!(checks["total_count"].as_u64(), Some(70));
        assert_eq!(checks_array.len(), 70);
        let check_ids = checks_array
            .iter()
            .map(|check| g0_json_u64(check, &["id"]).expect("check id"))
            .collect::<BTreeSet<_>>();
        assert_eq!(check_ids.len(), 70);
        let dco = checks_array
            .iter()
            .find(|check| g0_json_u64(check, &["app", "id"]) == Some(974_774))
            .expect("captured DCO check");
        assert_eq!(g0_json_u64(dco, &["id"]), Some(106_031_458_188));
        assert_eq!(g0_json_string(dco, &["name"]), Some("DCO"));
        assert_eq!(
            g0_json_u64(dco, &["check_suite", "id"]),
            Some(96_108_219_979)
        );
        assert_eq!(g0_json_string(dco, &["app", "slug"]), Some("dco-2"));
        assert_eq!(
            g0_json_string(dco, &["head_sha"]),
            Some("df9fb272c025f76cc8711560209afcdfd6cc4e00")
        );
        let checks_request = captured_page_request(
            "/repos/tailrocks/velnor/commits/df9fb272c025f76cc8711560209afcdfd6cc4e00/check-runs",
            "per_page=100&filter=all&page=1",
            70,
        );
        let selected_dco = g0_select_raw_member(
            checks.clone(),
            G0RawResponseSchema::CheckRunsPage,
            106_031_458_188,
            &checks_request,
        )
        .expect("DCO must be selected from the complete checks page");
        assert_eq!(g0_json_u64(&selected_dco, &["app", "id"]), Some(974_774));

        let dco_check = G0CheckProducer {
            context: "DCO".to_owned(),
            app_id: "974774".to_owned(),
            app_slug: "dco-2".to_owned(),
            provider: G0CheckProvider::ExternalApp,
            api: G0ApiKind::Rest,
            check_suite_id: 96_108_219_979,
            check_run_id: 106_031_458_188,
            source_sha: "df9fb272c025f76cc8711560209afcdfd6cc4e00".to_owned(),
            event: "pull_request".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            html_url: "https://github.com/tailrocks/velnor/runs/106031458188".to_owned(),
            raw_object_refs: Vec::new(),
        };
        assert!(g0_check_run_identity_valid(
            &selected_dco,
            &dco_check,
            974_774
        ));
        let mut wrong_app = dco_check.clone();
        wrong_app.app_id = "12526".to_owned();
        wrong_app.app_slug = "sonarqubecloud".to_owned();
        assert!(!g0_check_run_identity_valid(
            &selected_dco,
            &wrong_app,
            12_526
        ));
        let mut wrong_suite = dco_check.clone();
        wrong_suite.check_suite_id = 96_108_224_766;
        assert!(!g0_check_run_identity_valid(
            &selected_dco,
            &wrong_suite,
            974_774
        ));

        let mut valid_request = checks_request.clone();
        valid_request.request_id = "real-checks-request".to_owned();
        valid_request.response_raw_ref = "real-checks-raw".to_owned();
        let valid_raw = captured_raw_reference(
            "real-checks-raw",
            "real-checks-request",
            "check_runs",
            &checks_bytes,
        );
        let mut wrong_app_page = checks.clone();
        let wrong_app_member = wrong_app_page["check_runs"]
            .as_array_mut()
            .expect("captured check-runs array")
            .iter_mut()
            .find(|value| g0_json_u64(value, &["id"]) == Some(dco_check.check_run_id))
            .expect("captured DCO member");
        wrong_app_member["app"]["id"] = json!(12_526);
        wrong_app_member["app"]["slug"] = json!("sonarqubecloud");
        let wrong_app_bytes = canonical_json(&wrong_app_page).into_bytes();
        let wrong_app_raw = captured_raw_reference(
            "real-checks-raw",
            "real-checks-request",
            "check_runs",
            &wrong_app_bytes,
        );
        let wrong_app_selected = g0_capture_raw_json(
            &["real-checks-raw".to_owned()],
            "check_runs",
            dco_check.check_run_id,
            std::slice::from_ref(&checks_request.endpoint_or_operation),
            std::slice::from_ref(&valid_request),
            std::slice::from_ref(&wrong_app_raw),
        )
        .expect("rehashed wrong-app page still has a typed member");
        assert!(!g0_check_run_identity_valid(
            &wrong_app_selected,
            &dco_check,
            974_774
        ));

        let mut wrong_suite_page = checks.clone();
        let wrong_suite_member = wrong_suite_page["check_runs"]
            .as_array_mut()
            .expect("captured check-runs array")
            .iter_mut()
            .find(|value| g0_json_u64(value, &["id"]) == Some(dco_check.check_run_id))
            .expect("captured DCO member");
        wrong_suite_member["check_suite"]["id"] = json!(96_108_224_766u64);
        let wrong_suite_bytes = canonical_json(&wrong_suite_page).into_bytes();
        let wrong_suite_raw = captured_raw_reference(
            "real-checks-raw",
            "real-checks-request",
            "check_runs",
            &wrong_suite_bytes,
        );
        let wrong_suite_selected = g0_capture_raw_json(
            &["real-checks-raw".to_owned()],
            "check_runs",
            dco_check.check_run_id,
            std::slice::from_ref(&checks_request.endpoint_or_operation),
            std::slice::from_ref(&valid_request),
            std::slice::from_ref(&wrong_suite_raw),
        )
        .expect("rehashed wrong-suite page still has a typed member");
        assert!(!g0_check_run_identity_valid(
            &wrong_suite_selected,
            &dco_check,
            974_774
        ));
        assert!(g0_capture_raw_json(
            &["real-checks-raw".to_owned()],
            "check_runs",
            dco_check.check_run_id,
            std::slice::from_ref(&checks_request.endpoint_or_operation),
            std::slice::from_ref(&valid_request),
            std::slice::from_ref(&valid_raw),
        )
        .is_some());
        let mut wrong_endpoint = valid_request.clone();
        wrong_endpoint.endpoint_or_operation =
            "/repos/tailrocks/velnor/check-runs/106031458188".to_owned();
        assert!(g0_capture_raw_json(
            &["real-checks-raw".to_owned()],
            "check_runs",
            dco_check.check_run_id,
            std::slice::from_ref(&checks_request.endpoint_or_operation),
            &[wrong_endpoint],
            std::slice::from_ref(&valid_raw),
        )
        .is_none());
        assert!(g0_capture_raw_json(
            &["real-checks-raw".to_owned()],
            "check_runs",
            dco_check.check_run_id + 1,
            std::slice::from_ref(&checks_request.endpoint_or_operation),
            std::slice::from_ref(&valid_request),
            std::slice::from_ref(&valid_raw),
        )
        .is_none());
        let mut rehashed_page = checks_bytes.clone();
        let old_count = b"\"total_count\":70";
        let new_count = b"\"total_count\":69";
        let count_offset = rehashed_page
            .windows(old_count.len())
            .position(|window| window == old_count)
            .expect("captured checks count");
        rehashed_page.splice(
            count_offset..count_offset + old_count.len(),
            new_count.iter().copied(),
        );
        let rehashed_raw = captured_raw_reference(
            "real-checks-raw",
            "real-checks-request",
            "check_runs",
            &rehashed_page,
        );
        assert!(g0_capture_raw_json(
            &["real-checks-raw".to_owned()],
            "check_runs",
            dco_check.check_run_id,
            &[checks_request.endpoint_or_operation],
            &[valid_request],
            &[rehashed_raw],
        )
        .is_none());

        let sonar: Value = serde_json::from_slice(&sonar_bytes).expect("Sonar checks page JSON");
        let sonar_check = sonar["check_runs"]
            .as_array()
            .expect("Sonar checks envelope")
            .iter()
            .find(|check| g0_json_u64(check, &["app", "id"]) == Some(12_526))
            .expect("captured Sonar check");
        assert_eq!(g0_json_u64(sonar_check, &["id"]), Some(105_978_471_119));
        assert_eq!(
            g0_json_string(sonar_check, &["conclusion"]),
            Some("failure")
        );
        assert_eq!(
            g0_json_string(sonar_check, &["app", "slug"]),
            Some("sonarqubecloud")
        );
        assert_eq!(
            g0_json_string(sonar_check, &["head_sha"]),
            Some("c501e90d014c207234ed94ea41f7a1c9b6ea0c7c")
        );
    }

    #[test]
    fn authenticated_supplement_binds_real_apps_and_suites() {
        // Exact body bytes from
        // G0/real-api-fixture-supplement-20260920T085311Z-corrected-20260920T090934Z,
        // manifest fea65b61ff1668ff58762066b207ac1c9534452d328080f1f0680e7cb571447e.
        let supplement = |name: &str| -> Vec<u8> {
            let encoded = match name {
                "app-dco" => include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/apps/dco-2.body.base64"
                ),
                "app-actions" => include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/apps/github-actions.body.base64"
                ),
                "app-sonar" => include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/apps/sonarqubecloud.body.base64"
                ),
                "suite-dco" => include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/suites/velnor-96108219979.body.base64"
                ),
                "suite-actions" => include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/suites/velnor-96108227551.body.base64"
                ),
                "suite-sonar" => include_str!(
                    "testdata/g0/real-api-fixture-supplement-20260920-085311/suites/homebrew-96059348318.body.base64"
                ),
                _ => panic!("unknown supplement body {name}"),
            };
            BASE64
                .decode(encoded.trim())
                .expect("supplement body base64")
        };
        let mut requests = Vec::new();
        let mut raw_objects = Vec::new();
        #[allow(
            clippy::too_many_arguments,
            reason = "fixture helper mirrors one captured request/raw provenance tuple"
        )]
        fn add_capture(
            requests: &mut Vec<G0RequestRecord>,
            raw_objects: &mut Vec<G0RawObjectRef>,
            raw_id: &str,
            request_id: &str,
            object_kind: &str,
            endpoint: String,
            query: &str,
            items: u32,
            bytes: &[u8],
        ) {
            let mut request = captured_page_request(&endpoint, query, items);
            request.request_id = request_id.to_owned();
            request.response_raw_ref = raw_id.to_owned();
            requests.push(request);
            raw_objects.push(captured_raw_reference(
                raw_id,
                request_id,
                object_kind,
                bytes,
            ));
        }

        let check_bytes = real_api_fixture_entry(
            "raw/velnor/commit-df9fb272c025f76cc8711560209afcdfd6cc4e00/check-runs/page-0001.body",
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-velnor-checks",
            "real-velnor-checks-request",
            "check_runs",
            "/repos/tailrocks/velnor/commits/df9fb272c025f76cc8711560209afcdfd6cc4e00/check-runs"
                .to_owned(),
            "per_page=100&filter=all&page=1",
            70,
            &check_bytes,
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-suite-dco",
            "real-suite-dco-request",
            "check_suite",
            "/repos/tailrocks/velnor/check-suites/96108219979".to_owned(),
            "",
            1,
            &supplement("suite-dco"),
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-app-dco",
            "real-app-dco-request",
            "app",
            "/apps/dco-2".to_owned(),
            "",
            1,
            &supplement("app-dco"),
        );

        let dco = G0CheckProducer {
            context: "DCO".to_owned(),
            app_id: "974774".to_owned(),
            app_slug: "dco-2".to_owned(),
            provider: G0CheckProvider::ExternalApp,
            api: G0ApiKind::Rest,
            check_suite_id: 96108219979,
            check_run_id: 106031458188,
            source_sha: "df9fb272c025f76cc8711560209afcdfd6cc4e00".to_owned(),
            event: "pull_request".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            html_url: "https://github.com/tailrocks/velnor/runs/106031458188".to_owned(),
            raw_object_refs: vec![
                "real-velnor-checks".to_owned(),
                "real-suite-dco".to_owned(),
                "real-app-dco".to_owned(),
            ],
        };
        assert!(g0_check_raw_evidence_valid(
            "tailrocks/velnor",
            &dco,
            &requests,
            &raw_objects
        ));

        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-suite-actions",
            "real-suite-actions-request",
            "check_suite",
            "/repos/tailrocks/velnor/check-suites/96108227551".to_owned(),
            "",
            1,
            &supplement("suite-actions"),
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-app-actions",
            "real-app-actions-request",
            "app",
            "/apps/github-actions".to_owned(),
            "",
            1,
            &supplement("app-actions"),
        );
        let run_bytes = real_api_fixture_entry("raw/velnor/run-35493166478/run.body");
        let jobs_bytes =
            real_api_fixture_entry("raw/velnor/run-35493166478/attempt-1/jobs/page-0001.body");
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-velnor-run",
            "real-velnor-run-request",
            "workflow_run",
            "/repos/tailrocks/velnor/actions/runs/35493166478".to_owned(),
            "",
            1,
            &run_bytes,
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-velnor-jobs",
            "real-velnor-jobs-request",
            "workflow_attempt_jobs",
            "/repos/tailrocks/velnor/actions/runs/35493166478/attempts/1/jobs".to_owned(),
            "per_page=100&page=1",
            68,
            &jobs_bytes,
        );
        let actions = G0CheckProducer {
            context: "Control / Planning".to_owned(),
            app_id: "15368".to_owned(),
            app_slug: "github-actions".to_owned(),
            provider: G0CheckProvider::GithubActions {
                workflow_run_id: 35493166478,
                run_attempt: 1,
                job_id: 106031464296,
                job_run_id: 35493166478,
                job_run_attempt: 1,
                job_check_run_id: 106031464296,
                job_source_sha: "df9fb272c025f76cc8711560209afcdfd6cc4e00".to_owned(),
                job_html_url:
                    "https://github.com/tailrocks/velnor/actions/runs/35493166478/job/106031464296"
                        .to_owned(),
                actual_checkout_sha: "df9fb272c025f76cc8711560209afcdfd6cc4e00".to_owned(),
            },
            api: G0ApiKind::Rest,
            check_suite_id: 96108227551,
            check_run_id: 106031464296,
            source_sha: "df9fb272c025f76cc8711560209afcdfd6cc4e00".to_owned(),
            event: "pull_request".to_owned(),
            status: "completed".to_owned(),
            conclusion: "success".to_owned(),
            html_url:
                "https://github.com/tailrocks/velnor/actions/runs/35493166478/job/106031464296"
                    .to_owned(),
            raw_object_refs: vec![
                "real-velnor-checks".to_owned(),
                "real-suite-actions".to_owned(),
                "real-app-actions".to_owned(),
                "real-velnor-run".to_owned(),
                "real-velnor-jobs".to_owned(),
            ],
        };
        assert!(g0_check_raw_evidence_valid(
            "tailrocks/velnor",
            &actions,
            &requests,
            &raw_objects
        ));

        let homebrew_checks = real_api_fixture_entry(
            "raw/homebrew-tap/commit-c501e90d014c207234ed94ea41f7a1c9b6ea0c7c/check-runs/page-0001.body",
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-homebrew-checks",
            "real-homebrew-checks-request",
            "check_run",
            "/repos/jackin-project/homebrew-tap/commits/c501e90d014c207234ed94ea41f7a1c9b6ea0c7c/check-runs"
                .to_owned(),
            "per_page=100&filter=latest&page=1",
            6,
            &homebrew_checks,
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-suite-sonar",
            "real-suite-sonar-request",
            "check_suite",
            "/repos/jackin-project/homebrew-tap/check-suites/96059348318".to_owned(),
            "",
            1,
            &supplement("suite-sonar"),
        );
        add_capture(
            &mut requests,
            &mut raw_objects,
            "real-app-sonar",
            "real-app-sonar-request",
            "app",
            "/apps/sonarqubecloud".to_owned(),
            "",
            1,
            &supplement("app-sonar"),
        );
        let sonar = G0CheckProducer {
            context: "SonarCloud Code Analysis".to_owned(),
            app_id: "12526".to_owned(),
            app_slug: "sonarqubecloud".to_owned(),
            provider: G0CheckProvider::ExternalApp,
            api: G0ApiKind::Rest,
            check_suite_id: 96059348318,
            check_run_id: 105978471119,
            source_sha: "c501e90d014c207234ed94ea41f7a1c9b6ea0c7c".to_owned(),
            event: "push".to_owned(),
            status: "completed".to_owned(),
            conclusion: "failure".to_owned(),
            html_url: "https://github.com/jackin-project/homebrew-tap/runs/105978471119".to_owned(),
            raw_object_refs: vec![
                "real-homebrew-checks".to_owned(),
                "real-suite-sonar".to_owned(),
                "real-app-sonar".to_owned(),
            ],
        };
        // The captured Homebrew page used GitHub's `filter=latest` provider
        // query. It is a valid raw diagnostic, but cannot authorize an
        // all-check-runs inventory or a successful evidence chain.
        assert!(!g0_check_raw_evidence_valid(
            "jackin-project/homebrew-tap",
            &sonar,
            &requests,
            &raw_objects
        ));

        let app_index = raw_objects
            .iter()
            .position(|raw| raw.raw_id == "real-app-dco")
            .expect("DCO App capture");
        let app_body = serde_json::from_slice::<Value>(
            &BASE64
                .decode(&raw_objects[app_index].bytes_base64)
                .expect("DCO App bytes"),
        )
        .expect("DCO App JSON");
        let mut wrong_app_body = app_body;
        wrong_app_body["id"] = json!(12_526);
        wrong_app_body["slug"] = json!("sonarqubecloud");
        let wrong_app_bytes = canonical_json(&wrong_app_body).into_bytes();
        raw_objects[app_index] = captured_raw_reference(
            "real-app-dco",
            "real-app-dco-request",
            "app",
            &wrong_app_bytes,
        );
        assert!(!g0_check_raw_evidence_valid(
            "tailrocks/velnor",
            &dco,
            &requests,
            &raw_objects
        ));

        let suite_index = raw_objects
            .iter()
            .position(|raw| raw.raw_id == "real-suite-dco")
            .expect("DCO suite capture");
        let suite_body = serde_json::from_slice::<Value>(
            &BASE64
                .decode(&raw_objects[suite_index].bytes_base64)
                .expect("DCO suite bytes"),
        )
        .expect("DCO suite JSON");
        let mut wrong_suite_body = suite_body;
        wrong_suite_body["app"]["id"] = json!(15_368u64);
        wrong_suite_body["app"]["slug"] = json!("github-actions");
        let wrong_suite_bytes = canonical_json(&wrong_suite_body).into_bytes();
        raw_objects[suite_index] = captured_raw_reference(
            "real-suite-dco",
            "real-suite-dco-request",
            "check_suite",
            &wrong_suite_bytes,
        );
        assert!(!g0_check_raw_evidence_valid(
            "tailrocks/velnor",
            &dco,
            &requests,
            &raw_objects
        ));
    }

    #[test]
    fn g0_snapshot_bytes_are_content_bound() {
        let mut inventory = typed_inventory(minimal_g0_collector());
        inventory.collector_snapshot.snapshot_id = "rewritten".to_owned();
        let snapshot = SnapshotDocument {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            snapshot_id: "snapshot".to_owned(),
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            observed_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            source: SnapshotSource {
                collector: "fixture".to_owned(),
                collector_revision: sha('a'),
                api_base: "https://api.github.com".to_owned(),
                captured_at_utc: "2026-09-20T00:00:00Z".to_owned(),
                read_only: true,
                page_count: 1,
                permission_scopes: vec!["metadata:read".to_owned()],
            },
            repositories: Vec::new(),
        };
        let manifest = ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            source: SourceIdentity {
                repository: REVIEWED_SOURCE_REPOSITORY.to_owned(),
                revision: REVIEWED_SOURCE_REVISION.to_owned(),
                digest: REVIEWED_SOURCE_DIGEST.to_owned(),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: Vec::new(),
        };
        let mut findings = Vec::new();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-snapshot-bytes"));
        inventory.collector_snapshot_bytes_base64 = BASE64.encode(b"{}");
        findings.clear();
        check_g0_inventory(&manifest, &snapshot, Some(&inventory), &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-collector-digest"));
    }

    #[test]
    fn g0_pagination_missing_next_page_fails_closed() {
        let mut collector = minimal_g0_collector();
        collector.requests.push(G0RequestRecord {
            request_id: "request-1".to_owned(),
            api: G0ApiKind::Rest,
            method: "GET".to_owned(),
            endpoint_or_operation: "/repos/tailrocks/velnor".to_owned(),
            query_base64: BASE64.encode(&canonical_rest_query("page=1")),
            variables_base64: BASE64.encode(b"{}"),
            query_sha256: digest_bytes(&canonical_rest_query("page=1")),
            variables_sha256: digest_bytes(b"{}"),
            auth_identity_ref: "collector.auth".to_owned(),
            started_at_utc: "2026-09-20T00:00:00Z".to_owned(),
            completed_at_utc: "2026-09-20T00:00:01Z".to_owned(),
            http_status: 200,
            api_request_id: "request-id".to_owned(),
            rate_limit_ref: "collector.rate_limit".to_owned(),
            page: G0Page {
                number: 1,
                per_page: 100,
                link_next: Some("https://api.github.com/page/2".to_owned()),
                cursor_in: None,
                cursor_out: None,
                has_next_page: true,
                items_returned: 100,
            },
            response_raw_ref: "raw-1".to_owned(),
            error_raw_ref: None,
            state: G0RequestState::Complete,
            complete: true,
            truncation_reason: None,
        });
        collector.raw_objects.push(G0RawObjectRef {
            raw_id: "raw-1".to_owned(),
            request_id: "request-1".to_owned(),
            object_kind: "repository".to_owned(),
            canonicalization: "jcs".to_owned(),
            sha256: digest_bytes(b"{}"),
            byte_length: 2,
            bytes_base64: BASE64.encode(b"{}"),
            media_type: "application/json".to_owned(),
            storage_ref: format!(
                "sha256://{}",
                digest_bytes(b"{}")
                    .strip_prefix("sha256:")
                    .expect("digest has prefix")
            ),
            original_sha256: digest_bytes(b"{}"),
            original_byte_length: 2,
            original_storage_ref: format!(
                "sha256://{}",
                digest_bytes(b"{}")
                    .strip_prefix("sha256:")
                    .expect("digest has prefix")
            ),
        });
        let mut second = collector.requests[0].clone();
        second.request_id = "request-2".to_owned();
        second.query_base64 = BASE64.encode(&canonical_rest_query("page=2"));
        second.query_sha256 = digest_bytes(&canonical_rest_query("page=2"));
        second.page.number = 2;
        second.page.link_next = None;
        second.page.has_next_page = false;
        second.page.items_returned = 1;
        second.response_raw_ref = "raw-2".to_owned();
        collector.requests.push(second);
        let second_bytes = b"{\"page\":2}";
        let second_digest = digest_bytes(second_bytes);
        collector.raw_objects.push(G0RawObjectRef {
            raw_id: "raw-2".to_owned(),
            request_id: "request-2".to_owned(),
            object_kind: "repository".to_owned(),
            canonicalization: "jcs".to_owned(),
            sha256: second_digest.clone(),
            byte_length: second_bytes.len() as u64,
            bytes_base64: BASE64.encode(second_bytes),
            media_type: "application/json".to_owned(),
            storage_ref: format!(
                "sha256://{}",
                second_digest
                    .strip_prefix("sha256:")
                    .expect("digest has prefix")
            ),
            original_sha256: second_digest.clone(),
            original_byte_length: second_bytes.len() as u64,
            original_storage_ref: format!(
                "sha256://{}",
                second_digest
                    .strip_prefix("sha256:")
                    .expect("digest has prefix")
            ),
        });
        let mut findings = Vec::new();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-pagination"));
        collector.requests[0].page.link_next =
            Some("https://api.github.com.evil.example/repos/tailrocks/velnor?page=2".to_owned());
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-pagination"));
        collector.raw_objects[0].bytes_base64 = BASE64.encode(b"tampered");
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-raw-object"));
        collector.raw_objects[0].bytes_base64 = BASE64.encode(b"{}");
        collector.requests[0].method = "DELETE".to_owned();
        findings.clear();
        check_g0_request_provenance(&collector, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-request-incomplete"));
    }

    #[test]
    fn g0_graph_dangling_edge_and_wrong_model_fail_closed() {
        let mut collector = minimal_g0_collector();
        collector.dependency_graph.nodes.push(G0GraphNode {
            id: "workload:owner/repo:scan".to_owned(),
            kind: "workload".to_owned(),
            repository: "owner/repo".to_owned(),
            workload_id: "scan".to_owned(),
            applicability: "required".to_owned(),
            source_sha: sha('a'),
            source_ref: "refs/heads/main".to_owned(),
            raw_object_refs: vec!["raw-graph".to_owned()],
        });
        collector.dependency_graph.edges.push(G0GraphEdge {
            from: "workload:owner/repo:scan".to_owned(),
            to: "missing:child".to_owned(),
            kind: "child".to_owned(),
            required: true,
            source_sha: sha('a'),
            source_ref: "refs/heads/main".to_owned(),
            target_source_sha: sha('a'),
            target_source_ref: "refs/heads/main".to_owned(),
            raw_object_refs: vec!["raw-graph".to_owned()],
        });
        collector.dependency_graph.raw_object_refs = vec!["raw-graph".to_owned()];
        collector.model_session.effective = true;
        collector.model_session.agents.push(G0AgentModel {
            agent_id: "agent".to_owned(),
            model: EXPECTED_AGENT_MODEL.to_owned(),
            effort: EXPECTED_AGENT_EFFORT.to_owned(),
            effective: true,
            raw_object_refs: vec!["raw-model".to_owned()],
        });
        collector.model_session.raw_object_refs = vec!["raw-model".to_owned()];
        let manifest = ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            manifest_id: REVIEWED_MANIFEST_ID.to_owned(),
            source: SourceIdentity {
                repository: REVIEWED_SOURCE_REPOSITORY.to_owned(),
                revision: REVIEWED_SOURCE_REVISION.to_owned(),
                digest: REVIEWED_SOURCE_DIGEST.to_owned(),
                reviewed_by: "reviewer".to_owned(),
            },
            repositories: Vec::new(),
        };
        let raw_ids = BTreeSet::from(["raw-graph".to_owned(), "raw-model".to_owned()]);
        let mut findings = Vec::new();
        check_g0_dependency_graph(&manifest, &collector, &raw_ids, &mut findings);
        check_g0_model_session(&collector, &raw_ids, &collector.raw_objects, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-dependency-edge"));
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-model-session"));
    }

    #[test]
    fn strict_schema_rejects_flat_release_alias_and_unknown_nested_field() {
        let mut record = serde_json::to_value(EvidenceRecord::default()).unwrap();
        record["release_applicability"] = json!("required");
        assert!(serde_json::from_value::<EvidenceRecord>(record).is_err());

        let nested = json!({
            "applicability": "required",
            "legacy_install": true
        });
        assert!(serde_json::from_value::<InstallEvidence>(nested).is_err());
    }

    #[test]
    fn strict_digest_and_target_parsing_rejects_coercion() {
        assert!(valid_digest(&digest('a')));
        assert!(!valid_digest(&"a".repeat(DIGEST_LENGTH)));
        assert!(!valid_digest(&format!(
            "SHA256:{}",
            "a".repeat(DIGEST_LENGTH)
        )));
        assert!(valid_target("linux-amd64"));
        assert!(!valid_target("linux_amd64"));
        assert!(!valid_target(" LINUX-AMD64"));
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
            evidence_role: EvidenceRole::DefaultBranch,
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
            CheckMode::Offline,
        );
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "live-required"));
    }

    #[test]
    fn source_job_checker_rejects_collapsed_matrix_instances() {
        let (manifest, _snapshot, inventory) = complete_g0_fixture();
        let manifest_repo = &manifest.repositories[0];
        let workflow = &inventory.collector_snapshot.repositories[0].workflows[0];
        let expected = &manifest_repo.expected_jobs[0];
        let job = crate::g0_workflow::DerivedWorkflowJob {
            job_id: expected.job_id.clone(),
            provider: expected.provider.clone(),
            platform: expected.platform.clone(),
            architecture: expected.architecture.clone(),
            uses_reusable_workflow: false,
            matrix: BTreeMap::from([(String::from("os"), String::from("ubuntu-24.04"))]),
        };
        let mut second = job.clone();
        second
            .matrix
            .insert("os".to_owned(), "ubuntu-22.04".to_owned());
        let plan = DerivedWorkflowPlan {
            jobs: vec![job, second],
            child_edges: Vec::new(),
            events: BTreeSet::from([String::from("push"), String::from("pull_request")]),
        };
        let raw_ids = workflow
            .source_jobs
            .iter()
            .flat_map(|job| job.raw_object_refs.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut findings = Vec::new();
        check_g0_source_jobs(
            &manifest_repo.repository,
            manifest_repo,
            workflow,
            &plan,
            &raw_ids,
            &mut findings,
        );
        assert!(findings
            .iter()
            .any(|finding| finding.code == "g0-source-job-matrix"));
    }

    #[test]
    fn manifest_job_target_must_match_workload_map() {
        let (manifest, _snapshot, _inventory) = complete_g0_fixture();
        let mut repository = manifest.repositories[0].clone();
        repository.expected_jobs[0].platform = "macos".to_owned();
        repository.expected_jobs[0].architecture = "arm64".to_owned();
        let mut findings = Vec::new();
        check_manifest_repository(&repository, &mut findings);
        assert!(findings
            .iter()
            .any(|finding| finding.code == "expected-job-target"));
    }

    #[tokio::test]
    async fn live_checker_requires_producer_typed_capture() {
        let (manifest, snapshot, _) = complete_g0_fixture();
        let evidence = EvidenceDocument {
            schema_version: EVIDENCE_SCHEMA_VERSION,
            manifest_id: manifest.manifest_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            stage: "G0".to_owned(),
            records: Vec::new(),
            reviewer_attestation: None,
            g0_inventory: None,
        };
        let directory = std::fs::canonicalize(std::env::temp_dir())
            .expect("canonical temp directory")
            .join(format!(
                "velnor-live-capture-required-{}",
                std::process::id()
            ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create fixture directory");
        let manifest_path = directory.join("manifest.json");
        let snapshot_path = directory.join("snapshot.json");
        let evidence_path = directory.join("evidence.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("serialize manifest"),
        )
        .expect("write manifest");
        std::fs::write(
            &snapshot_path,
            serde_json::to_vec(&snapshot).expect("serialize snapshot"),
        )
        .expect("write snapshot");
        std::fs::write(
            &evidence_path,
            serde_json::to_vec(&evidence).expect("serialize evidence"),
        )
        .expect("write evidence");

        let error = check_paths_live(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: manifest_path,
            snapshot: snapshot_path,
            evidence: evidence_path,
            release_manifest: None,
            live: true,
        })
        .await
        .expect_err("live mode must reject a result-only envelope");
        assert!(error
            .to_string()
            .contains("trusted authenticated collector"));
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn public_live_checker_rejects_caller_authored_capture_before_reading_files() {
        let error = check_paths_live(&EvidenceCheckInput {
            stage: "G0".to_owned(),
            manifest: PathBuf::from("caller-authored-manifest.json"),
            snapshot: PathBuf::from("caller-authored-snapshot.json"),
            evidence: PathBuf::from("caller-authored-evidence.json"),
            release_manifest: None,
            live: true,
        })
        .await
        .expect_err("public live entrypoint must not upgrade local files");
        assert!(error
            .to_string()
            .contains("trusted authenticated collector/current-API reconciliation"));
    }

    #[tokio::test]
    async fn evidence_check_command_rejects_synthetic_live_capture() {
        let error = evidence_check(EvidenceCheckArgs {
            stage: "G0".to_owned(),
            manifest: PathBuf::from("caller-authored-manifest.json"),
            snapshot: PathBuf::from("caller-authored-snapshot.json"),
            evidence: PathBuf::from("caller-authored-evidence.json"),
            release_manifest: None,
            live: true,
            json: true,
        })
        .await
        .expect_err("the public command handler must fail closed for synthetic live input");
        assert!(error
            .to_string()
            .contains("trusted authenticated collector/current-API reconciliation"));
    }
}
